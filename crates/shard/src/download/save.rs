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

/// Make ring the process crypto provider for rustls.
///
/// reqwest is built with `rustls-no-provider` so aws-lc-rs (which will not link
/// into the iOS cdylib) is never pulled in; the price is that rustls has no
/// default provider until one is installed. Do it once, before the first client
/// is built. Idempotent — a second install just returns Err, which we ignore.
fn use_ring() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

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
    use_ring();
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
        // Opus lives in a WebM here, but with no video track — so `.weba`, the
        // WebM-audio extension. It marks the file as audio (the phone's library
        // sorts music from video by extension, and players show it as a song)
        // without renaming the container it actually is.
        "weba"
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
    use_ring();
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

/// The renditions an HLS master lists, highest first, for a quality menu.
///
/// Fetched and parsed here so the list can be shown before anything is chosen;
/// an empty result means the URL was a media playlist already (one quality) or
/// could not be read. Kept quick with a short timeout — it blocks the caller.
pub fn hls_variants(manifest_url: &str, referer: &str) -> Vec<crate::download::hls::Variant> {
    use crate::download::hls;
    use_ring();
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent(UA)
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let Ok(response) = media_get(&client, manifest_url, referer) else { return Vec::new() };
    let Ok(text) = response.text() else { return Vec::new() };
    if hls::is_master(&text) {
        hls::variants(&text, manifest_url)
    } else {
        Vec::new()
    }
}

/// The itag that stands for "audio only" — the music row. Matches the desktop
/// queue's marker so the same sentinel means the same thing on the phone.
pub const MUSIC_ITAG: u32 = u32::MAX;

/// Quality rows for a captured YouTube offer: (itag, label, detail).
///
/// The same list the desktop shows, without the desktop-only queue around it,
/// so the phone can offer the identical choices from the identical parse. The
/// music row (audio only) leads, then one row per resolution.
pub fn youtube_qualities(offer_json: &str) -> Result<Vec<(u32, String, String)>> {
    use crate::config::AudioQuality;
    use crate::download::youtube::{AudioWish, Offer};
    let offer = Offer::parse(offer_json)?;
    // portable=false so music comes out as Opus (in WebM): it is smaller and
    // better than AAC, and the phone's library plays WebM through VLCKit now.
    let wish = AudioWish { language: String::new(), quality: AudioQuality::Best, portable: false };
    let mut rows = Vec::new();
    if let Some(audio) = offer.best_audio(&wish) {
        rows.push((
            MUSIC_ITAG,
            "음악만 저장".to_string(),
            format!("{} · {} {}k", human(audio.size()), audio.codec(), audio.bitrate / 1000),
        ));
    }
    // Best audio for the video rows may differ (Opus is fine inside a video).
    let video_wish =
        AudioWish { language: String::new(), quality: AudioQuality::Best, portable: false };
    let audio = offer.best_audio(&video_wish);
    // Show AV1 only — it is the smallest at a given quality, and the user asked to
    // drop H.264 (which also had an AVPlayer playback glitch) and VP9. If a video
    // has no AV1 at all, fall back to every codec so a download is still possible;
    // there the codec is shown in the label to tell them apart.
    let mut av1: Vec<(u32, String, String)> = Vec::new();
    let mut fallback: Vec<(u32, String, String)> = Vec::new();
    let mut seen_fallback: Vec<String> = Vec::new();
    let mut seen_av1: Vec<String> = Vec::new();
    for video in offer.video_tracks() {
        let codec = video.codec();
        let total = video.size() + audio.map(|a| a.size()).unwrap_or(0);
        let size = if video.size_is_exact() { human(total) } else { format!("약 {}", human(total)) };
        if codec == "AV1" {
            if !seen_av1.contains(&video.quality) {
                seen_av1.push(video.quality.clone());
                av1.push((video.itag, video.quality.clone(), size));
            }
        } else {
            let key = format!("{}|{}", video.quality, codec);
            if !seen_fallback.contains(&key) {
                seen_fallback.push(key);
                let label = if codec.is_empty() { video.quality.clone() } else { format!("{} · {}", video.quality, codec) };
                fallback.push((video.itag, label, size));
            }
        }
    }
    rows.extend(if av1.is_empty() { fallback } else { av1 });
    Ok(rows)
}

/// Human byte count, small and dependency-free.
fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

