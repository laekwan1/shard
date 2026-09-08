//! 원격 설정(설정 계층 1단계) — 애플이 **서버 쪽에서** 바꿀 수 있는 "얕은 값"(개발자 포털 상수 등)을
//! Veil에서 받아 내장값 위에 덮는다. 그 값이 바뀌어 재서명이 깨지면 앱을 다시 빌드·설치하지 않고 Veil의
//! 텍스트 파일 한 줄만 고쳐 고칠 수 있게 하려는 것 — anisette 서버가 로그인 계층을 서버로 흡수하는 것과
//! 같은 결의 다음 단계다.
//!
//! **절대 흐름을 깨지 않는다.** 파일이 없거나·아는 키가 없거나·받기 실패하면 각 값은 내장 기본값 그대로다.
//! 이 계층은 "애플이 옮겼을 때 덮어쓸 수 있게"만 하고, 아무 설정이 없어도 지금과 동일하게 동작한다.
//!
//! 형식은 JSON이 아니라 `key=value` 텍스트다 — 파싱에 새 의존성(serde_json)이 필요 없고(CLAUDE.md:
//! 의존성 늘리지 않기), Veil에서 손으로 고치기도 쉽다:
//! ```text
//! # Shard 재서명 원격 설정
//! dev_client_id=XABBG36SBA
//! dev_protocol_version=QH65B2
//! dev_base_url=https://developerservices2.apple.com/services
//! ```
//!
//! **두는 파일**(state_dir):
//! - `config_url.txt` — 이 URL을 GET해 아래를 받는다(사용자/Veil이 넣는다; anisette_url.txt와 같은 방식).
//! - `shard-config.txt` — 위에서 받아 캐시한 값(refresh가 쓰고 load가 읽는다).

use std::path::Path;
use std::time::Duration;

/// 애플이 서버 쪽에서 바꿀 수 있는, 재서명이 쓰는 상수들. 모두 내장 기본값이 있어 설정이 없어도 돈다.
/// 새 값을 원격화하려면 필드 + [`RemoteConfig::apply`]의 match 한 줄 + [`RemoteConfig::LOOKS_VALID`]에
/// 키만 늘리면 된다.
#[derive(Clone, Debug)]
pub struct RemoteConfig {
    /// 개발자 포털 clientId (원본 상수 `XABBG36SBA`).
    pub dev_client_id: String,
    /// 개발자 포털 protocolVersion (원본 상수 `QH65B2`).
    pub dev_protocol_version: String,
    /// 개발자 포털 베이스 URL — `{base}/{version}/{seg}{action}?clientId={id}` 로 쓰인다.
    pub dev_base_url: String,
    /// anisette 서버 URL. **비어 있으면 무시** — 이때 auth.rs가 (사용자 수동값 > CI 빌드값 > 공유 기본)을
    /// 쓴다. Veil에서 이 값을 두면 재빌드 없이 anisette 서버를 옮길 수 있다(로그인 계층이 가장 자주
    /// 깨지므로 원격화 효용이 크다). 사용자가 앱에서 직접 넣은 값은 여전히 이보다 우선한다.
    pub anisette_url: String,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            dev_client_id: "XABBG36SBA".into(),
            dev_protocol_version: "QH65B2".into(),
            dev_base_url: "https://developerservices2.apple.com/services".into(),
            anisette_url: String::new(), // 비어 있음 = 원격 오버라이드 없음(auth.rs가 기존 3단으로 고름)
        }
    }
}

impl RemoteConfig {
    /// 응답이 우리 설정 파일이 맞는지 볼 때 찾는 키들(엉뚱한 HTML 페이지를 캐시하지 않게).
    const LOOKS_VALID: [&'static str; 4] =
        ["dev_client_id=", "dev_protocol_version=", "dev_base_url=", "anisette_url="];

    /// 캐시된 설정(`<state_dir>/shard-config.txt`)을 읽어 기본값 위에 덮는다. 파일이 없거나 아는 키가
    /// 없으면 그 값은 기본값 그대로 — 부분 설정도 안전하고, 구버전 앱이 모르는 새 키를 만나도 무시한다.
    pub fn load(state_dir: &Path) -> Self {
        let mut cfg = Self::default();
        if let Ok(text) = std::fs::read_to_string(state_dir.join("shard-config.txt")) {
            cfg.apply(&text);
        }
        cfg
    }

    fn apply(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            if v.is_empty() {
                continue;
            }
            match k.trim() {
                "dev_client_id" => self.dev_client_id = v.to_string(),
                "dev_protocol_version" => self.dev_protocol_version = v.to_string(),
                "dev_base_url" => self.dev_base_url = v.to_string(),
                "anisette_url" => self.anisette_url = v.to_string(),
                _ => {} // 미래 키는 무시 — 앞으로 이 목록만 늘리면 된다
            }
        }
    }

    /// 최선노력: `<state_dir>/config_url.txt`에 URL이 있으면 GET해 `shard-config.txt`에 캐시한다.
    /// 실패·타임아웃·엉뚱한 응답이면 캐시를 안 건드린다 — 기존/기본값 유지(흐름을 절대 안 깬다). resign
    /// 크레이트의 reqwest(rustls-tls)는 이미 iOS에서 포털 POST로 쓰이므로 여기 GET도 같은 경로다.
    pub async fn refresh(state_dir: &Path) {
        let Some(url) = std::fs::read_to_string(state_dir.join("config_url.txt"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| s.starts_with("http"))
        else {
            return;
        };
        let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(8)).build() else {
            return;
        };
        let Ok(resp) = client.get(&url).send().await else { return };
        if !resp.status().is_success() {
            return;
        }
        let Ok(text) = resp.text().await else { return };
        if Self::LOOKS_VALID.iter().any(|k| text.contains(k)) {
            let _ = std::fs::write(state_dir.join("shard-config.txt"), text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_partial_config_keeps_the_built_in_defaults() {
        // 부분 설정: 하나만 덮고 나머지는 기본값. 모르는 키/주석/빈 줄은 무시.
        let mut cfg = RemoteConfig::default();
        cfg.apply("# 주석\n\ndev_protocol_version = ZZ99Q1 \nfuture_key=whatever\n");
        assert_eq!(cfg.dev_protocol_version, "ZZ99Q1");
        assert_eq!(cfg.dev_client_id, "XABBG36SBA"); // 안 건드린 값은 기본값
        assert_eq!(cfg.dev_base_url, "https://developerservices2.apple.com/services");
    }
}
