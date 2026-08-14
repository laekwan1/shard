//! Reading the hostname out of a QUIC Initial packet.
//!
//! Over TCP the ClientHello arrives in the clear and the hostname is simply
//! there. QUIC encrypts its first packet too — but under keys derived from the
//! connection ID that is printed in the same packet's header, which is public
//! by construction. Anyone who sees the packet can derive the keys. The
//! encryption is there to stop middleboxes *changing* the handshake, not to
//! hide it.
//!
//! So the hostname is readable, and reading it is what lets a QUIC connection
//! be judged by domain the way a TCP one is. Without this the only choices are
//! to let all QUIC through — losing the policy — or to block it and make the
//! browser fall back to TCP, which costs a round trip on every first
//! connection.
//!
//! Nothing here decrypts anything the sender meant to keep private: the keys
//! come from the packet itself, and only the client's Initial packet is
//! readable this way.

use crate::parse::{tls, Hostname};
use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes128Gcm, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;

/// QUIC v1 (RFC 9001 §5.2).
const SALT_V1: [u8; 20] = [
    0x38, 0x76, 0x2c, 0xf7, 0xf5, 0x59, 0x34, 0xb3, 0x4d, 0x17, 0x9a, 0xe6, 0xa4, 0xc8, 0x0c, 0xad,
    0xcc, 0xbb, 0x7f, 0x0a,
];

/// QUIC v2 (RFC 9369 §3.3.1).
const SALT_V2: [u8; 20] = [
    0x0d, 0xed, 0xe3, 0xde, 0xf7, 0x00, 0xa6, 0xdb, 0x81, 0x93, 0x81, 0xbe, 0x6e, 0x26, 0x9d, 0xcb,
    0xf9, 0xbd, 0x2e, 0xd9,
];

const VERSION_1: u32 = 0x0000_0001;
const VERSION_2: u32 = 0x6b33_43cf;

/// v2 renames the labels so that a v1 reader cannot decrypt a v2 packet.
struct Labels {
    key: &'static str,
    iv: &'static str,
    hp: &'static str,
}

const LABELS_V1: Labels = Labels { key: "quic key", iv: "quic iv", hp: "quic hp" };
const LABELS_V2: Labels = Labels { key: "quicv2 key", iv: "quicv2 iv", hp: "quicv2 hp" };

/// The hostname in a client's Initial packet, if there is one to be had.
///
/// Returns `None` for anything that is not a client Initial, for a packet whose
/// authentication fails — which includes every server packet and every retry —
/// and for a ClientHello that this datagram does not carry whole.
pub fn sni(datagram: &[u8]) -> Option<Hostname> {
    let packet = Initial::parse(datagram)?;
    let keys = Keys::derive(packet.version, packet.dcid)?;
    let plaintext = packet.open(&keys)?;
    let crypto = crypto_stream(&plaintext)?;
    // Offsets are meaningless to the caller here: the hostname sits inside an
    // encrypted, reassembled stream, not at a position in the datagram. What
    // matters is the name, and a QUIC packet is never split on it.
    tls::handshake_sni(&crypto)
}

/// A client Initial packet, located but not yet opened.
struct Initial<'a> {
    version: u32,
    dcid: &'a [u8],
    /// The whole packet: header protection is undone against this.
    packet: &'a [u8],
    /// Where the packet number starts — the header ends here.
    number_at: usize,
    /// End of this packet within the datagram.
    end: usize,
}

impl<'a> Initial<'a> {
    fn parse(datagram: &'a [u8]) -> Option<Self> {
        // Long header, fixed bit set, Initial type.
        let first = *datagram.first()?;
        if first & 0xc0 != 0xc0 {
            return None;
        }
        let version = u32::from_be_bytes(datagram.get(1..5)?.try_into().ok()?);
        let type_bits = (first & 0x30) >> 4;
        let is_initial = match version {
            VERSION_1 => type_bits == 0,
            // v2 renumbers the packet types; Initial became 1.
            VERSION_2 => type_bits == 1,
            _ => false,
        };
        if !is_initial {
            return None;
        }

        let mut at = 5;
        let dcid_len = *datagram.get(at)? as usize;
        at += 1;
        if dcid_len > 20 {
            return None;
        }
        let dcid = datagram.get(at..at + dcid_len)?;
        at += dcid_len;

        let scid_len = *datagram.get(at)? as usize;
        at += 1;
        if scid_len > 20 {
            return None;
        }
        at += scid_len;

        // Token, which a first-flight Initial usually leaves empty.
        let (token_len, used) = varint(datagram, at)?;
        at = used + token_len as usize;

        let (length, used) = varint(datagram, at)?;
        let number_at = used;
        let end = number_at.checked_add(length as usize)?;
        if end > datagram.len() {
            return None;
        }

        Some(Self { version, dcid, packet: datagram, number_at, end })
    }

