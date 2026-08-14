//! Managing the sing-box child process.
//!
//! The core is validated before it is launched and its stderr is pumped into a
//! ring buffer, because a tunnel that silently fails to start is exactly the
//! situation where the user needs to see the reason.

use crate::config::Config;
use crate::profile::Profile;
use crate::singbox;

use anyhow::{anyhow, bail, Context, Result};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_LOG_LINES: usize = 200;

/// Recent core output, shared with the UI.
pub type LogBuffer = Arc<Mutex<VecDeque<String>>>;

pub fn new_log_buffer() -> LogBuffer {
    Arc::new(Mutex::new(VecDeque::with_capacity(MAX_LOG_LINES)))
}

/// Locate `sing-box.exe`: beside our executable in a release layout, or in the
/// repository's vendor directory during development.
pub fn locate_binary() -> Result<PathBuf> {
    let exe_dir = uikit::config::exe_dir();
    let candidates = [
        exe_dir.join("sing-box.exe"),
        exe_dir.join("vendor").join("singbox").join("sing-box.exe"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/singbox/sing-box.exe"),
    ];
    candidates
        .into_iter()
        .find(|p| p.exists())
        .ok_or_else(|| anyhow!("sing-box.exe를 찾을 수 없습니다 (실행 파일과 같은 폴더에 있어야 합니다)"))
}

pub fn config_path() -> PathBuf {
    uikit::config::app_dir(crate::config::APP_NAME).join("sing-box.json")
}

/// Write the generated configuration and have sing-box validate it.
///
/// Validating first turns "the child died immediately" into a specific
/// complaint about the field that is wrong.
pub fn write_and_check(cfg: &Config, profile: &Profile) -> Result<PathBuf> {
    let binary = locate_binary()?;
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let value = singbox::build(cfg, profile);
    std::fs::write(&path, serde_json::to_vec_pretty(&value)?)
        .with_context(|| format!("{} 쓰기 실패", path.display()))?;

    let output = Command::new(&binary)
        .arg("check")
        .arg("-c")
        .arg(&path)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("sing-box를 실행할 수 없습니다")?;

    if !output.status.success() {
        bail!("설정이 거부되었습니다: {}", tidy(&output.stderr));
    }
    Ok(path)
}

/// sing-box writes ANSI-coloured, timestamped lines; strip the noise so the
/// message is readable in the UI.
fn tidy(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .map(strip_ansi)
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" / ")
}

fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Skip the CSI sequence up to and including its final letter.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub struct Core {
    child: Child,
    pub log: LogBuffer,
}

impl Core {
    /// Validate the configuration, then launch the core.
    pub fn start(cfg: &Config, profile: &Profile, log: LogBuffer) -> Result<Self> {
        let binary = locate_binary()?;
        let config = write_and_check(cfg, profile)?;

        let mut child = Command::new(&binary)
            .arg("run")
            .arg("-c")
            .arg(&config)
            // sing-box resolves relative resource paths against the working
            // directory, and wintun.dll must be findable here too.
            .current_dir(binary.parent().unwrap_or(Path::new(".")))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .context("sing-box를 시작할 수 없습니다")?;

        if let Some(stderr) = child.stderr.take() {
            let log = log.clone();
            std::thread::Builder::new()
                .name("veil-core-log".to_string())
                .spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        let line = strip_ansi(&line);
                        tracing::debug!(target: "sing-box", "{line}");
                        let mut buffer = log.lock();
                        if buffer.len() == MAX_LOG_LINES {
                            buffer.pop_front();
                        }
                        buffer.push_back(line);
                    }
                })
                .ok();
        }

        Ok(Self { child, log })
    }

    /// `Ok(())` while running; `Err` carries the last log lines once it has died.
    pub fn health(&mut self) -> Result<()> {
        match self.child.try_wait() {
            Ok(None) => Ok(()),
            Ok(Some(status)) => {
                let tail: Vec<String> = self.log.lock().iter().rev().take(3).cloned().collect();
                Err(anyhow!("코어가 종료되었습니다 ({status}): {}", tail.join(" / ")))
            }
            Err(e) => Err(anyhow!("코어 상태를 확인할 수 없습니다: {e}")),
        }
    }

    pub fn stop(&mut self) {
        // sing-box has no graceful signal on Windows; killing it releases the
        // TUN adapter, and the kill switch stays up until we tear it down.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Cumulative byte counters from sing-box's Clash API.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Traffic {
    pub up: u64,
    pub down: u64,
    pub connections: usize,
}

