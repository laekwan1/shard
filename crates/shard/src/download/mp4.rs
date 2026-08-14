//! Reading frames out of a fragmented MP4.
//!
//! What YouTube sends for an AV1, H.264 or AAC stream is not a plain MP4: it is
//! a setup segment followed by fragments, each carrying its own table of where
//! its samples are and how long they last. That shape exists so a player can
//! start in the middle, and it is also what makes this readable without holding
//! the file in memory — every fragment is self-describing.
//!
//! Only enough of the format is parsed to hand the muxer frames: what the codec
//! is, its setup bytes, and for each sample its position, length, time and
//! whether it can be decoded on its own. Everything else in an MP4 exists to
//! describe things this does not need.

use anyhow::{bail, Context, Result};

/// A stream, as the setup segment describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Stream {
    /// Matroska's name for the codec, ready for the muxer.
    pub codec_id: String,
    /// The codec's own setup bytes, verbatim.
    pub codec_private: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sample_rate: f64,
    pub channels: u32,
    /// Ticks per second for every time in this track.
    pub timescale: u32,
    pub is_video: bool,
}

/// One frame, located in the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    pub at: u64,
    pub len: usize,
    /// Presentation time, in the track's own ticks.
    pub time: u64,
    pub duration: u32,
    pub keyframe: bool,
}

impl Sample {
    pub fn time_ms(&self, timescale: u32) -> u64 {
        if timescale == 0 {
            return 0;
        }
        self.time * 1_000 / timescale as u64
    }
}

