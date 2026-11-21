//! TLS 1.3 配置：把加密策略映射成密码套件，证书校验改为“验证签名，信任由上层按指纹决定”。

use std::sync::Arc;
use std::time::Duration;

use peerdesk_core::crypto::{Policy, Suite};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::crypto::ring::cipher_suite::{
    TLS13_AES_128_GCM_SHA256, TLS13_AES_256_GCM_SHA384, TLS13_CHACHA20_POLY1305_SHA256,
};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::SupportedCipherSuite;

use crate::tls_verifier::AnyCertificate;

use crate::{Identity, NetError};

/// 应用层协议名。
const ALPN: &[u8] = b"peerdesk/1";
/// 连接空闲多久后断开（毫秒）。
const IDLE_TIMEOUT_MS: u32 = 10_000;
/// QUIC 包的长度上限（字节）。
const MAX_UDP_PAYLOAD: u16 = 1400;
/// 保活间隔。
const KEEP_ALIVE: Duration = Duration::from_secs(2);
/// 数据报收发缓冲。发送缓冲满时最旧的数据报被丢弃，符合“新帧优先”。
const DATAGRAM_BUFFER: usize = 4 * 1024 * 1024;

fn tls_suite(suite: Suite) -> Option<SupportedCipherSuite> {
    match suite {
        Suite::Aes256Gcm => Some(TLS13_AES_256_GCM_SHA384),
        Suite::ChaCha20Poly1305 => Some(TLS13_CHACHA20_POLY1305_SHA256),
        Suite::Aes128Gcm => Some(TLS13_AES_128_GCM_SHA256),
        // QUIC 强制加密，没有对应的密码套件。
        Suite::None => None,
    }
}

/// 按策略构造加密提供者：只包含策略允许的套件，顺序即偏好。
fn provider_for(policy: &Policy) -> Result<Arc<CryptoProvider>, NetError> {
    let cipher_suites: Vec<SupportedCipherSuite> = policy
        .suites()
        .iter()
        .filter_map(|s| tls_suite(*s))
        .collect();
    if cipher_suites.is_empty() {
        return Err(NetError::Tls(
            "QUIC 传输强制加密：加密策略里至少要有一个加密套件".into(),
        ));
    }
    Ok(Arc::new(CryptoProvider {
        cipher_suites,
        ..rustls::crypto::ring::default_provider()
    }))
}

fn tls_err(what: &str, e: impl std::fmt::Display) -> NetError {
    NetError::Tls(format!("{what}：{e}"))
}

/// QUIC 的初始包固定用 AES-128-GCM 保护，密钥由公开的连接标识导出，不提供保密性，
/// 所以它不受加密策略约束；真正的会话密钥只用策略允许的套件。
fn initial_suite() -> Result<rustls::quic::Suite, NetError> {
    TLS13_AES_128_GCM_SHA256
        .tls13()
        .and_then(|suite| suite.quic_suite())
        .ok_or(NetError::Protocol("加密库缺少 QUIC 初始包所需的套件"))
}

/// 握手时的往返时间估计。
const INITIAL_RTT: Duration = Duration::from_millis(100);

fn transport() -> Arc<quinn::TransportConfig> {
    // 包长上限留出余量：经中继时每个包前面还要加中继包头，不能超过常见链路的 MTU。
    let mut mtu = quinn::MtuDiscoveryConfig::default();
    mtu.upper_bound(MAX_UDP_PAYLOAD);
    let mut transport = quinn::TransportConfig::default();
    transport
        .max_idle_timeout(Some(quinn::IdleTimeout::from(quinn::VarInt::from_u32(
            IDLE_TIMEOUT_MS,
        ))))
        .keep_alive_interval(Some(KEEP_ALIVE))
        // 还没测到往返时间时按 100 毫秒估计（默认 333 毫秒）：打洞和中继刚接通时第一个握手包
        // 常常被丢（对方还没打开 NAT 或还没接上中继），这样约 0.3 秒就重发，而不是约 1 秒。
        .initial_rtt(INITIAL_RTT)
        .datagram_receive_buffer_size(Some(DATAGRAM_BUFFER))
        .datagram_send_buffer_size(DATAGRAM_BUFFER)
        .mtu_discovery_config(Some(mtu))
        .congestion_controller_factory(congestion(std::env::var("PEERDESK_CC").ok().as_deref()));
    Arc::new(transport)
}

