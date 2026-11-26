#!/usr/bin/env python3
"""真实程序 + 真实网络轨迹回放（测量用，不是门槛）。

做法照 Pantheon / mahimahi：两端跑真实的 peerdesk（真实采集、编码、QUIC、解码），中间放一个用户态 UDP 中继，按微软
MMSys 2024 挑战赛的轨迹（crates/e2e/traces/ms-*.csv）回放带宽和丢包；延迟、抖动、排队上限和仿真（crates/core/src/sim.rs）
一样：单向 25 ms + 0–3 ms 抖动、逐包随机丢包、两个方向同样的限速、排队超过 200 ms 就丢。
被控端的桌面在 Xvfb 里：壁纸、编辑器、文档、弹窗、视频窗口，按和仿真（crates/e2e 的工作负载）同样的节奏打字、滚动、
拖动、切换窗口、播视频，两遍。

两端打开逐帧时间日志（PEERDESK_FRAME_LOG，见 crates/net/src/framelog.rs），按和仿真同样的定义算等待：画面变了之后，
控制端显示出采集时间不早于这次变化的帧才算等完；超过 200 ms 算一次冻结。

为什么不用 tc netem：这台机器的内核没有 netem（只有 tbf），而且用户态中继不要 root，Mac、Windows 上也能跑。

用法（Linux，要 Xvfb、xterm、xdotool、ImageMagick、ffplay）：
  python3 -I scripts/trace-lab.py [--bin target/release/peerdesk] [--traces ms-09401,ms-02560] \\
      [--modes off,2500] [--runs 1] [--out target/trace-lab]
模式：off（不开节奏器）或倍数千分比（PEERDESK_PACE=1、PEERDESK_PACE_FACTOR=倍数）。结果表在 <out>/trace-lab.md。
"""
import argparse
import heapq
import os
import random
import re
import select
import shutil
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DISPLAY = ":94"
W, H = 1280, 720
FPS = 30
DELAY_S = 0.025
JITTER_S = 0.003
QUEUE_S = 0.200
FREEZE_S = 0.200
# 和 crates/e2e/tests/ms_traces.rs 的工作负载一样（帧数按 30 帧换算成秒），做两遍
OFFICE = [("idle", 30), ("typing", 90), ("idle", 30), ("scrolling", 60), ("idle", 30), ("dragging", 60),
          ("idle", 30), ("switching", 15), ("idle", 30), ("video", 90), ("idle", 30)]
WORKLOAD = OFFICE + OFFICE


# Wi-Fi 式抖动（秒，0 表示不加）：每个包额外晚到 0..N，但不乱序（真实 Wi-Fi 的抖动是一阵一阵的，基本不乱序）
WIFI_JITTER_S = 0.0


def trace_by_name(name):
    """`const-<kbps>`：恒定速率、不丢包的合成链路（宽带）；其他名字按 crates/e2e/traces 里的真实轨迹。"""
    if name.startswith("const-"):
        return [(0.0, int(name.split("-", 1)[1]) * 1000, 0)]
    return load_trace(ROOT / f"crates/e2e/traces/{name}.csv")


def load_trace(path):
    rows = []
    for line in path.read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        ms, bps, loss = (int(x) for x in line.split(","))
        rows.append((ms / 1000.0, bps, loss))
    return rows


class Relay(threading.Thread):
    """用户态链路：控制端 ⇄ 中继 ⇄ 被控端，两个方向同样的限速、丢包、延迟、排队上限（和仿真的瓶颈模型一样）。"""

    def __init__(self, host_addr, trace, seed):
        super().__init__(daemon=True)
        self.host_addr = host_addr
        self.trace = trace
        self.rng = random.Random(seed)
        self.front = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.front.bind(("127.0.0.1", 0))
        self.back = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.back.bind(("127.0.0.1", 0))
        self.port = self.front.getsockname()[1]
        self.client = None
        self.start_at = None
        self.started = threading.Event()
        self.stop = threading.Event()
        self.busy = {"down": 0.0, "up": 0.0}
        self.heap = []
        self.last_at = {"down": 0.0, "up": 0.0}
        self.seq = 0
        self.stats = {"sent": 0, "lost": 0, "queue_dropped": 0}

    def link(self, now):
        t = now - (self.start_at or now)
        rate, loss = self.trace[0][1], self.trace[0][2]
        for at, bps, lp in self.trace:
            if at <= t:
                rate, loss = bps, lp
            else:
                break
        return rate, loss

    def enqueue(self, direction, data, now):
        if self.start_at is None:
            self.start_at = now
            self.started.set()
        rate, loss = self.link(now)
        self.stats["sent"] += 1
        if self.rng.random() * 1000 < loss:
            self.stats["lost"] += 1
            return
        start = max(self.busy[direction], now)
        if start - now > QUEUE_S:
            self.stats["queue_dropped"] += 1
            return
        done = start + len(data) * 8 / rate
        self.busy[direction] = done
        at = done + DELAY_S + self.rng.random() * JITTER_S
        if WIFI_JITTER_S > 0:
            at = max(self.last_at[direction], at + self.rng.random() * WIFI_JITTER_S)
            self.last_at[direction] = at
        self.seq += 1
        heapq.heappush(self.heap, (at, self.seq, direction, data))

    def run(self):
        socks = [self.front, self.back]
        while not self.stop.is_set():
            now = time.monotonic()
            while self.heap and self.heap[0][0] <= now:
                _, _, direction, data = heapq.heappop(self.heap)
                try:
                    if direction == "down" and self.client:
                        self.front.sendto(data, self.client)
                    elif direction == "up":
                        self.back.sendto(data, self.host_addr)
                except OSError:
                    pass
            timeout = 0.005 if not self.heap else max(0.0, min(0.005, self.heap[0][0] - now))
            ready, _, _ = select.select(socks, [], [], timeout)
            now = time.monotonic()
            for s in ready:
                try:
                    data, addr = s.recvfrom(65536)
                except OSError:
                    continue
                if s is self.front:
                    self.client = addr
                    self.enqueue("up", data, now)
                else:
                    self.enqueue("down", data, now)


