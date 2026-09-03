//! ④ 설치 (iOS 17+) — RemoteXPC/RSD 경로. 설계·근거는 docs/RSD-설치.md.
//!
//! **왜 이 파일이 install.rs(classic)를 대체하나**: iOS 17+는 classic lockdown(포트 62078 평문)을
//! 네트워크로 폐기하고 RemoteXPC/RSD 터널로 대체했다. minimuxer/classic idevice는 lockdown 첫
//! 요청(QueryType)에서 기기가 연결을 리셋해(폰 확인) 설치가 불가능했다. iOS 17.4~26 온디바이스
//! 설치(StikDebug 방식)는 RSD로만 된다.
//!
//! **두 아키텍처**(어느 쪽인지는 `rsd_probe`가 판별):
//! - **A**: 루프백 VPN(StosVPN/LocalDevVPN)이 10.7.0.1:49152에 RSD를 노출하고 서브넷 전체를
//!   라우팅하면 — `RsdProvider`가 그냥 `IpAddr`이고 서비스마다 `TcpStream::connect`. 터널을 우리가
//!   세울 필요 없고 앱에 페어링 파일도 불필요(VPN이 상류에서 소비). 서비스에
//!   `…installation_proxy.shim.remote`가 보이면 A.
//! - **B**: raw remoted RSD만 노출되면(서비스에 `…tunnelservice`만) — RpPairingFile+jktcp
//!   userspace 어댑터로 우리가 터널을 세워야 한다(features tunnel_tcp_stack/remote_pairing 추가).
//!
//! ⚠️ 검증 0: 실기기+루프백 VPN이 있어야만 동작 확인. 헤드리스는 컴파일까지.

use std::net::SocketAddr;

use anyhow::{anyhow, Result};
use idevice::rsd::RsdHandshake;

/// ④ 1단계 스모크 — RSD에 붙어 서비스 목록을 읽고 **Architecture A/B를 판별**한다.
/// A/B는 설치 구현이 완전히 달라지므로, 코드를 쓰기 전에 폰에서 이걸로 확정한다("측정 먼저").
pub async fn rsd_probe(addr: SocketAddr, log: &mut dyn FnMut(&str)) -> Result<String> {
    // ① RSD 포트로 TCP(터널 너머). 라우트가 없으면 OS가 오래 매달리므로 10s로 자른다.
    log(&format!("① RSD 연결... ({addr})"));
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .map_err(|_| {
        anyhow!("[① 연결] 시간초과 10s — {addr}에 못 닿음. 루프백 VPN(StosVPN)이 켜져 이 포트를 라우팅하는지 확인.")
    })?
    .map_err(|e| anyhow!("[① 연결] 실패(터널/포트): {e}"))?;

    // ② RSD 핸드셰이크(RemoteXPC over HTTP/2). classic lockdown QueryType이 아니라 여기서부터가
    //    iOS 17+ 경로 — 이게 되면 iOS 26에서 막히던 벽을 넘은 것.
    log("② RSD 핸드셰이크(RemoteXPC)...");
    let hs = RsdHandshake::new(stream)
        .await
        .map_err(|e| anyhow!("[② RSD] 핸드셰이크 실패(포트가 RSD가 아니거나 터널 밖): {e:?}"))?;

    let mut names: Vec<String> = hs.services.keys().cloned().collect();
    names.sort();
    log(&format!("RSD OK — 서비스 {}개:", names.len()));
    for n in &names {
        log(&format!("  · {n}"));
    }

    // A/B 판별: 설치 서비스가 이미 보이면 터널 안(A). tunnelservice만 보이면 우리가 터널을 세워야(B).
    let has_install = names.iter().any(|n| n.contains("installation_proxy"));
    let has_afc = names.iter().any(|n| n.contains("com.apple.afc"));
    let only_tunnel = !has_install
        && names
            .iter()
            .any(|n| n.contains("tunnelservice") || n.contains("coredevice.untrusted"));

    if has_install && has_afc {
        Ok(format!(
            "✅ Architecture A — 터널 안(installation_proxy+afc 노출). 서비스 {}개. 바로 설치 가능.",
            names.len()
        ))
    } else if has_install {
        Ok(format!(
            "Architecture A(installation_proxy는 있는데 afc 미검출). 서비스 {}개. 목록 확인.",
            names.len()
        ))
    } else if only_tunnel {
        Ok(format!(
            "Architecture B — raw remoted(tunnelservice만). 우리가 터널을 세워야 함(RpPairing+jktcp). 서비스 {}개.",
            names.len()
        ))
    } else {
        Ok(format!(
            "RSD는 통과했으나 설치/터널 서비스 미검출 — 서비스 {}개, 위 목록으로 판단 필요.",
            names.len()
        ))
    }
}
