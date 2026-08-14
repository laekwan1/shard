//! Differential diagnosis of how a host is blocked.
//!
//! "It does not connect" has several very different causes and only one of them
//! is something Shard can fix. This separates them by changing one variable at
//! a time:
//!
//! - resolve twice, plainly and encrypted — a disagreement means DNS blocking
//! - open the connection but name a *different*, unblocked host in the
//!   handshake — if that succeeds where the real name failed, the hostname is
//!   what is being matched, which is exactly what desync defeats
//! - if neither name works but TCP connects, the block is deeper than the
//!   hostname; if TCP itself fails, the address is blocked
//!
//! Usage:
//!     $env:DIAGNOSE_HOST="example.com"
//!     cargo test -p shard --test diagnose -- --ignored --nocapture

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use shard::config::Doh;
use shard::desync::build_client_hello;

const TIMEOUT: Duration = Duration::from_secs(6);
/// A name that is not blocked anywhere, used as the control.
const CONTROL_HOST: &str = "www.iana.org";

#[derive(Debug)]
enum Attempt {
    Replied(usize),
    Refused(String),
    Silent,
}

impl Attempt {
    fn ok(&self) -> bool {
        matches!(self, Attempt::Replied(_))
    }

    fn describe(&self) -> String {
        match self {
            Attempt::Replied(n) => format!("응답 {n}바이트"),
            Attempt::Refused(e) => format!("거부/끊김 ({e})"),
            Attempt::Silent => "무응답 (타임아웃)".to_string(),
        }
    }
}

fn connect(addr: SocketAddr) -> Result<TcpStream, String> {
    let stream = TcpStream::connect_timeout(&addr, TIMEOUT).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(TIMEOUT)).ok();
    stream.set_nodelay(true).ok();
    Ok(stream)
}

fn exchange(addr: SocketAddr, payload: &[u8]) -> Attempt {
    let mut stream = match connect(addr) {
        Ok(s) => s,
        Err(e) => return Attempt::Refused(format!("connect: {e}")),
    };
    if let Err(e) = stream.write_all(payload) {
        return Attempt::Refused(format!("write: {e}"));
    }
    let mut buf = [0u8; 64];
    match stream.read(&mut buf) {
        Ok(0) => Attempt::Refused("서버가 응답 없이 닫음".into()),
        Ok(n) => Attempt::Replied(n),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut
            || e.kind() == std::io::ErrorKind::WouldBlock =>
        {
            Attempt::Silent
        }
        Err(e) => Attempt::Refused(e.to_string()),
    }
}

/// Complete a real HTTPS request against a specific address.
///
/// Getting *a* reply only proves something is listening. A DNS redirection to a
/// notice server also replies — and is only distinguishable by whether it can
/// present a valid certificate for the name we asked for.
fn https_identity(host: &str, ip: IpAddr) -> String {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return "런타임 생성 실패".into();
    };
    runtime.block_on(async {
        let client = match reqwest::Client::builder()
            .timeout(TIMEOUT)
            .resolve(host, SocketAddr::new(ip, 443))
            .build()
        {
            Ok(c) => c,
            Err(e) => return format!("클라이언트 생성 실패: {e}"),
        };
        match client.get(format!("https://{host}/")).send().await {
            Ok(response) => {
                let status = response.status();
                let bytes = response.bytes().await.map(|b| b.len()).unwrap_or(0);
                format!("HTTP {status}, 본문 {bytes}바이트 — 인증서 검증 통과 (진짜 서버)")
            }
            Err(e) => {
                let text = e.to_string();
                let kind = if text.contains("certificate") || text.contains("Certificate") {
                    "인증서 검증 실패 — 다른 서버가 응답하고 있습니다 (DNS 리다이렉션)"
                } else if text.contains("timed out") || text.contains("timeout") {
                    "타임아웃 — 핸드셰이크가 중간에 버려짐"
                } else if text.contains("reset") || text.contains("closed") {
                    "연결이 끊김 — 리셋 주입 가능성"
                } else {
                    "실패"
                };
                format!("{kind} ({text})")
            }
        }
    })
}

fn http_get(host: &str) -> Vec<u8> {
    format!("GET / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: curl/8\r\nConnection: close\r\n\r\n").into_bytes()
}