def sh(*args, **kw):
    return subprocess.run(args, capture_output=True, text=True, env={**os.environ, "DISPLAY": DISPLAY}, **kw)


def window_id(title, tries=80):
    for _ in range(tries):
        out = sh("xdotool", "search", "--name", f"^{title}$").stdout.split()
        if out:
            return out[0]
        time.sleep(0.1)
    raise RuntimeError(f"找不到窗口 {title}")


class Desktop:
    """Xvfb 里的办公桌面。每次实验重新搭，内容由种子决定。"""

    def __init__(self, work, seed):
        self.work = work
        self.seed = seed
        self.procs = []
        self.flag = work / "scrolling.flag"
        self.typed = 0
        self.switched = False
        self.drag_dir = 1

    def spawn(self, *args):
        p = subprocess.Popen(args, env={**os.environ, "DISPLAY": DISPLAY}, stdout=subprocess.DEVNULL,
                             stderr=subprocess.DEVNULL)
        self.procs.append(p)
        return p

    def setup(self):
        rng = random.Random(self.seed)
        wall = self.work / "wall.png"
        # 照片一样的壁纸：分形噪声加模糊
        subprocess.run(["convert", "-size", f"{W}x{H}", "-seed", str(self.seed), "plasma:fractal", "-blur", "0x3", str(wall)],
                       check=True)
        sh("display", "-window", "root", str(wall))
        words = "the of and to in is for on that with as by this be from or are at an it have not but which their " \
                "network frame delay packet window remote desktop screen video quality encoder session".split()
        doc = self.work / "doc.txt"
        doc.write_text("\n".join(" ".join(rng.choice(words) for _ in range(rng.randint(6, 14))) for _ in range(400)))
        printer = self.work / "printer.py"
        printer.write_text(
            "import os,sys,time\n"
            f"lines=open({str(doc)!r}).read().splitlines()\n"
            "i=0\n"
            "for l in lines[:30]: print(l)\n"
            "i=30\n"
            "while True:\n"
            f"    if os.path.exists({str(self.flag)!r}):\n"
            "        print(lines[i%len(lines)]); sys.stdout.flush(); i+=1\n"
            "    time.sleep(1/8.5)\n")
        # 文档（滚动、拖动的对象）、编辑器（打字）、弹窗（切换）、视频
        self.spawn("xterm", "-title", "doc", "-geometry", "80x30+60+60", "-bg", "white", "-fg", "black", "-fa", "DejaVu Sans Mono",
                   "-fs", "9", "-e", sys.executable, "-I", str(printer))
        self.spawn("xterm", "-title", "editor", "-geometry", "86x22+640+70", "-bg", "#fdf6e3", "-fg", "#333",
                   "-fa", "DejaVu Sans Mono", "-fs", "10", "-e", "cat")
        self.spawn("xterm", "-title", "popup", "-geometry", "100x30+250+120", "-bg", "#202830", "-fg", "#e0e0e0",
                   "-fa", "DejaVu Sans Mono", "-fs", "10", "-e", "sh", "-c", f"cat {doc}; sleep 100000")
        self.spawn("ffplay", "-loglevel", "quiet", "-noborder", "-left", "760", "-top", "420", "-x", "480", "-y", "270",
                   "-window_title", "video", "-f", "lavfi", "-i", "mandelbrot=size=480x270:rate=30")
        self.doc = window_id("doc")
        self.editor = window_id("editor")
        self.popup = window_id("popup")
        self.video = window_id("video")
        sh("xdotool", "windowunmap", self.popup)
        time.sleep(0.5)
        self.focus(self.video)
        sh("xdotool", "key", "space")  # 视频先暂停
        self.focus(self.editor)
        self.doc_pos = [60, 60]
        time.sleep(1.0)

    def focus(self, win):
        sh("xdotool", "windowfocus", "--sync", win)

    def step(self, action, frame):
        if action == "typing" and frame % 3 == 0:
            ch = "abcdefghijklmnopqrstuvwxyz"[self.typed % 26] if self.typed % 9 else "space"
            sh("xdotool", "key", ch)
            self.typed += 1
        elif action == "dragging":
            x = self.doc_pos[0] + 8 * self.drag_dir
            if x > W // 2 or x < 20:
                self.drag_dir = -self.drag_dir
            self.doc_pos[0] = x
            sh("xdotool", "windowmove", self.doc, str(x), str(self.doc_pos[1]))
        elif action == "switching" and frame == 0 and not self.switched:
            self.switched = True
            sh("xdotool", "windowmap", self.popup)
            sh("xdotool", "windowraise", self.popup)

    def begin(self, action):
        if action == "scrolling":
            self.flag.touch()
        elif action == "video":
            self.focus(self.video)
            sh("xdotool", "key", "space")
            self.focus(self.editor)

    def end(self, action):
        if action == "scrolling":
            self.flag.unlink(missing_ok=True)
        elif action == "video":
            self.focus(self.video)
            sh("xdotool", "key", "space")
            self.focus(self.editor)

    def run(self, t0):
        """按工作负载走一遍；t0 是开始时刻（单调时钟）。"""
        t = t0
        for action, frames in WORKLOAD:
            self.begin(action)
            for f in range(frames):
                t += 1 / FPS
                self.step(action, f)
                delay = t - time.monotonic()
                if delay > 0:
                    time.sleep(delay)
            self.end(action)

    def close(self):
        self.flag.unlink(missing_ok=True)
        for p in self.procs:
            p.kill()
        for p in self.procs:
            p.wait()


