#!/usr/bin/env python3
"""M2b：把字节模型的残差拆成“序列之间”（偏差）和“序列之内”（帧间波动）两部分。

为什么：模型文件里的残差标准差 σ 是全部拟合行合在一起的，里面主要是结构误差（这一类内容、这个 QP 整段偏大或偏小），
不是帧与帧之间的随机波动。把它当成每帧独立的扰动（字节 × 2^(σZ)）会把同一段内容的总字节系统地抬高 2^(σ²·ln2/2) 倍
（v2 是 1.77 倍，v1 是 3.23 倍），还凭空多出“偶尔一个大帧”。扰动只该用序列之内的 σ（data-m2b.md 4.14 第 4 条）。

用法：scripts/m2b-residuals.py <测量输出目录（里面有 out-main 等，和 m2b-fit.py 同一个）> [报告输出.md]
依赖 numpy、scipy（经 m2b-fit.py）。只读测量数据和仓库里的两个模型文件，不拟合、不写模型文件。

口径：
- 残差 r = log2(实测字节 − 帧头) − log2(模型预测 − 帧头)，只算拟合集（split=fit）里进拟合的那些行（和 m2b-fit.py 一样）。
- 一个“序列” = 同一段画面按同一个 QP 连着编的一串帧（测量里的 seq 加 QP）。
- 序列之间 σ_b：各序列残差均值的标准差（按帧数加权）；序列之内 σ_w：每帧残差减去本序列均值之后的标准差。σ² = σ_b² + σ_w²。
- 核对：模型预测的是每帧的中位（在 log2 上做的最小二乘）。给每帧乘 2^(σ_w·Z) 之后，每个序列的总字节和实测比有没有系统偏差——
  解析算期望（Σ预测 × 2^(σ_w²·ln2/2)），再用固定种子抽样一遍核对。
"""
import importlib.util
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("m2bfit", os.path.join(HERE, "m2b-fit.py"))
fit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fit)

V2_FILE = os.path.join(HERE, "..", "crates", "media", "models", "openh264-v2.tsv")
LN2 = np.log(2.0)


def read_v2():
    theta, head = {}, None
    with open(V2_FILE, encoding="utf-8") as f:
        for line in f:
            p = line.rstrip("\n").split("\t")
            if p[0] == "参数":
                theta[p[1]] = float(p[2])
            elif p[0] == "帧头":
                head = int(p[1])
    return [theta[n] for n in fit.THETA_NAMES], head


def split(r, groups):
    """返回（总 σ、序列之间 σ_b、序列之内 σ_w、序列数、行数、序列内残差的 E[2^w]）。"""
    uniq, inv = np.unique(groups, return_inverse=True)
    n = np.bincount(inv)
    mean = np.bincount(inv, weights=r) / n
    within = r - mean[inv]
    sb = float(np.sqrt(np.average((mean - np.average(mean, weights=n)) ** 2, weights=n)))
    sw = float(np.sqrt(np.mean(within ** 2)))
    return float(np.std(r)), sb, sw, len(uniq), len(r), float(np.mean(np.exp2(within)))


def totals(pred, actual, groups, sw, seed=20261128):
    """每个序列的 Σ预测 ÷ Σ实测（log2）：不加扰动、加序列内扰动（解析期望、抽样一遍）。"""
    uniq, inv = np.unique(groups, return_inverse=True)
    sa = np.bincount(inv, weights=actual)
    sp = np.bincount(inv, weights=pred)
    rng = np.random.default_rng(seed)
    sj = np.bincount(inv, weights=pred * np.exp2(sw * rng.standard_normal(len(pred))))
    base = np.log2(sp / sa)
    lift = sw * sw * LN2 / 2  # log2 E[2^(σZ)]
    return base, base + lift, np.log2(sj / sa), sa


def q(a, p):
    return float(np.percentile(a, p))


