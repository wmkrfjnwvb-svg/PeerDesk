#!/usr/bin/env python3
"""A1 第五步：三档对账的汇总（docs/research/Q-redesign/a1-plan.md 第 4 节）。

读批量运行器（crates/e2e/examples/batch.rs）的 CSV，同一个会话（`session` 列）在各档的结果两两对上，逐项给出分布和差。
基准是 L1（codec=h264）。其余的档按（编码档, 字节模型, 扰动）分组，每组一张表。只用标准库。

用法：scripts/a1-reconcile.py [--speed] <batch 的 CSV>...
  --speed  另出速度表（仿真秒数 ÷ 墙上秒数；只有用 --jobs 1 跑出来的 CSV 才有意义），按办公、视频两组。

容差（第 4 节）：两秒窗口的字节误差中位 ≤15%、90% 分位 ≤30%；帧数差 ≤5%；冻结、降码率、换档、探测这类计数只看方向。
窗口误差的口径：每个会话 Σ|本档窗口字节 − L1 窗口字节| ÷ Σ L1 窗口字节（按字节加权，和 M2 的判据 2 同一个定义）。
会话在某个时刻走岔之后单次轨迹没有可比性，所以看的是一批会话上的分布。
"""
import csv
import sys
from collections import defaultdict

STATES = None  # 码率状态的列名，从表头里取
WINDOW_MEDIAN, WINDOW_P90, FRAMES = 0.15, 0.30, 0.05


def load(paths):
    rows = []
    for path in paths:
        with open(path, newline="", encoding="utf-8") as f:
            for r in csv.DictReader(f):
                if r.get("error"):
                    print(f"跳过出错的一行：{r.get('world_hash')} {r['error']}", file=sys.stderr)
                    continue
                rows.append(r)
    return rows


def num(r, key):
    v = r.get(key, "")
    return float(v) if v not in ("", "-") else float("nan")


def pct(values, q):
    v = sorted(x for x in values if x == x)
    if not v:
        return float("nan")
    k = (len(v) - 1) * q / 100
    lo, hi = int(k), min(int(k) + 1, len(v) - 1)
    return v[lo] + (v[hi] - v[lo]) * (k - lo)


def windows(r):
    text = r.get("window_bytes", "")
    return [int(x) for x in text.split(";")] if text else []


def window_error(a, base):
    wa, wb = windows(a), windows(base)
    n = max(len(wa), len(wb))
    wa += [0] * (n - len(wa))
    wb += [0] * (n - len(wb))
    total = sum(wb)
    return sum(abs(x - y) for x, y in zip(wa, wb)) / total if total else float("nan")


def rel(a, b):
    return (a - b) / b if b else (0.0 if a == 0 else float("nan"))


def fmt_pct(x):
    return "-" if x != x else f"{x:+.0%}"


def verdict(ok):
    return "过" if ok else "**没过**"


def kind(name):
    return "视频" if "video" in name else "办公"


def config(r):
    return (r["codec"], r["byte_model"], r.get("byte_noise", "default"))


