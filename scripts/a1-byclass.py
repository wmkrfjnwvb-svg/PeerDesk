#!/usr/bin/env python3
"""A1 S5-3：按帧类别拆字节的汇总（读 crates/e2e/examples/byclass.rs 的 CSV）。

第一个 CSV 是基准（L1，真编码器），其余各档和它按 `session` 列对上，只比两边都跑成功的会话。
输出：
  1. 三类帧（变化、补清晰、关键帧）的个数、字节合计、平均每帧字节，各档 ÷ 基准；
  2. 补清晰链：条数、平均每条几帧、平均每条多少字节、链上第 k 步的平均字节，各档 ÷ 基准；
  3. 按场景名分组的同样对比，挑总字节差得最大的 N 个（`--top`，默认 10）。
只用标准库。

用法：scripts/a1-byclass.py [--top N] 基准.csv 对比1.csv [对比2.csv ...]
"""
import csv
import math
import sys
from collections import defaultdict

CLASSES = [("content", "变化"), ("refine", "补清晰"), ("key", "关键帧")]
STEPS = ["step1", "step2", "step3", "step4", "step5", "step6p"]


def load(path):
    rows = {}
    with open(path, newline="", encoding="utf-8") as f:
        for r in csv.DictReader(f):
            if r.get("error"):
                print(f"跳过出错的一行（{path}）：{r.get('world_hash')} {r['error']}", file=sys.stderr)
                continue
            rows[r["session"]] = r
    return rows


def label(path, rows):
    r = next(iter(rows.values()), {})
    return f"{r.get('codec', '?')}/{r.get('byte_model', '?')}/{r.get('byte_noise', '?')}"


def tot(rows, keys, col):
    return sum(float(rows[k][col]) for k in keys)


def ratio(a, b):
    return a / b if b else float("nan")


def fmt(x, nd=2):
    return "-" if x != x else f"{x:.{nd}f}"


def summary(rows, keys):
    """一批会话的合计量。"""
    s = {}
    for c, _ in CLASSES:
        n, b = tot(rows, keys, f"{c}_n"), tot(rows, keys, f"{c}_bytes")
        s[c] = (n, b, ratio(b, n))
        n, b = tot(rows, keys, f"idle_{c}_n"), tot(rows, keys, f"idle_{c}_bytes")
        s["idle_" + c] = (n, b, ratio(b, n))
    chains = tot(rows, keys, "chains")
    s["chains"] = chains
    s["chain_frames"] = tot(rows, keys, "refine_n")
    s["chain_bytes"] = tot(rows, keys, "refine_bytes")
    for k in STEPS:
        n, b = tot(rows, keys, f"{k}_n"), tot(rows, keys, f"{k}_bytes")
        s[k] = (n, b, ratio(b, n))
    s["dropped"] = tot(rows, keys, "dropped")
    s["nooutput"] = tot(rows, keys, "nooutput")
    s["total_n"] = sum(s[c][0] for c, _ in CLASSES)
    s["total_b"] = sum(s[c][1] for c, _ in CLASSES)
    return s


