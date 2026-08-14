//! Name resolution that the ISP does not get to answer.
//!
//! This is not an optimisation. On a Korean line the resolver answers a blocked
//! name with its own address — measured: `xvideos.com` comes back as
//! `168.126.63.1`, which is KT's DNS server. A connection to that address
//! reaches a warning page, and no amount of work on the TCP stream matters
//! because the packets were never going to the right place.
//!
//! So the name is resolved over HTTPS to a resolver of our choosing, and any
//! address known to be a redirection target is discarded whatever the source.
//!
//! Blocking rather than async on purpose: the rest of the phone engine is
//! thread-per-connection, and adding a runtime for one request per connection
//! would cost more than it saves.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Resolvers reached by address, so nothing has to be resolved to resolve.
///
/// Both families, and that is not a nicety: Korean mobile data is frequently
/// IPv6-only behind 464XLAT, where a v4 literal is simply unreachable. With
/// only the v4 address here every lookup would wait out the timeout before
/// falling back, and a page that names twenty hosts would appear to hang.
const DOH_ADDRESSES: &[(IpAddr, u16)] = &[
    (IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 443),
    (
        IpAddr::V6(std::net::Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111)),
        443,
    ),
];
const DOH_HOST: &str = "cloudflare-dns.com";
const DOH_PATH: &str = "/dns-query";

/// Short on purpose. This is in front of every first connection, so a resolver
/// that is not answering has to be given up on quickly rather than correctly.
const TIMEOUT: Duration = Duration::from_millis(2500);
/// After this many failures in a row, stop trying for a while.
const FAILURES_BEFORE_PAUSE: u32 = 3;
const PAUSE: Duration = Duration::from_secs(120);
const CACHE_TTL: Duration = Duration::from_secs(300);
/// A DNS reply larger than this is not one we asked for.
const MAX_REPLY: usize = 8 * 1024;

const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;

/// Addresses that mean "blocked", not "here is the site".
///
/// KT answers with the address of its own resolver. Treating that as a real
/// answer is what puts the browser on the warning page.
/// Loopback is deliberately not here: an app that refused to resolve
/// `localhost` would break its own proxy and any local development, and a
/// loopback answer for a public name is not the signature seen on this line.
const REDIRECTORS: &[IpAddr] = &[
    IpAddr::V4(Ipv4Addr::new(168, 126, 63, 1)),
    IpAddr::V4(Ipv4Addr::new(168, 126, 63, 2)),
    IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
];

/// True when this address is a block page rather than the site.
pub fn is_redirector(address: &IpAddr) -> bool {
    REDIRECTORS.contains(address)
}

/// Drop the block-page addresses, failing if that leaves nothing.
///
/// Failing is the right outcome: dialling the remaining block page would show
/// the warning site, which looks like the bypass quietly not working rather
/// than like a name that could not be resolved honestly.
fn keep_usable(host: &str, addresses: Vec<IpAddr>) -> Result<Vec<IpAddr>> {
    let usable: Vec<IpAddr> = addresses.into_iter().filter(|a| !is_redirector(a)).collect();
    if usable.is_empty() {
        bail!("{host} 의 주소가 모두 차단 안내 주소였습니다");
    }
    Ok(usable)
}

#[derive(Clone)]
pub struct Resolver {
    cache: Arc<Mutex<HashMap<String, Cached>>>,
    /// Set false to fall back to the platform resolver, for a network where
    /// the DoH endpoint itself cannot be reached.
    enabled: bool,
    health: Arc<Mutex<Health>>,
}

/// Whether the encrypted resolver is worth asking right now.
///
/// On a network that blocks it, retrying for every name would add the timeout
/// to every single connection. One round of failures is enough to conclude the
/// network is hostile to it and stand down for a while.
#[derive(Default)]
struct Health {
    consecutive_failures: u32,
    paused_until: Option<Instant>,
}

impl Health {
    fn usable(&self) -> bool {
        self.paused_until.map(|until| Instant::now() >= until).unwrap_or(true)
    }

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.paused_until = None;
    }

    fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= FAILURES_BEFORE_PAUSE {
            self.paused_until = Some(Instant::now() + PAUSE);
            self.consecutive_failures = 0;
        }
    }
}

