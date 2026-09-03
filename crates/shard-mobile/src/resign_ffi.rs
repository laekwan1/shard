//! iOS 자체 서명 엔진의 C ABI (feature = "resign"). Swift가 재서명(+설치)을 부른다.
//!
//! 엔진 본체는 `resign` 크레이트(①②③④⑤). 여기선 C 문자열/콜백만 마샬링해 동기 진입점
//! `resign::engine::resign_and_install_blocking`를 호출한다. 반환 규약은 다른 shard_* 함수와
//! 동일: 소유 JSON C 문자열(`{"ok":true,"path":...}` 또는 `{"ok":false,"error":...}`),
//! `shard_string_free`로 해제.
//!
//! ⚠️ 실제 동작은 애플 실서버 + 실기기가 있어야 확인된다(폰). 여기까지는 링크·컴파일.

use std::ffi::{c_char, c_void, CStr, CString};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;

/// 2FA 코드를 돌려주는 콜백. 반환 문자열은 콜백이 리턴할 때까지 유효한 NUL 종단 UTF-8이면 되고,
/// Rust가 즉시 복사한다(소유권 이전 없음). null이면 빈 코드로 취급.
pub type ShardTfa = extern "C" fn(ctx: *mut c_void) -> *const c_char;
/// 진행 로그 한 줄(Swift가 화면에 표시). `line`은 이 호출 동안만 유효.
pub type ShardLog = extern "C" fn(ctx: *mut c_void, line: *const c_char);

/// 로그인 → 재서명 → (기기 정보가 있으면) 설치를, 이 스레드에서 **동기로** 실행한다.
///
/// # Safety
/// 모든 `*const c_char`는 null 또는 유효한 NUL 종단 UTF-8. `device_addr`/`pairing_path`는 null 가능
/// (그러면 설치는 건너뛰고 서명만). `tfa`/`log`는 이 스레드에서 `ctx`와 함께 호출된다.
#[no_mangle]
pub unsafe extern "C" fn shard_resign_run(
    email: *const c_char,
    password: *const c_char,
    bundle_id: *const c_char,
    app_name: *const c_char,
    ipa_path: *const c_char,
    state_dir: *const c_char,
    work_dir: *const c_char,
    device_addr: *const c_char,
    pairing_path: *const c_char,
    tfa: ShardTfa,
    log: ShardLog,
    ctx: *mut c_void,
) -> *mut c_char {
    macro_rules! required {
        ($p:expr, $name:expr) => {
            match unsafe { arg($p) } {
                Some(s) => s,
                None => return err(concat!($name, "이(가) 없습니다")),
            }
        };
    }
    let email = required!(email, "이메일");
    let password = required!(password, "비밀번호");
    let bundle_id = required!(bundle_id, "번들 ID");
    let app_name = required!(app_name, "앱 이름");
    let ipa_path = required!(ipa_path, "ipa 경로");
    let state_dir = required!(state_dir, "상태 폴더");
    let work_dir = required!(work_dir, "작업 폴더");
    let device_addr = unsafe { arg(device_addr) }.and_then(|s| IpAddr::from_str(&s).ok());
    let pairing_path = unsafe { arg(pairing_path) }.map(PathBuf::from);

    let params = resign::engine::ResignAndInstall {
        req: resign::engine::ResignRequest {
            email,
            password,
            bundle_id,
            app_name,
        },
        ipa: PathBuf::from(ipa_path),
        state_dir: PathBuf::from(state_dir),
        work_dir: PathBuf::from(work_dir),
        device_addr,
        pairing_path,
    };

    let tfa_fn = || -> String {
        let p = tfa(ctx);
        if p.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("").to_string()
        }
    };
    let mut log_fn = |line: &str| {
        if let Ok(c) = CString::new(line) {
            log(ctx, c.as_ptr());
        }
    };

    match resign::engine::resign_and_install_blocking(params, &tfa_fn, &mut log_fn) {
        Ok(path) => ok(&path.to_string_lossy()),
        Err(e) => err(&format!("{e:#}")),
    }
}

/// 첫 폰 테스트: .ipa/설치 없이 로그인→인증서→App ID→프로파일 발급(②③)만 검증한다.
/// 반환 {"ok":true,"path":"team=...; appId=...; profile=...B"} 또는 {"ok":false,"error":"..."}.
///
/// # Safety
/// 문자열 인자는 유효한 NUL 종단 UTF-8. `tfa`/`log`는 이 스레드에서 `ctx`와 함께 호출된다.
#[no_mangle]
pub unsafe extern "C" fn shard_resign_verify(
    email: *const c_char,
    password: *const c_char,
    bundle_id: *const c_char,
    app_name: *const c_char,
    state_dir: *const c_char,
    tfa: ShardTfa,
    log: ShardLog,
    ctx: *mut c_void,
) -> *mut c_char {
    macro_rules! required {
        ($p:expr, $name:expr) => {
            match unsafe { arg($p) } {
                Some(s) => s,
                None => return err(concat!($name, "이(가) 없습니다")),
            }
        };
    }
    let email = required!(email, "이메일");
    let password = required!(password, "비밀번호");
    let bundle_id = required!(bundle_id, "번들 ID");
    let app_name = required!(app_name, "앱 이름");
    let state_dir = required!(state_dir, "상태 폴더");

    let tfa_fn = || -> String {
        let p = tfa(ctx);
        if p.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("").to_string()
        }
    };
    let mut log_fn = |line: &str| {
        if let Ok(c) = CString::new(line) {
            log(ctx, c.as_ptr());
        }
    };

    match resign::engine::verify_apple_flow_blocking(
        email,
        password,
        bundle_id,
        app_name,
        PathBuf::from(state_dir),
        &tfa_fn,
        &mut log_fn,
    ) {
        Ok(summary) => ok(&summary),
        Err(e) => err(&format!("{e:#}")),
    }
}

