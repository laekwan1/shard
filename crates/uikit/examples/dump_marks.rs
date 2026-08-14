//! Write the marks as raw RGBA so they can be looked at.
//!
//! The icons are generated from distance fields rather than drawn, so the only
//! way to know what one looks like is to render it.

fn main() -> std::io::Result<()> {
    for (name, art) in [
        ("shard-on", uikit::icon::shard_at(128, true)),
        ("shard-off", uikit::icon::shard_at(128, false)),
        ("veil-on", uikit::icon::veil_at(128, true)),
    ] {
        std::fs::write(format!("{name}.rgba"), &art.pixels)?;
        println!("{name}: {}x{}", art.width, art.height);
    }
    Ok(())
}
