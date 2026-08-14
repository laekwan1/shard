//! The local listener the browser talks to.
//!
//! One port speaks both HTTP proxying and SOCKS5, told apart by the first byte
//! a client sends: SOCKS5 always opens with 0x05, and no HTTP method does. That
//! saves binding two ports and means the phone only has one number to remember.
//!
//! Some destinations must not go through the tunnel at all. Korean banking and
//! government sites reject a foreign exit address outright, so they are dialled
//! directly — the tunnel would not make them private, it would make them fail.

use crate::client::{splice, Client};
use anyhow::{bail, Context, Result};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const MAX_HEAD: usize = 32 * 1024;

#[derive(Default, Debug)]
pub struct Stats {
    pub connections: AtomicU64,
    pub tunnelled: AtomicU64,
    pub direct: AtomicU64,
    pub failed: AtomicU64,
    pub bytes_up: AtomicU64,
    pub bytes_down: AtomicU64,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct StatsSnapshot {
    pub connections: u64,
    pub tunnelled: u64,
    pub direct: u64,
    pub failed: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
}

impl Stats {
    pub fn snapshot(&self) -> StatsSnapshot {
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        StatsSnapshot {
            connections: get(&self.connections),
            tunnelled: get(&self.tunnelled),
            direct: get(&self.direct),
            failed: get(&self.failed),
            bytes_up: get(&self.bytes_up),
            bytes_down: get(&self.bytes_down),
        }
    }
}

/// Which destinations skip the tunnel.
#[derive(Clone, Default, Debug)]
pub struct DirectRules {
    suffixes: Vec<String>,
}

impl DirectRules {
    pub fn new(suffixes: impl IntoIterator<Item = String>) -> Self {
        Self { suffixes: suffixes.into_iter().map(|s| s.to_ascii_lowercase()).collect() }
    }

    /// True when `host` is the listed domain or a subdomain of it.
    ///
    /// Matching on a bare suffix would make `evilkbstar.com` match
    /// `kbstar.com`, so a boundary is required.
    pub fn applies_to(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.suffixes.iter().any(|suffix| {
            host == *suffix
                || (host.len() > suffix.len()
                    && host.ends_with(suffix.as_str())
                    && host.as_bytes()[host.len() - suffix.len() - 1] == b'.')
        })
    }
}

/// What the client asked for, once parsed.
#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
    /// Bytes already read that belong to the destination, not the handshake.
    pub opening: Vec<u8>,
    /// What to send back before relaying, if the protocol needs an ack.
    pub ack: Vec<u8>,
}

pub struct Inbound {
    listener: TcpListener,
    client: Arc<Client>,
    rules: DirectRules,
    pub stats: Arc<Stats>,
    running: Arc<AtomicBool>,
}

impl Inbound {
    pub async fn bind(port: u16, client: Client, rules: DirectRules) -> Result<Self> {
        // Loopback only. There is no authentication here, and there must not be
        // anything on the network able to reach it.
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .with_context(|| format!("127.0.0.1:{port} 바인드 실패"))?;
        Ok(Self {
            listener,
            client: Arc::new(client),
            rules,
            stats: Arc::new(Stats::default()),
            running: Arc::new(AtomicBool::new(true)),
        })
    }

    pub fn port(&self) -> Result<u16> {
        Ok(self.listener.local_addr()?.port())
    }

    pub fn stop_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.running)
    }

    pub async fn run(self) {
        while self.running.load(Ordering::Relaxed) {
            let Ok((socket, _)) = self.listener.accept().await else { continue };
            let client = Arc::clone(&self.client);
            let rules = self.rules.clone();
            let stats = Arc::clone(&self.stats);
            tokio::spawn(async move {
                stats.connections.fetch_add(1, Ordering::Relaxed);
                if let Err(e) = serve(socket, client, rules, &stats).await {
                    stats.failed.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!("connection failed: {e:#}");
                }
            });
        }
    }
}

async fn serve(
    mut socket: TcpStream,
    client: Arc<Client>,
    rules: DirectRules,
    stats: &Stats,
) -> Result<()> {
    let _ = socket.set_nodelay(true);
    let target = read_target(&mut socket).await?;

    if !target.ack.is_empty() {
        socket.write_all(&target.ack).await.context("응답을 보낼 수 없습니다")?;
    }

    let (up, down) = if rules.applies_to(&target.host) {
        stats.direct.fetch_add(1, Ordering::Relaxed);
        let mut upstream = TcpStream::connect((target.host.as_str(), target.port))
            .await
            .with_context(|| format!("{}:{} 직결 실패", target.host, target.port))?;
        let _ = upstream.set_nodelay(true);
        if !target.opening.is_empty() {
            upstream.write_all(&target.opening).await?;
        }
        splice(&mut socket, &mut upstream).await
    } else {
        stats.tunnelled.fetch_add(1, Ordering::Relaxed);
        let mut tunnel = client
            .connect_with(&target.host, target.port, &target.opening)
            .await
            .with_context(|| format!("{}:{} 터널 연결 실패", target.host, target.port))?;
        splice(&mut socket, &mut tunnel).await
    };

    stats.bytes_up.fetch_add(up + target.opening.len() as u64, Ordering::Relaxed);
    stats.bytes_down.fetch_add(down, Ordering::Relaxed);
    Ok(())
}

