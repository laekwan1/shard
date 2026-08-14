//! Raw WinDivert 2.2 bindings.
//!
//! Layout is transcribed from `vendor/windivert/windivert.h`. `WINDIVERT_ADDRESS`
//! is 80 bytes: an i64 timestamp, a packed u32 bitfield, a reserved u32 and a
//! 64-byte per-layer union.

#![allow(non_snake_case)]
// A complete binding surface is worth keeping even where the engine does not
// currently call every entry point.
#![allow(dead_code)]

use std::ffi::{c_char, c_void};

pub type Handle = *mut c_void;

pub const WINDIVERT_LAYER_NETWORK: u32 = 0;

pub const WINDIVERT_FLAG_SNIFF: u64 = 0x0001;
pub const WINDIVERT_FLAG_DROP: u64 = 0x0002;
pub const WINDIVERT_FLAG_RECV_ONLY: u64 = 0x0004;
pub const WINDIVERT_FLAG_SEND_ONLY: u64 = 0x0008;
pub const WINDIVERT_FLAG_NO_INSTALL: u64 = 0x0010;
pub const WINDIVERT_FLAG_FRAGMENTS: u64 = 0x0020;

pub const WINDIVERT_PARAM_QUEUE_LENGTH: u32 = 0;
pub const WINDIVERT_PARAM_QUEUE_TIME: u32 = 1;
pub const WINDIVERT_PARAM_QUEUE_SIZE: u32 = 2;

pub const WINDIVERT_SHUTDOWN_BOTH: u32 = 0x3;

/// Largest packet WinDivert will hand us.
pub const WINDIVERT_MTU_MAX: usize = 40 + 0xFFFF;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WinDivertAddress {
    pub timestamp: i64,
    /// MSVC packs these LSB-first: Layer:8, Event:8, Sniffed:1, Outbound:1,
    /// Loopback:1, Impostor:1, IPv6:1, IPChecksum:1, TCPChecksum:1,
    /// UDPChecksum:1, Reserved1:8.
    pub bitfield: u32,
    pub reserved2: u32,
    /// The per-layer union. At the NETWORK layer the first two u32s are
    /// `IfIdx` and `SubIfIdx`; we keep it opaque so the ABI stays exact.
    pub union_data: [u8; 64],
}

const _: () = assert!(std::mem::size_of::<WinDivertAddress>() == 80);

impl Default for WinDivertAddress {
    fn default() -> Self {
        Self { timestamp: 0, bitfield: 0, reserved2: 0, union_data: [0u8; 64] }
    }
}

impl WinDivertAddress {
    const OUTBOUND_BIT: u32 = 1 << 17;
    const IPV6_BIT: u32 = 1 << 20;
    const IP_CHECKSUM_BIT: u32 = 1 << 21;
    const TCP_CHECKSUM_BIT: u32 = 1 << 22;
    const UDP_CHECKSUM_BIT: u32 = 1 << 23;

    pub fn is_outbound(&self) -> bool {
        self.bitfield & Self::OUTBOUND_BIT != 0
    }

    pub fn is_ipv6(&self) -> bool {
        self.bitfield & Self::IPV6_BIT != 0
    }

    /// Clear the "checksums are already valid" hints so a subsequent
    /// `WinDivertHelperCalcChecksums` actually recomputes them.
    pub fn invalidate_checksums(&mut self) {
        self.bitfield &= !(Self::IP_CHECKSUM_BIT | Self::TCP_CHECKSUM_BIT | Self::UDP_CHECKSUM_BIT);
    }

    /// Claim the TCP checksum is already correct.
    ///
    /// Needed for the `BadSum` decoy: without this the stack — or a NIC doing
    /// checksum offload — would helpfully recompute the field and undo the
    /// corruption that makes the server drop the packet.
    pub fn mark_tcp_checksum_valid(&mut self) {
        self.bitfield |= Self::TCP_CHECKSUM_BIT;
    }
}

#[link(name = "WinDivert")]
extern "system" {
    pub fn WinDivertOpen(filter: *const c_char, layer: u32, priority: i16, flags: u64) -> Handle;

    pub fn WinDivertRecv(
        handle: Handle,
        pPacket: *mut u8,
        packetLen: u32,
        pRecvLen: *mut u32,
        pAddr: *mut WinDivertAddress,
    ) -> i32;

    pub fn WinDivertSend(
        handle: Handle,
        pPacket: *const u8,
        packetLen: u32,
        pSendLen: *mut u32,
        pAddr: *const WinDivertAddress,
    ) -> i32;

    pub fn WinDivertShutdown(handle: Handle, how: u32) -> i32;

    pub fn WinDivertClose(handle: Handle) -> i32;

    pub fn WinDivertSetParam(handle: Handle, param: u32, value: u64) -> i32;

    pub fn WinDivertHelperCalcChecksums(
        pPacket: *mut u8,
        packetLen: u32,
        pAddr: *mut WinDivertAddress,
        flags: u64,
    ) -> i32;
}
