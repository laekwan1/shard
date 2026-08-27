//! Write a plain (unfragmented) MP4 from demuxed H.264 + AAC.
//!
//! The Matroska writer ([`crate::download::mkv`]) is what the desktop and the
//! YouTube path use, and it takes any codec. But iOS will not play Matroska —
//! WKWebView and AVPlayer both refuse `.mkv`/`.webm` — and adding a player that
//! can would mean bundling something the size of VLC. A transport stream only
//! ever carries H.264 and AAC, which is exactly what MP4 was made for, so the
//! HLS-over-TS path writes MP4 instead and plays everywhere, phone included.
//!
//! This is a whole-file writer: it lays the samples down as one `mdat` and then
//! a `moov` describing them. Not streamable (the tables need every sample first)
//! but a downloaded file is fully in hand anyway.
//!
//! B-frames are handled the way MP4 wants: decode order in the file, an `stts`
//! of decode durations, and a `ctts` of composition offsets (presentation minus
//! decode). Audio needs neither — every AAC frame is a keyframe of 1024 samples.

use crate::download::ts::{Demuxed, Sample};
use anyhow::{bail, Result};
use std::io::Write;

/// A track laid out as one chunk, with the tables a `stbl` needs.
struct Track {
    /// Byte offset of this track's samples within the file.
    chunk_offset: u32,
    sizes: Vec<u32>,
    /// (count, delta) runs in the media timescale.
    stts: Vec<(u32, u32)>,
    /// (count, offset) runs; empty when every offset is zero (no B-frames).
    ctts: Vec<(u32, u32)>,
    /// 1-based sample numbers that are sync samples; empty means "all sync".
    sync: Vec<u32>,
    timescale: u32,
    /// Media duration in the media timescale.
    duration: u32,
}

const MOVIE_TIMESCALE: u32 = 1000;

/// Serialise `demuxed` as an MP4 to `out`. Returns nothing; the caller owns the
/// path and the extension.
pub fn write(demuxed: &Demuxed, out: &mut impl Write) -> Result<()> {
    if demuxed.video.is_empty() && demuxed.audio.is_empty() {
        bail!("전송 스트림에서 트랙을 찾지 못했습니다");
    }

    // The sample bytes go down first, so their offsets are known before the
    // moov that points at them is built. ftyp has a fixed size; mdat's payload
    // begins 8 bytes into the box.
    let ftyp = ftyp_box();
    let mut mdat_payload: Vec<u8> = Vec::new();
    let data_start = ftyp.len() as u32 + 8; // after mdat's size+type

    let mut video_track = None;
    if !demuxed.video.is_empty() {
        let offset = data_start + mdat_payload.len() as u32;
        for s in &demuxed.video {
            mdat_payload.extend_from_slice(&s.data);
        }
        video_track = Some(video_track_tables(&demuxed.video, offset));
    }

    let mut audio_track = None;
    if !demuxed.audio.is_empty() {
        let offset = data_start + mdat_payload.len() as u32;
        for s in &demuxed.audio {
            mdat_payload.extend_from_slice(&s.data);
        }
        audio_track = Some(audio_track_tables(
            &demuxed.audio,
            offset,
            demuxed.sample_rate.max(1),
        ));
    }

    // A 32-bit stco cannot address past 4 GiB; a single download that large is
    // not something this path produces, but refuse rather than write a file
    // that points into the wrong place.
    if (data_start as u64 + mdat_payload.len() as u64) > u32::MAX as u64 {
        bail!("파일이 너무 커서 MP4로 담을 수 없습니다");
    }

    out.write_all(&ftyp)?;
    out.write_all(&((mdat_payload.len() as u32 + 8).to_be_bytes()))?;
    out.write_all(b"mdat")?;
    out.write_all(&mdat_payload)?;

    let moov = moov_box(demuxed, video_track.as_ref(), audio_track.as_ref());
    out.write_all(&moov)?;
    out.flush()?;
    Ok(())
}