    /// Undo header protection, then the AEAD, returning the frames.
    fn open(&self, keys: &Keys) -> Option<Vec<u8>> {
        // The mask comes from ciphertext far enough past the packet number that
        // it is the same whatever the number's length turns out to be.
        let sample_at = self.number_at + 4;
        let sample = self.packet.get(sample_at..sample_at + 16)?;
        let mask = keys.mask(sample);

        let first = self.packet[0] ^ (mask[0] & 0x0f);
        let number_len = (first & 0x03) as usize + 1;

        let mut number_bytes = [0u8; 4];
        let raw = self.packet.get(self.number_at..self.number_at + number_len)?;
        for (i, byte) in raw.iter().enumerate() {
            number_bytes[i] = byte ^ mask[1 + i];
        }
        let mut number = 0u64;
        for byte in &number_bytes[..number_len] {
            number = (number << 8) | *byte as u64;
        }

        // The header is authenticated as sent, with protection removed.
        let mut header = self.packet.get(..self.number_at + number_len)?.to_vec();
        header[0] = first;
        for i in 0..number_len {
            header[self.number_at + i] = number_bytes[i];
        }

        let body = self.packet.get(self.number_at + number_len..self.end)?;
        keys.open(number, &header, body)
    }
}

/// The client's Initial secrets, derived from what the header already says.
struct Keys {
    key: [u8; 16],
    iv: [u8; 12],
    hp: [u8; 16],
}

impl Keys {
    fn derive(version: u32, dcid: &[u8]) -> Option<Self> {
        let (salt, labels) = match version {
            VERSION_1 => (SALT_V1, LABELS_V1),
            VERSION_2 => (SALT_V2, LABELS_V2),
            _ => return None,
        };
        let initial = Hkdf::<Sha256>::new(Some(&salt), dcid);
        let mut client = [0u8; 32];
        expand(&initial, "client in", &mut client)?;

        let client = Hkdf::<Sha256>::from_prk(&client).ok()?;
        let mut keys = Self { key: [0; 16], iv: [0; 12], hp: [0; 16] };
        expand(&client, labels.key, &mut keys.key)?;
        expand(&client, labels.iv, &mut keys.iv)?;
        expand(&client, labels.hp, &mut keys.hp)?;
        Some(keys)
    }

    /// The header-protection mask: one AES block over a ciphertext sample.
    fn mask(&self, sample: &[u8]) -> [u8; 16] {
        let cipher = Aes128::new(&self.hp.into());
        let mut block = [0u8; 16];
        block.copy_from_slice(sample);
        cipher.encrypt_block((&mut block).into());
        block
    }

    fn open(&self, number: u64, header: &[u8], body: &[u8]) -> Option<Vec<u8>> {
        // Nonce is the IV with the packet number exclusive-ored into its tail.
        let mut nonce = self.iv;
        for (i, byte) in number.to_be_bytes().iter().enumerate() {
            nonce[4 + i] ^= byte;
        }
        let cipher = Aes128Gcm::new(Key::<Aes128Gcm>::from_slice(&self.key));
        cipher
            .decrypt(Nonce::from_slice(&nonce), aes_gcm::aead::Payload { msg: body, aad: header })
            .ok()
    }
}

/// HKDF-Expand-Label, which QUIC borrows from TLS 1.3 unchanged.
fn expand<T: hkdf::HmacImpl<Sha256>>(
    from: &Hkdf<Sha256, T>,
    label: &str,
    out: &mut [u8],
) -> Option<()> {
    let mut info = Vec::with_capacity(4 + label.len() + 6);
    info.extend_from_slice(&(out.len() as u16).to_be_bytes());
    info.push((label.len() + 6) as u8);
    info.extend_from_slice(b"tls13 ");
    info.extend_from_slice(label.as_bytes());
    info.push(0); // empty context
    from.expand(&info, out).ok()
}

