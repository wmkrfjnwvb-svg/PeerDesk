#!/usr/bin/env python3
"""从被控端日志（已脱敏）抽出每秒一行的数据表和决定记录，给仿真、分析用。

用法：python3 -I scripts/log-to-trace.py 被控端.log 输出目录

输出：
- per-second.csv：每秒的发送统计（帧数、码率、目标、往返、在途、丢失、重传、纠错、画像）
  和最近一份反馈（排队、延迟趋势、丢包、送达、包串测速），加当时的码率状态。
- decisions.csv：发送端每一条“决定”（码率状态切换、升降码率、探测、帧率）和时间。
`session` 列是第几次会话（日志里每出现一次“连上来”加 1）。
"""

import csv
import os
import re
import sys

TIME = re.compile(r"^\d{4}-\d{2}-\d{2} (\d{2}):(\d{2}):(\d{2}\.\d+)Z")
SEND = re.compile(
    r"发送 帧 (\d+) 关键帧 (\d+) 画面 (\d+) kbps 线上 (\d+) kbps 目标 (\d+) kbps"
    r"(?: 帧率上限 (\d+))? 编码 (\d+) ms.*?在途 (\d+) KB 丢失 (\d+) 重传 (\d+) RTT (\d+) ms"
    r".*?纠错 (\d+)‰ 画像 (\w+)"
)
FEEDBACK = re.compile(
    r"最近反馈：排队 (\d+) ms，延迟趋势 (-?[\d.]+) ms，丢包 (\d+)‰，送达 (\d+) kbps，包串测速 (\d+) kbps"
)
STATE = re.compile(r"码率状态 \S+ → (\S+)：")

SECOND = [
    "session", "utc", "t_s", "frames", "keyframes", "video_kbps", "wire_kbps", "target_kbps",
    "fps_cap", "encode_ms", "inflight_kb", "lost", "retransmitted", "rtt_ms", "fec_permille",
    "profile", "queue_ms", "trend_ms", "loss_permille", "recv_kbps", "train_kbps", "state",
]


def seconds(m):
    return int(m.group(1)) * 3600 + int(m.group(2)) * 60 + float(m.group(3))


def main():
    if len(sys.argv) != 3:
        sys.exit("用法：log-to-trace.py 被控端.log 输出目录")
    os.makedirs(sys.argv[2], exist_ok=True)
    session, start, state = 0, None, "跟踪"
    rows, decisions = [], []
    pending = None
    with open(sys.argv[1], encoding="utf-8", errors="replace") as src:
        for line in src:
            t = TIME.match(line)
            if not t:
                continue
            utc = line[11:23]
            now = seconds(t)
            if start is None:
                start = now
            if "连上来" in line:
                session += 1
            if "stats: 决定 " in line:
                text = line.split("stats: 决定 ", 1)[1].strip()
                decisions.append([session, utc, f"{now - start:.3f}", text])
                s = STATE.search(text)
                if s:
                    state = s.group(1)
                continue
            m = SEND.search(line)
            if m:
                g = m.groups()
                pending = [session, utc, f"{now - start:.3f}", *g[:5], g[5] or "", *g[6:13]]
                continue
            f = FEEDBACK.search(line)
            if f and pending is not None:
                rows.append(pending + list(f.groups()) + [state])
                pending = None
    with open(os.path.join(sys.argv[2], "per-second.csv"), "w", newline="", encoding="utf-8") as out:
        w = csv.writer(out)
        w.writerow(SECOND)
        w.writerows(rows)
    with open(os.path.join(sys.argv[2], "decisions.csv"), "w", newline="", encoding="utf-8") as out:
        w = csv.writer(out)
        w.writerow(["session", "utc", "t_s", "decision"])
        w.writerows(decisions)
    print(f"{len(rows)} 秒、{len(decisions)} 条决定、{session} 次会话", file=sys.stderr)


if __name__ == "__main__":
    main()
