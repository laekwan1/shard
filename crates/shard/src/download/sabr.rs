//! YouTube's server-driven delivery, spoken from this side.
//!
//! YouTube no longer hands out an address per stream. Every format is listed
//! and none of them can be fetched — measured on a live page: 403 with the
//! address as given, 403 with the throttling parameter stripped, and the
//! player's own code no longer contains the function that used to rewrite it.
//! What the player does instead is POST a description of its situation to one
//! endpoint and let the server decide which format, and which seconds of it, to
//! send back.
//!
//! So that is what this does. The request is not built from nothing: a page
//! makes a real one while the video plays, and it is captured and used as a
//! template. That matters more than it sounds — the template carries the server
//! configuration and the bot-check token, both opaque and neither reproducible
//! outside a browser. Everything here changes is the part that says *what to
//! send*: which formats are on the menu, and how far along playback is.
//!
//! The reply is a stream of length-prefixed parts rather than a file, so the
//! media has to be picked out of it and reassembled per format.
//!
//! This will break. It is a private protocol with no promises attached, and the
//! field numbers below were read off a live request rather than a
//! specification. The cost of it breaking is that YouTube downloads stop
//! working until the numbers are read again; nothing else depends on this.

/// What a page captured: one real request, ready to be rewritten.
#[derive(Clone, Debug)]
pub struct Template {
    pub url: String,
    pub body: Vec<u8>,
}

/// A format as the player response describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub itag: u32,
    /// Identifies this exact encoding to the server; a format is the pair.
    pub last_modified: u64,
    pub xtags: String,
    /// Total size, from the page. Zero when it did not say.
    pub bytes: u64,
}

/// What has been received of one track, as the server needs it described.
#[derive(Clone, Debug, Default)]
pub struct Held {
    pub duration_ms: u64,
    pub last_sequence: u64,
}

// ---- request fields ------------------------------------------------------

const CLIENT_STATE: u32 = 1;
const SELECTED_FORMATS: u32 = 2;
const BUFFERED_RANGES: u32 = 3;
const AUDIO_CHOICES: u32 = 16;
const VIDEO_CHOICES: u32 = 17;

/// Inside ClientAbrState.
const VIEWPORT_WIDTH: u32 = 18;
const VIEWPORT_HEIGHT: u32 = 19;
const PLAYER_TIME_MS: u32 = 28;

// ---- reply parts ---------------------------------------------------------

const PART_MEDIA_HEADER: u64 = 20;
const PART_MEDIA: u64 = 21;

const HEADER_ID: u32 = 1;
const HEADER_ITAG: u32 = 3;
const HEADER_START_BYTE: u32 = 6;
const HEADER_SEQUENCE: u32 = 9;
const HEADER_DURATION_MS: u32 = 12;

/// Rewrite the captured request to ask for one pair of formats at one point in
/// the video.
///
/// Fields with no use here are copied across untouched. That is deliberate: the
/// request carries a great deal only the server understands, and dropping a
/// field because this file has no name for it would be guessing.
///
/// `audio_only` leaves the video menu out. Measured: without it the server
/// still sends video, but it picks the smallest there is — four hundred
/// kilobytes against fifty megabytes for the same stretch of sound. Nothing
/// found stops it sending video altogether; the field that looks like it should
/// (the track-types bitfield in the client state) changes nothing at all.
pub fn build(
    template: &[u8],
    video: &Track,
    audio: &Track,
    decoy: &Track,
    player_time_ms: u64,
    held: &[(&Track, &Held)],
    audio_only: bool,
) -> Vec<u8> {
    let fields = split(template);
    let mut out = Vec::with_capacity(template.len() + 128);

    if let Some(state) = fields.iter().find(|f| f.number == CLIENT_STATE) {
        write_length_field(&mut out, CLIENT_STATE, &playback_state(state.value, player_time_ms));
    }

    // A format that is deliberately *not* one of the two being downloaded.
    //
    // This field says what is already playing, and the server does not resend
    // the few hundred bytes of setup at the head of a stream it believes the
    // client already has. Naming the wanted format here — the obvious thing to
    // do — produced a file with every frame in it and no way to open it.
    // Naming something else leaves the target looking new.
    write_length_field(&mut out, SELECTED_FORMATS, &format_id(decoy));

    // What has arrived so far.
    //
    // Without this the server sends the opening of the stream and then keeps
    // answering with the same seconds: it decides what comes next from what the
    // client says it holds, and a client holding nothing is always at the
    // beginning.
    for (track, have) in held.iter().filter(|(_, h)| h.duration_ms > 0) {
        let mut range = Vec::new();
        write_length_field(&mut range, 1, &format_id(track));
        write_varint_field(&mut range, 2, 0);
        write_varint_field(&mut range, 3, have.duration_ms);
        write_varint_field(&mut range, 4, 1);
        write_varint_field(&mut range, 5, have.last_sequence);
        write_length_field(&mut out, BUFFERED_RANGES, &range);
    }

    for field in &fields {
        if matches!(
            field.number,
            CLIENT_STATE | SELECTED_FORMATS | BUFFERED_RANGES | AUDIO_CHOICES | VIDEO_CHOICES
        ) {
            continue;
        }
        out.extend_from_slice(field.raw);
    }

    // The menu, holding exactly what is wanted. The server picks from this, so
    // one entry each is what makes the choice stick — otherwise it switches
    // quality mid-download to suit a connection it is measuring, which is right
    // for playback and wrong for a file.
    write_length_field(&mut out, AUDIO_CHOICES, &format_id(audio));
    if !audio_only {
        write_length_field(&mut out, VIDEO_CHOICES, &format_id(video));
    }
    out
}

