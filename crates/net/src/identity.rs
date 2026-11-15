//! 设备身份：一张自签名证书和它的私钥。指纹是证书 DER 编码的 SHA-256。

use std::fmt::Write as _;
use std::path::Path;

use crate::NetError;

/// 证书指纹（SHA-256）。
pub type Fingerprint = [u8; 32];

/// 计算证书的指纹。
#[must_use]
pub fn fingerprint_of(cert_der: &[u8]) -> Fingerprint {
    let digest = ring::digest::digest(&ring::digest::SHA256, cert_der);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// 指纹的十六进制表示（64 个小写字符）。
#[must_use]
pub fn to_hex(fingerprint: &Fingerprint) -> String {
    let mut out = String::with_capacity(64);
    for byte in fingerprint {
        // 向 String 写入不会失败。
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// 解析十六进制指纹，不区分大小写，必须恰好 64 个字符。
pub fn parse_hex(text: &str) -> Result<Fingerprint, NetError> {
    let bad = || NetError::Tls(format!("指纹必须是 64 个十六进制字符：{text}"));
    let text = text.trim();
    if text.len() != 64 || !text.is_ascii() {
        return Err(bad());
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(text.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(pair).map_err(|_| bad())?;
        *slot = u8::from_str_radix(pair, 16).map_err(|_| bad())?;
    }
    Ok(out)
}

/// 设备身份。
#[derive(Clone)]
pub struct Identity {
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 不输出私钥。
        f.debug_struct("Identity")
            .field("fingerprint", &to_hex(&self.fingerprint()))
            .finish()
    }
}

const CERT_FILE: &str = "identity.crt";
const KEY_FILE: &str = "identity.key";

fn io(what: &str, path: &Path, e: &std::io::Error) -> NetError {
    NetError::Io(format!("{what} {}：{e}", path.display()))
}

impl Identity {
    /// 生成新的身份（ECDSA P-256 自签名证书）。
    pub fn generate() -> Result<Self, NetError> {
        let certified = rcgen::generate_simple_self_signed(vec!["peerdesk".to_owned()])
            .map_err(|e| NetError::Tls(format!("生成证书失败：{e}")))?;
        Ok(Self {
            cert_der: certified.cert.der().to_vec(),
            key_der: certified.key_pair.serialize_der(),
        })
    }

    /// 从目录加载身份；不存在时生成并保存。私钥文件只允许当前用户读写。
    pub fn load_or_create(dir: &Path) -> Result<Self, NetError> {
        let (cert_path, key_path) = (dir.join(CERT_FILE), dir.join(KEY_FILE));
        if cert_path.is_file() && key_path.is_file() {
            return Ok(Self {
                cert_der: std::fs::read(&cert_path).map_err(|e| io("读取", &cert_path, &e))?,
                key_der: std::fs::read(&key_path).map_err(|e| io("读取", &key_path, &e))?,
            });
        }
        let identity = Self::generate()?;
        std::fs::create_dir_all(dir).map_err(|e| io("创建目录", dir, &e))?;
        std::fs::write(&key_path, &identity.key_der).map_err(|e| io("写入", &key_path, &e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| io("设置权限", &key_path, &e))?;
        }
        std::fs::write(&cert_path, &identity.cert_der).map_err(|e| io("写入", &cert_path, &e))?;
        Ok(identity)
    }

    /// 证书的 DER 编码。
    #[must_use]
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// 私钥的 PKCS#8 DER 编码。
    #[must_use]
    pub fn key_der(&self) -> &[u8] {
        &self.key_der
    }

    /// 指纹。
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        fingerprint_of(&self.cert_der)
    }

    /// 由指纹前 8 个字节得到的设备标识，用于握手消息和日志。
    #[must_use]
    pub fn device_id(&self) -> u64 {
        let fp = self.fingerprint();
        u64::from_be_bytes([fp[0], fp[1], fp[2], fp[3], fp[4], fp[5], fp[6], fp[7]])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip_and_errors() {
        let fp: Fingerprint = std::array::from_fn(|i| u8::try_from(i * 7 % 256).unwrap());
        let hex = to_hex(&fp);
        assert_eq!(hex.len(), 64);
        assert_eq!(parse_hex(&hex).unwrap(), fp);
        assert_eq!(
            parse_hex(&format!("  {}\n", hex.to_uppercase())).unwrap(),
            fp
        );
        for bad in [
            "",
            "abcd",
            &"g".repeat(64),
            &"é".repeat(32),
            &"a".repeat(65),
        ] {
            assert!(matches!(parse_hex(bad), Err(NetError::Tls(_))), "{bad}");
        }
    }

    #[test]
    fn generated_identities_are_distinct() {
        let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_ne!(a.device_id(), b.device_id());
        assert_eq!(a.fingerprint(), fingerprint_of(a.cert_der()));
        assert!(!a.key_der().is_empty());
        let debug = format!("{a:?}");
        assert!(debug.contains(&to_hex(&a.fingerprint())));
    }

    #[test]
    fn load_or_create_persists() {
        let dir = std::env::temp_dir().join(format!("peerdesk-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = Identity::load_or_create(&dir).unwrap();
        let second = Identity::load_or_create(&dir).unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint());
        assert_eq!(first.key_der(), second.key_der());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(KEY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
        // 目录位置不可用：路径上有一个普通文件
        let file = std::env::temp_dir().join(format!("peerdesk-idfile-{}", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        assert!(matches!(
            Identity::load_or_create(&file.join("sub")),
            Err(NetError::Io(_))
        ));
        std::fs::remove_file(&file).unwrap();
    }
}