/// Walk the boxes at one level, calling `visit` for each.
///
/// A box is a length, a four-character kind, and a body — so the whole format
/// is this loop applied at every depth.
fn boxes(buffer: &[u8], mut at: usize, end: usize, mut visit: impl FnMut(&[u8; 4], usize, usize)) {
    while at + 8 <= end {
        let size = u32::from_be_bytes(buffer[at..at + 4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = buffer[at + 4..at + 8].try_into().unwrap();
        let (body, size) = if size == 1 {
            if at + 16 > end {
                return;
            }
            (at + 16, u64::from_be_bytes(buffer[at + 8..at + 16].try_into().unwrap()))
        } else if size == 0 {
            (at + 8, (end - at) as u64)
        } else {
            (at + 8, size)
        };
        let stop = match at.checked_add(size as usize) {
            Some(stop) if stop <= end && size >= 8 => stop,
            _ => return,
        };
        visit(&kind, body, stop);
        at = stop;
    }
}

/// Find one box by its path from the top, e.g. `[b"moov", b"trak"]`.
fn find(buffer: &[u8], path: &[&[u8; 4]]) -> Option<(usize, usize)> {
    let mut range = (0usize, buffer.len());
    for want in path {
        let mut found = None;
        boxes(buffer, range.0, range.1, |kind, body, end| {
            if found.is_none() && kind == *want {
                found = Some((body, end));
            }
        });
        range = found?;
    }
    Some(range)
}

/// Read the setup segment: what the track is and how to start decoding it.
pub fn stream(buffer: &[u8]) -> Result<Stream> {
    let (body, end) = find(buffer, &[b"moov", b"trak", b"mdia"]).context("트랙 정보가 없습니다")?;

    let mut timescale = 0u32;
    boxes(buffer, body, end, |kind, body, end| {
        if kind == b"mdhd" && body + 20 <= end {
            // Version 0 puts the timescale at 12 bytes in, version 1 at 20.
            let at = if buffer[body] == 1 { body + 20 } else { body + 12 };
            if at + 4 <= end {
                timescale = u32::from_be_bytes(buffer[at..at + 4].try_into().unwrap());
            }
        }
    });

    let (entry_body, entry_end) =
        sample_entry(buffer, body, end).context("코덱 정보를 찾지 못했습니다")?;
    // The four characters naming the entry sit immediately before its body.
    let kind = &buffer[entry_body - 4..entry_body];

    let mut stream = Stream {
        codec_id: String::new(),
        codec_private: Vec::new(),
        width: 0,
        height: 0,
        sample_rate: 0.0,
        channels: 0,
        timescale,
        is_video: false,
    };

    match kind {
        b"av01" | b"avc1" | b"avc3" | b"hev1" | b"hvc1" => {
            stream.is_video = true;
            // A visual sample entry keeps its dimensions at a fixed offset.
            if entry_body + 32 <= entry_end {
                stream.width =
                    u16::from_be_bytes(buffer[entry_body + 24..entry_body + 26].try_into()?) as u32;
                stream.height =
                    u16::from_be_bytes(buffer[entry_body + 26..entry_body + 28].try_into()?) as u32;
            }
            stream.codec_id = match kind {
                b"av01" => "V_AV1",
                b"hev1" | b"hvc1" => "V_MPEGH/ISO/HEVC",
                _ => "V_MPEG4/ISO/AVC",
            }
            .into();
            // The codec's configuration record, whatever it is called for this
            // codec, is copied out untouched — a decoder needs it exactly.
            let want: &[u8; 4] = match kind {
                b"av01" => b"av1C",
                b"hev1" | b"hvc1" => b"hvcC",
                _ => b"avcC",
            };
            boxes(buffer, entry_body + 78, entry_end, |kind, body, end| {
                if kind == want {
                    stream.codec_private = buffer[body..end].to_vec();
                }
            });
        }
        b"mp4a" | b"Opus" => {
            if entry_body + 28 <= entry_end {
                stream.channels =
                    u16::from_be_bytes(buffer[entry_body + 16..entry_body + 18].try_into()?) as u32;
                // A 16.16 fixed-point rate; the fraction is always zero here.
                stream.sample_rate =
                    u16::from_be_bytes(buffer[entry_body + 24..entry_body + 26].try_into()?) as f64;
            }
            if stream.sample_rate == 0.0 {
                stream.sample_rate = timescale as f64;
            }
            stream.codec_id = if kind == b"Opus" { "A_OPUS" } else { "A_AAC" }.into();
            boxes(buffer, entry_body + 28, entry_end, |kind, body, end| {
                match kind {
                    // AAC's setup lives inside a descriptor; the useful part is
                    // the audio-specific config at its tail.
                    b"esds" => stream.codec_private = audio_specific_config(&buffer[body..end]),
                    b"dOps" => stream.codec_private = opus_head(&buffer[body..end]),
                    _ => {}
                }
            });
        }
        other => bail!("다루지 않는 코덱입니다: {}", String::from_utf8_lossy(other)),
    }

    if stream.codec_private.is_empty() {
        bail!("코덱 설정 데이터가 없습니다");
    }
    Ok(stream)
}

/// The sample entry inside `stsd`, with its header already skipped.
fn sample_entry(buffer: &[u8], from: usize, to: usize) -> Option<(usize, usize)> {
    let (stbl_body, stbl_end) = {
        let mut found = None;
        boxes(buffer, from, to, |kind, body, end| {
            if kind == b"minf" {
                boxes(buffer, body, end, |kind, body, end| {
                    if kind == b"stbl" {
                        found = Some((body, end));
                    }
                });
            }
        });
        found?
    };
    let mut entry = None;
    boxes(buffer, stbl_body, stbl_end, |kind, body, end| {
        if kind == b"stsd" && body + 8 <= end {
            // Version and flags, then the entry count.
            boxes(buffer, body + 8, end, |_, body, end| {
                if entry.is_none() {
                    entry = Some((body, end));
                }
            });
        }
    });
    entry
}

/// Pull the AudioSpecificConfig out of an `esds` descriptor.
///
/// The descriptors nest with variable-length sizes, but the piece needed is
/// always introduced by tag 5 — and looking for that tag is far less code than
/// walking the tree correctly for the one field it would find anyway.
fn audio_specific_config(esds: &[u8]) -> Vec<u8> {
    let mut at = 4; // version and flags
    while at < esds.len() {
        let tag = esds[at];
        at += 1;
        let mut len = 0usize;
        for _ in 0..4 {
            let Some(byte) = esds.get(at) else { return Vec::new() };
            at += 1;
            len = (len << 7) | (*byte & 0x7f) as usize;
            if byte & 0x80 == 0 {
                break;
            }
        }
        match tag {
            // ES_Descriptor: skip its id and flags, then keep walking inward.
            0x03 => at += 3,
            // DecoderConfigDescriptor: skip the fixed fields, keep walking.
            0x04 => at += 13,
            // DecoderSpecificInfo — this is the one.
            0x05 => return esds.get(at..at + len).map(<[u8]>::to_vec).unwrap_or_default(),
            _ => at += len,
        }
    }
    Vec::new()
}

/// Rebuild an OpusHead header from the `dOps` box.
///
/// MP4 stores the same numbers as Matroska but drops the magic and reorders two
/// fields, so the header has to be assembled rather than copied.
fn opus_head(dops: &[u8]) -> Vec<u8> {
    if dops.len() < 11 {
        return Vec::new();
    }
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // version
    head.push(dops[1]); // channel count
    head.extend_from_slice(&u16::from_be_bytes([dops[2], dops[3]]).to_le_bytes()); // pre-skip
    head.extend_from_slice(&u32::from_be_bytes([dops[4], dops[5], dops[6], dops[7]]).to_le_bytes());
    head.extend_from_slice(&u16::from_be_bytes([dops[8], dops[9]]).to_le_bytes()); // output gain
    head.push(dops[10]); // channel mapping family
    head
}

/// Read every sample the fragments describe, in order.
///
/// `from` is where `buffer` begins in the whole file, so positions come back
/// absolute even when only part of the file is in hand.
pub fn samples(buffer: &[u8], from: u64) -> Vec<Sample> {
    let mut found = Vec::new();
    let mut time = 0u64;
    boxes(buffer, 0, buffer.len(), |kind, body, end| {
        if kind == b"moof" {
            let moof_at = body - 8;
            boxes(buffer, body, end, |kind, body, end| {
                if kind == b"traf" {
                    read_fragment(buffer, body, end, moof_at, from, &mut time, &mut found);
                }
            });
        }
    });
    found
}

/// One track fragment: defaults from `tfhd`, a start time from `tfdt`, and the
/// run of samples in `trun`.
fn read_fragment(
    buffer: &[u8],
    from: usize,
    to: usize,
    moof_at: usize,
    file_at: u64,
    time: &mut u64,
    out: &mut Vec<Sample>,
) {
    let mut base_offset = moof_at as u64;
    let mut default_duration = 0u32;
    let mut default_size = 0u32;
    let mut default_flags = 0u32;

    boxes(buffer, from, to, |kind, body, end| match kind {
        b"tfhd" => {
            let flags = u32::from_be_bytes(buffer[body..body + 4].try_into().unwrap()) & 0x00ff_ffff;
            let mut at = body + 4 + 4; // version/flags, track id
            if flags & 0x01 != 0 {
                if at + 8 <= end {
                    base_offset = u64::from_be_bytes(buffer[at..at + 8].try_into().unwrap());
                }
                at += 8;
            }
            if flags & 0x02 != 0 {
                at += 4; // sample description index
            }
            if flags & 0x08 != 0 && at + 4 <= end {
                default_duration = u32::from_be_bytes(buffer[at..at + 4].try_into().unwrap());
                at += 4;
            }
            if flags & 0x10 != 0 && at + 4 <= end {
                default_size = u32::from_be_bytes(buffer[at..at + 4].try_into().unwrap());
                at += 4;
            }
            if flags & 0x20 != 0 && at + 4 <= end {
                default_flags = u32::from_be_bytes(buffer[at..at + 4].try_into().unwrap());
            }
        }
        b"tfdt" => {
            // The fragment states its own start, which is what keeps timing
            // right when a download begins in the middle.
            if buffer[body] == 1 {
                if body + 12 <= end {
                    *time = u64::from_be_bytes(buffer[body + 4..body + 12].try_into().unwrap());
                }
            } else if body + 8 <= end {
                *time =
                    u32::from_be_bytes(buffer[body + 4..body + 8].try_into().unwrap()) as u64;
            }
        }
        _ => {}
    });

    boxes(buffer, from, to, |kind, body, end| {
        if kind != b"trun" {
            return;
        }
        let flags = u32::from_be_bytes(buffer[body..body + 4].try_into().unwrap()) & 0x00ff_ffff;
        let mut at = body + 4;
        let count = u32::from_be_bytes(buffer[at..at + 4].try_into().unwrap()) as usize;
        at += 4;
        let mut offset = base_offset;
        if flags & 0x0001 != 0 && at + 4 <= end {
            let relative = i32::from_be_bytes(buffer[at..at + 4].try_into().unwrap());
            offset = (moof_at as i64 + relative as i64).max(0) as u64;
            at += 4;
        }
        let mut first_flags = None;
        if flags & 0x0004 != 0 && at + 4 <= end {
            first_flags = Some(u32::from_be_bytes(buffer[at..at + 4].try_into().unwrap()));
            at += 4;
        }

        for index in 0..count {
            let mut duration = default_duration;
            let mut size = default_size;
            let mut sample_flags = default_flags;
            if flags & 0x0100 != 0 && at + 4 <= end {
                duration = u32::from_be_bytes(buffer[at..at + 4].try_into().unwrap());
                at += 4;
            }
            if flags & 0x0200 != 0 && at + 4 <= end {
                size = u32::from_be_bytes(buffer[at..at + 4].try_into().unwrap());
                at += 4;
            }
            if flags & 0x0400 != 0 && at + 4 <= end {
                sample_flags = u32::from_be_bytes(buffer[at..at + 4].try_into().unwrap());
                at += 4;
            }
            if flags & 0x0800 != 0 {
                at += 4; // composition offset, which the muxer does not use
            }
            if index == 0 {
                if let Some(first) = first_flags {
                    sample_flags = first;
                }
            }
            // "Not a sync sample" is a flag, so its absence is a keyframe.
            let keyframe = sample_flags & 0x0001_0000 == 0;
            out.push(Sample {
                at: file_at + offset,
                len: size as usize,
                time: *time,
                duration,
                keyframe,
            });
            offset += size as u64;
            *time += duration as u64;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn nest(kind: &[u8; 4], children: &[Vec<u8>]) -> Vec<u8> {
        boxed(kind, &children.concat())
    }

    /// A setup segment for an AV1 track at 640x360, 1000 ticks a second.
    fn av1_init() -> Vec<u8> {
        let mut mdhd = vec![0u8; 20];
        mdhd[12..16].copy_from_slice(&1_000u32.to_be_bytes());

        let mut entry = vec![0u8; 78];
        entry[24..26].copy_from_slice(&640u16.to_be_bytes());
        entry[26..28].copy_from_slice(&360u16.to_be_bytes());
        entry.extend_from_slice(&boxed(b"av1C", &[0x81, 0x00, 0x0c, 0x00]));

        let stsd = boxed(b"stsd", &[&[0u8; 8][..], &boxed(b"av01", &entry)].concat());
        nest(
            b"moov",
            &[nest(
                b"trak",
                &[nest(
                    b"mdia",
                    &[boxed(b"mdhd", &mdhd), nest(b"minf", &[nest(b"stbl", &[stsd])])],
                )],
            )],
        )
    }

    #[test]
    fn reads_a_video_track_s_codec_and_size() {
        let stream = stream(&av1_init()).expect("stream");
        assert_eq!(stream.codec_id, "V_AV1");
        assert_eq!(stream.codec_private, vec![0x81, 0x00, 0x0c, 0x00]);
        assert_eq!((stream.width, stream.height), (640, 360));
        assert_eq!(stream.timescale, 1_000);
        assert!(stream.is_video);
    }

    #[test]
    fn refuses_a_track_with_no_setup_bytes() {
        let mut entry = vec![0u8; 78];
        entry.extend_from_slice(&boxed(b"pasp", &[0; 8])); // not the config
        let stsd = boxed(b"stsd", &[&[0u8; 8][..], &boxed(b"av01", &entry)].concat());
        let file = nest(
            b"moov",
            &[nest(
                b"trak",
                &[nest(
                    b"mdia",
                    &[boxed(b"mdhd", &vec![0u8; 20]), nest(b"minf", &[nest(b"stbl", &[stsd])])],
                )],
            )],
        );
        assert!(stream(&file).is_err());
    }

    #[test]
    fn rebuilds_an_opus_header_from_its_mp4_form() {
        let mut dops = vec![0u8; 11];
        dops[1] = 2; // channels
        dops[2..4].copy_from_slice(&312u16.to_be_bytes()); // pre-skip
        dops[4..8].copy_from_slice(&48_000u32.to_be_bytes());
        let head = opus_head(&dops);
        assert_eq!(&head[..8], b"OpusHead");
        assert_eq!(head[9], 2);
        assert_eq!(u16::from_le_bytes([head[10], head[11]]), 312);
        assert_eq!(u32::from_le_bytes([head[12], head[13], head[14], head[15]]), 48_000);
    }

    #[test]
    fn finds_the_aac_config_inside_its_descriptor() {
        // esds: version/flags, ES_Descriptor, DecoderConfig, DecoderSpecific.
        let mut esds = vec![0u8; 4];
        esds.extend_from_slice(&[0x03, 0x19]);
        esds.extend_from_slice(&[0, 0, 0]); // es id, flags
        esds.extend_from_slice(&[0x04, 0x11]);
        esds.extend_from_slice(&[0u8; 13]);
        esds.extend_from_slice(&[0x05, 0x02, 0x12, 0x10]);
        assert_eq!(audio_specific_config(&esds), vec![0x12, 0x10]);
    }

    /// A fragment holding two samples, the first a keyframe.
    fn fragment(start_time: u64, sizes: &[u32]) -> Vec<u8> {
        let mut tfhd = vec![0u8; 4];
        tfhd[3] = 0x20; // default sample flags present
        tfhd.extend_from_slice(&1u32.to_be_bytes()); // track id
        tfhd.extend_from_slice(&0u32.to_be_bytes()); // default flags: keyframe

        let mut tfdt = vec![1u8, 0, 0, 0];
        tfdt.extend_from_slice(&start_time.to_be_bytes());

        let mut trun = vec![0u8, 0x00, 0x03, 0x01]; // data offset + duration + size
        trun[1] = 0x00;
        trun[2] = 0x07; // duration, size, flags present
        trun[3] = 0x01; // data offset present
        trun.extend_from_slice(&(sizes.len() as u32).to_be_bytes());
        trun.extend_from_slice(&0i32.to_be_bytes()); // patched below
        for (index, size) in sizes.iter().enumerate() {
            trun.extend_from_slice(&40u32.to_be_bytes()); // duration
            trun.extend_from_slice(&size.to_be_bytes());
            // Only the first sample is a sync sample.
            let flags: u32 = if index == 0 { 0 } else { 0x0001_0000 };
            trun.extend_from_slice(&flags.to_be_bytes());
        }

        let traf = nest(b"traf", &[boxed(b"tfhd", &tfhd), boxed(b"tfdt", &tfdt), boxed(b"trun", &trun)]);
        let moof = nest(b"moof", &[traf]);
        let mdat = boxed(b"mdat", &vec![0u8; sizes.iter().sum::<u32>() as usize]);

        // The data offset is measured from the start of `moof`.
        let mut file = moof;
        let offset = (file.len() + 8) as i32;
        // The offset field is the last thing before the per-sample rows.
        let trun_at = file.len() - (sizes.len() * 12) - 4;
        file[trun_at..trun_at + 4].copy_from_slice(&offset.to_be_bytes());
        file.extend_from_slice(&mdat);
        file
    }

    #[test]
    fn reads_samples_with_their_times_and_positions() {
        let file = fragment(0, &[100, 200]);
        let found = samples(&file, 0);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].len, 100);
        assert_eq!(found[1].len, 200);
        assert_eq!(found[1].at, found[0].at + 100);
        assert_eq!(found[0].time, 0);
        assert_eq!(found[1].time, 40);
        assert!(found[0].keyframe);
        assert!(!found[1].keyframe);
    }

    #[test]
    fn a_fragment_states_its_own_start_time() {
        let file = fragment(96_000, &[10]);
        let found = samples(&file, 0);
        assert_eq!(found[0].time, 96_000);
    }

    #[test]
    fn positions_are_absolute_when_the_buffer_starts_late() {
        let file = fragment(0, &[10]);
        let found = samples(&file, 5_000);
        assert_eq!(found[0].at, 5_000 + samples(&file, 0)[0].at);
    }

    #[test]
    fn times_convert_to_milliseconds_through_the_timescale() {
        let sample = Sample { at: 0, len: 0, time: 96_000, duration: 0, keyframe: true };
        assert_eq!(sample.time_ms(48_000), 2_000);
    }

    #[test]
    fn garbage_yields_nothing_rather_than_panicking() {
        assert!(samples(&[0xff; 64], 0).is_empty());
        assert!(stream(&[0xff; 64]).is_err());
    }
}
