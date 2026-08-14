//! Zero-copy IPv4/IPv6 + TCP/UDP header views.
//!
//! Only what the desync engine needs: locate the payload, read and rewrite the
//! few fields we manipulate (sequence number, TTL, lengths, checksum).

pub const PROTO_TCP: u8 = 6;
pub const PROTO_UDP: u8 = 17;

pub const TCP_FIN: u8 = 0x01;
pub const TCP_SYN: u8 = 0x02;
pub const TCP_RST: u8 = 0x04;
pub const TCP_PSH: u8 = 0x08;
pub const TCP_ACK: u8 = 0x10;
pub const TCP_URG: u8 = 0x20;

const IPV4_MIN_HDR: usize = 20;
const IPV6_HDR: usize = 40;
const TCP_MIN_HDR: usize = 20;
const UDP_HDR: usize = 8;

/// Byte offsets of every part of a parsed packet.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub ipv6: bool,
    pub ip_hdr_len: usize,
    pub proto: u8,
    pub l4_off: usize,
    pub l4_hdr_len: usize,
    pub payload_off: usize,
    pub payload_len: usize,
}

impl Layout {
    pub fn payload<'a>(&self, pkt: &'a [u8]) -> &'a [u8] {
        &pkt[self.payload_off..self.payload_off + self.payload_len]
    }

    /// Total header bytes that a crafted fragment must copy verbatim.
    pub fn headers_len(&self) -> usize {
        self.ip_hdr_len + self.l4_hdr_len
    }
}

fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Parse an IP packet down to its L4 payload. Returns `None` for anything we
/// will not touch: truncated packets, non-TCP/UDP, or IP fragments after the
/// first (whose payload offsets would be meaningless).
pub fn parse(pkt: &[u8]) -> Option<Layout> {
    if pkt.is_empty() {
        return None;
    }
    match pkt[0] >> 4 {
        4 => parse_v4(pkt),
        6 => parse_v6(pkt),
        _ => None,
    }
}

fn parse_v4(pkt: &[u8]) -> Option<Layout> {
    if pkt.len() < IPV4_MIN_HDR {
        return None;
    }
    let ip_hdr_len = ((pkt[0] & 0x0f) as usize) * 4;
    if ip_hdr_len < IPV4_MIN_HDR || ip_hdr_len > pkt.len() {
        return None;
    }
    // Ignore non-initial fragments: bits 0..12 of the flags/offset word.
    if be16(pkt, 6) & 0x1fff != 0 {
        return None;
    }
    let total_len = (be16(pkt, 2) as usize).min(pkt.len());
    let proto = pkt[9];
    finish(pkt, false, ip_hdr_len, proto, total_len)
}

fn parse_v6(pkt: &[u8]) -> Option<Layout> {
    if pkt.len() < IPV6_HDR {
        return None;
    }
    let total_len = (IPV6_HDR + be16(pkt, 4) as usize).min(pkt.len());
    let mut next = pkt[6];
    let mut off = IPV6_HDR;

    // Walk extension headers so the L4 offset is right on packets that carry them.
    loop {
        match next {
            PROTO_TCP | PROTO_UDP => break,
            // Hop-by-hop, routing, destination options, mobility: [next, len]
            // where len counts 8-octet units beyond the first.
            0 | 43 | 60 | 135 => {
                if off + 2 > pkt.len() {
                    return None;
                }
                next = pkt[off];
                off += (pkt[off + 1] as usize + 1) * 8;
            }
            // Fragment header is a fixed 8 bytes.
            44 => {
                if off + 8 > pkt.len() {
                    return None;
                }
                // Non-initial fragment: offset field is bits 0..13 of byte 2..4.
                if be16(pkt, off + 2) & 0xfff8 != 0 {
                    return None;
                }
                next = pkt[off];
                off += 8;
            }
            // Authentication header: len counts 4-octet units, minus two.
            51 => {
                if off + 2 > pkt.len() {
                    return None;
                }
                next = pkt[off];
                off += (pkt[off + 1] as usize + 2) * 4;
            }
            _ => return None,
        }
        if off >= pkt.len() {
            return None;
        }
    }
    finish(pkt, true, off, next, total_len)
}

