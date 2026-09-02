//! ③ Apple ID 로그인 + anisette + ② 전송층(인증된 plist POST).
//!
//! - **로그인/anisette**: icloud_auth/omnisette(SideStore)를 감싼다 — 이 부분은 이미 Rust로
//!   구현돼 있어(로그인·2FA·GSA·anisette) 그대로 쓴다.
//! - **Xcode GS 토큰**: 개발자 포털(②) 인증에 필요한 `X-Apple-GS-Token`. icloud_auth의
//!   `get_app_token`이 GSA `apptokens` 요청까지 하고 **응답의 암호화 토큰(et) 복호 직전 `todo!()`**
//!   라, 그 마지막 단계를 우리가 마저 한다(AES-GCM). `spd`가 public이라 fork 없이 가능.
//! - **Transport**: dev_api.rs의 심을 구현 — 포털 헤더(anisette + dsid + GS 토큰) + plist POST.
//!
//! 검증 경계: **컴파일까지 확인**. 실제 왕복은 Apple ID 로그인 세션 + 애플 실서버가 있어야 하므로
//! **폰(또는 실계정)** 에서 실증한다. 크립토/헤더/plist 키는 전부 동작하는 참조(Dadoum/Sideloader의
//! developersession.d·appleaccount.d, icloud_auth get_app_token)에서 옮겼지만, 미세한 형식 오차는
//! 폰 왕복으로만 드러난다 — 그 build-test-fix 루프가 이 층의 검증 방식이다.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::aes::Aes256;
use aes_gcm::AesGcm;
use anyhow::{anyhow, bail, Context, Result};
use hmac::{Hmac, Mac};
use icloud_auth::AppleAccount;
use omnisette::AnisetteConfiguration;
use plist::{Dictionary, Value};
use sha2::Sha256;

use crate::dev_api::Transport;

/// Xcode 개발자 인증에 쓰는 앱 식별자(GSA apptokens의 대상).
const XCODE_APP_ID: &str = "com.apple.gs.xcode.auth";
/// GSA 엔드포인트(icloud_auth와 동일). apptokens 요청을 여기로 POST.
const GSA_ENDPOINT: &str = "https://gsa.apple.com/grandslam/GsService2";

/// AES-256-GCM, **16바이트 nonce**(애플 et 형식). 기본 12B가 아니라 16B라 커스텀 타입이 필요하다.
type XcodeGcm = AesGcm<Aes256, aes_gcm::aead::consts::U16>;
type HmacSha256 = Hmac<Sha256>;

/// gsa.apple.com은 **애플 자체 루트 CA**로 서명돼 webpki 기본 루트로는 `UnknownIssuer`가 난다
/// (폰 로그로 확인). icloud_auth의 로그인 클라이언트가 애플 루트를 명시적으로 추가하는 이유.
/// 공개 루트 + 애플 루트를 모두 신뢰하는 클라이언트를 만든다.
fn apple_client() -> Result<reqwest::Client> {
    let root = reqwest::Certificate::from_der(include_bytes!("apple_root.der"))
        .context("애플 루트 인증서")?;
    reqwest::Client::builder()
        .add_root_certificate(root)
        .build()
        .context("reqwest 클라이언트")
}

/// 로그인된 애플 세션. ②(Developer API)가 이 세션의 dsid·anisette·GS 토큰을 실어 요청한다.
pub struct AppleSession {
    account: AppleAccount,
    /// Xcode GS 토큰 캐시(요청마다 GSA 왕복을 피함). 세션 수명 동안 유효.
    cached_gs_token: Mutex<Option<String>>,
}

