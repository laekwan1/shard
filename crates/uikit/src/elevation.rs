//! Administrator checks. Both apps need elevation — Shard to open the WinDivert
//! driver, Veil to create the TUN adapter and firewall rules.

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// True when the current process runs with an elevated token.
pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size,
            &mut size,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// Relaunch this same executable elevated, passing `args`, and report whether
/// the prompt was accepted.
///
/// `runas` is the verb that raises the UAC prompt; `ShellExecuteW` returns a
/// value above 32 when a process was started and a small error code (the user
/// declining is `SE_ERR_ACCESSDENIED`, 5) when it was not. The caller is meant
/// to quit once this succeeds, so the elevated copy is the only one left —
/// which also frees the single-instance claim for it to take.
pub fn relaunch_elevated(args: &str) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let Ok(exe) = std::env::current_exe() else { return false };
        let mut verb: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();
        let mut file: Vec<u16> =
            exe.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        let mut params: Vec<u16> = args.encode_utf16().chain(std::iter::once(0)).collect();
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_mut_ptr(),
                file.as_mut_ptr(),
                if args.is_empty() { std::ptr::null() } else { params.as_mut_ptr() },
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // The return is an HINSTANCE by history; a value over 32 means success.
        result as isize > 32
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        false
    }
}
