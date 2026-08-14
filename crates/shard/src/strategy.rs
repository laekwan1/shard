//! Bypass strategy: the knobs, their defaults, and what each one costs.
//!
//! Every field here is exposed in the settings window. The doc comments are the
//! source for the tooltips, so they describe the trade-off rather than the
//! mechanism.

use serde::{Deserialize, Serialize};

/// How the first data segment of a connection is broken up.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Desync {
    /// Pass through untouched. Zero cost, zero bypass.
    #[default]
    None,
    /// Split the ClientHello into ordered fragments. Beats DPI that parses
    /// packet-by-packet; useless against one that reassembles the stream.
    Split,
    /// Split, then send the later fragment first. Costs nothing extra and also
    /// defeats reassembly that assumes arrival order.
    Disorder,
    /// Send a decoy segment that dies before the server. The DPI records the
    /// decoy's hostname; the real request follows on the same sequence.
    Fake,
    /// Decoy followed by ordered fragments.
    FakeSplit,
    /// Decoy followed by reversed fragments. Strongest combination, three extra
    /// packets per connection.
    FakeDisorder,
}

impl Desync {
    pub fn label(self) -> &'static str {
        match self {
            Desync::None => "없음 (통과)",
            Desync::Split => "분할",
            Desync::Disorder => "역순 분할",
            Desync::Fake => "디코이",
            Desync::FakeSplit => "디코이 + 분할",
            Desync::FakeDisorder => "디코이 + 역순 분할",
        }
    }

    pub fn uses_fake(self) -> bool {
        matches!(self, Desync::Fake | Desync::FakeSplit | Desync::FakeDisorder)
    }

    pub fn splits(self) -> bool {
        matches!(self, Desync::Split | Desync::Disorder | Desync::FakeSplit | Desync::FakeDisorder)
    }

    pub fn reverses(self) -> bool {
        matches!(self, Desync::Disorder | Desync::FakeDisorder)
    }

    pub const ALL: &'static [Desync] = &[
        Desync::None,
        Desync::Split,
        Desync::Disorder,
        Desync::Fake,
        Desync::FakeSplit,
        Desync::FakeDisorder,
    ];
}

/// How the decoy segment is made invisible to the real server while staying
/// visible to the middlebox.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fooling {
    /// Expire the decoy in transit with a low TTL. Needs the hop count to the
    /// middlebox to be right, which `auto_ttl` measures.
    #[default]
    Ttl,
    /// Send a deliberately wrong TCP checksum. The server discards it; most DPI
    /// never verifies. Works at any distance, but some NICs recompute checksums
    /// in hardware and would silently repair it.
    BadSum,
    /// Use a sequence number far outside the window. The server ignores it,
    /// stateless DPI still parses it. Fails against stateful DPI.
    BadSeq,
}

impl Fooling {
    pub fn label(self) -> &'static str {
        match self {
            Fooling::Ttl => "TTL 만료",
            Fooling::BadSum => "잘못된 체크섬",
            Fooling::BadSeq => "잘못된 시퀀스",
        }
    }

    pub const ALL: &'static [Fooling] = &[Fooling::Ttl, Fooling::BadSum, Fooling::BadSeq];
}

/// Where the payload is cut.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitAt {
    /// Middle of the hostname. Guarantees neither fragment contains the whole
    /// string, which is what a substring matcher looks for.
    #[default]
    HostMidpoint,
    /// Immediately after the TLS record header, so the handshake type and
    /// length land in a separate segment.
    RecordHeader,
    /// A fixed byte offset into the payload.
    Fixed,
}

impl SplitAt {
    pub fn label(self) -> &'static str {
        match self {
            SplitAt::HostMidpoint => "호스트명 중앙",
            SplitAt::RecordHeader => "레코드 헤더 뒤",
            SplitAt::Fixed => "고정 위치",
        }
    }

    pub const ALL: &'static [SplitAt] = &[SplitAt::HostMidpoint, SplitAt::RecordHeader, SplitAt::Fixed];
}

/// What to do with QUIC, which carries its ClientHello encrypted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuicMode {
    /// Leave QUIC alone. Fastest, but a blocked site stays blocked over QUIC.
    Pass,
    /// Drop connection setup so the browser falls back to TCP, where the desync
    /// engine works. Costs QUIC's latency advantage; adds ~100-300 ms to the
    /// first connection per host.
    #[default]
    Drop,
    /// Send a low-TTL decoy datagram before each Initial. Keeps QUIC working
    /// when it succeeds, but is far less reliable than falling back.
    Decoy,
}

