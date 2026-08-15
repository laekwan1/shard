//! Headless strategy finder.
//!
//! Runs the desync engine directly, walks the whole strategy ladder against a
//! real HTTPS request, and prints what happened at each rung. This exists so a
//! blocked site can be characterised in one run instead of a person toggling
//! settings in the window and reporting back.
//!
//!     probe.exe www.example.com
//!
//! Requires administrator rights, and the Shard window must not be running —
//! two engines diverting the same packets would each see half of them.

// A console is the point here, unlike the tray app.
use shard::config::{Config, Scope};
use shard::engine::{Engine, Shared};
use shard::prober::{self, Progress};
use shard::strategy::Strategy;

/// Collects the report so it survives the run.
///
/// Windows will not let a caller both elevate a process and redirect its
/// output, so stdout alone is unreadable from whatever launched this. Every
/// line is mirrored to a file that the caller can pick up afterwards.
struct Report {
    lines: Vec<String>,
    path: std::path::PathBuf,
}

impl Report {
    fn new() -> Self {
        let path = uikit::config::app_dir(shard::config::APP_NAME).join("probe-report.txt");
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
        Self { lines: Vec::new(), path }
    }

    fn say(&mut self, line: impl Into<String>) {
        let line = line.into();
        println!("{line}");
        self.lines.push(line);
        // Flush every line: a run that hangs or is killed still leaves what it
        // learned up to that point.
        let _ = std::fs::write(&self.path, self.lines.join("\r\n"));
    }
}

/// Send an oversized ClientHello the way a browser would and say what came back.
///
/// Written in one call so the kernel segments it exactly as it does for Chrome,
/// rather than us choosing the boundary.
fn big_hello_result(host: &str, size: usize, sni_last: bool) -> String {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    let Some(addr) = (host, 443u16).to_socket_addrs().ok().and_then(|mut a| a.next()) else {
        return "주소 확인 실패".into();
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(5)) else {
        return "연결 실패".into();
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(6)));
    let _ = stream.set_nodelay(true);

    let hello = if sni_last {
        shard::desync::build_client_hello_sni_last(host, size)
    } else {
        shard::desync::build_client_hello(host, size)
    };
    if let Err(e) = stream.write_all(&hello) {
        return format!("전송 실패: {e}");
    }
    let mut buf = [0u8; 16];
    match stream.read(&mut buf) {
        Ok(0) => "응답 없이 닫힘 — 차단".into(),
        Ok(n) => format!("성공 (응답 {n}바이트)"),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => "무응답 (타임아웃) — 차단".into(),
        Err(e) => format!("끊김 — 차단 ({e})"),
    }
}

