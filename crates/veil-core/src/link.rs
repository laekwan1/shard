//! Share links.
//!
//! One line that carries everything a client needs, so setting up a phone is
//! pasting a string rather than filling a form. The shape is the usual
//! `trojan://` one, with the certificate pin as an extra parameter — clients
//! that do not know about it ignore it, and ours refuses to connect without it.

use crate::client::Server;
use crate::tls::Trust;
use anyhow::{bail, Context, Result};
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, CONTROLS};

/// Everything that must survive being put in a URL's userinfo or query.
const ESCAPE: &AsciiSet = &CONTROLS
    .add(b' ').add(b'"').add(b'#').add(b'<').add(b'>').add(b'?').add(b'`')
    .add(b'{').add(b'}').add(b'/').add(b':').add(b'@').add(b'&').add(b'=')
    .add(b'+').add(b'%');

/// Render a server as a share link.
pub fn build(server: &Server, name: &str) -> String {
    let password = utf8_percent_encode(&server.password, ESCAPE).to_string();
    let host = if server.host.contains(':') {
        // A bare IPv6 address would otherwise be read as host:port.
        format!("[{}]", server.host)
    } else {
        server.host.clone()
    };

    let mut link = format!("trojan://{password}@{host}:{}", server.port);
    let mut query = vec![format!("sni={}", utf8_percent_encode(&server.sni, ESCAPE))];
    if let Trust::Pinned(pin) = &server.trust {
        query.push(format!("pin={pin}"));
    }
    link.push('?');
    link.push_str(&query.join("&"));

    if !name.is_empty() {
        link.push('#');
        link.push_str(&utf8_percent_encode(name, ESCAPE).to_string());
    }
    link
}

/// Parse a share link. Returns the server and the label it carried.
pub fn parse(link: &str) -> Result<(Server, String)> {
    let rest = link
        .trim()
        .strip_prefix("trojan://")
        .context("trojan:// 로 시작하는 링크가 아닙니다")?;

    let (rest, name) = match rest.split_once('#') {
        Some((head, tag)) => (head, decode(tag)),
        None => (rest, String::new()),
    };
    let (rest, query) = match rest.split_once('?') {
        Some((head, query)) => (head, query),
        None => (rest, ""),
    };

    let (password, authority) = rest.rsplit_once('@').context("링크에 비밀번호가 없습니다")?;
    let password = decode(password);
    if password.is_empty() {
        bail!("비밀번호가 비어 있습니다");
    }

    let (host, port) = crate::inbound::split_authority(authority, 443)?;
    if host.is_empty() {
        bail!("서버 주소가 비어 있습니다");
    }

    let mut sni = None;
    let mut trust = Trust::WebPki;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "sni" | "peer" | "host" => sni = Some(decode(value)),
            "pin" => trust = Trust::pinned(&decode(value))?,
            // Unknown parameters are other clients' business, not ours.
            _ => {}
        }
    }

    let server = Server::new(host.clone(), port, password)
        .with_sni(sni.unwrap_or(host))
        .with_trust(trust);
    Ok((server, name))
}

fn decode(text: &str) -> String {
    percent_decode_str(text).decode_utf8_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin() -> Trust {
        Trust::pinned(&"ab".repeat(32)).unwrap()
    }

    #[test]
    fn a_link_survives_a_round_trip() {
        let original = Server::new("203.0.113.9", 443, "hunter2")
            .with_sni("www.microsoft.com")
            .with_trust(pin());

        let (parsed, name) = parse(&build(&original, "오라클")).unwrap();

        assert_eq!(parsed.host, original.host);
        assert_eq!(parsed.port, original.port);
        assert_eq!(parsed.password, original.password);
        assert_eq!(parsed.sni, original.sni);
        assert_eq!(parsed.trust, original.trust);
        assert_eq!(name, "오라클");
    }

    #[test]
    fn a_password_with_url_characters_survives() {
        // A password containing @ or # would split the link in the wrong place
        // if it were not escaped.
        for password in ["p@ss#word", "a b&c=d", "100%sure", "colon:slash/"] {
            let server = Server::new("example.com", 443, password).with_trust(pin());
            let (parsed, _) = parse(&build(&server, "")).unwrap();
            assert_eq!(parsed.password, password, "failed for {password:?}");
        }
    }

    #[test]
    fn an_ipv6_server_survives() {
        let server = Server::new("2001:db8::1", 8443, "pw").with_trust(pin());
        let link = build(&server, "");
        assert!(link.contains("[2001:db8::1]:8443"), "{link}");

        let (parsed, _) = parse(&link).unwrap();
        assert_eq!(parsed.host, "2001:db8::1");
        assert_eq!(parsed.port, 8443);
    }

    #[test]
    fn a_link_without_a_pin_falls_back_to_public_authorities() {
        // What a server with a real domain and a real certificate looks like.
        let (parsed, _) = parse("trojan://pw@veil.example.com:443?sni=veil.example.com").unwrap();
        assert_eq!(parsed.trust, Trust::WebPki);
        assert_eq!(parsed.sni, "veil.example.com");
    }

    #[test]
    fn the_sni_defaults_to_the_host() {
        let (parsed, _) = parse("trojan://pw@veil.example.com:443").unwrap();
        assert_eq!(parsed.sni, "veil.example.com");
    }

    #[test]
    fn unknown_parameters_are_ignored() {
        // Links written for other clients carry parameters we have no use for;
        // refusing them would reject links that are perfectly usable.
        let (parsed, _) =
            parse("trojan://pw@example.com:443?sni=a.example&type=tcp&headerType=none&alpn=h2")
                .unwrap();
        assert_eq!(parsed.sni, "a.example");
    }

    #[test]
    fn a_malformed_link_is_rejected() {
        for bad in [
            "",
            "https://example.com",
            "trojan://example.com:443",          // no password
            "trojan://pw@:443",                   // no host
            "trojan://@example.com:443",          // empty password
            "trojan://pw@example.com:443?pin=xy", // pin is not a 32-byte hash
        ] {
            assert!(parse(bad).is_err(), "should have been rejected: {bad:?}");
        }
    }

    #[test]
    fn a_link_with_no_port_defaults_to_443() {
        let (parsed, _) = parse("trojan://pw@example.com").unwrap();
        assert_eq!(parsed.port, 443);
    }
}
