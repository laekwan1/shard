//! EBML — the bytes Matroska is made of.
//!
//! Both halves of muxing need this. The writer needs it to produce the output
//! file; the reader needs it because one of the inputs is already Matroska —
//! YouTube's VP9 and Opus streams are WebM, which is Matroska with a shorter
//! list of allowed codecs.
//!
//! The format is two ideas. Every element is an identifier, a length, and a
//! body; identifiers and lengths are variable-width integers whose first byte
//! says how many bytes it occupies. Elements nest, so a file is a tree.
//!
//! The two variable-width forms differ in one detail that is easy to miss and
//! produces a file nothing will open: an identifier keeps its length marker as
//! part of its value, and a length throws it away.

/// A Matroska element identifier, stored with its marker bits intact.
pub type Id = u32;

// Only the elements this code writes or reads. Matroska has hundreds.
pub const EBML_HEADER: Id = 0x1A45_DFA3;
pub const EBML_VERSION: Id = 0x4286;
pub const EBML_READ_VERSION: Id = 0x42F7;
pub const EBML_MAX_ID_LENGTH: Id = 0x42F2;
pub const EBML_MAX_SIZE_LENGTH: Id = 0x42F3;
pub const DOC_TYPE: Id = 0x4282;
pub const DOC_TYPE_VERSION: Id = 0x4287;
pub const DOC_TYPE_READ_VERSION: Id = 0x4285;

pub const SEGMENT: Id = 0x1853_8067;
pub const INFO: Id = 0x1549_A966;
pub const TIMESTAMP_SCALE: Id = 0x2AD7_B1;
pub const DURATION: Id = 0x4489;
pub const MUXING_APP: Id = 0x4D80;
pub const WRITING_APP: Id = 0x5741;

pub const TRACKS: Id = 0x1654_AE6B;
pub const TRACK_ENTRY: Id = 0xAE;
pub const TRACK_NUMBER: Id = 0xD7;
pub const TRACK_UID: Id = 0x73C5;
pub const TRACK_TYPE: Id = 0x83;
pub const CODEC_ID: Id = 0x86;
pub const CODEC_PRIVATE: Id = 0x63A2;
pub const DEFAULT_DURATION: Id = 0x23E3_83;
pub const VIDEO: Id = 0xE0;
pub const PIXEL_WIDTH: Id = 0xB0;
pub const PIXEL_HEIGHT: Id = 0xBA;
pub const AUDIO: Id = 0xE1;
pub const SAMPLING_FREQUENCY: Id = 0xB5;
pub const CHANNELS: Id = 0x9F;

pub const CLUSTER: Id = 0x1F43_B675;
pub const TIMESTAMP: Id = 0xE7;
pub const SIMPLE_BLOCK: Id = 0xA3;
pub const BLOCK_GROUP: Id = 0xA0;
pub const BLOCK: Id = 0xA1;

pub const CUES: Id = 0x1C53_BB6B;

/// The table at the front saying where the other tables are.
pub const SEEK_HEAD: Id = 0x114D_9B74;
pub const SEEK: Id = 0x4DBB;
pub const SEEK_ID: Id = 0x53AB;
pub const SEEK_POSITION: Id = 0x53AC;
pub const CUE_POINT: Id = 0xBB;
pub const CUE_TIME: Id = 0xB3;
pub const CUE_TRACK_POSITIONS: Id = 0xB7;
pub const CUE_TRACK: Id = 0xF7;
pub const CUE_CLUSTER_POSITION: Id = 0xF1;

pub const TRACK_TYPE_VIDEO: u64 = 1;
pub const TRACK_TYPE_AUDIO: u64 = 2;

// ---- writing -------------------------------------------------------------

/// Append an element identifier, which is written exactly as it is numbered.
pub fn put_id(out: &mut Vec<u8>, id: Id) {
    let bytes = id.to_be_bytes();
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(3);
    out.extend_from_slice(&bytes[first..]);
}

