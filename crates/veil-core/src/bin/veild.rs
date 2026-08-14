//! The client, runnable.
//!
//! ```text
//! veild <share-link> [local-port]
//! curl -x http://127.0.0.1:<port> https://example.com
//! ```
//!
//! The same code the phone runs, with a command line instead of a screen. Being
//! able to point curl at it is what makes the phone build testable without a
//! phone — the Android app differs only in who calls `Inbound::run`.

use anyhow::{Context, Result};
use veil_core::inbound::{DirectRules, Inbound};
use veil_core::{client::Client, link};

/// Korean banking and government domains, which must not leave through a
/// foreign address or they refuse the session outright.
const DIRECT: &[&str] = &[
    "kbstar.com", "shinhan.com", "wooribank.com", "hanabank.com", "nonghyup.com",
    "ibk.co.kr", "kebhana.com", "citibank.co.kr", "standardchartered.co.kr",
    "kakaobank.com", "kbanknow.com", "tossbank.com",
    "koreanbank.or.kr", "kftc.or.kr", "yessign.or.kr", "crosscert.com",
    "go.kr", "or.kr", "re.kr", "naver.com", "daum.net", "kakao.com",
];

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "veil_core=info,veild=info".into()),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let link = args.next().context("사용법: veild <공유링크> [로컬포트]")?;
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(0);

    let (server, name) = link::parse(&link)?;
    let client = Client::new(server.clone())?;

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let rules = DirectRules::new(DIRECT.iter().map(|s| s.to_string()));
        let inbound = Inbound::bind(port, client, rules).await?;
        let bound = inbound.port()?;
        let stats = std::sync::Arc::clone(&inbound.stats);

        println!("서버   : {} ({}:{})", name, server.host, server.port);
        println!("프록시 : http://127.0.0.1:{bound}  (SOCKS5도 같은 포트)");

        // A line a second is enough to see whether anything is moving.
        tokio::spawn(async move {
            let mut previous = stats.snapshot();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let now = stats.snapshot();
                if now.connections != previous.connections || now.bytes_down != previous.bytes_down {
                    println!(
                        "연결 {} · 터널 {} · 직결 {} · 실패 {} · ↑{} ↓{}",
                        now.connections, now.tunnelled, now.direct, now.failed,
                        now.bytes_up, now.bytes_down
                    );
                }
                previous = now;
            }
        });

        inbound.run().await;
        Ok::<(), anyhow::Error>(())
    })
}
