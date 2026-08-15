//! Writing a Matroska file.
//!
//! This exists because the two streams YouTube sends have to become one file,
//! and the alternative on Windows — Media Foundation — only muxes H.264 and
//! AAC reliably. Accepting that would mean downloading H.264 for everything:
//! measured on one clip at 1080p60, 246 MB instead of 119 MB for the same
//! picture in AV1. Writing the container ourselves is the difference.
//!
//! Matroska is the right container for that job because it takes any codec.
//! MP4 would need a per-codec sample entry and a rewritten sample table; this
//! needs a codec identifier and the frames.
//!
//! Frames are written a cluster at a time so a three-hour film does not have to
//! fit in memory. The two lengths that are only known at the end — the segment
//! and the duration — are left as fixed-width placeholders and filled in by
//! seeking back, rather than by holding the file open in memory or by declaring
//! an unknown length that some players handle poorly.

use crate::download::ebml::*;
#[cfg(test)]
use crate::download::ebml;
use anyhow::Result;
use std::io::{Seek, SeekFrom, Write};

/// What kind of stream a track carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
}

/// Everything the container needs to know about one stream.
#[derive(Clone, Debug)]
pub struct TrackSpec {
    pub kind: Kind,
    /// Matroska's name for the codec: `V_AV01`, `V_VP9`, `A_OPUS`, `A_AAC`.
    pub codec_id: String,
    /// The codec's own setup bytes, verbatim. A decoder cannot start without
    /// them and Matroska does not interpret them.
    pub codec_private: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sample_rate: f64,
    pub channels: u32,
    /// Nominal frame or packet length, in nanoseconds. Zero when unknown.
    pub default_duration_ns: u64,
}

impl TrackSpec {
    pub fn video(codec_id: &str, width: u32, height: u32, codec_private: Vec<u8>) -> Self {
        Self {
            kind: Kind::Video,
            codec_id: codec_id.into(),
            codec_private,
            width,
            height,
            sample_rate: 0.0,
            channels: 0,
            default_duration_ns: 0,
        }
    }

    pub fn audio(codec_id: &str, sample_rate: f64, channels: u32, codec_private: Vec<u8>) -> Self {
        Self {
            kind: Kind::Audio,
            codec_id: codec_id.into(),
            codec_private,
            width: 0,
            height: 0,
            sample_rate,
            channels,
            default_duration_ns: 0,
        }
    }
}

