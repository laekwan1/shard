//! The server half: verify, dial, relay — or serve a website to a stranger.
//!
//! The fallback is what makes the server uninteresting to look at. Anyone who
//! connects without the password gets whatever the fallback address serves,
//! byte for byte, including the bytes they already sent. A prober trying to
//! work out what is running here finds a web server, because from their side
//! that is genuinely all it is.

use crate::client::splice;
use crate::protocol::{hash_matches, password_hash, Request};
use crate::{CONNECT_TIMEOUT, RELAY_BUFFER};
use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

/// What [`Fallback::Builtin`] serves. Deliberately dull: a page nobody would
/// look at twice is a better disguise than one that invites questions.
const BUILTIN_BODY: &str = concat!(
    "<!doctype html><html><head><title>Welcome</title></head>",
    "<body><h1>It works!</h1><p>This is the default page.</p></body></html>",
);

/// The page with its headers. Built rather than written out so the declared
/// length can never drift from the body — a mismatch leaves a browser waiting,
/// which is precisely the odd behaviour the fallback exists to avoid.
fn builtin_page() -> &'static [u8] {
    static PAGE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    PAGE.get_or_init(|| {
        format!(
            "HTTP/1.1 200 OK\r\n\
             Server: nginx\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n{BUILTIN_BODY}",
            BUILTIN_BODY.len()
        )
        .into_bytes()
    })
}

/// A header longer than this is not one of ours.
const MAX_HEADER: usize = 1024;
/// How long a client has to send its header before the fallback takes over.
const HEADER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// What an unauthenticated connection is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// Hand the connection to a real web server, usually on loopback. The most
    /// convincing option, because the visitor is talking to an actual site.
    Address(String),
    /// Answer from here with a small static page. Less convincing under close
    /// inspection — one page, no assets — but it needs nothing installed, which
    /// matters on a box whose only job is this.
    Builtin,
}

impl Fallback {
    /// `"builtin"`, or a `host:port` to forward to.
    pub fn parse(spec: &str) -> Self {
        if spec.eq_ignore_ascii_case("builtin") {
            Fallback::Builtin
        } else {
            Fallback::Address(spec.to_string())
        }
    }
}

pub struct Config {
    pub listen: SocketAddr,
    pub password: String,
    pub fallback: Fallback,
}

/// What happened to one connection, for logging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Relayed { host: String, port: u16, up: u64, down: u64 },
    /// Sent to the fallback: wrong password, malformed header, or silence.
    FellBack { reason: &'static str },
}

pub struct Server {
    config: Config,
    expected: String,
    acceptor: TlsAcceptor,
    listener: TcpListener,
}

