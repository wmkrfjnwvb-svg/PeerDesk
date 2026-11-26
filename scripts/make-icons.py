#!/usr/bin/env python3
"""生成 PeerDesk 的全部图标（标志设计见 packaging/icons/README.md）。

一份几何参数，同时输出矢量母版（SVG）和各平台用的位图，保证处处一致：
  packaging/icons/peerdesk.svg            方块图标母版（Windows、Linux、网站）
  packaging/icons/peerdesk-mark.svg       只有标志（单色，菜单栏、文档里用）
  packaging/icons/peerdesk-{16..1024}.png 方块图标
  packaging/icons/peerdesk.ico            Windows（16–256 各尺寸分别绘制，不是缩放出来的）
  packaging/icons/peerdesk.icns           macOS（按苹果的图标网格：1024 画布里 824 的圆角方块，带投影）
  packaging/icons/tray-*.png              托盘 / 菜单栏
  crates/client/assets/window-icon.png    窗口图标（编进客户端）

用法：python3 scripts/make-icons.py [预览图路径]
依赖：Pillow（pip install pillow）。生成结果提交进仓库，打包时不需要重新生成。
"""

import sys
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "packaging" / "icons"
ASSETS = ROOT / "crates" / "client" / "assets"

# 颜色和界面的“信号”方向一致：墨色底、白色标志，颜色只用来说连接（直连青绿，深色底上的那一档）。
INK = (0x15, 0x18, 0x1D)
LINK = (0x3C, 0xC5, 0xB8)
WHITE = (0xFF, 0xFF, 0xFF)
MUTED = (0x8A, 0x92, 0x9E)

# 标志：小写 p 和 d，d 就是 p 转 180°——两端是对等的同一个软件（peer），碗是屏幕（desk），
# 中间的点是两台设备之间的直连。整个标志绕中心转 180° 不变。20 格画法，中心 (10, 10)。
# 标准版用于 64 px 及以上；48 px 及以下用加粗、放大的光学尺寸版，小图上笔画和点不糊。
REGULAR = {"stroke": 2.4, "radius": 3.4, "bowl": (1.2, 5.8, 8.6, 13.2), "ext": 5.6, "dot": 1.2, "cap": 1.2, "scale": 0.62}
SMALL = {"stroke": 2.8, "radius": 3.4, "bowl": (0.8, 5.6, 8.7, 13.4), "ext": 5.4, "dot": 1.35, "cap": 1.4, "scale": 0.74}
# 方块圆角占边长的比例（接近苹果图标的圆角）。
CORNER = 0.2237


def variant(size):
    return SMALL if size <= 48 else REGULAR


def rot(box):
    """绕 (10, 10) 转 180°。"""
    x0, y0, x1, y1 = box
    return (20 - x1, 20 - y1, 20 - x0, 20 - y0)


def shapes(v):
    """标志的几何：[(种类, 框, 圆角)]，种类是 'fill' 或 'hole'，按顺序画。"""
    s, r = v["stroke"], v["radius"]
    x0, y0, x1, y1 = v["bowl"]
    left = [
        ("fill", (x0, y0, x1, y1), r),
        ("hole", (x0 + s, y0 + s, x1 - s, y1 - s), max(r - s, 0.0)),
        # p 的竖笔：从碗的左边中段往下伸出，圆头
        ("fill", (x0, y0 + r, x0 + s, y1 + v["ext"]), v["cap"]),
    ]
    right = [(kind, rot(box), rad) for kind, box, rad in left]
    return left[:2] + right[:2] + [left[2], right[2]]


# ---------- 位图 ----------


def mark_mask(px, v):
    """边长 px 的标志掩码（L 模式，255 是标志）。"""
    m = Image.new("L", (px, px), 0)
    d = ImageDraw.Draw(m)
    u = px / 20.0
    for kind, (a, b, c, e), rad in shapes(v):
        d.rounded_rectangle([a * u, b * u, c * u, e * u], radius=rad * u, fill=255 if kind == "fill" else 0)
    return m


def dot_box(center, px, v):
    rr = v["dot"] * px / 20.0
    return [center - rr, center - rr, center + rr, center + rr]


