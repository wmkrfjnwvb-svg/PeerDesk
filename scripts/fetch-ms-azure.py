#!/usr/bin/env python3
"""从微软 Azure 公开存储里按需取挑战赛的两份大数据集（不下载整包）。

!!! 许可：这两份数据（仿真集 emulated_dataset，约 8 GB；测试床集 testbed_dataset，约 35 GB）的许可
!!! 只写了免责声明（“AS IS”，见挑战赛仓库 README 的 “Dataset licenses” 一节），没写授权。
!!! 所以只在本地用：不能把数据、也不能把从中摘出来的任何片段提交进仓库（设计文档 1.10 节）。
!!! 默认缓存目录 ~/.cache/peerdesk/ms-azure/ 在仓库之外；不要把缓存目录设到仓库里。

地址来自挑战赛仓库的 download-emulated-dataset.sh 和 download-testbed-dataset.sh：
  https://dnschallengepublic.blob.core.windows.net/bwechallenge-2023/{emulated,testbed}_dataset/<数据集>_chunk_<N>.zip
仿真集 3 块（chunk_0..2），测试床集 5 块（chunk_0..4）。每块几个 GB，所以不整包下载：
  1. 用 HTTP HEAD 取总大小；
  2. 用 HTTP Range 读 zip 末尾，找到中央目录（支持 zip64），中央目录缓存成一个 JSON；
  3. 按需用 Range 只取单个成员的字节，解压后校验大小和 CRC32，通过才落盘。

用法：
  python3 -I scripts/fetch-ms-azure.py list <数据集> <块号> [--filter 子串] [--limit N]
  python3 -I scripts/fetch-ms-azure.py get  <数据集> <块号> <成员名>... [--max-mb 50]
  python3 -I scripts/fetch-ms-azure.py clean            # 删掉整个缓存
  <数据集> 是 emulated 或 testbed；全局参数 --cache <目录> 改缓存位置。
get 默认拒绝大于 --max-mb（默认 50 MB）的成员，避免手滑下载一个巨大的文件。
"""
import argparse
import json
import os
import shutil
import struct
import sys
import urllib.request
import zlib
from pathlib import Path

BASE = "https://dnschallengepublic.blob.core.windows.net/bwechallenge-2023"
CHUNKS = {"emulated": 3, "testbed": 5}
DEFAULT_CACHE = Path("~/.cache/peerdesk/ms-azure").expanduser()
TAIL = 128 * 1024  # 读末尾这么多字节找中央目录结束记录


class Fail(Exception):
    pass


def url_of(kind, n):
    if kind not in CHUNKS:
        raise Fail(f"数据集只能是 emulated 或 testbed，收到 {kind!r}")
    if not 0 <= n < CHUNKS[kind]:
        raise Fail(f"{kind} 只有 chunk_0..chunk_{CHUNKS[kind] - 1}，收到 {n}")
    return f"{BASE}/{kind}_dataset/{kind}_dataset_chunk_{n}.zip"


def http(url, method="GET", rng=None):
    req = urllib.request.Request(url, method=method)
    if rng:
        req.add_header("Range", f"bytes={rng[0]}-{rng[1]}")
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            body = b"" if method == "HEAD" else r.read()
            return r.status, dict(r.headers), body
    except Exception as e:  # 网络错误统一报原因和办法，不静默
        raise Fail(f"请求 {url} 失败：{e}。检查网络和代理（HTTPS_PROXY / SSL_CERT_FILE）后重试。") from e


def total_size(url):
    _, h, _ = http(url, "HEAD")
    n = h.get("Content-Length")
    if n is None:
        raise Fail("服务器没返回 Content-Length，没法按范围读")
    return int(n)


def fetch_range(url, lo, hi):
    """取 [lo, hi]（含），并核对返回的确实是 206 和期望的字节数。"""
    status, _, body = http(url, "GET", (lo, hi))
    if status != 206:
        raise Fail(f"服务器不支持 Range（状态码 {status}），不能只取部分文件")
    if len(body) != hi - lo + 1:
        raise Fail(f"范围 {lo}-{hi} 应返回 {hi - lo + 1} 字节，实际 {len(body)}")
    return body


