//! AAC → MP3, for the music-only save when the user asks for MP3.
//!
//! The music row hands us raw AAC frames and the stream's AudioSpecificConfig —
//! the same ones the .m4a path re-muxes. Here they are decoded to PCM with
//! symphonia (pure Rust, no container step: the frames are fed straight to the
//! AAC decoder) and re-encoded with LAME. A plain resample would defeat the
//! point of a re-encode, so it is LAME at a high constant rate; the original AAC
//! is smaller for the same sound, which is why MP3 is a switch and not the only
//! way out. Opus is not handled here — decoding it would need another library —
//! so an Opus track keeps its .weba.

use anyhow::{anyhow, bail, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CodecParameters, DecoderOptions, CODEC_TYPE_AAC};
use symphonia::core::formats::Packet;

/// Prepend an ID3v2 tag carrying the cover, so a music player shows the picture
/// the way the .m4a path embeds it. MP3 has no box to rewrite like MP4 does; the
/// tag simply sits at the front of the file, which is where every player looks.
///
/// `kind` is "jpg" or "png" — the two the caller already narrowed the picture to.
pub fn with_cover_id3(mp3: &[u8], picture: &[u8], kind: &str) -> Vec<u8> {
    let mime: &[u8] = if kind == "png" { b"image/png" } else { b"image/jpeg" };

    // APIC frame body: encoding(latin1) + MIME\0 + picture type(front cover) +
    // description\0 + the image bytes.
    let mut body = Vec::new();
    body.push(0x00);
    body.extend_from_slice(mime);
    body.push(0x00);
    body.push(0x03); // front cover
    body.push(0x00); // empty description
    body.extend_from_slice(picture);

    let mut frame = Vec::new();
    frame.extend_from_slice(b"APIC");
    frame.extend_from_slice(&syncsafe(body.len() as u32)); // v2.4 frame sizes are syncsafe
    frame.extend_from_slice(&[0x00, 0x00]); // no frame flags
    frame.extend_from_slice(&body);

    let mut tag = Vec::new();
    tag.extend_from_slice(b"ID3");
    tag.extend_from_slice(&[0x04, 0x00]); // ID3v2.4.0
    tag.push(0x00); // no tag flags
    tag.extend_from_slice(&syncsafe(frame.len() as u32));
    tag.extend_from_slice(&frame);

    let mut out = Vec::with_capacity(tag.len() + mp3.len());
    out.extend_from_slice(&tag);
    out.extend_from_slice(mp3);
    out
}

/// Read the cover back out of an ID3v2 tag — the counterpart to [with_cover_id3],
/// so the library's cover route can serve an MP3's art the same way it serves an
/// .m4a's. Returns the picture and whether it is a JPEG or PNG.
pub fn id3_cover(file: &[u8]) -> Option<(Vec<u8>, &'static str)> {
    if file.len() < 10 || &file[0..3] != b"ID3" {
        return None;
    }
    let version_major = file[3];
    let tag_end = (10 + unsyncsafe(&file[6..10]) as usize).min(file.len());
    let mut pos = 10usize;
    while pos + 10 <= tag_end {
        let id = &file[pos..pos + 4];
        // v2.4 frame sizes are syncsafe; v2.3's are plain. We write v2.4.
        let size = if version_major >= 4 {
            unsyncsafe(&file[pos + 4..pos + 8]) as usize
        } else {
            u32::from_be_bytes([file[pos + 4], file[pos + 5], file[pos + 6], file[pos + 7]]) as usize
        };
        let data_start = pos + 10;
        if size == 0 || data_start + size > file.len() {
            break;
        }
        if id == b"APIC" {
            return parse_apic(&file[data_start..data_start + size]);
        }
        pos = data_start + size;
    }
    None
}

/// Pull the picture out of an APIC frame body: encoding, MIME\0, type,
/// description\0, then the image bytes (see [with_cover_id3]).
fn parse_apic(data: &[u8]) -> Option<(Vec<u8>, &'static str)> {
    if data.is_empty() {
        return None;
    }
    let mut i = 1; // skip the text-encoding byte
    let mime_start = i;
    while i < data.len() && data[i] != 0 {
        i += 1;
    }
    let mime = &data[mime_start..i];
    i += 1; // the MIME's null
    if i >= data.len() {
        return None;
    }
    i += 1; // the picture-type byte
    while i < data.len() && data[i] != 0 {
        i += 1; // the description (latin1, so a single-byte null terminates it)
    }
    i += 1; // the description's null
    if i > data.len() {
        return None;
    }
    let kind = if mime.windows(3).any(|w| w.eq_ignore_ascii_case(b"png")) { "png" } else { "jpg" };
    Some((data[i..].to_vec(), kind))
}

/// Decode ID3's syncsafe 28-bit size (the inverse of [syncsafe]).
fn unsyncsafe(b: &[u8]) -> u32 {
    ((b[0] as u32 & 0x7f) << 21)
        | ((b[1] as u32 & 0x7f) << 14)
        | ((b[2] as u32 & 0x7f) << 7)
        | (b[3] as u32 & 0x7f)
}

