//! Tray-resident settings window.

use crate::config::{Config, Scope, APP_NAME};
use crate::core::StatusKind;
use crate::engine::{Shared, StatsSnapshot};
use crate::prober::{self, Progress};
use crate::strategy::{Desync, Fooling, QuicMode, SplitAt};

use crossbeam_channel::Receiver;
use eframe::egui;
use egui::{Color32, RichText, Ui};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use std::sync::Arc;
use std::time::Instant;
use uikit::theme::{BAD, GOOD, MUTED, WARN};
use uikit::tray::{self, CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, TrayEvents};
use uikit::widgets;
use uikit::{icon, tray_icon::TrayIcon};

const ACCENT: Color32 = Color32::from_rgb(45, 212, 191);

/// Languages worth offering by name. Anything else the video has is still
/// reachable through its own default track.
const LANGUAGES: &[(&str, &str)] = &[
    ("ko", "한국어"),
    ("en", "English"),
    ("ja", "日本語"),
    ("zh", "中文"),
    ("es", "Español"),
    ("fr", "Français"),
    ("de", "Deutsch"),
];

/// The window has two states: the one control that matters, and everywhere
/// else. Keeping them apart is what lets the main screen stay a single button.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Home,
    Settings,
    Library,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Status,
    Strategy,
    Domains,
    Dns,
    Activity,
}

impl Tab {
    const ALL: [(Tab, &'static str); 5] = [
        (Tab::Status, "상태"),
        (Tab::Strategy, "전략"),
        (Tab::Domains, "도메인"),
        (Tab::Dns, "DNS"),
        (Tab::Activity, "활동"),
    ];
}

struct ProbeRun {
    host: String,
    rx: Receiver<Progress>,
    lines: Vec<(String, Option<bool>)>,
    finished: bool,
}

/// Handles for the menu entries, kept so events can be matched by id and the
/// checkmark kept in sync. The `Menu` itself is owned by the tray icon.
struct TrayMenu {
    toggle: CheckMenuItem,
    open: MenuItem,
    quit: MenuItem,
}

pub struct ShardApp {
    shared: Arc<Shared>,
    /// The engine and its DNS, without a face — shared with the new shell.
    core: crate::core::EngineCore,

    tray: TrayIcon,
    tray_menu: TrayMenu,
    events: TrayEvents,
    /// Held purely to keep the registration alive; dropping it unbinds the key.
    _hotkeys: Option<GlobalHotKeyManager>,
    hotkey_rx: Receiver<GlobalHotKeyEvent>,

    view: View,
    tab: Tab,

    probe: Option<ProbeRun>,
    probe_host: String,
    new_domain: String,
    new_exclude: String,
    quitting: bool,
    /// The download window, while one is open.
    browser: Option<crate::download::browser::Window>,
    /// What that window's page has to say.
    browser_events: Option<std::sync::mpsc::Receiver<crate::download::browser::Event>>,
    /// What the page last offered, kept so a click can be matched to a format.
    offer: Option<crate::download::youtube::Offer>,
    /// The downloads in flight (and just-finished), so several run at once.
    downloads: Vec<Download>,
    /// What the library screen is looking at.
    shelf: crate::library::Kind,
    /// Which folder the shelf is narrowed to, or none for all of it.
    shelf_folder: Option<String>,
    /// What is on the shelf, read when the screen is opened and after a change.
    shelf_items: Vec<crate::library::Item>,
    /// The name being typed into the new-folder box, when one is open.
    new_folder: Option<String>,
    /// Set when something has changed what is on a shelf, so the library reads
    /// it again the next time it is drawn rather than on a timer.
    shelf_stale: bool,
    /// The row being held down, and since when: a file moves only once it has
    /// been picked up deliberately, not the moment the pointer crosses it.
    holding: Option<(usize, Instant)>,
    /// The file being given another name, and the name so far.
    renaming: Option<(std::path::PathBuf, String)>,
}

/// What a running download has to report.
enum SaveStep {
    Progress(u64, u64),
    Done,
    Failed(String),
}

/// One download among possibly several, and where it has got to.
struct Download {
    title: String,
    rx: std::sync::mpsc::Receiver<SaveStep>,
    done: u64,
    total: u64,
    state: DownloadState,
    /// The finished line: where it saved to, or why it failed.
    note: String,
    /// Flipped to stop it early.
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[derive(PartialEq)]
enum DownloadState {
    Running,
    Done,
    Failed,
}

/// A string field out of a small message, without a parser for one value.
fn field(payload: &str, name: &str) -> Option<String> {
    let key = format!("\"{name}\":\"");
    let at = payload.find(&key)? + key.len();
    let rest = &payload[at..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// "32,32,32" as Windows wants a colour: blue, green, red, in that order.
fn colour_ref(text: &str) -> Option<u32> {
    let mut parts = text.split(',').map(|p| p.trim().parse::<u32>().ok());
    let red = parts.next()??;
    let green = parts.next()??;
    let blue = parts.next()??;
    Some((blue << 16) | (green << 8) | red)
}

/// The itag in `{"choose":401}`, without pulling in a parser for one number.
fn chosen(payload: &str) -> Option<u32> {
    let at = payload.find("\"choose\"")?;
    let rest = &payload[at + 8..];
    let digits: String = rest.chars().skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// One row per resolution, carrying the smallest encoding of it.
///
/// The full list runs to sixteen entries because each resolution is offered in
/// three codecs, and reading it means comparing the same number three times to
/// reach an answer that is always the same: take the small one. Filtering to a
/// preferred codec instead would empty the list on every video YouTube never
/// bothered to encode that way, which is most of the older ones.
fn rows_for(
    offer: &crate::download::youtube::Offer,
    wish: &crate::download::youtube::AudioWish,
) -> Vec<(u32, String, String)> {
    let audio = match offer.best_audio(wish) {
        Some(audio) => audio,
        None => return Vec::new(),
    };
    let mut seen: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    for video in offer.video_tracks() {
        if seen.contains(&video.quality) {
            continue;
        }
        seen.push(video.quality.clone());
        let total = video.size() + audio.size();
        // Marked when the number is ours rather than the page's, so a figure
        // that turns out to be off by a few per cent does not read as a lie.
        let size = if video.size_is_exact() {
            human(total)
        } else {
            format!("약 {}", human(total))
        };
        rows.push((
            video.itag,
            video.quality.clone(),
            // The audio's bitrate is spelled out because it is the one choice
            // the user cannot see any other way, and it is what "the sound is
            // poor" turns out to be about.
            format!(
                "{} · {} + {} {}k",
                size,
                video.codec(),
                audio.codec(),
                audio.bitrate / 1000
            ),
        ));
    }
    rows
}

/// The marker itag the music-only row carries, so a click on it is told apart
/// from a click on a real video row. No real format uses it.
const MUSIC_ITAG: u32 = u32::MAX;

/// The music-only row, shown at the top of the list.
///
/// It names what is about to be saved and how large it will be — what someone
/// about to spend ten megabytes wants to see before they spend it — and carries
/// the marker itag rather than the audio's own, so choosing it saves the sound
/// alone rather than being mistaken for a format.
fn music_row(offer: &crate::download::youtube::Offer, wish: &crate::download::youtube::AudioWish)
    -> Option<(u32, String, String)>
{
    let audio = offer.best_audio(wish)?;
    Some((
        MUSIC_ITAG,
        "음악만 저장".into(),
        format!(
            "{} · {} {}k{}",
            human(audio.size()),
            audio.codec(),
            audio.bitrate / 1000,
            if audio.audio_name.is_empty() {
                String::new()
            } else {
                format!(" · {}", audio.audio_name)
            }
        ),
    ))
}

/// The two shelves as two tabs, half the window each.
///
/// Drawn as a browser draws its tabs: the chosen one is the same colour as the
/// sheet below it and has no line under it, so the folders and the list read as
/// that tab's contents. Returns the shelf pressed, if one was.
fn shelf_tabs(ui: &mut Ui, current: crate::library::Kind) -> Option<crate::library::Kind> {
    use crate::library::Kind;

    let height = 40.0;
    let room = ui.available_width();
    let (strip, _) = ui.allocate_exact_size(egui::vec2(room, height), egui::Sense::hover());
    let sheet = Color32::from_rgb(0x1a, 0x1a, 0x1d);
    let mut picked = None;

    for (at, kind) in Kind::ALL.into_iter().enumerate() {
        let half = strip.width() / 2.0;
        let rect = egui::Rect::from_min_size(
            egui::pos2(strip.left() + half * at as f32, strip.top()),
            egui::vec2(half, height),
        );
        let response = ui.interact(rect, ui.id().with(("shelf", at)), egui::Sense::click());
        let on = kind == current;
        if response.clicked() {
            picked = Some(kind);
        }
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let hot = ui
            .ctx()
            .animate_bool_with_time(response.id.with("hot"), response.hovered(), 0.12);
        let painter = ui.painter();
        if on {
            // The chosen tab is the sheet: rounded at the top, open at the
            // bottom, so it and what it holds are one shape.
            painter.rect_filled(
                rect,
                egui::CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 },
                sheet,
            );
        } else {
            painter.rect_filled(
                rect.shrink2(egui::vec2(0.0, 3.0)),
                egui::CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 },
                blend_to(Color32::from_rgb(0x13, 0x13, 0x16), sheet, hot * 0.6),
            );
            // The line under the tabs that are not open, which the open one
            // breaks through.
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.left(), rect.bottom() - 1.0),
                    egui::vec2(rect.width(), 1.0),
                ),
                0.0,
                Color32::from_rgb(0x2e, 0x2e, 0x34),
            );
        }

