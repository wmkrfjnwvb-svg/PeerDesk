//! 字体：拉丁字母用 Inter，中文用思源黑体（Noto Sans SC）的常用字子集，图标用 Phosphor 的子集，
//! 都打在程序里（各系统字形一致，不依赖系统装了什么）。子集由 `scripts/build-fonts.py` 生成。
//!
//! 子集之外的字（生僻字、日文、韩文，多半出现在文件名和设备名里）由系统字体兜底：运行时从系统里
//! 找一个中文字体放在最后。找不到时这些字会显示成方框，不能静默：`Installed::system_cjk` 为空，
//! 调用方要写日志并在界面上告诉用户原因和办法。
//!
//! egui 没有字重的概念，每个字重是一个字体族：正文是 `Proportional`，中等和半粗是具名的字体族。
//! 中文的中等、半粗字重只带界面文案里出现的字（标题和按钮的文字都来自文案），其余的字退回常规字重。
//! 没有单独的等宽字体：Inter 的数字已经换成等宽数字，`Monospace` 和正文用同一组字体。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ab_glyph::{Font as _, FontRef, ScaleFont as _};
use eframe::egui::{FontData, FontDefinitions, FontFamily, FontTweak};

/// 中等字重（500）的字体族名。
pub const MEDIUM: &str = "medium";
/// 半粗字重（600）的字体族名。
pub const SEMIBOLD: &str = "semibold";

const INTER_REGULAR: &str = "inter-regular";
const INTER_MEDIUM: &str = "inter-medium";
const INTER_SEMIBOLD: &str = "inter-semibold";
const CJK_REGULAR: &str = "noto-sans-sc-regular";
const CJK_MEDIUM: &str = "noto-sans-sc-medium";
const CJK_SEMIBOLD: &str = "noto-sans-sc-semibold";
const ICONS: &str = "phosphor";
const EMOJI: &str = "noto-emoji";
const SYSTEM_CJK: &str = "system-cjk";

/// 打进程序的字体（名字、文件内容）。
const EMBEDDED: [(&str, &[u8]); 8] = [
    (
        INTER_REGULAR,
        include_bytes!("../../assets/fonts/inter-regular.otf"),
    ),
    (
        INTER_MEDIUM,
        include_bytes!("../../assets/fonts/inter-medium.otf"),
    ),
    (
        INTER_SEMIBOLD,
        include_bytes!("../../assets/fonts/inter-semibold.otf"),
    ),
    (
        CJK_REGULAR,
        include_bytes!("../../assets/fonts/noto-sans-sc-regular.otf"),
    ),
    (
        CJK_MEDIUM,
        include_bytes!("../../assets/fonts/noto-sans-sc-medium.ttf"),
    ),
    (
        CJK_SEMIBOLD,
        include_bytes!("../../assets/fonts/noto-sans-sc-semibold.ttf"),
    ),
    (
        ICONS,
        include_bytes!("../../assets/fonts/phosphor-subset.ttf"),
    ),
    // 表情符号（文件名、设备名里可能有）：沿用 egui 自带的单色 Noto Emoji
    (EMOJI, epaint_default_fonts::NOTO_EMOJI_REGULAR),
];

/// egui 给自带的 Noto Emoji 用的缩放：不缩小的话表情比文字大一圈。
const EMOJI_SCALE: f32 = 0.81;

/// 每个字体族按顺序用哪些字体：前面的字体里没有的字，依次到后面的字体里找。
fn families() -> [(FontFamily, Vec<&'static str>); 4] {
    let regular = vec![INTER_REGULAR, CJK_REGULAR, ICONS, EMOJI];
    [
        (FontFamily::Proportional, regular.clone()),
        (FontFamily::Monospace, regular),
        (
            FontFamily::Name(MEDIUM.into()),
            vec![INTER_MEDIUM, CJK_MEDIUM, CJK_REGULAR, ICONS, EMOJI],
        ),
        (
            FontFamily::Name(SEMIBOLD.into()),
            vec![INTER_SEMIBOLD, CJK_SEMIBOLD, CJK_REGULAR, ICONS, EMOJI],
        ),
    ]
}

/// 一个候选字体文件，`index` 是字体集（.ttc）里的第几个字体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// 文件路径。
    pub path: PathBuf,
    /// 字体集里的序号。
    pub index: u32,
}

fn candidate(path: impl Into<PathBuf>, index: u32) -> Candidate {
    Candidate {
        path: path.into(),
        index,
    }
}

/// 这个系统上可能有的中文字体，按偏好排列。
#[cfg(windows)]
#[must_use]
pub fn candidates() -> Vec<Candidate> {
    let windir =
        std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let fonts = windir.join("Fonts");
    ["msyh.ttc", "msyh.ttf", "simhei.ttf", "simsun.ttc"]
        .into_iter()
        .map(|name| candidate(fonts.join(name), 0))
        .collect()
}

