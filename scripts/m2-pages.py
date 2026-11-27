#!/usr/bin/env python3
"""M2 的留出集：真字体的文字页做成打字、滚动序列（PNG，按序号排好），给 crates/e2e/examples/measure.rs 的 pngseq 任务读。

用法：scripts/m2-pages.py <输出目录>
字体：仓库里的 Inter、思源黑体简体（OFL）和系统的 DejaVu Sans Mono；文字是自己写的，没有许可问题。
画面是 Pillow 画的（灰度抗锯齿，真字体），不是真实程序的画面；Xvfb 录屏另算。
"""
import os
import sys

from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
FONTS = {
    "en": os.path.join(ROOT, "crates/client/assets/fonts/inter-regular.otf"),
    "zh": os.path.join(ROOT, "crates/client/assets/fonts/noto-sans-sc-regular.otf"),
    "code": "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
}
TEXT = {
    "en": (
        "Remote desktop sessions spend most of their time almost still and then change all at once. "
        "A cursor blinks, a few characters appear, a window is dragged, and suddenly the whole screen is new. "
        "The encoder has to keep small text legible while the picture moves, and it has to sharpen the picture "
        "quickly once it stops. Numbers such as 1l1I 0O0 rn m are easy to confuse, so they are included here. "
    ),
    "zh": (
        "远程桌面大部分时间几乎静止，偶尔突然整屏变化。光标闪烁，敲下几个字，拖动一个窗口，画面就全换了。"
        "编码器要在画面移动时保持小字能认出来，停下之后又要很快补清晰。网络变差时宁可慢一点，也不能糊到看不清。"
        "笔画多的字比如“赢”“鬱”“靈”和容易混的字比如“己已巳”“末未”也要写进来。"
    ),
    "code": (
        "fn rate_for(frame: &Frame, qp: u8) -> usize { let area = frame.changed * 1000 / frame.total; "
        "if area > 900 { return KEY_BYTES >> (qp / 6); } (area as usize) << 4 } // 0O 1lI rn m "
        "let mut sum = 0u64; for (i, b) in bytes.iter().enumerate() { sum += (*b as u64) ^ (i as u64); } "
    ),
}
THEMES = {"light": ((255, 255, 255), (30, 30, 30)), "dark": ((30, 30, 30), (212, 212, 212))}


def layout(kind, size, width, margin):
    font = ImageFont.truetype(FONTS[kind], size)
    words = (TEXT[kind] * 40).replace("\n", " ")
    lines, cur = [], ""
    # 中文没有空格：按字符折行；英文和代码按词
    units = list(words) if kind == "zh" else words.split(" ")
    sep = "" if kind == "zh" else " "
    for u in units:
        trial = (cur + sep + u) if cur else u
        if font.getlength(trial) > width - 2 * margin:
            lines.append(cur)
            cur = u
        else:
            cur = trial
    lines.append(cur)
    return font, lines


def page(lines, font, size, bg, fg, w, h, margin, upto=None, top=0):
    img = Image.new("RGB", (w, h), bg)
    d = ImageDraw.Draw(img)
    step = int(size * 1.5)
    left = upto
    for i, line in enumerate(lines):
        y = margin + i * step - top
        if y > h:
            break
        if y + step < 0:
            if left is not None:
                left -= len(line)
            continue
        if left is not None:
            if left <= 0:
                break
            line = line[:left]
            left -= len(line)
        d.text((margin, y), line, font=font, fill=fg)
    return img


def main(out):
    w, h, n, margin = 1920, 1080, 60, 40
    for kind in ("en", "zh", "code"):
        for size in (11, 14):
            for theme, (bg, fg) in THEMES.items():
                font, lines = layout(kind, size, w, margin)
                step = int(size * 1.5)
                per_page = (h - 2 * margin) // step
                total = sum(len(x) for x in lines[:per_page])
                # 打字：页面里已经有一半字，之后每帧多出一个字
                d = os.path.join(out, f"type-{kind}-{size}-{theme}")
                os.makedirs(d, exist_ok=True)
                for k in range(n):
                    page(lines, font, size, bg, fg, w, h, margin, upto=total // 2 + k).save(
                        os.path.join(d, f"{k:04d}.png")
                    )
                # 滚动：整页每帧上移 4 像素
                d = os.path.join(out, f"scroll-{kind}-{size}-{theme}")
                os.makedirs(d, exist_ok=True)
                for k in range(n):
                    page(lines, font, size, bg, fg, w, h, margin, top=4 * k).save(
                        os.path.join(d, f"{k:04d}.png")
                    )


if __name__ == "__main__":
    main(sys.argv[1])
