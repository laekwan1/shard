//! An HTTP proxy front end.
//!
//! Android's WebView can be pointed at a proxy for one app's web views alone,
//! which turns out to remove the hardest part of the phone design: with the
//! browser inside the app there is no need for a VPN interface, no userspace
//! TCP stack, and no permission prompt. Only what the user browses in this app
//! is affected, which is exactly the intent.
//!
//! WebView speaks plain HTTP proxying for `http://` and CONNECT for `https://`,
//! so both are handled here. Everything downstream is the same relay the SOCKS
//! front end uses.

use crate::relay::{self, Engine, Outcome};
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
/// A request head longer than this is not a browser.
const MAX_HEAD: usize = 32 * 1024;

/// What the client asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    /// `CONNECT host:port` — a tunnel for TLS.
    Connect { host: String, port: u16 },
    /// An ordinary request with an absolute URL, as proxies receive them.
    Plain { host: String, port: u16, head: Vec<u8> },
}

impl Request {
    pub fn host(&self) -> &str {
        match self {
            Request::Connect { host, .. } | Request::Plain { host, .. } => host,
        }
    }

    pub fn port(&self) -> u16 {
        match self {
            Request::Connect { port, .. } | Request::Plain { port, .. } => *port,
        }
    }
}

/// The request head, and whatever arrived behind it.
///
/// The second half is not a detail. Reading a head means reading whatever the
/// same packet carried, and what it carries is very often the beginning of the
/// payload — a POST body, or a TLS handshake sent immediately behind a CONNECT.
/// Those bytes used to go into a buffer that was then dropped, so the payload
/// was silently short by however much had arrived early and the far end waited
/// for a request that never finished.
pub fn read_request(stream: &TcpStream) -> Result<(Request, Vec<u8>)> {
    stream.set_read_timeout(Some(HEADER_TIMEOUT)).ok();
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    reader.read_line(&mut request_line).context("요청 줄을 읽을 수 없습니다")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();
    if method.is_empty() || target.is_empty() {
        bail!("빈 요청");
    }

    // Collect the headers so a plain request can be forwarded verbatim.
    let mut headers = Vec::new();
    let mut total = request_line.len();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        total += n;
        if n == 0 || total > MAX_HEAD {
            break;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        headers.push(line);
    }
    stream.set_read_timeout(None).ok();

    // Whatever the reader took beyond the head belongs to the payload.
    let over = reader.buffer().to_vec();

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_authority(&target, 443)?;
        return Ok((Request::Connect { host, port }, over));
    }

    // Absolute-form: GET http://host/path HTTP/1.1
    let rest = target
        .strip_prefix("http://")
        .context("프록시 요청은 절대 URL이어야 합니다")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = split_authority(authority, 80)?;

    // Rewrite to origin-form, which is what a server expects.
    let mut head = format!("{method} {path} {version}\r\n").into_bytes();
    for line in headers {
        // Hop-by-hop headers must not be forwarded.
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("proxy-connection:") || lower.starts_with("proxy-authorization:") {
            continue;
        }
        head.extend_from_slice(line.as_bytes());
    }
    head.extend_from_slice(b"\r\n");
    Ok((Request::Plain { host, port, head }, over))
}

fn split_authority(authority: &str, default_port: u16) -> Result<(String, u16)> {
    // A bracketed IPv6 literal carries colons that are not a port separator.
    if authority.starts_with('[') {
        let close = authority.find(']').context("닫히지 않은 IPv6 주소")?;
        let port = authority[close + 1..]
            .strip_prefix(':')
            .and_then(|p| p.parse().ok())
            .unwrap_or(default_port);
        return Ok((authority[1..close].to_string(), port));
    }
    if authority.is_empty() {
        bail!("빈 대상 주소");
    }
    // More than one colon and no brackets means a bare IPv6 address. Splitting
    // on the last colon would silently dial a truncated address on a port that
    // was really part of the address.
    if authority.matches(':').count() > 1 {
        return Ok((authority.to_string(), default_port));
    }
    match authority.rsplit_once(':') {
        // An empty host is not a host — ":443" must not become a hostname.
        Some((host, _)) if host.is_empty() => bail!("빈 대상 주소"),
        Some((host, port)) => Ok((host.to_string(), port.parse().unwrap_or(default_port))),
        None => Ok((authority.to_string(), default_port)),
    }
}

