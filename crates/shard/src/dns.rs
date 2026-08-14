//! Just enough DNS wire format to see which addresses a name resolved to.
//!
//! The forwarder itself relays queries verbatim — RFC 8484 carries the same
//! wire format over HTTPS, so no rewriting is needed. Parsing exists only to
//! build the address-to-hostname map that lets QUIC, which hides its SNI, still
//! be matched against per-domain policy.

pub const TYPE_A: u16 = 1;
pub const TYPE_AAAA: u16 = 28;

const HEADER_LEN: usize = 12;
/// Compression pointers can chain; anything deeper is a malformed or hostile
/// message trying to make us loop.
const MAX_POINTER_HOPS: usize = 16;

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

/// Read a possibly compressed name. Returns the name and the offset just past
/// it in the original position (not past the pointer target).
fn read_name(buf: &[u8], start: usize) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut at = start;
    let mut after = None;
    let mut hops = 0usize;

    loop {
        let len = *buf.get(at)?;
        if len & 0xc0 == 0xc0 {
            let target = ((len as usize & 0x3f) << 8) | *buf.get(at + 1)? as usize;
            after.get_or_insert(at + 2);
            hops += 1;
            if hops > MAX_POINTER_HOPS || target >= buf.len() {
                return None;
            }
            at = target;
            continue;
        }
        if len == 0 {
            after.get_or_insert(at + 1);
            break;
        }
        let end = at + 1 + len as usize;
        let label = buf.get(at + 1..end)?;
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(&String::from_utf8_lossy(label));
        at = end;
    }
    Some((out.to_ascii_lowercase(), after.unwrap_or(at)))
}

/// Build a standard recursive query.
pub fn build_query(name: &str, qtype: u16, id: u16) -> Vec<u8> {
    let mut msg = Vec::with_capacity(name.len() + 18);
    msg.extend_from_slice(&id.to_be_bytes());
    msg.extend_from_slice(&0x0100u16.to_be_bytes()); // standard query, recursion desired
    msg.extend_from_slice(&1u16.to_be_bytes()); // one question
    msg.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // no answer, authority or additional
    for label in name.trim_end_matches('.').split('.') {
        let bytes = label.as_bytes();
        let len = bytes.len().min(63); // labels are capped at 63 octets
        msg.push(len as u8);
        msg.extend_from_slice(&bytes[..len]);
    }
    msg.push(0); // root label
    msg.extend_from_slice(&qtype.to_be_bytes());
    msg.extend_from_slice(&1u16.to_be_bytes()); // class IN
    msg
}

/// The question name of a query or response.
pub fn question_name(msg: &[u8]) -> Option<String> {
    if msg.len() < HEADER_LEN || be16(msg, 4)? == 0 {
        return None;
    }
    read_name(msg, HEADER_LEN).map(|(name, _)| name)
}

/// Extract the question name and every A/AAAA address in the answer section,
/// each widened to 16 bytes so v4 and v6 share one key type.
pub fn answer_addresses(msg: &[u8]) -> Option<(String, Vec<[u8; 16]>)> {
    if msg.len() < HEADER_LEN {
        return None;
    }
    let qd = be16(msg, 4)?;
    let an = be16(msg, 6)?;
    if qd == 0 {
        return None;
    }

    let (name, mut at) = read_name(msg, HEADER_LEN)?;
    at += 4; // qtype + qclass
    // Additional questions are legal but vanishingly rare; skip them properly.
    for _ in 1..qd {
        let (_, next) = read_name(msg, at)?;
        at = next + 4;
    }

    let mut addrs = Vec::new();
    for _ in 0..an {
        let (_, next) = read_name(msg, at)?;
        at = next;
        let rtype = be16(msg, at)?;
        let rdlen = be16(msg, at + 8)? as usize;
        at += 10;
        let rdata = msg.get(at..at + rdlen)?;
        match (rtype, rdlen) {
            (TYPE_A, 4) => {
                // IPv4-mapped, so a caller can tell an A record from an AAAA
                // one and dial it. Putting the four bytes at the front instead
                // reads back as an unrelated IPv6 address, which fails as
                // "network unreachable" a long way from here.
                let mut a = [0u8; 16];
                a[10] = 0xff;
                a[11] = 0xff;
                a[12..].copy_from_slice(rdata);
                addrs.push(a);
            }
            (TYPE_AAAA, 16) => {
                let mut a = [0u8; 16];
                a.copy_from_slice(rdata);
                addrs.push(a);
            }
            _ => {}
        }
        at += rdlen;
    }
    Some((name, addrs))
}

