//! Local DNS listener that forwards over HTTPS.
//!
//! Queries are relayed byte-for-byte: RFC 8484 carries the same wire format, so
//! there is nothing to translate. Responses are parsed only to feed the
//! address-to-hostname map the engine uses for QUIC.
//!
//! Closing the plaintext-DNS channel matters as much as the SNI work — an ISP
//! that cannot read the hostname from the handshake can still read it from an
//! unencrypted lookup a millisecond earlier.

use crate::config::Doh;
use crate::engine::Shared;
use anyhow::{anyhow, bail, Context, Result};
use std::net::{IpAddr, SocketAddr};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::watch;

const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// EDNS0 lets responses exceed the classic 512-byte limit.
const MAX_UDP_QUERY: usize = 4096;

pub struct Forwarder {
    stop: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
    pub listen: String,
}

impl Forwarder {
    /// Bind and start serving. Returns once the socket is bound, so a port
    /// conflict surfaces here rather than silently in a background thread.
    pub fn start(cfg: Doh, shared: Arc<Shared>) -> Result<Self> {
        if cfg.upstreams.is_empty() {
            bail!("DoH 업스트림이 비어 있습니다");
        }
        let listen = cfg.listen.clone();
        let (stop, stop_rx) = watch::channel(false);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let thread = std::thread::Builder::new()
            .name("shard-doh".to_string())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                rt.block_on(serve(cfg, shared, stop_rx, ready_tx));
            })?;

        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self { stop, thread: Some(thread), listen }),
            Ok(Err(e)) => Err(anyhow!("DNS 리스너를 시작할 수 없습니다: {e}")),
            Err(_) => Err(anyhow!("DNS 리스너가 응답하지 않습니다")),
        }
    }

    pub fn stop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Forwarder {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn serve(
    cfg: Doh,
    shared: Arc<Shared>,
    mut stop: watch::Receiver<bool>,
    ready: mpsc::Sender<Result<(), String>>,
) {
    let udp = match UdpSocket::bind(&cfg.listen).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            let _ = ready.send(Err(format!("{} 바인드 실패: {e}", cfg.listen)));
            return;
        }
    };
    // TCP is the fallback path clients take for truncated answers; without it
    // large responses would silently fail.
    let tcp = TcpListener::bind(&cfg.listen).await.ok();
    let _ = ready.send(Ok(()));

    let client = match build_client(&cfg) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            tracing::error!("could not build DoH client: {e}");
            return;
        }
    };
    let upstreams = Arc::new(cfg.upstreams.clone());
    tracing::info!("DoH forwarder listening on {}", cfg.listen);

    let mut buf = vec![0u8; MAX_UDP_QUERY];
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            result = udp.recv_from(&mut buf) => {
                let Ok((n, peer)) = result else { continue };
                let query = buf[..n].to_vec();
                let (udp, client, upstreams, shared) =
                    (udp.clone(), client.clone(), upstreams.clone(), shared.clone());
                tokio::spawn(async move {
                    let reply = exchange(&client, &upstreams, &query, &shared).await;
                    let _ = udp.send_to(&reply, peer).await;
                });
            }
            result = accept(tcp.as_ref()) => {
                let Some(Ok((mut stream, _))) = result else { continue };
                let (client, upstreams, shared) = (client.clone(), upstreams.clone(), shared.clone());
                tokio::spawn(async move {
                    let _ = serve_tcp(&mut stream, &client, &upstreams, &shared).await;
                });
            }
        }
    }
    tracing::info!("DoH forwarder stopped");
}

/// `select!` needs a future even when TCP failed to bind; this one never resolves.
async fn accept(
    listener: Option<&TcpListener>,
) -> Option<std::io::Result<(tokio::net::TcpStream, SocketAddr)>> {
    match listener {
        Some(l) => Some(l.accept().await),
        None => std::future::pending().await,
    }
}

