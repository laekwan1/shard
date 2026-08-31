//! ② Apple Developer Services API — 인증서·App ID·프로비저닝 프로파일 발급/갱신.
//!
//! 왜 여기서 직접 구현하나: SideStore의 Rust 포팅본(apple-private-apis/`apple-dev-apis`)은
//! `XcodeSession::with()`가 `todo!()`인 **스텁**이다(2026-08-31 소스 확인). 실제 ② 로직은
//! SideStore가 AltSign(Swift/ObjC, `ALTAppleAPI`)으로 처리한다 — Rust 포팅본이 없다. 그래서
//! 우리는 그 요청형식을 **참고**해 Rust로 직접 짠다: `developerservices2.apple.com`에 ③ 세션의
//! auth token + anisette를 실어 plist POST.
//!
//! 검증 경계: 이 요청들은 애플 실서버 + 로그인 세션이 있어야만 검증된다 → **폰(또는 실계정)**.
//! 지금은 시그니처/흐름 골격만 두고, 요청 본문(plist)은 AltSign 참조해 다음 증분에서 채운다.
//! 스텁은 `todo!()` 대신 `bail!`로 둔다 — 실수로 호출돼도 패닉이 아니라 에러로 드러나게.

use anyhow::{bail, Result};

use crate::auth::AppleSession;

/// 무료 개발 인증서(개인 팀). 서명(①)에 쓰는 인증서. 개인키는 우리가 만들어 CSR로 제출하고
/// 보관한다(apple-codesign의 서명 키와 연결).
pub struct DevCertificate {
    pub serial: String,
    pub cert_der: Vec<u8>,
}

/// 앱 식별자(App ID). 번들ID당 하나. 무료 계정은 주당 10개 한도.
pub struct AppId {
    pub identifier: String,
    pub name: String,
}

/// 프로비저닝 프로파일(7일). **㉮ 무중단 갱신의 대상** = 이것만 새로 받아 misagent에 심으면
/// 앱 재설치 없이 유효기간이 연장된다.
pub struct ProvisioningProfile {
    pub bundle_id: String,
    pub profile_der: Vec<u8>,
}

/// free personal team.
pub struct Team {
    pub team_id: String,
}

/// ② 클라이언트. ③ 세션(auth token + anisette)을 실어 애플 개발자 서비스에 말한다.
pub struct DeveloperApi<'a> {
    session: &'a AppleSession,
}

impl<'a> DeveloperApi<'a> {
    pub fn new(session: &'a AppleSession) -> Self {
        Self { session }
    }

    // 아래 연산은 AltSign `ALTAppleAPI`에 대응한다. 요청 본문은 폰 검증과 함께 채운다.
    // session은 각 요청 헤더(X-Apple-I-MD 등 anisette + auth token)에 쓰인다.

    /// 개인 팀 조회(listTeams). 대개 하나.
    pub async fn team(&self) -> Result<Team> {
        let _ = self.session;
        bail!("② TODO: listTeams — AltSign 참조, 폰에서 검증")
    }

    /// 유효한 개발 인증서가 있으면 반환, 없으면 CSR 제출해 새로 발급
    /// (listAllDevelopmentCerts + submitDevelopmentCSR).
    pub async fn ensure_certificate(&self) -> Result<DevCertificate> {
        let _ = self.session;
        bail!("② TODO: listAllDevelopmentCerts + submitDevelopmentCSR")
    }

    /// 번들ID의 App ID 확보(listAppIds + addAppId). 주당 10개 한도라 있으면 재사용.
    pub async fn ensure_app_id(&self, bundle_id: &str) -> Result<AppId> {
        let _ = (self.session, bundle_id);
        bail!("② TODO: listAppIds + addAppId")
    }

    /// 프로파일 발급/갱신(downloadTeamProvisioningProfile). **㉮ 갱신의 핵심** — 이걸 새로 받아
    /// misagent에 심는 게 무중단 재서명이다.
    pub async fn issue_profile(&self, bundle_id: &str) -> Result<ProvisioningProfile> {
        let _ = (self.session, bundle_id);
        bail!("② TODO: downloadTeamProvisioningProfile")
    }
}
