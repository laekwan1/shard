//! The client half: open a tunnel, state a destination, hand back a stream.

use crate::protocol::Request;
use crate::tls::{self, Trust};
use crate::{CONNECT_TIMEOUT, RELAY_BUFFER};
use anyhow::{Context, Result};
use rustls::pki_types::ServerName;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};

/// Everything needed to reach one server.
#[derive(Clone, Debug)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub password: String,
    /// Name sent in the TLS handshake. With a pinned certificate this need not
    /// be the server's real name, and choosing an unremarkable one is what
    /// keeps the handshake from standing out.
    pub sni: String,
    pub trust: Trust,
}

impl Server {
    pub fn new(host: impl Into<String>, port: u16, password: impl Into<String>) -> Self {
        let host = host.into();
        Self {
            sni: host.clone(),
            host,
            port,
            password: password.into(),
            trust: Trust::WebPki,
        }
    }

    pub fn with_trust(mut self, trust: Trust) -> Self {
        self.trust = trust;
        self
    }

    pub fn with_sni(mut self, sni: impl Into<String>) -> Self {
        self.sni = sni.into();
        self
    }
}

/// A pooled-nothing client: one TLS connection per outbound request.
///
/// Reusing a connection would mean multiplexing, and multiplexing means a
/// framing layer of our own on top of TLS — more code, and a distinctive
/// pattern on the wire. One connection per request is what a browser already
/// does, so it blends in and stays simple.
pub struct Client {
    server: Server,
    connector: TlsConnector,
}

impl Client {
    pub fn new(server: Server) -> Result<Self> {
        let config = tls::client_config(&server.trust)?;
        Ok(Self { server, connector: TlsConnector::from(Arc::clone(&config)) })
    }

    pub fn server(&self) -> &Server {
        &self.server
    }

    /// Open a tunnelled connection to `host:port`.
    ///
    /// The returned stream starts at the destination's first byte: the header
    /// has already been sent, so a caller can treat it as if it had dialled the
    /// destination directly.
    pub async fn connect(&self, host: &str, port: u16) -> Result<TlsStream<TcpStream>> {
        let mut stream = self.open().await?;
        let header = Request::new(host, port).encode(&self.server.password)?;
        stream.write_all(&header).await.context("요청 헤더를 보낼 수 없습니다")?;
        // Without this the header sits in the buffer until the caller writes,
        // and a protocol where the server speaks first would deadlock.
        stream.flush().await.context("요청 헤더를 내보낼 수 없습니다")?;
        Ok(stream)
    }

    /// Open a tunnelled connection and send `opening` with the header.
    ///
    /// One write rather than two, so the header and the first payload share a
    /// TLS record — which is what a real client's first flight looks like.
    pub async fn connect_with(
        &self,
        host: &str,
        port: u16,
        opening: &[u8],
    ) -> Result<TlsStream<TcpStream>> {
        let mut stream = self.open().await?;
        let mut first = Request::new(host, port).encode(&self.server.password)?;
        first.extend_from_slice(opening);
        stream.write_all(&first).await.context("요청을 보낼 수 없습니다")?;
        stream.flush().await.context("요청을 내보낼 수 없습니다")?;
        Ok(stream)
    }

    async fn open(&self) -> Result<TlsStream<TcpStream>> {
        let address = (self.server.host.as_str(), self.server.port);
        let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address))
            .await
            .with_context(|| format!("{}:{} 연결이 시간 초과되었습니다", self.server.host, self.server.port))?
            .with_context(|| format!("{}:{} 에 연결할 수 없습니다", self.server.host, self.server.port))?;
        // The tunnel carries interactive traffic; waiting to coalesce writes
        // adds latency to every request.
        let _ = tcp.set_nodelay(true);

        let name = ServerName::try_from(self.server.sni.clone())
            .with_context(|| format!("SNI로 쓸 수 없는 이름입니다: {}", self.server.sni))?;

        tokio::time::timeout(CONNECT_TIMEOUT, self.connector.connect(name, tcp))
            .await
            .context("TLS 핸드셰이크가 시간 초과되었습니다")?
            .context("TLS 핸드셰이크에 실패했습니다")
    }
}

/// Copy in both directions, returning (up, down) byte counts.
///
/// The two directions end on different rules, and getting either wrong is a
/// real failure rather than an inefficiency:
///
/// - The client finishing does *not* end the exchange. A client that has sent
///   its whole request and half-closed is still waiting for the response;
///   stopping there would discard it.
/// - The server finishing *does* end it. Once the far end has closed there is
///   nothing more to carry, and a browser routinely leaves its request side
///   open forever — waiting for it would hold both sockets and the task until
///   the process exits.
pub async fn splice<A, B>(a: &mut A, b: &mut B) -> (u64, u64)
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let (mut a_read, mut a_write) = tokio::io::split(a);
    let (mut b_read, mut b_write) = tokio::io::split(b);

    let up = async {
        let n = copy(&mut a_read, &mut b_write).await;
        // Half-close so the far end sees the end of the request rather than
        // waiting for a byte that is never coming.
        let _ = b_write.shutdown().await;
        n
    };
    let down = async {
        let n = copy(&mut b_read, &mut a_write).await;
        let _ = a_write.shutdown().await;
        n
    };

    tokio::pin!(up);
    tokio::pin!(down);

    let mut up_bytes = 0u64;
    let mut up_done = false;
    let down_bytes = loop {
        tokio::select! {
            n = &mut up, if !up_done => {
                up_bytes = n;
                up_done = true;
            }
            n = &mut down => break n,
        }
    };
    (up_bytes, down_bytes)
}

async fn copy<R, W>(from: &mut R, to: &mut W) -> u64
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; RELAY_BUFFER];
    let mut total = 0u64;
    loop {
        match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if to.write_all(&buf[..n]).await.is_err() {
                    break;
                }
                total += n as u64;
            }
        }
    }
    total
}
