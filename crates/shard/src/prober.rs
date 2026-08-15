//! Automatic strategy discovery.
//!
//! Guessing which desync a given ISP falls for is exactly the part users get
//! stuck on. The prober walks the strategy ladder cheapest-first, opening a
//! real TLS handshake to the target through the live engine, and keeps the
//! first rung that gets a reply. That is why the ladder is ordered by cost:
//! stopping early means the steady-state overhead is the minimum that works.

use crate::engine::Shared;
use crate::strategy::Strategy;
use crossbeam_channel::{unbounded, Receiver};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
const READ_TIMEOUT: Duration = Duration::from_secs(4);
/// A blocked host often lets the very first connection through before the
/// middlebox catches up, so one success is not evidence.
const TRIALS: usize = 2;

#[derive(Clone, Debug)]
pub enum Progress {
    Started { host: String, rungs: usize },
    Dns { system: Option<String>, encrypted: Option<String>, tampered: bool },
    Baseline { reachable: bool, addr: SocketAddr },
    Attempt { index: usize, label: String, ok: bool, elapsed_ms: u64 },
    /// `silent` when every attempt ran out of time rather than being refused:
    /// the difference between a machine reading the handshake and an address
    /// that answers nothing at all.
    Finished { winner: Option<String>, silent: bool },
    Error(String),
}

/// What a probe concluded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The site works untouched; no strategy was saved.
    AlreadyWorks,
    /// The handshake was never the problem: the system resolver returns a
    /// different address than encrypted DNS does, and the encrypted one works.
    /// No desync can fix that — encrypted DNS is the fix.
    DnsTampered,
    /// A strategy was found and stored as a per-host override.
    Learned(String),
    /// Every rung failed.
    NoStrategy,
    Error(String),
}

/// Where the probe's address came from, and whether the two sources agree.
struct Resolution {
    addr: SocketAddr,
    system: Option<IpAddr>,
    encrypted: Option<IpAddr>,
}

impl Resolution {
    /// Disagreement between an ISP resolver and an encrypted one is the
    /// signature of DNS-level blocking.
    fn tampered(&self) -> bool {
        matches!((self.system, self.encrypted), (Some(a), Some(b)) if a != b)
    }
}

/// Run a probe on a background thread. The receiver closes when it finishes.
pub fn spawn(shared: Arc<Shared>, host: String) -> Receiver<Progress> {
    let (tx, rx) = unbounded();
    let _ = std::thread::Builder::new()
        .name("shard-prober".to_string())
        .spawn(move || {
            let report = |p: Progress| {
                let _ = tx.send(p);
            };
            execute(&shared, &host, &report);
        });
    rx
}

/// One step of a probe, as a line to put on screen: what happened, and whether
/// it went well (`None` where that does not apply).
///
/// Here rather than in a window, because both windows show the same run and two
/// wordings of the same step would be two accounts of it.
pub fn say(progress: Progress) -> Vec<(String, Option<bool>)> {
    match progress {
        Progress::Started { host, rungs } => {
            vec![(format!("{host} — {rungs}개 전략 시도"), None)]
        }
        Progress::Dns { system, encrypted, tampered } => {
            let describe = |a: &Option<String>| a.clone().unwrap_or_else(|| "실패".to_string());
            // A tick only when both answered and they agree. Encrypted DNS
            // failing takes half the ground from under everything below, and it
            // was being marked as though the line had passed.
            let well = !tampered && system.is_some() && encrypted.is_some();
            let mut lines = vec![(
                format!("DNS — 시스템 {} / 암호화 {}", describe(&system), describe(&encrypted)),
                Some(well),
            )];
            if tampered {
                lines.push(("두 응답이 다릅니다. DNS가 조작되고 있습니다.".to_string(), Some(false)));
            } else if encrypted.is_none() {
                lines.push((
                    "암호화 DNS가 이 이름에 답하지 않았습니다 — 상위 서버가 막고 있거나, 그 서버까지 닿지 못하고 있습니다."
                        .to_string(),
                    Some(false),
                ));
            }
            lines
        }
        // The address every rung below is tried against. Named, because a run
        // where everything fails at once is nearly always a run aimed at the
        // wrong place — and nothing else on screen would say so.
        Progress::Baseline { reachable, addr } => vec![(
            if reachable {
                format!("기준 연결 성공 ({}) — 우회가 필요 없습니다", addr.ip())
            } else {
                format!("기준 연결 차단됨 ({}) — 전략 탐색 시작", addr.ip())
            },
            Some(reachable),
        )],
        Progress::Attempt { index, label, ok, elapsed_ms } => {
            vec![(format!("{}. {label} ({elapsed_ms}ms)", index + 1), Some(ok))]
        }
        // How the attempts failed says which kind of block it is. Answered and
        // then cut off is a machine reading the handshake; never answered at all
        // is the address itself being unreachable, and no way of splitting a
        // packet changes where it is addressed to.
        Progress::Finished { winner, silent } => vec![match winner {
            Some(label) => (format!("성공: {label} — 저장됨"), Some(true)),
            None if silent => (
                "모든 시도가 응답 없이 끝났습니다 — 주소 자체가 막혀 있다는 뜻이며, ClientHello를 어떻게 나눠도 달라지지 않습니다. 이 경우 Veil이 답입니다."
                    .to_string(),
                Some(false),
            ),
            None => (
                "통하는 전략을 찾지 못했습니다. 연결은 닿았다가 끊겼으니 검사 장비가 조각을 다시 맞추고 있을 수 있습니다. 이 경우도 Veil이 답입니다."
                    .to_string(),
                Some(false),
            ),
        }],
        Progress::Error(e) => vec![(e, Some(false))],
    }
}

