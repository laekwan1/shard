//! Re-asking YouTube's InnerTube API as an *app* client, for the full format list.
//!
//! Why this exists: m.youtube.com (MWEB), which the browser shows, lists formats only up to
//! 720p in its `streamingData`, and those come SABR-only (no plaintext URL). YouTube's app
//! clients — ANDROID, IOS — get the full DASH ladder up to 2160p, and crucially with
//! **plaintext** URLs: no `signatureCipher` to unscramble, so no JS engine is needed (we have
//! none and won't add one). Measured 2026-09: ANDROID(39)·IOS(32) return plaintext formats
//! incl. 2160p and a `Range` GET answers 206 — even from a datacenter IP; a real device is
//! more permissive. ANDROID_VR was bot-blocked (LOGIN_REQUIRED), so it is not in the ladder.
//!
//! "ANDROID client" is **not** Android code running here — it is only what we *claim to be*
//! in the request (context.client + headers), exactly as yt-dlp does. Same YouTube backend,
//! a fuller menu because of who we say we are.
//!
//! Any failure (bot-check, no plaintext URL, network) returns `Err`, and the caller falls
//! back to the existing SABR ≤720p path — no regression.

use anyhow::{bail, Result};
use serde::Deserialize;

use super::youtube::Format;

/// One InnerTube client identity to try. App clients give plaintext URLs; web clients
/// (WEB/MWEB/TVHTML5) give `signatureCipher`, so they are deliberately not in the ladder.
struct ClientId {
    /// `context.client.clientName` and the `X-YouTube-Client-Name` id.
    name: &'static str,
    id: &'static str,
    version: &'static str,
    user_agent: &'static str,
    /// Extra client-context JSON fields, already comma-terminated (e.g. androidSdkVersion).
    extra: &'static str,
}

// Try order (first OK-with-plaintext wins). ANDROID and IOS both worked in testing; ANDROID
// lists the most formats so it leads. Versions are pinned and cheap to bump right here when
// YouTube tightens a client — that is the one maintenance point this feature carries.
const CLIENTS: &[ClientId] = &[
    ClientId {
        name: "ANDROID",
        id: "3",
        version: "20.10.38",
        user_agent: "com.google.android.youtube/20.10.38 (Linux; U; Android 14) gzip",
        extra: r#""androidSdkVersion":34,"osName":"Android","osVersion":"14","#,
    },
    ClientId {
        name: "IOS",
        id: "5",
        version: "20.10.4",
        user_agent: "com.google.ios.youtube/20.10.4 (iPhone16,2; U; CPU iOS 18_3 like Mac OS X)",
        extra: r#""deviceMake":"Apple","deviceModel":"iPhone16,2","osName":"iPhone","osVersion":"18.3.0.22D63","#,
    },
];

#[derive(Deserialize)]
struct PlayerResponse {
    #[serde(rename = "streamingData")]
    streaming_data: Option<StreamingData>,
    #[serde(rename = "playabilityStatus")]
    playability: Option<Playability>,
}

#[derive(Deserialize)]
struct StreamingData {
    #[serde(default, rename = "adaptiveFormats")]
    adaptive: Vec<RawFormat>,
}

#[derive(Deserialize)]
struct Playability {
    #[serde(default)]
    status: String,
}

/// InnerTube's raw format shape. Its field names differ from the ASK-normalized [`Format`]
/// (`qualityLabel` vs `quality`, `contentLength` vs `bytes`, `approxDurationMs` vs
/// `durationMs`), and it carries both `quality` and `qualityLabel` — so a dedicated struct +
/// converter is clearer than piling serde aliases on `Format`.
#[derive(Deserialize)]
struct RawFormat {
    itag: u32,
    #[serde(default)]
    url: String,
    #[serde(default, rename = "signatureCipher")]
    signature_cipher: String,
    #[serde(default, rename = "mimeType")]
    mime_type: String,
    #[serde(default)]
    bitrate: u64,
    #[serde(default, rename = "contentLength")]
    content_length: String,
    #[serde(default, rename = "qualityLabel")]
    quality_label: String,
    #[serde(default, rename = "audioQuality")]
    audio_quality: String,
    #[serde(default, rename = "lastModified")]
    last_modified: String,
    #[serde(default, rename = "approxDurationMs")]
    approx_duration_ms: String,
    #[serde(default)]
    xtags: String,
    #[serde(default, rename = "audioTrack")]
    audio_track: Option<AudioTrack>,
}

