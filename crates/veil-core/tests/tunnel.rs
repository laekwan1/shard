//! The client and the server, run against each other.
//!
//! Unit tests can prove the header encodes and decodes; only this can prove a
//! byte put in one end comes out the other. Everything runs in one process on
//! loopback, so it needs no network and no fixture beyond a certificate
//! generated on the spot.

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use veil_core::client::{Client, Server as ServerSpec};
use veil_core::inbound::{DirectRules, Inbound};
use veil_core::server::{Config, Fallback, Outcome, Server};
use veil_core::tls::{self, Trust};

const PASSWORD: &str = "correct-horse-battery-staple";

/// A certificate and the pin that identifies it.
fn certificate() -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>, Trust) {
    let generated = rcgen::generate_simple_self_signed(vec!["veil.invalid".into()]).unwrap();
    let der = CertificateDer::from(generated.cert.der().to_vec());
    let pin = Trust::pinned(&tls::fingerprint(&der)).unwrap();
    let key = PrivateKeyDer::try_from(generated.signing_key.serialize_der()).unwrap();
    (vec![der], key, pin)
}

/// A TCP server that replies with `reply` and reports what it received.
async fn destination(reply: &'static [u8]) -> (SocketAddr, tokio::sync::oneshot::Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        // Read one batch, answer, then drain until the peer is done.
        let n = socket.read(&mut buf).await.unwrap_or(0);
        got.extend_from_slice(&buf[..n]);
        let _ = socket.write_all(reply).await;
        let _ = socket.shutdown().await;
        let _ = tx.send(got);
    });
    (address, rx)
}

/// Our server, listening, with `fallback` for anyone who fails to authenticate.
async fn tunnel_server(fallback: String) -> (SocketAddr, Trust, tokio::sync::mpsc::UnboundedReceiver<Outcome>) {
    let (certificates, key, pin) = certificate();
    let tls_config = tls::server_config(certificates, key).unwrap();
    let server = Server::bind(
        Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            password: PASSWORD.to_string(),
            fallback: Fallback::parse(&fallback),
        },
        tls_config,
    )
    .await
    .unwrap();

    let address = server.local_addr().unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let _ = server
            .run(move |result| {
                if let Ok(outcome) = result {
                    let _ = tx.send(outcome);
                }
            })
            .await;
    });
    (address, pin, rx)
}

fn client_for(address: SocketAddr, pin: Trust, password: &str) -> Client {
    Client::new(
        ServerSpec::new(address.ip().to_string(), address.port(), password)
            .with_trust(pin)
            // With a pinned certificate the name is cosmetic, which is the
            // point: it can be something unremarkable.
            .with_sni("www.microsoft.com"),
    )
    .unwrap()
}

#[tokio::test]
async fn a_request_reaches_the_destination_and_the_reply_comes_back() {
    let (target, arrived) = destination(b"HELLO-BACK").await;
    let (server, pin, mut outcomes) = tunnel_server("127.0.0.1:9".into()).await;
    let client = client_for(server, pin, PASSWORD);

    let mut stream = client.connect(&target.ip().to_string(), target.port()).await.unwrap();
    stream.write_all(b"PING").await.unwrap();
    stream.flush().await.unwrap();

    let mut back = Vec::new();
    stream.read_to_end(&mut back).await.unwrap();

    assert_eq!(back, b"HELLO-BACK", "the destination's reply must survive the tunnel");
    assert_eq!(arrived.await.unwrap(), b"PING", "the payload must arrive unchanged");

    match outcomes.recv().await.unwrap() {
        Outcome::Relayed { host, port, .. } => {
            assert_eq!((host.as_str(), port), (target.ip().to_string().as_str(), target.port()));
        }
        other => panic!("expected a relay, got {other:?}"),
    }
}

#[tokio::test]
async fn the_opening_payload_can_ride_with_the_header() {
    // One write means header and first payload share a TLS record, which is
    // what a real client's opening flight looks like.
    let (target, arrived) = destination(b"OK").await;
    let (server, pin, _outcomes) = tunnel_server("127.0.0.1:9".into()).await;
    let client = client_for(server, pin, PASSWORD);

    let mut stream = client
        .connect_with(&target.ip().to_string(), target.port(), b"GET / HTTP/1.1\r\n\r\n")
        .await
        .unwrap();

    let mut back = Vec::new();
    stream.read_to_end(&mut back).await.unwrap();
    assert_eq!(back, b"OK");
    assert_eq!(arrived.await.unwrap(), b"GET / HTTP/1.1\r\n\r\n");
}

