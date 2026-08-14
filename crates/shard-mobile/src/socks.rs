//! A SOCKS5 front end for the relay.
//!
//! The phone build will be fed by a TUN interface, but that needs a userspace
//! TCP stack and a signed app before any of it can be tried. A SOCKS listener
//! reaches the same relay through a few dozen lines and runs on a desktop, so
//! the socket-level desync can be measured against a real blocked site long
//! before a line of Swift exists.
//!
//! It is not throwaway scaffolding either: pointing a browser at this is a
//! perfectly good way to use the engine without a driver or elevation.

use crate::relay::{self, Engine, Outcome};
use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const VERSION: u8 = 5;
const CMD_CONNECT: u8 = 1;
const ATYP_IPV4: u8 = 1;
const ATYP_DOMAIN: u8 = 3;
const ATYP_IPV6: u8 = 4;

const REPLY_OK: u8 = 0;
const REPLY_GENERAL_FAILURE: u8 = 1;
const REPLY_HOST_UNREACHABLE: u8 = 4;
const REPLY_COMMAND_NOT_SUPPORTED: u8 = 7;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Where the client asked to go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Addr(SocketAddr),
    Domain(String, u16),
}

impl Target {
    pub fn describe(&self) -> String {
        match self {
            Target::Addr(a) => a.to_string(),
            Target::Domain(host, port) => format!("{host}:{port}"),
        }
    }

    /// Every address this target resolves to, in the order to try them.
    ///
    /// A hostname usually has both A and AAAA records, and taking only the
    /// first strands the connection whenever the network has no working route
    /// for that family — commonly IPv6.
    pub fn resolve(&self) -> Result<Vec<SocketAddr>> {
        match self {
            Target::Addr(a) => Ok(vec![*a]),
            Target::Domain(host, port) => {
                let addrs: Vec<SocketAddr> = (host.as_str(), *port)
                    .to_socket_addrs()
                    .with_context(|| format!("{host} 주소를 확인할 수 없습니다"))?
                    .collect();
                if addrs.is_empty() {
                    bail!("{host} 에 대한 주소 레코드가 없습니다");
                }
                Ok(addrs)
            }
        }
    }
}

/// Read the greeting and the CONNECT request.
pub fn accept_request(stream: &mut TcpStream) -> Result<Target> {
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).ok();

    // Greeting: version, method count, methods.
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).context("SOCKS 인사말을 읽을 수 없습니다")?;
    if head[0] != VERSION {
        bail!("SOCKS5가 아닙니다 (version {})", head[0]);
    }
    let mut methods = vec![0u8; head[1] as usize];
    stream.read_exact(&mut methods)?;
    // No authentication. This listens on loopback only.
    stream.write_all(&[VERSION, 0])?;

    // Request: version, command, reserved, address type.
    let mut request = [0u8; 4];
    stream.read_exact(&mut request).context("SOCKS 요청을 읽을 수 없습니다")?;
    if request[0] != VERSION {
        bail!("잘못된 SOCKS 버전");
    }
    if request[1] != CMD_CONNECT {
        reply(stream, REPLY_COMMAND_NOT_SUPPORTED);
        bail!("CONNECT만 지원합니다 (command {})", request[1]);
    }

    let target = match request[3] {
        ATYP_IPV4 => {
            let mut raw = [0u8; 6];
            stream.read_exact(&mut raw)?;
            let ip = Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3]);
            Target::Addr(SocketAddr::new(IpAddr::V4(ip), u16::from_be_bytes([raw[4], raw[5]])))
        }
        ATYP_IPV6 => {
            let mut raw = [0u8; 18];
            stream.read_exact(&mut raw)?;
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&raw[..16]);
            let ip = Ipv6Addr::from(octets);
            Target::Addr(SocketAddr::new(IpAddr::V6(ip), u16::from_be_bytes([raw[16], raw[17]])))
        }
        ATYP_DOMAIN => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len)?;
            let mut host = vec![0u8; len[0] as usize];
            stream.read_exact(&mut host)?;
            let mut port = [0u8; 2];
            stream.read_exact(&mut port)?;
            let host = String::from_utf8(host).context("호스트명이 UTF-8이 아닙니다")?;
            Target::Domain(host, u16::from_be_bytes(port))
        }
        other => {
            reply(stream, REPLY_GENERAL_FAILURE);
            bail!("알 수 없는 주소 형식 {other}");
        }
    };

    stream.set_read_timeout(None).ok();
    Ok(target)
}

