//! Veil's on-disk configuration.

use crate::profile::{Profile, TorTransport};
use serde::{Deserialize, Serialize};

pub const APP_NAME: &str = "Veil";

/// How traffic reaches the tunnel.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// A virtual adapter capturing everything, including apps that ignore proxy
    /// settings. Requires the wintun driver and elevation.
    #[default]
    Tun,
    /// A local SOCKS/HTTP listener. No driver, no elevation, but only
    /// proxy-aware applications are covered — and anything else leaks.
    Proxy,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Tun => "TUN (전체 트래픽)",
            Mode::Proxy => "로컬 프록시",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Routing {
    /// When true everything goes through the tunnel except the direct lists.
    pub default_proxy: bool,
    /// Hostnames that always bypass the tunnel — banking, work VPNs, anything
    /// that breaks or gets flagged when it appears from a foreign address.
    pub direct_domains: Vec<String>,
    /// Executable names that bypass the tunnel. Matches every install of that
    /// name, which is usually what you want for a game or an updater.
    pub direct_processes: Vec<String>,
    /// Full executable paths that bypass the tunnel. Use this when the name
    /// alone would be too broad — Tor Browser's binary is called `firefox.exe`
    /// like any other Firefox, and only the path tells them apart.
    pub direct_process_paths: Vec<String>,
    /// Hostnames forced through the tunnel when the default is direct.
    pub proxy_domains: Vec<String>,
    /// Keep private ranges off the tunnel so the LAN, printers and NAS work.
    pub bypass_private: bool,
    /// Reject QUIC so browsers fall back to TCP. Several proxy protocols carry
    /// UDP poorly, and a silently failing QUIC connection is worse than a
    /// slightly slower TCP one.
    pub block_quic: bool,
}

impl Default for Routing {
    fn default() -> Self {
        Self {
            default_proxy: true,
            direct_domains: Vec::new(),
            direct_processes: Vec::new(),
            direct_process_paths: Vec::new(),
            proxy_domains: Vec::new(),
            bypass_private: true,
            block_quic: true,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Dns {
    /// Resolver used for proxied names. It is queried *through* the tunnel, so
    /// the local network never sees which hostnames are being looked up.
    pub remote: String,
    /// Resolver for names that bypass the tunnel.
    pub local: String,
    /// `prefer_ipv4`, `ipv4_only`, `prefer_ipv6`, `ipv6_only`.
    pub strategy: String,
}

impl Default for Dns {
    fn default() -> Self {
        Self {
            remote: "https://1.1.1.1/dns-query".to_string(),
            local: "1.1.1.1".to_string(),
            strategy: "prefer_ipv4".to_string(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Tor {
    pub socks_port: u16,
    pub control_port: u16,
    pub transport: TorTransport,
    /// Bridge lines from <https://bridges.torproject.org>.
    pub bridges: Vec<String>,
}

impl Default for Tor {
    fn default() -> Self {
        Self {
            socks_port: 9250,
            control_port: 9251,
            transport: TorTransport::WebTunnel,
            bridges: Vec::new(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Subscription {
    pub name: String,
    pub url: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub start_on_launch: bool,
    pub active: Option<usize>,
    pub profiles: Vec<Profile>,
    pub subscriptions: Vec<Subscription>,
    pub mode: Mode,
    /// Local listener port in `Proxy` mode. Also used for the loopback
    /// listener in TUN mode so proxy-aware apps can opt in explicitly.
    pub mixed_port: u16,
    /// TUN network stack: `system` is fastest but relies on the OS stack,
    /// `gvisor` is a userspace stack that is slower and more compatible, and
    /// `mixed` uses the system stack for TCP and gvisor for UDP.
    pub tun_stack: String,
    /// Block all traffic whenever the tunnel is not up.
    ///
    /// The window between the tunnel dropping and the user noticing is exactly
    /// when the real address leaks, so this defaults on.
    pub kill_switch: bool,
    /// Drop IPv6 entirely. Many tunnels are v4-only, and a v6 route that
    /// bypasses them defeats the whole arrangement.
    pub block_ipv6: bool,
    pub routing: Routing,
    pub dns: Dns,
    pub tor: Tor,
    /// Port for sing-box's Clash API, used for live throughput readings.
    pub clash_api_port: u16,
    pub hotkey: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            start_on_launch: false,
            active: None,
            profiles: Vec::new(),
            subscriptions: Vec::new(),
            mode: Mode::Tun,
            mixed_port: 2080,
            tun_stack: "mixed".to_string(),
            kill_switch: true,
            block_ipv6: true,
            routing: Routing::default(),
            dns: Dns::default(),
            tor: Tor::default(),
            clash_api_port: 9090,
            hotkey: "Ctrl+Shift+KeyV".to_string(),
        }
    }
}

impl Config {
    pub fn path() -> std::path::PathBuf {
        uikit::config::app_dir(APP_NAME).join("config.toml")
    }

    pub fn load() -> Self {
        let mut cfg: Self = uikit::config::load_or_default(&Self::path());
        cfg.clamp_active();
        cfg
    }

    pub fn save(&self) -> anyhow::Result<()> {
        uikit::config::save(&Self::path(), self)
    }

    /// A stale index would panic on slicing after profiles are removed.
    pub fn clamp_active(&mut self) {
        match self.active {
            Some(index) if index >= self.profiles.len() => {
                self.active = if self.profiles.is_empty() { None } else { Some(0) };
            }
            None if !self.profiles.is_empty() => self.active = Some(0),
            _ => {}
        }
    }

    pub fn active_profile(&self) -> Option<&Profile> {
        self.active.and_then(|index| self.profiles.get(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Outbound, Tls, Transport};

    fn profile(name: &str) -> Profile {
        Profile {
            name: name.to_string(),
            outbound: Outbound::Trojan {
                server: "t.example".into(),
                port: 443,
                password: "pw".into(),
                tls: Tls::default(),
                transport: Transport::Tcp,
            },
        }
    }

    #[test]
    fn round_trips_through_toml() {
        let mut cfg = Config::default();
        cfg.profiles.push(profile("A"));
        cfg.active = Some(0);
        cfg.routing.direct_domains.push("bank.example".into());
        cfg.subscriptions.push(Subscription { name: "S".into(), url: "https://x/sub".into() });

        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn clamps_a_stale_active_index() {
        let mut cfg = Config { profiles: vec![profile("A")], active: Some(5), ..Default::default() };
        cfg.clamp_active();
        assert_eq!(cfg.active, Some(0));

        let mut empty = Config { profiles: vec![], active: Some(3), ..Default::default() };
        empty.clamp_active();
        assert_eq!(empty.active, None);
    }

    #[test]
    fn selects_the_first_profile_when_none_is_active() {
        let mut cfg = Config { profiles: vec![profile("A"), profile("B")], active: None, ..Default::default() };
        cfg.clamp_active();
        assert_eq!(cfg.active_profile().map(|p| p.name.as_str()), Some("A"));
    }

    #[test]
    fn safe_defaults_are_on() {
        let cfg = Config::default();
        assert!(cfg.kill_switch, "the leak window is exactly when this matters");
        assert!(cfg.block_ipv6);
        assert!(cfg.routing.bypass_private);
    }
}
