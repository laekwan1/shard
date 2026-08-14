//! JNI entry points.
//!
//! [`crate::jni`] exposes a plain C ABI, which is what a Swift Network
//! Extension links directly. Android's `System.loadLibrary` instead looks for
//! symbols named after the Java class, so this module is a thin renaming layer
//! over the same four calls — no logic lives here.

use crate::jni as abi;
use ::jni::objects::{JClass, JString};
use ::jni::sys::{jboolean, jint, jstring};
use ::jni::JNIEnv;
use std::ffi::CString;

/// Route Rust's tracing output into logcat, so a failure on the phone is
/// diagnosable without attaching a debugger.
#[no_mangle]
pub extern "system" fn Java_net_shard_Native_initLogging(_env: JNIEnv, _class: JClass) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Debug)
            .with_tag("shard"),
    );
}

#[no_mangle]
pub extern "system" fn Java_net_shard_Native_start(
    mut env: JNIEnv,
    _class: JClass,
    config_dir: JString,
    port: jint,
) -> jint {
    // A null or unreadable path is not fatal — the engine falls back to its
    // built-in defaults rather than refusing to start.
    let dir = if config_dir.is_null() {
        None
    } else {
        env.get_string(&config_dir).ok().map(|s| s.to_string_lossy().into_owned())
    };

    let port = port.clamp(0, u16::MAX as jint) as u16;
    match dir.and_then(|d| CString::new(d).ok()) {
        Some(c) => unsafe { abi::shard_start(c.as_ptr(), port) },
        None => unsafe { abi::shard_start(std::ptr::null(), port) },
    }
}

#[no_mangle]
pub extern "system" fn Java_net_shard_Native_stop(_env: JNIEnv, _class: JClass) {
    abi::shard_stop();
}

#[no_mangle]
pub extern "system" fn Java_net_shard_Native_isRunning(_env: JNIEnv, _class: JClass) -> jboolean {
    u8::from(abi::shard_is_running() != 0)
}

#[no_mangle]
pub extern "system" fn Java_net_shard_Native_statsJson(env: JNIEnv, _class: JClass) -> jstring {
    let ptr = abi::shard_stats_json();
    let text = if ptr.is_null() {
        String::from("{}")
    } else {
        let owned = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
        unsafe { abi::shard_string_free(ptr) };
        owned
    };
    match env.new_string(text) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}
