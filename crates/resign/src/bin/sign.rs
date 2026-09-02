//! PC에서 ⑤ 전체 재서명을 돌려 **실제 서명된 .ipa**를 만든다(설치 ④는 빼고).
//!
//! 로그인 → 팀 → 인증서(새 키+CSR) → App ID → 프로파일 → **①로 애플 인증서·프로파일 박아 재서명**
//! → .ipa 재포장. 폰 빌드의 `resign_and_install_blocking`을 device 없이 돌린다(서명만).
//!
//! ⚠️ 매 실행마다 새 인증서를 발급한다(무료 계정 dev 인증서 한도 2~3개 주의). 실제 제품은
//! 키·인증서를 앱에 저장해 재사용해야 한다(후속). Apple ID/비번/2FA는 stdin, 저장 안 함.
//!
//! 실행: `cargo run -p resign --bin sign`  또는 `verify.exe`와 같은 폴더의 `sign.exe`.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use resign::engine::{resign_and_install_blocking, ResignAndInstall, ResignRequest};

fn prompt(label: &str) -> String {
    print!("{label}: ");
    io::stdout().flush().ok();
    let mut s = String::new();
    io::stdin().lock().read_line(&mut s).unwrap_or(0);
    s.trim().to_string()
}

fn main() {
    println!("=== 재서명 ⑤ 전체 서명 (PC) — 서명된 .ipa 생성 (설치는 폰 몫) ===");
    let email = prompt("Apple ID (신뢰기기 있는 계정)");
    let password = prompt("비밀번호");
    let ipa_in = prompt("미서명 .ipa 경로 (엔터 = release/ios/Shard-unsigned.ipa)");
    let ipa = if ipa_in.is_empty() {
        "release/ios/Shard-unsigned.ipa".to_string()
    } else {
        ipa_in
    };

    let work_dir = std::env::temp_dir().join("shard-resign-sign");
    let state_dir = std::env::temp_dir().join("shard-resign-verify"); // anisette 캐시 재사용

    let params = ResignAndInstall {
        req: ResignRequest {
            email,
            password,
            bundle_id: "net.sw.shard".to_string(),
            app_name: "Shard".to_string(),
        },
        ipa: PathBuf::from(ipa),
        state_dir,
        work_dir,
        // 설치는 안 한다 — 서명만. device_addr/pairing이 없으면 서명된 .ipa 경로만 돌려준다.
        device_addr: None,
        pairing_path: None,
    };

    let tfa = || prompt("2FA 코드(기기로 온 6자리)");
    let mut log = |line: &str| println!("  · {line}");

    match resign_and_install_blocking(params, &tfa, &mut log) {
        Ok(path) => {
            println!("\n✅ 서명된 .ipa 생성 — {}", path.display());
            println!("   (이 .ipa는 당신의 실제 애플 개발 인증서+프로파일로 서명됨.)");
        }
        Err(e) => println!("\n❌ 실패 — {e:#}"),
    }
}
