//! Tracing setup: a rolling daily log under `%APPDATA%\<app>\logs` plus stderr.

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialise logging. Keep the returned guard alive for the process lifetime;
/// dropping it stops the background writer and loses buffered lines.
pub fn init(app: &str, default_level: &str) -> Option<WorkerGuard> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(default_level));

    let log_dir = crate::config::app_dir(app).join("logs");
    let (writer, guard) = match std::fs::create_dir_all(&log_dir) {
        Ok(()) => {
            let appender = tracing_appender::rolling::daily(&log_dir, format!("{app}.log"));
            let (w, g) = tracing_appender::non_blocking(appender);
            (Some(w), Some(g))
        }
        Err(_) => (None, None),
    };

    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_ansi(true).with_target(false).with_writer(std::io::stderr));

    match writer {
        Some(w) => registry
            .with(fmt::layer().with_ansi(false).with_target(false).with_writer(w))
            .init(),
        None => registry.init(),
    }

    guard
}
