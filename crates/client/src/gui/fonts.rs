//! 中文字体：egui 自带的字体没有中日韩字形，运行时从系统里找一个装上。找不到就只用自带字体。

use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    (defs, used)
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
    fn candidates_exist_for_this_system_and_lookup_does_not_fail() {
        assert!(!candidates().is_empty());
        let (defs, used) = definitions();
        assert_eq!(defs.font_data.contains_key("system-cjk"), used.is_some());
    }
}
