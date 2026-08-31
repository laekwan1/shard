//! iOS 재서명 엔진 PoC — 1단계: apple-codesign 서명 API가 닿는지 검증.
//!
//! 여기서 증명하는 것: 관대 라이선스(apple-codesign, Apache/MPL)만으로 iOS 서명의
//! 핵심 타입에 접근된다 = ① 서명 조각은 SideStore(AGPL) 없이 갈 수 있다.
use apple_codesign::SigningSettings;

fn main() -> anyhow::Result<()> {
    // 서명 설정 객체를 만들 수 있으면 서명 파이프라인 진입점이 확보된 것.
    let settings = SigningSettings::default();
    println!("resign PoC — apple-codesign signing API reachable");
    println!("  binary identifier: {:?}", settings.binary_identifier(apple_codesign::SettingsScope::Main));
    Ok(())
}
