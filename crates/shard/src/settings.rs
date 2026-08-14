//! The settings, described once.
//!
//! Every setting is a name, a label, and what kind of thing it is. The page
//! draws whatever this hands it and sends back a name and a value; nothing about
//! a particular setting is written twice, so adding one is adding a line here
//! rather than a control in one place and a reader in another.

use crate::config::{AudioQuality, Config, Scope};
use crate::strategy::{Desync, Fooling, SplitAt};

/// What sort of control a setting needs.
enum Kind {
    Toggle(bool),
    /// One of a fixed set: the value now, and every value with its label.
    Choice(String, Vec<(String, String)>),
    Number(i64, i64, i64),
    Text(String),
    /// A list of names, one per line — domains, upstreams.
    Lines(Vec<String>),
}

struct Item {
    key: &'static str,
    label: &'static str,
    help: &'static str,
    kind: Kind,
}

fn choice<T: Copy + PartialEq>(
    now: T,
    all: &[T],
    name: impl Fn(T) -> &'static str,
    label: impl Fn(T) -> &'static str,
) -> Kind {
    Kind::Choice(
        name(now).to_string(),
        all.iter().map(|&v| (name(v).to_string(), label(v).to_string())).collect(),
    )
}

fn desync_name(v: Desync) -> &'static str {
    match v {
        Desync::None => "none",
        Desync::Split => "split",
        Desync::Disorder => "disorder",
        Desync::Fake => "fake",
        Desync::FakeSplit => "fakesplit",
        Desync::FakeDisorder => "fakedisorder",
    }
}

fn fooling_name(v: Fooling) -> &'static str {
    match v {
        Fooling::Ttl => "ttl",
        Fooling::BadSum => "badsum",
        Fooling::BadSeq => "badseq",
    }
}

fn split_name(v: SplitAt) -> &'static str {
    match v {
        SplitAt::HostMidpoint => "host",
        SplitAt::RecordHeader => "record",
        SplitAt::Fixed => "fixed",
    }
}

fn scope_name(v: Scope) -> &'static str {
    match v {
        Scope::All => "all",
        Scope::Listed => "listed",
    }
}

fn audio_name(v: AudioQuality) -> &'static str {
    match v {
        AudioQuality::Best => "best",
        AudioQuality::Balanced => "balanced",
        AudioQuality::Small => "small",
    }
}