/// One frame, addressed to a track.
#[derive(Clone, Debug)]
pub struct Frame {
    /// 1-based, matching the order tracks were declared.
    pub track: u64,
    /// When this frame is shown. This is what the block states: Matroska times
    /// are presentation times.
    pub time_ms: u64,
    /// When it is decoded, which is the order frames must be written in.
    ///
    /// A codec may hold a frame back and decode it before the ones it is shown
    /// after. Matroska says nothing about decoding order — a reader hands blocks
    /// to the decoder in the order it finds them — so the order here is the
    /// decoder's, and only the stated time is the viewer's.
    pub decode_ms: u64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

/// Timestamps are milliseconds, which is what both source containers give and
/// what a block's 16-bit relative field can express over a long cluster.
const TIMESTAMP_SCALE_NS: u64 = 1_000_000;

/// A cluster is closed when either bound is reached.
///
/// The block's timestamp is a signed 16-bit offset from the cluster, so a
/// cluster may not span more than about 32 seconds whatever else happens. Five
/// is the usual choice: short enough to seek into, long enough that the
/// per-cluster overhead disappears.
const CLUSTER_MS: u64 = 5_000;
const CLUSTER_BYTES: usize = 4 * 1024 * 1024;

/// The point at which a cluster must end whether a keyframe arrived or not.
/// Past this a block's offset no longer fits in the signed 16 bits it has.
const MAX_CLUSTER_SPAN_MS: u64 = 30_000;

/// Reserved for the segment length, which is only known at the end.
const SEGMENT_SIZE_WIDTH: usize = 8;

pub struct Writer<W: Write + Seek> {
    out: W,
    /// Where the segment's length placeholder sits.
    segment_size_at: u64,
    /// Where the segment body begins, which is what lengths are measured from.
    segment_body_at: u64,
    /// Where the duration's placeholder sits.
    duration_at: u64,
    cluster: Cluster,
    cues: Vec<(u64, u64, u64)>,
    last_time_ms: u64,
    video_track: Option<u64>,
}

#[derive(Default)]
struct Cluster {
    /// Absolute time of the cluster, which its blocks are relative to.
    base_ms: u64,
    blocks: Vec<u8>,
    started: bool,
}

/// Whether these tracks make a WebM, or only a Matroska.
///
/// WebM is Matroska narrowed to a short list of codecs, and everything written
/// here is already inside that list when the codecs are. Saying so in the header
/// is what lets a browser — and Windows' own player — open the file at all:
/// both refuse a file that calls itself `matroska`, and neither looks inside to
/// find out that it could have played it. YouTube's own streams are VP9 or AV1
/// with Opus, so this is the usual case, not the exception.
pub fn is_webm(tracks: &[TrackSpec]) -> bool {
    tracks.iter().all(|track| {
        matches!(
            track.codec_id.as_str(),
            "V_VP8" | "V_VP9" | "V_AV01" | "A_OPUS" | "A_VORBIS"
        )
    })
}

/// What the file should call itself, given what is in it.
fn doc_type(tracks: &[TrackSpec]) -> &'static str {
    if is_webm(tracks) { "webm" } else { "matroska" }
}

/// The extension that matches [`doc_type`], so the name does not lie either.
pub fn extension(tracks: &[TrackSpec]) -> &'static str {
    if is_webm(tracks) { "webm" } else { "mkv" }
}

impl<W: Write + Seek> Writer<W> {
    /// Open a file and declare its tracks. Tracks are numbered from one in the
    /// order given, which is what [`Frame::track`] refers to.
    pub fn new(mut out: W, tracks: &[TrackSpec]) -> Result<Self> {
        let mut header = Vec::new();
        put_uint(&mut header, EBML_VERSION, 1);
        put_uint(&mut header, EBML_READ_VERSION, 1);
        put_uint(&mut header, EBML_MAX_ID_LENGTH, 4);
        put_uint(&mut header, EBML_MAX_SIZE_LENGTH, 8);
        put_string(&mut header, DOC_TYPE, doc_type(tracks));
        put_uint(&mut header, DOC_TYPE_VERSION, 4);
        put_uint(&mut header, DOC_TYPE_READ_VERSION, 2);

        let mut file = Vec::new();
        put_element(&mut file, EBML_HEADER, &header);
        put_id(&mut file, SEGMENT);
        let segment_size_at = file.len() as u64;
        // A fixed-width placeholder, so filling it in later cannot move
        // anything that follows.
        file.push(0x01);
        file.extend_from_slice(&[0u8; SEGMENT_SIZE_WIDTH - 1]);
        let segment_body_at = file.len() as u64;

        let mut info = Vec::new();
        put_uint(&mut info, TIMESTAMP_SCALE, TIMESTAMP_SCALE_NS);
        put_string(&mut info, MUXING_APP, "shard");
        put_string(&mut info, WRITING_APP, "shard");
        put_id(&mut info, DURATION);
        put_size(&mut info, 8);
        // Where the eight duration bytes will sit, measured inside the body
        // first and then placed — the body is still growing at this point, so
        // anything measured against its final length would be wrong.
        let duration_in_body = info.len() as u64;
        info.extend_from_slice(&0f64.to_be_bytes());
        let duration_at =
            segment_body_at + element_header_len(INFO, &info) + duration_in_body;
        put_element(&mut file, INFO, &info);

        let mut entries = Vec::new();
        let mut video_track = None;
        for (index, spec) in tracks.iter().enumerate() {
            let number = index as u64 + 1;
            if spec.kind == Kind::Video {
                video_track.get_or_insert(number);
            }
            put_element(&mut entries, TRACK_ENTRY, &track_entry(number, spec));
        }
        put_element(&mut file, TRACKS, &entries);

        out.write_all(&file)?;
        Ok(Self {
            out,
            segment_size_at,
            segment_body_at,
            duration_at,
            cluster: Cluster::default(),
            cues: Vec::new(),
            last_time_ms: 0,
            video_track,
        })
    }