        // The mark, not a word: a film frame for video, a note for music.
        let ink = if on { Color32::WHITE } else { blend_to(MUTED, Color32::WHITE, hot) };
        let centre = rect.center();
        match kind {
            Kind::Video => {
                let frame = egui::Rect::from_center_size(centre, egui::vec2(26.0, 19.0));
                painter.rect_stroke(
                    frame,
                    4.0,
                    egui::Stroke::new(1.6, ink),
                    egui::StrokeKind::Middle,
                );
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        egui::pos2(centre.x - 3.0, centre.y - 4.8),
                        egui::pos2(centre.x - 3.0, centre.y + 4.8),
                        egui::pos2(centre.x + 5.2, centre.y),
                    ],
                    ink,
                    egui::Stroke::NONE,
                ));
            }
            Kind::Music => {
                let stem = egui::Stroke::new(1.8, ink);
                let left = egui::pos2(centre.x - 5.0, centre.y + 5.0);
                let right = egui::pos2(centre.x + 6.5, centre.y + 3.0);
                painter.line_segment(
                    [left, egui::pos2(left.x, centre.y - 8.0)],
                    stem,
                );
                painter.line_segment(
                    [right, egui::pos2(right.x, centre.y - 9.8)],
                    stem,
                );
                painter.line_segment(
                    [
                        egui::pos2(left.x, centre.y - 8.0),
                        egui::pos2(right.x, centre.y - 9.8),
                    ],
                    egui::Stroke::new(2.4, ink),
                );
                painter.circle_filled(egui::pos2(left.x - 2.2, left.y + 0.8), 3.1, ink);
                painter.circle_filled(egui::pos2(right.x - 2.2, right.y + 0.8), 3.1, ink);
            }
        }
    }
    picked
}

/// The way to make a folder: a ring with a cross in it, lighting on approach.
fn plus_button(ui: &mut Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let hot = ui.ctx().animate_bool_with_time(response.id, response.hovered(), 0.13);
        let painter = ui.painter();
        let colour = blend_to(MUTED, Color32::WHITE, hot);
        if hot > 0.01 {
            painter.circle_filled(
                rect.center(),
                14.0,
                Color32::from_rgba_unmultiplied(255, 255, 255, (18.0 * hot) as u8),
            );
        }
        painter.circle_stroke(rect.center(), 11.0, egui::Stroke::new(1.3, colour));
        let arm = 5.0;
        let stroke = egui::Stroke::new(1.6, colour);
        painter.line_segment(
            [
                egui::pos2(rect.center().x - arm, rect.center().y),
                egui::pos2(rect.center().x + arm, rect.center().y),
            ],
            stroke,
        );
        painter.line_segment(
            [
                egui::pos2(rect.center().x, rect.center().y - arm),
                egui::pos2(rect.center().x, rect.center().y + arm),
            ],
            stroke,
        );
    }
    response.on_hover_text("새 폴더")
}

/// Mix two colours, for the lit states above.
fn blend_to(from: Color32, to: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
    Color32::from_rgb(
        mix(from.r(), to.r()),
        mix(from.g(), to.g()),
        mix(from.b(), to.b()),
    )
}

/// A label drawn as a tab: used for the "보관함" chip beside the app name.
fn shelf_tab(ui: &mut Ui, on: bool, label: &str) -> egui::Response {
    let text = egui::WidgetText::from(RichText::new(label).size(13.0).strong());
    let galley = text.into_galley(ui, None, f32::INFINITY, egui::TextStyle::Button);
    let size = egui::vec2(galley.size().x + 30.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if on {
            painter.rect_filled(rect, 9.0, Color32::from_rgb(0x34, 0x34, 0x3b));
        } else if response.hovered() {
            painter.rect_filled(rect, 9.0, Color32::from_rgb(0x24, 0x24, 0x29));
        }
        let colour = if on { Color32::WHITE } else { MUTED };
        painter.galley(
            rect.center() - galley.size() / 2.0,
            galley,
            colour,
        );
    }
    response
}

/// A folder, drawn as a browser tab would be: the one in force underlined in the
/// accent. It is also somewhere to put things — a row dragged onto it is filed
/// there, which is the only way a file is moved.
fn folder_tab(
    ui: &mut Ui,
    on: bool,
    label: &str,
    dropped: &mut Option<(usize, String)>,
    target: &str,
) -> bool {
    let text = egui::WidgetText::from(RichText::new(label).size(13.0));
    let galley = text.into_galley(ui, None, f32::INFINITY, egui::TextStyle::Button);
    let size = egui::vec2(galley.size().x + 24.0, 28.0);

    let (response, payload) = ui.dnd_drop_zone::<usize, ()>(egui::Frame::NONE, |ui| {
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let colour = if on { Color32::WHITE } else { MUTED };
            painter.galley(
                egui::pos2(rect.center().x - galley.size().x / 2.0, rect.top() + 4.0),
                galley,
                colour,
            );
            if on {
                let line = egui::Rect::from_min_size(
                    egui::pos2(rect.left() + 6.0, rect.bottom() - 3.0),
                    egui::vec2(rect.width() - 12.0, 2.5),
                );
                painter.rect_filled(line, 2.0, ACCENT);
            }
        }
    });
    if let Some(index) = payload {
        *dropped = Some((*index, target.to_string()));
    }
    // The zone swallows clicks, so the label is asked whether it was pressed.
    response.response.interact(egui::Sense::click()).clicked()
}

/// The play mark on a library row: a filled triangle in a ring.
///
/// Placed at a rect the caller works out rather than allocated where it happens
/// to land, so what is drawn and what answers the pointer are the same square.
fn play_mark(ui: &mut Ui, rect: egui::Rect) -> egui::Response {
    let response = ui.interact(rect, ui.id().with(("play", rect.left() as i32, rect.top() as i32)), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let lit = response.hovered();
        let colour = if lit { Color32::WHITE } else { MUTED };
        painter.circle_stroke(rect.center(), 12.0, egui::Stroke::new(1.4, colour));
        let mark = 6.0;
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(rect.center().x - mark * 0.5, rect.center().y - mark),
                egui::pos2(rect.center().x - mark * 0.5, rect.center().y + mark),
                egui::pos2(rect.center().x + mark, rect.center().y),
            ],
            colour,
            egui::Stroke::NONE,
        ));
    }
    response.on_hover_text("재생")
}