/// Every setting, in the order the page shows them, grouped by what they are
/// about.
fn groups(cfg: &Config) -> Vec<(&'static str, Vec<Item>)> {
    vec![
        (
            "일반",
            vec![
                Item {
                    key: "start_engine_on_launch",
                    label: "시작할 때 엔진 켜기",
                    help: "프로그램을 열면 우회를 바로 시작합니다.",
                    kind: Kind::Toggle(cfg.start_engine_on_launch),
                },
                Item {
                    key: "detect_silent_drops",
                    label: "조용한 차단 감지",
                    help: "오류 없이 응답만 오지 않는 차단을 알아냅니다.",
                    kind: Kind::Toggle(cfg.detect_silent_drops),
                },
                Item {
                    key: "worker_threads",
                    label: "작업 스레드",
                    help: "패킷을 다루는 일꾼 수. 늘린다고 빨라지지는 않습니다.",
                    kind: Kind::Number(cfg.worker_threads as i64, 1, 8),
                },
                Item {
                    key: "hotkey",
                    label: "단축키",
                    help: "엔진을 켜고 끄는 전역 단축키입니다.",
                    kind: Kind::Text(cfg.hotkey.clone()),
                },
            ],
        ),
        (
            "적용 범위",
            vec![
                Item {
                    key: "scope",
                    label: "어디에 적용할지",
                    help: "전체는 손볼 것이 없고, 지정은 목록에 있는 곳에만 씁니다.",
                    kind: choice(cfg.scope, &[Scope::All, Scope::Listed], scope_name, Scope::label),
                },
                Item {
                    key: "domains",
                    label: "대상 도메인",
                    help: "한 줄에 하나씩. '지정 도메인만'일 때 쓰입니다.",
                    kind: Kind::Lines(cfg.domains.clone()),
                },
                Item {
                    key: "exclude",
                    label: "예외 도메인",
                    help: "한 줄에 하나씩. 여기 적힌 곳은 건드리지 않습니다.",
                    kind: Kind::Lines(cfg.exclude.clone()),
                },
            ],
        ),
        (
            "자동 학습",
            vec![
                Item {
                    key: "auto_learn",
                    label: "막히는 사이트 학습",
                    help: "막힌 곳을 감지해 통하는 전략을 스스로 찾습니다.",
                    kind: Kind::Toggle(cfg.auto_learn),
                },
                Item {
                    key: "auto_learn_threshold",
                    label: "학습 임계값",
                    help: "몇 번 막혀야 학습을 시작할지.",
                    kind: Kind::Number(cfg.auto_learn_threshold as i64, 1, 20),
                },
                Item {
                    key: "auto_learn_cooldown_min",
                    label: "재학습 간격(분)",
                    help: "같은 곳을 다시 학습하기까지 기다리는 시간.",
                    kind: Kind::Number(cfg.auto_learn_cooldown_min as i64, 1, 1440),
                },
            ],
        ),
        (
            "우회 방식",
            vec![
                Item {
                    key: "strategy.desync",
                    label: "방식",
                    help: "쪼개기·순서 뒤집기·미끼 중 무엇을 쓸지.",
                    kind: choice(cfg.strategy.desync, Desync::ALL, desync_name, Desync::label),
                },
                Item {
                    key: "strategy.split_at",
                    label: "자르는 위치",
                    help: "어디에서 나눌지. 호스트 중간이 가장 무난합니다.",
                    kind: choice(cfg.strategy.split_at, SplitAt::ALL, split_name, SplitAt::label),
                },
                Item {
                    key: "strategy.fixed_split_offset",
                    label: "고정 위치(바이트)",
                    help: "'고정 위치'를 골랐을 때 쓰는 자리.",
                    kind: Kind::Number(cfg.strategy.fixed_split_offset as i64, 0, 1500),
                },
                Item {
                    key: "strategy.extra_splits",
                    label: "추가 조각",
                    help: "더 잘게 나눌수록 까다로운 검사를 넘지만 패킷이 늡니다.",
                    kind: Kind::Number(cfg.strategy.extra_splits as i64, 0, 8),
                },
                Item {
                    key: "strategy.fooling",
                    label: "미끼를 죽이는 법",
                    help: "미끼가 서버에 닿지 않게 만드는 방법.",
                    kind: choice(cfg.strategy.fooling, Fooling::ALL, fooling_name, Fooling::label),
                },
                Item {
                    key: "strategy.fake_repeats",
                    label: "미끼 반복",
                    help: "드문드문 보는 장비를 상대로는 여러 번이 필요합니다.",
                    kind: Kind::Number(cfg.strategy.fake_repeats as i64, 1, 8),
                },
                Item {
                    key: "strategy.auto_ttl",
                    label: "TTL 자동",
                    help: "서버까지의 거리를 재서 미끼가 한 홉 앞에서 죽게 합니다.",
                    kind: Kind::Toggle(cfg.strategy.auto_ttl),
                },
                Item {
                    key: "strategy.fake_ttl",
                    label: "TTL 고정값",
                    help: "자동이 꺼져 있을 때 쓰는 값.",
                    kind: Kind::Number(cfg.strategy.fake_ttl as i64, 1, 64),
                },
                Item {
                    key: "strategy.auto_ttl_delta",
                    label: "TTL 여유",
                    help: "잰 거리에서 몇 홉을 뺄지.",
                    kind: Kind::Number(cfg.strategy.auto_ttl_delta as i64, 0, 8),
                },
                Item {
                    key: "strategy.auto_ttl_cap",
                    label: "TTL 상한",
                    help: "자동으로 구한 값이 이보다 커지지 않게 합니다.",
                    kind: Kind::Number(cfg.strategy.auto_ttl_cap as i64, 1, 12),
                },
            ],
        ),
        (
            "암호화 DNS",
            vec![
                Item {
                    key: "doh.enabled",
                    label: "DoH 사용",
                    help: "이름을 묻는 것까지 암호화합니다.",
                    kind: Kind::Toggle(cfg.doh.enabled),
                },
                Item {
                    key: "doh.set_system_dns",
                    label: "시스템 DNS로 지정",
                    help: "이 프로그램이 켜져 있는 동안 기기의 DNS를 여기로 돌립니다.",
                    kind: Kind::Toggle(cfg.doh.set_system_dns),
                },
                Item {
                    key: "doh.listen",
                    label: "받는 주소",
                    help: "이 컴퓨터 안에서만 쓰는 주소입니다.",
                    kind: Kind::Text(cfg.doh.listen.clone()),
                },
                Item {
                    key: "doh.upstreams",
                    label: "상위 서버",
                    help: "한 줄에 하나씩. 위에서부터 씁니다.",
                    kind: Kind::Lines(cfg.doh.upstreams.clone()),
                },
                Item {
                    key: "doh.bootstrap",
                    label: "부트스트랩",
                    help: "상위 서버의 주소를 처음 알아낼 때 쓰는 곳.",
                    kind: Kind::Lines(cfg.doh.bootstrap.clone()),
                },
            ],
        ),
        (
            "영상 받기",
            vec![
                Item {
                    key: "download.audio",
                    label: "음질",
                    help: "화질은 받을 때 고르고, 음질만 여기서 정해 둡니다.",
                    kind: choice(
                        cfg.download.audio,
                        AudioQuality::ALL,
                        audio_name,
                        AudioQuality::label,
                    ),
                },
                Item {
                    key: "download.audio_language",
                    label: "음성 언어",
                    help: "비워 두면 영상의 기본 언어를 씁니다. 예: ko, en",
                    kind: Kind::Text(cfg.download.audio_language.clone()),
                },
            ],
        ),
    ]
}

