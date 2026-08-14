//! Turns a `Strategy` into the concrete packets to inject.
//!
//! Nothing here talks to the driver; it is pure buffer manipulation so the
//! whole engine is testable without loading a kernel driver.

use crate::net::{self, Layout};
use crate::parse::http::HostHeader;
use crate::strategy::{Fooling, QuicMode, SplitAt, Strategy};

/// A sequence number offset far outside any plausible receive window: the
/// server drops it, a stateless middlebox still parses the payload.
const BAD_SEQ_OFFSET: u32 = 100_000;

/// What the connection is carrying, so the decoy can look like the same thing.
///
/// A TLS ClientHello sent to a plain-HTTP server is parsed as a request line
/// and answered with `400 Bad Request`, which the browser then shows instead of
/// the site. The decoy has to speak the protocol it is imitating.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Protocol {
    Tls,
    Http,
}

/// One packet to inject.
pub struct Emit {
    pub bytes: Vec<u8>,
    /// Corrupt the TCP checksum *after* the driver recomputes it. It cannot be
    /// baked in beforehand because `WinDivertHelperCalcChecksums` would just
    /// overwrite the bad value.
    pub corrupt_checksum: bool,
}

/// What to do with a QUIC datagram.
pub enum QuicAction {
    Pass,
    Drop,
    /// Send this decoy first, then let the original through.
    Decoy(Emit),
}

/// Build the injection plan for a TCP segment.
///
/// `payload` may differ from the packet's own payload when HTTP headers were
/// mangled. `split_hint` is the preferred cut offset, normally the middle of
/// the hostname. Returns `None` when the strategy would not change anything,
/// so the caller can pass the original packet through untouched.
pub fn plan_tcp(
    pkt: &[u8],
    l: &Layout,
    payload: &[u8],
    split_hint: Option<usize>,
    s: &Strategy,
    hops: Option<u8>,
    protocol: Protocol,
) -> Option<Vec<Emit>> {
    if payload.is_empty() {
        return None;
    }
    let points = split_points(payload.len(), split_hint, s);
    let rewritten = payload != l.payload(pkt);

    let ttl = decoy_ttl(s, hops);
    // A decoy that reaches the server corrupts the stream instead of protecting
    // it, so drop it rather than guess a distance. Only TTL fooling depends on
    // knowing how far away the server is.
    let send_decoy = s.desync.uses_fake() && (s.fooling != Fooling::Ttl || ttl.is_some());

    if points.is_empty() && !send_decoy && !rewritten {
        return None;
    }

    let base_seq = net::tcp_seq(pkt, l);
    let mut emits = Vec::with_capacity(points.len() + 1 + s.fake_repeats as usize);

    if send_decoy {
        let decoy = build_decoy(protocol, &s.decoy_host, payload.len());
        for _ in 0..s.fake_repeats.max(1) {
            let mut bytes = segment(pkt, l, &decoy, base_seq);
            let mut corrupt_checksum = false;
            match s.fooling {
                Fooling::Ttl => net::set_ttl(&mut bytes, l, ttl.unwrap_or(1)),
                Fooling::BadSum => corrupt_checksum = true,
                Fooling::BadSeq => {
                    net::set_tcp_seq(&mut bytes, l, base_seq.wrapping_sub(BAD_SEQ_OFFSET))
                }
            }
            emits.push(Emit { bytes, corrupt_checksum });
        }
    }

    let mut real: Vec<Emit> = ranges(payload.len(), &points)
        .into_iter()
        .map(|r| Emit {
            bytes: segment(pkt, l, &payload[r.clone()], base_seq.wrapping_add(r.start as u32)),
            corrupt_checksum: false,
        })
        .collect();
    if s.desync.reverses() {
        real.reverse();
    }
    emits.extend(real);

    Some(emits)
}