/// A title cut to fit one line, with an ellipsis where it was cut.
///
/// Counted in characters rather than bytes: a Korean title is three bytes a
/// letter, and cutting by bytes would both truncate it far too early and split
/// a letter in half.
fn shorten(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_string();
    }
    let kept: String = text.chars().take(most.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn human(bytes: u64) -> String {
    const GB: u64 = 1 << 30;
    const MB: u64 = 1 << 20;
    match bytes {
        0 => "크기 미상".into(),
        b if b >= GB => format!("{:.1} GB", b as f64 / GB as f64),
        b if b >= MB => format!("{} MB", b / MB),
        b => format!("{} KB", (b / 1024).max(1)),
    }
}

/// Where saved videos go: the user's own Videos folder, in a folder of ours —
/// beside their other videos, the way music lands beside their music.
fn videos_folder() -> std::path::PathBuf {
    let base = std::env::var("USERPROFILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("Videos").join("Shard")
}

/// Where music-only downloads land: the user's own Music folder, under Shard.
///
/// Beside `Music`, not among the video downloads, so a music app pointed at the
/// Music library finds them and a folder of songs is only songs.
fn music_folder() -> std::path::PathBuf {
    let base = std::env::var("USERPROFILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("Music").join("Shard")
}

impl ShardApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> anyhow::Result<Self> {
        uikit::theme::install(&cc.egui_ctx, ACCENT);

        let config = Config::load();
        let autostart = config.start_engine_on_launch;
        let hotkey_spec = config.hotkey.clone();
        let shared = Shared::new(config);

        let events = tray::install_handlers(&cc.egui_ctx);
        let (menu, tray_menu) = build_menu()?;
        let tray = tray::build("Shard — 우회 꺼짐", &icon::shard(false), menu)?;

        let (_hotkeys, hotkey_rx) = install_hotkey(&hotkey_spec, &cc.egui_ctx);

        let mut app = Self {
            shared: shared.clone(),
            core: crate::core::EngineCore::new(shared),
            tray,
            tray_menu,
            events,
            _hotkeys,
            hotkey_rx,
            view: View::Home,
            tab: Tab::Status,
            probe: None,
            probe_host: String::new(),
            new_domain: String::new(),
            new_exclude: String::new(),
            quitting: false,
            browser: None,
            browser_events: None,
            offer: None,
            downloads: Vec::new(),
            shelf: crate::library::Kind::Video,
            shelf_folder: None,
            shelf_items: Vec::new(),
            new_folder: None,
            shelf_stale: true,
            holding: None,
            renaming: None,
        };

        if autostart {
            app.start();
        }
        Ok(app)
    }

    fn running(&self) -> bool {
        self.core.running()
    }

    /// Open the download window, or bring the open one forward.
    ///
    /// It is a window of its own on a thread of its own rather than a view in
    /// here: this application owns the main thread's event loop, and a second
    /// one cannot share it.
    fn open_browser(&mut self) {
        if let Some(window) = &self.browser {
            if window.focus().is_ok() {
                return;
            }
            // It was closed from its own title bar, so the handle is stale.
            self.browser = None;
        }
        let script = format!(
            "{}
{}",
            crate::download::youtube::RECORDER,
            crate::download::youtube::CONTROL
        );
        match crate::download::browser::open("https://www.google.com/", &script, "Shard Browser") {
            Ok((window, events)) => {
                self.browser = Some(window);
                self.browser_events = Some(events);
            }
            Err(e) => self.set_status(StatusKind::Bad, format!("브라우저를 열지 못했습니다: {e}")),
        }
    }

    /// Everything the download window has said since the last frame.
    fn drain_browser(&mut self) {
        use crate::download::browser::{Command, Event};
        use crate::download::youtube;

        let mut messages = Vec::new();
        if let Some(events) = &self.browser_events {
            while let Ok(event) = events.try_recv() {
                messages.push(event);
            }
        }
        for event in messages {
            // The window is gone; the handle it left behind is not worth
            // keeping, and holding one made reopening ask a dead thread first.
            if matches!(event, Event::Closed) {
                tracing::info!("browser window closed");
                self.browser = None;
                self.browser_events = None;
                continue;
            }
            let Event::Offer(payload) = event else { continue };
            if payload.contains("\"frame\"") {
                // Which tab said it. Every page keeps reporting while it is
                // open, background ones included, so a report with no name on
                // it was applied to whichever tab was in front — and a tab in
                // the back renamed and recoloured the one being looked at.
                let tab = field(&payload, "tab").and_then(|t| t.parse::<u64>().ok());
                // Matched on the field rather than on the word appearing
                // anywhere: the text a page reports is its own title, and a
                // page called "url" or "colour" would otherwise be filed as
                // one. JSON.stringify writes no spaces, so this is exact.
                let kind = field(&payload, "frame").unwrap_or_default();
                if kind == "hide" {
                    self.tell_page(Command::Toolbar(false));
                } else if let Some(tab) = tab {
                    match kind.as_str() {
                        "colour" => {
                            if let Some(colour) =
                                field(&payload, "text").and_then(|t| colour_ref(&t))
                            {
                                self.tell_page(Command::PageColour(tab, colour));
                            }
                        }
                        "url" => {
                            if let Some(text) = field(&payload, "text") {
                                self.tell_page(Command::TabUrl(tab, text));
                            }
                        }
                        "title" => {
                            if let Some(text) = field(&payload, "text") {
                                self.tell_page(Command::TabTitle(tab, text));
                            }
                        }
                        _ => {}
                    }
                }
                continue;
            }
            if payload.contains("\"ask\"") {
                self.tell_page(Command::Evaluate(youtube::ASK.into()));
            } else if let Some(itag) = chosen(&payload) {
                self.begin_save(itag);
            } else {
                self.show_qualities(&payload);
            }
        }
        self.drain_saving();
    }

    /// Turn what the page reported into a list it can show.
    fn show_qualities(&mut self, payload: &str) {
        use crate::download::browser::Command;
        use crate::download::youtube::{self, Offer};

        let offer = match Offer::parse(payload) {
            Ok(offer) => offer,
            Err(_) => return,
        };
        if offer.template().is_none() {
            let message = if offer.formats.is_empty() {
                "이 페이지에서 영상 정보를 읽지 못했습니다."
            } else {
                "영상을 잠시 재생한 뒤 다시 눌러 주세요."
            };
            self.tell_page(Command::Evaluate(youtube::say_script(message, true)));
            return;
        }
        // The video rows, and a music-only row at the top — the same shape the
        // phone shows, so there is no separate "include video" setting to find.
        let mut rows = rows_for(&offer, &self.audio_wish(false));
        if let Some(music) = music_row(&offer, &self.audio_wish(true)) {
            rows.insert(0, music);
        }
        if rows.is_empty() {
            // Say what was actually found. "No qualities" is true of a page
            // that offered nothing and of one whose formats were all rejected
            // here, and those need different things done about them.
            // Counted after the filters this code applies, not before: the
            // first version of this message reported what the page listed and
            // so said twenty audio formats while the chooser had none.
            let video = offer.video_tracks().len();
            let audio = offer.formats.iter().filter(|f| f.is_audio()).count();
            let chosen = offer.best_audio(&self.audio_wish(false)).map(|f| f.itag).unwrap_or(0);
            self.tell_page(Command::Evaluate(youtube::say_script(
                &format!(
                    "받을 수 있는 화질이 없습니다.
(형식 {} · 쓸 수 있는 영상 {} · 음성 {} · 고른 음성 {})",
                    offer.formats.len(),
                    video,
                    audio,
                    chosen
                ),
                true,
            )));
            return;
        }
        self.tell_page(Command::Evaluate(youtube::list_script(&rows)));
        self.offer = Some(offer);
    }

    /// Start fetching the chosen format on a thread of its own.
    fn begin_save(&mut self, itag: u32) {
        use crate::download::browser::Command;
        use crate::download::{save, youtube};

        let Some(offer) = self.offer.clone() else { return };
        let Some(template) = offer.template() else { return };
        // The row decides it: the music row carries a marker itag, every other
        // row a real video one.
        let audio_only = itag == MUSIC_ITAG;
        let wish = self.audio_wish(audio_only);
        let Some(audio) = offer.best_audio(&wish) else { return };
        let Some(video) = (if audio_only {
            offer.video_tracks().into_iter().last()
        } else {
            offer.formats.iter().find(|f| f.itag == itag)
        }) else {
            return;
        };
        // Naming the wanted format as already playing costs the stream's
        // opening bytes, so anything else is named instead.
        let decoy = offer
            .video_tracks()
            .into_iter()
            .find(|f| f.itag != itag)
            .map(|f| f.track())
            .unwrap_or_else(|| audio.track());

        let job = save::Job {
            template,
            video: video.track(),
            audio: audio.track(),
            decoy,
            title: offer.title.clone(),
            // Music of its own goes to a Music folder, not among the videos, so
            // a folder of songs is a folder of songs.
            into: if audio_only { music_folder() } else { videos_folder() },
            cover: offer.thumb.clone(),
            audio_only,
            mp4: false,   // desktop plays through the WebView, which opens MKV/WebM too
            // The old two-window UI is frozen; MP3 is offered only in the shell.
            music_mp3: false,
        };
        let expected = job.video.bytes + job.audio.bytes;
        let title = offer.title.clone();

        let (tx, rx) = std::sync::mpsc::channel::<SaveStep>();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = cancel.clone();
        std::thread::spawn(move || {
            let mut report = |p: crate::download::pull::Progress| {
                let done = p.video + p.audio;
                let _ = tx.send(SaveStep::Progress(done, expected));
            };
            let outcome = save::run(&job, &mut report, &|| {
                stop.load(std::sync::atomic::Ordering::Relaxed)
            });
            let _ = tx.send(match outcome {
                Ok(_) => SaveStep::Done,
                Err(e) => SaveStep::Failed(format!("{e:#}")),
            });
        });
        self.downloads.push(Download {
            title,
            rx,
            done: 0,
            total: expected,
            state: DownloadState::Running,
            note: String::new(),
            cancel,
        });
        // Said, then gone. The panel closing by itself is what leaves the page
        // ready for the next video straight away; how far along it is belongs in
        // the Shard window, not on top of what is being watched.
        self.tell_page(Command::Evaluate(youtube::flash_script("받는 중")));
    }

    fn drain_saving(&mut self) {
        use crate::download::browser::Command;
        use crate::download::youtube;

        let mut stale = false;
        for download in &mut self.downloads {
            while let Ok(step) = download.rx.try_recv() {
                match step {
                    SaveStep::Progress(done, total) => {
                        download.done = done;
                        if total > 0 {
                            download.total = total;
                        }
                    }
                    SaveStep::Done => {
                        download.state = DownloadState::Done;
                        download.note = "저장했습니다".into();
                        // There is a new file on a shelf: the library reads it
                        // again the moment it is looked at, rather than polling
                        // to notice what this program has just put there.
                        stale = true;
                    }
                    SaveStep::Failed(why) => {
                        download.state = DownloadState::Failed;
                        download.note = format!("실패: {why}");
                    }
                }
            }
        }
        // Announce each finished download in the browser overlay, then drop it
        // from the list so only what is still running is kept.
        if stale {
            self.shelf_stale = true;
        }
        // A success says so and goes; a failure stays until it has been read.
        let finished: Vec<(String, bool)> = self
            .downloads
            .iter()
            .filter(|d| d.state != DownloadState::Running)
            .map(|d| (d.note.clone(), d.state == DownloadState::Failed))
            .collect();
        self.downloads.retain(|d| d.state == DownloadState::Running);
        for (note, failed) in finished {
            let script = if failed {
                youtube::say_script(&note, true)
            } else {
                youtube::flash_script(&note)
            };
            self.tell_page(Command::Evaluate(script));
        }
    }

    /// What the settings say about sound, in the form the chooser wants.
    ///
    /// [portable] is set for the music-only row: a file saved on its own is a
    /// music file, and it has to play on a phone rather than only in this
    /// program, so it takes the compatible codec. A video's own soundtrack does
    /// not, and picks the smaller one.
    fn audio_wish(&self, portable: bool) -> crate::download::youtube::AudioWish {
        let cfg = self.shared.config.read();
        crate::download::youtube::AudioWish {
            language: cfg.download.audio_language.clone(),
            quality: cfg.download.audio,
            portable,
        }
    }

    fn tell_page(&mut self, command: crate::download::browser::Command) {
        if let Some(window) = &self.browser {
            if window.send(command).is_err() {
                self.browser = None;
                self.browser_events = None;
            }
        }
    }

    fn set_status(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.core.set_status(kind, message);
    }

    fn start(&mut self) {
        self.core.start();
        self.refresh_tray();
    }

    fn stop(&mut self) {
        self.core.stop();
        self.refresh_tray();
    }

    fn toggle(&mut self) {
        self.core.toggle();
        self.refresh_tray();
    }

    fn refresh_tray(&self) {
        let running = self.running();
        let art = match (running, self.core.status_kind) {
            (true, StatusKind::Warn) => icon::warn(true),
            (true, _) => icon::shard(true),
            (false, _) => icon::shard(false),
        };
        tray::set_icon(&self.tray, &art);
        let tooltip = if running { "Shard — 우회 동작 중" } else { "Shard — 우회 꺼짐" };
        let _ = self.tray.set_tooltip(Some(tooltip));
        self.tray_menu.toggle.set_checked(running);
    }

    /// Not named `save`: that would shadow `eframe::App::save`.
    fn save_config(&self) {
        self.core.save_config();
    }

    /// Restart the engine so a changed filter or worker count takes effect.
    fn restart_if_running(&mut self) {
        if self.running() {
            self.stop();
            self.start();
        }
    }

    fn pump_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.events.menu.try_recv() {
            let id = &event.id;
            if id == self.tray_menu.toggle.id() {
                self.toggle();
                // Reflect the real state: a failed start must not leave it ticked.
                self.tray_menu.toggle.set_checked(self.running());
            } else if id == self.tray_menu.open.id() {
                tray::show_window(ctx);
            } else if id == self.tray_menu.quit.id() {
                tracing::info!("quit chosen from the tray menu");
                self.quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        while let Ok(event) = self.events.tray.try_recv() {
            if tray::is_activation(&event) {
                tray::show_window(ctx);
            }
        }
        while self.hotkey_rx.try_recv().is_ok() {
            // Fires on both press and release; act on one edge only.
            self.toggle();
        }
    }

    fn pump_probe(&mut self) {
        let Some(probe) = self.probe.as_mut() else { return };
        while let Ok(progress) = probe.rx.try_recv() {
            probe.finished |= prober::is_last(&progress);
            probe.lines.extend(prober::say(progress));
        }
    }
}

impl eframe::App for ShardApp {
    /// Non-drawing work: tray clicks, probe progress, and the close-to-tray
    /// interception. Runs even while the window is hidden, which is exactly
    /// when tray events need handling.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump_events(ctx);
        self.pump_probe();
        // The download window runs on its own thread and reports here. It has
        // to be drained even while this window is hidden, which is most of the
        // time — the browser is used with the settings closed.
        self.drain_browser();
        if self.browser.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        if uikit::tray::handle_close(ctx, self.quitting) {
            tracing::info!("close intercepted, hiding to tray");
            self.save_config();
        }
        if self.quitting {
            self.stop();
            self.save_config();
        }

        // Something is always scheduled, even with nothing to draw.
        //
        // This used to be conditional on the engine running, and the window
        // vanished from the tray when it was closed with the engine off —
        // which is exactly the case where nothing was scheduled. The condition
        // was the only thing that differed between the two, so it is the only
        // thing this changes. A wake-up a second costs nothing while idle.
        let busy = self.probe.as_ref().is_some_and(|p| !p.finished)
            || (self.running() && self.tab != Tab::Strategy);
        ctx.request_repaint_after(std::time::Duration::from_millis(if busy { 500 } else { 1000 }));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        match self.view {
            View::Home => self.home(ui),
            View::Settings => self.settings(ui),
            View::Library => self.library(ui),
        }
    }
}

impl ShardApp {
    /// One button, the state it is in, and the two places to go from here.
    fn home(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(34.0);
                ui.label(RichText::new("S H A R D").size(13.0).color(MUTED).strong());
                ui.add_space(38.0);

                if widgets::power_button(ui, self.running(), ACCENT, 156.0).clicked() {
                    self.toggle();
                }

                ui.add_space(26.0);
                let (headline, colour) = match (self.running(), self.core.status_kind) {
                    (_, StatusKind::Bad) => ("문제 발생", BAD),
                    (true, StatusKind::Warn) => ("동작 중 · 주의", WARN),
                    (true, _) => ("우회 중", GOOD),
                    (false, _) => ("꺼짐", MUTED),
                };
                ui.label(RichText::new(headline).size(20.0).color(colour).strong());

                // The most common misunderstanding is that this window is where
                // you open a site. It is not — the engine is transparent to
                // every program on the machine — so say that outright.
                let detail = if self.running() {
                    "브라우저에서 평소처럼 접속하면 됩니다".to_string()
                } else if self.core.status_kind == StatusKind::Bad {
                    self.core.status.clone()
                } else {
                    "버튼을 눌러 시작하세요".to_string()
                };
                ui.label(RichText::new(detail).size(12.0).color(MUTED));

                if self.running() {
                    let auto = self.shared.config.read().auto_learn;
                    let note = if auto {
                        "막히는 사이트는 알아서 감지하고 학습합니다"
                    } else {
                        "자동 학습이 꺼져 있습니다 — 설정에서 켤 수 있습니다"
                    };
                    ui.label(RichText::new(note).size(11.0).color(MUTED));
                }

                // What is being fetched, one line each. Several downloads run at
                // once, so they are listed rather than being a single line the
                // next one overwrites — and each can be called off on its own.
                if !self.downloads.is_empty() {
                    ui.add_space(20.0);
                    for download in &self.downloads {
                        let fraction = if download.total > 0 {
                            (download.done as f64 / download.total as f64).clamp(0.0, 1.0) as f32
                        } else {
                            0.0
                        };
                        // One line each: what it is, how far along, and the way
                        // to call it off. A download is a passing thing, not
                        // something to give three lines of the main screen to.
                        ui.horizontal(|ui| {
                            let row = 320.0;
                            ui.add_space(((ui.available_width() - row) / 2.0).max(0.0));
                            ui.label(
                                RichText::new(shorten(&download.title, 24)).size(11.0).color(MUTED),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("✕").on_hover_text("받기 취소").clicked() {
                                        download
                                            .cancel
                                            .store(true, std::sync::atomic::Ordering::Relaxed);
                                    }
                                    ui.add(
                                        egui::ProgressBar::new(fraction).desired_width(96.0).text(
                                            RichText::new(format!(
                                                "{}%",
                                                (fraction * 100.0) as u32
                                            ))
                                            .size(10.0),
                                        ),
                                    );
                                },
                            );
                        });
                        ui.add_space(6.0);
                    }
                }

                // The counters that used to sit here are gone. They were the
                // engine talking about itself — none of them told anyone
                // whether a site would load, which is the only question the
                // main screen exists to answer. They are still in the settings
                // window for anyone who wants them.
                // Pinned to the bottom rather than following the text above.
                // The status line changes length as the engine starts and
                // stops, and buttons that move when a message changes are
                // buttons that get missed.
                let remaining = ui.available_height();
                ui.add_space((remaining - 58.0).max(12.0));
                ui.horizontal(|ui| {
                    // Centred by hand: three fixed-width buttons with a gap
                    // between each.
                    let row = 34.0 * 3.0 + 18.0 * 2.0;
                    ui.add_space((ui.available_width() - row) / 2.0);
                    if widgets::icon_button(ui, widgets::Glyph::Settings, "설정").clicked() {
                        self.view = View::Settings;
                    }
                    ui.add_space(18.0);
                    if widgets::icon_button(ui, widgets::Glyph::Browser, "Shard 브라우저 — 영상 받기")
                        .clicked()
                    {
                        self.open_browser();
                    }
                    ui.add_space(18.0);
                    // The library last: what has been saved is where you end up,
                    // and its play mark tells it apart from the browser's window.
                    if widgets::icon_button(ui, widgets::Glyph::Library, "보관함 — 받아 둔 영상과 음악")
                        .clicked()
                    {
                        self.view = View::Library;
                        self.reload_shelf();
                    }
                });
            });
        });
    }

    /// Re-read the shelf being shown, and drop a folder filter that has gone.
    fn reload_shelf(&mut self) {
        self.shelf_stale = false;
        self.shelf_items = crate::library::items(self.shelf);
        let names = crate::library::folders(self.shelf);
        if let Some(folder) = &self.shelf_folder {
            if !names.contains(folder) {
                self.shelf_folder = None;
            }
        }
    }

    /// The offline library: what has been saved, and what can be done with it.
    ///
    /// Two shelves, videos and music, matching the two folders downloads land
    /// in. Opening a file hands it to whatever the user plays that kind with
    /// rather than playing it here — their player already knows their subtitles
    /// and their volume, and a media player is a large thing to write badly.
    fn library(&mut self, ui: &mut Ui) {
        use crate::library::{self, Kind};

        // Read when something changes it, not on a clock: a download finishing
        // marks the shelf stale (see `drain_saving`), and so does anything done
        // here. Polling a folder several times a second to notice a file this
        // program saved itself is work to learn what it already knew.
        if self.shelf_stale {
            self.reload_shelf();
        }

        let mut dropped_on: Option<(usize, String)> = None;

        egui::Panel::top("library-header").show(ui, |ui| {
            ui.add_space(8.0);
            // The name of the window, and the way back: pressing "Shard" is
            // going home, the same as it is in the browser.
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Label::new(RichText::new("Shard").size(15.0).strong())
                        .sense(egui::Sense::click()))
                    .on_hover_text("홈으로")
                    .clicked()
                {
                    self.view = View::Home;
                }
                ui.add_space(10.0);
                shelf_tab(ui, true, "보관함");
            });
            ui.add_space(8.0);

            // The two shelves as two tabs, half the width each, with everything
            // below them sitting on the sheet the chosen one opens onto — the
            // shape a browser's tabs make, so it reads as "this tab's contents"
            // rather than as two buttons that happen to be above a list.
            if let Some(picked) = shelf_tabs(ui, self.shelf) {
                if picked != self.shelf {
                    self.shelf = picked;
                    self.shelf_folder = None;
                    self.reload_shelf();
                }
            }

            // The folders on this shelf, under the shelves they belong to. Each
            // one takes a row dragged onto it — that is how something is filed,
            // rather than picking a folder out of a menu.
            let names = library::folders(self.shelf);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                if folder_tab(ui, self.shelf_folder.is_none(), "전체", &mut dropped_on, "") {
                    self.shelf_folder = None;
                }
                for name in &names {
                    let on = self.shelf_folder.as_deref() == Some(name.as_str());
                    if folder_tab(ui, on, name, &mut dropped_on, name) {
                        self.shelf_folder = if on { None } else { Some(name.clone()) };
                    }
                }
                // The way to make one, at the end of the row of them.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(6.0);
                    if plus_button(ui).clicked() {
                        self.new_folder = Some(String::new());
                    }
                });
            });

            // Making a folder: a line to type in, right where it will appear.
            if let Some(name) = &mut self.new_folder {
                ui.add_space(4.0);
                let mut made = None;
                ui.horizontal(|ui| {
                    ui.label("폴더 이름");
                    ui.text_edit_singleline(name);
                    if ui.button("만들기").clicked() {
                        made = Some(name.clone());
                    }
                    if ui.button("취소").clicked() {
                        made = Some(String::new());
                    }
                });
                if let Some(wanted) = made {
                    if !wanted.trim().is_empty() {
                        library::add_folder(self.shelf, &wanted);
                    }
                    self.new_folder = None;
                    self.reload_shelf();
                }
            }
            ui.add_space(6.0);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            // What the list is showing: the shelf, narrowed to a folder.
            let showing: Vec<library::Item> = self
                .shelf_items
                .iter()
                .filter(|i| match &self.shelf_folder {
                    Some(folder) => &i.folder == folder,
                    None => true,
                })
                .cloned()
                .collect();

            if showing.is_empty() {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| {
                    let note = match self.shelf {
                        Kind::Video => "저장한 영상이 없습니다. 브라우저에서 영상을 받으면 여기에 모입니다.",
                        Kind::Music => "받은 음악이 없습니다. 음악만 저장하면 여기에 모입니다.",
                    };
                    ui.label(RichText::new(note).size(12.0).color(MUTED));
                });
                return;
            }

            // Acted on after the list is drawn: a row cannot delete or move
            // itself while the list it is in is being walked.
            // Let go anywhere and nothing is being carried any more, whether or
            // not the pointer was still over the row it was picked up from.
            if !ui.input(|i| i.pointer.any_down()) {
                self.holding = None;
            }

            let mut open = None;
            let mut remove = None;
            let mut rename: Option<usize> = None;
            let mut move_to: Option<(usize, String)> = dropped_on.take();

            // Renaming happens in place, at the top of the list: a box that
            // appears where the thing being renamed is, rather than a window
            // over it.
            if let Some((path, text)) = &mut self.renaming {
                let mut done = false;
                let mut cancelled = false;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("이름").size(11.0).color(MUTED));
                    let box_ = ui.add(egui::TextEdit::singleline(text).desired_width(320.0));
                    box_.request_focus();
                    if box_.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        done = true;
                    }
                    if ui.button("바꾸기").clicked() {
                        done = true;
                    }
                    if ui.button("취소").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        cancelled = true;
                    }
                });
                ui.separator();
                if done {
                    let wanted = text.clone();
                    let target = path.clone();
                    if let Some(item) = self.shelf_items.iter().find(|i| i.path == target) {
                        if !library::rename(item, &wanted) {
                            self.set_status(StatusKind::Bad, "이름을 바꾸지 못했습니다");
                        }
                    }
                    self.renaming = None;
                    self.shelf_stale = true;
                } else if cancelled {
                    self.renaming = None;
                }
            }

            egui::ScrollArea::vertical().show(ui, |ui| {
                for (index, item) in showing.iter().enumerate() {
                    // Held down, then moved: a file is picked up deliberately.
                    // Dragging the moment the pointer crosses a row turns every
                    // stray flick of the hand into a file put somewhere else.
                    let armed = matches!(
                        self.holding,
                        Some((held, since)) if held == index && since.elapsed().as_millis() > 260
                    );

                    let draw = |ui: &mut Ui| {
                        // Nothing in a row is text to select: dragging across a
                        // title used to highlight it like a document, which both
                        // looked broken and fought the drag that files it.
                        ui.style_mut().interaction.selectable_labels = false;
                        ui.horizontal(|ui| {
                            // One line: what it is, then how big and how old —
                            // read across, not down.
                            let mut detail = vec![library::human(item.bytes)];
                            let age = library::age(item.saved_at);
                            if !age.is_empty() {
                                detail.push(age);
                            }
                            ui.label(RichText::new(shorten(&item.title, 46)).size(13.0));
                            ui.label(
                                RichText::new(format!("  {}", detail.join("  ·  ")))
                                    .size(10.0)
                                    .color(MUTED),
                            );
                            // The play mark at the far end, where the eye lands
                            // after reading the line. Its own rect is taken from
                            // the right of the row and interacted with directly:
                            // asking a nested layout for it left the mark drawn
                            // in one place and answering the pointer in another,
                            // so it only lit in one corner.
                            let room = ui.available_rect_before_wrap();
                            let side = 30.0;
                            let rect = egui::Rect::from_min_size(
                                egui::pos2(room.right() - side, room.center().y - side / 2.0),
                                egui::vec2(side, side),
                            );
                            if play_mark(ui, rect).clicked() {
                                open = Some(index);
                            }
                        });
                    };

                    let row = if armed {
                        ui.dnd_drag_source(egui::Id::new(("lib-row", index)), index, draw).response
                    } else {
                        ui.scope(draw).response.interact(egui::Sense::click_and_drag())
                    };

                    // Held long enough and it becomes something to carry; let go
                    // and it is a row again.
                    if row.is_pointer_button_down_on() {
                        if !matches!(self.holding, Some((held, _)) if held == index) {
                            self.holding = Some((index, Instant::now()));
                        }
                    } else if matches!(self.holding, Some((held, _)) if held == index) && !armed {
                        self.holding = None;
                    }
                    // A hand while something is actually held, and nothing the
                    // rest of the time. The "grabbing" cursor is drawn on
                    // Windows as a four-way cross, which reads as "resize" and
                    // is what was showing over the whole list; the pointing hand
                    // is the one that means "carrying this".
                    if armed && ui.input(|i| i.pointer.any_down()) {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }

                    // Deleting is not a button on every row — a list of them is a
                    // row of ways to lose something. It is where a second press
                    // puts it: under the right button.
                    row.context_menu(|ui| {
                        if ui.button("이름 바꾸기").clicked() {
                            rename = Some(index);
                            ui.close();
                        }
                        if !item.folder.is_empty() && ui.button("폴더에서 빼기").clicked() {
                            move_to = Some((index, String::new()));
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("삭제").clicked() {
                            remove = Some(index);
                            ui.close();
                        }
                    });
                    if row.double_clicked() {
                        open = Some(index);
                    }
                    ui.separator();
                }
            });

            if let Some(index) = open {
                if !library::open(&showing[index].path) {
                    self.set_status(StatusKind::Bad, "이 파일을 여는 프로그램이 없습니다");
                }
            }
            if let Some(index) = rename {
                let item = &showing[index];
                self.renaming = Some((item.path.clone(), item.title.clone()));
            }
            if let Some((index, folder)) = move_to {
                if !library::move_to(&showing[index], &folder) {
                    self.set_status(StatusKind::Bad, "옮기지 못했습니다");
                }
                self.reload_shelf();
            }
            if let Some(index) = remove {
                if !library::delete(&showing[index]) {
                    self.set_status(StatusKind::Bad, "삭제하지 못했습니다");
                }
                self.reload_shelf();
            }
        });
    }

    fn settings(&mut self, ui: &mut Ui) {
        egui::Panel::top("settings-header").show(ui, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("← 돌아가기").clicked() {
                    self.view = View::Home;
                }
                ui.label(RichText::new("설정").color(MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (label, colour) = match self.core.status_kind {
                        StatusKind::Good => ("동작 중", GOOD),
                        StatusKind::Warn => ("주의", WARN),
                        StatusKind::Bad => ("오류", BAD),
                        StatusKind::Idle => ("정지", MUTED),
                    };
                    ui.label(RichText::new(label).color(colour).small());
                });
            });
            if !self.core.status.is_empty() && self.core.status_kind != StatusKind::Idle {
                let colour = match self.core.status_kind {
                    StatusKind::Bad => BAD,
                    StatusKind::Warn => WARN,
                    _ => MUTED,
                };
                ui.label(RichText::new(&self.core.status).color(colour).small());
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                for (tab, label) in Tab::ALL {
                    ui.selectable_value(&mut self.tab, tab, label);
                }
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                Tab::Status => self.status_tab(ui),
                Tab::Strategy => self.strategy_tab(ui),
                Tab::Domains => self.domains_tab(ui),
                Tab::Dns => self.dns_tab(ui),
                Tab::Activity => self.activity_tab(ui),
            });
        });
    }
}

