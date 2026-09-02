//! ⑤ 재서명 오케스트레이션 — ①②③를 하나로 잇는다(설치 ④는 별도).
//!
//! 흐름(참조: Dadoum/Sideloader `certificateidentity.d` + `sign.d`):
//!   로그인(③) → 팀 → 인증서(키 생성+CSR+제출+취득) → App ID → 프로파일(②)
//!   → 프로파일을 `embedded.mobileprovision`로 박고, 프로파일의 Entitlements로 재서명(①).
//!
//! 검증 경계: **호스트 컴파일까지**. 로그인·발급·프로파일은 애플 실서버+세션이 있어야 하니
//! **폰(또는 실계정)** 에서 실증한다. 이 함수가 "설치(④)만 빼고 재서명 전 과정을 코드로 이은"
//! 지점이다.

use std::fs::{self, File};
use std::io::{self, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use apple_codesign::{
    create_self_signed_code_signing_certificate, CertificateProfile, SettingsScope, SigningSettings,
    UnifiedSigner,
};
use plist::{Dictionary, Value};
use x509_certificate::{
    CapturedX509Certificate, InMemorySigningKeyPair, KeyAlgorithm, X509CertificateBuilder,
};

use crate::auth::AppleSession;
use crate::dev_api::{AppId, DeveloperApi, DeveloperTeam};

/// 재서명 입력.
pub struct ResignRequest {
    pub email: String,
    pub password: String,
    /// 번들 식별자 (예: net.sw.shard).
    pub bundle_id: String,
    /// 표시 이름 (예: Shard).
    pub app_name: String,
}

/// 전체 재서명. 성공 시 서명된 `.app` 경로를 돌려준다(설치는 ④의 몫).
/// `tfa`는 2FA 코드 콜백(폰 UI가 물어봄), `ipa`는 미서명 .ipa, `work`는 작업 폴더.
pub async fn resign_app(
    req: &ResignRequest,
    ipa: &Path,
    tfa: impl Fn() -> String,
    state_dir: PathBuf,
    work: &Path,
    log: &mut dyn FnMut(&str),
) -> Result<PathBuf> {
    // 1) 로그인 (③)
    let session =
        AppleSession::login(req.email.clone(), req.password.clone(), tfa, state_dir, log).await?;
    let dev = DeveloperApi::new(session);

    // 2) 팀 (무료 개인 팀은 대개 하나)
    let team = dev
        .list_teams()
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("개발 팀 없음(무료 계정인지 확인)"))?;

    // 3) 인증서: 키 생성 → CSR → 제출 → 취득. 이 키가 서명키가 된다.
    let key = generate_rsa_signing_key(&req.app_name)?;
    let apple_cert = ensure_certificate(&dev, &team, &req.app_name, &key).await?;

    // 4) App ID (있으면 재사용)
    let app_id = ensure_app_id(&dev, &team, &req.bundle_id, &req.app_name).await?;

    // 5) 프로파일 (②) — ㉮ 무중단 갱신의 대상이기도 하다.
    let profile = dev.download_profile(&team, &app_id).await?;

    // 6) 재서명 (①): 번들에 프로파일 박고, 프로파일의 Entitlements로 서명.
    let app_dir = extract_ipa(ipa, &work.join("extracted"))?;
    fs::write(
        app_dir.join("embedded.mobileprovision"),
        &profile.encoded_profile,
    )
    .context("embedded.mobileprovision 쓰기")?;
    let entitlements_xml = entitlements_xml_from_profile(&profile.encoded_profile)?;

    let signed = work.join("signed").join(app_dir.file_name().unwrap());
    fs::create_dir_all(signed.parent().unwrap())?;

    let mut settings = SigningSettings::default();
    settings.set_signing_key(&key, apple_cert);
    settings
        .set_entitlements_xml(SettingsScope::Main, &entitlements_xml)
        .map_err(|e| anyhow!("엔티틀먼트 설정: {e:?}"))?;
    UnifiedSigner::new(settings)
        .sign_path(&app_dir, &signed)
        .map_err(|e| anyhow!("재서명 실패: {e:?}"))?;

    // 7) 설치용 .ipa로 재포장(Payload/<app>).
    let out_ipa = work.join("Shard-signed.ipa");
    repackage_ipa(&signed, &out_ipa)?;
    Ok(out_ipa)
}

