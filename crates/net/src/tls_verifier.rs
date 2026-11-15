//! 证书校验器：接受任何证书，严格验证握手签名。
//!
//! 这个文件不计入覆盖率门槛：其中处理 TLS 1.2 签名的两个函数只能由 TLS 协议栈调用
//! （入参类型无法在外部构造），而本项目只启用 TLS 1.3，它们永远不会被调用。
//! TLS 1.3 的校验路径在每一个真实连接测试里都会执行。

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};

/// 接受任何证书，但严格验证握手签名（证明对端持有证书私钥）。
/// 是否信任这张证书由上层在握手后按指纹判断。
#[derive(Debug)]
pub(crate) struct AnyCertificate(Arc<CryptoProvider>);

impl AnyCertificate {
    pub(crate) const fn new(provider: Arc<CryptoProvider>) -> Self {
        Self(provider)
    }

    fn tls12_unsupported() -> rustls::Error {
        rustls::Error::General("只支持 TLS 1.3".into())
    }
}

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(Self::tls12_unsupported())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

impl ClientCertVerifier for AnyCertificate {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(Self::tls12_unsupported())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertises_schemes_and_no_root_hints() {
        let v = AnyCertificate::new(Arc::new(rustls::crypto::ring::default_provider()));
        assert!(!ServerCertVerifier::supported_verify_schemes(&v).is_empty());
        assert!(!ClientCertVerifier::supported_verify_schemes(&v).is_empty());
        assert!(ClientCertVerifier::root_hint_subjects(&v).is_empty());
        assert!(format!("{:?}", AnyCertificate::tls12_unsupported()).contains("TLS 1.3"));
    }
}
