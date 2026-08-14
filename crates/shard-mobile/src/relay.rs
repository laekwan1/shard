//! Relaying one connection, with the desync applied to its opening payload.
//!
//! This is the piece the whole phone design turns on. Whatever feeds it —
//! a TUN interface on a phone, a SOCKS listener on a desktop — the job is the
//! same: read the client's first write, decide how to break it up, send it that
//! way, then get out of the way and copy bytes.
//!
//! Only the opening payload is touched. Everything after it is a plain copy, so
//! throughput is unaffected once the connection is established.

use crate::{Mode, Plan, Policy};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How long to wait for the client's opening write before giving up on finding
/// a hostname and relaying blind.
const FIRST_WRITE_TIMEOUT: Duration = Duration::from_millis(1500);
const RELAY_BUFFER: usize = 32 * 1024;
/// How often the upload side wakes to notice the connection is over.
const IDLE_TICK: Duration = Duration::from_millis(250);

#[derive(Default)]
pub struct Stats {
    pub connections: AtomicU64,
    pub desynced: AtomicU64,
    pub passed_through: AtomicU64,
    pub failed: AtomicU64,
    pub bytes_up: AtomicU64,
    pub bytes_down: AtomicU64,
}

/// A plain copy of the counters, for a UI.
#[derive(Clone, Copy, Default, Debug)]
pub struct StatsSnapshot {
    pub connections: u64,
    pub desynced: u64,
    pub passed_through: u64,
    pub failed: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
}

impl Stats {
    pub fn snapshot(&self) -> StatsSnapshot {
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        StatsSnapshot {
            connections: get(&self.connections),
            desynced: get(&self.desynced),
            passed_through: get(&self.passed_through),
            failed: get(&self.failed),
            bytes_up: get(&self.bytes_up),
            bytes_down: get(&self.bytes_down),
        }
    }
}

/// What happened to one connection, for logging and learning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub host: Option<String>,
    pub plan: Plan,
}

/// Relay `client` to `target`, desyncing the first payload.
///
/// Takes both streams by value because it owns them until the connection ends.
pub fn relay(
    client: TcpStream,
    upstream: TcpStream,
    policy: &Policy,
    stats: &Stats,
) -> std::io::Result<Outcome> {
    relay_after(client, upstream, &[], policy, stats)
}

/// The same, when some of the client's first write has already been read.
///
/// A front end that parses a request head reads whatever arrived with it, and
/// what arrives with it is often the beginning of the payload — a POST body, or
/// a TLS handshake sent immediately behind a CONNECT. Those bytes are the
/// opening write; handing them over here is what stops them being read once and
/// then waited for a second time that never comes.
pub fn relay_after(
    mut client: TcpStream,
    mut upstream: TcpStream,
    already: &[u8],
    policy: &Policy,
    stats: &Stats,
) -> std::io::Result<Outcome> {
    stats.connections.fetch_add(1, Ordering::Relaxed);

    // Segment boundaries only survive if the kernel is not allowed to merge
    // writes, which is the entire mechanism here.
    let _ = upstream.set_nodelay(true);
    let _ = client.set_nodelay(true);

    // The opening write is the only one carrying a hostname. Anything the
    // caller already took off the client is the start of it.
    let mut opening = vec![0u8; RELAY_BUFFER.max(already.len() + RELAY_BUFFER)];
    opening[..already.len()].copy_from_slice(already);
    let mut read = already.len();
    // Only waited for when nothing has been handed over. A client that has
    // already spoken is not going to be asked to speak again before its words
    // are passed on.
    if read == 0 {
    let _ = client.set_read_timeout(Some(FIRST_WRITE_TIMEOUT));
    read = match client.read(&mut opening) {
        // The client half-closed without sending anything, or has not spoken
        // yet. Either way the server may still have something to say, so relay
        // rather than tearing the connection down.
        Ok(0) => 0,
        Ok(n) => n,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock => 0,
        Err(e) => {
            stats.failed.fetch_add(1, Ordering::Relaxed);
            return Err(e);
        }
    };
    let _ = client.set_read_timeout(None);
    }
    let opening = &opening[..read];

    let host = policy.hostname(opening);
    let plan = if opening.is_empty() { Plan { mode: Mode::None, at: 0 } } else { policy.plan(opening) };

    if plan.mode == Mode::None {
        stats.passed_through.fetch_add(1, Ordering::Relaxed);
    } else {
        stats.desynced.fetch_add(1, Ordering::Relaxed);
    }

    if !opening.is_empty() {
        if let Err(e) = crate::socket::send_desynced(&mut upstream, opening, plan) {
            stats.failed.fetch_add(1, Ordering::Relaxed);
            return Err(e);
        }
        stats.bytes_up.fetch_add(opening.len() as u64, Ordering::Relaxed);
    }

    pump(client, upstream, stats)?;
    Ok(Outcome { host, plan })
}