impl ShardApp {
    fn status_tab(&mut self, ui: &mut Ui) {
        let stats = self.shared.stats.snapshot();
        ui.add_space(6.0);
        section(ui, "처리량", |ui| {
            counters(ui, &stats);
        });

        ui.add_space(10.0);
        section(ui, "비용", |ui| {
            let cfg = self.shared.config.read();
            let extra = cfg.strategy.extra_packets();
            ui.label(format!("새 연결당 추가 패킷: {extra}개"));
            ui.label(
                RichText::new(
                    "경로가 바뀌지 않으므로 대역폭 손실은 없습니다. 비용은 연결 시작 시의 \
                     패킷 몇 개와, 분할에 예민한 사이트가 깨질 수 있다는 위험뿐입니다.",
                )
                .color(MUTED)
                .small(),
            );
        });

        ui.add_space(10.0);
        let mut changed = false;

        section(ui, "영상 받기", |ui| {
            ui.label(
                RichText::new(
                    "브라우저 창에서 영상을 저장할 때 쓰는 기준입니다. 화질은 받을 때 고르고,                      음악만 받으려면 목록 맨 위의 '음악만 저장'을 고르면 됩니다. 소리는 매번                      고르기 번거로우니 음질만 여기서 정해 둡니다.",
                )
                .color(MUTED)
                .small(),
            );
            ui.add_space(6.0);

            let mut cfg = self.shared.config.write();
            ui.horizontal(|ui| {
                ui.label("음질");
                for option in crate::config::AudioQuality::ALL {
                    changed |= ui
                        .selectable_value(&mut cfg.download.audio, *option, option.label())
                        .changed();
                }
            });
            ui.label(
                RichText::new(
                    "최상은 10분 영상에 약 10 MB, 보통은 약 5 MB입니다. 영상이 보통                      그 열 배가 넘으니 최상을 권합니다.",
                )
                .color(MUTED)
                .small(),
            );

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("음성 언어");
                let mut language = cfg.download.audio_language.clone();
                let response = egui::ComboBox::from_id_salt("audio-language")
                    .selected_text(match language.as_str() {
                        "" => "영상 기본값".to_string(),
                        code => LANGUAGES
                            .iter()
                            .find(|(c, _)| *c == code)
                            .map(|(_, name)| name.to_string())
                            .unwrap_or_else(|| code.to_string()),
                    })
                    .show_ui(ui, |ui| {
                        let mut picked = false;
                        picked |= ui.selectable_value(&mut language, String::new(), "영상 기본값").changed();
                        for (code, name) in LANGUAGES {
                            picked |= ui
                                .selectable_value(&mut language, (*code).to_string(), *name)
                                .changed();
                        }
                        picked
                    });
                if response.inner.unwrap_or(false) {
                    cfg.download.audio_language = language;
                    changed = true;
                }
            });
            ui.label(
                RichText::new(
                    "더빙이 있는 영상에서만 의미가 있습니다. 고른 언어가 없으면 영상이                      기본으로 트는 트랙을 씁니다.",
                )
                .color(MUTED)
                .small(),
            );
        });

        ui.add_space(10.0);
        section(ui, "자동 학습", |ui| {
            ui.label(
                RichText::new(
                    "켜 두면 그것으로 끝입니다. 기본 전략이 통하지 않는 사이트를 스스로 감지해 \
                     백그라운드에서 전략을 찾아내고, 그 도메인 전용으로 저장합니다.",
                )
                .color(MUTED)
                .small(),
            );
            let mut cfg = self.shared.config.write();
            changed |= ui.checkbox(&mut cfg.auto_learn, "차단을 감지하면 알아서 학습").changed();
            ui.add_enabled_ui(cfg.auto_learn, |ui| {
                changed |= ui
                    .checkbox(&mut cfg.detect_silent_drops, "무응답도 차단으로 간주")
                    .on_hover_text(
                        "리셋을 주입하는 망은 감지가 공짜지만, 조용히 버리는 망은 오지 않는 응답을 \
                         기다려야 알 수 있습니다. 인바운드 데이터 패킷을 봐야 해서 바쁜 회선에서 CPU가 몇 % 늘어납니다.",
                    )
                    .changed();
                changed |= ui
                    .add(egui::Slider::new(&mut cfg.auto_learn_threshold, 1..=5).text("탐색 시작 실패 횟수"))
                    .on_hover_text("한 번의 리셋은 서버가 끊은 것일 수 있습니다. 반복되면 차단입니다.")
                    .changed();
                changed |= ui
                    .add(egui::Slider::new(&mut cfg.auto_learn_cooldown_min, 5..=240).text("재탐색 간격 (분)"))
                    .changed();
            });
            drop(cfg);

            let suspects = self.shared.suspect_count();
            if suspects > 0 {
                ui.label(RichText::new(format!("의심 중인 호스트 {suspects}개")).color(WARN).small());
            }
            ui.label(
                RichText::new(format!(
                    "차단 감지 {} · 탐색 {} · 학습 성공 {}",
                    stats.blocks_detected, stats.probes_run, stats.strategies_learned
                ))
                .color(MUTED)
                .small(),
            );
        });
        if changed {
            self.save_config();
        }

        ui.add_space(10.0);
        section(ui, "차단 검사", |ui| {
            ui.label(
                RichText::new(
                    "접속용이 아니라 진단용입니다. 여기에 넣은 도메인이 실제로 차단되는지 확인하고, \
                     통하는 전략을 찾아 저장합니다. 접속은 브라우저에서 평소처럼 하세요.",
                )
                .color(MUTED)
                .small(),
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.probe_host)
                        .hint_text("example.com")
                        .desired_width(260.0),
                );
                let busy = self.probe.as_ref().is_some_and(|p| !p.finished);
                let can_run = !busy && !self.probe_host.trim().is_empty() && self.running();
                if ui.add_enabled(can_run, egui::Button::new("탐색 시작")).clicked() {
                    // Accept a pasted URL, not just a bare hostname.
                    let host = crate::config::normalise_host(&self.probe_host);
                    let rx = prober::spawn(self.shared.clone(), host.clone());
                    self.probe = Some(ProbeRun { host, rx, lines: Vec::new(), finished: false });
                }
                if !self.running() {
                    ui.label(RichText::new("엔진이 꺼져 있으면 탐색할 수 없습니다").color(WARN).small());
                }
            });

            if let Some(probe) = &self.probe {
                ui.add_space(4.0);
                ui.label(RichText::new(format!("대상: {}", probe.host)).color(MUTED).small());
                for (line, ok) in &probe.lines {
                    let colour = match ok {
                        Some(true) => GOOD,
                        Some(false) => BAD,
                        None => MUTED,
                    };
                    ui.label(RichText::new(line).color(colour).small());
                }
            }
        });
    }

    fn strategy_tab(&mut self, ui: &mut Ui) {
        let mut cfg = self.shared.config.write();
        let before = cfg.strategy.clone();
        let s = &mut cfg.strategy;
        ui.add_space(6.0);

        section(ui, "우회 방식", |ui| {
            picker(ui, "방식", &mut s.desync, Desync::ALL, Desync::label)
                .on_hover_text("디코이는 검열 장비만 먹는 가짜 패킷, 분할은 호스트명을 조각내는 방식입니다.");
            ui.add_enabled_ui(s.desync.uses_fake(), |ui| {
                picker(ui, "디코이 무력화", &mut s.fooling, Fooling::ALL, Fooling::label).on_hover_text(
                    "TTL: 거리 계산이 맞아야 하지만 가장 확실합니다.\n\
                     체크섬: 거리와 무관하지만 NIC 오프로드가 되돌릴 수 있습니다.\n\
                     시퀀스: 상태 추적형 DPI에는 통하지 않습니다.",
                );
                ui.add(egui::Slider::new(&mut s.fake_repeats, 1..=5).text("디코이 반복"))
                    .on_hover_text("검열 장비가 모든 패킷을 보지 않고 샘플링만 한다면 반복이 필요합니다.");
            });
        });

        ui.add_space(10.0);
        section(ui, "분할", |ui| {
            ui.add_enabled_ui(s.desync.splits(), |ui| {
                picker(ui, "분할 위치", &mut s.split_at, SplitAt::ALL, SplitAt::label)
                    .on_hover_text("호스트명 중앙이 기본값입니다. 어느 조각에도 전체 문자열이 남지 않습니다.");
                if s.split_at == SplitAt::Fixed {
                    ui.add(egui::Slider::new(&mut s.fixed_split_offset, 1..=64).text("고정 오프셋"));
                }
                ui.add(egui::Slider::new(&mut s.extra_splits, 0..=4).text("추가 분할"))
                    .on_hover_text("조각이 많을수록 재조립형 DPI에 강해지지만 연결마다 패킷이 늘어납니다.");
            });
        });

        ui.add_space(10.0);
        section(ui, "TTL", |ui| {
            ui.checkbox(&mut s.auto_ttl, "홉 수 자동 측정")
                .on_hover_text("SYN-ACK의 TTL로 서버까지의 거리를 재서, 디코이가 서버 직전에 소멸하도록 맞춥니다.");
            if s.auto_ttl {
                ui.add(egui::Slider::new(&mut s.auto_ttl_delta, 1..=5).text("서버 앞 여유 홉"));
            } else {
                ui.add(egui::Slider::new(&mut s.fake_ttl, 1..=32).text("고정 TTL"));
            }
            ui.text_edit_singleline(&mut s.decoy_host);
            ui.label(RichText::new("디코이가 광고할 호스트명 — 차단되지 않은 이름이면 무엇이든 됩니다").color(MUTED).small());
        });

        ui.add_space(10.0);
        section(ui, "평문 HTTP", |ui| {
            ui.checkbox(&mut s.http_split, "Host 헤더 값 분할");
            ui.checkbox(&mut s.http_host_case, "Host → hOsT 변조")
                .on_hover_text("RFC 7230상 헤더 이름은 대소문자 무관이므로 서버는 영향받지 않습니다.");
            ui.checkbox(&mut s.http_host_space, "Host: 뒤 공백 추가");
        });

        ui.add_space(10.0);
        section(ui, "QUIC", |ui| {
            picker(ui, "처리", &mut s.quic, QuicMode::ALL, QuicMode::label).on_hover_text(
                "QUIC은 ClientHello가 암호화되어 호스트명을 읽을 수 없습니다.\n\
                 차단하면 브라우저가 TCP로 폴백해 위 전략이 적용됩니다.",
            );
        });

        ui.add_space(10.0);
        let extra = s.extra_packets();
        ui.label(
            RichText::new(format!("현재 설정 비용: 새 연결당 추가 패킷 {extra}개, 대역폭 손실 없음"))
                .color(if extra > 5 { WARN } else { MUTED }),
        );

        let changed = *s != before;
        drop(cfg);
        if changed {
            self.save_config();
        }
    }

    fn domains_tab(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        let mut changed = false;
        let mut cfg = self.shared.config.write();

        section(ui, "적용 범위", |ui| {
            for scope in [Scope::All, Scope::Listed] {
                if ui.radio_value(&mut cfg.scope, scope, scope.label()).changed() {
                    changed = true;
                }
            }
            ui.label(
                RichText::new(
                    "전체는 관리할 목록이 없어 편하지만 분할에 예민한 사이트가 깨질 수 있습니다. \
                     그럴 때 아래 예외 목록에 넣으세요.",
                )
                .color(MUTED)
                .small(),
            );
        });

        ui.add_space(10.0);
        section(ui, "대상 도메인", |ui| {
            changed |= domain_list(ui, &mut cfg.domains, &mut self.new_domain, "지정 범위일 때만 사용됩니다");
        });

        ui.add_space(10.0);
        section(ui, "예외 도메인", |ui| {
            changed |= domain_list(ui, &mut cfg.exclude, &mut self.new_exclude, "범위와 무관하게 항상 통과시킵니다");
        });

        ui.add_space(10.0);
        section(ui, "도메인별 전략", |ui| {
            if cfg.overrides.is_empty() {
                ui.label(RichText::new("아직 없습니다. 자동 탐색이 성공하면 여기에 저장됩니다.").color(MUTED).small());
            }
            let mut remove = None;
            for (pattern, strategy) in cfg.overrides.iter() {
                ui.horizontal(|ui| {
                    if ui.small_button("삭제").clicked() {
                        remove = Some(pattern.clone());
                    }
                    ui.label(RichText::new(pattern).strong());
                    ui.label(
                        RichText::new(format!("{} · {}", strategy.desync.label(), strategy.fooling.label()))
                            .color(MUTED)
                            .small(),
                    );
                });
            }
            if let Some(pattern) = remove {
                cfg.overrides.remove(&pattern);
                changed = true;
            }
        });

        drop(cfg);
        if changed {
            self.save_config();
        }
    }

    fn dns_tab(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        let mut changed = false;
        let mut restart = false;
        let mut cfg = self.shared.config.write();

        section(ui, "암호화 DNS", |ui| {
            ui.label(
                RichText::new(
                    "핸드셰이크에서 호스트명을 못 읽어도 그 직전의 평문 DNS 조회로 다 드러납니다. \
                     이 채널을 같이 닫아야 의미가 있습니다.",
                )
                .color(MUTED)
                .small(),
            );
            changed |= ui.checkbox(&mut cfg.doh.enabled, "DoH 포워더 사용").changed();
            changed |= ui
                .checkbox(&mut cfg.doh.set_system_dns, "시스템 DNS를 포워더로 변경 (종료 시 복구)")
                .changed();
            ui.horizontal(|ui| {
                ui.label("수신 주소");
                changed |= ui.text_edit_singleline(&mut cfg.doh.listen).changed();
            });
        });

        ui.add_space(10.0);
        section(ui, "업스트림", |ui| {
            ui.label(RichText::new("URL과 같은 순서의 부트스트랩 IP가 짝을 이룹니다").color(MUTED).small());
            let mut remove = None;
            for index in 0..cfg.doh.upstreams.len() {
                ui.horizontal(|ui| {
                    if ui.small_button("삭제").clicked() {
                        remove = Some(index);
                    }
                    changed |= ui
                        .add(egui::TextEdit::singleline(&mut cfg.doh.upstreams[index]).desired_width(300.0))
                        .changed();
                    if let Some(bootstrap) = cfg.doh.bootstrap.get_mut(index) {
                        changed |= ui
                            .add(egui::TextEdit::singleline(bootstrap).desired_width(120.0))
                            .changed();
                    }
                });
            }
            if let Some(index) = remove {
                cfg.doh.upstreams.remove(index);
                if index < cfg.doh.bootstrap.len() {
                    cfg.doh.bootstrap.remove(index);
                }
                changed = true;
            }
            if ui.button("추가").clicked() {
                cfg.doh.upstreams.push(String::new());
                cfg.doh.bootstrap.push(String::new());
                changed = true;
            }
        });

        ui.add_space(10.0);
        section(ui, "엔진", |ui| {
            changed |= ui.checkbox(&mut cfg.start_engine_on_launch, "실행 시 자동 시작").changed();
            let workers = ui.add(egui::Slider::new(&mut cfg.worker_threads, 1..=8).text("패킷 워커"));
            if workers.changed() {
                changed = true;
                restart = true;
            }
            ui.horizontal(|ui| {
                ui.label("전역 단축키");
                changed |= ui.text_edit_singleline(&mut cfg.hotkey).changed();
                ui.label(RichText::new("변경은 재시작 후 적용").color(MUTED).small());
            });
        });

        ui.add_space(10.0);
        section(ui, "초기화", |ui| {
            ui.label(
                RichText::new(
                    "우회는 연결이 맺힐 때마다 그 자리에서 일어나고 아무것도 남기지 않습니다. \
                     디스크에 남는 것은 학습한 도메인별 전략과 이 설정뿐입니다.",
                )
                .color(MUTED)
                .small(),
            );
            ui.horizontal(|ui| {
                if ui.button("학습 결과만 지우기").clicked() {
                    cfg.overrides.clear();
                    changed = true;
                }
                if ui.button("전체 초기화").clicked() {
                    let hotkey = cfg.hotkey.clone();
                    *cfg = Config::default();
                    // Rebinding a hotkey needs a restart, so keep the live one.
                    cfg.hotkey = hotkey;
                    changed = true;
                    restart = true;
                }
            });
        });

        drop(cfg);
        if changed {
            self.save_config();
        }
        if restart {
            self.restart_if_running();
        }
    }

    fn activity_tab(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        let events = self.shared.recent_events();
        if events.is_empty() {
            ui.label(RichText::new("아직 처리한 연결이 없습니다.").color(MUTED));
            return;
        }
        let now = Instant::now();
        egui::Grid::new("activity").num_columns(3).striped(true).show(ui, |ui| {
            for event in events.iter().take(120) {
                let age = now.saturating_duration_since(event.at).as_secs();
                ui.label(RichText::new(format!("{age}초 전")).color(MUTED).small());
                ui.label(RichText::new(&event.host).strong());
                ui.label(RichText::new(&event.action).color(MUTED).small());
                ui.end_row();
            }
        });
    }
}

