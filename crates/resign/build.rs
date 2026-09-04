// CI 시크릿 ANISETTE_URL이 바뀌면 auth.rs의 `option_env!("ANISETTE_URL")`를 다시 컴파일하도록 알린다.
// 이 값(전용 anisette 서버 주소)은 **소스·커밋에 남기지 않는다**(저장소 PUBLIC — CLAUDE.md 보안 규칙).
// GitHub 암호화 시크릿에만 있고, iOS CI(ios-app.yml)가 빌드 때 env로 주입해 앱에 기본값으로 박는다.
fn main() {
    println!("cargo:rerun-if-env-changed=ANISETTE_URL");
}
