//! The wire format.
//!
//! Everything a request carries, in order: who you are, where you want to go,
//! and then the bytes. There is no encryption here and none is needed — this
//! rides inside a real TLS 1.3 connection, so to anything watching the wire it
//! is an ordinary HTTPS session.
//!
//! ```text
//! hex(sha224(password))  CR LF
//! CMD  ATYP  ADDR  PORT  CR LF
//! payload…
//! ```
//!
//! This is the Trojan format. Following a published one rather than inventing a
//! private one is a deliberate choice: it means the client can be tested
//! against an independent server and the server against an independent client,
//! so "it works between our own two halves" is never mistaken for "it is
//! correct".

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha224};

pub const CRLF: [u8; 2] = [0x0D, 0x0A];
/// Length of the hex-encoded SHA-224 that opens every request.
pub const PASSWORD_HEX_LEN: usize = 56;

const CMD_CONNECT: u8 = 0x01;
const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;

/// Where a request wants to go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address {
    Ipv4([u8; 4]),
    Domain(String),
    Ipv6([u8; 16]),
}

impl Address {
    /// Parse a host string the way a proxy receives it.
    pub fn parse(host: &str) -> Self {
        if let Ok(v4) = host.parse::<std::net::Ipv4Addr>() {
            return Address::Ipv4(v4.octets());
        }
        // A bracketed literal is how a URL carries IPv6; accept both forms.
        let bare = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
        if let Ok(v6) = bare.parse::<std::net::Ipv6Addr>() {
            return Address::Ipv6(v6.octets());
        }
        Address::Domain(host.to_string())
    }

    pub fn to_host(&self) -> String {
        match self {
            Address::Ipv4(o) => std::net::Ipv4Addr::from(*o).to_string(),
            Address::Ipv6(o) => std::net::Ipv6Addr::from(*o).to_string(),
            Address::Domain(d) => d.clone(),
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Address::Ipv4(o) => {
                out.push(ATYP_IPV4);
                out.extend_from_slice(o);
            }
            Address::Domain(d) => {
                out.push(ATYP_DOMAIN);
                // A domain is length-prefixed with a single byte, so anything
                // longer than 255 cannot be represented.
                out.push(d.len() as u8);
                out.extend_from_slice(d.as_bytes());
            }
            Address::Ipv6(o) => {
                out.push(ATYP_IPV6);
                out.extend_from_slice(o);
            }
        }
    }
}

/// One request's opening header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub address: Address,
    pub port: u16,
}

impl Request {
    pub fn new(host: &str, port: u16) -> Self {
        Self { address: Address::parse(host), port }
    }

    pub fn host(&self) -> String {
        self.address.to_host()
    }

    /// Serialise the whole opening block, `password` included.
    pub fn encode(&self, password: &str) -> Result<Vec<u8>> {
        if let Address::Domain(d) = &self.address {
            if d.is_empty() || d.len() > 255 {
                bail!("도메인 길이가 1~255 바이트를 벗어납니다: {} 바이트", d.len());
            }
        }

        let mut out = Vec::with_capacity(64);
        out.extend_from_slice(password_hash(password).as_bytes());
        out.extend_from_slice(&CRLF);
        out.push(CMD_CONNECT);
        self.address.encode(&mut out);
        out.extend_from_slice(&self.port.to_be_bytes());
        out.extend_from_slice(&CRLF);
        Ok(out)
    }

    /// Read a header out of `input`, returning it and how many bytes it used.
    ///
    /// Returns `Ok(None)` when `input` is a valid prefix that is simply not
    /// complete yet, so a caller reading from a socket can wait for more rather
    /// than treating a short read as an error.
    pub fn decode(input: &[u8]) -> Result<Option<(Request, usize)>> {
        let mut at = PASSWORD_HEX_LEN;
        if input.len() < at + 2 {
            return Ok(None);
        }
        if input[at..at + 2] != CRLF {
            bail!("비밀번호 뒤에 CRLF가 없습니다");
        }
        at += 2;

        let Some(&cmd) = input.get(at) else { return Ok(None) };
        if cmd != CMD_CONNECT {
            bail!("지원하지 않는 명령입니다: 0x{cmd:02x}");
        }
        at += 1;

        let Some(&atyp) = input.get(at) else { return Ok(None) };
        at += 1;

        let address = match atyp {
            ATYP_IPV4 => {
                let Some(bytes) = input.get(at..at + 4) else { return Ok(None) };
                at += 4;
                Address::Ipv4(bytes.try_into().expect("checked length"))
            }
            ATYP_IPV6 => {
                let Some(bytes) = input.get(at..at + 16) else { return Ok(None) };
                at += 16;
                Address::Ipv6(bytes.try_into().expect("checked length"))
            }
            ATYP_DOMAIN => {
                let Some(&len) = input.get(at) else { return Ok(None) };
                at += 1;
                let Some(bytes) = input.get(at..at + len as usize) else { return Ok(None) };
                at += len as usize;
                Address::Domain(
                    String::from_utf8(bytes.to_vec()).context("도메인이 UTF-8이 아닙니다")?,
                )
            }
            other => bail!("알 수 없는 주소 종류입니다: 0x{other:02x}"),
        };

        let Some(port) = input.get(at..at + 2) else { return Ok(None) };
        let port = u16::from_be_bytes([port[0], port[1]]);
        at += 2;

        let Some(tail) = input.get(at..at + 2) else { return Ok(None) };
        if tail != CRLF {
            bail!("주소 뒤에 CRLF가 없습니다");
        }
        at += 2;

        Ok(Some((Request { address, port }, at)))
    }