/// Reassemble the CRYPTO frames into the handshake bytes they carry.
///
/// A ClientHello that does not fit in one packet is spread across several, and
/// the parts can arrive with gaps. Only the run starting at offset zero is
/// usable — `server_name` sits early enough that it is almost always inside it,
/// and waiting for the rest would mean deciding after the handshake is gone.
fn crypto_stream(frames: &[u8]) -> Option<Vec<u8>> {
    let mut pieces: Vec<(u64, &[u8])> = Vec::new();
    let mut at = 0usize;

    while at < frames.len() {
        let frame_type = *frames.get(at)?;
        at += 1;
        match frame_type {
            // PADDING and PING carry nothing; Initial packets are mostly padding.
            0x00 | 0x01 => continue,
            // CRYPTO
            0x06 => {
                let (offset, used) = varint(frames, at)?;
                let (length, used) = varint(frames, used)?;
                let end = used.checked_add(length as usize)?;
                pieces.push((offset, frames.get(used..end)?));
                at = end;
            }
            // ACK, with or without ECN counts.
            0x02 | 0x03 => {
                let (_largest, used) = varint(frames, at)?;
                let (_delay, used) = varint(frames, used)?;
                let (ranges, used) = varint(frames, used)?;
                let (_first, mut used) = varint(frames, used)?;
                for _ in 0..ranges {
                    let (_gap, next) = varint(frames, used)?;
                    let (_len, next) = varint(frames, next)?;
                    used = next;
                }
                if frame_type == 0x03 {
                    for _ in 0..3 {
                        let (_count, next) = varint(frames, used)?;
                        used = next;
                    }
                }
                at = used;
            }
            // Anything else has no place in an Initial packet, and guessing at
            // its length would mean reading the rest of the frames as garbage.
            _ => break,
        }
    }

    pieces.sort_by_key(|(offset, _)| *offset);
    let mut out: Vec<u8> = Vec::new();
    for (offset, bytes) in pieces {
        if offset > out.len() as u64 {
            break; // a gap: everything past it belongs somewhere unknown
        }
        let skip = (out.len() as u64 - offset) as usize;
        if skip < bytes.len() {
            out.extend_from_slice(&bytes[skip..]);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// QUIC's variable-length integer. Returns the value and the offset past it.
fn varint(buf: &[u8], at: usize) -> Option<(u64, usize)> {
    let first = *buf.get(at)?;
    let len = 1usize << (first >> 6);
    let mut value = (first & 0x3f) as u64;
    for i in 1..len {
        value = (value << 8) | *buf.get(at + i)? as u64;
    }
    Some((value, at + len))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 9001 A.1: the derivation is fixed by the specification, so a wrong
    /// implementation is wrong against every real packet, not just ours.
    #[test]
    fn derives_the_published_client_keys() {
        let dcid = [0x83, 0x94, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08];
        let keys = Keys::derive(VERSION_1, &dcid).expect("v1 derivation");
        assert_eq!(hex::encode(keys.key), "1f369613dd76d5467730efcbe3b1a22d");
        assert_eq!(hex::encode(keys.iv), "fa044b2f42a3fd3b46fb255c");
        assert_eq!(hex::encode(keys.hp), "9f50449e04a0e810283a1e9933adedd2");
    }

    #[test]
    fn reads_a_hostname_out_of_a_packet_it_built() {
        let hello = client_hello(b"example.com");
        let datagram = seal_initial(&hello);
        let found = sni(&datagram).expect("hostname");
        assert_eq!(found.name, "example.com");
    }

    #[test]
    fn ignores_a_short_header_packet() {
        assert!(sni(&[0x40, 0, 0, 0, 0, 0, 0, 0]).is_none());
    }

    #[test]
    fn ignores_a_packet_whose_keys_do_not_open_it() {
        let mut datagram = seal_initial(&client_hello(b"example.com"));
        let last = datagram.len() - 1;
        datagram[last] ^= 0xff;
        assert!(sni(&datagram).is_none());
    }

    #[test]
    fn reassembles_a_hello_split_across_frames() {
        let hello = client_hello(b"split.example");
        let mut frames = Vec::new();
        let cut = hello.len() / 2;
        // Deliberately out of order: the sender is free to send them that way.
        frames.extend_from_slice(&[0x06]);
        frames.extend_from_slice(&varint_bytes(cut as u64));
        frames.extend_from_slice(&varint_bytes((hello.len() - cut) as u64));
        frames.extend_from_slice(&hello[cut..]);
        frames.extend_from_slice(&[0x06, 0x00]);
        frames.extend_from_slice(&varint_bytes(cut as u64));
        frames.extend_from_slice(&hello[..cut]);

        let joined = crypto_stream(&frames).expect("reassembled");
        assert_eq!(joined, hello);
    }

    #[test]
    fn stops_at_a_gap_rather_than_joining_across_it() {
        let mut frames = vec![0x06];
        frames.extend_from_slice(&varint_bytes(0));
        frames.extend_from_slice(&varint_bytes(3));
        frames.extend_from_slice(b"abc");
        frames.push(0x06);
        frames.extend_from_slice(&varint_bytes(100)); // far past the end
        frames.extend_from_slice(&varint_bytes(3));
        frames.extend_from_slice(b"xyz");
        assert_eq!(crypto_stream(&frames).unwrap(), b"abc");
    }

    // ---- helpers ----------------------------------------------------------

    fn varint_bytes(value: u64) -> Vec<u8> {
        if value < 64 {
            vec![value as u8]
        } else if value < 16384 {
            let v = value as u16 | 0x4000;
            v.to_be_bytes().to_vec()
        } else {
            let v = value as u32 | 0x8000_0000;
            v.to_be_bytes().to_vec()
        }
    }

    /// The smallest ClientHello that still carries a server_name.
    fn client_hello(host: &[u8]) -> Vec<u8> {
        let mut sni_ext = vec![0x00];
        sni_ext.extend_from_slice(&(host.len() as u16).to_be_bytes());
        sni_ext.extend_from_slice(host);
        let mut list = ((sni_ext.len()) as u16).to_be_bytes().to_vec();
        list.extend_from_slice(&sni_ext);
        let mut ext = vec![0x00, 0x00];
        ext.extend_from_slice(&(list.len() as u16).to_be_bytes());
        ext.extend_from_slice(&list);

        let mut body = vec![0x03, 0x03];
        body.extend_from_slice(&[0u8; 32]);
        body.push(0); // session id
        body.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]); // one cipher suite
        body.extend_from_slice(&[0x01, 0x00]); // one compression method
        body.extend_from_slice(&(ext.len() as u16).to_be_bytes());
        body.extend_from_slice(&ext);

        let mut out = vec![0x01];
        out.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        out.extend_from_slice(&body);
        out
    }

    /// Build a real Initial packet around `hello`, encrypting it the way a
    /// client would. Round-tripping proves the header protection and the AEAD
    /// agree with each other; the published-keys test above is what ties the
    /// pair to the specification.
    fn seal_initial(hello: &[u8]) -> Vec<u8> {
        let dcid = [0x83, 0x94, 0xc8, 0xf0, 0x3e, 0x51, 0x57, 0x08];
        let keys = Keys::derive(VERSION_1, &dcid).unwrap();

        let mut frames = vec![0x06];
        frames.extend_from_slice(&varint_bytes(0));
        frames.extend_from_slice(&varint_bytes(hello.len() as u64));
        frames.extend_from_slice(hello);
        // Initial packets are padded so the sample has something to read.
        frames.resize(frames.len().max(1200), 0);

        let number: u32 = 2;
        let number_bytes = number.to_be_bytes();
        let mut header = vec![0xc3]; // long, fixed, Initial, 4-byte number
        header.extend_from_slice(&VERSION_1.to_be_bytes());
        header.push(dcid.len() as u8);
        header.extend_from_slice(&dcid);
        header.push(0); // no source connection id
        header.push(0); // empty token
        let length = frames.len() + 16 + number_bytes.len();
        header.extend_from_slice(&varint_bytes_min4(length as u64));
        let number_at = header.len();
        header.extend_from_slice(&number_bytes);

        let mut nonce = keys.iv;
        for (i, byte) in (number as u64).to_be_bytes().iter().enumerate() {
            nonce[4 + i] ^= byte;
        }
        let cipher = Aes128Gcm::new(Key::<Aes128Gcm>::from_slice(&keys.key));
        let sealed = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                aes_gcm::aead::Payload { msg: &frames, aad: &header },
            )
            .unwrap();

        let mut packet = header;
        packet.extend_from_slice(&sealed);

        let sample_at = number_at + 4;
        let mask = keys.mask(&packet[sample_at..sample_at + 16]);
        packet[0] ^= mask[0] & 0x0f;
        for i in 0..4 {
            packet[number_at + i] ^= mask[1 + i];
        }
        packet
    }

    /// A four-byte varint, so the length field's own size never shifts.
    fn varint_bytes_min4(value: u64) -> Vec<u8> {
        ((value as u32) | 0x8000_0000).to_be_bytes().to_vec()
    }
}