/// Copy in both directions until either side closes.
///
/// Public because a front end that has already consumed the opening payload —
/// the HTTP proxy reads the request head to parse it — needs to hand over the
/// two streams without going through the first-write logic again.
pub fn pump(client: TcpStream, upstream: TcpStream, stats: &Stats) -> std::io::Result<()> {
    let mut client_read = client.try_clone()?;
    let mut client_write = client;
    let mut up_read = upstream.try_clone()?;
    let mut up_write = upstream;

    // The upload side sits in a blocking read waiting for a browser that has
    // finished asking and may never speak again. Waking it on a timer is what
    // lets it notice the connection is over: on Windows a shutdown issued from
    // another thread does not reliably interrupt a read already in progress,
    // so without this the thread survives until the process exits.
    let _ = client_read.set_read_timeout(Some(IDLE_TICK));
    let done = AtomicBool::new(false);
    let finished = &done;

    // A scoped thread keeps both halves sharing the same stats without an Arc.
    std::thread::scope(|scope| {
        let up = scope.spawn(move || {
            let n = copy_until(&mut client_read, &mut up_write, finished);
            // Half-close so the server sees EOF rather than waiting for more.
            let _ = up_write.shutdown(Shutdown::Write);
            n
        });

        let down = copy(&mut up_read, &mut client_write);
        stats.bytes_down.fetch_add(down, Ordering::Relaxed);

        // The server is done, so the connection is over.
        done.store(true, Ordering::Relaxed);
        let _ = client_write.shutdown(Shutdown::Both);

        if let Ok(n) = up.join() {
            stats.bytes_up.fetch_add(n, Ordering::Relaxed);
        }
    });
    Ok(())
}

/// Copy until the source ends, an error occurs, or the other direction reports
/// the connection finished.
fn copy_until(from: &mut impl Read, to: &mut impl Write, done: &AtomicBool) -> u64 {
    let mut buf = vec![0u8; RELAY_BUFFER];
    let mut total = 0u64;
    loop {
        match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if to.write_all(&buf[..n]).is_err() {
                    break;
                }
                total += n as u64;
            }
            // An idle tick, not a failure — keep waiting unless we are done.
            Err(e) if timed_out(&e) => {
                if done.load(Ordering::Relaxed) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    total
}

fn timed_out(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock)
}

fn copy(from: &mut impl Read, to: &mut impl Write) -> u64 {
    let mut buf = vec![0u8; RELAY_BUFFER];
    let mut total = 0u64;
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if to.write_all(&buf[..n]).is_err() {
                    break;
                }
                total += n as u64;
            }
        }
    }
    total
}

/// Open the upstream connection for a session.
pub fn connect(target: SocketAddr, timeout: Duration) -> std::io::Result<TcpStream> {
    TcpStream::connect_timeout(&target, timeout)
}

/// Shared handle for a running engine.
pub struct Engine {
    pub policy: Policy,
    pub stats: Arc<Stats>,
    /// Names are resolved here, never by the platform directly — the platform's
    /// answer for a blocked name is the ISP's block page.
    pub resolver: crate::resolve::Resolver,
}

