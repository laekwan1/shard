//! The phone engine, runnable on a desktop.
//!
//! This is the exact code path the Android app uses: the same HTTP proxy front
//! end, the same policy, the same socket-level desync. Being able to point curl
//! at it is what makes the phone build testable without a phone.
//!
//! ```text
//! proxyd [port]
//! curl -x http://127.0.0.1:<port> https://blocked.example
//! ```

use shard_mobile::{http_proxy, Engine, Policy};
use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    let port: u16 = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(0);

    let engine = Arc::new(Engine::new(Policy::from_config()));
    let server = http_proxy::Server::bind(port)?;
    println!("http proxy on 127.0.0.1:{}", server.port);
    println!("engine: {:?}", engine.policy.config().strategy);

    server.run(engine, |result| match result {
        Ok(outcome) => println!(
            "{:<40} {:?}",
            outcome.host.unwrap_or_else(|| "-".into()),
            outcome.plan
        ),
        Err(e) => println!("failed: {e:#}"),
    });
    Ok(())
}