/// Send a reply. The bound address is not meaningful for CONNECT, and every
/// client in practice ignores it.
fn reply(stream: &mut TcpStream, code: u8) {
    let _ = stream.write_all(&[VERSION, code, 0, ATYP_IPV4, 0, 0, 0, 0, 0, 0]);
}

/// Handle one accepted client from start to finish.
pub fn serve_one(mut client: TcpStream, engine: &Engine) -> Result<Outcome> {
    let target = match accept_request(&mut client) {
        Ok(t) => t,
        Err(e) => {
            engine.stats.failed.fetch_add(1, Ordering::Relaxed);
            return Err(e);
        }
    };

    let addrs = match target.resolve() {
        Ok(a) => a,
        Err(e) => {
            reply(&mut client, REPLY_HOST_UNREACHABLE);
            engine.stats.failed.fetch_add(1, Ordering::Relaxed);
            return Err(e);
        }
    };

    let mut upstream = None;
    let mut last: Option<std::io::Error> = None;
    for addr in &addrs {
        match relay::connect(*addr, CONNECT_TIMEOUT) {
            Ok(s) => {
                upstream = Some(s);
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    let Some(upstream) = upstream else {
        reply(&mut client, REPLY_HOST_UNREACHABLE);
        engine.stats.failed.fetch_add(1, Ordering::Relaxed);
        let e = last.unwrap_or_else(|| std::io::Error::other("no address"));
        return Err(e).with_context(|| format!("{} 연결 실패", target.describe()));
    };

    // The client only starts sending once it sees success, so this has to come
    // before the relay reads the opening payload.
    reply(&mut client, REPLY_OK);

    relay::relay(client, upstream, &engine.policy, &engine.stats).map_err(Into::into)
}

/// A listener running on its own thread.
pub struct Server {
    pub port: u16,
    running: Arc<AtomicBool>,
    listener: TcpListener,
}

impl Server {
    /// Bind to loopback. Never bind this to a public interface — it performs no
    /// authentication.
    pub fn bind(port: u16) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .with_context(|| format!("127.0.0.1:{port} 바인드 실패"))?;
        let port = listener.local_addr()?.port();
        Ok(Self { port, running: Arc::new(AtomicBool::new(true)), listener })
    }

    pub fn stop_handle(&self) -> Arc<AtomicBool> {
        self.running.clone()
    }

    /// Serve until stopped. `on_outcome` sees every finished connection.
    pub fn run(self, engine: Arc<Engine>, on_outcome: impl Fn(Result<Outcome>) + Send + Sync + 'static) {
        let on_outcome = Arc::new(on_outcome);
        for incoming in self.listener.incoming() {
            if !self.running.load(Ordering::Relaxed) {
                break;
            }
            let Ok(client) = incoming else { continue };
            let engine = engine.clone();
            let on_outcome = on_outcome.clone();
            // One thread per connection: a phone handles tens at a time, not
            // thousands, and this keeps the relay free of an async runtime.
            std::thread::spawn(move || on_outcome(serve_one(client, &engine)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Policy;
    use shard::config::Config;
    use std::sync::mpsc;

    /// Minimal SOCKS5 client: greet, CONNECT, return the stream.
    fn socks_connect(port: u16, target: &Target) -> std::io::Result<TcpStream> {
        let mut s = TcpStream::connect(("127.0.0.1", port))?;
        s.set_nodelay(true)?;
        s.write_all(&[VERSION, 1, 0])?;
        let mut ack = [0u8; 2];
        s.read_exact(&mut ack)?;
        assert_eq!(ack, [VERSION, 0]);

        let mut request = vec![VERSION, CMD_CONNECT, 0];
        match target {
            Target::Addr(SocketAddr::V4(a)) => {
                request.push(ATYP_IPV4);
                request.extend_from_slice(&a.ip().octets());
                request.extend_from_slice(&a.port().to_be_bytes());
            }
            Target::Domain(host, port) => {
                request.push(ATYP_DOMAIN);
                request.push(host.len() as u8);
                request.extend_from_slice(host.as_bytes());
                request.extend_from_slice(&port.to_be_bytes());
            }
            other => panic!("unsupported in test: {other:?}"),
        }
        s.write_all(&request)?;
        let mut reply = [0u8; 10];
        s.read_exact(&mut reply)?;
        assert_eq!(reply[1], REPLY_OK, "SOCKS reply code");
        Ok(s)
    }

    fn recording_server() -> (SocketAddr, mpsc::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                socket.set_read_timeout(Some(Duration::from_millis(600))).ok();
                let _ = socket.write_all(b"OK");
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
        let engine_for_thread = engine.clone();
        std::thread::spawn(move || {
            server.run(engine_for_thread, move |result| {
                let _ = tx.send(match result {
                    Ok(o) => format!("{}|{:?}", o.host.unwrap_or_default(), o.plan.mode),
                    Err(e) => format!("error|{e}"),
                });
            });
        });
        (port, engine, rx)
    }

    #[test]
    fn a_connection_through_socks_is_desynced_and_arrives_intact() {
        let (upstream, recorded) = recording_server();
        let (port, engine, outcomes) = start_server(Policy::new(Config::default()));

        let payload = shard::desync::build_client_hello("blocked.example", 300);
        let mut client = socks_connect(port, &Target::Addr(upstream)).unwrap();
        client.write_all(&payload).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let mut back = Vec::new();
        client.set_read_timeout(Some(Duration::from_millis(800))).ok();
        let _ = client.read_to_end(&mut back);

        let arrived = recorded.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(arrived, payload, "the server must receive the exact bytes");
        assert_eq!(back, b"OK");

        let outcome = outcomes.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(outcome, "blocked.example|SplitOob");
        assert_eq!(engine.stats.snapshot().desynced, 1);
    }

    #[test]
    fn an_excluded_host_passes_through() {
        let (upstream, recorded) = recording_server();
        let config = Config { exclude: vec!["bank.example".into()], ..Default::default() };
        let (port, engine, _outcomes) = start_server(Policy::new(config));

        let payload = shard::desync::build_client_hello("bank.example", 300);
        let mut client = socks_connect(port, &Target::Addr(upstream)).unwrap();
        client.write_all(&payload).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();

        assert_eq!(recorded.recv_timeout(Duration::from_secs(3)).unwrap(), payload);
        assert_eq!(engine.stats.snapshot().passed_through, 1);
    }

    #[test]
    fn a_domain_target_is_resolved() {
        let (upstream, _recorded) = recording_server();
        let (port, _engine, _outcomes) = start_server(Policy::new(Config::default()));
        // localhost resolves to ::1 before 127.0.0.1 on Windows, and the test
        // server only listens on IPv4 — so this also covers falling through to
        // the next address, which real dual-stack hosts need.
        let target = Target::Domain("localhost".into(), upstream.port());
        assert!(socks_connect(port, &target).is_ok());
    }

    #[test]
    fn resolution_returns_every_address_family() {
        let addrs = Target::Domain("localhost".into(), 443).resolve().unwrap();
        assert!(!addrs.is_empty());
        assert!(addrs.iter().all(|a| a.port() == 443));

        // A literal address needs no lookup.
        let literal: SocketAddr = "203.0.113.7:443".parse().unwrap();
        assert_eq!(Target::Addr(literal).resolve().unwrap(), vec![literal]);
    }

    #[test]
    fn an_unreachable_target_is_reported_not_hung() {
        let (port, _engine, outcomes) = start_server(Policy::new(Config::default()));
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(&[VERSION, 1, 0]).unwrap();
        let mut ack = [0u8; 2];
        s.read_exact(&mut ack).unwrap();

        // Port 1 on loopback: nothing is listening.
        let mut request = vec![VERSION, CMD_CONNECT, 0, ATYP_IPV4, 127, 0, 0, 1];
        request.extend_from_slice(&1u16.to_be_bytes());
        s.write_all(&request).unwrap();

        let mut reply = [0u8; 10];
        s.read_exact(&mut reply).unwrap();
        assert_ne!(reply[1], REPLY_OK, "failure must be reported in the reply");
        assert!(outcomes.recv_timeout(Duration::from_secs(3)).unwrap().starts_with("error|"));
    }

    #[test]
    fn a_non_socks5_client_is_rejected() {
        let (port, _engine, outcomes) = start_server(Policy::new(Config::default()));
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
        drop(s);
        assert!(outcomes.recv_timeout(Duration::from_secs(3)).unwrap().starts_with("error|"));
    }

    #[test]
    fn targets_describe_themselves_for_logging() {
        assert_eq!(Target::Domain("a.example".into(), 443).describe(), "a.example:443");
        assert_eq!(
            Target::Addr("1.2.3.4:80".parse().unwrap()).describe(),
            "1.2.3.4:80"
        );
    }
}