impl Engine {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            stats: Arc::new(Stats::default()),
            resolver: crate::resolve::Resolver::default(),
        }
    }

    /// An engine that resolves through the platform, for tests and for a
    /// network where the encrypted resolver cannot be reached.
    pub fn with_platform_dns(policy: Policy) -> Self {
        Self {
            policy,
            stats: Arc::new(Stats::default()),
            resolver: crate::resolve::Resolver::new(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shard::config::Config;
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// Server that records exactly what arrived and echoes a fixed reply.
    fn recording_server(reply: &'static [u8]) -> (SocketAddr, mpsc::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                socket.set_read_timeout(Some(Duration::from_millis(600))).ok();
                let mut got = Vec::new();
                let mut buf = [0u8; 4096];
                let _ = socket.write_all(reply);
                while let Ok(n) = socket.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    got.extend_from_slice(&buf[..n]);
                }
                let _ = tx.send(got);
            }
        });
        (addr, rx)
    }

    /// Drive one relay: connect a client pair, hand both ends over, send the
    /// payload, and collect what the far end saw.
    fn run_relay(policy: &Policy, payload: Vec<u8>) -> (Outcome, Vec<u8>, Vec<u8>, StatsSnapshot) {
        let (server_addr, recorded) = recording_server(b"HELLO-BACK");
        let gate = TcpListener::bind("127.0.0.1:0").unwrap();
        let gate_addr = gate.local_addr().unwrap();

        let stats = Stats::default();
        let outcome = std::thread::scope(|scope| {
            let sender = scope.spawn(move || {
                let mut c = TcpStream::connect(gate_addr).unwrap();
                c.set_nodelay(true).unwrap();
                c.write_all(&payload).unwrap();
                c.shutdown(Shutdown::Write).unwrap();
                let mut back = Vec::new();
                c.set_read_timeout(Some(Duration::from_millis(800))).ok();
                let _ = c.read_to_end(&mut back);
                back
            });

            let (client, _) = gate.accept().unwrap();
            let upstream = TcpStream::connect(server_addr).unwrap();
            let outcome = relay(client, upstream, policy, &stats).unwrap();
            let back = sender.join().unwrap();
            (outcome, back)
        });

        let arrived = recorded.recv_timeout(Duration::from_secs(3)).unwrap_or_default();
        (outcome.0, arrived, outcome.1, stats.snapshot())
    }

    fn hello(host: &str) -> Vec<u8> {
        shard::desync::build_client_hello(host, 300)
    }

    #[test]
    fn the_far_end_receives_the_payload_intact() {
        // Desync must be invisible to the server: same bytes, same order, and
        // the hostname still readable.
        let policy = Policy::new(Config::default());
        let payload = hello("blocked.example");
        let (outcome, arrived, _, stats) = run_relay(&policy, payload.clone());

        assert_eq!(outcome.plan.mode, Mode::SplitOob);
        assert_eq!(outcome.host.as_deref(), Some("blocked.example"));
        assert_eq!(arrived, payload, "the urgent byte must not land in the stream");
        assert_eq!(stats.desynced, 1);
        assert_eq!(stats.passed_through, 0);
    }

    #[test]
    fn the_reply_reaches_the_client() {
        let policy = Policy::new(Config::default());
        let (_, _, back, stats) = run_relay(&policy, hello("blocked.example"));
        assert_eq!(back, b"HELLO-BACK");
        assert!(stats.bytes_down >= 10);
    }

    #[test]
    fn an_excluded_host_passes_through_untouched() {
        let config = Config { exclude: vec!["bank.example".into()], ..Default::default() };
        let policy = Policy::new(config);
        let payload = hello("bank.example");
        let (outcome, arrived, _, stats) = run_relay(&policy, payload.clone());

        assert_eq!(outcome.plan.mode, Mode::None);
        assert_eq!(arrived, payload);
        assert_eq!(stats.passed_through, 1);
        assert_eq!(stats.desynced, 0);
    }

    #[test]
    fn plain_http_is_handled_too() {
        let policy = Policy::new(Config::default());
        let payload = b"GET / HTTP/1.1\r\nHost: blocked.example\r\nAccept: */*\r\n\r\n".to_vec();
        let (outcome, arrived, _, _) = run_relay(&policy, payload.clone());

        assert_eq!(outcome.host.as_deref(), Some("blocked.example"));
        assert_eq!(arrived, payload);
    }

    #[test]
    fn a_client_that_says_nothing_is_not_desynced() {
        // Some protocols have the server speak first; there is nothing to split.
        let policy = Policy::new(Config::default());
        let (outcome, _, back, _) = run_relay(&policy, Vec::new());
        assert_eq!(outcome.plan.mode, Mode::None);
        assert_eq!(back, b"HELLO-BACK");
    }
}
