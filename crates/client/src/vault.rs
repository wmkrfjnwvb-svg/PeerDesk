//! 保存的连接密码（加密）。
//!
//! 文件 `passwords.vault` 在数据目录里，内容用 XChaCha20-Poly1305 加密，文件里没有明文。密钥是随机生成的 32 字节，
//! 存在系统的钥匙串里（Windows 凭据管理器、macOS 钥匙串、Linux Secret Service，见 `keychain.rs`），不和文件放在一起。
//! 钥匙串用不了时不保存密码（[`VaultError::NoKeychain`]），界面说明原因，不退回明文。
//!
//! 以后多设备同步：只同步这个加密文件，密钥用账户密钥再包一层（端到端加密，服务器看不到内容）。
//! 这里只管本机的加密存取，同步时再加一层。
//!
//! 文件格式：`PDV1`（4 字节）+ 随机数（24 字节）+ 密文。明文是一行一条：`地址的十六进制\t密码的十六进制`。

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

/// 文件名（在数据目录里）。
pub const FILE: &str = "passwords.vault";
const MAGIC: &[u8; 4] = b"PDV1";
const NONCE_LEN: usize = 24;

/// 存放加密密钥的地方（系统钥匙串；测试里用内存）。
pub trait KeyStore {
    /// 读出密钥；还没有时为 `None`。
    ///
    /// # Errors
    /// 钥匙串用不了。
    fn load(&self) -> Result<Option<[u8; 32]>, String>;

    /// 保存密钥。
    ///
    /// # Errors
    /// 钥匙串用不了。
    fn store(&self, key: &[u8; 32]) -> Result<(), String>;
}

/// 打开或保存失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultError {
    /// 系统钥匙串用不了：不保存密码。
    NoKeychain(String),
    /// 文件读写失败。
    Io(String),
    /// 文件解不开（被改过，或者密钥换了）：不覆盖它，原样留着。
    Corrupt,
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoKeychain(why) => write!(f, "系统钥匙串用不了，不保存密码：{why}"),
            Self::Io(why) => write!(f, "保存的密码读写失败：{why}"),
            Self::Corrupt => write!(f, "保存的密码文件解不开（可能被改过或换了电脑），没有动它"),
        }
    }
}

/// 保存的密码：地址 → 密码。
pub struct Vault {
    path: PathBuf,
    cipher: XChaCha20Poly1305,
    entries: BTreeMap<String, String>,
}

impl fmt::Debug for Vault {
    // 不把密码打进日志
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vault")
            .field("path", &self.path)
            .field("entries", &self.entries.len())
            .finish_non_exhaustive()
    }
}

impl Vault {
    /// 打开数据目录里的密码文件；没有文件时是空的。钥匙串里还没有密钥时生成一个存进去。
    ///
    /// # Errors
    /// 钥匙串用不了、文件读不了或解不开。
    pub fn open(dir: &Path, keys: &dyn KeyStore) -> Result<Self, VaultError> {
        let path = dir.join(FILE);
        let key = match keys.load().map_err(VaultError::NoKeychain)? {
            Some(key) => key,
            None => {
                // 没有密钥但有文件：文件是用别的密钥加密的（例如钥匙串被清过），不能用新密钥覆盖它
                if path.exists() {
                    return Err(VaultError::Corrupt);
                }
                let mut key = [0u8; 32];
                getrandom::getrandom(&mut key)
                    .map_err(|e| VaultError::NoKeychain(e.to_string()))?;
                keys.store(&key).map_err(VaultError::NoKeychain)?;
                key
            }
        };
        let cipher = XChaCha20Poly1305::new(&key.into());
        let entries = match std::fs::read(&path) {
            Ok(bytes) => decrypt(&cipher, &bytes).ok_or(VaultError::Corrupt)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(VaultError::Io(e.to_string())),
        };
        Ok(Self {
            path,
            cipher,
            entries,
        })
    }

    /// `addr` 保存的密码。
    #[must_use]
    pub fn get(&self, addr: &str) -> Option<&str> {
        self.entries.get(addr.trim()).map(String::as_str)
    }

    /// 保存 `addr` 的密码（空密码等于删掉）。
    ///
    /// # Errors
    /// 写文件失败。
    pub fn put(&mut self, addr: &str, password: &str) -> Result<(), VaultError> {
        if password.is_empty() {
            return self.remove(addr);
        }
        let addr = addr.trim().to_owned();
        if self.entries.get(&addr).map(String::as_str) == Some(password) {
            return Ok(());
        }
        self.entries.insert(addr, password.to_owned());
        self.save()
    }

    /// 删掉 `addr` 的密码。
    ///
    /// # Errors
    /// 写文件失败。
    pub fn remove(&mut self, addr: &str) -> Result<(), VaultError> {
        if self.entries.remove(addr.trim()).is_none() {
            return Ok(());
        }
        self.save()
    }

    fn save(&self) -> Result<(), VaultError> {
        let bytes = encrypt(&self.cipher, &self.entries).map_err(VaultError::Io)?;
        // 先写临时文件再改名：写到一半断电也不会把原来的文件弄坏
        let tmp = self.path.with_extension("vault.tmp");
        std::fs::write(&tmp, bytes).map_err(|e| VaultError::Io(e.to_string()))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| VaultError::Io(e.to_string()))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            text.get(i..i + 2)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        })
        .collect()
}

