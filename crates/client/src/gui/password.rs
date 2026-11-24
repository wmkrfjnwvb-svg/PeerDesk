//! 会话密码：8 个字符，只用不容易看错的字符（没有 0/o、1/l/i）。
//!
//! 存在数据目录里（和设备身份放在一起，只有本用户能读），重启后不变；用户点“换一个”才换。

use std::path::Path;

/// 数据目录里保存密码的文件名。
pub const FILE: &str = "session-password";

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

/// 是不是一个合法的密码（长度对、只用字母表里的字符）。
#[must_use]
pub fn is_valid(password: &str) -> bool {
    password.len() == LEN && password.bytes().all(|c| ALPHABET.contains(&c))
}

/// 读出上次的密码；没有或文件坏了就生成一个新的并保存。
///
/// # Errors
/// 系统随机数不可用，或者存不下来（存不下来时重启会换密码，要告诉用户）。
pub fn load_or_create(dir: &Path) -> Result<String, String> {
    if let Ok(text) = std::fs::read_to_string(dir.join(FILE)) {
        let text = text.trim();
        if is_valid(text) {
            return Ok(text.to_owned());
        }
    }
    let password = generate()?;
    save(dir, &password)?;
    Ok(password)
}

/// 保存密码（用户换了密码时调用）。Unix 上文件只有本用户能读写。
///
/// # Errors
/// 写不进数据目录。
pub fn save(dir: &Path, password: &str) -> Result<(), String> {
    let path = dir.join(FILE);
    std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(&path, password))
        .map_err(|e| format!("密码存不下来（{}）：{e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("密码文件权限设不了（{}）：{e}", path.display()))?;
    }
    Ok(())
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
    fn survives_restarts_until_changed() {
        let dir = std::env::temp_dir().join(format!("peerdesk-pw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = load_or_create(&dir).unwrap();
        assert_eq!(load_or_create(&dir).unwrap(), first, "重启后不变");
        let next = generate().unwrap();
        save(&dir, &next).unwrap();
        assert_eq!(load_or_create(&dir).unwrap(), next, "换了之后记住新的");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.join(FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "只有本用户能读");
        }
        // 文件坏了：换一个新的
        std::fs::write(dir.join(FILE), "0000").unwrap();
        let fresh = load_or_create(&dir).unwrap();
        assert!(is_valid(&fresh));
        assert_ne!(fresh, "0000");
        // 存不下来（目录是个文件）：报错说明
        let blocked = dir.join(FILE).join("x");
        assert!(load_or_create(&blocked).unwrap_err().contains("存不下来"));
        let _ = std::fs::remove_dir_all(&dir);
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
