//! First-run setup: make a certificate, a password, and the link to paste.
//!
//! The point of doing this in the binary rather than in a shell script is that
//! the pin the client needs is derived from the certificate that was just
//! written. Anything that computes it separately can get it wrong, and a wrong
//! pin fails at the handshake with nothing useful to say.

use anyhow::{Context, Result};
use rustls_pki_types::CertificateDer;
use std::path::Path;
use veil_core::client::Server;
use veil_core::link;
use veil_core::tls::Trust;

use crate::settings::Settings;

/// A name that raises no questions. It never resolves anywhere and nothing
/// checks it — with a pinned certificate the client ignores the name entirely —
/// but it is what appears in the TLS handshake, so it should look ordinary.
const DEFAULT_SNI: &str = "www.bing.com";

pub struct Generated {
    pub settings: Settings,
    pub link: String,
    pub fingerprint: String,
}

/// Write cert.pem, key.pem and config.toml into `dir`, and return the link.
pub fn run(dir: &Path, host: &str, port: u16) -> Result<Generated> {
    std::fs::create_dir_all(dir).with_context(|| format!("{} 를 만들 수 없습니다", dir.display()))?;

    let sni = DEFAULT_SNI.to_string();
    let generated = rcgen::generate_simple_self_signed(vec![sni.clone()])
        .context("인증서를 만들 수 없습니다")?;
    let der = CertificateDer::from(generated.cert.der().to_vec());
    let fingerprint = veil_core::tls::fingerprint(&der);

    let certificate_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    std::fs::write(&certificate_path, generated.cert.pem()).context("인증서를 저장할 수 없습니다")?;
    std::fs::write(&key_path, generated.signing_key.serialize_pem()).context("개인키를 저장할 수 없습니다")?;
    restrict(&key_path)?;

    let settings = Settings {
        listen: format!("0.0.0.0:{port}"),
        // 128 bits, so recording a session and grinding the hash offline buys
        // nothing.
        password: veil_core::tls::random_hex(16)?,
        certificate: certificate_path,
        key: key_path,
        fallback: "builtin".to_string(),
        sni: sni.clone(),
        name: "veil".to_string(),
    };
    settings.validate()?;

    let config_path = dir.join("config.toml");
    std::fs::write(&config_path, toml::to_string_pretty(&settings)?)
        .context("설정을 저장할 수 없습니다")?;
    restrict(&config_path)?;

    let server = Server::new(host, port, settings.password.clone())
        .with_sni(sni)
        .with_trust(Trust::pinned(&fingerprint)?);

    Ok(Generated { link: link::build(&server, &settings.name), settings, fingerprint })
}

/// Keep the key and the password readable only by the account that owns them.
#[cfg(unix)]
fn restrict(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("{} 권한을 설정할 수 없습니다", path.display()))
}

#[cfg(not(unix))]
fn restrict(_path: &Path) -> Result<()> {
    // Windows has no direct equivalent and the server does not run there.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("veil-setup-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn setup_writes_everything_the_server_needs() {
        let dir = temp_dir("files");
        let generated = run(&dir, "203.0.113.9", 443).unwrap();

        for file in ["cert.pem", "key.pem", "config.toml"] {
            assert!(dir.join(file).exists(), "{file} was not written");
        }
        // The written config must be one the server will actually accept.
        let reloaded = Settings::load(&dir.join("config.toml")).unwrap();
        assert_eq!(reloaded.password, generated.settings.password);
    }

    #[test]
    fn the_link_carries_the_pin_of_the_certificate_just_written() {
        // This is the whole reason setup lives here: a pin computed anywhere
        // else can disagree with the file on disk, and the failure is a
        // handshake error that says nothing useful.
        let dir = temp_dir("pin");
        let generated = run(&dir, "203.0.113.9", 8443).unwrap();

        let (server, name) = veil_core::link::parse(&generated.link).unwrap();
        assert_eq!(server.trust, Trust::pinned(&generated.fingerprint).unwrap());
        assert_eq!(server.host, "203.0.113.9");
        assert_eq!(server.port, 8443);
        assert_eq!(server.password, generated.settings.password);
        assert_eq!(name, "veil");

        // And the pin must match the certificate file, not just the value in
        // memory when the link was built.
        let pem = std::fs::read_to_string(dir.join("cert.pem")).unwrap();
        let (certificates, _) =
            veil_core::tls::load_pem(&pem, &std::fs::read_to_string(dir.join("key.pem")).unwrap())
                .unwrap();
        assert_eq!(veil_core::tls::fingerprint(&certificates[0]), generated.fingerprint);
    }

    #[test]
    fn generated_passwords_are_long_and_not_repeated() {
        let a = veil_core::tls::random_hex(16).unwrap();
        let b = veil_core::tls::random_hex(16).unwrap();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b, "two runs produced the same password");
    }

    #[test]
    fn two_setups_never_produce_the_same_credentials() {
        // A fixed password or a fixed certificate would make every server this
        // tool creates interchangeable with every other one.
        let first = run(&temp_dir("unique-a"), "203.0.113.9", 443).unwrap();
        let second = run(&temp_dir("unique-b"), "203.0.113.9", 443).unwrap();

        assert_ne!(first.settings.password, second.settings.password);
        assert_ne!(first.fingerprint, second.fingerprint);
    }
}
