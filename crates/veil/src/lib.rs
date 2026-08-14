//! Veil — tunnel and anonymity manager.
//!
//! Veil buys concealment with speed, which is the opposite trade from Shard.
//! It manages a sing-box core process (REALITY, Hysteria2, Trojan,
//! Shadowsocks) and a tor daemon for the cases where trusting a single server
//! is not good enough, plus the surrounding machinery that decides whether the
//! concealment actually holds: a kill switch, DNS that resolves inside the
//! tunnel, and IPv6 containment.

//! The portable half — share links, profiles, and the sing-box configuration
//! they generate — builds everywhere and is what the phone app links. Managing
//! processes, firewall rules and a window is Windows-only and sits behind the
//! `desktop` feature.

pub mod config;
pub mod link;
pub mod presets;
pub mod profile;
pub mod singbox;

#[cfg(feature = "desktop")]
pub mod core;
#[cfg(feature = "desktop")]
pub mod handoff;
#[cfg(feature = "desktop")]
pub mod killswitch;
#[cfg(feature = "desktop")]
pub mod tor;
#[cfg(feature = "desktop")]
pub mod tor_browser;
#[cfg(feature = "desktop")]
pub mod ui;
