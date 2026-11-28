//! 手机端界面认得核心的每个原因：Kotlin 里有同样的代号和按钮（`Reasons.kt`），中英文的
//! 一句原因、一句办法都有（`strings.xml`），中文里没有英文原文。核心加了原因而手机端没跟上时，这里失败。
#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;

use peerdesk_net::reason::{Action, Reason};

fn android(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../android/app/src/main")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读不了 {}：{e}", path.display()))
}

/// `strings.xml` 里某个名字的文字（还原 `\'`、`&amp;` 这些转义）。
fn string(xml: &str, name: &str) -> Option<String> {
    let start = format!("<string name=\"{name}\">");
    let from = xml.find(&start)? + start.len();
    let len = xml.get(from..)?.find("</string>")?;
    Some(
        xml.get(from..from + len)?
            .replace("\\'", "'")
            .replace("&lt;", "<")
            .replace("&amp;", "&"),
    )
}

fn has_cjk(text: &str) -> bool {
    text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// 中文里允许的英文：产品名和通用写法。
fn has_ascii_word(text: &str) -> bool {
    let rest = text.replace("PeerDesk", "").replace("Wi-Fi", "");
    rest.chars().any(|c| c.is_ascii_alphabetic())
}

#[test]
fn every_reason_has_android_texts_in_both_languages() {
    let en = android("res/values/strings.xml");
    let zh = android("res/values-zh-rCN/strings.xml");
    for r in Reason::ALL {
        for part in ["cause", "fix"] {
            let name = format!("reason_{}_{part}", r.code());
            let e = string(&en, &name).unwrap_or_else(|| panic!("英文缺 {name}"));
            let z = string(&zh, &name).unwrap_or_else(|| panic!("中文缺 {name}"));
            assert!(!has_cjk(&e), "{name}：{e}");
            assert!(has_cjk(&z) && !has_ascii_word(&z), "{name}：{z}");
        }
    }
}

#[test]
fn kotlin_knows_every_code_and_the_same_buttons() {
    let kt = android("java/ai/peerdesk/app/Reasons.kt");
    for r in Reason::ALL {
        let line = kt
            .lines()
            .find(|l| l.contains(&format!("(\"{}\"", r.code())))
            .unwrap_or_else(|| panic!("Reasons.kt 里没有 {}", r.code()));
        let want: Vec<&str> = r
            .actions()
            .iter()
            .map(|a| match a {
                Action::Retry => "ReasonAction.Retry",
                Action::ServerSettings => "ReasonAction.ServerSettings",
                Action::Diagnostics => "ReasonAction.Diagnostics",
            })
            .collect();
        let list = line
            .rsplit_once("listOf(")
            .and_then(|(_, rest)| rest.split_once(')'))
            .map(|(inside, _)| inside)
            .unwrap_or_else(|| panic!("{line}"));
        let got: Vec<&str> = list
            .split(',')
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .collect();
        assert_eq!(got, want, "{}", r.code());
    }
}
