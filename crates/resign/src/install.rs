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
    services::misagent::MisagentClient,
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
