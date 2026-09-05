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
    cryptography::DigestType, create_self_signed_code_signing_certificate, CertificateProfile,
    SettingsScope, SigningSettings, UnifiedSigner,
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
    // 1) 로그인 (③) — 저장된 세션이 있으면 재로그인 없이 복원(계정 잠금 위험 감소).
    let (session, _resumed) = AppleSession::resume_or_login(
        req.email.clone(),
        req.password.clone(),
        tfa,
        state_dir.clone(),
        log,
    )
    .await?;
    let dev = DeveloperApi::new(session);

    // 2) 팀 (무료 개인 팀은 대개 하나)
    log("[②③ 팀] 조회...");
    let team = dev
        .list_teams()
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("[②③ 팀] 개발 팀 없음(무료 계정인지 확인)"))?;
    log(&format!("[②③ 팀] {} ({})", team.name, team.team_id));

    // 3) 서명 신원(키+인증서) — 저장된 게 유효하면 재사용(재발급/폐기 없음). 이 키가 서명키가 된다.
    log("[⑤ 인증서] 확보...");
    let (key, apple_cert) =
        ensure_signing_identity(&dev, &team, &req.app_name, &state_dir, log).await?;

    // 4) App ID — **현재 번들ID 그대로** 확보(있으면 재사용). 자기 자신 갱신은 설치된 앱과 같은
    //    번들ID여야 installation_proxy가 *업그레이드*(데이터 보존)로 처리한다 — 다른 id면 별개 앱이
    //    돼 버린다. 팀ID 접미는 발급 '테스트'(run_verify_flow)에서만 쓴다(거긴 실앱이 없어 net.sw.shard
    //    전역충돌을 피하려는 것). 실제 재서명은 req.bundle_id(= 실행 중 번들의 CFBundleIdentifier).
    log(&format!("[② App ID] 확보... ({})", req.bundle_id));
    let app_id = ensure_app_id(&dev, &team, &req.bundle_id, &req.app_name).await?;

    // 5) 프로파일 (②) — ㉮ 무중단 갱신의 대상이기도 하다.
    log("[② 프로파일] 발급...");
    let profile = dev.download_profile(&team, &app_id).await?;
    log(&format!("[② 프로파일] {} bytes.", profile.encoded_profile.len()));

    // 6) 재서명 (①): 번들에 프로파일 박고, 프로파일의 Entitlements로 서명.
    log("[⑤ 서명] 번들 추출·번들ID 맞춤·프로파일·엔티틀먼트...");
    let app_dir = extract_ipa(ipa, &work.join("extracted"))?;
    // 앱의 CFBundleIdentifier를 발급받은 App ID와 맞춘다 — 프로파일의 application-identifier와 안 맞으면
    // 서명/설치/실행이 거부된다. 자기 갱신은 req.bundle_id가 이미 실행 중 번들ID라 사실상 no-op이지만,
    // 포장 과정에서 어긋나지 않게 확정한다.
    rewrite_bundle_identifier(&app_dir, &req.bundle_id)?;
    fs::write(
        app_dir.join("embedded.mobileprovision"),
        &profile.encoded_profile,
    )
    .context("embedded.mobileprovision 쓰기")?;
    let entitlements_xml = entitlements_xml_from_profile(&profile.encoded_profile)?;
    // 진단: 실제로 **서명에 박는** 엔티틀먼트(최소 4개여야 함). 위 프로파일 전체 키 로그와 함께 보면
    // 0xe8008016이 계속 날 때 "엔티틀먼트가 틀렸나"(이 XML로 판별) vs "엔티틀먼트는 맞는데 서명기
    // (apple-codesign)가 iOS 26이 안 받는 서명을 만드나"가 갈린다. 한 줄로 접어 찍는다.
    log(&format!(
        "[진단] 서명 엔티틀먼트({}자): {}",
        entitlements_xml.len(),
        entitlements_xml.replace('\n', " ").replace("  ", " ")
    ));
    // 진단: iOS 설치 직전 0xe8008016(invalid entitlements) 원인 좁히기. 서명·엔티틀먼트 내용은 정상
    // 확인됨(PC 재현: 메인 exe에 XML+DER 정확히 박힘) → 이제 프로파일이 **이 기기 UDID**와 **서명 인증서**를
    // 포함하는지가 관건. 무료 개발 프로파일은 ProvisionedDevices에 기기가, DeveloperCertificates에 서명
    // 인증서가 있어야 iOS가 설치를 받는다. 기기가 빠졌으면 dev_api에 addDevice(UDID 등록) 구현이 필요.
    if let Ok(pl) = plist_from_mobileprovision(&profile.encoded_profile) {
        let udids: Vec<String> = pl
            .get("ProvisionedDevices")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_string().map(str::to_string)).collect())
            .unwrap_or_default();
        let certs = pl
            .get("DeveloperCertificates")
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(0);
        if let Some(ent) = pl.get("Entitlements").and_then(Value::as_dictionary) {
            let gta = ent.get("get-task-allow").and_then(Value::as_boolean);
            let appid = ent
                .get("application-identifier")
                .and_then(Value::as_string)
                .unwrap_or("?");
            let team = ent
                .get("com.apple.developer.team-identifier")
                .and_then(Value::as_string)
                .unwrap_or("?");
            let kag: Vec<String> = ent
                .get("keychain-access-groups")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_string().map(str::to_string)).collect())
                .unwrap_or_default();
            log(&format!(
                "[진단] app-id={appid}, team={team}, get-task-allow={gta:?}, keychain={kag:?}"
            ));
            // 프로파일이 실은 어떤 키를 실었는지 전체 — 최소집합(위 4개)으로 줄이기 전에 무엇을
            // 버렸는지 확인용. 0xe8008016이 계속 나면 여기 여분 키(앱이 못 쓰는 항목)가 범인이었는지,
            // 아니면 서명기(apple-codesign) 쪽 문제인지 이 로그로 갈린다.
            let all_keys: Vec<&str> = ent.keys().map(String::as_str).collect();
            log(&format!(
                "[진단] 프로파일 엔티틀먼트 전체 키 {}개: {}",
                all_keys.len(),
                all_keys.join(", ")
            ));
        }
        log(&format!("[진단] 프로파일 등록기기 {}대: {}", udids.len(), udids.join(", ")));
        log(&format!("[진단] 프로파일 인증서 {certs}개"));
    }

    let signed = work.join("signed").join(app_dir.file_name().unwrap());
    fs::create_dir_all(signed.parent().unwrap())?;

    let mut settings = SigningSettings::default();
    settings.set_signing_key(&key, apple_cert);
    settings
        .set_entitlements_xml(SettingsScope::Main, &entitlements_xml)
        .map_err(|e| anyhow!("엔티틀먼트 설정: {e:?}"))?;
    // 이중 CodeDirectory(SHA-1 주 + SHA-256 대체) — zsign/SideStore가 iOS 26에 설치되는 마지막 조각.
    // apple-codesign은 modern 타깃 메인 exe에 **단일 SHA-256** CD만 낸다(폰 실측 확인). 그런데 폰
    // 진단으로 신원(app-id·서명자·기기 등록)과 엔티틀먼트(정확히 SideStore 최소 4키)가 모두 맞는데도
    // 설치가 0xe8008016 ApplicationVerificationFailed로 거부됐다 — 남은 차이는 서명 구조뿐이고, 그게
    // 이 이중 CD다. zsign 기본값(hashType 1=SHA-1 주, 2=SHA-256 대체)을 그대로 맞춘다: 주를 SHA-1로,
    // SHA-256을 extra(대체 CD)로. iOS 26은 강한 것(SHA-256)을 쓰되 둘 다 있는 hash agility를 요구한다.
    settings.set_digest_type(SettingsScope::Main, DigestType::Sha1);
    settings.add_extra_digest(SettingsScope::Main, DigestType::Sha256);
    UnifiedSigner::new(settings)
        .sign_path(&app_dir, &signed)
        .map_err(|e| anyhow!("[⑤ 서명] 실패: {e:?}"))?;
    log("[⑤ 서명] 완료.");

    // 7) 설치용 .ipa로 재포장(Payload/<app>).
    let out_ipa = work.join("Shard-signed.ipa");
    repackage_ipa(&signed, &out_ipa)?;
    log("[⑤ 서명] .ipa 재포장 완료.");
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
    let result = rt.block_on(async {
        // resign_app은 저장 세션을 재사용(resume_or_login)한다. 그 세션이 만료면 포털이 1100("session
        // has expired")으로 거부한다 — 그때 세션을 비우고 같은 실행에서 새 로그인으로 한 번 재시도한다
        // (verify 경로와 같은 "한 번 눌러 반드시 결과" 보장; 없으면 사용자가 두 번 눌러야 했다 — 폰 확인).
        let signed = match resign_app(&p.req, &p.ipa, || tfa(), p.state_dir.clone(), &p.work_dir, log).await {
            Ok(s) => s,
            Err(e) => {
                let es = format!("{e:#}").to_lowercase();
                if es.contains("1100") || es.contains("expired") {
                    log("저장 세션 만료(1100) — 세션을 비우고 새로 로그인해 재시도합니다.");
                    crate::auth::AppleSession::clear_session(&p.state_dir);
                    resign_app(&p.req, &p.ipa, || tfa(), p.state_dir.clone(), &p.work_dir, log).await?
                } else {
                    return Err(e);
                }
            }
        };
        log("서명 완료.");
        if let (Some(addr), Some(pairing_path)) = (p.device_addr, p.pairing_path.as_ref()) {
            log(&format!("[④ 설치] lockdownd 연결({addr})..."));
            let pairing =
                fs::read(pairing_path).map_err(|e| anyhow!("[④ 설치] 페어링 읽기 실패: {e}"))?;
            let provider = crate::install::tcp_provider(addr, &pairing, "shard-resign")
                .map_err(|e| anyhow!("[④ 설치] provider 실패: {e:#}"))?;
            log("[④ 설치] 앱 업그레이드 설치(installation_proxy)...");
            crate::install::install_or_upgrade_app(&provider, &signed)
                .await
                .map_err(|e| anyhow!("[④ 설치] 실패: {e:#}"))?;
            log("[④ 설치] 완료 — 앱이 교체됩니다. 앱을 다시 열어 주세요.");
        }
        Ok(signed)
    });
    // 실패 시 저장 세션이 만료됐을 수 있으니 지운다 — 다음 실행이 새로 로그인하게.
    if result.is_err() {
        crate::auth::AppleSession::clear_session(&p.state_dir);
    }
    result
}

