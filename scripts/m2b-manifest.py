#!/usr/bin/env python3
"""M2b（B3）测量清单生成。分法和 M2 完全一样（沿用 scripts/m2-manifest.py 的常量和 split 规则），不是跑完再挑。
对应 docs/research/Q-redesign/m2b-plan.md 第 5 节、data-m2b.md 第 4 节。

用法：scripts/m2b-manifest.py <输出目录> [真字体页面目录（scripts/m2-pages.py 生成的）]
输出清单，交给 crates/e2e/examples/measure.rs：
  round-main.txt    M2 的 main 轮原样（合成桌面）：7 类 × 12 种子 × 5 分辨率 × QP，60 帧
  round-pages.txt   M2 的真字体页面：打字、整页滚动（全部留出；M2 的 main 轮里也有）
  round-refine.txt  M2 的静止补清晰链（10 条链 × 12 种子 × 5 分辨率）。场景切换门槛不重测：它和字节模型的新输入无关，沿用 M2 的 850
  round-roll.txt    新增的滚动窗口组：窗口宽高取整屏的 25%、50%、75%、100%（win*）
  round-direct.txt  诊断（不进拟合，测量之后加的，原因见 data-m2b.md 4.9）：直接按 q 编一个关键帧，再按同一个 q 连编 9 帧（q = 12…20，12 个种子，1080p），
                    和 B2 的 hold 链（先 QP 30 再降到 q）比低 QP 地板
B2 的链（hold 链、desc 链）用 `measure floor <输出目录> <第几份> <共几份>` 单独跑，任务写死在代码里（12 个种子，种子 6、7、9、11 留出）。
集：
- main、refine：和 M2 相同（种子 6、7、9、11 和整个 2560×1440 留出，其余拟合；真字体页面全部留出）。
- 滚动窗口组（M2 没有，在测量之前写死的分法）：字体 en、zh 进拟合集，code 留出。理由：拟合集里只有合成桌面，
  整屏移动（full_move）一次也没有出现过，f 项没有数据可拟；留出整种字体（等宽代码）比留出个别窗口大小更接近“没见过的内容”。
  M2 的真字体页面（type-*、scroll-*）仍然全部留出，一个不动。
"""
import importlib.util
import os
import random
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("m2m", os.path.join(HERE, "m2-manifest.py"))
m2 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m2)


def main(out, pages=None):
    os.makedirs(out, exist_ok=True)
    rng = random.Random(1)
    main_jobs, pages_jobs, refine_jobs, roll_jobs = [], [], [], []
    for c in m2.CLASSES:
        for s in m2.SEEDS:
            for (w, h) in m2.RES:
                q = ",".join(map(str, m2.qps(w, h)))
                main_jobs.append(f"seq {m2.split(s, w)} {c} {s} {w} {h} {m2.FRAMES} {q} -")
    for s in m2.SEEDS:
        for (w, h) in m2.RES:
            refine_jobs.append(f"refine {m2.split(s, w)} {s} {w} {h} {';'.join(m2.CHAINS)}")
    q1080 = ",".join(map(str, m2.QP_1080))
    for name in sorted(os.listdir(pages)) if pages else []:
        path = os.path.join(pages, name)
        if name.startswith(("type-", "scroll-")):
            cls = "typing" if name.startswith("type-") else "scrolling"
            pages_jobs.append(f"pngseq hold {cls} real-{name} {path} {q1080} -")
        elif name.startswith("win"):
            kind = name.split("-")[1]
            split = "hold" if kind == "code" else "fit"
            roll_jobs.append(f"pngseq {split} rolling real-{name} {path} {q1080} -")
    chains = ";".join(",".join([str(q)] * 10) for q in range(12, 21))
    direct_jobs = [f"refine {'hold' if sd in m2.HOLD_SEEDS else 'fit'} {sd} 1920 1080 {chains}" for sd in m2.SEEDS]
    for name, jobs in (("round-main", main_jobs), ("round-pages", pages_jobs), ("round-refine", refine_jobs), ("round-roll", roll_jobs), ("round-direct", direct_jobs)):
        if not jobs:
            continue
        rng.shuffle(jobs)  # 打散，各份的工作量差不多
        with open(os.path.join(out, name + ".txt"), "w") as f:
            f.write("# 由 scripts/m2b-manifest.py 生成，不要手改\n")
            f.write("\n".join(jobs) + "\n")
        print(name, len(jobs))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else None)
