# SideStore로 Shard 설치·자동 재서명 — 처음부터 (iLoader 방식)

무료 Apple ID로 사이드로드한 앱은 **7일마다 재서명**해야 계속 열린다. Sideloadly는
그걸 매번 케이블로 수동으로(그때마다 PC 필요) 해야 하지만, **SideStore**는 아이폰
안에서 스스로(인터넷만 있으면) 재서명해준다 — **초기 1회 설치 뒤엔 PC가 필요 없다.**

> **iLoader vs Sideloadly**: 역할은 비슷하지만(윈도우+USB+Apple ID로 .ipa 설치) 목적이
> 다르다. Sideloadly는 대상 앱을 직접 설치하고 7일마다 PC로 재실행해야 한다. iLoader는
> **SideStore를 설치하는 1회용 부트스트래퍼**이고, 이후 재서명은 SideStore가 자동으로
> 한다. **자동 재서명 능력은 iLoader가 아니라 SideStore에서 온다.**

> **왜 SideStore인가**: 오픈소스라 감사 가능, Apple ID 비밀번호는 애플로만 전송(SideStore
> 서버로 안 감). 아래 **보조 Apple ID**로 위험을 격리한다.

---

## 0. 준비물

- **아이폰** (개발자 모드 ON: 설정 › 개인정보 보호 및 보안 › 개발자 모드)
- **Windows PC (64비트)** — **초기 1회 SideStore 설치에만** 필요. 이후 안 씀.
  (32비트 Windows, ARM64 Windows 10은 미지원.)
- **보조 Apple ID** — 아래 1번에서 새로 만든다. **메인 계정은 절대 쓰지 말 것.**
- USB 케이블

## 1. 보조 Apple ID 만들기 (보안 격리)

서명 도구/SideStore에 넣을 **버리는 계정**을 하나 만든다. 결제수단·iCloud 사진 등
개인정보를 넣지 않는다.

1. [appleid.apple.com](https://appleid.apple.com) → **Apple ID 생성**(다른 이메일).
2. **2단계 인증**을 켠다(필수).
3. (권장) **로그인 및 보안 › 앱 암호**에서 **앱 암호(App-Specific Password)**를 만들어 둔다.
   비밀번호 대신 이걸 넣으면 도구가 털려도 메인 자격증명은 안전.

## 2. PC에 애플 드라이버 (iTunes)

- **iTunes**를 설치한다. **apple.com 다운로드판 권장**(Microsoft Store판보다 안정적).
  기기 인식용 드라이버 때문.

## 3. iLoader로 SideStore 설치 (초기 1회, PC 필요)

정확한 최신 절차는 **[docs.sidestore.io](https://docs.sidestore.io/docs/installation/prerequisites)**
가 기준이다(버전에 따라 UI가 바뀐다). 큰 흐름:

1. **iLoader**(SideStore의 공식 Windows 설치 도구)를 GitHub에서 받는다 — **MSI 권장**(EXE도 있음).
2. iLoader 실행. 아이폰을 **USB로 연결** → 아이폰에서 **"이 컴퓨터를 신뢰"** → 암호.
3. iLoader에서 **Apple 계정 로그인**(👉 **보조 Apple ID + 앱 암호**). 기기 선택 →
   **"Install SideStore (Stable)"**.
4. 아이폰: 설정 › 일반 › **VPN 및 기기 관리** → "개발자 앱"에서 **본인 Apple 계정 이름**
   → **신뢰**.
5. SideStore 앱을 열고 **같은 Apple ID로 로그인**한다(SideStore가 자신·다른 앱을 재서명하려면
   계정이 필요하다).

## 4. LocalDevVPN — 온디바이스 재서명 통로 (핵심)

SideStore는 **LocalDevVPN**(로컬/루프백 VPN)으로 폰이 "자신을 신뢰된 컴퓨터"처럼 여기게
해, PC 없이 스스로 설치·재서명한다.

1. 아이폰 App Store(또는 AltStore PAL 소스)에서 **LocalDevVPN** 설치 → 실행 → **연결(ON)**.
   "VPN 구성 허용" 뜨면 **허용** + 암호.
2. 이 VPN은 **외부로 트래픽을 보내지 않는다**(로컬 루프백). 설치/업데이트/재서명 API를 여는
   통로일 뿐이다. **SideStore로 설치·갱신할 때는 반드시 켜져 있어야 한다.**

## 5. Shard 설치 (앞으로는 Sideloadly 대신 이걸로)

1. **LocalDevVPN이 ON인지 확인.**
2. 새 `Shard-unsigned.ipa`를 아이폰으로 옮긴다(iCloud Drive/파일 앱/이메일/클라우드 등.
   윈도우라 AirDrop 없음).
3. **SideStore 앱 → My Apps → ＋ →** 옮긴 `Shard-unsigned.ipa` 선택 → 설치.
   서명 갱신을 이제 **SideStore가 관리**한다.

## 6. 자동 재서명 — 운용법

재서명이 일어나려면 **① LocalDevVPN ON + ② SideStore가 실행(도는 순간)** 둘 다 필요하다.

- **LocalDevVPN은 항상 켜두고, SideStore를 며칠에 한 번 열어**준다(iOS 백그라운드가
  불확실해서). 열면 만료 임박 앱을 자동 갱신한다. 수동 **Refresh All**도 있다.
- 인터넷(WiFi/셀룰러 무관)만 있으면 7일 만료 전에 갱신된다.

## 7. Shard 우회와 충돌하지 않는다

- **iOS Shard의 우회는 시스템 VPN이 아니라 앱 내부 "로컬 프록시"** 다(WKWebView를 그
  프록시로 가리킨다, iOS 17+; `WebView.swift`의 bypass proxy). VPN 슬롯을 안 쓰므로
  **LocalDevVPN과 공존**한다 — 둘 다 켜도 된다.
- ⚠️ 다만 **제3의 진짜 VPN 앱(시스템 VPN)** 을 쓰면 그건 LocalDevVPN과 슬롯이 충돌한다
  (iOS는 VPN 동시 하나). Shard 우회는 해당 없음.

---

## 알아둘 한계 (애플이 강제 — 무엇으로도 못 바꿈)

- **무료 계정은 7일 만료.** SideStore는 **만료 전 자동 갱신**만, 만료 자체는 못 없앤다.
- **동시 설치 앱 3개** 제한(SideStore가 슬롯 하나 차지할 수 있음).
- **주당 앱 ID 10개** 제한 — 새 빌드를 하루에 여러 번 갈면 걸릴 수 있다.
- 이 한계들을 없애는 유일한 합법적 길은 **유료 개발자 계정(1년 서명)** 뿐이다.

## 새 빌드(내가 주는 새 .ipa) 반영

- 자동 재서명은 "서명 만료"를 막는 것이고, **"새 버전 설치"는 별개**다.
- 새 `.ipa`가 나오면 **LocalDevVPN 켠 채 SideStore → ＋로 다시 설치**하면 된다(그 순간
  재서명도 됨).

## 문제 해결

- **"Apple ID 또는 암호가 올바르지 않음"**: 앱 암호를 쓰거나 2단계 인증 코드를 정확히.
  보조 계정이 잠기면 appleid.apple.com에서 풀 것.
- **설치/갱신이 안 됨**: LocalDevVPN이 **연결(ON)** 인지 먼저 확인. 그다음 SideStore를 열어
  **Refresh All**.
- **7일 지나 앱이 안 열림**: SideStore를 열어 Refresh All. 안 되면 ＋로 재설치.
- **anisette 관련 오류**: SideStore 문서(docs.sidestore.io)의 anisette 안내를 따른다.
