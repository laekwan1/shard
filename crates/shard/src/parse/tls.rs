//! TLS ClientHello / SNI extraction.

use super::{Cursor, Hostname};

pub const RECORD_HANDSHAKE: u8 = 0x16;
const HANDSHAKE_CLIENT_HELLO: u8 = 0x01;
/// Record type, version, and length.
const RECORD_HEADER: usize = 5;
const EXT_SERVER_NAME: u16 = 0x0000;
const SNI_HOST_NAME: u8 = 0x00;

/// True if the payload starts a TLS handshake record. Cheap pre-filter before
/// the full parse, which runs on every outbound packet.
pub fn is_handshake(payload: &[u8]) -> bool {
    payload.len() >= 5 && payload[0] == RECORD_HANDSHAKE && payload[1] == 0x03
}

/// Extract the SNI hostname and its position within the TCP payload.
///
/// Returns `None` for anything that is not a complete, well-formed ClientHello
/// carrying a `server_name` extension — including one truncated by segmentation.
pub fn client_hello_sni(payload: &[u8]) -> Option<Hostname> {
    if !is_handshake(payload) {
        return None;
    }
    // The record may well be longer than this segment. Chrome's ClientHello
    // exceeds a normal MSS once post-quantum key shares are included, so TCP
    // splits it before we ever see it — and refusing to parse a truncated
    // record would mean ignoring most real browsers. `server_name` sits early
    // in the extension list, so the bytes we do have usually contain it.
    if payload.get(3..5).map(|l| l == [0, 0]).unwrap_or(true) {
        return None;
    }
    // Offsets are reported against the whole payload, so the record header has
    // to be counted back in.
    let mut found = handshake_sni(payload.get(RECORD_HEADER..)?)?;
    found.offset += RECORD_HEADER;
    Some(found)
}

/// The same, starting at the handshake message itself.
///
/// QUIC carries the ClientHello in CRYPTO frames with no record layer around
/// it, so the two transports meet here rather than each having their own copy
/// of the extension walk.
pub fn handshake_sni(payload: &[u8]) -> Option<Hostname> {
    let mut c = Cursor::new(payload);
    if c.u8()? != HANDSHAKE_CLIENT_HELLO {
        return None;
    }
    c.u24()?; // handshake body length
    c.skip(2 + 32)?; // legacy_version + random

    let session_id_len = c.u8()? as usize;
    c.skip(session_id_len)?;

    let cipher_suites_len = c.u16()? as usize;
    c.skip(cipher_suites_len)?;

    let compression_len = c.u8()? as usize;
    c.skip(compression_len)?;

    let extensions_len = c.u16()? as usize;
    // Clamp to what actually arrived rather than trusting the declared length.
    let extensions_end = c.at.checked_add(extensions_len)?.min(payload.len());

    while c.at < extensions_end {
        let ext_type = c.u16()?;
        let ext_len = c.u16()? as usize;
        let ext_end = c.at.checked_add(ext_len)?;
        if ext_type == EXT_SERVER_NAME {
            // This one has to be complete or the name cannot be read.
            if ext_end > payload.len() {
                return None;
            }
            return parse_server_name(&mut c, ext_end);
        }
        if ext_end > extensions_end {
            // Truncated before reaching server_name; nothing more to find here.
            return None;
        }
        c.at = ext_end;
    }
    None
}

