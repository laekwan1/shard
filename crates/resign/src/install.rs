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
    services::{
        heartbeat::HeartbeatClient, installation_proxy::InstallationProxyClient,
        lockdown::LockdownClient, misagent::MisagentClient,
    },
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
    let pairing_file =
        PairingFile::from_bytes(pairing).map_err(|e| anyhow!("페어링 파싱: {e:?}"))?;

    // ① TCP 연결(터널 너머 lockdownd 포트). 라우트가 없으면 OS가 ~75s 매달리므로 10s로 자른다.
    log(&format!("① 연결... ({addr}:{})", LockdownClient::LOCKDOWND_PORT));
    let mut lockdown = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        LockdownClient::connect(&provider),
    )
    .await
    .map_err(|_| {
        anyhow!("[① 연결] 시간초과 10s — 앱이 {addr}에 못 닿음. LocalDevVPN이 켜져 연결됐는지, 그리고 기기 IP가 정말 {addr}인지 확인(LocalDevVPN 설정의 device IP가 다를 수 있음).")
    })?
    .map_err(|e| anyhow!("[① 연결] 실패(터널/주소): {e:?}"))?;

    // ② 세션(SSL, 페어링) — 여기서 early eof면 raw lockdown이 안 통하는 것(iOS 26 usbmux 계층 필요 신호).
    log("② 세션 시작(SSL, 페어링)...");
    lockdown
        .start_session(&pairing_file)
        .await
        .map_err(|e| anyhow!("[② 세션] 실패(early eof=usbmux 계층 필요 / 페어링 무효): {e:?}"))?;
    let version = lockdown
        .get_value(Some("ProductVersion"), None)
        .await
        .ok()
        .and_then(|v| v.as_string().map(str::to_string))
        .unwrap_or_default();
    log(&format!("세션 OK — iOS {version}."));

    // ③ 하트비트 — iOS는 하트비트 클라이언트가 없으면 서비스 연결을 닫는다(설치 중 연결 유지에 필수).
    log("③ 하트비트 시작(연결 유지)...");
    let mut hb = HeartbeatClient::connect(&provider)
        .await
        .map_err(|e| anyhow!("[③ 하트비트] 연결 실패: {e:?}"))?;
    let interval = hb
        .get_marco(15)
        .await
        .map_err(|e| anyhow!("[③ 하트비트] marco 실패: {e:?}"))?;
    hb.send_polo().await.ok();
    log(&format!("하트비트 OK(interval {interval})."));

    // ④ installation_proxy — 설치 서비스가 실제 응답하는지(㉯ 설치의 전제).
    log("④ installation_proxy 앱 목록...");
    let mut inst = InstallationProxyClient::connect(&provider)
        .await
        .map_err(|e| anyhow!("[④ instproxy] 연결 실패: {e:?}"))?;
    let apps = inst
        .get_apps(None, None)
        .await
        .map_err(|e| anyhow!("[④ instproxy] 목록 실패: {e:?}"))?;
    Ok(format!(
        "설치 서비스 통과 — iOS {version}, 설치앱 {}개(전송 계층 OK)",
        apps.len()
    ))
}