/// Download a YouTube video (or its audio alone) from a captured offer.
///
/// `offer_json` is the `ytInitialPlayerResponse` the page script captured;
/// `itag` names the wanted video format, or [`MUSIC_ITAG`] for audio only. The
/// Job is built exactly as the desktop builds it, then run through the SABR
/// path — the phone reuses the most-tested download code rather than a new one.
pub fn run_youtube(
    offer_json: &str,
    itag: u32,
    into: &Path,
    on_progress: &mut dyn FnMut(u64, u64),
    cancelled: &dyn Fn() -> bool,
) -> Result<PathBuf> {
    use crate::config::AudioQuality;
    use crate::download::youtube::{AudioWish, Offer};
    let offer = Offer::parse(offer_json)?;
    let template = offer.template().ok_or_else(|| anyhow!("받을 것을 찾지 못했습니다"))?;

    let audio_only = itag == MUSIC_ITAG;
    // portable=false: prefer Opus (in WebM) even for audio-only music. VLCKit
    // plays WebM on the phone now, and Opus is smaller and better than AAC.
    let wish =
        AudioWish { language: String::new(), quality: AudioQuality::Best, portable: false };
    let audio = offer.best_audio(&wish).ok_or_else(|| anyhow!("음성을 찾지 못했습니다"))?;
    let video = if audio_only {
        offer.video_tracks().into_iter().last()
    } else {
        offer.formats.iter().find(|f| f.itag == itag)
    }
    .ok_or_else(|| anyhow!("고른 화질을 찾지 못했습니다"))?;

    // Name a format other than the wanted one as "already playing", so the wire
    // is not primed for the bytes we are about to ask for.
    let decoy = offer
        .video_tracks()
        .into_iter()
        .find(|f| f.itag != itag)
        .map(|f| f.track())
        .unwrap_or_else(|| audio.track());

    // Put the resolution in a video's filename so two qualities of the same video
    // do not overwrite each other (downloading AV1 replaced the H.264 before).
    // Music keeps the plain title (its cover is filed under that name).
    let title = if audio_only {
        offer.title.clone()
    } else if video.quality.is_empty() {
        offer.title.clone()
    } else {
        format!("{} ({})", offer.title, video.quality)
    };
    let job = Job {
        template,
        video: video.track(),
        audio: audio.track(),
        decoy,
        title,
        into: into.to_path_buf(),
        cover: offer.thumb.clone(),
        audio_only,
    };
    let expected = job.video.bytes + job.audio.bytes;
    let mut progress = |p: Progress| on_progress(p.video + p.audio, expected);
    run(&job, &mut progress, cancelled)
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
    // is a playable file — but a live/DVR HLS numbers its fragments from the
    // stream's own clock (test-streams.mux.dev starts near 14s), so the movie
    // opened at 14s and the player buffered forever waiting for the 0–14s that
    // never comes. Rebase each track's fragment decode times to start at 0.
    let whole = rebase_fmp4(whole);
    let output = into.join(format!("{}.mp4", safe_name(title)));
    std::fs::write(&output, &whole)?;
    Ok(output)
}

/// Subtract each track's first fragment `tfdt` (base_media_decode_time) from all
/// of its fragments, so a fragmented MP4 whose timeline starts well after zero
/// begins at zero instead. Per-track (each `traf` names its `tfhd` track id) so
/// audio and video keep their relative offset. Unknown/odd boxes are skipped; on
/// anything unexpected it leaves the bytes as they were.
fn rebase_fmp4(mut data: Vec<u8>) -> Vec<u8> {
    // (value_offset, is_64bit, track_id, value)
    let mut occ: Vec<(usize, bool, u32, u64)> = Vec::new();
    let be32 = |d: &[u8], p: usize| u32::from_be_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]]);
    let be64 = |d: &[u8], p: usize| u64::from_be_bytes([
        d[p], d[p + 1], d[p + 2], d[p + 3], d[p + 4], d[p + 5], d[p + 6], d[p + 7],
    ]);

    // The child boxes of a container [start, end): (type, payload_start, box_end).
    fn boxes(d: &[u8], start: usize, end: usize) -> Vec<([u8; 4], usize, usize)> {
        let mut v = Vec::new();
        let mut i = start;
        while i + 8 <= end {
            let size = u32::from_be_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]]) as usize;
            let typ = [d[i + 4], d[i + 5], d[i + 6], d[i + 7]];
            let box_end = if size == 0 { end } else if size < 8 { break } else { i + size };
            if box_end > end || box_end <= i { break; }
            v.push((typ, i + 8, box_end));
            i = box_end;
        }
        v
    }

    for (typ, ps, be) in boxes(&data, 0, data.len()) {
        if &typ != b"moof" { continue; }
        for (t2, ps2, be2) in boxes(&data, ps, be) {
            if &t2 != b"traf" { continue; }
            let mut track_id = 0u32;
            let mut tfdt: Option<(usize, bool, u64)> = None;
            for (t3, ps3, be3) in boxes(&data, ps2, be2) {
                if &t3 == b"tfhd" && ps3 + 8 <= be3 {
                    track_id = be32(&data, ps3 + 4);      // after version+flags
                } else if &t3 == b"tfdt" {
                    let ver = data[ps3];
                    let p = ps3 + 4;                       // after version+flags
                    if ver == 1 && p + 8 <= be3 {
                        tfdt = Some((p, true, be64(&data, p)));
                    } else if ver == 0 && p + 4 <= be3 {
                        tfdt = Some((p, false, be32(&data, p) as u64));
                    }
                }
            }
            if let Some((off, is64, val)) = tfdt {
                occ.push((off, is64, track_id, val));
            }
        }
    }

    // First value seen per track is the base to subtract.
    let mut base: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    for &(_, _, tid, val) in &occ {
        base.entry(tid).or_insert(val);
    }
    for (off, is64, tid, val) in occ {
        let nv = val.saturating_sub(base[&tid]);
        if is64 {
            data[off..off + 8].copy_from_slice(&nv.to_be_bytes());
        } else {
            data[off..off + 4].copy_from_slice(&(nv as u32).to_be_bytes());
        }
    }
    data
}

/// Demux a transport stream and write it back out as MP4.
///
/// MP4 rather than Matroska so the file plays everywhere, iOS included — a TS
/// only carries H.264 and AAC, which is exactly MP4's home ground. The desktop
/// used to get .mkv here and this changes it to .mp4; WebView2 plays MP4 too, so
/// nothing is lost and the phone gains a file AVPlayer will open.
fn remux_ts(data: &[u8], into: &Path, title: &str) -> Result<PathBuf> {
    use crate::download::{mp4mux, ts};
    let demuxed = ts::demux(data)?;
    if demuxed.video.is_empty() && demuxed.audio.is_empty() {
        bail!("전송 스트림에서 트랙을 찾지 못했습니다");
    }

    let output = into.join(format!("{}.mp4", safe_name(title)));
    let mut file = std::io::BufWriter::new(File::create(&output)?);
    mp4mux::write(&demuxed, &mut file)?;
    file.flush()?;
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