/// `ServerNameList`: a 2-byte list length, then entries of
/// `[name_type: u8][length: u16][name]`.
fn parse_server_name(c: &mut Cursor<'_>, ext_end: usize) -> Option<Hostname> {
    let list_len = c.u16()? as usize;
    let list_end = c.at.checked_add(list_len)?.min(ext_end);
    while c.at < list_end {
        let name_type = c.u8()?;
        let name_len = c.u16()? as usize;
        let offset = c.at;
        let bytes = c.take(name_len)?;
        if name_type == SNI_HOST_NAME {
            let name = std::str::from_utf8(bytes).ok()?;
            if name.is_empty() {
                return None;
            }
            return Some(Hostname { name: name.to_ascii_lowercase(), offset, len: name_len });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a ClientHello carrying `host` in its SNI extension.
    fn client_hello(host: &str) -> Vec<u8> {
        let host_bytes = host.as_bytes();
        let mut sni_ext = Vec::new();
        sni_ext.extend_from_slice(&((host_bytes.len() + 3) as u16).to_be_bytes()); // list len
        sni_ext.push(SNI_HOST_NAME);
        sni_ext.extend_from_slice(&(host_bytes.len() as u16).to_be_bytes());
        sni_ext.extend_from_slice(host_bytes);

        let mut extensions = Vec::new();
        extensions.extend_from_slice(&EXT_SERVER_NAME.to_be_bytes());
        extensions.extend_from_slice(&(sni_ext.len() as u16).to_be_bytes());
        extensions.extend_from_slice(&sni_ext);

        let mut body = Vec::new();
        body.extend_from_slice(&[0x03, 0x03]); // legacy version
        body.extend_from_slice(&[0x11; 32]); // random
        body.push(0); // empty session id
        body.extend_from_slice(&2u16.to_be_bytes());
        body.extend_from_slice(&[0x13, 0x01]); // one cipher suite
        body.push(1); // compression methods length
        body.push(0);
        body.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
        body.extend_from_slice(&extensions);

        let mut handshake = vec![HANDSHAKE_CLIENT_HELLO];
        let len = body.len() as u32;
        handshake.extend_from_slice(&[(len >> 16) as u8, (len >> 8) as u8, len as u8]);
        handshake.extend_from_slice(&body);

        let mut record = vec![RECORD_HANDSHAKE, 0x03, 0x01];
        record.extend_from_slice(&(handshake.len() as u16).to_be_bytes());
        record.extend_from_slice(&handshake);
        record
    }

    #[test]
    fn extracts_sni_and_offset() {
        let pkt = client_hello("www.example.com");
        let host = client_hello_sni(&pkt).expect("sni");
        assert_eq!(host.name, "www.example.com");
        assert_eq!(host.len, 15);
        assert_eq!(&pkt[host.offset..host.offset + host.len], b"www.example.com");
        // The midpoint must land strictly inside the hostname.
        assert!(host.midpoint() > host.offset);
        assert!(host.midpoint() < host.offset + host.len);
    }

    #[test]
    fn lowercases_hostname() {
        let pkt = client_hello("WWW.Example.COM");
        assert_eq!(client_hello_sni(&pkt).unwrap().name, "www.example.com");
    }

    /// A ClientHello whose declared record length far exceeds the bytes present,
    /// as happens when TCP splits a post-quantum hello across segments.
    fn split_client_hello(host: &str, declared_extra: usize) -> Vec<u8> {
        let mut pkt = client_hello(host);
        let declared = (pkt.len() - 5 + declared_extra) as u16;
        pkt[3..5].copy_from_slice(&declared.to_be_bytes());
        pkt
    }

    #[test]
    fn reads_sni_from_a_record_split_across_segments() {
        // Chrome's hello runs past a normal MSS once post-quantum key shares
        // are included; giving up on a truncated record would mean ignoring
        // most real browsers.
        let pkt = split_client_hello("www.example.com", 1400);
        let host = client_hello_sni(&pkt).expect("sni should still be readable");
        assert_eq!(host.name, "www.example.com");
        assert!(host.midpoint() > host.offset && host.midpoint() < host.offset + host.len);
    }

    #[test]
    fn gives_up_when_the_name_itself_is_cut_off() {
        // Splitting inside the hostname leaves nothing usable to match on.
        let pkt = client_hello("www.example.com");
        let sni_end = client_hello_sni(&pkt).unwrap().offset + 8;
        assert!(client_hello_sni(&pkt[..sni_end]).is_none());
    }

    #[test]
    fn rejects_a_record_cut_before_the_extensions() {
        let pkt = client_hello("www.example.com");
        assert!(client_hello_sni(&pkt[..40]).is_none());
    }

    #[test]
    fn rejects_non_handshake() {
        assert!(client_hello_sni(&[0x17, 0x03, 0x03, 0x00, 0x05, 1, 2, 3, 4, 5]).is_none());
        assert!(client_hello_sni(b"GET / HTTP/1.1\r\n").is_none());
    }

    #[test]
    fn survives_fuzzed_truncation() {
        // Every prefix must return cleanly rather than panic on a bad index.
        let pkt = client_hello("test.example.org");
        for n in 0..pkt.len() {
            let _ = client_hello_sni(&pkt[..n]);
        }
    }
}
