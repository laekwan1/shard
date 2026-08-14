//! Shard — a local DPI/SNI bypass engine.
//!
//! Shard never proxies traffic. It intercepts outbound packets with WinDivert,
//! rewrites the first data segment of a connection so a middlebox cannot read
//! the hostname out of it, and lets the packets continue to their real
//! destination. There is no tunnel and no extra hop, which is why it costs
//! effectively no bandwidth or latency — and also why it provides no anonymity
//! whatsoever. That is Veil's job.

//! The portable core — parsers, strategy model, config format — builds
//! everywhere and is what the phone app links. Everything that needs the packet
//! driver or a window sits behind the `desktop` feature, so a phone build does
//! not drag in WinDivert, a GL context or a tray icon.

pub mod config;
pub mod desync;
// Wire-format DNS is portable, and the phone needs it for the same reason the
// desktop does: a resolver that answers a blocked name with the ISP's own
// address makes every other defence irrelevant.
pub mod dns;
// The YouTube protocol work is portable and heavily tested; only the browser
// window and the file writing that use it are desktop-only.
pub mod download;
pub mod net;
pub mod parse;
pub mod strategy;

#[cfg(feature = "desktop")]
pub mod doh;
#[cfg(feature = "desktop")]
pub mod engine;
/// What has been saved, read back off the user's own media folders.
#[cfg(feature = "desktop")]
pub mod library;
/// The program without a face: the engine and its DNS, driven by either shell.
#[cfg(feature = "desktop")]
pub mod core;
/// Saving a video: the offer, the list, and the bytes.
#[cfg(feature = "desktop")]
pub mod downloads;
/// The settings, described once and driven from the page.
#[cfg(feature = "desktop")]
pub mod settings;
/// The one window: the shell page, and the frame around it.
#[cfg(feature = "desktop")]
pub mod shell;
#[cfg(feature = "desktop")]
pub mod prober;
#[cfg(feature = "desktop")]
pub mod sysdns;
#[cfg(feature = "desktop")]
pub mod ui;
#[cfg(feature = "desktop")]
pub mod windivert;
