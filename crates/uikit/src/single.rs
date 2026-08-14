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

/// Tell the user where the copy they already have is, and leave.
///
/// A tray program that exits silently looks broken: the icon is small and easy
/// to miss, so someone who starts it twice has usually not seen the first one.
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