/// ④+⑤ 자기 자신 갱신: 실행 중 앱 번들을 재서명해 자기 자신에 업그레이드 설치까지 한 흐름으로.
/// `bundle_id`는 실행 중 번들의 CFBundleIdentifier(같아야 in-place 업그레이드), `app_bundle_path`는
/// Bundle.main.bundlePath. `device_addr`(터널)+`pairing_path`가 있으면 설치까지, 없으면 서명만.
/// 반환 {"ok":true,"path":"<서명된 .ipa>"} 또는 {"ok":false,"error":"[단계] ..."}.
///
/// # Safety
/// 문자열 인자는 유효한 NUL 종단 UTF-8. device_addr/pairing_path는 NULL 가능. tfa/log는 이 스레드에서.
#[no_mangle]
pub unsafe extern "C" fn shard_resign_selfupdate(
    email: *const c_char,
    password: *const c_char,
    bundle_id: *const c_char,
    app_name: *const c_char,
    app_bundle_path: *const c_char,
    state_dir: *const c_char,
    work_dir: *const c_char,
    device_addr: *const c_char,
    pairing_path: *const c_char,
    tfa: ShardTfa,
    log: ShardLog,
    ctx: *mut c_void,
) -> *mut c_char {
    macro_rules! required {
        ($p:expr, $name:expr) => {
            match unsafe { arg($p) } {
                Some(s) => s,
                None => return err(concat!($name, "이(가) 없습니다")),
            }
        };
    }
    let email = required!(email, "이메일");
    let password = required!(password, "비밀번호");
    let bundle_id = required!(bundle_id, "번들 ID");
    let app_name = required!(app_name, "앱 이름");
    let app_bundle_path = required!(app_bundle_path, "앱 번들 경로");
    let state_dir = required!(state_dir, "상태 폴더");
    let work_dir = required!(work_dir, "작업 폴더");
    let device_addr = unsafe { arg(device_addr) }.and_then(|s| IpAddr::from_str(&s).ok());
    let pairing_path = unsafe { arg(pairing_path) }.map(PathBuf::from);

    let tfa_fn = || -> String {
        let p = tfa(ctx);
        if p.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("").to_string()
        }
    };
    let mut log_fn = |line: &str| {
        if let Ok(c) = CString::new(line) {
            log(ctx, c.as_ptr());
        }
    };

    let req = resign::engine::ResignRequest {
        email,
        password,
        bundle_id,
        app_name,
    };
    match resign::engine::resign_selfupdate_blocking(
        req,
        PathBuf::from(app_bundle_path),
        PathBuf::from(state_dir),
        PathBuf::from(work_dir),
        device_addr,
        pairing_path,
        &tfa_fn,
        &mut log_fn,
    ) {
        Ok(path) => ok(&path.to_string_lossy()),
        Err(e) => err(&format!("{e:#}")),
    }
}

/// ④ 1단계 스모크 테스트: 페어링 파일 + 터널 주소로 폰 lockdownd에 붙는지 확인.
/// 반환 {"ok":true,"path":"lockdownd 통과 — iOS …"} 또는 {"ok":false,"error":"…"}.
///
/// # Safety
/// 문자열 인자는 유효한 NUL 종단 UTF-8. `log`는 이 스레드에서 `ctx`와 함께 호출된다.
#[no_mangle]
pub unsafe extern "C" fn shard_resign_probe(
    pairing_path: *const c_char,
    addr: *const c_char,
    log: ShardLog,
    ctx: *mut c_void,
) -> *mut c_char {
    let pairing_path = match unsafe { arg(pairing_path) } {
        Some(s) => s,
        None => return err("페어링 경로가 없습니다"),
    };
    let addr_s = match unsafe { arg(addr) } {
        Some(s) => s,
        None => return err("주소가 없습니다"),
    };
    let addr = match IpAddr::from_str(&addr_s) {
        Ok(a) => a,
        Err(_) => return err("주소 형식이 잘못됨(예: 10.7.0.1)"),
    };
    let pairing = match std::fs::read(&pairing_path) {
        Ok(b) => b,
        Err(e) => return err(&format!("페어링 파일 읽기 실패: {e}")),
    };
    let mut log_fn = |line: &str| {
        if let Ok(c) = CString::new(line) {
            log(ctx, c.as_ptr());
        }
    };
    match resign::engine::probe_lockdownd_blocking(addr, pairing, &mut log_fn) {
        Ok(s) => ok(&s),
        Err(e) => err(&format!("{e:#}")),
    }
}

