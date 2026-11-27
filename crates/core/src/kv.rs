//! 通用的“一行一个键值”文本的读写，加内容哈希。世界文件（`crates/e2e/src/world.rs`，一次仿真会话的全部输入）用它，
//! 以后别的文本输入文件（操作时间线、模型文件）也可以用。
//!
//! 格式：
//! - 一行一个键值：键（小写字母、数字、`_`、`.`、`-`）、空白、值（到行尾，两端的空白去掉）；
//! - 空行忽略，整行以 `#` 开头的是注释；
//! - 值里想放几个小字段（例如网络的一段链路），写成 `名=值` 用空白隔开（[`Kv::parse_fields`]）。
//!
//! 读的规矩（输入文件是会话的全部输入，宁可报错，不许静默用默认值）：
//! - 键重复、行写得不对、值解析不了：报错，带行号和键名；
//! - 缺的键：[`Kv::take`] 一类的函数报错；
//! - 没人认的键：调用 [`Kv::finish`] 时报错，列出全部多余的键。
//!
//! 写的规矩：[`KvWriter`] 按调用顺序逐行写，键的顺序由调用方固定，所以同一份输入写出来逐字节相同；
//! 哈希是 [`content_hash`]（blake3 取前 8 字节的 16 位十六进制），文件按它命名。

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// 读写出错，消息是写给人看的中文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvError(pub String);

impl fmt::Display for KvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for KvError {}

fn err<T>(msg: String) -> Result<T, KvError> {
    Err(KvError(msg))
}

/// 键合法吗：非空，只含小写字母、数字、`_`、`.`、`-`。
#[must_use]
pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
        })
}

#[derive(Debug, Clone)]
struct Entry {
    value: String,
    /// 在文件里的第几行（从 1 数）；小字段没有行号，用 0。
    line: usize,
    taken: bool,
}

/// 读出来的键值。逐个 `take` 走，最后 [`Self::finish`] 检查有没有没人认的键。
#[derive(Debug, Clone)]
pub struct Kv {
    entries: HashMap<String, Entry>,
    /// 出错时放在消息前面，说明是哪一份东西（空表示整个文件）。
    context: String,
}

impl Kv {
    /// 读整个文本。
    ///
    /// # Errors
    /// 某一行没有值、键不合法、键重复。
    pub fn parse_text(text: &str) -> Result<Self, KvError> {
        let mut entries: HashMap<String, Entry> = HashMap::new();
        for (i, raw) in text.lines().enumerate() {
            let line = i.saturating_add(1);
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let (key, value) = match trimmed.split_once(char::is_whitespace) {
                Some((k, v)) => (k, v.trim()),
                None => (trimmed, ""),
            };
            if !valid_key(key) {
                return err(format!(
                    "第 {line} 行：键 {key:?} 不合法（只能用小写字母、数字、_ . -）"
                ));
            }
            if value.is_empty() {
                return err(format!("第 {line} 行：键 {key} 没有值"));
            }
            if let Some(first) = entries.get(key) {
                return err(format!(
                    "第 {line} 行：键 {key} 重复（第一次在第 {} 行）",
                    first.line
                ));
            }
            let _ = entries.insert(
                key.to_owned(),
                Entry {
                    value: value.to_owned(),
                    line,
                    taken: false,
                },
            );
        }
        Ok(Self {
            entries,
            context: String::new(),
        })
    }

    /// 读一个值里的小字段：`名=值 名=值 ...`。`context` 写进错误消息（例如 `net.3`）。
    ///
    /// # Errors
    /// 字段不是 `名=值` 的形式、名字不合法或重复。
    pub fn parse_fields(value: &str, context: &str) -> Result<Self, KvError> {
        let mut entries: HashMap<String, Entry> = HashMap::new();
        for token in value.split_whitespace() {
            let Some((name, v)) = token.split_once('=') else {
                return err(format!("{context}：字段 {token:?} 不是 名=值 的形式"));
            };
            if !valid_key(name) {
                return err(format!("{context}：字段名 {name:?} 不合法"));
            }
            if v.is_empty() {
                return err(format!("{context}：字段 {name} 没有值"));
            }
            let entry = Entry {
                value: v.to_owned(),
                line: 0,
                taken: false,
            };
            if entries.insert(name.to_owned(), entry).is_some() {
                return err(format!("{context}：字段 {name} 重复"));
            }
        }
        Ok(Self {
            entries,
            context: context.to_owned(),
        })
    }

