//! Server profiles and their translation into sing-box outbounds.
//!
//! One model covers every protocol Veil speaks, so the UI, the link parser and
//! the config generator all agree on what a profile is.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// uTLS fingerprints sing-box accepts. Presenting a real browser's TLS
/// fingerprint is what stops a middlebox distinguishing the tunnel by shape
/// alone, so it is on by default rather than an expert option.
pub const FINGERPRINTS: &[&str] = &["chrome", "firefox", "safari", "edge", "ios", "android", "random"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reality {
    pub public_key: String,
    pub short_id: String,
}

impl Default for Reality {
    fn default() -> Self {
        Self { public_key: String::new(), short_id: String::new() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tls {
    pub enabled: bool,
    /// SNI presented to the server. With REALITY this is the borrowed name of
    /// a real, unrelated site.
    pub sni: String,
    pub alpn: Vec<String>,
    /// Skip certificate verification. Only ever correct for a self-signed test
    /// server; on a real one it hands any middlebox a trivial interception.
    pub insecure: bool,
    pub fingerprint: String,
    pub reality: Option<Reality>,
}

impl Default for Tls {
    fn default() -> Self {
        Self {
            enabled: true,
            sni: String::new(),
            alpn: Vec::new(),
            insecure: false,
            fingerprint: "chrome".to_string(),
            reality: None,
        }
    }
}

impl Tls {
    fn to_json(&self) -> Value {
        let mut tls = json!({
            "enabled": self.enabled,
            "insecure": self.insecure,
        });
        if !self.sni.is_empty() {
            tls["server_name"] = json!(self.sni);
        }
        if !self.alpn.is_empty() {
            tls["alpn"] = json!(self.alpn);
        }
        if !self.fingerprint.is_empty() {
            tls["utls"] = json!({ "enabled": true, "fingerprint": self.fingerprint });
        }
        if let Some(reality) = &self.reality {
            tls["reality"] = json!({
                "enabled": true,
                "public_key": reality.public_key,
                "short_id": reality.short_id,
            });
        }
        tls
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Transport {
    Tcp,
    Ws { path: String, host: String },
    Grpc { service_name: String },
    HttpUpgrade { path: String, host: String },
}

impl Default for Transport {
    fn default() -> Self {
        Transport::Tcp
    }
}

impl Transport {
    fn to_json(&self) -> Option<Value> {
        match self {
            Transport::Tcp => None,
            Transport::Ws { path, host } => {
                let mut v = json!({ "type": "ws", "path": path });
                if !host.is_empty() {
                    v["headers"] = json!({ "Host": host });
                }
                Some(v)
            }
            Transport::Grpc { service_name } => {
                Some(json!({ "type": "grpc", "service_name": service_name }))
            }
            Transport::HttpUpgrade { path, host } => {
                let mut v = json!({ "type": "httpupgrade", "path": path });
                if !host.is_empty() {
                    v["host"] = json!(host);
                }
                Some(v)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TorTransport {
    /// No pluggable transport. Fine where Tor itself is not blocked.
    None,
    /// Random-looking bytes. The long-standing default.
    Obfs4,
    /// Looks like an ordinary HTTPS website; currently the most reliable.
    WebTunnel,
    /// Volunteer WebRTC proxies; needs no bridge lines to be obtained.
    Snowflake,
}

impl TorTransport {
    pub fn label(self) -> &'static str {
        match self {
            TorTransport::None => "없음",
            TorTransport::Obfs4 => "obfs4",
            TorTransport::WebTunnel => "webtunnel",
            TorTransport::Snowflake => "snowflake",
        }
    }

    pub const ALL: &'static [TorTransport] = &[
        TorTransport::None,
        TorTransport::Obfs4,
        TorTransport::WebTunnel,
        TorTransport::Snowflake,
    ];
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum Outbound {
    Vless {
        server: String,
        port: u16,
        uuid: String,
        /// `xtls-rprx-vision` when the server enables it; empty otherwise.
        flow: String,
        tls: Tls,
        transport: Transport,
    },
    Trojan {
        server: String,
        port: u16,
        password: String,
        tls: Tls,
        transport: Transport,
    },
    Shadowsocks {
        server: String,
        port: u16,
        method: String,
        password: String,
    },
    Hysteria2 {
        server: String,
        port: u16,
        password: String,
        /// Salamander obfuscation password; empty disables it.
        obfs_password: String,
        tls: Tls,
        /// Bandwidth hints in Mbps. Hysteria's congestion control needs these
        /// to be roughly honest; wildly overstating them is what makes it
        /// antisocial on a shared link.
        up_mbps: u32,
        down_mbps: u32,
    },
    /// Routed through a locally managed tor daemon's SOCKS port.
    Tor {
        transport: TorTransport,
        /// Bridge lines from bridges.torproject.org. Ignored for Snowflake.
        bridges: Vec<String>,
    },
}

impl Outbound {
    pub fn protocol_label(&self) -> &'static str {
        match self {
            Outbound::Vless { tls, .. } if tls.reality.is_some() => "VLESS + REALITY",
            Outbound::Vless { .. } => "VLESS",
            Outbound::Trojan { .. } => "Trojan",
            Outbound::Shadowsocks { .. } => "Shadowsocks",
            Outbound::Hysteria2 { .. } => "Hysteria2",
            Outbound::Tor { .. } => "Tor",
        }
    }

    pub fn endpoint(&self) -> String {
        match self {
            Outbound::Vless { server, port, .. }
            | Outbound::Trojan { server, port, .. }
            | Outbound::Shadowsocks { server, port, .. }
            | Outbound::Hysteria2 { server, port, .. } => format!("{server}:{port}"),
            Outbound::Tor { transport, .. } => format!("tor ({})", transport.label()),
        }
    }

    /// Rough resistance tier, mirroring the escalation a censor can deploy.
    /// Shown in the UI so the speed/robustness trade is visible at the point of
    /// choosing a profile.
    pub fn resistance(&self) -> (&'static str, &'static str) {
        match self {
            Outbound::Vless { tls, .. } if tls.reality.is_some() => {
                ("능동 프로빙까지 저항", "프로빙하면 진짜 유명 사이트로 연결되어 구분이 어렵습니다")
            }
            Outbound::Hysteria2 { .. } => ("지문 저항", "QUIC 자체가 소수라 튈 수 있고 UDP 차단에 취약합니다"),
            Outbound::Vless { .. } | Outbound::Trojan { .. } => {
                ("지문 저항", "능동 프로빙에는 응답 패턴으로 노출될 수 있습니다")
            }
            Outbound::Shadowsocks { .. } => ("지문 저항", "능동 프로빙에 가장 취약한 축입니다"),
            Outbound::Tor { .. } => ("익명성 최상", "3홉이라 느리고 exit IP 차단·캡차가 잦습니다"),
        }
    }

    /// Sing-box outbound object. `tor_socks_port` is where the locally managed
    /// tor daemon listens; Tor is reached as a plain SOCKS upstream because
    /// sing-box has no tor outbound of its own.
    pub fn to_json(&self, tag: &str, tor_socks_port: u16) -> Value {
        match self {
            Outbound::Vless { server, port, uuid, flow, tls, transport } => {
                let mut v = json!({
                    "type": "vless",
                    "tag": tag,
                    "server": server,
                    "server_port": port,
                    "uuid": uuid,
                    "packet_encoding": "xudp",
                    "tls": tls.to_json(),
                });
                if !flow.is_empty() {
                    v["flow"] = json!(flow);
                }
                if let Some(t) = transport.to_json() {
                    v["transport"] = t;
                }
                v
            }
            Outbound::Trojan { server, port, password, tls, transport } => {
                let mut v = json!({
                    "type": "trojan",
                    "tag": tag,
                    "server": server,
                    "server_port": port,
                    "password": password,
                    "tls": tls.to_json(),
                });
                if let Some(t) = transport.to_json() {
                    v["transport"] = t;
                }
                v
            }
            Outbound::Shadowsocks { server, port, method, password } => json!({
                "type": "shadowsocks",
                "tag": tag,
                "server": server,
                "server_port": port,
                "method": method,
                "password": password,
            }),
            Outbound::Hysteria2 { server, port, password, obfs_password, tls, up_mbps, down_mbps } => {
                let mut v = json!({
                    "type": "hysteria2",
                    "tag": tag,
                    "server": server,
                    "server_port": port,
                    "password": password,
                    "tls": tls.to_json(),
                });
                if !obfs_password.is_empty() {
                    v["obfs"] = json!({ "type": "salamander", "password": obfs_password });
                }
                if *up_mbps > 0 {
                    v["up_mbps"] = json!(up_mbps);
                }
                if *down_mbps > 0 {
                    v["down_mbps"] = json!(down_mbps);
                }
                v
            }
            Outbound::Tor { .. } => json!({
                "type": "socks",
                "tag": tag,
                "server": "127.0.0.1",
                "server_port": tor_socks_port,
                "version": "5",
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub outbound: Outbound,
}

impl Profile {
    pub fn uses_tor(&self) -> bool {
        matches!(self.outbound, Outbound::Tor { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reality_profile() -> Outbound {
        Outbound::Vless {
            server: "203.0.113.7".into(),
            port: 443,
            uuid: "8f1c9d2e-0000-4000-8000-000000000001".into(),
            flow: "xtls-rprx-vision".into(),
            tls: Tls {
                sni: "www.lovelive-anime.jp".into(),
                fingerprint: "chrome".into(),
                reality: Some(Reality { public_key: "PUBKEY".into(), short_id: "abcd".into() }),
                ..Default::default()
            },
            transport: Transport::Tcp,
        }
    }

    #[test]
    fn reality_outbound_carries_every_required_field() {
        let v = reality_profile().to_json("proxy", 9050);
        assert_eq!(v["type"], "vless");
        assert_eq!(v["server_port"], 443);
        assert_eq!(v["flow"], "xtls-rprx-vision");
        assert_eq!(v["tls"]["server_name"], "www.lovelive-anime.jp");
        assert_eq!(v["tls"]["reality"]["enabled"], true);
        assert_eq!(v["tls"]["reality"]["public_key"], "PUBKEY");
        assert_eq!(v["tls"]["utls"]["fingerprint"], "chrome");
    }

    #[test]
    fn plain_vless_omits_reality_and_empty_flow() {
        let outbound = Outbound::Vless {
            server: "a.example".into(),
            port: 8443,
            uuid: "uuid".into(),
            flow: String::new(),
            tls: Tls { sni: "a.example".into(), ..Default::default() },
            transport: Transport::Tcp,
        };
        let v = outbound.to_json("proxy", 9050);
        assert!(v["tls"].get("reality").is_none());
        assert!(v.get("flow").is_none(), "an empty flow must not be emitted");
    }

    #[test]
    fn websocket_transport_sets_the_host_header() {
        let outbound = Outbound::Trojan {
            server: "b.example".into(),
            port: 443,
            password: "pw".into(),
            tls: Tls::default(),
            transport: Transport::Ws { path: "/ray".into(), host: "cdn.example".into() },
        };
        let v = outbound.to_json("proxy", 9050);
        assert_eq!(v["transport"]["type"], "ws");
        assert_eq!(v["transport"]["path"], "/ray");
        assert_eq!(v["transport"]["headers"]["Host"], "cdn.example");
    }

    #[test]
    fn hysteria2_emits_obfs_and_bandwidth_only_when_set() {
        let bare = Outbound::Hysteria2 {
            server: "c.example".into(),
            port: 443,
            password: "pw".into(),
            obfs_password: String::new(),
            tls: Tls::default(),
            up_mbps: 0,
            down_mbps: 0,
        };
        let v = bare.to_json("proxy", 9050);
        assert!(v.get("obfs").is_none());
        assert!(v.get("up_mbps").is_none());

        let full = Outbound::Hysteria2 {
            server: "c.example".into(),
            port: 443,
            password: "pw".into(),
            obfs_password: "salt".into(),
            tls: Tls::default(),
            up_mbps: 50,
            down_mbps: 200,
        };
        let v = full.to_json("proxy", 9050);
        assert_eq!(v["obfs"]["type"], "salamander");
        assert_eq!(v["obfs"]["password"], "salt");
        assert_eq!(v["down_mbps"], 200);
    }

    #[test]
    fn tor_becomes_a_socks_upstream_on_the_managed_port() {
        let outbound = Outbound::Tor { transport: TorTransport::Obfs4, bridges: vec![] };
        let v = outbound.to_json("proxy", 9250);
        assert_eq!(v["type"], "socks");
        assert_eq!(v["server"], "127.0.0.1");
        assert_eq!(v["server_port"], 9250);
    }

    #[test]
    fn profiles_round_trip_through_toml() {
        let profile = Profile { name: "본서버".into(), outbound: reality_profile() };
        let text = toml::to_string(&profile).unwrap();
        let back: Profile = toml::from_str(&text).unwrap();
        assert_eq!(profile, back);
    }

    #[test]
    fn labels_distinguish_reality_from_plain_vless() {
        assert_eq!(reality_profile().protocol_label(), "VLESS + REALITY");
        let plain = Outbound::Vless {
            server: "a".into(),
            port: 1,
            uuid: "u".into(),
            flow: String::new(),
            tls: Tls::default(),
            transport: Transport::Tcp,
        };
        assert_eq!(plain.protocol_label(), "VLESS");
    }
}
