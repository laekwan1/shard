// Release builds are tray apps; a console window would just flash and linger.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    let _log = uikit::logging::init(shard::config::APP_NAME, "info");

    // A second copy would open its own packet handle against the same filter,
    // so every connection would be split and decoyed twice — more likely to
    // break a site than to bypass anything.
    let Some(_claim) = uikit::single::claim(shard::config::APP_NAME) else {
        uikit::single::point_at_the_running_copy(shard::config::APP_NAME);
        return Ok(());
    };

    // The embedded manifest normally guarantees this, but a build run without
    // it would otherwise fail deep inside WinDivertOpen with a bare error code.
    if !uikit::elevation::is_elevated() {
        tracing::warn!("관리자 권한 없이 실행 중 — WinDivert 드라이버를 열 수 없습니다");
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
