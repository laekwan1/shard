//! What the page has to be asked, and how to read its answer.
//!
//! Two scripts. The first is injected before the page's own code runs and does
//! nothing but keep a copy of the delivery request the player sends — that
//! request cannot be rebuilt from outside, because it carries a server
//! configuration blob and a bot-check token that only a running page produces.
//! The second is run on demand and reports the format list along with that
//! captured request.
//!
//! The scripts are the same ones the phone app uses, with the bridge changed:
//! there the page calls into a Java object, here it posts a message.

use crate::config::AudioQuality;
use crate::download::sabr::{Template, Track};
use anyhow::{anyhow, Result};
use serde::Deserialize;

/// One format as the page describes it.
#[derive(Clone, Debug, Deserialize)]
pub struct Format {
    pub itag: u32,
    #[serde(default, rename = "mimeType")]
    pub mime_type: String,
    #[serde(default)]
    pub quality: String,
    #[serde(default)]
    pub bitrate: u64,
    /// Strings, because the page sends them as strings — they exceed what
    /// JavaScript can hold as a number.
    #[serde(default)]
    pub bytes: String,
    #[serde(default, rename = "lastModified")]
    pub last_modified: String,
    #[serde(default)]
    pub xtags: String,
    /// Also a string, and also absent on some pages.
    #[serde(default, rename = "durationMs")]
    pub duration_ms: String,
    /// Which language this track carries, when the page says so.
    #[serde(default, rename = "audioLanguage")]
    pub audio_language: String,
    /// What the page shows for it: "Korean", "English (original)".
    #[serde(default, rename = "audioName")]
    pub audio_name: String,
    /// True for the track the video plays by default.
    #[serde(default, rename = "audioDefault")]
    pub audio_default: bool,
}

impl Format {
    /// "AV1", "VP9", "H.264", "Opus", "AAC" — what the size hinges on.
    pub fn codec(&self) -> &'static str {
        let mime = &self.mime_type;
        if mime.contains("av01") {
            "AV1"
        } else if mime.contains("vp9") || mime.contains("vp09") {
            "VP9"
        } else if mime.contains("avc1") {
            "H.264"
        } else if mime.contains("opus") {
            "Opus"
        } else if mime.contains("mp4a") {
            "AAC"
        } else {
            ""
        }
    }

    pub fn is_video(&self) -> bool {
        self.mime_type.starts_with("video")
    }

    pub fn is_audio(&self) -> bool {
        self.mime_type.starts_with("audio")
    }

    /// Exact when the page said so, estimated when it did not.
    ///
    /// The desktop site leaves `contentLength` out — which is why the first
    /// version found no audio at all: it required a size, and every audio
    /// format reported zero. A size is worth having and not worth requiring,
    /// so a missing one is worked out from the bitrate instead.
    pub fn size(&self) -> u64 {
        let stated: u64 = self.bytes.parse().unwrap_or(0);
        if stated > 0 {
            return stated;
        }
        let duration_ms: u64 = self.duration_ms.parse().unwrap_or(0);
        if duration_ms == 0 || self.bitrate == 0 {
            return 0;
        }
        self.bitrate / 8 * duration_ms / 1_000
    }

    /// Whether [`size`] is the page's number or ours.
    pub fn size_is_exact(&self) -> bool {
        self.bytes.parse::<u64>().unwrap_or(0) > 0
    }

    /// The number in "1080p60", for ordering.
    pub fn height(&self) -> u32 {
        let digits: String =
            self.quality.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().unwrap_or(0)
    }

    pub fn track(&self) -> Track {
        Track {
            itag: self.itag,
            last_modified: self.last_modified.parse().unwrap_or(0),
            xtags: self.xtags.clone(),
            bytes: self.size(),
        }
    }
}

/// Everything the page had to offer.
#[derive(Clone, Debug, Deserialize)]
pub struct Offer {
    #[serde(default)]
    pub formats: Vec<Format>,
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "templateUrl")]
    pub template_url: String,
    #[serde(default, rename = "templateBody")]
    pub template_body: String,
    /// The picture the video is listed under, if the page named one.
    #[serde(default)]
    pub thumb: String,
    /// Says which step failed when there is nothing to offer.
    #[serde(default)]
    pub reason: String,
    /// A direct progressive media URL the page was seen fetching or listing —
    /// the simple case, saved as-is with no muxing. Empty on YouTube, where the
    /// SABR template above is used instead.
    #[serde(default)]
    pub media: String,
    /// An HLS master or media playlist URL, for sites that stream over HLS.
    #[serde(default)]
    pub hls: String,
    /// Every HLS playlist URL the page fetched, newline-separated — so the one
    /// that is really the master can be found when the first is not.
    #[serde(default, rename = "hlsList")]
    pub hls_list: String,
    /// The address to send as `Referer` when fetching the two above: arbitrary
    /// CDNs refuse a request that does not look like it came from the page.
    #[serde(default)]
    pub referer: String,
}

