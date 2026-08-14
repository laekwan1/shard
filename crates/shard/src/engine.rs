//! The packet intercept loop, and the learner that sits on top of it.
//!
//! One WinDivert handle diverts outbound connection-opening packets, which we
//! rewrite and re-inject. A second handle *sniffs* inbound packets — sniffing
//! does not delay them — for three things: how many hops away each server is
//! (so decoys get a TTL that clears every middlebox but never arrives), resets
//! injected by a middlebox, and whether a handshake was ever answered.
//!
//! The last two are what make the on/off switch sufficient on its own. A host
//! the default strategy fails on is noticed, probed in the background, and
//! given its own strategy without the user doing anything.

use crate::config::{Config, Scope};
use crate::desync::{self, Emit, QuicAction};
use crate::net::{self, FlowKey, Layout};
use crate::parse::{self, http, quic, tls};
use crate::prober::{self, Outcome};
use crate::windivert::{ffi, recalc_checksums, Diverter};

use anyhow::{Context, Result};
use parking_lot::{Mutex, RwLock};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Keep the UI's activity list bounded.
const MAX_EVENTS: usize = 250;
/// A reset this soon after a handshake is a middlebox, not a busy server.
const RESET_WINDOW: Duration = Duration::from_secs(6);
/// How long to wait for a reply before calling a handshake unanswered.
const SILENT_TIMEOUT: Duration = Duration::from_secs(8);
/// Flow records are only interesting until one of the above fires.
const FLOW_TTL: Duration = Duration::from_secs(60);
/// Cap the flow table so a scan or a very busy link cannot grow it without end.
const MAX_FLOWS: usize = 4096;
/// Evidence older than this is stale; a site that failed an hour ago and works
/// now should not be probed on the strength of that.
const SUSPICION_TTL: Duration = Duration::from_secs(300);
/// How long the remainder of a split ClientHello is expected to arrive within.
/// The segments are sent back to back, so this only has to survive scheduling.
const HANDSHAKE_WINDOW: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct Stats {
    pub packets_seen: AtomicU64,
    pub tls_handled: AtomicU64,
    pub http_handled: AtomicU64,
    pub quic_dropped: AtomicU64,
    pub quic_decoyed: AtomicU64,
    pub decoys_sent: AtomicU64,
    pub fragments_sent: AtomicU64,
    pub passed_through: AtomicU64,
    pub blocks_detected: AtomicU64,
    pub probes_run: AtomicU64,
    pub strategies_learned: AtomicU64,
    /// TLS handshake records we could not read a hostname out of. A high count
    /// means the engine is seeing the traffic but failing to act on it, which
    /// looks identical to doing nothing at all from the outside.
    pub tls_unparsed: AtomicU64,
    /// Later segments of a ClientHello that did not fit in one packet.
    pub handshake_continuations: AtomicU64,
    pub errors: AtomicU64,
}

/// Plain copy of the counters for rendering.
#[derive(Clone, Copy, Default)]
pub struct StatsSnapshot {
    pub packets_seen: u64,
    pub tls_handled: u64,
    pub http_handled: u64,
    pub quic_dropped: u64,
    pub quic_decoyed: u64,
    pub decoys_sent: u64,
    pub fragments_sent: u64,
    pub passed_through: u64,
    pub blocks_detected: u64,
    pub probes_run: u64,
    pub strategies_learned: u64,
    pub tls_unparsed: u64,
    pub handshake_continuations: u64,
    pub errors: u64,
}

impl Stats {
    pub fn snapshot(&self) -> StatsSnapshot {
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        StatsSnapshot {
            packets_seen: get(&self.packets_seen),
            tls_handled: get(&self.tls_handled),
            http_handled: get(&self.http_handled),
            quic_dropped: get(&self.quic_dropped),
            quic_decoyed: get(&self.quic_decoyed),
            decoys_sent: get(&self.decoys_sent),
            fragments_sent: get(&self.fragments_sent),
            passed_through: get(&self.passed_through),
            blocks_detected: get(&self.blocks_detected),
            probes_run: get(&self.probes_run),
            strategies_learned: get(&self.strategies_learned),
            tls_unparsed: get(&self.tls_unparsed),
            handshake_continuations: get(&self.handshake_continuations),
            errors: get(&self.errors),
        }
    }
}

#[derive(Clone)]
pub struct Event {
    pub at: Instant,
    pub host: String,
    pub action: String,
}

/// A connection we acted on, kept so an inbound reset or silence can be
/// attributed back to the hostname that caused it.
struct FlowRecord {
    host: String,
    at: Instant,
    answered: bool,
}

/// Accumulated evidence that a host is being blocked.
struct Suspect {
    failures: u8,
    last: Instant,
}