#[tokio::test]
async fn a_wrong_password_is_served_the_fallback_site() {
    // The point of the fallback: someone probing the port finds a web server,
    // not a refusal that says "something else lives here".
    let (site, seen) = destination(b"HTTP/1.1 200 OK\r\n\r\nnothing to see").await;
    let (server, pin, mut outcomes) = tunnel_server(site.to_string()).await;
    let client = client_for(server, pin, "wrong-password");

    let mut stream = client.connect("example.com", 443).await.unwrap();
    let mut back = Vec::new();
    stream.read_to_end(&mut back).await.unwrap();

    assert_eq!(back, b"HTTP/1.1 200 OK\r\n\r\nnothing to see");
    assert!(matches!(outcomes.recv().await.unwrap(), Outcome::FellBack { .. }));

    // The fallback must receive what the client actually sent, byte for byte —
    // a prober replaying a real request has to get a coherent answer.
    let replayed = seen.await.unwrap();
    assert!(!replayed.is_empty(), "the fallback saw nothing");
}

#[tokio::test]
async fn a_probe_is_answered_as_promptly_as_a_web_server_would() {
    // A fixed delay before the fallback answers would identify this port just
    // as surely as a refusal does.
    let (site, _) = destination(b"HTTP/1.1 200 OK\r\n\r\nweb").await;
    let (server, pin, _outcomes) = tunnel_server(site.to_string()).await;

    let config = tls::client_config(&pin).unwrap();
    let connector = tokio_rustls::TlsConnector::from(config);
    let tcp = TcpStream::connect(server).await.unwrap();
    let name = rustls::pki_types::ServerName::try_from("www.microsoft.com").unwrap();
    let mut stream = connector.connect(name, tcp).await.unwrap();

    let started = std::time::Instant::now();
    stream.write_all(b"GET / HTTP/1.1\r\nHost: probe\r\n\r\n").await.unwrap();
    let mut back = Vec::new();
    stream.read_to_end(&mut back).await.unwrap();

    assert_eq!(back, b"HTTP/1.1 200 OK\r\n\r\nweb");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "the fallback took {:?}, which is a tell",
        started.elapsed()
    );
}

#[tokio::test]
async fn garbage_is_served_the_fallback_site_too() {
    let (site, _) = destination(b"HTTP/1.1 200 OK\r\n\r\nweb").await;
    let (server, pin, mut outcomes) = tunnel_server(site.to_string()).await;

    // Speak plain HTTP at it, as a scanner would.
    let config = tls::client_config(&pin).unwrap();
    let connector = tokio_rustls::TlsConnector::from(config);
    let tcp = TcpStream::connect(server).await.unwrap();
    let name = rustls::pki_types::ServerName::try_from("www.microsoft.com").unwrap();
    let mut stream = connector.connect(name, tcp).await.unwrap();

    stream.write_all(b"GET / HTTP/1.1\r\nHost: probe\r\n\r\n").await.unwrap();
    let mut back = Vec::new();
    stream.read_to_end(&mut back).await.unwrap();

    assert_eq!(back, b"HTTP/1.1 200 OK\r\n\r\nweb");
    assert!(matches!(outcomes.recv().await.unwrap(), Outcome::FellBack { .. }));
}

#[tokio::test]
async fn the_builtin_fallback_answers_without_a_web_server() {
    // A box whose only job is the tunnel has nothing else listening; the
    // disguise has to work with nothing installed.
    let (server, pin, _outcomes) = tunnel_server("builtin".into()).await;
    let client = client_for(server, pin, "wrong-password");

    let mut stream = client.connect("example.com", 443).await.unwrap();
    let mut back = Vec::new();
    stream.read_to_end(&mut back).await.unwrap();

    let text = String::from_utf8(back).unwrap();
    assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
    assert!(text.contains("<html>"), "{text}");
    // The declared length must match the body, or a browser hangs waiting.
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let declared: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length: "))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(declared, body.len(), "Content-Length does not match the body");
}

#[tokio::test]
async fn a_pin_for_a_different_certificate_is_refused() {
    let (server, _real_pin, _outcomes) = tunnel_server("127.0.0.1:9".into()).await;
    // A pin taken from some other certificate must not open the connection.
    let (_, _, other_pin) = certificate();
    let client = client_for(server, other_pin, PASSWORD);

    assert!(
        client.connect("example.com", 443).await.is_err(),
        "the handshake must fail when the certificate is not the pinned one"
    );
}

// ---- the local listener ---------------------------------------------------

async fn inbound_for(server: SocketAddr, pin: Trust, rules: DirectRules) -> u16 {
    let client = client_for(server, pin, PASSWORD);
    let inbound = Inbound::bind(0, client, rules).await.unwrap();
    let port = inbound.port().unwrap();
    tokio::spawn(inbound.run());
    port
}

