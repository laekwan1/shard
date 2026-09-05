//! ④ 설치 (iOS 17+) — RemotePairing → TLS-PSK 터널 → 터널 안 RSD → installation_proxy.
//! 설계·근거는 docs/RSD-설치.md.
//!
//! **왜 rppairing인가**(폰에서 측정으로 확정): classic lockdown(62078)은 iOS 26에서 QueryType에서
//! 죽고, 루프백 VPN(LocalDevVPN)의 10.7.0.1:49152는 **RemotePairing(JSON) 엔드포인트**다 — RSD/
//! RemoteXPC가 아니다. 그래서 bare `RsdHandshake::new`(HTTP/2)를 보내면 기기가 리셋한다(errno 54,
//! 폰 확인). StikJIT과 동일하게 `tunnel_create_rppairing` 경로를 쓴다: 직접 TCP → RPPairing pair-verify
//! → TLS-PSK 터널 → jktcp userspace 어댑터(실제 TUN 불필요) → 터널 **안쪽** RSD → 설치.
//!
//! **페어링**: 이 경로는 **RpPairingFile(Ed25519)** 이 필요하다. classic `.mobiledevicepairing`
//! (RSA 인증서+EscrowBag)과는 키 재질 자체가 달라 변환이 없다 — `RpPairingFile::from_bytes`가 classic을
//! 거부한다. 사용자는 idevice_pair로 'Remote pairing' 파일을 1회 발급해 가져온다(그 뒤 재사용).
//!
//! ⚠️ 검증 0: 실기기+LocalDevVPN+RP페어링이 있어야만 동작 확인. 헤드리스는 컴파일까지.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use anyhow::{anyhow, Result};
use idevice::remote_pairing::{
    connect_tls_psk_tunnel_native, RemotePairingClient, RpPairingFile, RpPairingSocket,
};
use idevice::rsd::RsdHandshake;
use idevice::tcp::adapter::Adapter;
use idevice::tcp::handle::AdapterHandle;
use idevice::utils::installation::install_package_rsd;

// 우리 호스트가 기기에 제시하는 이름. pair-verify는 파일 안 Ed25519 신원으로 하므로 이 값은 크게
// 중요치 않다(pair-setup 때만 identifier 계산에 쓰임). 안정적인 고정값을 둔다.
const HOST: &str = "Shard";

/// RpPairingFile을 바이트에서 로드. classic `.mobiledevicepairing`이면 여기서 실패한다(포맷이 다름) —
/// 그게 곧 "RP 페어링을 마련하라"는 신호다.
fn load_rp_pairing(pairing: &[u8]) -> Result<RpPairingFile> {
    RpPairingFile::from_bytes(pairing).map_err(|e| {
        anyhow!(
            "RP 페어링 파일이 아닙니다({e:?}) — iOS 17+ 설치는 RP 포맷(Ed25519) 페어링이 필요합니다. \
             idevice_pair로 'Remote pairing' 파일을 발급해 가져오세요(classic .mobiledevicepairing은 안 됨)."
        )
    })
}

/// `tunnel_create_rppairing` 재현: 직접 TCP → RPPairing(pair-verify) → TLS-PSK 터널 → jktcp userspace
/// 어댑터 → 터널 안 RSD 핸드셰이크. **어댑터는 백그라운드 태스크를 띄우므로 반환 후에도 살려둬야
/// 한다**(드롭하면 스트림이 BrokenPipe로 죽는다) — 호출부가 설치 끝까지 소유한다.
async fn rppairing_tunnel(
    addr: SocketAddr,
    rpf: &mut RpPairingFile,
    log: &mut dyn FnMut(&str),
) -> Result<(AdapterHandle, RsdHandshake)> {
    // ① 직접 TCP. 라우트가 없으면 오래 매달리므로 10s로 자른다.
    log(&format!("① RemotePairing 연결... ({addr})"));
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .map_err(|_| anyhow!("[① 연결] 시간초과 10s — {addr}에 못 닿음(LocalDevVPN 켜짐·연결 확인)"))?
    .map_err(|e| anyhow!("[① 연결] 실패: {e}"))?;

    // ② RPPairing pair-verify(Ed25519). 파일이 무효/만료거나 터널이 이미 점유 중이면 리셋된다.
    let conn = RpPairingSocket::new(stream);
    let mut rpc = RemotePairingClient::new(conn, HOST);
    log("② pair-verify (RPPairing)...");
    rpc.connect(rpf, || async { "000000".to_string() })
        .await
        .map_err(|e| {
            anyhow!("[② 페어링] pair-verify 실패(RP 페어링이 이 기기 것/유효한지, 또는 터널 점유 중인지 확인): {e:?}")
        })?;

    // ③ TLS-PSK 터널: 페어링으로 유도한 키로 암호화된 터널을 연다.
    log("③ TLS-PSK 터널 수립...");
    let tunnel_port = rpc
        .create_tcp_listener()
        .await
        .map_err(|e| anyhow!("[③ 터널] listener 생성 실패: {e:?}"))?;
    let mut tunnel_addr = addr;
    tunnel_addr.set_port(tunnel_port);
    let tstream = tokio::net::TcpStream::connect(tunnel_addr)
        .await
        .map_err(|e| anyhow!("[③ 터널] 연결 실패: {e}"))?;
    let tunnel = connect_tls_psk_tunnel_native(tstream, rpc.encryption_key())
        .await
        .map_err(|e| anyhow!("[③ 터널] TLS-PSK 핸드셰이크 실패: {e:?}"))?;

    // ④ jktcp userspace 어댑터를 터널 위에 얹고, 터널 안 RSD 포트에 붙어 핸드셰이크.
    let client_ip: IpAddr = tunnel
        .info
        .client_address
        .parse()
        .map_err(|e| anyhow!("client ip 파싱: {e}"))?;
    let server_ip: IpAddr = tunnel
        .info
        .server_address
        .parse()
        .map_err(|e| anyhow!("server ip 파싱: {e}"))?;
    let mtu = tunnel.info.mtu as usize;
    let rsd_port = tunnel.info.server_rsd_port;
    let raw = tunnel.into_inner();
    let mut adapter = Adapter::new(Box::new(raw), client_ip, server_ip);
    adapter.set_mss(mtu.saturating_sub(60)); // IPv6+TCP 헤더분 — 안 빼면 큰 전송이 깨진다(finish_tunnel과 동일)
    let mut adapter = adapter.to_async_handle();
    log("④ 터널 안 RSD 핸드셰이크...");
    let rsd_stream = adapter
        .connect(rsd_port)
        .await
        .map_err(|e| anyhow!("[④ RSD] 어댑터 connect 실패: {e:?}"))?;
    let handshake = RsdHandshake::new(rsd_stream)
        .await
        .map_err(|e| anyhow!("[④ RSD] 핸드셰이크 실패: {e:?}"))?;

    Ok((adapter, handshake))
}

