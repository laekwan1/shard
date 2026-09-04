//! iOS 재서명 엔진 — ① 서명: 실제 iOS 앱 번들을 PC에서 재서명하고 검증한다.
//!
//! 무엇을 증명하나
//! - 관대 라이선스 apple-codesign(Apache/MPL)만으로, macOS 아닌 PC에서 실제
//!   `Shard.app`(중첩 프레임워크 MobileVLCKit 포함)을 재서명하고, 서명이 실제로
//!   박혔음을(각 Mach-O의 CodeDirectory + 암호 서명 CMS) 다시 읽어 검증할 수 있다.
//! - 즉 ① 조각은 SideStore 없이, 실기기 없이 여기까지 확실히 동작한다.
//!
//! 한계 (docs/재서명-엔진.md 참고)
//! - 여기 쓰는 인증서는 self-signed다. iOS는 self-signed로 서명된 앱의 *설치*를 거부한다 —
//!   애플이 발급한 개발 인증서라야 하고 그건 ②(Developer API)의 몫, 설치는 ④(minimuxer,
//!   실기기)의 몫. 그래서 이 PoC의 증명 범위는 "서명 파이프라인이 실제 번들에 대해
//!   동작한다"까지다. 설치 가능성이 아니라 서명 생성/구조를 증명한다.
//! - 실전에선 아래 (cert, key)만 ②가 발급한 값으로 바뀐다. 파이프라인은 그대로다.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use apple_codesign::{
    create_self_signed_code_signing_certificate, CertificateProfile, SettingsScope,
    SignatureEntity, SignatureReader, SigningSettings, UnifiedSigner,
};
use x509_certificate::KeyAlgorithm;

fn main() -> Result<()> {
    // 기본 대상은 배포용 미서명 .ipa. 워크스페이스 루트에서 실행한다고 가정.
    let ipa = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("release/ios/Shard-unsigned.ipa"));
    if !ipa.exists() {
        bail!(
            "ipa를 못 찾음: {} — 워크스페이스 루트에서 실행하거나 경로를 인자로 넘겨라",
            ipa.display()
        );
    }

    // 매 실행마다 깨끗한 작업 폴더에서. (이전 서명 잔재가 검증을 오염시키지 않도록.)
    let work = std::env::temp_dir().join("shard-resign-poc");
    if work.exists() {
        fs::remove_dir_all(&work).ok();
    }
    fs::create_dir_all(&work)?;

    println!("① 서명 PoC — 실제 Shard.app 재서명·검증");
    println!("  대상 .ipa : {}", ipa.display());

    // 1) .ipa(zip) 풀기 → Payload/*.app. 실제 엔진도 .ipa를 풀어 번들을 다룬다.
    let extracted = work.join("extracted");
    let app = extract_ipa(&ipa, &extracted)?;
    println!(
        "  앱 번들   : {}",
        app.strip_prefix(&extracted).unwrap_or(&app).display()
    );

    // 2) self-signed 개발 인증서 + 키. 실전에선 ②가 애플 발급 값으로 대체.
    //    RSA-2048은 애플 개발 인증서와 같은 알고리즘이라 파이프라인이 동일하게 성립한다.
    let (cert, key) = create_self_signed_code_signing_certificate(
        KeyAlgorithm::Rsa,
        CertificateProfile::AppleDevelopment,
        "SHARDPOC01",
        "Shard Resign PoC",
        "US",
        chrono::Duration::try_days(365).context("잘못된 유효기간")?,
    )
    .context("self-signed 인증서 생성 실패")?;
    println!("  인증서    : self-signed AppleDevelopment RSA-2048 (설치용 아님 — ②가 대체)");

    // 3) 서명 설정에 신원 주입. 이게 없으면 ad-hoc(암호 서명 없는 다이제스트뿐)이 된다.
    let mut settings = SigningSettings::default();
    settings.set_signing_key(&key, cert);

    // 진단(0xe8008016): 실 흐름과 같은 4-키 엔티틀먼트를 Main 스코프로 넣고, 서명 후 메인 실행파일에
    // XML+DER가 실제로 박히는지 아래 검증에서 센다. 값 구조는 폰 진단 로그의 실제 프로파일 엔티틀먼트와 동일.
    const TEST_ENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>application-identifier</key><string>DM94SF72RB.net.sw.shard.DM94SF72RB</string>
