//! Shard's on-disk configuration.

use crate::strategy::Strategy;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const APP_NAME: &str = "Shard";

/// Which connections the desync engine touches.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Every outbound HTTP/HTTPS connection. Works without any list to
    /// maintain, at the cost of a few extra packets on every new connection
    /// and the small chance of upsetting a site that is picky about
    /// segmentation. `exclude` exists for those.
    #[default]
    All,
    /// Only hostnames in `domains`. Minimum overhead and zero blast radius,
    /// but a newly blocked site does nothing until it is added.
    Listed,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::All => "전체 트래픽",
            Scope::Listed => "지정 도메인만",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Doh {
    pub enabled: bool,
    /// Where the local forwarder listens. Point the system resolver here, or
    /// enable `set_system_dns` to have Shard do it.
    pub listen: String,
    /// DoH endpoints, tried in order.
    pub upstreams: Vec<String>,
    /// Literal addresses for the upstream hosts, so resolving the resolver
    /// does not depend on the DNS we are trying to replace.
    pub bootstrap: Vec<String>,
    /// Repoint every active adapter's DNS at `listen` while the engine runs,
    /// and restore the previous setting on stop.
    pub set_system_dns: bool,
}

impl Default for Doh {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: "127.0.0.1:53".to_string(),
            upstreams: vec![
                "https://cloudflare-dns.com/dns-query".to_string(),
                "https://dns.google/dns-query".to_string(),
            ],
            bootstrap: vec!["1.1.1.1".to_string(), "8.8.8.8".to_string()],
            set_system_dns: false,
        }
    }
}

/// Bumped whenever a *default* changes in a way that has to reach existing
/// installs. A saved config pins every field, so without this an improved
/// default would only ever apply to fresh installs — and a user hitting the
/// exact problem the change fixes would keep hitting it.
pub const CURRENT_VERSION: u32 = 3;

/// A saved site — the shape the browser's home page (bookmarks, history) is built
/// from, mirroring the phone's `Bookmark`.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
}

/// The browser's home page: what the user pinned, where they have been, and the page
/// the home button opens — the desktop half of the phone's home/favorites.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Browser {
    /// The page the home button opens; empty means the start page (bookmarks).
    pub homepage: String,
    pub bookmarks: Vec<Bookmark>,
    /// Recently visited pages, newest first, capped.
    pub history: Vec<Bookmark>,
    /// host → visit count, for the "자주 방문" tiles.
    pub visits: BTreeMap<String, u32>,
    /// Hosts removed from "자주 방문", so a deleted tile does not return.
    pub hidden_frequent: Vec<String>,
    /// Block ad/tracker requests (the WebResourceRequested 403 and the AD_STRIP
    /// injection). On by default. A toggle because blocking can make YouTube's
    /// player wait for the ad it cannot load — turning it off is how that is told
    /// apart from a slow page. Default is true, so a custom `Default` (not derived,
    /// which would give false) and `#[serde(default)]` keep old configs blocking.
    pub block_ads: bool,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            homepage: String::new(),
            bookmarks: Vec::new(),
            history: Vec::new(),
            visits: BTreeMap::new(),
            hidden_frequent: Vec::new(),
            block_ads: true,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Schema version of the file this was loaded from. See `CURRENT_VERSION`.
    pub version: u32,
    pub start_engine_on_launch: bool,
    pub scope: Scope,
    /// Hostname patterns the engine applies to when `scope` is `Listed`.
    pub domains: Vec<String>,
    /// Hostname patterns always passed through, whatever the scope. Use this
    /// when desync breaks a specific site.
    pub exclude: Vec<String>,
    pub ports: Vec<u16>,
    /// Packet workers. More helps only on very busy links; each one holds a
    /// blocking receive on the driver.
    pub worker_threads: u8,
    pub strategy: Strategy,
    /// What to prefer when saving video from the browser window.
    #[serde(default)]
    pub download: Download,
    /// Per-hostname strategies, normally written by the auto-prober.
    pub overrides: BTreeMap<String, Strategy>,
    /// Watch for connections that fail after the engine has handled them, and
    /// probe those hosts in the background without being asked.
    ///
    /// This is what makes the on/off switch sufficient on its own: the default
    /// strategy covers most sites, and anything it fails on gets its own
    /// strategy learned automatically.
    pub auto_learn: bool,
    /// Failures observed before a host is probed. One reset can be a genuine
    /// server hangup; a pattern is a block.
    pub auto_learn_threshold: u8,
    /// Minutes before the same host may be probed again, so a host with no
    /// working strategy is not retried in a loop.
    pub auto_learn_cooldown_min: u16,
    /// Also count "handshake sent, nothing ever came back" as a failure.
    ///
    /// Some networks inject a reset, which is free to detect; others drop the
    /// packet silently, which can only be spotted by watching for the reply
    /// that never arrives. That means sniffing inbound data packets, so it
    /// costs a few percent CPU on a busy link.
    pub detect_silent_drops: bool,
    pub doh: Doh,
    /// Global hotkey for toggling the engine, in `global-hotkey` syntax.
    pub hotkey: String,
    /// The browser's home page — bookmarks, history, homepage.
    #[serde(default)]
    pub browser: Browser,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            // Off, so starting the program is not the same as switching the
            // machine's traffic through it. The switch is the whole interface;
            // it should be the user who throws it.
            start_engine_on_launch: false,
            scope: Scope::All,
            domains: Vec::new(),
            exclude: Vec::new(),
            ports: vec![80, 443],
            worker_threads: 2,
            strategy: Strategy::default(),
            download: Download::default(),
            overrides: BTreeMap::new(),
            auto_learn: true,
            auto_learn_threshold: 2,
            auto_learn_cooldown_min: 30,
            detect_silent_drops: true,
            doh: Doh::default(),
            hotkey: "Ctrl+Shift+KeyS".to_string(),
            browser: Browser::default(),
        }
    }
}

