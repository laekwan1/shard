//! Socket-level desync, for platforms where packets cannot be crafted.
//!
//! On iOS and Android an app receives packets from a TUN interface but cannot
//! put raw IP back on the wire — everything outbound goes through an ordinary
//! socket. That rules out the decoy the desktop engine relies on: a decoy has
//! to occupy the same sequence numbers as the real request, and a socket only
//! ever moves forward.
//!
//! Two things a socket *can* do turn out to be enough:
//!
//! - choose where one write ends and the next begins, which becomes a TCP
//!   segment boundary once Nagle is off
//! - send a byte as TCP urgent data, which the receiver's stack pulls out of
//!   the stream while a middlebox reassembling raw payload keeps it inline
//!
//! The second is the important one. The server sees a clean ClientHello; the
//! middlebox sees one with a stray byte wedged into the hostname, so its match
//! fails. Measured against a Korean ISP, splitting alone failed every time and
//! split-plus-urgent succeeded — which is the whole reason this module exists.

use std::io::{self, Write};
use std::net::TcpStream;

/// How the first write of a connection is broken up.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    /// Send untouched.
    #[default]
    None,
    /// Two writes with a boundary inside the hostname.
    Split,
    /// Split, with one byte of urgent data at the seam. The only combination
    /// measured to defeat a reassembling middlebox.
    SplitOob,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::None => "없음",
            Mode::Split => "분할",
            Mode::SplitOob => "분할 + OOB",
        }
    }

    pub const ALL: &'static [Mode] = &[Mode::None, Mode::Split, Mode::SplitOob];
}

/// Where to cut, and how.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Plan {
    pub mode: Mode,
    /// Byte offset of the cut within the payload.
    pub at: usize,
}

impl Default for Plan {
    fn default() -> Self {
        Self { mode: Mode::SplitOob, at: 1 }
    }
}

/// Decide how to send the opening payload of a connection.
///
/// The hostname's midpoint is preferred so neither fragment contains the whole
/// string. When the payload carries no hostname we cannot do better than an
/// early fixed offset — which is also what works against matchers that only
/// look at the first segment.
pub fn plan(payload: &[u8], mode: Mode, fallback: usize) -> Plan {
    if mode == Mode::None || payload.len() < 2 {
        return Plan { mode: Mode::None, at: 0 };
    }
    let at = shard::parse::tls::client_hello_sni(payload)
        .map(|host| host.midpoint())
        .or_else(|| shard::parse::http::host_header(payload).map(|h| h.host.midpoint()))
        .unwrap_or(fallback)
        .clamp(1, payload.len() - 1);
    Plan { mode, at }
}

/// Send a payload according to the plan.
///
/// Nagle must already be off, or the kernel coalesces the writes back into one
/// segment and the split never reaches the wire.
pub fn send_desynced(stream: &mut TcpStream, payload: &[u8], plan: Plan) -> io::Result<()> {
    match plan.mode {
        Mode::None => stream.write_all(payload),
        Mode::Split => {
            let at = plan.at.min(payload.len());
            stream.write_all(&payload[..at])?;
            stream.write_all(&payload[at..])
        }
        Mode::SplitOob => {
            let at = plan.at.min(payload.len());
            stream.write_all(&payload[..at])?;
            // Occupies a sequence number, but the peer's stack lifts it out of
            // the byte stream — so the server's parser never sees it and a
            // middlebox's does.
            send_urgent(stream, b'x')?;
            stream.write_all(&payload[at..])
        }
    }
}

/// Send one byte as TCP urgent data. `std` has no API for this, so it goes
/// straight to the platform's `send`.
#[cfg(windows)]
pub fn send_urgent(stream: &TcpStream, byte: u8) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    const MSG_OOB: i32 = 0x1;
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
        Err(io::Error::last_os_error())
    }
}

#[cfg(unix)]
pub fn send_urgent(stream: &TcpStream, byte: u8) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let buf = [byte];
    let sent = unsafe {
        libc::send(stream.as_raw_fd(), buf.as_ptr().cast(), 1, libc::MSG_OOB)
    };
    if sent == 1 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;

    /// A ClientHello for `host`, built by the desktop crate so both engines
    /// agree on the wire format.
    fn hello(host: &str, pad: usize) -> Vec<u8> {
        shard::desync::build_client_hello(host, pad)
    }

    #[test]
    fn plan_cuts_inside_the_hostname() {
        let payload = hello("www.example.com", 0);
        let host = shard::parse::tls::client_hello_sni(&payload).unwrap();
        let plan = plan(&payload, Mode::SplitOob, 1);

        assert_eq!(plan.mode, Mode::SplitOob);
        assert!(plan.at > host.offset, "cut must land after the name starts");
        assert!(plan.at < host.offset + host.len, "and before it ends");
    }

    #[test]
    fn plan_falls_back_when_there_is_no_hostname() {
        let payload = vec![0u8; 64];
        assert_eq!(plan(&payload, Mode::Split, 3).at, 3);
    }

    #[test]
    fn plan_stays_inside_the_payload() {
        let payload = hello("a.test", 0);
        // A fallback past the end would otherwise slice out of bounds.
        let plan = plan(&payload, Mode::Split, 99_999);
        assert!(plan.at >= 1 && plan.at < payload.len());
    }

    #[test]
    fn none_mode_plans_nothing() {
        let payload = hello("a.test", 0);
        assert_eq!(plan(&payload, Mode::None, 1).mode, Mode::None);
        // Too short to split meaningfully.
        assert_eq!(plan(&[0x16], Mode::SplitOob, 1).mode, Mode::None);
    }

    /// Echo server that returns everything it received in the normal stream.
    /// Urgent bytes are excluded by default, which is exactly the property the
    /// technique depends on.
    fn echo_server() -> (u16, mpsc::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut got = Vec::new();
                let mut buf = [0u8; 4096];
                socket.set_read_timeout(Some(std::time::Duration::from_millis(700))).ok();
                while let Ok(n) = socket.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    got.extend_from_slice(&buf[..n]);
                }
                let _ = tx.send(got);
            }
        });
        (port, rx)
    }

    #[test]
    fn split_delivers_every_byte_in_order() {
        let payload = hello("www.example.com", 400);
        let (port, rx) = echo_server();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_nodelay(true).unwrap();

        send_desynced(&mut stream, &payload, Plan { mode: Mode::Split, at: 40 }).unwrap();
        drop(stream);

        let got = rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        assert_eq!(got, payload, "splitting must not change what arrives");
    }

    #[test]
    fn the_urgent_byte_never_reaches_the_application() {
        // This is the whole mechanism: the server's parser sees a clean
        // ClientHello while anything reassembling raw payload sees an extra
        // byte in the middle of the hostname.
        let payload = hello("www.example.com", 400);
        let (port, rx) = echo_server();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_nodelay(true).unwrap();

        let plan = plan(&payload, Mode::SplitOob, 1);
        send_desynced(&mut stream, &payload, plan).unwrap();
        drop(stream);

        let got = rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
        assert_eq!(got, payload, "the urgent byte must be lifted out of the stream");
        assert_eq!(
            shard::parse::tls::client_hello_sni(&got).map(|h| h.name),
            Some("www.example.com".to_string()),
            "the server must still be able to read the real hostname"
        );
    }

    #[test]
    fn none_mode_sends_the_payload_untouched() {
        let payload = hello("a.test", 100);
        let (port, rx) = echo_server();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        send_desynced(&mut stream, &payload, Plan { mode: Mode::None, at: 0 }).unwrap();
        drop(stream);
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap(), payload);
    }
}
