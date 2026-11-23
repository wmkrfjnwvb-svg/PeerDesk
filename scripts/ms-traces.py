#!/usr/bin/env python3
"""把微软 MMSys 2024 带宽估计挑战赛的仿真通话（带真实带宽和丢包）转成端到端测试用的网络轨迹。

数据：https://github.com/microsoft/RL4BandwidthEstimationChallenge（数据 CC BY 4.0，见 THIRD_PARTY_NOTICES.md）
用法：python3 -I scripts/ms-traces.py <数据目录> <输出目录> <通话号>...
每一步 60 ms（挑战赛说明里观测和带宽估计的间隔）。取每通里带宽和丢包变化最多的 WINDOW_S 秒，
只写变化的时刻：`毫秒,带宽 bps,丢包千分比`。缺失值（nan）沿用上一步。
"""
import json
import math
import sys
from pathlib import Path

STEP_MS = 60
WINDOW_S = 40


def clean(values, scale):
    out, last = [], 0
    for v in values:
        if v is not None and not (isinstance(v, float) and math.isnan(v)):
            last = max(0, round(v * scale))
        out.append(last)
    return out


def main():
    src, dst, ids = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3:]
    n = WINDOW_S * 1000 // STEP_MS
    for call in ids:
        d = json.loads((src / f"{call}.json").read_text())
        rate = clean(d["true_capacity"], 1)
        loss = [min(1000, x) for x in clean(d["true_loss_rate"], 10)]  # 百分数 → 千分比
        rows = list(zip(rate, loss))
        # 变化最多的窗口（并列时取最早的），窗口外的不要
        changes = [int(a != b) for a, b in zip(rows, rows[1:])] + [0]
        best, start = -1, 0
        for i in range(max(1, len(rows) - n + 1)):
            score = sum(changes[i : i + n])
            if score > best:
                best, start = score, i
        window = rows[start : start + n]
        lines = [f"# 微软 MMSys 2024 带宽估计挑战赛仿真通话 {call}（CC BY 4.0），第 {start * STEP_MS / 1000:.1f} 秒起 {WINDOW_S} 秒",
                 "# 毫秒,带宽bps,丢包千分比"]
        prev = None
        for i, row in enumerate(window):
            if row != prev:
                lines.append(f"{i * STEP_MS},{row[0]},{row[1]}")
                prev = row
        (dst / f"ms-{call}.csv").write_text("\n".join(lines) + "\n")
        print(f"ms-{call}: {len(lines) - 2} 次变化，带宽 {min(r for r, _ in window) / 1e3:.0f}–{max(r for r, _ in window) / 1e3:.0f} kbps，"
              f"丢包最高 {max(l for _, l in window) / 10:.1f}%")


if __name__ == "__main__":
    main()