/// State shared between the packet workers, the learner and the UI.
pub struct Shared {
    pub config: RwLock<Config>,
    pub stats: Stats,
    /// Measured hop count per destination address.
    pub hops: RwLock<HashMap<[u8; 16], u8>>,
    /// Address to hostname, populated by the DoH forwarder. Lets the engine
    /// apply per-domain policy to QUIC, whose hostname is encrypted.
    pub dns: RwLock<HashMap<[u8; 16], String>>,
    pub events: Mutex<VecDeque<Event>>,
    flows: Mutex<HashMap<FlowKey, FlowRecord>>,
    /// Flows whose ClientHello arrived incomplete, with the deadline for
    /// treating further segments as part of it.
    continuations: Mutex<HashMap<FlowKey, Instant>>,
    suspects: Mutex<HashMap<String, Suspect>>,
    /// Hosts currently being probed. The prober's own failed attempts must not
    /// count as fresh evidence, or every probe would queue another one.
    pub probing: Mutex<HashSet<String>>,
    cooldown: Mutex<HashMap<String, Instant>>,
}

impl Shared {
    pub fn new(config: Config) -> Arc<Self> {
        Arc::new(Self {
            config: RwLock::new(config),
            stats: Stats::default(),
            hops: RwLock::new(HashMap::new()),
            dns: RwLock::new(HashMap::new()),
            events: Mutex::new(VecDeque::with_capacity(MAX_EVENTS)),
            flows: Mutex::new(HashMap::new()),
            continuations: Mutex::new(HashMap::new()),
            suspects: Mutex::new(HashMap::new()),
            probing: Mutex::new(HashSet::new()),
            cooldown: Mutex::new(HashMap::new()),
        })
    }

    pub fn record(&self, host: &str, action: impl Into<String>) {
        let action = action.into();
        // Mirror to the log file. The in-memory list is only visible inside the
        // window, which is no use when something has to be diagnosed after the
        // fact or from outside the process.
        tracing::info!(target: "activity", "{host} — {action}");
        let mut events = self.events.lock();
        if events.len() == MAX_EVENTS {
            events.pop_front();
        }
        events.push_back(Event { at: Instant::now(), host: host.to_string(), action });
    }

    pub fn recent_events(&self) -> Vec<Event> {
        self.events.lock().iter().rev().cloned().collect()
    }

    /// Remember which host an address belongs to, for QUIC and for connections
    /// that carry no SNI.
    pub fn note_resolution(&self, addr: [u8; 16], host: &str) {
        self.dns.write().insert(addr, host.to_string());
    }

    /// Note that we acted on a connection, so its fate can be observed.
    fn track_flow(&self, key: FlowKey, host: &str) {
        let mut flows = self.flows.lock();
        if flows.len() >= MAX_FLOWS {
            // Cheapest sound eviction: drop the oldest half in one pass rather
            // than sorting the whole table on the packet path.
            let cutoff = Instant::now() - RESET_WINDOW;
            flows.retain(|_, record| record.at > cutoff);
            if flows.len() >= MAX_FLOWS {
                flows.clear();
            }
        }
        flows.insert(key, FlowRecord { host: host.to_string(), at: Instant::now(), answered: false });
    }

    fn mark_answered(&self, key: &FlowKey) {
        if let Some(record) = self.flows.lock().get_mut(key) {
            record.answered = true;
        }
    }

    /// A reset arrived for a connection we handled.
    fn note_reset(&self, key: &FlowKey) {
        let host = {
            let mut flows = self.flows.lock();
            match flows.get(key) {
                Some(record) if !record.answered && record.at.elapsed() < RESET_WINDOW => {
                    let host = record.host.clone();
                    flows.remove(key);
                    host
                }
                _ => return,
            }
        };
        self.note_failure(&host, "리셋 주입 감지");
    }

    fn note_failure(&self, host: &str, reason: &str) {
        if self.probing.lock().contains(host) {
            return;
        }
        // The decoy host is contacted constantly by our own decoys and never by
        // the user. Treating it as a site that keeps failing would have the
        // learner probe it forever for no reason.
        if self.config.read().strategy.decoy_host.eq_ignore_ascii_case(host) {
            return;
        }
        self.stats.blocks_detected.fetch_add(1, Ordering::Relaxed);
        let mut suspects = self.suspects.lock();
        let entry = suspects.entry(host.to_string()).or_insert(Suspect { failures: 0, last: Instant::now() });
        // Stale evidence should not accumulate across hours.
        if entry.last.elapsed() > SUSPICION_TTL {
            entry.failures = 0;
        }
        entry.failures = entry.failures.saturating_add(1);
        entry.last = Instant::now();
        let failures = entry.failures;
        drop(suspects);
        self.record(host, format!("{reason} ({failures}회)"));
    }