impl Config {
    pub fn path() -> std::path::PathBuf {
        uikit::config::app_dir(APP_NAME).join("config.toml")
    }

    pub fn load() -> Self {
        let mut cfg: Self = uikit::config::load_or_default(&Self::path());
        if cfg.migrate() {
            if let Err(e) = cfg.save() {
                tracing::error!("could not save migrated config: {e}");
            }
        }
        cfg
    }

    /// Bring a config written by an older build up to date. Returns true if
    /// anything changed.
    ///
    /// Only the global strategy is reset: domain lists and learned per-host
    /// overrides are the user's data and survive. Learned overrides were
    /// produced by a probe, so they reflect what actually worked rather than a
    /// stale default.
    fn migrate(&mut self) -> bool {
        if self.version >= CURRENT_VERSION {
            return false;
        }
        tracing::warn!(
            "설정을 v{} → v{}로 갱신합니다 (기본 전략을 안전한 값으로 되돌립니다)",
            self.version,
            CURRENT_VERSION
        );
        self.strategy = Strategy::default();
        self.version = CURRENT_VERSION;
        true
    }

    pub fn save(&self) -> anyhow::Result<()> {
        uikit::config::save(&Self::path(), self)
    }

    /// Strategy for a hostname: the most specific override, else the default.
    pub fn strategy_for(&self, host: &str) -> &Strategy {
        best_match(&self.overrides, host).unwrap_or(&self.strategy)
    }

    /// Whether the engine should touch this hostname at all.
    pub fn applies_to(&self, host: &str) -> bool {
        if matches_any(&self.exclude, host) {
            return false;
        }
        match self.scope {
            Scope::All => true,
            Scope::Listed => matches_any(&self.domains, host),
        }
    }
}