/// Decide what happens to a QUIC datagram. QUIC's ClientHello is encrypted, so
/// this is a per-connection decision rather than a per-hostname one.
pub fn plan_quic(pkt: &[u8], l: &Layout, s: &Strategy, hops: Option<u8>) -> QuicAction {
    match s.quic {
        QuicMode::Pass => QuicAction::Pass,
        QuicMode::Drop => QuicAction::Drop,
        QuicMode::Decoy => {
            // Same rule as TCP: without a measured distance the decoy could
            // arrive, and an unexpected datagram is worse than none.
            let Some(ttl) = decoy_ttl(s, hops) else { return QuicAction::Pass };
            let len = l.payload_len.min(1200);
            // Random-looking filler: an Initial's body is AEAD ciphertext, so
            // anything structured would look less like QUIC, not more.
            let filler: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(7)).collect();
            let mut bytes = segment_udp(pkt, l, &filler);
            net::set_ttl(&mut bytes, l, ttl);
            QuicAction::Decoy(Emit { bytes, corrupt_checksum: false })
        }
    }
}

/// Apply the plain-HTTP header tweaks. Returns the new payload and the adjusted
/// hostname offset, or `None` when nothing was enabled.
pub fn mangle_http(payload: &[u8], header: &HostHeader, s: &Strategy) -> Option<(Vec<u8>, usize)> {
    if !s.http_host_case && !s.http_host_space {
        return None;
    }
    let mut out = payload.to_vec();
    if s.http_host_case {
        // "Host" -> "hOsT". RFC 7230 makes field names case-insensitive, so
        // only a matcher that compares raw bytes is affected.
        const MANGLED: &[u8; 4] = b"hOsT";
        out[header.name_offset..header.name_offset + 4].copy_from_slice(MANGLED);
    }
    let mut host_offset = header.host.offset;
    if s.http_host_space {
        out.insert(header.separator_offset, b' ');
        host_offset += 1;
    }
    Some((out, host_offset))
}

/// TTL for the decoy: one hop short of the server, so it passes every middlebox
/// on the path but never arrives.
///
/// `None` means the distance is unknown and the caller must not send a decoy.
/// A guessed value that turns out to be too large delivers the decoy to the
/// server, which then treats the real request as a retransmission and drops it
/// — the connection breaks in a way that looks nothing like a block.
fn decoy_ttl(s: &Strategy, hops: Option<u8>) -> Option<u8> {
    if s.auto_ttl {
        return hops.map(|h| {
            let derived = h.saturating_sub(s.auto_ttl_delta);
            // The cap is what protects against asymmetric routing, where the
            // measured return path is longer than the path out.
            derived.min(s.auto_ttl_cap.max(1)).max(1)
        });
    }
    Some(s.fake_ttl.max(1))
}

/// A decoy that looks like whatever the real connection is carrying.
fn build_decoy(protocol: Protocol, host: &str, target_len: usize) -> Vec<u8> {
    match protocol {
        Protocol::Tls => build_client_hello(host, target_len),
        Protocol::Http => build_http_request(host, target_len),
    }
}

/// A well-formed HTTP request for `host`, padded to `target_len`.
///
/// Padding rides in a custom header so the request stays valid: a server that
/// does receive it answers normally instead of rejecting the connection.
pub fn build_http_request(host: &str, target_len: usize) -> Vec<u8> {
    let base = format!(
        "GET / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Mozilla/5.0\r\nAccept: */*\r\n"
    );
    const PAD_HEADER: &str = "X-Pad: ";
    let tail = "\r\n";
    let minimum = base.len() + tail.len();

    let mut out = base;
    if target_len > minimum + PAD_HEADER.len() + 2 {
        let width = target_len - minimum - PAD_HEADER.len() - 2;
        out.push_str(PAD_HEADER);
        out.extend(std::iter::repeat_n('0', width));
        out.push_str("\r\n");
    }
    out.push_str(tail);
    out.into_bytes()
}