def table(label, pairs, emit):
    """pairs：[(本档的行, L1 的行)]。"""
    n = len(pairs)
    emit(f"### {label}（{n} 个会话）")
    emit("")
    we = [window_error(a, b) for a, b in pairs]
    by = [rel(num(a, "sent_bytes"), num(b, "sent_bytes")) for a, b in pairs]
    fr = [rel(num(a, "frames"), num(b, "frames")) for a, b in pairs]
    med, p90 = pct(we, 50), pct(we, 90)
    fmed = pct([abs(x) for x in fr], 50)
    emit("| 量 | 这一档 | L1 | 差 | 容差 | 结论 |")
    emit("|---|---|---|---|---|---|")
    emit(f"| 两秒窗口字节误差：中位 | {med:.0%} | – | – | ≤{WINDOW_MEDIAN:.0%} | {verdict(med <= WINDOW_MEDIAN)} |")
    emit(f"| 两秒窗口字节误差：90% 分位 | {p90:.0%} | – | – | ≤{WINDOW_P90:.0%} | {verdict(p90 <= WINDOW_P90)} |")
    emit(f"| 总字节（每个会话的相对差）：中位 / 25% / 75% | {fmt_pct(pct(by, 50))} / {fmt_pct(pct(by, 25))} / {fmt_pct(pct(by, 75))} | – | – | 只报告 | |")
    sa, sb = sum(num(a, "sent_bytes") for a, _ in pairs), sum(num(b, "sent_bytes") for _, b in pairs)
    emit(f"| 总字节（全部会话合计，MB） | {sa / 1e6:.1f} | {sb / 1e6:.1f} | {fmt_pct(rel(sa, sb))} | 只报告 | |")
    emit(f"| 帧数（每个会话相对差的绝对值）：中位 / 90% 分位 | {fmed:.1%} / {pct([abs(x) for x in fr], 90):.1%} | – | – | 中位 ≤{FRAMES:.0%} | {verdict(fmed <= FRAMES)} |")

    def both(key, title, digits=0, agg=sum, note="方向"):
        a = agg([num(x, key) for x, _ in pairs])
        b = agg([num(y, key) for _, y in pairs])
        emit(f"| {title} | {a:.{digits}f} | {b:.{digits}f} | {fmt_pct(rel(a, b))} | {note} | |")

    both("frames", "帧数合计")
    both("keyframes", "关键帧合计")
    both("freezes", "冻结次数合计")
    differ = sum(1 for a, b in pairs if num(a, "freezes") != num(b, "freezes"))
    emit(f"| 冻结次数不同的会话 | {differ} / {n} | – | – | 只报告 | |")
    med_of = lambda v: pct(v, 50)
    p90_of = lambda v: pct(v, 90)
    both("max_wait_ms", "最长等待（ms）：各会话的中位", 0, med_of, "只报告")
    both("max_wait_ms", "最长等待（ms）：各会话的 90% 分位", 0, p90_of, "只报告")
    both("first_picture_ms", "第一幅画面（ms）：中位", 0, med_of, "只报告")
    both("rate_time_p50_kbps", "码率中位（按时间，kbps）：各会话的中位", 0, med_of, "只报告")
    both("rate_time_p5_kbps", "码率 5% 分位（按时间，kbps）：各会话的中位", 0, med_of, "只报告")
    ratio = [num(a, "rate_time_p50_kbps") / num(b, "rate_time_p50_kbps") for a, b in pairs if num(b, "rate_time_p50_kbps") > 0]
    emit(f"| 码率中位，本档 ÷ L1：中位 / 10% / 90% | {pct(ratio, 50):.2f} / {pct(ratio, 10):.2f} / {pct(ratio, 90):.2f} | – | – | 只报告 | |")
    both("rate_cuts", "降码率次数合计")
    both("rate_deep_cuts", "其中降幅过半")
    both("fps_changes", "帧率换档次数合计")
    both("retransmitted", "重传分片合计")
    both("recoveries", "判丢恢复合计")
    both("skipped", "等关键帧跳过的帧合计")
    both("bad", "花屏（必须是 0）", note="必须是 0")
    for key, title in (("probe_verify_rounds", "核实探测轮数"), ("probe_periodic_rounds", "周期探测轮数"), ("probe_idle_sent", "静止探测组数"),
                       ("probe_bytes", "探测字节")):
        both(key, title + "合计")
    for col in VERDICTS:
        both(col, f"运行中探测结论 {col.removeprefix('probe_run_')} 合计")
    for col in STATES:
        a = sum(num(x, col) * num(x, "sim_s") for x, _ in pairs) / max(sum(num(x, "sim_s") for x, _ in pairs), 1e-9)
        b = sum(num(y, col) * num(y, "sim_s") for _, y in pairs) / max(sum(num(y, "sim_s") for _, y in pairs), 1e-9)
        emit(f"| 状态时间占比 {col.removeprefix('state_').removesuffix('_permille')}（‰，按时长加权） | {a:.0f} | {b:.0f} | {a - b:+.0f}‰ | 只报告 | |")
    sharp = lambda r: [float(x) for x in r.get("sharp_ms", "").split(";") if x not in ("", "-")]
    sa_, sb_ = [x for a, _ in pairs for x in sharp(a)], [x for _, b in pairs for x in sharp(b)]
    emit(f"| 停下后变清晰（ms）：次数 / 中位 | {len(sa_)} / {pct(sa_, 50):.0f} | {len(sb_)} / {pct(sb_, 50):.0f} | – | 只报告（L0、L0.5 的清晰是按 QP 的代理） | |")
    emit("")
    worst = sorted(zip(we, pairs), key=lambda t: -(t[0] if t[0] == t[0] else -1))[:5]
    emit("窗口误差最大的 5 个会话：" + "；".join(f"{a['name']}（种子 {a.get('seed', '?')}）{e:.0%}" for e, (a, _) in worst))
    emit("")


