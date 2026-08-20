//! From a chosen quality to a finished file.
//!
//! Everything this needs already exists and is tested on its own: [`pull`]
//! holds the conversation, [`mp4`] and [`webm`] read what comes back, [`mkv`]
//! writes the result. This is the wiring, and it is deliberately thin — the
//! parts that can be wrong in interesting ways are the ones with tests around
//! them.
//!
//! One thing here is not available on the phone. Android's muxer will not put
//! Opus in an MP4, so a download there has to match the audio to the video's
//! container and ends up with AAC beside AV1. Writing the container ourselves
//! removes that constraint: the smallest video and the smallest audio can be
//! chosen independently and put in the same file.

use crate::download::pull::{self, Progress, Sink};
use crate::download::sabr::{Template, Track};
use crate::download::{mkv, mp4, webm};
use anyhow::{anyhow, bail, Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// What to fetch and where to put it.
pub struct Job {
    pub template: Template,
    pub video: Track,
    pub audio: Track,
    /// A format that is neither of the two, named as already playing.
    pub decoy: Track,
    /// Used for the file's name, after being made safe.
    pub title: String,
    pub into: PathBuf,
    /// A picture for the file, fetched once it is saved. Empty when the page
    /// named none.
    pub cover: String,
    /// Keep only the sound.
    ///
    /// The video menu is left out of the request, which does not stop the
    /// server sending video but does make it send the smallest there is —
    /// measured at four hundred kilobytes against fifty megabytes. It is
    /// thrown away.
    pub audio_only: bool,
}

/// Fetch both streams and join them. Returns where the file landed.
///
/// Blocks; call it off any thread with a message loop on it.
pub fn run(
    job: &Job,
    on_progress: &mut dyn FnMut(Progress),
    cancelled: &dyn Fn() -> bool,
) -> Result<PathBuf> {
    std::fs::create_dir_all(&job.into).ok();
    let scratch = std::env::temp_dir().join("shard-download");
    std::fs::create_dir_all(&scratch)?;

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let video_path = scratch.join(format!("v-{stamp}"));
    let audio_path = scratch.join(format!("a-{stamp}"));

    let outcome = (|| -> Result<PathBuf> {
        let mut video_sink = FileAt::create(&video_path)?;
        let mut audio_sink = FileAt::create(&audio_path)?;

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()?;
        let mut post = |url: &str, body: &[u8]| -> Result<Vec<u8>> {
            let response = client
                .post(url)
                .header("Content-Type", "application/x-protobuf")
                .header("Origin", "https://www.youtube.com")
                .header("Referer", "https://www.youtube.com/")
                .body(body.to_vec())
                .send()?;
            let status = response.status();
            if !status.is_success() {
                bail!("서버가 {} 로 응답했습니다", status.as_u16());
            }
            Ok(response.bytes()?.to_vec())
        };

        let done = pull::pull(
            &job.template,
            &job.video,
            &job.audio,
            &job.decoy,
            &mut video_sink,
            &mut audio_sink,
            &mut post,
            on_progress,
            cancelled,
            job.audio_only,
        )?;
        video_sink.flush()?;
        audio_sink.flush()?;

        // A short stream is a broken file. One that plays for a while and then
        // stops is worse than one that never appears, so this refuses rather
        // than saving what arrived.
        if !job.audio_only {
            whole("영상", done.video, job.video.bytes)?;
        }
        whole("음성", done.audio, job.audio.bytes)?;

        let saved = if job.audio_only {
            // The stream is already a container a player will open, so it is
            // moved rather than rebuilt — nothing is re-encoded and nothing is
            // repackaged.
            let extension = audio_extension(&std::fs::read(&audio_path)?);
            let output = job.into.join(format!("{}.{extension}", safe_name(&job.title)));
            std::fs::copy(&audio_path, &output)?;
            output
        } else {
            join_into(&video_path, &audio_path, &job.into, &safe_name(&job.title))?
        };

        // A picture for a song, put inside the song.
        //
        // Only for music, and never beside it: a video already shows what it is
        // — the list takes a frame out of the film itself — and a second file
        // for every download is a folder nobody wants to look at. Failure here
        // is not the download's failure; the sound is saved either way.
        if job.audio_only && !job.cover.is_empty() {
            if let Err(e) = keep_cover(&client, &job.cover, &saved) {
                tracing::warn!("could not put the cover in: {e:#}");
            }
        }
        Ok(saved)
    })();

    let _ = std::fs::remove_file(&video_path);
    let _ = std::fs::remove_file(&audio_path);
    outcome
}

/// Fetch a picture and put it into the saved file's own header.
///
/// Inside rather than beside: a music player, Explorer and this program all look
/// in the same place for a cover, and one file is one file.
fn keep_cover(client: &reqwest::blocking::Client, url: &str, saved: &Path) -> Result<()> {
    // The addresses to try, largest first. The biggest is not always there —
    // a song uploaded as a still often has no full-size picture — and what
    // comes back is not always the format the address promises.
    let mut tried = Vec::new();
    for url in also_try(url) {
        match fetch_picture(client, &url) {
            Ok(found) => {
                tried.clear();
                return put_in(found, saved);
            }
            Err(e) => tried.push(format!("{url}: {e}")),
        }
    }
    bail!("{}", tried.join(" / "));
}

/// The same picture at smaller sizes, for when the largest is missing.
fn also_try(url: &str) -> Vec<String> {
    let mut all = vec![url.to_string()];
    if let Some((base, name)) = url.rsplit_once('/') {
        for other in ["hqdefault.jpg", "mqdefault.jpg"] {
            if name != other {
                all.push(format!("{base}/{other}"));
            }
        }
    }
    all
}

fn fetch_picture(client: &reqwest::blocking::Client, url: &str) -> Result<(Vec<u8>, &'static str)> {
    // Asked for as a photograph. Without this the answer to an address ending
    // in `.jpg` is as likely to be AVIF, which no music player would know what
    // to do with once it was inside the file.
    let response = client.get(url).header("Accept", "image/jpeg,image/png;q=0.9").send()?;
    if !response.status().is_success() {
        bail!("{}", response.status().as_u16());
    }
    let bytes = response.bytes()?.to_vec();
    // A cover is tens of kilobytes; anything far larger is not one.
    if bytes.is_empty() || bytes.len() > 4 * 1024 * 1024 {
        bail!("{} 바이트", bytes.len());
    }
    match picture_kind(&bytes) {
        // Only these two go inside a file: they are the two the format names.
        Some(kind @ ("jpg" | "png")) => Ok((bytes, kind)),
        Some(other) => bail!("{other}"),
        None => bail!("그림이 아님"),
    }
}

fn put_in((bytes, kind): (Vec<u8>, &'static str), saved: &Path) -> Result<()> {
    let file = std::fs::read(saved)?;
    let Some(with) = mp4::with_cover(&file, &bytes, kind) else {
        // Opus in WebM, say. Nothing is written rather than a file rewritten
        // into something a player might not open.
        bail!("이 형식에는 그림을 넣을 수 없습니다");
    };
    std::fs::write(saved, with)?;
    Ok(())
}

/// What a picture actually is, by its first bytes.
///
/// Not by the address it came from: an address ending in `.jpg` is served as
/// WebP as often as not, and a file named for something it is not opens in this
/// program — which sniffs — and nowhere else.
pub fn picture_kind(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("jpg");
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("png");
    }
    if bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    // Named so a log line says what actually arrived; nothing puts one of these
    // into a file.
    if bytes.len() > 12 && &bytes[4..8] == b"ftyp" && &bytes[8..12] == b"avif" {
        return Some("avif");
    }
    None
}

/// What to call an audio stream, judged by what it turns out to be.
fn audio_extension(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        // Opus lives in WebM here. Named for the container rather than the
        // codec, because that is what it is.
        "webm"
    } else {
        "m4a"
    }
}