/// DNS over TCP frames each message with a two-byte length prefix.
async fn serve_tcp(
    stream: &mut tokio::net::TcpStream,
    client: &reqwest::Client,
    upstreams: &[String],
    shared: &Shared,
) -> Result<()> {
    loop {
        let mut len_buf = [0u8; 2];
        if stream.read_exact(&mut len_buf).await.is_err() {
            return Ok(()); // client hung up
        }
        let len = u16::from_be_bytes(len_buf) as usize;
        if len == 0 || len > 65_535 {
            return Ok(());
        }
        let mut query = vec![0u8; len];
        stream.read_exact(&mut query).await?;

        let reply = exchange(client, upstreams, &query, shared).await;
        stream.write_all(&(reply.len() as u16).to_be_bytes()).await?;
        stream.write_all(&reply).await?;
    }
}

/// Forward one query and harvest the addresses from the answer.
async fn exchange(
    client: &reqwest::Client,
    upstreams: &[String],
    query: &[u8],
    shared: &Shared,
) -> Vec<u8> {
    match forward(client, upstreams, query).await {
        Ok(reply) => {
            if let Some((name, addrs)) = crate::dns::answer_addresses(&reply) {
                for addr in addrs {
                    shared.note_resolution(addr, &name);
                }
            }
            reply
        }
        Err(e) => {
            tracing::warn!("DoH lookup failed: {e}");
            crate::dns::servfail(query)
        }
    }
}

async fn forward(client: &reqwest::Client, upstreams: &[String], query: &[u8]) -> Result<Vec<u8>> {
    let mut last: Option<anyhow::Error> = None;
    // Kept in case every resolver refuses, so something still goes back.
    let mut refused: Option<Vec<u8>> = None;
    for url in upstreams {
        let attempt = client
            .post(url)
            .header("content-type", "application/dns-message")
            .header("accept", "application/dns-message")
            .body(query.to_vec())
            .send()
            .await;
        match attempt {
            Ok(response) if response.status().is_success() => {
                let reply = response.bytes().await?.to_vec();
                // A resolver that will not answer for a name says so with an
                // address of nothing rather than with an error. That is a
                // refusal, not an answer, and the next resolver may not share
                // it — trying it is the whole point of having more than one.
                if refuses(&reply) {
                    tracing::info!("{url} answered with nothing; trying the next resolver");
                    refused = Some(reply);
                    continue;
                }
                return Ok(reply);
            }
            Ok(response) => last = Some(anyhow!("{url} 응답 {}", response.status())),
            Err(e) => last = Some(anyhow!("{url}: {e}")),
        }
    }
    // Every one of them refused. The answer still goes back: it is what the
    // network says, and inventing a different one would be worse.
    if let Some(reply) = refused {
        return Ok(reply);
    }
    Err(last.unwrap_or_else(|| anyhow!("사용 가능한 업스트림이 없습니다")))
}

/// Whether an answer is a refusal written as an address.
///
/// `0.0.0.0` and `::` are nowhere. A resolver hands one back for a name it has
/// been told not to answer for, and a browser given it simply fails to connect
/// with nothing to say about why.
fn refuses(reply: &[u8]) -> bool {
    match crate::dns::answer_addresses(reply) {
        Some((_, addresses)) if !addresses.is_empty() => addresses.iter().all(nowhere),
        _ => false,
    }
}

/// Whether an address is nowhere at all.
///
/// A four-byte answer is kept in the sixteen-byte form with `::ffff:` in front
/// of it, so "all zeroes" is not the test — the marker in the middle is never
/// zero.
fn nowhere(addr: &[u8; 16]) -> bool {
    let mapped = addr[..10].iter().all(|b| *b == 0) && addr[10] == 0xff && addr[11] == 0xff;
    if mapped {
        addr[12..].iter().all(|b| *b == 0)
    } else {
        addr.iter().all(|b| *b == 0)
    }
}