/// ClientAbrState with the playback position moved and the viewport opened up.
fn playback_state(original: &[u8], player_time_ms: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(original.len() + 16);
    for field in split(original) {
        match field.number {
            PLAYER_TIME_MS => write_varint_field(&mut out, PLAYER_TIME_MS, player_time_ms),
            // A small viewport is a request for a small picture: the server
            // answered a 640x360 window with 360p however the formats were
            // listed. This says the screen is large enough for any of them.
            VIEWPORT_WIDTH => write_varint_field(&mut out, VIEWPORT_WIDTH, 3840),
            VIEWPORT_HEIGHT => write_varint_field(&mut out, VIEWPORT_HEIGHT, 2160),
            _ => out.extend_from_slice(field.raw),
        }
    }
    out
}

fn format_id(track: &Track) -> Vec<u8> {
    let mut out = Vec::with_capacity(24);
    write_varint_field(&mut out, 1, track.itag as u64);
    write_varint_field(&mut out, 2, track.last_modified);
    if !track.xtags.is_empty() {
        write_length_field(&mut out, 3, track.xtags.as_bytes());
    }
    out
}

// ---- reply ---------------------------------------------------------------

/// A run of bytes and where in the stream it belongs.
#[derive(Debug, PartialEq, Eq)]
pub struct Piece {
    pub at: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct Runs {
    pub pieces: Vec<Piece>,
    pub duration_ms: u64,
    pub last_sequence: u64,
}

/// Take the media belonging to `itag` out of a reply.
///
/// The reply interleaves both tracks, and each run of media bytes is introduced
/// by a header naming the format it belongs to and where in the stream it
/// starts. Anything headed for the other track is skipped, so the audio
/// arriving alongside a video download is simply dropped.
pub fn runs(reply: &[u8], itag: u32) -> Runs {
    let mut found = Runs::default();
    // Headers are addressed by id and the media parts refer back to them, so
    // which track a run belongs to — and where it starts — is only knowable by
    // remembering the headers seen so far.
    let mut itag_of: Vec<(u64, u32)> = Vec::new();
    let mut start_of: Vec<(u64, u64)> = Vec::new();
    let mut filled: Vec<(u64, u64)> = Vec::new();
    let mut at = 0usize;

    while at < reply.len() {
        let Some((kind, next)) = ump_varint(reply, at) else { break };
        let Some((size, next)) = ump_varint(reply, next) else { break };
        let start = next;
        let Some(end) = start.checked_add(size as usize).filter(|e| *e <= reply.len()) else {
            break;
        };

        match kind {
            PART_MEDIA_HEADER => {
                let (mut id, mut header_itag, mut duration, mut start_byte, mut sequence) =
                    (0u64, 0u32, 0u64, 0u64, 0u64);
                for field in split(&reply[start..end]) {
                    match field.number {
                        HEADER_ID => id = field.as_u64(),
                        HEADER_ITAG => header_itag = field.as_u64() as u32,
                        HEADER_DURATION_MS => duration = field.as_u64(),
                        HEADER_START_BYTE => start_byte = field.as_u64(),
                        HEADER_SEQUENCE => sequence = field.as_u64(),
                        _ => {}
                    }
                }
                set(&mut itag_of, id, header_itag);
                set(&mut start_of, id, start_byte);
                set(&mut filled, id, 0);
                if header_itag == itag {
                    found.duration_ms += duration;
                    found.last_sequence = found.last_sequence.max(sequence);
                }
            }
            PART_MEDIA => {
                // A media part opens with the id of the header describing it,
                // and a run can be split across several parts, so each one
                // continues where the previous for that header stopped.
                let Some((id, body)) = ump_varint(reply, start) else {
                    at = end;
                    continue;
                };
                if get(&itag_of, id) == Some(itag) {
                    let already = get(&filled, id).unwrap_or(0);
                    let bytes = reply[body..end].to_vec();
                    let grown = already + bytes.len() as u64;
                    found.pieces.push(Piece { at: get(&start_of, id).unwrap_or(0) + already, bytes });
                    set(&mut filled, id, grown);
                }
            }
            _ => {}
        }
        at = end;
    }
    found
}

fn set<T: Copy>(list: &mut Vec<(u64, T)>, key: u64, value: T) {
    match list.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = value,
        None => list.push((key, value)),
    }
}

