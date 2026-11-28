#!/usr/bin/env python3
"""两个版本在同一批世界文件上的配对比较（批量运行器 `crates/e2e/examples/batch.rs` 的两份 CSV）。

用法：scripts/paired-compare.py 改前.csv 改后.csv [--by-name]

按 world_hash 配对（同一个世界、同一个种子），每个量报：两版的均值、差的均值和两倍标准误、
改后更好 / 更差 / 一样的会话数。判断照 CLAUDE.md：差的均值加减两倍标准误不跨 0 才算有差别。
--by-name 另外按场景名分组，每组报冻结、最长等待、码率 5% 分位的差。
"""
import csv
import math
import sys

# (列名, 越小越好？, 说明)
METRICS = [
    ("freezes", True, "冻结次数"),
    ("max_wait_ms", True, "最长等待 ms"),
    ("rate_time_p50_kbps", False, "码率中位（按时间）kbps"),
    ("rate_time_p5_kbps", False, "码率 5% 分位（按时间）kbps"),
    ("rate_cuts", True, "降码率次数"),
    ("rate_deep_cuts", True, "降幅过半次数"),
    ("fps_changes", True, "帧率换档次数"),
    ("frames", False, "帧数"),
    ("keyframes", True, "关键帧"),
    ("retransmitted", True, "重传分片"),
    ("sent_bytes", None, "发出字节"),
    ("sharp_total_ms", True, "停下后变清晰合计 ms"),
]


def num(row, col):
    if col == "sharp_total_ms":
        parts = [p for p in row.get("sharp_ms", "").split(";") if p not in ("", "-")]
        return float(sum(float(p) for p in parts))
    v = row.get(col, "")
    return float(v) if v not in ("", "-") else float("nan")


def load(path):
    with open(path, encoding="utf-8") as f:
        return {r["world_hash"]: r for r in csv.DictReader(f) if not r.get("error")}


def stats(diffs):
    n = len(diffs)
    if n == 0:
        return float("nan"), float("nan")
    mean = sum(diffs) / n
    if n < 2:
        return mean, float("nan")
    var = sum((d - mean) ** 2 for d in diffs) / (n - 1)
    return mean, 2 * math.sqrt(var / n)


def report(pairs, title):
    print(f"\n## {title}（{len(pairs)} 对）\n")
    print("| 量 | 改前均值 | 改后均值 | 差（改后−改前）± 2 标准误 | 有差别 | 改后更好 / 更差 / 一样 |")
    print("|---|---|---|---|---|---|")
    for col, smaller_better, label in METRICS:
        a = [num(x, col) for x, _ in pairs]
        b = [num(y, col) for _, y in pairs]
        ok = [(p, q) for p, q in zip(a, b) if not (math.isnan(p) or math.isnan(q))]
        if not ok:
            continue
        d = [q - p for p, q in ok]
        mean, se2 = stats(d)
        sig = "是" if not math.isnan(se2) and abs(mean) > se2 else "否"
        if smaller_better is None:
            better = worse = "-"
            same = sum(1 for x in d if x == 0)
        else:
            better = sum(1 for x in d if (x < 0 if smaller_better else x > 0))
            worse = sum(1 for x in d if (x > 0 if smaller_better else x < 0))
            same = len(d) - better - worse
        ma = sum(p for p, _ in ok) / len(ok)
        mb = sum(q for _, q in ok) / len(ok)
        print(f"| {label} | {ma:.1f} | {mb:.1f} | {mean:+.1f} ± {se2:.1f} | {sig} | {better} / {worse} / {same} |")


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if len(args) != 2:
        sys.exit(__doc__)
    before, after = load(args[0]), load(args[1])
    keys = sorted(set(before) & set(after))
    missing = (set(before) ^ set(after))
    if missing:
        print(f"注意：{len(missing)} 个世界只在一边有，没有配对", file=sys.stderr)
    pairs = [(before[k], after[k]) for k in keys]
    report(pairs, "全部")
    if "--by-name" in sys.argv:
        names = sorted({p[0]["name"] for p in pairs})
        print("\n## 按场景名\n")
        print("| 场景 | 对数 | 冻结 改前→改后 | 最长等待中位 改前→改后 | 码率 5% 分位均值 改前→改后 | 降幅过半 改前→改后 |")
        print("|---|---|---|---|---|---|")
        for n in names:
            g = [p for p in pairs if p[0]["name"] == n]
            def tot(col, side):
                return sum(num(p[side], col) for p in g)
            def med(col, side):
                v = sorted(num(p[side], col) for p in g)
                return v[len(v) // 2]
            print(f"| {n} | {len(g)} | {tot('freezes',0):.0f} → {tot('freezes',1):.0f} | "
                  f"{med('max_wait_ms',0):.0f} → {med('max_wait_ms',1):.0f} | "
                  f"{tot('rate_time_p5_kbps',0)/len(g):.0f} → {tot('rate_time_p5_kbps',1)/len(g):.0f} | "
                  f"{tot('rate_deep_cuts',0):.0f} → {tot('rate_deep_cuts',1):.0f} |")


if __name__ == "__main__":
    main()
