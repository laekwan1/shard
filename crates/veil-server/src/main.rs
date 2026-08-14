//! The server, the half that runs on the box.
//!
//! ```text
//! veil-server setup <공인IP> [포트]   인증서·비밀번호·설정을 만들고 링크를 출력
//! veil-server run [설정파일]           서버를 실행
//! ```
//!
//! `setup` exists so that standing a server up is one command whose output is
//! the exact line to paste into the phone. Every step it does by hand — making
//! a key, hashing a certificate, composing a link — is a step that can be done
//! slightly wrong in a way that only shows up as a handshake failure later.

mod settings;
mod setup;

use anyhow::{Context, Result};
use settings::Settings;
use std::path::PathBuf;
use veil_core::server::{Config, Fallback, Outcome, Server};
use veil_core::tls;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("setup") => {
            let host = args.next().context("공인 IP 또는 도메인을 지정하세요")?;
            let port = args.next().and_then(|p| p.parse().ok()).unwrap_or(443);
            let dir = args.next().map(PathBuf::from).unwrap_or_else(default_dir);
            do_setup(&host, port, &dir)
        }
        Some("run") => {
            let path = args.next().map(PathBuf::from).unwrap_or_else(default_config);
            do_run(&path)
        }
        Some(other) => {
            eprintln!("알 수 없는 명령입니다: {other}\n");
            usage();
            std::process::exit(2);
        }
        None => {
            usage();
            std::process::exit(2);
        }
    }
}

fn usage() {
    eprintln!(
        "veil-server\n\n\
         \x20 setup <공인IP|도메인> [포트] [디렉터리]   인증서와 설정을 만들고 접속 링크를 출력합니다\n\
         \x20 run [설정파일]                             서버를 실행합니다 (기본: /etc/veil/config.toml)\n"
    );
}

fn default_dir() -> PathBuf {
    PathBuf::from("/etc/veil")
}

fn default_config() -> PathBuf {
    default_dir().join("config.toml")
}

fn do_setup(host: &str, port: u16, dir: &std::path::Path) -> Result<()> {
    let generated = setup::run(dir, host, port)?;

    println!("설정을 만들었습니다: {}", dir.display());
    println!("  인증서 지문: {}", generated.fingerprint);
    println!("  듣는 주소  : {}", generated.settings.listen);
    println!();
    println!("아래 한 줄을 폰의 Veil 에 붙여넣으세요:");
    println!();
    println!("{}", generated.link);
    println!();
    println!("실행: veil-server run {}", dir.join("config.toml").display());
    Ok(())
}

fn do_run(path: &PathBuf) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "veil_core=info,veil_server=info".into()),
        )
        .init();

    let settings = Settings::load(path)?;
    let certificate = std::fs::read_to_string(&settings.certificate)
        .with_context(|| format!("{} 를 읽을 수 없습니다", settings.certificate.display()))?;
    let key = std::fs::read_to_string(&settings.key)
        .with_context(|| format!("{} 를 읽을 수 없습니다", settings.key.display()))?;
    let (certificates, key) = tls::load_pem(&certificate, &key)?;

    let runtime = tokio::runtime::Runtime::new().context("런타임을 만들 수 없습니다")?;
    runtime.block_on(async move {
        let server = Server::bind(
            Config {
                listen: settings.address()?,
                password: settings.password.clone(),
                fallback: Fallback::parse(&settings.fallback),
            },
            tls::server_config(certificates, key)?,
        )
        .await?;

        tracing::info!("듣는 중: {}", server.local_addr()?);
        tokio::select! {
            result = server.run(report) => result,
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("종료합니다");
                Ok(())
            }
        }
    })
}

/// One line per connection. Deliberately without the client's address: the
/// server has no reason to keep a record of who connected, and a log that
/// exists is a log that can be taken.
fn report(outcome: Result<Outcome>) {
    match outcome {
        Ok(Outcome::Relayed { host, port, up, down }) => {
            tracing::info!("{host}:{port}  ↑{up} ↓{down}");
        }
        Ok(Outcome::FellBack { reason }) => tracing::info!("폴백: {reason}"),
        Err(e) => tracing::debug!("연결 실패: {e:#}"),
    }
}