/// Build the video track's tables from its samples (decode order).
fn video_track_tables(samples: &[Sample], chunk_offset: u32) -> Track {
    let sizes: Vec<u32> = samples.iter().map(|s| s.data.len() as u32).collect();

    // Decode durations: the gap to the next frame's decode time. The last frame
    // has no successor, so it reuses the previous gap (a single frame's error at
    // the very end is inaudible and invisible).
    let mut deltas: Vec<u32> = Vec::with_capacity(samples.len());
    for i in 0..samples.len() {
        let d = if i + 1 < samples.len() {
            samples[i + 1].decode_ms.saturating_sub(samples[i].decode_ms) as u32
        } else {
            *deltas.last().unwrap_or(&33)
        };
        deltas.push(d);
    }
    let stts = run_length(&deltas);
    let duration: u32 = deltas.iter().sum();

    // Composition offset: presentation minus decode. Zero for a stream without
    // reordering; a run-length of zeros still round-trips, but an all-zero ctts
    // is dropped entirely.
    let offsets: Vec<u32> = samples
        .iter()
        .map(|s| s.time_ms.saturating_sub(s.decode_ms) as u32)
        .collect();
    let ctts = if offsets.iter().any(|&o| o != 0) {
        run_length(&offsets)
    } else {
        Vec::new()
    };

    let sync: Vec<u32> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.keyframe)
        .map(|(i, _)| i as u32 + 1)
        .collect();

    Track {
        chunk_offset,
        sizes,
        stts,
        ctts,
        // If every frame is a keyframe, an stss listing all of them is the same
        // as none; leave it out so the player treats all as sync.
        sync: if sync.len() == samples.len() { Vec::new() } else { sync },
        timescale: MOVIE_TIMESCALE,
        duration,
    }
}

/// Build the audio track's tables. Every AAC frame is 1024 samples, so the
/// media timescale is the sample rate and one stts run covers the lot — exact,
/// with none of the drift a millisecond clock would accumulate.
fn audio_track_tables(samples: &[Sample], chunk_offset: u32, sample_rate: u32) -> Track {
    let sizes: Vec<u32> = samples.iter().map(|s| s.data.len() as u32).collect();
    let stts = vec![(samples.len() as u32, 1024u32)];
    let duration = samples.len() as u32 * 1024;
    Track {
        chunk_offset,
        sizes,
        stts,
        ctts: Vec::new(),
        sync: Vec::new(),
        timescale: sample_rate,
        duration,
    }
}

/// Collapse a per-sample list into (count, value) runs.
fn run_length(values: &[u32]) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &v in values {
        match runs.last_mut() {
            Some(last) if last.1 == v => last.0 += 1,
            _ => runs.push((1, v)),
        }
    }
    runs
}

// ---- box construction -------------------------------------------------------

/// A box: 4-byte size, 4-byte type, payload.
fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + payload.len());
    v.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
    v.extend_from_slice(kind);
    v.extend_from_slice(payload);
    v
}

/// A full box: box with a leading version(1) + flags(3).
fn full(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(4 + payload.len());
    body.push(version);
    body.extend_from_slice(&flags.to_be_bytes()[1..]);
    body.extend_from_slice(payload);
    atom(kind, &body)
}

fn ftyp_box() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(b"isom");
    p.extend_from_slice(&0x200u32.to_be_bytes());
    for brand in [b"isom", b"iso2", b"avc1", b"mp41"] {
        p.extend_from_slice(brand);
    }
    atom(b"ftyp", &p)
}

const MATRIX: [u32; 9] = [
    0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000,
];

fn moov_box(demuxed: &Demuxed, video: Option<&Track>, audio: Option<&Track>) -> Vec<u8> {
    let mut track_id = 0u32;
    let movie_duration = [video, audio]
        .iter()
        .flatten()
        .map(|t| (t.duration as u64 * MOVIE_TIMESCALE as u64 / t.timescale as u64) as u32)
        .max()
        .unwrap_or(0);

    let mut children = mvhd(movie_duration, track_count(video, audio) + 1);
    if let Some(t) = video {
        track_id += 1;
        children.extend_from_slice(&video_trak(demuxed, t, track_id));
    }
    if let Some(t) = audio {
        track_id += 1;
        children.extend_from_slice(&audio_trak(demuxed, t, track_id));
    }
    atom(b"moov", &children)
}

