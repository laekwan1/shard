//! C ABI for saving a video the page is playing.
//!
//! The desktop reaches [`shard::download::save`] from its own browser window;
//! the phone reaches the same functions from here. iOS especially benefits:
//! WKWebView cannot intercept sub-requests, so a page script captures the media
//! URL and hands it down, and the fetch-and-mux that turns that URL into a file
//! is this crate's — the exact code the desktop uses, not a Swift rewrite.
//!
//! Two entry points cover most sites: [`shard_download_hls`] for segmented
//! streams (`.m3u8`) and [`shard_download_direct`] for a plain file URL.
//! YouTube's SABR path is deliberately left out for now — it needs the whole
//! captured request, not just a URL, and is a larger surface to bridge.
//!
//! Each call blocks until the download finishes; the caller runs it off the UI
//! thread. Progress and cancellation cross the boundary as C function pointers
//! so the platform can drive a progress bar and a stop button without Rust
//! knowing anything about either.

use shard::download::save;
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;

/// Called as bytes land: `(ctx, downloaded, total)`. `total` is 0 when the
/// length is not known ahead of time (a live playlist, a chunked response).
pub type ProgressCb = extern "C" fn(ctx: *mut c_void, done: u64, total: u64);

/// Polled between chunks: return non-zero to abort. `ctx` is the same pointer
/// passed to the download call.
pub type CancelCb = extern "C" fn(ctx: *mut c_void) -> c_int;

use std::ffi::c_int;

/// Save a segmented (HLS) stream to a file.
///
/// Returns an owned JSON string the caller must release with
/// [`shard_string_free`]: `{"ok":true,"path":"..."}` on success, or
/// `{"ok":false,"error":"..."}` with a Korean message on failure.
///
/// # Safety
/// `manifest_url`, `into_dir` and `title` must be valid NUL-terminated UTF-8.
/// `referer` and `title` may be null (treated as empty). `progress`/`cancel`
/// may be null; when non-null they are called on this thread with `ctx`.
#[no_mangle]
pub unsafe extern "C" fn shard_download_hls(
    manifest_url: *const c_char,
    referer: *const c_char,
    into_dir: *const c_char,
    title: *const c_char,
    progress: Option<ProgressCb>,
    cancel: Option<CancelCb>,
    ctx: *mut c_void,
) -> *mut c_char {
    let Some(url) = (unsafe { str_arg(manifest_url) }) else {
        return result_err("주소를 읽지 못했습니다");
    };
    let referer = unsafe { str_arg(referer) }.unwrap_or_default();
    let Some(dir) = (unsafe { str_arg(into_dir) }) else {
        return result_err("저장 폴더를 읽지 못했습니다");
    };
    let title = unsafe { str_arg(title) }.unwrap_or_else(|| "video".to_string());

    let holder = CallbackHolder { progress, cancel, ctx };
    let mut on_progress = |done: u64, total: u64| holder.progress(done, total);
    let cancelled = || holder.cancelled();

    match save::run_hls(&url, &referer, Path::new(&dir), &title, &mut on_progress, &cancelled) {
        Ok(path) => result_ok(&path.to_string_lossy()),
        Err(e) => result_err(&e.to_string()),
    }
}

/// Quality rows for a captured YouTube offer.
///
/// `offer_json` is the `ytInitialPlayerResponse` the page script captured.
/// Returns an owned JSON string the caller frees with `shard_string_free`:
/// `{"ok":true,"rows":[{"itag":N,"label":"...","detail":"..."}]}` or
/// `{"ok":false,"error":"..."}`. The itag `4294967295` (u32::MAX) is the
/// audio-only row.
///
/// # Safety
/// `offer_json` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn shard_youtube_qualities(offer_json: *const c_char) -> *mut c_char {
    let Some(json) = (unsafe { str_arg(offer_json) }) else {
        return result_err("영상 정보를 읽지 못했습니다");
    };
    match save::youtube_qualities(&json) {
        Ok(rows) => {
            let mut items = String::new();
            for (i, (itag, label, detail)) in rows.iter().enumerate() {
                if i > 0 {
                    items.push(',');
                }
                items.push_str(&format!(
                    r#"{{"itag":{},"label":{},"detail":{}}}"#,
                    itag,
                    json_string(label),
                    json_string(detail)
                ));
            }
            into_c_string(format!(r#"{{"ok":true,"rows":[{}]}}"#, items))
        }
        Err(e) => result_err(&e.to_string()),
    }
}

/// Download a YouTube video (or its audio alone) from a captured offer.
///
/// `itag` names the wanted video format, or `4294967295` for audio only. Same
/// return contract, progress/cancel callbacks, and safety notes as
/// [`shard_download_hls`].
///
/// # Safety
/// See [`shard_download_hls`]. `offer_json` and `into_dir` must be valid
/// NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn shard_download_youtube(
    offer_json: *const c_char,
    itag: u32,
    into_dir: *const c_char,
    progress: Option<ProgressCb>,
    cancel: Option<CancelCb>,
    ctx: *mut c_void,
) -> *mut c_char {
    let Some(json) = (unsafe { str_arg(offer_json) }) else {
        return result_err("영상 정보를 읽지 못했습니다");
    };
    let Some(dir) = (unsafe { str_arg(into_dir) }) else {
        return result_err("저장 폴더를 읽지 못했습니다");
    };

    let holder = CallbackHolder { progress, cancel, ctx };
    let mut on_progress = |done: u64, total: u64| holder.progress(done, total);
    let cancelled = || holder.cancelled();

    match save::run_youtube(&json, itag, Path::new(&dir), &mut on_progress, &cancelled) {
        Ok(path) => result_ok(&path.to_string_lossy()),
        Err(e) => result_err(&e.to_string()),
    }
}