impl AppleSession {
    /// Apple ID로 로그인. `tfa`는 2FA 코드를 돌려주는 콜백(폰 UI가 사용자에게 물어봄).
    /// anisette 상태는 `state_dir`에 기기별로 캐시된다. `log`로 단계를 알린다(어디서 막히는지 보이게).
    ///
    /// anisette를 로그인과 분리해 먼저 받는다 — 사이드로드 iOS에선 온디바이스 ADI(SSC)가 막혀
    /// 이 단계에서 멈추는 일이 잦아, 로그로 정확히 짚기 위함.
    pub async fn login(
        email: String,
        password: String,
        tfa: impl Fn() -> String,
        state_dir: PathBuf,
        log: &mut dyn FnMut(&str),
    ) -> Result<Self> {
        // anisette_url을 두면 (fork 패치가) v1 원격을 쓴다. ani.sidestore.io는 v1(GET)을 지원 —
        // v3 프로비저닝(EndProvisioningError로 실패)을 피한다.
        let config = AnisetteConfiguration::new()
            .set_configuration_path(state_dir.clone())
            .set_anisette_url("https://ani.sidestore.io".to_string());
        log("anisette 준비 중(원격 v1: ani.sidestore.io)...");
        let anisette = icloud_auth::anisette::AnisetteData::new(config)
            .await
            .map_err(|e| anyhow!("anisette 실패: {e:?}"))?;
        log("anisette OK. Apple ID 로그인 요청 중...");
        let account = AppleAccount::login_with_anisette(
            move || (email.clone(), password.clone()),
            tfa,
            anisette,
        )
        .await
        .map_err(|e| anyhow!("apple login failed: {e:?}"))?;
        let session = Self {
            account,
            cached_gs_token: Mutex::new(None),
        };
        // 세션을 저장해 다음엔 재로그인을 피한다 — 반복 로그인이 계정 잠금의 최대 원인.
        session.save_session(&state_dir);
        Ok(session)
    }

    /// 저장된 세션(spd)이 있으면 로그인 없이 복원한다. anisette만 가볍게 새로 받고(로그인 아님)
    /// 저장해 둔 세션 데이터를 붙인다. 없거나 못 읽으면 None → 호출자가 로그인.
    /// **반복 로그인이 애플의 "낯선 기기 반복 접근" 잠금을 유발하므로, 가능하면 이걸로 건너뛴다.**
    pub async fn resume(state_dir: &Path, log: &mut dyn FnMut(&str)) -> Option<Self> {
        let path = state_dir.join("session.plist");
        let spd = plist::Value::from_reader_xml(std::fs::File::open(&path).ok()?)
            .ok()?
            .into_dictionary()?;
        log("저장된 세션 발견 — 로그인 생략(anisette만 갱신)...");
        let config = AnisetteConfiguration::new()
            .set_configuration_path(state_dir.to_path_buf())
            .set_anisette_url("https://ani.sidestore.io".to_string());
        let anisette = icloud_auth::anisette::AnisetteData::new(config).await.ok()?;
        let mut account = AppleAccount::new_with_anisette(anisette).ok()?;
        account.spd = Some(spd);
        Some(Self {
            account,
            cached_gs_token: Mutex::new(None),
        })
    }

    /// 저장된 세션이 있으면 복원, 없으면 로그인. 반환의 bool은 "복원했나"(true=로그인 안 함).
    pub async fn resume_or_login(
        email: String,
        password: String,
        tfa: impl Fn() -> String,
        state_dir: PathBuf,
        log: &mut dyn FnMut(&str),
    ) -> Result<(Self, bool)> {
        if let Some(s) = Self::resume(&state_dir, log).await {
            return Ok((s, true));
        }
        log("저장된 세션 없음 — 로그인합니다.");
        let s = Self::login(email, password, tfa, state_dir, log).await?;
        Ok((s, false))
    }

    /// spd(로그인 세션)를 파일로 저장. 다음 실행이 재로그인 없이 복원하게. (제품은 Keychain 권장.)
    fn save_session(&self, state_dir: &Path) {
        if let Some(spd) = &self.account.spd {
            let path = state_dir.join("session.plist");
            if let Ok(f) = std::fs::File::create(&path) {
                let _ = plist::to_writer_xml(f, &Value::Dictionary(spd.clone()));
            }
        }
    }

    /// 저장된 세션 삭제 — 만료/실패로 못 쓸 때 호출해 다음 실행이 새로 로그인하게.
    pub fn clear_session(state_dir: &Path) {
        let _ = std::fs::remove_file(state_dir.join("session.plist"));
    }

