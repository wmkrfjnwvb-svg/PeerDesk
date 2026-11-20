//! 会话密码：8 个字符，只用不容易看错的字符（没有 0/o、1/l/i）。

/// 可用的字符。
pub const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
/// 密码长度。被控端要求至少 8 个字符。
pub const LEN: usize = 8;

/// 从随机字节里取字符。为了均匀，丢弃落在字母表整数倍之外的字节。
/// 字节不够时返回 `None`。
pub fn from_bytes(bytes: impl IntoIterator<Item = u8>) -> Option<String> {
    let n = ALPHABET.len();
    // 256 以下最大的、字母表长度的整数倍。
    let limit = 256 - 256 % n;
    let password: String = bytes
        .into_iter()
        .filter(|b| usize::from(*b) < limit)
        .filter_map(|b| ALPHABET.get(usize::from(b) % n).copied().map(char::from))
        .take(LEN)
        .collect();
    (password.len() == LEN).then_some(password)
}

/// 生成一个新密码。系统随机数不可用时返回错误说明。
pub fn generate() -> Result<String, String> {
    loop {
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes).map_err(|e| e.to_string())?;
        if let Some(password) = from_bytes(bytes) {
            return Ok(password);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_only_the_unambiguous_alphabet() {
        for c in "01ilo".bytes() {
            assert!(!ALPHABET.contains(&c), "{c}");
        }
        let a = generate().unwrap();
        let b = generate().unwrap();
        assert_eq!(a.len(), LEN);
        assert!(a.bytes().all(|c| ALPHABET.contains(&c)));
        assert_ne!(a, b);
    }

    #[test]
    fn rejects_biased_bytes_and_needs_enough_of_them() {
        // 31 个字符：248 及以上的字节被丢弃。
        let bytes = [255, 248, 0, 1, 30, 31, 62, 247, 2, 3, 4];
        assert_eq!(from_bytes(bytes).unwrap(), "ab9aa9cd");
        assert!(from_bytes([0u8; 7]).is_none());
        assert!(from_bytes([250u8; 64]).is_none());
    }
}
