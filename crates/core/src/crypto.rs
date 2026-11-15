//! 可配置的加密套件。
//!
//! 用户可以选择套件，包括“不加密”。可选的加密算法只有公开标准化的 AEAD：
//! AES-GCM（NIST SP 800-38D）和 ChaCha20-Poly1305（RFC 8439）。不提供自创算法。
//!
//! 协商规则防止被动降级：只要两端有共同的加密套件，就绝不会选中“不加密”。

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes128Gcm, Aes256Gcm, Nonce};
use chacha20poly1305::ChaCha20Poly1305;
use std::fmt;

use crate::Error;

/// 加密套件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Suite {
    /// 不加密。只在两端都明确允许时才会被选中。
    None,
    /// AES-128-GCM。
    Aes128Gcm,
    /// AES-256-GCM。
    Aes256Gcm,
    /// ChaCha20-Poly1305。
    ChaCha20Poly1305,
}

impl Suite {
    /// 全部套件。
    pub const ALL: [Suite; 4] = [
        Suite::None,
        Suite::Aes128Gcm,
        Suite::Aes256Gcm,
        Suite::ChaCha20Poly1305,
    ];

    /// 线上编号。
    #[must_use]
    pub const fn id(self) -> u8 {
        match self {
            Suite::None => 0,
            Suite::Aes128Gcm => 1,
            Suite::Aes256Gcm => 2,
            Suite::ChaCha20Poly1305 => 3,
        }
    }

    /// 由线上编号解析。
    pub const fn from_id(id: u8) -> Result<Self, Error> {
        match id {
            0 => Ok(Suite::None),
            1 => Ok(Suite::Aes128Gcm),
            2 => Ok(Suite::Aes256Gcm),
            3 => Ok(Suite::ChaCha20Poly1305),
            other => Err(Error::UnknownSuite(other)),
        }
    }

    /// 配置文件里使用的名字。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Suite::None => "none",
            Suite::Aes128Gcm => "aes-128-gcm",
            Suite::Aes256Gcm => "aes-256-gcm",
            Suite::ChaCha20Poly1305 => "chacha20-poly1305",
        }
    }

    /// 由配置名解析，不区分大小写。
    pub fn parse(name: &str) -> Result<Self, Error> {
        let lower = name.trim().to_ascii_lowercase();
        Suite::ALL
            .into_iter()
            .find(|s| s.name() == lower)
            .ok_or_else(|| Error::UnknownSuiteName(name.to_owned()))
    }

    /// 密钥长度（字节）。
    #[must_use]
    pub const fn key_len(self) -> usize {
        match self {
            Suite::None => 0,
            Suite::Aes128Gcm => 16,
            Suite::Aes256Gcm | Suite::ChaCha20Poly1305 => 32,
        }
    }

    /// 认证标签带来的额外长度（字节）。
    #[must_use]
    pub const fn overhead(self) -> usize {
        match self {
            Suite::None => 0,
            Suite::Aes128Gcm | Suite::Aes256Gcm | Suite::ChaCha20Poly1305 => 16,
        }
    }

    /// 是否加密。
    #[must_use]
    pub const fn is_encrypted(self) -> bool {
        match self {
            Suite::None => false,
            Suite::Aes128Gcm | Suite::Aes256Gcm | Suite::ChaCha20Poly1305 => true,
        }
    }

    /// 算法本身是否属于 FIPS 批准的算法。
    ///
    /// 注意：这只说明算法，不代表本实现通过了 FIPS 140 模块认证。
    #[must_use]
    pub const fn is_fips_algorithm(self) -> bool {
        match self {
            Suite::Aes128Gcm | Suite::Aes256Gcm => true,
            Suite::None | Suite::ChaCha20Poly1305 => false,
        }
    }
}

impl fmt::Display for Suite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// 本端允许的套件，按偏好从高到低排列，无重复。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    allowed: Vec<Suite>,
}

impl Policy {
    /// 推荐配置：只允许加密，AES-256-GCM 优先。
    #[must_use]
    pub fn recommended() -> Self {
        Self {
            allowed: vec![Suite::Aes256Gcm, Suite::ChaCha20Poly1305, Suite::Aes128Gcm],
        }
    }