<key>com.apple.developer.team-identifier</key><string>DM94SF72RB</string>
<key>get-task-allow</key><true/>
<key>keychain-access-groups</key><array><string>DM94SF72RB.net.sw.shard.DM94SF72RB</string></array>
</dict></plist>"#;
    settings
        .set_entitlements_xml(SettingsScope::Main, TEST_ENT)
        .context("엔티틀먼트 설정")?;

    // 4) 번들 재서명. apple-codesign이 중첩 프레임워크(MobileVLCKit)를 먼저 서명하고
    //    최상위 앱을 서명하며 _CodeSignature/CodeResources를 쓴다.
    let signed = work.join("signed").join(app.file_name().unwrap());
    fs::create_dir_all(signed.parent().unwrap())?;
    UnifiedSigner::new(settings)
        .sign_path(&app, &signed)
        .context("번들 서명 실패")?;
    println!("  서명 출력 : {}", signed.display());
    println!();

    // 5) 검증 — 서명된 번들을 *다시 읽어* Mach-O마다 서명이 박혔는지 센다.
    //    코드가 "서명했다"고 말하는 것으로는 부족하다(CLAUDE.md: 결과물을 뜯어본다).
    let entities = SignatureReader::from_path(&signed)
        .context("서명 읽기 실패")?
        .entities()
        .context("엔티티 열거 실패")?;

    let mut signed_machos = 0usize;
    let mut cms_signed = 0usize;
    let mut cs_files = 0usize;
    for e in &entities {
        match &e.entity {
            SignatureEntity::MachO(m) => match &m.signature {
                Some(sig) => {
                    signed_machos += 1;
                    let has_cms = sig.cms.is_some();
                    if has_cms {
                        cms_signed += 1;
                    }
                    let ident = sig
                        .code_directory
                        .as_ref()
                        .map(|cd| cd.identifier.as_str())
                        .unwrap_or("<no-cd>");
                    println!(
                        "    Mach-O  서명:O  CMS:{}  id={}  ent[XML {}줄/DER {}줄]  ({})",
                        if has_cms { "O" } else { "X(ad-hoc)" },
                        ident,
                        sig.entitlements_plist.len(),
                        sig.entitlements_der_plist.len(),
                        e.path.display()
                    );
                    // 메인 실행파일(…/Shard.app/Shard)이면 엔티틀먼트 실물을 찍는다 — 0xe8008016 진단.
                    if e.path.file_name().and_then(|n| n.to_str()) == Some("Shard")
                        && !sig.entitlements_plist.is_empty()
                    {
                        println!("      ── 메인 exe 엔티틀먼트(XML) ──");
                        for l in &sig.entitlements_plist {
                            println!("      {l}");
                        }
                        println!("      ── DER 엔티틀먼트 줄 수: {} ──", sig.entitlements_der_plist.len());
                    }
                }
                None => {
                    println!("    Mach-O  서명:X  ({})", e.path.display());
                }
            },
            SignatureEntity::BundleCodeSignatureFile(_) => cs_files += 1,
            _ => {}
        }
    }

    println!();
    println!(
        "  검증: 서명된 Mach-O {}개(그중 CMS 암호서명 {}개), _CodeSignature 파일 {}개",
        signed_machos, cms_signed, cs_files
    );
    if signed_machos == 0 || cms_signed == 0 {
        bail!("서명이 제대로 안 박혔다 — 파이프라인 실패");
    }
    println!("  ✅ ① 서명 파이프라인: 실제 Shard.app 재서명·검증 성공 (PC, self-signed).");
    Ok(())
}

/// .ipa(zip)를 dest로 풀고 Payload/ 아래 첫 .app 디렉터리 경로를 돌려준다.
fn extract_ipa(ipa: &Path, dest: &Path) -> Result<PathBuf> {
    let file = File::open(ipa).with_context(|| format!("open {}", ipa.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("깨진 zip")?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        // enclosed_name는 zip-slip(../ 탈출)을 막아 dest 밖으로 못 쓰게 한다.
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out = dest.join(&rel);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
        } else {
            if let Some(p) = out.parent() {
                fs::create_dir_all(p)?;
            }
            let mut w =
                File::create(&out).with_context(|| format!("create {}", out.display()))?;
            io::copy(&mut entry, &mut w)?;
        }
    }
    let payload = dest.join("Payload");
    let app = fs::read_dir(&payload)
        .with_context(|| format!("ipa에 Payload/ 없음 ({})", payload.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "app").unwrap_or(false))
        .context("Payload/에 .app 없음")?;
    Ok(app)
}
