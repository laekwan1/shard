//! Shared shell for Shard and Veil.
//!
//! Both apps are tray-resident Windows programs with an egui settings window,
//! a TOML config file under `%APPDATA%`, and a requirement to run elevated.
//! Everything that is identical between them lives here.

//! The config format and logging are portable and always available; everything
//! that draws or talks to Windows sits behind the `gui` feature so the phone
//! build can link the shared config code without a GL context.

pub mod config;
pub mod icon;
pub mod logging;

#[cfg(feature = "gui")]
pub mod elevation;
#[cfg(feature = "gui")]
pub mod single;
#[cfg(feature = "gui")]
pub mod theme;
#[cfg(feature = "gui")]
pub mod tray;
#[cfg(feature = "gui")]
pub mod widgets;

#[cfg(feature = "gui")]
pub use eframe;
#[cfg(feature = "gui")]
pub use egui;
#[cfg(feature = "gui")]
pub use tray_icon;
