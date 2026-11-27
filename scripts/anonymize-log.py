#!/usr/bin/env python3
"""把用户发来的 PeerDesk 日志脱敏，之后才能放进仓库当仿真和分析数据。

用法：python3 -I scripts/anonymize-log.py 输入.log 输出.log

换掉的东西（同一个值总是换成同一个占位符，日志里的对应关系不变）：
- IPv4 地址（127.0.0.1、0.0.0.0 保留）→ ip-1、ip-2……（端口保留）
- IPv6 地址 → ip6-1……
- 16 位以上的十六进制串（会话号、设备指纹）→ id-1……
- 设备码（9 位数字，日志里写成“设备码 123 456 789”）→ 设备码 000 000 001……
- 用户目录（/Users/名字、/home/名字、C:\\Users\\名字）→ user

不动的：时间、码率、延迟、丢包等全部数字，以及中文的决定说明。
"""

import re
import sys

IPV4 = re.compile(r"(?<![\d.])(\d{1,3}(?:\.\d{1,3}){3})(?![\d.])")
# 中括号里的、或者至少带 3 个冒号的十六进制段（避免误伤“01:55:56”这样的时间）
IPV6 = re.compile(r"(?<![0-9A-Za-z:])((?:[0-9a-fA-F]{1,4}:){3,7}[0-9a-fA-F]{1,4}|(?:[0-9a-fA-F]{1,4}:){1,6}:(?:[0-9a-fA-F]{1,4}(?::[0-9a-fA-F]{1,4})*)?)(?![0-9A-Za-z:])")
HEX = re.compile(r"(?<![0-9a-fA-F])([0-9a-f]{16,64})(?![0-9a-fA-F])")
CODE = re.compile(r"设备码(\s*)(\d{3}\s?\d{3}\s?\d{3})")
HOME = re.compile(r"(/Users/|/home/|C:\\\\Users\\\\|C:\\Users\\)([^/\\\s\"']+)")
KEEP_V4 = {"127.0.0.1", "0.0.0.0"}


class Mapper:
    """同一个值换成同一个占位符。"""

    def __init__(self, prefix):
        self.prefix = prefix
        self.seen = {}

    def __call__(self, value):
        if value not in self.seen:
            self.seen[value] = f"{self.prefix}{len(self.seen) + 1}"
        return self.seen[value]


def looks_like_time(text):
    # 日志里的时间“01:55:56.123”：段都是十进制、最多 3 段
    return re.fullmatch(r"\d{1,2}(:\d{2}){1,2}(\.\d+)?", text) is not None


def main():
    if len(sys.argv) != 3:
        sys.exit("用法：anonymize-log.py 输入.log 输出.log")
    v4, v6, ids = Mapper("ip-"), Mapper("ip6-"), Mapper("id-")
    codes = {}

    def code(m):
        digits = re.sub(r"\s", "", m.group(2))
        if digits not in codes:
            n = len(codes) + 1
            codes[digits] = f"{n:09d}"
        c = codes[digits]
        return f"设备码{m.group(1)}{c[0:3]} {c[3:6]} {c[6:9]}"

    def ipv4(m):
        ip = m.group(1)
        if ip in KEEP_V4 or not all(0 <= int(p) <= 255 for p in ip.split(".")):
            return ip
        return v4(ip)

    def ipv6(m):
        text = m.group(1)
        return text if looks_like_time(text) else v6(text.lower())

    with open(sys.argv[1], encoding="utf-8", errors="replace") as src, open(
        sys.argv[2], "w", encoding="utf-8"
    ) as dst:
        for line in src:
            line = CODE.sub(code, line)
            line = HOME.sub(lambda m: m.group(1) + "user", line)
            line = IPV6.sub(ipv6, line)
            line = IPV4.sub(ipv4, line)
            line = HEX.sub(lambda m: ids(m.group(1)), line)
            dst.write(line)
    print(
        f"换掉 IPv4 {len(v4.seen)} 个、IPv6 {len(v6.seen)} 个、编号 {len(ids.seen)} 个、设备码 {len(codes)} 个",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