    /// Add a frame. Frames must arrive in time order across all tracks.
    pub fn add(&mut self, frame: &Frame) -> Result<()> {
        // A new cluster starts on a video keyframe, so every cluster can be
        // decoded from its own beginning — which is what seeking needs.
        let opens = Some(frame.track) == self.video_track && frame.keyframe;
        let span = frame.time_ms.saturating_sub(self.cluster.base_ms);
        let full = span >= CLUSTER_MS || self.cluster.blocks.len() >= CLUSTER_BYTES;
        // Clusters end at keyframes where possible, so each one can be decoded
        // from its own start — that is what seeking into the middle of a file
        // needs. The hard bound is separate and not negotiable: a block states
        // its time as a signed 16-bit offset from its cluster.
        if !self.cluster.started || (full && opens) || span >= MAX_CLUSTER_SPAN_MS {
            self.flush_cluster()?;
            self.cluster.base_ms = frame.time_ms;
            self.cluster.started = true;
            if opens {
                let at = self.position()? - self.segment_body_at;
                self.cues.push((frame.time_ms, frame.track, at));
            }
        }
        let relative = (frame.time_ms as i64 - self.cluster.base_ms as i64) as i16;

        let mut block = Vec::with_capacity(frame.data.len() + 8);
        put_size(&mut block, frame.track);
        block.extend_from_slice(&relative.to_be_bytes());
        block.push(if frame.keyframe { 0x80 } else { 0x00 });
        block.extend_from_slice(&frame.data);
        put_element(&mut self.cluster.blocks, SIMPLE_BLOCK, &block);

        self.last_time_ms = self.last_time_ms.max(frame.time_ms);
        Ok(())
    }

    /// Close the file: last cluster, cue table, then the two lengths.
    pub fn finish(mut self) -> Result<W> {
        self.flush_cluster()?;

        let mut cues = Vec::new();
        for (time_ms, track, at) in &self.cues {
            let mut positions = Vec::new();
            put_uint(&mut positions, CUE_TRACK, *track);
            put_uint(&mut positions, CUE_CLUSTER_POSITION, *at);
            let mut point = Vec::new();
            put_uint(&mut point, CUE_TIME, *time_ms);
            put_element(&mut point, CUE_TRACK_POSITIONS, &positions);
            put_element(&mut cues, CUE_POINT, &point);
        }
        if !cues.is_empty() {
            let mut block = Vec::new();
            put_element(&mut block, CUES, &cues);
            self.out.write_all(&block)?;
        }

        let end = self.position()?;
        let duration = (self.last_time_ms as f64) + 1.0;
        self.out.seek(SeekFrom::Start(self.duration_at))?;
        self.out.write_all(&duration.to_be_bytes())?;

        // The placeholder is eight bytes wide with its marker already set, so
        // only the seven value bytes are written back.
        let size = end - self.segment_body_at;
        self.out.seek(SeekFrom::Start(self.segment_size_at + 1))?;
        self.out.write_all(&size.to_be_bytes()[1..])?;

        self.out.seek(SeekFrom::Start(end))?;
        Ok(self.out)
    }

    fn flush_cluster(&mut self) -> Result<()> {
        if self.cluster.blocks.is_empty() {
            return Ok(());
        }
        let mut body = Vec::with_capacity(self.cluster.blocks.len() + 16);
        put_uint(&mut body, TIMESTAMP, self.cluster.base_ms);
        body.extend_from_slice(&self.cluster.blocks);
        let mut block = Vec::new();
        put_element(&mut block, CLUSTER, &body);
        self.out.write_all(&block)?;
        self.cluster.blocks.clear();
        Ok(())
    }

