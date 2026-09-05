//! iOS 재서명 엔진 — ① 서명: 실제 iOS 앱 번들을 PC에서 재서명하고 검증한다.
//!
//! 무엇을 증명하나
//! - 관대 라이선스 apple-codesign(Apache/MPL)만으로, macOS 아닌 PC에서 실제
//!   `Shard.app`(중첩 프레임워크 MobileVLCKit 포함)을 재서명하고, 서명이 실제로
//!   박혔음을(각 Mach-O의 CodeDirectory + 암호 서명 CMS) 다시 읽어 검증할 수 있다.
//! - 즉 ① 조각은 SideStore 없이, 실기기 없이 여기까지 확실히 동작한다.
//!
//! 모드
//! - `resign [<ipa>] [<out.app>]` — <ipa>(기본 release/ios/Shard-unsigned.ipa)를 재서명하고
//!   구조를 덤프한다. <out.app>을 주면 서명 결과 .app을 그 경로에 낸다(CI가 zsign 서명본과
//!   나란히 비교하려고 결정적 위치가 필요하다).
//! - `resign --dump <ipa|app>` — 서명하지 않고 이미 서명된 번들의 구조만 덤프한다. zsign이 서명한
//!   번들을 **같은 apple-codesign 리더**로 읽어, 두 서명기의 슬롯·해시타입·엔티틀먼트·CMS를
//!   같은 잣대로 대조하기 위한 2차 뷰.
//!
//! 한계 (docs/재서명-엔진.md 참고)
//! - 여기 쓰는 인증서는 self-signed다. iOS는 self-signed로 서명된 앱의 *설치*를 거부한다 —
//!   애플이 발급한 개발 인증서라야 하고 그건 ②(Developer API)의 몫, 설치는 ④(minimuxer,
//!   실기기)의 몫. 그래서 이 PoC의 증명 범위는 "서명 파이프라인이 실제 번들에 대해
//!   동작한다"까지다. 설치 가능성이 아니라 서명 생성/구조를 증명한다.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use apple_codesign::{
    create_self_signed_code_signing_certificate, CertificateProfile, SettingsScope,
    SignatureEntity, SignatureReader, SigningSettings, UnifiedSigner,
};
use x509_certificate::KeyAlgorithm;