fn get<T: Copy>(list: &[(u64, T)], key: u64) -> Option<T> {
    list.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

// ---- wire formats --------------------------------------------------------

struct Field<'a> {
    number: u32,
    /// The field exactly as it came, so it can be written back untouched.
    raw: &'a [u8],
    /// For a length-delimited field, the payload past its length.
    value: &'a [u8],
}

impl Field<'_> {
    fn as_u64(&self) -> u64 {
        varint(self.value, 0).map(|(v, _)| v).unwrap_or(0)
    }
}

/// Break a protobuf message into its fields, keeping each one's original bytes.
fn split(buffer: &[u8]) -> Vec<Field<'_>> {
    let mut fields = Vec::new();
    let mut at = 0usize;
    while at < buffer.len() {
        let start = at;
        let Some((key, next)) = varint(buffer, at) else { break };
        at = next;
        let number = (key >> 3) as u32;
        let wire = key & 7;
        if number == 0 {
            break;
        }
        let value_start = at;
        at = match wire {
            0 => match varint(buffer, at) {
                Some((_, next)) => next,
                None => break,
            },
            1 => at + 8,
            5 => at + 4,
            2 => match varint(buffer, at) {
                Some((len, next)) => match next.checked_add(len as usize) {
                    Some(end) => end,
                    None => break,
                },
                None => break,
            },
            _ => break,
        };
        if at > buffer.len() {
            break;
        }
        let value = if wire == 2 {
            match varint(buffer, value_start) {
                Some((_, body)) => &buffer[body..at],
                None => break,
            }
        } else {
            &buffer[value_start..at]
        };
        fields.push(Field { number, raw: &buffer[start..at], value });
    }
    fields
}