fn whole(what: &str, got: u64, expected: u64) -> Result<()> {
    if got == 0 {
        bail!("{what} 데이터를 받지 못했습니다");
    }
    if expected > 0 && got < expected {
        bail!("{what}이 중간에 끊겼습니다 ({}%)", got * 100 / expected);
    }
    Ok(())
}

/// Read both streams and write one Matroska file.
/// Join the pair into `into/<stem>.<container>`, and say where it landed.
///
/// The extension is decided by what the streams turn out to be rather than fixed
/// in advance: a VP9-and-Opus pair is a WebM and is named one, which is what
/// lets every player — including the one built into Windows, and the browser
/// engine this app already carries — open it.
pub fn join_into(video: &Path, audio: &Path, into: &Path, stem: &str) -> Result<std::path::PathBuf> {
    let video_bytes = std::fs::read(video).context("영상 파일을 읽을 수 없습니다")?;
    let audio_bytes = std::fs::read(audio).context("음성 파일을 읽을 수 없습니다")?;
    let video_stream = read_stream(&video_bytes).context("영상")?;
    let audio_stream = read_stream(&audio_bytes).context("음성")?;
    let specs = [video_stream.spec(), audio_stream.spec()];
    let output = into.join(format!("{stem}.{}", mkv::extension(&specs)));
    join(video, audio, &output)?;
    Ok(output)
}