def render_tile(size, inset=0.0, shadow=False, dot=LINK):
    """方块图标。inset 是方块到画布边的留白比例（macOS 网格是 100/1024）。"""
    v = variant(size)
    ss = 8 if size < 256 else 2  # 超采样后缩小，边缘抗锯齿
    n = size * ss
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    i0 = inset * n
    body = [i0, i0, n - i0, n - i0]
    radius = CORNER * (n - 2 * i0)
    if shadow:
        # 苹果图标网格的投影：向下 12/1024、模糊 28/1024、30% 黑
        from PIL import ImageFilter

        sh = Image.new("L", (n, n), 0)
        off = 12 / 1024 * n
        ImageDraw.Draw(sh).rounded_rectangle([body[0], body[1] + off, body[2], body[3] + off], radius=radius, fill=77)
        sh = sh.filter(ImageFilter.GaussianBlur(28 / 1024 * n / 2))
        img.paste(Image.new("RGBA", (n, n), (0, 0, 0, 255)), (0, 0), sh)
    tile = Image.new("L", (n, n), 0)
    ImageDraw.Draw(tile).rounded_rectangle(body, radius=radius, fill=255)
    img.paste(Image.new("RGBA", (n, n), INK + (255,)), (0, 0), tile)
    side = n - 2 * i0
    mpx = int(side * v["scale"])
    off = int((n - mpx) / 2)
    img.paste(Image.new("RGBA", (mpx, mpx), WHITE + (255,)), (off, off), mark_mask(mpx, v))
    ImageDraw.Draw(img).ellipse(dot_box(n / 2, mpx, v), fill=dot + (255,))
    return img.resize((size, size), Image.LANCZOS)


def render_mark(size, color, dot):
    """只有标志、透明底（托盘、菜单栏）。标志撑满画布的 90%。"""
    v = variant(size)
    ss = 8
    n = size * ss
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    mpx = int(n * 0.9)
    off = int((n - mpx) / 2)
    img.paste(Image.new("RGBA", (mpx, mpx), color + (255,)), (off, off), mark_mask(mpx, v))
    ImageDraw.Draw(img).ellipse(dot_box(n / 2, mpx, v), fill=dot + (255,))
    return img.resize((size, size), Image.LANCZOS)


# ---------- 矢量 ----------


def rr_path(box, rad):
    x0, y0, x1, y1 = box
    rad = min(rad, (x1 - x0) / 2, (y1 - y0) / 2)
    f = lambda x: f"{x:.3f}".rstrip("0").rstrip(".")
    return (
        f"M{f(x0 + rad)} {f(y0)}H{f(x1 - rad)}A{f(rad)} {f(rad)} 0 0 1 {f(x1)} {f(y0 + rad)}"
        f"V{f(y1 - rad)}A{f(rad)} {f(rad)} 0 0 1 {f(x1 - rad)} {f(y1)}"
        f"H{f(x0 + rad)}A{f(rad)} {f(rad)} 0 0 1 {f(x0)} {f(y1 - rad)}"
        f"V{f(y0 + rad)}A{f(rad)} {f(rad)} 0 0 1 {f(x0 + rad)} {f(y0)}Z"
    )


def mark_svg_body(v, fg, dot):
    """标志的 SVG 片段，坐标是 20 格。两个碗各用一条 evenodd 路径（外框加内孔），竖笔单独画。"""
    sh = shapes(v)
    parts = []
    for i in (0, 2):
        (_, outer, ro), (_, inner, ri) = sh[i], sh[i + 1]
        parts.append(f'<path fill="{fg}" fill-rule="evenodd" d="{rr_path(outer, ro)}{rr_path(inner, ri)}"/>')
    for _, box, rad in sh[4:]:
        parts.append(f'<path fill="{fg}" d="{rr_path(box, rad)}"/>')
    parts.append(f'<circle fill="{dot}" cx="10" cy="10" r="{v["dot"]}"/>')
    return "".join(parts)


def hexc(c):
    return "#{:02X}{:02X}{:02X}".format(*c)


