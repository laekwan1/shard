//! ② Apple Developer Services API — 인증서·App ID·프로비저닝 프로파일 발급/갱신.
//!
//! 출처(추측 아님): `Dadoum/Sideloader`의 `source/server/developersession.d`를 충실히 Rust로
//! 옮긴 것. 그 프로젝트는 omnisette(우리 ③)와 같은 저자의 **동작하는** 사이드로더라, 엔드포인트·
//! 요청/응답 plist 형식·resultCode 처리를 그대로 가져왔다. (SideStore apple-private-apis의
//! `apple-dev-apis`는 `XcodeSession::with()=todo!()` 스텁이라 여기서 안 씀.)
//!
//! 두 층(원본과 동일):
//! - **DeveloperApi::send** — clientId/protocolVersion/requestId/userLocale를 넣고 URL을 만들고
//!   `resultCode`를 검사한다.
//! - **Transport::post_plist** — 인증 헤더(anisette + dsid + Xcode 앱 토큰) + plist 인코딩 + POST +
//!   plist 파싱. **이 심(seam)은 auth.rs가 구현한다**(다음 증분; 앱 토큰 = icloud_auth의
//!   get_app_token 마지막 AES-GCM 복호를 우리가 마저 함, spd가 public이라 fork 불필요).
//!
//! 검증 경계: 요청/응답 **형식**은 여기서 컴파일로 확정. 실제 왕복은 로그인 세션+애플 실서버가
//! 있어야 하니 **폰(또는 실계정)** 에서 검증한다.

use anyhow::{anyhow, bail, Result};
use plist::{Dictionary, Value};

const CLIENT_ID: &str = "XABBG36SBA";
const PROTOCOL_VERSION: &str = "QH65B2";

/// 개발자 포털 URL. `{seg}`는 기기종류 세그먼트, `{action}`은 `listTeams.action` 등.
fn portal_url(device: DeviceType, action: &str) -> String {
    format!(
        "https://developerservices2.apple.com/services/{PROTOCOL_VERSION}/{seg}{action}?clientId={CLIENT_ID}",
        seg = device.url_segment(),
    )
}

/// 개발자 포털 요청의 기기종류 세그먼트(원본 `urlSegment`).
#[derive(Clone, Copy)]
pub enum DeviceType {
    Any,
    IOs,
}

impl DeviceType {
    fn url_segment(self) -> &'static str {
        match self {
            DeviceType::Any => "",
            DeviceType::IOs => "ios/",
        }
    }
}

/// 인증된 plist POST. auth.rs의 세션이 구현한다 — 헤더에 anisette + dsid + Xcode 앱 토큰을 싣고,
/// 본문을 `text/x-xml-plist`로 인코딩해 보내고, 응답 plist dict를 돌려준다.
pub trait Transport {
    fn post_plist(
        &self,
        url: &str,
        body: Dictionary,
    ) -> impl std::future::Future<Output = Result<Dictionary>> + Send;
}

// ── 응답 모델 (원본 struct 그대로) ─────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct DeveloperTeam {
    pub name: String,
    pub team_id: String,
}

#[derive(Clone, Debug)]
pub struct DevelopmentCertificate {
    pub name: String,
    pub certificate_id: String,
    pub serial_number: String,
    pub cert_content: Vec<u8>,
    pub machine_name: String,
}

