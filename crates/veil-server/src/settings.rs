//! What the server reads at start-up.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Where to listen. 443 is the point — a tunnel on an unusual port is
    /// unusual, and being unremarkable is the entire defence.
    pub listen: String,
    pub password: String,
    /// PEM certificate chain and private key.
    pub certificate: PathBuf,
    pub key: PathBuf,
    /// `builtin`, or `host:port` of a real web server to hand strangers to.
    pub fallback: String,
    /// Name the certificate is issued for, used when printing a share link.
    pub sni: String,
    /// Label for the share link.
    pub name: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:443".to_string(),
            password: String::new(),
            certificate: PathBuf::from("cert.pem"),
            key: PathBuf::from("key.pem"),
            fallback: "builtin".to_string(),
            sni: String::new(),
            name: "veil".to_string(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("{} 를 읽을 수 없습니다", path.display()))?;
        let settings: Self =
            toml::from_str(&text).with_context(|| format!("{} 형식이 잘못되었습니다", path.display()))?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn address(&self) -> Result<SocketAddr> {
        self.listen
            .parse()
            .with_context(|| format!("listen 값이 주소:포트 형식이 아닙니다: {}", self.listen))
    }

    /// Refuse to start on a configuration that would look fine and be unsafe.
    pub fn validate(&self) -> Result<()> {
        if self.password.is_empty() {
            bail!("password 가 비어 있습니다 — 누구나 터널을 쓸 수 있게 됩니다");
        }
        // Short passwords are the realistic failure: the hash on the wire is
        // offline-guessable by anyone who records a session.
        if self.password.len() < 16 {
            bail!("password 가 너무 짧습니다 ({}자) — 16자 이상을 쓰세요", self.password.len());
        }
        self.address()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> Settings {
        Settings { password: "a-long-enough-password".into(), ..Default::default() }
    }

    #[test]
    fn a_valid_configuration_is_accepted() {
        assert!(valid().validate().is_ok());
        assert_eq!(valid().address().unwrap().port(), 443);
    }

    #[test]
    fn an_empty_password_is_refused() {
        // Starting anyway would put an open relay on the internet.
        assert!(Settings::default().validate().is_err());
    }

    #[test]
    fn a_short_password_is_refused() {
        let settings = Settings { password: "hunter2".into(), ..Default::default() };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn a_malformed_listen_address_is_refused() {
        let settings = Settings { listen: "443".into(), ..valid() };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn settings_round_trip_through_toml() {
        let original = valid();
        let parsed: Settings = toml::from_str(&toml::to_string_pretty(&original).unwrap()).unwrap();
        assert_eq!(parsed.password, original.password);
        assert_eq!(parsed.listen, original.listen);
        assert_eq!(parsed.fallback, original.fallback);
    }

    #[test]
    fn omitted_fields_take_their_defaults() {
        // The generated file is short on purpose; everything else must fill in.
        let parsed: Settings = toml::from_str(r#"password = "a-long-enough-password""#).unwrap();
        assert_eq!(parsed.listen, "0.0.0.0:443");
        assert_eq!(parsed.fallback, "builtin");
        assert!(parsed.validate().is_ok());
    }
}