def analyze(host_log, ctrl_log, start_unix, end_unix):
    """按仿真同样的定义算等待：返回 (首帧 s, [等待 s...])。"""
    captured = {}
    events = []
    for line in host_log.read_text().splitlines():
        parts = line.split()
        if parts[0] == "C":
            events.append((int(parts[1]) / 1e6, 0, None))
        elif parts[0] == "E":
            captured[int(parts[1])] = int(parts[2]) / 1e6
    for line in ctrl_log.read_text().splitlines():
        parts = line.split()
        if parts[0] == "S":
            events.append((int(parts[2]) / 1e6, 1, int(parts[1])))
    events.sort()
    waiting, first, waits = None, None, []
    for at, kind, fid in events:
        if at > end_unix:
            break
        if kind == 0:
            if waiting is None and at >= start_unix - 1.0:
                waiting = at
        elif waiting is not None and captured.get(fid, 0) >= waiting:
            if first is None:
                first = at - waiting
            else:
                waits.append(at - waiting)
            waiting = None
    if waiting is not None and first is not None:
        waits.append(end_unix - waiting)
    return first, waits


def one_run(args, name, trace, mode, run, out):
    work = out / f"{name}-{mode}-{run}"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    seed = run * 1000 + sum(map(ord, name))
    desk = Desktop(work, seed)
    desk.setup()
    env = {**os.environ, "DISPLAY": DISPLAY, "PEERDESK_FRAME_LOG": str(work / "host.frames")}
    # 模式：off / 数字（节奏器关、开并按这个系数）；probe-on / probe-off（节奏器关，起步探测开、关）
    env["PEERDESK_PACE"] = "0" if mode in ("off", "probe-on", "probe-off") else "1"
    if mode not in ("off", "probe-on", "probe-off"):
        env["PEERDESK_PACE_FACTOR"] = mode
    if mode == "probe-off":
        env["PEERDESK_START_PROBE"] = "0"
    host_log = open(work / "host.log", "w")
    host = subprocess.Popen([args.bin, "host", "--listen", "127.0.0.1:0", "--data-dir", str(work / "host"), "--open", "--once",
                             "--fps", str(FPS), "--source", "screen"], env=env, stdout=host_log, stderr=subprocess.STDOUT)
    addr = pin = None
    for _ in range(200):
        text = (work / "host.log").read_text()
        a = re.search(r"^监听 (\S+)", text, re.M)
        p = re.search(r"^指纹 (\S+)", text, re.M)
        if a and p:
            addr, pin = a.group(1), p.group(1)
            break
        time.sleep(0.05)
    if not addr:
        host.kill()
        desk.close()
        raise RuntimeError(f"被控端没起来：{work / 'host.log'}")
    hh, hp = addr.rsplit(":", 1)
    relay = Relay((hh, int(hp)), trace, seed)
    relay.start()
    seconds = int(sum(f for _, f in WORKLOAD) / FPS) + 4
    cenv = {**os.environ, "PEERDESK_FRAME_LOG": str(work / "ctrl.frames")}
    ctrl = subprocess.Popen([args.bin, "connect", f"127.0.0.1:{relay.port}", "--pin", pin, "--data-dir", str(work / "ctrl"),
                             "--seconds", str(seconds)], env=cenv, stdout=open(work / "ctrl.log", "w"), stderr=subprocess.STDOUT)
    if not relay.started.wait(20):
        ctrl.kill()
        host.kill()
        desk.close()
        raise RuntimeError("会话没开始")
    t0 = time.monotonic()
    start_unix = time.time()
    desk.run(t0)
    end_unix = time.time()
    ctrl.wait(timeout=60)
    host.wait(timeout=30)
    relay.stop.set()
    desk.close()
    first, waits = analyze(work / "host.frames", work / "ctrl.frames", start_unix, end_unix)
    freezes = sum(1 for w in waits if w > FREEZE_S)
    # 开头的码率：被控端每秒一行统计里的“目标”，取前 3 秒；起步探测的结果
    text = (work / "host.log").read_text(errors="replace")
    targets = [int(x) for x in re.findall(r"stats: 发送 .*?目标 (\d+) kbps", text)][:3]
    probe = re.search(r"(起步探测[^\n]*|不做起步探测[^\n]*)", text)
    return {
        "first": first, "freezes": freezes, "max": max(waits, default=0.0), "n": len(waits),
        "lost": relay.stats["lost"], "qdrop": relay.stats["queue_dropped"], "sent": relay.stats["sent"],
        "start": "/".join(str(t) for t in targets), "probe": probe.group(1)[:60] if probe else "",
    }


