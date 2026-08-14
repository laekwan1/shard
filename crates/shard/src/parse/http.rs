//! Plain HTTP request parsing — enough to find the `Host` header.

use super::Hostname;

const METHODS: &[&[u8]] = &[
    b"GET ", b"POST ", b"HEAD ", b"PUT ", b"DELETE ", b"OPTIONS ", b"CONNECT ", b"PATCH ", b"TRACE ",
];

/// Cheap check that this payload begins an HTTP/1.x request.
pub fn is_request(payload: &[u8]) -> bool {
    METHODS.iter().any(|m| payload.starts_with(m))
}

/// Where the `Host` header sits, for both hostname splitting and header mangling.
#[derive(Clone, Debug)]
pub struct HostHeader {
    pub host: Hostname,
    /// Offset of the literal `Host` field name within the payload.
    pub name_offset: usize,
    /// Offset of the space(s) between `Host:` and the value.
    pub separator_offset: usize,
}

pub fn host_header(payload: &[u8]) -> Option<HostHeader> {
    if !is_request(payload) {
        return None;
    }
    // Header block only; never scan into a request body.
    let end = find(payload, b"\r\n\r\n").unwrap_or(payload.len());
    let header_block = &payload[..end];

    let mut line_start = find(header_block, b"\r\n")? + 2;
    while line_start < header_block.len() {
        let line_end = find(&header_block[line_start..], b"\r\n")
            .map(|o| line_start + o)
            .unwrap_or(header_block.len());
        let line = &header_block[line_start..line_end];

        if line.len() > 5 && line[..4].eq_ignore_ascii_case(b"host") && line[4] == b':' {
            let separator_offset = line_start + 5;
            let mut value_start = 5;
            while value_start < line.len() && (line[value_start] == b' ' || line[value_start] == b'\t') {
                value_start += 1;
            }
            let mut value_end = line.len();
            while value_end > value_start && (line[value_end - 1] == b' ' || line[value_end - 1] == b'\t') {
                value_end -= 1;
            }
            if value_end <= value_start {
                return None;
            }
            let value = &line[value_start..value_end];
            let name = std::str::from_utf8(value).ok()?.to_ascii_lowercase();
            // Strip any :port so the name matches the same rules as an SNI.
            let name = name.split(':').next().unwrap_or(&name).to_string();
            return Some(HostHeader {
                host: Hostname {
                    name,
                    offset: line_start + value_start,
                    len: value_end - value_start,
                },
                name_offset: line_start,
                separator_offset,
            });
        }
        line_start = line_end + 2;
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_host_header() {
        let req = b"GET /index.html HTTP/1.1\r\nUser-Agent: x\r\nHost: www.example.com\r\n\r\n";
        let h = host_header(req).expect("host");
        assert_eq!(h.host.name, "www.example.com");
        assert_eq!(&req[h.host.offset..h.host.offset + h.host.len], b"www.example.com");
        assert_eq!(&req[h.name_offset..h.name_offset + 4], b"Host");
    }

    #[test]
    fn is_case_insensitive_and_strips_port() {
        let req = b"POST /x HTTP/1.1\r\nHOST:  Example.COM:8080  \r\n\r\n";
        let h = host_header(req).expect("host");
        assert_eq!(h.host.name, "example.com");
    }

    #[test]
    fn ignores_body_content() {
        let req = b"POST /x HTTP/1.1\r\nContent-Length: 20\r\n\r\nHost: evil.example\r\n";
        assert!(host_header(req).is_none());
    }

    #[test]
    fn rejects_non_http() {
        assert!(host_header(b"\x16\x03\x01\x00\x05hello").is_none());
        assert!(host_header(b"").is_none());
    }
}