/// ④+⑤ 자기 자신 갱신 — 실행 중인 앱 번들을 미서명 .ipa로 싸서 발급→재서명→(터널이 있으면)설치까지
/// 한 흐름으로. `app_bundle`은 실행 중 `.app` 경로(Bundle.main.bundlePath), `req.bundle_id`는 그 번들의
/// 현재 CFBundleIdentifier(같아야 in-place 업그레이드). 단계별 로그로 어디서 막히는지 드러낸다.
pub fn resign_selfupdate_blocking(
    req: ResignRequest,
    app_bundle: PathBuf,
    state_dir: PathBuf,
    work_dir: PathBuf,
    device_addr: Option<IpAddr>,
    pairing_path: Option<PathBuf>,
    tfa: &dyn Fn() -> String,
    log: &mut dyn FnMut(&str),
) -> Result<PathBuf> {
    // 실행 중 번들을 미서명 .ipa(Payload/<app>)로 포장한다. 앱은 자기 번들을 읽을 수 있다.
    log("자기 번들 포장 중(미서명 .ipa)...");
    fs::create_dir_all(&work_dir).context("작업 폴더 생성")?;
    let unsigned_ipa = work_dir.join("self-unsigned.ipa");
    let _ = fs::remove_file(&unsigned_ipa);
    repackage_ipa(&app_bundle, &unsigned_ipa)
        .map_err(|e| anyhow!("[포장] 자기 번들 .ipa 실패: {e:#}"))?;
    log(&format!(
        "포장 완료({} bytes). 발급→재서명→설치 진행...",
        fs::metadata(&unsigned_ipa).map(|m| m.len()).unwrap_or(0)
    ));

    let params = ResignAndInstall {
        req,
        ipa: unsigned_ipa,
        state_dir,
        work_dir,
        device_addr,
        pairing_path,
    };
    resign_and_install_blocking(params, tfa, log)
}

