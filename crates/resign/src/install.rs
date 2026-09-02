//! ④ 설치/갱신 — `idevice`(jkcoxson)로 폰 lockdownd에 붙어 프로파일(㉮)·앱(㉯)을 넣는다.
//!
//! 두 경로(설계: docs/재서명-엔진.md):
//! - **㉮ `install_profile`** = `misagent`로 프로비저닝 프로파일만 갱신 → **재설치 없이** 유효기간
//!   연장(무중단). 7일 주기 일상 경로.
//! - **㉯ `install_or_upgrade_app`** = `installation_proxy`로 앱 설치/업그레이드. 인증서 만료·새빌드·
//!   첫설치용. 같은 번들ID면 업그레이드라 앱 데이터 보존.
//!
//! 연결은 **LocalDevVPN 터널**이 노출하는 엔드포인트에 `TcpProvider`(+페어링 파일)로 붙는다.
//! 실제 주소·페어링 파일은 iOS 연결 시점에 터널/기기에서 온다.
//!
//! ⚠️ **검증 0**: 이 모듈은 실제 기기 연결(lockdownd 핸드셰이크)이 있어야만 동작을 확인할 수 있다.
//! 헤드리스로는 컴파일까지만. idevice의 공개 API를 그대로 호출하는 얇은 층이라, 폰에서
//! build-test-fix로 다듬는다.

use std::net::IpAddr;
use std::path::Path;

use anyhow::{anyhow, Result};
use idevice::{
    pairing_file::PairingFile,
    provider::{IdeviceProvider, TcpProvider},
    services::{lockdown::LockdownClient, misagent::MisagentClient},
    utils::installation::install_package,
    IdeviceService,
};

/// LocalDevVPN 터널 엔드포인트로 향하는 provider. `pairing`은 기기 페어링 파일(plist) 바이트.
pub fn tcp_provider(addr: IpAddr, pairing: &[u8], label: &str) -> Result<TcpProvider> {
    let pairing_file =
        PairingFile::from_bytes(pairing).map_err(|e| anyhow!("페어링 파일 파싱: {e:?}"))?;
    Ok(TcpProvider {
        addr,
        scope_id: None,
        pairing_file,
        label: label.to_string(),
    })
}

/// ㉮ 프로파일만 갱신(무중단, 재설치 없음). `profile`은 mobileprovision 바이트(②의 download_profile 결과).
pub async fn install_profile(provider: &dyn IdeviceProvider, profile: Vec<u8>) -> Result<()> {
    let mut mis = MisagentClient::connect(provider)
        .await
        .map_err(|e| anyhow!("misagent 연결: {e:?}"))?;
    mis.install(profile)
        .await
        .map_err(|e| anyhow!("misagent 프로파일 설치: {e:?}"))
}

/// ㉯ 앱 설치/업그레이드(재설치). `ipa`는 서명된 .ipa 경로(⑤의 resign_app 결과).
pub async fn install_or_upgrade_app(provider: &dyn IdeviceProvider, ipa: &Path) -> Result<()> {
    install_package(provider, ipa, None)
        .await
        .map_err(|e| anyhow!("앱 설치: {e:?}"))
}

/// ④ 1단계 스모크 테스트 — 터널(StosVPN/LocalDevVPN)+페어링으로 폰 lockdownd에 실제로 붙는지 확인한다.
/// 설치(㉯)·프로파일(㉮)의 전제인 **전송 계층**을 먼저 증명하는 make-or-break: 여기가 되면 나머지는
/// 그 위에 얹힌다. 안 되면 그대로의 에러(연결/페어링/주소 중 무엇인지)로 다음 수를 정한다.
///   connect → GetValue(ProductVersion)  : 연결이 되는지(세션 전에도 읽힘)
///   start_session(pairing)              : 페어링이 유효한지(SSL 핸드셰이크)
///   DeviceName·UDID                     : 부가 확인
pub async fn probe_lockdownd(
    addr: IpAddr,
    pairing: &[u8],
    log: &mut dyn FnMut(&str),
) -> Result<String> {
    let provider = tcp_provider(addr, pairing, "shard-probe")?;
    log(&format!("lockdownd 연결 중... ({addr}:{})", LockdownClient::LOCKDOWND_PORT));
    let mut lockdown = LockdownClient::connect(&provider)
        .await
        .map_err(|e| anyhow!("lockdownd 연결 실패(터널/주소 확인): {e:?}"))?;

    log("연결됨. ProductVersion 질의...");
    let version = lockdown
        .get_value(Some("ProductVersion"), None)
        .await
        .map_err(|e| anyhow!("ProductVersion 질의 실패: {e:?}"))?;
    let version = version.as_string().unwrap_or("?").to_string();

    log(&format!("iOS {version}. 세션 시작(페어링 검증)..."));
    let pairing_file =
        PairingFile::from_bytes(pairing).map_err(|e| anyhow!("페어링 파싱: {e:?}"))?;
    lockdown
        .start_session(&pairing_file)
        .await
        .map_err(|e| anyhow!("세션 시작 실패(페어링 무효/만료 가능): {e:?}"))?;
    log("세션 OK — 페어링 유효, lockdownd 통과.");

    let name = lockdown
        .get_value(Some("DeviceName"), None)
        .await
        .ok()
        .and_then(|v| v.as_string().map(str::to_string))
        .unwrap_or_default();
    let udid = lockdown
        .get_value(Some("UniqueDeviceID"), None)
        .await
        .ok()
        .and_then(|v| v.as_string().map(str::to_string))
        .unwrap_or_default();
    Ok(format!("lockdownd 통과 — iOS {version}, 기기 '{name}', UDID {udid}"))
}