impl Offer {
    pub fn parse(payload: &str) -> Result<Self> {
        serde_json::from_str(payload).map_err(|e| anyhow!("페이지 응답을 읽지 못했습니다: {e}"))
    }

    /// The captured request, if the player has fetched anything yet.
    pub fn template(&self) -> Option<Template> {
        if self.template_url.is_empty() || self.template_body.is_empty() {
            return None;
        }
        Some(Template { url: self.template_url.clone(), body: decode_base64(&self.template_body)? })
    }

    /// Video tracks worth offering, largest first and smallest-per-size within
    /// each resolution, so the efficient codec is the obvious pick.
    pub fn video_tracks(&self) -> Vec<&Format> {
        let named: Vec<_> = self
            .formats
            .iter()
            .filter(|f| f.is_video() && f.last_modified.parse::<u64>().unwrap_or(0) > 0)
            .collect();
        let untagged: Vec<_> = named.iter().filter(|f| f.xtags.is_empty()).copied().collect();
        let mut found = if untagged.is_empty() { named } else { untagged };
        found.sort_by(|a, b| b.height().cmp(&a.height()).then(a.size().cmp(&b.size())));
        found
    }

    /// The audio track to take, given what the user asked for.
    ///
    /// Two separate questions live here and were confused at first. *Which
    /// language* is one: preferring the untagged track picks the original,
    /// which on a dubbed video is not the language the viewer was listening
    /// to — a Korean viewer got English. *How good* is the other: an early
    /// version took the smallest usable track and left music thin.
    ///
    /// So the language is decided first and the quality within it second.
    pub fn best_audio(&self, wish: &AudioWish) -> Option<&Format> {
        let all: Vec<_> = self.formats.iter().filter(|f| f.is_audio()).collect();
        if all.is_empty() {
            return None;
        }

        // Language, in order of how well it is known: what was asked for, then
        // what the page plays by default, then anything.
        let wanted: Vec<_> = if wish.language.is_empty() {
            Vec::new()
        } else {
            all.iter().filter(|f| f.audio_language.starts_with(&wish.language)).copied().collect()
        };
        let default: Vec<_> = all.iter().filter(|f| f.audio_default).copied().collect();
        let candidates = if !wanted.is_empty() {
            wanted
        } else if !default.is_empty() {
            default
        } else {
            all
        };

        // Compatibility first, when it was asked for: an AAC track is a little
        // larger and plays everywhere — this phone's default app and the next
        // one's.
        let candidates = if wish.portable {
            let aac: Vec<_> = candidates.iter().filter(|f| f.codec() == "AAC").copied().collect();
            if aac.is_empty() { candidates } else { aac }
        } else {
            candidates
        };

        // A file saved on its own is music, kept for keeps, and takes the best
        // the track offers — above the ceiling a soundtrack muxed into a video
        // sits under, because here the few extra megabytes buy something back.
        // The phone build saves this same highest-bitrate track, so the two
        // stay in step for the same video.
        if wish.portable {
            return candidates.iter().max_by_key(|f| f.bitrate).copied();
        }

        match wish.quality {
            // Best under a ceiling, above which the ear gets nothing back.
            AudioQuality::Best => candidates
                .iter()
                .filter(|f| f.bitrate <= 200_000)
                .max_by_key(|f| (f.codec() == "Opus", f.bitrate))
                .or_else(|| candidates.iter().min_by_key(|f| f.bitrate))
                .copied(),
            AudioQuality::Balanced => candidates
                .iter()
                .filter(|f| (64_000..=110_000).contains(&f.bitrate))
                .max_by_key(|f| f.bitrate)
                .or_else(|| candidates.iter().min_by_key(|f| f.bitrate))
                .copied(),
            AudioQuality::Small => candidates.iter().min_by_key(|f| f.bitrate).copied(),
        }
    }
}

/// What the user asked for, as the chooser needs it.
pub struct AudioWish {
    /// "ko", "en", or empty for whatever the video defaults to.
    pub language: String,
    pub quality: AudioQuality,
    /// Prefer a codec every phone will take, even at some cost in size.
    ///
    /// Opus is the better codec and the smaller file, and inside a video it is
    /// the right choice — the container is ours and any player that opens the
    /// video opens the sound with it. On its own it is a different question:
    /// Opus arrives in a WebM container, which iPhones do not play at all and
    /// which Android music apps do not list even though the phone can play it.
    /// A music file nobody's music app will show is not a music file.
    pub portable: bool,
}