    /// Retire flow records, emitting a failure for handshakes that were never
    /// answered. Returns nothing; evidence lands in `suspects`.
    fn prune_flows(&self, detect_silent: bool) {
        let mut unanswered = Vec::new();
        {
            let mut flows = self.flows.lock();
            flows.retain(|_, record| {
                let age = record.at.elapsed();
                if record.answered {
                    return age < RESET_WINDOW;
                }
                if detect_silent && age > SILENT_TIMEOUT {
                    unanswered.push(record.host.clone());
                    return false;
                }
                age < FLOW_TTL
            });
        }
        for host in unanswered {
            self.note_failure(&host, "응답 없음");
        }
    }

    /// The next host worth probing, if any.
    fn next_candidate(&self, threshold: u8, cooldown: Duration) -> Option<String> {
        let mut suspects = self.suspects.lock();
        let cooldowns = self.cooldown.lock();
        let candidate = suspects
            .iter()
            .filter(|(host, suspect)| {
                suspect.failures >= threshold
                    && suspect.last.elapsed() < SUSPICION_TTL
                    && cooldowns.get(*host).is_none_or(|last| last.elapsed() > cooldown)
            })
            // Most evidence first, so the worst offender is handled soonest.
            .max_by_key(|(_, suspect)| suspect.failures)
            .map(|(host, _)| host.clone());
        if let Some(host) = &candidate {
            suspects.remove(host);
        }
        candidate
    }

    pub fn suspect_count(&self) -> usize {
        self.suspects.lock().len()
    }

    /// Note that a ClientHello did not fit in one segment, so the rest of it is
    /// still to come on this flow.
    fn expect_continuation(&self, key: FlowKey) {
        let mut pending = self.continuations.lock();
        if pending.len() > MAX_FLOWS {
            let cutoff = Instant::now();
            pending.retain(|_, deadline| *deadline > cutoff);
        }
        pending.insert(key, Instant::now() + HANDSHAKE_WINDOW);
    }

    /// Is this segment the rest of a ClientHello we already started on?
    ///
    /// It matters because a middlebox reassembles the segments before matching:
    /// desyncing only the first one leaves the hostname perfectly readable in
    /// the joined stream.
    fn is_continuation(&self, key: &FlowKey) -> bool {
        let mut pending = self.continuations.lock();
        match pending.get(key) {
            Some(deadline) if *deadline > Instant::now() => true,
            Some(_) => {
                pending.remove(key);
                false
            }
            None => false,
        }
    }

    fn finish_continuation(&self, key: &FlowKey) {
        self.continuations.lock().remove(key);
    }
}

pub struct Engine {
    shared: Arc<Shared>,
    running: Arc<AtomicBool>,
    diverters: Vec<Arc<Diverter>>,
    threads: Vec<JoinHandle<()>>,
}