/// Save a plain file URL (a direct `<video src>` or a progressive MP4).
///
/// Same return contract and safety notes as [`shard_download_hls`].
///
/// # Safety
/// See [`shard_download_hls`].
#[no_mangle]
pub unsafe extern "C" fn shard_download_direct(
    url: *const c_char,
    referer: *const c_char,
    into_dir: *const c_char,
    title: *const c_char,
    progress: Option<ProgressCb>,
    cancel: Option<CancelCb>,
    ctx: *mut c_void,
) -> *mut c_char {
    let Some(url) = (unsafe { str_arg(url) }) else {
        return result_err("주소를 읽지 못했습니다");
    };
    let referer = unsafe { str_arg(referer) }.unwrap_or_default();
    let Some(dir) = (unsafe { str_arg(into_dir) }) else {
        return result_err("저장 폴더를 읽지 못했습니다");
    };
    let title = unsafe { str_arg(title) }.unwrap_or_else(|| "video".to_string());

    let holder = CallbackHolder { progress, cancel, ctx };
    let mut on_progress = |done: u64, total: u64| holder.progress(done, total);
    let cancelled = || holder.cancelled();

    match save::run_direct(&url, &referer, Path::new(&dir), &title, &mut on_progress, &cancelled) {
        Ok(path) => result_ok(&path.to_string_lossy()),
        Err(e) => result_err(&e.to_string()),
    }
}

// Strings returned here are released with `shard_string_free`, defined once in
// `jni.rs` — it frees any CString this library hands out, so there is one free
// for the whole C ABI rather than one per module.

/// The C callbacks plus their context, gathered so the two closures the save
/// functions want can both reach them. Not sent across threads — the save call
/// blocks on the caller's thread and invokes these there.
struct CallbackHolder {
    progress: Option<ProgressCb>,
    cancel: Option<CancelCb>,
    ctx: *mut c_void,
}

impl CallbackHolder {
    fn progress(&self, done: u64, total: u64) {
        if let Some(cb) = self.progress {
            cb(self.ctx, done, total);
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.map(|cb| cb(self.ctx) != 0).unwrap_or(false)
    }
}

/// Read a C string argument into an owned `String`, or `None` if null/invalid.
///
/// # Safety
/// `ptr` must be null or a valid NUL-terminated UTF-8 string.
unsafe fn str_arg(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().ok().map(|s| s.to_string())
}

fn result_ok(path: &str) -> *mut c_char {
    into_c_string(format!(r#"{{"ok":true,"path":{}}}"#, json_string(path)))
}

fn result_err(message: &str) -> *mut c_char {
    into_c_string(format!(r#"{{"ok":false,"error":{}}}"#, json_string(message)))
}

/// Minimal JSON string escaping — enough for a file path or an error message,
/// which is all that ever passes through here. Avoids pulling serde_json in for
/// two fields.
fn json_string(s: &str) -> String {
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

fn into_c_string(text: String) -> *mut c_char {
    CString::new(text)
        .unwrap_or_else(|_| CString::new(r#"{"ok":false,"error":"내부 오류"}"#).expect("no NUL"))
        .into_raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_with_quotes_and_backslashes_stays_valid_json() {
        // A Windows path or an odd title must not break the JSON the app parses.
        let json = json_string(r#"C:\a\"b".mp4"#);
        assert_eq!(json, r#""C:\\a\\\"b\".mp4""#);
    }

    #[test]
    fn an_error_result_is_shaped_for_the_caller() {
        let ptr = result_err("화질을 찾지 못했습니다");
        let text = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_string();
        unsafe { crate::jni::shard_string_free(ptr) };
        assert!(text.starts_with(r#"{"ok":false,"error":"#), "got: {text}");
        assert!(text.contains("화질"), "got: {text}");
    }
}
