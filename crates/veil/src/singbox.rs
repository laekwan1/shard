//! Generating sing-box configuration.
//!
//! Everything the UI exposes ends up here as JSON. The generated file is
//! validated with `sing-box check` before the core is started, so a bad
//! combination surfaces as a readable message instead of a crashed child
//! process.

use crate::config::{Config, Mode};
use crate::profile::Profile;
use serde_json::{json, Value};

pub const PROXY_TAG: &str = "proxy";
pub const DIRECT_TAG: &str = "direct";

/// Build the full sing-box configuration for one profile.
pub fn build(cfg: &Config, profile: &Profile) -> Value {
    json!({
        "log": { "level": "warn", "timestamp": true },
        "dns": dns(cfg),
        "inbounds": inbounds(cfg),
        "outbounds": [
            profile.outbound.to_json(PROXY_TAG, cfg.tor.socks_port),
            { "type": "direct", "tag": DIRECT_TAG },
        ],
        "route": route(cfg),
        "experimental": {
            "clash_api": { "external_controller": format!("127.0.0.1:{}", cfg.clash_api_port) }
        }
    })
}

/// Resolvers for proxied names are reached *through* the tunnel, so the local
/// network never learns which hostnames are being looked up. Names that bypass
/// the tunnel are resolved locally, or they would resolve to the wrong region.
fn dns(cfg: &Config) -> Value {
    let mut rules = vec![json!({ "clash_mode": "Direct", "server": "local" })];
    if !cfg.routing.direct_domains.is_empty() {
        rules.push(json!({ "domain_suffix": cfg.routing.direct_domains, "server": "local" }));
    }

    json!({
        "servers": [
            dns_server("remote", &cfg.dns.remote, PROXY_TAG),
            dns_server("local", &cfg.dns.local, DIRECT_TAG),
        ],
        "rules": rules,
        "final": "remote",
        "strategy": cfg.dns.strategy,
        "independent_cache": true,
    })
}

/// Translate a resolver spec into sing-box's typed server object.
///
/// `detour` is omitted for direct resolution. sing-box 1.12+ rejects a DNS
/// server whose detour points at a bare `direct` outbound — it refuses at
/// startup with "detour to an empty direct outbound makes no sense", which
/// `sing-box check` does not catch because the config is structurally valid.
fn dns_server(tag: &str, spec: &str, detour: &str) -> Value {
    let (kind, rest) = match spec.split_once("://") {
        Some(("https", rest)) => ("https", rest),
        Some(("tls", rest)) => ("tls", rest),
        Some(("quic", rest)) => ("quic", rest),
        Some(("udp", rest)) => ("udp", rest),
        // A bare address is plain UDP.
        _ => ("udp", spec),
    };
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, Some(format!("/{path}"))),
        None => (rest, None),
    };

    let mut server = json!({ "type": kind, "tag": tag, "server": host });
    if detour != DIRECT_TAG {
        server["detour"] = json!(detour);
    }
    if let (Some(path), "https") = (path, kind) {
        server["path"] = json!(path);
    }
    server
}

fn inbounds(cfg: &Config) -> Value {
    // The loopback listener exists in both modes: even with TUN running, some
    // tools are easier to point at an explicit proxy than to route.
    let mixed = json!({
        "type": "mixed",
        "tag": "mixed-in",
        "listen": "127.0.0.1",
        "listen_port": cfg.mixed_port,
    });

    match cfg.mode {
        Mode::Proxy => json!([mixed]),
        Mode::Tun => {
            let mut address = vec!["172.19.0.1/30"];
            if !cfg.block_ipv6 {
                address.push("fdfe:dcba:9876::1/126");
            }
            json!([
                {
                    "type": "tun",
                    "tag": "tun-in",
                    "address": address,
                    "auto_route": true,
                    // Without strict routing, traffic can escape the tunnel via
                    // a more specific route the OS still holds.
                    "strict_route": true,
                    "stack": cfg.tun_stack,
                },
                mixed
            ])
        }
    }
}

