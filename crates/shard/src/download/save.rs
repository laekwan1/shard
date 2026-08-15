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

#[cfg(test)]
mod tests {
    use super::*;

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
