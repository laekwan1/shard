# iOS 26 온디바이스 설치 — RSD/RemoteXPC 터널 설계

착수 전 검토용 설계·위험 문서(사용자가 "설계·리스크부터" 선택, 2026-09-03).
이 문서가 서면 [재서명-엔진.md](재서명-엔진.md)의 ④ 설치 부분을 대체한다.

## 왜 이 문서 (측정으로 확정한 배경)

minimuxer(SideStore의 classic usbmux+lockdown muxer)로 iOS 26 온디바이스 설치를
시도했고, **폰에서 측정으로 막다른 길을 확정**했다:

1. 터널·muxer·기기인식(UDID `00008140-…`)까지 **정상**. `test_device_connection()`
   (10.7.0.1:62078 직접 TCP)도 성공. 딱 하나 — **하트비트 lockdown이 `UnknownError` 반복**.
2. idevice C-레벨 stderr를 `freopen`으로 붙잡아 진짜 원인 확인:
   ```
   lockdown.c:648 lockdownd_client_new(): QueryType failed in the lockdownd client.
   idevice.c:769 ... socket_receive_timeout returned -54 (Connection reset by peer)
   ```
   즉 **lockdown 첫 평문 요청(QueryType)에서 기기가 연결을 리셋** — SSL·페어링에 닿기도 전.
   **→ 페어링 재발급으로는 안 고쳐진다**(거기까지 가지도 못함).
3. 원인: **iOS 17+가 classic lockdown(포트 62078 평문)을 네트워크로는 폐기**하고
   **RemoteXPC/RSD 터널**로 대체했다. minimuxer는 결국 classic lockdown을 TCP로 보내므로
   (로컬 usbmux 셔틀을 거쳐도 최종 목적지는 10.7.0.1:62078 classic) **iOS 26에서 불가**.
   이건 raw idevice가 "SSL 전 early eof"로 막히던 그 벽과 **동일**하다.

근거: pymobiledevice3 "Remote Access and Tunneling (iOS 17+)", SideStore #722/#951
(같은 `heartbeat UnknownError`), 그리고 우리 폰 stderr 로그.

## 진짜 경로 (검증된 레퍼런스: StikDebug/StikJIT)

**StikDebug**가 iOS 17.4~26에서 **온디바이스로**(무료 인증서 + 별도 루프백 VPN + 페어링
파일, 자체 Network Extension 없음) RSD 터널을 세워 JIT를 한다. 같은 터널로
`installation_proxy`를 태우면 설치가 된다. 핵심 building block이 전부 **jkcoxson `idevice`
FFI(`libidevice_ffi.a`)** 에 C ABI로 이미 있다:

| FFI | 역할 |
|---|---|
| `tunnel_create_remotexpc(addr, pairing)` | RSD 주소로 **RemoteXPC 터널** 수립 — **classic lockdown 불필요**(iOS 26 통과). 무선 대안 `tunnel_create_rppairing`도 있음. USB용 `tunnel_create_usb`는 우리 대상 아님 |
| `AdapterHandle` + `adapter_connect(port)` / `adapter_send` / `adapter_recv` | **userspace TCP 스택**. 실제 TUN/utun을 안 만들어 **NetworkExtension 엔타이틀먼트 불필요**(무료 인증서 OK) |
| `idevice_rsd_checkin`, `rsd_get_services`(`CRsdService.uses_remote_xpc`) | RSD 서비스 탐색(포트 매핑) |
| `afc_client_connect_rsd(adapter)` | 서명 .ipa를 기기 스테이징에 업로드 |
| `installation_proxy_connect_rsd(adapter)` + `installation_proxy_install[_with_callback]` | 설치(진행 콜백). in-place면 업그레이드라 데이터 보존 |

### 주소·포트 (StikJIT 기본값 — R1 해소)
- 기기 주소 **10.7.0.1** (루프백 VPN: LocalDevVPN/StosVPN이 라우팅).
- RSD 포트 **49152** (RemoteXPC/RSD 리스너). minimuxer가 쓰던 62078(classic)이 **아니다**.
- StikJIT은 먼저 10.7.0.1:49152 도달성(NWConnection)만 확인한 뒤 터널을 세운다(`EndpointProbe`).

## 흐름 (한 줄)

페어링 로드 → `tunnel_create_remotexpc(10.7.0.1:49152, pairing)` → adapter+내부 RSD →
RSD 핸드셰이크·서비스목록 → `afc`로 서명 .ipa 업로드 → `installation_proxy_install` → 완료.
**서명(②③⑤)은 기존 `crates/resign` 그대로**, ④ 전송만 이걸로 교체.

