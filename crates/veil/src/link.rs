//! Share-link and subscription parsing.
//!
//! These formats are conventions rather than specifications, and providers
//! disagree on the details, so every field is treated as optional with a
//! sensible fallback rather than rejected.

use crate::profile::{Outbound, Profile, Reality, Tls, Transport};
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use std::collections::HashMap;
use url::Url;

/// Decode base64 in whichever dialect the provider happened to use.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    let trimmed: String = input.split_whitespace().collect();
    for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
        if let Ok(bytes) = engine.decode(trimmed.as_bytes()) {
            return Some(bytes);
        }
    }
    None
}

fn percent_decode(input: &str) -> String {
    percent_encoding::percent_decode_str(input).decode_utf8_lossy().into_owned()
}

/// Query parameters as a plain map; duplicates keep the last value.
fn query_map(url: &Url) -> HashMap<String, String> {
    url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

fn get<'a>(params: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    params.get(key).map(String::as_str).filter(|v| !v.is_empty())
}

/// Hosts arrive bracketed for IPv6; sing-box wants the bare address.
fn host_of(url: &Url) -> Result<String> {
    let host = url.host_str().ok_or_else(|| anyhow!("호스트가 없습니다"))?;
    Ok(host.trim_start_matches('[').trim_end_matches(']').to_string())
}

fn port_of(url: &Url) -> Result<u16> {
    url.port().ok_or_else(|| anyhow!("포트가 없습니다"))
}

fn name_of(url: &Url, fallback: &str) -> String {
    match url.fragment() {
        Some(f) if !f.is_empty() => percent_decode(f),
        _ => fallback.to_string(),
    }
}