/// Read totals from the Clash API. Rates are derived by the caller from two
/// samples, which avoids holding a streaming connection open.
pub async fn traffic(client: &reqwest::Client, port: u16) -> Result<Traffic> {
    let url = format!("http://127.0.0.1:{port}/connections");
    let body: serde_json::Value = client.get(&url).send().await?.json().await?;
    Ok(Traffic {
        up: body["uploadTotal"].as_u64().unwrap_or(0),
        down: body["downloadTotal"].as_u64().unwrap_or(0),
        connections: body["connections"].as_array().map_or(0, Vec::len),
    })
}

/// A traffic reading plus the rate since the previous one.
#[derive(Clone, Copy, Default, Debug)]
pub struct TrafficSample {
    pub total: Traffic,
    pub up_bps: u64,
    pub down_bps: u64,
}

/// Polls the Clash API once a second on its own runtime.
///
/// Polling cumulative totals and differencing them is simpler than holding the
/// streaming endpoint open, and a dropped sample just shows as one flat second.
pub struct TrafficMonitor {
    stop: tokio::sync::watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub latest: Arc<Mutex<TrafficSample>>,
}

impl TrafficMonitor {
    pub fn start(port: u16) -> Self {
        let latest = Arc::new(Mutex::new(TrafficSample::default()));
        let (stop, mut stop_rx) = tokio::sync::watch::channel(false);
        let shared = latest.clone();

        let thread = std::thread::Builder::new()
            .name("veil-traffic".to_string())
            .spawn(move || {
                let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                    return;
                };
                rt.block_on(async move {
                    let client = reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(2))
                        .build()
                        .unwrap_or_default();
                    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));
                    let mut previous: Option<Traffic> = None;
                    loop {
                        tokio::select! {
                            _ = stop_rx.changed() => break,
                            _ = ticker.tick() => {
                                let Ok(now) = traffic(&client, port).await else { continue };
                                let sample = TrafficSample {
                                    total: now,
                                    up_bps: previous.map_or(0, |p| now.up.saturating_sub(p.up)),
                                    down_bps: previous.map_or(0, |p| now.down.saturating_sub(p.down)),
                                };
                                previous = Some(now);
                                *shared.lock() = sample;
                            }
                        }
                    }
                });
            })
            .ok();

        Self { stop, thread, latest }
    }

    pub fn sample(&self) -> TrafficSample {
        *self.latest.lock()
    }
}

impl Drop for TrafficMonitor {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Human-readable byte rate for the UI.
pub fn format_rate(bytes_per_second: u64) -> String {
    const UNITS: [(&str, u64); 4] =
        [("GB/s", 1 << 30), ("MB/s", 1 << 20), ("KB/s", 1 << 10), ("B/s", 1)];
    for (unit, scale) in UNITS {
        if bytes_per_second >= scale {
            return format!("{:.1} {unit}", bytes_per_second as f64 / scale as f64);
        }
    }
    "0 B/s".to_string()
}

/// Human-readable cumulative total.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [("GB", 1 << 30), ("MB", 1 << 20), ("KB", 1 << 10), ("B", 1)];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            return format!("{:.1} {unit}", bytes as f64 / scale as f64);
        }
    }
    "0 B".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_rates_at_each_scale() {
        assert_eq!(format_rate(0), "0 B/s");
        assert_eq!(format_rate(512), "512.0 B/s");
        assert_eq!(format_rate(1536), "1.5 KB/s");
        assert_eq!(format_rate(5 << 20), "5.0 MB/s");
        assert_eq!(format_rate(3 << 30), "3.0 GB/s");
    }

    #[test]
    fn formats_totals_at_each_scale() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(1 << 30), "1.0 GB");
    }

    #[test]
    fn strips_ansi_colour_codes() {
        let line = "\u{1b}[31mERROR\u{1b}[0m[0000] something broke";
        assert_eq!(strip_ansi(line), "ERROR[0000] something broke");
    }

    #[test]
    fn leaves_plain_lines_alone() {
        assert_eq!(strip_ansi("plain output"), "plain output");
    }

    #[test]
    fn tidy_joins_and_drops_blank_lines() {
        let raw = b"\x1b[31mFATAL\x1b[0m one\n\n  \nsecond line\n";
        assert_eq!(tidy(raw), "FATAL one / second line");
    }

    #[test]
    fn locates_the_vendored_binary() {
        // The repository always has it; a packaged build finds it beside the exe.
        assert!(locate_binary().is_ok());
    }

    #[test]
    fn log_buffer_stays_bounded() {
        let log = new_log_buffer();
        for i in 0..MAX_LOG_LINES + 25 {
            let mut buffer = log.lock();
            if buffer.len() == MAX_LOG_LINES {
                buffer.pop_front();
            }
            buffer.push_back(format!("line {i}"));
        }
        assert_eq!(log.lock().len(), MAX_LOG_LINES);
    }
}
