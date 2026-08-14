//! Reading frames out of a WebM stream.
//!
//! YouTube sends VP9 and Opus as WebM, which is Matroska with a shorter list of
//! allowed codecs — the same container this crate writes. So the reading is
//! done with the same primitives as the writing, and the work is small: find
//! the track's codec and setup bytes, then walk the clusters pulling blocks.
//!
//! A block states its time as a signed offset from its cluster, which is why
//! the cluster's own timestamp has to be carried along rather than each block
//! being read in isolation.

use crate::download::ebml::{self, *};
use anyhow::{bail, Context, Result};

/// A stream, as its Tracks element describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Stream {
    pub codec_id: String,
    pub codec_private: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sample_rate: f64,
    pub channels: u32,
    pub is_video: bool,
    /// The track number its blocks carry, which is not always one.
    pub number: u64,
}

/// One frame, located in the buffer it was read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub time_ms: u64,
    pub keyframe: bool,
    /// Where the frame's bytes start and end within the buffer.
    pub at: usize,
    pub len: usize,
}

/// Read the first track's description out of a stream's opening bytes.
pub fn stream(buffer: &[u8]) -> Result<Stream> {
    let tracks = walk(buffer, &[SEGMENT, TRACKS]).context("트랙 정보가 없습니다")?;
    let entry = ebml::children(buffer, &tracks)
        .into_iter()
        .find(|c| c.id == TRACK_ENTRY)
        .context("트랙이 비어 있습니다")?;

    let mut found = Stream {
        codec_id: String::new(),
        codec_private: Vec::new(),
        width: 0,
        height: 0,
        sample_rate: 0.0,
        channels: 0,
        is_video: false,
        number: 1,
    };

    for field in ebml::children(buffer, &entry) {
        match field.id {
            TRACK_NUMBER => found.number = ebml::uint(buffer, &field),
            TRACK_TYPE => found.is_video = ebml::uint(buffer, &field) == TRACK_TYPE_VIDEO,
            CODEC_ID => found.codec_id = ebml::string(buffer, &field),
            CODEC_PRIVATE => found.codec_private = buffer[field.body..field.end].to_vec(),
            VIDEO => {
                for size in ebml::children(buffer, &field) {
                    match size.id {
                        PIXEL_WIDTH => found.width = ebml::uint(buffer, &size) as u32,
                        PIXEL_HEIGHT => found.height = ebml::uint(buffer, &size) as u32,
                        _ => {}
                    }
                }
            }
            AUDIO => {
                for setting in ebml::children(buffer, &field) {
                    match setting.id {
                        SAMPLING_FREQUENCY => {
                            found.sample_rate = float(buffer, &setting);
                        }
                        CHANNELS => found.channels = ebml::uint(buffer, &setting) as u32,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    if found.codec_id.is_empty() {
        bail!("코덱을 알 수 없습니다");
    }
    Ok(found)
}

/// Every block belonging to `track`, in the order they appear.
///
/// Clusters that cannot be parsed end the walk rather than being skipped: past
/// a broken length there is no way to know where the next element begins, and
/// guessing would produce frames made of the wrong bytes.
pub fn blocks(buffer: &[u8], track: u64) -> Vec<Block> {
    let Some(segment) = walk(buffer, &[SEGMENT]) else { return Vec::new() };
    let mut found = Vec::new();
    let mut at = segment.body;

    while at < segment.end {
        let Some(element) = ebml::read(buffer, at) else { break };
        if element.next <= at {
            break;
        }
        if element.id == CLUSTER {
            let children = ebml::children(buffer, &element);
            let base = children
                .iter()
                .find(|c| c.id == TIMESTAMP)
                .map(|c| ebml::uint(buffer, c))
                .unwrap_or(0);
            for child in &children {
                match child.id {
                    SIMPLE_BLOCK => {
                        if let Some(block) = read_block(buffer, child, base, track, true) {
                            found.push(block);
                        }
                    }
                    // A block inside a group is the same shape, minus the flag
                    // byte's meaning — such a block is a keyframe only if the
                    // group says nothing to the contrary, which for the streams
                    // this reads it never does.
                    BLOCK_GROUP => {
                        for inner in ebml::children(buffer, child) {
                            if inner.id == BLOCK {
                                if let Some(block) = read_block(buffer, &inner, base, track, false)
                                {
                                    found.push(block);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        at = element.next;
    }
    found
}

/// A block's header: track number, time offset, flags, then the frame.
fn read_block(
    buffer: &[u8],
    element: &ebml::Element,
    base_ms: u64,
    want: u64,
    flags_meaningful: bool,
) -> Option<Block> {
    let body = element.body;
    let (number, after) = read_vint(buffer, body)?;
    if number != want {
        return None;
    }
    let relative = i16::from_be_bytes([*buffer.get(after)?, *buffer.get(after + 1)?]);
    let flags = *buffer.get(after + 2)?;
    let data = after + 3;
    if data > element.end {
        return None;
    }
    // Lacing packs several frames into one block. YouTube does not use it, and
    // reading a laced block as one frame would hand the muxer nonsense, so it
    // is refused rather than guessed at.
    if flags & 0x06 != 0 {
        return None;
    }
    let time_ms = (base_ms as i64 + relative as i64).max(0) as u64;
    Some(Block {
        time_ms,
        keyframe: if flags_meaningful { flags & 0x80 != 0 } else { true },
        at: data,
        len: element.end - data,
    })
}

/// The track number in a block header — a length-style variable integer, whose
/// marker bit is not part of the value.
fn read_vint(buffer: &[u8], at: usize) -> Option<(u64, usize)> {
    let first = *buffer.get(at)?;
    if first == 0 {
        return None;
    }
    let width = first.leading_zeros() as usize + 1;
    let mut value = (first as u64) & ((1 << (8 - width)) - 1);
    for i in 1..width {
        value = (value << 8) | *buffer.get(at + i)? as u64;
    }
    Some((value, at + width))
}

/// Matroska floats are four or eight bytes, and both appear in the wild.
fn float(buffer: &[u8], element: &ebml::Element) -> f64 {
    match element.end - element.body {
        4 => f32::from_be_bytes(buffer[element.body..element.end].try_into().unwrap()) as f64,
        8 => f64::from_be_bytes(buffer[element.body..element.end].try_into().unwrap()),
        _ => 0.0,
    }
}

/// Follow a path of element ids from the top of the buffer.
fn walk(buffer: &[u8], path: &[Id]) -> Option<ebml::Element> {
    let mut at = 0usize;
    let mut parent = loop {
        let element = ebml::read(buffer, at)?;
        if element.id == path[0] {
            break element;
        }
        if element.next <= at {
            return None;
        }
        at = element.next;
    };
    for id in &path[1..] {
        parent = ebml::children(buffer, &parent).into_iter().find(|c| c.id == *id)?;
    }
    Some(parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream header declaring one Opus track, as YouTube's audio arrives.
    fn opus_header() -> Vec<u8> {
        let mut audio = Vec::new();
        put_float(&mut audio, SAMPLING_FREQUENCY, 48_000.0);
        put_uint(&mut audio, CHANNELS, 2);

        let mut entry = Vec::new();
        put_uint(&mut entry, TRACK_NUMBER, 1);
        put_uint(&mut entry, TRACK_TYPE, TRACK_TYPE_AUDIO);
        put_string(&mut entry, CODEC_ID, "A_OPUS");
        put_element(&mut entry, CODEC_PRIVATE, b"OpusHead-bytes");
        put_element(&mut entry, AUDIO, &audio);

        let mut tracks = Vec::new();
        put_element(&mut tracks, TRACK_ENTRY, &entry);

        let mut segment = Vec::new();
        put_element(&mut segment, TRACKS, &tracks);

        let mut file = Vec::new();
        put_element(&mut file, SEGMENT, &segment);
        file
    }

    /// Append a cluster of blocks to an existing file.
    fn with_cluster(file: &[u8], base_ms: u64, blocks: &[(u64, i16, bool, &[u8])]) -> Vec<u8> {
        let mut body = Vec::new();
        put_uint(&mut body, TIMESTAMP, base_ms);
        for (track, relative, keyframe, data) in blocks {
            let mut block = Vec::new();
            put_size(&mut block, *track);
            block.extend_from_slice(&relative.to_be_bytes());
            block.push(if *keyframe { 0x80 } else { 0x00 });
            block.extend_from_slice(data);
            put_element(&mut body, SIMPLE_BLOCK, &block);
        }
        let mut cluster = Vec::new();
        put_element(&mut cluster, CLUSTER, &body);

        // Rebuild the segment with the cluster inside it.
        let segment = walk(file, &[SEGMENT]).expect("segment");
        let mut inner = file[segment.body..segment.end].to_vec();
        inner.extend_from_slice(&cluster);
        let mut out = Vec::new();
        put_element(&mut out, SEGMENT, &inner);
        out
    }

    #[test]
    fn reads_an_audio_track_s_description() {
        let found = stream(&opus_header()).expect("stream");
        assert_eq!(found.codec_id, "A_OPUS");
        assert_eq!(found.codec_private, b"OpusHead-bytes");
        assert_eq!(found.sample_rate, 48_000.0);
        assert_eq!(found.channels, 2);
        assert!(!found.is_video);
    }

    #[test]
    fn refuses_a_stream_with_no_codec() {
        let mut entry = Vec::new();
        put_uint(&mut entry, TRACK_NUMBER, 1);
        let mut tracks = Vec::new();
        put_element(&mut tracks, TRACK_ENTRY, &entry);
        let mut segment = Vec::new();
        put_element(&mut segment, TRACKS, &tracks);
        let mut file = Vec::new();
        put_element(&mut file, SEGMENT, &segment);
        assert!(stream(&file).is_err());
    }

    #[test]
    fn reads_blocks_with_absolute_times() {
        let file = with_cluster(&opus_header(), 10_000, &[(1, 0, true, b"one"), (1, 20, false, b"two")]);
        let found = blocks(&file, 1);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].time_ms, 10_000);
        assert_eq!(found[1].time_ms, 10_020);
        assert!(found[0].keyframe);
        assert!(!found[1].keyframe);
        assert_eq!(&file[found[1].at..found[1].at + found[1].len], b"two");
    }

    #[test]
    fn a_negative_offset_reaches_back_before_its_cluster() {
        let file = with_cluster(&opus_header(), 1_000, &[(1, -40, true, b"early")]);
        assert_eq!(blocks(&file, 1)[0].time_ms, 960);
    }

    #[test]
    fn blocks_for_another_track_are_left_alone() {
        let file = with_cluster(&opus_header(), 0, &[(1, 0, true, b"mine"), (2, 0, true, b"theirs")]);
        let found = blocks(&file, 1);
        assert_eq!(found.len(), 1);
        assert_eq!(&file[found[0].at..found[0].at + found[0].len], b"mine");
    }

    #[test]
    fn several_clusters_are_read_in_order() {
        let file = with_cluster(&opus_header(), 0, &[(1, 0, true, b"a")]);
        let file = with_cluster(&file, 5_000, &[(1, 0, true, b"b")]);
        let found = blocks(&file, 1);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].time_ms, 0);
        assert_eq!(found[1].time_ms, 5_000);
    }

    #[test]
    fn a_laced_block_is_refused_rather_than_misread() {
        let mut body = Vec::new();
        put_uint(&mut body, TIMESTAMP, 0);
        let mut block = Vec::new();
        put_size(&mut block, 1);
        block.extend_from_slice(&0i16.to_be_bytes());
        block.push(0x80 | 0x02); // keyframe, with lacing turned on
        block.extend_from_slice(b"laced");
        put_element(&mut body, SIMPLE_BLOCK, &block);

        let mut cluster = Vec::new();
        put_element(&mut cluster, CLUSTER, &body);
        let header = opus_header();
        let segment = walk(&header, &[SEGMENT]).expect("segment");
        let mut inner = header[segment.body..segment.end].to_vec();
        inner.extend_from_slice(&cluster);
        let mut file = Vec::new();
        put_element(&mut file, SEGMENT, &inner);

        assert!(blocks(&file, 1).is_empty());
    }

    #[test]
    fn garbage_yields_nothing_rather_than_panicking() {
        assert!(blocks(&[0xff; 64], 1).is_empty());
        assert!(stream(&[0xff; 64]).is_err());
    }
}
