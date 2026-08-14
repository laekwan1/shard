//! Application-layer parsing, only as deep as the desync engine needs.
//!
//! Everything here reads a single TCP segment or UDP datagram. A ClientHello
//! that spans segments is simply not recognised — acting on the first segment
//! is the whole point, and by the time the second arrives the DPI box has
//! already seen the hostname.

pub mod http;
pub mod quic;
pub mod quic_initial;
pub mod tls;

/// What we found in a packet's payload, and where the hostname sits inside it.
#[derive(Clone, Debug)]
pub struct Hostname {
    pub name: String,
    /// Offset of the hostname bytes within the payload.
    pub offset: usize,
    pub len: usize,
}

impl Hostname {
    /// Payload offset that lands in the middle of the hostname — the split
    /// point that most reliably straddles a DPI box's string match.
    pub fn midpoint(&self) -> usize {
        self.offset + self.len / 2
    }
}

/// Little bounds-checked cursor; every parser here walks untrusted bytes.
pub(crate) struct Cursor<'a> {
    pub buf: &'a [u8],
    pub at: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    pub fn u8(&mut self) -> Option<u8> {
        let v = *self.buf.get(self.at)?;
        self.at += 1;
        Some(v)
    }

    pub fn u16(&mut self) -> Option<u16> {
        let hi = *self.buf.get(self.at)? as u16;
        let lo = *self.buf.get(self.at + 1)? as u16;
        self.at += 2;
        Some((hi << 8) | lo)
    }

    pub fn u24(&mut self) -> Option<u32> {
        let a = *self.buf.get(self.at)? as u32;
        let b = *self.buf.get(self.at + 1)? as u32;
        let c = *self.buf.get(self.at + 2)? as u32;
        self.at += 3;
        Some((a << 16) | (b << 8) | c)
    }

    pub fn skip(&mut self, n: usize) -> Option<()> {
        let next = self.at.checked_add(n)?;
        if next > self.buf.len() {
            return None;
        }
        self.at = next;
        Some(())
    }

    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.buf.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

}
