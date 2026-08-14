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
                return Ok(response.bytes().await?.to_vec());
            }
            Ok(response) => last = Some(anyhow!("{url} 응답 {}", response.status())),
            Err(e) => last = Some(anyhow!("{url}: {e}")),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("사용 가능한 업스트림이 없습니다")))
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
        let reply = forward(&client, &cfg.upstreams, &query).await.ok()?;
        let (_, addresses) = crate::dns::answer_addresses(&reply)?;
        addresses
            .into_iter()
            .next()
            .map(|a| IpAddr::from(std::net::Ipv4Addr::new(a[0], a[1], a[2], a[3])))
    })
}

/// Pin each upstream's address so resolving the resolver does not depend on the
/// very DNS we are replacing — and cannot be redirected by a poisoned answer.
fn build_client(cfg: &Doh) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(QUERY_TIMEOUT)
        .pool_idle_timeout(Duration::from_secs(90))
        .user_agent("shard/0.1");

    for (index, url) in cfg.upstreams.iter().enumerate() {
        let Some(host) = url_host(url) else {
            tracing::warn!("skipping malformed upstream {url}");
            continue;
        };
        let Some(bootstrap) = cfg.bootstrap.get(index) else { continue };
        match bootstrap.parse::<IpAddr>() {
            Ok(ip) => builder = builder.resolve(&host, SocketAddr::new(ip, 443)),
            Err(e) => tracing::warn!("bootstrap {bootstrap} is not an address: {e}"),
        }
    }
    builder.build().context("building the DoH HTTP client")
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
    fn empty_upstreams_are_rejected_at_start() {
        let shared = crate::engine::Shared::new(crate::config::Config::default());
        let cfg = Doh { upstreams: vec![], ..Default::default() };
        assert!(Forwarder::start(cfg, shared).is_err());
    }
}
