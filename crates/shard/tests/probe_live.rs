//! Live checks for the probe's own reachability test.
//!
//! The prober decides a strategy works by sending a ClientHello and seeing
//! whether anything comes back. If that hello is malformed enough that healthy
//! servers drop it instead of answering, every rung fails and the probe reports
//! "no strategy found" regardless of whether the site was ever blocked.
//!
//! Network-dependent, so these are ignored by default:
//!     cargo test -p shard --test probe_live -- --ignored --nocapture

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use shard::desync::build_client_hello;

/// Sites that are reachable from essentially anywhere and terminate TLS
/// themselves, spanning different stacks (Google, Cloudflare, a CDN, Akamai).
const REACHABLE: &[&str] = &[
    "www.google.com",
    "www.cloudflare.com",
    "example.com",
    "www.microsoft.com",
];

fn handshake(host: &str) -> Result<Vec<u8>, String> {
    let addr = (host, 443u16)
        .to_socket_addrs()
        .map_err(|e| format!("resolve: {e}"))?
        .next()
        .ok_or("no address")?;

    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| format!("connect: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    stream.set_nodelay(true).ok();

    stream
        .write_all(&build_client_hello(host, 0))
        .map_err(|e| format!("write: {e}"))?;

    let mut buf = [0u8; 64];
    match stream.read(&mut buf) {
        Ok(0) => Err("server closed without replying".to_string()),
        Ok(n) => Ok(buf[..n].to_vec()),
        Err(e) => Err(format!("read: {e}")),
    }
}

fn describe(reply: &[u8]) -> String {
    match reply.first() {
        Some(0x16) => "handshake (ServerHello)".to_string(),
        Some(0x15) => format!("alert, level {:?} desc {:?}", reply.get(5), reply.get(6)),
        Some(other) => format!("record type 0x{other:02x}"),
        None => "empty".to_string(),
    }
}

#[test]
#[ignore = "requires network access"]
fn probe_hello_gets_a_reply_from_reachable_sites() {
    let mut failures = Vec::new();
    for host in REACHABLE {
        match handshake(host) {
            Ok(reply) => println!("  {host:<24} OK  {}", describe(&reply)),
            Err(e) => {
                println!("  {host:<24} FAIL  {e}");
                failures.push(format!("{host}: {e}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "the probe's ClientHello is rejected by healthy servers, so every strategy \
         would look like a failure:\n  {}",
        failures.join("\n  ")
    );
}
