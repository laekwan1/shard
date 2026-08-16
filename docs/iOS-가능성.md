# iOS 가능성 — 우회 없이 "영상 받기" 중심

2026-08-17 조사. 사용자가 **우회는 빼고 영상 받기 위주여도 좋다**고 했으므로,
가장 큰 벽이었던 "WKWebView를 로컬 프록시로 못 돌린다"는 없어진다. 남은 질문은
**iOS에서 영상 다운로더가 되는가**다. 결론부터: **된다. 그리고 생각보다 유리한
위치다** — 단, 맥이 있어야 시작하고, 배포는 사이드로딩뿐이다.

---

## 왜 유리한가 — 어려운 절반이 이미 이식 가능한 Rust다

iOS 다운로더에서 제일 어려운 것은 "받은 조각들을 ffmpeg 없이 하나의 파일로
합치는 것(먹싱)"이다. **그게 이미 `crates/shard/src/download/`에 있다.**

- `ebml.rs` · `mkv.rs` · `webm.rs` — Matroska/WebM 먹서 (어떤 코덱이든 담음)
- `mp4.rs` — MP4 조각 demux (B프레임 순서까지 다룸)
- `pull.rs` — HLS/DASH 조각 받기
- `youtube.rs` · `sabr.rs` — 유튜브 포맷·SABR 처리
- 데스크톱 전용은 `browser.rs`(다운로드 창)뿐

이 모듈은 `desktop` 기능에 **묶여 있지 않다** — `cargo check -p shard
--no-default-features`가 통과한다. WebView2·windows 의존 0. 즉 **iOS(aarch64)로
그대로 컴파일된다.**

그리고 `shard-mobile`은 이미 iOS를 위한 `staticlib`을 내보내고, 엔진을 C ABI로
노출하는 `jni.rs`가 있다. **여기에 다운로드 진입점을 몇 개 더 얹으면**, 안드로이드가
Kotlin으로 새로 짠 다운로더(`Muxer.kt`·`Hls.kt`)를 iOS에서 Swift로 다시 짤 필요가
없다 — 데스크톱이 실제로 쓰는, 가장 검증된 그 Rust 코드를 쓴다.

> 참고: 안드로이드는 이 Rust 다운로드 엔진을 **안 쓴다** — Kotlin으로 따로 짰다.
> 이 Rust 엔진은 지금 데스크톱만 쓴다. iOS는 데스크톱과 같은 것을 쓰는 게 맞다.

---

## 안드로이드의 각 수단이 iOS에서 무엇이 되나

| 하는 일 | 안드로이드 | iOS | 상태 |
|---|---|---|---|
| 페이지의 영상 요청을 봄 | `shouldInterceptRequest`(네트워크 계층, 모든 요청) | **없음** — http/https 하위요청 가로채기 불가 | ⚠️ 약해짐 |
| 유튜브 포맷 잡기 | 문서시작 JS 주입 + 브리지(`YouTube.RECORDER`) | `WKUserScript(.atDocumentStart)` + `WKScriptMessageHandler` | ✅ 그대로 이식 |
| 받기·먹싱 | Kotlin `Muxer`/`Hls`(MediaMuxer) | **Rust `download/`를 C ABI로** | ✅ 더 나음 |
| 파일 저장 | MediaStore `Movies/Shard` | Files 앱(공유 컨테이너) 또는 Photos | ✅ 됨(아래 주의) |
| 백그라운드 다운로드 | 포그라운드 서비스 | `URLSession` background | ✅ 됨 |

**가장 중요한 약점 하나**: iOS에는 `shouldInterceptRequest`가 없다. 그래서 페이지가
받는 영상을 **네트워크에서** 낚아채지 못하고, **JS 훅**(`fetch`/`XHR`/`MediaSource`/
`video.src` 가로채기)에만 의존한다. 이것으로 **유튜브와 대부분의 스트리밍
사이트는 잡히지만**, 순수 네이티브 `<video src>` + range 요청만 쓰는 일부 사이트는
놓친다. 즉 **유튜브 경로는 강하고, 범용 사이트 경로는 안드로이드보다 약하다.**