/// Build a SERVFAIL reply for a query we could not forward, so the client fails
/// fast instead of waiting out its own timeout.
pub fn servfail(query: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; HEADER_LEN];
    let n = query.len().min(HEADER_LEN);
    out[..n].copy_from_slice(&query[..n]);
    out[2] = 0x80; // QR = response
    out[3] = 0x02; // RCODE = SERVFAIL
    out[6..12].fill(0); // no answer, authority or additional records
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_name(name: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for label in name.split('.') {
            out.push(label.len() as u8);
            out.extend_from_slice(label.as_bytes());
        }
        out.push(0);
        out
    }

    fn response(name: &str, records: &[(u16, &[u8])]) -> Vec<u8> {
        let mut msg = vec![0x12, 0x34, 0x81, 0x80];
        msg.extend_from_slice(&1u16.to_be_bytes()); // qdcount
        msg.extend_from_slice(&(records.len() as u16).to_be_bytes());
        msg.extend_from_slice(&0u16.to_be_bytes());
        msg.extend_from_slice(&0u16.to_be_bytes());
        msg.extend_from_slice(&encode_name(name));
        msg.extend_from_slice(&1u16.to_be_bytes()); // qtype A
        msg.extend_from_slice(&1u16.to_be_bytes()); // qclass IN
        for (rtype, rdata) in records {
            msg.extend_from_slice(&[0xc0, 0x0c]); // pointer back to the question
            msg.extend_from_slice(&rtype.to_be_bytes());
            msg.extend_from_slice(&1u16.to_be_bytes());
            msg.extend_from_slice(&300u32.to_be_bytes());
            msg.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
            msg.extend_from_slice(rdata);
        }
        msg
    }

    /// An A record must come back as an address that can actually be dialled.
    #[test]
    fn extracts_a_records_as_ipv4_mapped() {
        let msg = response("www.example.com", &[(TYPE_A, &[93, 184, 216, 34])]);
        let (name, addrs) = answer_addresses(&msg).unwrap();
        assert_eq!(name, "www.example.com");
        assert_eq!(addrs.len(), 1);

        let address = std::net::Ipv6Addr::from(addrs[0]);
        assert_eq!(
            address.to_ipv4_mapped(),
            Some(std::net::Ipv4Addr::new(93, 184, 216, 34)),
            "an A record read back as {address} would be unreachable"
        );
    }

    #[test]
    fn extracts_aaaa_records() {
        let v6 = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        let msg = response("ipv6.example", &[(TYPE_AAAA, &v6)]);
        let (_, addrs) = answer_addresses(&msg).unwrap();
        assert_eq!(addrs[0], v6);
    }

    #[test]
    fn skips_record_types_we_do_not_care_about() {
        let cname = encode_name("target.example.com");
        let msg = response(
            "www.example.com",
            &[(5, &cname), (TYPE_A, &[1, 2, 3, 4])],
        );
        let (_, addrs) = answer_addresses(&msg).unwrap();
        assert_eq!(addrs.len(), 1);
        assert_eq!(
            std::net::Ipv6Addr::from(addrs[0]).to_ipv4_mapped(),
            Some(std::net::Ipv4Addr::new(1, 2, 3, 4))
        );
    }

    #[test]
    fn reads_the_question_name() {
        let msg = response("api.test.co.kr", &[]);
        assert_eq!(question_name(&msg).as_deref(), Some("api.test.co.kr"));
    }

    #[test]
    fn rejects_compression_loops() {
        // A pointer to itself must terminate rather than spin.
        let mut msg = vec![0x12, 0x34, 0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0];
        msg.extend_from_slice(&[0xc0, 0x0c]);
        assert!(answer_addresses(&msg).is_none());
    }

    #[test]
    fn survives_arbitrary_truncation() {
        let msg = response("www.example.com", &[(TYPE_A, &[93, 184, 216, 34])]);
        for n in 0..msg.len() {
            let _ = answer_addresses(&msg[..n]);
            let _ = question_name(&msg[..n]);
        }
    }

    #[test]
    fn builds_a_query_our_own_parser_understands() {
        let query = build_query("api.test.co.kr", TYPE_A, 0xbeef);
        assert_eq!(&query[..2], &[0xbe, 0xef]);
        assert_eq!(u16::from_be_bytes([query[4], query[5]]), 1, "one question");
        assert_eq!(question_name(&query).as_deref(), Some("api.test.co.kr"));
        // qtype and qclass close the message.
        assert_eq!(&query[query.len() - 4..], &[0, 1, 0, 1]);
    }

    #[test]
    fn query_labels_are_length_capped() {
        let long = "a".repeat(200);
        let query = build_query(&format!("{long}.test"), TYPE_A, 1);
        assert_eq!(query[12], 63, "an over-long label must be truncated, not overflow");
        assert!(question_name(&query).is_some());
    }

    #[test]
    fn servfail_echoes_the_transaction_id() {
        let query = vec![0xab, 0xcd, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        let reply = servfail(&query);
        assert_eq!(&reply[..2], &[0xab, 0xcd]);
        assert_eq!(reply[2] & 0x80, 0x80, "QR bit must mark this a response");
        assert_eq!(reply[3] & 0x0f, 2, "RCODE must be SERVFAIL");
    }
}
