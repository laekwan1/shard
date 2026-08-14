//! Safe wrapper over the WinDivert driver handle.

pub mod ffi;

use anyhow::{bail, Result};
use ffi::{Handle, WinDivertAddress};
use std::ffi::CString;

const INVALID_HANDLE: isize = -1;

/// An open WinDivert handle at the NETWORK layer.
///
/// `recv` blocks; call `shutdown` from another thread to unblock it, after
/// which `recv` returns `Ok(None)`.
pub struct Diverter {
    handle: Handle,
}

// The driver handle is usable from any thread and WinDivert serialises its own
// I/O, so sharing one handle between the engine threads is sound.
unsafe impl Send for Diverter {}
unsafe impl Sync for Diverter {}

impl Diverter {
    pub fn open(filter: &str, priority: i16, flags: u64) -> Result<Self> {
        let c_filter = CString::new(filter)?;
        let handle = unsafe {
            ffi::WinDivertOpen(c_filter.as_ptr(), ffi::WINDIVERT_LAYER_NETWORK, priority, flags)
        };
        if handle as isize == INVALID_HANDLE {
            bail!(open_error(std::io::Error::last_os_error()));
        }
        Ok(Self { handle })
    }

    /// Receive one packet. `Ok(None)` means the handle was shut down.
    pub fn recv(&self, buf: &mut [u8], addr: &mut WinDivertAddress) -> Result<Option<usize>> {
        let mut len: u32 = 0;
        let ok = unsafe {
            ffi::WinDivertRecv(self.handle, buf.as_mut_ptr(), buf.len() as u32, &mut len, addr)
        };
        if ok == 0 {
            let err = std::io::Error::last_os_error();
            // ERROR_NO_DATA after shutdown, ERROR_OPERATION_ABORTED on close.
            return match err.raw_os_error() {
                Some(232) | Some(995) => Ok(None),
                _ => Err(anyhow::anyhow!("WinDivertRecv failed: {err}")),
            };
        }
        Ok(Some(len as usize))
    }

    /// Inject a packet. Used both to re-emit the original and to send crafted
    /// fragments and decoys.
    pub fn send(&self, packet: &[u8], addr: &WinDivertAddress) -> Result<usize> {
        let mut sent: u32 = 0;
        let ok = unsafe {
            ffi::WinDivertSend(self.handle, packet.as_ptr(), packet.len() as u32, &mut sent, addr)
        };
        if ok == 0 {
            bail!("WinDivertSend failed: {}", std::io::Error::last_os_error());
        }
        Ok(sent as usize)
    }

    pub fn set_param(&self, param: u32, value: u64) -> Result<()> {
        if unsafe { ffi::WinDivertSetParam(self.handle, param, value) } == 0 {
            bail!("WinDivertSetParam({param}) failed: {}", std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// Unblock a pending `recv` so the engine thread can exit.
    pub fn shutdown(&self) {
        unsafe { ffi::WinDivertShutdown(self.handle, ffi::WINDIVERT_SHUTDOWN_BOTH) };
    }
}

impl Drop for Diverter {
    fn drop(&mut self) {
        unsafe { ffi::WinDivertClose(self.handle) };
    }
}

/// Recompute IP and L4 checksums in place after we have rewritten a packet.
pub fn recalc_checksums(packet: &mut [u8], addr: &mut WinDivertAddress) -> bool {
    addr.invalidate_checksums();
    unsafe {
        ffi::WinDivertHelperCalcChecksums(packet.as_mut_ptr(), packet.len() as u32, addr, 0) != 0
    }
}

/// WinDivertOpen's failure modes are opaque; the raw code is what actually
/// tells the user what to fix.
fn open_error(err: std::io::Error) -> String {
    let hint = match err.raw_os_error() {
        Some(2) => "WinDivert.dll / WinDivert64.sys 가 실행 파일과 같은 폴더에 있어야 합니다",
        Some(5) => "관리자 권한으로 실행해야 합니다",
        Some(87) => "필터 문법이 올바르지 않습니다",
        Some(577) => "드라이버 서명 검증에 실패했습니다 (Secure Boot / 테스트 서명 설정 확인)",
        Some(1060) => "WinDivert 서비스가 설치되지 않았습니다",
        Some(1275) => "드라이버가 정책에 의해 차단되었습니다",
        Some(1753) | Some(1058) => "이전 WinDivert 인스턴스가 남아 있습니다. 재부팅 후 다시 시도하세요",
        _ => "알 수 없는 오류",
    };
    format!("WinDivert 드라이버를 열 수 없습니다: {err} — {hint}")
}