pub fn join(video: &Path, audio: &Path, output: &Path) -> Result<()> {
    let video_bytes = std::fs::read(video).context("영상 파일을 읽을 수 없습니다")?;
    let audio_bytes = std::fs::read(audio).context("음성 파일을 읽을 수 없습니다")?;

    let video_stream = read_stream(&video_bytes).context("영상")?;
    let audio_stream = read_stream(&audio_bytes).context("음성")?;

    let mut frames: Vec<mkv::Frame> = Vec::new();
    collect(&video_bytes, &video_stream, 1, &mut frames)?;
    collect(&audio_bytes, &audio_stream, 2, &mut frames)?;
    if frames.is_empty() {
        bail!("합칠 프레임이 없습니다");
    }
    // In the order a decoder has to receive them, not the order they are shown
    // in. Matroska carries no decoding times: a reader feeds blocks to the
    // decoder in the order it finds them, and each block says only when its
    // frame appears. Sorting by the showing time put reordered video into the
    // decoder backwards — every frame that refers to a later one arrived before
    // the frame it refers to, which is a picture that breaks up and drifts.
    //
    // A stable sort, so two frames decoded in the same millisecond keep the
    // order their own track had them in.
    frames.sort_by_key(|frame| frame.decode_ms);

    let file = File::create(output).context("저장 파일을 만들 수 없습니다")?;
    let mut writer = mkv::Writer::new(file, &[video_stream.spec(), audio_stream.spec()])?;
    for frame in &frames {
        writer.add(frame)?;
    }
    writer.finish()?;
    Ok(())
}

/// A stream as either reader described it, plus how to read its frames.
enum Stream {
    Mp4(mp4::Stream),
    Webm(webm::Stream),
}

impl Stream {
    fn spec(&self) -> mkv::TrackSpec {
        match self {
            Stream::Mp4(s) => build_spec(
                s.is_video,
                &s.codec_id,
                s.codec_private.clone(),
                s.width,
                s.height,
                s.sample_rate,
                s.channels,
            ),
            Stream::Webm(s) => build_spec(
                s.is_video,
                &s.codec_id,
                s.codec_private.clone(),
                s.width,
                s.height,
                s.sample_rate,
                s.channels,
            ),
        }
    }
}

fn build_spec(
    is_video: bool,
    codec_id: &str,
    codec_private: Vec<u8>,
    width: u32,
    height: u32,
    sample_rate: f64,
    channels: u32,
) -> mkv::TrackSpec {
    if is_video {
        mkv::TrackSpec::video(codec_id, width, height, codec_private)
    } else {
        mkv::TrackSpec::audio(
            codec_id,
            if sample_rate > 0.0 { sample_rate } else { 48_000.0 },
            if channels > 0 { channels } else { 2 },
            codec_private,
        )
    }
}

/// Which of the two containers this is, decided by what opens it.
fn read_stream(bytes: &[u8]) -> Result<Stream> {
    // Matroska announces itself in its first four bytes; anything else here is
    // the fragmented MP4 the other reader handles.
    if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return Ok(Stream::Webm(webm::stream(bytes)?));
    }
    Ok(Stream::Mp4(mp4::stream(bytes)?))
}