fn track_count(video: Option<&Track>, audio: Option<&Track>) -> u32 {
    video.is_some() as u32 + audio.is_some() as u32
}

fn mvhd(duration: u32, next_track_id: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // creation
    p.extend_from_slice(&0u32.to_be_bytes()); // modification
    p.extend_from_slice(&MOVIE_TIMESCALE.to_be_bytes());
    p.extend_from_slice(&duration.to_be_bytes());
    p.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
    p.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
    p.extend_from_slice(&0u16.to_be_bytes()); // reserved
    p.extend_from_slice(&[0u8; 8]); // reserved
    for m in MATRIX {
        p.extend_from_slice(&m.to_be_bytes());
    }
    p.extend_from_slice(&[0u8; 24]); // pre_defined
    p.extend_from_slice(&next_track_id.to_be_bytes());
    full(b"mvhd", 0, 0, &p)
}

fn video_trak(demuxed: &Demuxed, t: &Track, id: u32) -> Vec<u8> {
    let w = demuxed.width.max(1);
    let h = demuxed.height.max(1);
    let movie_dur = (t.duration as u64 * MOVIE_TIMESCALE as u64 / t.timescale as u64) as u32;
    let tkhd = tkhd(id, movie_dur, w, h, 0);
    // AV1 is 'av01' with an 'av1C' config box; H.264 is 'avc1' with 'avcC'. Both are
    // the same VisualSampleEntry otherwise. AVPlayer plays AV1 on devices with an AV1
    // decoder (iPhone 15 Pro+); H.264 plays everywhere.
    let (fourcc, config): (&[u8; 4], Vec<u8>) = if demuxed.video_av1 {
        (b"av01", atom(b"av1C", &demuxed.avcc))
    } else {
        (b"avc1", atom(b"avcC", &demuxed.avcc))
    };
    let entry = visual_entry(fourcc, w as u16, h as u16, &config);
    let stbl = stbl(&entry, t);
    let minf = atom(b"minf", &[vmhd(), dinf(), stbl].concat());
    let mdia = atom(
        b"mdia",
        &[mdhd(t.timescale, t.duration), hdlr(b"vide", "VideoHandler"), minf].concat(),
    );
    atom(b"trak", &[tkhd, mdia].concat())
}

fn audio_trak(demuxed: &Demuxed, t: &Track, id: u32) -> Vec<u8> {
    let movie_dur = (t.duration as u64 * MOVIE_TIMESCALE as u64 / t.timescale as u64) as u32;
    let tkhd = tkhd(id, movie_dur, 0, 0, 0x0100);
    let esds = esds(&demuxed.asc);
    let entry = mp4a_entry(demuxed.channels.max(1) as u16, demuxed.sample_rate.max(1), &esds);
    let stbl = stbl(&entry, t);
    let minf = atom(b"minf", &[smhd(), dinf(), stbl].concat());
    let mdia = atom(
        b"mdia",
        &[mdhd(t.timescale, t.duration), hdlr(b"soun", "SoundHandler"), minf].concat(),
    );
    atom(b"trak", &[tkhd, mdia].concat())
}

fn tkhd(id: u32, duration: u32, width: u32, height: u32, volume: u16) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // creation
    p.extend_from_slice(&0u32.to_be_bytes()); // modification
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes()); // reserved
    p.extend_from_slice(&duration.to_be_bytes());
    p.extend_from_slice(&[0u8; 8]); // reserved
    p.extend_from_slice(&0i16.to_be_bytes()); // layer
    p.extend_from_slice(&0i16.to_be_bytes()); // alternate_group
    p.extend_from_slice(&volume.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes()); // reserved
    for m in MATRIX {
        p.extend_from_slice(&m.to_be_bytes());
    }
    // 16.16 fixed-point width/height.
    p.extend_from_slice(&(width << 16).to_be_bytes());
    p.extend_from_slice(&(height << 16).to_be_bytes());
    // Enabled | in movie | in preview.
    full(b"tkhd", 0, 0x7, &p)
}