fn encrypt(
    cipher: &XChaCha20Poly1305,
    entries: &BTreeMap<String, String>,
) -> Result<Vec<u8>, String> {
    let plain: String = entries
        .iter()
        .map(|(a, p)| format!("{}\t{}\n", hex(a.as_bytes()), hex(p.as_bytes())))
        .collect();
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce).map_err(|e| e.to_string())?;
    let sealed = cipher
        .encrypt(XNonce::from_slice(&nonce), plain.as_bytes())
        .map_err(|_| "加密失败".to_owned())?;
    let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + sealed.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

fn decrypt(cipher: &XChaCha20Poly1305, bytes: &[u8]) -> Option<BTreeMap<String, String>> {
    let rest = bytes.strip_prefix(MAGIC.as_slice())?;
    let nonce = rest.get(..NONCE_LEN)?;
    let sealed = rest.get(NONCE_LEN..)?;
    let plain = cipher.decrypt(XNonce::from_slice(nonce), sealed).ok()?;
    let text = String::from_utf8(plain).ok()?;
    let mut entries = BTreeMap::new();
    for line in text.lines() {
        let (a, p) = line.split_once('\t')?;
        let addr = String::from_utf8(unhex(a)?).ok()?;
        let password = String::from_utf8(unhex(p)?).ok()?;
        entries.insert(addr, password);
    }
    Some(entries)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Memory {
        key: RefCell<Option<[u8; 32]>>,
        broken: bool,
    }

    impl KeyStore for Memory {
        fn load(&self) -> Result<Option<[u8; 32]>, String> {
            if self.broken {
                return Err("没有 Secret Service".to_owned());
            }
            Ok(*self.key.borrow())
        }
        fn store(&self, key: &[u8; 32]) -> Result<(), String> {
            *self.key.borrow_mut() = Some(*key);
            Ok(())
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("peerdesk-vault-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn passwords_are_saved_encrypted_and_read_back() {
        let dir = temp_dir("roundtrip");
        let keys = Memory::default();
        let mut v = Vault::open(&dir, &keys).unwrap();
        assert!(v.get("482193507").is_none());
        v.put("482193507", "k7Qp93tw").unwrap();
        v.put(" 10.0.0.2:7000 ", "pa\tss\nword").unwrap();
        // 文件里没有明文
        let bytes = std::fs::read(dir.join(FILE)).unwrap();
        assert!(bytes.starts_with(MAGIC));
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("k7Qp93tw") && !text.contains("482193507"));
        // 再打开（同一个密钥）读得回来
        let again = Vault::open(&dir, &keys).unwrap();
        assert_eq!(again.get("482193507"), Some("k7Qp93tw"));
        assert_eq!(again.get("10.0.0.2:7000"), Some("pa\tss\nword"));
        assert!(format!("{again:?}").contains("entries: 2"));
        assert!(!format!("{again:?}").contains("k7Qp"));
        // 同样的密码不重写；空密码等于删掉；删不存在的不报错
        let mut again = again;
        again.put("482193507", "k7Qp93tw").unwrap();
        again.put("482193507", "").unwrap();
        assert!(again.get("482193507").is_none());
        again.remove("nobody").unwrap();
        let third = Vault::open(&dir, &keys).unwrap();
        assert!(third.get("482193507").is_none());
        assert_eq!(third.get("10.0.0.2:7000"), Some("pa\tss\nword"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_keychain_means_nothing_is_saved_and_a_tampered_file_is_left_alone() {
        let dir = temp_dir("broken");
        // 钥匙串用不了：打不开，说明原因
        let err = Vault::open(
            &dir,
            &Memory {
                broken: true,
                ..Memory::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, VaultError::NoKeychain(_)));
        assert!(err.to_string().contains("钥匙串"));
        assert!(!dir.join(FILE).exists());
        // 文件被改过：解不开，不覆盖
        let keys = Memory::default();
        let mut v = Vault::open(&dir, &keys).unwrap();
        v.put("a", "b").unwrap();
        let mut bytes = std::fs::read(dir.join(FILE)).unwrap();
        if let Some(last) = bytes.last_mut() {
            *last ^= 1;
        }
        std::fs::write(dir.join(FILE), &bytes).unwrap();
        assert_eq!(Vault::open(&dir, &keys).unwrap_err(), VaultError::Corrupt);
        assert!(VaultError::Corrupt.to_string().contains("没有动它"));
        // 钥匙串被清了（没有密钥）但文件还在：不拿新密钥覆盖
        assert_eq!(
            Vault::open(&dir, &Memory::default()).unwrap_err(),
            VaultError::Corrupt
        );
        assert_eq!(std::fs::read(dir.join(FILE)).unwrap(), bytes);
        // 读不了文件（是个目录）：读写错误
        let odd = temp_dir("odd");
        std::fs::create_dir_all(odd.join(FILE)).unwrap();
        let keys2 = Memory::default();
        keys2.store(&[7u8; 32]).unwrap();
        assert!(matches!(Vault::open(&odd, &keys2), Err(VaultError::Io(_))));
        assert!(VaultError::Io("x".into()).to_string().contains("读写失败"));
        // 别的格式、短文件、坏的十六进制都解不开
        let cipher = XChaCha20Poly1305::new(&[1u8; 32].into());
        assert!(decrypt(&cipher, b"XXXX").is_none());
        assert!(decrypt(&cipher, b"PDV1short").is_none());
        assert!(unhex("abc").is_none());
        assert!(unhex("zz").is_none());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&odd);
    }
}