fn alpn_of(params: &HashMap<String, String>) -> Vec<String> {
    get(params, "alpn")
        .map(|a| percent_decode(a).split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

fn truthy(value: Option<&str>) -> bool {
    matches!(value, Some("1") | Some("true") | Some("yes"))
}

/// Build the TLS block from the common `security=`/`sni=`/`fp=` conventions.
fn tls_of(params: &HashMap<String, String>, default_sni: &str) -> Tls {
    let security = get(params, "security").unwrap_or("none");
    let sni = get(params, "sni")
        .or_else(|| get(params, "peer"))
        .map(percent_decode)
        .unwrap_or_else(|| default_sni.to_string());

    let reality = (security == "reality").then(|| Reality {
        public_key: get(params, "pbk").unwrap_or_default().to_string(),
        short_id: get(params, "sid").unwrap_or_default().to_string(),
    });

    Tls {
        enabled: security == "tls" || security == "reality",
        sni,
        alpn: alpn_of(params),
        insecure: truthy(get(params, "allowInsecure")) || truthy(get(params, "insecure")),
        fingerprint: get(params, "fp").unwrap_or("chrome").to_string(),
        reality,
    }
}

fn transport_of(params: &HashMap<String, String>) -> Transport {
    let path = get(params, "path").map(percent_decode).unwrap_or_else(|| "/".to_string());
    let host = get(params, "host").map(percent_decode).unwrap_or_default();
    match get(params, "type").unwrap_or("tcp") {
        "ws" => Transport::Ws { path, host },
        "grpc" => Transport::Grpc {
            service_name: get(params, "serviceName").map(percent_decode).unwrap_or_default(),
        },
        "httpupgrade" => Transport::HttpUpgrade { path, host },
        _ => Transport::Tcp,
    }
}

/// Parse one share link into a profile.
pub fn parse_link(input: &str) -> Result<Profile> {
    let input = input.trim();
    let scheme = input.split("://").next().unwrap_or_default().to_ascii_lowercase();
    match scheme.as_str() {
        "vless" => parse_vless(input),
        "trojan" => parse_trojan(input),
        "ss" => parse_shadowsocks(input),
        "hysteria2" | "hy2" => parse_hysteria2(input),
        other => bail!("지원하지 않는 형식입니다: {other}"),
    }
}

fn parse_vless(input: &str) -> Result<Profile> {
    let url = Url::parse(input).context("vless 링크를 해석할 수 없습니다")?;
    let params = query_map(&url);
    let server = host_of(&url)?;
    let port = port_of(&url)?;
    let uuid = percent_decode(url.username());
    if uuid.is_empty() {
        bail!("UUID가 없습니다");
    }
    Ok(Profile {
        name: name_of(&url, &format!("{server}:{port}")),
        outbound: Outbound::Vless {
            tls: tls_of(&params, &server),
            transport: transport_of(&params),
            server,
            port,
            uuid,
            flow: get(&params, "flow").unwrap_or_default().to_string(),
        },
    })
}

fn parse_trojan(input: &str) -> Result<Profile> {
    let url = Url::parse(input).context("trojan 링크를 해석할 수 없습니다")?;
    let params = query_map(&url);
    let server = host_of(&url)?;
    let port = port_of(&url)?;
    let password = percent_decode(url.username());
    if password.is_empty() {
        bail!("비밀번호가 없습니다");
    }
    // Trojan is TLS by definition; providers routinely omit `security=tls`.
    let mut tls = tls_of(&params, &server);
    tls.enabled = true;
    Ok(Profile {
        name: name_of(&url, &format!("{server}:{port}")),
        outbound: Outbound::Trojan {
            tls,
            transport: transport_of(&params),
            server,
            port,
            password,
        },
    })
}

fn parse_hysteria2(input: &str) -> Result<Profile> {
    let url = Url::parse(input).context("hysteria2 링크를 해석할 수 없습니다")?;
    let params = query_map(&url);
    let server = host_of(&url)?;
    let port = port_of(&url)?;
    // Some providers put the password in the userinfo password slot instead.
    let password = match (url.username(), url.password()) {
        (user, Some(pass)) if !pass.is_empty() => format!("{}:{}", percent_decode(user), percent_decode(pass)),
        (user, _) => percent_decode(user),
    };
    if password.is_empty() {
        bail!("비밀번호가 없습니다");
    }

    let mut tls = tls_of(&params, &server);
    // Hysteria2 always runs over QUIC's TLS, whatever the link says.
    tls.enabled = true;
    if tls.alpn.is_empty() {
        tls.alpn = vec!["h3".to_string()];
    }

    let obfs_password = match get(&params, "obfs") {
        Some("salamander") => get(&params, "obfs-password")
            .or_else(|| get(&params, "obfs_password"))
            .map(percent_decode)
            .unwrap_or_default(),
        _ => String::new(),
    };

    Ok(Profile {
        name: name_of(&url, &format!("{server}:{port}")),
        outbound: Outbound::Hysteria2 {
            tls,
            server,
            port,
            password,
            obfs_password,
            up_mbps: get(&params, "up").and_then(|v| v.parse().ok()).unwrap_or(0),
            down_mbps: get(&params, "down").and_then(|v| v.parse().ok()).unwrap_or(0),
        },
    })
}

/// Shadowsocks links come in three shapes; all of them are in the wild.
fn parse_shadowsocks(input: &str) -> Result<Profile> {
    let body = input.strip_prefix("ss://").ok_or_else(|| anyhow!("ss 링크가 아닙니다"))?;
    let (body, fragment) = match body.split_once('#') {
        Some((b, f)) => (b, Some(percent_decode(f))),
        None => (body, None),
    };
    // Strip any plugin parameters; Veil does not run SIP003 plugins.
    let body = body.split('?').next().unwrap_or(body);

    let (userinfo, endpoint) = match body.rsplit_once('@') {
        // SIP002: userinfo@host:port, where userinfo may be base64.
        Some((user, endpoint)) => (percent_decode(user), endpoint.to_string()),
        // Legacy: the whole thing is one base64 blob.
        None => {
            let decoded = base64_decode(body).ok_or_else(|| anyhow!("base64를 해석할 수 없습니다"))?;
            let decoded = String::from_utf8(decoded).context("ss 링크가 UTF-8이 아닙니다")?;
            let (user, endpoint) = decoded
                .rsplit_once('@')
                .ok_or_else(|| anyhow!("method:password@host:port 형식이 아닙니다"))?;
            (user.to_string(), endpoint.to_string())
        }
    };

    // The userinfo is either `method:password` outright or base64 of it.
    let credentials = if userinfo.contains(':') {
        userinfo
    } else {
        let decoded = base64_decode(&userinfo).ok_or_else(|| anyhow!("자격 증명을 해석할 수 없습니다"))?;
        String::from_utf8(decoded).context("자격 증명이 UTF-8이 아닙니다")?
    };
    let (method, password) = credentials
        .split_once(':')
        .ok_or_else(|| anyhow!("method:password 형식이 아닙니다"))?;

    let (server, port) = endpoint
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("host:port 형식이 아닙니다"))?;
    let server = server.trim_start_matches('[').trim_end_matches(']').to_string();
    let port: u16 = port.parse().context("포트를 해석할 수 없습니다")?;

    Ok(Profile {
        name: fragment.unwrap_or_else(|| format!("{server}:{port}")),
        outbound: Outbound::Shadowsocks {
            server,
            port,
            method: method.to_string(),
            password: password.to_string(),
        },
    })
}

/// Parse a subscription body: either raw links one per line, or the whole thing
/// base64-encoded. Unparseable lines are skipped with a warning rather than
/// failing the import, because one bad entry should not lose the rest.
pub fn parse_subscription(body: &str) -> (Vec<Profile>, Vec<String>) {
    let decoded;
    let text = match base64_decode(body) {
        Some(bytes) if looks_like_links(&bytes) => {
            decoded = String::from_utf8_lossy(&bytes).into_owned();
            decoded.as_str()
        }
        _ => body,
    };

    let mut profiles = Vec::new();
    let mut errors = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match parse_link(line) {
            Ok(p) => profiles.push(p),
            Err(e) => errors.push(format!("{line}: {e}")),
        }
    }
    (profiles, errors)
}