fn collect(bytes: &[u8], stream: &Stream, track: u64, out: &mut Vec<mkv::Frame>) -> Result<()> {
    match stream {
        Stream::Mp4(described) => {
            for sample in mp4::samples(bytes, 0) {
                let at = sample.at as usize;
                let end = at
                    .checked_add(sample.len)
                    .filter(|e| *e <= bytes.len())
                    .ok_or_else(|| anyhow!("조각이 파일 밖을 가리킵니다"))?;
                out.push(mkv::Frame {
                    track,
                    time_ms: sample.show_ms(described.timescale),
                    decode_ms: sample.time_ms(described.timescale),
                    keyframe: sample.keyframe,
                    data: bytes[at..end].to_vec(),
                });
            }
        }
        Stream::Webm(described) => {
            for block in webm::blocks(bytes, described.number) {
                let end = block.at + block.len;
                if end > bytes.len() {
                    continue;
                }
                out.push(mkv::Frame {
                    track,
                    time_ms: block.time_ms,
                    // WebM states one time per block and forbids reordering, so
                    // the two are the same thing there.
                    decode_ms: block.time_ms,
                    keyframe: block.keyframe,
                    data: bytes[block.at..end].to_vec(),
                });
            }
        }
    }
    Ok(())
}

/// A file written by position, which is what the download loop needs.
struct FileAt(File);

impl FileAt {
    fn create(path: &Path) -> Result<Self> {
        Ok(Self(File::options().create(true).read(true).write(true).truncate(true).open(path)?))
    }

    fn flush(&mut self) -> Result<()> {
        self.0.flush()?;
        Ok(())
    }
}

impl Sink for FileAt {
    fn write_at(&mut self, at: u64, bytes: &[u8]) -> std::io::Result<()> {
        self.0.seek(SeekFrom::Start(at))?;
        self.0.write_all(bytes)
    }
}

/// Strip what a filesystem will not take, and keep the length sane.
pub fn safe_name(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| if "\\/:*?\"<>|\r\n".contains(c) { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.').trim();
    let short: String = trimmed.chars().take(80).collect();
    if short.trim().is_empty() {
        "video".into()
    } else {
        short.trim().to_string()
    }
}

/// Reading a file is unused here but keeps the import honest for callers that
/// hand this module bytes they already hold.
#[allow(dead_code)]
fn unused(mut file: File) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    file.read_to_end(&mut out)?;
    Ok(out)
}

// ---- the non-YouTube paths: a direct file, or an HLS stream ----------------
//
// YouTube is a captured request replayed with a decoy; these two are the plain
// cases everything else uses. A `Referer` that matches the page is the whole
// trick to not being turned away by a CDN, so both send it.

/// A browser-shaped user agent, so a CDN that sniffs for one is not surprised.
const UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

fn media_client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .user_agent(UA)
        .build()?)
}

/// GET, with the page as `Referer`. Returns the response for streaming.
fn media_get(
    client: &reqwest::blocking::Client,
    url: &str,
    referer: &str,
) -> Result<reqwest::blocking::Response> {
    let mut request = client.get(url);
    if !referer.is_empty() {
        request = request.header("Referer", referer);
    }
    let response = request.send()?;
    if !response.status().is_success() {
        bail!("서버가 {} 로 응답했습니다", response.status().as_u16());
    }
    Ok(response)
}

/// Download a plain progressive file straight to disk.
///
/// The simplest case: one URL, one file, no muxing. The extension is taken from
/// the URL where it has one, defaulting to mp4 — the only container a plain
/// video download tends to be.
pub fn run_direct(
    url: &str,
    referer: &str,
    into: &Path,
    title: &str,
    on_progress: &mut dyn FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<PathBuf> {
    std::fs::create_dir_all(into).ok();
    let client = media_client()?;
    let mut response = media_get(&client, url, referer)?;
    let total = response.content_length().unwrap_or(0);

    let extension = url_extension(url).unwrap_or("mp4");
    let output = into.join(format!("{}.{extension}", safe_name(title)));
    let mut file = File::create(&output)?;

    let mut done: u64 = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancelled() {
            let _ = std::fs::remove_file(&output);
            bail!("취소되었습니다");
        }
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])?;
        done += read as u64;
        on_progress(done, total.max(done));
    }
    file.flush()?;
    if done == 0 {
        let _ = std::fs::remove_file(&output);
        bail!("받은 내용이 없습니다");
    }
    Ok(output)
}

