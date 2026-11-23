//! 中文字体：egui 自带的字体没有中日韩字形，运行时从系统里找一个装上。找不到就只用自带字体。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ab_glyph::{Font as _, FontRef, ScaleFont as _};
use eframe::egui::{FontData, FontDefinitions, FontFamily};

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

/// 把字体加到两种字体族的最后，作为后备：拉丁字母仍用自带字体，中文用它。
pub fn install(defs: &mut FontDefinitions, found: &Candidate, bytes: Vec<u8>) {
    let mut data = FontData::from_owned(bytes);
    data.index = found.index;
    let name = "system-cjk".to_owned();
    let _ = defs.font_data.insert(name.clone(), Arc::new(data));
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        defs.families.entry(family).or_default().push(name.clone());
    }
}

/// 自带字体加上找到的系统中文字体。返回用了哪个文件（没找到时为 `None`）。
#[must_use]
pub fn definitions() -> (FontDefinitions, Option<PathBuf>) {
    definitions_from(load_first(&candidates(), |p| std::fs::read(p)))
}

fn definitions_from(found: Option<(Candidate, Vec<u8>)>) -> (FontDefinitions, Option<PathBuf>) {
    let mut defs = FontDefinitions::default();
    // Phosphor 图标字体（MIT）：图标按钮、导航用
    egui_phosphor::add_to_fonts(&mut defs, egui_phosphor::Variant::Regular);
    let used = found.map(|(c, bytes)| {
        install(&mut defs, &c, bytes);
        c.path
    });
    align(&mut defs);
    (defs, used)
}

/// 量字形用的字号：大一些，取整误差可以忽略。
const MEASURE_PX: f32 = 1000.0;

/// 后备字体和它的参照字形：图标用圆圈（Phosphor 的图标都画在同一个方框中心），中文用“中”。
const REFERENCES: [(&str, char); 2] = [("phosphor", '\u{E18A}'), ("system-cjk", '中')];

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
/// 根因：egui 按字体的 ascent/descent 摆放后备字体，不看字形实际画在哪里。各系统中文字体
/// （macOS 苹方、Windows 雅黑、Linux Noto）和图标字体的度量差别很大，于是在按钮、标签里
/// 中文和图标整体偏下或偏上。这里在启动时量出每个后备字体参照字形的实际中心，
/// 用 `y_offset_factor` 把它移到主字体“H”的中心上，和系统、字体文件无关。
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
    use super::*;

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
    fn installs_as_a_fallback_for_both_families() {
        let mut defs = FontDefinitions::default();
        install(&mut defs, &candidate("/x.ttc", 2), vec![0; 4]);
        assert_eq!(defs.font_data["system-cjk"].index, 2);
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            assert_eq!(defs.families[&family].last().unwrap(), "system-cjk");
        }
    }

    #[test]
    fn definitions_with_and_without_a_cjk_font() {
        let (defs, used) = definitions_from(Some((candidate("/x.ttc", 0), vec![0; 4])));
        assert_eq!(used, Some(PathBuf::from("/x.ttc")));
        assert!(defs.font_data.contains_key("system-cjk"));
        let (defs, used) = definitions_from(None);
        assert!(used.is_none() && !defs.font_data.contains_key("system-cjk"));
    }

    #[test]
    fn icons_and_cjk_are_centered_on_latin_text() {
        let (mut defs, _) = definitions_from(None);
        // 用主字体自己冒充一个中文后备字体：对齐后偏移应为 0
        let primary = defs.families[&FontFamily::Proportional][0].clone();
        let copy = defs.font_data[&primary].clone();
        let _ = defs.font_data.insert("system-cjk".into(), copy);
        // 参照字形不存在的字体不改（主字体里没有“中”）
        align(&mut defs);
        assert!(defs.font_data["system-cjk"].tweak.y_offset_factor.abs() < 1e-6);
        let (target, row) = glyph_center(&defs.font_data[&primary], 'H', None).unwrap();
        let icons = &defs.font_data["phosphor"];
        let (center, _) = glyph_center(icons, '\u{E18A}', Some(row)).unwrap();
        let shifted = center + icons.tweak.y_offset_factor;
        assert!((shifted - target).abs() < 1e-4, "{shifted} vs {target}");
        // 没有主字体时什么都不做
        let mut empty = FontDefinitions::empty();
        align(&mut empty);
    }

    #[test]
    fn candidates_exist_for_this_system_and_lookup_does_not_fail() {
        assert!(!candidates().is_empty());
        let (defs, used) = definitions();
        assert_eq!(defs.font_data.contains_key("system-cjk"), used.is_some());
    }
}
