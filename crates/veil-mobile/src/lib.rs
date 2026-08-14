//! Veil for phones.
//!
//! The tunnel runs inside the app. That is the whole design change: the earlier
//! attempt shipped the sing-box binary and launched it, which Android refuses —
//! the core needs a netlink socket to watch the network and an ordinary app is
//! not allowed one, so it starts and exits. A library in the app's own process
//! has nothing to ask permission for.
//!
//! What is left here is small on purpose. The protocol, the TLS, the local
//! listener and the routing rules all live in `veil-core`, shared with the
//! desktop and the server, so a link that works on one works on all of them.

#[cfg(target_os = "android")]
pub mod android;

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use veil_core::client::Client;
use veil_core::inbound::{DirectRules, Inbound, Stats, StatsSnapshot};
use veil_core::link;

/// The running tunnel. One per process — a phone has one.
static RUNNING: OnceLock<Mutex<Option<Running>>> = OnceLock::new();

struct Running {
    runtime: tokio::runtime::Runtime,
    stats: Arc<Stats>,
    stop: Arc<AtomicBool>,
    port: u16,
}

fn slot() -> &'static Mutex<Option<Running>> {
    RUNNING.get_or_init(|| Mutex::new(None))
}

/// Start the tunnel for `share_link`, listening on `port` (0 to be assigned one).
///
/// Returns the bound port. The listener is up before this returns, so the
/// caller can point a browser at it immediately rather than racing it.
pub fn start(share_link: &str, port: u16) -> Result<u16> {
    let mut guard = slot().lock().map_err(|_| anyhow::anyhow!("내부 잠금 오류"))?;
    if guard.is_some() {
        anyhow::bail!("이미 실행 중입니다");
    }

    let (server, _name) = link::parse(share_link)?;
    let client = Client::new(server)?;

    // Two worker threads: enough for a browser's concurrency, and small enough
    // not to matter on a phone.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("런타임을 만들 수 없습니다")?;

    let rules = DirectRules::new(veil_core::presets::korean_direct());
    let inbound = runtime.block_on(Inbound::bind(port, client, rules))?;
    let bound = inbound.port()?;
    let stats = Arc::clone(&inbound.stats);
    let stop = inbound.stop_handle();

    runtime.spawn(inbound.run());
    *guard = Some(Running { runtime, stats, stop, port: bound });
    Ok(bound)
}

/// Stop the tunnel. Safe to call when nothing is running.
pub fn stop() {
    let Ok(mut guard) = slot().lock() else { return };
    let Some(running) = guard.take() else { return };

    running.stop.store(false, Ordering::SeqCst);
    // The accept loop only checks the flag between connections, so give it one
    // to return from.
    let _ = std::net::TcpStream::connect(("127.0.0.1", running.port));
    // Not `drop`: that waits for every task, and a relay with a live connection
    // would hold the app on whatever thread called this.
    running.runtime.shutdown_background();
}

pub fn is_running() -> bool {
    slot().lock().map(|g| g.is_some()).unwrap_or(false)
}

pub fn stats() -> StatsSnapshot {
    slot()
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|r| r.stats.snapshot()))
        .unwrap_or_default()
}

/// Check a link without starting anything, so the UI can report why it is bad.
pub fn check_link(share_link: &str) -> Result<String> {
    let (server, name) = link::parse(share_link)?;
    Ok(if name.is_empty() { server.host } else { format!("{name} ({})", server.host) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Built rather than written out: a pin is 64 hex characters, and one
    /// typed by hand is one character short about half the time.
    fn link() -> String {
        format!(
            "trojan://a-password@203.0.113.9:443?sni=www.bing.com&pin={}#오라클",
            "ab".repeat(32)
        )
    }

    #[test]
    fn a_good_link_is_described_for_the_user() {
        assert_eq!(check_link(&link()).unwrap(), "오라클 (203.0.113.9)");
    }

    #[test]
    fn a_bad_link_says_why() {
        for bad in ["", "https://example.com", "trojan://@example.com:443"] {
            assert!(check_link(bad).is_err(), "should have been rejected: {bad:?}");
        }
    }

    #[test]
    fn stats_are_available_before_anything_starts() {
        // The UI polls this on a timer from the moment it opens.
        assert_eq!(stats().connections, 0);
        assert!(!is_running());
    }

    #[test]
    fn stopping_when_nothing_runs_is_harmless() {
        stop();
        stop();
        assert!(!is_running());
    }

    #[test]
    fn the_tunnel_starts_and_stops() {
        // The server is not reachable, which does not matter: the listener has
        // to come up regardless, or the browser has nowhere to point.
        let port = start(&link(), 0).expect("the local listener must bind");
        assert!(port > 0);
        assert!(is_running());

        // A second start must not replace the first silently.
        assert!(start(&link(), 0).is_err());

        stop();
        assert!(!is_running());
    }
}