/// ④ 1단계 스모크 테스트(동기, iOS C ABI용) — 터널+페어링으로 lockdownd에 붙는지 확인.
pub fn probe_lockdownd_blocking(
    addr: IpAddr,
    pairing: Vec<u8>,
    log: &mut dyn FnMut(&str),
) -> Result<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow!("tokio 런타임: {e}"))?;
    rt.block_on(crate::install::probe_lockdownd(addr, &pairing, log))
}

/// ④ RSD 스모크(동기, iOS C ABI용) — rppairing 터널을 세우고 터널 안 RSD 서비스 목록을 반환.
/// jktcp 어댑터가 백그라운드 태스크를 띄우므로 **LocalSet 위에서** 돈다(ffi run_sync_local과 동일 —
/// current_thread block_on 단독이면 어댑터 태스크가 안 돌 수 있다). `pairing`은 RpPairingFile 바이트.
pub fn rsd_probe_blocking(
    addr: std::net::SocketAddr,
    pairing: Vec<u8>,
    log: &mut dyn FnMut(&str),
) -> Result<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow!("tokio 런타임: {e}"))?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, crate::rsd::rsd_probe(addr, pairing, log))
}

/// ④ RSD 설치(동기, iOS C ABI용) — rppairing 터널 위에서 서명된 .ipa를 업로드+설치. 같은 번들ID면
/// in-place 업그레이드(데이터 보존). `pairing`은 RpPairingFile 바이트, `ipa`는 ⑤가 만든 서명된 .ipa.
pub fn rsd_install_blocking(
    addr: std::net::SocketAddr,
    pairing: Vec<u8>,
    ipa: std::path::PathBuf,
    log: &mut dyn FnMut(&str),
) -> Result<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow!("tokio 런타임: {e}"))?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, crate::rsd::rsd_install(addr, pairing, &ipa, log))
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
        // 1) 저장된 세션을 먼저 시도(재로그인 회피 = 잠금 위험 감소). 복원 세션이 실패하면 — 공유
        //    anisette 정체성 변화로 GS 토큰 재요청이 거부(et 없음)되거나 세션이 만료된 것 — 지우고
        //    같은 실행에서 새 로그인으로 재시도한다. 사용자는 한 번만 누르고 결과는 반드시 나온다.
        if let Some(session) = AppleSession::resume(&state_dir, log).await {
            let dev = DeveloperApi::new(session);
            match run_verify_flow(&dev, &bundle_id, &app_name, &state_dir, log).await {
                Ok(s) => return Ok(s),
                Err(e) => {
                    log(&format!("복원 세션 실패({e:#}) — 새로 로그인해 재시도합니다."));
                    AppleSession::clear_session(&state_dir);
                }
            }
        }
        // 2) 새 로그인 후 같은 흐름.
        let session =
            AppleSession::login(email, password, || tfa(), state_dir.clone(), log).await?;
        let dev = DeveloperApi::new(session);
        run_verify_flow(&dev, &bundle_id, &app_name, &state_dir, log).await
    })
}

