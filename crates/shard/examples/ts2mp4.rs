//! Verification helper: demux a .ts and write it as .mp4 via the same path the
//! downloader uses. Not shipped — a way to check the MP4 writer against a real
//! transport stream with ffprobe. `cargo run --example ts2mp4 --features download -- in.ts out.mp4`.
use shard::download::{mp4mux, ts};
use std::io::BufWriter;

fn main() {
    let mut args = std::env::args().skip(1);
    let input = args.next().expect("usage: ts2mp4 <in.ts> <out.mp4>");
    let output = args.next().expect("usage: ts2mp4 <in.ts> <out.mp4>");
    let data = std::fs::read(&input).expect("read ts");
    let demuxed = ts::demux(&data).expect("demux ts");
    eprintln!(
        "video {} samples {}x{}, audio {} samples {}Hz {}ch",
        demuxed.video.len(),
        demuxed.width,
        demuxed.height,
        demuxed.audio.len(),
        demuxed.sample_rate,
        demuxed.channels
    );
    let mut file = BufWriter::new(std::fs::File::create(&output).expect("create mp4"));
    mp4mux::write(&demuxed, &mut file).expect("write mp4");
}