impl QuicMode {
    pub fn label(self) -> &'static str {
        match self {
            QuicMode::Pass => "통과",
            QuicMode::Drop => "차단 (TCP로 폴백)",
            QuicMode::Decoy => "디코이",
        }
    }

    pub const ALL: &'static [QuicMode] = &[QuicMode::Pass, QuicMode::Drop, QuicMode::Decoy];
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Strategy {
    pub desync: Desync,
    pub fooling: Fooling,
    pub split_at: SplitAt,
    /// Byte offset used when `split_at` is `Fixed`, and the fallback position
    /// when no hostname was found.
    pub fixed_split_offset: u16,
    /// Extra cut points beyond the first. More fragments defeat more
    /// aggressive reassembly; each one adds a packet to every new connection.
    pub extra_splits: u8,
    /// How many times the decoy is repeated. A middlebox that samples rather
    /// than inspecting every packet needs more than one.
    pub fake_repeats: u8,
    /// TTL for the decoy when `auto_ttl` is off.
    pub fake_ttl: u8,
    /// Derive the decoy TTL from the measured hop count to the destination,
    /// so it expires one hop short of the server regardless of distance.
    pub auto_ttl: bool,
    /// Hops subtracted from the measured distance.
    pub auto_ttl_delta: u8,
    /// Hard ceiling on the derived TTL.
    ///
    /// The hop count is measured from packets coming *back*, and routing is
    /// often asymmetric — the outbound path can be shorter than the return
    /// path, in which case `hops - delta` still reaches the server and the
    /// decoy corrupts the connection instead of protecting it. Censorship
    /// equipment sits inside the ISP, a handful of hops away, so a decoy never
    /// needs to travel far and capping costs nothing.
    pub auto_ttl_cap: u8,
    /// Hostname put in the decoy ClientHello. Anything unblocked works; the
    /// point is that the middlebox caches this name for the connection.
    pub decoy_host: String,
    /// Also split the plain-HTTP `Host` header value.
    pub http_split: bool,
    /// Rewrite `Host` as `hOsT`. Defeats case-sensitive matching at no cost;
    /// RFC 7230 makes header names case-insensitive so servers do not care.
    pub http_host_case: bool,
    /// Insert an extra space after `Host:`. Some parsers key on the exact
    /// byte sequence; compliant servers tolerate it.
    pub http_host_space: bool,
    pub quic: QuicMode,
}

impl Default for Strategy {
    fn default() -> Self {
        Self {
            // Measured, not assumed: against a real Korean ISP every split
            // variant failed in ~300 ms — the middlebox reassembles the stream
            // before matching — while the decoy got a full HTTPS request
            // through. Splitting is the gentler option and stays first on the
            // ladder, but it is not a useful default where it never works.
            //
            // What makes the decoy safe enough to default to is `auto_ttl_cap`:
            // before it existed the decoy could reach the server and break the
            // connection outright.
            desync: Desync::Fake,
            fooling: Fooling::Ttl,
            split_at: SplitAt::HostMidpoint,
            fixed_split_offset: 2,
            extra_splits: 0,
            fake_repeats: 1,
            fake_ttl: 6,
            auto_ttl: true,
            auto_ttl_delta: 1,
            auto_ttl_cap: 8,
            decoy_host: "www.iana.org".to_string(),
            http_split: true,
            http_host_case: true,
            http_host_space: false,
            quic: QuicMode::Drop,
        }
    }
}

impl Strategy {
    /// A pass-through strategy, used as the baseline when probing.
    pub fn passthrough() -> Self {
        Self { desync: Desync::None, quic: QuicMode::Pass, http_host_case: false, http_split: false, ..Default::default() }
    }

    /// Rough cost in extra packets per new connection, for the UI.
    pub fn extra_packets(&self) -> usize {
        let fakes = if self.desync.uses_fake() { self.fake_repeats.max(1) as usize } else { 0 };
        let frags = if self.desync.splits() { 1 + self.extra_splits as usize } else { 0 };
        fakes + frags
    }