/// 실 흐름과 같은 4-키 엔티틀먼트. zsign 대조 때 `-e`로 넘기는 파일과 **같은 내용**이어야
/// 두 서명기의 DER/XML 엔티틀먼트를 바이트로 비교하는 의미가 있다.
const TEST_ENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>application-identifier</key><string>DM94SF72RB.net.sw.shard.DM94SF72RB</string>
<key>com.apple.developer.team-identifier</key><string>DM94SF72RB</string>
<key>get-task-allow</key><true/>
<key>keychain-access-groups</key><array><string>DM94SF72RB.net.sw.shard.DM94SF72RB</string></array>
</dict></plist>"#;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // ── 덤프 전용 모드: 이미 서명된 번들(예: zsign 출력)의 구조만 읽어 찍는다 ──
    if args.first().map(String::as_str) == Some("--dump") {
        let target = PathBuf::from(args.get(1).context("--dump <ipa|app> 경로 필요")?);
        let work = std::env::temp_dir().join("shard-resign-dump");
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work)?;
        // .app 디렉터리면 그대로, .ipa면 풀어서 Payload/*.app.
        let app = if target.is_dir() {
            target.clone()
        } else {
            extract_ipa(&target, &work.join("x"))?
        };
        println!("=== 덤프(서명 안 함): {} ===", app.display());
        dump_signature(&app)?;
        return Ok(());
    }

    // ── strip 검증 모드: 프레임워크 서명 제거(Mach-O 수술)가 무결한지 확인 ──
    // 바이너리에 엔티틀먼트로 서명(Sideloadly 재현) → strip → 파싱/재서명 확인.
    if args.first().map(String::as_str) == Some("--strip-test") {
        let bin = PathBuf::from(args.get(1).context("--strip-test <mach-o 바이너리> 필요")?);
        let work = std::env::temp_dir().join("shard-strip-test");
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work)?;
        let (cert, key) = create_self_signed_code_signing_certificate(
            KeyAlgorithm::Rsa, CertificateProfile::AppleDevelopment, "STRIP", "Strip Test", "US",
            chrono::Duration::try_days(365).context("유효기간")?,
        )?;
        let mut s1 = SigningSettings::default();
        s1.set_signing_key(&key, cert);
        s1.set_entitlements_xml(SettingsScope::Main, TEST_ENT)?;
        let signed = work.join("bin");
        UnifiedSigner::new(s1).sign_path(&bin, &signed).context("1) 서명")?;
        println!("1) 엔티틀먼트로 서명   → {}", macho_state(&signed)?);
        let stripped = resign::engine::strip_macho_code_signature(&signed)?;
        println!("2) strip(실행={stripped})    → {}", macho_state(&signed)?);
        let (cert2, key2) = create_self_signed_code_signing_certificate(
            KeyAlgorithm::Rsa, CertificateProfile::AppleDevelopment, "STRIP2", "Strip Test2", "US",
            chrono::Duration::try_days(365).context("유효기간")?,
        )?;
        let mut s2 = SigningSettings::default();
        s2.set_signing_key(&key2, cert2);
        let resigned = work.join("bin2");
        UnifiedSigner::new(s2).sign_path(&signed, &resigned).context("3) 재서명")?;
        println!("3) 엔티틀먼트 없이 재서명 → {}", macho_state(&resigned)?);
        println!("   기대: 1)엔티틀먼트 O → 2)서명 X → 3)서명 O·엔티틀먼트 0줄");
        return Ok(());
    }

    // ── strip-only 검증: 원본 그대로 strip(서명 먼저 안 함 — fat 경로를 그대로 테스트) ──
    if args.first().map(String::as_str) == Some("--strip-only") {
        let bin = PathBuf::from(args.get(1).context("--strip-only <mach-o 바이너리> 필요")?);
        let work = std::env::temp_dir().join("shard-strip-only");
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work)?;
        let target = work.join("bin");
        fs::copy(&bin, &target)?;
        println!("0) 원본            → {}", macho_state(&target).unwrap_or_else(|e| format!("읽기 실패: {e:?}")));
        let stripped = resign::engine::strip_macho_code_signature(&target)?;
        println!("1) strip(실행={stripped})  → {}", macho_state(&target).unwrap_or_else(|e| format!("읽기 실패(손상?): {e:?}")));
        // 재서명(엔티틀먼트 없이) — 깨끗한 상태에서 다시 서명되는지.
        let (cert, key) = create_self_signed_code_signing_certificate(
            KeyAlgorithm::Rsa, CertificateProfile::AppleDevelopment, "SO", "Strip Only", "US",
            chrono::Duration::try_days(365).context("유효기간")?,
        )?;
        let mut s = SigningSettings::default();
        s.set_signing_key(&key, cert);
        let re = work.join("bin2");
        UnifiedSigner::new(s).sign_path(&target, &re).context("재서명")?;
        println!("2) 엔티틀먼트 없이 재서명 → {}", macho_state(&re)?);
        return Ok(());
    }

    // ── fat strip 검증: 서명된 thin을 fat(1아치)로 감싸 strip이 서명을 제거하는지 확인 ──
    // (디바이스의 MobileVLCKit이 '서명된 fat'이라, apple-codesign이 thin으로 바꿔버리지 않는 이 경로가 관건)
    if args.first().map(String::as_str) == Some("--fat-strip-test") {
        let bin = PathBuf::from(args.get(1).context("--fat-strip-test <서명된 thin 바이너리> 필요")?);
        let thin = fs::read(&bin)?;
        if thin.len() < 12 {
            anyhow::bail!("바이너리가 너무 작다");
        }
        let cputype = u32::from_le_bytes([thin[4], thin[5], thin[6], thin[7]]);
        let cpusubtype = u32::from_le_bytes([thin[8], thin[9], thin[10], thin[11]]);
        // fat(1아치) 래핑: cafebabe, nfat=1, arch(cputype,cpusubtype,offset,size,align=14/16KB).
        let align = 14u32;
        let al = 1usize << align;
        let offset = ((8 + 20 + al - 1) & !(al - 1)) as u32;
        let mut fat = Vec::new();
        fat.extend_from_slice(&0xcafe_babeu32.to_be_bytes());
        fat.extend_from_slice(&1u32.to_be_bytes());
        fat.extend_from_slice(&cputype.to_be_bytes());
        fat.extend_from_slice(&cpusubtype.to_be_bytes());
        fat.extend_from_slice(&offset.to_be_bytes());
        fat.extend_from_slice(&(thin.len() as u32).to_be_bytes());
        fat.extend_from_slice(&align.to_be_bytes());
        fat.resize(offset as usize, 0);
        fat.extend_from_slice(&thin);
        let work = std::env::temp_dir().join("shard-fat-strip");
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work)?;
        let fatpath = work.join("fatbin");
        fs::write(&fatpath, &fat)?;
        println!("0) fat 래핑(1아치) → {}", macho_state(&fatpath).unwrap_or_else(|e| format!("읽기 실패: {e:?}")));
        let stripped = resign::engine::strip_macho_code_signature(&fatpath)?;
        println!("1) fat strip(={stripped}) → {}", macho_state(&fatpath).unwrap_or_else(|e| format!("읽기 실패(손상?): {e:?}")));
        println!("   기대: 0)서명 O → 1)서명 X (fat 재조립 무결)");
        return Ok(());
    }

    // ── 서명 모드 ──
    let ipa = args
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("release/ios/Shard-unsigned.ipa"));
    if !ipa.exists() {
        bail!(
            "ipa를 못 찾음: {} — 워크스페이스 루트에서 실행하거나 경로를 인자로 넘겨라",
            ipa.display()
        );
    }
    // 선택 2번째 인자: 서명 결과 .app을 낼 결정적 경로(CI 대조용).
    let out_app: Option<PathBuf> = args.get(1).map(PathBuf::from);

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
    settings
        .set_entitlements_xml(SettingsScope::Main, TEST_ENT)
        .context("엔티틀먼트 설정")?;
    // 이중 CD(SHA-1 주 + SHA-256 대체) — 엔진(engine.rs)과 같은 설정. 아래 덤프가 메인 exe에서
    // "CD[sha1 주+대체1]"로 나와야 zsign/SideStore와 같은 hash agility가 박힌 것.
    settings.set_digest_type(SettingsScope::Main, apple_codesign::cryptography::DigestType::Sha1);
    settings.add_extra_digest(SettingsScope::Main, apple_codesign::cryptography::DigestType::Sha256);

    // 4) 번들 재서명. apple-codesign이 중첩 프레임워크(MobileVLCKit)를 먼저 서명하고
    //    최상위 앱을 서명하며 _CodeSignature/CodeResources를 쓴다.
    let signed = out_app.unwrap_or_else(|| work.join("signed").join(app.file_name().unwrap()));
    fs::create_dir_all(signed.parent().unwrap())?;
    if signed.exists() {
        fs::remove_dir_all(&signed).ok();
    }
    UnifiedSigner::new(settings)
        .sign_path(&app, &signed)
        .context("번들 서명 실패")?;
    println!("  서명 출력 : {}", signed.display());
    println!();

    // 5) 검증 — 서명된 번들을 *다시 읽어* Mach-O마다 서명이 박혔는지 센다.
    dump_signature(&signed)?;
    Ok(())
}

