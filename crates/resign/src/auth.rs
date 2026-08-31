//! ③ Apple ID 로그인 + anisette — icloud_auth/omnisette(SideStore apple-private-apis) 위 얇은 층.
//!
//! 왜 포팅이 아니라 의존인가: 이 부분은 SideStore가 **이미 Rust로 구현**해 뒀고(로그인·2FA·GSA·
//! anisette), 우리 워크스페이스에서 그대로 컴파일된다(호스트 확인). 애플이 자주 깨는 곳이라
//! 직접 재구현하면 유지보수가 지옥 — 검증된 코드를 쓰고, 나중에 필요하면 트림한다.
//!
//! 검증 경계: 여기까지는 컴파일로만 확인. **실제 로그인은 Apple ID+2FA와 anisette가 필요 =
//! 실기기(또는 anisette 서버)에서만 실증**된다. 아래는 "그 크레이트를 어떻게 부르는가"의 골격.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use icloud_auth::AppleAccount;
use omnisette::AnisetteConfiguration;

/// 로그인된 애플 세션. ②(Developer API)가 이 세션의 인증 토큰·anisette를 실어 요청한다.
pub struct AppleSession {
    account: AppleAccount,
}

impl AppleSession {
    /// Apple ID로 로그인한다. `tfa`는 2FA 코드를 돌려주는 콜백(폰 UI가 사용자에게 물어봄).
    /// anisette 상태는 `state_dir`에 기기별로 캐시된다.
    ///
    /// NOTE(다음 증분): icloud_auth의 `login`은 2FA를 **동기 콜백**으로 받는다. 폰에선 2FA 입력이
    /// 비동기라, `login_email_pass`+`send_2fa_to_devices`+`verify_2fa` 단계머신으로 바꿔 UI에
    /// 붙여야 자연스럽다. 지금은 흐름 확인용으로 콜백형 `login`을 감싼다.
    pub async fn login(
        email: String,
        password: String,
        tfa: impl Fn() -> String,
        state_dir: PathBuf,
    ) -> Result<Self> {
        let config = AnisetteConfiguration::new().set_configuration_path(state_dir);
        let account = AppleAccount::login(move || (email.clone(), password.clone()), tfa, config)
            .await
            .map_err(|e| anyhow!("apple login failed: {e:?}"))?;
        Ok(Self { account })
    }

    /// 현재 anisette 데이터(만료 시 내부에서 자동 갱신). ②의 모든 요청 헤더에 실린다.
    pub async fn anisette(&self) -> icloud_auth::anisette::AnisetteData {
        self.account.get_anisette().await
    }

    /// ②(dev_api)가 요청 헤더(auth token)를 만들 때 쓸 내부 계정 핸들. ②가 아직 골격이라
    /// 지금은 미사용 — 구현되면 쓰인다.
    #[allow(dead_code)]
    pub(crate) fn account(&self) -> &AppleAccount {
        &self.account
    }
}