impl Engine {
    /// Open the driver and spawn the workers.
    pub fn start(shared: Arc<Shared>) -> Result<Self> {
        let (outbound, inbound, workers, auto_learn) = {
            let cfg = shared.config.read();
            (
                outbound_filter(&cfg.ports),
                inbound_filter(&cfg.ports, cfg.auto_learn, cfg.detect_silent_drops),
                cfg.worker_threads.clamp(1, 8),
                cfg.auto_learn,
            )
        };

        let divert = Arc::new(
            Diverter::open(&outbound, 0, 0).context("아웃바운드 필터를 여는 데 실패했습니다")?,
        );
        // A deep queue absorbs bursts; without it the driver drops packets under
        // load and connections stall rather than merely slowing down.
        divert.set_param(ffi::WINDIVERT_PARAM_QUEUE_LENGTH, 8192)?;
        divert.set_param(ffi::WINDIVERT_PARAM_QUEUE_TIME, 4000)?;

        let sniff = Arc::new(
            Diverter::open(
                &inbound,
                -1000,
                ffi::WINDIVERT_FLAG_SNIFF | ffi::WINDIVERT_FLAG_RECV_ONLY,
            )
            .context("인바운드 스니퍼를 여는 데 실패했습니다")?,
        );

        let running = Arc::new(AtomicBool::new(true));
        let mut threads = Vec::new();

        for id in 0..workers {
            let shared = shared.clone();
            let divert = divert.clone();
            let running = running.clone();
            threads.push(
                std::thread::Builder::new()
                    .name(format!("shard-worker-{id}"))
                    .spawn(move || worker_loop(shared, divert, running))?,
            );
        }
        {
            let shared = shared.clone();
            let sniff = sniff.clone();
            let running = running.clone();
            threads.push(
                std::thread::Builder::new()
                    .name("shard-observer".to_string())
                    .spawn(move || observe_loop(shared, sniff, running))?,
            );
        }
        if auto_learn {
            let shared = shared.clone();
            let running = running.clone();
            threads.push(
                std::thread::Builder::new()
                    .name("shard-learner".to_string())
                    .spawn(move || learn_loop(shared, running))?,
            );
        }

        tracing::info!("engine started with {workers} workers, auto_learn={auto_learn}");
        Ok(Self { shared, running, diverters: vec![divert, sniff], threads })
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        // Unblock the receives so the threads can observe the flag.
        for d in &self.diverters {
            d.shutdown();
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        self.diverters.clear();
        tracing::info!("engine stopped");
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if !self.threads.is_empty() {
            self.stop();
        }
    }
}

/// WinDivert's filter language runs in the kernel, so narrowing here is far
/// cheaper than filtering in user space.
fn outbound_filter(ports: &[u16]) -> String {
    let ports = normalise_ports(ports);
    let tcp = ports.iter().map(|p| format!("tcp.DstPort == {p}")).collect::<Vec<_>>().join(" or ");
    // Only packets that carry data can hold a ClientHello or a request line,
    // so pure ACKs never leave the kernel.
    format!("outbound and !loopback and ((tcp.PayloadLength > 0 and ({tcp})) or udp.DstPort == 443)")
}

/// The sniffer's reach depends on what the learner needs to see.
///
/// SYN-ACKs are always wanted for hop measurement. Resets are nearly free and
/// catch injected blocks. Inbound data packets are what reveal a *silent* drop,
/// but they are also the bulk of a busy link's traffic, which is why that is a
/// separate switch.
fn inbound_filter(ports: &[u16], auto_learn: bool, detect_silent: bool) -> String {
    let ports = normalise_ports(ports);
    let src = ports.iter().map(|p| format!("tcp.SrcPort == {p}")).collect::<Vec<_>>().join(" or ");

    let mut events = vec!["(tcp.Syn == 1 and tcp.Ack == 1)".to_string()];
    if auto_learn {
        events.push("tcp.Rst == 1".to_string());
        if detect_silent {
            events.push("tcp.PayloadLength > 0".to_string());
        }
    }
    format!("inbound and !loopback and ({src}) and ({})", events.join(" or "))
}

fn normalise_ports(ports: &[u16]) -> Vec<u16> {
    if ports.is_empty() {
        vec![80, 443]
    } else {
        ports.to_vec()
    }
}

fn worker_loop(shared: Arc<Shared>, divert: Arc<Diverter>, running: Arc<AtomicBool>) {
    let mut buf = vec![0u8; ffi::WINDIVERT_MTU_MAX];
    let mut addr = ffi::WinDivertAddress::default();

    while running.load(Ordering::Relaxed) {
        let len = match divert.recv(&mut buf, &mut addr) {
            Ok(Some(n)) => n,
            Ok(None) => break,
            Err(e) => {
                shared.stats.errors.fetch_add(1, Ordering::Relaxed);
                tracing::warn!("recv failed: {e}");
                continue;
            }
        };
        shared.stats.packets_seen.fetch_add(1, Ordering::Relaxed);

        let forward = match handle(&shared, &divert, &buf[..len], &addr) {
            Verdict::Forward => true,
            Verdict::Drop => false,
        };
        if forward {
            shared.stats.passed_through.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = divert.send(&buf[..len], &addr) {
                shared.stats.errors.fetch_add(1, Ordering::Relaxed);
                tracing::warn!("reinject failed: {e}");
            }
        }
    }
}

/// Watch inbound packets: learn hop distances, and notice connections that a
/// middlebox killed.
fn observe_loop(shared: Arc<Shared>, sniff: Arc<Diverter>, running: Arc<AtomicBool>) {
    let mut buf = vec![0u8; ffi::WINDIVERT_MTU_MAX];
    let mut addr = ffi::WinDivertAddress::default();

    while running.load(Ordering::Relaxed) {
        let len = match sniff.recv(&mut buf, &mut addr) {
            Ok(Some(n)) => n,
            Ok(None) => break,
            Err(_) => continue,
        };
        let pkt = &buf[..len];
        let Some(l) = net::parse(pkt) else { continue };
        if l.proto != net::PROTO_TCP {
            continue;
        }

        let flags = net::tcp_flags(pkt, &l);
        let source = net::src_addr(pkt, &l);

        // Any packet from the server reveals the distance, not just the
        // SYN-ACK. Learning from all of them closes the race where the first
        // request goes out before the handshake has been observed — and
        // without a distance the engine has to skip the decoy entirely.
        if !shared.hops.read().contains_key(&source) {
            if let Some(hops) = desync::hops_from_ttl(net::ttl(pkt, &l)) {
                shared.hops.write().insert(source, hops);
            }
        }
        if flags & net::TCP_SYN != 0 {
            continue;
        }

        let key = reverse_flow_key(pkt, &l);
        if flags & net::TCP_RST != 0 {
            shared.note_reset(&key);
        } else if l.payload_len > 0 {
            // The server answered, so this connection was never blocked.
            shared.mark_answered(&key);
        }
    }
}

/// Probe hosts that keep failing, and retire stale evidence.
fn learn_loop(shared: Arc<Shared>, running: Arc<AtomicBool>) {
    while running.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_secs(1));
        if !running.load(Ordering::Relaxed) {
            break;
        }

