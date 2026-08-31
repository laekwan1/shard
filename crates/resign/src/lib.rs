//! iOS 자체 서명 엔진 (라이브러리).
//!
//! 조각(설계: docs/재서명-엔진.md):
//! - **① 서명** — apple-codesign. bin `main.rs`가 실제 `Shard.app` 재서명·검증으로 확증.
//! - **③ 로그인+anisette** — `auth`: icloud_auth/omnisette(SideStore) 위 얇은 층. 실제 구현이라
//!   그대로 의존. 애플이 자주 깨는 부분이지만 여기선 이미 돌아가는 코드를 쓴다.
//! - **② Developer API** — `dev_api`: apple-private-apis엔 스텁(todo!)뿐이라 **우리가 직접 구현**.
//!   지금은 시그니처/흐름 골격, 요청 본문은 AltSign 참조해 폰 검증과 함께 채운다.
//! - **④ 설치/갱신** — (다음 증분) jkcoxson/idevice 포팅: misagent(㉮ 무중단)/installation_proxy(㉯).
//!
//! 검증 경계: **호스트/CI 컴파일까지가 헤드리스 상한**. 실제 로그인·발급·설치는 Apple ID·anisette·
//! 폰 lockdownd가 있어야 하므로 **실기기(폰)** 에서 실증된다. iOS로는 shard-mobile에 C ABI를
//! feature로 얹어 xcframework로 빌드한다(shard→shard-mobile와 같은 길).

pub mod auth;
pub mod dev_api;
pub mod engine;
pub mod install;