impl Server {
    pub async fn bind(config: Config, tls: Arc<rustls::ServerConfig>) -> Result<Self> {
        let listener = TcpListener::bind(config.listen)
            .await
            .with_context(|| format!("{} 바인드에 실패했습니다", config.listen))?;
        Ok(Self {
            expected: password_hash(&config.password),
            acceptor: TlsAcceptor::from(tls),
            config,
            listener,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.listener.local_addr()?)
    }

    /// Accept forever, handing each outcome to `on_outcome`.
    pub async fn run(self, on_outcome: impl Fn(Result<Outcome>) + Send + Sync + 'static) -> Result<()> {
        let on_outcome = Arc::new(on_outcome);
        let shared = Arc::new(Shared {
            expected: self.expected,
            fallback: self.config.fallback,
            acceptor: self.acceptor,
        });

        loop {
            let (socket, _) = match self.listener.accept().await {
                Ok(pair) => pair,
                // One failed accept is not a reason to stop serving.
                Err(e) => {
                    tracing::warn!("accept failed: {e}");
                    continue;
                }
            };
            let shared = Arc::clone(&shared);
            let on_outcome = Arc::clone(&on_outcome);
            tokio::spawn(async move {
                on_outcome(shared.serve(socket).await);
            });
        }
    }
}

/// Whether what has arrived so far could still be the opening hash.
///
/// Only the bytes covering the hash are judged; past that the decoder takes
/// over. An empty buffer is undecided, not wrong.
fn looks_like_hash(buffered: &[u8]) -> bool {
    let checked = buffered.len().min(crate::protocol::PASSWORD_HEX_LEN);
    buffered[..checked].iter().all(u8::is_ascii_hexdigit)
}

struct Shared {
    expected: String,
    fallback: Fallback,
    acceptor: TlsAcceptor,
}

impl Shared {
    async fn serve(&self, socket: TcpStream) -> Result<Outcome> {
        let _ = socket.set_nodelay(true);
        let mut tls = self.acceptor.accept(socket).await.context("TLS 핸드셰이크 실패")?;

        // Read until the header is complete, or until it is clear it never
        // will be. Whatever arrives is kept: the fallback has to replay it.
        let mut buffered = Vec::with_capacity(512);
        let mut chunk = vec![0u8; RELAY_BUFFER];
        let (request, header_len) = loop {
            // Anything that is not hex cannot become a password hash however
            // much more arrives. Deciding that now rather than at the timeout
            // is what keeps the server from answering a short probe exactly ten
            // seconds later every time — a real web server replies at once, and
            // a consistent delay is itself a way to recognise this port.
            if !looks_like_hash(&buffered) {
                return self.fall_back(tls, &buffered, "헤더 형식이 다릅니다").await;
            }

            match Request::decode(&buffered) {
                Ok(Some(parsed)) => break parsed,
                Ok(None) if buffered.len() >= MAX_HEADER => {
                    return self.fall_back(tls, &buffered, "헤더가 너무 깁니다").await;
                }
                Ok(None) => {}
                Err(_) => return self.fall_back(tls, &buffered, "헤더 형식이 다릅니다").await,
            }

            let read = match tokio::time::timeout(HEADER_TIMEOUT, tls.read(&mut chunk)).await {
                Ok(Ok(0)) => return self.fall_back(tls, &buffered, "헤더 도중 연결이 끊겼습니다").await,
                Ok(Ok(n)) => n,
                Ok(Err(e)) => return Err(e).context("헤더를 읽을 수 없습니다"),
                Err(_) => return self.fall_back(tls, &buffered, "헤더를 기다리다 시간이 지났습니다").await,
            };
            buffered.extend_from_slice(&chunk[..read]);
        };

        let claimed = Request::claimed_password(&buffered).unwrap_or_default();
        if !hash_matches(&self.expected, claimed) {
            return self.fall_back(tls, &buffered, "비밀번호가 다릅니다").await;
        }

        let host = request.host();
        let port = request.port;
        let mut upstream = tokio::time::timeout(
            CONNECT_TIMEOUT,
            TcpStream::connect((host.as_str(), port)),
        )
        .await
        .with_context(|| format!("{host}:{port} 연결 시간 초과"))?
        .with_context(|| format!("{host}:{port} 에 연결할 수 없습니다"))?;
        let _ = upstream.set_nodelay(true);

        // Anything that arrived with the header belongs to the destination.
        let leftover = &buffered[header_len..];
        if !leftover.is_empty() {
            upstream.write_all(leftover).await.context("첫 페이로드 전달 실패")?;
        }

        let (up, down) = splice(&mut tls, &mut upstream).await;
        Ok(Outcome::Relayed { host, port, up: up + leftover.len() as u64, down })
    }

    /// Hand the connection to the fallback, replaying what was already read.
    async fn fall_back<S>(&self, mut client: S, already_read: &[u8], reason: &'static str) -> Result<Outcome>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let address = match &self.fallback {
            Fallback::Builtin => {
                let _ = client.write_all(builtin_page()).await;
                let _ = client.shutdown().await;
                return Ok(Outcome::FellBack { reason });
            }
            Fallback::Address(address) => address,
        };

        let mut upstream = match TcpStream::connect(address).await {
            Ok(s) => s,
            // Nothing sensible to say — closing is what a web server behind a
            // failed upstream would do anyway.
            Err(e) => {
                tracing::debug!("fallback {address} unreachable: {e}");
                return Ok(Outcome::FellBack { reason });
            }
        };
        let _ = upstream.set_nodelay(true);
        if !already_read.is_empty() {
            let _ = upstream.write_all(already_read).await;
        }
        splice(&mut client, &mut upstream).await;
        Ok(Outcome::FellBack { reason })
    }
}
