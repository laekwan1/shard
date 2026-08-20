// Release builds are tray apps; a console window would just flash and linger.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    let _log = uikit::logging::init(shard::config::APP_NAME, "info");

    // A second copy would open its own packet handle against the same filter,
    // so every connection would be split and decoyed twice — more likely to
    // break a site than to bypass anything.
    // If a media file was double-clicked, this is its path. When another copy is
    // already running it is handed that path to play and this one steps aside;
    // otherwise it is remembered for the window about to open.
    let opened_file = media_argument();

    let Some(claim) = uikit::single::claim(shard::config::APP_NAME) else {
        if let Some(path) = &opened_file {
            shard::shell::send_file_to_running_copy(path);
        } else {
            uikit::single::wake_the_running_copy(shard::config::APP_NAME);
        }
        return Ok(());
    };
    // Held where the engine switch can let it go: turning the engine on without
    // a token relaunches this elevated, and the elevated copy can only take the
    // claim once this one has released it. See `single::release`.
    uikit::single::hold(shard::config::APP_NAME, claim);
    if let Some(path) = opened_file {
        shard::shell::set_opened_file(path);
    }

    // Runs unelevated now (asInvoker): only the engine needs a token, and it
    // asks for one when it is switched on rather than the whole program doing so
    // up front. So this is a note, not a failure.
    if !uikit::elevation::is_elevated() {
        tracing::info!("관리자 권한 없이 실행 중 — 엔진을 켤 때 승격합니다");
    }

    // One window holds everything: the engine switch, the browser, the library
    // and the settings. The two-window build it replaced is still here behind
    // `--legacy`, for the one screen that has not moved across yet — the block
    // test — and so a fault in the new shell has somewhere to fall back to.
    // Which of the two the user asked for. The flag says it outright; the name
    // says it for a copy of this program kept beside the other one, so both can
    // sit in the same folder and be opened by double-clicking either.
    let asked_for_classic = std::env::args().any(|arg| arg == "--legacy")
        || std::env::current_exe()
            .ok()
            .and_then(|path| path.file_stem().map(|s| s.to_string_lossy().to_lowercase()))
            .is_some_and(|name| name.contains("classic") || name.contains("legacy"));

    // Offer Shard as a program media files can be opened with. Cheap, idempotent
    // and per-user, so it is done on every start rather than tracked.
    shard::shell::register_file_types();

    if !asked_for_classic {
        let outcome = shard::shell::preview();
        match &outcome {
            Ok(()) => tracing::info!("shell closed"),
            Err(e) => tracing::error!("shell failed: {e:#}"),
        }
        return outcome;
    }

    // Logged on the way out. When the program vanished from the tray it was not
    // clear whether it had exited at all or only lost its icon, and those need
    // different things done about them.
    let outcome = shard::ui::run();
    match &outcome {
        Ok(()) => tracing::info!("ui loop ended, exiting"),
        Err(e) => tracing::error!("ui loop ended with an error: {e:#}"),
    }
    outcome
}

/// The path of a media file passed as an argument, if one was — what Explorer
/// hands us when Shard is the program a video or a song opens with.
///
/// Only a real file with a media extension, so a stray flag is never mistaken
/// for a file and the player never opens on nothing.
fn media_argument() -> Option<std::path::PathBuf> {
    const KINDS: &[&str] =
        &["mp4", "mkv", "webm", "mov", "m4v", "avi", "mp3", "m4a", "aac", "flac", "wav", "opus", "ogg"];
    std::env::args_os().skip(1).map(std::path::PathBuf::from).find(|path| {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| KINDS.contains(&e.to_ascii_lowercase().as_str()))
            .unwrap_or(false)
            && path.is_file()
    })
}
