#!/usr/bin/env python3
"""M2 测量清单生成（分法写死在这里，不是跑完再挑）。对应 docs/research/Q-redesign/m2-m3-protocol.md 1.3、1.5 节。

用法：scripts/m2-manifest.py <输出目录> [真字体页面目录]
输出三个清单，交给 crates/e2e/examples/measure.rs：
  round-main.txt    主体：类别 × 种子 × 分辨率 × QP，60 帧；加真字体页面（留出）
  round-key.txt     强制关键帧：1080p、5 个 QP，每个序列在第 20、40 帧各强制一次
  round-refine.txt  静止补清晰（链）和场景切换门槛（换掉 10%–100%）
  round-scene2.txt  场景切换补测（第一轮之后加的：换进来的是视频噪声，变化面积能到 100%）
集：种子 6、7、9、11 和整个 2560×1440 是 hold（留出），其余 fit；真字体页面全部 hold。
"""
import os
import random
import sys

CLASSES = ["pointer", "typing", "scrolling", "dragging", "switching", "video", "fullvideo"]
SEEDS = list(range(12))  # 0-7 照片壁纸，8-9 大片纯色，10-11 密纹理（见 measure.rs 的 style_of）
HOLD_SEEDS = {6, 7, 9, 11}
RES = [(1280, 720), (1920, 1080), (2560, 1440), (2880, 1800), (3840, 2160)]
QP_1080 = list(range(12, 52, 3))  # 14 个
QP_OTHER = [16, 24, 32, 40, 48]
QP_KEY = [16, 24, 32, 40, 48]
FRAMES = 60
CHAINS = ["48,36", "40,28", "36,24", "28,20", "24,16", "20,12",
          "48,36,24,12", "44,36,28,20", "40,32,24,16", "36,30,24,18"]


def split(seed, w):
    return "hold" if seed in HOLD_SEEDS or w == 2560 else "fit"


def qps(w, h):
    return QP_1080 if (w, h) == (1920, 1080) else QP_OTHER


def main(out, pages=None):
    os.makedirs(out, exist_ok=True)
    rng = random.Random(1)
    main_jobs, key_jobs, refine_jobs = [], [], []
    for c in CLASSES:
        for s in SEEDS:
            for (w, h) in RES:
                q = ",".join(map(str, qps(w, h)))
                main_jobs.append(f"seq {split(s, w)} {c} {s} {w} {h} {FRAMES} {q} -")
            key_jobs.append(
                f"seq {split(s, 1920)} {c} {s} 1920 1080 42 {','.join(map(str, QP_KEY))} 20,40")
    for s in SEEDS:
        for (w, h) in RES:
            refine_jobs.append(f"refine {split(s, w)} {s} {w} {h} {';'.join(CHAINS)}")
    for s in range(6):
        for qp in (24, 36):
            refine_jobs.append(
                f"scene {split(s, 1920)} {s} 1920 1080 {qp} {','.join(str(a) for a in range(100, 1001, 100))}")
    # 场景切换的补测（第一轮的桌面换桌面最多变 60%，门槛上下没有数据）：换进来的是视频噪声，变化面积能到 100%；其他分辨率各一个种子
    scene2 = []
    areas = ",".join(str(a) for a in range(100, 1001, 100))
    for s in range(6):
        for qp in (24, 36):
            scene2.append(f"scenev fit {s} 1920 1080 {qp} {areas}")
    for (w, h) in [(1280, 720), (2560, 1440), (3840, 2160)]:
        scene2.append(f"scenev hold 1 {w} {h} 28 {areas}")
        scene2.append(f"scene hold 1 {w} {h} 28 {areas}")
    if pages:
        for name in sorted(os.listdir(pages)):
            cls = "typing" if name.startswith("type-") else "scrolling"
            main_jobs.append(
                f"pngseq hold {cls} real-{name} {os.path.join(pages, name)} "
                f"{','.join(map(str, QP_1080))} -")
    for name, jobs in (("round-main", main_jobs), ("round-key", key_jobs), ("round-refine", refine_jobs), ("round-scene2", scene2)):
        rng.shuffle(jobs)  # 打散，各份的工作量差不多
        with open(os.path.join(out, name + ".txt"), "w") as f:
            f.write("# 由 scripts/m2-manifest.py 生成，不要手改\n")
            f.write("\n".join(jobs) + "\n")
        print(name, len(jobs))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else None)
