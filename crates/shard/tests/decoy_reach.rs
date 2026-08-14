//! What a plain-HTTP server does when it receives the decoy.
//!
//! The decoy is always a TLS ClientHello. That is right for port 443, where a
//! stray one is discarded as a bad handshake. On port 80 the server parses the
//! same bytes as a request line and answers `400 Bad Request` — which the
//! browser then shows instead of the site.
//!
//!     cargo test -p shard --test decoy_reach -- --ignored --nocapture

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use shard::desync::build_client_hello;

fn send_and_read(host: &str, port: u16, payload: &[u8]) -> String {
    let Some(addr) = (host, port).to_socket_addrs().ok().and_then(|mut a| a.next()) else {
        return "주소 확인 실패".into();
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(5)) else {
        return "연결 실패".into();
    };
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    if stream.write_all(payload).is_err() {
        return "전송 실패".into();
    }
    let mut buf = vec![0u8; 200];
    match stream.read(&mut buf) {
        Ok(0) => "응답 없이 닫힘".into(),
        Ok(n) => String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or("").to_string(),
        Err(e) => format!("읽기 실패: {e}"),
    }
}

#[test]
#[ignore = "requires network access"]
fn a_tls_decoy_on_port_80_produces_bad_request() {
    let host = std::env::var("DIAGNOSE_HOST").unwrap_or_else(|_| "example.com".to_string());

    let decoy = build_client_hello("www.iana.org", 0);
    let first_line = send_and_read(&host, 80, &decoy);
    println!("포트 80 + TLS 디코이 -> {first_line}");

    let request = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    println!("포트 80 + 정상 요청   -> {}", send_and_read(&host, 80, request.as_bytes()));

    assert!(
        first_line.contains("400") || first_line.contains("Bad Request"),
        "expected the server to reject binary as a request line, got: {first_line}"
    );
}
