//! C ABI for the platform layer.
//!
//! Kotlin reaches this through JNI's `System.loadLibrary`; Swift would link the
//! same symbols directly. Keeping the surface to plain C types rather than a
//! JNI-specific binding means one interface serves both, and there is nothing
//! platform-shaped to keep in sync.
//!
//! Everything here is `unsafe` at the boundary and safe behind it: pointers are
//! validated once on the way in, and no Rust type ever crosses.

use crate::{http_proxy, Engine, Policy};
use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// The running engine. One per process — a phone has one tunnel.
static ENGINE: OnceLock<Mutex<Option<Running>>> = OnceLock::new();

struct Running {
    engine: Arc<Engine>,
    stop: Arc<AtomicBool>,
    port: u16,
}

fn slot() -> &'static Mutex<Option<Running>> {
    ENGINE.get_or_init(|| Mutex::new(None))
}

/// Start the local proxy. Returns the bound port, or a negative error code.
///
/// `config_dir` is where the settings file lives, so the phone and desktop
/// builds read the same format. Pass null to use built-in defaults.
///
/// # Safety
/// `config_dir` must be null or a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn shard_start(config_dir: *const c_char, port: u16) -> c_int {
    let mut guard = match slot().lock() {
        Ok(g) => g,
        Err(_) => return -1,
    };
    if guard.is_some() {
        return -2; // already running
    }

    if !config_dir.is_null() {
        match unsafe { CStr::from_ptr(config_dir) }.to_str() {
            // The config module reads %APPDATA% on Windows and this variable
            // elsewhere, so one path setting covers both.
            Ok(dir) => std::env::set_var("APPDATA", dir),
            Err(_) => return -3,
        }
    }

    let engine = Arc::new(Engine::new(Policy::from_config()));
    // An HTTP proxy rather than SOCKS: WebView can be pointed at one for this
    // app alone, which is what removes the need for a VPN interface entirely.
    let server = match http_proxy::Server::bind(port) {
        Ok(s) => s,
        Err(_) => return -4,
    };
    let bound = server.port;
    let stop = server.stop_handle();

    let engine_for_thread = engine.clone();
    std::thread::Builder::new()
        .name("shard-proxy".to_string())
        .spawn(move || {
            server.run(engine_for_thread, |result| {
                if let Err(e) = result {
                    tracing::debug!("connection failed: {e}");
                }
            });
        })
        .ok();

    *guard = Some(Running { engine, stop, port: bound });
    bound as c_int
}

/// Stop the proxy. Safe to call when nothing is running.
#[no_mangle]
pub extern "C" fn shard_stop() {
    let Ok(mut guard) = slot().lock() else { return };
    if let Some(running) = guard.take() {
        running.stop.store(false, Ordering::SeqCst);
        // Unblock the accept loop by connecting to it once.
        let _ = std::net::TcpStream::connect(("127.0.0.1", running.port));
    }
}

#[no_mangle]
pub extern "C" fn shard_is_running() -> c_int {
    slot().lock().map(|g| g.is_some() as c_int).unwrap_or(0)
}

/// Counters as a JSON object, for the UI to render however it likes.
///
/// Returns an owned C string; the caller must release it with
/// [`shard_string_free`].
#[no_mangle]
pub extern "C" fn shard_stats_json() -> *mut c_char {
    let stats = slot()
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|r| r.engine.stats.snapshot()))
        .unwrap_or_default();

    let json = format!(
        r#"{{"connections":{},"desynced":{},"passedThrough":{},"failed":{},"bytesUp":{},"bytesDown":{}}}"#,
        stats.connections,
        stats.desynced,
        stats.passed_through,
        stats.failed,
        stats.bytes_up,
        stats.bytes_down
    );
    into_c_string(json)
}

/// Release a string returned by this library.
///
/// # Safety
/// `ptr` must be null or a pointer this library returned and has not freed.
#[no_mangle]
pub unsafe extern "C" fn shard_string_free(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(unsafe { CString::from_raw(ptr) });
    }
}

fn into_c_string(text: String) -> *mut c_char {
    // A NUL can only appear if a hostname contained one, which the parsers
    // already reject; fall back rather than panic across the boundary.
    CString::new(text)
        .unwrap_or_else(|_| CString::new("{}").expect("literal has no NUL"))
        .into_raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_are_valid_json_even_when_stopped() {
        let ptr = shard_stats_json();
        assert!(!ptr.is_null());
        let text = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_string();
        unsafe { shard_string_free(ptr) };

        assert!(text.starts_with('{') && text.ends_with('}'), "got: {text}");
        for key in ["connections", "desynced", "passedThrough", "failed", "bytesUp", "bytesDown"] {
            assert!(text.contains(key), "{key} missing from {text}");
        }
    }

    #[test]
    fn freeing_null_is_a_no_op() {
        unsafe { shard_string_free(std::ptr::null_mut()) };
    }

    #[test]
    fn start_stop_round_trips_and_refuses_double_start() {
        assert_eq!(shard_is_running(), 0);

        // Port 0 asks the OS for a free one.
        let port = unsafe { shard_start(std::ptr::null(), 0) };
        assert!(port > 0, "start returned {port}");
        assert_eq!(shard_is_running(), 1);

        // A second start must not replace the first silently.
        assert_eq!(unsafe { shard_start(std::ptr::null(), 0) }, -2);

        shard_stop();
        assert_eq!(shard_is_running(), 0);
        // Stopping twice is harmless.
        shard_stop();
    }
}