def pooled_chain_pcts(rows, keys, col):
    """各会话链的中位/90% 分位的中位数（会话内的分位已在 CSV 里）。"""
    v = sorted(float(rows[k][col]) for k in keys if float(rows[k]["chains"]) > 0)
    return v[len(v) // 2] if v else float("nan")


def table_classes(base, others):
    print("\n== 1. 三类帧（全部会话合计）==")
    print(f"{'类别':<8}{'档':<28}{'帧数':>10}{'字节合计':>14}{'每帧字节':>10}{'帧数÷L1':>9}{'字节÷L1':>9}{'每帧÷L1':>9}")
    for c, name in CLASSES + [("idle_content", "停下:变化"), ("idle_refine", "停下:补清晰"), ("idle_key", "停下:关键帧")]:
        b = base["s"][c]
        for tag, o in [(base["label"], base)] + [(x["label"], x) for x in others]:
            n, by, per = o["s"][c]
            r = ("", "", "") if o is base else (fmt(ratio(n, b[0])), fmt(ratio(by, b[1])), fmt(ratio(per, b[2])))
            print(f"{name:<8}{tag:<28}{n:>10.0f}{by:>14.0f}{per:>10.0f}{r[0]:>9}{r[1]:>9}{r[2]:>9}")
    print("\n没出帧的任务：")
    for o in [base] + others:
        print(f"  {o['label']:<28} 编码器丢 {o['s']['dropped']:.0f}，没有要发的 {o['s']['nooutput']:.0f}；总帧 {o['s']['total_n']:.0f}，总字节 {o['s']['total_b']:.0f}")


def table_chains(base, others):
    print("\n== 2. 补清晰链（一个非补清晰帧之后连续的补清晰帧）==")
    print(f"{'档':<28}{'条数':>8}{'均长(帧)':>10}{'每条字节':>10}{'链长p50':>9}{'链长p90':>9}{'链字节p50':>11}{'链字节p90':>11}{'条数÷L1':>9}{'均长÷L1':>9}{'每条÷L1':>9}")
    b = base["s"]
    bl, bb = ratio(b["chain_frames"], b["chains"]), ratio(b["chain_bytes"], b["chains"])
    for o in [base] + others:
        s = o["s"]
        ln, by = ratio(s["chain_frames"], s["chains"]), ratio(s["chain_bytes"], s["chains"])
        p = [pooled_chain_pcts(o["rows"], o["keys"], c) for c in ("chain_len_p50", "chain_len_p90", "chain_bytes_p50", "chain_bytes_p90")]
        r = ("", "", "") if o is base else (fmt(ratio(s["chains"], b["chains"])), fmt(ratio(ln, bl)), fmt(ratio(by, bb)))
        print(f"{o['label']:<28}{s['chains']:>8.0f}{ln:>10.2f}{by:>10.0f}{p[0]:>9.0f}{p[1]:>9.0f}{p[2]:>11.0f}{p[3]:>11.0f}{r[0]:>9}{r[1]:>9}{r[2]:>9}")
    print("（链长、链字节的 p50/p90 是各会话自己的分位再取中位数）")
    print("\n链上第 k 步（第 6 步含以后）：该步的帧数 / 平均字节 / 模型÷L1")
    print(f"{'步':<8}" + "".join(f"{o['label']:>34}" for o in [base] + others))
    for k in STEPS:
        line = f"{k:<8}"
        for o in [base] + others:
            n, _, per = o["s"][k]
            r = "" if o is base else f" ×{fmt(ratio(per, base['s'][k][2]))}"
            line += f"{n:>10.0f} {per:>10.0f}{r:>13}"
        print(line)


def table_names(base, others, top):
    print(f"\n== 3. 按场景名分组，总字节差最大的 {top} 个（每个对比档各一张）==")
    groups = defaultdict(list)
    for k in base["keys"]:
        groups[base["rows"][k]["name"]].append(k)
    for o in others:
        print(f"\n-- {o['label']} ÷ L1 --")
        scored = []
        for name, keys in groups.items():
            ks = [k for k in keys if k in o["rows"]]
            if not ks:
                continue
            sb, so = summary(base["rows"], ks), summary(o["rows"], ks)
            if sb["total_b"] <= 0 or so["total_b"] <= 0:
                continue
            scored.append((abs(math.log(so["total_b"] / sb["total_b"])), name, len(ks), sb, so))
        scored.sort(reverse=True)
        print(f"{'场景':<46}{'会话':>5}{'总字节L1':>11}{'÷L1':>6} | {'变化字节÷':>9}{'补清晰字节÷':>11}{'关键帧字节÷':>11} | {'变化帧÷':>7}{'补清晰帧÷':>9}{'每帧÷':>6}")
        for _, name, n, sb, so in scored[:top]:
            print(
                f"{name[:45]:<46}{n:>5}{sb['total_b']:>11.0f}{fmt(ratio(so['total_b'], sb['total_b'])):>6} | "
                f"{fmt(ratio(so['content'][1], sb['content'][1])):>9}{fmt(ratio(so['refine'][1], sb['refine'][1])):>11}{fmt(ratio(so['key'][1], sb['key'][1])):>11} | "
                f"{fmt(ratio(so['content'][0], sb['content'][0])):>7}{fmt(ratio(so['refine'][0], sb['refine'][0])):>9}"
                f"{fmt(ratio(so['total_b'] / so['total_n'], sb['total_b'] / sb['total_n'])):>6}"
            )
        ratios = sorted(so["total_b"] / sb["total_b"] for _, _, _, sb, so in scored)
        if ratios:
            print(f"（{len(ratios)} 个场景名，总字节比 最小 {ratios[0]:.2f}，中位 {ratios[len(ratios) // 2]:.2f}，最大 {ratios[-1]:.2f}）")


def main():
    args = sys.argv[1:]
    top = 10
    if args[:1] == ["--top"]:
        top, args = int(args[1]), args[2:]
    if len(args) < 2:
        print(__doc__)
        return 2
    loaded = [(p, load(p)) for p in args]
    common = set(loaded[0][1])
    for _, rows in loaded[1:]:
        common &= set(rows)
    keys = sorted(common)
    print(f"共同的会话 {len(keys)} 个（基准 {len(loaded[0][1])} 个）")
    runs = []
    for p, rows in loaded:
        runs.append({"label": label(p, rows), "rows": rows, "keys": keys, "s": summary(rows, keys)})
    table_classes(runs[0], runs[1:])
    table_chains(runs[0], runs[1:])
    table_names(runs[0], runs[1:], top)
    return 0


if __name__ == "__main__":
    sys.exit(main())
