//! One copy at a time.
//!
//! Both apps take over something the machine has only one of. Shard opens the
//! packet driver and binds a DNS listener; Veil creates a network adapter and
//! firewall rules. A second copy does not double the effect — it fights the
//! first. Two Shards handle the same packet twice, so a connection gets two
//! decoys and two splits and may fail outright, and the second one's DNS
//! listener cannot bind at all.
//!
//! Nothing prevented that, so starting the program twice quietly gave a machine
//! two engines. This makes the second copy notice and step aside.

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;

/// Held for the life of the process; dropping it lets the next copy start.
pub struct Claim(HANDLE);

// Safety: a mutex handle is a plain kernel handle with no thread affinity.
unsafe impl Send for Claim {}

impl Drop for Claim {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// Claim the machine for `app`, or report that someone else already has it.
///
/// The name is prefixed `Global\` so the claim spans sessions: the program runs
/// elevated, and an elevated copy and a normal one would otherwise each think
/// they were alone.
pub fn claim(app: &str) -> Option<Claim> {
    let mut name: Vec<u16> = format!("Global\\{app}-single-instance").encode_utf16().collect();
    name.push(0);

    let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    if handle.is_null() {
        // The claim could not be made at all — which is not evidence that
        // another copy is running, so this does not stop the program.
        return Some(Claim(std::ptr::null_mut()));
    }
    if unsafe { windows_sys::Win32::Foundation::GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe { CloseHandle(handle) };
        return None;
    }
    Some(Claim(handle))
}

/// The window message the first copy watches for, so a second copy can ask it
/// to come back to the front.
///
/// `RegisterWindowMessageW` returns the same value in every process for a given
/// string, so the two copies agree on the number without sharing any state. The
/// string carries the app name so Shard and Veil do not wake each other.
#[cfg(windows)]
pub fn wake_message(app: &str) -> u32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW;
    let mut name: Vec<u16> = format!("{app}-wake-single-instance").encode_utf16().collect();
    name.push(0);
    unsafe { RegisterWindowMessageW(name.as_ptr()) }
}

/// Bring the copy they already have to the front, then leave.
///
/// A tray program that exits silently looks broken, and a message box that only
/// says "already running" makes the user hunt the tray for the window. Instead
/// the first copy is told, through a message every top-level window on the
/// desktop receives, to show itself — so double-clicking the icon a second time
/// does the natural thing and raises the window that is already there.
pub fn wake_the_running_copy(app: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, HWND_BROADCAST};
        // Broadcast, not a targeted send: the first copy's window may be hidden
        // in the tray, and finding it would mean sharing its class name here.
        // The message id is app-specific, so only the running Shard answers.
        let msg = wake_message(app);
        unsafe { PostMessageW(HWND_BROADCAST, msg, 0, 0) };
    }
    #[cfg(not(windows))]
    let _ = app;
}

/// Tell the user where the copy they already have is, and leave.
///
/// The fallback for a window that does not answer [`wake_message`] (Veil's does
/// not yet): a tray program that exits silently looks broken, so at least say
/// why nothing opened.
pub fn point_at_the_running_copy(app: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
        let text = format!("{app}는 이미 실행 중입니다.\n작업 표시줄 오른쪽 트레이 아이콘을 확인하세요.");
        let mut text: Vec<u16> = text.encode_utf16().collect();
        text.push(0);
        let mut title: Vec<u16> = app.encode_utf16().collect();
        title.push(0);
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONINFORMATION,
            )
        };
    }
    #[cfg(not(windows))]
    let _ = app;
}
