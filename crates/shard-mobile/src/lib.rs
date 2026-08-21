//! Shard for phones.
//!
//! The desktop engine crafts packets with a kernel driver. Neither iOS nor
//! Android will let an app do that: both hand you a TUN interface to read from,
//! but everything outbound has to leave through an ordinary socket. So the
//! phone build is not a port — it terminates each TCP connection locally and
//! reproduces the desync with the only tools a socket offers.
//!
//! What carries over unchanged is the part that matters: the ClientHello
//! parser, hostname matching and the strategy model all come from the `shard`
//! crate. What is new is [`socket`], which does with write boundaries and
//! urgent data what the desktop does with crafted segments.
//!
//! Measured against a Korean ISP that reassembles the stream before matching:
//! splitting alone failed every time, split-plus-urgent succeeded.

pub mod download;
pub mod http_proxy;
pub mod jni;
pub mod resolve;
pub mod tls;
#[cfg(target_os = "android")]
pub mod android;
pub mod relay;
pub mod socket;
pub mod socks;

pub use relay::{Engine, Outcome, Stats, StatsSnapshot};
pub use socket::{Mode, Plan};

/// Everything the platform layer needs to decide how to treat a connection.
pub struct Policy {
    config: shard::config::Config,
    /// Cut position when no hostname can be read from the payload.
    pub fallback_split: usize,
}

impl Policy {
    pub fn new(config: shard::config::Config) -> Self {
        Self { config, fallback_split: 1 }
    }

    /// Load the same configuration file the desktop app uses, so both builds
    /// behave identically given the same settings.
    pub fn from_config() -> Self {
        Self::new(shard::config::Config::load())
    }

    pub fn config(&self) -> &shard::config::Config {
        &self.config
    }

    pub fn config_mut(&mut self) -> &mut shard::config::Config {
        &mut self.config
    }

    /// Hostname carried by an opening payload, if any.
    pub fn hostname(&self, payload: &[u8]) -> Option<String> {
        shard::parse::tls::client_hello_sni(payload)
            .map(|h| h.name)
            .or_else(|| shard::parse::http::host_header(payload).map(|h| h.host.name))
    }

    /// How to send this connection's first payload.
    ///
    /// A payload with no readable hostname is still treated under a blanket
    /// policy — a browser's ClientHello often runs past one segment, and
    /// skipping those would mean skipping most real traffic.
    pub fn plan(&self, payload: &[u8]) -> Plan {
        let host = self.hostname(payload);
        let applies = match (&host, self.config.scope) {
            (Some(h), _) => self.config.applies_to(h),
            (None, shard::config::Scope::All) => true,
            (None, shard::config::Scope::Listed) => false,
        };
        if !applies {
            return Plan { mode: Mode::None, at: 0 };
        }

        // The desktop strategy model has knobs a socket cannot honour — TTL,
        // decoys, sequence tricks. Only the split survives the translation, and
        // the urgent byte is what takes over the decoy's job.
        let desktop = match &host {
            Some(h) => self.config.strategy_for(h),
            None => &self.config.strategy,
        };
        let mode = if desktop.desync.splits() || desktop.desync.uses_fake() {
            Mode::SplitOob
        } else {
            Mode::None
        };
        socket::plan(payload, mode, self.fallback_split)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shard::config::{Config, Scope};
    use shard::strategy::{Desync, Strategy};

    fn hello(host: &str) -> Vec<u8> {
        shard::desync::build_client_hello(host, 0)
    }

    #[test]
    fn a_blocked_host_gets_split_with_urgent_data() {
        let policy = Policy::new(Config::default());
        let plan = policy.plan(&hello("blocked.example"));
        assert_eq!(plan.mode, Mode::SplitOob);
        assert!(plan.at > 0);
    }

    #[test]
    fn an_excluded_host_is_left_alone() {
        let config = Config { exclude: vec!["bank.example".into()], ..Default::default() };
        let policy = Policy::new(config);
        assert_eq!(policy.plan(&hello("login.bank.example")).mode, Mode::None);
    }

    #[test]
    fn a_listed_scope_skips_unknown_payloads() {
        // Without a hostname there is nothing to match a list against, so the
        // conservative reading of "only these domains" is to do nothing.
        let config = Config {
            scope: Scope::Listed,
            domains: vec!["blocked.example".into()],
            ..Default::default()
        };
        let policy = Policy::new(config);
        assert_eq!(policy.plan(&hello("blocked.example")).mode, Mode::SplitOob);
        assert_eq!(policy.plan(&hello("other.example")).mode, Mode::None);
        assert_eq!(policy.plan(&[0u8; 200]).mode, Mode::None);
    }

    #[test]
    fn an_unreadable_handshake_still_gets_treated_under_a_blanket_policy() {
        // Chrome's hello regularly spans two segments; ignoring those would
        // ignore most browsing.
        let policy = Policy::new(Config::default());
        let mut truncated = hello("blocked.example");
        truncated.truncate(60);
        assert_eq!(policy.plan(&truncated).mode, Mode::SplitOob);
    }

    #[test]
    fn a_passthrough_strategy_disables_desync() {
        let config = Config { strategy: Strategy::passthrough(), ..Default::default() };
        let policy = Policy::new(config);
        assert_eq!(policy.plan(&hello("anything.example")).mode, Mode::None);
    }

    #[test]
    fn decoy_strategies_translate_to_the_urgent_variant() {
        // A socket cannot send a decoy, so the strategy that replaces it is the
        // one that was measured to work.
        for desync in [Desync::Split, Desync::Fake, Desync::FakeDisorder] {
            let config = Config {
                strategy: Strategy { desync, ..Default::default() },
                ..Default::default()
            };
            let policy = Policy::new(config);
            assert_eq!(policy.plan(&hello("x.example")).mode, Mode::SplitOob, "{desync:?}");
        }
    }

    #[test]
    fn hostnames_are_read_from_both_protocols() {
        let policy = Policy::new(Config::default());
        assert_eq!(policy.hostname(&hello("a.example")).as_deref(), Some("a.example"));

        let request = b"GET / HTTP/1.1\r\nHost: b.example\r\n\r\n";
        assert_eq!(policy.hostname(request).as_deref(), Some("b.example"));
        assert_eq!(policy.hostname(&[0u8; 40]), None);
    }
}