#[test]
#[ignore = "requires network access; set DIAGNOSE_HOST"]
fn diagnose_a_host() {
    let host = std::env::var("DIAGNOSE_HOST").unwrap_or_else(|_| "example.com".to_string());
    println!("\n=== {host} ===");

    if !std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq shard.exe", "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("shard.exe"))
        .unwrap_or(false)
    {
        println!("(shard.exe 미실행 — 순수한 네트워크 상태를 보고 있습니다)");
    } else {
        println!("!! shard.exe 실행 중 — 엔진이 켜져 있으면 결과가 우회된 상태일 수 있습니다");
    }

    // 1. Two resolvers.
    let system: Option<IpAddr> =
        (host.as_str(), 443u16).to_socket_addrs().ok().and_then(|mut a| a.next()).map(|a| a.ip());
    let encrypted = shard::doh::resolve_encrypted(&Doh::default(), &host);

    println!("\n[DNS]");
    println!("  시스템 리졸버 : {}", system.map(|a| a.to_string()).unwrap_or("실패".into()));
    println!("  암호화 DNS    : {}", encrypted.map(|a| a.to_string()).unwrap_or("실패".into()));
    let differ = matches!((system, encrypted), (Some(a), Some(b)) if a != b);
    if differ {
        // Not proof on its own: a CDN legitimately hands different addresses to
        // different resolvers. Whether each one *works* is the real evidence.
        println!("  => 두 응답이 다릅니다 (CDN의 정상적인 차이일 수도 있습니다)");
    }

    // 2. Test every address we were given, so a poisoned answer shows up as the
    //    one that fails while the other succeeds.
    let mut targets: Vec<(&str, IpAddr)> = Vec::new();
    if let Some(a) = system {
        targets.push(("시스템", a));
    }
    if let Some(a) = encrypted {
        if Some(a) != system {
            targets.push(("암호화", a));
        }
    }
    if targets.is_empty() {
        println!("\n결론: 어느 리졸버로도 주소를 얻지 못했습니다 (도메인이 없거나 DNS가 완전히 막힘)");
        return;
    }

    let mut verdicts = Vec::new();
    for (source, ip) in &targets {
        println!("\n[{source} 응답 {ip}]");
        for port in [443u16, 80] {
            let addr = SocketAddr::new(*ip, port);

            if let Err(e) = connect(addr) {
                println!("  {port:>3}: TCP 연결 실패 ({e}) — 주소/포트 단위 차단");
                verdicts.push((*source, port, "주소 차단"));
                continue;
            }

            // Same connection, real name versus a name nobody blocks.
            let (real, control) = if port == 443 {
                (
                    exchange(addr, &build_client_hello(&host, 0)),
                    exchange(addr, &build_client_hello(CONTROL_HOST, 0)),
                )
            } else {
                (exchange(addr, &http_get(&host)), exchange(addr, &http_get(CONTROL_HOST)))
            };
            println!("  {port:>3}: 실제 이름 {} / 대조 이름 {}", real.describe(), control.describe());

            let mut verdict = match (real.ok(), control.ok()) {
                (true, _) => "통과",
                (false, true) => "이름 기반 차단",
                (false, false) => "이름 무관 실패",
            };

            // A raw reply is not proof of identity; check the certificate.
            if port == 443 && real.ok() {
                let identity = https_identity(&host, *ip);
                println!("       실제 요청: {identity}");
                if !identity.contains("진짜 서버") {
                    verdict = if identity.contains("DNS 리다이렉션") {
                        "가짜 서버 (DNS 리다이렉션)"
                    } else {
                        "핸드셰이크 차단"
                    };
                }
            }
            println!("       => {verdict}");
            verdicts.push((*source, port, verdict));
        }
    }

    // Judge on 443 only. Plain HTTP frequently "works" because a notice server
    // answers it, which says nothing about whether the real site is reachable.
    println!("\n[정리]");
    let tls = |source: &str| -> Option<&str> {
        verdicts.iter().find(|(s, p, _)| *s == source && *p == 443).map(|(_, _, v)| *v)
    };
    let any_tls = |v: &str| verdicts.iter().any(|(_, p, verdict)| *p == 443 && *verdict == v);

    if any_tls("이름 기반 차단") {
        println!("  같은 주소인데 호스트명이 실제 이름일 때만 끊깁니다.");
        println!("  => SNI 차단입니다. Shard의 desync가 다루는 바로 그 유형입니다.");
    } else if any_tls("가짜 서버 (DNS 리다이렉션)") {
        println!("  응답은 오지만 인증서가 맞지 않습니다 — 다른 서버가 대신 답하고 있습니다.");
        println!("  => DNS 차단입니다. Shard의 DNS 탭에서 '시스템 DNS를 포워더로 변경'을 켜세요.");
    } else if tls("시스템") != tls("암호화") && tls("암호화") == Some("통과") {
        println!("  시스템 리졸버 주소로는 안 되고 암호화 DNS 주소로는 됩니다.");
        println!("  => DNS 차단입니다. 암호화 DNS를 켜세요.");
    } else if any_tls("통과") {
        println!("  443이 정상 동작합니다. 이 회선에서 차단 상태가 아닙니다.");
    } else {
        println!("  이름과 무관하게 443이 실패합니다 — 주소 단위 차단이거나 서버 문제입니다.");
        println!("  => Shard로는 해결되지 않습니다. Veil이 필요합니다.");
    }
    if verdicts.iter().any(|(_, p, v)| *p == 80 && *v == "통과") && !any_tls("통과") {
        println!("  (80포트만 응답하는 것은 대개 차단 안내 페이지입니다.)");
    }
}
