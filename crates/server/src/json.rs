//! 极小的 JSON 输出工具。服务端只需要输出几种固定结构，不值得为此引入序列化库。

use std::fmt::Write as _;

/// 把字符串转成带引号、已转义的 JSON 字符串。
#[must_use]
pub fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(2));
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                // 向 String 写入不会失败。
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes() {
        assert_eq!(string("plain"), "\"plain\"");
        assert_eq!(string(""), "\"\"");
        assert_eq!(string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(string("l1\nl2\r\tx"), "\"l1\\nl2\\r\\tx\"");
        assert_eq!(string("\u{1}"), "\"\\u0001\"");
        assert_eq!(string("区域"), "\"区域\"");
    }
}