    /// 只用 FIPS 批准算法的配置。
    #[must_use]
    pub fn fips() -> Self {
        Self {
            allowed: vec![Suite::Aes256Gcm, Suite::Aes128Gcm],
        }
    }

    /// 不加密。只应在受信任的内网或调试时使用。
    #[must_use]
    pub fn none() -> Self {
        Self {
            allowed: vec![Suite::None],
        }
    }

    /// 自定义列表，按给定顺序作为偏好，自动去重；不能为空。
    pub fn custom(suites: &[Suite]) -> Result<Self, Error> {
        let mut allowed = Vec::with_capacity(suites.len());
        for suite in suites {
            if !allowed.contains(suite) {
                allowed.push(*suite);
            }
        }
        if allowed.is_empty() {
            return Err(Error::EmptyPolicy);
        }
        Ok(Self { allowed })
    }

    /// 由预设名构造：`recommended`、`fips`、`none`。
    pub fn preset(name: &str) -> Result<Self, Error> {
        match name.trim().to_ascii_lowercase().as_str() {
            "recommended" => Ok(Self::recommended()),
            "fips" => Ok(Self::fips()),
            "none" => Ok(Self::none()),
            other => Err(Error::Config(format!("未知的加密预设：{other}"))),
        }
    }

    /// 允许的套件，按偏好排列。
    #[must_use]
    pub fn suites(&self) -> &[Suite] {
        &self.allowed
    }

    /// 是否允许某个套件。
    #[must_use]
    pub fn allows(&self, suite: Suite) -> bool {
        self.allowed.contains(&suite)
    }

    /// 是否允许不加密。
    #[must_use]
    pub fn allows_plaintext(&self) -> bool {
        self.allows(Suite::None)
    }
}

/// 在本端策略和对端提供的列表之间选出套件。
///
/// 先按本端偏好找共同的加密套件；只有不存在共同的加密套件、且两端都列出了
/// [`Suite::None`] 时才返回不加密。
pub fn negotiate(local: &Policy, remote: &[Suite]) -> Result<Suite, Error> {
    let common = |s: &&Suite| remote.contains(s);
    local
        .suites()
        .iter()
        .filter(|s| s.is_encrypted())
        .find(common)
        .or_else(|| {
            local
                .suites()
                .iter()
                .filter(|s| !s.is_encrypted())
                .find(common)
        })
        .copied()
        .ok_or(Error::NoCommonSuite)
}

enum Cipher {
    None,
    Aes128(Box<Aes128Gcm>),
    Aes256(Box<Aes256Gcm>),
    ChaCha(Box<ChaCha20Poly1305>),
}

/// 按包加解密。随机数由 4 字节方向前缀和 8 字节包序号组成，同一密钥下序号不得重复。
pub struct Sealer {
    suite: Suite,
    cipher: Cipher,
    prefix: [u8; 4],
}

impl fmt::Debug for Sealer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 不输出密钥材料。
        f.debug_struct("Sealer")
            .field("suite", &self.suite)
            .finish()
    }
}

impl Sealer {
    /// 创建。`key` 的长度必须等于套件要求；`direction` 用来区分两个发送方向。
    pub fn new(suite: Suite, key: &[u8], direction: u32) -> Result<Self, Error> {
        let bad_len = |_| Error::KeyLength {
            need: suite.key_len(),
            have: key.len(),
        };
        let cipher = match suite {
            Suite::None => {
                if !key.is_empty() {
                    return Err(Error::KeyLength {
                        need: 0,
                        have: key.len(),
                    });
                }
                Cipher::None
            }
            Suite::Aes128Gcm => {
                Cipher::Aes128(Box::new(Aes128Gcm::new_from_slice(key).map_err(bad_len)?))
            }
            Suite::Aes256Gcm => {
                Cipher::Aes256(Box::new(Aes256Gcm::new_from_slice(key).map_err(bad_len)?))
            }
            Suite::ChaCha20Poly1305 => Cipher::ChaCha(Box::new(
                ChaCha20Poly1305::new_from_slice(key).map_err(bad_len)?,
            )),
        };
        Ok(Self {
            suite,
            cipher,
            prefix: direction.to_be_bytes(),
        })
    }