fn split_points(len: usize, hint: Option<usize>, s: &Strategy) -> Vec<usize> {
    if !s.desync.splits() || len < 2 {
        return Vec::new();
    }
    let primary = match s.split_at {
        SplitAt::HostMidpoint => hint.unwrap_or(s.fixed_split_offset as usize),
        SplitAt::RecordHeader => 5,
        SplitAt::Fixed => s.fixed_split_offset as usize,
    };
    let mut points = vec![primary.clamp(1, len - 1)];
    let extra = s.extra_splits as usize;
    for i in 1..=extra {
        points.push((len * i / (extra + 1)).clamp(1, len - 1));
    }
    points.sort_unstable();
    points.dedup();
    points
}

fn ranges(len: usize, points: &[usize]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::with_capacity(points.len() + 1);
    let mut start = 0usize;
    for &p in points {
        if p > start {
            out.push(start..p);
            start = p;
        }
    }
    if start < len {
        out.push(start..len);
    }
    out
}

/// Copy the packet's headers verbatim and attach a new payload.
fn segment(pkt: &[u8], l: &Layout, payload: &[u8], seq: u32) -> Vec<u8> {
    let hdr = l.headers_len();
    let mut out = Vec::with_capacity(hdr + payload.len());
    out.extend_from_slice(&pkt[..hdr]);
    out.extend_from_slice(payload);
    net::set_tcp_seq(&mut out, l, seq);
    net::set_payload_len(&mut out, l, payload.len());
    out
}

fn segment_udp(pkt: &[u8], l: &Layout, payload: &[u8]) -> Vec<u8> {
    let hdr = l.headers_len();
    let mut out = Vec::with_capacity(hdr + payload.len());
    out.extend_from_slice(&pkt[..hdr]);
    out.extend_from_slice(payload);
    net::set_payload_len(&mut out, l, payload.len());
    out
}

fn push_ext(out: &mut Vec<u8>, ext_type: u16, body: &[u8]) {
    out.extend_from_slice(&ext_type.to_be_bytes());
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(body);
}

/// Build a ClientHello advertising `host`, padded to `target_len` when it fits.
///
/// Used for two things. As a decoy it has to look real: random filler would be
/// ignored by any middlebox that validates structure before caching a hostname,
/// and the padding makes it occupy exactly the sequence range the real payload
/// will reuse. The prober sends the same thing for real, to find out whether a
/// given strategy actually gets a handshake through.
pub fn build_client_hello(host: &str, target_len: usize) -> Vec<u8> {
    let host = host.as_bytes();
    let base = assemble_client_hello(host, 0);
    // The padding extension costs 4 bytes of header before any filler.
    if target_len > base.len() + 4 {
        assemble_client_hello(host, target_len - base.len() - 4)
    } else {
        base
    }
}

/// A ClientHello with `server_name` placed *after* a large padding extension,
/// so on the wire it lands beyond the first TCP segment.
///
/// Chrome randomises extension order and its hello exceeds a normal MSS, so
/// this arrangement happens in practice — and it is the case where the
/// hostname simply cannot be read from the opening segment. Used by the probe
/// to reproduce it deliberately rather than waiting to be surprised by it.
pub fn build_client_hello_sni_last(host: &str, target_len: usize) -> Vec<u8> {
    let host = host.as_bytes();
    let base = assemble_client_hello_ordered(host, 0, true);
    if target_len > base.len() + 4 {
        assemble_client_hello_ordered(host, target_len - base.len() - 4, true)
    } else {
        base
    }
}

fn assemble_client_hello(host: &[u8], padding: usize) -> Vec<u8> {
    assemble_client_hello_ordered(host, padding, false)
}

