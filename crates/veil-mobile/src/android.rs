//! JNI entry points.
//!
//! A thin renaming layer over [`crate`] — no logic lives here, so nothing can
//! behave differently on Android than it does under test.

use ::jni::objects::{JClass, JString};
use ::jni::sys::{jboolean, jint, jstring};
use ::jni::JNIEnv;

/// Route tracing into logcat, so a failure on the phone is diagnosable.
#[no_mangle]
pub extern "system" fn Java_net_veil_Native_initLogging(_env: JNIEnv, _class: JClass) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Debug)
            .with_tag("veil"),
    );
}

/// Bound port, or a negative code: -1 bad link, -2 already running, -3 other.
#[no_mangle]
pub extern "system" fn Java_net_veil_Native_start(
    mut env: JNIEnv,
    _class: JClass,
    link: JString,
    port: jint,
) -> jint {
    let Ok(text) = env.get_string(&link) else { return -1 };
    let text = text.to_string_lossy().into_owned();
    let port = port.clamp(0, u16::MAX as jint) as u16;

    match crate::start(&text, port) {
        Ok(bound) => bound as jint,
        Err(e) => {
            log::error!("start failed: {e:#}");
            if crate::is_running() {
                -2
            } else if crate::check_link(&text).is_err() {
                -1
            } else {
                -3
            }
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_net_veil_Native_stop(_env: JNIEnv, _class: JClass) {
    crate::stop();
}

#[no_mangle]
pub extern "system" fn Java_net_veil_Native_isRunning(_env: JNIEnv, _class: JClass) -> jboolean {
    u8::from(crate::is_running())
}

/// Counters as JSON, for the UI to render however it likes.
#[no_mangle]
pub extern "system" fn Java_net_veil_Native_statsJson(env: JNIEnv, _class: JClass) -> jstring {
    let s = crate::stats();
    let json = format!(
        r#"{{"connections":{},"tunnelled":{},"direct":{},"failed":{},"bytesUp":{},"bytesDown":{}}}"#,
        s.connections, s.tunnelled, s.direct, s.failed, s.bytes_up, s.bytes_down
    );
    match env.new_string(json) {
        Ok(v) => v.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// A human-readable description of a link, or `error: …` explaining why not.
#[no_mangle]
pub extern "system" fn Java_net_veil_Native_describeLink(
    mut env: JNIEnv,
    _class: JClass,
    link: JString,
) -> jstring {
    let text = env
        .get_string(&link)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let result = match crate::check_link(&text) {
        Ok(description) => description,
        // The chain carries the reason, which is what the user needs to fix.
        Err(e) => format!("error: {e:#}"),
    };
    match env.new_string(result) {
        Ok(v) => v.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}