/// Work out where the client wants to go, whichever protocol it speaks.
async fn read_target(socket: &mut TcpStream) -> Result<Target> {
    let mut first = [0u8; 1];
    // Peek rather than read: the HTTP parser wants the whole request line.
    let n = socket.peek(&mut first).await.context("첫 바이트를 읽을 수 없습니다")?;
    if n == 0 {
        bail!("클라이언트가 아무것도 보내지 않았습니다");
    }
    if first[0] == 0x05 {
        socks5(socket).await
    } else {
        http(socket).await
    }
}

// ---- SOCKS5 ---------------------------------------------------------------

async fn socks5(socket: &mut TcpStream) -> Result<Target> {
    let mut head = [0u8; 2];
    socket.read_exact(&mut head).await.context("SOCKS 인사말을 읽을 수 없습니다")?;
    let mut methods = vec![0u8; head[1] as usize];
    socket.read_exact(&mut methods).await.context("SOCKS 인증 방식을 읽을 수 없습니다")?;
    // 0x00: no authentication. The listener is loopback-only, so there is
    // nothing a password would protect against here.
    socket.write_all(&[0x05, 0x00]).await?;

    let mut request = [0u8; 4];
    socket.read_exact(&mut request).await.context("SOCKS 요청을 읽을 수 없습니다")?;
    if request[1] != 0x01 {
        bail!("SOCKS CONNECT만 지원합니다 (받은 명령: 0x{:02x})", request[1]);
    }

    let host = match request[3] {
        0x01 => {
            let mut octets = [0u8; 4];
            socket.read_exact(&mut octets).await?;
            std::net::Ipv4Addr::from(octets).to_string()
        }
        0x03 => {
            let mut len = [0u8; 1];
            socket.read_exact(&mut len).await?;
            let mut name = vec![0u8; len[0] as usize];
            socket.read_exact(&mut name).await?;
            String::from_utf8(name).context("SOCKS 도메인이 UTF-8이 아닙니다")?
        }
        0x04 => {
            let mut octets = [0u8; 16];
            socket.read_exact(&mut octets).await?;
            std::net::Ipv6Addr::from(octets).to_string()
        }
        other => bail!("알 수 없는 SOCKS 주소 종류: 0x{other:02x}"),
    };
    let mut port = [0u8; 2];
    socket.read_exact(&mut port).await?;

    Ok(Target {
        host,
        port: u16::from_be_bytes(port),
        opening: Vec::new(),
        // Success, bound to 0.0.0.0:0 — clients ignore the address.
        ack: vec![0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0],
    })
}

// ---- HTTP -----------------------------------------------------------------

async fn http(socket: &mut TcpStream) -> Result<Target> {
    let mut reader = BufReader::new(socket);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await.context("요청 줄을 읽을 수 없습니다")?;

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();
    if method.is_empty() || target.is_empty() {
        bail!("빈 요청");
    }

    let mut headers = Vec::new();
    let mut total = request_line.len();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        total += n;
        if n == 0 || total > MAX_HEAD || line == "\r\n" || line == "\n" {
            break;
        }
        headers.push(line);
    }

    // Whatever the reader took beyond the head belongs to the destination.
    //
    // A reader fills itself in blocks, so reading a request head reads whatever
    // else arrived in the same packet — and what arrives with it is very often
    // the start of the payload: a POST body, or the TLS handshake a client
    // sends immediately behind a CONNECT without waiting to be answered. Those
    // bytes went out with the reader, which left every upload short by however
    // much had arrived early and some connections never started at all.
    let over = reader.buffer().to_vec();

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_authority(&target, 443)?;
        return Ok(Target {
            host,
            port,
            opening: over,
            ack: b"HTTP/1.1 200 Connection Established\r\n\r\n".to_vec(),
        });
    }

    let rest = target
        .strip_prefix("http://")
        .context("프록시 요청은 절대 URL이어야 합니다")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = split_authority(authority, 80)?;

    // Rewrite to origin form, which is what a server expects.
    let mut opening = format!("{method} {path} {version}\r\n").into_bytes();
    for line in headers {
        let lower = line.to_ascii_lowercase();
        // Hop-by-hop headers describe the link to the proxy, not to the server.
        if lower.starts_with("proxy-connection:") || lower.starts_with("proxy-authorization:") {
            continue;
        }
        opening.extend_from_slice(line.as_bytes());
    }
    opening.extend_from_slice(b"\r\n");
    // The body, or as much of it as came with the head.
    opening.extend_from_slice(&over);

    Ok(Target { host, port, opening, ack: Vec::new() })
}