fn assemble_client_hello_ordered(host: &[u8], padding: usize, padding_first: bool) -> Vec<u8> {
    let mut server_name = Vec::with_capacity(host.len() + 5);
    server_name.extend_from_slice(&((host.len() + 3) as u16).to_be_bytes());
    server_name.push(0); // host_name
    server_name.extend_from_slice(&(host.len() as u16).to_be_bytes());
    server_name.extend_from_slice(host);

    let mut extensions = Vec::new();
    if padding_first && padding > 0 {
        push_ext(&mut extensions, 0x0015, &vec![0u8; padding]);
    }
    push_ext(&mut extensions, 0x0000, &server_name);
    // supported_versions announcing TLS 1.3, so a modern server answers rather
    // than rejecting the hello outright when the prober sends this for real.
    push_ext(&mut extensions, 0x002b, &[0x02, 0x03, 0x04]);
    if !padding_first && padding > 0 {
        push_ext(&mut extensions, 0x0015, &vec![0u8; padding]);
    }

    let mut body = Vec::with_capacity(extensions.len() + 48);
    body.extend_from_slice(&[0x03, 0x03]); // legacy_version
    body.extend_from_slice(&[0x5a; 32]); // random
    body.push(0); // empty session id
    body.extend_from_slice(&4u16.to_be_bytes());
    body.extend_from_slice(&[0x13, 0x01]); // TLS_AES_128_GCM_SHA256
    body.extend_from_slice(&[0xc0, 0x2f]); // ECDHE_RSA_WITH_AES_128_GCM_SHA256
    body.push(1);
    body.push(0); // null compression
    body.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
    body.extend_from_slice(&extensions);

    let mut handshake = Vec::with_capacity(body.len() + 4);
    handshake.push(0x01); // client_hello
    let len = body.len() as u32;
    handshake.extend_from_slice(&[(len >> 16) as u8, (len >> 8) as u8, len as u8]);
    handshake.extend_from_slice(&body);

    let mut record = Vec::with_capacity(handshake.len() + 5);
    record.extend_from_slice(&[0x16, 0x03, 0x01]);
    record.extend_from_slice(&(handshake.len() as u16).to_be_bytes());
    record.extend_from_slice(&handshake);
    record
}