    /// Ladder of strategies the auto-prober walks, cheapest and least invasive
    /// first. Stopping at the first success keeps the steady-state overhead at
    /// the minimum that actually works.
    pub fn ladder() -> Vec<(&'static str, Strategy)> {
        let base = Strategy::default();
        let at = |offset: u16| Strategy {
            desync: Desync::Split,
            split_at: SplitAt::Fixed,
            fixed_split_offset: offset,
            ..base.clone()
        };
        vec![
            ("분할 (호스트명 중앙)", Strategy { desync: Desync::Split, ..base.clone() }),
            // Cutting in the first couple of bytes is the classic that works
            // against matchers which only ever look at the opening segment.
            ("분할 (1바이트)", at(1)),
            ("분할 (2바이트)", at(2)),
            ("역순 분할", Strategy { desync: Desync::Disorder, ..base.clone() }),
            ("역순 분할 (1바이트)", Strategy { desync: Desync::Disorder, split_at: SplitAt::Fixed, fixed_split_offset: 1, ..base.clone() }),
            ("분할 (레코드 헤더)", Strategy { desync: Desync::Split, split_at: SplitAt::RecordHeader, ..base.clone() }),
            (
                "다중 분할",
                Strategy { desync: Desync::Split, extra_splits: 3, ..base.clone() },
            ),
            ("디코이 (TTL)", Strategy { desync: Desync::Fake, fooling: Fooling::Ttl, ..base.clone() }),
            ("디코이 (체크섬)", Strategy { desync: Desync::Fake, fooling: Fooling::BadSum, ..base.clone() }),
            ("디코이 + 분할", Strategy { desync: Desync::FakeSplit, ..base.clone() }),
            ("디코이 + 역순 분할", Strategy { desync: Desync::FakeDisorder, ..base.clone() }),
            (
                "디코이 x3 + 역순 분할 + 다중 분할",
                Strategy { desync: Desync::FakeDisorder, fake_repeats: 3, extra_splits: 2, ..base.clone() },
            ),
            (
                "디코이 + 역순 분할 (체크섬)",
                Strategy { desync: Desync::FakeDisorder, fooling: Fooling::BadSum, fake_repeats: 2, ..base },
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips_through_toml() {
        let s = Strategy::default();
        let text = toml::to_string(&s).unwrap();
        let back: Strategy = toml::from_str(&text).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let partial: Strategy = toml::from_str("desync = \"split\"").unwrap();
        assert_eq!(partial.desync, Desync::Split);
        assert_eq!(partial.decoy_host, Strategy::default().decoy_host);
    }

    #[test]
    fn the_default_decoy_is_bounded() {
        // The decoy is the default because splitting alone measurably fails
        // against a reassembling middlebox. What keeps it safe is that its TTL
        // is derived from a measurement and then capped — a fixed guess is
        // exactly what used to send it all the way to the server.
        let s = Strategy::default();
        assert!(s.desync.uses_fake());
        assert!(s.auto_ttl, "a guessed TTL is what broke connections");
        assert!(
            (1..=12).contains(&s.auto_ttl_cap),
            "the cap has to stay inside the ISP, got {}",
            s.auto_ttl_cap
        );
    }

    #[test]
    fn packet_cost_tracks_the_knobs() {
        assert_eq!(Strategy::passthrough().extra_packets(), 0);
        let s = Strategy { desync: Desync::FakeDisorder, fake_repeats: 3, extra_splits: 2, ..Default::default() };
        assert_eq!(s.extra_packets(), 3 + 3);
    }

    #[test]
    fn ladder_is_ordered_cheapest_first() {
        let ladder = Strategy::ladder();
        assert!(!ladder.is_empty());
        let first = ladder.first().unwrap().1.extra_packets();
        let last = ladder.last().unwrap().1.extra_packets();
        assert!(first <= last, "ladder should escalate, not de-escalate");
    }

    #[test]
    fn ladder_tries_every_safe_rung_before_any_decoy() {
        // A decoy can break a connection outright, so it must never be reached
        // while a split-only option remains untried.
        let ladder = Strategy::ladder();
        let first_decoy = ladder.iter().position(|(_, s)| s.desync.uses_fake()).expect("decoy rungs exist");
        assert!(
            ladder[..first_decoy].iter().all(|(_, s)| !s.desync.uses_fake()),
            "split-only rungs must all come first"
        );
        assert!(first_decoy >= 5, "there should be several safe rungs to try, got {first_decoy}");
    }

    #[test]
    fn ladder_labels_are_unique() {
        // Duplicated labels in the probe log would be impossible to tell apart.
        let ladder = Strategy::ladder();
        let mut labels: Vec<&str> = ladder.iter().map(|(l, _)| *l).collect();
        labels.sort_unstable();
        let count = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), count);
    }
}