    /// spd(로그인 후 서버가 준 세션 데이터)에서 문자열 필드. adsid/GsIdmsToken 등.
    fn spd_str(&self, key: &str) -> Result<String> {
        self.account
            .spd
            .as_ref()
            .ok_or_else(|| anyhow!("no spd (로그인 안 됨)"))?
            .get(key)
            .and_then(Value::as_string)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("spd['{key}'] 없음"))
    }

    /// spd의 데이터 필드. sk(세션키)/c 등.
    fn spd_data(&self, key: &str) -> Result<Vec<u8>> {
        self.account
            .spd
            .as_ref()
            .ok_or_else(|| anyhow!("no spd (로그인 안 됨)"))?
            .get(key)
            .and_then(Value::as_data)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| anyhow!("spd['{key}'] 없음"))
    }

    /// 개발자 포털 인증용 Xcode GS 토큰(캐시). 없으면 GSA에서 받아 복호한다.
    async fn gs_token(&self) -> Result<String> {
        if let Some(t) = self.cached_gs_token.lock().unwrap().clone() {
            return Ok(t);
        }
        let t = self.fetch_xcode_gs_token().await?;
        *self.cached_gs_token.lock().unwrap() = Some(t.clone());
        Ok(t)
    }

    /// GSA `apptokens`로 Xcode 앱 토큰을 받아 복호한다(icloud_auth get_app_token의 완성판).
    async fn fetch_xcode_gs_token(&self) -> Result<String> {
        let adsid = self.spd_str("adsid")?;
        let idms_token = self.spd_str("GsIdmsToken")?;
        let sk = self.spd_data("sk")?;
        let c = self.spd_data("c")?;
        let anisette = self.account.get_anisette().await;

        // checksum = HMAC-SHA256(sk, "apptokens" || adsid || appId) — icloud_auth create_checksum과 동일.
        let mut mac = <HmacSha256 as Mac>::new_from_slice(&sk)
            .map_err(|_| anyhow!("bad session key length"))?;
        mac.update(b"apptokens");
        mac.update(adsid.as_bytes());
        mac.update(XCODE_APP_ID.as_bytes());
        let checksum = mac.finalize().into_bytes().to_vec();

        // 요청 plist(키는 icloud_auth AuthTokenRequest의 serde 이름과 동일: Header/Request, o/t/u...).
        let mut header = Dictionary::new();
        header.insert("Version".into(), "1.0.1".into());
        let mut body = Dictionary::new();
        body.insert("app".into(), Value::Array(vec![XCODE_APP_ID.into()]));
        body.insert("c".into(), Value::Data(c));
        body.insert("cpd".into(), Value::Dictionary(anisette.to_plist(true, false, false)));
        body.insert("o".into(), "apptokens".into());
        body.insert("t".into(), idms_token.into());
        body.insert("u".into(), adsid.into());
        body.insert("checksum".into(), Value::Data(checksum));
        let mut req = Dictionary::new();
        req.insert("Header".into(), Value::Dictionary(header));
        req.insert("Request".into(), Value::Dictionary(body));

        let mut xml = Vec::new();
        plist::to_writer_xml(&mut xml, &Value::Dictionary(req)).context("plist 직렬화")?;

        let client = apple_client()?;
        let resp = client
            .post(GSA_ENDPOINT)
            .header("Content-Type", "text/x-xml-plist")
            .header("Accept", "*/*")
            .header("User-Agent", "akd/1.0 CFNetwork/978.0.7 Darwin/18.7.0")
            .header(
                "X-MMe-Client-Info",
                anisette
                    .get_header("x-mme-client-info")
                    .map_err(|e| anyhow!("anisette client-info: {e:?}"))?,
            )
            .body(xml)
            .send()
            .await
            .context("GSA apptokens POST")?;
        let bytes = resp.bytes().await.context("GSA 응답 읽기")?;

        // 응답은 {"Response": { "et": <encryptedToken>, "Status": ... }} (icloud_auth parse_response와 동일).
        let outer: Dictionary = plist::from_bytes(&bytes).context("GSA 응답 plist")?;
        let response = outer
            .get("Response")
            .and_then(Value::as_dictionary)
            .ok_or_else(|| anyhow!("GSA 응답에 Response 없음"))?;
        let et = response
            .get("et")
            .and_then(Value::as_data)
            .ok_or_else(|| anyhow!("GSA 응답에 et 없음(2FA/인증 실패 가능)"))?;

        decrypt_app_token(&sk, et)
    }
}