/// ②③ 발급 흐름(팀→인증서→App ID→프로파일). 세션이 복원이든 새 로그인이든 동일하게 돈다.
async fn run_verify_flow(
    dev: &DeveloperApi<AppleSession>,
    bundle_id: &str,
    app_name: &str,
    state_dir: &Path,
    log: &mut dyn FnMut(&str),
) -> Result<String> {
    log("팀 조회...");
    let team = dev
        .list_teams()
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("개발 팀 없음(무료 계정 확인)"))?;
    log(&format!("팀: {} ({})", team.name, team.team_id));

    // 서명 신원(키+인증서) — 저장된 게 유효하면 재사용(재발급/폐기 없음, 매일 갱신 대비).
    let _identity = ensure_signing_identity(dev, &team, app_name, state_dir, log).await?;

    // App ID 식별자는 애플 전역에서 고유해야 한다 — 다른 계정이 net.sw.shard를 선점하면 9401.
    // 팀 ID를 붙여 팀 고유로 만든다(AltStore/SideStore 방식; 팀 ID는 전역 고유).
    let effective_bundle_id = team_unique_bundle_id(bundle_id, &team.team_id);
    log(&format!("App ID 확보... ({effective_bundle_id})"));
    let app_id = ensure_app_id(dev, &team, &effective_bundle_id, app_name).await?;
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
    // 무료 계정은 개발 인증서 한도가 낮고(2~3개), 우리는 매번 새 키를 만들어 기존 인증서를 재사용
    // 못 한다(개인키가 없어). 그래서 기존 개발 인증서를 폐기하고 새로 발급한다 — 안 그러면 애플이
    // 7460("이미 인증서가 있음")으로 거부한다(폰/PC 로그로 확인).
    // ⚠️ 같은 Apple ID로 서명된 다른 앱이 영향받을 수 있다. 제품에선 키+인증서를 저장해 재사용해야
    //    churn이 없다(후속작업).
    for c in dev.list_certificates(team).await? {
        let _ = dev.revoke_certificate(team, &c.serial_number).await; // 실패해도 계속
    }

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