/// A 28-bit size spread over four bytes with the top bit of each left clear —
/// the encoding ID3 uses so a size can never be mistaken for an MP3 frame sync.
fn syncsafe(n: u32) -> [u8; 4] {
    [
        ((n >> 21) & 0x7f) as u8,
        ((n >> 14) & 0x7f) as u8,
        ((n >> 7) & 0x7f) as u8,
        (n & 0x7f) as u8,
    ]
}

/// One decoded, re-encoded music file. `frames` are the raw AAC access units in
/// order; `asc` is the AudioSpecificConfig that describes them.
pub fn from_aac(
    asc: &[u8],
    sample_rate: u32,
    channels: u32,
    frames: &[Vec<u8>],
) -> Result<Vec<u8>> {
    if frames.is_empty() {
        bail!("음성 프레임이 없습니다");
    }
    let pcm = decode_aac(asc, sample_rate, channels, frames)?;
    encode_mp3(&pcm, sample_rate, channels)
}

/// Decode the AAC frames to interleaved 16-bit PCM.
///
/// The frames go through the decoder as packets rather than through a container
/// reader: we already demuxed them, and symphonia's AAC decoder takes the config
/// from the extra data (the ASC), so no ADTS header has to be synthesised.
fn decode_aac(asc: &[u8], sample_rate: u32, channels: u32, frames: &[Vec<u8>]) -> Result<Vec<i16>> {
    let mut params = CodecParameters::new();
    params.for_codec(CODEC_TYPE_AAC);
    if sample_rate > 0 {
        params.with_sample_rate(sample_rate);
    }
    if !asc.is_empty() {
        params.with_extra_data(asc.to_vec().into_boxed_slice());
    }

    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| anyhow!("AAC 디코더를 만들 수 없습니다: {e}"))?;

    let mut pcm = Vec::<i16>::new();
    let mut sbuf: Option<SampleBuffer<i16>> = None;
    let mut ts: u64 = 0;
    for frame in frames {
        // Each AAC access unit is a fixed 1024 samples; the exact timestamps do
        // not matter to a straight decode, only that they advance.
        let packet = Packet::new_from_slice(0, ts, 1024, frame);
        ts += 1024;
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // A single bad frame should not lose the whole song; skip it.
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(anyhow!("AAC 디코드 실패: {e}")),
        };
        let spec = *decoded.spec();
        let capacity = decoded.capacity() as u64;
        let buf = sbuf.get_or_insert_with(|| SampleBuffer::<i16>::new(capacity, spec));
        buf.copy_interleaved_ref(decoded);
        pcm.extend_from_slice(buf.samples());
    }
    if pcm.is_empty() {
        bail!("AAC에서 소리를 얻지 못했습니다");
    }
    // channels is only used to shape the encoder; a decode that disagrees with
    // the container header would still have produced interleaved samples above.
    let _ = channels;
    Ok(pcm)
}

/// Encode interleaved 16-bit PCM to MP3 with LAME at a high constant rate.
fn encode_mp3(pcm: &[i16], sample_rate: u32, channels: u32) -> Result<Vec<u8>> {
    use mp3lame_encoder::{Bitrate, Builder, FlushNoGap, InterleavedPcm, Quality};

    let mut builder = Builder::new().ok_or_else(|| anyhow!("MP3 인코더를 만들 수 없습니다"))?;
    builder
        .set_num_channels(channels.clamp(1, 2) as u8)
        .map_err(|e| anyhow!("채널 설정 실패: {e}"))?;
    builder
        .set_sample_rate(if sample_rate == 0 { 48_000 } else { sample_rate })
        .map_err(|e| anyhow!("샘플레이트 설정 실패: {e}"))?;
    // 320 kbps CBR: the top MP3 rate, so the re-encode gives up as little as an
    // MP3 can against the AAC it came from.
    builder
        .set_brate(Bitrate::Kbps320)
        .map_err(|e| anyhow!("비트레이트 설정 실패: {e}"))?;
    builder
        .set_quality(Quality::Best)
        .map_err(|e| anyhow!("품질 설정 실패: {e}"))?;
    let mut encoder = builder
        .build()
        .map_err(|e| anyhow!("MP3 인코더 초기화 실패: {e}"))?;

    let mut out = Vec::new();
    let frame_count = pcm.len() / channels.clamp(1, 2).max(1) as usize;
    out.reserve(mp3lame_encoder::max_required_buffer_size(frame_count) + 7200);

    let encoded = encoder
        .encode(InterleavedPcm(pcm), out.spare_capacity_mut())
        .map_err(|e| anyhow!("MP3 인코딩 실패: {e}"))?;
    unsafe { out.set_len(out.len() + encoded) };

    let flushed = encoder
        .flush::<FlushNoGap>(out.spare_capacity_mut())
        .map_err(|e| anyhow!("MP3 마무리 실패: {e}"))?;
    unsafe { out.set_len(out.len() + flushed) };

    if out.is_empty() {
        bail!("MP3 데이터가 비었습니다");
    }
    Ok(out)
}