/// One-shot encrypted lookup.
///
/// The prober needs this: resolving through the system resolver would mean
/// testing a hostname against whatever address the network under test chose to
/// hand back. If that answer is poisoned, every strategy fails for a reason
/// that has nothing to do with the handshake, and the probe reports "no
/// strategy works" when the real problem is DNS.
pub fn resolve_encrypted(cfg: &Doh, host: &str) -> Option<IpAddr> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    runtime.block_on(async {
        let client = build_client(cfg).ok()?;
        let query = crate::dns::build_query(host, crate::dns::TYPE_A, 0x5344);
        let reply = match forward(&client, &cfg.upstreams, &query).await {
            Ok(reply) => reply,
            Err(e) => {
                tracing::info!("encrypted lookup for {host} failed: {e}");
                return None;
            }
        };
        let (_, addresses) = crate::dns::answer_addresses(&reply)?;
        addresses
            .into_iter()
            .map(|a| IpAddr::from(std::net::Ipv4Addr::new(a[0], a[1], a[2], a[3])))
            .find(|a| !a.is_unspecified())
    })
}

/// Pin each upstream's address so resolving the resolver does not depend on the
/// very DNS we are replacing — and cannot be redirected by a poisoned answer.
fn build_client(cfg: &Doh) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(QUERY_TIMEOUT)
        .pool_idle_timeout(Duration::from_secs(90))
        .user_agent("shard/0.1");

    // The named entries first, so a list that says which host it means is not
    // also read as a list to be counted through.
    let named: Vec<(String, IpAddr)> = cfg.bootstrap.iter().filter_map(|line| named_bootstrap(line)).collect();
    let bare: Vec<&String> = cfg.bootstrap.iter().filter(|line| named_bootstrap(line).is_none()).collect();

    for (index, url) in cfg.upstreams.iter().enumerate() {
        let Some(host) = url_host(url) else {
            tracing::warn!("skipping malformed upstream {url}");
            continue;
        };
        // Named for this host if it says so; otherwise the one that stands in
        // the same place in the list, which is what the two lists have always
        // meant when they are simply two lists of the same length.
        let found = named
            .iter()
            .find(|(name, _)| *name == host)
            .map(|(_, ip)| *ip)
            .or_else(|| match bare.get(index) {
                Some(line) => match line.parse::<IpAddr>() {
                    Ok(ip) => Some(ip),
                    Err(e) => {
                        tracing::warn!("bootstrap {line} is not an address: {e}");
                        None
                    }
                },
                // Said out loud, because the effect of getting this wrong is a
                // resolver that quietly stops being reachable without the DNS
                // it was brought in to replace.
                None => {
                    tracing::warn!("no bootstrap address for {host}; it will be looked up normally");
                    None
                }
            });
        if let Some(ip) = found {
            builder = builder.resolve(&host, SocketAddr::new(ip, 443));
        }
    }
    builder.build().context("building the DoH HTTP client")
}

/// A bootstrap line that names the host it belongs to: `dns.example 1.2.3.4`,
/// or with an `=` between them.
///
/// Two lists paired by position hold only while nobody edits them. Reordering
/// the upstreams, or removing one, pairs every line below it with the wrong
/// host — and a host pinned to somebody else's address simply stops working,
/// with nothing on screen to say why. Naming the host makes the pairing the
/// user's own words rather than an accident of line numbers.
fn named_bootstrap(line: &str) -> Option<(String, IpAddr)> {
    // Trimmed before it is split: a line that begins with a space would
    // otherwise split there and be read as having no name at all.
    let line = line.trim();
    let (name, address) = line.split_once('=').or_else(|| line.split_once(char::is_whitespace))?;
    let ip = address.trim().parse::<IpAddr>().ok()?;
    let name = name.trim().to_ascii_lowercase();
    (!name.is_empty()).then_some((name, ip))
}