    /// 出错消息的前缀。
    fn at(&self, key: &str) -> String {
        let line = self.entries.get(key).map_or(0, |e| e.line);
        match (self.context.is_empty(), line) {
            (true, 0) => key.to_owned(),
            (true, n) => format!("第 {n} 行 {key}"),
            (false, _) => format!("{}：{key}", self.context),
        }
    }

    /// 有没有这个键（不算取走）。
    #[must_use]
    pub fn has(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// 取走一个必须有的值。
    ///
    /// # Errors
    /// 没有这个键。
    pub fn take(&mut self, key: &str) -> Result<String, KvError> {
        match self.entries.get_mut(key) {
            Some(e) => {
                e.taken = true;
                Ok(e.value.clone())
            }
            None => err(match self.context.is_empty() {
                true => format!("缺少键 {key}"),
                false => format!("{}：缺少字段 {key}", self.context),
            }),
        }
    }

    /// 取走一个数（或别的能从文本读出来的类型）。`what` 说明它应当是什么，写进错误消息。
    ///
    /// # Errors
    /// 没有这个键，或值读不出来。
    pub fn take_parsed<T: FromStr>(&mut self, key: &str, what: &str) -> Result<T, KvError> {
        let value = self.take(key)?;
        value
            .parse::<T>()
            .or_else(|_| err(format!("{}：值 {value:?} 不对，应当是{what}", self.at(key))))
    }

    /// 取走一个 u64。
    ///
    /// # Errors
    /// 没有这个键，或不是非负整数。
    pub fn take_u64(&mut self, key: &str) -> Result<u64, KvError> {
        self.take_parsed(key, "非负整数（0 到 18446744073709551615）")
    }

    /// 取走一个 u32。
    ///
    /// # Errors
    /// 没有这个键，或不是 0 到 4294967295 的整数。
    pub fn take_u32(&mut self, key: &str) -> Result<u32, KvError> {
        self.take_parsed(key, "非负整数（0 到 4294967295）")
    }

    /// 取走一个 u16。
    ///
    /// # Errors
    /// 没有这个键，或不是 0 到 65535 的整数。
    pub fn take_u16(&mut self, key: &str) -> Result<u16, KvError> {
        self.take_parsed(key, "非负整数（0 到 65535）")
    }

    /// 取走一个开关：`0` 或 `1`。
    ///
    /// # Errors
    /// 没有这个键，或不是 0、1。
    pub fn take_bool(&mut self, key: &str) -> Result<bool, KvError> {
        let value = self.take(key)?;
        match value.as_str() {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => err(format!(
                "{}：值 {value:?} 不对，应当是 0 或 1",
                self.at(key)
            )),
        }
    }

    /// 取走一个可以没有的 u64：`-` 表示没有。
    ///
    /// # Errors
    /// 没有这个键，或既不是 `-` 也不是非负整数。
    pub fn take_opt_u64(&mut self, key: &str) -> Result<Option<u64>, KvError> {
        let value = self.take(key)?;
        if value == "-" {
            return Ok(None);
        }
        value.parse::<u64>().map(Some).or_else(|_| {
            err(format!(
                "{}：值 {value:?} 不对，应当是 - 或非负整数",
                self.at(key)
            ))
        })
    }

    /// 取走一个“几选一”的值：`choices` 是（文本里的名字，对应的值）。
    ///
    /// # Errors
    /// 没有这个键，或名字不在 `choices` 里（消息列出全部可选的名字）。
    pub fn take_choice<T: Copy>(&mut self, key: &str, choices: &[(&str, T)]) -> Result<T, KvError> {
        let value = self.take(key)?;
        if let Some((_, v)) = choices.iter().find(|(name, _)| *name == value) {
            return Ok(*v);
        }
        let names: Vec<&str> = choices.iter().map(|(name, _)| *name).collect();
        err(format!(
            "{}：值 {value:?} 不认识，可选：{}",
            self.at(key),
            names.join("、")
        ))
    }

    /// 检查全部键都被取走了。
    ///
    /// # Errors
    /// 有没人认的键（消息列出全部，按行号排）。
    pub fn finish(self) -> Result<(), KvError> {
        let mut left: Vec<(&String, &Entry)> =
            self.entries.iter().filter(|(_, e)| !e.taken).collect();
        if left.is_empty() {
            return Ok(());
        }
        left.sort_by(|a, b| (a.1.line, a.0).cmp(&(b.1.line, b.0)));
        let list: Vec<String> = left
            .iter()
            .map(|(k, e)| {
                if e.line == 0 {
                    (*k).clone()
                } else {
                    format!("{k}（第 {} 行）", e.line)
                }
            })
            .collect();
        let prefix = if self.context.is_empty() {
            String::new()
        } else {
            format!("{}：", self.context)
        };
        err(format!("{prefix}不认识的键：{}", list.join("、")))
    }
}

/// 写“一行一个键值”的文本。键的顺序就是调用的顺序。
#[derive(Debug, Clone, Default)]
pub struct KvWriter {
    out: String,
}

impl KvWriter {
    /// 空文本。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 写一行注释（`text` 里不能有换行）。
    ///
    /// # Errors
    /// `text` 里有换行。
    pub fn comment(&mut self, text: &str) -> Result<(), KvError> {
        if text.contains(['\n', '\r']) {
            return err("注释里不能有换行".to_owned());
        }
        self.out.push_str("# ");
        self.out.push_str(text);
        self.out.push('\n');
        Ok(())
    }