// --- small building blocks -------------------------------------------------

fn section(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) {
    ui.label(RichText::new(title).strong().color(ACCENT));
    ui.add_space(2.0);
    egui::Frame::group(ui.style()).show(ui, body);
}

/// Radio-style picker over a fixed set of enum values.
fn picker<T: PartialEq + Copy>(
    ui: &mut Ui,
    label: &str,
    value: &mut T,
    options: &[T],
    name: fn(T) -> &'static str,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.label(label);
        for &option in options {
            ui.selectable_value(value, option, name(option));
        }
    })
    .response
}

fn domain_list(ui: &mut Ui, list: &mut Vec<String>, draft: &mut String, hint: &str) -> bool {
    let mut changed = false;
    ui.label(RichText::new(hint).color(MUTED).small());
    ui.horizontal(|ui| {
        let entry = ui.add(
            egui::TextEdit::singleline(draft)
                .hint_text("example.com · *.example.com · =example.com")
                .desired_width(280.0),
        );
        let submitted = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.button("추가").clicked() || submitted) && !draft.trim().is_empty() {
            // Patterns keep their `*.` / `=` prefix; everything else is reduced
            // from whatever URL the user pasted.
            let entry = match draft.trim() {
                p if p.starts_with('=') => format!("={}", crate::config::normalise_host(&p[1..])),
                p if p.starts_with("*.") => format!("*.{}", crate::config::normalise_host(&p[2..])),
                p => crate::config::normalise_host(p),
            };
            list.push(entry);
            draft.clear();
            changed = true;
        }
    });
    let mut remove = None;
    for (index, pattern) in list.iter().enumerate() {
        ui.horizontal(|ui| {
            if ui.small_button("삭제").clicked() {
                remove = Some(index);
            }
            ui.label(pattern);
        });
    }
    if let Some(index) = remove {
        list.remove(index);
        changed = true;
    }
    changed
}