/// Estimate hops to a host from the TTL of a packet it sent us. Stacks start at
/// 64, 128 or 255, so rounding up to the next of those recovers the distance.
pub fn hops_from_ttl(observed: u8) -> Option<u8> {
    let initial = [64u8, 128, 255].into_iter().find(|&i| observed <= i)?;
    let hops = initial - observed;
    // A path longer than 40 hops means our guess of the initial value was wrong.
    (hops <= 40).then_some(hops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::parse;
    use crate::strategy::Desync;

    fn v4_tcp(payload: &[u8]) -> Vec<u8> {
        let total = 20 + 20 + payload.len();
        let mut pkt = vec![0u8; total];
        pkt[0] = 0x45;
        pkt[2..4].copy_from_slice(&(total as u16).to_be_bytes());
        pkt[8] = 64;
        pkt[9] = net::PROTO_TCP;
        pkt[20..22].copy_from_slice(&40000u16.to_be_bytes());
        pkt[22..24].copy_from_slice(&443u16.to_be_bytes());
        pkt[24..28].copy_from_slice(&1000u32.to_be_bytes());
        pkt[32] = 0x50;
        pkt[33] = net::TCP_PSH | net::TCP_ACK;
        pkt[40..].copy_from_slice(payload);
        pkt
    }

    fn strategy(desync: Desync) -> Strategy {
        Strategy { desync, auto_ttl: false, fake_ttl: 5, ..Default::default() }
    }

    #[test]
    fn split_covers_payload_exactly_once() {
        let payload: Vec<u8> = (0..100u8).collect();
        let pkt = v4_tcp(&payload);
        let l = parse(&pkt).unwrap();
        let emits = plan_tcp(&pkt, &l, &payload, Some(40), &strategy(Desync::Split), None, Protocol::Tls).unwrap();
        assert_eq!(emits.len(), 2);

        let mut rebuilt = Vec::new();
        for e in &emits {
            let el = parse(&e.bytes).unwrap();
            rebuilt.extend_from_slice(el.payload(&e.bytes));
        }
        assert_eq!(rebuilt, payload);
    }

    #[test]
    fn split_sequence_numbers_are_contiguous() {
        let payload: Vec<u8> = (0..100u8).collect();
        let pkt = v4_tcp(&payload);
        let l = parse(&pkt).unwrap();
        let emits = plan_tcp(&pkt, &l, &payload, Some(40), &strategy(Desync::Split), None, Protocol::Tls).unwrap();

        let first = parse(&emits[0].bytes).unwrap();
        let second = parse(&emits[1].bytes).unwrap();
        assert_eq!(net::tcp_seq(&emits[0].bytes, &first), 1000);
        assert_eq!(net::tcp_seq(&emits[1].bytes, &second), 1000 + 40);
        assert_eq!(first.payload_len, 40);
        assert_eq!(second.payload_len, 60);
    }

    #[test]
    fn disorder_reverses_transmission_but_not_sequence() {
        let payload: Vec<u8> = (0..100u8).collect();
        let pkt = v4_tcp(&payload);
        let l = parse(&pkt).unwrap();
        let emits = plan_tcp(&pkt, &l, &payload, Some(40), &strategy(Desync::Disorder), None, Protocol::Tls).unwrap();

        let first = parse(&emits[0].bytes).unwrap();
        // The tail goes out first, carrying the higher sequence number.
        assert_eq!(net::tcp_seq(&emits[0].bytes, &first), 1040);
        assert_eq!(first.payload_len, 60);
    }

    #[test]
    fn decoy_precedes_real_data_and_expires_early() {
        let payload: Vec<u8> = (0..200u8).collect();
        let pkt = v4_tcp(&payload);
        let l = parse(&pkt).unwrap();
        let s = Strategy { fake_repeats: 2, ..strategy(Desync::FakeSplit) };
        let emits = plan_tcp(&pkt, &l, &payload, Some(80), &s, None, Protocol::Tls).unwrap();

        assert_eq!(emits.len(), 2 + 2);
        for decoy in &emits[..2] {
            let dl = parse(&decoy.bytes).unwrap();
            assert_eq!(net::ttl(&decoy.bytes, &dl), 5);
            assert_eq!(net::tcp_seq(&decoy.bytes, &dl), 1000);
            assert_eq!(dl.payload(&decoy.bytes)[0], 0x16, "decoy must look like a handshake");
        }
        // Real fragments keep the original TTL.
        let real = parse(&emits[2].bytes).unwrap();
        assert_eq!(net::ttl(&emits[2].bytes, &real), 64);
    }

    #[test]
    fn decoy_advertises_the_configured_host() {
        let hello = build_client_hello("www.iana.org", 0);
        let host = crate::parse::tls::client_hello_sni(&hello).expect("parseable decoy");
        assert_eq!(host.name, "www.iana.org");
    }

    #[test]
    fn decoy_is_padded_to_match_the_real_payload() {
        for target in [200usize, 517, 1200] {
            let hello = build_client_hello("example.com", target);
            assert_eq!(hello.len(), target, "padding should hit the target exactly");
            assert!(crate::parse::tls::client_hello_sni(&hello).is_some());
        }
    }

    #[test]
    fn bad_sum_and_bad_seq_are_marked_not_baked() {
        let payload: Vec<u8> = (0..100u8).collect();
        let pkt = v4_tcp(&payload);
        let l = parse(&pkt).unwrap();

        let s = Strategy { fooling: Fooling::BadSum, ..strategy(Desync::Fake) };
        let emits = plan_tcp(&pkt, &l, &payload, None, &s, None, Protocol::Tls).unwrap();
        assert!(emits[0].corrupt_checksum);

        let s = Strategy { fooling: Fooling::BadSeq, ..strategy(Desync::Fake) };
        let emits = plan_tcp(&pkt, &l, &payload, None, &s, None, Protocol::Tls).unwrap();
        let dl = parse(&emits[0].bytes).unwrap();
        assert_eq!(net::tcp_seq(&emits[0].bytes, &dl), 1000u32.wrapping_sub(BAD_SEQ_OFFSET));
        assert!(!emits[0].corrupt_checksum);
    }

    #[test]
    fn passthrough_strategy_plans_nothing() {
        let payload: Vec<u8> = (0..50u8).collect();
        let pkt = v4_tcp(&payload);
        let l = parse(&pkt).unwrap();
        assert!(plan_tcp(&pkt, &l, &payload, None, &Strategy::passthrough(), None, Protocol::Tls).is_none());
    }

    #[test]
    fn extra_splits_produce_more_fragments() {
        let payload: Vec<u8> = (0..=255u8).collect();
        let pkt = v4_tcp(&payload);
        let l = parse(&pkt).unwrap();
        let s = Strategy { extra_splits: 3, ..strategy(Desync::Split) };
        let emits = plan_tcp(&pkt, &l, &payload, Some(128), &s, None, Protocol::Tls).unwrap();
        assert!(emits.len() >= 4);

        let mut rebuilt = Vec::new();
        let mut ordered: Vec<_> = emits.iter().collect();
        ordered.sort_by_key(|e| {
            let el = parse(&e.bytes).unwrap();
            net::tcp_seq(&e.bytes, &el)
        });
        for e in ordered {
            let el = parse(&e.bytes).unwrap();
            rebuilt.extend_from_slice(el.payload(&e.bytes));
        }
        assert_eq!(rebuilt, payload);
    }

    #[test]
    fn auto_ttl_is_capped_against_asymmetric_routing() {
        // A 14-hop return path does not mean the way out is 14 hops. Trusting
        // it sends the decoy all the way to the server.
        let s = Strategy { auto_ttl: true, auto_ttl_delta: 1, auto_ttl_cap: 8, ..Default::default() };
        assert_eq!(decoy_ttl(&s, Some(14)), Some(8), "the cap must win over the measurement");
        // A short path is still used as measured.
        assert_eq!(decoy_ttl(&s, Some(6)), Some(5));
    }

    #[test]
    fn auto_ttl_stops_one_hop_short() {
        let s = Strategy { auto_ttl: true, auto_ttl_delta: 1, auto_ttl_cap: 64, ..Default::default() };
        assert_eq!(decoy_ttl(&s, Some(12)), Some(11));
        // Never underflows to zero, which the stack would reject.
        assert_eq!(decoy_ttl(&s, Some(0)), Some(1));
        // Unknown distance yields no TTL at all rather than a guess.
        assert_eq!(decoy_ttl(&s, None), None);
        // With auto measurement off, the fixed value is used as configured.
        let fixed = Strategy { auto_ttl: false, fake_ttl: 5, ..Default::default() };
        assert_eq!(decoy_ttl(&fixed, None), Some(5));
    }

    #[test]
    fn no_decoy_is_sent_when_the_distance_is_unknown() {
        // A decoy that reaches the server makes the real request look like a
        // retransmission and the connection breaks — worse than not trying.
        let payload: Vec<u8> = (0..200u8).collect();
        let pkt = v4_tcp(&payload);
        let l = parse(&pkt).unwrap();
        let s = Strategy { desync: Desync::FakeSplit, fooling: Fooling::Ttl, auto_ttl: true, ..Default::default() };

        let without = plan_tcp(&pkt, &l, &payload, Some(80), &s, None, Protocol::Tls).unwrap();
        let with = plan_tcp(&pkt, &l, &payload, Some(80), &s, Some(12), Protocol::Tls).unwrap();
        assert_eq!(without.len(), 2, "fragments only");
        assert_eq!(with.len(), 3, "decoy plus fragments");
    }

    #[test]
    fn checksum_fooling_does_not_need_a_measured_distance() {
        // Only TTL fooling depends on knowing how far away the server is.
        let payload: Vec<u8> = (0..100u8).collect();
        let pkt = v4_tcp(&payload);
        let l = parse(&pkt).unwrap();
        let s = Strategy { desync: Desync::Fake, fooling: Fooling::BadSum, auto_ttl: true, ..Default::default() };
        let emits = plan_tcp(&pkt, &l, &payload, None, &s, None, Protocol::Tls).unwrap();
        assert!(emits[0].corrupt_checksum);
    }

    #[test]
    fn an_http_target_gets_an_http_decoy() {
        // A ClientHello sent to a plain-HTTP server comes back as 400 Bad
        // Request, which the browser shows instead of the site.
        let payload = b"GET / HTTP/1.1\r\nHost: blocked.example\r\n\r\n".to_vec();
        let mut pkt = v4_tcp(&payload);
        pkt[22..24].copy_from_slice(&80u16.to_be_bytes());
        let l = parse(&pkt).unwrap();
        let s = Strategy { desync: Desync::Fake, auto_ttl: false, fake_ttl: 5, ..Default::default() };

        let emits = plan_tcp(&pkt, &l, &payload, None, &s, None, Protocol::Http).unwrap();
        let decoy = parse(&emits[0].bytes).unwrap();
        let body = decoy.payload(&emits[0].bytes);
        assert!(body.starts_with(b"GET "), "decoy must be a request, got {:?}", &body[..8.min(body.len())]);
        assert_ne!(body[0], 0x16, "must not be a TLS record");
        assert!(crate::parse::http::host_header(body).is_some(), "decoy must parse as HTTP");
    }

    #[test]
    fn http_decoy_pads_to_the_target_length() {
        for target in [120usize, 400, 900] {
            let request = build_http_request("www.iana.org", target);
            assert_eq!(request.len(), target, "padding should hit the target exactly");
            let header = crate::parse::http::host_header(&request).expect("valid request");
            assert_eq!(header.host.name, "www.iana.org");
        }
    }

    #[test]
    fn http_decoy_stays_valid_when_it_cannot_be_padded() {
        let request = build_http_request("www.iana.org", 10);
        assert!(crate::parse::http::host_header(&request).is_some());
        assert!(request.ends_with(b"\r\n\r\n"));
    }

    #[test]
    fn hop_estimate_handles_each_initial_ttl() {
        assert_eq!(hops_from_ttl(52), Some(12)); // 64 - 52
        assert_eq!(hops_from_ttl(120), Some(8)); // 128 - 120
        assert_eq!(hops_from_ttl(245), Some(10)); // 255 - 245
        assert_eq!(hops_from_ttl(64), Some(0));
        assert_eq!(hops_from_ttl(1), None); // implausible distance
    }

    #[test]
    fn http_mangling_shifts_the_host_offset() {
        let req = b"GET / HTTP/1.1\r\nHost: blocked.example\r\n\r\n";
        let header = crate::parse::http::host_header(req).unwrap();
        let s = Strategy { http_host_case: true, http_host_space: true, ..Default::default() };
        let (out, offset) = mangle_http(req, &header, &s).unwrap();

        assert_eq!(&out[header.name_offset..header.name_offset + 4], b"hOsT");
        assert_eq!(&out[offset..offset + header.host.len], b"blocked.example");
        assert_eq!(out.len(), req.len() + 1);
    }

    #[test]
    fn quic_modes_map_to_actions() {
        let payload = vec![0xc0u8; 100];
        let total = 28 + payload.len();
        let mut pkt = vec![0u8; total];
        pkt[0] = 0x45;
        pkt[2..4].copy_from_slice(&(total as u16).to_be_bytes());
        pkt[8] = 64;
        pkt[9] = net::PROTO_UDP;
        pkt[22..24].copy_from_slice(&443u16.to_be_bytes());
        pkt[24..26].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        pkt[28..].copy_from_slice(&payload);
        let l = parse(&pkt).unwrap();

        assert!(matches!(
            plan_quic(&pkt, &l, &Strategy { quic: QuicMode::Pass, ..Default::default() }, None),
            QuicAction::Pass
        ));
        assert!(matches!(
            plan_quic(&pkt, &l, &Strategy { quic: QuicMode::Drop, ..Default::default() }, None),
            QuicAction::Drop
        ));
        let s = Strategy { quic: QuicMode::Decoy, auto_ttl: false, fake_ttl: 4, ..Default::default() };
        match plan_quic(&pkt, &l, &s, None) {
            QuicAction::Decoy(e) => {
                let el = parse(&e.bytes).unwrap();
                assert_eq!(net::ttl(&e.bytes, &el), 4);
                assert_eq!(el.payload_len, payload.len());
            }
            _ => panic!("expected a decoy"),
        }
    }
}