    /// 写一行键值。
    ///
    /// # Errors
    /// 键不合法；值是空的、含换行、或两端有空白（读回来会不一样）。
    pub fn put(&mut self, key: &str, value: &str) -> Result<(), KvError> {
        if !valid_key(key) {
            return err(format!("键 {key:?} 不合法（只能用小写字母、数字、_ . -）"));
        }
        if value.is_empty() || value.contains(['\n', '\r']) || value.trim() != value {
            return err(format!(
                "键 {key} 的值 {value:?} 不能写进文本（空、含换行、或两端有空白）"
            ));
        }
        self.out.push_str(key);
        self.out.push(' ');
        self.out.push_str(value);
        self.out.push('\n');
        Ok(())
    }

    /// 写一行数（或别的能显示成文本的值）。
    ///
    /// # Errors
    /// 同 [`Self::put`]。
    pub fn put_display(&mut self, key: &str, value: impl fmt::Display) -> Result<(), KvError> {
        self.put(key, &value.to_string())
    }

    /// 写一个开关：`0` 或 `1`。
    ///
    /// # Errors
    /// 键不合法。
    pub fn put_bool(&mut self, key: &str, value: bool) -> Result<(), KvError> {
        self.put(key, if value { "1" } else { "0" })
    }

    /// 写一个可以没有的 u64：没有写 `-`。
    ///
    /// # Errors
    /// 键不合法。
    pub fn put_opt_u64(&mut self, key: &str, value: Option<u64>) -> Result<(), KvError> {
        match value {
            Some(v) => self.put_display(key, v),
            None => self.put(key, "-"),
        }
    }