fn counters(ui: &mut Ui, stats: &StatsSnapshot) {
    egui::Grid::new("counters").num_columns(4).spacing([24.0, 4.0]).show(ui, |ui| {
        let rows = [
            ("검사한 패킷", stats.packets_seen),
            ("통과", stats.passed_through),
            ("TLS 처리", stats.tls_handled),
            ("HTTP 처리", stats.http_handled),
            ("디코이 전송", stats.decoys_sent),
            ("조각 전송", stats.fragments_sent),
            ("QUIC 차단", stats.quic_dropped),
            ("차단 감지", stats.blocks_detected),
            ("학습한 전략", stats.strategies_learned),
            ("해석 실패", stats.tls_unparsed),
            ("오류", stats.errors),
        ];
        for (index, (label, value)) in rows.iter().enumerate() {
            ui.label(RichText::new(*label).color(MUTED).small());
            let alarming = matches!(*label, "오류" | "해석 실패") && *value > 0;
            let colour = if alarming { BAD } else { ui.visuals().text_color() };
            ui.label(RichText::new(value.to_string()).color(colour).strong());
            if index % 2 == 1 {
                ui.end_row();
            }
        }
    });
}

fn build_menu() -> anyhow::Result<(Menu, TrayMenu)> {
    let menu = Menu::new();
    let toggle = CheckMenuItem::new("우회 사용", true, false, None);
    let open = MenuItem::new("설정 열기", true, None);
    let quit = MenuItem::new("종료", true, None);
    menu.append(&toggle)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&open)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit)?;
    Ok((menu, TrayMenu { toggle, open, quit }))
}

