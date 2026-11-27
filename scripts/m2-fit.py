#!/usr/bin/env python3
"""M2 拟合和留出检验（形式、判据见 docs/research/Q-redesign/m2-m3-protocol.md 1.1、1.5 节）。

用法：scripts/m2-fit.py <测量输出目录（里面有 out-main、out-key、out-refine）> <模型文件输出路径> [--report]
只用 numpy 和标准库。只拿 split=fit 的行拟合；split=hold 的行只用来算误差，不参与任何选择。
"""
import csv
import glob
import os
import sys

import numpy as np

CLASSES = ["pointer", "typing", "scrolling", "dragging", "switching", "video", "fullvideo"]
# fullvideo 的每一帧后端都自己出了关键帧（见报告），没有普通帧可拟合
NORMAL_CLASSES = [c for c in CLASSES if c != "fullvideo"]
POOL = "any"
SCENE_STEP = 50


def load(root):
    rows = []
    for rnd in ("main", "key", "refine", "scene2"):
        for path in sorted(glob.glob(os.path.join(root, f"out-{rnd}", "shard-*.csv"))):
            with open(path) as f:
                for r in csv.DictReader(f):
                    r["round"] = rnd
                    rows.append(r)
    n = len(rows)
    d = {
        "round": np.array([r["round"] for r in rows]),
        "seq": np.array([r["seq"] for r in rows]),
        "cls": np.array([r["class"] for r in rows]),
        "split": np.array([r["split"] for r in rows]),
        "w": np.array([int(r["w"]) for r in rows]),
        "h": np.array([int(r["h"]) for r in rows]),
        "frame": np.array([int(r["frame"]) for r in rows]),
        "qp": np.array([int(r["qp"]) for r in rows]),
        "asked": np.array([int(r["asked_key"]) for r in rows]),
        "key": np.array([int(r["got_key"]) for r in rows]),
        "bytes": np.array([int(r["bytes"]) for r in rows]),
        "chg": np.array([int(r["changed_permille"]) for r in rows]),
        "det": np.array([int(r["detail_permille"]) for r in rows]),
        "prev": np.array([-1 if r["prev_qp"] == "-" else int(r["prev_qp"]) for r in rows]),
        "us": np.array([int(r["enc_us"]) for r in rows]),
    }
    d["pix"] = d["w"] * d["h"]
    d["real"] = np.array([s.startswith("real-") for s in d["seq"]])
    print(f"读入 {n} 行", file=sys.stderr)
    return d


def lg(x):
    return np.log2(np.maximum(x, 1))