/// Whether a step means the run is over.
pub fn is_last(progress: &Progress) -> bool {
    matches!(progress, Progress::Finished { .. } | Progress::Error(_))
}

/// Run a probe on the calling thread, discarding progress. Used by the
/// automatic learner, which reports through the engine's event log instead.
pub fn probe_blocking(shared: &Arc<Shared>, host: &str) -> Outcome {
    execute(shared, host, &|_| {})
}

fn execute(shared: &Arc<Shared>, host: &str, report: &dyn Fn(Progress)) -> Outcome {
    let resolution = match resolve(shared, host) {
        Ok(r) => r,
        Err(e) => {
            let message = format!("{host} 주소를 확인할 수 없습니다: {e}");
            report(Progress::Error(message.clone()));
            return Outcome::Error(message);
        }
    };
    let addr = resolution.addr;
    let tampered = resolution.tampered();
    report(Progress::Dns {
        system: resolution.system.map(|a| a.to_string()),
        encrypted: resolution.encrypted.map(|a| a.to_string()),
        tampered,
    });

    // Our own failed attempts would otherwise look like fresh evidence that the
    // host is blocked and queue another probe behind this one.
    let _quiet = Quiet::new(shared, host);

    let ladder = Strategy::ladder();
    report(Progress::Started { host: host.to_string(), rungs: ladder.len() });

    // If the site already works untouched, applying a desync would be pure
    // overhead — and the per-domain override exists to record exactly that.
    let baseline = {
        let _guard = Override::install(shared, host, Strategy::passthrough());
        trial(addr, host)
    };
    report(Progress::Baseline { reachable: baseline, addr });
    if baseline {
        report(Progress::Finished { winner: None, silent: false });
        // Reaching the site at the encrypted answer's address while the system
        // resolver points somewhere else means the block was in DNS all along.
        return if tampered { Outcome::DnsTampered } else { Outcome::AlreadyWorks };
    }

    // Whether every attempt simply ran out of time. One that is refused comes
    // back in a fraction of a second; one that is never answered takes the whole
    // wait, and those are two different kinds of block.
    let mut all_silent = true;
    for (index, (label, strategy)) in ladder.into_iter().enumerate() {
        let started = Instant::now();
        let ok = {
            let _guard = Override::install(shared, host, strategy.clone());
            trial(addr, host)
        };
        let elapsed_ms = started.elapsed().as_millis() as u64;
        all_silent &= elapsed_ms >= (CONNECT_TIMEOUT + READ_TIMEOUT).as_millis() as u64;
        report(Progress::Attempt { index, label: label.to_string(), ok, elapsed_ms });

        if ok {
            commit(shared, host, strategy);
            report(Progress::Finished { winner: Some(label.to_string()), silent: false });
            return Outcome::Learned(label.to_string());
        }
    }
    report(Progress::Finished { winner: None, silent: all_silent });
    Outcome::NoStrategy
}

/// Suppresses block detection for one host while it is being probed.
struct Quiet<'a> {
    shared: &'a Shared,
    host: String,
}

impl<'a> Quiet<'a> {
    fn new(shared: &'a Shared, host: &str) -> Self {
        shared.probing.lock().insert(host.to_string());
        Self { shared, host: host.to_string() }
    }
}

impl Drop for Quiet<'_> {
    fn drop(&mut self) {
        self.shared.probing.lock().remove(&self.host);
    }
}