/// 这个系统上可能有的中文字体，按偏好排列。
#[cfg(target_os = "macos")]
#[must_use]
pub fn candidates() -> Vec<Candidate> {
    [
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/STHeiti Medium.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/Library/Fonts/Arial Unicode.ttf",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    ]
    .into_iter()
    .map(|path| candidate(path, 0))
    .collect()
}

/// 这个系统上可能有的中文字体，按偏好排列。Noto CJK 字体集里第 2 个是简体中文。
#[cfg(not(any(windows, target_os = "macos")))]
#[must_use]
pub fn candidates() -> Vec<Candidate> {
    let noto = [
        "/usr/share/fonts/opentype/noto",
        "/usr/share/fonts/noto-cjk",
        "/usr/share/fonts/google-noto-cjk",
        "/usr/share/fonts/OTF",
    ]
    .into_iter()
    .map(|dir| candidate(Path::new(dir).join("NotoSansCJK-Regular.ttc"), 2));
    let wqy = [
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
    ]
    .into_iter()
    .map(|path| candidate(path, 0));
    noto.chain(wqy).collect()
}

/// 依次读取候选字体，返回第一个读得到的。
pub fn load_first(
    candidates: &[Candidate],
    read: impl Fn(&Path) -> std::io::Result<Vec<u8>>,
) -> Option<(Candidate, Vec<u8>)> {
    candidates.iter().find_map(|c| {
        read(&c.path)
            .ok()
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| (c.clone(), bytes))
    })
}

/// 把系统字体加到每个字体族的最后，作为兜底：打进程序的字体里没有的字才用它。
pub fn install(defs: &mut FontDefinitions, found: &Candidate, bytes: Vec<u8>) {
    let mut data = FontData::from_owned(bytes);
    data.index = found.index;
    let _ = defs.font_data.insert(SYSTEM_CJK.to_owned(), Arc::new(data));
    for names in defs.families.values_mut() {
        names.push(SYSTEM_CJK.to_owned());
    }
}

/// 字体装好之后的情况。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// 兜底用的系统中文字体。`None` 表示没找到：常用字不受影响，生僻字会显示成方框。
    pub system_cjk: Option<PathBuf>,
}

/// 打进程序的字体加上找到的系统中文字体。没找到系统字体时写一条警告日志。
#[must_use]
pub fn definitions() -> (FontDefinitions, Installed) {
    let (defs, installed) = definitions_from(load_first(&candidates(), |p| std::fs::read(p)));
    if installed.system_cjk.is_none() {
        log::warn!(
            "没找到系统中文字体，生僻字会显示成方框；找过这些位置：{:?}",
            candidates().iter().map(|c| &c.path).collect::<Vec<_>>()
        );
    }
    (defs, installed)
}

fn definitions_from(found: Option<(Candidate, Vec<u8>)>) -> (FontDefinitions, Installed) {
    let mut defs = FontDefinitions::empty();
    for (name, bytes) in EMBEDDED {
        let mut data = FontData::from_static(bytes);
        if name == EMOJI {
            data.tweak = FontTweak {
                scale: EMOJI_SCALE,
                ..Default::default()
            };
        }
        let _ = defs.font_data.insert(name.to_owned(), Arc::new(data));
    }
    for (family, names) in families() {
        let _ = defs
            .families
            .insert(family, names.into_iter().map(str::to_owned).collect());
    }
    let system_cjk = found.map(|(c, bytes)| {
        install(&mut defs, &c, bytes);
        c.path
    });
    align(&mut defs);
    (defs, Installed { system_cjk })
}

/// 字体里有没有这个字（没有时 ab_glyph 给的是 .notdef）。读不懂的字体当作没有。
#[must_use]
pub fn has_glyph(data: &FontData, c: char) -> bool {
    FontRef::try_from_slice_and_index(&data.font, data.index)
        .is_ok_and(|font| font.glyph_id(c).0 != 0)
}

/// 量字形用的字号：大一些，取整误差可以忽略。
const MEASURE_PX: f32 = 1000.0;

/// 要对齐的字体和它的参照字形：图标用圆圈（Phosphor 的图标都画在同一个方框中心），中文用“中”。
const REFERENCES: [(&str, char); 5] = [
    (ICONS, '\u{E18A}'),
    (CJK_REGULAR, '中'),
    (CJK_MEDIUM, '中'),
    (CJK_SEMIBOLD, '中'),
    (SYSTEM_CJK, '中'),
];