        let (enabled, detect_silent, threshold, cooldown) = {
            let cfg = shared.config.read();
            (
                cfg.auto_learn,
                cfg.detect_silent_drops,
                cfg.auto_learn_threshold.max(1),
                Duration::from_secs(cfg.auto_learn_cooldown_min.max(1) as u64 * 60),
            )
        };
        if !enabled {
            continue;
        }

        shared.prune_flows(detect_silent);

        let Some(host) = shared.next_candidate(threshold, cooldown) else { continue };
        shared.cooldown.lock().insert(host.clone(), Instant::now());
        shared.stats.probes_run.fetch_add(1, Ordering::Relaxed);
        shared.record(&host, "자동 탐색 시작");

        match prober::probe_blocking(&shared, &host) {
            Outcome::Learned(label) => {
                shared.stats.strategies_learned.fetch_add(1, Ordering::Relaxed);
                shared.record(&host, format!("자동 학습 성공: {label}"));
            }
            Outcome::AlreadyWorks => shared.record(&host, "차단이 아니었습니다"),
            Outcome::DnsTampered => shared.record(
                &host,
                "DNS 차단입니다 — DNS 탭에서 '시스템 DNS를 포워더로 변경'을 켜세요",
            ),
            Outcome::NoStrategy => shared.record(&host, "통하는 전략을 찾지 못했습니다"),
            Outcome::Error(e) => shared.record(&host, format!("탐색 실패: {e}")),
        }
    }
}

/// An inbound packet's flow seen from the outbound side, so it matches what
/// `track_flow` stored.
fn reverse_flow_key(pkt: &[u8], l: &Layout) -> FlowKey {
    FlowKey {
        src_port: net::dst_port(pkt, l),
        dst_port: net::src_port(pkt, l),
        dst: net::src_addr(pkt, l),
    }
}

enum Verdict {
    Forward,
    Drop,
}

fn handle(shared: &Shared, divert: &Diverter, pkt: &[u8], addr: &ffi::WinDivertAddress) -> Verdict {
    let Some(l) = net::parse(pkt) else { return Verdict::Forward };
    match l.proto {
        net::PROTO_TCP => handle_tcp(shared, divert, pkt, &l, addr),
        net::PROTO_UDP => handle_udp(shared, divert, pkt, &l, addr),
        _ => Verdict::Forward,
    }
}

