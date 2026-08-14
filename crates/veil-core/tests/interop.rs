//! Our implementation against an independent one.
//!
//! The tunnel tests prove the two halves agree with each other, which is a
//! weaker claim than it sounds: two halves written from the same misreading
//! agree perfectly. These run our client against sing-box's server and
//! sing-box's client against our server, so the wire format is checked against
//! something that was not written here.
//!
//! Skipped when sing-box is not vendored, so a checkout without it still tests.

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use veil_core::client::{Client, Server as ServerSpec};
use veil_core::server::{Config, Fallback, Server};
use veil_core::tls::{self, Trust};

const PASSWORD: &str = "interop-password-9f2c";
const SNI: &str = "veil.invalid";

fn singbox() -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/singbox/sing-box.exe");
    path.exists().then_some(path)
}

/// A certificate on disk, plus the pin that identifies it.
struct Certificate {
    dir: PathBuf,
    pin: Trust,
}

fn write_certificate(name: &str) -> Certificate {
    let generated = rcgen::generate_simple_self_signed(vec![SNI.into()]).unwrap();
    let der = rustls::pki_types::CertificateDer::from(generated.cert.der().to_vec());
    let pin = Trust::pinned(&tls::fingerprint(&der)).unwrap();

    let dir = std::env::temp_dir().join(format!("veil-interop-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cert.pem"), generated.cert.pem()).unwrap();
    std::fs::write(dir.join("key.pem"), generated.signing_key.serialize_pem()).unwrap();
    Certificate { dir, pin }
}

/// A free port, released immediately. sing-box needs a number in its config
/// before it starts, so there is no way to let the OS choose.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Start sing-box on `config`, waiting until `port` accepts connections.
fn start_singbox(name: &str, config: serde_json::Value, port: u16) -> Option<Child> {
    let binary = singbox()?;
    let path = std::env::temp_dir().join(format!("veil-interop-{name}.json"));
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(serde_json::to_string_pretty(&config).unwrap().as_bytes()).unwrap();
    drop(file);

    let child = Command::new(&binary)
        .arg("run")
        .arg("-c")
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("sing-box should start");

    for _ in 0..80 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Some(child);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("sing-box did not open port {port}");
}

/// Kills the child even if the test panics.
struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A TCP server that echoes a fixed reply and reports what it received.
async fn destination(reply: &'static [u8]) -> (SocketAddr, tokio::sync::oneshot::Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let n = socket.read(&mut buf).await.unwrap_or(0);
        let _ = socket.write_all(reply).await;
        let _ = socket.shutdown().await;
        let _ = tx.send(buf[..n].to_vec());
    });
    (address, rx)
}

// ---------------------------------------------------------------------------

/// Our client, sing-box's server.
#[tokio::test]
async fn our_client_is_understood_by_sing_box() {
    let Some(_) = singbox() else {
        eprintln!("sing-box not vendored; skipping");
        return;
    };

    let certificate = write_certificate("server");
    let tunnel_port = free_port();
    let config = serde_json::json!({
        "log": { "level": "error" },
        "inbounds": [{
            "type": "trojan",
            "listen": "127.0.0.1",
            "listen_port": tunnel_port,
            "users": [{ "name": "veil", "password": PASSWORD }],
            "tls": {
                "enabled": true,
                "server_name": SNI,
                "certificate_path": certificate.dir.join("cert.pem"),
                "key_path": certificate.dir.join("key.pem"),
            }
        }],
        "outbounds": [{ "type": "direct" }],
    });
    let _running = Running(start_singbox("server", config, tunnel_port).unwrap());

    let (target, arrived) = destination(b"HELLO-FROM-DESTINATION").await;
    let client = Client::new(
        ServerSpec::new("127.0.0.1", tunnel_port, PASSWORD)
            .with_trust(certificate.pin)
            .with_sni(SNI),
    )
    .unwrap();

    let mut stream = client
        .connect_with(&target.ip().to_string(), target.port(), b"PING-THROUGH-SINGBOX")
        .await
        .expect("sing-box rejected our request");

    let mut back = Vec::new();
    stream.read_to_end(&mut back).await.unwrap();

    assert_eq!(back, b"HELLO-FROM-DESTINATION");
    assert_eq!(
        arrived.await.unwrap(),
        b"PING-THROUGH-SINGBOX",
        "sing-box forwarded something other than what we sent"
    );
}

/// sing-box's client, our server.
#[tokio::test]
async fn sing_box_is_understood_by_our_server() {
    let Some(_) = singbox() else {
        eprintln!("sing-box not vendored; skipping");
        return;
    };

    let certificate = write_certificate("client");
    let (certificates, key) = tls::load_pem(
        &std::fs::read_to_string(certificate.dir.join("cert.pem")).unwrap(),
        &std::fs::read_to_string(certificate.dir.join("key.pem")).unwrap(),
    )
    .unwrap();

    let our_port = free_port();
    let server = Server::bind(
        Config {
            listen: format!("127.0.0.1:{our_port}").parse().unwrap(),
            password: PASSWORD.to_string(),
            fallback: Fallback::Builtin,
        },
        tls::server_config(certificates, key).unwrap(),
    )
    .await
    .unwrap();
    tokio::spawn(async move {
        let _ = server.run(|_| {}).await;
    });

    // sing-box takes the client role: a local proxy that dials our server.
    let proxy_port = free_port();
    let config = serde_json::json!({
        "log": { "level": "error" },
        "inbounds": [{
            "type": "mixed",
            "listen": "127.0.0.1",
            "listen_port": proxy_port,
        }],
        "outbounds": [{
            "type": "trojan",
            "server": "127.0.0.1",
            "server_port": our_port,
            "password": PASSWORD,
            "tls": {
                "enabled": true,
                "server_name": SNI,
                "certificate_path": certificate.dir.join("cert.pem"),
            }
        }],
    });
    let _running = Running(start_singbox("client", config, proxy_port).unwrap());

    let (target, arrived) = destination(b"HELLO-FROM-DESTINATION").await;
    let mut socket = TcpStream::connect(("127.0.0.1", proxy_port)).await.unwrap();
    socket
        .write_all(format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", target.port()).as_bytes())
        .await
        .unwrap();

    let mut ack = [0u8; 39];
    socket.read_exact(&mut ack).await.unwrap();
    assert!(
        String::from_utf8_lossy(&ack).contains("200"),
        "sing-box could not open the tunnel: {}",
        String::from_utf8_lossy(&ack)
    );

    socket.write_all(b"PING-FROM-SINGBOX").await.unwrap();
    let mut back = Vec::new();
    socket.read_to_end(&mut back).await.unwrap();

    assert_eq!(back, b"HELLO-FROM-DESTINATION");
    assert_eq!(
        arrived.await.unwrap(),
        b"PING-FROM-SINGBOX",
        "our server forwarded something other than what sing-box sent"
    );
}