## 빌드 구성 (오히려 단순해짐)

- **`libidevice_ffi.a` 하나**(idevice 저장소 `ffi/` 크레이트, `aarch64-apple-ios`)가
  **minimuxer + libimobiledevice + OpenSSL 세 xcframework를 대체**한다.
- idevice는 **순수 Rust(rustls/ring)** — C 라이브러리·OpenSSL 의존이 없다 → 그동안 씨름한
  링크·실행 문제(동적 OpenSSL embed, libimobiledevice 심볼)가 **통째로 사라진다**.
- 우리 `crates/resign`은 이미 idevice(fork)를 쓰므로 버전 정합만 맞추면 된다.
- CI(`ios-app.yml`): "minimuxer+libimobiledevice+OpenSSL 받기" 단계를
  "`libidevice_ffi.a` 빌드(또는 검증용 프리빌트)"로 교체.

## 단계별 계획

1. **`libidevice_ffi.a` 확보**: jkcoxson/idevice `ffi`를 iOS 타깃으로 빌드(feature:
   tunnel, rsd, installation_proxy, afc, core_device_proxy). **먼저 StikJIT 프리빌트로 배선을
   검증**해 리스크를 분리 → 이후 자체 빌드로 고정(우리 fork rev 정합).
2. **Swift 배선(신규 `ios/Shard/Sources/RsdInstall.swift`)**: StikJIT `IdeviceFFI.swift`/
   `StikJIT.swift`를 참고해 tunnel→adapter→rsd→afc→install 시퀀스 + 진행 콜백, 실패 지점별 에러.
3. **시트 배선**: "연결 테스트"=도달성(49152)+터널+RSD 서비스목록, "지금 갱신"=서명(resign)→
   afc 업로드→install. 로그·진단은 지금 만들어둔 fileTail 방식 재사용.
4. **minimuxer 제거**: minimuxer/libimobiledevice/OpenSSL xcframework·Swift 바인딩·
   `project.yml`·`ios-app.yml` 항목 삭제. `crates/resign/src/install.rs`의 구 idevice(classic)
   경로도 정리(사문화).
5. **문서·검증**: 폰에서 도달성→터널→RSD 서비스→업로드→설치 단계 로그로 실증.

## 위험·미지수 (착수 전/중 확인)

- **R1 RSD 포트 [해소]**: 10.7.0.1:**49152**(StikJIT 기본). 단 **StosVPN이 49152를 라우팅하는지**
  폰에서 확인 필요(LocalDevVPN은 StikDebug로 검증됨; StosVPN도 SideStore가 RSD로 갔으니 유력).
  안 되면 LocalDevVPN으로.
- **R2 페어링 포맷**: RemoteXPC 터널이 기존 `.mobiledevicepairing`을 그대로 받는지, 아니면
  `rp_pairing_file_*`(RP 포맷)이 필요한지. FFI에 `rp_pairing_file_from_bytes`가 있어 변환 경로 존재.
- **R3 FFI 빌드**: idevice `ffi` 크레이트를 iOS로 빌드하는 feature·타깃 정합(우리 fork rev 호환).
  → 프리빌트로 먼저 검증해 빌드 리스크와 배선 리스크를 분리.
- **R4 Developer Mode**: iOS 16+ RSD/RemoteXPC는 기기 Developer Mode ON이 전제일 수 있음
  (사이드로드 이력이면 대개 켜짐) — 폰에서 확인.
- **R5 get-task-allow [무관 예상]**: JIT엔 필요하지만 **설치엔 불필요**(installation_proxy는
  디바이스 서비스). 우리 케이스와 무관할 것.
- **R6 DDI [무관 예상]**: JIT/일부 서비스는 Developer Disk Image 마운트가 필요하지만
  **installation_proxy 설치는 DDI 불필요** — StikJIT의 DDI 단계는 우리에게 필요 없다.

## 검증 경계

헤드리스/CI는 링크·컴파일까지. 터널·RSD·설치는 **실기기 + 루프백 VPN + 페어링**이 있어야
동작을 확인할 수 있다 → 폰에서 단계 로그로 실증.

## 코드 지도(예정)

- **유지**: `crates/resign`(②③⑤ 서명 — 이미 완성·폰 검증).
- **신규**: `libidevice_ffi.a`(idevice ffi) + `ios/Shard/Sources/RsdInstall.swift`(④ 설치, FFI 호출).
- **제거**: minimuxer/libimobiledevice/OpenSSL xcframework·바인딩, `install.rs`의 구 idevice(classic) 경로.