/// Base64 without a dependency: the payload is one field of one message.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (index, byte) in TABLE.iter().enumerate() {
        lookup[*byte as usize] = index as u8;
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        if byte == b'=' || byte == b'\n' || byte == b'\r' {
            continue;
        }
        let value = lookup[byte as usize];
        if value == 255 {
            return None;
        }
        buffer = (buffer << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

/// Injected before the page's own scripts run.
///
/// The timing is the whole point. Injecting on load instead was tried first,
/// and by then YouTube's player had already taken its own reference to `fetch`,
/// so replacing the global changed nothing and the request went unseen.
pub const RECORDER: &str = r#"
(function () {
  if (window.__shardSabrReady) { window.__shardSabr = null; return; }
  window.__shardSabrReady = true;
  window.__shardSabr = null;
  // What the media capture holds for non-YouTube sites: the last progressive
  // file and the last HLS playlist the page was seen asking the network for.
  // A blob: src on the <video> is not a URL anything can fetch, so the real
  // request underneath it — the .mp4 or the .m3u8 — is caught here instead.
  window.__shardMedia = window.__shardMedia || { mp4: '', m3u8: '' };

  // Which video the page is on. YouTube moves between videos without reloading,
  // so the captured request has to be tagged with the video it belongs to — a
  // template left over from the previous video downloads its audio into the new
  // one and the file fails ("음성 데이터를 받지 못했습니다").
  function shardVid() {
    try {
      var m = location.href.match(/[?&]v=([^&]+)/) || location.href.match(/\/shorts\/([^/?#]+)/);
      return m ? m[1] : location.pathname;
    } catch (e) { return ''; }
  }
  window.__shardVid = shardVid;

  function noteMedia(url) {
    try {
      if (typeof url !== 'string') return;
      var bare = url.split('?')[0].toLowerCase();
      // Skip tiny/streaming-ad fragments where we can: keep the last full file
      // and the last playlist. The page usually fetches the real one last.
      // Every .m3u8 the page asks for is kept: the first is usually the master
      // that lists the qualities, but sites differ, so Rust is given all of them
      // and picks whichever actually parses as a master. The mp4 is the last
      // progressive file, for sites that expose one.
      if (bare.indexOf('.m3u8') >= 0) {
        if (!window.__shardMedia.m3u8) window.__shardMedia.m3u8 = url;
        window.__shardMedia.list = window.__shardMedia.list || [];
        if (window.__shardMedia.list.indexOf(url) < 0) window.__shardMedia.list.push(url);
      } else if (/\.mp4$/.test(bare) || /\.mp4\//.test(bare)) {
        window.__shardMedia.mp4 = url;
      }
    } catch (e) {}
  }

  var original = window.fetch;
  window.fetch = function (input, init) {
    try {
      var isRequest = (typeof Request !== 'undefined') && (input instanceof Request);
      var url = isRequest ? input.url : String(input);
      noteMedia(url);
      if (url.indexOf('videoplayback') >= 0) {
        var method = (init && init.method) || (isRequest ? input.method : 'GET');
        if (method === 'POST') {
          // The body has to be read through a clone: reading a request's body
          // consumes it, and the player is about to send this one.
          var source = (init && init.body != null) ? null : (isRequest ? input.clone() : null);
          var bytes = source ? source.arrayBuffer() : Promise.resolve(init.body);
          Promise.resolve(bytes).then(function (raw) {
            var u8 = raw instanceof ArrayBuffer ? new Uint8Array(raw)
                   : (raw instanceof Uint8Array ? raw : null);
            if (!u8) return;
            var s = '';
            for (var i = 0; i < u8.length; i++) s += String.fromCharCode(u8[i]);
            window.__shardSabr = { url: url, body: btoa(s), vid: shardVid() };
          }).catch(function () {});
        }
      }
    } catch (e) {}
    return original.apply(this, arguments);
  };

  // XHR too: hls.js and many players fetch the manifest and its segments over
  // XMLHttpRequest rather than fetch, so a fetch-only hook would never see them.
  try {
    var openOriginal = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function (method, url) {
      try { noteMedia(String(url)); } catch (e) {}
      return openOriginal.apply(this, arguments);
    };
  } catch (e) {}
})();
"#;

/// Run on demand: report the formats and the captured request.
pub const ASK: &str = r#"
(function () {
  function player() {
    // Gather EVERY element exposing getPlayerResponse, not the first match.
    // Shorts uses `#shorts-player` (not `#movie_player`) AND pre-renders the
    // neighbouring shorts, so several players exist at once — grabbing the first
    // one gave the wrong (off-screen) short, or none, and the download failed.
    var seen = [], out = [];
    function add(el) {
      if (el && typeof el.getPlayerResponse === 'function' && seen.indexOf(el) < 0) {
        seen.push(el); out.push(el);
      }
    }
    var named = document.querySelectorAll('#movie_player, #shorts-player, .html5-video-player');
    for (var i = 0; i < named.length; i++) add(named[i]);
    var videos = document.getElementsByTagName('video');
    for (var i = 0; i < videos.length; i++) {
      var node = videos[i], depth = 0;
      while (node && depth++ < 12) {
        if (typeof node.getPlayerResponse === 'function') { add(node); break; }
        node = node.parentElement;
      }
    }
    if (out.length <= 1) return out[0] || null;
    // More than one: pick the player the user is actually watching — the one
    // most in view, and among those the one that is playing (a paused neighbour
    // scores lower). This is what makes swiping between shorts download the RIGHT
    // short rather than whichever happened to render first.
    var vh = window.innerHeight || document.documentElement.clientHeight || 0;
    var best = null, bestScore = -1;
    for (var i = 0; i < out.length; i++) {
      var el = out[i];
      var vid = el.tagName === 'VIDEO' ? el : el.querySelector('video');
      var r = (vid || el).getBoundingClientRect();
      var visible = Math.max(0, Math.min(r.bottom, vh) - Math.max(r.top, 0));
      var score = visible + (vid && !vid.paused && vid.readyState >= 2 ? 1e7 : 0);
      if (score > bestScore) { bestScore = score; best = el; }
    }
    return best;
  }

  // The player is asked first and the global is only the fallback: that global
  // describes whichever video the page was opened on, and YouTube moves between
  // videos without loading a page.
  function response() {
    var p = player();
    if (p && typeof p.getPlayerResponse === 'function') {
      try {
        var live = p.getPlayerResponse();
        if (live && live.streamingData) return live;
      } catch (e) {}
    }
    if (window.ytInitialPlayerResponse && window.ytInitialPlayerResponse.streamingData) {
      return window.ytInitialPlayerResponse;
    }
    return null;
  }

  function send(payload) { window.ipc.postMessage(JSON.stringify(payload)); }

  // The largest of the pictures the page lists for this video: a song has no
  // cover of its own, and this is what stands in for one.
  function thumb(data) {
    try {
      var all = data.videoDetails.thumbnail.thumbnails || [];
      var best = null;
      for (var i = 0; i < all.length; i++) {
        if (!best || (all[i].width || 0) > (best.width || 0)) best = all[i];
      }
      return best && best.url ? String(best.url) : '';
    } catch (e) {
      return '';
    }
  }

  // Not YouTube: report whatever direct media the page was seen fetching, plus
  // anything a plain <video> points straight at. A blob src is useless — it
  // names memory, not a file — so it is passed over in favour of the captured
  // request underneath it.
  function fallback() {
    var media = window.__shardMedia || { mp4: '', m3u8: '' };
    media.list = media.list || [];
    // The player may not have fetched the master over the network yet, but its
    // URL is usually sitting in the page's own scripts (a flashvars object, a
    // JSON blob). Scan for every .m3u8 there and add it, unescaping the \/ that
    // JSON writes its slashes as, so Rust has the master to read qualities from.
    try {
      var raw = document.documentElement.innerHTML.match(/https?:[^\s"'<>\\]+(?:\\\/[^\s"'<>\\]+)*\.m3u8[^\s"'<>]*/g) || [];
      for (var i = 0; i < raw.length; i++) {
        var u = raw[i].replace(/\\\//g, '/');
        if (media.list.indexOf(u) < 0) media.list.push(u);
        if (!media.m3u8) media.m3u8 = u;
      }
    } catch (e) {}
    var mp4 = media.mp4 || '';
    var hls = media.m3u8 || '';
    if (!mp4) {
      var vids = document.getElementsByTagName('video');
      for (var i = 0; i < vids.length; i++) {
        var src = vids[i].currentSrc || vids[i].src || '';
        if (src && src.indexOf('blob:') !== 0 && src.split('?')[0].toLowerCase().indexOf('.mp4') >= 0) {
          mp4 = src; break;
        }
      }
    }
    var picture = '';
    var og = document.querySelector('meta[property="og:image"]');
    if (og) picture = og.getAttribute('content') || '';
    return {
      formats: [],
      media: mp4,
      hls: hls,
      hlsList: (media.list || []).join('\n'),
      referer: location.href,
      title: document.title || '',
      thumb: picture,
      reason: (mp4 || hls) ? '' : (player() ? 'no-streams' : 'no-player')
    };
  }

  var data = response();
  if (!data || !data.streamingData) {
    send(fallback());
    return;
  }

  var out = [];
  var lists = [data.streamingData.formats || [], data.streamingData.adaptiveFormats || []];
  for (var l = 0; l < lists.length; l++) {
    for (var i = 0; i < lists[l].length; i++) {
      var f = lists[l][i];
      out.push({
        itag: f.itag,
        mimeType: f.mimeType || '',
        quality: f.qualityLabel || f.audioQuality || '',
        bitrate: f.bitrate || 0,
        bytes: String(f.contentLength || '0'),
        lastModified: String(f.lastModified || '0'),
        durationMs: String(f.approxDurationMs || 0),
        xtags: f.xtags || '',
        audioLanguage: (f.audioTrack && f.audioTrack.id ? String(f.audioTrack.id).split('.')[0] : ''),
        audioName: (f.audioTrack && f.audioTrack.displayName) || '',
        audioDefault: !!(f.audioTrack && f.audioTrack.audioIsDefault)
      });
    }
  }

  // The video must have actually started before its request is usable. While it
  // is still buffering (the spinner) the player DOES send a videoplayback request
  // — so a template exists — but the SABR session is not yet delivering media, and
  // a download against it comes back with no audio and fails. Measured: every
  // failure had the player not playing; every success had it playing. currentTime
  // past zero means media has flowed and the request will answer, so gate on it
  // and let the missing-template path say "play it first" instead.
  function started() {
    try {
      var vs = document.getElementsByTagName('video');
      for (var i = 0; i < vs.length; i++) if (vs[i].currentTime > 0.1) return true;
      return false;
    } catch (e) { return true; }
  }
  // Only use the captured request if it belongs to THIS video (a template left
  // over from a previous one, after moving between videos without a page load,
  // would download the wrong audio) AND the video has begun playing.
  var here = (window.__shardVid ? window.__shardVid() : '');
  var captured = window.__shardSabr;
  if (captured && captured.vid && here && captured.vid !== here) captured = null;
  if (captured && !started()) captured = null;
  send({
    formats: out,
    title: (data.videoDetails || {}).title || '',
    thumb: thumb(data),
    templateUrl: captured ? captured.url : '',
    templateBody: captured ? captured.body : '',
    reason: captured ? '' : 'not-played'
  });
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(json: &str) -> Offer {
        Offer::parse(json).expect("offer")
    }

    #[test]
    fn reads_a_format_list() {
        let found = offer(
            r#"{"title":"clip","formats":[
                {"itag":401,"mimeType":"video/mp4; codecs=\"av01.0.13M\"","quality":"2160p60",
                 "bitrate":14605421,"bytes":"3328395810","lastModified":"1786086212663337"},
                {"itag":251,"mimeType":"audio/webm; codecs=\"opus\"","quality":"AUDIO_QUALITY_MEDIUM",
                 "bitrate":139283,"bytes":"31801205","lastModified":"1786078981725454"}]}"#,
        );
        assert_eq!(found.title, "clip");
        assert_eq!(found.formats[0].codec(), "AV1");
        assert_eq!(found.formats[0].height(), 2160);
        assert_eq!(found.formats[0].size(), 3_328_395_810);
        assert!(found.formats[1].is_audio());
    }

    #[test]
    fn orders_video_by_resolution_then_size() {
        let found = offer(
            r#"{"formats":[
                {"itag":1,"mimeType":"video/mp4","quality":"1080p","bytes":"246","lastModified":"1"},
                {"itag":2,"mimeType":"video/mp4","quality":"2160p","bytes":"900","lastModified":"1"},
                {"itag":3,"mimeType":"video/webm","quality":"1080p","bytes":"119","lastModified":"1"}]}"#,
        );
        let order: Vec<u32> = found.video_tracks().iter().map(|f| f.itag).collect();
        // Biggest picture first; within a resolution, the smaller file leads.
        assert_eq!(order, vec![2, 3, 1]);
    }

    fn wish() -> AudioWish {
        AudioWish { language: String::new(), quality: AudioQuality::Best, portable: false }
    }

    #[test]
    fn picks_the_best_track_under_the_ceiling() {
        let found = offer(
            r#"{"formats":[
                {"itag":249,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":48000,"bytes":"2","lastModified":"1"},
                {"itag":250,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":76000,"bytes":"5","lastModified":"1"},
                {"itag":251,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":130000,"bytes":"10","lastModified":"1"}]}"#,
        );
        // The 130k track, not the 76k one: beside a video of a hundred
        // megabytes the difference in size is not worth the difference in sound.
        assert_eq!(found.best_audio(&wish()).expect("audio").itag, 251);
    }

    #[test]
    fn the_asked_for_language_wins_over_the_original() {
        let found = offer(
            r#"{"formats":[
                {"itag":251,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":130000,
                 "lastModified":"1","audioLanguage":"en","audioName":"English original",
                 "audioDefault":true},
                {"itag":251,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":130000,
                 "lastModified":"2","audioLanguage":"ko","audioName":"Korean"}]}"#,
        );
        let korean =
            AudioWish { language: "ko".into(), quality: AudioQuality::Best, portable: false };
        assert_eq!(found.best_audio(&korean).expect("audio").audio_language, "ko");
        // Asking for nothing takes the track the video itself plays.
        assert_eq!(found.best_audio(&wish()).expect("audio").audio_language, "en");
    }

    #[test]
    fn a_language_the_video_does_not_have_falls_back_to_its_default() {
        let found = offer(
            r#"{"formats":[
                {"itag":251,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":130000,
                 "lastModified":"1","audioLanguage":"en","audioDefault":true}]}"#,
        );
        let japanese =
            AudioWish { language: "ja".into(), quality: AudioQuality::Best, portable: false };
        assert_eq!(found.best_audio(&japanese).expect("audio").audio_language, "en");
    }

    #[test]
    fn a_music_file_takes_the_codec_every_phone_plays() {
        let found = offer(
            r#"{"formats":[
                {"itag":251,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":130000,"lastModified":"1"},
                {"itag":140,"mimeType":"audio/mp4; codecs=\"mp4a.40.2\"","bitrate":128000,"lastModified":"1"}]}"#,
        );
        let portable =
            AudioWish { language: String::new(), quality: AudioQuality::Best, portable: true };
        assert_eq!(found.best_audio(&portable).expect("audio").codec(), "AAC");
        // Inside a video the better codec wins instead.
        assert_eq!(found.best_audio(&wish()).expect("audio").codec(), "Opus");
    }

    #[test]
    fn music_takes_the_highest_bitrate_aac_above_the_soundtrack_ceiling() {
        // A track offering 256k AAC beside the usual 128k. Muxed into a video
        // the 200k ceiling would keep 128k; saved as music it takes the 256k,
        // matching what the phone build saves for the same video.
        let found = offer(
            r#"{"formats":[
                {"itag":140,"mimeType":"audio/mp4; codecs=\"mp4a.40.2\"","bitrate":128000,"lastModified":"1"},
                {"itag":141,"mimeType":"audio/mp4; codecs=\"mp4a.40.2\"","bitrate":256000,"lastModified":"1"}]}"#,
        );
        let music =
            AudioWish { language: String::new(), quality: AudioQuality::Best, portable: true };
        assert_eq!(found.best_audio(&music).expect("audio").itag, 141);
        // The same offer muxed into a video keeps the gentler ceiling.
        assert_eq!(found.best_audio(&wish()).expect("audio").itag, 140);
    }

    #[test]
    fn asking_for_the_smallest_gets_the_smallest() {
        let found = offer(
            r#"{"formats":[
                {"itag":249,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":50000,"lastModified":"1"},
                {"itag":251,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":130000,"lastModified":"1"}]}"#,
        );
        let small =
            AudioWish { language: String::new(), quality: AudioQuality::Small, portable: false };
        assert_eq!(found.best_audio(&small).expect("audio").itag, 249);
    }

    #[test]
    fn a_page_that_has_not_played_yet_has_no_template() {
        let found = offer(r#"{"formats":[],"reason":"not-played"}"#);
        assert!(found.template().is_none());
        assert_eq!(found.reason, "not-played");
    }

    #[test]
    fn decodes_the_captured_request() {
        let found = offer(r#"{"templateUrl":"https://x/videoplayback","templateBody":"aGVsbG8="}"#);
        let template = found.template().expect("template");
        assert_eq!(template.body, b"hello");
    }

    #[test]
    fn rejects_a_body_that_is_not_base64() {
        let found = offer(r#"{"templateUrl":"https://x","templateBody":"not base64!"}"#);
        assert!(found.template().is_none());
    }
}

/// The download control, injected into every page.
///
/// Drawn in the page rather than as a window of its own: the browser is already
/// there, and a native list would be a second set of colours, fonts and
/// scrolling to keep in step with the rest. It talks to the app by posting
/// messages, which is the same channel the format list already uses.
pub const CONTROL: &str = r#"
(function () {
  if (window.__shardUi) return;
  window.__shardUi = true;
  // The top frame only. This is injected into every frame a page has, and a
  // page with ten adverts in it was getting ten copies of the control, ten
  // timers and ten scroll listeners — each one measuring video elements and
  // forcing the browser to lay the page out again to answer.
  if (window.top !== window) return;

  // Styles are set property by property rather than through a stylesheet.
  // A page may forbid script-inserted <style> through its content policy, and
  // when it does the whole control disappears with no error anyone would see;
  // the object model is not covered by that rule.
  function style(el, rules) {
    for (var key in rules) el.style[key] = rules[key];
  }

  var PANEL = {
    position: 'fixed', right: '18px', bottom: '18px', zIndex: '2147483647',
    width: '330px', maxHeight: '70vh', overflowY: 'auto',
    background: '#111418', color: '#e2e8f0', border: '1px solid #2b323c',
    borderRadius: '11px', font: '13px system-ui, sans-serif',
    boxShadow: '0 10px 40px rgba(0,0,0,.5)'
  };

  function root() { return document.body || document.documentElement; }

  function button() {
    var b = document.getElementById('shard-b');
    if (b && b.isConnected) return b;
    b = document.createElement('div');
    b.id = 'shard-b';
    b.textContent = '⤓ 영상 받기';
    style(b, {
      position: 'fixed', right: '18px', bottom: '18px', zIndex: '2147483647',
      background: '#111418', color: '#e2e8f0', border: '1px solid #2b323c',
      borderRadius: '9px', padding: '9px 14px', font: '13px system-ui, sans-serif',
      cursor: 'pointer', opacity: '.9'
    });
    b.onclick = function () { window.ipc.postMessage(JSON.stringify({ ask: 1 })); };
    root().appendChild(b);
    return b;
  }

  /// Sit in the corner of the video, or in the corner of the window when there
  /// is no video to sit on. Placed by measurement rather than by inserting the
  /// control into the player: players rebuild their own controls constantly and
  /// anything left inside one is thrown away with them.
  function place(b) {
    var best = null, area = 0;
    var videos = document.getElementsByTagName('video');
    for (var i = 0; i < videos.length; i++) {
      var r = videos[i].getBoundingClientRect();
      if (r.width * r.height > area && r.width > 120 && r.height > 80) {
        area = r.width * r.height;
        best = r;
      }
    }
    if (!best) {
      b.style.top = '';
      b.style.left = '';
      b.style.right = '18px';
      b.style.bottom = '18px';
      return;
    }
    b.style.bottom = '';
    b.style.right = '';
    b.style.top = Math.max(8, best.top + 10) + 'px';
    // Measured after it is on screen, so its own width is known.
    var width = b.offsetWidth || 96;
    b.style.left = Math.max(8, best.right - width - 10) + 'px';
  }

  function panel() {
    var p = document.getElementById('shard-p');
    if (p && p.isConnected) return p;
    p = document.createElement('div');
    p.id = 'shard-p';
    style(p, PANEL);
    root().appendChild(p);
    return p;
  }

  function row(text, detail, onclick) {
    var r = document.createElement('div');
    style(r, { padding: '10px 14px', borderBottom: '1px solid #1b2027', cursor: 'pointer' });
    var q = document.createElement('div');
    q.textContent = text;
    r.appendChild(q);
    if (detail) {
      var d = document.createElement('div');
      d.textContent = detail;
      style(d, { fontSize: '11px', color: '#8a92a2', marginTop: '2px' });
      r.appendChild(d);
    }
    r.onclick = onclick;
    return r;
  }

  function heading(text) {
    var h = document.createElement('div');
    h.textContent = text;
    style(h, {
      padding: '12px 14px', borderBottom: '1px solid #2b323c',
      fontSize: '13px', fontWeight: '600'
    });
    return h;
  }

  window.__shardClose = function () {
    var p = document.getElementById('shard-p');
    if (p) p.remove();
    button().style.display = '';
  };

  window.__shardList = function (rows) {
    button().style.display = 'none';
    var p = panel();
    p.textContent = '';
    p.appendChild(heading('화질 선택'));
    for (var i = 0; i < rows.length; i++) {
      (function (r) {
        p.appendChild(row(r.quality, r.detail, function () {
          window.ipc.postMessage(JSON.stringify({ choose: r.itag }));
        }));
      })(rows[i]);
    }
    p.appendChild(row('닫기', '', window.__shardClose));
  };

  // Something already true, with the way past it on the panel.
  //
  // Said every time rather than once and then let through: a warning that stops
  // warning is one nobody can rely on. The way past it is a row, so it takes one
  // press rather than pressing the same thing again somewhere else.
  window.__shardAgain = function (text, itag) {
    button().style.display = 'none';
    var p = panel();
    p.textContent = '';
    p.appendChild(heading('영상 받기'));
    var m = document.createElement('div');
    m.textContent = text;
    style(m, { padding: '14px', color: '#8a92a2', fontSize: '12px', lineHeight: '1.5' });
    p.appendChild(m);
    p.appendChild(row('다시 받기', '', function () {
      window.ipc.postMessage(JSON.stringify({ choose: itag, force: 1 }));
    }));
    p.appendChild(row('닫기', '', window.__shardClose));
  };

  window.__shardSay = function (text, closable) {
    button().style.display = 'none';
    var p = panel();
    p.textContent = '';
    p.appendChild(heading('영상 받기'));
    var m = document.createElement('div');
    m.textContent = text;
    style(m, { padding: '14px', color: '#8a92a2', fontSize: '12px', lineHeight: '1.5' });
    p.appendChild(m);
    if (closable) p.appendChild(row('닫기', '', window.__shardClose));
  };

  // Said and gone. A download takes minutes and the page is for watching; a
  // panel that has to be dismissed is one more thing between the user and the
  // next video, so this one leaves on its own.
  window.__shardFlash = function (text) {
    window.__shardSay(text, false);
    setTimeout(window.__shardClose, 1600);
  };

  // Kept alive rather than placed once. This runs before the document exists,
  // and the sites it runs on replace their own body as they navigate — a
  // control appended once is gone the moment that happens.
  function keep() {
    if (!root()) return;
    if (document.getElementById('shard-p')) return;
    place(button());
  }

  // Measuring an element's position forces the browser to work out the whole
  // page's layout before it can answer. Doing that straight from a scroll
  // event means doing it for every event in the stream, which is what made
  // scrolling and page changes feel heavy. Asking for a frame instead collapses
  // a burst of events into one measurement, taken when the browser was going to
  // lay the page out anyway.
  var due = false;
  function soon() {
    if (due) return;
    due = true;
    requestAnimationFrame(function () {
      due = false;
      keep();
    });
  }

  keep();
  // Some responsive grids (xvideos' home thumbnails) lay out once, early, before
  // the view has settled, and render OVERLAPPING until something makes them
  // recompute. A plain resize event does it, page-side only — once on load, and
  // again shortly after for grids that fill in late. Parity with iOS (didFinish).
  function nudgeLayout() {
    try {
      window.dispatchEvent(new Event('resize'));
      setTimeout(function () { window.dispatchEvent(new Event('resize')); }, 350);
    } catch (e) {}
  }
  window.addEventListener('load', nudgeLayout);
  document.addEventListener('DOMContentLoaded', soon);
  // Often enough to follow a player that resizes or moves, rarely enough to
  // cost nothing on a page with no video on it.
  setInterval(soon, 1200);
  window.addEventListener('resize', soon);

  // Best-effort YouTube ad skipping — parity with iOS/Android. Defensive: acts only when an
  // ad is actually shown, all in try/catch, so a markup change just makes it a no-op.
  function skipAds() {
    if (document.hidden) return;
    try {
      var skip = document.querySelector('.ytp-ad-skip-button, .ytp-ad-skip-button-modern, .ytp-skip-ad-button, .ytp-ad-skip-button-container button');
      if (skip) { skip.click(); return; }
      var player = document.querySelector('.html5-video-player');
      if (player && player.classList.contains('ad-showing')) {
        var v = player.querySelector('video');
        if (v && isFinite(v.duration) && v.duration > 0) v.currentTime = v.duration;
      }
      var overlayClose = document.querySelector('.ytp-ad-overlay-close-button');
      if (overlayClose) { try { overlayClose.click(); } catch (e) {} }
    } catch (e) {}
  }
  setInterval(skipAds, 350);
  // React the instant an ad appears rather than up to 350ms later — that poll gap
  // is why an ad sometimes flashed for a frame before being skipped. The player
  // gains the `ad-showing` class and the skip button is inserted; watching for both
  // catches them immediately. The player is rebuilt on SPA navigation, so re-attach.
  try {
    var adWatch = new MutationObserver(function () { skipAds(); });
    var watchPlayer = function () {
      var p = document.querySelector('.html5-video-player');
      if (p && p.__shardAdWatched !== true) {
        p.__shardAdWatched = true;
        adWatch.observe(p, { attributes: true, attributeFilter: ['class'], childList: true, subtree: true });
      }
    };
    watchPlayer();
    setInterval(watchPlayer, 2000);
  } catch (e) {}

  // Live chat opens itself on a stream/premiere and takes a column of the page,
  // which is in the way when scrolling. Collapse it — but ONCE per video, so a
  // user who opens it back up is left alone (tracked by URL, which changes as
  // YouTube moves between videos without loading a page). Best-effort: the
  // `collapsed` attribute is how the frame's own button toggles it.
  var chatDoneFor = '';
  function collapseChat() {
    try {
      var chat = document.querySelector('ytd-live-chat-frame');
      if (!chat) return;
      if (chatDoneFor !== location.href && !chat.hasAttribute('collapsed')) {
        chat.setAttribute('collapsed', '');
        chatDoneFor = location.href;
      }
    } catch (e) {}
  }
  setInterval(collapseChat, 500);

  // Not in the capture phase, and passive: this only reads, so the browser is
  // free to scroll without waiting to hear whether it may.
  window.addEventListener('scroll', soon, { passive: true });

  // A click anywhere off the panel puts it away. The panel floats over the page,
  // and once it was open — a quality list or a download in progress — the only way
  // out was its own 닫기 row; a click on the video behind it left it sitting there.
  // Capture phase so it is seen before the page swallows the click; the button is
  // exempt because that is what opens the panel, and clicks inside the panel are
  // its own rows.
  document.addEventListener('pointerdown', function (e) {
    var p = document.getElementById('shard-p');
    if (!p || p.contains(e.target)) return;
    var b = document.getElementById('shard-b');
    if (b && b.contains(e.target)) return;
    // Close ONLY — the click that dismisses the panel must not also reach the page
    // under it (playing the video, following a link). Stop this pointerdown, and
    // eat the click that would otherwise follow it. The timeout drops the eater if
    // no click comes (a drag, say), so an unrelated later click is not swallowed.
    e.preventDefault();
    e.stopPropagation();
    window.__shardClose();
    var eat = function (ev) {
      ev.preventDefault();
      ev.stopPropagation();
      document.removeEventListener('click', eat, true);
    };
    document.addEventListener('click', eat, true);
    setTimeout(function () { document.removeEventListener('click', eat, true); }, 400);
  }, true);
})();
"#;

/// Ask the page to say something, with a row that goes ahead anyway.
pub fn again_script(text: &str, itag: u32) -> String {
    format!(
        "window.__shardAgain && window.__shardAgain('{}', {itag});",
        escape(text)
    )
}

/// Ask the page to show a list of qualities.
pub fn list_script(rows: &[(u32, String, String)]) -> String {
    let mut json = String::from("[");
    for (index, (itag, quality, detail)) in rows.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "{{\"itag\":{itag},\"quality\":\"{}\",\"detail\":\"{}\"}}",
            escape(quality),
            escape(detail)
        ));
    }
    json.push(']');
    format!("window.__shardList({json});")
}

/// Ask the page to show a line of text instead of a list.
pub fn say_script(text: &str, closable: bool) -> String {
    format!("window.__shardSay('{}', {});", escape(text), closable)
}

/// The same panel, but it puts itself away after a moment.
pub fn flash_script(text: &str) -> String {
    format!("window.__shardFlash('{}');", escape(text))
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('"', "\\\"")
        .replace('\n', " ")
}
