// CI 시크릿 ANISETTE_URL이 바뀌면 auth.rs의 `option_env!("ANISETTE_URL")`를 다시 컴파일하도록 알린다.
// 이 값(전용 anisette 서버 주소)은 **소스·커밋에 남기지 않는다**(저장소 PUBLIC — CLAUDE.md 보안 규칙).
// GitHub 암호화 시크릿에만 있고, iOS CI(ios-app.yml)가 빌드 때 env로 주입해 앱에 기본값으로 박는다.
//
// 그리고 **iOS 타깃일 때만** 벤더된 zsign(C++) + C 심을 컴파일해 링크한다. 왜 zsign: apple-codesign은
// 중첩 프레임워크(구형 minos)에 SHA-1 주 CD를 강제 주입해 iOS 26이 0xe8008001로 거부하는데, 이걸
// 설정으로 못 끈다(memory: ios-zsign-ondevice-pivot). zsign은 번들 전체를 SHA-256 단독으로 한 패스
// 재서명해 이 범주를 없앤다(SideStore가 iOS 26에 실제로 쓰는 서명기). OpenSSL은 앱이 이미 가져오는
// OpenSSL.xcframework(krzyzanowskim) 헤더를 재활용하고, libcrypto 링크는 최종 앱(Xcode)에서 된다.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=ANISETTE_URL");
    println!("cargo:rerun-if-env-changed=ZSIGN_OPENSSL_INCLUDE");
    println!("cargo:rerun-if-changed=csrc/zsign_shim.cpp");
    println!("cargo:rerun-if-changed=vendor/zsign/src");

    // zsign은 iOS 온디바이스 서명 전용. PC/기타 타깃은 apple-codesign 경로만 쓴다(engine.rs의 cfg 분기).
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "ios" {
        return;
    }

    // OpenSSL 헤더 루트 — `<openssl/pem.h>`가 풀리도록 openssl/ 서브디렉터리를 가진 경로.
    // (krzyzanowskim OpenSSL.xcframework의 헤더는 Headers/pem.h 처럼 평면 배치라, CI가 openssl/ 심링크로
    // 감싼 디렉터리를 만들어 이 env로 넘긴다.) iOS 빌드엔 필수.
    let ossl_inc = match env::var("ZSIGN_OPENSSL_INCLUDE") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => panic!(
            "iOS 빌드엔 ZSIGN_OPENSSL_INCLUDE가 필요하다 — OpenSSL.xcframework 헤더를 openssl/ 로 감싼 \
             디렉터리 경로. ios-app.yml/resign-ios.yml가 OpenSSL fetch 후 설정한다."
        ),
    };

    let vendor = PathBuf::from("vendor/zsign/src");

    // C++ 소스: zsign.cpp(=CLI main + fork/exec install)만 제외 — 우리 심이 진입점을 대신한다.
    let mut cpp = cc::Build::new();
    cpp.cpp(true)
        .std("c++14")
        .flag_if_supported("-fvisibility=hidden")
        .include(&vendor)
        .include(vendor.join("common"))
        .include(vendor.join("third-party/zlib"))
        .include(&ossl_inc)
        .warnings(false);
    for name in [
        "archo", "bundle", "certcheck", "macho", "metadata", "openssl", "signing",
    ] {
        cpp.file(vendor.join(format!("{name}.cpp")));
    }
    for name in ["archive", "fs", "json", "log", "sha", "timer", "util"] {
        cpp.file(vendor.join("common").join(format!("{name}.cpp")));
    }
    cpp.file("csrc/zsign_shim.cpp");
    cpp.compile("zsign_cpp");

    // vendored zlib + minizip(ioapi/zip/unzip)은 C라 별도 빌드.
    let mut c = cc::Build::new();
    c.include(&vendor)
        .include(vendor.join("common"))
        .include(vendor.join("third-party/zlib"))
        .warnings(false);
    let zlib = vendor.join("third-party/zlib");
    if let Ok(rd) = std::fs::read_dir(&zlib) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("c") {
                c.file(p);
            }
        }
    }
    for name in ["ioapi", "zip", "unzip"] {
        let p = vendor.join("third-party/minizip").join(format!("{name}.c"));
        if Path::new(&p).exists() {
            c.file(p);
        }
    }
    c.compile("zsign_c");

    // C++ 표준 라이브러리(libc++)는 최종 앱(Xcode)이 링크한다 — iOS 앱은 C++/ObjC++(VLCKit)가 있어
    // 이미 libc++를 링크한다. libcrypto/libssl(OpenSSL)도 OpenSSL.xcframework로 앱에서 링크된다.
}
