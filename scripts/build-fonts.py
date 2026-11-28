#!/usr/bin/env python3
"""生成打进客户端的字体子集（crates/client/assets/fonts/）。

为什么要子集：整套字体太大（思源黑体一个字重 16 MB 以上）。界面只需要拉丁字母、
常用汉字和用到的图标。生僻字由系统字体兜底（见 crates/client/src/gui/fonts.rs）。

产物和取字范围：
- inter-{regular,medium,semibold}.otf：Inter 4.0。基本拉丁、Latin-1、Latin Extended-A、
  常用标点、箭头、货币符号。数字换成等宽数字（把 OpenType 的 tnum 替换直接写进 cmap：
  egui 不做 OpenType 特性替换，而设计要求数字等宽）。
- noto-sans-sc-regular.otf：思源黑体（Noto Sans CJK SC 2.004）Regular。GB 2312 全部
  （一级、二级共 6763 个汉字和符号区）、中日韩标点、全角字符，加上界面文案里出现的字。
- noto-sans-sc-{medium,semibold}.ttf：Noto Sans SC 500、600。只含界面文案（i18n.rs、失败原因的
  reasons.rs 和平台层的授权引导）里出现的字：标题和按钮才用这两个字重，它们的文字都来自文案。
  用户数据（设备名、文件名）里文案之外的字退回 Regular。
- phosphor-subset.ttf：Phosphor Regular，只含 theme.rs 的 icons! 清单里点名的图标。

字体来源（都是 SIL OFL 1.1 或 MIT，见 THIRD_PARTY_NOTICES.md）：
- Inter：https://github.com/rsms/inter 4.0 的静态 OTF（Debian 包 fonts-inter 4.0）。
- Noto Sans CJK：https://github.com/notofonts/noto-cjk Sans 2.004 的
  NotoSansCJK-Regular.ttc（Debian 包 fonts-noto-cjk），第 2 个字体是简体中文。
- Noto Sans SC 500/600：npm 包 @fontsource/noto-sans-sc 5.3.1（Google Fonts 的静态实例，
  只取 files/noto-sans-sc-chinese-simplified-{500,600}-normal.woff2）。
- Phosphor：egui-phosphor 0.10.0 自带的 res/Phosphor.ttf。

用法（需要 fonttools 和 brotli：pip install fonttools brotli）：
  scripts/build-fonts.py \
    --inter-dir /usr/share/fonts/opentype/inter \
    --noto-ttc /usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc \
    --fontsource-dir <解开的 @fontsource/noto-sans-sc>/files \
    --phosphor-dir ~/.cargo/registry/src/*/egui-phosphor-0.10.0

改了文案或图标清单后，客户端的测试会指出缺哪些字（gui::fonts 的测试），重新运行本脚本即可。
"""

import argparse
import re
import sys
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "crates/client/assets/fonts"
# 界面文案所在的文件：中文的中等、半粗字重只收这些文件里出现的字。
COPY_SOURCES = [
    ROOT / "crates/client/src/gui/i18n.rs",
    ROOT / "crates/client/src/gui/reasons.rs",
    ROOT / "crates/platform/src/guide.rs",
]
ICON_LIST = ROOT / "crates/client/src/gui/theme.rs"

# 拉丁字体的取字范围。
LATIN = (
    list(range(0x20, 0x7F))
    + list(range(0xA0, 0x180))
    + list(range(0x2010, 0x2028))  # 连字符、破折号、弯引号、省略号
    + [0x2030, 0x2032, 0x2033, 0x2039, 0x203A, 0x20AC, 0x2122]
    + list(range(0x2190, 0x219A))  # 箭头
    + [0x2212, 0x2248, 0x2260, 0x2264, 0x2265]
)

# 保留的 name 表条目：版权（0）、字体名（1、2、4、6）、版本（5）、许可证说明和地址（13、14）。
# OFL 要求每份拷贝都带版权和许可声明，留在字体文件里；全文在 THIRD_PARTY_NOTICES.md。
NAME_IDS = [0, 1, 2, 4, 5, 6, 13, 14]


def gb2312():
    """GB 2312 的全部字符（符号区加一级、二级汉字）。"""
    out = set()
    for hi in range(0xA1, 0xF8):
        for lo in range(0xA1, 0xFF):
            try:
                out.add(ord(bytes([hi, lo]).decode("gb2312")))
            except UnicodeDecodeError:
                pass
    return out


def copy_chars():
    """界面文案文件里出现的所有非 ASCII 字符（注释里的也算，多收无害）。"""
    out = set()
    for path in COPY_SOURCES:
        out |= {ord(c) for c in path.read_text(encoding="utf-8") if ord(c) > 0x7F}
    return out