def speed(rows, emit):
    emit("## 速度（仿真秒数 ÷ 墙上秒数；单线程跑的才有意义）")
    emit("")
    emit("| 档 | 字节模型 | 扰动 | 组 | 会话 | 仿真（秒） | 墙上（秒） | 倍实时 | 各会话倍数的中位 / 最慢 |")
    emit("|---|---|---|---|---|---|---|---|---|")
    groups = defaultdict(list)
    for r in rows:
        groups[config(r) + (kind(r["name"]),)].append(r)
    for key in sorted(groups):
        g = groups[key]
        sim = sum(num(r, "sim_s") for r in g)
        wall = sum(num(r, "wall_ms") for r in g) / 1000
        each = [num(r, "sim_s") / max(num(r, "wall_ms") / 1000, 1e-9) for r in g]
        emit(f"| {key[0]} | {key[1]} | {key[2]} | {key[3]} | {len(g)} | {sim:.0f} | {wall:.2f} | {sim / max(wall, 1e-9):.1f} | {pct(each, 50):.1f} / {min(each):.1f} |")
    emit("")


def main():
    global STATES, VERDICTS
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if not args:
        sys.exit(__doc__)
    rows = load(args)
    if not rows:
        sys.exit("没有可用的行")
    cols = list(rows[0].keys())
    STATES = [c for c in cols if c.startswith("state_")]
    VERDICTS = [c for c in cols if c.startswith("probe_run_") and c != "probe_run_sent"]
    out = []

    def emit(s=""):
        out.append(s)

    by = defaultdict(dict)
    for r in rows:
        if r["codec"] == "tile":
            continue
        cfg = config(r)
        if r["session"] in by[cfg]:
            print(f"同一个会话在 {cfg} 里出现两次，用后一次：{r['session']} {r['name']}", file=sys.stderr)
        by[cfg][r["session"]] = r
    base = by.get(("h264", "default", "default"), {})
    emit("# A1 三档对账")
    emit("")
    emit(f"会话数：L1 {len(base)}；" + "；".join(f"{'/'.join(c)} {len(v)}" for c, v in sorted(by.items()) if c[0] != "h264"))
    emit("")
    if not base:
        emit("没有 L1（codec=h264）的行，没法对账。")
    emit("## 各档对 L1")
    emit("")
    for cfg in sorted(c for c in by if c[0] != "h264"):
        pairs = [(r, base[s]) for s, r in by[cfg].items() if s in base]
        missing = len(by[cfg]) - len(pairs)
        if missing:
            emit(f"（{'/'.join(cfg)}：{missing} 个会话在 L1 里找不到，没有比）")
        if pairs:
            table(f"{cfg[0]}，字节模型 {cfg[1]}，扰动 {cfg[2]}", pairs, emit)
    # L0 对 L0.5：同一份内容的两种表示，应当一模一样
    emit("## L0 对 L0.5（同一个字节模型、同一种扰动；应当一模一样）")
    emit("")
    emit("| 字节模型 | 扰动 | 会话 | 总字节、帧数、关键帧、冻结、各窗口字节全同的 | 不同的会话 |")
    emit("|---|---|---|---|---|")
    for cfg in sorted(c for c in by if c[0] == "h264-l0"):
        other = by.get(("h264-l05", cfg[1], cfg[2]), {})
        same, diff = 0, []
        for s, r in by[cfg].items():
            if s not in other:
                continue
            o = other[s]
            if all(r[k] == o[k] for k in ("sent_bytes", "frames", "keyframes", "freezes", "window_bytes", "rate_cuts", "fps_changes")):
                same += 1
            else:
                diff.append(r["name"])
        emit(f"| {cfg[1]} | {cfg[2]} | {same + len(diff)} | {same} | {'、'.join(sorted(set(diff))[:8]) or '无'} |")
    emit("")
    if "--speed" in sys.argv:
        speed(rows, emit)
    print("\n".join(out))


if __name__ == "__main__":
    main()