fn mdhd(timescale: u32, duration: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // creation
    p.extend_from_slice(&0u32.to_be_bytes()); // modification
    p.extend_from_slice(&timescale.to_be_bytes());
    p.extend_from_slice(&duration.to_be_bytes());
    p.extend_from_slice(&0x55c4u16.to_be_bytes()); // language 'und'
    p.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
    full(b"mdhd", 0, 0, &p)
}

fn hdlr(handler: &[u8; 4], name: &str) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
    p.extend_from_slice(handler);
    p.extend_from_slice(&[0u8; 12]); // reserved
    p.extend_from_slice(name.as_bytes());
    p.push(0);
    full(b"hdlr", 0, 0, &p)
}

fn vmhd() -> Vec<u8> {
    full(b"vmhd", 0, 1, &[0u8; 8]) // flags=1, graphicsmode + opcolor all zero
}

fn smhd() -> Vec<u8> {
    full(b"smhd", 0, 0, &[0u8; 4]) // balance + reserved
}

fn dinf() -> Vec<u8> {
    // One self-contained data reference: a 'url ' with flags=1 (data is here).
    let url = full(b"url ", 0, 1, &[]);
    let mut dref_payload = 1u32.to_be_bytes().to_vec(); // entry_count
    dref_payload.extend_from_slice(&url);
    let dref = full(b"dref", 0, 0, &dref_payload);
    atom(b"dinf", &dref)
}

fn stbl(sample_entry: &[u8], t: &Track) -> Vec<u8> {
    let mut stsd_payload = 1u32.to_be_bytes().to_vec(); // entry_count
    stsd_payload.extend_from_slice(sample_entry);
    let stsd = full(b"stsd", 0, 0, &stsd_payload);

    let stts = table(b"stts", &t.stts, |(count, val)| {
        [count.to_be_bytes(), val.to_be_bytes()].concat()
    });

    let mut boxes = vec![stsd, stts];

    if !t.ctts.is_empty() {
        boxes.push(table(b"ctts", &t.ctts, |(count, val)| {
            [count.to_be_bytes(), val.to_be_bytes()].concat()
        }));
    }
    if !t.sync.is_empty() {
        boxes.push(table(b"stss", &t.sync, |n| n.to_be_bytes().to_vec()));
    }

    // One chunk holds every sample of the track.
    let stsc_entry = [1u32, t.sizes.len() as u32, 1u32]
        .iter()
        .flat_map(|v| v.to_be_bytes())
        .collect::<Vec<u8>>();
    let mut stsc_payload = 1u32.to_be_bytes().to_vec();
    stsc_payload.extend_from_slice(&stsc_entry);
    boxes.push(full(b"stsc", 0, 0, &stsc_payload));

    // stsz with an explicit size per sample (sample_size field zero).
    let mut stsz_payload = 0u32.to_be_bytes().to_vec(); // sample_size
    stsz_payload.extend_from_slice(&(t.sizes.len() as u32).to_be_bytes());
    for s in &t.sizes {
        stsz_payload.extend_from_slice(&s.to_be_bytes());
    }
    boxes.push(full(b"stsz", 0, 0, &stsz_payload));

    // Single chunk, single offset.
    let mut stco_payload = 1u32.to_be_bytes().to_vec();
    stco_payload.extend_from_slice(&t.chunk_offset.to_be_bytes());
    boxes.push(full(b"stco", 0, 0, &stco_payload));

    atom(b"stbl", &boxes.concat())
}

/// A full box holding a count-prefixed list of entries.
fn table<T: Copy>(kind: &[u8; 4], entries: &[T], each: impl Fn(T) -> Vec<u8>) -> Vec<u8> {
    let mut p = (entries.len() as u32).to_be_bytes().to_vec();
    for &e in entries {
        p.extend_from_slice(&each(e));
    }
    full(kind, 0, 0, &p)
}

