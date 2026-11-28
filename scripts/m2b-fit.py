#!/usr/bin/env python3
"""M2b（B3）：选形式、拟合、留出检验。形式和判据在测量之前写定，见 docs/research/Q-redesign/m2b-plan.md 第 3、4 节，
B3 补充见 docs/research/Q-redesign/data-m2b.md 第 3 节第 9、10 条。

用法：scripts/m2b-fit.py <测量输出目录（里面有 out-main、out-pages、out-roll、out-refine、out-floor）> <模型文件输出路径> [--report]
依赖 numpy、scipy。

流程：
1. 只用 split=fit 的行，按种子分 4 折交叉验证 F0–F3，比“全部类别合在一起、按字节加权的两秒窗口误差”的中位数（Σ|预测-实测| ÷ Σ实测）；
   相差不到 2 个百分点取参数少的；F0 只作对照，不参选。
2. 选中的形式在全部拟合行上重拟合，写模型文件。
3. 留出集（split=hold）只算一次：M2 判据 1–5 按类别报，新增的按字节加权口径、整屏滚动、desc 链、换 QP 后第 2、3 帧的尾巴各自单列；
   和 M2 的 v1 模型、占位模型比。

面积用“碰到的宏块数 × 256”（审查第 9 条）；像素数只在对照表里出现。
"""
import csv
import glob
import os
import sys
import time

import numpy as np
from scipy.optimize import least_squares

CLASSES = ["pointer", "typing", "scrolling", "dragging", "switching", "video", "fullvideo"]
NORMAL_CLASSES = [c for c in CLASSES if c != "fullvideo"]
FIT_SEEDS = [0, 1, 2, 3, 4, 5, 8, 10]  # 拟合集的种子（留出：6、7、9、11）
FOLDS = 4
SCENE_THR = 850  # 场景切换门槛，沿用 M2（预测时变化面积 ≥ 它的帧按关键帧算）
FLOOR_MAX_QP = 17
MB_PX = 256

# M2 的 v1 模型（crates/media/models/openh264-v1.tsv）：这里直接读那个文件，不抄数
HERE = os.path.dirname(os.path.abspath(__file__))
V1_FILE = os.path.join(HERE, "..", "crates", "media", "models", "openh264-v1.tsv")


# ---------------------------------------------------------------- 读数据
def parse_gap(text):
    if text == "-":
        return []
    return [tuple(map(int, p.split(":"))) for p in text.split("|")]


def load(root):
    rows = []
    for rnd in ("main", "pages", "roll", "refine", "floor"):
        for path in sorted(glob.glob(os.path.join(root, f"out-{rnd}", "*.csv"))):
            with open(path) as f:
                for r in csv.DictReader(f):
                    if None in r.values() or None in r:  # 写到一半的最后一行
                        continue
                    r["round"] = rnd
                    rows.append(r)
    n = len(rows)
    ints = ["w", "h", "frame", "qp", "asked_key", "got_key", "bytes", "changed_permille", "detail_permille", "enc_us",
            "dirty_px", "moved_px", "dirty_mbs", "moved_mbs", "full_move", "dirty_detail", "repeat",
            "changed_px", "region_px", "uncovered_px"]
    d = {k: np.array([int(r[k]) for r in rows]) for k in ints}
    for k in ("round", "seq", "class", "split"):
        d[k] = np.array([r[k] for r in rows])
    d["prev"] = np.array([-1 if r["prev_qp"] == "-" else int(r["prev_qp"]) for r in rows])
    d["psnr"] = np.array([float("nan") if r["psnr"] == "-" else float(r["psnr"]) for r in rows])
    d["pix"] = d["w"] * d["h"]
    d["real"] = np.array([s.startswith("real-") for s in d["seq"]])
    # 静止宏块画质差分布：稀疏（行号，画质差，个数）
    gi, gg, gn = [], [], []
    for i, r in enumerate(rows):
        for g, c in parse_gap(r["gap"]):
            gi.append(i)
            gg.append(g)
            gn.append(c)
    d["gap_i"], d["gap_g"], d["gap_n"] = np.array(gi, int), np.array(gg, int), np.array(gn, float)
    # 种子（合成序列、补清晰链、地板链从名字里读；真字体页面没有）
    seeds = np.full(n, -1)
    for i, s in enumerate(d["seq"]):
        if not s.startswith("real-"):
            try:
                seeds[i] = int(s.split("-")[1][1:])
            except (ValueError, IndexError):
                pass
    d["seed"] = seeds
    # 页面序列没有种子：拟合集里的滚动窗口页面按名字排序后轮流分到 4 折
    fit_pages = sorted({s for s, sp in zip(d["seq"], d["split"]) if s.startswith("real-win") and sp == "fit"})
    page_fold = {name: k % FOLDS for k, name in enumerate(fit_pages)}
    fold = np.full(n, -1)
    for i in range(n):
        if d["split"][i] != "fit":
            continue
        if d["seed"][i] >= 0:
            fold[i] = FIT_SEEDS.index(int(d["seed"][i])) // (len(FIT_SEEDS) // FOLDS)
        else:
            fold[i] = page_fold.get(d["seq"][i], -1)
    d["fold"] = fold
    print(f"读入 {n} 行", file=sys.stderr)
    return d


def lg(x):
    return np.log2(np.maximum(x, 1))


def derive(d):
    """每行的模型输入（宏块数 × 256 当面积）。"""
    d["key"] = d["got_key"] == 1
    d["Ad"] = d["dirty_mbs"] * MB_PX
    d["Am"] = d["moved_mbs"] * MB_PX
    d["lAd"] = lg(d["Ad"])
    d["lAm"] = lg(d["Am"])
    d["ldd"] = lg(d["dirty_detail"])
    d["ldt"] = lg(d["detail_permille"])
    d["static_px"] = np.maximum(d["pix"] - MB_PX * (d["dirty_mbs"] + d["moved_mbs"]), 0)
    d["qpf"] = d["qp"].astype(float)
    n = len(d["qp"])
    d["has_d"] = d["Ad"] > 0
    d["has_m"] = d["Am"] > 0
    d["full"] = d["full_move"].astype(float)
    return d


def sub(d, m):
    """行子集（gap 稀疏表也跟着重编号）。"""
    idx = np.nonzero(m)[0]
    out = {}
    pos = np.full(len(m), -1)
    pos[idx] = np.arange(len(idx))
    for k, v in d.items():
        if k in ("gap_i", "gap_g", "gap_n"):
            continue
        out[k] = v[idx]
    keep = m[d["gap_i"]]
    out["gap_i"] = pos[d["gap_i"][keep]]
    out["gap_g"] = d["gap_g"][keep]
    out["gap_n"] = d["gap_n"][keep]
    return out


def nrows(d):
    return len(d["qp"])


# ---------------------------------------------------------------- v1 / 占位 / 关键帧行
def read_v1():
    rows = {}
    scene = None
    for line in open(V1_FILE):
        line = line.strip()
        if not line or line.startswith("#") or line.startswith("类别"):
            continue
        f = line.split("\t")
        if f[0] == "scene_key_permille":
            scene = int(f[1])
            continue
        rows[(f[0], f[1])] = dict(head=int(f[2]), a=float(f[3]), b=float(f[4]), c=float(f[5]), d=float(f[6]),
                                  e=float(f[7]), sigma=float(f[8]))
    return rows, scene


V1, V1_SCENE = read_v1()
KEY = V1[("any", "key")]  # M2 的关键帧行，不重拟（方案第 3 节）


