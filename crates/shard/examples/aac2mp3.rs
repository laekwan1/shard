//! Verify the AAC→MP3 path end to end: demux an .m4a to raw AAC frames + ASC the
//! same way the save path does, decode+encode to MP3, write it out.
//!
//! Run: cargo run -p shard --example aac2mp3 -- in.m4a out.mp3

use shard::download::{mp3, mp4};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let input = args.get(1).expect("usage: aac2mp3 <in.m4a> <out.mp3>");
    let output = args.get(2).expect("usage: aac2mp3 <in.m4a> <out.mp3>");

    let bytes = std::fs::read(input)?;
    let stream = mp4::stream(&bytes)?;
    let mut frames = Vec::new();
    for s in mp4::samples(&bytes, 0) {
        let at = s.at as usize;
        let end = at + s.len;
        frames.push(bytes[at..end].to_vec());
    }
    eprintln!(
        "demuxed {} AAC frames, sr={}, ch={}, asc={} bytes",
        frames.len(),
        stream.sample_rate,
        stream.channels,
        stream.codec_private.len()
    );

    let mut data = mp3::from_aac(
        &stream.codec_private,
        stream.sample_rate as u32,
        stream.channels,
        &frames,
    )?;
    // Optional third arg: a JPEG to embed as an ID3 cover, to check that path too.
    if let Some(cover) = args.get(3) {
        let pic = std::fs::read(cover)?;
        data = mp3::with_cover_id3(&data, &pic, "jpg");
        eprintln!("embedded {}-byte cover", pic.len());
    }
    std::fs::write(output, &data)?;
    eprintln!("wrote {} ({} bytes)", output, data.len());
    Ok(())
}