/// A VisualSampleEntry (`avc1`/`av01`/…) — same layout for every video codec; only the
/// fourcc and the trailing config box (`avcC`/`av1C`) differ. `config` is the already-
/// boxed config record.
fn visual_entry(fourcc: &[u8; 4], width: u16, height: u16, config: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0u8; 6]); // reserved
    p.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    p.extend_from_slice(&[0u8; 16]); // pre_defined + reserved + pre_defined[3]
    p.extend_from_slice(&width.to_be_bytes());
    p.extend_from_slice(&height.to_be_bytes());
    p.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // horiz res 72dpi
    p.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // vert res 72dpi
    p.extend_from_slice(&0u32.to_be_bytes()); // reserved
    p.extend_from_slice(&1u16.to_be_bytes()); // frame_count
    p.extend_from_slice(&[0u8; 32]); // compressorname
    p.extend_from_slice(&0x0018u16.to_be_bytes()); // depth
    p.extend_from_slice(&0xffffu16.to_be_bytes()); // pre_defined -1
    p.extend_from_slice(config);
    atom(fourcc, &p)
}

fn mp4a_entry(channels: u16, sample_rate: u32, esds: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0u8; 6]); // reserved
    p.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    p.extend_from_slice(&[0u8; 8]); // reserved
    p.extend_from_slice(&channels.to_be_bytes());
    p.extend_from_slice(&16u16.to_be_bytes()); // sample size
    p.extend_from_slice(&0u16.to_be_bytes()); // pre_defined
    p.extend_from_slice(&0u16.to_be_bytes()); // reserved
    p.extend_from_slice(&(sample_rate << 16).to_be_bytes()); // 16.16
    p.extend_from_slice(esds);
    atom(b"mp4a", &p)
}

