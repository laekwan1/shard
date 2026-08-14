//! Feasibility test for a phone port.
//!
//! On Android and iOS an app gets a TUN interface but cannot put raw IP packets
//! back on the wire — everything outbound has to go through an ordinary socket.
//! That rules out the decoy strategy this project relies on, because a decoy
//! has to occupy the same sequence numbers as the real request and a socket
//! only ever moves forward.
//!
//! What a socket *can* do is choose where one write ends and the next begins,
//! and send a byte as TCP urgent data. Urgent data is the interesting one: the
//! server and a middlebox often disagree about whether that byte belongs in the
//! stream, so the middlebox reassembles a hostname with a stray byte in it
//! while the server sees the request intact.
//!
//! This runs those socket-only techniques against a real blocked host and says
//! which, if any, get a reply. **Shard's engine must be off** — otherwise it is
//! measuring the engine, not the technique.
//!
//!     mobile.exe www.example.com

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::os::windows::io::AsRawSocket;
use std::time::Duration;

use shard::desync::build_client_hello;

const TIMEOUT: Duration = Duration::from_secs(6);
/// Winsock's flag for TCP urgent data.
const MSG_OOB: i32 = 0x1;

fn connect(addr: SocketAddr) -> Result<TcpStream, String> {
    let stream = TcpStream::connect_timeout(&addr, TIMEOUT).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(TIMEOUT)).ok();
    // Without this the kernel would coalesce our carefully separated writes
    // back into one segment, erasing the whole point.
    stream.set_nodelay(true).ok();
    Ok(stream)
}

/// Send one byte as urgent data. std has no API for this, so it goes through
/// winsock directly — the same call a phone app would make on BSD sockets.
fn send_urgent(stream: &TcpStream, byte: u8) -> Result<(), String> {
    let sent = unsafe {
        windows_sys::Win32::Networking::WinSock::send(
            stream.as_raw_socket() as _,
            [byte].as_ptr(),
            1,
            MSG_OOB,
        )
    };
    if sent == 1 {
        Ok(())
    } else {
        Err(format!("OOB 전송 실패 ({sent})"))
    }
}

fn read_reply(stream: &mut TcpStream) -> String {
    let mut buf = [0u8; 16];
    match stream.read(&mut buf) {
        Ok(0) => "응답 없이 닫힘".into(),
        Ok(n) => format!("성공 (응답 {n}바이트)"),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => "무응답 (타임아웃)".into(),
        Err(_) => "끊김 (리셋)".into(),
    }
}

/// Techniques a phone app could actually implement.
enum Technique {
    /// No manipulation at all — establishes that the host is blocked.
    Plain,
    /// Two writes with a boundary at `at`.
    Split { at: usize },
    /// Split, with one byte of urgent data at the seam.
    SplitOob { at: usize },
    /// Urgent byte first, then the whole hello.
    OobFirst,
}

impl Technique {
    fn label(&self) -> String {
        match self {
            Technique::Plain => "그대로 전송 (대조)".into(),
            Technique::Split { at } => format!("분할 @{at}"),
            Technique::SplitOob { at } => format!("분할 @{at} + OOB"),
            Technique::OobFirst => "OOB 먼저".into(),
        }
    }

    fn run(&self, addr: SocketAddr, hello: &[u8]) -> String {
        let mut stream = match connect(addr) {
            Ok(s) => s,
            Err(e) => return format!("연결 실패: {e}"),
        };
        let write = |s: &mut TcpStream, bytes: &[u8]| -> Result<(), String> {
            s.write_all(bytes).map_err(|e| e.to_string())
        };

        let result = match self {
            Technique::Plain => write(&mut stream, hello),
            Technique::Split { at } => {
                let at = (*at).min(hello.len());
                write(&mut stream, &hello[..at])
                    .and_then(|()| write(&mut stream, &hello[at..]))
            }
            Technique::SplitOob { at } => {
                let at = (*at).min(hello.len());
                write(&mut stream, &hello[..at])
                    .and_then(|()| send_urgent(&stream, b'x'))
                    .and_then(|()| write(&mut stream, &hello[at..]))
            }
            Technique::OobFirst => {
                send_urgent(&stream, b'x').and_then(|()| write(&mut stream, hello))
            }
        };
        if let Err(e) = result {
            return format!("전송 실패: {e}");
        }
        read_reply(&mut stream)
    }
}

/// Mirrors output to a file. The crate's binaries all carry a
/// requireAdministrator manifest, and Windows will not let a caller both
/// elevate a process and capture its stdout.
struct Report {
    lines: Vec<String>,
    path: std::path::PathBuf,
}

impl Report {
    fn new() -> Self {
        let path = uikit::config::app_dir(shard::config::APP_NAME).join("mobile-report.txt");
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
        Self { lines: Vec::new(), path }
    }

    fn say(&mut self, line: impl Into<String>) {
        let line = line.into();
        println!("{line}");
        self.lines.push(line);
        let _ = std::fs::write(&self.path, self.lines.join("\r\n"));
    }
}

fn main() {
    let mut report = Report::new();
    let host = match std::env::args().nth(1) {
        Some(h) => shard::config::normalise_host(&h),
        None => {
            eprintln!("usage: mobile.exe <hostname>");
            std::process::exit(2);
        }
    };

    let Some(addr) = (host.as_str(), 443u16).to_socket_addrs().ok().and_then(|mut a| a.next()) else {
        eprintln!("{host} 주소를 확인할 수 없습니다");
        std::process::exit(1);
    };

    report.say(format!("대상: {host} ({addr})"));
    report.say("Shard 엔진이 꺼져 있어야 정확합니다.");

    // Browser-sized, so the result reflects what a phone browser would send.
    let hello = build_client_hello(&host, 1800);
    report.say(format!("ClientHello {} 바이트", hello.len()));

    let techniques = [
        Technique::Plain,
        Technique::Split { at: 1 },
        Technique::Split { at: 40 },
        Technique::SplitOob { at: 1 },
        Technique::SplitOob { at: 40 },
        Technique::SplitOob { at: 76 },
        Technique::OobFirst,
    ];

    let mut any = false;
    for technique in &techniques {
        let outcome = technique.run(addr, &hello);
        let ok = outcome.starts_with("성공");
        any |= ok && !matches!(technique, Technique::Plain);
        report.say(format!("  {:<22} {}", technique.label(), outcome));
    }

    report.say("=== 결론 ===");
    if any {
        report.say("소켓만으로 뚫리는 기법이 있습니다 — 모바일 포팅이 성립합니다.");
    } else {
        report.say("소켓 수준 기법으로는 뚫리지 않습니다.");
        report.say("이 회선에서 모바일 앱은 우회에 실패합니다. 폰은 Veil(터널) 경로가 답입니다.");
    }
}
