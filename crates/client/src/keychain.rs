//! 系统钥匙串里存保存密码用的加密密钥（见 `vault.rs`）：Windows 凭据管理器、macOS 钥匙串、Linux Secret Service。
//!
//! 只调用系统接口，在覆盖率排除名单里（`scripts/coverage.sh`）。测试构建不碰开发者电脑的钥匙串：
//! 逻辑测试用内存里的 `KeyStore`。

use crate::vault::KeyStore;

const SERVICE: &str = "PeerDesk";
const ACCOUNT: &str = "saved-passwords-key";

/// 系统钥匙串。
#[derive(Debug, Default)]
pub struct SystemKeyStore;

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| e.to_string())
}

impl KeyStore for SystemKeyStore {
    fn load(&self) -> Result<Option<[u8; 32]>, String> {
        match entry()?.get_password() {
            Ok(text) => {
                let bytes: Vec<u8> = (0..text.len())
                    .step_by(2)
                    .filter_map(|i| {
                        text.get(i..i + 2)
                            .and_then(|h| u8::from_str_radix(h, 16).ok())
                    })
                    .collect();
                <[u8; 32]>::try_from(bytes.as_slice())
                    .map(Some)
                    .map_err(|_| "钥匙串里的密钥格式不对".to_owned())
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn store(&self, key: &[u8; 32]) -> Result<(), String> {
        let text: String = key.iter().map(|b| format!("{b:02x}")).collect();
        entry()?.set_password(&text).map_err(|e| e.to_string())
    }
}
