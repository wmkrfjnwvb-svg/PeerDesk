#!/usr/bin/env python3
"""把微软 MMSys 2024 带宽估计挑战赛仓库自带的 24 通评测通话整通转成网络轨迹（不截窗口），并给出统计。

数据：https://github.com/microsoft/RL4BandwidthEstimationChallenge 仓库的 data/ 目录（许可见 THIRD_PARTY_NOTICES.md，
“Microsoft MMSys 2024”一节）。不要用它转换 Azure 上另外下载的大数据集：那两份没有写明授权，转出来的东西不能提交进仓库。
和 scripts/ms-traces.py 的区别：整通保留、不截 40 秒窗口；输出到另一个目录（crates/e2e/traces/ms-full/），不碰现有的 12 条。
格式相同：`毫秒,带宽 bps,丢包千分比`，只写变化的时刻。

每步 60 ms：文件里没有时间戳，依据见 docs/research/Q-redesign/data-m0-ms24.md 第 1 节。
缺失值（nan）：这 24 通里只出现在结尾 1–2 步，沿用上一步；开头的 nan（这 24 通没有）用第一个有效值补。
带宽取整到 bps（数据里有 7999999.999… 这类浮点误差），丢包百分数乘 10 取整成千分比，负零按 0。

用法：
  python3 -I scripts/ms-traces-full.py <数据目录> <输出目录>            # 转换 <数据目录> 下全部 *.json
  python3 -I scripts/ms-traces-full.py <数据目录> <输出目录> --stats    # 同时在标准输出打印统计表（Markdown）
"""
import json
import math
import statistics
import sys
from pathlib import Path

STEP_MS = 60


def is_nan(v):
    return v is None or (isinstance(v, float) and math.isnan(v))


def clean(values, scale):
    """nan 处理见文件头；返回整数列表。"""
    first = next((v for v in values if not is_nan(v)), 0)
    last = max(0, round(first * scale))
    out = []
    for v in values:
        if not is_nan(v):
            last = max(0, round(v * scale))
        out.append(last)
    return out


def shape(rate, loss):
    """按数据判形状。规则是我们自己定的（微软没有给每通的场景标签，论文只列了 5 类：
    固定低带宽、固定高带宽、波动带宽、突发丢包、波动突发丢包）：
    - 丢包均值 ≥ 1%：突发丢包（带宽也在变则“变化+丢包”）；
    - 否则带宽 (最大-最小)/中位 > 0.2：变化；
    - 否则中位带宽 < 2 Mbps：固定低，否则固定高。
    “限速”（traffic policing）从这两条序列里分不出来，不判。"""
    med = statistics.median(rate)
    varies = (max(rate) - min(rate)) / med > 0.2
    if statistics.mean(loss) >= 10:
        return "变化+丢包" if varies else "突发丢包"
    if varies:
        return "变化"
    return "固定低" if med < 2_000_000 else "固定高"


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    want_stats = "--stats" in sys.argv
    src, dst = Path(args[0]), Path(args[1])
    dst.mkdir(parents=True, exist_ok=True)
    rows_out = []
    for f in sorted(src.glob("*.json")):
        call = f.stem
        d = json.loads(f.read_text())
        rate = clean(d["true_capacity"], 1)
        loss = [min(1000, x) for x in clean(d["true_loss_rate"], 10)]  # 百分数 → 千分比
        total_ms = len(rate) * STEP_MS
        lines = [
            f"# 微软 MMSys 2024 带宽估计挑战赛仿真通话 {call}（CC BY 4.0 / MIT），整通 {total_ms / 1000:.2f} 秒（{len(rate)} 步 × {STEP_MS} ms）",
            "# 来源：https://github.com/microsoft/RL4BandwidthEstimationChallenge 的 data/ 目录；转换：scripts/ms-traces-full.py",
            "# 毫秒,带宽bps,丢包千分比",
        ]
        prev, changes = None, 0
        for i, row in enumerate(zip(rate, loss)):
            if row != prev:
                lines.append(f"{i * STEP_MS},{row[0]},{row[1]}")
                if prev is not None:
                    changes += 1
                prev = row
        (dst / f"ms-{call}.csv").write_text("\n".join(lines) + "\n")
        rate_changes = sum(1 for a, b in zip(rate, rate[1:]) if a != b)
        loss_changes = sum(1 for a, b in zip(loss, loss[1:]) if a != b)
        rows_out.append((call, total_ms / 1000, min(rate), statistics.median(rate), max(rate), rate_changes,
                         statistics.mean(loss) / 10, max(loss) / 10, loss_changes, shape(rate, loss)))
        print(f"ms-{call}: {len(lines) - 3} 个时刻（带宽变化 {rate_changes}，丢包变化 {loss_changes}），{total_ms / 1000:.1f} 秒")
    if want_stats:
        print("\n| 通话 | 时长 s | 带宽最小 kbps | 中位 kbps | 最大 kbps | 带宽变化次数 | 丢包均值 % | 丢包最大 % | 丢包变化次数 | 形状 |")
        print("|---|---|---|---|---|---|---|---|---|---|")
        for c, t, lo, md, hi, rc, lm, lx, lc, sh in rows_out:
            print(f"| {c} | {t:.1f} | {lo / 1e3:.0f} | {md / 1e3:.0f} | {hi / 1e3:.0f} | {rc} | {lm:.2f} | {lx:.1f} | {lc} | {sh} |")


if __name__ == "__main__":
    main()