#[tokio::test]
async fn an_http_proxy_request_goes_through_the_tunnel() {
    let (target, arrived) = destination(b"HTTP/1.1 200 OK\r\n\r\nhi").await;
    let (server, pin, _outcomes) = tunnel_server("127.0.0.1:9".into()).await;
    let port = inbound_for(server, pin, DirectRules::default()).await;

    let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let request = format!(
        "GET http://{}:{}/video HTTP/1.1\r\nHost: blocked.example\r\nProxy-Connection: keep-alive\r\n\r\n",
        target.ip(),
        target.port()
    );
    socket.write_all(request.as_bytes()).await.unwrap();

    let mut back = Vec::new();
    socket.read_to_end(&mut back).await.unwrap();
    assert_eq!(back, b"HTTP/1.1 200 OK\r\n\r\nhi");

    let seen = String::from_utf8(arrived.await.unwrap()).unwrap();
    assert!(seen.starts_with("GET /video HTTP/1.1\r\n"), "not rewritten to origin form: {seen}");
    assert!(seen.contains("Host: blocked.example"));
    assert!(
        !seen.to_ascii_lowercase().contains("proxy-connection"),
        "a hop-by-hop header reached the server"
    );
}

#[tokio::test]
async fn a_connect_tunnel_goes_through_the_tunnel() {
    let (target, arrived) = destination(b"SERVER-HELLO").await;
    let (server, pin, _outcomes) = tunnel_server("127.0.0.1:9".into()).await;
    let port = inbound_for(server, pin, DirectRules::default()).await;

    let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    socket
        .write_all(format!("CONNECT {}:{} HTTP/1.1\r\n\r\n", target.ip(), target.port()).as_bytes())
        .await
        .unwrap();

    let mut ack = [0u8; 39];
    socket.read_exact(&mut ack).await.unwrap();
    assert!(String::from_utf8_lossy(&ack).contains("200"), "{:?}", String::from_utf8_lossy(&ack));

    socket.write_all(b"CLIENT-HELLO").await.unwrap();
    let mut back = Vec::new();
    socket.read_to_end(&mut back).await.unwrap();

    assert_eq!(back, b"SERVER-HELLO");
    assert_eq!(arrived.await.unwrap(), b"CLIENT-HELLO");
}

#[tokio::test]
async fn socks5_goes_through_the_tunnel() {
    let (target, arrived) = destination(b"SERVER-HELLO").await;
    let (server, pin, _outcomes) = tunnel_server("127.0.0.1:9".into()).await;
    let port = inbound_for(server, pin, DirectRules::default()).await;

    let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    // Greeting: SOCKS5, one method, no authentication.
    socket.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
    let mut choice = [0u8; 2];
    socket.read_exact(&mut choice).await.unwrap();
    assert_eq!(choice, [0x05, 0x00]);

    let mut request = vec![0x05, 0x01, 0x00, 0x01];
    request.extend_from_slice(&match target.ip() {
        std::net::IpAddr::V4(v4) => v4.octets(),
        other => panic!("expected IPv4, got {other}"),
    });
    request.extend_from_slice(&target.port().to_be_bytes());
    socket.write_all(&request).await.unwrap();

    let mut reply = [0u8; 10];
    socket.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply[..2], [0x05, 0x00], "SOCKS must report success");

    socket.write_all(b"CLIENT-HELLO").await.unwrap();
    let mut back = Vec::new();
    socket.read_to_end(&mut back).await.unwrap();

    assert_eq!(back, b"SERVER-HELLO");
    assert_eq!(arrived.await.unwrap(), b"CLIENT-HELLO");
}

#[tokio::test]
async fn a_direct_domain_never_touches_the_tunnel() {
    // A bank behind a foreign exit address refuses the session, so these must
    // leave the phone directly. If the rule failed open, the failure would be
    // silent and look like the bank being down.
    let (target, arrived) = destination(b"BANK-OK").await;
    let (server, pin, mut outcomes) = tunnel_server("127.0.0.1:9".into()).await;
    let rules = DirectRules::new(["localhost".into(), "127.0.0.1".into()]);
    let port = inbound_for(server, pin, rules).await;

    let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    socket
        .write_all(format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", target.port()).as_bytes())
        .await
        .unwrap();
    let mut ack = [0u8; 39];
    socket.read_exact(&mut ack).await.unwrap();

    socket.write_all(b"HI").await.unwrap();
    let mut back = Vec::new();
    socket.read_to_end(&mut back).await.unwrap();
    assert_eq!(back, b"BANK-OK");
    assert_eq!(arrived.await.unwrap(), b"HI");

    // The tunnel server must not have seen this connection at all.
    let saw_anything = tokio::time::timeout(std::time::Duration::from_millis(400), outcomes.recv()).await;
    assert!(saw_anything.is_err(), "a direct domain reached the tunnel: {saw_anything:?}");
}