def tile_svg():
    v = REGULAR
    s = v["scale"]
    k = 1024 * s / 20
    o = 1024 * (1 - s) / 2
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">'
        "<title>PeerDesk</title>"
        f'<rect width="1024" height="1024" rx="{1024 * CORNER:.1f}" fill="{hexc(INK)}"/>'
        f'<g transform="translate({o:.2f} {o:.2f}) scale({k:.4f})">{mark_svg_body(v, hexc(WHITE), hexc(LINK))}</g>'
        "</svg>\n"
    )


def mark_svg():
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20">'
        "<title>PeerDesk</title>"
        f"{mark_svg_body(REGULAR, 'currentColor', 'currentColor')}"
        "</svg>\n"
    )


# ---------- 输出 ----------


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    ASSETS.mkdir(parents=True, exist_ok=True)
    (OUT / "peerdesk.svg").write_text(tile_svg(), encoding="utf-8")
    (OUT / "peerdesk-mark.svg").write_text(mark_svg(), encoding="utf-8")

    # Windows、Linux：方块留一点边（16 px 以下不留，像素太宝贵）
    def flat(size):
        return render_tile(size, inset=0.0 if size <= 24 else 1 / 32)

    for size in (16, 24, 32, 48, 64, 128, 256, 512, 1024):
        flat(size).save(OUT / f"peerdesk-{size}.png")
    ico_sizes = (16, 24, 32, 48, 64, 128, 256)
    ico = [flat(s) for s in ico_sizes]
    ico[-1].save(OUT / "peerdesk.ico", sizes=[(s, s) for s in ico_sizes], append_images=ico[:-1])

    # macOS：苹果图标网格，1024 画布里 824 的圆角方块（留白 100），带投影
    mac = {s: render_tile(s, inset=100 / 1024, shadow=s >= 64) for s in (16, 32, 64, 128, 256, 512, 1024)}
    mac[1024].save(OUT / "peerdesk.icns", append_images=[mac[s] for s in (16, 32, 64, 128, 256, 512)])

    # 托盘：macOS 菜单栏用模板图（纯黑加透明，系统按深浅色自动着色），18 pt 高；
    # Windows、Linux 用彩色方块。点亮青绿点表示“有人连着”，平时点是灰的——颜色只说连接。
    render_mark(18, (0, 0, 0), (0, 0, 0)).save(OUT / "tray-template.png")
    render_mark(36, (0, 0, 0), (0, 0, 0)).save(OUT / "tray-template@2x.png")
    for size in (16, 32):
        render_tile(size, dot=MUTED).save(OUT / f"tray-idle-{size}.png")
        render_tile(size, dot=LINK).save(OUT / f"tray-active-{size}.png")

    # 窗口图标：编进客户端
    flat(256).save(ASSETS / "window-icon.png")

    if len(sys.argv) > 1:
        preview(Path(sys.argv[1]))


def preview(path):
    """预览图：大图、各尺寸原样、托盘在深浅菜单栏上。"""
    bg_light, bg_dark = (244, 245, 247, 255), (32, 34, 38, 255)
    sheet = Image.new("RGBA", (1200, 700), bg_light)
    dark = Image.new("RGBA", (600, 700), bg_dark)
    sheet.paste(dark, (600, 0))
    big = render_tile(400, inset=1 / 32)
    for x in (100, 700):
        sheet.paste(big, (x, 40), big)
    x = 40
    for s in (128, 64, 48, 32, 24, 16):
        im = render_tile(s, inset=0 if s <= 24 else 1 / 32)
        for base in (0, 600):
            sheet.paste(im, (base + x, 520 - s // 2), im)
        x += s + 20
    d = ImageDraw.Draw(sheet)
    for base, fg in ((0, (0, 0, 0)), (600, (255, 255, 255))):
        t = render_mark(36, fg, fg)
        sheet.paste(t, (base + 60, 630), t)
        for i, dot in enumerate((MUTED, LINK)):
            im = render_tile(32, dot=dot)
            sheet.paste(im, (base + 130 + i * 50, 632), im)
    d.text((40, 680), "menu bar template / tray idle / tray active", fill=(90, 98, 112, 255))
    sheet.save(path)


if __name__ == "__main__":
    main()