#[derive(Clone)]
struct Cached {
    addresses: Vec<IpAddr>,
    at: Instant,
}

impl Default for Resolver {
    fn default() -> Self {
        Self::new(true)
    }
}

impl Resolver {
    pub fn new(enabled: bool) -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
            enabled,
            health: Arc::new(Mutex::new(Health::default())),
        }
    }

    /// Whether the encrypted resolver will be tried for the next lookup.
    pub fn doh_available(&self) -> bool {
        self.enabled && self.health.lock().map(|h| h.usable()).unwrap_or(false)
    }

    /// Addresses for `host`, best source first.
    ///
    /// A literal address is returned as-is. Otherwise the encrypted resolver is
    /// tried, and the platform resolver is the fallback — filtered either way,
    /// because the platform's answer is the one that is poisoned.
    pub fn lookup(&self, host: &str) -> Result<Vec<IpAddr>> {
        if let Ok(literal) = host.parse::<IpAddr>() {
            return Ok(vec![literal]);
        }
        if let Some(hit) = self.cached(host) {
            return Ok(hit);
        }

        let mut addresses = Vec::new();
        if self.doh_available() {
            match self.over_https(host) {
                Ok(found) if !found.is_empty() => {
                    addresses = found;
                    if let Ok(mut health) = self.health.lock() {
                        health.record_success();
                    }
                }
                // An empty answer is a real answer — the name has no address of
                // that family — so it must not count against the resolver.
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!("DoH lookup for {host} failed: {e:#}");
                    if let Ok(mut health) = self.health.lock() {
                        health.record_failure();
                    }
                }
            }
        }
        if addresses.is_empty() {
            addresses = platform(host)?;
        }

        let usable = keep_usable(host, addresses)?;
        self.store(host, &usable);
        Ok(usable)
    }

    /// Resolved addresses paired with `port`, ready to dial.
    pub fn socket_addrs(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>> {
        Ok(self.lookup(host)?.into_iter().map(|a| SocketAddr::new(a, port)).collect())
    }

    fn cached(&self, host: &str) -> Option<Vec<IpAddr>> {
        let cache = self.cache.lock().ok()?;
        let hit = cache.get(host)?;
        (hit.at.elapsed() < CACHE_TTL).then(|| hit.addresses.clone())
    }

    fn store(&self, host: &str, addresses: &[IpAddr]) {
        if let Ok(mut cache) = self.cache.lock() {
            // A cache that only grows is a leak on a long-running app.
            if cache.len() > 512 {
                cache.clear();
            }
            cache.insert(host.to_string(), Cached { addresses: addresses.to_vec(), at: Instant::now() });
        }
    }

    /// Ask the encrypted resolver. IPv4 first; IPv6 only if there is no IPv4,
    /// because a phone with a broken v6 route would otherwise stall on it.
    fn over_https(&self, host: &str) -> Result<Vec<IpAddr>> {
        let mut found = query(host, TYPE_A)?;
        if found.is_empty() {
            found = query(host, TYPE_AAAA)?;
        }
        Ok(found)
    }
}

/// The platform resolver — used only when the encrypted one cannot be reached.
fn platform(host: &str) -> Result<Vec<IpAddr>> {
    let addresses: Vec<IpAddr> = (host, 0u16)
        .to_socket_addrs()
        .with_context(|| format!("{host} 주소를 확인할 수 없습니다"))?
        .map(|s| s.ip())
        .collect();
    if addresses.is_empty() {
        bail!("{host} 에 대한 주소 레코드가 없습니다");
    }
    Ok(addresses)
}

