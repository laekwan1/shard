//! HLS playlists.
//!
//! A streaming "video" is a playlist of segments, and a *master* playlist is a
//! list of those playlists at different bitrates. The page's own player picks
//! one silently; this is what lets the user pick instead — the same job the
//! Android side does in `Hls.kt`, ported here so the desktop can read the sites
//! that serve HLS (xvideos, pornhub and most tube sites) rather than only
//! YouTube's own streaming data.
//!
//! Pure and network-free on purpose, the same as [`super::sabr`]: parsing a
//! playlist is the part most likely to meet a shape it did not expect, and it is
//! the part that is cheapest to test. The fetching and the muxing live in
//! [`super::save`], which is where the network already is.

/// One rendition from a master playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub url: String,
    /// Pixels, when the playlist declares them.
    pub width: u32,
    pub height: u32,
    /// Bits per second, when declared.
    pub bandwidth: u64,
}

impl Variant {
    /// What the user sees in the quality list.
    pub fn label(&self) -> String {
        let quality = if self.height > 0 {
            format!("{}p", self.height)
        } else {
            "화질 미상".to_string()
        };
        let rate = if self.bandwidth > 0 {
            format!(" · {:.1} Mbps", self.bandwidth as f64 / 1_000_000.0)
        } else {
            String::new()
        };
        quality + &rate
    }
}

/// A key that locks a media playlist's segments, from `#EXT-X-KEY`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRef {
    /// The method as written — only `AES-128` is handled downstream; anything
    /// else is carried so the caller can refuse it rather than save garbage.
    pub method: String,
    pub uri: String,
    /// The initialisation vector, when the playlist pins one. Absent means the
    /// segment's media sequence number stands in for it.
    pub iv: Option<[u8; 16]>,
}

/// One segment of a media playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub url: String,
    /// `(length, offset)` from `#EXT-X-BYTERANGE`, when the segments are packed
    /// into one file at offsets rather than being one file each.
    pub byte_range: Option<(u64, u64)>,
    /// The key in force for this segment, if the playlist is encrypted.
    pub key: Option<KeyRef>,
}

/// Whether a URL names a playlist, by its extension. The query is dropped first:
/// a `.m3u8?token=…` is still a playlist.
pub fn is_playlist(url: &str) -> bool {
    url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase().ends_with(".m3u8")
}

/// True when `text` lists renditions rather than segments.
pub fn is_master(text: &str) -> bool {
    text.contains("#EXT-X-STREAM-INF")
}

/// The `#EXT-X-MAP` initialisation segment, when a media playlist has one.
///
/// Fragmented-MP4 playlists put the `moov` in a separate init segment that every
/// media segment is read against; without it the fragments cannot be parsed.
pub fn map_init(text: &str, base_url: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            if let Some(uri) = attr(rest, "URI") {
                return Some(resolve(base_url, &uri));
            }
        }
    }
    None
}

/// Renditions in a master playlist, highest quality first.
///
/// A rendition line and its URL are on separate lines, so the two are read
/// together; an attribute line with nothing after it is skipped rather than
/// paired with the wrong URL.
pub fn variants(text: &str, base_url: &str) -> Vec<Variant> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found: Vec<Variant> = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        if !line.starts_with("#EXT-X-STREAM-INF") {
            continue;
        }
        let Some(target) = lines[index + 1..]
            .iter()
            .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        else {
            continue;
        };
        let (width, height) = resolution(line);
        found.push(Variant {
            url: resolve(base_url, target.trim()),
            width,
            height,
            bandwidth: attr(line, "BANDWIDTH").and_then(|v| v.parse().ok()).unwrap_or(0),
        });
    }

    // Distinct by URL: a playlist often lists the same rendition twice with
    // different audio groups, and offering it twice is just confusing.
    let mut seen = std::collections::HashSet::new();
    found.retain(|v| seen.insert(v.url.clone()));
    // Highest first, by height then bitrate.
    found.sort_by(|a, b| b.height.cmp(&a.height).then(b.bandwidth.cmp(&a.bandwidth)));
    found
}

/// How long a media playlist runs, in seconds.
///
/// The playlist never states a byte count, but duration times bitrate is a good
/// enough figure to choose by — and choosing is the only thing it is for.
pub fn duration_seconds(text: &str) -> f64 {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("#EXTINF:"))
        .filter_map(|rest| {
            // `#EXTINF:6.006,title` — the number ends at the comma.
            let number = rest.split(',').next().unwrap_or(rest).trim();
            number.parse::<f64>().ok()
        })
        .sum()
}

/// Segments of a media playlist, in play order, each carrying the key and byte
/// range in force where the playlist declares them.
///
/// `#EXT-X-KEY` sets the key for every segment that follows until the next one;
/// `#EXT-X-BYTERANGE` applies to the one segment right after it, its offset
/// defaulting to the end of the previous range in the same resource.
pub fn segments(text: &str, base_url: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut key: Option<KeyRef> = None;
    let mut pending_range: Option<(u64, u64)> = None;
    let mut range_cursor: u64 = 0;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            key = parse_key(rest, base_url);
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            pending_range = parse_byte_range(rest, &mut range_cursor);
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        // A URL line: the segment itself.
        out.push(Segment {
            url: resolve(base_url, line),
            byte_range: pending_range.take(),
            key: key.clone(),
        });
    }
    out
}

