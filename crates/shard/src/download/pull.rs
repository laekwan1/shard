//! Asking for a whole stream, one reply at a time.
//!
//! A reply covers a few seconds, so a film is a conversation. Two things about
//! that conversation are easy to get wrong and expensive to get wrong, and both
//! were got wrong first:
//!
//! - **How far to jump.** Video is cut into roughly five-second pieces and
//!   audio into ten, so a reply covers more of one than the other. Advancing by
//!   the longer skips the tail of the shorter, and nothing ever asks for that
//!   moment again — the file ends up missing a slice in the middle, which no
//!   player will open. Advance by the shorter.
//! - **When to stop.** The server answers a position, and near the end it
//!   answers with what it already sent. A reply that repeats bytes is not empty
//!   and not progress; treating "not empty" as progress produced four and a
//!   half thousand requests for a video that never finished.
//!
//! The transport is a closure so that all of the above is testable without a
//! network, which is the point: this is the part with the arithmetic in it.

use crate::download::sabr::{self, Held, Template, Track};
use anyhow::{bail, Result};

/// Somewhere bytes go, addressed by position rather than appended.
///
/// Positions matter because a reply's runs can overlap what is already held or
/// sit past a gap; appending them in arrival order produces a file with the
/// right size and the wrong contents.
pub trait Sink {
    fn write_at(&mut self, at: u64, bytes: &[u8]) -> std::io::Result<()>;
}

/// How much of each stream is complete from the beginning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub video: u64,
    pub audio: u64,
}

/// Replies that add nothing before the download is abandoned.
const STALLS_BEFORE_GIVING_UP: u32 = 3;

/// When a reply does not say what it covered.
const STEP_WHEN_UNSAID: u64 = 5_000;

/// A hard ceiling on requests for one download.
///
/// Not a limit anything real should reach — a four-hour film at ten seconds a
/// reply is under fifteen hundred — but a runaway loop here means thousands of
/// requests a minute at YouTube, which is worth making impossible rather than
/// unlikely.
const MAX_REQUESTS: u32 = 4_000;

/// Pull `video` and `audio` to their sinks, in one conversation.
///
/// One conversation rather than two because a reply carries both streams
/// interleaved: asking separately would download each of them twice.
///
/// `audio_only` asks for the sound alone. Video still arrives — at its
/// smallest — so it is written but never waited for.
pub fn pull(
    template: &Template,
    video: &Track,
    audio: &Track,
    decoy: &Track,
    video_sink: &mut dyn Sink,
    audio_sink: &mut dyn Sink,
    post: &mut dyn FnMut(&str, &[u8]) -> Result<Vec<u8>>,
    on_progress: &mut dyn FnMut(Progress),
    cancelled: &dyn Fn() -> bool,
    audio_only: bool,
) -> Result<Progress> {
    let mut done = Progress::default();
    let mut player_time_ms = 0u64;
    let mut stalled = 0u32;
    let mut requests = 0u32;
    let (mut held_video, mut held_audio) = (Held::default(), Held::default());

    while !cancelled() && requests < MAX_REQUESTS {
        requests += 1;
        let request = sabr::build(
            &template.body,
            video,
            audio,
            decoy,
            player_time_ms,
            &[(video, &held_video), (audio, &held_audio)],
            audio_only,
        );
        let reply = post(&template.url, &request)?;

        let video_runs = sabr::runs(&reply, video.itag);
        let audio_runs = sabr::runs(&reply, audio.itag);
        let before = done;
        done.video = place(&video_runs, video_sink, done.video)?;
        done.audio = place(&audio_runs, audio_sink, done.audio)?;
        held_video.duration_ms += video_runs.duration_ms;
        held_video.last_sequence = held_video.last_sequence.max(video_runs.last_sequence);
        held_audio.duration_ms += audio_runs.duration_ms;
        held_audio.last_sequence = held_audio.last_sequence.max(audio_runs.last_sequence);

        // Either stream failing to grow is a stall, not just both — one
        // standing still is a gap that will not close, and letting the other
        // carry the loop hides it. A stream that has all its bytes is finished,
        // not stuck. And when the video was never asked for, whatever arrives
        // of it is incidental and must not decide anything.
        let video_matters = !audio_only;
        let stuck = (video_matters
            && done.video == before.video
            && !whole(done.video, video.bytes))
            || (done.audio == before.audio && !whole(done.audio, audio.bytes));
        if stuck {
            stalled += 1;
            if stalled >= STALLS_BEFORE_GIVING_UP {
                break;
            }
        } else {
            stalled = 0;
            on_progress(done);
        }

        let covered = [video_runs.duration_ms, audio_runs.duration_ms]
            .into_iter()
            .filter(|d| *d > 0)
            .min()
            .unwrap_or(0);
        player_time_ms += if covered > 0 { covered } else { STEP_WHEN_UNSAID };

        let video_done = audio_only || whole(done.video, video.bytes);
        if video_done && whole(done.audio, audio.bytes) {
            break;
        }
    }

    if requests >= MAX_REQUESTS {
        bail!("요청이 너무 많아 중단했습니다");
    }
    Ok(done)
}