fn handle_tcp(
    shared: &Shared,
    divert: &Diverter,
    pkt: &[u8],
    l: &Layout,
    addr: &ffi::WinDivertAddress,
) -> Verdict {
    let payload = l.payload(pkt);
    if payload.is_empty() {
        return Verdict::Forward;
    }

    // Only the packet that opens a connection carries a hostname; everything
    // after it falls straight through, which is why steady-state throughput is
    // unaffected.
    let tls_host = tls::client_hello_sni(payload);
    let http_host = if tls_host.is_none() { http::host_header(payload) } else { None };

    // A hostname is preferred but not required. Chrome's ClientHello runs to
    // about 2 KB once post-quantum key shares are included, and it randomises
    // extension order — so `server_name` regularly lands in the *second* TCP
    // segment, out of reach. Skipping those connections would mean skipping
    // most real browsing, which is exactly the case that matters.
    let named_host = match (&tls_host, &http_host) {
        (Some(h), _) => Some(h.name.clone()),
        (None, Some(h)) => Some(h.host.name.clone()),
        (None, None) => {
            let flow = net::flow_key(pkt, l);
            if tls::is_handshake(payload) {
                shared.stats.tls_unparsed.fetch_add(1, Ordering::Relaxed);
                // The rest of this hello is still coming, and it has to be
                // desynced too or the middlebox simply reads the hostname out
                // of the reassembled pair.
                shared.expect_continuation(flow);
            } else if shared.is_continuation(&flow) {
                shared.stats.handshake_continuations.fetch_add(1, Ordering::Relaxed);
                shared.finish_continuation(&flow);
            } else {
                return Verdict::Forward;
            }
            // Fall back to whatever the DoH forwarder learned about this
            // address; otherwise the connection is anonymous to us and only a
            // whole-traffic policy can apply.
            shared.dns.read().get(&net::dst_addr(pkt, l)).cloned()
        }
    };
    let recognised = tls_host.is_some() || http_host.is_some();

    let (strategy, auto_learn) = {
        let cfg = shared.config.read();
        let applies = match (&named_host, cfg.scope) {
            (Some(h), _) => cfg.applies_to(h),
            // Nothing to match a list against, so only a blanket policy can
            // decide — and a list in force means "leave the rest alone".
            (None, Scope::All) => true,
            (None, Scope::Listed) => false,
        };
        if !applies {
            return Verdict::Forward;
        }
        let strategy = match &named_host {
            Some(h) => cfg.strategy_for(h).clone(),
            None => cfg.strategy.clone(),
        };
        (strategy, cfg.auto_learn)
    };

    let host = named_host.unwrap_or_else(|| net::format_addr(&net::dst_addr(pkt, l), l.ipv6));

    // Remember the mapping so QUIC to the same address can be policed too.
    if recognised {
        shared.note_resolution(net::dst_addr(pkt, l), &host);
    }
    if auto_learn {
        shared.track_flow(net::flow_key(pkt, l), &host);
    }

    let (payload_buf, split_hint, protocol) = match (&tls_host, &http_host) {
        (Some(h), _) => {
            shared.stats.tls_handled.fetch_add(1, Ordering::Relaxed);
            (None, Some(h.midpoint()), desync::Protocol::Tls)
        }
        (None, Some(h)) => {
            shared.stats.http_handled.fetch_add(1, Ordering::Relaxed);
            match desync::mangle_http(payload, h, &strategy) {
                Some((buf, host_offset)) => {
                    let hint = strategy.http_split.then(|| host_offset + h.host.len / 2);
                    (Some(buf), hint, desync::Protocol::Http)
                }
                None => (
                    None,
                    strategy.http_split.then(|| h.host.midpoint()),
                    desync::Protocol::Http,
                ),
            }
        }
        // A handshake we could not read. The decoy needs no hostname, and a
        // split falls back to the configured offset.
        (None, None) => {
            shared.stats.tls_handled.fetch_add(1, Ordering::Relaxed);
            (None, None, desync::Protocol::Tls)
        }
    };
    let payload_ref: &[u8] = payload_buf.as_deref().unwrap_or(payload);

    let hops = shared.hops.read().get(&net::dst_addr(pkt, l)).copied();
    let Some(emits) = desync::plan_tcp(pkt, l, payload_ref, split_hint, &strategy, hops, protocol)
    else {
        return Verdict::Forward;
    };

    // The plan may have dropped the decoy when the hop distance was unknown, so
    // count from what it actually produced rather than from the strategy.
    let fragments = 1 + strategy.extra_splits as usize;
    let decoys = emits.len().saturating_sub(if strategy.desync.splits() { fragments } else { 1 });
    for (i, emit) in emits.into_iter().enumerate() {
        if i < decoys {
            shared.stats.decoys_sent.fetch_add(1, Ordering::Relaxed);
        } else {
            shared.stats.fragments_sent.fetch_add(1, Ordering::Relaxed);
        }
        send(shared, divert, emit, l, addr);
    }

    // Log what was done to this connection. Without it the activity view stays
    // empty even while the engine is busy, which reads as "nothing is
    // happening" — the opposite of the truth.
    shared.record(
        &host,
        format!(
            "{}{} · {}",
            strategy.desync.label(),
            if decoys == 0 && strategy.desync.uses_fake() { " (디코이 생략)" } else { "" },
            match hops {
                Some(h) => format!("{h}홉"),
                None => "거리 미측정".to_string(),
            }
        ),
    );
    Verdict::Drop
}