/// 서명된 `.app`을 `Payload/<app>` 구조의 .ipa(zip)로 묶는다.
fn repackage_ipa(app_dir: &Path, out: &Path) -> Result<()> {
    let app_name = app_dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("앱 이름 없음"))?;
    let mut zip = zip::ZipWriter::new(File::create(out).context("ipa 생성")?);
    let base = format!("Payload/{app_name}");
    add_dir_to_zip(&mut zip, app_dir, &base)?;
    zip.finish().context("ipa 마무리")?;
    Ok(())
}

/// 디렉터리를 재귀로 zip에 넣는다. zip 경로는 슬래시(/)로.
fn add_dir_to_zip(
    zip: &mut zip::ZipWriter<File>,
    dir: &Path,
    zip_prefix: &str,
) -> Result<()> {
    let opts: zip::write::FileOptions<()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| anyhow!("비 UTF-8 파일명"))?;
        let zip_path = format!("{zip_prefix}/{name}");
        if path.is_dir() {
            add_dir_to_zip(zip, &path, &zip_path)?;
        } else {
            zip.start_file(&zip_path, opts)
                .with_context(|| format!("zip 항목 {zip_path}"))?;
            let bytes = fs::read(&path)?;
            zip.write_all(&bytes)?;
        }
    }
    Ok(())
}

/// iOS C ABI(동기)에서 부르는 블로킹 진입점의 입력.
pub struct ResignAndInstall {
    pub req: ResignRequest,
    /// 미서명 .ipa 경로.
    pub ipa: PathBuf,
    /// anisette·세션 캐시 디렉터리(기기별).
    pub state_dir: PathBuf,
    /// 작업 폴더(추출·서명·재포장).
    pub work_dir: PathBuf,
    /// 설치까지 하려면 기기 연결(LocalDevVPN 터널 주소 + 페어링 파일). 없으면 서명만 하고 반환.
    pub device_addr: Option<IpAddr>,
    pub pairing_path: Option<PathBuf>,
}

/// 로그인→재서명(→설치)을 **동기로** 실행한다(iOS C ABI용). current-thread 런타임이라 Send 불필요.
/// `tfa`는 2FA 코드를 돌려주는 콜백(Swift 다이얼로그), `log`는 진행 로그(Swift 화면 표시).
pub fn resign_and_install_blocking(
    p: ResignAndInstall,
    tfa: &dyn Fn() -> String,
    log: &mut dyn FnMut(&str),
) -> Result<PathBuf> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow!("tokio 런타임: {e}"))?;
    rt.block_on(async {
        let signed = resign_app(&p.req, &p.ipa, || tfa(), p.state_dir.clone(), &p.work_dir, log).await?;
        log("서명 완료.");
        if let (Some(addr), Some(pairing_path)) = (p.device_addr, p.pairing_path.as_ref()) {
            log("기기에 설치 중...");
            let pairing = fs::read(pairing_path).context("페어링 파일 읽기")?;
            let provider = crate::install::tcp_provider(addr, &pairing, "shard-resign")?;
            crate::install::install_or_upgrade_app(&provider, &signed).await?;
            log("설치 완료.");
        }
        Ok(signed)
    })
}