def parse_directory(url, size):
    tail_start = max(0, size - TAIL)
    tail = fetch_range(url, tail_start, size - 1)
    i = tail.rfind(b"PK\x05\x06")
    if i < 0:
        raise Fail("末尾找不到 zip 中央目录结束记录，文件可能不是 zip")
    _disk, _cd_disk, _ent_here, n_entries, cd_size, cd_off, _comment = struct.unpack("<HHHHIIH", tail[i + 4:i + 22])
    if 0xFFFFFFFF in (cd_size, cd_off) or n_entries == 0xFFFF:  # zip64
        j = tail.rfind(b"PK\x06\x07", 0, i)
        if j < 0:
            raise Fail("zip64 定位记录缺失")
        (eocd64_off,) = struct.unpack("<Q", tail[j + 8:j + 16])
        rec = fetch_range(url, eocd64_off, eocd64_off + 55)
        if rec[:4] != b"PK\x06\x06":
            raise Fail("zip64 结束记录签名不对")
        n_entries, cd_size, cd_off = struct.unpack("<QQQ", rec[32:56])
    cd = fetch_range(url, cd_off, cd_off + cd_size - 1)
    out, p = [], 0
    for _ in range(n_entries):
        if cd[p:p + 4] != b"PK\x01\x02":
            raise Fail("中央目录条目签名不对，目录可能损坏")
        (_v, _need, flags, method, _t, _d, crc, csize, usize, nlen, elen, clen, _dn, _ia, _ea, lho) = \
            struct.unpack("<HHHHHHIIIHHHHHII", cd[p + 4:p + 46])
        name = cd[p + 46:p + 46 + nlen].decode("utf-8" if flags & 0x800 else "cp437")
        extra = cd[p + 46 + nlen:p + 46 + nlen + elen]
        q = 0
        while q + 4 <= len(extra):  # zip64 扩展字段：只补那些写成 0xFFFFFFFF 的值，顺序固定
            hid, dlen = struct.unpack("<HH", extra[q:q + 4])
            if hid == 1:
                z, k = extra[q + 4:q + 4 + dlen], 0
                if usize == 0xFFFFFFFF:
                    (usize,), k = struct.unpack("<Q", z[k:k + 8]), k + 8
                if csize == 0xFFFFFFFF:
                    (csize,), k = struct.unpack("<Q", z[k:k + 8]), k + 8
                if lho == 0xFFFFFFFF:
                    (lho,), k = struct.unpack("<Q", z[k:k + 8]), k + 8
            q += 4 + dlen
        out.append({"name": name, "method": method, "crc": crc, "csize": csize, "usize": usize, "lho": lho})
        p += 46 + nlen + elen + clen
    return out


def load_directory(cache, kind, n):
    url = url_of(kind, n)
    f = cache / f"{kind}_chunk_{n}.dir.json"
    size = total_size(url)
    if f.exists():
        d = json.loads(f.read_text())
        if d.get("size") == size:  # 大小没变才信缓存
            return url, d["entries"]
    entries = parse_directory(url, size)
    cache.mkdir(parents=True, exist_ok=True)
    f.write_text(json.dumps({"size": size, "entries": entries}))
    return url, entries


def get_member(url, cache, kind, n, e, max_mb):
    if e["usize"] > max_mb * 1024 * 1024:
        raise Fail(f"{e['name']} 解压后 {e['usize'] / 1e6:.1f} MB，超过 --max-mb {max_mb}；确实要取就调大它")
    dest = cache / f"{kind}_chunk_{n}" / e["name"]
    if dest.exists() and dest.stat().st_size == e["usize"]:
        return dest, "已在缓存"
    lh = fetch_range(url, e["lho"], e["lho"] + 29)  # 本地文件头 30 字节，里面的名字和 extra 长度可能和中央目录不同
    if lh[:4] != b"PK\x03\x04":
        raise Fail("本地文件头签名不对")
    nlen, elen = struct.unpack("<HH", lh[26:30])
    lo = e["lho"] + 30 + nlen + elen
    raw = fetch_range(url, lo, lo + e["csize"] - 1) if e["csize"] else b""
    if e["method"] == 0:
        data = raw
    elif e["method"] == 8:
        data = zlib.decompress(raw, -15)
    else:
        raise Fail(f"{e['name']} 用了不支持的压缩方式 {e['method']}")
    if len(data) != e["usize"]:
        raise Fail(f"{e['name']} 大小不对：目录写 {e['usize']}，解压得 {len(data)}。没落盘。")
    if (zlib.crc32(data) & 0xFFFFFFFF) != e["crc"]:
        raise Fail(f"{e['name']} CRC32 校验失败。没落盘，重试；反复失败说明服务器内容变了。")
    # 成员名来自外部数据：只允许落在缓存目录里
    if not dest.resolve().is_relative_to((cache / f"{kind}_chunk_{n}").resolve()):
        raise Fail(f"成员名 {e['name']!r} 想跳出缓存目录，拒绝")
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_bytes(data)
    return dest, f"取了 {e['csize']} 字节压缩数据，解压 {len(data)} 字节，大小和 CRC32 通过"


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--cache", type=Path, default=DEFAULT_CACHE)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("list")
    s.add_argument("kind"); s.add_argument("chunk", type=int)
    s.add_argument("--filter", default=""); s.add_argument("--limit", type=int, default=20)
    s = sub.add_parser("get")
    s.add_argument("kind"); s.add_argument("chunk", type=int); s.add_argument("members", nargs="+")
    s.add_argument("--max-mb", type=float, default=50)
    sub.add_parser("clean")
    a = ap.parse_args()
    cache = a.cache.expanduser()
    try:
        if a.cmd == "clean":
            shutil.rmtree(cache, ignore_errors=True)
            print(f"已删除 {cache}")
            return 0
        url, entries = load_directory(cache, a.kind, a.chunk)
        if a.cmd == "list":
            sel = [e for e in entries if a.filter in e["name"]]
            print(f"{url}：{len(entries)} 个成员，匹配 {len(sel)} 个，显示前 {a.limit} 个")
            for e in sel[:a.limit]:
                print(f"{e['usize']:>12}  {e['name']}")
            return 0
        by = {e["name"]: e for e in entries}
        for m in a.members:
            if m not in by:
                raise Fail(f"{url} 里没有成员 {m!r}（用 list --filter 查名字）")
            dest, note = get_member(url, cache, a.kind, a.chunk, by[m], a.max_mb)
            print(f"{dest}：{note}")
        return 0
    except Fail as e:
        print(f"失败：{e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