/// ④ 스모크 — rppairing 터널을 세우고 **터널 안** RSD 서비스 목록을 반환(설치 서비스가 보이는지 확인).
/// 여기까지 되면 iOS 26 온디바이스 설치의 전송 계층이 전부 증명된 것.
pub async fn rsd_probe(addr: SocketAddr, pairing: Vec<u8>, log: &mut dyn FnMut(&str)) -> Result<String> {
    let mut rpf = load_rp_pairing(&pairing)?;
    let (_adapter, hs) = rppairing_tunnel(addr, &mut rpf, log).await?;
    let mut names: Vec<String> = hs.services.keys().cloned().collect();
    names.sort();
    log(&format!("RSD OK — 터널 안 서비스 {}개:", names.len()));
    for n in &names {
        log(&format!("  · {n}"));
    }
    let has_install = names.iter().any(|n| n.contains("installation_proxy"));
    let has_afc = names.iter().any(|n| n.contains("com.apple.afc"));
    // _adapter는 여기서 드롭되며 터널이 닫힌다 — 프로브는 목록만 보면 되므로 괜찮다.
    if has_install && has_afc {
        Ok(format!(
            "✅ 터널·설치 준비됨 — installation_proxy+afc 확인(서비스 {}개). '지금 갱신'으로 설치 가능.",
            names.len()
        ))
    } else if has_install {
        Ok(format!(
            "터널 OK, installation_proxy는 있는데 afc 미검출(서비스 {}개). 위 목록 확인.",
            names.len()
        ))
    } else {
        Ok(format!(
            "터널은 섰으나 설치 서비스 미검출(서비스 {}개) — 위 목록으로 판단 필요.",
            names.len()
        ))
    }
}

/// ④+ 설치 — rppairing 터널 위에서 서명된 .ipa를 업로드(AFC)+설치(installation_proxy). 같은 번들ID면
/// in-place 업그레이드라 앱 데이터 보존. `ipa`는 ⑤ 재서명이 만든 서명된 .ipa 경로.
pub async fn rsd_install(
    addr: SocketAddr,
    pairing: Vec<u8>,
    ipa: &Path,
    log: &mut dyn FnMut(&str),
) -> Result<String> {
    let mut rpf = load_rp_pairing(&pairing)?;
    // 어댑터를 설치 끝까지 소유(드롭 금지 — 스트림이 죽는다).
    let (mut adapter, mut hs) = rppairing_tunnel(addr, &mut rpf, log).await?;
    // 진단(0xe8008016): 이 폰의 UDID를 찍어 위 "프로파일 등록기기" 목록과 대조하려는 것. 서명·엔티틀먼트
    // 는 완벽(PC 재현 확인)한데 설치가 거부되니, 남은 원인은 ⓐ 이 폰이 프로파일에 없음(→ addDevice
    // 구현이면 해결) ⓑ apple-codesign 서명이 iOS 26 비호환(→ zsign 교체)뿐이다. UDID가 목록에 있으면 ⓑ,
    // 없으면 ⓐ. 여기서 한 줄로 판별된다.
    match hs.properties.get("UniqueDeviceID").and_then(|v| v.as_string()) {
        Some(u) => log(&format!("[진단] 이 폰 UDID: {u} — 위 프로파일 등록기기 목록에 있는지 확인")),
        None => {
            let keys: Vec<&str> = hs.properties.keys().map(String::as_str).collect();
            log(&format!("[진단] UDID 프로퍼티 못 찾음. 가용 키: {}", keys.join(", ")));
        }
    }
    log("⑤ AFC 업로드 + installation_proxy 설치...");
    // options=None이면 helper가 .ipa에서 CFBundleIdentifier를 읽어 PublicStaging 업로드 후 설치한다.
    install_package_rsd(&mut adapter, &mut hs, ipa, None)
        .await
        .map_err(|e| anyhow!("[⑤ 설치] 실패: {e:?}"))?;
    Ok("설치 완료 🎉 — 앱이 교체됩니다. 다시 여세요.".to_string())
}
