//! QUIC long-header classification.
//!
//! The ClientHello inside a QUIC Initial is encrypted under keys derived from
//! the connection ID, so the hostname is not readable the way it is for TLS
//! over TCP. Shard therefore treats QUIC as a coarse-grained decision — pass,
//! drop, or decoy — and relies on browsers falling back to TCP when QUIC does
//! not establish. See `strategy::QuicMode`.

/// QUIC v1 (RFC 9000).
const VERSION_1: u32 = 0x0000_0001;
/// QUIC v2 (RFC 9369), which renumbers the long-header packet types.
const VERSION_2: u32 = 0x6b33_43cf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LongPacket {
    Initial,
    ZeroRtt,
    Handshake,
    Retry,
    VersionNegotiation,
    Unknown,
}

/// Classify a UDP payload as a QUIC long-header packet.
///
/// Returns `None` for short-header packets (an established connection) and for
/// anything that does not look like QUIC at all.
pub fn classify(payload: &[u8]) -> Option<LongPacket> {
    // Long header form bit, and the fixed bit that every QUIC version sets.
    if payload.len() < 7 || payload[0] & 0x80 == 0 {
        return None;
    }
    let version = u32::from_be_bytes([payload[1], payload[2], payload[3], payload[4]]);
    if version == 0 {
        return Some(LongPacket::VersionNegotiation);
    }

    // Connection ID lengths must fit, otherwise this is not a QUIC header.
    let dcid_len = payload[5] as usize;
    if dcid_len > 20 || payload.len() < 6 + dcid_len + 1 {
        return None;
    }
    let scid_len = payload[6 + dcid_len] as usize;
    if scid_len > 20 || payload.len() < 7 + dcid_len + scid_len {
        return None;
    }

    let type_bits = (payload[0] & 0x30) >> 4;
    Some(match version {
        VERSION_1 => match type_bits {
            0 => LongPacket::Initial,
            1 => LongPacket::ZeroRtt,
            2 => LongPacket::Handshake,
            3 => LongPacket::Retry,
            _ => LongPacket::Unknown,
        },
        VERSION_2 => match type_bits {
            1 => LongPacket::Initial,
            2 => LongPacket::ZeroRtt,
            3 => LongPacket::Handshake,
            0 => LongPacket::Retry,
            _ => LongPacket::Unknown,
        },
        // A version we do not know, but structurally a long header: treat it as
        // connection setup, which is what the QUIC policy cares about.
        _ => LongPacket::Unknown,
    })
}

/// True when this datagram is starting a new QUIC connection, which is the
/// only point where blocking QUIC forces a fallback to TCP.
pub fn is_connection_start(payload: &[u8]) -> bool {
    matches!(classify(payload), Some(LongPacket::Initial) | Some(LongPacket::Unknown))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long_header(first: u8, version: u32, dcid: &[u8], scid: &[u8]) -> Vec<u8> {
        let mut p = vec![first];
        p.extend_from_slice(&version.to_be_bytes());
        p.push(dcid.len() as u8);
        p.extend_from_slice(dcid);
        p.push(scid.len() as u8);
        p.extend_from_slice(scid);
        p.extend_from_slice(&[0u8; 32]);
        p
    }

    #[test]
    fn detects_v1_initial() {
        let p = long_header(0xc0, VERSION_1, &[1, 2, 3, 4, 5, 6, 7, 8], &[9, 9, 9, 9]);
        assert_eq!(classify(&p), Some(LongPacket::Initial));
        assert!(is_connection_start(&p));
    }

    #[test]
    fn detects_v2_initial_renumbering() {
        let p = long_header(0xd0, VERSION_2, &[1, 2, 3, 4], &[]);
        assert_eq!(classify(&p), Some(LongPacket::Initial));
    }

    #[test]
    fn ignores_short_header() {
        let p = long_header(0x40, VERSION_1, &[1, 2, 3, 4], &[]);
        assert_eq!(classify(&p), None);
    }

    #[test]
    fn detects_version_negotiation() {
        let p = long_header(0xc0, 0, &[1, 2], &[3, 4]);
        assert_eq!(classify(&p), Some(LongPacket::VersionNegotiation));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(classify(&[]), None);
        assert_eq!(classify(&[0xc0, 0, 0, 0, 1, 200]), None);
        for n in 0..40usize {
            let _ = classify(&vec![0xc0u8; n]);
        }
    }
}