/// Host portion of an `https://host/path` URL, without pulling in a URL parser.
fn url_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let host = rest.split('/').next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = host.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_upstream_hosts() {
        assert_eq!(url_host("https://cloudflare-dns.com/dns-query").as_deref(), Some("cloudflare-dns.com"));
        assert_eq!(url_host("https://dns.google/dns-query").as_deref(), Some("dns.google"));
        assert_eq!(url_host("https://example.com:8443/q").as_deref(), Some("example.com"));
        assert_eq!(url_host("https://DNS.Example.COM/q").as_deref(), Some("dns.example.com"));
    }

    #[test]
    fn rejects_non_https_upstreams() {
        // Plain HTTP would defeat the point of the whole module.
        assert!(url_host("http://dns.example/q").is_none());
        assert!(url_host("dns.example").is_none());
        assert!(url_host("https:///q").is_none());
    }

    #[test]
    fn client_builds_with_bootstrap_pinning() {
        let cfg = Doh::default();
        assert!(build_client(&cfg).is_ok());
    }

    #[test]
    fn client_tolerates_missing_bootstrap_entries() {
        let cfg = Doh {
            upstreams: vec!["https://a.example/dns-query".into(), "https://b.example/dns-query".into()],
            bootstrap: vec!["1.1.1.1".into()],
            ..Default::default()
        };
        assert!(build_client(&cfg).is_ok());
    }

    #[test]
    fn an_answer_of_nothing_is_a_refusal() {
        // One A record of 0.0.0.0 is how a resolver says it will not answer for
        // a name. The next resolver may not agree, so the reply is not taken.
        let mut reply = crate::dns::build_query("blocked.example", crate::dns::TYPE_A, 1);
        reply[2] = 0x81; // a response
        reply[3] = 0x80;
        reply[7] = 1; // one answer
        reply.extend_from_slice(&[0xc0, 0x0c]); // the name, pointed at
        reply.extend_from_slice(&[0, 1, 0, 1]); // A, IN
        reply.extend_from_slice(&[0, 0, 0, 60]); // time to live
        reply.extend_from_slice(&[0, 4, 0, 0, 0, 0]); // four bytes of nothing
        assert!(refuses(&reply));

        let mut real = reply.clone();
        let at = real.len() - 4;
        real[at..].copy_from_slice(&[93, 184, 216, 34]);
        assert!(!refuses(&real));

        // A question with no answer at all is not a refusal; it is a miss.
        let plain = crate::dns::build_query("nothing.example", crate::dns::TYPE_A, 1);
        assert!(!refuses(&plain));
    }

    #[test]
    fn bootstrap_lines_can_name_their_host() {
        assert_eq!(
            named_bootstrap("dns.google=8.8.8.8"),
            Some(("dns.google".to_string(), "8.8.8.8".parse().unwrap()))
        );
        assert_eq!(
            named_bootstrap("  DNS.Google   8.8.4.4 "),
            Some(("dns.google".to_string(), "8.8.4.4".parse().unwrap()))
        );
        // A bare address is not a naming; it keeps its place in the list.
        assert_eq!(named_bootstrap("1.1.1.1"), None);
        assert_eq!(named_bootstrap("dns.google=not-an-address"), None);
        assert_eq!(named_bootstrap("=1.1.1.1"), None);
    }

    #[test]
    fn a_named_bootstrap_is_not_counted_as_a_bare_one() {
        // The pairing that matters: the named line belongs to b, and a still
        // gets the first bare address rather than being pushed along by it.
        let cfg = Doh {
            upstreams: vec!["https://a.example/dns-query".into(), "https://b.example/dns-query".into()],
            bootstrap: vec!["b.example=9.9.9.9".into(), "1.1.1.1".into()],
            ..Default::default()
        };
        let named: Vec<_> = cfg.bootstrap.iter().filter_map(|l| named_bootstrap(l)).collect();
        let bare: Vec<_> = cfg.bootstrap.iter().filter(|l| named_bootstrap(l).is_none()).collect();
        assert_eq!(named.len(), 1);
        assert_eq!(bare, vec![&"1.1.1.1".to_string()]);
        assert!(build_client(&cfg).is_ok());
    }

    #[test]
    fn empty_upstreams_are_rejected_at_start() {
        let shared = crate::engine::Shared::new(crate::config::Config::default());
        let cfg = Doh { upstreams: vec![], ..Default::default() };
        assert!(Forwarder::start(cfg, shared).is_err());
    }
}