/// Register the global toggle key. A failure here is not fatal — the tray and
/// window still work — so it degrades to a warning.
fn install_hotkey(
    spec: &str,
    ctx: &egui::Context,
) -> (Option<GlobalHotKeyManager>, Receiver<GlobalHotKeyEvent>) {
    let (tx, rx) = crossbeam_channel::unbounded();
    let repaint = ctx.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state == global_hotkey::HotKeyState::Pressed {
            let _ = tx.send(event);
            repaint.request_repaint();
        }
    }));

    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("global hotkeys unavailable: {e}");
            return (None, rx);
        }
    };
    match spec.parse::<global_hotkey::hotkey::HotKey>() {
        Ok(hotkey) => {
            if let Err(e) = manager.register(hotkey) {
                tracing::warn!("could not register hotkey {spec}: {e}");
            }
        }
        Err(e) => tracing::warn!("hotkey {spec} is not valid: {e}"),
    }
    (Some(manager), rx)
}

pub fn run() -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Shard")
            // The same mark the tray draws, so the taskbar and the tray agree.
            .with_icon({
                let art = icon::shard_at(64, true);
                egui::IconData { rgba: art.pixels, width: art.width, height: art.height }
            })
            .with_inner_size([500.0, 620.0])
            .with_min_inner_size([440.0, 540.0])
            // Start in the tray; the window appears on click.
            .with_visible(false),
        ..Default::default()
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(|cc| Ok(Box::new(ShardApp::new(cc)?))),
    )
    .map_err(|e| anyhow::anyhow!("UI를 시작할 수 없습니다: {e}"))
}