/// 서명된 번들을 apple-codesign 리더로 읽어 Mach-O마다 서명 구조를 찍는다.
///
/// 코드가 "서명했다"고 말하는 것으로는 부족하다(CLAUDE.md: 결과물을 뜯어본다). zsign 대조에서는
/// 이 함수를 zsign 출력에도 그대로 돌려 같은 잣대로 슬롯·해시타입·엔티틀먼트를 본다.
fn dump_signature(app: &Path) -> Result<()> {
    let entities = SignatureReader::from_path(app)
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
                    let cd = sig.code_directory.as_ref();
                    let ident = cd.map(|cd| cd.identifier.as_str()).unwrap_or("<no-cd>");
                    let digest = cd.map(|cd| cd.digest_type.as_str()).unwrap_or("?");
                    let alts = sig.alternative_code_directories.len();
                    println!(
                        "    Mach-O  서명:O  CMS:{}  id={}  CD[{} 주+대체{}]  ent[XML {}줄/DER {}줄]  ({})",
                        if has_cms { "O" } else { "X(ad-hoc)" },
                        ident,
                        digest,
                        alts,
                        sig.entitlements_plist.len(),
                        sig.entitlements_der_plist.len(),
                        e.path.display()
                    );
                    // 메인 실행파일(…/Shard.app/Shard)이면 CD 세부를 찍는다 — 0xe8008016 진단.
                    // zsign은 SHA-1+SHA-256 이중 CD를 낸다. apple-codesign이 무엇을 내는지(digest,
                    // 이중 CD, flags, exec_seg_flags)가 iOS 26 설치 거부의 남은 후보다.
                    if e.path.file_name().and_then(|n| n.to_str()) == Some("Shard") {
                        if let Some(cd) = cd {
                            println!("      ── 메인 exe CD 세부 ──");
                            println!("      version={}  platform={}", cd.version, cd.platform);
                            println!("      digest_type(주)={}", cd.digest_type);
                            println!("      flags={}", cd.flags);
                            println!("      exec_seg_flags={:?}", cd.executable_segment_flags);
                            println!("      runtime_version={:?}", cd.runtime_version);
                            println!(
                                "      대체 CD {}개: {}",
                                sig.alternative_code_directories.len(),
                                sig.alternative_code_directories
                                    .iter()
                                    .map(|(slot, c)| format!("{slot}:{}", c.digest_type))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                        }
                        if !sig.entitlements_plist.is_empty() {
                            println!("      ── 메인 exe 엔티틀먼트(XML) ──");
                            for l in &sig.entitlements_plist {
                                println!("      {l}");
                            }
                            println!("      ── DER 엔티틀먼트 줄 수: {} ──", sig.entitlements_der_plist.len());
                        }
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
    if signed_machos == 0 {
        bail!("서명된 Mach-O가 없다 — 파이프라인 실패 또는 미서명 번들");
    }
    println!("  ✅ 덤프 완료.");
    Ok(())
}

/// 한 Mach-O의 서명 상태를 한 줄로: 서명 유무·엔티틀먼트 줄 수·CMS 유무.
fn macho_state(path: &Path) -> Result<String> {
    let entities = SignatureReader::from_path(path)?.entities()?;
    for e in &entities {
        if let SignatureEntity::MachO(m) = &e.entity {
            return Ok(match &m.signature {
                Some(sig) => format!(
                    "서명 O, 엔티틀먼트 {}줄, CMS {}",
                    sig.entitlements_plist.len(),
                    if sig.cms.is_some() { "O" } else { "X" }
                ),
                None => "서명 X".to_string(),
            });
        }
    }
    Ok("Mach-O 아님".to_string())
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
