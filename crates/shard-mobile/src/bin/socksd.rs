//! Run the phone engine as a local SOCKS5 proxy.
//!
//! Same code the phone build will use, reachable from a desktop browser or
//! curl. That makes the socket-level desync measurable against a real blocked
//! site without a signed app, a TUN interface, or a Mac.
//!
//!     socksd.exe [port]
//!     curl --proxy socks5h://127.0.0.1:1080 https://example.com
//!
//! Turn the desktop Shard engine off first, or both will act on the same
//! connection and the result means nothing.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use shard_mobile::{socks, Engine, Policy};

fn main() -> anyhow::Result<()> {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1080);

    let policy = Policy::from_config();
    let cfg = policy.config();
    println!("범위: {}  ·  기본 전략: {}", cfg.scope.label(), cfg.strategy.desync.label());
    println!("도메인별 학습 결과 {}건", cfg.overrides.len());

    let engine = Arc::new(Engine::new(policy));
    let server = socks::Server::bind(port)?;
    println!("\nSOCKS5 프록시: 127.0.0.1:{}", server.port);
    println!("curl --proxy socks5h://127.0.0.1:{} https://example.com\n", server.port);

    let stats = engine.stats.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(10));
        let s = stats.snapshot();
        if s.connections > 0 {
            println!(
                "[누적] 연결 {} · desync {} · 통과 {} · 실패 {} · 상행 {}KB · 하행 {}KB",
                s.connections,
                s.desynced,
                s.passed_through,
                s.failed,
                s.bytes_up / 1024,
                s.bytes_down / 1024
            );
        }
    });

    let counter = engine.stats.clone();
    server.run(engine, move |result| match result {
        Ok(outcome) => {
            let host = outcome.host.unwrap_or_else(|| "(이름 없음)".into());
            println!("  {host}  ·  {} @{}", outcome.plan.mode.label(), outcome.plan.at);
        }
        Err(e) => {
            // Client-side aborts are routine; only note them once in a while.
            if counter.failed.load(Ordering::Relaxed) % 10 == 1 {
                eprintln!("  실패: {e}");
            }
        }
    });
    Ok(())
}