/// et를 복호해 Xcode 앱 토큰 문자열을 뽑는다.
/// 형식(Dadoum appleaccount.d): "XYZ"(3B, AAD) + nonce(16B) + 암호문||태그. AES-256-GCM, 키=sk.
/// 복호 결과는 XML plist: ["t"][appId]["token"].
fn decrypt_app_token(session_key: &[u8], et: &[u8]) -> Result<String> {
    if et.len() < 3 + 16 + 16 || &et[0..3] != b"XYZ" {
        bail!("encrypted token 형식이 예상과 다름");
    }
    if session_key.len() != 32 {
        bail!("session key가 32B가 아님(AES-256 아님)");
    }
    let cipher = XcodeGcm::new(GenericArray::from_slice(session_key));
    let nonce = GenericArray::from_slice(&et[3..3 + 16]);
    let plaintext = cipher
        .decrypt(
            nonce,
            Payload {
                msg: &et[3 + 16..],
                aad: &et[0..3],
            },
        )
        .map_err(|_| anyhow!("app token 복호 실패(GCM)"))?;

    let decrypted: Dictionary = plist::from_bytes(&plaintext).context("복호된 토큰 plist")?;
    decrypted
        .get("t")
        .and_then(Value::as_dictionary)
        .and_then(|t| t.get(XCODE_APP_ID))
        .and_then(Value::as_dictionary)
        .and_then(|a| a.get("token"))
        .and_then(Value::as_string)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("복호된 토큰에 t[{XCODE_APP_ID}][token] 없음"))
}

impl Transport for AppleSession {
    async fn post_plist(&self, url: &str, body: Dictionary) -> Result<Dictionary> {
        let anisette = self.account.get_anisette().await;
        let adsid = self.spd_str("adsid")?;
        let token = self.gs_token().await?;

        // 포털 헤더(Dadoum appleaccount.d sendRequest): anisette + dsid + GS 토큰 + Xcode app-info.
        // generate_headers(cpd=false, client_info=true, app_info=true) = X-Apple-I-MD*, X-Mme-*,
        // X-Apple-I-Client-Time, X-Apple-Locale, X-Apple-I-TimeZone, X-Apple-App-Info, X-Xcode-Version.
        let mut headers: HashMap<String, String> = anisette.generate_headers(false, true, true);
        headers.insert("Content-Type".into(), "text/x-xml-plist".into());
        headers.insert("Accept".into(), "text/x-xml-plist".into());
        headers.insert("Accept-Language".into(), "en-us".into());
        headers.insert("X-Apple-I-Identity-Id".into(), adsid);
        headers.insert("X-Apple-GS-Token".into(), token);
        // Dadoum이 보내는데 우리가 빠뜨렸던 것 — 없으면 애플이 HTML 에러페이지를 돌려준다.
        headers.insert("User-Agent".into(), "Xcode".into());

        let mut xml = Vec::new();
        plist::to_writer_xml(&mut xml, &Value::Dictionary(body)).context("요청 plist 직렬화")?;

        let client = apple_client()?;
        let mut rb = client.post(url).body(xml);
        for (k, v) in headers {
            rb = rb.header(k, v);
        }
        let resp = rb.send().await.context("개발자 포털 POST")?;
        let status = resp.status();
        let bytes = resp.bytes().await.context("포털 응답 읽기")?;

        // 포털 응답은 dict가 최상위(resultCode 포함). plist가 아니면(압축/HTML 등) 상태·형식(hex)을 드러낸다.
        plist::from_bytes(&bytes).map_err(|e| {
            let hex: String = bytes
                .iter()
                .take(24)
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            anyhow!("포털 응답 파싱 실패 (HTTP {status}): {e} — 앞부분 hex: {hex}")
        })
    }
}
