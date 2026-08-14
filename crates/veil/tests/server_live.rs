//! End-to-end check against a real server.
//!
//! Parses a share link with the same code the app uses, generates a sing-box
//! client config with the same generator, runs the bundled core, and sends a
//! request through it. That exercises the whole chain — link parsing, config
//! generation, and the server itself — rather than any one piece in isolation.
//!
//!     $env:VEIL_TEST_LINK="vless://..."
//!     cargo test -p veil --test server_live -- --ignored --nocapture

use std::process::Command;
use std::time::{Duration, Instant};

use veil::config::{Config, Mode};
use veil::core;
use veil::link;

fn curl_through(port: u16, url: &str) -> Result<String, String> {
    let out = Command::new("curl")
        .args(["-s", "--max-time", "25", "--proxy", &format!("socks5h://127.0.0.1:{port}"), url])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    let body = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if body.is_empty() {
        return Err(format!("빈 응답 (curl exit {:?})", out.status.code()));
    }
    Ok(body)
}

fn direct(url: &str) -> Option<String> {
    let out = Command::new("curl").args(["-s", "--max-time", "15", url]).output().ok()?;
    let body = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!body.is_empty()).then_some(body)
}

#[test]
#[ignore = "requires a live server; set VEIL_TEST_LINK"]
fn a_share_link_produces_a_working_tunnel() {
    let link = std::env::var("VEIL_TEST_LINK").expect("VEIL_TEST_LINK 환경변수가 필요합니다");

    // 1. The app's own parser.
    let profile = link::parse_link(&link).expect("링크를 해석할 수 있어야 합니다");
    println!("프로필: {} · {}", profile.name, profile.outbound.protocol_label());
    println!("서버:   {}", profile.outbound.endpoint());

    // 2. The app's own config generator, in local-proxy mode so no TUN adapter
    //    or elevation is involved.
    let mut cfg = Config { mode: Mode::Proxy, mixed_port: 21080, ..Default::default() };
    cfg.routing.block_quic = false;

    let before = direct("https://api.ipify.org");
    println!("직결 IP: {}", before.clone().unwrap_or_else(|| "확인 실패".into()));

    // 3. The bundled core, validated and launched exactly as the app does.
    let log = core::new_log_buffer();
    let mut child = core::Core::start(&cfg, &profile, log.clone()).expect("코어가 시작되어야 합니다");

    // Give it a moment to bind and dial.
    let started = Instant::now();
    let mut body = Err("시작하지 못했습니다".to_string());
    while started.elapsed() < Duration::from_secs(25) {
        if let Err(e) = child.health() {
            let tail: Vec<String> = log.lock().iter().rev().take(8).cloned().collect();
            panic!("코어가 죽었습니다: {e}\n{}", tail.join("\n"));
        }
        body = curl_through(cfg.mixed_port, "https://api.ipify.org");
        if body.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(800));
    }

    let exit = match body {
        Ok(b) => b,
        Err(e) => {
            let tail: Vec<String> = log.lock().iter().rev().take(10).cloned().collect();
            child.stop();
            panic!("터널을 통한 요청 실패: {e}\n{}", tail.join("\n"));
        }
    };
    println!("터널 IP: {exit}");

    // 4. Throughput through the tunnel, so the number is real rather than a guess.
    let sample = Instant::now();
    let speed = Command::new("curl")
        .args([
            "-s", "-o", if cfg!(windows) { "NUL" } else { "/dev/null" },
            "--max-time", "60",
            "--proxy", &format!("socks5h://127.0.0.1:{}", cfg.mixed_port),
            "https://speed.cloudflare.com/__down?bytes=15000000",
        ])
        .status();
    if speed.map(|s| s.success()).unwrap_or(false) {
        let secs = sample.elapsed().as_secs_f64();
        println!("처리량:  15MB / {secs:.1}초 = {:.0} Mbps", 15.0 * 8.0 / secs);
    }

    child.stop();

    assert!(!exit.is_empty());
    if let Some(before) = before {
        assert_ne!(exit, before, "트래픽이 여전히 내 주소로 나가고 있습니다");
    }
    let server_ip = profile.outbound.endpoint();
    assert!(
        server_ip.starts_with(&exit) || server_ip.contains(&exit),
        "exit IP({exit})가 서버 주소({server_ip})와 다릅니다"
    );
}