/// 첫 폰 테스트용 — .ipa/설치 없이 **애플 실서버 왕복(②③)만** 검증한다:
/// 로그인 → 팀 → 인증서(CSR 제출) → App ID → 프로파일 발급. 성공하면 요약 문자열.
/// 이게 되면 가장 어려운 인증·발급이 폰에서 실증된 것 — 서명·설치는 그 다음.
pub fn verify_apple_flow_blocking(
    email: String,
    password: String,
    bundle_id: String,
    app_name: String,
    state_dir: PathBuf,
    tfa: &dyn Fn() -> String,
    log: &mut dyn FnMut(&str),
) -> Result<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow!("tokio 런타임: {e}"))?;
    rt.block_on(async {
        let session = AppleSession::login(email, password, || tfa(), state_dir, log).await?;
        let dev = DeveloperApi::new(session);

        log("팀 조회...");
        let team = dev
            .list_teams()
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("개발 팀 없음(무료 계정 확인)"))?;
        log(&format!("팀: {} ({})", team.name, team.team_id));

        log("인증서 확보(CSR 제출)...");
        let key = generate_rsa_signing_key(&app_name)?;
        let _cert = ensure_certificate(&dev, &team, &app_name, &key).await?;
        log("인증서 OK.");

        log("App ID 확보...");
        let app_id = ensure_app_id(&dev, &team, &bundle_id, &app_name).await?;
        log(&format!("App ID: {}", app_id.identifier));

        log("프로파일 발급...");
        let profile = dev.download_profile(&team, &app_id).await?;
        log(&format!(
            "프로파일: {} ({} bytes)",
            profile.name,
            profile.encoded_profile.len()
        ));

        Ok(format!(
            "team={}; appId={}; profile={}B",
            team.team_id,
            app_id.identifier,
            profile.encoded_profile.len()
        ))
    })
}

/// RSA 서명 키를 만든다. x509-certificate의 `generate_random`은 RSA를 지원하지 않아
/// `RsaKeyGenerationNotSupported`가 난다(폰에서 확인). apple-codesign의 self-signed 생성기는
/// rsa 크레이트로 RSA를 만드므로, 그걸로 **키만** 얻고 딸려오는 던지는 인증서는 버린다.
fn generate_rsa_signing_key(app_name: &str) -> Result<InMemorySigningKeyPair> {
    let (_throwaway_cert, key) = create_self_signed_code_signing_certificate(
        KeyAlgorithm::Rsa,
        CertificateProfile::AppleDevelopment,
        "TEMP",
        app_name,
        "US",
        chrono::Duration::try_days(1).ok_or_else(|| anyhow!("잘못된 유효기간"))?,
    )
    .map_err(|e| anyhow!("RSA 키 생성: {e:?}"))?;
    Ok(key)
}

/// 인증서 확보: 키로 CSR을 만들어 제출하고, 발급된 애플 인증서 DER을 취득한다.
/// (Dadoum certificateidentity.d: submitDevelopmentCSR → listAllDevelopmentCerts에서 매칭.)
async fn ensure_certificate(
    dev: &DeveloperApi<AppleSession>,
    team: &DeveloperTeam,
    app_name: &str,
    key: &InMemorySigningKeyPair,
) -> Result<CapturedX509Certificate> {
    let mut builder = X509CertificateBuilder::default();
    builder
        .subject()
        .append_common_name_utf8_string(&format!("{app_name} Development"))
        .map_err(|e| anyhow!("CSR subject: {e:?}"))?;
    let csr_pem = builder
        .create_certificate_signing_request(key)
        .map_err(|e| anyhow!("CSR 생성: {e:?}"))?
        .encode_pem()
        .map_err(|e| anyhow!("CSR PEM 인코딩: {e:?}"))?;

    let cert_id = dev.submit_csr(team, app_name, &csr_pem).await?;

    // 방금 만든 인증서를 목록에서 찾아 DER 취득. id가 안 맞으면(포털 응답 차이) 최신 것으로 폴백.
    let certs = dev.list_certificates(team).await?;
    let ours = certs
        .iter()
        .find(|c| c.certificate_id == cert_id)
        .or_else(|| certs.last())
        .ok_or_else(|| anyhow!("발급된 인증서를 못 찾음"))?;
    CapturedX509Certificate::from_der(ours.cert_content.clone())
        .map_err(|e| anyhow!("애플 인증서 파싱: {e:?}"))
}

/// 번들ID의 App ID 확보(있으면 재사용, 없으면 등록 후 재조회).
async fn ensure_app_id(
    dev: &DeveloperApi<AppleSession>,
    team: &DeveloperTeam,
    bundle_id: &str,
    app_name: &str,
) -> Result<AppId> {
    let list = dev.list_app_ids(team).await?;
    if let Some(existing) = list.app_ids.iter().find(|a| a.identifier == bundle_id) {
        return Ok(existing.clone());
    }
    dev.add_app_id(team, bundle_id, app_name).await?;
    dev.list_app_ids(team)
        .await?
        .app_ids
        .into_iter()
        .find(|a| a.identifier == bundle_id)
        .ok_or_else(|| anyhow!("등록한 App ID를 못 찾음"))
}