/// The esds box: an ES descriptor wrapping the AAC AudioSpecificConfig.
fn esds(asc: &[u8]) -> Vec<u8> {
    // DecoderSpecificInfo (tag 0x05) holds the ASC verbatim.
    let dsi = descriptor(0x05, asc);

    // DecoderConfigDescriptor (0x04).
    let mut dcd = Vec::new();
    dcd.push(0x40); // objectTypeIndication: Audio ISO/IEC 14496-3 (AAC)
    dcd.push(0x15); // streamType=5 (audio) <<2 | upStream=0 <<1 | reserved=1
    dcd.extend_from_slice(&[0, 0, 0]); // bufferSizeDB
    dcd.extend_from_slice(&0u32.to_be_bytes()); // maxBitrate
    dcd.extend_from_slice(&0u32.to_be_bytes()); // avgBitrate
    dcd.extend_from_slice(&dsi);
    let dcd = descriptor(0x04, &dcd);

    // SLConfigDescriptor (0x06): predefined value 2 (no SL header).
    let sl = descriptor(0x06, &[0x02]);

    // ES_Descriptor (0x03): ES_ID + flags, then the two child descriptors.
    let mut es = Vec::new();
    es.extend_from_slice(&0u16.to_be_bytes()); // ES_ID
    es.push(0); // flags
    es.extend_from_slice(&dcd);
    es.extend_from_slice(&sl);
    let es = descriptor(0x03, &es);

    full(b"esds", 0, 0, &es)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::ts::{Demuxed, Sample};

    fn sample(byte: u8, len: usize, time_ms: u64, decode_ms: u64, keyframe: bool) -> Sample {
        Sample { data: vec![byte; len], time_ms, decode_ms, keyframe }
    }

    /// Walk the top-level boxes, returning (type, offset, size).
    fn top_boxes(mp4: &[u8]) -> Vec<([u8; 4], usize, usize)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i + 8 <= mp4.len() {
            let size = u32::from_be_bytes(mp4[i..i + 4].try_into().unwrap()) as usize;
            let mut kind = [0u8; 4];
            kind.copy_from_slice(&mp4[i + 4..i + 8]);
            out.push((kind, i, size));
            if size == 0 {
                break;
            }
            i += size;
        }
        out
    }

    fn demuxed() -> Demuxed {
        // Three video frames in decode order with a B-frame reorder: the second
        // decoded frame is shown last, so it needs a composition offset.
        Demuxed {
            avcc: vec![1, 0x64, 0, 0x1f, 0xff, 0xe1, 0, 4, 0x67, 0x64, 0, 0x1f, 1, 0, 4, 0x68],
            video_av1: false,
            width: 320,
            height: 240,
            video: vec![
                sample(0xAA, 100, 0, 0, true),
                sample(0xBB, 60, 80, 40, false),
                sample(0xCC, 50, 40, 80, false),
            ],
            asc: vec![0x12, 0x10],
            sample_rate: 44100,
            channels: 2,
            audio: vec![sample(0xDD, 20, 0, 0, true), sample(0xEE, 20, 23, 23, true)],
        }
    }

    #[test]
    fn the_top_level_boxes_tile_the_whole_file() {
        let mut out = Vec::new();
        write(&demuxed(), &mut out).unwrap();
        let boxes = top_boxes(&out);
        let kinds: Vec<[u8; 4]> = boxes.iter().map(|b| b.0).collect();
        assert_eq!(kinds, vec![*b"ftyp", *b"mdat", *b"moov"], "got {kinds:?}");
        // Sizes must cover the file exactly with no gap or overrun.
        let end = boxes.last().map(|b| b.1 + b.2).unwrap();
        assert_eq!(end, out.len());
    }

    #[test]
    fn a_chunk_offset_points_at_the_samples_it_names() {
        let mut out = Vec::new();
        write(&demuxed(), &mut out).unwrap();
        // The video track's first sample is 100 bytes of 0xAA; the writer laid
        // video down first, so mdat's payload starts with it. Find mdat and
        // confirm its payload begins where a 32-bit reader would look.
        let mdat = top_boxes(&out).into_iter().find(|b| &b.0 == b"mdat").unwrap();
        let payload_start = mdat.1 + 8;
        assert_eq!(&out[payload_start..payload_start + 4], &[0xAA; 4]);
        // Audio (0xDD) sits right after the 210 bytes of video.
        let audio_start = payload_start + 100 + 60 + 50;
        assert_eq!(&out[audio_start..audio_start + 4], &[0xDD; 4]);
    }

    #[test]
    fn a_reordered_stream_gets_a_ctts_and_a_plain_one_does_not() {
        // With B-frames, offsets differ from zero → ctts present.
        let d = demuxed();
        let track = video_track_tables(&d.video, 40);
        assert!(!track.ctts.is_empty());
        // Audio is all-sync, in-order → no ctts, no stss.
        let audio = audio_track_tables(&d.audio, 0, 44100);
        assert!(audio.ctts.is_empty() && audio.sync.is_empty());
    }

    #[test]
    fn audio_only_and_video_only_both_write() {
        let mut audio_only = demuxed();
        audio_only.video = Vec::new();
        let mut out = Vec::new();
        write(&audio_only, &mut out).unwrap();
        assert!(top_boxes(&out).iter().any(|b| &b.0 == b"moov"));

        let mut video_only = demuxed();
        video_only.audio = Vec::new();
        let mut out2 = Vec::new();
        write(&video_only, &mut out2).unwrap();
        assert!(top_boxes(&out2).iter().any(|b| &b.0 == b"moov"));
    }
}

/// An MPEG-4 descriptor: tag, length (expandable), payload. The ASC is a few
/// bytes, so a single length byte always suffices here.
fn descriptor(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(2 + payload.len());
    v.push(tag);
    // The expandable size encoding uses 7 bits per byte, high bit "more". Our
    // payloads are small; emit as many bytes as the length needs.
    let mut len = payload.len();
    let mut bytes = vec![(len & 0x7f) as u8];
    len >>= 7;
    while len > 0 {
        bytes.push(0x80 | (len & 0x7f) as u8);
        len >>= 7;
    }
    bytes.reverse();
    v.extend_from_slice(&bytes);
    v.extend_from_slice(payload);
    v
}
