//! The TLS the engine itself uses.
//!
//! Only the encrypted resolver needs this. Everything the browser does is
//! relayed untouched — the engine never terminates the user's TLS, which is
//! what keeps it out of a position to read anything.

use anyhow::Result;
use rustls::{ClientConfig, RootCertStore};
use std::sync::{Arc, OnceLock};

/// Install the crypto provider once, before anything builds a TLS config.
///
/// rustls will not pick one implicitly, and the failure mode is a panic deep
/// inside a builder rather than an error — which on a phone means the app
/// simply disappears.
pub fn install_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Client configuration for the DoH endpoint.
///
/// Built once and shared: assembling the root store parses every public root
/// certificate, which is far too much work to repeat per lookup.
pub fn doh_config() -> Result<Arc<ClientConfig>> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    Ok(Arc::clone(CONFIG.get_or_init(|| {
        install_provider();
        let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        Arc::new(ClientConfig::builder().with_root_certificates(roots).with_no_client_auth())
    })))
}
