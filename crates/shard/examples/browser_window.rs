//! The browser window on its own, so it can be driven and looked at.
//!
//! The program it belongs to needs the packet driver and therefore needs
//! administrator rights, which puts it out of reach of anything that drives a
//! desktop. This opens the same window with the same code and nothing else, so
//! the frame, the tabs and the address can be exercised for real.
//!
//!     cargo run --example browser_window

fn main() -> anyhow::Result<()> {
    let script = format!(
        "{}\n{}",
        shard::download::youtube::RECORDER,
        shard::download::youtube::CONTROL
    );
    let (window, events) =
        shard::download::browser::open("https://www.google.com/", &script, "Shard Browser")?;

    // The window runs on a thread of its own and reports here. Nothing is done
    // with what it says; this is only to keep the process alive until it
    // closes, and to notice when it has.
    for event in events {
        if matches!(event, shard::download::browser::Event::Closed) {
            break;
        }
    }
    drop(window);
    Ok(())
}