#[derive(Clone, Debug)]
pub struct AppId {
    pub app_id_id: String,
    pub identifier: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct ListAppIds {
    pub app_ids: Vec<AppId>,
    pub max_quantity: u64,
    pub available_quantity: u64,
}

#[derive(Clone, Debug)]
pub struct ProvisioningProfile {
    pub provisioning_profile_id: String,
    pub name: String,
    /// mobileprovision DER — ④에서 misagent(㉮)/설치(㉯)에 쓴다.
    pub encoded_profile: Vec<u8>,
}

// ── 클라이언트 ────────────────────────────────────────────────────────────────

pub struct DeveloperApi<T: Transport> {
    transport: T,
    device: DeviceType,
}

impl<T: Transport> DeveloperApi<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            device: DeviceType::IOs,
        }
    }

    /// 공통 층: 기본 파라미터를 넣고 보내고 resultCode를 검사한다(원본 `sendRequest`).
    async fn send(&self, action: &str, mut params: Dictionary) -> Result<Dictionary> {
        params.insert("clientId".into(), CLIENT_ID.into());
        params.insert("protocolVersion".into(), PROTOCOL_VERSION.into());
        params.insert(
            "requestId".into(),
            uuid::Uuid::new_v4().to_string().to_uppercase().into(),
        );
        params.insert(
            "userLocale".into(),
            Value::Array(vec!["en_US".into()]),
        );

        let url = portal_url(self.device, action);
        let resp = self.transport.post_plist(&url, params).await?;

        // resultCode != 0 이면 실패. userString → resultString 순으로 메시지.
        let code = resp
            .get("resultCode")
            .and_then(Value::as_unsigned_integer)
            .unwrap_or(0);
        if code != 0 {
            let msg = str_of(&resp, "userString")
                .or_else(|| str_of(&resp, "resultString"))
                .unwrap_or("(null)");
            bail!("developer portal error {code}: {msg}");
        }
        Ok(resp)
    }

    /// 개인 팀 목록(대개 하나).
    pub async fn list_teams(&self) -> Result<Vec<DeveloperTeam>> {
        let resp = self.send("listTeams.action", Dictionary::new()).await?;
        let teams = array_of(&resp, "teams")?
            .iter()
            .filter_map(Value::as_dictionary)
            .map(|t| DeveloperTeam {
                name: str_of(t, "name").unwrap_or("").to_string(),
                team_id: str_of(t, "teamId").unwrap_or("").to_string(),
            })
            .collect();
        Ok(teams)
    }

    /// 유효한 개발 인증서 목록.
    pub async fn list_certificates(
        &self,
        team: &DeveloperTeam,
    ) -> Result<Vec<DevelopmentCertificate>> {
        let mut req = Dictionary::new();
        req.insert("teamId".into(), team.team_id.clone().into());
        let resp = self.send("listAllDevelopmentCerts.action", req).await?;
        let certs = array_of(&resp, "certificates")?
            .iter()
            .filter_map(Value::as_dictionary)
            .map(|c| DevelopmentCertificate {
                name: str_of(c, "name").unwrap_or("").to_string(),
                certificate_id: str_of(c, "certificateId").unwrap_or("").to_string(),
                serial_number: str_of(c, "serialNumber").unwrap_or("").to_string(),
                cert_content: data_of(c, "certContent").unwrap_or_default().to_vec(),
                machine_name: str_of(c, "machineName").unwrap_or("").to_string(),
            })
            .collect();
        Ok(certs)
    }

    /// CSR을 제출해 새 개발 인증서를 발급받는다. 반환은 certRequestId.
    /// `csr`은 PEM CSR 문자열. machineName은 우리 앱 이름.
    pub async fn submit_csr(
        &self,
        team: &DeveloperTeam,
        machine_name: &str,
        csr: &str,
    ) -> Result<String> {
        let mut req = Dictionary::new();
        req.insert("teamId".into(), team.team_id.clone().into());
        req.insert(
            "machineId".into(),
            uuid::Uuid::new_v4().to_string().to_uppercase().into(),
        );
        req.insert("machineName".into(), machine_name.into());
        req.insert("csrContent".into(), csr.into());
        let resp = self.send("submitDevelopmentCSR.action", req).await?;
        let cert_request = resp
            .get("certRequest")
            .and_then(Value::as_dictionary)
            .ok_or_else(|| anyhow!("submitDevelopmentCSR: certRequest 없음"))?;
        Ok(str_of(cert_request, "certRequestId")
            .ok_or_else(|| anyhow!("certRequestId 없음"))?
            .to_string())
    }

    /// App ID 목록(+한도). 무료 계정은 주당 10개.
    pub async fn list_app_ids(&self, team: &DeveloperTeam) -> Result<ListAppIds> {
        let mut req = Dictionary::new();
        req.insert("teamId".into(), team.team_id.clone().into());
        let resp = self.send("listAppIds.action", req).await?;
        let app_ids = array_of(&resp, "appIds")?
            .iter()
            .filter_map(Value::as_dictionary)
            .map(|a| AppId {
                app_id_id: str_of(a, "appIdId").unwrap_or("").to_string(),
                identifier: str_of(a, "identifier").unwrap_or("").to_string(),
                name: str_of(a, "name").unwrap_or("").to_string(),
            })
            .collect();
        Ok(ListAppIds {
            app_ids,
            max_quantity: resp.get("maxQuantity").and_then(Value::as_unsigned_integer).unwrap_or(u64::MAX),
            available_quantity: resp
                .get("availableQuantity")
                .and_then(Value::as_unsigned_integer)
                .unwrap_or(u64::MAX),
        })
    }

    /// 새 App ID 등록.
    pub async fn add_app_id(
        &self,
        team: &DeveloperTeam,
        identifier: &str,
        name: &str,
    ) -> Result<()> {
        let mut req = Dictionary::new();
        req.insert("identifier".into(), identifier.into());
        req.insert("name".into(), name.into());
        req.insert("teamId".into(), team.team_id.clone().into());
        self.send("addAppId.action", req).await?;
        Ok(())
    }

    /// 팀 프로비저닝 프로파일 발급/갱신. **㉮ 무중단 갱신의 핵심** — 이 결과를 misagent에 심는다.
    pub async fn download_profile(
        &self,
        team: &DeveloperTeam,
        app_id: &AppId,
    ) -> Result<ProvisioningProfile> {
        let mut req = Dictionary::new();
        req.insert("appIdId".into(), app_id.app_id_id.clone().into());
        req.insert("teamId".into(), team.team_id.clone().into());
        let resp = self.send("downloadTeamProvisioningProfile.action", req).await?;
        let pp = resp
            .get("provisioningProfile")
            .and_then(Value::as_dictionary)
            .ok_or_else(|| anyhow!("provisioningProfile 없음"))?;
        Ok(ProvisioningProfile {
            provisioning_profile_id: str_of(pp, "provisioningProfileId").unwrap_or("").to_string(),
            name: str_of(pp, "name").unwrap_or("").to_string(),
            encoded_profile: data_of(pp, "encodedProfile").unwrap_or_default().to_vec(),
        })
    }
}

// ── plist 추출 헬퍼 ───────────────────────────────────────────────────────────

fn str_of<'a>(d: &'a Dictionary, key: &str) -> Option<&'a str> {
    d.get(key).and_then(Value::as_string)
}

fn data_of<'a>(d: &'a Dictionary, key: &str) -> Option<&'a [u8]> {
    d.get(key).and_then(Value::as_data)
}

fn array_of<'a>(d: &'a Dictionary, key: &str) -> Result<&'a Vec<Value>> {
    d.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("응답에 배열 '{key}' 없음"))
}