fn main() -> anyhow::Result<()> {
    let mut report = Report::new();

    let host = match std::env::args().nth(1) {
        Some(h) => shard::config::normalise_host(&h),
        None => {
            report.say("usage: probe.exe <hostname>");
            std::process::exit(2);
        }
    };

    if !uikit::elevation::is_elevated() {
        report.say("관리자 권한이 필요합니다.");
        std::process::exit(1);
    }

    // `verify` answers a different question: not "which strategy works" but
    // "does the configuration as it stands get this site loading".
    let verify_only = std::env::args().nth(2).is_some_and(|a| a == "verify");

    report.say(format!("대상: {host}"));
    if verify_only {
        report.say("모드: 현재 설정 검증");
    } else {
        report.say(format!("사다리: {}개 전략", Strategy::ladder().len()));
    }

    // Work on the user's real configuration so a discovered strategy lands
    // where the app will read it — but put back anything switched off for the
    // run, or this tool would quietly disable their settings.
    let mut config = Config::load();
    let restore_auto_learn = config.auto_learn;
    let restore_scope = config.scope;
    // The background learner would probe in parallel and fight for the same
    // per-host override.
    config.auto_learn = false;
    config.scope = Scope::All;
    let shared = Shared::new(config);

    let mut engine = match Engine::start(shared.clone()) {
        Ok(e) => e,
        Err(e) => {
            report.say(format!("엔진을 시작할 수 없습니다: {e:#}"));
            std::process::exit(1);
        }
    };
    report.say("엔진 시작됨");

    if verify_only {
        let strategy = shared.config.read().strategy_for(&host).clone();
        report.say(format!("적용 전략: {} · {}", strategy.desync.label(), strategy.fooling.label()));

        let ok = prober::reachable(&shared, &host);
        report.say(format!("작은 ClientHello (rustls): {}", if ok { "성공" } else { "실패" }));

        // Reproduce what a browser actually sends. Chrome's hello passes 2 KB
        // once post-quantum key shares are in, so TCP splits it — and a hello
        // that arrives in two segments is a different case entirely for
        // anything trying to read the hostname out of it.
        for size in [1800usize, 2400] {
            report.say(format!(
                "큰 ClientHello ({size}B, SNI 앞쪽): {}",
                big_hello_result(&host, size, false)
            ));
        }
        // The hard case: the hostname is past the segment boundary, so nothing
        // can read it from the opening packet.
        for size in [2000usize, 2800] {
            report.say(format!(
                "큰 ClientHello ({size}B, SNI 둘째 조각): {}",
                big_hello_result(&host, size, true)
            ));
        }

        engine.stop();
        let stats = shared.stats.snapshot();
        report.say(format!(
            "처리: TLS {} · 조각 {} · 디코이 {} · 해석실패 {} · 오류 {}",
            stats.tls_handled, stats.fragments_sent, stats.decoys_sent, stats.tls_unparsed, stats.errors
        ));
        return Ok(());
    }

    let mut winner = None;
    for progress in prober::spawn(shared.clone(), host.clone()) {
        match progress {
            Progress::Started { rungs, .. } => report.say(format!("--- 탐색 시작 ({rungs}단계) ---")),
            Progress::Dns { system, encrypted, tampered } => report.say(format!(
                "DNS  시스템={} 암호화={}{}",
                system.unwrap_or_else(|| "실패".into()),
                encrypted.unwrap_or_else(|| "실패".into()),
                if tampered { "  << 응답이 다름" } else { "" }
            )),
            Progress::Baseline { reachable, addr } => report.say(format!(
                "기준 (우회 없음, {}): {}",
                addr.ip(),
                if reachable { "성공 — 차단 아님" } else { "실패 — 차단 확인" }
            )),
            Progress::Attempt { index, label, ok, elapsed_ms } => report.say(format!(
                "  {:>2}. {:<28} {}  ({elapsed_ms}ms)",
                index + 1,
                label,
                if ok { "성공" } else { "실패" }
            )),
            Progress::Finished { winner: w } => winner = Some(w),
            Progress::Error(e) => report.say(format!("오류: {e}")),
        }
    }

    engine.stop();

    {
        let mut cfg = shared.config.write();
        cfg.auto_learn = restore_auto_learn;
        cfg.scope = restore_scope;
        if let Err(e) = cfg.save() {
            report.say(format!("설정 복구 실패: {e}"));
        }
    }

    report.say("=== 결론 ===");
    match winner {
        Some(Some(label)) => report.say(format!("통하는 전략: {label}")),
        Some(None) => report.say("통하는 전략 없음 — 분할·디코이로는 뚫리지 않는 차단입니다"),
        None => report.say("탐색이 완료되지 않았습니다"),
    }

    let stats = shared.stats.snapshot();
    report.say(format!(
        "처리: TLS {} · 조각 {} · 디코이 {} · 차단감지 {} · 해석실패 {} · 오류 {}",
        stats.tls_handled,
        stats.fragments_sent,
        stats.decoys_sent,
        stats.blocks_detected,
        stats.tls_unparsed,
        stats.errors
    ));
    Ok(())
}