/// Reduce whatever the user pasted to a bare hostname.
///
/// People paste addresses, not hostnames — `http://www.example.com/path?x=1`
/// is the normal thing to have on the clipboard. Treating that as a hostname
/// fails at name resolution, which then looks like "nothing works" rather than
/// "that is not a hostname".
pub fn normalise_host(input: &str) -> String {
    let mut host = input.trim();
    if let Some((_, rest)) = host.split_once("://") {
        host = rest;
    }
    // Strip credentials, then path, query and fragment.
    if let Some((_, rest)) = host.rsplit_once('@') {
        host = rest;
    }
    host = host.split(['/', '?', '#']).next().unwrap_or(host);
    // A bracketed IPv6 literal keeps its brackets; otherwise drop any :port.
    if !host.starts_with('[') {
        if let Some((name, port)) = host.rsplit_once(':') {
            if port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() {
                host = name;
            }
        }
    }
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// Match a hostname against one pattern.
///
/// - `example.com` matches the name and any subdomain
/// - `*.example.com` matches subdomains only
/// - `=example.com` matches exactly
pub fn matches(pattern: &str, host: &str) -> bool {
    let host = host.trim_end_matches('.');
    if let Some(exact) = pattern.strip_prefix('=') {
        return exact.eq_ignore_ascii_case(host);
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return is_subdomain_of(host, suffix);
    }
    pattern.eq_ignore_ascii_case(host) || is_subdomain_of(host, pattern)
}

/// Case-insensitive suffix test over bytes, so a non-ASCII hostname cannot
/// panic on a slice that splits a character.
fn ends_with_ignore_case(host: &str, suffix: &str) -> bool {
    let (host, suffix) = (host.as_bytes(), suffix.as_bytes());
    host.len() >= suffix.len() && host[host.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

/// True when `host` sits strictly below `domain`, on a label boundary — so
/// `notexample.com` does not count as being under `example.com`.
fn is_subdomain_of(host: &str, domain: &str) -> bool {
    host.len() > domain.len()
        && ends_with_ignore_case(host, domain)
        && host.as_bytes()[host.len() - domain.len() - 1] == b'.'
}

pub fn matches_any(patterns: &[String], host: &str) -> bool {
    patterns.iter().any(|p| matches(p, host))
}

/// The longest matching pattern wins, so `api.example.com` beats `example.com`.
fn best_match<'a>(map: &'a BTreeMap<String, Strategy>, host: &str) -> Option<&'a Strategy> {
    map.iter()
        .filter(|(pattern, _)| matches(pattern, host))
        .max_by_key(|(pattern, _)| pattern.len())
        .map(|(_, strategy)| strategy)
}

/// Preferences for the download window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Download {
    pub audio: AudioQuality,
    /// Language to prefer, as YouTube spells it — "ko", "en", and so on.
    /// Empty means whatever the video treats as its default track.
    pub audio_language: String,
    // Two former fields lived here and are gone: `include_video` (video-vs-music
    // is now a per-download row, not a preference) and `music_mp3` (the M4A/MP3
    // choice, now two rows in the download list). `#[serde(default)]` means an old
    // config still carrying either key simply ignores it.
}

impl Default for Download {
    fn default() -> Self {
        Self {
            audio: AudioQuality::Best,
            audio_language: String::new(),
        }
    }
}

/// How much of the size budget the sound is worth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioQuality {
    /// The best track available. On a video of a hundred megabytes the few
    /// megabytes this costs over the middle track are not worth arguing about.
    Best,
    /// Around 70 kbps: clear for speech, thin under music.
    Balanced,
    /// The smallest on offer.
    Small,
}