**저장 형식 주의**: Rust 먹서는 Matroska(.mkv)/WebM를 쓴다. iOS **Photos**는 mkv를
잘 못 받는다 — **Files 앱**에 그대로 저장하는 편이 맞고, 사용자는 파일 앱·VLC 등에서
연다. mp4로 담고 싶으면 mp4 먹서를 Rust에 더해야 한다(demux는 있으나 writer는
mkv 쪽만 있음). 블로커는 아니고 UX 결정이다.

---

## 진짜 관문 둘 — 엔지니어링이 아니라 플랫폼

### 1. 배포: App Store 불가 → 사이드로딩뿐

App Store 심사 지침 **5.2.3**: *"제3자 출처(유튜브 등)에서 미디어를 저장·변환·
다운로드하는 기능을 그 출처의 명시적 허가 없이 넣지 말 것."* **적극적으로 거부**된다.
유튜브 다운로더는 스토어에 못 올린다.

따라서 유통은 **사이드로딩**:

- **무료 Apple ID** — 7일마다 재서명, 동시 3앱, 푸시 없음. SideStore는 기기 안에서
  로컬 VPN으로 스스로 갱신(맥 없이). AltStore는 같은 와이파이의 맥 필요.
- **유료 Apple Developer($99/년)** — 인증서 1년, 3앱 제한 없음. 그래도 스토어가
  아니라 사이드로딩이다.
- **EU 한정** — iOS 17.4+ 대체 마켓플레이스(AltStore PAL). 지역 제한.

### 2. 빌드: macOS + Xcode 필수

Rust를 iOS로 컴파일하고, 링크하고, 서명하고, 기기에 올리는 모든 단계가 **맥에서만**
된다. **지금 이 기계는 Windows다** — iOS는 여기서 시작조차 못 한다. 맥(또는 맥
클라우드 빌더)이 있어야 한다.

---

## 하면 이런 순서 (맥이 있다는 가정)

1. **엔진/다운로더를 iOS 스태틱 라이브러리로** — `cargo build --target
   aarch64-apple-ios`, `download/`를 C ABI로 노출, xcframework로 묶기. (Rust는 다 됨)
2. **최소 앱** — WKWebView + 주소창 + `WKUserScript`로 유튜브 캡처 → URL을 Rust로
   넘겨 받기·먹싱 → Files에 저장. 우회 없음.
3. **보관함** — 저장한 파일 목록·재생(AVPlayer). 안드로이드 UI 문법 차용.
4. **사이드로딩 배포** — 무료 ID로 시작, 필요하면 $99.

**공수 감**: 먹싱이 이미 있어 "다운로더를 처음부터"보다 훨씬 작다. 큰 덩어리는
(a) Rust→xcframework 빌드 세팅, (b) WKWebView + JS 캡처, (c) 저장·보관함 UI.
우회를 넣는다면 Network Extension(VPN 엔타이틀먼트, 유료 계정)이 별도로 붙지만,
지금 계획에선 뺀다.

---

## 필요한 결정 / 준비물

- **맥** (또는 맥 클라우드 CI) — 없으면 iOS는 시작 불가. 이게 첫 관문.
- **Apple ID** (무료로 시작 가능) / 오래 쓰려면 **$99 개발자 계정**.
- 저장 형식: 우선 **Files에 mkv/m4a 그대로** 저장으로 시작(가장 적은 코드).

## 불확실한 것 (정직하게)

- JS 캡처의 사이트별 신뢰도는 편차가 있다. 유튜브는 플레이어를 자주 바꾸므로
  캡처 스크립트 유지보수가 안드로이드와 마찬가지로 계속 필요하다.
- 사이드로딩 규칙·EU 마켓은 애플 정책에 따라 바뀐다.

---

## 출처

- WKWebView 요청 가로채기 한계: [Apple 개발자 포럼](https://developer.apple.com/forums/thread/87474), [WKURLSchemeHandler 논의](https://developer.apple.com/forums/thread/810843)
- FFmpegKit 은퇴(2025-01): [Saying Goodbye to FFmpegKit](https://tanersener.medium.com/saying-goodbye-to-ffmpegkit-33ae939767e1)
- 사이드로딩(무료 ID 7일·3앱, $99): [SideStore FAQ](https://docs.sidestore.io/docs/faq), [2026 사이드로딩 가이드](https://builds.io/blog/technologies/ios-technologies/how-to-sideload-apps-iphone/)
- App Store 5.2.3 다운로더 거부: [App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/), [5.2.3 포럼 사례](https://developer.apple.com/forums/thread/765340)