fn route(cfg: &Config) -> Value {
    let r = &cfg.routing;
    // Order matters: sing-box takes the first matching rule.
    let mut rules = vec![
        // Sniffing recovers the hostname from the traffic itself, which is what
        // makes domain rules work at all under TUN.
        json!({ "action": "sniff" }),
        json!({ "protocol": "dns", "action": "hijack-dns" }),
    ];

    if r.bypass_private {
        rules.push(json!({ "ip_is_private": true, "outbound": DIRECT_TAG }));
    }
    if r.block_quic {
        rules.push(json!({ "network": "udp", "port": 443, "action": "reject" }));
    }
    if !r.direct_processes.is_empty() {
        rules.push(json!({ "process_name": r.direct_processes, "outbound": DIRECT_TAG }));
    }
    if !r.direct_process_paths.is_empty() {
        rules.push(json!({ "process_path": r.direct_process_paths, "outbound": DIRECT_TAG }));
    }
    if !r.direct_domains.is_empty() {
        rules.push(json!({ "domain_suffix": r.direct_domains, "outbound": DIRECT_TAG }));
    }
    if !r.proxy_domains.is_empty() {
        rules.push(json!({ "domain_suffix": r.proxy_domains, "outbound": PROXY_TAG }));
    }

    json!({
        "rules": rules,
        "final": if r.default_proxy { PROXY_TAG } else { DIRECT_TAG },
        // Required so the tunnel's own packets are not routed back into itself.
        "auto_detect_interface": true,
        // The proxy server's own hostname has to be resolved outside the
        // tunnel — resolving it through the tunnel that is not up yet cannot
        // work, and sing-box 1.12+ refuses to guess.
        "default_domain_resolver": { "server": "local" },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Outbound, Reality, Tls, Transport};

    fn reality() -> Profile {
        Profile {
            name: "R".into(),
            outbound: Outbound::Vless {
                server: "203.0.113.7".into(),
                port: 443,
                uuid: "8f1c9d2e-0000-4000-8000-000000000001".into(),
                flow: "xtls-rprx-vision".into(),
                tls: Tls {
                    sni: "www.example.org".into(),
                    reality: Some(Reality { public_key: "PK".into(), short_id: "ab".into() }),
                    ..Default::default()
                },
                transport: Transport::Tcp,
            },
        }
    }

    #[test]
    fn tun_mode_emits_a_tun_and_a_loopback_inbound() {
        let cfg = Config::default();
        let v = build(&cfg, &reality());
        let inbounds = v["inbounds"].as_array().unwrap();
        assert_eq!(inbounds.len(), 2);
        assert_eq!(inbounds[0]["type"], "tun");
        assert_eq!(inbounds[0]["strict_route"], true);
        assert_eq!(inbounds[1]["type"], "mixed");
    }

    #[test]
    fn blocking_ipv6_drops_the_v6_tun_address() {
        let cfg = Config { block_ipv6: true, ..Default::default() };
        let v = build(&cfg, &reality());
        let addresses = v["inbounds"][0]["address"].as_array().unwrap();
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0], "172.19.0.1/30");

        let cfg = Config { block_ipv6: false, ..Default::default() };
        let v = build(&cfg, &reality());
        assert_eq!(v["inbounds"][0]["address"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn proxy_mode_emits_only_the_loopback_inbound() {
        let cfg = Config { mode: Mode::Proxy, ..Default::default() };
        let v = build(&cfg, &reality());
        let inbounds = v["inbounds"].as_array().unwrap();
        assert_eq!(inbounds.len(), 1);
        assert_eq!(inbounds[0]["type"], "mixed");
    }

    #[test]
    fn sniff_and_dns_hijack_come_before_everything_else() {
        let v = build(&Config::default(), &reality());
        let rules = v["route"]["rules"].as_array().unwrap();
        assert_eq!(rules[0]["action"], "sniff");
        assert_eq!(rules[1]["action"], "hijack-dns");
    }

    #[test]
    fn direct_lists_become_route_rules() {
        let mut cfg = Config::default();
        cfg.routing.direct_domains = vec!["bank.example".into()];
        cfg.routing.direct_processes = vec!["game.exe".into()];
        let v = build(&cfg, &reality());
        let rules = v["route"]["rules"].as_array().unwrap();

        let has_process = rules.iter().any(|r| r["process_name"][0] == "game.exe" && r["outbound"] == "direct");
        let has_domain = rules.iter().any(|r| r["domain_suffix"][0] == "bank.example" && r["outbound"] == "direct");
        assert!(has_process && has_domain);
    }

    #[test]
    fn a_process_path_rule_is_emitted_separately_from_names() {
        // Matching Tor Browser by name would also exclude an ordinary Firefox;
        // the path rule is what keeps the exclusion narrow.
        let mut cfg = Config::default();
        cfg.routing.direct_process_paths =
            vec![r"C:\Users\x\Desktop\Tor Browser\Browser\firefox.exe".into()];
        let v = build(&cfg, &reality());
        let rules = v["route"]["rules"].as_array().unwrap();

        assert!(rules.iter().any(|r| r["process_path"][0]
            .as_str()
            .is_some_and(|p| p.contains("Tor Browser"))));
        assert!(!rules.iter().any(|r| r.get("process_name").is_some()));
    }

    #[test]
    fn direct_domains_also_resolve_locally() {
        // Resolving a bypassed name through the tunnel would return a foreign
        // region's answer for a connection made from here.
        let mut cfg = Config::default();
        cfg.routing.direct_domains = vec!["bank.example".into()];
        let v = build(&cfg, &reality());
        let rules = v["dns"]["rules"].as_array().unwrap();
        assert!(rules.iter().any(|r| r["domain_suffix"][0] == "bank.example" && r["server"] == "local"));
    }

    #[test]
    fn quic_rejection_is_toggleable() {
        let cfg = Config::default();
        let v = build(&cfg, &reality());
        let rules = v["route"]["rules"].as_array().unwrap();
        assert!(rules.iter().any(|r| r["action"] == "reject" && r["port"] == 443));

        let mut cfg = Config::default();
        cfg.routing.block_quic = false;
        let v = build(&cfg, &reality());
        let rules = v["route"]["rules"].as_array().unwrap();
        assert!(!rules.iter().any(|r| r["action"] == "reject"));
    }

    #[test]
    fn final_outbound_follows_the_default_policy() {
        let mut cfg = Config::default();
        assert_eq!(build(&cfg, &reality())["route"]["final"], "proxy");
        cfg.routing.default_proxy = false;
        assert_eq!(build(&cfg, &reality())["route"]["final"], "direct");
    }

    #[test]
    fn dns_specs_map_to_typed_servers() {
        assert_eq!(
            dns_server("remote", "https://1.1.1.1/dns-query", "proxy"),
            json!({"type":"https","tag":"remote","server":"1.1.1.1","detour":"proxy","path":"/dns-query"})
        );
        assert_eq!(dns_server("t", "tls://9.9.9.9", "proxy")["type"], "tls");
        assert_eq!(dns_server("q", "quic://8.8.8.8", "proxy")["type"], "quic");
    }

    #[test]
    fn direct_dns_carries_no_detour() {
        // Regression: sing-box refuses to start when a DNS server detours to a
        // bare direct outbound, and `sing-box check` accepts the config anyway.
        let server = dns_server("local", "1.1.1.1", DIRECT_TAG);
        assert_eq!(server, json!({"type":"udp","tag":"local","server":"1.1.1.1"}));
        assert!(server.get("detour").is_none());

        let generated = build(&Config::default(), &reality());
        let local = &generated["dns"]["servers"][1];
        assert_eq!(local["tag"], "local");
        assert!(local.get("detour").is_none(), "got: {local}");
    }

    #[test]
    fn the_server_hostname_resolves_outside_the_tunnel() {
        let v = build(&Config::default(), &reality());
        assert_eq!(v["route"]["default_domain_resolver"]["server"], "local");
    }

    #[test]
    fn remote_dns_goes_through_the_tunnel() {
        let v = build(&Config::default(), &reality());
        let servers = v["dns"]["servers"].as_array().unwrap();
        assert_eq!(servers[0]["tag"], "remote");
        assert_eq!(servers[0]["detour"], "proxy", "proxied names must resolve inside the tunnel");
        // The local resolver carries no detour — see `direct_dns_carries_no_detour`.
        assert_eq!(servers[1]["tag"], "local");
        assert_eq!(v["dns"]["final"], "remote");
    }
}