/// QUIC 层的拥塞控制。画面和输入的码率由我们自己的实时层决定，这里只是安全网。
/// 默认 BBR：实验（scripts/e2e-live.sh，25 ms 延迟、2% 丢包、画面大量变化）里 Cubic 和 NewReno
/// 把随机丢包当拥塞，8 秒一帧都没送到；BBR 不看丢包，送到了。`PEERDESK_CC` 可换成 cubic、newreno 做对照。
fn congestion(name: Option<&str>) -> Arc<dyn quinn::congestion::ControllerFactory + Send + Sync> {
    match name {
        Some("cubic") => Arc::new(quinn::congestion::CubicConfig::default()),
        Some("newreno") => Arc::new(quinn::congestion::NewRenoConfig::default()),
        Some(_) | None => Arc::new(quinn::congestion::BbrConfig::default()),
    }
}

/// 构造一对 QUIC 配置：作为被连接方和作为发起方。
pub(crate) fn configs(
    identity: &Identity,
    policy: &Policy,
) -> Result<(quinn::ServerConfig, quinn::ClientConfig), NetError> {
    let provider = provider_for(policy)?;
    let verifier = Arc::new(AnyCertificate::new(Arc::clone(&provider)));
    let chain = vec![CertificateDer::from(identity.cert_der().to_vec())];
    let key = || PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key_der().to_vec()));
    let versions = [&rustls::version::TLS13];

    let mut server = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&versions)
        .map_err(|e| tls_err("TLS 版本配置失败", e))?
        .with_client_cert_verifier(verifier.clone())
        .with_single_cert(chain.clone(), key())
        .map_err(|e| tls_err("加载证书失败", e))?;
    server.alpn_protocols = vec![ALPN.to_vec()];
    // 套件按被连接方（被控端）的偏好选，与核心库的协商规则一致。
    server.ignore_client_order = true;

    let mut client = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&versions)
        .map_err(|e| tls_err("TLS 版本配置失败", e))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(chain, key())
        .map_err(|e| tls_err("加载证书失败", e))?;
    client.alpn_protocols = vec![ALPN.to_vec()];

    let transport = transport();
    let quic_server = QuicServerConfig::with_initial(Arc::new(server), initial_suite()?)
        .map_err(|e| tls_err("QUIC 配置失败", e))?;
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_server));
    server_config.transport_config(Arc::clone(&transport));

    let quic_client = QuicClientConfig::with_initial(Arc::new(client), initial_suite()?)
        .map_err(|e| tls_err("QUIC 配置失败", e))?;
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_client));
    client_config.transport_config(transport);
    Ok((server_config, client_config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_maps_to_cipher_suites_in_order() {
        let p = provider_for(&Policy::recommended()).unwrap();
        assert_eq!(
            p.cipher_suites,
            vec![
                TLS13_AES_256_GCM_SHA384,
                TLS13_CHACHA20_POLY1305_SHA256,
                TLS13_AES_128_GCM_SHA256
            ]
        );
        let custom = Policy::custom(&[Suite::None, Suite::ChaCha20Poly1305]).unwrap();
        assert_eq!(
            provider_for(&custom).unwrap().cipher_suites,
            vec![TLS13_CHACHA20_POLY1305_SHA256]
        );
        assert!(matches!(
            provider_for(&Policy::none()),
            Err(NetError::Tls(_))
        ));
    }

    #[test]
    fn configs_build_for_every_encrypted_policy() {
        let identity = Identity::generate().unwrap();
        for suite in [Suite::Aes128Gcm, Suite::Aes256Gcm, Suite::ChaCha20Poly1305] {
            let policy = Policy::custom(&[suite]).unwrap();
            assert!(configs(&identity, &policy).is_ok(), "{suite}");
        }
        assert!(configs(&identity, &Policy::fips()).is_ok());
        assert!(configs(&identity, &Policy::none()).is_err());
    }
}

#[cfg(test)]
mod cc_tests {
    use super::*;

    #[test]
    fn every_congestion_choice_builds() {
        for name in [
            None,
            Some("cubic"),
            Some("newreno"),
            Some("bbr"),
            Some("other"),
        ] {
            let _ = congestion(name);
        }
    }
}