    /// 取出写好的文本。
    #[must_use]
    pub fn finish(self) -> String {
        self.out
    }
}

/// 把小字段拼成一个值：`名=值 名=值`（读回来用 [`Kv::parse_fields`]）。
///
/// # Errors
/// 名字不合法，或值是空的、含空白（读回来会拆开）。
pub fn join_fields(fields: &[(&str, String)]) -> Result<String, KvError> {
    let mut parts = Vec::with_capacity(fields.len());
    for (name, value) in fields {
        if !valid_key(name) {
            return err(format!("字段名 {name:?} 不合法"));
        }
        if value.is_empty() || value.contains(char::is_whitespace) {
            return err(format!(
                "字段 {name} 的值 {value:?} 不能写进文本（空或含空白）"
            ));
        }
        parts.push(format!("{name}={value}"));
    }
    Ok(parts.join(" "))
}

/// 内容哈希：blake3 的前 8 字节，16 位小写十六进制。文件按它命名（同样的内容永远是同一个名字）。
#[must_use]
pub fn content_hash(bytes: &[u8]) -> String {
    let hex = blake3::hash(bytes).to_hex();
    hex.as_str().get(..16).unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lines_comments_and_blank_lines() {
        let mut kv =
            Kv::parse_text("# 注释\n\nname  hello world \nseed\t7\r\n  # 缩进的注释\n").unwrap();
        assert!(kv.has("name") && !kv.has("nope"));
        assert_eq!(kv.take("name").unwrap(), "hello world");
        assert_eq!(kv.take_u64("seed").unwrap(), 7);
        kv.finish().unwrap();
    }

    #[test]
    fn rejects_bad_lines_with_line_numbers() {
        let e = Kv::parse_text("a 1\nb\n").unwrap_err();
        assert_eq!(e.to_string(), "第 2 行：键 b 没有值");
        let e = Kv::parse_text("a 1\nBad 2\n").unwrap_err();
        assert!(e.0.starts_with("第 2 行：键 \"Bad\" 不合法"), "{e}");
        let e = Kv::parse_text("a 1\n\nb 2\na 3\n").unwrap_err();
        assert_eq!(e.to_string(), "第 4 行：键 a 重复（第一次在第 1 行）");
    }

    #[test]
    fn missing_and_unknown_keys_are_errors() {
        let mut kv = Kv::parse_text("x 1\nz 2\ny 3\n").unwrap();
        assert_eq!(kv.take("w").unwrap_err().to_string(), "缺少键 w");
        let _ = kv.take("x").unwrap();
        let e = kv.finish().unwrap_err();
        assert_eq!(e.to_string(), "不认识的键：z（第 2 行）、y（第 3 行）");
    }

    #[test]
    fn typed_getters_report_key_and_line() {
        let mut kv =
            Kv::parse_text("a 70000\nb 300\nc 5\nd -\ne 12\nf x\ng -3\nh 99999999999999999999\n")
                .unwrap();
        let e = kv.take_u16("a").unwrap_err();
        assert!(
            e.0.starts_with("第 1 行 a：值 \"70000\" 不对，应当是非负整数（0 到 65535）"),
            "{e}"
        );
        assert_eq!(kv.take_u32("b").unwrap(), 300);
        let e = kv.take_bool("c").unwrap_err();
        assert_eq!(e.to_string(), "第 3 行 c：值 \"5\" 不对，应当是 0 或 1");
        assert_eq!(kv.take_opt_u64("d").unwrap(), None);
        assert_eq!(kv.take_opt_u64("e").unwrap(), Some(12));
        let e = kv.take_opt_u64("f").unwrap_err();
        assert!(e.0.contains("应当是 - 或非负整数"), "{e}");
        let e = kv.take_u64("g").unwrap_err();
        assert!(e.0.starts_with("第 7 行 g："), "{e}");
        let e = kv.take_u32("h").unwrap_err();
        assert!(e.0.contains("0 到 4294967295"), "{e}");
        assert!(kv.take_u64("nope").is_err());
        assert!(kv.take_bool("nope").is_err());
        assert!(kv.take_opt_u64("nope").is_err());
    }

    #[test]
    fn bool_values() {
        let mut kv = Kv::parse_text("t 1\nf 0\n").unwrap();
        assert!(kv.take_bool("t").unwrap());
        assert!(!kv.take_bool("f").unwrap());
    }

    #[test]
    fn choices_list_the_options_on_error() {
        let choices = [("a", 1u8), ("b", 2)];
        let mut kv = Kv::parse_text("k b\nm c\n").unwrap();
        assert_eq!(kv.take_choice("k", &choices).unwrap(), 2);
        let e = kv.take_choice("m", &choices).unwrap_err();
        assert_eq!(e.to_string(), "第 2 行 m：值 \"c\" 不认识，可选：a、b");
        assert!(kv.take_choice("nope", &choices).is_err());
    }

    #[test]
    fn fields_parse_with_context() {
        let mut f = Kv::parse_fields("at=5  rate=-  up=1", "net.3").unwrap();
        assert_eq!(f.take_u64("at").unwrap(), 5);
        assert_eq!(f.take_opt_u64("rate").unwrap(), None);
        assert!(f.take_bool("up").unwrap());
        f.finish().unwrap();

        let mut f = Kv::parse_fields("at=x", "net.3").unwrap();
        assert!(f
            .take_u64("at")
            .unwrap_err()
            .0
            .starts_with("net.3：at：值 \"x\" 不对"));
        assert_eq!(
            f.take("gone").unwrap_err().to_string(),
            "net.3：缺少字段 gone"
        );
        let f = Kv::parse_fields("a=1 b=2 c=3", "net.3").unwrap();
        assert_eq!(
            f.finish().unwrap_err().to_string(),
            "net.3：不认识的键：a、b、c"
        );
    }

    #[test]
    fn bad_fields_are_rejected() {
        for (text, want) in [
            ("a", "net.0：字段 \"a\" 不是 名=值 的形式"),
            ("A=1", "net.0：字段名 \"A\" 不合法"),
            ("a=", "net.0：字段 a 没有值"),
            ("a=1 a=2", "net.0：字段 a 重复"),
        ] {
            assert_eq!(
                Kv::parse_fields(text, "net.0").unwrap_err().to_string(),
                want
            );
        }
        // 空的字段串合法（没有字段）
        Kv::parse_fields("  ", "net.0").unwrap().finish().unwrap();
    }

    #[test]
    fn writer_is_byte_stable_and_round_trips() {
        let build = || {
            let mut w = KvWriter::new();
            w.comment("头").unwrap();
            w.put("name", "a b").unwrap();
            w.put_display("seed", 9u64).unwrap();
            w.put_bool("on", true).unwrap();
            w.put_opt_u64("rate", None).unwrap();
            w.put_opt_u64("cap", Some(3)).unwrap();
            w.finish()
        };
        let text = build();
        assert_eq!(text, build());
        assert_eq!(text, "# 头\nname a b\nseed 9\non 1\nrate -\ncap 3\n");
        let mut kv = Kv::parse_text(&text).unwrap();
        assert_eq!(kv.take("name").unwrap(), "a b");
        assert_eq!(kv.take_u64("seed").unwrap(), 9);
        assert!(kv.take_bool("on").unwrap());
        assert_eq!(kv.take_opt_u64("rate").unwrap(), None);
        assert_eq!(kv.take_opt_u64("cap").unwrap(), Some(3));
        kv.finish().unwrap();
    }

    #[test]
    fn writer_refuses_what_would_not_read_back() {
        let mut w = KvWriter::new();
        assert!(w.comment("两\n行").is_err());
        assert!(w.put("Bad", "x").is_err());
        assert!(w.put("k", "").is_err());
        assert!(w.put("k", "a\nb").is_err());
        assert!(w.put("k", " a").is_err());
        assert!(w.put("k", "a ").is_err());
        assert_eq!(w.finish(), "");
    }

    #[test]
    fn join_fields_checks_names_and_values() {
        let s = join_fields(&[("a", "1".to_owned()), ("b", "-".to_owned())]).unwrap();
        assert_eq!(s, "a=1 b=-");
        assert_eq!(join_fields(&[]).unwrap(), "");
        assert!(join_fields(&[("A", "1".to_owned())]).is_err());
        assert!(join_fields(&[("a", String::new())]).is_err());
        assert!(join_fields(&[("a", "x y".to_owned())]).is_err());
    }

    #[test]
    fn hash_is_stable_and_content_dependent() {
        let h = content_hash(b"hello");
        assert_eq!(h.len(), 16);
        assert!(h
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(h, content_hash(b"hello"));
        assert_ne!(h, content_hash(b"hello\n"));
        // blake3("hello") 的前 8 字节
        assert_eq!(h, "ea8f163db38682925e4491c5e58d4bb3".get(..16).unwrap());
    }

    #[test]
    fn valid_keys() {
        for k in ["a", "net.12", "start_bps", "x-1"] {
            assert!(valid_key(k), "{k}");
        }
        for k in ["", "A", "a b", "中", "a=b"] {
            assert!(!valid_key(k), "{k}");
        }
    }
}