/// ④ RSD 스모크(iOS 17+): rppairing 터널(addr:port, 예 10.7.0.1:49152 + RP 페어링)을 세우고 터널 안
/// RSD 서비스 목록을 확인. classic lockdown(shard_resign_probe)은 iOS 26에서 죽어(QueryType RST) 대체.
/// `pairing_path`는 **RpPairingFile**(idevice_pair로 발급, classic .mobiledevicepairing 아님).
/// 반환 {"ok":true,"path":"…요약…"} 또는 {"ok":false,"error":"…"}. 서비스 목록은 log로 흐른다.
///
/// # Safety
/// `addr`/`pairing_path`는 유효한 NUL 종단 UTF-8. `log`는 이 스레드에서 `ctx`와 함께 호출된다.
#[no_mangle]
pub unsafe extern "C" fn shard_rsd_probe(
    addr: *const c_char,
    port: u16,
    pairing_path: *const c_char,
    log: ShardLog,
    ctx: *mut c_void,
) -> *mut c_char {
    let addr_s = match unsafe { arg(addr) } {
        Some(s) => s,
        None => return err("주소가 없습니다"),
    };
    let ip = match IpAddr::from_str(&addr_s) {
        Ok(a) => a,
        Err(_) => return err("주소 형식이 잘못됨(예: 10.7.0.1)"),
    };
    let pairing_path = match unsafe { arg(pairing_path) } {
        Some(s) => s,
        None => return err("페어링 경로가 없습니다"),
    };
    let pairing = match std::fs::read(&pairing_path) {
        Ok(b) => b,
        Err(e) => return err(&format!("페어링 파일 읽기 실패: {e}")),
    };
    let sockaddr = SocketAddr::new(ip, port);
    let mut log_fn = |line: &str| {
        if let Ok(c) = CString::new(line) {
            log(ctx, c.as_ptr());
        }
    };
    match resign::engine::rsd_probe_blocking(sockaddr, pairing, &mut log_fn) {
        Ok(s) => ok(&s),
        Err(e) => err(&format!("{e:#}")),
    }
}

/// ④ RSD 설치(iOS 17+): rppairing 터널 위에서 서명된 .ipa를 업로드(AFC)+설치(installation_proxy).
/// `pairing_path`는 RpPairingFile, `ipa_path`는 ⑤ 재서명이 만든 서명된 .ipa. 같은 번들ID면 in-place
/// 업그레이드(데이터 보존). 반환 {"ok":true,"path":"설치 완료…"} 또는 {"ok":false,"error":"…"}.
///
/// # Safety
/// 문자열 인자는 유효한 NUL 종단 UTF-8. `log`는 이 스레드에서 `ctx`와 함께 호출된다.
#[no_mangle]
pub unsafe extern "C" fn shard_rsd_install(
    addr: *const c_char,
    port: u16,
    pairing_path: *const c_char,
    ipa_path: *const c_char,
    log: ShardLog,
    ctx: *mut c_void,
) -> *mut c_char {
    let addr_s = match unsafe { arg(addr) } {
        Some(s) => s,
        None => return err("주소가 없습니다"),
    };
    let ip = match IpAddr::from_str(&addr_s) {
        Ok(a) => a,
        Err(_) => return err("주소 형식이 잘못됨(예: 10.7.0.1)"),
    };
    let pairing_path = match unsafe { arg(pairing_path) } {
        Some(s) => s,
        None => return err("페어링 경로가 없습니다"),
    };
    let ipa_path = match unsafe { arg(ipa_path) } {
        Some(s) => s,
        None => return err("ipa 경로가 없습니다"),
    };
    let pairing = match std::fs::read(&pairing_path) {
        Ok(b) => b,
        Err(e) => return err(&format!("페어링 파일 읽기 실패: {e}")),
    };
    let sockaddr = SocketAddr::new(ip, port);
    let mut log_fn = |line: &str| {
        if let Ok(c) = CString::new(line) {
            log(ctx, c.as_ptr());
        }
    };
    match resign::engine::rsd_install_blocking(sockaddr, pairing, PathBuf::from(ipa_path), &mut log_fn) {
        Ok(s) => ok(&s),
        Err(e) => err(&format!("{e:#}")),
    }
}

unsafe fn arg(p: *const c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(p) }.to_str().ok().map(|s| s.to_string())
    }
}

// shard_string_free(jni.rs)가 CString::from_raw로 해제하므로 into_raw로 넘겨야 한다.
fn into_raw(s: String) -> *mut c_char {
    CString::new(s)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

fn ok(path: &str) -> *mut c_char {
    into_raw(format!(r#"{{"ok":true,"path":{}}}"#, jstr(path)))
}

fn err(message: &str) -> *mut c_char {
    into_raw(format!(r#"{{"ok":false,"error":{}}}"#, jstr(message)))
}

/// 경로·에러 메시지용 최소 JSON 문자열 이스케이프(serde_json 안 끌어옴 — 다른 shard_* 규약과 동일).
fn jstr(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