    /// The password hex an incoming request claims, if enough has arrived.
    pub fn claimed_password(input: &[u8]) -> Option<&[u8]> {
        input.get(..PASSWORD_HEX_LEN)
    }
}

/// Hex-encoded SHA-224 of the password, which is what actually goes on the wire.
pub fn password_hash(password: &str) -> String {
    hex::encode(Sha224::digest(password.as_bytes()))
}

/// Compare two password hashes without an early exit.
///
/// A byte-at-a-time comparison leaks where the first difference is, which over
/// many attempts recovers the expected value. The cost of doing it properly is
/// nothing, so there is no reason not to.
pub fn hash_matches(expected: &str, claimed: &[u8]) -> bool {
    let expected = expected.as_bytes();
    if expected.len() != claimed.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in expected.iter().zip(claimed) {
        diff |= a ^ b;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_survives_a_round_trip() {
        for (host, port) in [
            ("example.com", 443u16),
            ("203.0.113.9", 80),
            ("2001:db8::1", 8443),
            ("[2001:db8::1]", 443),
        ] {
            let request = Request::new(host, port);
            let encoded = request.encode("hunter2").unwrap();
            let (decoded, used) = Request::decode(&encoded).unwrap().unwrap();

            assert_eq!(decoded, request, "{host}");
            assert_eq!(used, encoded.len(), "{host}: header length");
            assert_eq!(decoded.port, port);
        }
    }

    #[test]
    fn a_bracketed_literal_decodes_to_the_same_address_as_a_bare_one() {
        // A URL writes IPv6 in brackets and a header does not; both must reach
        // the same server or the tunnel would dial the wrong place.
        assert_eq!(Address::parse("[2001:db8::1]"), Address::parse("2001:db8::1"));
    }

    #[test]
    fn payload_after_the_header_is_left_alone() {
        let request = Request::new("example.com", 443);
        let mut wire = request.encode("pw").unwrap();
        let header_len = wire.len();
        wire.extend_from_slice(b"GET / HTTP/1.1\r\n\r\n");

        let (_, used) = Request::decode(&wire).unwrap().unwrap();
        assert_eq!(used, header_len);
        assert_eq!(&wire[used..], b"GET / HTTP/1.1\r\n\r\n");
    }

    #[test]
    fn a_truncated_header_asks_for_more_rather_than_failing() {
        // A socket hands over whatever has arrived; a short read is normal.
        let wire = Request::new("a-fairly-long-domain.example", 443).encode("pw").unwrap();
        for cut in 0..wire.len() {
            assert_eq!(
                Request::decode(&wire[..cut]).unwrap(),
                None,
                "{cut} bytes should be treated as incomplete"
            );
        }
        assert!(Request::decode(&wire).unwrap().is_some());
    }

    #[test]
    fn a_malformed_header_is_rejected() {
        let mut wire = Request::new("example.com", 443).encode("pw").unwrap();
        wire[PASSWORD_HEX_LEN] = b'X'; // where the CRLF should be
        assert!(Request::decode(&wire).is_err());

        let mut wire = Request::new("example.com", 443).encode("pw").unwrap();
        wire[PASSWORD_HEX_LEN + 2] = 0x09; // an unsupported command
        assert!(Request::decode(&wire).is_err());
    }

    #[test]
    fn the_password_hash_is_the_documented_one() {
        // Fixed vector: this is what an independent implementation must produce
        // for the two to interoperate at all.
        assert_eq!(
            password_hash("password"),
            "d63dc919e201d7bc4c825630d2cf25fdc93d4b2f0d46706d29038d01"
        );
        assert_eq!(password_hash("").len(), PASSWORD_HEX_LEN);
    }

    #[test]
    fn hash_comparison_accepts_only_the_exact_value() {
        let expected = password_hash("secret");
        assert!(hash_matches(&expected, expected.as_bytes()));
        assert!(!hash_matches(&expected, password_hash("secre").as_bytes()));
        assert!(!hash_matches(&expected, b"short"));
        // A prefix must not pass, which is the whole point of the fixed-time
        // comparison being length-checked first.
        assert!(!hash_matches(&expected, &expected.as_bytes()[..10]));
    }

    #[test]
    fn an_over_long_domain_is_refused_rather_than_truncated() {
        // The length prefix is one byte; silently wrapping would dial a
        // completely different host.
        let long = "a".repeat(256);
        assert!(Request::new(&long, 443).encode("pw").is_err());
        assert!(Request::new(&"a".repeat(255), 443).encode("pw").is_ok());
    }
}