/// mobileprovision(CMS로 감싼 plist)에서 Entitlements를 뽑아 XML로. (Dadoum sign.d의 profilePlist["Entitlements"].)
fn entitlements_xml_from_profile(profile: &[u8]) -> Result<String> {
    let plist = plist_from_mobileprovision(profile)?;
    let ent = plist
        .get("Entitlements")
        .and_then(Value::as_dictionary)
        .ok_or_else(|| anyhow!("프로파일에 Entitlements 없음"))?;
    let mut xml = Vec::new();
    plist::to_writer_xml(&mut xml, &Value::Dictionary(ent.clone()))?;
    String::from_utf8(xml).context("엔티틀먼트 XML utf8")
}

/// mobileprovision에서 plist를 꺼낸다. CMS 전체를 파싱하는 대신 안에 그대로 들어있는 XML plist
/// 구간을 스캔한다(zsign 등이 쓰는 방식) — 견고한 대안은 CMS SignedData 파싱.
fn plist_from_mobileprovision(data: &[u8]) -> Result<Dictionary> {
    let start = find_sub(data, b"<?xml")
        .or_else(|| find_sub(data, b"<plist"))
        .ok_or_else(|| anyhow!("프로파일에서 plist 시작을 못 찾음"))?;
    let end_tag = b"</plist>";
    let end = find_sub(&data[start..], end_tag)
        .map(|e| start + e + end_tag.len())
        .ok_or_else(|| anyhow!("프로파일에서 plist 끝을 못 찾음"))?;
    plist::from_bytes(&data[start..end]).context("프로파일 plist 파싱")
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 재포장이 iOS가 요구하는 Payload/<app>/... 구조를(중첩 프레임워크 포함) 만드는지 지킨다.
    // 이걸 틀리면 .ipa가 설치 불가라 되돌아온다.
    #[test]
    fn a_repackaged_ipa_nests_the_app_and_its_frameworks_under_payload() {
        let tmp = std::env::temp_dir().join("resign-repack-test");
        let _ = fs::remove_dir_all(&tmp);
        let app = tmp.join("Foo.app");
        fs::create_dir_all(app.join("Frameworks/Bar.framework")).unwrap();
        fs::write(app.join("Foo"), b"macho").unwrap();
        fs::write(app.join("Frameworks/Bar.framework/Bar"), b"fw").unwrap();

        let out = tmp.join("out.ipa");
        repackage_ipa(&app, &out).unwrap();

        let mut zip = zip::ZipArchive::new(File::open(&out).unwrap()).unwrap();
        let names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.iter().any(|n| n == "Payload/Foo.app/Foo"), "{names:?}");
        assert!(
            names
                .iter()
                .any(|n| n == "Payload/Foo.app/Frameworks/Bar.framework/Bar"),
            "{names:?}"
        );
    }
}

/// .ipa를 풀고 Payload/*.app 경로를 돌려준다. (main.rs의 것과 같은 로직 — 엔진용 사본.)
fn extract_ipa(ipa: &Path, dest: &Path) -> Result<PathBuf> {
    let mut zip = zip::ZipArchive::new(File::open(ipa).with_context(|| format!("open {}", ipa.display()))?)?;
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        let Some(rel) = e.enclosed_name() else {
            continue;
        };
        let out = dest.join(rel);
        if e.is_dir() {
            fs::create_dir_all(&out)?;
        } else {
            if let Some(p) = out.parent() {
                fs::create_dir_all(p)?;
            }
            io::copy(&mut e, &mut File::create(&out)?)?;
        }
    }
    fs::read_dir(dest.join("Payload"))
        .context("ipa에 Payload/ 없음")?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "app").unwrap_or(false))
        .ok_or_else(|| anyhow!("Payload/에 .app 없음"))
}