/// One DoH request, wire format in and out (RFC 8484).
fn query(host: &str, qtype: u16) -> Result<Vec<IpAddr>> {
    // The transaction id is not a security property here — the answer arrives
    // over TLS on a connection we opened — so a fixed value is fine and keeps
    // this free of a random source.
    let question = shard::dns::build_query(host, qtype, 0);

    let mut request = format!(
        "POST {DOH_PATH} HTTP/1.1\r\n\
         Host: {DOH_HOST}\r\n\
         Accept: application/dns-message\r\n\
         Content-Type: application/dns-message\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        question.len()
    )
    .into_bytes();
    request.extend_from_slice(&question);

    let reply = round_trip(&request)?;
    let body = http_body(&reply).context("DoH 응답 본문을 찾을 수 없습니다")?;

    let (_, raw) = shard::dns::answer_addresses(body).context("DNS 응답을 해석할 수 없습니다")?;
    Ok(raw.into_iter().map(to_ip).collect())
}

/// A 16-byte answer is either an IPv6 address or an IPv4 one in v4-mapped form.
fn to_ip(bytes: [u8; 16]) -> IpAddr {
    let v6 = std::net::Ipv6Addr::from(bytes);
    match v6.to_ipv4_mapped() {
        Some(v4) => IpAddr::V4(v4),
        None => IpAddr::V6(v6),
    }
}

/// Send `request` over TLS to the resolver and read the whole reply.
///
/// Each address family is tried in turn: on a v6-only network the v4 literal is
/// unreachable and vice versa, and there is no way to know in advance which one
/// this phone is on.
fn round_trip(request: &[u8]) -> Result<Vec<u8>> {
    let mut last = None;
    for (ip, port) in DOH_ADDRESSES {
        match round_trip_via(SocketAddr::new(*ip, *port), request) {
            Ok(reply) => return Ok(reply),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("DoH 주소가 설정되지 않았습니다")))
}

fn round_trip_via(address: SocketAddr, request: &[u8]) -> Result<Vec<u8>> {
    crate::tls::install_provider();

    let config = crate::tls::doh_config()?;
    let name = rustls::pki_types::ServerName::try_from(DOH_HOST).context("DoH 호스트 이름이 잘못되었습니다")?;
    let connection = rustls::ClientConnection::new(config, name).context("TLS 세션을 만들 수 없습니다")?;

    let socket = TcpStream::connect_timeout(&address, TIMEOUT)
        .with_context(|| format!("{address} 에 연결할 수 없습니다"))?;
    socket.set_read_timeout(Some(TIMEOUT)).ok();
    socket.set_write_timeout(Some(TIMEOUT)).ok();

    let mut stream = rustls::StreamOwned::new(connection, socket);
    stream.write_all(request).context("DoH 요청을 보낼 수 없습니다")?;
    stream.flush().ok();

    let mut reply = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                reply.extend_from_slice(&chunk[..n]);
                if reply.len() > MAX_REPLY {
                    break;
                }
                // The resolver closes after the body, but a peer that does not
                // would hold this open; stop once the declared body is in.
                if let Some(body) = http_body(&reply) {
                    if let Some(declared) = content_length(&reply) {
                        if body.len() >= declared {
                            break;
                        }
                    }
                }
            }
            // close_notify missing at the end of a response is common enough
            // that treating it as failure would break most requests.
            Err(e) if reply.is_empty() => return Err(e).context("DoH 응답을 읽을 수 없습니다"),
            Err(_) => break,
        }
    }
    Ok(reply)
}

/// The body of an HTTP response, if the headers are complete.
fn http_body(response: &[u8]) -> Option<&[u8]> {
    let split = response.windows(4).position(|w| w == b"\r\n\r\n")?;
    Some(&response[split + 4..])
}