/// `LEN[@OFFSET]` — the offset defaults to where the last range in the same
/// resource ended, which the cursor tracks.
fn parse_byte_range(rest: &str, cursor: &mut u64) -> Option<(u64, u64)> {
    let mut parts = rest.trim().split('@');
    let len: u64 = parts.next()?.trim().parse().ok()?;
    let offset: u64 = match parts.next() {
        Some(o) => o.trim().parse().ok()?,
        None => *cursor,
    };
    *cursor = offset + len;
    Some((len, offset))
}

fn parse_key(rest: &str, base_url: &str) -> Option<KeyRef> {
    let method = attr(rest, "METHOD").unwrap_or_default();
    // NONE turns encryption off for the segments that follow.
    if method.eq_ignore_ascii_case("NONE") || method.is_empty() {
        return None;
    }
    let uri = attr(rest, "URI").map(|u| resolve(base_url, &u)).unwrap_or_default();
    let iv = attr(rest, "IV").and_then(|v| parse_iv(&v));
    Some(KeyRef { method, uri, iv })
}

/// `0x` followed by 32 hex digits, big-endian.
fn parse_iv(value: &str) -> Option<[u8; 16]> {
    let hex = value.trim().strip_prefix("0x").or_else(|| value.trim().strip_prefix("0X"))?;
    if hex.len() != 32 {
        return None;
    }
    let mut iv = [0u8; 16];
    for (i, byte) in iv.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(iv)
}

/// `RESOLUTION=WIDTHxHEIGHT`, or `(0, 0)` when it is not declared.
fn resolution(line: &str) -> (u32, u32) {
    let Some(value) = attr(line, "RESOLUTION") else {
        return (0, 0);
    };
    let mut parts = value.split(['x', 'X']);
    let w = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let h = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    (w, h)
}

/// Read `NAME=VALUE` out of an attribute list, honouring quotes.
///
/// Hand-rolled rather than a regex because the crate keeps no regex dependency,
/// and the grammar is small: comma-separated pairs, values optionally in double
/// quotes and then allowed to contain commas.
fn attr(line: &str, name: &str) -> Option<String> {
    let mut rest = line;
    loop {
        let at = rest.find(name)?;
        let after = &rest[at + name.len()..];
        // Guard against matching a name that is a tail of a longer one, and
        // require the `=` right after.
        let before_ok = at == 0 || !rest.as_bytes()[at - 1].is_ascii_alphanumeric();
        if let Some(value) = after.strip_prefix('=') {
            if before_ok {
                return Some(read_value(value));
            }
        }
        // Not this occurrence — step past it and look again.
        rest = &rest[at + name.len()..];
    }
}

fn read_value(value: &str) -> String {
    let value = value.trim_start();
    if let Some(inner) = value.strip_prefix('"') {
        // Quoted: everything up to the closing quote, commas and all.
        inner.split('"').next().unwrap_or(inner).to_string()
    } else {
        // Bare: up to the next comma.
        value.split(',').next().unwrap_or(value).trim().to_string()
    }
}

/// Resolve a possibly relative playlist entry against the playlist's URL.
///
/// A small hand join rather than a URL crate — the cases a playlist actually
/// uses are few: an absolute URL, a scheme-relative `//host/…`, a rooted
/// `/path`, or a relative path that may climb with `../`.
pub fn resolve(base: &str, reference: &str) -> String {
    let reference = reference.trim();
    if reference.is_empty() {
        return base.to_string();
    }
    // Already absolute.
    if reference.contains("://") {
        return reference.to_string();
    }
    let (scheme, after_scheme) = match base.split_once("://") {
        Some((s, rest)) => (s, rest),
        None => return reference.to_string(),
    };
    // Scheme-relative: keep the base's scheme, take the rest wholesale.
    if let Some(rest) = reference.strip_prefix("//") {
        return format!("{scheme}://{rest}");
    }
    // The base's authority is up to its first '/', and its path is the rest.
    let (authority, base_path) = match after_scheme.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (after_scheme, String::new()),
    };
    // Rooted: replace the whole path.
    if reference.starts_with('/') {
        return format!("{scheme}://{authority}{}", normalise(reference));
    }
    // Relative: drop the base's last segment (and any query/fragment), then join.
    let base_dir = {
        let path = base_path.split(['?', '#']).next().unwrap_or(&base_path);
        match path.rfind('/') {
            Some(i) => &path[..=i],
            None => "/",
        }
    };
    let joined = format!("{base_dir}{reference}");
    format!("{scheme}://{authority}{}", normalise(&joined))
}