fn finish(pkt: &[u8], ipv6: bool, ip_hdr_len: usize, proto: u8, total_len: usize) -> Option<Layout> {
    let l4_off = ip_hdr_len;
    let l4_hdr_len = match proto {
        PROTO_TCP => {
            if l4_off + TCP_MIN_HDR > pkt.len() {
                return None;
            }
            let len = ((pkt[l4_off + 12] >> 4) as usize) * 4;
            if len < TCP_MIN_HDR {
                return None;
            }
            len
        }
        PROTO_UDP => {
            if l4_off + UDP_HDR > pkt.len() {
                return None;
            }
            UDP_HDR
        }
        _ => return None,
    };

    let payload_off = l4_off + l4_hdr_len;
    if payload_off > total_len || payload_off > pkt.len() {
        return None;
    }
    let payload_len = total_len - payload_off;
    if payload_off + payload_len > pkt.len() {
        return None;
    }

    Some(Layout { ipv6, ip_hdr_len, proto, l4_off, l4_hdr_len, payload_off, payload_len })
}

// --- Field accessors -------------------------------------------------------

pub fn ttl(pkt: &[u8], l: &Layout) -> u8 {
    if l.ipv6 {
        pkt[7]
    } else {
        pkt[8]
    }
}

pub fn set_ttl(pkt: &mut [u8], l: &Layout, value: u8) {
    if l.ipv6 {
        pkt[7] = value;
    } else {
        pkt[8] = value;
    }
}

pub fn tcp_seq(pkt: &[u8], l: &Layout) -> u32 {
    be32(pkt, l.l4_off + 4)
}

pub fn set_tcp_seq(pkt: &mut [u8], l: &Layout, seq: u32) {
    pkt[l.l4_off + 4..l.l4_off + 8].copy_from_slice(&seq.to_be_bytes());
}

pub fn tcp_flags(pkt: &[u8], l: &Layout) -> u8 {
    pkt[l.l4_off + 13]
}

pub fn set_tcp_flags(pkt: &mut [u8], l: &Layout, flags: u8) {
    pkt[l.l4_off + 13] = flags;
}

pub fn set_tcp_checksum(pkt: &mut [u8], l: &Layout, sum: u16) {
    pkt[l.l4_off + 16..l.l4_off + 18].copy_from_slice(&sum.to_be_bytes());
}

pub fn tcp_checksum(pkt: &[u8], l: &Layout) -> u16 {
    be16(pkt, l.l4_off + 16)
}

pub fn dst_port(pkt: &[u8], l: &Layout) -> u16 {
    be16(pkt, l.l4_off + 2)
}

pub fn src_port(pkt: &[u8], l: &Layout) -> u16 {
    be16(pkt, l.l4_off)
}

/// Rewrite the length fields after a payload has been resized. `new_payload`
/// is the payload length only; header lengths are taken from the layout.
pub fn set_payload_len(pkt: &mut [u8], l: &Layout, new_payload: usize) {
    if l.ipv6 {
        // The field counts everything after the 40-byte fixed header, which
        // includes any extension headers already accounted for in ip_hdr_len.
        let ext = l.ip_hdr_len - IPV6_HDR;
        let payload = (ext + l.l4_hdr_len + new_payload) as u16;
        pkt[4..6].copy_from_slice(&payload.to_be_bytes());
    } else {
        let total = (l.ip_hdr_len + l.l4_hdr_len + new_payload) as u16;
        pkt[2..4].copy_from_slice(&total.to_be_bytes());
    }
    if l.proto == PROTO_UDP {
        let udp_len = (UDP_HDR + new_payload) as u16;
        pkt[l.l4_off + 4..l.l4_off + 6].copy_from_slice(&udp_len.to_be_bytes());
    }
}