def bake_tabular_figures(font):
    """把 tnum 特性的单字替换写进 cmap：数字默认就是等宽的。"""
    gsub = font["GSUB"].table
    lookups = set()
    for record in gsub.FeatureList.FeatureRecord:
        if record.FeatureTag == "tnum":
            lookups |= set(record.Feature.LookupListIndex)
    mapping = {}
    for index in lookups:
        for sub in gsub.LookupList.Lookup[index].SubTable:
            if hasattr(sub, "ExtSubTable"):
                sub = sub.ExtSubTable
            mapping.update(getattr(sub, "mapping", {}))
    if not mapping:
        sys.exit("这个字体没有 tnum 特性")
    changed = 0
    for table in font["cmap"].tables:
        for code, name in list(table.cmap.items()):
            if name in mapping:
                table.cmap[code] = mapping[name]
                changed += 1
    if changed == 0:
        sys.exit("tnum 没有替换任何字符")


def write_subset(font, unicodes, path, keep_layout=False):
    options = subset.Options()
    options.name_IDs = NAME_IDS
    options.name_languages = [0x409]
    options.layout_features = ["*"] if keep_layout else []
    options.hinting = False
    options.notdef_outline = True
    options.glyph_names = False
    options.drop_tables += ["vhea", "vmtx", "BASE", "STAT", "gasp", "prep", "DSIG", "VORG"]
    options.flavor = None
    worker = subset.Subsetter(options)
    worker.populate(unicodes=sorted(unicodes))
    worker.subset(font)
    font.flavor = None
    font.save(path)
    covered = set(font.getBestCmap())
    print(f"{path.relative_to(ROOT)}: {path.stat().st_size} 字节，{len(covered)} 个字符")
    return covered


def icon_codes(phosphor_dir):
    """theme.rs 的 icons! 清单里的图标名，对应到 Phosphor 的码位。"""
    text = ICON_LIST.read_text(encoding="utf-8")
    block = re.search(r"icons!\s*\{(.*?)\n\}", text, re.S)
    if block is None:
        sys.exit("theme.rs 里没有找到 icons! 清单")
    names = re.findall(r"\b[A-Z][A-Z0-9_]*\b", re.sub(r"//.*", "", block.group(1)))
    table = (phosphor_dir / "src/variants/regular.rs").read_text(encoding="utf-8")
    codes = dict(re.findall(r'pub const (\w+): &str = "\\u\{([0-9A-Fa-f]+)\}";', table))
    missing = [n for n in names if n not in codes]
    if missing or not names:
        sys.exit(f"Phosphor 里没有这些图标：{missing}")
    return {int(codes[n], 16) for n in names}


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--inter-dir", type=Path, required=True)
    parser.add_argument("--noto-ttc", type=Path, required=True)
    parser.add_argument("--fontsource-dir", type=Path, required=True)
    parser.add_argument("--phosphor-dir", type=Path, required=True)
    args = parser.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)

    for weight in ["Regular", "Medium", "SemiBold"]:
        font = TTFont(args.inter_dir / f"Inter-{weight}.otf")
        bake_tabular_figures(font)
        latin = write_subset(font, LATIN, OUT / f"inter-{weight.lower()}.otf")

    # 文案里 Inter 没有的字（汉字、全角标点、个别符号）都要由中文字体提供；图标（私用区）除外
    copy = {c for c in copy_chars() if c not in latin and not 0xE000 <= c < 0xF900}
    cjk = gb2312() | copy | set(range(0x3000, 0x3040)) | set(range(0xFF00, 0xFFF0))
    font = TTFont(args.noto_ttc, fontNumber=2)
    family = font["name"].getDebugName(1)
    if "SC" not in family:
        sys.exit(f"字体集里第 2 个不是简体中文：{family}")
    covered = write_subset(font, cjk, OUT / "noto-sans-sc-regular.otf")
    lost = sorted(copy - covered)
    if lost:
        sys.exit("文案里这些字 Regular 没有：" + "".join(map(chr, lost)))

    for weight, name in [(500, "medium"), (600, "semibold")]:
        source = args.fontsource_dir / f"noto-sans-sc-chinese-simplified-{weight}-normal.woff2"
        font = TTFont(source)
        covered = write_subset(font, copy, OUT / f"noto-sans-sc-{name}.ttf")
        # 汉字必须都有；这个来源里没有的个别符号（例如带圈数字）退回 Regular
        lost = sorted(c for c in copy - covered if 0x4E00 <= c <= 0x9FFF)
        if lost:
            sys.exit(f"文案里这些字 {weight} 字重没有：" + "".join(map(chr, lost)))

    font = TTFont(args.phosphor_dir / "res/Phosphor.ttf")
    icons = icon_codes(args.phosphor_dir)
    covered = write_subset(font, icons, OUT / "phosphor-subset.ttf")
    if icons - covered:
        sys.exit(f"Phosphor.ttf 里缺 {len(icons - covered)} 个图标")


if __name__ == "__main__":
    main()