fn content_length(response: &[u8]) -> Option<usize> {
    let head = &response[..response.windows(4).position(|w| w == b"\r\n\r\n")?];
    std::str::from_utf8(head)
        .ok()?
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().ok())?
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_literal_address_is_returned_unchanged() {
        let resolver = Resolver::new(false);
        assert_eq!(resolver.lookup("203.0.113.9").unwrap(), vec!["203.0.113.9".parse::<IpAddr>().unwrap()]);
        assert_eq!(resolver.lookup("::1").unwrap(), vec!["::1".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn the_isps_redirection_address_is_recognised() {
        // The measured signature of a blocked name on this line.
        assert!(is_redirector(&"168.126.63.1".parse().unwrap()));
        assert!(!is_redirector(&"1.1.1.1".parse().unwrap()));
        assert!(!is_redirector(&"89.222.127.6".parse().unwrap()));
    }

    #[test]
    fn a_name_that_only_resolves_to_a_block_page_is_an_error() {
        // Measured: this is exactly what `xvideos.com` returns on this line.
        let poisoned = vec!["168.126.63.1".parse().unwrap()];
        let error = keep_usable("xvideos.com", poisoned).unwrap_err().to_string();
        assert!(error.contains("차단 안내"), "got: {error}");
    }

    #[test]
    fn a_real_address_alongside_a_block_page_survives() {
        // Measured: `www.pornhub.com` comes back with both, and taking the
        // first would land on the warning site about half the time.
        let mixed = vec![
            "168.126.63.1".parse().unwrap(),
            "66.254.114.41".parse().unwrap(),
        ];
        assert_eq!(
            keep_usable("www.pornhub.com", mixed).unwrap(),
            vec!["66.254.114.41".parse::<IpAddr>().unwrap()]
        );
    }

    #[test]
    fn loopback_still_resolves() {
        // The app's own proxy listens there; treating it as a block page would
        // break the engine against itself.
        let resolver = Resolver::new(false);
        assert!(resolver.lookup("localhost").is_ok());
    }

    #[test]
    fn a_v4_mapped_answer_becomes_a_v4_address() {
        // The wire parser returns everything as 16 bytes; keeping A records as
        // v6-mapped would make them undialable on a v4-only network.
        let mut mapped = [0u8; 16];
        mapped[10] = 0xff;
        mapped[11] = 0xff;
        mapped[12..].copy_from_slice(&[93, 184, 216, 34]);
        assert_eq!(to_ip(mapped), "93.184.216.34".parse::<IpAddr>().unwrap());

        let v6 = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        assert_eq!(to_ip(v6), "2001:db8::1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn an_http_body_is_found_after_the_headers() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc";
        assert_eq!(http_body(response), Some(&b"abc"[..]));
        assert_eq!(content_length(response), Some(3));
        assert_eq!(http_body(b"HTTP/1.1 200 OK\r\nContent-Len"), None);
    }

    #[test]
    fn both_address_families_are_available_for_the_resolver() {
        // A v4-only list makes every lookup wait out the timeout on an
        // IPv6-only mobile network, which reads as the app being broken.
        assert!(DOH_ADDRESSES.iter().any(|(ip, _)| ip.is_ipv4()));
        assert!(DOH_ADDRESSES.iter().any(|(ip, _)| ip.is_ipv6()));
    }

    #[test]
    fn repeated_failures_stand_the_resolver_down() {
        // On a network that blocks it, retrying per name would add the timeout
        // to every connection the browser makes.
        let mut health = Health::default();
        assert!(health.usable());

        for _ in 0..FAILURES_BEFORE_PAUSE {
            health.record_failure();
        }
        assert!(!health.usable(), "should have paused after repeated failures");
    }

    #[test]
    fn one_success_clears_the_failure_count() {
        let mut health = Health::default();
        health.record_failure();
        health.record_failure();
        health.record_success();
        health.record_failure();

        // Two before, one after: without the reset this would already be paused.
        assert!(health.usable());
    }

    #[test]
    fn the_cache_returns_what_was_stored_and_expires() {
        let resolver = Resolver::new(false);
        let address: IpAddr = "93.184.216.34".parse().unwrap();
        resolver.store("example.com", &[address]);
        assert_eq!(resolver.cached("example.com"), Some(vec![address]));

        // A stale entry must not be served.
        if let Ok(mut cache) = resolver.cache.lock() {
            if let Some(entry) = cache.get_mut("example.com") {
                entry.at = Instant::now() - CACHE_TTL - Duration::from_secs(1);
            }
        }
        assert_eq!(resolver.cached("example.com"), None);
    }
}