/// The settings as the page reads them.
pub fn as_json(cfg: &Config) -> String {
    let escape = crate::shell::escape;
    let mut out = String::from(r#"{"t":"settings","groups":["#);
    for (at, (title, items)) in groups(cfg).into_iter().enumerate() {
        if at > 0 {
            out.push(',');
        }
        out.push_str(&format!(r#"{{"title":"{}","items":["#, escape(title)));
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let body = match &item.kind {
                Kind::Toggle(on) => format!(r#""kind":"toggle","value":{on}"#),
                Kind::Text(text) => format!(r#""kind":"text","value":"{}""#, escape(text)),
                Kind::Number(now, low, high) => {
                    format!(r#""kind":"number","value":{now},"min":{low},"max":{high}"#)
                }
                Kind::Lines(lines) => format!(
                    r#""kind":"lines","value":"{}""#,
                    escape(&lines.join("\n"))
                ),
                Kind::Choice(now, all) => {
                    let options: Vec<String> = all
                        .iter()
                        .map(|(name, label)| {
                            format!(r#"{{"name":"{}","label":"{}"}}"#, escape(name), escape(label))
                        })
                        .collect();
                    format!(
                        r#""kind":"choice","value":"{}","options":[{}]"#,
                        escape(now),
                        options.join(",")
                    )
                }
            };
            out.push_str(&format!(
                r#"{{"key":"{}","label":"{}","help":"{}",{}}}"#,
                escape(item.key),
                escape(item.label),
                escape(item.help),
                body
            ));
        }
        out.push_str("]}");
    }
    out.push_str("]}");
    out
}

/// Whether the engine has to be started again for this setting to mean anything.
///
/// Some of them are read once, when the filter is built and the workers are
/// started. Changed while it is running, they would sit in the file looking
/// applied and do nothing until the next launch — so the engine is turned over.
pub fn needs_restart(key: &str) -> bool {
    matches!(
        key,
        "worker_threads"
            | "scope"
            | "domains"
            | "exclude"
            | "detect_silent_drops"
            | "doh.enabled"
            | "doh.listen"
            | "doh.upstreams"
            | "doh.bootstrap"
            | "doh.set_system_dns"
    )
}

/// Put one setting back, by the name the page was given.
///
/// Returns whether it landed: a name this build does not know, or a value that
/// does not fit it, changes nothing rather than half-applying.
pub fn apply(cfg: &mut Config, key: &str, value: &str) -> bool {
    let on = value == "true";
    let lines = || -> Vec<String> {
        value
            .split('\n')
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect()
    };
    let number = |low: i64, high: i64| value.trim().parse::<i64>().ok().map(|n| n.clamp(low, high));

    match key {
        "start_engine_on_launch" => cfg.start_engine_on_launch = on,
        "detect_silent_drops" => cfg.detect_silent_drops = on,
        "worker_threads" => match number(1, 8) {
            Some(n) => cfg.worker_threads = n as u8,
            None => return false,
        },
        "hotkey" => cfg.hotkey = value.trim().to_string(),
        "scope" => {
            cfg.scope = match value {
                "all" => Scope::All,
                "listed" => Scope::Listed,
                _ => return false,
            }
        }
        "domains" => cfg.domains = lines(),
        "exclude" => cfg.exclude = lines(),
        "auto_learn" => cfg.auto_learn = on,
        "auto_learn_threshold" => match number(1, 20) {
            Some(n) => cfg.auto_learn_threshold = n as u8,
            None => return false,
        },
        "auto_learn_cooldown_min" => match number(1, 1440) {
            Some(n) => cfg.auto_learn_cooldown_min = n as u16,
            None => return false,
        },
        "strategy.desync" => {
            cfg.strategy.desync = match value {
                "none" => Desync::None,
                "split" => Desync::Split,
                "disorder" => Desync::Disorder,
                "fake" => Desync::Fake,
                "fakesplit" => Desync::FakeSplit,
                "fakedisorder" => Desync::FakeDisorder,
                _ => return false,
            }
        }
        "strategy.split_at" => {
            cfg.strategy.split_at = match value {
                "host" => SplitAt::HostMidpoint,
                "record" => SplitAt::RecordHeader,
                "fixed" => SplitAt::Fixed,
                _ => return false,
            }
        }
        "strategy.fooling" => {
            cfg.strategy.fooling = match value {
                "ttl" => Fooling::Ttl,
                "badsum" => Fooling::BadSum,
                "badseq" => Fooling::BadSeq,
                _ => return false,
            }
        }
        "strategy.fixed_split_offset" => match number(0, 1500) {
            Some(n) => cfg.strategy.fixed_split_offset = n as u16,
            None => return false,
        },
        "strategy.extra_splits" => match number(0, 8) {
            Some(n) => cfg.strategy.extra_splits = n as u8,
            None => return false,
        },
        "strategy.fake_repeats" => match number(1, 8) {
            Some(n) => cfg.strategy.fake_repeats = n as u8,
            None => return false,
        },
        "strategy.auto_ttl" => cfg.strategy.auto_ttl = on,
        "strategy.fake_ttl" => match number(1, 64) {
            Some(n) => cfg.strategy.fake_ttl = n as u8,
            None => return false,
        },
        "strategy.auto_ttl_delta" => match number(0, 8) {
            Some(n) => cfg.strategy.auto_ttl_delta = n as u8,
            None => return false,
        },
        "strategy.auto_ttl_cap" => match number(1, 12) {
            Some(n) => cfg.strategy.auto_ttl_cap = n as u8,
            None => return false,
        },
        "doh.enabled" => cfg.doh.enabled = on,
        "doh.set_system_dns" => cfg.doh.set_system_dns = on,
        "doh.listen" => cfg.doh.listen = value.trim().to_string(),
        "doh.upstreams" => cfg.doh.upstreams = lines(),
        "doh.bootstrap" => cfg.doh.bootstrap = lines(),
        "download.audio" => {
            cfg.download.audio = match value {
                "best" => AudioQuality::Best,
                "balanced" => AudioQuality::Balanced,
                "small" => AudioQuality::Small,
                _ => return false,
            }
        }
        "download.audio_language" => cfg.download.audio_language = value.trim().to_string(),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_the_page_is_shown_can_be_put_back() {
        // The two halves are written apart, so this is what keeps them in step:
        // anything offered must also be readable, or the page would show a
        // control that does nothing.
        let mut cfg = Config::default();
        for (_, items) in groups(&cfg) {
            for item in items {
                let value = match &item.kind {
                    Kind::Toggle(on) => on.to_string(),
                    Kind::Text(text) => text.clone(),
                    Kind::Number(now, ..) => now.to_string(),
                    Kind::Lines(lines) => lines.join("\n"),
                    Kind::Choice(now, _) => now.clone(),
                };
                assert!(apply(&mut cfg, item.key, &value), "{} did not apply", item.key);
            }
        }
    }

    #[test]
    fn a_name_or_a_value_this_build_does_not_know_changes_nothing() {
        let mut cfg = Config::default();
        assert!(!apply(&mut cfg, "no.such.setting", "1"));
        assert!(!apply(&mut cfg, "scope", "sideways"));
        assert!(!apply(&mut cfg, "worker_threads", "많이"));
    }

    #[test]
    fn a_number_is_kept_inside_what_it_is_allowed_to_be() {
        let mut cfg = Config::default();
        assert!(apply(&mut cfg, "strategy.fake_ttl", "999"));
        assert_eq!(cfg.strategy.fake_ttl, 64);
        assert!(apply(&mut cfg, "auto_learn_threshold", "0"));
        assert_eq!(cfg.auto_learn_threshold, 1);
    }

    #[test]
    fn a_list_is_one_name_per_line_with_the_blanks_dropped() {
        let mut cfg = Config::default();
        assert!(apply(&mut cfg, "exclude", " a.com \n\n b.com \n"));
        assert_eq!(cfg.exclude, vec!["a.com".to_string(), "b.com".to_string()]);
    }
}