class Fit:
    """log2(字节 - 帧头) = a + b·QP + c·log2(面积) + d·log2(细节) [+ e·(上一帧QP - QP)]"""

    def __init__(self, area_total, with_e=False, extras=()):
        self.area_total = area_total
        self.with_e = with_e
        self.extras = extras

    def design(self, d, m):
        area = d["pix"][m] if self.area_total else np.maximum(d["pix"][m] * d["chg"][m] // 1000, 1)
        cols = [np.ones(m.sum()), d["qp"][m].astype(float), lg(area), lg(d["det"][m])]
        if self.with_e:
            cols.append((d["prev"][m] - d["qp"][m]).astype(float))
        for name in self.extras:  # 只在 --explore 里用：形式不够时的候选改法，不进模型文件
            q = d["qp"][m].astype(float)
            if name == "qp2":
                cols.append((q - 30) ** 2 / 100)
            elif name == "low":
                cols.append((q <= 17).astype(float))
            elif name == "res":
                cols.append(lg(d["pix"][m]))
        return np.stack(cols, axis=1)

    def fit(self, d, m, head):
        body = d["bytes"][m] - head
        ok = body >= 2  # 帧头之外几乎没有内容的帧（跳过帧）不进拟合，单独报告个数
        x = self.design(d, m)[ok]
        y = np.log2(body[ok])
        keep = [i for i in range(x.shape[1]) if i == 0 or x[:, i].std() > 1e-9]
        beta = np.zeros(x.shape[1])
        sol, *_ = np.linalg.lstsq(x[:, keep], y, rcond=None)
        beta[keep] = sol
        res = y - x @ beta
        self.head, self.beta = head, beta
        self.extra_beta = beta[(5 if self.with_e else 4):]
        self.sigma = float(res.std())
        self.n = int(ok.sum())
        self.skipped = int((~ok).sum())
        return self

    def predict(self, d, m):
        """预测的总字节数（含帧头）。"""
        x = self.design(d, m)
        return self.head + np.exp2(x @ self.beta)


STEP = [1000, 1122, 1260, 1414, 1587, 1782]


def placeholder(pix, chg, det, qp, key):
    """crates/media/src/fake.rs 的 DefaultByteModel（占位）的逐行移植，整数运算。"""
    out = np.zeros(len(pix))
    for i in range(len(pix)):
        q = min(int(qp[i]), 51)
        divisor = STEP[q % 6] << (q // 6)
        area = 1000 if key[i] else min(int(chg[i]), 1000)
        detail = 500 + min(int(det[i]), 1000)
        milli = int(pix[i]) * 1400 * detail * area // 1_000_000
        body = milli * 1000 // divisor // 1000
        out[i] = 24 + (40 if key[i] else 0) + body
    return out


def log_err(pred, actual):
    return np.abs(np.log2(np.maximum(pred, 1) / np.maximum(actual, 1)))


def pct(a, q):
    return float(np.percentile(a, q)) if len(a) else float("nan")


def main():
    root, out = sys.argv[1], sys.argv[2]
    d = load(root)
    is_fit = d["split"] == "fit"
    first = d["frame"] == 0
    # 关键帧：第一帧、强制的、场景切换自己出的（不含补清晰链和场景任务里的“换回来”帧，它们都是普通帧）
    key = (d["key"] == 1)
    normal = ~key & (d["frame"] > 0)
    refine = (d["cls"] == "refine") & (d["frame"] > 0)
    scene = d["cls"] == "scene"
    seqcls = np.isin(d["cls"], CLASSES)

    # 帧头：各种帧里最小的字节数（取不出内容的固定开销），按种类各取
    head_n = int(d["bytes"][normal & (d["chg"] == 0) & ~refine].min())
    head_k = 40  # SPS+PPS+片头约 40 字节（估计）；关键帧最小也有万字节以上，取多少都不影响（< 0.4%）
    print(f"帧头：普通 {head_n}，关键 {head_k}", file=sys.stderr)

    models = {}
    # 普通帧：每类一个，加各类合在一起的
    for c in NORMAL_CLASSES + [POOL]:
        m = normal & seqcls & is_fit & ((d["cls"] == c) if c != POOL else True) & ~d["real"]
        models[(c, "normal")] = Fit(False).fit(d, m, head_n)
    models[("any", "key")] = Fit(True).fit(d, key & is_fit & ~d["real"] & (d["cls"] != "scene"), head_k)
    models[("refine", "normal")] = Fit(True, True).fit(d, refine & is_fit, head_n)

    # 场景切换门槛：非第一帧、没要关键帧的帧里，变化面积大到哪一档起后端自己出关键帧
    cand = (d["frame"] > 0) & (d["asked"] == 0) & ~refine
    edges = list(range(0, 1001, SCENE_STEP))
    scene_table = []
    for lo in edges:
        sel = cand & (d["chg"] >= lo) & (d["chg"] < lo + SCENE_STEP + (1 if lo + SCENE_STEP >= 1000 else 0))
        if sel.sum():
            scene_table.append((lo, int(sel.sum()), float(d["key"][sel].mean())))
    thr = 1001
    for lo, n, frac in scene_table:
        if frac >= 0.5 and all(f >= 0.5 for l, _, f in scene_table if l >= lo):
            thr = lo
            break
    # 从没有哪档过半：用“出现自动关键帧的最小变化面积”之上的第一个档作参考，门槛按 1000 + 1（假后端里等于不触发）
    auto = cand & (d["key"] == 1)
    auto_min = int(d["chg"][auto].min()) if auto.any() else None

    def rf_err_all(m):
        return log_err(models[("refine", "normal")].predict(d, m), d["bytes"][m])

    # ---- 误差 ----
    report = {}

    def per_frame(c, mask_split):
        m = normal & seqcls & mask_split & ((d["cls"] == c) if c != POOL else True)
        mdl = models[(c, "normal")]
        return log_err(mdl.predict(d, m), d["bytes"][m]), m

    lines = []

    def emit(s=""):
        lines.append(s)
        print(s)

    emit("## 每类的系数（普通帧：log2(字节 - 帧头) = a + b·QP + c·log2(变化像素数) + d·log2(细节)）")
    emit(f"帧头：普通 {head_n}，关键 {head_k}")
    emit("| 类别 | 帧种类 | a | b | c | d | e | 残差σ | 拟合样本 | 跳过 |")
    emit("|---|---|---|---|---|---|---|---|---|---|")
    for (c, k), mdl in models.items():
        b = mdl.beta
        e = b[4] if len(b) > 4 else 0.0
        emit(f"| {c} | {k} | {b[0]:.4f} | {b[1]:.4f} | {b[2]:.4f} | {b[3]:.4f} | {e:.4f} | {mdl.sigma:.3f} | {mdl.n} | {mdl.skipped} |")

    emit("")
    emit("## 单帧误差 |log2(预测/实测)|（普通帧，中位 / 90% 分位；目标 ≤ 0.5 / ≤ 1.0）")
    emit("| 类别 | 拟合集 n | 中位 | 90% | 留出集 n | 中位 | 90% | 留出中位/拟合中位 |")
    emit("|---|---|---|---|---|---|---|---|")
    hold_med = {}
    for c in NORMAL_CLASSES + [POOL]:
        ef, mf = per_frame(c, is_fit & ~d["real"])
        eh, mh = per_frame(c, ~is_fit & ~d["real"])
        hold_med[(c, "normal")] = float(np.median(eh)) if len(eh) else float("nan")
        emit(f"| {c} | {len(ef)} | {pct(ef,50):.3f} | {pct(ef,90):.3f} | {len(eh)} | {pct(eh,50):.3f} | {pct(eh,90):.3f} | {pct(eh,50)/max(pct(ef,50),1e-9):.2f} |")
    # 分类用的系数 vs 各类合在一起的系数：同一批留出帧
    emit("")
    emit("各类合在一起的系数（any）在各类留出帧上的误差（假后端没有“类别”输入时只能这样）：")
    emit("| 类别 | 留出 n | 中位 | 90% |")
    emit("|---|---|---|---|")
    for c in NORMAL_CLASSES:
        m = normal & seqcls & ~is_fit & ~d["real"] & (d["cls"] == c)
        e = log_err(models[(POOL, "normal")].predict(d, m), d["bytes"][m])
        emit(f"| {c} | {int(m.sum())} | {pct(e,50):.3f} | {pct(e,90):.3f} |")
    emit("")
    emit("真字体页面（全部留出）：")
    emit("| 序列类别 | 用哪套系数 | n | 中位 | 90% | 实测/预测 的中位比 |")
    emit("|---|---|---|---|---|---|")
    for c in ("typing", "scrolling"):
        for use in (c, POOL):
            m = normal & d["real"] & (d["cls"] == c)
            p = models[(use, "normal")].predict(d, m)
            e = log_err(p, d["bytes"][m])
            emit(f"| {c} | {use} | {int(m.sum())} | {pct(e,50):.3f} | {pct(e,90):.3f} | {float(np.median(d['bytes'][m]/p)):.2f} |")

    # 窗口：每个（序列, QP）的第 1 帧到最后一帧（60 帧约 2 秒，30 fps）的字节总和；半窗（30 帧）也报
    emit("")
    emit("## 窗口字节总和的相对误差 |预测/实测 - 1|（窗口 = 59 个非首帧，约 2 秒；半窗 = 30 帧；中位 / 90% 分位；目标 ≤ 15% / ≤ 30%）")
    emit("预测时，变化面积 ≥ 场景门槛的帧按关键帧算（和假后端一样）。")
    emit("| 类别 | 集 | 窗口数 | 2 秒 中位 | 2 秒 90% | 1 秒 中位 | 1 秒 90% |")
    emit("|---|---|---|---|---|---|---|")
    mainrows = (d["round"] == "main") & seqcls
    seqs = {}
    for i in np.nonzero(mainrows & (d["frame"] > 0))[0]:
        seqs.setdefault((d["seq"][i], d["qp"][i]), []).append(i)
    thr_pred = thr if thr <= 1000 else 10_000
    key_model, any_model = models[("any", "key")], models[(POOL, "normal")]
    win_err = {}
    for (name, qp), idx in seqs.items():
        idx = np.array(sorted(idx, key=lambda i: d["frame"][i]))
        cls = d["cls"][idx[0]]
        mdl = models.get((cls, "normal"), models[(POOL, "normal")])
        mask = np.zeros(len(d["frame"]), bool)
        mask[idx] = True
        p = mdl.predict(d, mask)
        pk = key_model.predict(d, mask)
        isk = d["chg"][idx] >= thr_pred
        p = np.where(isk, pk, p)
        actual = d["bytes"][idx]
        for tag, sl in (("2s", slice(None)), ("1s", slice(0, 30))):
            e = abs(p[sl].sum() / actual[sl].sum() - 1)
            sp = "real" if d["real"][idx[0]] else ("fit" if is_fit[idx[0]] else "hold")
            win_err.setdefault((cls, sp, tag), []).append(e)
            win_err.setdefault(("ALL", sp, tag), []).append(e)
    for cls in CLASSES + ["ALL"]:
        for sp in ("fit", "hold", "real"):
            a, b = win_err.get((cls, sp, "2s"), []), win_err.get((cls, sp, "1s"), [])
            if a:
                emit(f"| {cls} | {sp} | {len(a)} | {pct(a,50):.1%} | {pct(a,90):.1%} | {pct(b,50):.1%} | {pct(b,90):.1%} |")

    # 判据 4：关键帧 / 普通帧比值；补清晰一步
    emit("")
    emit("## 关键帧对普通帧的比值（首帧字节 ÷ 后 59 帧平均）和补清晰一步的大小（留出集）")
    ratios = {"hold": [], "fit": []}
    for (name, qp), idx in seqs.items():
        cls = d["cls"][idx[0]]
        if d["real"][idx[0]] or cls == "fullvideo":
            continue
        i0 = np.nonzero((d["seq"] == name) & (d["qp"] == qp) & (d["frame"] == 0) & mainrows)[0]
        if not len(i0):
            continue
        mask0 = np.zeros(len(d["frame"]), bool)
        mask0[i0] = True
        pk = key_model.predict(d, mask0)[0]
        mask = np.zeros(len(d["frame"]), bool)
        mask[idx] = True
        pn = models[(cls, "normal")].predict(d, mask).mean()
        an = d["bytes"][idx].mean()
        ratios["fit" if is_fit[idx[0]] else "hold"].append(abs((pk / pn) / (d["bytes"][i0[0]] / an) - 1))
    for sp in ("fit", "hold"):
        emit(f"- 关键/普通比值相对误差（{sp}，{len(ratios[sp])} 个序列×QP）：中位 {pct(ratios[sp],50):.1%}，90% {pct(ratios[sp],90):.1%}")
    kf, kh = key & ~is_fit & (d["cls"] != "scene") & ~d["real"], key & is_fit & (d["cls"] != "scene")
    for tag, m in (("拟合", kh), ("留出", kf)):
        e = log_err(key_model.predict(d, m), d["bytes"][m])
        er = np.abs(key_model.predict(d, m) / d["bytes"][m] - 1)
        emit(f"- 关键帧单帧（{tag}，n={int(m.sum())}）：|log2| 中位 {pct(e,50):.3f}，90% {pct(e,90):.3f}；相对误差中位 {pct(er,50):.1%}")
    for tag, m in (("拟合", refine & is_fit), ("留出", refine & ~is_fit)):
        p = models[("refine", "normal")].predict(d, m)
        er = np.abs(p / d["bytes"][m] - 1)
        e = log_err(p, d["bytes"][m])
        emit(f"- 补清晰一步（{tag}，n={int(m.sum())}）：相对误差中位 {pct(er,50):.1%}，90% {pct(er,90):.1%}；|log2| 中位 {pct(e,50):.3f}")

    # 判据 5
    emit("")
    emit("## 场景切换：变化面积（千分比）分档里后端自己出关键帧的比例")
    emit("| 变化面积档 | 帧数 | 出关键帧的比例 |")
    emit("|---|---|---|")
    for lo, n, frac in scene_table:
        emit(f"| {lo}–{lo+SCENE_STEP-1 if lo+SCENE_STEP<1000 else 1000} | {n} | {frac:.1%} |")
    emit(f"- 自动关键帧里最小的变化面积：{auto_min}；按“过半且之后各档都过半”定的门槛：{thr if thr<=1000 else '没有（整屏换窗口也没有触发）'}")



    # 全屏视频：每一帧都是后端自己出的关键帧，用关键帧行估
    fv = (d["cls"] == "fullvideo") & (d["frame"] > 0)
    emit("")
    emit("## 全屏视频（每一帧都是后端自己出的关键帧）")
    emit(f"- 帧数 {int(fv.sum())}，其中后端出了关键帧的 {int((d['key'][fv] == 1).sum())}；变化面积 {int(d['chg'][fv].min())}–{int(d['chg'][fv].max())}‰")
    for tag, m in (("拟合集", fv & is_fit), ("留出集", fv & ~is_fit)):
        e = log_err(key_model.predict(d, m), d["bytes"][m])
        emit(f"- 用关键帧行估（{tag}，n={int(m.sum())}）：|log2| 中位 {pct(e,50):.3f}，90% {pct(e,90):.3f}")

    if "--explore" in sys.argv:
        emit("")
        emit("## 探索（不进模型文件）：形式不够用时按协议 1.5 节的顺序试的改法，每试一步重算留出集")
        emit("单帧 |log2| 中位 / 90% 分位（留出集）；最后一列是 2 秒窗口相对误差的中位（留出集，该类的全部窗口）。")
        variants = [("基准（协议 1.1 的形式）", ()), ("+ QP 二次项", ("qp2",)), ("+ QP 二次项 + 低 QP 段（QP≤17）", ("qp2", "low")),
                    ("+ log2(总像素)（分辨率单独进）", ("res",)), ("以上全加", ("qp2", "low", "res"))]
        emit("| 改法 | " + " | ".join(NORMAL_CLASSES) + " |")
        emit("|---|" + "---|" * len(NORMAL_CLASSES))
        for label, extras in variants:
            vm = {}
            cells = []
            for c in NORMAL_CLASSES:
                mf = normal & seqcls & is_fit & (d["cls"] == c) & ~d["real"]
                vm[c] = Fit(False, False, extras).fit(d, mf, head_n)
                mh = normal & seqcls & ~is_fit & (d["cls"] == c) & ~d["real"]
                e = log_err(vm[c].predict(d, mh), d["bytes"][mh])
                ws = []
                for (nm, qp), ids in seqs.items():
                    ids = np.array(ids)
                    if d["cls"][ids[0]] != c or d["real"][ids[0]] or is_fit[ids[0]]:
                        continue
                    msk = np.zeros(len(d["frame"]), bool)
                    msk[ids] = True
                    pp = np.where(d["chg"][ids] >= thr_pred, key_model.predict(d, msk), vm[c].predict(d, msk))
                    ws.append(abs(pp.sum() / d["bytes"][ids].sum() - 1))
                cells.append(f"{pct(e,50):.2f} / {pct(e,90):.2f}；{pct(ws,50):.0%}")
            emit(f"| {label} | " + " | ".join(cells) + " |")
        # 单调性：真后端自己在高 QP 端是不是单调
        emit("")
        emit("### 真后端的字节数随 QP 是否单调（每个序列：QP 从小到大排，后一个比前一个大超过 10% 的次数）")
        emit("| 类别 | 序列×相邻 QP 对数 | 上升超过 10% 的比例 | 其中发生在 QP ≥ 42 的比例 |")
        emit("|---|---|---|---|")
        for c in NORMAL_CLASSES:
            tot = up = hi = 0
            names = sorted({n for n, k in seqs if d["cls"][seqs[(n, k)][0]] == c and not d["real"][seqs[(n, k)][0]]})
            for n in names:
                med = {}
                for (nm, qp), ids in seqs.items():
                    if nm == n:
                        med[qp] = float(np.median(d["bytes"][np.array(ids)]))
                ks = sorted(med)
                for a, b in zip(ks, ks[1:]):
                    tot += 1
                    if med[b] > med[a] * 1.1:
                        up += 1
                        hi += b >= 42
            emit(f"| {c} | {tot} | {up / max(tot, 1):.1%} | {hi / max(up, 1):.0%} |")

    # 分辨率假设：只看“种子在拟合集里”的序列，按分辨率分开；2560×1440 整个没参与拟合，其他四档参与了
    emit("")
    emit("## 分辨率假设：普通帧单帧 |log2| 中位，按分辨率分（只含拟合种子的序列；2560×1440 整个分辨率没参与拟合）")
    resolutions = [(1280, 720), (1920, 1080), (2560, 1440), (2880, 1800), (3840, 2160)]
    emit("| 类别 | " + " | ".join(f"{w}×{h}" for w, h in resolutions) + " |")
    emit("|---|" + "---|" * len(resolutions))
    seed_ok = np.array([int(n.split("-")[1][1:]) not in (6, 7, 9, 11) if n.split("-")[0] in CLASSES and "-s" in n else False for n in d["seq"]])
    for c in NORMAL_CLASSES:
        cells = []
        for w, h in resolutions:
            m = normal & seqcls & seed_ok & (d["cls"] == c) & (d["w"] == w)
            e = log_err(models[(c, "normal")].predict(d, m), d["bytes"][m])
            cells.append(f"{pct(e,50):.2f}")
        emit(f"| {c} | " + " | ".join(cells) + " |")
    cells = []
    for w, h in resolutions:
        m = key & seed_ok & seqcls & (d["w"] == w)
        cells.append(f"{pct(log_err(key_model.predict(d, m), d['bytes'][m]),50):.2f}")
    emit("| 关键帧 | " + " | ".join(cells) + " |")

    # 和占位模型比（同一批留出帧；占位模型不认补清晰，补清晰帧变化面积是 0）
    emit("")
    emit("## 占位模型（DefaultByteModel）对实测：留出集上 log2(占位/实测) 的中位（带符号，正 = 占位偏大）和 |.| 的中位；拟合模型作对照")
    emit("| 帧 | 占位 带符号中位 | 占位 |.|中位 | 占位 窗口(2s)相对误差中位 | 拟合(本类) |.|中位 |")
    emit("|---|---|---|---|---|---|")
    hold = ~is_fit & ~d["real"]
    groups = [(c + " 普通帧", normal & seqcls & hold & (d["cls"] == c), c) for c in NORMAL_CLASSES]
    groups += [("关键帧", key & hold & (d["cls"] != "scene"), None), ("补清晰", refine & hold, None)]
    for name, m, pool in groups:
        if not m.any():
            continue
        idx = np.nonzero(m)[0]
        ph = placeholder(d["pix"][idx], d["chg"][idx], d["det"][idx], d["qp"][idx], d["key"][idx] == 1)
        r = np.log2(np.maximum(ph, 1) / d["bytes"][idx])
        fitted = log_err(models[(pool, "normal")].predict(d, m), d["bytes"][m]) if pool else (
            log_err(key_model.predict(d, m), d["bytes"][m]) if name == "关键帧" else rf_err_all(m))
        wins = []
        if name.endswith("普通帧"):
            for (nm, qp), ids in seqs.items():
                ids = np.array(ids)
                if d["cls"][ids[0]] != name.split()[0] or d["real"][ids[0]] or is_fit[ids[0]]:
                    continue
                pp = placeholder(d["pix"][ids], d["chg"][ids], d["det"][ids], d["qp"][ids], d["key"][ids] == 1)
                wins.append(abs(pp.sum() / d["bytes"][ids].sum() - 1))
        w = f"{pct(wins,50):.0%}" if wins else "-"
        emit(f"| {name} | {np.median(r):+.2f} | {np.median(np.abs(r)):.2f} | {w} | {np.median(fitted):.2f} |")

    # (面积, 细节) 覆盖
    emit("")
    emit("## (变化面积, 细节) 覆盖：1080p、普通帧的帧数（行 = 变化面积千分比档，列 = 细节千分比档）")
    amask = normal & seqcls & (d["pix"] == 1920 * 1080) & (d["qp"] == 24)
    ab = [0, 1, 10, 50, 100, 300, 600, 1001]
    db = [0, 50, 100, 150, 200, 300, 500, 1001]
    emit("| 面积＼细节 | " + " | ".join(f"{db[i]}–{db[i+1]-1}" for i in range(len(db) - 1)) + " |")
    emit("|---|" + "---|" * (len(db) - 1))
    for i in range(len(ab) - 1):
        cells = []
        for j in range(len(db) - 1):
            cells.append(str(int((amask & (d["chg"] >= ab[i]) & (d["chg"] < ab[i + 1]) & (d["det"] >= db[j]) & (d["det"] < db[j + 1])).sum())))
        emit(f"| {ab[i]}–{ab[i+1]-1} | " + " | ".join(cells) + " |")

    # 模型文件
    def held(c, k):
        return hold_med.get((c, k), float("nan"))

    kh_e = log_err(key_model.predict(d, kf), d["bytes"][kf])
    rf_e = log_err(models[("refine", "normal")].predict(d, refine & ~is_fit), d["bytes"][refine & ~is_fit])
    held_vals = {("any", "key"): float(np.median(kh_e)), ("refine", "normal"): float(np.median(rf_e))}
    with open(out, "w") as f:
        f.write("# 后端 openh264（屏幕内容实时模式，复杂度低，按固定 QP 编），2026-11-27，基于提交 a3a6bb6，工具 crates/e2e/examples/measure.rs（scripts/m2-*.py）\n")
        f.write("# 适用范围：QP 12–51，分辨率 1280×720 到 3840×2160；画面是合成桌面（RealisticDesktop），不是真实桌面，见 data-m2.md\n")
        f.write("# 普通帧：log2(字节-帧头) = a + b·QP + c·log2(变化的像素数) + d·log2(细节)；关键帧、补清晰用总像素数；补清晰再加 e·(上一帧QP-QP)\n")
        f.write("# 系数是 10^-6 精度的十进制；残差标准差是 log2 单位；留出中位误差是留出集上 |log2(预测/实测)| 的中位数\n")
        f.write("类别\t帧种类\t帧头\ta\tb\tc\td\te\t残差标准差\t样本数\t留出中位误差\n")
        for (c, k), mdl in models.items():
            b = mdl.beta
            e = b[4] if len(b) > 4 else 0.0
            hv = held_vals.get((c, k), held(c, k))
            f.write(f"{c}\t{k}\t{mdl.head}\t{b[0]:.6f}\t{b[1]:.6f}\t{b[2]:.6f}\t{b[3]:.6f}\t{e:.6f}\t{mdl.sigma:.6f}\t{mdl.n}\t{hv:.4f}\n")
        f.write("# 场景切换：变化面积达到多少（千分比）后端自己出关键帧（没有一档过半时写 1001，等于不会自己出）\n")
        f.write(f"scene_key_permille\t{thr}\n")
    if "--report" in sys.argv:
        with open(out + ".report.md", "w") as f:
            f.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
