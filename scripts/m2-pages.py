#!/usr/bin/env python3
"""M2 的留出集：真字体的文字页做成打字、滚动序列（PNG，按序号排好），给 crates/e2e/examples/measure.rs 的 pngseq 任务读。

用法：scripts/m2-pages.py <输出目录>
字体：仓库里的 Inter、思源黑体简体（OFL）和系统的 DejaVu Sans Mono；文字是自己写的，没有许可问题。
画面是 Pillow 画的（灰度抗锯齿，真字体），不是真实程序的画面；Xvfb 录屏另算。

M2b（B3）起，每个序列目录里多一个 regions.txt：第 k 行是第 k 帧相对第 k-1 帧的变化区域，和采集接口交的东西同一种
（脏区域 `d:x,y,w,h`、移动 `m:来源x,来源y,x,y,w,h`），第 0 行是 `whole`。区域是页面生成器自己交的，不是事后从像素里猜的：
- 打字：新字的外框（和上一张逐像素比出来的外接矩形，各外扩 1 像素）；
- 滚动：整页上移 4 像素 = 一个移动（目的地是除了最下面 4 行以外的整块）加最下面露出来的 4 行脏区域；
生成时逐像素核对：实际变了的像素都在区域里，移动区域里的像素确实是上一帧来源那一块的拷贝（核对不过就报错退出）。
另外生成一组“滚动窗口”（win25/50/75/100-<字体>-<字号>-<主题>）：窗口宽高取整屏的 25%、50%、75%、100%，
窗口居中，窗口外是静止的灰底，窗口里的文字每帧上移 4 像素；100% 就是整屏滚动（和 scroll-* 同样的画面）。
"""
import os
import sys

import numpy as np
from PIL import Image, ImageChops, ImageDraw, ImageFont

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


def save(img, d, k):
    img.save(os.path.join(d, f"{k:04d}.png"), compress_level=1)


def bbox_region(prev, cur, w, h):
    """两张图逐像素比出来的外接矩形，各外扩 1 像素；没变返回 None。"""
    box = ImageChops.difference(prev, cur).getbbox()
    if box is None:
        return None
    x0, y0 = max(box[0] - 1, 0), max(box[1] - 1, 0)
    x1, y1 = min(box[2] + 1, w), min(box[3] + 1, h)
    return (x0, y0, x1 - x0, y1 - y0)


def check(prev, cur, dirty, moves):
    """实际变了的像素都在区域里，移动区域里的像素确实是上一帧来源那一块的拷贝。"""
    a, b = np.asarray(prev), np.asarray(cur)
    changed = np.any(a != b, axis=2)
    covered = np.zeros(changed.shape, bool)
    for (x, y, w, h) in dirty:
        covered[y:y + h, x:x + w] = True
    for (fx, fy, x, y, w, h) in moves:
        covered[y:y + h, x:x + w] = True
        assert np.array_equal(b[y:y + h, x:x + w], a[fy:fy + h, fx:fx + w]), "移动区域不是上一帧来源的拷贝"
    assert not (changed & ~covered).any(), f"{int((changed & ~covered).sum())} 个变了的像素不在区域里"


def write_regions(d, lines):
    with open(os.path.join(d, "regions.txt"), "w") as f:
        f.write("\n".join(lines) + "\n")


def scroll_tokens(x0, y0, w, h, step=4):
    """窗口 (x0,y0,w,h) 里的内容上移 step 像素：移动加底下露出来的一条。"""
    mv = (x0, y0 + step, x0, y0, w, h - step)
    dr = (x0, y0 + h - step, w, step)
    return [dr], [mv], f"d:{dr[0]},{dr[1]},{dr[2]},{dr[3]} m:{mv[0]},{mv[1]},{mv[2]},{mv[3]},{mv[4]},{mv[5]}"


WINDOW_BG = (52, 54, 58)


def main(out, kinds=("en", "zh", "code"), sizes=(11, 14), themes=("light", "dark")):
    w, h, n, margin = 1920, 1080, 60, 40
    for kind in kinds:
        for size in sizes:
            for theme, (bg, fg) in THEMES.items():
                if theme not in themes:
                    continue
                font, lines = layout(kind, size, w, margin)
                step = int(size * 1.5)
                per_page = (h - 2 * margin) // step
                total = sum(len(x) for x in lines[:per_page])
                # 打字：页面里已经有一半字，之后每帧多出一个字
                d = os.path.join(out, f"type-{kind}-{size}-{theme}")
                os.makedirs(d, exist_ok=True)
                prev, regs = None, ["whole"]
                for k in range(n):
                    img = page(lines, font, size, bg, fg, w, h, margin, upto=total // 2 + k)
                    save(img, d, k)
                    if prev is not None:
                        r = bbox_region(prev, img, w, h)
                        # 新的一个字是空格时画面一点没变：区域是空的（这样的帧 M2 也编了）
                        check(prev, img, [r] if r else [], [])
                        regs.append("d:%d,%d,%d,%d" % r if r else "")
                    prev = img
                write_regions(d, regs)
                # 滚动：整页每帧上移 4 像素
                d = os.path.join(out, f"scroll-{kind}-{size}-{theme}")
                os.makedirs(d, exist_ok=True)
                prev, regs = None, ["whole"]
                for k in range(n):
                    img = page(lines, font, size, bg, fg, w, h, margin, top=4 * k)
                    save(img, d, k)
                    if prev is not None:
                        dirty, moves, tok = scroll_tokens(0, 0, w, h)
                        check(prev, img, dirty, moves)
                        regs.append(tok)
                    prev = img
                write_regions(d, regs)
                # 滚动窗口：窗口宽高取整屏的 25%、50%、75%、100%，居中，窗口外是静止的灰底
                for pct in (25, 50, 75, 100):
                    ww, wh = w * pct // 100, h * pct // 100
                    x0, y0 = (w - ww) // 2, (h - wh) // 2
                    wfont, wlines = layout(kind, size, ww, margin)
                    d = os.path.join(out, f"win{pct}-{kind}-{size}-{theme}")
                    os.makedirs(d, exist_ok=True)
                    prev, regs = None, ["whole"]
                    for k in range(n):
                        inner = page(wlines, wfont, size, bg, fg, ww, wh, margin, top=4 * k)
                        img = Image.new("RGB", (w, h), WINDOW_BG)
                        img.paste(inner, (x0, y0))
                        save(img, d, k)
                        if prev is not None:
                            dirty, moves, tok = scroll_tokens(x0, y0, ww, wh)
                            check(prev, img, dirty, moves)
                            regs.append(tok)
                        prev = img
                    write_regions(d, regs)


if __name__ == "__main__":
    # 可选的第二个参数只用来试跑：字体种类、字号、主题，例如 code,14,light
    if len(sys.argv) > 2:
        k, z, t = sys.argv[2].split(",")
        main(sys.argv[1], (k,), (int(z),), (t,))
    else:
        main(sys.argv[1])