def key_pred(d):
    r = KEY
    return r["head"] + np.exp2(r["a"] + r["b"] * d["qpf"] + r["c"] * lg(d["pix"]) + r["d"] * lg(d["detail_permille"]))


def v1_pred(d, cls=None):
    """M2 的 v1 模型：普通帧用 `cls` 这一类的行（None = 各类合在一起的 any，假后端实际能用的），
    关键帧用关键帧行，补清晰（画面没变且 QP 比上一帧小）用补清晰行；和 fitted.rs 的 row() 一致。"""
    out = np.zeros(nrows(d))
    key = d["key"]
    refine = (~key) & (d["changed_permille"] == 0) & (d["prev"] > d["qp"])
    plain = (~key) & (~refine)
    out[key] = key_pred(sub(d, key))
    r = V1[("refine", "normal")]
    m = refine
    if m.any():
        up = (d["prev"][m] - d["qp"][m]).astype(float)
        out[m] = r["head"] + np.exp2(r["a"] + r["b"] * d["qpf"][m] + r["c"] * lg(d["pix"][m])
                                     + r["d"] * lg(d["detail_permille"][m]) + r["e"] * up)
    if cls is None:
        classes = [(None, plain)]
    else:
        classes = [(c, plain & (d["class"] == c)) for c in NORMAL_CLASSES]
    for c, m in classes:
        if not m.any():
            continue
        r = V1[(c or "any", "normal")]
        area = np.maximum(d["pix"][m] * d["changed_permille"][m] // 1000, 1)
        out[m] = r["head"] + np.exp2(r["a"] + r["b"] * d["qpf"][m] + r["c"] * lg(area) + r["d"] * lg(d["detail_permille"][m]))
    return out


STEP = [1000, 1122, 1260, 1414, 1587, 1782]


def placeholder_pred(d):
    """crates/media/src/fake.rs 的 DefaultByteModel（占位）的逐行移植，整数运算。"""
    out = np.zeros(nrows(d))
    for i in range(nrows(d)):
        q = min(int(d["qp"][i]), 51)
        divisor = STEP[q % 6] << (q // 6)
        area = 1000 if d["key"][i] else min(int(d["changed_permille"][i]), 1000)
        detail = 500 + min(int(d["detail_permille"][i]), 1000)
        milli = int(d["pix"][i]) * 1400 * detail * area // 1_000_000
        body = milli * 1000 // divisor // 1000
        out[i] = 24 + (40 if d["key"][i] else 0) + body
    return out


# ---------------------------------------------------------------- 形式
class Form:
    name = ""
    nparams = 0
    head = 13

    def fit(self, d, m):
        raise NotImplementedError

    def body(self, d):
        raise NotImplementedError

    def predict(self, d):
        """所有行的预测字节数：关键帧用关键帧行，其余用本形式。"""
        out = self.head + self.body(d)
        k = d["key"]
        if k.any():
            out[k] = key_pred(sub(d, k))
        return out


class F0(Form):
    """M2 的形式，每类一套系数：log2(字节-帧头) = a + b·QP + c·log2(变化像素数) + d·log2(细节)，变化像素数 = 像素 × 变化面积千分比。"""
    name = "F0"
    nparams = 4 * len(NORMAL_CLASSES)

    def __init__(self, head=13):
        self.head = head
        self.beta = {}

    @staticmethod
    def design(d):
        area = np.maximum(d["pix"] * d["changed_permille"] // 1000, 1)
        return np.stack([np.ones(nrows(d)), d["qpf"], lg(area), lg(d["detail_permille"])], axis=1)

    def fit(self, d, m):
        for c in NORMAL_CLASSES:
            mm = m & (d["class"] == c) & (~d["real"]) & (d["round"] == "main")
            s = sub(d, mm)
            body = s["bytes"] - self.head
            ok = body >= 2
            x = self.design(s)[ok]
            y = np.log2(body[ok])
            keep = [i for i in range(4) if i == 0 or x[:, i].std() > 1e-9]
            beta = np.zeros(4)
            sol, *_ = np.linalg.lstsq(x[:, keep], y, rcond=None)
            beta[keep] = sol
            self.beta[c] = beta
        return self

    def body(self, d):
        out = np.zeros(nrows(d))
        x = self.design(d)
        for c in NORMAL_CLASSES:
            m = d["class"] == c
            out[m] = np.exp2(x[m] @ self.beta[c])
        # 没有这一类的行（滚动窗口、补清晰等）：F0 没有输入，按 scrolling 的系数（最接近）
        rest = ~np.isin(d["class"], NORMAL_CLASSES)
        out[rest] = np.exp2(x[rest] @ self.beta["scrolling"])
        return out


class LogLinear(Form):
    """F1：全部帧一个式子 log2(B-h) = a + b·QP + c·L(脏) + m·L(移动) + f·整屏移动 + d·log2(脏区域细节)；
    F2：再加 b2·QP² + g·max(0, 18-QP)。L(x) = log2(1+x)。"""

    def __init__(self, second, head=13):
        self.second = second
        self.name = "F2" if second else "F1"
        self.nparams = 8 if second else 6
        self.head = head
        self.beta = None

    def design(self, d):
        q = d["qpf"]
        cols = [np.ones(nrows(d)), q, np.log2(1 + d["Ad"]), np.log2(1 + d["Am"]), d["full"], d["ldd"]]
        if self.second:
            cols += [(q - 30) ** 2 / 100, np.maximum(0, 18 - q)]
        return np.stack(cols, axis=1)

    def fit(self, d, m):
        # 内容帧：脏区域或移动区域非空（静止补清晰帧由补清晰行管，这个式子在那里没有定义）
        mm = m & (d["has_d"] | d["has_m"]) & (d["class"] != "refine") & (~d["key"])
        s = sub(d, mm)
        body = s["bytes"] - self.head
        ok = body >= 2
        x = self.design(s)[ok]
        y = np.log2(body[ok])
        keep = [i for i in range(x.shape[1]) if i == 0 or x[:, i].std() > 1e-9]
        beta = np.zeros(x.shape[1])
        sol, *_ = np.linalg.lstsq(x[:, keep], y, rcond=None)
        beta[keep] = sol
        self.beta = beta
        self.dropped = [i for i in range(x.shape[1]) if i not in keep]
        return self

    def body(self, d):
        return np.exp2(self.design(d) @ self.beta)

    def predict(self, d):
        out = super().predict(d)
        # 画面没变的补清晰帧：沿用 M2 的补清晰行（这个式子没有补清晰项）
        static = (~d["key"]) & (~d["has_d"]) & (~d["has_m"])
        if static.any():
            out[static] = v1_pred(sub(d, static))
        return out


THETA_NAMES = ["a_d", "b_d", "c_d", "d_d", "a_m", "b_m", "c_m", "f", "a_s", "b_s", "d_s", "e", "a_f", "b_f"]
THETA_LO = [-60, -0.6, 0.0, -2.0, -60, -0.6, 0.0, -12.0, -60, -0.6, -2.0, 0.0, -60, -2.0]
THETA_HI = [40, 0.0, 1.6, 6.0, 40, 0.0, 1.6, 3.0, 40, 0.0, 6.0, 1.5, 40, 0.0]


class F3(Form):
    """B = h + D + M + S + 地板（各部分相加）。
    D = 2^(a_d + b_d·QP) · 脏面积^c_d · 脏区域细节^d_d
    M = 2^(a_m + b_m·QP) · 移动面积^c_m · 2^(f·整屏移动)
    S = 2^(a_s + b_s·QP) · 全帧细节^d_s · Σ n_g · 256 · 2^(e·g)
    地板（F3-常数）：QP ≤ 17 时加 2^(a_f + b_f·QP) · 静止像素数。"""
    name = "F3"
    nparams = 14

    def __init__(self, head=13):
        self.head = head
        self.theta = None

    def _terms(self, th, d):
        a_d, b_d, c_d, d_d, a_m, b_m, c_m, f, a_s, b_s, d_s, e, a_f, b_f = th
        q = d["qpf"]
        D = np.where(d["has_d"], np.exp2(a_d + b_d * q + c_d * d["lAd"] + d_d * d["ldd"]), 0.0)
        M = np.where(d["has_m"], np.exp2(a_m + b_m * q + c_m * d["lAm"] + f * d["full"]), 0.0)
        n = nrows(d)
        if len(d["gap_i"]):
            gs = np.bincount(d["gap_i"], weights=d["gap_n"] * MB_PX * np.exp2(e * d["gap_g"]), minlength=n)
        else:
            gs = np.zeros(n)
        S = np.exp2(a_s + b_s * q + d_s * d["ldt"]) * gs
        Fl = np.where(d["qp"] <= FLOOR_MAX_QP, np.exp2(a_f + b_f * q) * d["static_px"], 0.0)
        return D, M, S, Fl

    def body_with(self, th, d):
        D, M, S, Fl = self._terms(th, d)
        return D + M + S + Fl

    def body(self, d):
        return self.body_with(self.theta, d)

    def init_points(self, d, m):
        """初值：D 取各类合在一起的对数线性拟合；M 取 D 小一点；S 取 M2 的补清晰行；地板取 B2 的量级。"""
        s = sub(d, m & (d["has_d"] | d["has_m"]) & (~d["key"]) & (d["class"] != "refine"))
        ll = LogLinear(False, self.head).fit(d, m)
        a, b, c, m_, f, dd = ll.beta
        r = V1[("refine", "normal")]
        base = [a, min(b, 0.0), float(np.clip(c, 0.1, 1.5)), float(np.clip(dd, -1, 5)),
                a - 2, min(b, 0.0), float(np.clip(m_, 0.1, 1.5)), -3.0,
                r["a"] + 8, r["b"], r["d"], r["e"], -10.0, -0.3]
        base2 = list(base)
        base2[0], base2[4] = base[0] - 3, base[4] - 5
        base2[8] = r["a"]
        base2[13] = -0.2
        return [base, base2]

    def fit(self, d, m):
        # 拟合行：内容帧、补清晰链、地板的 hold 链（desc 链不进拟合：宏块画质表在逐步降 QP 时的假设已知不成立，审查第 10 条，只在留出报告里单列）
        # hold 链的第 2–4 帧是降 QP 之后的“尾巴”（B2：稳定要到第 4–6 帧），不是地板，也不是 F3 的任何一项：不进拟合，留出报告里单列误差
        tail = (d["class"] == "floorhold") & (d["frame"] >= 2) & (d["frame"] <= 4)
        mm = m & (~d["key"]) & (d["class"] != "floordesc") & (d["frame"] > 0) & (~tail)
        s = sub(d, mm)
        body = s["bytes"] - self.head
        ok = body >= 2
        s = sub(s, ok)
        y = np.log2((s["bytes"] - self.head).astype(float))

        def resid(th):
            return np.log2(np.maximum(self.body_with(th, s), 1e-3)) - y

        best = None
        for x0 in self.init_points(d, m):
            x0 = np.clip(x0, np.array(THETA_LO) + 1e-6, np.array(THETA_HI) - 1e-6)
            sol = least_squares(resid, x0, bounds=(THETA_LO, THETA_HI), x_scale="jac", max_nfev=200)
            if best is None or sol.cost < best.cost:
                best = sol
        self.theta = best.x
        self.cost = best.cost
        self.n_fit = len(y)
        self.resid_sigma = float(np.std(resid(best.x)))
        return self


# ---------------------------------------------------------------- 窗口
def with_pixel_area(d):
    """对照用：面积换成像素数（脏区域、移动区域的像素数，静止像素数也相应换）。形式不变，只换面积的单位（方案：像素数只用来对照）。"""
    dp = dict(d)
    dp["Ad"] = d["dirty_px"].astype(np.int64)
    dp["Am"] = d["moved_px"].astype(np.int64)
    dp["lAd"], dp["lAm"] = lg(dp["Ad"]), lg(dp["Am"])
    dp["has_d"], dp["has_m"] = dp["Ad"] > 0, dp["Am"] > 0
    dp["static_px"] = np.maximum(d["pix"] - dp["Ad"] - dp["Am"], 0)
    return dp


def window_groups(d, m):
    """main 轮合成序列里每个（序列, QP）的第 1 帧到最后一帧是一个窗口；返回（行下标列表的数组，组号）。"""
    idx = np.nonzero(m)[0]
    names = np.char.add(np.char.add(d["seq"][idx], "|"), d["qp"][idx].astype(str))
    uniq, inv = np.unique(names, return_inverse=True)
    return idx, inv, len(uniq)


def pooled_windows(d, pred, m, scene_key=True):
    """按字节加权的窗口误差：每个窗口 Σ|预测-实测| ÷ Σ实测；以及 M2 的净误差 |Σ预测 ÷ Σ实测 - 1|。"""
    idx, inv, g = window_groups(d, m)
    p = pred[idx]
    # 变化面积大的帧按关键帧算
    kp = key_pred(sub(d, np.isin(np.arange(nrows(d)), idx)))
    # sub 保持原顺序，idx 也是升序，所以对得上
    if scene_key:
        p = np.where(d["changed_permille"][idx] >= SCENE_THR, kp, p)
    a = d["bytes"][idx].astype(float)
    abs_err = np.bincount(inv, weights=np.abs(p - a), minlength=g)
    sum_a = np.bincount(inv, weights=a, minlength=g)
    sum_p = np.bincount(inv, weights=p, minlength=g)
    first = np.zeros(g, int)
    first[inv[::-1]] = idx[::-1]
    return abs_err / sum_a, np.abs(sum_p / sum_a - 1), first


def pct(a, q):
    a = np.asarray(a, float)
    return float(np.percentile(a, q)) if len(a) else float("nan")


def iv(x):
    """整数显示，没有数据（nan）显示 -。"""
    return "-" if x != x else str(int(x))


def med(a):
    a = np.asarray(a, float)
    return float(np.median(a)) if len(a) else float("nan")


def log_err(pred, actual):
    return np.abs(np.log2(np.maximum(pred, 1) / np.maximum(actual, 1)))


# ---------------------------------------------------------------- 交叉验证
def cv(d, forms_factory, head):
    fit_rows = d["split"] == "fit"
    synth = (d["round"] == "main") & np.isin(d["class"], CLASSES) & (d["frame"] > 0) & (~d["real"])
    roll = (d["round"] == "roll") & (d["frame"] > 0)
    res = {}
    for name, make in forms_factory.items():
        t0 = time.time()
        scores, scores_roll, parts = [], [], []
        by_class = {}
        for k in range(FOLDS):
            train = fit_rows & (d["fold"] != k)
            test = fit_rows & (d["fold"] == k)
            form = make(head).fit(d, train)
            pred = form.predict(d)
            ws, _, first = pooled_windows(d, pred, synth & test)
            scores.append(ws)
            for c in CLASSES:
                by_class.setdefault(c, []).append(ws[d["class"][first] == c])
            if (roll & test).any():
                wr, _, _ = pooled_windows(d, pred, roll & test)
                scores_roll.append(wr)
        allw = np.concatenate(scores)
        res[name] = dict(median=float(np.median(allw)), p90=pct(allw, 90), n=len(allw),
                         per_fold=[float(np.median(s)) for s in scores],
                         roll_median=float(np.median(np.concatenate(scores_roll))) if scores_roll else float("nan"),
                         params=make(head).nparams, secs=time.time() - t0,
                         classes={c: float(np.median(np.concatenate(v))) for c, v in by_class.items() if len(np.concatenate(v))})
        print(f"CV {name}: 窗口中位 {res[name]['median']:.1%}（90% {res[name]['p90']:.1%}，{len(allw)} 个窗口），{time.time()-t0:.0f} 秒",
              file=sys.stderr)
    return res


# ---------------------------------------------------------------- 报告
def main():
    root, out = sys.argv[1], sys.argv[2]
    lines = []

    def emit(s=""):
        lines.append(s)
        print(s)

    d = derive(load(root))
    # 帧头：静止的普通帧里最小的字节数（取不出内容的固定开销）
    static_normal = (~d["key"]) & (d["frame"] > 0) & (d["changed_px"] == 0)
    head = int(d["bytes"][static_normal].min())
    head_v1 = V1[("any", "normal")]["head"]
    emit(f"帧头：静止的普通帧里最小的是 {head} 字节（M2 的 v1 取 {head_v1}）；本轮沿用 {head_v1}。")
    head = head_v1

    # 数据体检
    emit("")
    emit("## 数据体检")
    emit(f"- 总行数 {nrows(d)}；各轮：" + "，".join(f"{r} {int((d['round'] == r).sum())}" for r in ('main', 'pages', 'roll', 'refine', 'floor')))
    emit(f"- 区域漏掉的像素（`uncovered_px`）总数：{int(d['uncovered_px'].sum())}（必须是 0）")
    content = (d["frame"] > 0) & (d["changed_px"] > 0)
    ratio = d["region_px"][content] / d["changed_px"][content]
    emit(f"- 区域面积 ÷ 实际变化像素数（有变化的帧 {int(content.sum())} 个）：中位 {np.median(ratio):.2f}，90% 分位 {pct(ratio, 90):.2f}，最大 {ratio.max():.1f}")
    for c in NORMAL_CLASSES + ["rolling"]:
        mm = content & (d["class"] == c) & (~d["real"] if c != "rolling" else True)
        if mm.any():
            r = d["region_px"][mm] / d["changed_px"][mm]
            mb = (d["Ad"][mm] + d["Am"][mm]) / d["changed_px"][mm]
            emit(f"  - {c}：像素区域 ÷ 实际变化 中位 {np.median(r):.2f}；宏块区域（碰到的宏块数×256）÷ 实际变化 中位 {np.median(mb):.2f}")
    emit(f"- 有 full_move 的帧：{int(d['full_move'].sum())} 个（来自：" + "、".join(sorted(set(d['seq'][d['full_move'] == 1]))[:6]) + "…）")
    emit(f"- 静止宏块画质差非空的帧：{len(set(d['gap_i'].tolist()))} 个")

    # ---- 选形式（只用拟合集）
    factories = {"F0": lambda h: F0(h), "F1": lambda h: LogLinear(False, h), "F2": lambda h: LogLinear(True, h), "F3": lambda h: F3(h)}
    emit("")
    emit("## 选形式：拟合集上按种子分 4 折交叉验证")
    emit("分数 = 全部类别合在一起、按字节加权的两秒窗口误差（Σ|预测-实测| ÷ Σ实测）的中位数，越小越好；窗口 = 合成序列里每个（序列, QP）的 59 个非首帧。"
         "折：种子 {0,1}、{2,3}、{4,5}、{8,10}（滚动窗口页面按名字轮流分到 4 折）。F0 只作对照。")
    res = cv(d, factories, head)
    emit("| 形式 | 参数个数 | 窗口误差中位 | 90% 分位 | 4 折各自的中位 | 滚动窗口（拟合集页面）中位 | 窗口数 |")
    emit("|---|---|---|---|---|---|---|")
    for name, r in res.items():
        emit(f"| {name} | {r['params']} | {r['median']:.1%} | {r['p90']:.1%} | {' / '.join(f'{x:.1%}' for x in r['per_fold'])} | {r['roll_median']:.1%} | {r['n']} |")
    # 对照：同样的形式，面积用像素数（不参选）
    dp = with_pixel_area(d)
    res_px = cv(dp, {"F1": lambda h: LogLinear(False, h), "F2": lambda h: LogLinear(True, h), "F3": lambda h: F3(h)}, head)
    emit("")
    emit("对照（不参选）：同样的形式，面积用像素数（脏区域像素数、移动区域像素数）代替宏块数 × 256：")
    emit("| 形式 | 窗口误差中位 | 90% 分位 | 滚动窗口（拟合集页面）中位 |")
    emit("|---|---|---|---|")
    for name, r in res_px.items():
        emit(f"| {name}（像素） | {r['median']:.1%} | {r['p90']:.1%} | {r['roll_median']:.1%} |")
    emit("")
    emit("按类别的窗口误差中位（只是看哪一类拖后腿；选形式不按类别）：")
    emit("| 形式 | " + " | ".join(CLASSES) + " |")
    emit("|---|" + "---|" * len(CLASSES))
    for name, r in res.items():
        emit(f"| {name} | " + " | ".join(f"{r['classes'].get(c, float('nan')):.1%}" for c in CLASSES) + " |")
    cand = {k: v for k, v in res.items() if k != "F0"}
    best = min(cand.values(), key=lambda v: v["median"])["median"]
    close = {k: v for k, v in cand.items() if v["median"] <= best + 0.02}
    chosen = min(close, key=lambda k: (close[k]["params"], close[k]["median"]))
    emit("")
    emit(f"最好的是 {min(cand, key=lambda k: cand[k]['median'])}（{best:.1%}）；相差不到 2 个百分点的有 {sorted(close)}，取参数最少的：**{chosen}**。")

    if "--cv-only" in sys.argv:  # 选形式阶段只看拟合集：不碰留出集
        if "--report" in sys.argv:
            with open(out + ".cv.md", "w") as f:
                f.write("\n".join(lines) + "\n")
        return

    # ---- 在全部拟合行上重拟合
    fit_rows = d["split"] == "fit"
    final = factories[chosen](head).fit(d, fit_rows)
    emit("")
    emit(f"## 选中的形式 {chosen} 在全部拟合行上的系数")
    if chosen == "F3":
        emit(f"拟合样本 {final.n_fit}，残差 σ（log2）{final.resid_sigma:.3f}")
        emit("| 参数 | 值 | 范围 | 贴边 |")
        emit("|---|---|---|---|")
        for nme, v, lo, hi in zip(THETA_NAMES, final.theta, THETA_LO, THETA_HI):
            edge = "是" if (abs(v - lo) < 1e-3 or abs(v - hi) < 1e-3) else ""
            emit(f"| {nme} | {v:.6f} | [{lo}, {hi}] | {edge} |")
    else:
        emit(f"系数 {np.round(final.beta, 6).tolist()}（未进入式子的列：{final.dropped}）")

    # 残差 σ（写进模型文件做扰动）：拟合用的那些行（帧头之外至少 2 字节的普通帧）上 log2 残差的标准差，和 M2 同一个定义
    if chosen == "F3":
        sigma = final.resid_sigma
    else:
        mm = fit_rows & (~d["key"]) & (d["frame"] > 0) & (d["has_d"] | d["has_m"]) & (d["class"] != "refine")
        s_ = sub(d, mm)
        ok = (s_["bytes"] - head) >= 2
        bodyp = final.predict(s_) - head
        sigma = float(np.std(np.log2(np.maximum(bodyp[ok], 1e-3)) - np.log2((s_["bytes"][ok] - head).astype(float))))
    emit(f"残差 σ（拟合行，写进模型文件做扰动）：{sigma:.3f}")

    evaluate(d, final, chosen, head, sigma, emit)
    pixel_control(d, dp, head, final, emit)
    direct_floor_report(root, emit)
    write_model(out, final, chosen, head, sigma, res, d)
    if "--report" in sys.argv:
        with open(out + ".report.md", "w") as f:
            f.write("\n".join(lines) + "\n")


# ---------------------------------------------------------------- 留出集
def evaluate(d, form, chosen, head, sigma, emit):
    hold = d["split"] == "hold"
    synth = (d["round"] == "main") & np.isin(d["class"], CLASSES) & (~d["real"])
    normal = (~d["key"]) & (d["frame"] > 0)
    pred = {"v2": form.predict(d), "v1-any": v1_pred(d), "v1-class": v1_pred(d, "per-class"), "占位": placeholder_pred(d)}
    emit("")
    emit("# 留出集（只算这一次）")

    # 判据 1
    emit("")
    emit("## 判据 1：单帧 |log2(预测/实测)|，普通帧，目标中位 ≤ 0.5、90% 分位 ≤ 1.0（拟合集 / 留出集；v2 对 v1-any、v1-class、占位）")
    emit("| 类别 | v2 拟合集 中位/90% | v2 留出 n | v2 留出 中位/90% | v1-any 留出 | v1-class 留出 | 占位 留出 |")
    emit("|---|---|---|---|---|---|---|")
    for c in NORMAL_CLASSES + ["ALL"]:
        mc = synth & normal & ((d["class"] == c) if c != "ALL" else np.isin(d["class"], NORMAL_CLASSES))
        e = {k: log_err(pred[k][mc & hold], d["bytes"][mc & hold]) for k in pred}
        ef = log_err(pred["v2"][mc & ~hold], d["bytes"][mc & ~hold])
        emit(f"| {c} | {pct(ef,50):.2f} / {pct(ef,90):.2f} | {int((mc & hold).sum())} | {pct(e['v2'],50):.2f} / {pct(e['v2'],90):.2f} | "
             f"{pct(e['v1-any'],50):.2f} / {pct(e['v1-any'],90):.2f} | {pct(e['v1-class'],50):.2f} / {pct(e['v1-class'],90):.2f} | "
             f"{pct(e['占位'],50):.2f} / {pct(e['占位'],90):.2f} |")
    # 各类带符号偏差
    emit("")
    emit("留出集单帧 log2(预测/实测) 带符号中位（负 = 预测偏小）：")
    emit("| 类别 | v2 | v1-any | 占位 |")
    emit("|---|---|---|---|")
    for c in NORMAL_CLASSES:
        mc = synth & normal & hold & (d["class"] == c)
        sg = {k: float(np.median(np.log2(np.maximum(pred[k][mc], 1) / d["bytes"][mc]))) for k in ("v2", "v1-any", "占位")}
        emit(f"| {c} | {sg['v2']:+.2f} | {sg['v1-any']:+.2f} | {sg['占位']:+.2f} |")

    # 判据 2：M2 的净误差，按类别；新增的按字节加权
    emit("")
    emit("## 判据 2：两秒窗口误差（窗口 = 59 个非首帧；变化面积 ≥ 850‰ 的帧按关键帧算）")
    emit("M2 口径 = |Σ预测 ÷ Σ实测 - 1|，目标中位 ≤ 15%、90% ≤ 30%，按类别；新增口径 = Σ|预测-实测| ÷ Σ实测（按字节加权，全部类别合在一起），同一目标。")
    emit("v1-class = M2 的 v1 模型用本类自己的系数（类别是 oracle，假后端实际拿不到），是 M2 报告里 27% / 95% 那一套。")
    emit("| 类别 | 窗口数（留出） | v2 M2 口径 中位/90% | v1-any M2 口径 | v1-class M2 口径 | 占位 M2 口径 | v2 加权口径 中位/90% | v1-any 加权 | v1-class 加权 | 占位 加权 |")
    emit("|---|---|---|---|---|---|---|---|---|---|")
    summary = {}
    for c in NORMAL_CLASSES + ["fullvideo", "ALL"]:
        mc = synth & (d["frame"] > 0) & hold & ((d["class"] == c) if c != "ALL" else True)
        cells, cells_w = [], []
        for k in ("v2", "v1-any", "v1-class", "占位"):
            w, net, _ = pooled_windows(d, pred[k], mc, scene_key=(k != "占位"))
            cells.append(f"{pct(net,50):.0%} / {pct(net,90):.0%}")
            cells_w.append(f"{pct(w,50):.1%} / {pct(w,90):.1%}")
            summary[(c, k)] = (pct(net, 50), pct(net, 90), pct(w, 50), pct(w, 90), len(w))
        emit(f"| {c} | {summary[(c,'v2')][4]} | {cells[0]} | {cells[1]} | {cells[2]} | {cells[3]} | {cells_w[0]} | {cells_w[1]} | {cells_w[2]} | {cells_w[3]} |")
    emit("")
    emit("拟合集上同样的窗口（判据 5 的对照）：")
    emit("| 类别 | v2 M2 口径 | v2 加权口径 |")
    emit("|---|---|---|")
    for c in ["ALL"]:
        mc = synth & (d["frame"] > 0) & (~hold)
        w, net, _ = pooled_windows(d, pred["v2"], mc)
        emit(f"| {c}（n={len(w)}） | {pct(net,50):.0%} / {pct(net,90):.0%} | {pct(w,50):.1%} / {pct(w,90):.1%} |")

    # 判据 3：单调
    emit("")
    emit("## 判据 3：对 QP 单调")
    viol = 0
    probe = sub(d, normal & synth & (d["split"] == "fit") & (d["seed"] == 0) & (d["frame"] < 4))
    base_n = nrows(probe)
    last = None
    for q in range(12, 52):
        probe["qp"] = np.full(base_n, q)
        probe["qpf"] = probe["qp"].astype(float)
        p = form.predict(probe)
        if last is not None:
            viol += int((p > last * (1 + 1e-9)).sum())
        last = p
    emit(f"- 模型：{base_n} 个真实输入（拟合种子 0 的前几帧，画质差分布等原样）逐个把 QP 从 12 扫到 51，预测变大的次数 {viol}（必须是 0）。Rust 里整数定点版另有单元测试和假后端体检（`checkup`）。")
    emit("- 真后端自己的非单调（M2 第 4 节）：未重测，沿用。")

    # 判据 4
    emit("")
    emit("## 判据 4：关键帧对普通帧的比值、补清晰一步，中位误差 ≤ 30%（留出集）")
    idx_main = np.nonzero(synth & (d["frame"] > 0) & hold & (d["class"] != "fullvideo"))[0]
    names = np.char.add(np.char.add(d["seq"], "|"), d["qp"].astype(str))
    ratios = {k: [] for k in ("v2", "v1-any")}
    first0 = {n: i for i, n in zip(np.nonzero(synth & (d["frame"] == 0) & hold)[0], names[synth & (d["frame"] == 0) & hold])}
    groups = {}
    for i in idx_main:
        groups.setdefault(names[i], []).append(i)
    for n, ids in groups.items():
        if n not in first0:
            continue
        i0 = first0[n]
        an = d["bytes"][ids].mean()
        for k in ratios:
            pn = pred[k][ids].mean()
            ratios[k].append(abs((pred[k][i0] / pn) / (d["bytes"][i0] / an) - 1))
    for k in ratios:
        emit(f"- 关键/普通比值相对误差（{k}，{len(ratios[k])} 个序列×QP）：中位 {pct(ratios[k],50):.1%}，90% {pct(ratios[k],90):.1%}")
    kf = synth & hold & d["key"]
    for k in ("v2", "v1-any"):
        er = np.abs(pred[k][kf] / d["bytes"][kf] - 1)
        emit(f"- 关键帧单帧（{k}，n={int(kf.sum())}）：相对误差中位 {pct(er,50):.1%}（关键帧行没有重拟，两边应当相同）")
    rf = (d["class"] == "refine") & (d["frame"] > 0) & hold
    for k in ("v2", "v1-any"):
        er = np.abs(pred[k][rf] / d["bytes"][rf] - 1)
        e2 = log_err(pred[k][rf], d["bytes"][rf])
        emit(f"- 补清晰一步（M2 的静止补清晰链，{k}，n={int(rf.sum())}）：相对误差中位 {pct(er,50):.1%}，90% {pct(er,90):.1%}；|log2| 中位 {pct(e2,50):.3f}")
    # 补清晰按链的种类
    emit("")
    emit("补清晰一步，按链和步（留出，v2 相对误差中位 / v1-any；实测字节中位）：")
    emit("| 链 | 步 | n | v2 | v1-any | 实测字节中位 |")
    emit("|---|---|---|---|---|---|")
    chains = sorted({s.split("-", 3)[3] for s in d["seq"][rf]})
    for ch in chains:
        for step in range(1, len(ch.split("_"))):
            m = rf & (np.char.endswith(d["seq"], "-" + ch)) & (d["frame"] == step)
            if m.any():
                a = [pct(np.abs(pred[k][m] / d["bytes"][m] - 1), 50) for k in ("v2", "v1-any")]
                emit(f"| {ch} | {step} | {int(m.sum())} | {a[0]:.0%} | {a[1]:.0%} | {iv(med(d['bytes'][m]))} |")

    # 判据 5
    emit("")
    emit("## 判据 5：留出集误差不超过拟合集的 1.5 倍（单帧中位的比值，v2）")
    cells = []
    for c in NORMAL_CLASSES:
        mc = synth & normal & (d["class"] == c)
        ef, eh = log_err(pred["v2"][mc & ~hold], d["bytes"][mc & ~hold]), log_err(pred["v2"][mc & hold], d["bytes"][mc & hold])
        cells.append(f"{c} {pct(eh,50)/max(pct(ef,50),1e-9):.2f}")
    emit("- " + "；".join(cells))

    # 单列：整屏滚动、真字体页面、滚动窗口
    emit("")
    emit("## 单列 1：真字体页面和整屏滚动（全部留出）")
    emit("单帧 log2 误差 中位 / 90%；窗口（59 帧）加权口径 中位 / M2 净口径 中位；v2 / v1-any / 占位；实测÷预测的中位比（v2）。")
    emit("| 页面 | n 帧 | v2 单帧 | v1-any 单帧 | 占位 单帧 | v2 窗口加权 | v2 窗口净 | v1-any 窗口净 | 占位 窗口净 | 实测/预测（v2） |")
    emit("|---|---|---|---|---|---|---|---|---|---|")
    pages = (d["round"] == "pages") & (d["frame"] > 0)
    for label, m in (("真字体 打字", pages & (d["class"] == "typing")), ("真字体 整屏滚动", pages & (d["class"] == "scrolling"))):
        e = {k: log_err(pred[k][m & normal], d["bytes"][m & normal]) for k in ("v2", "v1-any", "占位")}
        w2, n2, _ = pooled_windows(d, pred["v2"], m)
        _, n1, _ = pooled_windows(d, pred["v1-any"], m)
        _, np_, _ = pooled_windows(d, pred["占位"], m, scene_key=False)
        ratio = float(np.median(d["bytes"][m & normal] / pred["v2"][m & normal]))
        emit(f"| {label} | {int((m & normal).sum())} | {pct(e['v2'],50):.2f} / {pct(e['v2'],90):.2f} | {pct(e['v1-any'],50):.2f} / {pct(e['v1-any'],90):.2f} | "
             f"{pct(e['占位'],50):.2f} / {pct(e['占位'],90):.2f} | {pct(w2,50):.0%} | {pct(n2,50):.0%} | {pct(n1,50):.0%} | {pct(np_,50):.0%} | {ratio:.2f} |")
    emit("")
    emit("真字体页面打字，按 QP（留出，v2）：实测字节中位、v2 预测中位、实测÷预测的字节总和比；脏区域碰到的宏块数中位（合成桌面的打字是 1 个）：")
    emit("| QP | 实测中位 | v2 预测中位 | 字节总和比（实测÷预测） | 脏宏块数中位 | 脏区域为空的帧占比 |")
    emit("|---|---|---|---|---|---|")
    ptype = pages & (d["class"] == "typing") & normal
    for q in (12, 15, 18, 24, 36, 48):
        m = ptype & (d["qp"] == q)
        if m.any():
            emit(f"| {q} | {iv(med(d['bytes'][m]))} | {iv(med(pred['v2'][m]))} | {d['bytes'][m].sum() / pred['v2'][m].sum():.2f} | {iv(med(d['dirty_mbs'][m]))} | {float((d['dirty_mbs'][m] == 0).mean()):.0%} |")
    # 整屏滚动按字体
    emit("")
    emit("整屏滚动（M2 的 scroll-* 页面，留出）按字体种类和字号：v2、v1-any 的单帧 log2 误差中位，实测÷预测的中位，QP 24 的实测字节中位：")
    emit("| 页面 | v2 | v1-any | 实测÷预测(v2) | 实测字节中位(QP 24 附近) |")
    emit("|---|---|---|---|---|")
    for kind in ("en", "zh", "code"):
        for size in (11, 14):
            m = pages & (d["class"] == "scrolling") & np.char.startswith(d["seq"], f"real-scroll-{kind}-{size}-") & normal
            if m.any():
                e2, e1 = log_err(pred["v2"][m], d["bytes"][m]), log_err(pred["v1-any"][m], d["bytes"][m])
                m24 = m & (d["qp"] == 24)
                emit(f"| {kind} {size} | {pct(e2,50):.2f} | {pct(e1,50):.2f} | {float(np.median(d['bytes'][m]/pred['v2'][m])):.2f} | {iv(med(d['bytes'][m24]))} |")

    emit("")
    emit("## 单列 2：滚动窗口组（窗口宽高 = 整屏的 25%/50%/75%/100%）")
    emit("按窗口大小和集（en、zh 在拟合集里，code 留出）：单帧 log2 误差中位、实测字节中位（QP 24）、实测/预测中位、移动宏块数中位。")
    emit("| 窗口 | 集 | n 帧 | v2 单帧中位 | v1-any 单帧中位 | 实测字节(QP24 中位) | 实测/预测(v2) | 移动宏块数 | full_move |")
    emit("|---|---|---|---|---|---|---|---|---|")
    roll = (d["round"] == "roll") & (d["frame"] > 0) & normal
    for pctw in (25, 50, 75, 100):
        for sp in ("fit", "hold"):
            m = roll & np.char.startswith(d["seq"], f"real-win{pctw}-") & (d["split"] == sp)
            if m.any():
                m24 = m & (d["qp"] == 24)
                emit(f"| {pctw}% | {sp} | {int(m.sum())} | {pct(log_err(pred['v2'][m], d['bytes'][m]),50):.2f} | {pct(log_err(pred['v1-any'][m], d['bytes'][m]),50):.2f} | "
                     f"{iv(med(d['bytes'][m24]))} | {float(np.median(d['bytes'][m]/pred['v2'][m])):.2f} | {iv(med(d['moved_mbs'][m]))} | {int(d['full_move'][m].max())} |")
    # 移动项和移动面积的关系（QP 24，所有窗口大小）：字节 vs 移动宏块数
    emit("")
    emit("移动项和移动面积（QP 24，滚动窗口，每个窗口大小一个中位数）：字节数中位 ÷ 移动宏块数 / 字节数中位 ÷ 窗口内容宏块数")
    emit("| 窗口 | 移动宏块数 | 字节中位 | 字节/移动宏块 |")
    emit("|---|---|---|---|")
    for pctw in (25, 50, 75, 100):
        m = roll & np.char.startswith(d["seq"], f"real-win{pctw}-") & (d["qp"] == 24)
        if m.any():
            mb, by = float(np.median(d["moved_mbs"][m])), float(np.median(d["bytes"][m]))
            emit(f"| {pctw}% | {mb:.0f} | {by:.0f} | {by/max(mb,1):.2f} |")

    # 单列 3：desc 链、尾巴
    emit("")
    emit("## 单列 3：逐步补清晰（desc 链）和换 QP 后第 2、3 帧的尾巴")
    dsc = (d["class"] == "floordesc") & (d["frame"] > 0)
    emit("desc 链：从 QP 30 起每帧降 1，降到 12（19 帧）。不进拟合（宏块画质表假定“编过一次就到了这个 QP 的画质”，逐步往下降时到不了，审查第 10 条）。")
    emit("| 集 | 链数 | 帧数 | v2 单帧 log2 误差中位 | v2 整链字节 预测÷实测 | v1-any 整链 | 占位 整链 | 实测整链字节中位 |")
    emit("|---|---|---|---|---|---|---|---|---|")
    for label, sp in (("拟合集种子", d["split"] == "fit"), ("留出集种子", d["split"] == "hold"), ("全部", np.ones(nrows(d), bool))):
        m = dsc & sp
        if not m.any():
            continue
        seqs = np.unique(d["seq"][m])
        tot = {k: [] for k in ("v2", "v1-any", "占位", "act")}
        for s in seqs:
            mm = m & (d["seq"] == s)
            tot["act"].append(d["bytes"][mm].sum())
            for k in ("v2", "v1-any", "占位"):
                tot[k].append(pred[k][mm].sum())
        r = {k: float(np.median(np.array(tot[k]) / np.array(tot["act"]))) for k in ("v2", "v1-any", "占位")}
        emit(f"| {label} | {len(seqs)} | {int(m.sum())} | {pct(log_err(pred['v2'][m], d['bytes'][m]),50):.2f} | {r['v2']:.2f} | {r['v1-any']:.2f} | {r['占位']:.2f} | {iv(med(tot['act']))} |")
    emit("")
    emit("desc 链逐帧（全部 12 个种子的中位；预测 = v2）：")
    emit("| 第几帧 | QP | 实测字节 | v2 预测 | 预测÷实测 | 画质差分布（第一个种子） |")
    emit("|---|---|---|---|---|---|")
    for fr in range(1, 19):
        m = dsc & (d["frame"] == fr)
        if not m.any():
            continue
        gi = np.nonzero(m)[0][0]
        gaps = [(int(g), int(n)) for i, g, n in zip(d["gap_i"], d["gap_g"], d["gap_n"]) if i == gi][:3]
        emit(f"| {fr} | {int(d['qp'][m][0])} | {iv(med(d['bytes'][m]))} | {iv(med(pred['v2'][m]))} | {float(np.median(pred['v2'][m]/d['bytes'][m])):.2f} | {gaps} |")

    emit("")
    emit("hold 链（先按 QP 30 编一帧，再按同一个 q 连编 20 帧）：第 1 帧是降 QP 那一步，第 2、3 帧是尾巴，第 5 帧起是地板。")
    hold_chain = (d["class"] == "floorhold")
    emit("| 帧 | n | 实测字节中位 | v2 预测中位 | v2 log2 误差中位 | v1-any log2 误差中位 | 占位 log2 误差中位 | 实测占第 1 帧的比例 |")
    emit("|---|---|---|---|---|---|---|---|---|")
    first = {}
    for fr in (1, 2, 3, 4, 5, 10, 20):
        m = hold_chain & (d["frame"] == fr) & (d["qp"] <= 20)
        first[fr] = float(np.median(d["bytes"][m])) if m.any() else float("nan")
        emit(f"| {fr} | {int(m.sum())} | {iv(med(d['bytes'][m]))} | {iv(med(pred['v2'][m]))} | {pct(log_err(pred['v2'][m], d['bytes'][m]),50):.2f} | "
             f"{pct(log_err(pred['v1-any'][m], d['bytes'][m]),50):.2f} | {pct(log_err(pred['占位'][m], d['bytes'][m]),50):.2f} | {first[fr]/first[1] if fr > 1 else 1:.4f} |")
    emit("")
    emit("尾巴（第 2、3 帧）按 q（留出 + 拟合种子合并，v2 预测 ÷ 实测 的中位）：")
    emit("| q | 第 1 帧 实测 | v2 | 第 2 帧 实测 | v2 | 第 3 帧 实测 | v2 | 第 5 帧起（地板）实测 | v2 |")
    emit("|---|---|---|---|---|---|---|---|---|")
    for q in range(12, 21):
        cells = []
        for fr in (1, 2, 3, 5):
            m = hold_chain & (d["frame"] == fr) & (d["qp"] == q) & (d["seq"] != "") & np.char.endswith(d["seq"], f"-hold{q}-1920x1080")
            cells.append(f"{iv(med(d['bytes'][m]))} | {iv(med(pred['v2'][m]))}" if m.any() else "- | -")
        emit(f"| {q} | " + " | ".join(cells) + " |")
    tail = hold_chain & np.isin(d["frame"], (2, 3)) & hold
    et = log_err(pred["v2"][tail], d["bytes"][tail])
    floor_m = hold_chain & (d["frame"] >= 5) & hold
    ef = log_err(pred["v2"][floor_m], d["bytes"][floor_m])
    emit("")
    emit(f"- 尾巴（留出种子，第 2、3 帧，n={int(tail.sum())}）：v2 |log2| 中位 {pct(et,50):.2f}，90% {pct(et,90):.2f}；"
         f"预测÷实测 中位 {float(np.median(pred['v2'][tail] / d['bytes'][tail])):.2f}")
    emit(f"- 地板（留出种子，第 5 帧起，n={int(floor_m.sum())}）：v2 |log2| 中位 {pct(ef,50):.2f}，90% {pct(ef,90):.2f}")
    q_lo = floor_m & (d["qp"] <= 17)
    emit(f"  - 其中 q ≤ 17（有地板）：|log2| 中位 {pct(log_err(pred['v2'][q_lo], d['bytes'][q_lo]),50):.2f}；q ≥ 18（只剩帧头）：预测字节中位 {iv(med(pred['v2'][floor_m & (d['qp']>=18)]))}，实测中位 {iv(med(d['bytes'][floor_m & (d['qp']>=18)]))}")
    # 地板 + 第 1 帧（降 QP 一步）
    step1 = hold_chain & (d["frame"] == 1) & hold
    e1 = np.abs(pred["v2"][step1] / d["bytes"][step1] - 1)
    emit(f"- 降 QP 一步（第 1 帧，留出种子，n={int(step1.sum())}）：相对误差中位 {pct(e1,50):.1%}，90% {pct(e1,90):.1%}")


def pixel_control(d, dp, head, final, emit):
    """对照：F3 面积用像素数，在全部拟合行上重拟合，留出集上和宏块版并排（不参选，模型文件里是宏块版）。"""
    if not isinstance(final, F3):
        return
    hold = d["split"] == "hold"
    synth = (d["round"] == "main") & np.isin(d["class"], CLASSES) & (~d["real"])
    normal = (~d["key"]) & (d["frame"] > 0)
    fpx = F3(head).fit(dp, d["split"] == "fit")
    pm, pp = final.predict(d), fpx.predict(dp)
    emit("")
    emit("## 对照：F3 面积用像素数（留出集，不参选）")
    emit("同样的形式、同样的拟合行，只把脏、移动面积从“宏块数 × 256”换成像素数。单帧 log2 误差 中位 / 90%；窗口 M2 净口径 / 加权口径 的中位。")
    emit("| 类别 | 宏块面积 单帧 | 像素面积 单帧 | 宏块面积 窗口净 / 加权 | 像素面积 窗口净 / 加权 |")
    emit("|---|---|---|---|---|")
    for c in NORMAL_CLASSES + ["ALL"]:
        mc = synth & hold & ((d["class"] == c) if c != "ALL" else np.isin(d["class"], NORMAL_CLASSES))
        e = [log_err(p[mc & normal], d["bytes"][mc & normal]) for p in (pm, pp)]
        w = []
        for p in (pm, pp):
            ww, net, _ = pooled_windows(d, p, mc & (d["frame"] > 0))
            w.append(f"{pct(net,50):.0%} / {pct(ww,50):.1%}")
        emit(f"| {c} | {pct(e[0],50):.2f} / {pct(e[0],90):.2f} | {pct(e[1],50):.2f} / {pct(e[1],90):.2f} | {w[0]} | {w[1]} |")
    pages = (d["round"] == "pages") & (d["frame"] > 0)
    for label, m in (("真字体 打字", pages & (d["class"] == "typing")), ("真字体 整屏滚动", pages & (d["class"] == "scrolling"))):
        e = [log_err(p[m & normal], d["bytes"][m & normal]) for p in (pm, pp)]
        w = []
        for p in (pm, pp):
            ww, net, _ = pooled_windows(d, p, m)
            w.append(f"{pct(net,50):.0%} / {pct(ww,50):.1%}")
        emit(f"| {label} | {pct(e[0],50):.2f} / {pct(e[0],90):.2f} | {pct(e[1],50):.2f} / {pct(e[1],90):.2f} | {w[0]} | {w[1]} |")
    emit("")
    emit("像素面积版的系数：" + "，".join(f"{n} {v:.4f}" for n, v in zip(THETA_NAMES, fpx.theta)))


def direct_floor_report(root, emit):
    """诊断（不进拟合）：地板的大小和参考帧是怎么来的有关。out-direct 是直接按 q 编关键帧、再按 q 连编；
    out-floor 的 hold 链是先 QP 30 再降到 q。两者的中位字节数（12 个种子，1080p）。"""
    direct, hold = {}, {}
    for p_ in glob.glob(os.path.join(root, "out-direct", "*.csv")):
        for r in csv.DictReader(open(p_)):
            if None in r.values():
                continue
            direct.setdefault((int(r["qp"]), int(r["frame"])), []).append(int(r["bytes"]))
    for p_ in glob.glob(os.path.join(root, "out-floor", "*.csv")):
        for r in csv.DictReader(open(p_)):
            if None in r.values() or r["class"] != "floorhold":
                continue
            q = int(r["seq"].split("hold")[1].split("-")[0])
            hold.setdefault((q, int(r["frame"])), []).append(int(r["bytes"]))
    if not direct:
        return
    emit("")
    emit("## 诊断：低 QP 地板的大小和参考帧是怎么来的有关（不进拟合）")
    emit("字节数中位，12 个种子，1080p。“直接”= 第 0 帧直接按 q 编关键帧，之后按 q 连编同一幅画面；“hold 链”= 第 0 帧按 QP 30 编，之后按 q 连编（B2）。")
    emit("| q | 直接 第 1 帧 | 第 2 帧 | 第 5 帧 | 第 9 帧 | hold 链 第 1 帧（降 QP 一步） | 第 2 帧 | 第 3 帧 | 第 5 帧 | 第 9 帧 | 地板比（hold 第 9 帧 ÷ 直接第 9 帧） |")
    emit("|---|---|---|---|---|---|---|---|---|---|---|")
    for q in range(12, 21):
        a = [med(direct.get((q, f), [])) for f in (1, 2, 5, 9)]
        b = [med(hold.get((q, f), [])) for f in (1, 2, 3, 5, 9)]
        ratio = f"{b[4] / a[3]:.1f}" if a[3] == a[3] and b[4] == b[4] and a[3] > 14 else "-"
        emit(f"| {q} | " + " | ".join(iv(x) for x in a) + " | " + " | ".join(iv(x) for x in b) + f" | {ratio} |")


def write_model(path, form, chosen, head, sigma, res, d):
    key = KEY
    with open(path, "w") as f:
        f.write("# 后端 openh264（屏幕内容实时模式，复杂度低，按固定 QP 编），M2b（B3）重测拟合，工具 crates/e2e/examples/measure.rs + scripts/m2b-*.py\n")
        f.write("# 适用范围：QP 12–51，分辨率 1280×720 到 3840×2160；画面是合成桌面（RealisticDesktop）和真字体页面，不是真实桌面，见 data-m2b.md 第 4 节\n")
        f.write("# 输入是画面区域（假后端的 ModelInput.regions）：脏区域和移动区域碰到的宏块数 × 256 当面积、脏区域细节、整屏移动标记、静止宏块的画质差分布\n")
        f.write("# 帧字节 = 帧头 + D（新内容）+ M（搬来的）+ S（静止部分补清晰）+ 地板；关键帧沿用 M2 的关键帧行（不重拟）\n")
        f.write("# 系数是 10^-6 精度的十进制；残差标准差是 log2 单位；形式、判据和选择过程见 data-m2b.md 第 4 节\n")
        f.write(f"形式\t{chosen}\n")
        f.write(f"帧头\t{head}\n")
        f.write(f"残差标准差\t{sigma:.6f}\n")
        f.write("# 关键帧行：帧头 a b c d（log2(字节-帧头) = a + b·QP + c·log2(总像素) + d·log2(全帧细节)），原样取自 openh264-v1.tsv\n")
        f.write(f"关键帧\t{key['head']}\t{key['a']:.6f}\t{key['b']:.6f}\t{key['c']:.6f}\t{key['d']:.6f}\t{key['sigma']:.6f}\n")
        if chosen == "F3":
            f.write(f"# 地板（F3-常数）只在 QP ≤ 这个数时有：不随帧数衰减，只看 QP（B2）。但大小还和参考帧怎么来的、画面内容有关（直接按 q 编的关键帧之后小 1.4–4 倍，白底文字页几乎没有），单一的曲线只是折中，见 data-m2b.md 4.9\n地板最大QP\t{FLOOR_MAX_QP}\n")
            for nme, v in zip(THETA_NAMES, form.theta):
                f.write(f"参数\t{nme}\t{v:.6f}\n")
        else:
            names = ["a", "b", "c", "m", "f", "d"] + (["b2", "g"] if chosen == "F2" else [])
            for nme, v in zip(names, form.beta):
                f.write(f"参数\t{nme}\t{v:.6f}\n")
        f.write("# 场景切换：变化面积达到多少（千分比）后端自己出关键帧，沿用 M2（没有重测）\n")
        f.write(f"scene_key_permille\t{V1_SCENE}\n")


if __name__ == "__main__":
    main()
