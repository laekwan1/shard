// Release builds are tray apps; a console window would just flash and linger.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    let _log = uikit::logging::init(veil::config::APP_NAME, "info");

    // Two copies would each create a network adapter and its own firewall
    // rules, and the traffic would follow whichever won.
    let Some(_claim) = uikit::single::claim(veil::config::APP_NAME) else {
        uikit::single::point_at_the_running_copy(veil::config::APP_NAME);
        return Ok(());
    };

    if !uikit::elevation::is_elevated() {
        tracing::warn!("관리자 권한 없이 실행 중 — TUN 어댑터와 방화벽 규칙을 만들 수 없습니다");
    }

    veil::ui::run()
}
