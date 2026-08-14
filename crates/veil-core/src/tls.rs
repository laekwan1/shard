//! TLS setup for both ends.
//!
//! Two ways to trust a server, because there are two ways people run one. With
//! a domain name, an ordinary certificate from a public authority works and
//! there is nothing to configure. Without one — which is the common case for a
//! box that only has an IP address — the server presents its own certificate
//! and the client is told in advance exactly which one to expect. Pinning a
//! single certificate is stricter than trusting every authority on earth, not
//! weaker; what it costs is the ability to rotate the certificate silently.

use anyhow::{bail, Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, ServerConfig, SignatureScheme};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// How the client decides whether to trust the server it reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trust {
    /// An ordinary certificate, validated against the public authorities.
    WebPki,
    /// Exactly one certificate, identified by the SHA-256 of its DER bytes.
    Pinned(String),
}

impl Trust {
    /// Read a pin written as hex, with or without colons.
    pub fn pinned(fingerprint: &str) -> Result<Self> {
        let cleaned: String = fingerprint.chars().filter(|c| !matches!(c, ':' | ' ')).collect();
        let bytes = hex::decode(&cleaned).context("지문이 16진수가 아닙니다")?;
        if bytes.len() != 32 {
            bail!("SHA-256 지문은 32바이트여야 합니다 (받은 값: {}바이트)", bytes.len());
        }
        Ok(Trust::Pinned(cleaned.to_ascii_lowercase()))
    }
}

/// SHA-256 of a certificate's DER encoding — what [`Trust::Pinned`] holds.
pub fn fingerprint(certificate: &CertificateDer<'_>) -> String {
    hex::encode(Sha256::digest(certificate.as_ref()))
}

/// Install the crypto provider once, before anything builds a TLS config.
///
/// rustls will not pick one implicitly, and the failure mode is a panic deep
/// inside a builder rather than an error — which on a phone means the app
/// simply disappears.
pub fn install_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Already installed is not a failure; something else got there first.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// `bytes` random bytes, hex encoded.
///
/// The source is the OS generator by way of rustls's, so there is no home-made
/// randomness anywhere in this — which is the only acceptable answer when the
/// output becomes a password.
pub fn random_hex(bytes: usize) -> Result<String> {
    install_provider();
    let mut buffer = vec![0u8; bytes];
    rustls::crypto::CryptoProvider::get_default()
        .context("암호 공급자가 설치되지 않았습니다")?
        .secure_random
        .fill(&mut buffer)
        .map_err(|_| anyhow::anyhow!("운영체제 난수원을 읽을 수 없습니다"))?;
    Ok(hex::encode(buffer))
}

pub fn client_config(trust: &Trust) -> Result<Arc<ClientConfig>> {
    install_provider();
    let config = match trust {
        Trust::WebPki => {
            let roots = RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            ClientConfig::builder().with_root_certificates(roots).with_no_client_auth()
        }
        Trust::Pinned(hex) => ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedCertificate::new(hex.clone())))
            .with_no_client_auth(),
    };
    Ok(Arc::new(config))
}

pub fn server_config(certificates: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static>) -> Result<Arc<ServerConfig>> {
    install_provider();
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .context("인증서와 키가 맞지 않습니다")?;
    Ok(Arc::new(config))
}

/// Load a PEM certificate chain and its private key.
pub fn load_pem(certificate: &str, key: &str) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let certificates: Vec<_> = rustls_pemfile::certs(&mut certificate.as_bytes())
        .collect::<std::result::Result<_, _>>()
        .context("인증서를 읽을 수 없습니다")?;
    if certificates.is_empty() {
        bail!("인증서 파일에 인증서가 없습니다");
    }
    let key = rustls_pemfile::private_key(&mut key.as_bytes())
        .context("개인키를 읽을 수 없습니다")?
        .context("개인키 파일에 키가 없습니다")?;
    Ok((certificates, key))
}

/// Accepts one certificate and nothing else.
///
/// This deliberately ignores the name on the certificate. The pin already
/// identifies a single specific key, which is a stronger statement than "some
/// authority vouched for this name" — and it is what lets the client send a
/// plausible but unrelated SNI without the handshake failing.
#[derive(Debug)]
struct PinnedCertificate {
    expected: String,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl PinnedCertificate {
    fn new(expected: String) -> Self {
        Self { expected, provider: Arc::new(rustls::crypto::ring::default_provider()) }
    }
}

impl ServerCertVerifier for PinnedCertificate {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if fingerprint(end_entity) == self.expected {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("서버 인증서 지문이 다릅니다".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pin_is_accepted_with_or_without_separators() {
        let plain = "a".repeat(64);
        let colons = plain
            .as_bytes()
            .chunks(2)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join(":");

        assert_eq!(Trust::pinned(&plain).unwrap(), Trust::pinned(&colons).unwrap());
    }

    #[test]
    fn a_pin_of_the_wrong_length_is_refused() {
        // Truncating a fingerprint would silently weaken the check.
        assert!(Trust::pinned(&"a".repeat(62)).is_err());
        assert!(Trust::pinned(&"a".repeat(66)).is_err());
        assert!(Trust::pinned("not hex at all").is_err());
    }

    #[test]
    fn a_pin_is_case_insensitive() {
        let upper = "AB".repeat(32);
        let Trust::Pinned(stored) = Trust::pinned(&upper).unwrap() else { panic!() };
        assert_eq!(stored, "ab".repeat(32));
    }

    #[test]
    fn a_generated_certificate_hashes_to_a_stable_pin() {
        let cert = rcgen::generate_simple_self_signed(vec!["veil.test".into()]).unwrap();
        let der = CertificateDer::from(cert.cert.der().to_vec());

        let a = fingerprint(&der);
        assert_eq!(a, fingerprint(&der), "hashing must be deterministic");
        assert_eq!(a.len(), 64);
        assert!(Trust::pinned(&a).is_ok(), "a real fingerprint must parse as a pin");
    }
}