#[derive(Deserialize)]
struct AudioTrack {
    #[serde(default)]
    id: String,
    #[serde(default, rename = "displayName")]
    display_name: String,
    #[serde(default, rename = "audioIsDefault")]
    audio_is_default: bool,
}

impl RawFormat {
    /// Map to the crate's [`Format`], so `codec()`/`size()`/`height()`/`video_tracks()`/
    /// `best_audio()` all work unchanged — they only read mime/bytes/quality.
    fn into_format(self) -> Format {
        let quality = if self.quality_label.is_empty() {
            self.audio_quality
        } else {
            self.quality_label
        };
        let (audio_language, audio_name, audio_default) = match &self.audio_track {
            Some(t) => (
                t.id.split('.').next().unwrap_or("").to_string(),
                t.display_name.clone(),
                t.audio_is_default,
            ),
            None => (String::new(), String::new(), false),
        };
        Format {
            itag: self.itag,
            mime_type: self.mime_type,
            quality,
            bitrate: self.bitrate,
            bytes: self.content_length,
            last_modified: self.last_modified,
            xtags: self.xtags,
            duration_ms: self.approx_duration_ms,
            audio_language,
            audio_name,
            audio_default,
            url: self.url,
            signature_cipher: self.signature_cipher,
        }
    }
}

fn request_body(video_id: &str, c: &ClientId) -> Vec<u8> {
    // reqwest here has no `json` feature (Cargo.toml), so build the body by hand. video_id is
    // an 11-char YouTube id ([A-Za-z0-9_-]) that never needs escaping — the page validated it.
    format!(
        r#"{{"context":{{"client":{{"clientName":"{}","clientVersion":"{}",{}"hl":"en","gl":"US"}}}},"videoId":"{}","contentCheckOk":true,"racyCheckOk":true}}"#,
        c.name, c.version, c.extra, video_id
    )
    .into_bytes()
}

/// Ask InnerTube (app clients, in order) for the full format list. Returns the formats of the
/// first client that answers `playabilityStatus == OK` with at least one plaintext URL; `Err`
/// if none do — the caller then falls back to SABR (≤720p), so this never makes things worse.
pub fn formats(client: &reqwest::blocking::Client, video_id: &str) -> Result<Vec<Format>> {
    for c in CLIENTS {
        let resp = match client
            .post("https://www.youtube.com/youtubei/v1/player")
            .header("Content-Type", "application/json")
            .header("User-Agent", c.user_agent)
            .header("X-YouTube-Client-Name", c.id)
            .header("X-YouTube-Client-Version", c.version)
            .body(request_body(video_id, c))
            .send()
        {
            Ok(r) if r.status().is_success() => r,
            _ => continue,
        };
        let bytes = match resp.bytes() {
            Ok(b) => b,
            Err(_) => continue,
        };
        let pr: PlayerResponse = match serde_json::from_slice(&bytes) {
            Ok(p) => p,
            Err(_) => continue,
        };
        // LOGIN_REQUIRED (bot check), UNPLAYABLE, AGE gates — try the next client.
        if pr.playability.map(|p| p.status).as_deref() != Some("OK") {
            continue;
        }
        let formats: Vec<Format> = pr
            .streaming_data
            .map(|s| s.adaptive)
            .unwrap_or_default()
            .into_iter()
            .map(RawFormat::into_format)
            .collect();
        // Only useful if we can actually fetch it: web-cipher-only answers fall back.
        if formats.iter().any(|f| f.direct_url().is_some()) {
            return Ok(formats);
        }
    }
    bail!("InnerTube에서 직접 URL(평문 포맷)을 얻지 못했습니다");
}