impl AudioQuality {
    pub fn label(self) -> &'static str {
        match self {
            AudioQuality::Best => "최상 (약 130k)",
            AudioQuality::Balanced => "보통 (약 70k)",
            AudioQuality::Small => "최소",
        }
    }

    pub const ALL: &'static [AudioQuality] =
        &[AudioQuality::Best, AudioQuality::Balanced, AudioQuality::Small];
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::Desync;

    #[test]
    fn pasted_urls_reduce_to_a_hostname() {
        assert_eq!(normalise_host("http://www.tcafe.net/"), "www.tcafe.net");
        assert_eq!(normalise_host("https://Example.COM:8443/a/b?c=1#d"), "example.com");
        assert_eq!(normalise_host("  example.com  "), "example.com");
        assert_eq!(normalise_host("user:pw@example.com/x"), "example.com");
        assert_eq!(normalise_host("example.com."), "example.com");
    }

    #[test]
    fn normalising_keeps_ipv6_literals_intact() {
        // The colons inside brackets are not a port separator.
        assert_eq!(normalise_host("https://[2001:db8::1]/x"), "[2001:db8::1]");
    }

    #[test]
    fn suffix_patterns_match_subdomains() {
        assert!(matches("example.com", "example.com"));
        assert!(matches("example.com", "api.example.com"));
        assert!(matches("example.com", "a.b.example.com"));
        // Must not match a name that merely ends with the same letters.
        assert!(!matches("example.com", "notexample.com"));
        assert!(!matches("example.com", "example.com.evil.net"));
    }

    #[test]
    fn wildcard_excludes_the_bare_domain() {
        assert!(matches("*.example.com", "api.example.com"));
        assert!(!matches("*.example.com", "example.com"));
    }

    #[test]
    fn exact_prefix_disables_suffix_matching() {
        assert!(matches("=example.com", "example.com"));
        assert!(!matches("=example.com", "api.example.com"));
    }

    #[test]
    fn trailing_dot_and_case_are_ignored() {
        assert!(matches("example.com", "API.Example.COM."));
    }

    #[test]
    fn exclude_wins_over_scope() {
        let cfg = Config {
            scope: Scope::All,
            exclude: vec!["bank.example".to_string()],
            ..Default::default()
        };
        assert!(cfg.applies_to("anything.test"));
        assert!(!cfg.applies_to("bank.example"));
        assert!(!cfg.applies_to("login.bank.example"));
    }

    #[test]
    fn listed_scope_requires_a_match() {
        let cfg = Config {
            scope: Scope::Listed,
            domains: vec!["example.com".to_string()],
            ..Default::default()
        };
        assert!(cfg.applies_to("api.example.com"));
        assert!(!cfg.applies_to("other.test"));
    }

    #[test]
    fn most_specific_override_wins() {
        let mut cfg = Config::default();
        cfg.overrides.insert(
            "example.com".to_string(),
            Strategy { desync: Desync::Split, ..Default::default() },
        );
        cfg.overrides.insert(
            "api.example.com".to_string(),
            Strategy { desync: Desync::FakeDisorder, ..Default::default() },
        );

        assert_eq!(cfg.strategy_for("api.example.com").desync, Desync::FakeDisorder);
        assert_eq!(cfg.strategy_for("www.example.com").desync, Desync::Split);
        // No override at all falls back to the global default.
        assert_eq!(cfg.strategy_for("other.test").desync, cfg.strategy.desync);
    }

    #[test]
    fn an_old_config_gets_the_new_default_strategy() {
        // The exact situation this exists for: a stored `fake_disorder` kept
        // overriding the safer default that replaced it.
        let mut cfg = Config {
            version: 0,
            strategy: Strategy { desync: Desync::FakeDisorder, ..Default::default() },
            domains: vec!["keep.example".to_string()],
            ..Default::default()
        };
        cfg.overrides.insert("=learned.example".into(), Strategy { desync: Desync::Split, ..Default::default() });

        assert!(cfg.migrate());
        assert_eq!(cfg.strategy, Strategy::default());
        assert_eq!(cfg.version, CURRENT_VERSION);
        // The user's own data must survive the reset.
        assert_eq!(cfg.domains, vec!["keep.example".to_string()]);
        assert_eq!(cfg.overrides.len(), 1, "probe results reflect what worked; keep them");
    }

    #[test]
    fn a_current_config_is_left_alone() {
        let mut cfg = Config {
            strategy: Strategy { desync: Desync::FakeDisorder, ..Default::default() },
            ..Default::default()
        };
        let before = cfg.clone();
        assert!(!cfg.migrate());
        assert_eq!(cfg, before, "a deliberate choice must not be overwritten");
    }

    #[test]
    fn config_round_trips_through_toml() {
        let mut cfg = Config::default();
        cfg.overrides.insert("a.test".to_string(), Strategy::passthrough());
        cfg.domains.push("b.test".to_string());
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg, back);
    }
}
