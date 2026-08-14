//! Print the tray marks as text, so the shapes the desktop draws can be
//! compared with anything that claims to reproduce them.
//!
//! `cargo test -p uikit --test marks -- --nocapture`

use uikit::icon;

fn show(name: &str, mark: icon::Rgba) {
    println!("\n{name}  ({}x{})", mark.width, mark.height);
    // The colour is in the pixels; report the first opaque one.
    let colour = mark
        .pixels
        .chunks(4)
        .find(|p| p[3] > 200)
        .map(|p| format!("#{:02X}{:02X}{:02X}", p[0], p[1], p[2]))
        .unwrap_or_else(|| "(empty)".into());
    println!("colour {colour}");

    // Downsample to something a terminal can show.
    let step = mark.width / 32;
    for y in 0..32 {
        let mut row = String::new();
        for x in 0..32 {
            let i = ((y * step * mark.width + x * step) * 4 + 3) as usize;
            row.push(match mark.pixels[i] {
                0..=40 => ' ',
                41..=140 => '.',
                141..=220 => '+',
                _ => '#',
            });
        }
        println!("{row}");
    }
}

/// The colour of a mark's opaque pixels.
fn colour_of(mark: &icon::Rgba) -> [u8; 3] {
    let p = mark.pixels.chunks(4).find(|p| p[3] > 200).expect("the mark is blank");
    [p[0], p[1], p[2]]
}

/// How much of the canvas the mark covers.
fn coverage(mark: &icon::Rgba) -> f32 {
    let opaque = mark.pixels.chunks(4).filter(|p| p[3] > 128).count();
    opaque as f32 / (mark.width * mark.height) as f32
}

#[test]
fn the_tray_marks_are_what_they_claim_to_be() {
    show("shard (running)", icon::shard(true));
    show("veil (running)", icon::veil(true));

    // The phone builds reproduce these as vector drawables, so the colours are
    // written down in two places. A change here without a change there would
    // otherwise ship two apps wearing different marks.
    assert_eq!(colour_of(&icon::shard(true)), icon::CYAN, "Shard's mark is cyan");
    assert_eq!(colour_of(&icon::veil(true)), icon::VIOLET, "Veil's mark is violet");
    assert_eq!(colour_of(&icon::shard(false)), icon::GREY, "a stopped mark is grey");

    // A mark that covered almost nothing, or almost everything, would mean the
    // distance field had been broken into something unrecognisable.
    for (name, mark) in [("shard", icon::shard(true)), ("veil", icon::veil(true))] {
        let filled = coverage(&mark);
        assert!((0.25..0.75).contains(&filled), "{name} covers {filled:.2} of the canvas");
    }
}