/// How many audio segments to ask for at once.
///
/// One of the batch re-covers the boundary (see below) and is spent, so the useful
/// width is one less. Kept small: this is several concurrent requests at YouTube
/// for one download, and past a handful the round-trip is already well hidden.
const AUDIO_BATCH: u64 = 5;

/// Pull the audio alone, several segments per round-trip.
///
/// Audio segments are small in bytes — a thirty-second piece is about half a
/// megabyte — but each request pays the same server round-trip a multi-megabyte
/// video request does. Fetched one at a time the round-trip dominates and audio
/// crawls: measured against the same connection, one megabyte a second where video
/// managed seven. Several requests in flight at once, each at a different point in
/// the stream, overlap those round-trips.
///
/// Two things keep it correct where the naive version corrupts:
///
/// - **The buffer each request reports is derived from the contiguous bytes held,
///   not accumulated across replies.** Replies arrive out of order and can overlap
///   or fall short of an estimated segment size; a running total would drift and
///   the drift would never close. `done.audio / seg_bytes` is the truth every
///   round, so a bad estimate self-corrects on the next one.
/// - **The batch is biased back one segment.** An overlap is free — [`place_pieces`]
///   writes by position and the repeat is identical — but a gap is fatal: a segment
///   skipped past the contiguous end is never asked for again and the file ends up
///   holed. Starting one segment behind the boundary guarantees the next needed
///   segment is always somewhere in the batch, at the cost of one repeated request.
///
/// If it still falls short — the server refusing the concurrency, an estimate off
/// by more than a segment — the caller retries with the proven one-at-a-time
/// [`pull`]; nothing here has to be perfect, only fast when it works.
pub fn pull_audio(
    template: &Template,
    audio: &Track,
    decoy: &Track,
    audio_sink: &mut dyn Sink,
    post: &(dyn Fn(&str, &[u8]) -> Result<Vec<u8>> + Sync),
    on_progress: &mut dyn FnMut(Progress),
    cancelled: &dyn Fn() -> bool,
) -> Result<Progress> {
    let mut done = Progress::default();
    let mut stalled = 0u32;
    let mut requests = 0u32;
    let mut seg_dur = 0u64;
    let mut seg_bytes = 0u64;

    while !cancelled() && requests < MAX_REQUESTS {
        // The first request goes alone: the segment size that spaces a batch is
        // not known until one reply is in.
        let batch = if seg_dur > 0 && seg_bytes > 0 { AUDIO_BATCH } else { 1 };
        requests += batch as u32;

        // One segment behind the contiguous boundary, then forward: the reported
        // buffer says "I hold this many segments", and the server answers with the
        // one after.
        let base = (done.audio / seg_bytes.max(1)).saturating_sub(1);
        let bodies: Vec<Vec<u8>> = (0..batch)
            .map(|i| {
                let seq = base + i;
                let dur = seq * seg_dur;
                let held = Held { duration_ms: dur, last_sequence: seq };
                sabr::build(&template.body, decoy, audio, decoy, dur, &[(audio, &held)], true)
            })
            .collect();

        // The whole batch in flight at once. A single request failing is not fatal
        // while another made progress — the missing stretch is asked for again next
        // round from the contiguous end — so an error is surfaced only when every
        // request failed and nothing at all came back.
        let replies: Vec<Result<Vec<u8>>> = std::thread::scope(|s| {
            let handles: Vec<_> =
                bodies.iter().map(|body| s.spawn(|| post(&template.url, body))).collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| bail!("다운로드 스레드가 죽었습니다")))
                .collect()
        });

        let mut pieces: Vec<sabr::Piece> = Vec::new();
        let mut any_ok = false;
        let mut first_error: Option<anyhow::Error> = None;
        for reply in replies {
            match reply {
                Ok(bytes) => {
                    any_ok = true;
                    let mut runs = sabr::runs(&bytes, audio.itag);
                    if seg_dur == 0 && runs.duration_ms > 0 {
                        seg_dur = runs.duration_ms;
                        seg_bytes = runs.pieces.iter().map(|p| p.bytes.len() as u64).sum();
                    }
                    pieces.append(&mut runs.pieces);
                }
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        if !any_ok {
            if let Some(e) = first_error {
                return Err(e);
            }
        }

        let before = done.audio;
        done.audio = place_pieces(&pieces, audio_sink, done.audio)?;

        if done.audio == before {
            stalled += 1;
            if stalled >= STALLS_BEFORE_GIVING_UP {
                break;
            }
        } else {
            stalled = 0;
            on_progress(done);
        }

        if whole(done.audio, audio.bytes) {
            break;
        }
    }

    if requests >= MAX_REQUESTS {
        bail!("요청이 너무 많아 중단했습니다");
    }
    Ok(done)
}

