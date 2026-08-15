# Shard / Veil — 이 저장소에서 일하는 법

Shard는 서버를 거치지 않는 DPI/SNI 우회(창 하나짜리 Windows 앱 + 안드로이드 앱),
Veil은 자기 서버나 Tor를 거치는 익명화다. 둘은 답하는 문제가 다르다 — 어느 쪽이
필요한지는 [docs/Shard.md](docs/Shard.md), [docs/Veil.md](docs/Veil.md)에 있다.

## "이어하자" 라고만 하면

1. [docs/다음-작업.md](docs/다음-작업.md) — 지금 무엇을 하던 중이고 다음이 무엇인지
2. [docs/결함-기록.md](docs/결함-기록.md) — 이미 밟은 지뢰. **작업 전에 읽는다**
3. [docs/작업-기록.md](docs/작업-기록.md) — 왜 그렇게 만들었는지

이 세 문서가 인수인계의 전부다. 작업이 끝날 때마다 갱신한다 — 갱신하지 않은
문서는 다음 사람을 잘못된 곳으로 보낸다.

## 무엇을 어디서 만드나

| 경로 | 무엇 |
|---|---|
| `crates/shard/` | 우회 엔진, 다운로드, 창 하나짜리 UI(`src/shell.rs` + `assets/ui/`) |
| `crates/shard/src/ui.rs` | 옛 창 두 개짜리 UI. 새 기능은 넣지 않는다 |
| `crates/shard-mobile/` | 폰용 엔진 (소켓 기반, C ABI) |
| `crates/uikit/` | 두 앱이 함께 쓰는 창·트레이·설정 |
| `android/core/` | 안드로이드 앱 본체 (Kotlin) |
| `release/PC/Shard/` | 배포되는 최신 창 하나짜리 빌드 |
| `release/PC/Shard-old/` | 창 두 개짜리 옛 모습. 갱신하지 않는다 |
| `release/android/Shard/` | 배포되는 APK |

## 코드 규칙

**주석은 "왜"를 적는다.** 무엇을 하는지는 코드가 말한다. 왜 그 방법이어야
했는지, 다른 방법이 왜 안 됐는지를 적는다 — 그것만이 다음 사람이 같은 자리에서
다시 고민하지 않게 한다. 영어로 쓴다.

**사용자에게 보이는 글자는 한국어.** 로그와 주석은 영어.

**고친 버그는 주석에 남긴다.** "이렇게 하지 않으면 무엇이 어떻게 깨지는지"를
한 줄로. 그 한 줄이 없으면 누군가 다시 되돌린다.

**테스트는 고친 것을 지키는 것만.** 이름은 문장으로 쓴다 —
`a_frame_is_timed_by_when_it_is_shown_not_when_it_is_decoded`.

**의존성은 늘리지 않는다.** OS 바인딩과 암호 원시함수는 예외.

## 반드시 지킬 것

- **두 형태 모두 빌드한다.** `cargo build -p shard` 와
  `cargo check -p shard --no-default-features`. 폰은 `desktop` 기능 없이 같은
  크레이트를 쓴다 — 창 전용 모듈을 부르면 폰 빌드가 깨지고, 창에서는 보이지 않는다.
- **XML 레이아웃을 문자열로 옮기지 않는다.** 두 번 깨뜨렸다. 직접 고쳐 쓴다.
- **스크립트가 중간에 멈추면 아무것도 쓰이지 않는다.** 반영됐다고 보고하기 전에
  파일을 확인한다.
- **결과물을 확인한다.** 코드만 읽고 고쳤다고 말하지 않는다 — 만들어진 파일을
  뜯어보고, 화면을 띄워 본다.

## 확인 절차

**PC 화면** — UI만 따로 띄워 측정한다. `assets/ui/`의 세 파일을 임시 폴더에
복사하고 Rust 쪽을 흉내내는 `stub.js`를 붙인 뒤 `python -m http.server 8777`,
그리고 브라우저 도구로 위치·크기·색을 잰다. `node --check app.js`는 매번.

**폰 화면** — 에뮬레이터에 올려 눈으로 본다.

```
android/gradlew :shard:assembleDebug -q --offline
adb install -r android/shard/build/outputs/apk/debug/shard-debug.apk
adb shell input tap <x> <y>
adb exec-out screencap -p > shot.png
```

AVD 이름 `shardtest`, adb는 `%LOCALAPPDATA%\Android\Sdk\platform-tools\adb.exe`.
Git Bash에서는 `MSYS_NO_PATHCONV=1`을 붙여야 `/sdcard` 경로가 망가지지 않는다.

**저장한 파일** — 컨테이너를 직접 뜯어 본다. 타임스탬프가 역행하는지, 그림이
들어갔는지, 표가 앞에 있는지. 오늘 두 번, 코드는 맞아 보이는데 파일이 틀렸다.

## 배포

빌드 → `release/PC/Shard/shard.exe` 또는 `release/android/Shard/Shard.apk`로 복사.
실행 중이면 교체되지 않는다 — 사용자에게 닫아 달라고 한다.

## 커밋

한국어로, 무엇을 왜 바꿨는지. 제목은 그 변경이 무엇을 되찾았는지 한 줄로.

## 절대 하지 않는 것

- DuckDNS 토큰, SSH 키, 서버 정보를 대화나 커밋에 넣지 않는다.
  `.gitignore`가 `*.key`, `*.pem`, `id_rsa*`, `ssh-key-*`, `*.env`를 막는다.