fn varint(buffer: &[u8], from: usize) -> Option<(u64, usize)> {
    let mut result = 0u64;
    let mut shift = 0u32;
    let mut at = from;
    while at < buffer.len() {
        let byte = buffer[at];
        at += 1;
        result |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some((result, at));
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
    None
}

/// The reply uses a different variable-length integer from the request: the top
/// bits of the first byte say how many bytes follow, rather than each byte
/// carrying a continuation bit.
fn ump_varint(buffer: &[u8], from: usize) -> Option<(u64, usize)> {
    let first = *buffer.get(from)? as u64;
    let at = |offset: usize| buffer.get(from + offset).copied().unwrap_or(0) as u64;
    let (value, len) = match first {
        0x00..=0x7f => (first, 1),
        0x80..=0xbf => ((first & 0x3f) | (at(1) << 6), 2),
        0xc0..=0xdf => ((first & 0x1f) | (at(1) << 5) | (at(2) << 13), 3),
        0xe0..=0xef => ((first & 0x0f) | (at(1) << 4) | (at(2) << 12) | (at(3) << 20), 4),
        _ => (at(1) | (at(2) << 8) | (at(3) << 16) | (at(4) << 24), 5),
    };
    (from + len <= buffer.len()).then_some((value, from + len))
}

fn write_varint(out: &mut Vec<u8>, value: u64) {
    let mut remaining = value;
    loop {
        let mut byte = (remaining & 0x7f) as u8;
        remaining >>= 7;
        if remaining != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if remaining == 0 {
            break;
        }
    }
}

fn write_varint_field(out: &mut Vec<u8>, number: u32, value: u64) {
    write_varint(out, (number as u64) << 3);
    write_varint(out, value);
}

fn write_length_field(out: &mut Vec<u8>, number: u32, payload: &[u8]) {
    write_varint(out, ((number as u64) << 3) | 2);
    write_varint(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(itag: u32) -> Track {
        Track { itag, last_modified: 1_719_205_674_939_623, xtags: String::new(), bytes: 0 }
    }

    /// A template with a client state and a field this code has no name for.
    fn template() -> Vec<u8> {
        let mut state = Vec::new();
        write_varint_field(&mut state, VIEWPORT_WIDTH, 640);
        write_varint_field(&mut state, VIEWPORT_HEIGHT, 360);
        write_varint_field(&mut state, PLAYER_TIME_MS, 336_067);
        write_varint_field(&mut state, 57, 10);

        let mut out = Vec::new();
        write_length_field(&mut out, CLIENT_STATE, &state);
        write_length_field(&mut out, SELECTED_FORMATS, &format_id(&track(251)));
        write_length_field(&mut out, 5, b"opaque server configuration");
        write_length_field(&mut out, 19, b"opaque bot-check token");
        write_length_field(&mut out, VIDEO_CHOICES, &format_id(&track(315)));
        out
    }

    fn state_of(request: &[u8]) -> Vec<(u32, u64)> {
        let fields = split(request);
        let state = fields.iter().find(|f| f.number == CLIENT_STATE).expect("client state");
        split(state.value).iter().map(|f| (f.number, f.as_u64())).collect()
    }

    #[test]
    fn keeps_fields_it_does_not_understand() {
        let request = build(&template(), &track(401), &track(251), &track(315), 0, &[], false);
        let carried: Vec<_> = split(&request)
            .iter()
            .filter(|f| f.number == 5 || f.number == 19)
            .map(|f| f.value.to_vec())
            .collect();
        assert_eq!(
            carried,
            vec![b"opaque server configuration".to_vec(), b"opaque bot-check token".to_vec()]
        );
    }

    #[test]
    fn asks_for_the_wanted_formats_and_names_another_as_playing() {
        let (video, audio, decoy) = (track(401), track(251), track(315));
        let request = build(&template(), &video, &audio, &decoy, 0, &[], false);
        let fields = split(&request);

        let menu = |number: u32| {
            fields
                .iter()
                .filter(|f| f.number == number)
                .map(|f| split(f.value)[0].as_u64())
                .collect::<Vec<_>>()
        };
        assert_eq!(menu(VIDEO_CHOICES), vec![401]);
        assert_eq!(menu(AUDIO_CHOICES), vec![251]);
        // The decoy, and only the decoy: naming the target here costs the
        // stream's opening bytes.
        assert_eq!(menu(SELECTED_FORMATS), vec![315]);
    }

    #[test]
    fn moves_the_position_and_opens_the_viewport() {
        let request = build(&template(), &track(401), &track(251), &track(315), 20_133, &[], false);
        let state = state_of(&request);
        assert!(state.contains(&(PLAYER_TIME_MS, 20_133)));
        assert!(state.contains(&(VIEWPORT_WIDTH, 3840)));
        assert!(state.contains(&(VIEWPORT_HEIGHT, 2160)));
        // Untouched state survives.
        assert!(state.contains(&(57, 10)));
    }

    #[test]
    fn reports_what_is_already_held() {
        let video = track(401);
        let have = Held { duration_ms: 20_133, last_sequence: 4 };
        let request = build(&template(), &video, &track(251), &track(315), 0, &[(&video, &have)], false);
        let fields = split(&request);
        let range = fields.iter().find(|f| f.number == BUFFERED_RANGES).expect("range");
        let parts: Vec<_> = split(range.value).iter().map(|f| (f.number, f.as_u64())).collect();
        assert!(parts.contains(&(3, 20_133)));
        assert!(parts.contains(&(5, 4)));
    }

    #[test]
    fn leaves_out_ranges_for_a_stream_with_nothing_in_it() {
        let video = track(401);
        let nothing = Held::default();
        let request =
            build(&template(), &video, &track(251), &track(315), 0, &[(&video, &nothing)], false);
        assert!(split(&request).iter().all(|f| f.number != BUFFERED_RANGES));
    }

    // ---- replies ----------------------------------------------------------

    fn ump_part(out: &mut Vec<u8>, kind: u64, payload: &[u8]) {
        // Two-byte form covers everything these tests build.
        let write = |out: &mut Vec<u8>, value: u64| {
            if value < 0x80 {
                out.push(value as u8);
            } else {
                out.push(0x80 | (value & 0x3f) as u8);
                out.push((value >> 6) as u8);
            }
        };
        write(out, kind);
        write(out, payload.len() as u64);
        out.extend_from_slice(payload);
    }

    fn header(id: u64, itag: u32, start: u64, sequence: u64, duration: u64) -> Vec<u8> {
        let mut out = Vec::new();
        write_varint_field(&mut out, HEADER_ID, id);
        write_varint_field(&mut out, HEADER_ITAG, itag as u64);
        write_varint_field(&mut out, HEADER_START_BYTE, start);
        write_varint_field(&mut out, HEADER_SEQUENCE, sequence);
        write_varint_field(&mut out, HEADER_DURATION_MS, duration);
        out
    }

    fn media(id: u64, bytes: &[u8]) -> Vec<u8> {
        let mut out = vec![id as u8];
        out.extend_from_slice(bytes);
        out
    }

    #[test]
    fn keeps_only_the_wanted_track() {
        let mut reply = Vec::new();
        ump_part(&mut reply, PART_MEDIA_HEADER, &header(0, 401, 0, 1, 4_000));
        ump_part(&mut reply, PART_MEDIA, &media(0, b"video-bytes"));
        ump_part(&mut reply, PART_MEDIA_HEADER, &header(1, 251, 0, 1, 10_000));
        ump_part(&mut reply, PART_MEDIA, &media(1, b"audio-bytes"));

        let video = runs(&reply, 401);
        assert_eq!(video.pieces, vec![Piece { at: 0, bytes: b"video-bytes".to_vec() }]);
        assert_eq!(video.duration_ms, 4_000);

        let audio = runs(&reply, 251);
        assert_eq!(audio.pieces, vec![Piece { at: 0, bytes: b"audio-bytes".to_vec() }]);
        assert_eq!(audio.duration_ms, 10_000);
    }

    #[test]
    fn continues_a_run_split_across_parts() {
        let mut reply = Vec::new();
        ump_part(&mut reply, PART_MEDIA_HEADER, &header(0, 401, 1_000, 2, 4_000));
        ump_part(&mut reply, PART_MEDIA, &media(0, b"abc"));
        ump_part(&mut reply, PART_MEDIA, &media(0, b"de"));

        let got = runs(&reply, 401);
        assert_eq!(
            got.pieces,
            vec![
                Piece { at: 1_000, bytes: b"abc".to_vec() },
                Piece { at: 1_003, bytes: b"de".to_vec() },
            ]
        );
        assert_eq!(got.last_sequence, 2);
    }

    #[test]
    fn sums_only_the_wanted_track_s_time() {
        let mut reply = Vec::new();
        ump_part(&mut reply, PART_MEDIA_HEADER, &header(0, 401, 0, 1, 4_000));
        ump_part(&mut reply, PART_MEDIA, &media(0, b"x"));
        ump_part(&mut reply, PART_MEDIA_HEADER, &header(1, 401, 100, 2, 4_500));
        ump_part(&mut reply, PART_MEDIA, &media(1, b"y"));
        ump_part(&mut reply, PART_MEDIA_HEADER, &header(2, 251, 0, 9, 10_000));
        assert_eq!(runs(&reply, 401).duration_ms, 8_500);
    }

    #[test]
    fn survives_a_truncated_reply() {
        let mut reply = Vec::new();
        ump_part(&mut reply, PART_MEDIA_HEADER, &header(0, 401, 0, 1, 4_000));
        ump_part(&mut reply, PART_MEDIA, &media(0, b"kept"));
        let full = reply.len();
        ump_part(&mut reply, PART_MEDIA, &media(0, b"cut short"));
        reply.truncate(full + 4);

        let got = runs(&reply, 401);
        assert_eq!(got.pieces, vec![Piece { at: 0, bytes: b"kept".to_vec() }]);
    }

    #[test]
    fn a_reply_with_nothing_for_us_is_empty_not_wrong() {
        let mut reply = Vec::new();
        ump_part(&mut reply, PART_MEDIA_HEADER, &header(0, 251, 0, 1, 10_000));
        ump_part(&mut reply, PART_MEDIA, &media(0, b"audio"));
        let got = runs(&reply, 401);
        assert!(got.pieces.is_empty());
        assert_eq!(got.duration_ms, 0);
    }
}