/// Resolve twice: once the way the operating system would, and once over
/// encrypted DNS. The encrypted answer is the one we trust and test against.
fn resolve(shared: &Shared, host: &str) -> anyhow::Result<Resolution> {
    let system = (host, 443u16).to_socket_addrs().ok().and_then(|mut a| a.next()).map(|a| a.ip());

    let doh_cfg = shared.config.read().doh.clone();
    let encrypted = crate::doh::resolve_encrypted(&doh_cfg, host);

    // Nothing is somewhere. A resolver that will not answer for a name often
    // says `0.0.0.0` rather than saying nothing, and a connection to that is
    // refused before it leaves the machine — which arrived here as thirteen
    // strategies failing in no time at all, reading exactly like a site blocked
    // by every means at once.
    let chosen = usable(encrypted).or(usable(system)).ok_or_else(|| {
        match (encrypted, system) {
            (Some(_), _) | (_, Some(_)) => anyhow::anyhow!(
                "리졸버가 주소 대신 {} 를 돌려줬습니다 — 이름 자체가 막혀 있다는 뜻입니다.",
                encrypted.or(system).map(|a| a.to_string()).unwrap_or_default()
            ),
            _ => anyhow::anyhow!("어느 리졸버에서도 주소를 얻지 못했습니다"),
        }
    })?;
    Ok(Resolution { addr: SocketAddr::new(chosen, 443), system, encrypted })
}

/// An address worth opening a connection to.
///
/// The unspecified address is a refusal written as a number; the loopback one
/// would be this machine answering itself.
fn usable(addr: Option<IpAddr>) -> Option<IpAddr> {
    addr.filter(|a| !a.is_unspecified() && !a.is_loopback() && !a.is_multicast())
}

/// A rung passes only if every trial succeeds.
fn trial(addr: SocketAddr, host: &str) -> bool {
    (0..TRIALS).all(|_| request_completes(addr, host))
}

/// Can a full HTTPS request to this host complete right now?
///
/// Uses whatever policy the engine currently has, so it answers "does the
/// configuration as it stands actually work" rather than testing a candidate.
pub fn reachable(shared: &Arc<Shared>, host: &str) -> bool {
    match resolve(shared, host) {
        Ok(resolution) => request_completes(resolution.addr, host),
        Err(_) => false,
    }
}

/// Complete a real HTTPS request.
///
/// The bar has to be the whole exchange, not merely "something came back".
/// Evading an SNI match is only half the job: a decoy that reaches the server,
/// or fragments it reassembles wrongly, still leaves a connection that answers
/// the first packet and then dies. Accepting that as success would save a
/// strategy the browser cannot actually use — which is exactly what a probe is
/// supposed to rule out.
fn request_completes(addr: SocketAddr, host: &str) -> bool {
    // Said out loud when it goes wrong. Everything below answers with a plain
    // yes or no, and a run where every rung failed in no time at all is a run
    // that never reached the network — which reads the same as "all blocked"
    // unless the reason is written down somewhere.
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::warn!("probe could not start a runtime: {e}");
            return false;
        }
    };
    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(READ_TIMEOUT + CONNECT_TIMEOUT)
            // Pin the address so every rung is tested against the same server,
            // and never reuse a connection — a pooled one would skip the
            // handshake the desync exists to protect.
            .resolve(host, addr)
            .pool_max_idle_per_host(0)
            .user_agent("Mozilla/5.0")
            .build();
        let client = match client {
            Ok(client) => client,
            Err(e) => {
                tracing::warn!("probe could not build a client: {e}");
                return false;
            }
        };
        match client.get(format!("https://{host}/")).send().await {
            Ok(_) => true,
            Err(e) => {
                tracing::info!("probe request to {host} at {addr} failed: {e}");
                false
            }
        }
    })
}

/// Persist a winning strategy as a permanent per-host override.
fn commit(shared: &Shared, host: &str, strategy: Strategy) {
    let mut cfg = shared.config.write();
    cfg.overrides.insert(exact_key(host), strategy);
    if cfg.scope == crate::config::Scope::Listed && !cfg.applies_to(host) {
        cfg.domains.push(host.to_string());
    }
    if let Err(e) = cfg.save() {
        tracing::error!("could not save probe result: {e}");
    }
}

/// Probing must not disturb policy for any other host, so overrides are keyed
/// as exact matches — which also outrank any suffix pattern already present.
fn exact_key(host: &str) -> String {
    format!("={host}")
}

/// Applies a strategy for the duration of one trial and puts the config back
/// afterwards, including on an early return.
struct Override<'a> {
    shared: &'a Shared,
    key: String,
    previous: Option<Strategy>,
    added_domain: bool,
}