    fn position(&mut self) -> Result<u64> {
        Ok(self.out.stream_position()?)
    }
}

fn track_entry(number: u64, spec: &TrackSpec) -> Vec<u8> {
    let mut entry = Vec::new();
    put_uint(&mut entry, TRACK_NUMBER, number);
    put_uint(&mut entry, TRACK_UID, number);
    put_uint(
        &mut entry,
        TRACK_TYPE,
        match spec.kind {
            Kind::Video => TRACK_TYPE_VIDEO,
            Kind::Audio => TRACK_TYPE_AUDIO,
        },
    );
    put_string(&mut entry, CODEC_ID, &spec.codec_id);
    if !spec.codec_private.is_empty() {
        put_element(&mut entry, CODEC_PRIVATE, &spec.codec_private);
    }
    if spec.default_duration_ns > 0 {
        put_uint(&mut entry, DEFAULT_DURATION, spec.default_duration_ns);
    }
    match spec.kind {
        Kind::Video => {
            let mut video = Vec::new();
            put_uint(&mut video, PIXEL_WIDTH, spec.width as u64);
            put_uint(&mut video, PIXEL_HEIGHT, spec.height as u64);
            put_element(&mut entry, VIDEO, &video);
        }
        Kind::Audio => {
            let mut audio = Vec::new();
            put_float(&mut audio, SAMPLING_FREQUENCY, spec.sample_rate);
            put_uint(&mut audio, CHANNELS, spec.channels as u64);
            put_element(&mut entry, AUDIO, &audio);
        }
    }
    entry
}