def main():
    global W, H, DELAY_S, WIFI_JITTER_S
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=str(ROOT / "target/release/peerdesk"))
    ap.add_argument("--traces", default="")
    ap.add_argument("--modes", default="off,2500")
    ap.add_argument("--runs", type=int, default=1)
    ap.add_argument("--out", default=str(ROOT / "target/trace-lab"))
    ap.add_argument("--size", default=f"{W}x{H}", help="被控端屏幕，例如 2880x1800")
    ap.add_argument("--delay-ms", type=float, default=DELAY_S * 1000, help="单程延迟")
    ap.add_argument("--wifi-jitter-ms", type=float, default=0.0, help="Wi-Fi 式抖动（不乱序）")
    args = ap.parse_args()
    W, H = (int(x) for x in args.size.split("x"))
    DELAY_S = args.delay_ms / 1000
    WIFI_JITTER_S = args.wifi_jitter_ms / 1000
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    names = [n for n in args.traces.split(",") if n] or sorted(p.stem for p in (ROOT / "crates/e2e/traces").glob("ms-*.csv"))
    xvfb = subprocess.Popen(["Xvfb", DISPLAY, "-screen", "0", f"{W}x{H}x24"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1.0)
    table = out / "trace-lab.md"
    rows = ["| 轨迹 | 模式 | 次 | 冻结（>200 ms） | 最长等待 ms | 首帧 ms | 等待次数 | 丢包 / 排队丢 / 发出 | 前 3 秒码率 kbps | 起步探测 |",
            "|---|---|---|---|---|---|---|---|---|---|"]
    totals = {}
    try:
        for name in names:
            trace = trace_by_name(name)
            for run in range(1, args.runs + 1):
                for mode in args.modes.split(","):
                    r = one_run(args, name, trace, mode, run, out)
                    first = "无" if r["first"] is None else f"{r['first'] * 1000:.0f}"
                    row = (f"| {name} | {mode} | {run} | {r['freezes']} | {r['max'] * 1000:.0f} | {first} | {r['n']} | "
                           f"{r['lost']} / {r['qdrop']} / {r['sent']} | {r['start']} | {r['probe']} |")
                    rows.append(row)
                    print(row, flush=True)
                    t = totals.setdefault(mode, [0, 0, 0.0, 0.0])
                    t[0] += 1
                    t[1] += r["freezes"]
                    t[2] += r["max"]
                    t[3] = max(t[3], r["max"])
                    table.write_text("\n".join(rows) + "\n")
    finally:
        xvfb.kill()
    for mode, (n, fr, wsum, wmax) in totals.items():
        line = f"合计 {mode}：{n} 次，冻结 {fr} 次，最长等待平均 {wsum / max(n, 1) * 1000:.0f} ms、最大 {wmax * 1000:.0f} ms"
        rows.append("")
        rows.append(line)
        print(line, flush=True)
    table.write_text("\n".join(rows) + "\n")


if __name__ == "__main__":
    main()