/// Connection identity, used to key per-flow state.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FlowKey {
    pub src_port: u16,
    pub dst_port: u16,
    pub dst: [u8; 16],
}

/// Destination address, zero-padded to 16 bytes so v4 and v6 share a key type.
pub fn dst_addr(pkt: &[u8], l: &Layout) -> [u8; 16] {
    let mut out = [0u8; 16];
    if l.ipv6 {
        out.copy_from_slice(&pkt[24..40]);
    } else {
        out[..4].copy_from_slice(&pkt[16..20]);
    }
    out
}

pub fn src_addr(pkt: &[u8], l: &Layout) -> [u8; 16] {
    let mut out = [0u8; 16];
    if l.ipv6 {
        out.copy_from_slice(&pkt[8..24]);
    } else {
        out[..4].copy_from_slice(&pkt[12..16]);
    }
    out
}

/// Render an address key back into something printable.
pub fn format_addr(addr: &[u8; 16], ipv6: bool) -> String {
    if ipv6 {
        let segs: Vec<String> = addr
            .chunks_exact(2)
            .map(|c| format!("{:x}", u16::from_be_bytes([c[0], c[1]])))
            .collect();
        segs.join(":")
    } else {
        format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3])
    }
}

pub fn flow_key(pkt: &[u8], l: &Layout) -> FlowKey {
    let mut dst = [0u8; 16];
    if l.ipv6 {
        dst.copy_from_slice(&pkt[24..40]);
    } else {
        dst[..4].copy_from_slice(&pkt[16..20]);
    }
    FlowKey { src_port: src_port(pkt, l), dst_port: dst_port(pkt, l), dst }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal IPv4 + TCP packet with `payload` bytes after a 20-byte TCP header.
    fn v4_tcp(payload: &[u8]) -> Vec<u8> {
        let total = 20 + 20 + payload.len();
        let mut pkt = vec![0u8; total];
        pkt[0] = 0x45;
        pkt[2..4].copy_from_slice(&(total as u16).to_be_bytes());
        pkt[8] = 64;
        pkt[9] = PROTO_TCP;
        pkt[20..22].copy_from_slice(&1234u16.to_be_bytes());
        pkt[22..24].copy_from_slice(&443u16.to_be_bytes());
        pkt[24..28].copy_from_slice(&0x1000_0000u32.to_be_bytes());
        pkt[32] = 0x50; // data offset 5 words
        pkt[33] = TCP_PSH | TCP_ACK;
        pkt[40..].copy_from_slice(payload);
        pkt
    }

    #[test]
    fn parses_ipv4_tcp() {
        let pkt = v4_tcp(b"hello");
        let l = parse(&pkt).expect("layout");
        assert!(!l.ipv6);
        assert_eq!(l.ip_hdr_len, 20);
        assert_eq!(l.l4_hdr_len, 20);
        assert_eq!(l.payload_off, 40);
        assert_eq!(l.payload_len, 5);
        assert_eq!(l.payload(&pkt), b"hello");
        assert_eq!(tcp_seq(&pkt, &l), 0x1000_0000);
        assert_eq!(dst_port(&pkt, &l), 443);
        assert_eq!(ttl(&pkt, &l), 64);
    }

    #[test]
    fn rewrites_lengths_and_seq() {
        let mut pkt = v4_tcp(b"hello");
        let l = parse(&pkt).unwrap();
        set_tcp_seq(&mut pkt, &l, 0x2000_0000);
        set_payload_len(&mut pkt, &l, 2);
        assert_eq!(tcp_seq(&pkt, &l), 0x2000_0000);
        assert_eq!(u16::from_be_bytes([pkt[2], pkt[3]]), 42);
    }

    #[test]
    fn rejects_non_initial_fragments() {
        let mut pkt = v4_tcp(b"hello");
        pkt[6..8].copy_from_slice(&0x0001u16.to_be_bytes());
        assert!(parse(&pkt).is_none());
    }

    #[test]
    fn rejects_truncated() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[0x45, 0, 0]).is_none());
    }
}