/// Resolve through the engine's own resolver.
///
/// Never the platform one directly: on this line it answers a blocked name
/// with the ISP's own address, and connecting there reaches a warning page no
/// matter what the TCP stream looks like.
fn resolve(engine: &Engine, host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    engine.resolver.socket_addrs(host, port)
}

/// Try each resolved address until one connects — a dual-stack host with a
/// broken IPv6 route would otherwise fail outright.
fn dial(addrs: &[SocketAddr]) -> std::io::Result<TcpStream> {
    let mut last = None;
    for addr in addrs {
        match relay::connect(*addr, CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no address")))
}

/// Handle one accepted client.
pub fn serve_one(mut client: TcpStream, engine: &Engine) -> Result<Outcome> {
    let (request, over) = match read_request(&client) {
        Ok(r) => r,
        Err(e) => {
            engine.stats.failed.fetch_add(1, Ordering::Relaxed);
            let _ = client.write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
            return Err(e);
        }
    };

    let addrs = match resolve(engine, request.host(), request.port()) {
        Ok(a) => a,
        Err(e) => {
            engine.stats.failed.fetch_add(1, Ordering::Relaxed);
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n");
            return Err(e);
        }
    };

    let mut upstream = match dial(&addrs) {
        Ok(s) => s,
        Err(e) => {
            engine.stats.failed.fetch_add(1, Ordering::Relaxed);
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n");
            return Err(e).with_context(|| format!("{} 연결 실패", request.host()));
        }
    };

    match request {
        Request::Connect { .. } => {
            // The client waits for this before starting its handshake, so the
            // relay must not read until it has been sent.
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .context("CONNECT 응답 전송 실패")?;
            relay::relay(client, upstream, &engine.policy, &engine.stats).map_err(Into::into)
        }
        Request::Plain { host, head, .. } => {
            // The head has already been consumed, so desync it here rather than
            // letting the relay wait for a first write that will never come.
            let plan = engine.policy.plan(&head);
            let _ = upstream.set_nodelay(true);
            crate::socket::send_desynced(&mut upstream, &head, plan)
                .context("요청 헤드 전송 실패")?;
            // The body, or as much of it as came with the head.
            if !over.is_empty() {
                upstream.write_all(&over).context("요청 본문 전송 실패")?;
            }
            if plan.mode == crate::Mode::None {
                engine.stats.passed_through.fetch_add(1, Ordering::Relaxed);
            } else {
                engine.stats.desynced.fetch_add(1, Ordering::Relaxed);
            }
            engine.stats.connections.fetch_add(1, Ordering::Relaxed);

            relay::pump(client, upstream, &engine.stats)?;
            // Report the name the policy actually matched on. It equals the URL
            // authority in practice, but the learner needs the one the decision
            // was made from, not the address that was dialled.
            let matched = engine.policy.hostname(&head).unwrap_or(host);
            Ok(Outcome { host: Some(matched), plan })
        }
    }
}

/// A listener running on its own thread.
pub struct Server {
    pub port: u16,
    running: Arc<AtomicBool>,
    listener: TcpListener,
}

impl Server {
    /// Bind to loopback. This performs no authentication, so it must never be
    /// reachable from outside the device.
    pub fn bind(port: u16) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .with_context(|| format!("127.0.0.1:{port} 바인드 실패"))?;
        let port = listener.local_addr()?.port();
        Ok(Self { port, running: Arc::new(AtomicBool::new(true)), listener })
    }

    pub fn stop_handle(&self) -> Arc<AtomicBool> {
        self.running.clone()
    }

    pub fn run(self, engine: Arc<Engine>, on_outcome: impl Fn(Result<Outcome>) + Send + Sync + 'static) {
        let on_outcome = Arc::new(on_outcome);
        for incoming in self.listener.incoming() {
            if !self.running.load(Ordering::Relaxed) {
                break;
            }
            let Ok(client) = incoming else { continue };
            let engine = engine.clone();
            let on_outcome = on_outcome.clone();
            std::thread::spawn(move || on_outcome(serve_one(client, &engine)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Policy;
    use shard::config::Config;
    use std::io::Read;
    use std::sync::mpsc;

    fn request_from(text: &str) -> Result<Request> {
        read_pair(text).map(|(r, _)| r)
    }

    fn read_pair(text: &str) -> Result<(Request, Vec<u8>)> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let text = text.to_string();
        std::thread::spawn(move || {
            let mut s = TcpStream::connect(addr).unwrap();
            let _ = s.write_all(text.as_bytes());
            std::thread::sleep(Duration::from_millis(200));
        });
        let (stream, _) = listener.accept().unwrap();
        // Read once everything the client sent is certainly in the receive
        // queue. Without the wait the reader may fill itself before the body
        // arrives, and the test would be measuring the network's timing rather
        // than whether the bytes are kept.
        std::thread::sleep(Duration::from_millis(150));
        read_request(&stream)
    }

    /// A body that arrives with the head is not lost.
    ///
    /// It was. The head was parsed through a reader that fills itself eight
    /// kilobytes at a time; whatever it took beyond the head went with it when
    /// it was dropped, and the relay then read from a socket those bytes had
    /// already left. Every upload through the proxy was short by however much
    /// had arrived early — which for a small POST is all of it.
    #[test]
    fn a_body_arriving_with_the_head_is_kept() {
        let head = "POST http://example.com/upload HTTP/1.1\r\n"
            .to_string()
            + "Host: example.com\r\n"
            + "Content-Length: 11\r\n"
            + "\r\n"
            + "hello world";
        let (request, over) = read_pair(&head).unwrap();
        assert!(matches!(request, Request::Plain { .. }));
        assert_eq!(over, b"hello world");
    }

    /// The same for the handshake a client sends straight behind a CONNECT.
    #[test]
    fn a_handshake_arriving_with_a_connect_is_kept() {
        let head = "CONNECT example.com:443 HTTP/1.1\r\n"
            .to_string()
            + "Host: example.com\r\n"
            + "\r\n"
            + "\x16\x03\x01hello";
        let (request, over) = read_pair(&head).unwrap();
        assert_eq!(request, Request::Connect { host: "example.com".into(), port: 443 });
        assert_eq!(over, b"\x16\x03\x01hello");
    }

    #[test]
    fn parses_connect() {
        let r = request_from("CONNECT example.com:443 HTTP/1.1\r\nHost: example.com\r\n\r\n").unwrap();
        assert_eq!(r, Request::Connect { host: "example.com".into(), port: 443 });
    }

    #[test]
    fn connect_defaults_to_443_without_a_port() {
        let r = request_from("CONNECT example.com HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(r.port(), 443);
    }

    #[test]
    fn rewrites_a_plain_request_to_origin_form() {
        let r = request_from(
            "GET http://example.com/path?x=1 HTTP/1.1\r\nHost: example.com\r\nProxy-Connection: keep-alive\r\n\r\n",
        )
        .unwrap();
        let Request::Plain { host, port, head } = r else { panic!("expected plain") };
        assert_eq!((host.as_str(), port), ("example.com", 80));

        let text = String::from_utf8(head).unwrap();
        assert!(text.starts_with("GET /path?x=1 HTTP/1.1\r\n"), "got: {text}");
        assert!(text.contains("Host: example.com"));
        // Hop-by-hop headers must not reach the server.
        assert!(!text.to_ascii_lowercase().contains("proxy-connection"));
        assert!(text.ends_with("\r\n\r\n"));
    }

    #[test]
    fn keeps_ipv6_literals_intact() {
        let r = request_from("CONNECT [2001:db8::1]:8443 HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(r, Request::Connect { host: "2001:db8::1".into(), port: 8443 });
    }

    #[test]
    fn rejects_an_authority_without_a_host() {
        // ":443" is not a hostname, and treating it as one would dial garbage.
        for bad in ["", ":443", ":"] {
            assert!(split_authority(bad, 80).is_err(), "should have been rejected: {bad:?}");
        }
        // A bare IPv6 literal must keep all of its colons.
        assert_eq!(split_authority("2001:db8::1", 443).unwrap(), ("2001:db8::1".into(), 443));
    }

    #[test]
    fn rejects_a_relative_target() {
        // A proxy only ever receives absolute URLs; a relative one means the
        // client is not talking to a proxy at all.
        assert!(request_from("GET /index.html HTTP/1.1\r\n\r\n").is_err());
        assert!(request_from("\r\n").is_err());
    }

    fn recording_server() -> (SocketAddr, mpsc::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                socket.set_read_timeout(Some(Duration::from_millis(600))).ok();
                let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi");
                let mut got = Vec::new();
                let mut buf = [0u8; 4096];
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

    fn start_server(policy: Policy) -> (u16, Arc<Engine>, mpsc::Receiver<String>) {
        let engine = Arc::new(Engine::new(policy));
        let server = Server::bind(0).unwrap();
        let port = server.port;
        let (tx, rx) = mpsc::channel();
        let for_thread = engine.clone();
        std::thread::spawn(move || {
            server.run(for_thread, move |result| {
                let _ = tx.send(match result {
                    Ok(o) => format!("{}|{:?}", o.host.unwrap_or_default(), o.plan.mode),
                    Err(e) => format!("error|{e}"),
                });
            });
        });
        (port, engine, rx)
    }

    #[test]
    fn a_connect_tunnel_desyncs_the_handshake() {
        let (upstream, recorded) = recording_server();
        let (port, engine, outcomes) = start_server(Policy::new(Config::default()));

        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client.set_nodelay(true).unwrap();
        client
            .write_all(format!("CONNECT {}:{} HTTP/1.1\r\n\r\n", upstream.ip(), upstream.port()).as_bytes())
            .unwrap();

        let mut reply = [0u8; 39];
        client.read_exact(&mut reply).unwrap();
        assert!(String::from_utf8_lossy(&reply).contains("200"), "{:?}", String::from_utf8_lossy(&reply));

        let payload = shard::desync::build_client_hello("blocked.example", 300);
        client.write_all(&payload).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();

        assert_eq!(recorded.recv_timeout(Duration::from_secs(3)).unwrap(), payload);
        assert_eq!(outcomes.recv_timeout(Duration::from_secs(3)).unwrap(), "blocked.example|SplitOob");
        assert_eq!(engine.stats.snapshot().desynced, 1);
    }

    #[test]
    fn a_plain_request_reaches_the_server_rewritten() {
        let (upstream, recorded) = recording_server();
        let (port, _engine, outcomes) = start_server(Policy::new(Config::default()));

        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client.set_nodelay(true).unwrap();
        let request = format!(
            "GET http://{}:{}/video HTTP/1.1\r\nHost: blocked.example\r\n\r\n",
            upstream.ip(),
            upstream.port()
        );
        client.write_all(request.as_bytes()).unwrap();

        // Check the outcome first: if serve_one failed, its message says why,
        // which is far more useful than a timeout on the recorded bytes.
        let outcome = outcomes.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(outcome, "blocked.example|SplitOob");

        let mut back = Vec::new();
        client.set_read_timeout(Some(Duration::from_millis(800))).ok();
        let _ = client.read_to_end(&mut back);
        assert!(String::from_utf8_lossy(&back).contains("200 OK"), "got: {back:?}");

        let arrived = String::from_utf8(recorded.recv_timeout(Duration::from_secs(3)).unwrap()).unwrap();
        assert!(arrived.starts_with("GET /video HTTP/1.1"), "got: {arrived}");
        assert!(arrived.contains("Host: blocked.example"));
    }
}