/// Guard against treating a plain-text subscription as base64 by accident:
/// short link-like strings can decode to arbitrary bytes.
fn looks_like_links(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.contains("://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vless_reality() {
        let link = "vless://8f1c9d2e-0000-4000-8000-000000000001@203.0.113.7:443\
                    ?encryption=none&security=reality&sni=www.lovelive-anime.jp&fp=chrome\
                    &pbk=PUBKEY&sid=abcd&type=tcp&flow=xtls-rprx-vision#%EB%B3%B8%EC%84%9C%EB%B2%84";
        let profile = parse_link(link).unwrap();
        assert_eq!(profile.name, "본서버");
        let Outbound::Vless { server, port, uuid, flow, tls, transport } = profile.outbound else {
            panic!("expected vless");
        };
        assert_eq!(server, "203.0.113.7");
        assert_eq!(port, 443);
        assert_eq!(uuid, "8f1c9d2e-0000-4000-8000-000000000001");
        assert_eq!(flow, "xtls-rprx-vision");
        assert_eq!(tls.sni, "www.lovelive-anime.jp");
        assert_eq!(tls.fingerprint, "chrome");
        assert_eq!(tls.reality.unwrap().public_key, "PUBKEY");
        assert_eq!(transport, Transport::Tcp);
    }

    #[test]
    fn parses_vless_over_websocket() {
        let link = "vless://uuid@cdn.example:443?security=tls&type=ws&path=%2Fray&host=front.example#ws";
        let Outbound::Vless { transport, tls, .. } = parse_link(link).unwrap().outbound else {
            panic!("expected vless");
        };
        assert_eq!(transport, Transport::Ws { path: "/ray".into(), host: "front.example".into() });
        assert!(tls.enabled);
        assert!(tls.reality.is_none());
    }

    #[test]
    fn trojan_is_tls_even_without_the_parameter() {
        let profile = parse_link("trojan://p%40ssword@t.example:443#T").unwrap();
        let Outbound::Trojan { password, tls, .. } = profile.outbound else { panic!() };
        assert_eq!(password, "p@ssword", "percent-encoded passwords must be decoded");
        assert!(tls.enabled);
    }

    #[test]
    fn parses_sip002_shadowsocks() {
        // base64 of "aes-256-gcm:secret"
        let user = base64::engine::general_purpose::STANDARD.encode("aes-256-gcm:secret");
        let link = format!("ss://{user}@s.example:8388#SS");
        let profile = parse_link(&link).unwrap();
        let Outbound::Shadowsocks { server, port, method, password } = profile.outbound else {
            panic!()
        };
        assert_eq!((server.as_str(), port), ("s.example", 8388));
        assert_eq!(method, "aes-256-gcm");
        assert_eq!(password, "secret");
        assert_eq!(profile.name, "SS");
    }

    #[test]
    fn parses_legacy_shadowsocks_blob() {
        let blob = base64::engine::general_purpose::STANDARD
            .encode("chacha20-ietf-poly1305:pw@s.example:8388");
        let profile = parse_link(&format!("ss://{blob}#Legacy")).unwrap();
        let Outbound::Shadowsocks { method, password, port, .. } = profile.outbound else { panic!() };
        assert_eq!(method, "chacha20-ietf-poly1305");
        assert_eq!(password, "pw");
        assert_eq!(port, 8388);
    }

    #[test]
    fn parses_plain_shadowsocks_userinfo() {
        let profile = parse_link("ss://2022-blake3-aes-128-gcm:key==@s.example:9000").unwrap();
        let Outbound::Shadowsocks { method, password, .. } = profile.outbound else { panic!() };
        assert_eq!(method, "2022-blake3-aes-128-gcm");
        assert_eq!(password, "key==");
    }

    #[test]
    fn parses_hysteria2_with_obfuscation() {
        let link = "hy2://pw@h.example:443?sni=h.example&obfs=salamander&obfs-password=salt&up=50&down=200#H2";
        let Outbound::Hysteria2 { password, obfs_password, up_mbps, down_mbps, tls, .. } =
            parse_link(link).unwrap().outbound
        else {
            panic!()
        };
        assert_eq!(password, "pw");
        assert_eq!(obfs_password, "salt");
        assert_eq!((up_mbps, down_mbps), (50, 200));
        assert!(tls.enabled);
        assert_eq!(tls.alpn, vec!["h3".to_string()], "QUIC needs an h3 default");
    }

    #[test]
    fn insecure_flag_is_honoured_under_either_spelling() {
        let a = parse_link("trojan://p@t.example:443?allowInsecure=1").unwrap();
        let Outbound::Trojan { tls, .. } = a.outbound else { panic!() };
        assert!(tls.insecure);

        let b = parse_link("hy2://p@h.example:443?insecure=true").unwrap();
        let Outbound::Hysteria2 { tls, .. } = b.outbound else { panic!() };
        assert!(tls.insecure);
    }

    #[test]
    fn falls_back_to_the_endpoint_when_unnamed() {
        assert_eq!(parse_link("trojan://p@t.example:443").unwrap().name, "t.example:443");
    }

    #[test]
    fn rejects_incomplete_links() {
        assert!(parse_link("vless://@host:443").is_err(), "missing uuid");
        assert!(parse_link("vless://uuid@host").is_err(), "missing port");
        assert!(parse_link("ftp://host:21").is_err(), "unsupported scheme");
        assert!(parse_link("").is_err());
    }

    #[test]
    fn reads_a_plain_text_subscription() {
        let body = "trojan://a@one.example:443#One\n\
                    # a comment\n\
                    \n\
                    hy2://b@two.example:443#Two";
        let (profiles, errors) = parse_subscription(body);
        assert_eq!(profiles.len(), 2);
        assert!(errors.is_empty());
        assert_eq!(profiles[1].name, "Two");
    }

    #[test]
    fn reads_a_base64_subscription() {
        let plain = "trojan://a@one.example:443#One\nhy2://b@two.example:443#Two";
        let encoded = base64::engine::general_purpose::STANDARD.encode(plain);
        let (profiles, errors) = parse_subscription(&encoded);
        assert_eq!(profiles.len(), 2);
        assert!(errors.is_empty());
    }

    #[test]
    fn a_bad_line_does_not_lose_the_good_ones() {
        let body = "trojan://a@one.example:443#One\nnot-a-link\nhy2://b@two.example:443#Two";
        let (profiles, errors) = parse_subscription(body);
        assert_eq!(profiles.len(), 2);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn plain_text_is_not_mistaken_for_base64() {
        // This line happens to be valid base64 alphabet, but decoding it would
        // produce noise rather than links.
        let body = "trojan://abcd@one.example:443#One";
        let (profiles, _) = parse_subscription(body);
        assert_eq!(profiles.len(), 1);
    }
}
