//! Live check of the bundled Tor stack.
//!
//! Exercises the real code path — locate the bundled binaries, generate a
//! torrc, launch tor with lyrebird and the built-in bridges, wait for the
//! circuit, then send a request through its SOCKS port and confirm the exit
//! address is not ours.
//!
//! Network-dependent and slow, so ignored by default:
//!     cargo test -p veil --test tor_live -- --ignored --nocapture

use std::process::Command;
use std::time::{Duration, Instant};

use veil::config::Tor as TorConfig;
use veil::profile::TorTransport;
use veil::tor::{self, Tor};

/// Bridged bootstrap is slow; obfs4 usually lands inside a minute.
const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(180);

fn direct_ip() -> Option<String> {
    let out = Command::new("curl")
        .args(["-s", "--max-time", "15", "https://api.ipify.org"])
        .output()
        .ok()?;
    let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!ip.is_empty()).then_some(ip)
}

fn through_socks(port: u16, url: &str) -> Result<String, String> {
    let out = Command::new("curl")
        .args([
            "-s",
            "--max-time",
            "60",
            "--socks5-hostname",
            &format!("127.0.0.1:{port}"),
            url,
        ])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    let body = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if body.is_empty() {
        return Err(format!("empty response (curl exit {:?})", out.status.code()));
    }
    Ok(body)
}

#[test]
#[ignore = "requires network access and takes up to three minutes"]
fn tor_bootstraps_and_carries_traffic() {
    let paths = tor::locate().expect("bundled tor");
    println!("tor:      {}", paths.tor.display());
    println!("lyrebird: {}", paths.lyrebird.display());

    let cfg = TorConfig {
        socks_port: 9250,
        control_port: 9251,
        transport: TorTransport::Obfs4,
        bridges: Vec::new(),
    };
    let bridges = tor::effective_bridges(&cfg, &paths);
    println!("bridges:  {} built-in obfs4 lines", bridges.len());
    assert!(!bridges.is_empty(), "the bundle must supply default obfs4 bridges");

    let before = direct_ip();
    println!("direct:   {}", before.clone().unwrap_or_else(|| "unknown".into()));

    let mut tor = Tor::start(&cfg).expect("tor should start");

    let started = Instant::now();
    let mut last = 0;
    while started.elapsed() < BOOTSTRAP_TIMEOUT {
        if let Err(e) = tor.health() {
            let log: Vec<String> = tor.log.lock().iter().rev().take(10).cloned().collect();
            panic!("tor died: {e}\n{}", log.join("\n"));
        }
        let progress = tor.progress();
        if progress != last {
            println!("bootstrap {progress}%");
            last = progress;
        }
        if progress >= 100 {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    if tor.progress() < 100 {
        let log: Vec<String> = tor.log.lock().iter().rev().take(15).cloned().collect();
        panic!("bootstrap stalled at {}%:\n{}", tor.progress(), log.join("\n"));
    }
    println!("bootstrapped in {:?}", started.elapsed());

    let exit = through_socks(cfg.socks_port, "https://api.ipify.org").expect("request through tor");
    println!("exit:     {exit}");
    assert!(!exit.is_empty());
    if let Some(before) = before {
        assert_ne!(exit, before, "traffic must not be leaving from our own address");
    }

    // check.torproject.org is authoritative about whether the exit is a relay.
    match through_socks(cfg.socks_port, "https://check.torproject.org/api/ip") {
        Ok(body) => {
            println!("check:    {body}");
            assert!(body.contains("\"IsTor\":true"), "exit was not recognised as a Tor relay: {body}");
        }
        Err(e) => println!("check:    skipped ({e})"),
    }
}
