//! PC에서 ②③ 흐름을 직접 돌려 빠르게 검증한다 — CI+폰 15분 루프 없이 터미널에서 즉시 반복.
//!
//! 로그인 → 팀 → 인증서(CSR 제출) → App ID → 프로파일 발급까지. 폰 빌드와 **같은 코드**
//! (`resign::engine::verify_apple_flow_blocking`)를 돈다. 설치(④)만 폰이 필요하지 ②③은 PC에서 된다.
//!
//! Apple ID/비밀번호는 stdin으로 입력받아 **애플로만**(SRP) 전송한다 — 이 프로그램은 값을 저장하지
//! 않는다. 2FA 코드도 stdin(코드는 당신 신뢰기기로 애플이 보냄).
//!
//! 실행: `cargo run -p resign --bin verify`
//! ⚠️ 비밀번호는 터미널에 그대로 보인다(빠른 검증용). 공용 화면에선 주의.

use std::io::{self, BufRead, Write};

fn prompt(label: &str) -> String {
    print!("{label}: ");
    io::stdout().flush().ok();
    let mut s = String::new();
    io::stdin().lock().read_line(&mut s).unwrap_or(0);
    s.trim().to_string()
}

fn main() {
    println!("=== 재서명 ②③ 검증 (PC) — 로그인→팀→인증서→App ID→프로파일 ===");
    let email = prompt("Apple ID (신뢰기기 있는 계정)");
    let password = prompt("비밀번호");

    // anisette·세션 캐시. 폰과 무관하게 PC에서 독립.
    let state_dir = std::env::temp_dir().join("shard-resign-verify");
    let _ = std::fs::create_dir_all(&state_dir);

    let tfa = || prompt("2FA 코드(기기로 온 6자리)");
    let mut log = |line: &str| println!("  · {line}");

    match resign::engine::verify_apple_flow_blocking(
        email,
        password,
        "net.sw.shard".to_string(),
        "Shard".to_string(),
        state_dir,
        &tfa,
        &mut log,
    ) {
        Ok(summary) => println!("\n✅ 성공 — {summary}"),
        // {:#} = 전체 에러 체인(원인까지).
        Err(e) => println!("\n❌ 실패 — {e:#}"),
    }
}