def describe(name, r, pred, actual, groups, classes, emit):
    emit(f"### {name}")
    emit("")
    emit("| 类别 | 行数 | 序列数 | σ 总 | σ_b 序列之间 | σ_w 序列之内 | 序列内方差占比 | 序列内残差的 E[2^w]（实测） | 2^(σ_w²·ln2/2)（对数正态） |")
    emit("|---|---|---|---|---|---|---|---|---|")
    out = {}
    for c in ["全部"] + sorted(set(classes)):
        m = np.ones(len(r), bool) if c == "全部" else classes == c
        if m.sum() < 50:
            continue
        st, sb, sw, ng, nr, lift = split(r[m], groups[m])
        out[c] = (st, sb, sw)
        emit(f"| {c} | {nr} | {ng} | {st:.3f} | {sb:.3f} | {sw:.3f} | {sw * sw / max(st * st, 1e-12):.0%} | {lift:.3f} | {np.exp2(sw * sw * LN2 / 2):.3f} |")
    st, sb, sw = out["全部"]
    base, expect, sampled, sa = totals(pred, actual, groups, sw)
    emit("")
    emit(f"每个序列的总字节，模型 ÷ 实测（log2，0 表示相等；按序列数算分位，再按实测字节加权算全部序列合计）。扰动用全部类别合在一起的 σ_w = {sw:.3f}：")
    emit("")
    emit("| 扰动 | 中位 | 25% | 75% | 全部序列合计 Σ模型 ÷ Σ实测 |")
    emit("|---|---|---|---|---|")
    tot_a = sa.sum()
    for label, v in (("关", base), ("序列内 σ_w（解析期望）", expect), ("序列内 σ_w（抽样一遍）", sampled),
                     ("现在的全 σ（解析期望）", base + st * st * LN2 / 2)):
        ratio = float(np.sum(np.exp2(v) * sa) / tot_a)
        emit(f"| {label} | {q(v, 50):+.3f} | {q(v, 25):+.3f} | {q(v, 75):+.3f} | {ratio:.3f} |")
    emit("")
    return out


def main():
    root = sys.argv[1]
    lines = []

    def emit(s=""):
        lines.append(s)
        print(s)

    d = fit.derive(fit.load(root))
    theta, head = read_v2()
    fit_rows = d["split"] == "fit"
    emit("# 字节模型的残差：序列之间和序列之内（拟合集）")
    emit("")
    emit("残差 r = log2(实测 − 帧头) − log2(预测 − 帧头)。序列 = 同一段画面按同一个 QP 连着编的一串帧。")
    emit("")

    # ---- v2（F3）：和 m2b-fit.py 的 F3.fit 用同样的行
    tail = (d["class"] == "floorhold") & (d["frame"] >= 2) & (d["frame"] <= 4)
    mm = fit_rows & (~d["key"]) & (d["class"] != "floordesc") & (d["frame"] > 0) & (~tail)
    s = fit.sub(d, mm)
    s = fit.sub(s, (s["bytes"] - head) >= 2)
    form = fit.F3(head)
    form.theta = np.array(theta)
    body = np.maximum(form.body(s), 1e-3)
    actual_body = (s["bytes"] - head).astype(float)
    r = np.log2(actual_body) - np.log2(body)
    groups = np.char.add(np.char.add(s["seq"], "|"), s["qp"].astype(str))
    v2 = describe("openh264-v2（F3，模型文件里的系数）", r, body + head, s["bytes"].astype(float), groups, s["class"], emit)

    # ---- v1-any：main 轮六类内容的普通帧（M2 的 any 行就是这六类合在一起拟合的；指针、打字的变化面积常常不到千分之一，也算）
    mm = fit_rows & (~d["key"]) & (d["frame"] > 0) & (d["round"] == "main") & np.isin(d["class"], fit.NORMAL_CLASSES)
    s = fit.sub(d, mm)
    h1 = fit.V1[("any", "normal")]["head"]
    s = fit.sub(s, (s["bytes"] - h1) >= 2)
    pred = np.asarray(fit.v1_pred(s), float)
    body = np.maximum(pred - h1, 1e-3)
    r = np.log2((s["bytes"] - h1).astype(float)) - np.log2(body)
    groups = np.char.add(np.char.add(s["seq"], "|"), s["qp"].astype(str))
    v1 = describe("openh264-v1（各类合在一起的 any 行；数据是 M2b 重测的 main 轮）", r, pred, s["bytes"].astype(float), groups, s["class"], emit)

    emit("## 写进模型的数")
    emit("")
    emit(f"- openh264-v2：序列内残差标准差 {v2['全部'][2]:.6f}（log2）；全部残差 {v2['全部'][0]:.6f}，序列之间 {v2['全部'][1]:.6f}")
    emit(f"- openh264-v1（any）：序列内残差标准差 {v1['全部'][2]:.6f}（log2）；全部残差 {v1['全部'][0]:.6f}，序列之间 {v1['全部'][1]:.6f}")
    if len(sys.argv) > 2:
        with open(sys.argv[2], "w", encoding="utf-8") as f:
            f.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