/// Append a length, in the fewest bytes that hold it.
///
/// The marker bit says how wide the field is and is not part of the value, so
/// each width loses one bit — seven usable in one byte, fourteen in two.
pub fn put_size(out: &mut Vec<u8>, size: u64) {
    for width in 1..=8u32 {
        let bits = 7 * width;
        let limit = (1u64 << bits) - 1;
        // An all-ones value means "unknown length", so it cannot be used here.
        if size < limit {
            // The marker is a single set bit directly above the value's bits.
            let bytes = (size | (1u64 << bits)).to_be_bytes();
            out.extend_from_slice(&bytes[8 - width as usize..]);
            return;
        }
    }
    // Eight bytes hold anything a file can contain.
    out.extend_from_slice(&(size | (1u64 << 56)).to_be_bytes());
}

/// A whole element: identifier, length, body.
pub fn put_element(out: &mut Vec<u8>, id: Id, body: &[u8]) {
    put_id(out, id);
    put_size(out, body.len() as u64);
    out.extend_from_slice(body);
}

/// An unsigned integer element, in as few bytes as carry it.
/// A number written to a stated width, so it can be filled in later without
/// anything after it having to move.
pub fn put_uint_fixed(out: &mut Vec<u8>, id: Id, value: u64, width: usize) {
    let bytes = value.to_be_bytes();
    put_element(out, id, &bytes[8 - width..]);
}

pub fn put_uint(out: &mut Vec<u8>, id: Id, value: u64) {
    let bytes = value.to_be_bytes();
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(7);
    put_element(out, id, &bytes[first..]);
}

pub fn put_float(out: &mut Vec<u8>, id: Id, value: f64) {
    put_element(out, id, &value.to_be_bytes());
}

pub fn put_string(out: &mut Vec<u8>, id: Id, value: &str) {
    put_element(out, id, value.as_bytes());
}

// ---- reading -------------------------------------------------------------

/// One element found in a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Element {
    pub id: Id,
    /// Where the body starts.
    pub body: usize,
    /// Where the body ends. For an unknown-length element, the end of input.
    pub end: usize,
    /// Where the next sibling begins.
    pub next: usize,
}

/// Read the element beginning at `at`.
///
/// Returns `None` when the buffer does not hold a whole header, which is how a
/// truncated file stops the walk rather than producing nonsense.
pub fn read(buffer: &[u8], at: usize) -> Option<Element> {
    let (id, after_id) = read_id(buffer, at)?;
    let (size, after_size) = read_size(buffer, after_id)?;
    let end = match size {
        // Unknown length: a live-streamed Segment or Cluster. It runs to the
        // end of what we have, which is what a file on disk means anyway.
        None => buffer.len(),
        Some(len) => after_size.checked_add(len as usize)?.min(buffer.len()),
    };
    Some(Element { id, body: after_size, end, next: end })
}

fn read_id(buffer: &[u8], at: usize) -> Option<(Id, usize)> {
    let first = *buffer.get(at)?;
    if first == 0 {
        return None;
    }
    let width = first.leading_zeros() as usize + 1;
    if width > 4 {
        return None;
    }
    let mut id: u32 = 0;
    for i in 0..width {
        id = (id << 8) | *buffer.get(at + i)? as u32;
    }
    Some((id, at + width))
}

/// A length, or `None` for the all-ones "unknown" encoding.
fn read_size(buffer: &[u8], at: usize) -> Option<(Option<u64>, usize)> {
    let first = *buffer.get(at)?;
    if first == 0 {
        return None;
    }
    let width = first.leading_zeros() as usize + 1;
    let mut value = (first as u64) & ((1 << (8 - width)) - 1);
    let mut unknown = value == (1 << (8 - width)) - 1;
    for i in 1..width {
        let byte = *buffer.get(at + i)?;
        unknown &= byte == 0xff;
        value = (value << 8) | byte as u64;
    }
    Some((if unknown { None } else { Some(value) }, at + width))
}

/// The direct children of an element, in order.
pub fn children(buffer: &[u8], parent: &Element) -> Vec<Element> {
    let mut found = Vec::new();
    let mut at = parent.body;
    while at < parent.end {
        let Some(child) = read(buffer, at) else { break };
        if child.next <= at {
            break;
        }
        at = child.next;
        found.push(child);
    }
    found
}

