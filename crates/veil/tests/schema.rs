//! Validate generated configuration against the real sing-box binary.
//!
//! The schema moves between sing-box releases, and a config that merely looks
//! right will fail at runtime as a dead child process. Running the bundled
//! core's own `check` command is the only honest verification.

use std::path::PathBuf;
use std::process::Command;

use veil::config::{Config, Mode};
use veil::profile::{Outbound, Profile, Reality, Tls, TorTransport, Transport};
use veil::singbox;

fn singbox_binary() -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../vendor/singbox/sing-box.exe");
    path.exists().then_some(path)
}

/// Run `sing-box check` over a generated config, returning stderr on failure.
fn check(name: &str, cfg: &Config, profile: &Profile) -> Result<(), String> {
    let Some(binary) = singbox_binary() else {
        eprintln!("sing-box binary not vendored; skipping {name}");
        return Ok(());
    };

    let value = singbox::build(cfg, profile);
    let dir = std::env::temp_dir().join("veil-schema-tests");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{name}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;

    let output = Command::new(&binary)
        .arg("check")
        .arg("-c")
        .arg(&path)
        .output()
        .map_err(|e| format!("could not run sing-box: {e}"))?;

    if output.status.success() {
        let _ = std::fs::remove_file(&path);
        Ok(())
    } else {
        Err(format!(
            "{name} rejected:\n{}\n{}",
            String::from_utf8_lossy(&output.stderr).trim(),
            serde_json::to_string_pretty(&value).unwrap_or_default()
        ))
    }
}

fn reality() -> Profile {
    Profile {
        name: "reality".into(),
        outbound: Outbound::Vless {
            server: "203.0.113.7".into(),
            port: 443,
            uuid: "8f1c9d2e-0000-4000-8000-000000000001".into(),
            flow: "xtls-rprx-vision".into(),
            tls: Tls {
                sni: "www.example.org".into(),
                fingerprint: "chrome".into(),
                reality: Some(Reality {
                    // A syntactically valid x25519 public key; the value is
                    // never used because `check` does not dial.
                    public_key: "jNXHt1yRo0vDuchQlIP6Z0ZvjT3KtzVI-T4E7RoLJS0".into(),
                    short_id: "0123abcd".into(),
                }),
                ..Default::default()
            },
            transport: Transport::Tcp,
        },
    }
}

fn hysteria2() -> Profile {
    Profile {
        name: "hysteria2".into(),
        outbound: Outbound::Hysteria2 {
            server: "198.51.100.9".into(),
            port: 443,
            password: "pw".into(),
            obfs_password: "salt".into(),
            tls: Tls { sni: "h.example".into(), alpn: vec!["h3".into()], ..Default::default() },
            up_mbps: 50,
            down_mbps: 200,
        },
    }
}

fn trojan_ws() -> Profile {
    Profile {
        name: "trojan".into(),
        outbound: Outbound::Trojan {
            server: "cdn.example".into(),
            port: 443,
            password: "pw".into(),
            tls: Tls { sni: "cdn.example".into(), ..Default::default() },
            transport: Transport::Ws { path: "/ray".into(), host: "front.example".into() },
        },
    }
}

fn shadowsocks() -> Profile {
    Profile {
        name: "shadowsocks".into(),
        outbound: Outbound::Shadowsocks {
            server: "203.0.113.20".into(),
            port: 8388,
            method: "2022-blake3-aes-128-gcm".into(),
            // sing-box validates 2022 key material, so this must be 16 real bytes.
            password: "OTdKRUpUdE5tZjhVdlpMSw==".into(),
        },
    }
}

fn tor() -> Profile {
    Profile {
        name: "tor".into(),
        outbound: Outbound::Tor { transport: TorTransport::WebTunnel, bridges: vec![] },
    }
}

#[test]
fn every_protocol_produces_a_valid_config() {
    let cfg = Config::default();
    for profile in [reality(), hysteria2(), trojan_ws(), shadowsocks(), tor()] {
        let name = profile.name.clone();
        if let Err(e) = check(&name, &cfg, &profile) {
            panic!("{e}");
        }
    }
}

#[test]
fn proxy_mode_produces_a_valid_config() {
    let cfg = Config { mode: Mode::Proxy, ..Default::default() };
    if let Err(e) = check("proxy-mode", &cfg, &reality()) {
        panic!("{e}");
    }
}

#[test]
fn routing_options_produce_a_valid_config() {
    let mut cfg = Config::default();
    cfg.routing.direct_domains = vec!["bank.example".into(), "work.example".into()];
    cfg.routing.direct_processes = vec!["game.exe".into()];
    cfg.routing.proxy_domains = vec!["blocked.example".into()];
    cfg.routing.default_proxy = false;
    cfg.routing.block_quic = true;
    cfg.block_ipv6 = false;
    if let Err(e) = check("routing", &cfg, &reality()) {
        panic!("{e}");
    }
}

#[test]
fn every_tun_stack_produces_a_valid_config() {
    for stack in ["system", "gvisor", "mixed"] {
        let cfg = Config { tun_stack: stack.into(), ..Default::default() };
        if let Err(e) = check(&format!("stack-{stack}"), &cfg, &reality()) {
            panic!("{e}");
        }
    }
}

#[test]
fn dns_variants_produce_a_valid_config() {
    for remote in ["https://1.1.1.1/dns-query", "tls://9.9.9.9", "quic://8.8.8.8", "1.1.1.1"] {
        let mut cfg = Config::default();
        cfg.dns.remote = remote.into();
        let name = format!("dns-{}", remote.replace([':', '/', '.'], "_"));
        if let Err(e) = check(&name, &cfg, &reality()) {
            panic!("{e}");
        }
    }
}