fn handle_udp(
    shared: &Shared,
    divert: &Diverter,
    pkt: &[u8],
    l: &Layout,
    addr: &ffi::WinDivertAddress,
) -> Verdict {
    if net::dst_port(pkt, l) != 443 {
        return Verdict::Forward;
    }
    let payload = l.payload(pkt);
    if !quic::is_connection_start(payload) {
        return Verdict::Forward;
    }

    let dst = net::dst_addr(pkt, l);

    // The hostname, preferably from the packet itself.
    //
    // A QUIC Initial is encrypted under keys derived from the connection ID it
    // prints in its own header, so the name inside is readable — see
    // `parse::quic_initial`. That beats the address map on both counts: one
    // address serves many domains at the big CDNs, and a name resolved before
    // Shard started, or by another resolver, is not in the map at all. The map
    // stays as the fallback for the packets this cannot open — a resumed
    // connection, or a ClientHello spread across datagrams.
    let host = parse::quic_initial::sni(payload)
        .map(|found| found.name)
        .or_else(|| shared.dns.read().get(&dst).cloned());

    let strategy = {
        let cfg = shared.config.read();
        let applies = match (&host, cfg.scope) {
            (Some(h), _) => cfg.applies_to(h),
            // With no hostname we cannot honour a list, so leave it alone.
            (None, Scope::Listed) => false,
            (None, Scope::All) => true,
        };
        if !applies {
            return Verdict::Forward;
        }
        match &host {
            Some(h) => cfg.strategy_for(h).clone(),
            None => cfg.strategy.clone(),
        }
    };

    let label = host.unwrap_or_else(|| net::format_addr(&dst, l.ipv6));
    let hops = shared.hops.read().get(&dst).copied();

    match desync::plan_quic(pkt, l, &strategy, hops) {
        QuicAction::Pass => Verdict::Forward,
        QuicAction::Drop => {
            shared.stats.quic_dropped.fetch_add(1, Ordering::Relaxed);
            let _ = label;
            Verdict::Drop
        }
        QuicAction::Decoy(emit) => {
            shared.stats.quic_decoyed.fetch_add(1, Ordering::Relaxed);
            send(shared, divert, emit, l, addr);
            Verdict::Forward
        }
    }
}