/// 一个字形在一行里的竖直中心，从行顶往下量，单位是字号。
///
/// 照 egui 的排版规则算：后备字体的基线 = 它的 ascent + ½ ×（主字体行高 − 它的行高），
/// 再加上字形实际轮廓的上下边界。`row` 是主字体的行高（同一单位）。
fn glyph_center(data: &FontData, c: char, row: Option<f32>) -> Option<(f32, f32)> {
    let font = FontRef::try_from_slice_and_index(&data.font, data.index).ok()?;
    let scaled = font.as_scaled(MEASURE_PX);
    let height = (scaled.ascent() - scaled.descent() + scaled.line_gap()) / MEASURE_PX;
    let baseline = scaled.ascent() / MEASURE_PX + 0.5 * (row.unwrap_or(height) - height);
    // 字体里没有这个字时 ab_glyph 给的是 .notdef（方框），不能拿来量
    if font.glyph_id(c).0 == 0 {
        return None;
    }
    let glyph = scaled.scaled_glyph(c);
    let bounds = scaled.outline_glyph(glyph)?.px_bounds();
    Some((
        baseline + 0.5 * (bounds.min.y + bounds.max.y) / MEASURE_PX,
        height,
    ))
}

/// 让中文和图标与拉丁字母在一行里竖直居中。
///
/// 根因：egui 按字体的 ascent/descent 摆放后备字体，不看字形实际画在哪里。中文字体（思源黑体、
/// macOS 苹方、Windows 雅黑）和图标字体的度量和 Inter 差别很大，于是在按钮、标签里
/// 中文和图标整体偏下或偏上。这里量出每个后备字体参照字形的实际中心，
/// 用 `y_offset_factor` 把它移到主字体“H”的中心上，和系统、字体文件无关。
/// 三个字重的 Inter 竖直度量相同，所以只按正文的主字体量一次。
pub fn align(defs: &mut FontDefinitions) {
    let Some(primary) = defs
        .families
        .get(&FontFamily::Proportional)
        .and_then(|f| f.first())
        .and_then(|name| defs.font_data.get(name))
    else {
        return;
    };
    let Some((target, row)) = glyph_center(primary, 'H', None) else {
        return;
    };
    for (name, c) in REFERENCES {
        let Some(data) = defs.font_data.get_mut(name) else {
            continue;
        };
        if let Some((center, _)) = glyph_center(data, c, Some(row)) {
            Arc::make_mut(data).tweak.y_offset_factor = target - center;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;
    use crate::gui::theme::icon;

    #[test]
    fn picks_the_first_readable_candidate() {
        let list = vec![
            candidate("/missing", 0),
            candidate("/empty", 0),
            candidate("/good", 2),
            candidate("/also-good", 0),
        ];
        let read = |p: &Path| match p.to_str() {
            Some("/empty") => Ok(Vec::new()),
            Some("/good" | "/also-good") => Ok(vec![1, 2, 3]),
            Some(_) | None => Err(std::io::Error::other("no")),
        };
        let (found, bytes) = load_first(&list, read).unwrap();
        assert_eq!(found, candidate("/good", 2));
        assert_eq!(bytes, vec![1, 2, 3]);
        assert!(load_first(&list[..2], read).is_none());
    }

    #[test]
    fn system_font_is_the_last_fallback_of_every_family() {
        let (defs, installed) = definitions_from(Some((candidate("/x.ttc", 2), vec![0; 4])));
        assert_eq!(installed.system_cjk, Some(PathBuf::from("/x.ttc")));
        assert_eq!(defs.font_data[SYSTEM_CJK].index, 2);
        assert_eq!(defs.families.len(), 4);
        for names in defs.families.values() {
            assert_eq!(names.last().unwrap(), SYSTEM_CJK);
        }
        // 没找到系统字体：打进程序的字体照常可用，结果里说明没有兜底
        let (defs, installed) = definitions_from(None);
        assert!(installed.system_cjk.is_none() && !defs.font_data.contains_key(SYSTEM_CJK));
        for names in defs.families.values() {
            assert_eq!(names.last().unwrap(), EMOJI);
        }
    }

    /// 每个字体族的主字体是对应字重的 Inter，后面跟着同字重的中文、常规字重的中文、图标。
    #[test]
    fn each_weight_has_its_own_family() {
        let (defs, _) = definitions_from(None);
        for (family, latin, cjk) in [
            (FontFamily::Proportional, INTER_REGULAR, CJK_REGULAR),
            (FontFamily::Monospace, INTER_REGULAR, CJK_REGULAR),
            (FontFamily::Name(MEDIUM.into()), INTER_MEDIUM, CJK_MEDIUM),
            (
                FontFamily::Name(SEMIBOLD.into()),
                INTER_SEMIBOLD,
                CJK_SEMIBOLD,
            ),
        ] {
            let names = &defs.families[&family];
            assert_eq!((names[0].as_str(), names[1].as_str()), (latin, cjk));
            assert!(names.iter().any(|n| n == CJK_REGULAR));
            assert!(names.iter().any(|n| n == ICONS));
            for name in names {
                assert!(defs.font_data.contains_key(name), "{name}");
            }
        }
        assert!((defs.font_data[EMOJI].tweak.scale - EMOJI_SCALE).abs() < 1e-6);
    }

    /// 清单里点名的图标，字体子集里都有（少了就重新运行 scripts/build-fonts.py）。
    #[test]
    fn every_listed_icon_is_in_the_subset() {
        let (defs, _) = definitions_from(None);
        let icons = &defs.font_data[ICONS];
        for glyph in icon::ALL {
            let c = glyph.chars().next().unwrap();
            assert!(has_glyph(icons, c), "图标 U+{:04X} 不在子集里", c as u32);
        }
        assert!(!has_glyph(icons, '中'));
        assert!(!has_glyph(&FontData::from_static(&[0; 4]), 'A'));
    }

    /// 界面文案里的每个字都有字形：拉丁字母在 Inter 里，中文在思源黑体里，而且三个字重都有
    /// （少了就重新运行 scripts/build-fonts.py）。文案文件和脚本读的是同一批。
    #[test]
    fn every_character_of_the_copy_is_covered_in_all_weights() {
        let (defs, _) = definitions_from(None);
        let copy = [
            include_str!("i18n.rs"),
            include_str!("../../../platform/src/guide.rs"),
        ]
        .concat();
        // 中等、半粗字重只要求汉字齐全：个别符号（例如带圈数字）那个来源里没有，退回常规字重
        let han = |c: &char| ('\u{4E00}'..='\u{9FFF}').contains(c);
        for (latin, cjk, han_only) in [
            (INTER_REGULAR, CJK_REGULAR, false),
            (INTER_MEDIUM, CJK_MEDIUM, true),
            (INTER_SEMIBOLD, CJK_SEMIBOLD, true),
        ] {
            let missing: String = copy
                .chars()
                .filter(|c| !c.is_control() && !('\u{E000}'..'\u{F900}').contains(c))
                .filter(|c| !han_only || c.is_ascii() || han(c))
                .filter(|c| {
                    !has_glyph(&defs.font_data[latin], *c) && !has_glyph(&defs.font_data[cjk], *c)
                })
                .collect();
            assert!(missing.is_empty(), "{latin}/{cjk} 缺这些字：{missing}");
        }
        // 数字是等宽的：1 和 8 一样宽
        let font = FontRef::try_from_slice(&defs.font_data[INTER_REGULAR].font).unwrap();
        let scaled = font.as_scaled(MEASURE_PX);
        let width = |c: char| scaled.h_advance(font.glyph_id(c));
        assert!((width('1') - width('8')).abs() < 1e-3);
        assert!(width('i') < width('m'));
    }

    #[test]
    fn icons_and_cjk_are_centered_on_latin_text() {
        let (mut defs, _) = definitions_from(None);
        let primary = defs.families[&FontFamily::Proportional][0].clone();
        let (target, row) = glyph_center(&defs.font_data[&primary], 'H', None).unwrap();
        for (name, c) in REFERENCES {
            let Some(data) = defs.font_data.get(name) else {
                assert_eq!(name, SYSTEM_CJK);
                continue;
            };
            let (center, _) = glyph_center(data, c, Some(row)).unwrap();
            let shifted = center + data.tweak.y_offset_factor;
            assert!(
                (shifted - target).abs() < 1e-4,
                "{name}: {shifted} vs {target}"
            );
        }
        // 用主字体自己冒充系统字体：主字体里没有“中”，参照字形不存在的字体不改
        let copy = defs.font_data[&primary].clone();
        let _ = defs.font_data.insert(SYSTEM_CJK.into(), copy);
        align(&mut defs);
        assert!(defs.font_data[SYSTEM_CJK].tweak.y_offset_factor.abs() < 1e-6);
        // 没有主字体、主字体读不懂时什么都不做
        let mut empty = FontDefinitions::empty();
        align(&mut empty);
        let _ = empty
            .font_data
            .insert("bad".into(), Arc::new(FontData::from_static(&[0; 4])));
        let _ = empty
            .families
            .insert(FontFamily::Proportional, vec!["bad".into()]);
        align(&mut empty);
    }

    #[test]
    fn candidates_exist_for_this_system_and_lookup_does_not_fail() {
        assert!(!candidates().is_empty());
        let (defs, installed) = definitions();
        assert_eq!(
            defs.font_data.contains_key(SYSTEM_CJK),
            installed.system_cjk.is_some()
        );
    }
}
