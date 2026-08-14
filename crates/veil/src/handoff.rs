//! Getting a profile onto a phone.
//!
//! The desktop app cannot run on iOS, but everything it actually produces —
//! the credential and the routing policy — is portable. This turns a profile
//! into the two forms a phone client can consume: a QR code to scan, and a
//! config file to import.

use crate::config::Config;
use crate::profile::{Outbound, Profile};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Rebuild the share link for a profile, so it can be shown as a QR code or
/// copied. Tor has no link form — it is not a server you can point a phone at.
pub fn share_link(profile: &Profile) -> Result<String> {
    let name = percent_encoding::utf8_percent_encode(
        &profile.name,
        percent_encoding::NON_ALPHANUMERIC,
    );

    Ok(match &profile.outbound {
        Outbound::Vless { server, port, uuid, flow, tls, transport } => {
            let mut params = vec!["encryption=none".to_string()];
            match &tls.reality {
                Some(reality) => {
                    params.push("security=reality".into());
                    params.push(format!("pbk={}", reality.public_key));
                    if !reality.short_id.is_empty() {
                        params.push(format!("sid={}", reality.short_id));
                    }
                }
                None if tls.enabled => params.push("security=tls".into()),
                None => params.push("security=none".into()),
            }
            if !tls.sni.is_empty() {
                params.push(format!("sni={}", tls.sni));
            }
            if !tls.fingerprint.is_empty() {
                params.push(format!("fp={}", tls.fingerprint));
            }
            if !flow.is_empty() {
                params.push(format!("flow={flow}"));
            }
            params.extend(transport_params(transport));
            format!("vless://{uuid}@{server}:{port}?{}#{name}", params.join("&"))
        }
        Outbound::Trojan { server, port, password, tls, transport } => {
            let mut params = vec!["security=tls".to_string()];
            if !tls.sni.is_empty() {
                params.push(format!("sni={}", tls.sni));
            }
            params.extend(transport_params(transport));
            format!("trojan://{password}@{server}:{port}?{}#{name}", params.join("&"))
        }
        Outbound::Shadowsocks { server, port, method, password } => {
            use base64::Engine;
            let userinfo = base64::engine::general_purpose::STANDARD
                .encode(format!("{method}:{password}"));
            format!("ss://{userinfo}@{server}:{port}#{name}")
        }
        Outbound::Hysteria2 { server, port, password, obfs_password, tls, .. } => {
            let mut params = Vec::new();
            if !tls.sni.is_empty() {
                params.push(format!("sni={}", tls.sni));
            }
            if !obfs_password.is_empty() {
                params.push("obfs=salamander".into());
                params.push(format!("obfs-password={obfs_password}"));
            }
            let query = if params.is_empty() { String::new() } else { format!("?{}", params.join("&")) };
            format!("hysteria2://{password}@{server}:{port}{query}#{name}")
        }
        Outbound::Tor { .. } => {
            bail!("Tor 프로필은 공유 링크로 옮길 수 없습니다. 폰에서는 Onion Browser를 쓰세요")
        }
    })
}

fn transport_params(transport: &crate::profile::Transport) -> Vec<String> {
    use crate::profile::Transport;
    match transport {
        Transport::Tcp => vec!["type=tcp".to_string()],
        Transport::Ws { path, host } => {
            let mut v = vec!["type=ws".to_string(), format!("path={path}")];
            if !host.is_empty() {
                v.push(format!("host={host}"));
            }
            v
        }
        Transport::Grpc { service_name } => {
            vec!["type=grpc".to_string(), format!("serviceName={service_name}")]
        }
        Transport::HttpUpgrade { path, host } => {
            let mut v = vec!["type=httpupgrade".to_string(), format!("path={path}")];
            if !host.is_empty() {
                v.push(format!("host={host}"));
            }
            v
        }
    }
}

/// A QR code as a square grid of booleans, true meaning a dark module.
pub struct QrMatrix {
    pub size: usize,
    pub dark: Vec<bool>,
}

impl QrMatrix {
    pub fn is_dark(&self, x: usize, y: usize) -> bool {
        self.dark.get(y * self.size + x).copied().unwrap_or(false)
    }
}

/// Encode text as a QR code.
pub fn qr(text: &str) -> Result<QrMatrix> {
    let code = qrcode::QrCode::new(text.as_bytes()).context("QR 코드를 만들 수 없습니다")?;
    let size = code.width();
    let dark = code
        .to_colors()
        .into_iter()
        .map(|c| c == qrcode::Color::Dark)
        .collect();
    Ok(QrMatrix { size, dark })
}

/// Files written for a phone handoff.
pub struct Handoff {
    pub folder: PathBuf,
    pub link: PathBuf,
    pub config: PathBuf,
    pub domains: PathBuf,
}