/// Collapse `.` and `..` in a path, the way a browser would before requesting.
fn normalise(path: &str) -> String {
    // Keep any query/fragment aside; only the path portion has segments.
    let (path_only, tail) = match path.find(['?', '#']) {
        Some(i) => (&path[..i], &path[i..]),
        None => (path, ""),
    };
    let mut out: Vec<&str> = Vec::new();
    for segment in path_only.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    let mut result = String::from("/");
    result.push_str(&out.join("/"));
    result.push_str(tail);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_playlist_is_recognised_by_its_extension_past_any_query() {
        assert!(is_playlist("https://x.com/a/index.m3u8"));
        assert!(is_playlist("https://x.com/a/index.M3U8?token=abc"));
        assert!(!is_playlist("https://x.com/a/seg.ts"));
        assert!(!is_playlist("https://x.com/a/video.mp4"));
    }

    #[test]
    fn a_master_is_told_from_a_media_playlist_by_its_stream_tag() {
        assert!(is_master("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nlow.m3u8"));
        assert!(!is_master("#EXTM3U\n#EXTINF:6.0,\nseg0.ts"));
    }

    #[test]
    fn variants_come_back_highest_first_and_deduplicated() {
        let text = "#EXTM3U\n\
            #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\n\
            360/index.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720\n\
            720/index.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720,AUDIO=\"en\"\n\
            720/index.m3u8\n";
        let v = variants(text, "https://cdn.example.com/hls/master.m3u8");
        assert_eq!(v.len(), 2, "the duplicate 720 rendition is dropped");
        assert_eq!(v[0].height, 720);
        assert_eq!(v[0].url, "https://cdn.example.com/hls/720/index.m3u8");
        assert_eq!(v[1].height, 360);
        assert_eq!(v[0].label(), "720p · 2.5 Mbps");
    }

    #[test]
    fn duration_sums_every_segment_length() {
        let text = "#EXTM3U\n#EXTINF:6.006,\nseg0.ts\n#EXTINF:5.9,\nseg1.ts\n";
        assert!((duration_seconds(text) - 11.906).abs() < 1e-9);
    }

    #[test]
    fn segments_resolve_relative_and_rooted_and_absolute() {
        let text = "#EXTM3U\n\
            #EXTINF:6,\nseg0.ts\n\
            #EXTINF:6,\n/abs/seg1.ts\n\
            #EXTINF:6,\nhttps://other.cdn/seg2.ts\n";
        let s = segments(text, "https://cdn.example.com/hls/360/index.m3u8");
        assert_eq!(s[0].url, "https://cdn.example.com/hls/360/seg0.ts");
        assert_eq!(s[1].url, "https://cdn.example.com/abs/seg1.ts");
        assert_eq!(s[2].url, "https://other.cdn/seg2.ts");
    }

    #[test]
    fn a_climbing_relative_path_is_normalised() {
        assert_eq!(
            resolve("https://c.com/a/b/index.m3u8", "../x/seg.ts"),
            "https://c.com/a/x/seg.ts"
        );
        assert_eq!(
            resolve("https://c.com/a/b/index.m3u8", "//other.com/y.m3u8"),
            "https://other.com/y.m3u8"
        );
    }

    #[test]
    fn a_key_carries_its_method_uri_and_iv() {
        let text = "#EXTM3U\n\
            #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x00000000000000000000000000000001\n\
            #EXTINF:6,\nseg0.ts\n";
        let s = segments(text, "https://cdn.example.com/hls/index.m3u8");
        let key = s[0].key.as_ref().expect("the segment is encrypted");
        assert_eq!(key.method, "AES-128");
        assert_eq!(key.uri, "https://cdn.example.com/hls/key.bin");
        assert_eq!(key.iv.unwrap()[15], 1);
    }

    #[test]
    fn a_none_key_turns_encryption_back_off() {
        let text = "#EXTM3U\n\
            #EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n\
            #EXTINF:6,\na.ts\n\
            #EXT-X-KEY:METHOD=NONE\n\
            #EXTINF:6,\nb.ts\n";
        let s = segments(text, "https://c.com/i.m3u8");
        assert!(s[0].key.is_some());
        assert!(s[1].key.is_none(), "NONE clears the key for what follows");
    }

    #[test]
    fn byte_ranges_default_their_offset_to_the_previous_end() {
        let text = "#EXTM3U\n\
            #EXT-X-BYTERANGE:1000@0\n#EXTINF:6,\nall.mp4\n\
            #EXT-X-BYTERANGE:2000\n#EXTINF:6,\nall.mp4\n";
        let s = segments(text, "https://c.com/i.m3u8");
        assert_eq!(s[0].byte_range, Some((1000, 0)));
        assert_eq!(s[1].byte_range, Some((2000, 1000)), "offset follows the last range");
    }

    #[test]
    fn a_map_init_segment_is_found_and_resolved() {
        let text = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:6,\nseg0.m4s\n";
        assert_eq!(
            map_init(text, "https://c.com/v/index.m3u8"),
            Some("https://c.com/v/init.mp4".to_string())
        );
    }
}