pub(crate) fn split_authority(authority: &str, default_port: u16) -> Result<(String, u16)> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt as _;

    /// Speak to the inbound the way a browser does, and see what it made of it.
    async fn read_from(text: &'static str) -> Target {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut client = TcpStream::connect(addr).await.unwrap();
            let _ = client.write_all(text.as_bytes()).await;
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });
        let (mut socket, _) = listener.accept().await.unwrap();
        // Read once everything sent is certainly in the receive queue, so the
        // test measures what is kept rather than how the network split it.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        read_target(&mut socket).await.unwrap()
    }

    /// A body that arrives with the head is carried through, not dropped.
    ///
    /// The head is parsed through a reader that fills itself in blocks, and
    /// whatever it took past the head went with it — so an upload was short by
    /// however much had arrived early, which for a small POST is all of it.
    #[tokio::test]
    async fn a_body_arriving_with_the_head_is_kept() {
        let target = read_from(
            "POST http://example.com/upload HTTP/1.1\r\n\
             Host: example.com\r\n\
             Content-Length: 11\r\n\
             \r\n\
             hello world",
        )
        .await;
        assert_eq!(target.host, "example.com");
        assert_eq!(target.port, 80);
        let sent = String::from_utf8(target.opening).unwrap();
        assert!(sent.starts_with("POST /upload HTTP/1.1\r\n"), "got: {sent}");
        assert!(sent.ends_with("\r\n\r\nhello world"), "the body was lost: {sent}");
    }

    /// And the handshake a client sends straight behind a CONNECT.
    #[tokio::test]
    async fn a_handshake_arriving_with_a_connect_is_kept() {
        let target = read_from(
            "CONNECT example.com:443 HTTP/1.1\r\n\
             Host: example.com\r\n\
             \r\n\
             \x16\x03\x01hello",
        )
        .await;
        assert_eq!((target.host.as_str(), target.port), ("example.com", 443));
        assert_eq!(target.opening, b"\x16\x03\x01hello");
    }

    #[test]
    fn direct_rules_match_a_domain_and_its_subdomains() {
        let rules = DirectRules::new(["kbstar.com".into(), "go.kr".into()]);
        assert!(rules.applies_to("kbstar.com"));
        assert!(rules.applies_to("obank.kbstar.com"));
        assert!(rules.applies_to("www.nts.go.kr"));
        assert!(rules.applies_to("KBSTAR.COM"), "matching must ignore case");
    }

    #[test]
    fn direct_rules_stop_at_a_label_boundary() {
        // Without the boundary check a lookalike domain would be handed the
        // same treatment as the bank it imitates.
        let rules = DirectRules::new(["kbstar.com".into()]);
        assert!(!rules.applies_to("evilkbstar.com"));
        assert!(!rules.applies_to("kbstar.com.attacker.example"));
        assert!(!rules.applies_to("example.com"));
    }

    #[test]
    fn an_empty_rule_set_sends_everything_through_the_tunnel() {
        assert!(!DirectRules::default().applies_to("anything.example"));
    }

    #[test]
    fn authorities_split_into_host_and_port() {
        assert_eq!(split_authority("example.com", 80).unwrap(), ("example.com".into(), 80));
        assert_eq!(split_authority("example.com:8443", 80).unwrap(), ("example.com".into(), 8443));
        assert_eq!(split_authority("[2001:db8::1]:443", 80).unwrap(), ("2001:db8::1".into(), 443));
        assert_eq!(split_authority("[2001:db8::1]", 443).unwrap(), ("2001:db8::1".into(), 443));
    }

    #[test]
    fn an_authority_without_a_host_is_refused() {
        // ":443" is not a hostname, and treating it as one would dial garbage.
        for bad in ["", ":443", ":"] {
            assert!(split_authority(bad, 80).is_err(), "should have been rejected: {bad:?}");
        }
    }

    #[test]
    fn a_bare_ipv6_address_keeps_all_of_its_colons() {
        // Splitting on the last colon would give host "2001:db8:" port 1.
        assert_eq!(split_authority("2001:db8::1", 443).unwrap(), ("2001:db8::1".into(), 443));
    }
}