/// Write everything a phone needs into one folder.
///
/// Three files rather than one, because phone clients disagree about what they
/// accept: some import a link, some a sing-box config, and the rest only let
/// you paste routing rules by hand.
pub fn export(cfg: &Config, profile: &Profile, folder: &Path) -> Result<Handoff> {
    std::fs::create_dir_all(folder)
        .with_context(|| format!("{} 폴더를 만들 수 없습니다", folder.display()))?;

    let link_text = share_link(profile)?;
    let link = folder.join("link.txt");
    std::fs::write(&link, &link_text).context("링크 파일을 쓸 수 없습니다")?;

    // A full sing-box client config, so a sing-box-based phone app gets the
    // same routing policy as the desktop rather than a bare server entry.
    let config = folder.join("singbox-client.json");
    let value = crate::singbox::build(cfg, profile);
    std::fs::write(&config, serde_json::to_vec_pretty(&value)?)
        .context("설정 파일을 쓸 수 없습니다")?;

    // Plain list for apps that only offer a routing text box.
    let domains = folder.join("direct-domains.txt");
    let mut list = String::from(
        "# 터널을 우회할 도메인 — 앱의 라우팅 설정에 붙여넣으세요\n\
         # 은행·증권·정부는 해외 IP를 사기로 간주해 계정을 잠급니다\n\n",
    );
    for domain in &cfg.routing.direct_domains {
        list.push_str(domain);
        list.push('\n');
    }
    std::fs::write(&domains, list).context("도메인 목록을 쓸 수 없습니다")?;

    Ok(Handoff { folder: folder.to_path_buf(), link, config, domains })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link;
    use crate::profile::{Reality, Tls, TorTransport, Transport};

    fn reality() -> Profile {
        Profile {
            name: "REALITY".into(),
            outbound: Outbound::Vless {
                server: "203.0.113.7".into(),
                port: 443,
                uuid: "8f1c9d2e-0000-4000-8000-000000000001".into(),
                flow: "xtls-rprx-vision".into(),
                tls: Tls {
                    sni: "www.example.org".into(),
                    fingerprint: "chrome".into(),
                    reality: Some(Reality { public_key: "PK".into(), short_id: "ab12".into() }),
                    ..Default::default()
                },
                transport: Transport::Tcp,
            },
        }
    }

    #[test]
    fn a_rebuilt_link_parses_back_to_the_same_profile() {
        // The round trip is the whole guarantee: whatever the phone scans must
        // describe the identical server.
        let original = reality();
        let text = share_link(&original).unwrap();
        let parsed = link::parse_link(&text).expect("생성한 링크는 다시 읽을 수 있어야 합니다");
        assert_eq!(parsed.outbound, original.outbound);
        assert_eq!(parsed.name, original.name);
    }

    #[test]
    fn round_trips_every_protocol() {
        use crate::profile::Tls;
        let cases = vec![
            reality().outbound,
            Outbound::Trojan {
                server: "t.example".into(),
                port: 443,
                password: "pw".into(),
                tls: Tls { sni: "t.example".into(), ..Default::default() },
                transport: Transport::Ws { path: "/ray".into(), host: "cdn.example".into() },
            },
            Outbound::Shadowsocks {
                server: "s.example".into(),
                port: 8388,
                method: "aes-256-gcm".into(),
                password: "secret".into(),
            },
            Outbound::Hysteria2 {
                server: "h.example".into(),
                port: 443,
                password: "pw".into(),
                obfs_password: "salt".into(),
                tls: Tls { sni: "h.example".into(), alpn: vec!["h3".into()], ..Default::default() },
                up_mbps: 0,
                down_mbps: 0,
            },
        ];
        for outbound in cases {
            let profile = Profile { name: "P".into(), outbound: outbound.clone() };
            let text = share_link(&profile).unwrap();
            let parsed = link::parse_link(&text).unwrap_or_else(|e| panic!("{text} -> {e}"));
            assert_eq!(parsed.outbound, outbound, "{text}");
        }
    }

    #[test]
    fn tor_has_no_link_form() {
        let profile = Profile {
            name: "Tor".into(),
            outbound: Outbound::Tor { transport: TorTransport::Obfs4, bridges: vec![] },
        };
        assert!(share_link(&profile).is_err());
    }

    #[test]
    fn qr_encodes_a_full_length_link() {
        // Share links run to a couple of hundred characters; the encoder has to
        // pick a version large enough rather than fail.
        let text = share_link(&reality()).unwrap();
        let matrix = qr(&text).expect("링크 길이가 QR 용량을 넘지 않아야 합니다");
        assert!(matrix.size >= 21);
        assert_eq!(matrix.dark.len(), matrix.size * matrix.size);
        // Finder pattern: the top-left corner module is always dark.
        assert!(matrix.is_dark(0, 0));
    }

    #[test]
    fn export_writes_all_three_files() {
        let mut cfg = Config::default();
        cfg.routing.direct_domains = vec!["kbstar.com".into(), "go.kr".into()];
        let dir = std::env::temp_dir().join(format!("veil-handoff-{}", std::process::id()));

        let out = export(&cfg, &reality(), &dir).unwrap();
        assert!(out.link.exists() && out.config.exists() && out.domains.exists());

        let domains = std::fs::read_to_string(&out.domains).unwrap();
        assert!(domains.contains("kbstar.com") && domains.contains("go.kr"));

        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&out.config).unwrap()).unwrap();
        assert_eq!(config["outbounds"][0]["type"], "vless");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