/// Write a reply's runs and report how much is complete from the beginning.
fn place(runs: &sabr::Runs, sink: &mut dyn Sink, from: u64) -> Result<u64> {
    place_pieces(&runs.pieces, sink, from)
}

/// Write runs from one or several replies at their byte positions and report how
/// much is complete from the beginning.
///
/// Taking a flat slice — not one reply — is what makes the parallel audio path
/// correct: pieces from several replies that arrived in any order are sorted by
/// position here together, so the contiguous count is the true one. Placing each
/// reply on its own instead would miss a run that only becomes contiguous once an
/// earlier reply's run is in.
fn place_pieces(pieces: &[sabr::Piece], sink: &mut dyn Sink, from: u64) -> Result<u64> {
    let mut contiguous = from;
    let mut ordered: Vec<_> = pieces.iter().collect();
    ordered.sort_by_key(|piece| piece.at);
    for piece in ordered {
        sink.write_at(piece.at, &piece.bytes)?;
        if piece.at <= contiguous {
            contiguous = contiguous.max(piece.at + piece.bytes.len() as u64);
        }
    }
    Ok(contiguous)
}

/// Whether a stream has all of its bytes, when its size is known at all.
fn whole(got: u64, expected: u64) -> bool {
    expected > 0 && got >= expected
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A file in memory, written by position exactly as the real one is.
    #[derive(Default)]
    struct Buffer(Vec<u8>);

    impl Sink for Buffer {
        fn write_at(&mut self, at: u64, bytes: &[u8]) -> std::io::Result<()> {
            let end = at as usize + bytes.len();
            if self.0.len() < end {
                self.0.resize(end, 0);
            }
            self.0[at as usize..end].copy_from_slice(bytes);
            Ok(())
        }
    }

    fn track(itag: u32, bytes: u64) -> Track {
        Track { itag, last_modified: 1, xtags: String::new(), bytes }
    }

    fn template() -> Template {
        Template { url: "https://example.invalid/videoplayback".into(), body: Vec::new() }
    }

    /// A reply carrying one run for each track, at the given positions.
    fn reply(parts: &[(u32, u64, u64, u64, &[u8])]) -> Vec<u8> {
        fn ump(out: &mut Vec<u8>, kind: u64, payload: &[u8]) {
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
        fn varint_field(out: &mut Vec<u8>, number: u32, value: u64) {
            let mut key = (number as u64) << 3;
            loop {
                let mut byte = (key & 0x7f) as u8;
                key >>= 7;
                if key != 0 {
                    byte |= 0x80;
                }
                out.push(byte);
                if key == 0 {
                    break;
                }
            }
            let mut v = value;
            loop {
                let mut byte = (v & 0x7f) as u8;
                v >>= 7;
                if v != 0 {
                    byte |= 0x80;
                }
                out.push(byte);
                if v == 0 {
                    break;
                }
            }
        }

        let mut out = Vec::new();
        for (id, (itag, start, sequence, duration, bytes)) in parts.iter().enumerate() {
            let mut header = Vec::new();
            varint_field(&mut header, 1, id as u64);
            varint_field(&mut header, 3, *itag as u64);
            varint_field(&mut header, 6, *start);
            varint_field(&mut header, 9, *sequence);
            varint_field(&mut header, 12, *duration);
            ump(&mut out, 20, &header);

            let mut media = vec![id as u8];
            media.extend_from_slice(bytes);
            ump(&mut out, 21, &media);
        }
        out
    }

    /// Drives `pull` over a scripted list of replies, recording the positions
    /// asked for so the advance rule can be checked.
    fn run(
        replies: Vec<Vec<u8>>,
        video: Track,
        audio: Track,
    ) -> (Result<Progress>, Buffer, Buffer, usize) {
        let calls = Rc::new(RefCell::new(0usize));
        let seen = calls.clone();
        let mut post = move |_: &str, _: &[u8]| -> Result<Vec<u8>> {
            let mut n = seen.borrow_mut();
            let out = replies.get(*n).cloned().unwrap_or_default();
            *n += 1;
            Ok(out)
        };
        let (mut v, mut a) = (Buffer::default(), Buffer::default());
        let outcome = pull(
            &template(),
            &video,
            &audio,
            &track(999, 0),
            &mut v,
            &mut a,
            &mut post,
            &mut |_| {},
            &|| false,
            false,
        );
        let count = *calls.borrow();
        (outcome, v, a, count)
    }

    #[test]
    fn finishes_when_both_streams_are_complete() {
        let replies = vec![
            reply(&[(401, 0, 1, 5_000, b"vvvv"), (251, 0, 1, 10_000, b"aaaaaaaa")]),
            reply(&[(401, 4, 2, 5_000, b"vvvv")]),
        ];
        let (done, video, audio, calls) = run(replies, track(401, 8), track(251, 8));
        assert_eq!(done.unwrap(), Progress { video: 8, audio: 8 });
        assert_eq!(video.0, b"vvvvvvvv");
        assert_eq!(audio.0, b"aaaaaaaa");
        assert_eq!(calls, 2);
    }

    /// The bug this file exists to prevent: advancing by the longer of the two
    /// leaves a hole in the shorter, and the hole is never revisited.
    #[test]
    fn a_stream_that_stops_growing_ends_the_download() {
        // Audio keeps arriving; video never advances past its first run.
        let stuck = reply(&[(401, 0, 1, 5_000, b"vvvv"), (251, 0, 1, 10_000, b"aaaa")]);
        let replies = vec![stuck.clone(), stuck.clone(), stuck.clone(), stuck];
        let (done, video, _, calls) = run(replies, track(401, 1_000), track(251, 1_000));
        // One reply that helped, then three that did not. It gives up rather
        // than asking forever.
        assert_eq!(calls, 4);
        assert_eq!(done.unwrap().video, 4);
        assert_eq!(video.0, b"vvvv");
    }

    #[test]
    fn a_run_past_a_gap_is_written_but_not_counted() {
        let replies = vec![
            reply(&[(401, 0, 1, 5_000, b"aa"), (251, 0, 1, 5_000, b"aa")]),
            // Video jumps ahead, leaving bytes 2..10 unfilled.
            reply(&[(401, 10, 3, 5_000, b"zz"), (251, 2, 2, 5_000, b"bb")]),
        ];
        let (done, video, _, _) = run(replies, track(401, 12), track(251, 4));
        // Written where it belongs...
        assert_eq!(&video.0[10..12], b"zz");
        // ...but the stream is only complete to the gap.
        assert_eq!(done.unwrap().video, 2);
    }

    #[test]
    fn overlapping_runs_do_not_double_count() {
        let replies = vec![
            reply(&[(401, 0, 1, 5_000, b"abcd"), (251, 0, 1, 5_000, b"xy")]),
            // The server resends part of what we hold, then continues.
            reply(&[(401, 2, 2, 5_000, b"cdef"), (251, 2, 2, 5_000, b"zw")]),
        ];
        let (done, video, _, _) = run(replies, track(401, 6), track(251, 4));
        assert_eq!(video.0, b"abcdef");
        assert_eq!(done.unwrap().video, 6);
    }

    #[test]
    fn an_empty_reply_is_a_stall_not_an_error() {
        let (done, _, _, calls) = run(vec![], track(401, 100), track(251, 100));
        assert_eq!(calls, 3);
        assert_eq!(done.unwrap(), Progress::default());
    }

    #[test]
    fn a_failing_request_stops_rather_than_retrying_blindly() {
        let mut post =
            |_: &str, _: &[u8]| -> Result<Vec<u8>> { bail!("서버가 403 로 응답했습니다") };
        let (mut v, mut a) = (Buffer::default(), Buffer::default());
        let outcome = pull(
            &template(),
            &track(401, 10),
            &track(251, 10),
            &track(999, 0),
            &mut v,
            &mut a,
            &mut post,
            &mut |_| {},
            &|| false,
            false,
        );
        assert!(outcome.is_err());
    }

    /// The buffered-range sequence a request reports (field 3 → field 5), or 0
    /// when it reports nothing held. This is how a fake server knows which segment
    /// the batched audio path is asking each of its concurrent requests for.
    fn reported_sequence(body: &[u8]) -> u64 {
        fn varint(b: &[u8], i: &mut usize) -> u64 {
            let (mut v, mut shift) = (0u64, 0u32);
            while *i < b.len() {
                let byte = b[*i];
                *i += 1;
                v |= ((byte & 0x7f) as u64) << shift;
                if byte & 0x80 == 0 {
                    break;
                }
                shift += 7;
            }
            v
        }
        // Field 5 (a varint) inside a length-delimited sub-message.
        fn inner_seq(b: &[u8]) -> u64 {
            let (mut i, mut seq) = (0usize, 0u64);
            while i < b.len() {
                let key = varint(b, &mut i);
                match key & 7 {
                    0 => {
                        let v = varint(b, &mut i);
                        if key >> 3 == 5 {
                            seq = v;
                        }
                    }
                    2 => {
                        let len = varint(b, &mut i) as usize;
                        i = (i + len).min(b.len());
                    }
                    _ => break,
                }
            }
            seq
        }
        let (mut i, mut best) = (0usize, 0u64);
        while i < body.len() {
            let key = varint(body, &mut i);
            match key & 7 {
                0 => {
                    let _ = varint(body, &mut i);
                }
                2 => {
                    let len = varint(body, &mut i) as usize;
                    let end = (i + len).min(body.len());
                    if key >> 3 == 3 {
                        best = best.max(inner_seq(&body[i..end]));
                    }
                    i = end;
                }
                _ => break,
            }
        }
        best
    }

    /// A server that hands back the one segment after whatever a request says it
    /// holds — pure, so it is safe to call from the batched path's threads at once.
    fn serving_post(
        itag: u32,
        total: u64,
        seg_bytes: usize,
        seg_dur: u64,
    ) -> impl Fn(&str, &[u8]) -> Result<Vec<u8>> + Sync {
        move |_url: &str, body: &[u8]| {
            let want = reported_sequence(body) + 1; // sequences are 1-based
            if want > total {
                return Ok(Vec::new()); // past the end
            }
            let start = (want - 1) * seg_bytes as u64;
            let byte = b'A' + ((want - 1) % 26) as u8;
            Ok(reply(&[(itag, start, want, seg_dur, &vec![byte; seg_bytes])]))
        }
    }

    #[test]
    fn parallel_audio_assembles_the_whole_stream_in_order() {
        let (itag, total, seg_bytes) = (251u32, 10u64, 7usize);
        let audio = track(itag, total * seg_bytes as u64);
        let post = serving_post(itag, total, seg_bytes, 1_000);
        let mut sink = Buffer::default();
        let done = pull_audio(
            &template(),
            &audio,
            &track(999, 0),
            &mut sink,
            &post,
            &mut |_| {},
            &|| false,
        )
        .unwrap();

        assert_eq!(done.audio, total * seg_bytes as u64);
        let mut expected = Vec::new();
        for s in 1..=total {
            expected.extend(std::iter::repeat(b'A' + ((s - 1) % 26) as u8).take(seg_bytes));
        }
        // Out-of-order arrival and the deliberately overlapping boundary request
        // must still leave every byte in its place.
        assert_eq!(sink.0, expected);
    }

    #[test]
    fn parallel_audio_surfaces_an_error_when_every_request_fails() {
        let post = |_: &str, _: &[u8]| -> Result<Vec<u8>> { bail!("서버가 403 로 응답했습니다") };
        let mut sink = Buffer::default();
        let outcome = pull_audio(
            &template(),
            &track(251, 100),
            &track(999, 0),
            &mut sink,
            &post,
            &mut |_| {},
            &|| false,
        );
        assert!(outcome.is_err());
    }

    #[test]
    fn cancelling_stops_the_conversation() {
        let mut post = |_: &str, _: &[u8]| -> Result<Vec<u8>> { unreachable!("never asked") };
        let (mut v, mut a) = (Buffer::default(), Buffer::default());
        let done = pull(
            &template(),
            &track(401, 10),
            &track(251, 10),
            &track(999, 0),
            &mut v,
            &mut a,
            &mut post,
            &mut |_| {},
            &|| true,
            false,
        );
        assert_eq!(done.unwrap(), Progress::default());
    }
}