/// Read an element's body as an unsigned integer.
pub fn uint(buffer: &[u8], element: &Element) -> u64 {
    buffer[element.body..element.end].iter().fold(0u64, |acc, b| (acc << 8) | *b as u64)
}

/// Read an element's body as a string, ignoring anything that is not UTF-8.
pub fn string(buffer: &[u8], element: &Element) -> String {
    String::from_utf8_lossy(&buffer[element.body..element.end]).trim_end_matches('\0').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_identifiers_at_their_natural_width() {
        let mut out = Vec::new();
        put_id(&mut out, TRACK_NUMBER); // 0xD7
        put_id(&mut out, TRACK_UID); // 0x73C5
        put_id(&mut out, SEGMENT); // 0x18538067
        assert_eq!(out, vec![0xD7, 0x73, 0xC5, 0x18, 0x53, 0x80, 0x67]);
    }

    #[test]
    fn writes_lengths_in_the_fewest_bytes() {
        let cases: [(u64, &[u8]); 4] = [
            (0, &[0x80]),
            (1, &[0x81]),
            (126, &[0xFE]),
            // 127 is the unknown-length marker at one byte, so it moves up.
            (127, &[0x40, 0x7F]),
        ];
        for (size, expected) in cases {
            let mut out = Vec::new();
            put_size(&mut out, size);
            assert_eq!(out, expected, "size {size}");
        }
    }

    #[test]
    fn lengths_round_trip_through_the_reader() {
        // Read back from the encoding rather than from a file: a length is
        // allowed to describe a body larger than any test wants to allocate.
        for size in [0u64, 1, 126, 127, 300, 16_000, 16_383, 1 << 20, 1 << 40] {
            let mut encoded = Vec::new();
            put_size(&mut encoded, size);
            let (read_back, consumed) = read_size(&encoded, 0).expect("length");
            assert_eq!(read_back, Some(size), "size {size}");
            assert_eq!(consumed, encoded.len(), "size {size}");
        }
    }

    #[test]
    fn integers_drop_their_leading_zeros() {
        let mut out = Vec::new();
        put_uint(&mut out, TRACK_NUMBER, 1);
        assert_eq!(out, vec![0xD7, 0x81, 0x01]);

        let mut wide = Vec::new();
        put_uint(&mut wide, TRACK_NUMBER, 0x1234);
        assert_eq!(wide, vec![0xD7, 0x82, 0x12, 0x34]);
    }

    #[test]
    fn walks_a_tree_of_elements() {
        let mut tracks = Vec::new();
        let mut entry = Vec::new();
        put_uint(&mut entry, TRACK_NUMBER, 1);
        put_string(&mut entry, CODEC_ID, "V_VP9");
        put_element(&mut tracks, TRACK_ENTRY, &entry);
        let mut file = Vec::new();
        put_element(&mut file, TRACKS, &tracks);

        let root = read(&file, 0).expect("tracks");
        assert_eq!(root.id, TRACKS);
        let entries = children(&file, &root);
        assert_eq!(entries.len(), 1);
        let fields = children(&file, &entries[0]);
        assert_eq!(uint(&file, &fields[0]), 1);
        assert_eq!(string(&file, &fields[1]), "V_VP9");
    }

    #[test]
    fn an_unknown_length_element_runs_to_the_end() {
        let mut file = Vec::new();
        put_id(&mut file, SEGMENT);
        file.push(0xFF); // unknown length
        put_uint(&mut file, TIMESTAMP, 7);
        let segment = read(&file, 0).expect("segment");
        assert_eq!(segment.end, file.len());
        assert_eq!(uint(&file, &children(&file, &segment)[0]), 7);
    }

    #[test]
    fn a_truncated_header_stops_the_walk() {
        let mut file = Vec::new();
        put_element(&mut file, TIMESTAMP, &[1]);
        put_id(&mut file, CLUSTER); // header cut off here
        let parent = Element { id: SEGMENT, body: 0, end: file.len(), next: file.len() };
        assert_eq!(children(&file, &parent).len(), 1);
    }
}