impl<'a> Override<'a> {
    fn install(shared: &'a Shared, host: &str, strategy: Strategy) -> Self {
        let key = exact_key(host);
        let mut cfg = shared.config.write();
        let previous = cfg.overrides.insert(key.clone(), strategy);
        // With a domain list in force the engine would otherwise ignore the
        // probe entirely and every rung would look identical.
        let added_domain = if cfg.scope == crate::config::Scope::Listed && !cfg.applies_to(host) {
            cfg.domains.push(host.to_string());
            true
        } else {
            false
        };
        drop(cfg);
        Self { shared, key, previous, added_domain }
    }
}

impl Drop for Override<'_> {
    fn drop(&mut self) {
        let mut cfg = self.shared.config.write();
        match self.previous.take() {
            Some(prev) => {
                cfg.overrides.insert(self.key.clone(), prev);
            }
            None => {
                cfg.overrides.remove(&self.key);
            }
        }
        if self.added_domain {
            cfg.domains.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Scope};
    use crate::desync;
    use crate::strategy::Desync;

    #[test]
    fn the_probe_can_build_the_client_it_tests_with() {
        // Nothing here touches the network. It is the step before that: if the
        // client cannot be built at all, every rung fails in no time and the
        // screen reads as though the whole internet were blocked.
        let client = reqwest::Client::builder()
            .timeout(READ_TIMEOUT + CONNECT_TIMEOUT)
            .resolve("example.com", "93.184.216.34:443".parse().unwrap())
            .pool_max_idle_per_host(0)
            .user_agent("Mozilla/5.0")
            .build();
        assert!(client.is_ok(), "{:?}", client.err());
    }

    #[test]
    fn an_address_that_is_a_refusal_is_not_an_address() {
        use std::net::{Ipv4Addr, Ipv6Addr};
        assert_eq!(usable(Some(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)))), Some(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4))));
        // What a resolver returns for a name it will not answer for.
        assert_eq!(usable(Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED))), None);
        assert_eq!(usable(Some(IpAddr::V6(Ipv6Addr::UNSPECIFIED))), None);
        assert_eq!(usable(Some(IpAddr::V4(Ipv4Addr::LOCALHOST))), None);
        assert_eq!(usable(None), None);
    }

    #[test]
    fn exact_keys_outrank_suffix_patterns() {
        let mut cfg = Config::default();
        cfg.overrides.insert(
            "example.com".to_string(),
            Strategy { desync: Desync::Split, ..Default::default() },
        );
        cfg.overrides.insert(
            exact_key("example.com"),
            Strategy { desync: Desync::FakeDisorder, ..Default::default() },
        );
        assert_eq!(cfg.strategy_for("example.com").desync, Desync::FakeDisorder);
        // A subdomain still falls back to the broader suffix rule.
        assert_eq!(cfg.strategy_for("api.example.com").desync, Desync::Split);
    }

    #[test]
    fn override_guard_restores_an_absent_entry() {
        let shared = Shared::new(Config::default());
        {
            let _g = Override::install(&shared, "probe.test", Strategy::passthrough());
            assert!(shared.config.read().overrides.contains_key(&exact_key("probe.test")));
        }
        assert!(!shared.config.read().overrides.contains_key(&exact_key("probe.test")));
    }

    #[test]
    fn override_guard_restores_a_previous_entry() {
        let mut cfg = Config::default();
        cfg.overrides.insert(
            exact_key("probe.test"),
            Strategy { desync: Desync::Split, ..Default::default() },
        );
        let shared = Shared::new(cfg);
        {
            let _g = Override::install(&shared, "probe.test", Strategy::passthrough());
            assert_eq!(shared.config.read().strategy_for("probe.test").desync, Desync::None);
        }
        assert_eq!(shared.config.read().strategy_for("probe.test").desync, Desync::Split);
    }

    #[test]
    fn probing_temporarily_widens_a_listed_scope() {
        let cfg = Config { scope: Scope::Listed, domains: vec![], ..Default::default() };
        let shared = Shared::new(cfg);
        {
            let _g = Override::install(&shared, "probe.test", Strategy::passthrough());
            assert!(shared.config.read().applies_to("probe.test"));
        }
        assert!(!shared.config.read().applies_to("probe.test"));
    }

    #[test]
    fn probe_client_hello_carries_the_target_name() {
        let hello = desync::build_client_hello("probe.test", 0);
        let sni = crate::parse::tls::client_hello_sni(&hello).unwrap();
        assert_eq!(sni.name, "probe.test");
    }
}
