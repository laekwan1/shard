//! The tunnel, ours end to end.
//!
//! A client opens a real TLS 1.3 connection to the server, states where it
//! wants to go, and then the two copy bytes. That is the whole design. What
//! makes it hard to block is not cleverness but ordinariness: a middlebox
//! inspecting the connection finds a genuine TLS handshake to a genuine
//! certificate, because that is exactly what it is.
//!
//! The one thing this crate does not implement is the cryptography. TLS comes
//! from rustls and the hash from sha2 — writing either by hand is how people
//! ship something that looks fine and is broken.
//!
//! Both halves live here so the client and the server can never drift apart,
//! and so a test can run one against the other in a single process.

pub mod client;
pub mod inbound;
pub mod link;
pub mod presets;
pub mod protocol;
pub mod server;
pub mod tls;

pub use protocol::{Address, Request};

/// How long to wait for a TCP connection or a TLS handshake.
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);
/// Copy buffer for the relay loops.
pub(crate) const RELAY_BUFFER: usize = 32 * 1024;