/// 서명 신원(키+인증서) 확보. **저장된 것이 애플에 아직 유효하면 그대로 재사용**(재발급·폐기 없음).
/// 매번 새 키로 재발급하면 무료 한도(7460)에 걸리고, 기존 dev 인증서를 폐기해 같은 Apple ID의 다른
/// 앱까지 영향받는다. 재사용하면 갱신이 **프로파일만 갈아끼우는 무중단(㉮)** 으로 가벼워진다 —
/// 매일 갱신도 부담 없다. 저장은 앱 컨테이너(state_dir) 파일; 제품은 Keychain 권장(후속).
async fn ensure_signing_identity(
    dev: &DeveloperApi<AppleSession>,
    team: &DeveloperTeam,
    app_name: &str,
    state_dir: &Path,
    log: &mut dyn FnMut(&str),
) -> Result<(InMemorySigningKeyPair, CapturedX509Certificate)> {
    let key_path = state_dir.join("signkey.p8");
    let cert_path = state_dir.join("signcert.der");

    // 1) 저장된 키+인증서가 애플 목록에 아직 있으면(=유효) 재사용. DER 바이트로 대조 — 직렬번호
    //    문자열 포맷 차이에 안 흔들린다.
    if let (Ok(key_der), Ok(cert_der)) = (fs::read(&key_path), fs::read(&cert_path)) {
        if let (Ok(key), Ok(cert)) = (
            InMemorySigningKeyPair::from_pkcs8_der(&key_der),
            CapturedX509Certificate::from_der(cert_der.clone()),
        ) {
            if let Ok(list) = dev.list_certificates(team).await {
                if list.iter().any(|c| c.cert_content == cert_der) {
                    log("기존 인증서 재사용(재발급 없음).");
                    return Ok((key, cert));
                }
            }
        }
    }

    // 2) 없거나 만료/폐기됨 → 새 키로 발급하고 저장(다음엔 재사용).
    log("인증서 확보(CSR 제출)...");
    let key = generate_rsa_signing_key(app_name)?;
    let cert = ensure_certificate(dev, team, app_name, &key).await?;
    let _ = fs::write(&key_path, &*key.to_pkcs8_one_asymmetric_key_der());
    let _ = fs::write(&cert_path, cert.constructed_data());
    log("인증서 OK(저장됨).");
    Ok((key, cert))
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

/// App ID 식별자를 팀 고유로 만든다: `base.teamId`. App ID는 애플 전역에서 유일해야 해서
/// 여러 계정이 같은 base(net.sw.shard)를 못 쓴다(9401) — 팀 ID(전역 유일)를 붙여 회피한다.
fn team_unique_bundle_id(base: &str, team_id: &str) -> String {
    format!("{base}.{team_id}")
}

/// 앱 번들 Info.plist의 CFBundleIdentifier를 바꾼다(팀 고유 App ID와 일치시키려고).
/// Info.plist는 대개 바이너리 plist라, plist 크레이트로 읽어 값만 고쳐 다시 바이너리로 쓴다.
/// 안 맞추면 프로파일의 application-identifier(teamId.식별자)와 어긋나 서명/설치/실행이 거부된다.
fn rewrite_bundle_identifier(app_dir: &Path, bundle_id: &str) -> Result<()> {
    let plist_path = app_dir.join("Info.plist");
    let mut dict = Value::from_file(&plist_path)
        .map_err(|e| anyhow!("Info.plist 읽기: {e:?}"))?
        .into_dictionary()
        .ok_or_else(|| anyhow!("Info.plist가 딕셔너리가 아님"))?;
    dict.insert("CFBundleIdentifier".into(), bundle_id.into());
    let mut buf = Vec::new();
    plist::to_writer_binary(&mut buf, &Value::Dictionary(dict))
        .map_err(|e| anyhow!("Info.plist 직렬화: {e:?}"))?;
    fs::write(&plist_path, buf).context("Info.plist 쓰기")?;
    Ok(())
}

/// mobileprovision(CMS로 감싼 plist)에서 Entitlements를 뽑아 XML로. (Dadoum sign.d의 profilePlist["Entitlements"].)
fn entitlements_xml_from_profile(profile: &[u8]) -> Result<String> {
    let plist = plist_from_mobileprovision(profile)?;
    let ent = plist
        .get("Entitlements")
        .and_then(Value::as_dictionary)
        .ok_or_else(|| anyhow!("프로파일에 Entitlements 없음"))?;
    // 프로파일의 엔티틀먼트를 **통째로 복사하지 않고 최소 집합(4개)만** 골라 서명한다.
    //
    // AltStore/SideStore가 iOS 17~26에 실제로 설치되는 핵심이 이것이다: 재서명 엔티틀먼트를 딱 이
    // 네 개로 줄인다. 무료 개발 프로파일(iOS Team Provisioning Profile)은 이 넷 말고도 앱이 실제로
    // 쓰지 않는 항목(팀 기본 data-protection 등)이나 값이 어긋나는 항목을 얹어 오는데, 그걸 서명에
    // 그대로 박으면 installd가 0xe8008016(invalid entitlements)로 **설치를 거부**한다(SideStore #782:
    // 항목 과다/불일치가 원인 — 폰 확인: keychain 와일드카드를 구체값으로 고쳐도 전체 복사가 남아
    // 여전히 0xe8008016이었다). Shard는 확장(.appex)도 앱그룹도 없어(번들 확인: 프레임워크 2개뿐)
    // 아래 넷이면 충분하고, 이게 SideStore가 실제로 넣는 그 집합이다. 넷을 넘기지 않는 게 규칙.
    let mut min = Dictionary::new();
    // application-identifier: 프로파일 값 그대로(구체 TEAMID.번들ID).
    if let Some(v) = ent.get("application-identifier").cloned() {
        min.insert("application-identifier".into(), v);
    }
    // 팀 식별자.
    if let Some(v) = ent.get("com.apple.developer.team-identifier").cloned() {
        min.insert("com.apple.developer.team-identifier".into(), v);
    }
    // 개발 서명이라 반드시 true라야 설치·실행된다.
    min.insert(
        "get-task-allow".into(),
        Value::Boolean(ent.get("get-task-allow").and_then(Value::as_boolean).unwrap_or(true)),
    );
    // keychain-access-groups: 와일드카드(`TEAMID.*`)가 아니라 **구체 그룹 하나**(=application-identifier).
    // 와일드카드를 서명에 박으면 그것만으로도 0xe8008016이 난다(Xcode·Sideloadly도 여기서 구체값으로 편다).
    if let Some(appid) = ent.get("application-identifier").and_then(Value::as_string) {
        min.insert(
            "keychain-access-groups".into(),
            Value::Array(vec![Value::String(appid.to_string())]),
        );
    }

    let mut xml = Vec::new();
    plist::to_writer_xml(&mut xml, &Value::Dictionary(min))?;
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

    // 서명 엔티틀먼트는 프로파일 전체가 아니라 **최소 4개**여야 iOS installd가 설치를 받는다
    // (0xe8008016). 프로파일이 얹어 오는 여분 키(앱이 못 쓰는 항목)를 그대로 박으면 거부되고,
    // keychain 와일드카드도 구체값으로 펴야 한다. 이 둘을 이 테스트가 지킨다 — 되돌리면 설치가 깨진다.
    #[test]
    fn signed_entitlements_are_the_minimal_four_with_a_concrete_keychain_group() {
        // 프로파일은 CMS로 감싸여 있으나 우리는 안의 <?xml…</plist> 구간만 훑으므로 plist 그대로 넘긴다.
        let profile = br#"<?xml version="1.0"?>
<plist version="1.0"><dict>
  <key>Entitlements</key><dict>
    <key>application-identifier</key><string>DM94SF72RB.net.sw.shard.DM94SF72RB</string>
    <key>com.apple.developer.team-identifier</key><string>DM94SF72RB</string>
    <key>get-task-allow</key><true/>
    <key>keychain-access-groups</key><array><string>DM94SF72RB.*</string></array>
    <key>com.apple.developer.default-data-protection</key><string>NSFileProtectionComplete</string>
  </dict>
</dict></plist>"#;

        let xml = entitlements_xml_from_profile(profile).unwrap();

        // 남아야 할 넷.
        assert!(xml.contains("application-identifier"), "{xml}");
        assert!(xml.contains("DM94SF72RB.net.sw.shard.DM94SF72RB"), "{xml}");
        assert!(xml.contains("com.apple.developer.team-identifier"), "{xml}");
        assert!(xml.contains("get-task-allow"), "{xml}");
        assert!(xml.contains("keychain-access-groups"), "{xml}");
        // keychain은 구체값 하나, 와일드카드는 사라져야 한다.
        assert!(!xml.contains("DM94SF72RB.*"), "keychain 와일드카드가 남았다: {xml}");
        // 프로파일이 얹어 온 여분 키는 버려야 한다.
        assert!(
            !xml.contains("default-data-protection"),
            "여분 엔티틀먼트가 서명에 남았다(0xe8008016 원인): {xml}"
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
