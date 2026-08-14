//! Tray icon plumbing shared by both apps.
//!
//! On Windows the tray icon must be created on the thread that owns the message
//! loop, which for eframe is the main thread inside the app creation callback.

use crate::icon::Rgba;
use anyhow::Result;
use crossbeam_channel::Receiver;
use tray_icon::menu::MenuEvent;
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

pub use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};

/// Event streams for the tray, forwarded from tray-icon's global handlers.
pub struct TrayEvents {
    pub menu: Receiver<MenuEvent>,
    pub tray: Receiver<TrayIconEvent>,
}

/// Route tray and menu events into channels and repaint the UI when one arrives.
///
/// tray-icon's default handler publishes to a global channel that we would have
/// to poll on a timer; forwarding instead lets the window stay fully idle until
/// the user actually clicks something.
pub fn install_handlers(ctx: &egui::Context) -> TrayEvents {
    let (menu_tx, menu) = crossbeam_channel::unbounded();
    let repaint = ctx.clone();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_tx.send(event);
        repaint.request_repaint();
    }));

    let (tray_tx, tray) = crossbeam_channel::unbounded();
    let repaint = ctx.clone();
    TrayIconEvent::set_event_handler(Some(move |event| {
        let _ = tray_tx.send(event);
        repaint.request_repaint();
    }));

    TrayEvents { menu, tray }
}

pub fn build(tooltip: &str, icon: &Rgba, menu: Menu) -> Result<TrayIcon> {
    Ok(TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(tooltip)
        .with_icon(icon.to_tray_icon()?)
        .build()?)
}

pub fn set_icon(tray: &TrayIcon, icon: &Rgba) {
    match icon.to_tray_icon() {
        Ok(i) => {
            if let Err(e) = tray.set_icon(Some(i)) {
                tracing::warn!("could not update tray icon: {e}");
            }
        }
        Err(e) => tracing::warn!("could not build tray icon: {e}"),
    }
}

/// True when the event is a left click on the icon body, the gesture users
/// expect to open the window. Right clicks belong to the context menu.
pub fn is_activation(event: &TrayIconEvent) -> bool {
    matches!(
        event,
        TrayIconEvent::Click {
            button: tray_icon::MouseButton::Left,
            button_state: tray_icon::MouseButtonState::Up,
            ..
        } | TrayIconEvent::DoubleClick {
            button: tray_icon::MouseButton::Left,
            ..
        }
    )
}

pub fn show_window(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
}

pub fn hide_window(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
}

/// Turn the window's close button into "hide to tray".
///
/// Pass `quitting: true` once the user has actually chosen to exit, otherwise
/// the app could never be closed at all. Returns true when a close was
/// intercepted, so the caller can persist config at that point.
pub fn handle_close(ctx: &egui::Context, quitting: bool) -> bool {
    if quitting || !ctx.input(|i| i.viewport().close_requested()) {
        return false;
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
    hide_window(ctx);
    true
}