/// Fix up checksums and inject one crafted packet.
fn send(shared: &Shared, divert: &Diverter, mut emit: Emit, l: &Layout, addr: &ffi::WinDivertAddress) {
    let mut addr = *addr;
    if !recalc_checksums(&mut emit.bytes, &mut addr) {
        shared.stats.errors.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if emit.corrupt_checksum {
        let good = net::tcp_checksum(&emit.bytes, l);
        // Any wrong value will do, but zero is a legitimate checksum, so avoid it.
        let bad = match good ^ 0x5555 {
            0 => 0x1234,
            other => other,
        };
        net::set_tcp_checksum(&mut emit.bytes, l, bad);
        addr.mark_tcp_checksum_valid();
    }
    if let Err(e) = divert.send(&emit.bytes, &addr) {
        shared.stats.errors.fetch_add(1, Ordering::Relaxed);
        tracing::warn!("inject failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(port: u16) -> FlowKey {
        let mut dst = [0u8; 16];
        dst[..4].copy_from_slice(&[93, 184, 216, 34]);
        FlowKey { src_port: port, dst_port: 443, dst }
    }

    #[test]
    fn outbound_filter_narrows_to_data_carrying_packets() {
        let f = outbound_filter(&[80, 443]);
        assert!(f.contains("tcp.PayloadLength > 0"));
        assert!(f.contains("tcp.DstPort == 80"));
        assert!(f.contains("udp.DstPort == 443"));
        assert!(f.starts_with("outbound and !loopback"));
    }

    #[test]
    fn inbound_filter_grows_with_what_the_learner_needs() {
        // Hop measurement only.
        let minimal = inbound_filter(&[443], false, false);
        assert!(minimal.contains("tcp.Syn == 1"));
        assert!(!minimal.contains("tcp.Rst"));
        assert!(!minimal.contains("PayloadLength"));

        // Reset detection is nearly free.
        let resets = inbound_filter(&[443], true, false);
        assert!(resets.contains("tcp.Rst == 1"));
        assert!(!resets.contains("PayloadLength"));

        // Silent-drop detection needs the data packets too.
        let full = inbound_filter(&[443], true, true);
        assert!(full.contains("tcp.PayloadLength > 0"));
        assert!(full.contains("tcp.SrcPort == 443"));
    }

    #[test]
    fn filters_fall_back_when_no_ports_configured() {
        assert!(outbound_filter(&[]).contains("tcp.DstPort == 443"));
        assert!(inbound_filter(&[], true, true).contains("tcp.SrcPort == 80"));
    }

    #[test]
    fn a_reset_after_a_handshake_counts_as_a_block() {
        let shared = Shared::new(Config::default());
        shared.track_flow(key(40000), "blocked.example");
        shared.note_reset(&key(40000));
        assert_eq!(shared.stats.snapshot().blocks_detected, 1);
        assert_eq!(shared.suspect_count(), 1);
    }

    #[test]
    fn a_reset_after_the_server_replied_is_not_a_block() {
        // Ordinary connection teardown must not look like censorship.
        let shared = Shared::new(Config::default());
        shared.track_flow(key(40001), "fine.example");
        shared.mark_answered(&key(40001));
        shared.note_reset(&key(40001));
        assert_eq!(shared.stats.snapshot().blocks_detected, 0);
        assert_eq!(shared.suspect_count(), 0);
    }

    #[test]
    fn a_reset_for_an_untracked_flow_is_ignored() {
        let shared = Shared::new(Config::default());
        shared.note_reset(&key(40002));
        assert_eq!(shared.suspect_count(), 0);
    }

    #[test]
    fn probing_a_host_suppresses_its_own_failures() {
        // The prober deliberately makes failing connections; counting those
        // would queue another probe behind every probe.
        let shared = Shared::new(Config::default());
        shared.probing.lock().insert("under-test.example".to_string());
        shared.track_flow(key(40003), "under-test.example");
        shared.note_reset(&key(40003));
        assert_eq!(shared.suspect_count(), 0);
    }

    #[test]
    fn the_decoy_host_is_never_treated_as_blocked() {
        // Our own decoys hammer it; the user never asked for it.
        let shared = Shared::new(Config::default());
        let decoy = shared.config.read().strategy.decoy_host.clone();
        for _ in 0..5 {
            shared.note_failure(&decoy, "리셋");
        }
        assert_eq!(shared.suspect_count(), 0);
        assert_eq!(shared.stats.snapshot().blocks_detected, 0);
    }

    #[test]
    fn a_candidate_needs_to_reach_the_threshold() {
        let shared = Shared::new(Config::default());
        shared.note_failure("blocked.example", "리셋");
        assert_eq!(shared.next_candidate(2, Duration::from_secs(60)), None);

        shared.note_failure("blocked.example", "리셋");
        assert_eq!(shared.next_candidate(2, Duration::from_secs(60)).as_deref(), Some("blocked.example"));
        // Taking it clears the evidence so it is not probed twice.
        assert_eq!(shared.suspect_count(), 0);
    }

    #[test]
    fn the_worst_offender_is_probed_first() {
        let shared = Shared::new(Config::default());
        shared.note_failure("a.example", "리셋");
        shared.note_failure("a.example", "리셋");
        for _ in 0..5 {
            shared.note_failure("b.example", "리셋");
        }
        assert_eq!(shared.next_candidate(2, Duration::from_secs(60)).as_deref(), Some("b.example"));
    }

    #[test]
    fn cooldown_blocks_an_immediate_reprobe() {
        let shared = Shared::new(Config::default());
        shared.cooldown.lock().insert("recent.example".to_string(), Instant::now());
        shared.note_failure("recent.example", "리셋");
        shared.note_failure("recent.example", "리셋");
        assert_eq!(shared.next_candidate(2, Duration::from_secs(3600)), None);
        // A cooldown that has already elapsed does not block anything.
        assert_eq!(
            shared.next_candidate(2, Duration::from_nanos(1)).as_deref(),
            Some("recent.example")
        );
    }

    #[test]
    fn unanswered_handshakes_are_only_counted_when_asked() {
        let shared = Shared::new(Config::default());
        shared.track_flow(key(40004), "silent.example");
        // Nothing is old enough yet, and detection is off.
        shared.prune_flows(false);
        assert_eq!(shared.suspect_count(), 0);
    }

    #[test]
    fn the_flow_table_stays_bounded() {
        let shared = Shared::new(Config::default());
        for port in 0..(MAX_FLOWS as u32 + 500) {
            shared.track_flow(key(port as u16), "busy.example");
        }
        assert!(shared.flows.lock().len() <= MAX_FLOWS);
    }

    #[test]
    fn event_log_stays_bounded() {
        let shared = Shared::new(Config::default());
        for i in 0..MAX_EVENTS + 50 {
            shared.record(&format!("host{i}.test"), "분할");
        }
        assert_eq!(shared.events.lock().len(), MAX_EVENTS);
        assert_eq!(shared.recent_events()[0].host, format!("host{}.test", MAX_EVENTS + 49));
    }

    #[test]
    fn resolutions_are_recorded_for_quic_policy() {
        let shared = Shared::new(Config::default());
        let mut addr = [0u8; 16];
        addr[..4].copy_from_slice(&[93, 184, 216, 34]);
        shared.note_resolution(addr, "example.com");
        assert_eq!(shared.dns.read().get(&addr).map(String::as_str), Some("example.com"));
    }

    #[test]
    fn stats_snapshot_reflects_counters() {
        let stats = Stats::default();
        stats.decoys_sent.fetch_add(3, Ordering::Relaxed);
        stats.strategies_learned.fetch_add(1, Ordering::Relaxed);
        let snap = stats.snapshot();
        assert_eq!(snap.decoys_sent, 3);
        assert_eq!(snap.strategies_learned, 1);
        assert_eq!(snap.errors, 0);
    }
}