/// Bytes an element's identifier and length occupy in front of its body.
fn element_header_len(id: Id, body: &[u8]) -> u64 {
    let mut probe = Vec::new();
    put_id(&mut probe, id);
    put_size(&mut probe, body.len() as u64);
    probe.len() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn tracks() -> Vec<TrackSpec> {
        vec![
            TrackSpec::video("V_AV01", 640, 360, vec![0x81, 0x00, 0x0c, 0x00]),
            TrackSpec::audio("A_OPUS", 48_000.0, 2, b"OpusHead".to_vec()),
        ]
    }

    fn frame(track: u64, time_ms: u64, keyframe: bool, data: &[u8]) -> Frame {
        Frame { track, time_ms, decode_ms: time_ms, keyframe, data: data.to_vec() }
    }

    fn build(frames: &[Frame]) -> Vec<u8> {
        let mut writer = Writer::new(Cursor::new(Vec::new()), &tracks()).expect("header");
        for f in frames {
            writer.add(f).expect("frame");
        }
        writer.finish().expect("finish").into_inner()
    }

    fn find(buffer: &[u8], path: &[Id]) -> Option<ebml::Element> {
        // Walk the top level for the first requested id.
        let mut parent;
        let mut at = 0usize;
        loop {
            let element = ebml::read(buffer, at)?;
            if element.id == path[0] {
                parent = element;
                break;
            }
            at = element.next;
            if at >= buffer.len() {
                return None;
            }
        }
        for id in &path[1..] {
            parent = ebml::children(buffer, &parent).into_iter().find(|c| c.id == *id)?;
        }
        Some(parent)
    }

    #[test]
    fn a_file_of_webm_codecs_calls_itself_webm() {
        // The fixture is AV1 with Opus — YouTube's usual pair, and inside what
        // WebM allows. Calling it `matroska` is what stopped browsers and
        // Windows' own player from opening a file they could decode perfectly.
        let file = build(&[frame(1, 0, true, b"key")]);
        let header = ebml::read(&file, 0).expect("header");
        assert_eq!(header.id, EBML_HEADER);
        let doc = ebml::children(&file, &header)
            .into_iter()
            .find(|c| c.id == DOC_TYPE)
            .expect("doctype");
        assert_eq!(ebml::string(&file, &doc), "webm");
    }

    #[test]
    fn a_codec_webm_does_not_allow_stays_matroska() {
        let h264 = TrackSpec::video("V_MPEG4/ISO/AVC", 1920, 1080, Vec::new());
        let aac = TrackSpec::audio("A_AAC", 48_000.0, 2, Vec::new());
        assert!(!is_webm(&[h264.clone(), aac.clone()]));
        assert_eq!(extension(&[h264, aac]), "mkv");

        let av1 = TrackSpec::video("V_AV01", 1920, 1080, Vec::new());
        let opus = TrackSpec::audio("A_OPUS", 48_000.0, 2, Vec::new());
        assert!(is_webm(&[av1.clone(), opus.clone()]));
        assert_eq!(extension(&[av1, opus]), "webm");
    }

    #[test]
    fn the_segment_length_covers_everything_after_it() {
        let file = build(&[frame(1, 0, true, b"key"), frame(2, 0, true, b"snd")]);
        let segment = find(&file, &[SEGMENT]).expect("segment");
        // Not the unknown-length encoding, and it reaches the last byte.
        assert_eq!(segment.end, file.len());
        assert!(segment.end > segment.body);
    }

    #[test]
    fn declares_both_tracks_with_their_codecs() {
        let file = build(&[frame(1, 0, true, b"v")]);
        let tracks = find(&file, &[SEGMENT, TRACKS]).expect("tracks");
        let entries = ebml::children(&file, &tracks);
        assert_eq!(entries.len(), 2);

        let codec_of = |entry: &ebml::Element| {
            let found = ebml::children(&file, entry)
                .into_iter()
                .find(|c| c.id == CODEC_ID)
                .expect("codec id");
            ebml::string(&file, &found)
        };
        assert_eq!(codec_of(&entries[0]), "V_AV01");
        assert_eq!(codec_of(&entries[1]), "A_OPUS");
    }

    #[test]
    fn keeps_the_codec_setup_bytes_verbatim() {
        let file = build(&[frame(1, 0, true, b"v")]);
        let tracks = find(&file, &[SEGMENT, TRACKS]).expect("tracks");
        let entry = ebml::children(&file, &tracks).remove(1);
        let private = ebml::children(&file, &entry)
            .into_iter()
            .find(|c| c.id == CODEC_PRIVATE)
            .expect("codec private");
        assert_eq!(&file[private.body..private.end], b"OpusHead");
    }

    #[test]
    fn a_block_carries_its_track_time_and_payload() {
        let file = build(&[frame(1, 0, true, b"first"), frame(2, 40, false, b"sound")]);
        let cluster = find(&file, &[SEGMENT, CLUSTER]).expect("cluster");
        let blocks: Vec<_> = ebml::children(&file, &cluster)
            .into_iter()
            .filter(|c| c.id == SIMPLE_BLOCK)
            .collect();
        assert_eq!(blocks.len(), 2);

        let second = &file[blocks[1].body..blocks[1].end];
        assert_eq!(second[0], 0x82, "track 2, one-byte length marker");
        assert_eq!(i16::from_be_bytes([second[1], second[2]]), 40);
        assert_eq!(second[3], 0x00, "not a keyframe");
        assert_eq!(&second[4..], b"sound");
    }

    #[test]
    fn a_keyframe_flag_survives() {
        let file = build(&[frame(1, 0, true, b"k")]);
        let cluster = find(&file, &[SEGMENT, CLUSTER]).expect("cluster");
        let block = ebml::children(&file, &cluster)
            .into_iter()
            .find(|c| c.id == SIMPLE_BLOCK)
            .expect("block");
        assert_eq!(file[block.body + 3], 0x80);
    }

    #[test]
    fn long_content_is_split_into_several_clusters() {
        let frames: Vec<_> = (0..6)
            .map(|i| frame(1, i * 4_000, true, b"keyframe-every-four-seconds"))
            .collect();
        let file = build(&frames);
        let clusters = count_top_level(&file, CLUSTER);
        // Twenty seconds of keyframes every four, against a five-second target:
        // it splits at the first keyframe past each boundary.
        assert!(clusters >= 3, "expected several clusters, found {clusters}");
    }

    #[test]
    fn frames_are_written_in_the_order_they_decode_and_timed_by_when_they_show() {
        // A run the way a codec that reorders produces one: the frame shown last
        // is decoded second, because the two after it refer to it.
        let held = |decode: u64, show: u64| Frame {
            track: 1,
            time_ms: show,
            decode_ms: decode,
            keyframe: decode == 0,
            data: vec![decode as u8],
        };
        let file = build(&[held(0, 0), held(33, 132), held(66, 66), held(99, 99)]);

        let cluster = find(&file, &[SEGMENT, CLUSTER]).expect("cluster");
        let base = {
            let kids = ebml::children(&file, &cluster);
            let stamp = kids.iter().find(|c| c.id == TIMESTAMP).expect("cluster time");
            ebml::uint(&file, stamp) as i64
        };
        let times: Vec<i64> = ebml::children(&file, &cluster)
            .into_iter()
            .filter(|c| c.id == SIMPLE_BLOCK)
            .map(|b| {
                let body = &file[b.body..b.end];
                base + i16::from_be_bytes([body[1], body[2]]) as i64
            })
            .collect();

        // Stored in decoding order, each saying when it is shown — so the times
        // step backwards in the file, which is exactly what a reader needs to
        // hand the decoder frames in the order it can use them.
        assert_eq!(times, vec![0, 132, 66, 99]);
    }

    #[test]
    fn every_cluster_timestamp_is_the_base_for_its_blocks() {
        let frames: Vec<_> =
            (0..4).map(|i| frame(1, i * 6_000, true, b"spread out")).collect();
        let file = build(&frames);

        let segment = find(&file, &[SEGMENT]).expect("segment");
        let mut at = segment.body;
        while at < segment.end {
            let element = ebml::read(&file, at).expect("element");
            if element.id == CLUSTER {
                let kids = ebml::children(&file, &element);
                let base = kids.iter().find(|c| c.id == TIMESTAMP).expect("cluster time");
                let base = ebml::uint(&file, base);
                for block in kids.iter().filter(|c| c.id == SIMPLE_BLOCK) {
                    let body = &file[block.body..block.end];
                    let relative = i16::from_be_bytes([body[1], body[2]]) as i64;
                    let absolute = base as i64 + relative;
                    assert!(absolute % 6_000 == 0, "block landed at {absolute}");
                }
            }
            at = element.next;
        }
    }

    #[test]
    fn seek_points_are_recorded_for_keyframes() {
        let frames: Vec<_> = (0..4).map(|i| frame(1, i * 6_000, true, b"key")).collect();
        let file = build(&frames);
        let cues = find(&file, &[SEGMENT, CUES]).expect("cues");
        let points = ebml::children(&file, &cues);
        assert!(points.len() >= 3, "expected a cue per cluster, found {}", points.len());
    }

    #[test]
    fn duration_reaches_the_last_frame() {
        let file = build(&[frame(1, 0, true, b"a"), frame(1, 9_000, true, b"b")]);
        let duration = find(&file, &[SEGMENT, INFO, DURATION]).expect("duration");
        let bytes: [u8; 8] = file[duration.body..duration.end].try_into().expect("f64");
        assert!(f64::from_be_bytes(bytes) >= 9_000.0);
    }

    fn count_top_level(file: &[u8], id: Id) -> usize {
        let segment = find(file, &[SEGMENT]).expect("segment");
        let mut at = segment.body;
        let mut found = 0;
        while at < segment.end {
            let Some(element) = ebml::read(file, at) else { break };
            if element.id == id {
                found += 1;
            }
            if element.next <= at {
                break;
            }
            at = element.next;
        }
        found
    }
}