/// Download an HLS stream and join it into one file.
///
/// The master playlist's highest rendition is taken (the page's own player
/// picks silently; here the best is the obvious want). fMP4 segments joined
/// onto their init segment are already a playable MP4, so no muxing is needed
/// for the common case; a stream of MPEG-TS segments is refused for now with a
/// clear message rather than saving a file the in-app player cannot open.
pub fn run_hls(
    manifest_url: &str,
    referer: &str,
    into: &Path,
    title: &str,
    on_progress: &mut dyn FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<PathBuf> {
    use crate::download::hls;
    std::fs::create_dir_all(into).ok();
    let client = media_client()?;

    // The master, then the chosen media playlist. A media playlist has no
    // variants, so `variants` comes back empty and the master URL is used as-is.
    let master = media_get(&client, manifest_url, referer)?.text()?;
    let (media_url, media_text) = if hls::is_master(&master) {
        let variants = hls::variants(&master, manifest_url);
        let best = variants.first().ok_or_else(|| anyhow!("화질을 찾지 못했습니다"))?;
        (best.url.clone(), media_get(&client, &best.url, referer)?.text()?)
    } else {
        (manifest_url.to_string(), master)
    };

    let segments = hls::segments(&media_text, &media_url);
    if segments.is_empty() {
        bail!("스트림에 조각이 없습니다");
    }
    let init = hls::map_init(&media_text, &media_url);

    let count = segments.len() as u64;
    let mut key_cache: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();

    // Sniff the first segment to decide the container. fMP4 is joinable onto its
    // init segment as-is; MPEG-TS has to be demuxed and repackaged.
    let raw_first = fetch_segment(&client, &segments[0], referer, cancelled)?;
    let first = match &segments[0].key {
        Some(key) => decrypt_segment(&client, key, referer, 0, &raw_first, &mut key_cache)?,
        None => raw_first,
    };
    let looks_ts = init.is_none() && first.first() == Some(&0x47);

    // Gather every segment's bytes, decrypting where the playlist says to.
    let mut whole: Vec<u8> = Vec::new();
    if let Some(init_url) = &init {
        let bytes = fetch_segment(
            &client,
            &hls::Segment { url: init_url.clone(), byte_range: None, key: None },
            referer,
            cancelled,
        )?;
        whole.extend_from_slice(&bytes);
    }
    for (index, segment) in segments.iter().enumerate() {
        if cancelled() {
            bail!("취소되었습니다");
        }
        let mut bytes = if index == 0 {
            first.clone()
        } else {
            let raw = fetch_segment(&client, segment, referer, cancelled)?;
            match &segment.key {
                Some(key) => decrypt_segment(&client, key, referer, index as u64, &raw, &mut key_cache)?,
                None => raw,
            }
        };
        whole.append(&mut bytes);
        on_progress(index as u64 + 1, count);
    }

    if looks_ts {
        // Transport stream: pull the tracks out and write a Matroska file the
        // in-app player can open. A raw .ts it cannot.
        return remux_ts(&whole, into, title);
    }

    // fMP4 (or a progressive MP4 served in pieces): the concatenation already
    // is a playable file.
    let output = into.join(format!("{}.mp4", safe_name(title)));
    std::fs::write(&output, &whole)?;
    Ok(output)
}

/// Demux a transport stream and write it back out as Matroska.
fn remux_ts(data: &[u8], into: &Path, title: &str) -> Result<PathBuf> {
    use crate::download::{mkv, ts};
    let demuxed = ts::demux(data)?;

    let mut specs = Vec::new();
    let mut video_track = None;
    let mut audio_track = None;
    if !demuxed.video.is_empty() {
        video_track = Some(specs.len() as u64 + 1);
        specs.push(mkv::TrackSpec::video(
            "V_MPEG4/ISO/AVC",
            demuxed.width.max(1),
            demuxed.height.max(1),
            demuxed.avcc.clone(),
        ));
    }
    if !demuxed.audio.is_empty() {
        audio_track = Some(specs.len() as u64 + 1);
        specs.push(mkv::TrackSpec::audio(
            "A_AAC",
            demuxed.sample_rate.max(1) as f64,
            demuxed.channels.max(1),
            demuxed.asc.clone(),
        ));
    }
    if specs.is_empty() {
        bail!("전송 스트림에서 트랙을 찾지 못했습니다");
    }

    let output = into.join(format!("{}.{}", safe_name(title), mkv::extension(&specs)));
    let mut writer = mkv::Writer::new(File::create(&output)?, &specs)?;

    // Interleave the two tracks by decode time so a reader never has to seek
    // backwards. A simple merge of the two already-ordered runs does it.
    let mut vi = 0;
    let mut ai = 0;
    loop {
        let v = demuxed.video.get(vi);
        let a = demuxed.audio.get(ai);
        let take_video = match (v, a) {
            (Some(v), Some(a)) => v.decode_ms <= a.decode_ms,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if take_video {
            let s = v.unwrap();
            writer.add(&mkv::Frame {
                track: video_track.unwrap(),
                time_ms: s.time_ms,
                decode_ms: s.decode_ms,
                keyframe: s.keyframe,
                data: s.data.clone(),
            })?;
            vi += 1;
        } else {
            let s = a.unwrap();
            writer.add(&mkv::Frame {
                track: audio_track.unwrap(),
                time_ms: s.time_ms,
                decode_ms: s.decode_ms,
                keyframe: true,
                data: s.data.clone(),
            })?;
            ai += 1;
        }
    }
    writer.finish()?;
    Ok(output)
}

/// One segment's bytes, honouring an `#EXT-X-BYTERANGE` when it has one.
fn fetch_segment(
    client: &reqwest::blocking::Client,
    segment: &crate::download::hls::Segment,
    referer: &str,
    _cancelled: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    let mut request = client.get(&segment.url);
    if !referer.is_empty() {
        request = request.header("Referer", referer);
    }
    if let Some((len, offset)) = segment.byte_range {
        request = request.header("Range", format!("bytes={}-{}", offset, offset + len - 1));
    }
    let response = request.send()?;
    if !response.status().is_success() {
        bail!("조각을 받지 못했습니다 ({})", response.status().as_u16());
    }
    Ok(response.bytes()?.to_vec())
}

/// Decrypt an AES-128-CBC segment. The IV is the one the playlist pins, or the
/// segment's sequence number when it pins none.
fn decrypt_segment(
    client: &reqwest::blocking::Client,
    key: &crate::download::hls::KeyRef,
    referer: &str,
    sequence: u64,
    data: &[u8],
    cache: &mut std::collections::HashMap<String, Vec<u8>>,
) -> Result<Vec<u8>> {
    if !key.method.eq_ignore_ascii_case("AES-128") {
        bail!("지원하지 않는 암호화 방식입니다: {}", key.method);
    }
    let bytes = if let Some(k) = cache.get(&key.uri) {
        k.clone()
    } else {
        let k = media_get(client, &key.uri, referer)?.bytes()?.to_vec();
        cache.insert(key.uri.clone(), k.clone());
        k
    };
    if bytes.len() != 16 {
        bail!("암호 키 길이가 올바르지 않습니다");
    }

    let iv = key.iv.unwrap_or_else(|| {
        let mut iv = [0u8; 16];
        iv[8..].copy_from_slice(&sequence.to_be_bytes());
        iv
    });

    let mut key16 = [0u8; 16];
    key16.copy_from_slice(&bytes);
    cbc_decrypt(&key16, &iv, data)
}

/// AES-128-CBC, with the PKCS7 padding removed — the shape every HLS
/// `#EXT-X-KEY:METHOD=AES-128` segment is in.
fn cbc_decrypt(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::generic_array::GenericArray;
    use aes::cipher::{BlockDecrypt, KeyInit};
    if data.is_empty() || data.len() % 16 != 0 {
        bail!("암호문 길이가 블록에 맞지 않습니다");
    }
    let cipher = aes::Aes128::new(GenericArray::from_slice(key));
    let mut out = Vec::with_capacity(data.len());
    let mut prev = *iv;
    for chunk in data.chunks_exact(16) {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        for i in 0..16 {
            out.push(block[i] ^ prev[i]);
        }
        prev.copy_from_slice(chunk);
    }
    // PKCS7: a final byte in 1..=16 says how many padding bytes to drop.
    if let Some(&pad) = out.last() {
        let pad = pad as usize;
        if (1..=16).contains(&pad) && pad <= out.len() {
            out.truncate(out.len() - pad);
        }
    }
    Ok(out)
}

/// The extension in a URL's path, lower-cased, without the dot. `None` when the
/// last path segment has no extension.
fn url_extension(url: &str) -> Option<&str> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let last = path.rsplit('/').next().unwrap_or(path);
    let dot = last.rfind('.')?;
    let ext = &last[dot + 1..];
    if ext.is_empty() || ext.len() > 4 {
        None
    } else {
        Some(ext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn aes_128_cbc_matches_the_nist_vectors() {
        // NIST SP 800-38A, F.2.2 CBC-AES128.Decrypt, first two blocks. The
        // plaintext ends in 0x51, which is not a valid PKCS7 pad length, so no
        // bytes are stripped and the whole 32 come back.
        let key: [u8; 16] = hex("2b7e151628aed2a6abf7158809cf4f3c").try_into().unwrap();
        let iv: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let cipher = hex("7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2");
        let plain = hex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51");
        assert_eq!(cbc_decrypt(&key, &iv, &cipher).unwrap(), plain);
    }

    #[test]
    fn pkcs7_padding_is_stripped() {
        // One block of 0x10 bytes is all padding: AES-CBC of 16 pad bytes
        // decrypts to nothing after unpadding.
        let key = [0u8; 16];
        let iv = [0u8; 16];
        use aes::cipher::generic_array::GenericArray;
        use aes::cipher::{BlockEncrypt, KeyInit};
        let mut block = GenericArray::clone_from_slice(&[0x10u8; 16]);
        aes::Aes128::new(GenericArray::from_slice(&key)).encrypt_block(&mut block);
        assert!(cbc_decrypt(&key, &iv, &block).unwrap().is_empty());
    }

    #[test]
    fn an_extension_is_read_from_a_url_past_its_query() {
        assert_eq!(url_extension("https://x.com/a/video.mp4?token=1"), Some("mp4"));
        assert_eq!(url_extension("https://x.com/a/list.m3u8"), Some("m3u8"));
        assert_eq!(url_extension("https://x.com/a/stream"), None);
    }

    #[test]
    fn a_picture_is_named_for_what_it_actually_is() {
        assert_eq!(picture_kind(&[0xff, 0xd8, 0xff, 0xe0]), Some("jpg"));
        assert_eq!(picture_kind(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0]), Some("png"));
        let mut webp = b"RIFF    WEBPVP8 ".to_vec();
        webp.push(0);
        assert_eq!(picture_kind(&webp), Some("webp"));
        // An address ending in .jpg that answers with a page is not a picture.
        assert_eq!(picture_kind(b"<!doctype html>"), None);
    }

    #[test]
    fn names_survive_what_a_filesystem_refuses() {
        assert_eq!(safe_name("a/b:c?d"), "a b c d");
        assert_eq!(safe_name("  trailing.  "), "trailing");
        assert_eq!(safe_name(""), "video");
        assert_eq!(safe_name("..."), "video");
        assert_eq!(safe_name(&"x".repeat(200)).chars().count(), 80);
    }

    #[test]
    fn a_matroska_stream_is_recognised_by_its_first_bytes() {
        let webm = [0x1a, 0x45, 0xdf, 0xa3, 0, 0, 0, 0];
        assert!(matches!(read_stream(&webm), Err(_) | Ok(Stream::Webm(_))));
        // An MP4 begins with a box length, never with that signature.
        let mp4 = [0, 0, 0, 0x18, b'f', b't', b'y', b'p'];
        assert!(matches!(read_stream(&mp4), Err(_) | Ok(Stream::Mp4(_))));
    }
}
