//! 文件管理窗口的状态（不画界面）：一栏的路径、列表、选择、上一级、进入文件夹；大小和时间的显示。
//!
//! 对方的路径用对方系统的分隔符：路径里有反斜杠（例如 `C:\Users`）就当 Windows，否则用 `/`。

use std::collections::BTreeSet;

use peerdesk_core::browse::{Entry, Place, Reply};

/// 一栏（本机或对方）。
#[derive(Debug, Default, Clone)]
pub struct Pane {
    /// 当前路径（可以手输）。
    pub path: String,
    /// 列表。
    pub entries: Vec<Entry>,
    /// 列表被截断了（文件太多）。
    pub truncated: bool,
    /// 选中的名字。
    pub selected: BTreeSet<String>,
    /// 出错原因（列不出来、新建失败……）。
    pub error: Option<String>,
    /// 预置位置。
    pub places: Vec<Place>,
    /// 正在等对方回答。
    pub loading: bool,
}

/// 路径用哪个分隔符。
fn separator(path: &str) -> char {
    if path.contains('\\') && !path.starts_with('/') {
        '\\'
    } else {
        '/'
    }
}

/// `path` 下的 `name`。
#[must_use]
pub fn child(path: &str, name: &str) -> String {
    let sep = separator(path);
    if path.ends_with(sep) {
        format!("{path}{name}")
    } else {
        format!("{path}{sep}{name}")
    }
}

/// 上一级；已经是根（`/`、`C:\`）时为 `None`。
#[must_use]
pub fn parent(path: &str) -> Option<String> {
    let sep = separator(path);
    let trimmed = path.trim_end_matches(sep);
    if trimmed.is_empty() || (sep == '\\' && trimmed.ends_with(':') && !trimmed.contains('\\')) {
        return None;
    }
    let cut = trimmed.rfind(sep)?;
    let up = trimmed.get(..cut).unwrap_or_default();
    Some(if up.is_empty() {
        sep.to_string()
    } else if sep == '\\' && up.ends_with(':') && !up.contains('\\') {
        format!("{up}\\")
    } else {
        up.to_owned()
    })
}

impl Pane {
    /// 收到列表（或失败）：换掉内容，清掉选择。
    pub fn set_listing(&mut self, path: &str, reply: Reply) {
        self.loading = false;
        match reply {
            Reply::List { entries, truncated } => {
                path.clone_into(&mut self.path);
                self.entries = entries;
                self.truncated = truncated;
                self.selected.clear();
                self.error = None;
            }
            Reply::Failed(why) => self.error = Some(why),
            Reply::Places(_) | Reply::Done | Reply::Ready => {
                self.error = Some("对方的答复不对".into());
            }
        }
    }

    /// 点了一项：`add` 为真（按着 Ctrl/Cmd）时加选或取消，否则只选它。
    pub fn click(&mut self, name: &str, add: bool) {
        if add {
            if !self.selected.remove(name) {
                self.selected.insert(name.to_owned());
            }
        } else {
            self.selected.clear();
            self.selected.insert(name.to_owned());
        }
    }

    /// 双击的那一项如果是文件夹，返回它的路径（进入）。
    #[must_use]
    pub fn open(&self, name: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|e| e.name == name && e.dir)
            .map(|_| child(&self.path, name))
    }

    /// 选中的名字（按列表顺序）。
    #[must_use]
    pub fn selected_names(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| self.selected.contains(&e.name))
            .map(|e| e.name.clone())
            .collect()
    }
}

/// 字节数给人看：B、KB、MB、GB、TB。
#[must_use]
pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let name = UNITS.get(unit).copied().unwrap_or("B");
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {name}")
    }
}

/// 秒数给人看：1:05、1:02:03。
#[must_use]
pub fn duration_text(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Unix 秒给人看（本地时区不好取，用 UTC 日期和时间，够区分新旧）。
#[must_use]
pub fn time_text(unix: u64) -> String {
    if unix == 0 {
        return String::new();
    }
    let days = unix / 86_400;
    let (y, m, d) = civil(i64::try_from(days).unwrap_or(0));
    let secs = unix % 86_400;
    format!("{y}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, secs / 60 % 60)
}

/// 从 1970-01-01 起的天数 → 公历年月日（Howard Hinnant 的算法）。
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (
        y,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn paths_in_both_styles() {
        assert_eq!(child("/home/u", "a"), "/home/u/a");
        assert_eq!(child("/", "a"), "/a");
        assert_eq!(child("C:\\Users", "李"), "C:\\Users\\李");
        assert_eq!(child("C:\\", "x"), "C:\\x");
        assert_eq!(parent("/home/u"), Some("/home".into()));
        assert_eq!(parent("/home"), Some("/".into()));
        assert_eq!(parent("/home/u/"), Some("/home".into()));
        assert_eq!(parent("/"), None);
        assert_eq!(parent("C:\\Users\\李"), Some("C:\\Users".into()));
        assert_eq!(parent("C:\\Users"), Some("C:\\".into()));
        assert_eq!(parent("C:\\"), None);
        assert_eq!(parent("relative"), None);
    }

    #[test]
    fn listing_selection_and_open() {
        let e = |name: &str, dir| Entry {
            name: name.into(),
            dir,
            size: 1,
            modified: 0,
        };
        let mut pane = Pane::default();
        pane.set_listing(
            "/d",
            Reply::List {
                entries: vec![e("夹", true), e("a", false), e("b", false)],
                truncated: false,
            },
        );
        assert_eq!(pane.path, "/d");
        pane.click("b", false);
        pane.click("a", true);
        assert_eq!(pane.selected_names(), ["a", "b"]);
        pane.click("a", true);
        assert_eq!(pane.selected_names(), ["b"]);
        pane.click("夹", false);
        assert_eq!(pane.selected_names(), ["夹"]);
        assert_eq!(pane.open("夹"), Some("/d/夹".into()));
        assert_eq!(pane.open("a"), None);
        // 失败：保留原来的列表，显示原因
        pane.set_listing("/x", Reply::Failed("没有权限".into()));
        assert_eq!(pane.path, "/d");
        assert_eq!(pane.error.as_deref(), Some("没有权限"));
        pane.set_listing("/x", Reply::Done);
        assert!(pane.error.is_some());
    }

    #[test]
    fn human_readable() {
        assert_eq!(size_text(0), "0 B");
        assert_eq!(size_text(1023), "1023 B");
        assert_eq!(size_text(1536), "1.5 KB");
        assert_eq!(size_text(5 * 1024 * 1024 * 1024), "5.0 GB");
        assert_eq!(size_text(u64::MAX), "16777216.0 TB");
        assert_eq!(duration_text(65), "1:05");
        assert_eq!(duration_text(3723), "1:02:03");
        assert_eq!(time_text(0), "");
        assert_eq!(time_text(1_700_000_000), "2023-11-14 22:13");
        assert_eq!(time_text(951_782_400), "2000-02-29 00:00");
    }
}
