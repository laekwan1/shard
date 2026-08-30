//! Verify a cover can be embedded in the music .m4a our writer produces — the
//! path that had no thumbnail. Demux an AAC .m4a to samples, re-mux with mp4mux
//! (non-fragmented, moov after mdat, like save_audio_only), then with_cover, and
//! check the result actually carries the cover.
//!
//! Run: cargo run -p shard --example covertest -- in_frag.m4a cover.jpg

use shard::download::{mp4, mp4mux, ts};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let input = args.get(1).expect("usage: covertest <in_frag.m4a> <cover.jpg>");
    let cover = args.get(2).expect("usage: covertest <in_frag.m4a> <cover.jpg>");

    let bytes = std::fs::read(input)?;
    let stream = mp4::stream(&bytes)?;
    let mut audio = Vec::new();
    for s in mp4::samples(&bytes, 0) {
        let at = s.at as usize;
        audio.push(ts::Sample {
            data: bytes[at..at + s.len].to_vec(),
            time_ms: s.show_ms(stream.timescale),
            decode_ms: s.time_ms(stream.timescale),
            keyframe: s.keyframe,
        });
    }
    let demuxed = ts::Demuxed {
        avcc: Vec::new(),
        video_av1: false,
        video_timescale: 1000,
        width: 0,
        height: 0,
        video: Vec::new(),
        asc: stream.codec_private.clone(),
        sample_rate: stream.sample_rate as u32,
        channels: stream.channels,
        audio,
    };
    let mut m4a = Vec::new();
    mp4mux::write(&demuxed, &mut m4a)?;
    eprintln!("music .m4a: {} bytes", m4a.len());

    // Where is moov relative to mdat? (with_cover needs moov after mdat.)
    let moov = find(&m4a, b"moov");
    let mdat = find(&m4a, b"mdat");
    eprintln!("moov@{:?} mdat@{:?} (moov must be > mdat)", moov, mdat);
    eprintln!("has_cover before: {}", mp4::has_cover(&m4a));

    let pic = std::fs::read(cover)?;
    let withc = mp4::with_cover(&m4a, &pic, "jpg").ok_or_else(|| anyhow::anyhow!("with_cover returned None"))?;
    eprintln!("with_cover ok: {} bytes", withc.len());
    eprintln!("has_cover after (whole): {}", mp4::has_cover(&withc));

    // carries_cover reads only the tail — check the cover is findable there.
    let tail = &withc[withc.len().saturating_sub(512 * 1024)..];
    eprintln!("has_cover in tail 512KB: {}", mp4::has_cover(tail));

    // And the extracted picture round-trips.
    match mp4::cover(&withc) {
        Some((p, k)) => eprintln!("extracted cover (mp4): {} bytes, kind={}", p.len(), k),
        None => eprintln!("extract FAILED"),
    }

    // MP3 path: an ID3 APIC cover, read back by id3_cover.
    let pic = std::fs::read(cover)?;
    let mp3_with = shard::download::mp3::with_cover_id3(b"\xff\xfbfakeaudio", &pic, "jpg");
    eprintln!("mp3 head starts ID3: {}", mp3_with.starts_with(b"ID3"));
    match shard::download::mp3::id3_cover(&mp3_with) {
        Some((p, k)) => eprintln!("extracted cover (id3): {} bytes, kind={}", p.len(), k),
        None => eprintln!("id3 extract FAILED"),
    }
    Ok(())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