    /// 使用的套件。
    #[must_use]
    pub const fn suite(&self) -> Suite {
        self.suite
    }

    fn nonce(&self, seq: u64) -> [u8; 12] {
        let mut nonce = [0u8; 12];
        let (head, tail) = nonce.split_at_mut(4);
        head.copy_from_slice(&self.prefix);
        tail.copy_from_slice(&seq.to_be_bytes());
        nonce
    }

    /// 加密并认证。`aad` 是只认证不加密的关联数据（通常是包头）。
    pub fn seal(&self, seq: u64, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, Error> {
        let nonce = self.nonce(seq);
        let nonce = Nonce::from_slice(&nonce);
        let payload = Payload {
            msg: plaintext,
            aad,
        };
        match &self.cipher {
            Cipher::None => Ok(plaintext.to_vec()),
            Cipher::Aes128(c) => c.encrypt(nonce, payload).map_err(|_| Error::SealFailed),
            Cipher::Aes256(c) => c.encrypt(nonce, payload).map_err(|_| Error::SealFailed),
            Cipher::ChaCha(c) => c.encrypt(nonce, payload).map_err(|_| Error::SealFailed),
        }
    }

    /// 解密并校验。任何篡改（密文、关联数据、序号）都会失败。
    pub fn open(&self, seq: u64, aad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, Error> {
        let nonce = self.nonce(seq);
        let nonce = Nonce::from_slice(&nonce);
        let payload = Payload {
            msg: ciphertext,
            aad,
        };
        match &self.cipher {
            Cipher::None => Ok(ciphertext.to_vec()),
            Cipher::Aes128(c) => c.decrypt(nonce, payload).map_err(|_| Error::OpenFailed),
            Cipher::Aes256(c) => c.decrypt(nonce, payload).map_err(|_| Error::OpenFailed),
            Cipher::ChaCha(c) => c.decrypt(nonce, payload).map_err(|_| Error::OpenFailed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suite_ids_and_names_roundtrip() {
        for suite in Suite::ALL {
            assert_eq!(Suite::from_id(suite.id()).unwrap(), suite);
            assert_eq!(Suite::parse(suite.name()).unwrap(), suite);
            assert_eq!(suite.to_string(), suite.name());
            assert_eq!(suite.is_encrypted(), suite.key_len() > 0);
            assert_eq!(suite.is_encrypted(), suite.overhead() == 16);
        }
        assert_eq!(Suite::parse(" AES-256-GCM ").unwrap(), Suite::Aes256Gcm);
        assert_eq!(Suite::from_id(4), Err(Error::UnknownSuite(4)));
        assert_eq!(
            Suite::parse("des"),
            Err(Error::UnknownSuiteName("des".into()))
        );
        assert!(Suite::Aes128Gcm.is_fips_algorithm());
        assert!(Suite::Aes256Gcm.is_fips_algorithm());
        assert!(!Suite::ChaCha20Poly1305.is_fips_algorithm());
        assert!(!Suite::None.is_fips_algorithm());
    }

    #[test]
    fn policies() {
        assert!(!Policy::recommended().allows_plaintext());
        assert!(Policy::fips()
            .suites()
            .iter()
            .all(|s| s.is_fips_algorithm()));
        assert!(Policy::none().allows_plaintext());
        assert_eq!(Policy::custom(&[]), Err(Error::EmptyPolicy));
        let p = Policy::custom(&[Suite::None, Suite::Aes128Gcm, Suite::None]).unwrap();
        assert_eq!(p.suites(), &[Suite::None, Suite::Aes128Gcm]);
        assert_eq!(Policy::preset("FIPS").unwrap(), Policy::fips());
        assert_eq!(
            Policy::preset("recommended").unwrap(),
            Policy::recommended()
        );
        assert_eq!(Policy::preset("none").unwrap(), Policy::none());
        assert!(matches!(Policy::preset("x"), Err(Error::Config(_))));
    }

    #[test]
    fn negotiation_prefers_local_order_and_never_downgrades() {
        let local = Policy::recommended();
        assert_eq!(
            negotiate(&local, &[Suite::Aes128Gcm, Suite::ChaCha20Poly1305]).unwrap(),
            Suite::ChaCha20Poly1305
        );
        assert_eq!(negotiate(&local, &[Suite::None]), Err(Error::NoCommonSuite));
        assert_eq!(negotiate(&local, &[]), Err(Error::NoCommonSuite));

        // 本端把“不加密”排在最前，只要有共同的加密套件仍然选加密。
        let lax = Policy::custom(&[Suite::None, Suite::Aes128Gcm]).unwrap();
        assert_eq!(
            negotiate(&lax, &[Suite::None, Suite::Aes128Gcm]).unwrap(),
            Suite::Aes128Gcm
        );
        // 两端都允许且没有共同加密套件时才是不加密。
        assert_eq!(
            negotiate(&lax, &[Suite::None, Suite::Aes256Gcm]).unwrap(),
            Suite::None
        );
        assert_eq!(
            negotiate(&Policy::none(), &[Suite::None]).unwrap(),
            Suite::None
        );
    }

    fn key_for(suite: Suite) -> Vec<u8> {
        (0..suite.key_len())
            .map(|i| u8::try_from(i).unwrap())
            .collect()
    }

    #[test]
    fn seal_open_roundtrip_for_every_suite() {
        for suite in Suite::ALL {
            let sealer = Sealer::new(suite, &key_for(suite), 1).unwrap();
            assert_eq!(sealer.suite(), suite);
            let sealed = sealer.seal(7, b"header", b"payload").unwrap();
            assert_eq!(sealed.len(), 7 + suite.overhead());
            assert_eq!(sealer.open(7, b"header", &sealed).unwrap(), b"payload");
            // 空明文也合法
            let empty = sealer.seal(8, b"", b"").unwrap();
            assert_eq!(sealer.open(8, b"", &empty).unwrap(), b"");
            assert!(format!("{sealer:?}").contains(suite_debug(suite)));
        }
    }

    fn suite_debug(suite: Suite) -> &'static str {
        match suite {
            Suite::None => "None",
            Suite::Aes128Gcm => "Aes128Gcm",
            Suite::Aes256Gcm => "Aes256Gcm",
            Suite::ChaCha20Poly1305 => "ChaCha20Poly1305",
        }
    }

    #[test]
    fn tampering_is_detected_by_encrypted_suites() {
        for suite in Suite::ALL.into_iter().filter(|s| s.is_encrypted()) {
            let a = Sealer::new(suite, &key_for(suite), 1).unwrap();
            let sealed = a.seal(1, b"aad", b"secret").unwrap();
            assert_ne!(&sealed[..6], b"secret");

            let mut flipped = sealed.clone();
            flipped[0] ^= 1;
            assert_eq!(a.open(1, b"aad", &flipped), Err(Error::OpenFailed));
            assert_eq!(a.open(2, b"aad", &sealed), Err(Error::OpenFailed));
            assert_eq!(a.open(1, b"aaX", &sealed), Err(Error::OpenFailed));
            assert_eq!(a.open(1, b"aad", &sealed[..3]), Err(Error::OpenFailed));
            assert_eq!(a.open(1, b"aad", &[]), Err(Error::OpenFailed));

            // 方向不同，随机数不同，互相解不开。
            let b = Sealer::new(suite, &key_for(suite), 2).unwrap();
            assert_eq!(b.open(1, b"aad", &sealed), Err(Error::OpenFailed));
        }
    }

    #[test]
    fn key_length_is_checked() {
        for suite in Suite::ALL {
            let wrong = vec![0u8; suite.key_len() + 1];
            assert_eq!(
                Sealer::new(suite, &wrong, 0).err(),
                Some(Error::KeyLength {
                    need: suite.key_len(),
                    have: suite.key_len() + 1
                })
            );
        }
    }
}
