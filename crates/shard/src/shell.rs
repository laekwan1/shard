//! The one window.
//!
//! Shard used to be two: a settings window drawn with a UI toolkit, and a
//! separate browser window with a WebView2 inside it. They could not become one
//! by putting the toolkit over the web view — a child window cannot be drawn
//! over, which is why the browser was its own window in the first place.
//!
//! So the arrangement is the other way round: this window's own contents are a
//! web view, and the browsing tabs are further web views laid out underneath the
//! strip at the top. Rust keeps what only Rust can — the engine, the window
//! itself, the files — and the shell page asks for those over one message
//! channel. The chip at the left of the strip is always the way home.

use anyhow::{anyhow, Result};
use std::cell::RefCell;
use std::rc::Rc;
use wry::http;
use wry::raw_window_handle;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows_sys::Win32::Graphics::Gdi::CreateSolidBrush;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// The colour behind the page while it is still coming up, so the window does
/// not flash white on the way in.
const SURFACE: u32 = 0x0010_0e0e;

/// The timer that keeps the loop turning while nothing is being pressed.
const TICK_TIMER: usize = 1;

/// How wide the invisible grip around the window is, in pixels.
const RESIZE_EDGE: i32 = 4;

/// How tall the title strip is — a Windows caption's own height, since that is
/// what it stands in for. The page draws it; this is what the layout code has to
/// agree with, so it is stated once and read from both sides.
pub const BAR: i32 = 32;

/// How much of the window the page keeps while a site is being looked at: the
/// title strip with its tabs, and the address row under it. Everything below is
/// the page being browsed, which is a child web view of its own.
pub const CHROME: i32 = BAR + 46;

/// The size the window opens at.
///
/// The settings window asked for 500×620 of *content* and Windows put a caption
/// on top of that. Here the caption is part of the window, so the same content
/// means asking for that much again plus the strip — otherwise everything sits
/// a caption's height higher than it used to.
const WIDTH: i32 = 500;
const HEIGHT: i32 = 620 + BAR;

/// The smallest it may be dragged to, the same floor the settings window had.
const MIN_WIDTH: i32 = 440;
const MIN_HEIGHT: i32 = 540 + BAR;

const CLASS: &[u16] = &[
    b'S' as u16, b'h' as u16, b'a' as u16, b'r' as u16, b'd' as u16, b'S' as u16, b'h' as u16,
    b'e' as u16, b'l' as u16, b'l' as u16, 0,
];

thread_local! {
    /// The shell's web view, for the window procedure to resize as the window
    /// changes. Held here rather than passed through `SetWindowLongPtr` because
    /// there is exactly one shell per process and the procedure is a plain
    /// function.
    static SHELL: RefCell<Option<Rc<wry::WebView>>> = const { RefCell::new(None) };
    /// The pages being browsed, in strip order, and which is in front. Kept
    /// beside the shell for the same reason: the window procedure has to lay
    /// them out as the window is dragged, and it is handed nothing but a handle.
    static TABS: RefCell<Vec<Rc<wry::WebView>>> = const { RefCell::new(Vec::new()) };
    static SHOWING: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    /// Set once the program has been asked to end, so the close button knows
    /// whether it is tidying the window away or shutting everything down.
    static QUITTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Put the shell and the page in front where they belong.
///
/// One place works this out, called both when something changes and while the
/// window is being dragged — the two used to disagree, and a resized window left
/// the site a strip short of the bottom.
///
/// Every measurement here is in real pixels. The window is told the screen's
/// true resolution (see [`create_window`]), so mixing "logical" and physical
/// units would put the page in the wrong place the moment a display is not at
/// 100%.
fn relayout(hwnd: HWND) {
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    unsafe { GetClientRect(hwnd, &mut rect) };
    let edge = if unsafe { IsZoomed(hwnd) } != 0 { 0 } else { RESIZE_EDGE };
    let width = (rect.right - rect.left - edge * 2).max(0) as u32;
    let height = (rect.bottom - rect.top - edge * 2).max(0) as u32;
    let showing = SHOWING.with(|cell| cell.get());
    let chrome = chrome_height(hwnd);

    // With a site in front the page keeps only its chrome — the tabs and the
    // address row — and the site fills what is left. With one of our own screens
    // up, the page is the whole window.
    let ours = if showing.is_some() { (chrome as u32).min(height) } else { height };
    SHELL.with(|cell| {
        if let Some(view) = cell.borrow().as_ref() {
            let _ = view.set_bounds(wry::Rect {
                position: wry::dpi::PhysicalPosition::new(edge, edge).into(),
                size: wry::dpi::PhysicalSize::new(width, ours).into(),
            });
        }
    });

    TABS.with(|cell| {
        for (at, view) in cell.borrow().iter().enumerate() {
            let front = showing == Some(at);
            // Hidden rather than closed: a tab left behind keeps its page, and
            // keeps playing, which is what coming back to it should find.
            let _ = view.set_visible(front);
            if front {
                let _ = view.set_bounds(wry::Rect {
                    position: wry::dpi::PhysicalPosition::new(edge, edge + chrome).into(),
                    size: wry::dpi::PhysicalSize::new(
                        width,
                        height.saturating_sub(chrome as u32),
                    )
                    .into(),
                });
            }
        }
    });
}

/// How tall the page's chrome is in real pixels on this window's display.
///
/// The page lays its strip out in its own units and the browser scales them by
/// the display's zoom; the site underneath is placed in real pixels. Without
/// this the two disagreed on a display at 125% and the page overlapped the site.
fn chrome_height(hwnd: HWND) -> i32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let dpi = if dpi == 0 { 96 } else { dpi };
    (CHROME * dpi as i32) / 96
}

/// What the page asked for. Parsed here rather than in every caller so the
/// message format stays one thing.
#[derive(Debug, Clone, PartialEq)]
pub enum Ask {
    /// The page is up and wants the state it missed.
    Ready,
    EngineToggle,
    /// Go to a screen. Browsing is Rust's business — it owns the tab views —
    /// so it comes through here rather than being drawn by the page.
    Nav(String),
    DownloadCancel(u64),
    /// Read a shelf: "video" or "music".
    LibraryList(String),
    LibraryFolder { id: u64, folder: String },
    LibraryRename { id: u64, title: String },
    LibraryDelete(u64),
    LibraryNewFolder { kind: String, name: String },
    /// Browsing: a tab to open, pick or drop, and where to point it.
    TabNew(String),
    TabPick(usize),
    TabShut(usize),
    Steer { what: String, url: String },
    /// A page answered the download panel — which quality was chosen.
    Chose(u64),
    /// What a page can give, for the list to be built from.
    PageOffer(String),
    /// The settings: read them, or put one back.
    SettingsRead,
    SettingsSet { key: String, value: String },
    /// The window itself: what a page cannot do to the frame around it.
    WindowDrag,
    WindowMinimise,
    WindowMaximise,
    WindowClose,
    /// Anything this build does not know, kept whole so it can be reported.
    Unknown(String),
}

/// Read one message from the page.
///
/// A hand-written reader rather than a parser for a handful of fields: the
/// shape is ours on both ends, and a dependency that turns three keys into a
/// struct is a dependency that has to be kept in step with the page anyway.
pub fn read_ask(body: &str) -> Ask {
    let op = field(body, "op").unwrap_or_default();
    match op.as_str() {
        "ready" => Ask::Ready,
        "engine.toggle" => Ask::EngineToggle,
        "nav" => Ask::Nav(field(body, "to").unwrap_or_else(|| "home".into())),
        "download.cancel" => Ask::DownloadCancel(number(body, "id").unwrap_or(0)),
        "library.list" => Ask::LibraryList(field(body, "kind").unwrap_or_else(|| "video".into())),
        "library.folder" => Ask::LibraryFolder {
            id: number(body, "id").unwrap_or(0),
            folder: field(body, "folder").unwrap_or_default(),
        },
        "library.rename" => Ask::LibraryRename {
            id: number(body, "id").unwrap_or(0),
            title: field(body, "title").unwrap_or_default(),
        },
        "library.delete" => Ask::LibraryDelete(number(body, "id").unwrap_or(0)),
        "library.newFolder" => Ask::LibraryNewFolder {
            kind: field(body, "kind").unwrap_or_else(|| "video".into()),
            name: field(body, "name").unwrap_or_default(),
        },
        "tab.new" => Ask::TabNew(field(body, "url").unwrap_or_default()),
        "tab.pick" => Ask::TabPick(number(body, "at").unwrap_or(0) as usize),
        "tab.shut" => Ask::TabShut(number(body, "at").unwrap_or(0) as usize),
        "steer" => Ask::Steer {
            what: field(body, "what").unwrap_or_else(|| "go".into()),
            url: field(body, "url").unwrap_or_default(),
        },
        "chose" => Ask::Chose(number(body, "itag").unwrap_or(0)),
        "settings.read" => Ask::SettingsRead,
        "settings.set" => Ask::SettingsSet {
            key: field(body, "key").unwrap_or_default(),
            value: field(body, "value").unwrap_or_default(),
        },
        "window.drag" => Ask::WindowDrag,
        "window.minimise" => Ask::WindowMinimise,
        "window.maximise" => Ask::WindowMaximise,
        "window.close" => Ask::WindowClose,
        _ => Ask::Unknown(body.to_string()),
    }
}

/// A string field out of the message, as the page meant it.
///
/// Through the escaping, not up to the first quote: a title with a quote in it
/// is written `\"` by the page, and stopping at the backslash cut the value
/// short — which is how a rename could quietly lose half a name.
fn field(body: &str, name: &str) -> Option<String> {
    Some(unescape(&raw_field(body, name)?))
}

/// A field taken whole, escapes and all — for values that carry newlines.
fn raw_field(body: &str, name: &str) -> Option<String> {
    let key = format!("\"{name}\":\"");
    let at = body.find(&key)? + key.len();
    let rest = &body[at..];
    // The closing quote is the first one not spoken for by a backslash.
    let mut out = String::new();
    let mut escaped = false;
    for c in rest.chars() {
        if escaped {
            out.push('\\');
            out.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '"' => return Some(out),
            c => out.push(c),
        }
    }
    None
}

/// One character written as `\uXXXX`, taking the second half of a pair with it.
fn coded(chars: &mut std::str::Chars<'_>) -> char {
    fn four(chars: &mut std::str::Chars<'_>) -> Option<u32> {
        let mut hex = String::with_capacity(4);
        for _ in 0..4 {
            hex.push(chars.next()?);
        }
        u32::from_str_radix(&hex, 16).ok()
    }
    let Some(first) = four(chars) else { return '\u{fffd}' };
    // Not the first half of a pair: it stands for itself.
    if !(0xd800..0xdc00).contains(&first) {
        return char::from_u32(first).unwrap_or('\u{fffd}');
    }
    // The other half follows as its own escape; only taken if it is really
    // there, so a lone half does not swallow the character after it.
    let mut rest = chars.clone();
    if rest.next() == Some('\\') && rest.next() == Some('u') {
        if let Some(low) = four(&mut rest) {
            if (0xdc00..0xe000).contains(&low) {
                *chars = rest;
                let point = 0x1_0000 + ((first - 0xd800) << 10) + (low - 0xdc00);
                return char::from_u32(point).unwrap_or('\u{fffd}');
            }
        }
    }
    '\u{fffd}'
}

/// The other side of [`escape`]: what the page wrote, as it meant it.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('/') => out.push('/'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            // A character written by its number, and the pair of them anything
            // past the basic range is written as. Decoding only the first half
            // of such a pair left a broken half-character behind.
            Some('u') => out.push(coded(&mut chars)),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// A number field, for the ids that name a download or a tab.
fn number(body: &str, name: &str) -> Option<u64> {
    let key = format!("\"{name}\":");
    let at = body.find(&key)? + key.len();
    let rest = body[at..].trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Text on its way into a script, with the characters that would end it early
/// taken out.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            // JavaScript ends a string literal at these two as surely as at a
            // newline, and a page title can carry them.
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            '\r' => {}
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// One of the shell's own files, by the path the page asks for.
///
/// Built into the executable rather than written beside it: the program is one
/// file, and a UI that can go missing between installs is a UI that will.
fn asset(path: &str) -> Option<(&'static [u8], &'static str)> {
    match path {
        "" | "/" | "/index.html" => Some((
            include_bytes!("../assets/ui/index.html"),
            "text/html; charset=utf-8",
        )),
        "/app.css" => Some((
            include_bytes!("../assets/ui/app.css"),
            "text/css; charset=utf-8",
        )),
        "/app.js" => Some((
            include_bytes!("../assets/ui/app.js"),
            "text/javascript; charset=utf-8",
        )),
        _ => None,
    }
}

/// Serve one of the shell's own files.
pub fn serve(uri: &str) -> (u16, &'static str, &'static [u8]) {
    match asset(&path_of(uri)) {
        Some((body, mime)) => (200, mime, body),
        None => (404, "text/plain; charset=utf-8", b"not found".as_slice()),
    }
}

/// The path out of an address, without the host or the query.
fn path_of(uri: &str) -> String {
    uri.split_once("://")
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_once('/').map(|(_, p)| p))
        .map(|p| format!("/{}", p.split('?').next().unwrap_or("")))
        .unwrap_or_else(|| "/".into())
}

// ---- the files the page is allowed to play ---------------------------------
//
// A saved file is reached by a number this run made up, never by its path. The
// page cannot ask for a file it was not handed, so there is no name to doctor
// and no `..` to climb out with, and a Korean title needs no escaping on the way
// through an address.

static MEDIA: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<u64, std::path::PathBuf>>,
> = std::sync::OnceLock::new();

fn media_table() -> &'static std::sync::Mutex<std::collections::HashMap<u64, std::path::PathBuf>> {
    MEDIA.get_or_init(Default::default)
}

/// Hand out the number this file will be asked for by.
pub fn register_media(path: &std::path::Path) -> u64 {
    let mut table = media_table().lock().unwrap_or_else(|e| e.into_inner());
    // Already known: the same file keeps the same number for the run, so a list
    // drawn twice does not make the page reload what it is already playing.
    if let Some((id, _)) = table.iter().find(|(_, kept)| kept.as_path() == path) {
        return *id;
    }
    let id = table.len() as u64 + 1;
    table.insert(id, path.to_path_buf());
    id
}

/// Follow a file that has been renamed or moved, keeping its number.
///
/// The number is what the player is holding; if the table still pointed at the
/// old name, tidying the shelf while something was playing would stop it dead
/// with nothing said.
pub fn remember_moved(from: &std::path::Path, to: &std::path::Path) {
    let mut table = media_table().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, kept)) = table.iter_mut().find(|(_, kept)| kept.as_path() == from) {
        *kept = to.to_path_buf();
    }
}

fn media_path(id: u64) -> Option<std::path::PathBuf> {
    media_table()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&id)
        .cloned()
}

/// What a file is, as the page's player needs to be told.
fn media_mime(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "m4a" => "audio/mp4",
        "opus" | "oga" => "audio/ogg",
        "mp3" => "audio/mpeg",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        _ => "application/octet-stream",
    }
}

/// How much of a file to hand over at once.
///
/// A slice rather than the whole thing: a film is gigabytes, the answer is held
/// in memory on the way out, and the player asks for the next piece as it needs
/// it. Small enough to stay responsive, large enough that a seek does not turn
/// into a hundred requests.
const SLICE: u64 = 2 * 1024 * 1024;

/// Read `Range: bytes=start-end`, in the forms a media player sends.
fn wanted_range(header: Option<&str>, len: u64) -> (u64, u64) {
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else {
        // No range asked for is the player's first look at the file: give it the
        // opening slice and let it come back for the rest.
        return (0, SLICE.min(len).saturating_sub(1));
    };
    // Only the first range of a list is answered; a player asking for several at
    // once takes a single one back happily, and multipart replies are a format
    // of their own for no gain here.
    let spec = spec.split(',').next().unwrap_or("").trim();
    let last = len.saturating_sub(1);
    let (from, to) = spec.split_once('-').unwrap_or((spec, ""));
    if from.trim().is_empty() {
        // `bytes=-500`: the last 500 bytes, which is how a player looks for the
        // index some containers keep at the end.
        let want = to.trim().parse::<u64>().unwrap_or(0).min(len);
        let start = len.saturating_sub(want.max(1));
        return (start, last.min(start + SLICE - 1));
    }
    let start = from.trim().parse::<u64>().unwrap_or(0).min(last);
    let end = to
        .trim()
        .parse::<u64>()
        .unwrap_or(u64::MAX)
        .min(last)
        .min(start + SLICE - 1);
    (start, end.max(start))
}

/// Answer one request from the shell page: its own files, or a slice of a saved
/// one.
pub fn respond(uri: &str, range: Option<&str>) -> http::Response<std::borrow::Cow<'static, [u8]>> {
    let path = path_of(uri);
    let build = http::Response::builder();

    if let Some(rest) = path.strip_prefix("/media/") {
        let id = rest.split('/').next().unwrap_or("").parse::<u64>().unwrap_or(0);
        let Some(file) = media_path(id) else { return not_found() };
        let Ok(mut handle) = std::fs::File::open(&file) else { return not_found() };
        let len = handle.metadata().map(|m| m.len()).unwrap_or(0);
        if len == 0 {
            return not_found();
        }
        let (start, end) = wanted_range(range, len);
        let mut body = vec![0u8; (end - start + 1) as usize];
        use std::io::{Read, Seek};
        if handle.seek(std::io::SeekFrom::Start(start)).is_err()
            || handle.read_exact(&mut body).is_err()
        {
            return not_found();
        }
        return build
            .status(206)
            .header("Content-Type", media_mime(&file))
            .header("Accept-Ranges", "bytes")
            .header("Content-Range", format!("bytes {start}-{end}/{len}"))
            .header("Content-Length", (end - start + 1).to_string())
            .header("Cache-Control", "no-store")
            .body(std::borrow::Cow::Owned(body))
            .unwrap_or_else(|_| not_found());
    }

    match asset(&path) {
        Some((body, mime)) => build
            .status(200)
            .header("Content-Type", mime)
            .header("Cache-Control", "no-store")
            .body(std::borrow::Cow::Borrowed(body))
            .unwrap_or_else(|_| not_found()),
        None => not_found(),
    }
}

fn not_found() -> http::Response<std::borrow::Cow<'static, [u8]>> {
    http::Response::builder()
        .status(404)
        .header("Content-Type", "text/plain; charset=utf-8")
        .body(std::borrow::Cow::Borrowed(b"not found".as_slice()))
        .expect("a 404 is always well formed")
}

/// Make the window and put the shell page in it.
///
/// `on_ask` is called on this thread for every message the page sends, for as
/// long as the window lives.
pub fn open(title: &str, on_ask: impl Fn(&Shell, Ask) + 'static) -> Result<Shell> {
    let hwnd = create_window(title)?;
    let host = Host(hwnd);

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let view = wry::WebViewBuilder::new()
        .with_asynchronous_custom_protocol("shard".into(), |_id, request, responder| {
            let uri = request.uri().to_string();
            let range = request
                .headers()
                .get("Range")
                .and_then(|value| value.to_str().ok())
                .map(|s| s.to_string());
            // The shell's own files are a handful of kilobytes already in the
            // executable: answering on the spot is faster than handing them to
            // a thread.
            if !path_of(&uri).starts_with("/media/") {
                responder.respond(respond(&uri, range.as_deref()));
                return;
            }
            // A slice of a film is megabytes off a disk, and this runs on the
            // thread that draws the window: reading it here stopped everything
            // — the scrubber, the buttons, the page — for as long as the read
            // took, which is exactly when the player is asking most often.
            std::thread::spawn(move || {
                responder.respond(respond(&uri, range.as_deref()));
            });
        })
        .with_url("shard://shard.localhost/index.html")
        .with_ipc_handler(move |request| {
            let _ = tx.send(request.into_body());
        })
        .with_background_color((0x0e, 0x0e, 0x10, 0xff))
        .build_as_child(&host)
        .map_err(|e| anyhow!("WebView2를 시작하지 못했습니다: {e}"))?;

    let view = Rc::new(view);
    SHELL.with(|cell| *cell.borrow_mut() = Some(view.clone()));
    let (to_pages, pages) = std::sync::mpsc::channel();
    let shell = Shell {
        hwnd,
        view,
        asks: rx,
        tabs: RefCell::new(Vec::new()),
        showing: std::cell::Cell::new(None),
        pages,
        to_pages,
        answer: RefCell::new(Some(Box::new(on_ask))),
    };
    shell.lay_out();
    unsafe { ShowWindow(hwnd, SW_SHOW) };
    // The page is served and the channel is open; the first messages arrive
    // once the loop starts turning.
    Ok(shell)
}

/// The tray icon, its menu, and the state the menu shows.
struct Tray {
    icon: uikit::tray::TrayIcon,
    events: uikit::tray::TrayEvents,
    toggle: uikit::tray::CheckMenuItem,
    open: uikit::tray::MenuItem,
    quit: uikit::tray::MenuItem,
}

impl Tray {
    fn build() -> Result<Self> {
        use uikit::tray::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
        let events = uikit::tray::watch();
        let menu = Menu::new();
        let toggle = CheckMenuItem::new("우회 켜기", true, false, None);
        let open = MenuItem::new("창 열기", true, None);
        let quit = MenuItem::new("종료", true, None);
        menu.append(&toggle)?;
        menu.append(&open)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&quit)?;
        let icon = uikit::tray::build("Shard — 우회 꺼짐", &uikit::icon::shard(false), menu)?;
        Ok(Self { icon, events, toggle, open, quit })
    }

    /// Show what the engine is doing, in the icon and the tooltip.
    fn follow(&self, core: &crate::core::EngineCore) {
        let running = core.running();
        let art = match (running, core.status_kind) {
            (true, crate::core::StatusKind::Warn) => uikit::icon::warn(true),
            (true, _) => uikit::icon::shard(true),
            (false, _) => uikit::icon::shard(false),
        };
        uikit::tray::set_icon(&self.icon, &art);
        let _ = self
            .icon
            .set_tooltip(Some(if running { "Shard — 우회 동작 중" } else { "Shard — 우회 꺼짐" }));
        self.toggle.set_checked(running);
    }
}

/// Open the shell, with the real engine behind it.
pub fn preview() -> Result<()> {
    use crate::core::EngineCore;
    use crate::engine::Shared;

    let config = crate::config::Config::load();
    let shared = Shared::new(config);
    let core = std::rc::Rc::new(RefCell::new(EngineCore::new(shared.clone())));
    let saving = std::rc::Rc::new(RefCell::new(crate::downloads::Downloads::new(shared)));

    // Honoured here as it is in the settled window: the setting is offered on
    // the settings screen, so a build that ignored it would be lying.
    if core.borrow().shared.config.read().start_engine_on_launch {
        core.borrow_mut().start();
    }

    // The switch in the notification area. It is what lets the window be put
    // away while the bypass keeps running — closing the window is tidying it
    // out of the way, not stopping the program.
    let tray = Tray::build()?;
    tray.follow(&core.borrow());

    let engine = core.clone();
    let jobs = saving.clone();
    let shell = open("Shard", move |shell, ask| match ask {
        Ask::Ready => {
            shell.say_engine(&engine.borrow());
            shell.say_downloads(&jobs.borrow());
            shell.say_tabs();
        }
        Ask::EngineToggle => {
            engine.borrow_mut().toggle();
            shell.say_engine(&engine.borrow());
        }
        // Going to the browser opens a tab the first time and comes back to
        // what was left the rest of the time; going anywhere else hides it,
        // which is what gives the shell the whole window again.
        Ask::Nav(to) => {
            if to == "browser" {
                if shell.tab_count() == 0 {
                    shell.open_tab("https://www.youtube.com/");
                } else {
                    shell.show_tab(Some(shell.tab_count() - 1));
                }
            } else {
                shell.show_tab(None);
            }
        }
        Ask::TabNew(url) => shell.open_tab(if url.is_empty() {
            "https://www.youtube.com/"
        } else {
            &url
        }),
        Ask::TabPick(at) => shell.show_tab(Some(at)),
        Ask::TabShut(at) => shell.close_tab(at),
        Ask::Steer { what, url } => shell.steer(&what, &url),

        // The list of what can be saved, and the row that was pressed on it.
        Ask::PageOffer(payload) => {
            let script = jobs.borrow_mut().qualities(&payload);
            shell.tell_page(&script);
        }
        Ask::Chose(itag) => {
            let script = jobs.borrow_mut().begin(itag as u32);
            shell.tell_page(&script);
            shell.say_downloads(&jobs.borrow());
        }
        Ask::DownloadCancel(id) => jobs.borrow().cancel(id),

        Ask::SettingsRead => {
            let json = crate::settings::as_json(&engine.borrow().shared.config.read());
            shell.tell(&json);
        }
        Ask::SettingsSet { key, value } => {
            let landed = {
                let core = engine.borrow();
                let mut cfg = core.shared.config.write();
                crate::settings::apply(&mut cfg, &key, &value)
            };
            if landed {
                // Written the moment it is changed: a setting that only survives
                // a tidy exit is a setting that goes missing after a crash.
                engine.borrow().save_config();
                // Some of them are read when the engine starts, so a change made
                // while it is running means nothing until it is started again.
                if crate::settings::needs_restart(&key) {
                    engine.borrow_mut().restart_if_running();
                }
            } else {
                tracing::warn!("settings: {key} did not apply");
            }
            // What was stored, not what was typed: a value out of range is
            // clamped, and the control has to show where it actually landed.
            let json = crate::settings::as_json(&engine.borrow().shared.config.read());
            shell.tell(&json);
        }

        Ask::LibraryList(kind) => shell.say_library(shelf_of(&kind)),
        Ask::LibraryFolder { id, folder } => {
            with_item(id, |item| {
                let was = item.path.clone();
                if crate::library::move_to(item, &folder) {
                    if let Some(name) = was.file_name() {
                        let mut moved = item.kind.folder();
                        if !folder.is_empty() {
                            moved = moved.join(crate::library::clean(&folder));
                        }
                        remember_moved(&was, &moved.join(name));
                    }
                }
            });
            shell.say_library(shelf_of_id(id));
        }
        Ask::LibraryRename { id, title } => {
            with_item(id, |item| {
                let was = item.path.clone();
                if crate::library::rename(item, &title) {
                    let clean = crate::library::clean(&title);
                    let moved = match was.extension().and_then(|e| e.to_str()) {
                        Some(extension) => was.with_file_name(format!("{clean}.{extension}")),
                        None => was.with_file_name(clean),
                    };
                    remember_moved(&was, &moved);
                }
            });
            shell.say_library(shelf_of_id(id));
        }
        Ask::LibraryDelete(id) => {
            with_item(id, |item| {
                crate::library::delete(item);
            });
            shell.say_library(shelf_of_id(id));
        }
        Ask::LibraryNewFolder { kind, name } => {
            let shelf = shelf_of(&kind);
            crate::library::add_folder(shelf, &name);
            shell.say_library(shelf);
        }
        other => tracing::info!("shell ask: {other:?}"),
    })?;

    // What the running downloads have to say, taken in on the same beat as the
    // window's own messages: they finish on their own threads, and the page
    // learns about it here.
    shell.run(|shell| {
        // The tray, first: it is how the window comes back once it has been put
        // away, so it has to be answered even while nothing else is happening.
        while let Ok(event) = tray.events.tray.try_recv() {
            if uikit::tray::is_activation(&event) {
                shell.show();
            }
        }
        while let Ok(event) = tray.events.menu.try_recv() {
            let id = event.id();
            if id == tray.toggle.id() {
                core.borrow_mut().toggle();
                shell.say_engine(&core.borrow());
                tray.follow(&core.borrow());
            } else if id == tray.open.id() {
                shell.show();
            } else if id == tray.quit.id() {
                shell.quit();
            }
        }

        let drained = saving.borrow_mut().drain();
        for (note, failed) in &drained.finished {
            shell.tell_page(&if *failed {
                crate::download::youtube::say_script(note, true)
            } else {
                crate::download::youtube::flash_script(note)
            });
        }
        if !drained.finished.is_empty() {
            shell.say_downloads(&saving.borrow());
        } else if !saving.borrow().list.is_empty() {
            shell.say_downloads(&saving.borrow());
        }
        if drained.saved {
            // A new file is on a shelf: if the library is being looked at, it
            // shows up without anything being pressed.
            shell.tell(r#"{"t":"saved"}"#);
        }
    });
    // The machine's own DNS goes back before the window does.
    core.borrow_mut().stop();
    Ok(())
}

/// A tab's label, taken from its address: the host, which is what a tab strip
/// has room for and what tells one site from another at a glance.
fn title_of(url: &str) -> String {
    url.split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or(url)
        .trim_start_matches("www.")
        .to_string()
}

/// Which shelf a name from the page means.
fn shelf_of(kind: &str) -> crate::library::Kind {
    if kind == "music" { crate::library::Kind::Music } else { crate::library::Kind::Video }
}

/// Which shelf the file behind a number is on, worked out from where it lives.
fn shelf_of_id(id: u64) -> crate::library::Kind {
    let music_root = crate::library::Kind::Music.folder();
    match media_path(id) {
        Some(path) if path.starts_with(&music_root) => crate::library::Kind::Music,
        _ => crate::library::Kind::Video,
    }
}

/// Do something to the saved file behind a number, if it is still there.
fn with_item(id: u64, act: impl FnOnce(&crate::library::Item)) {
    let Some(path) = media_path(id) else { return };
    let shelf = shelf_of_id(id);
    if let Some(item) = crate::library::items(shelf).into_iter().find(|i| i.path == path) {
        act(&item);
    }
}

/// The window, the shell page, and the sites being browsed inside it.
pub struct Shell {
    hwnd: HWND,
    view: Rc<wry::WebView>,
    asks: std::sync::mpsc::Receiver<String>,
    /// One web view per tab, in the order the strip shows them.
    tabs: RefCell<Vec<Tab>>,
    /// Which tab is in front, or none while a screen of ours is up.
    showing: std::cell::Cell<Option<usize>>,
    /// What the pages have to say — the same channel the separate window used,
    /// so the capture that rides on the page hooks arrives unchanged.
    pages: std::sync::mpsc::Receiver<crate::download::browser::Event>,
    to_pages: std::sync::mpsc::Sender<crate::download::browser::Event>,
    /// What to do with what the page asks for.
    ///
    /// Kept here rather than passed in at each turn of the loop: it was handed
    /// to `open` and dropped when `open` returned, and the loop's own per-tick
    /// closure quietly took its place — so every ask but the window buttons was
    /// thrown away and the program did almost nothing it was told.
    answer: RefCell<Option<Box<dyn Fn(&Shell, Ask)>>>,
}

/// One tab: the page, and what the strip needs to draw it.
pub struct Tab {
    view: Rc<wry::WebView>,
    pub title: String,
    pub url: String,
}

impl Shell {
    /// Run until the window is closed, answering the page as it asks.
    ///
    /// `tick` is called every turn — after the page's messages, and on the
    /// timer that keeps running while nothing is being pressed — for the work
    /// that has to happen whether or not anyone is typing.
    pub fn run(&self, tick: impl Fn(&Shell)) {
        let mut message = MSG {
            hwnd: std::ptr::null_mut(),
            message: 0,
            wParam: 0,
            lParam: 0,
            time: 0,
            pt: POINT { x: 0, y: 0 },
        };
        loop {
            let got = unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
            if got <= 0 {
                break;
            }
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            self.pump();
            tick(self);
        }
    }

    /// Deal with whatever the pages have said since last time.
    pub fn pump(&self) {
        // What the sites being browsed had to say first: an address that moved,
        // and the answers the download hooks send back.
        for event in self.page_events() {
            match event {
                crate::download::browser::Event::Navigated(url) => {
                    if let Some(at) = self.showing.get() {
                        if let Some(tab) = self.tabs.borrow_mut().get_mut(at) {
                            tab.title = title_of(&url);
                            tab.url = url;
                        }
                    }
                    self.say_tabs();
                }
                crate::download::browser::Event::Offer(payload) => self.from_page(&payload),
                crate::download::browser::Event::Closed => {}
            }
        }

        while let Ok(body) = self.asks.try_recv() {
            match read_ask(&body) {
                Ask::WindowDrag => self.drag(),
                Ask::WindowMinimise => unsafe {
                    ShowWindow(self.hwnd, SW_MINIMIZE);
                },
                Ask::WindowMaximise => self.maximise(),
                Ask::WindowClose => unsafe {
                    PostMessageW(self.hwnd, WM_CLOSE, 0, 0);
                },
                other => self.answer(other),
            }
        }
    }

    /// Hand one ask to whoever is answering them.
    fn answer(&self, ask: Ask) {
        // Written down as it arrives. The page and this side are two programs
        // talking, and when a screen does not open the first question is always
        // whether the ask ever got here.
        tracing::info!("ask: {ask:?}");
        // Taken out and put back, so answering can reach back into the shell —
        // opening a tab, sending the library — without the handler being held
        // borrowed while it runs.
        let held = self.answer.borrow_mut().take();
        if let Some(handler) = held {
            handler(self, ask);
            *self.answer.borrow_mut() = Some(handler);
        }
    }

    /// What a browsed page said, sorted by what kind of thing it is.
    ///
    /// One channel carries three quite different things: the frame's own
    /// heartbeat (the page reporting its title and colour a few times a second),
    /// a press of the download button, and the list of what it can offer. The
    /// heartbeat is by far the most common, and reading it as an offer — which
    /// is what matching on substrings did — put a page's own text through the
    /// download parser several times a second.
    fn from_page(&self, payload: &str) {
        if let Some(frame) = field(payload, "frame") {
            // The hooks put the value in `text`, whatever the report is about.
            let Some(text) = field(payload, "text") else { return };
            let Some(at) = self.showing.get() else { return };
            let changed = {
                let mut tabs = self.tabs.borrow_mut();
                match (frame.as_str(), tabs.get_mut(at)) {
                    ("title", Some(tab)) if !text.trim().is_empty() && tab.title != text => {
                        tab.title = text;
                        true
                    }
                    ("url", Some(tab)) if tab.url != text => {
                        tab.url = text;
                        true
                    }
                    // The page's own colour, which the strip does not follow.
                    _ => false,
                }
            };
            // Only when it actually moved. These arrive a few times a second per
            // page, and redrawing the strip for each was work done to say
            // nothing had changed.
            if changed {
                self.say_tabs();
            }
            return;
        }
        if payload.contains("\"ask\"") {
            self.tell_page(crate::download::youtube::ASK);
        } else if let Some(itag) = crate::downloads::chosen(payload) {
            self.answer(Ask::Chose(itag as u64));
        } else {
            self.answer(Ask::PageOffer(payload.to_string()));
        }
    }

    // ---- the sites being browsed -------------------------------------------

    /// Open a page in a tab of its own and bring it to the front.
    pub fn open_tab(&self, url: &str) {
        // The same three the separate window injected: the frame's own hooks,
        // the recorder that captures the SABR request, and the control that puts
        // the download button on the page. Without the last two a page never
        // offers anything and nothing can be saved.
        let startup = format!(
            "{}\n{}\n{}",
            crate::download::browser::PAGE_HOOKS,
            crate::download::youtube::RECORDER,
            crate::download::youtube::CONTROL,
        );
        let id = self.tabs.borrow().len() as u64 + 1;
        match crate::download::browser::new_view(self.hwnd, url, &startup, &self.to_pages, id) {
            Ok(view) => {
                let view = Rc::new(view);
                TABS.with(|cell| cell.borrow_mut().push(view.clone()));
                self.tabs.borrow_mut().push(Tab {
                    view,
                    title: title_of(url),
                    url: url.to_string(),
                });
                let at = self.tabs.borrow().len() - 1;
                self.show_tab(Some(at));
            }
            Err(e) => tracing::error!("could not open a tab: {e:#}"),
        }
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.borrow().len()
    }

    /// Bring one tab forward, or none of them when a screen of ours is up.
    ///
    /// The others are hidden rather than closed: coming back to a tab finds the
    /// page where it was left, still playing if it was playing.
    pub fn show_tab(&self, which: Option<usize>) {
        // Never past the end: a stale index from the page would otherwise show
        // nothing and leave the strip claiming a tab that is not there.
        let which = which.filter(|at| *at < self.tabs.borrow().len());
        self.showing.set(which);
        SHOWING.with(|cell| cell.set(which));
        self.lay_out();
        // Keys go where the eye is: to the page in front, or back to our own
        // screens when there is none.
        match which.and_then(|at| self.tabs.borrow().get(at).map(|t| t.view.clone())) {
            Some(view) => {
                let _ = view.focus();
            }
            None => {
                let _ = self.view.focus();
            }
        }
        self.say_tabs();
    }

    pub fn close_tab(&self, at: usize) {
        {
            let mut tabs = self.tabs.borrow_mut();
            if at >= tabs.len() {
                return;
            }
            tabs.remove(at);
        }
        TABS.with(|cell| {
            let mut views = cell.borrow_mut();
            if at < views.len() {
                views.remove(at);
            }
        });
        let left = self.tabs.borrow().len();
        // By position, not by length: closing a tab to the left of the one in
        // front shifts it down one, and closing the front one falls to whatever
        // took its place.
        let showing = match self.showing.get() {
            _ if left == 0 => None,
            Some(current) if current > at => Some(current - 1),
            Some(current) if current == at => Some(current.min(left - 1)),
            current => current,
        };
        self.show_tab(showing);
    }

    /// Tell the page which tabs there are and which is in front.
    pub fn say_tabs(&self) {
        let tabs = self.tabs.borrow();
        let rows: Vec<String> = tabs
            .iter()
            .map(|tab| {
                format!(
                    r#"{{"title":"{}","url":"{}"}}"#,
                    escape(&tab.title),
                    escape(&tab.url)
                )
            })
            .collect();
        self.tell(&format!(
            r#"{{"t":"tabs","list":[{}],"at":{}}}"#,
            rows.join(","),
            match self.showing.get() {
                Some(at) => at.to_string(),
                None => "null".to_string(),
            }
        ));
    }

    /// Do something to the tab in front: go somewhere, or step through history.
    pub fn steer(&self, what: &str, url: &str) {
        let view = {
            let tabs = self.tabs.borrow();
            self.showing.get().and_then(|at| tabs.get(at).map(|t| t.view.clone()))
        };
        let Some(view) = view else { return };
        let _ = match what {
            "go" => view.load_url(url),
            "back" => view.evaluate_script("history.back()"),
            "forward" => view.evaluate_script("history.forward()"),
            _ => view.evaluate_script("location.reload()"),
        };
        // What was steered is what should take the keyboard.
        let _ = view.focus();
    }

    /// Send a script to the page in front — how the download panel is drawn on
    /// whatever site is being watched.
    pub fn tell_page(&self, script: &str) {
        let tabs = self.tabs.borrow();
        if let Some(tab) = self.showing.get().and_then(|at| tabs.get(at)) {
            let _ = tab.view.evaluate_script(script);
        }
    }

    /// Whatever the pages have said since last time.
    pub fn page_events(&self) -> Vec<crate::download::browser::Event> {
        let mut out = Vec::new();
        while let Ok(event) = self.pages.try_recv() {
            out.push(event);
        }
        out
    }

    // ---- what the page is told ---------------------------------------------

    /// Send a shelf: its folders, and every file on it with the number the
    /// player will ask for it by.
    pub fn say_library(&self, kind: crate::library::Kind) {
        let items = crate::library::items(kind);
        let folders = crate::library::folders(kind);
        let rows: Vec<String> = items
            .iter()
            .map(|item| {
                format!(
                    r#"{{"id":{},"key":"{}","title":"{}","folder":"{}","size":"{}","age":"{}"}}"#,
                    register_media(&item.path),
                    // Something that means the same file next time the program
                    // runs: the number does not, and playback positions kept
                    // against it pointed at whatever took that number later.
                    durable_key(&item.path),
                    escape(&item.title),
                    escape(&item.folder),
                    escape(&crate::library::human(item.bytes)),
                    escape(&crate::library::age(item.saved_at)),
                )
            })
            .collect();
        let names: Vec<String> = folders.iter().map(|n| format!("\"{}\"", escape(n))).collect();
        self.tell(&format!(
            r#"{{"t":"library","kind":"{}","folders":[{}],"items":[{}]}}"#,
            if kind == crate::library::Kind::Music { "music" } else { "video" },
            names.join(","),
            rows.join(","),
        ));
    }

    /// Tell the page what is being fetched, so the home screen can show it.
    pub fn say_downloads(&self, downloads: &crate::downloads::Downloads) {
        let rows: Vec<String> = downloads
            .list
            .iter()
            .map(|job| {
                format!(
                    r#"{{"id":{},"title":"{}","fraction":{:.4}}}"#,
                    job.id,
                    escape(&job.title),
                    job.fraction()
                )
            })
            .collect();
        self.tell(&format!(r#"{{"t":"downloads","list":[{}]}}"#, rows.join(",")));
    }

    /// Tell the page how the engine stands, in the shape its home screen reads.
    pub fn say_engine(&self, core: &crate::core::EngineCore) {
        let (headline, kind) = core.headline();
        self.tell(&format!(
            r#"{{"t":"engine","running":{},"kind":"{}","headline":"{}","detail":"{}","note":"{}"}}"#,
            core.running(),
            kind,
            escape(headline),
            escape(&core.detail()),
            escape(core.note()),
        ));
    }

    /// Say something to the page. The body is a JSON object, as the page reads
    /// it in `window.__shard.push`.
    pub fn tell(&self, json: &str) {
        let _ = self
            .view
            .evaluate_script(&format!("window.__shard&&window.__shard.push({json})"));
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Put everything where it goes, now.
    fn lay_out(&self) {
        relayout(self.hwnd);
    }

    /// Hand the drag to the system, so snapping works the way it does anywhere.
    fn drag(&self) {
        unsafe {
            ReleaseCapture();
            SendMessageW(self.hwnd, WM_NCLBUTTONDOWN, HTCAPTION as usize, 0);
        }
    }

    fn maximise(&self) {
        let zoomed = unsafe { IsZoomed(self.hwnd) } != 0;
        unsafe { ShowWindow(self.hwnd, if zoomed { SW_RESTORE } else { SW_MAXIMIZE }) };
    }

    /// Put the window away without stopping anything.
    ///
    /// Closing is tidying: the bypass carries on, the downloads carry on, and
    /// the icon in the notification area is how it all comes back. Ending the
    /// program is a separate thing, and it is asked for from there.
    pub fn hide(&self) {
        unsafe { ShowWindow(self.hwnd, SW_HIDE) };
    }

    /// Bring it back, in front of whatever is there.
    pub fn show(&self) {
        unsafe {
            ShowWindow(self.hwnd, SW_SHOW);
            SetForegroundWindow(self.hwnd);
        }
        self.lay_out();
    }

    /// End the program for good.
    pub fn quit(&self) {
        QUITTING.with(|cell| cell.set(true));
        unsafe { PostMessageW(self.hwnd, WM_CLOSE, 0, 0) };
    }
}

impl Drop for Shell {
    /// Let the pages go before the window they are children of.
    ///
    /// A web view closes itself when it is dropped, and closing it needs the
    /// parent window still to be there. Dropped the other way round — the window
    /// destroyed first, the views after — the close runs against a handle that
    /// is no longer a window, which leaves the browser process behind.
    fn drop(&mut self) {
        self.tabs.borrow_mut().clear();
        TABS.with(|cell| cell.borrow_mut().clear());
        SHELL.with(|cell| cell.borrow_mut().take());
    }
}

/// Something that names the same file the next time the program runs.
///
/// The numbers handed to the page are made up per run and handed out in the
/// order files are listed, so a position remembered against one pointed at a
/// different file the next day. This is a plain digest of the path, which is
/// what the page keeps its resume positions under.
fn durable_key(path: &std::path::Path) -> String {
    // FNV-1a, 64-bit: a few lines, no dependency, and far better spread than a
    // sum for what is only a lookup key.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
}

/// A window handle in the shape wry wants.
struct Host(HWND);

impl raw_window_handle::HasWindowHandle for Host {
    fn window_handle(
        &self,
    ) -> std::result::Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError>
    {
        let mut handle = raw_window_handle::Win32WindowHandle::new(
            std::num::NonZeroIsize::new(self.0 as isize)
                .ok_or(raw_window_handle::HandleError::Unavailable)?,
        );
        handle.hinstance =
            std::num::NonZeroIsize::new(unsafe { GetModuleHandleW(std::ptr::null()) as isize });
        Ok(unsafe {
            raw_window_handle::WindowHandle::borrow_raw(raw_window_handle::RawWindowHandle::Win32(
                handle,
            ))
        })
    }
}

fn create_window(title: &str) -> Result<HWND> {
    // Told the truth about the screen before anything is measured. Without this
    // Windows hands the program a made-up resolution and then stretches what it
    // draws to fit the real one, which is why the text looked soft on a display
    // running above 100%.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };

    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(procedure),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance as _,
        hIcon: unsafe { LoadIconW(instance as _, 1 as *const u16) },
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        hbrBackground: unsafe { CreateSolidBrush(SURFACE) } as _,
        lpszMenuName: std::ptr::null(),
        lpszClassName: CLASS.as_ptr(),
    };
    unsafe { RegisterClassW(&class) };

    let mut wide: Vec<u16> = title.encode_utf16().collect();
    wide.push(0);
    // No caption of the system's own: the page draws the strip, so the frame
    // keeps only what it is needed for — resizing, snapping, and the taskbar.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            CLASS.as_ptr(),
            wide.as_ptr(),
            WS_POPUP
                | WS_THICKFRAME
                | WS_SYSMENU
                | WS_MINIMIZEBOX
                | WS_MAXIMIZEBOX
                | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            WIDTH,
            HEIGHT,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance as _,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(anyhow!("창을 만들지 못했습니다"));
    }
    dark_title_bar(hwnd);
    // A beat of its own. The loop only turns when a message arrives, and a
    // download reports on another thread — without this the bar sat still and a
    // finished download went unnoticed until something was clicked.
    unsafe { SetTimer(hwnd, TICK_TIMER, 250, None) };
    Ok(hwnd)
}

/// Ask the desktop manager for a dark, rounded frame — the same three lines the
/// browser window has always used.
///
/// The border itself is left to Windows. An imitation of a system edge is never
/// quite the system edge, and setting a border colour of our own is what put a
/// pale outline around the window. The light frame that appears when a window
/// loses focus is stopped instead by answering the deactivation notice without
/// letting the frame be redrawn (`WM_NCACTIVATE`).
fn dark_title_bar(hwnd: HWND) {
    const CORNER_PREFERENCE: u32 = 33;
    const ROUND: i32 = 2;
    let on: i32 = 1;
    let round: i32 = ROUND;
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&on as *const i32).cast(),
            4,
        );
        DwmSetWindowAttribute(hwnd, CORNER_PREFERENCE, (&round as *const i32).cast(), 4);
    }
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        // The frame keeps its resizing grip but loses its caption: the client
        // area is the whole window, and the page draws the bar itself.
        WM_NCCALCSIZE if wparam != 0 => {
            let params = lparam as *mut NCCALCSIZE_PARAMS;
            let rect = unsafe { &mut (*params).rgrc[0] };
            // A maximised window is sized past the monitor by the frame's width;
            // without this its edges — and the taskbar — end up underneath it.
            if unsafe { IsZoomed(hwnd) } != 0 {
                let border = unsafe { GetSystemMetrics(SM_CXSIZEFRAME) }
                    + unsafe { GetSystemMetrics(SM_CXPADDEDBORDER) };
                rect.left += border;
                rect.right -= border;
                rect.top += border;
                rect.bottom -= border;
            }
            0
        }
        // The grip around the edges. Everything inside is the page's; the strip
        // it draws asks for the drag itself rather than being hit-tested here,
        // because only the page knows where its own buttons are.
        WM_NCHITTEST => {
            let x = (lparam & 0xffff) as i16 as i32;
            let y = ((lparam >> 16) & 0xffff) as i16 as i32;
            let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            unsafe { GetWindowRect(hwnd, &mut rect) };
            if unsafe { IsZoomed(hwnd) } == 0 {
                let left = x < rect.left + RESIZE_EDGE;
                let right = x >= rect.right - RESIZE_EDGE;
                let top = y < rect.top + RESIZE_EDGE;
                let bottom = y >= rect.bottom - RESIZE_EDGE;
                let hit = match (left, right, top, bottom) {
                    (true, _, true, _) => HTTOPLEFT,
                    (_, true, true, _) => HTTOPRIGHT,
                    (true, _, _, true) => HTBOTTOMLEFT,
                    (_, true, _, true) => HTBOTTOMRIGHT,
                    (true, ..) => HTLEFT,
                    (_, true, ..) => HTRIGHT,
                    (_, _, true, _) => HTTOP,
                    (_, _, _, true) => HTBOTTOM,
                    _ => HTCLIENT,
                };
                if hit != HTCLIENT {
                    return hit as LRESULT;
                }
            }
            HTCLIENT as LRESULT
        }
        // The close button is "put it away", the way it was when the settings
        // window closed to the tray. Only an explicit quit ends the program, and
        // that sets the flag before asking the window to close.
        WM_CLOSE if !QUITTING.with(|cell| cell.get()) => {
            unsafe { ShowWindow(hwnd, SW_HIDE) };
            0
        }
        // Never let the frame be redrawn as "inactive": that redraw is the pale
        // outline that appears the moment the window loses focus.
        WM_NCACTIVATE => 1,
        // A click on an unfocused window should both bring it forward and press
        // what was pressed, rather than being spent on the activation alone.
        WM_MOUSEACTIVATE => MA_ACTIVATE as LRESULT,
        WM_ACTIVATE => {
            dark_title_bar(hwnd);
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_SIZE => {
            relayout(hwnd);
            0
        }
        // How small it may be made. Below this the strip's own buttons start
        // overlapping each other, so there is nothing to be gained by allowing
        // it — and a window that can be shrunk into nonsense looks broken.
        WM_GETMINMAXINFO => {
            let info = lparam as *mut MINMAXINFO;
            if !info.is_null() {
                let dpi = unsafe { GetDpiForWindow(hwnd) };
                let dpi = if dpi == 0 { 96 } else { dpi } as i32;
                unsafe {
                    (*info).ptMinTrackSize.x = (MIN_WIDTH * dpi) / 96;
                    (*info).ptMinTrackSize.y = (MIN_HEIGHT * dpi) / 96;
                }
            }
            0
        }
        // Dragged onto a display at another scale. Windows says where the window
        // should now be and how big; taking it is what keeps the text sharp
        // instead of stretched.
        WM_DPICHANGED => {
            let suggested = lparam as *const RECT;
            if !suggested.is_null() {
                let rect = unsafe { &*suggested };
                unsafe {
                    SetWindowPos(
                        hwnd,
                        std::ptr::null_mut(),
                        rect.left,
                        rect.top,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                };
            }
            relayout(hwnd);
            0
        }
        // Keys belong to whatever is in front. Without this the frame keeps the
        // focus it is given and nothing typed reaches the page.
        WM_SETFOCUS => {
            let showing = SHOWING.with(|cell| cell.get());
            match showing {
                Some(at) => TABS.with(|cell| {
                    if let Some(view) = cell.borrow().get(at) {
                        let _ = view.focus();
                    }
                }),
                None => SHELL.with(|cell| {
                    if let Some(view) = cell.borrow().as_ref() {
                        let _ = view.focus();
                    }
                }),
            }
            0
        }
        // Logging off or shutting down does not send WM_DESTROY: the process is
        // ended where it stands. Anything of the machine's that we changed has
        // to be put back here, or the computer comes back up pointing its DNS at
        // a program that is no longer running.
        WM_QUERYENDSESSION | WM_ENDSESSION => {
            crate::core::restore_system_dns();
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_DESTROY => {
            unsafe { KillTimer(hwnd, TICK_TIMER) };
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_asks_are_read_by_their_name() {
        assert_eq!(read_ask(r#"{"op":"ready"}"#), Ask::Ready);
        assert_eq!(read_ask(r#"{"op":"engine.toggle"}"#), Ask::EngineToggle);
        assert_eq!(read_ask(r#"{"op":"window.drag"}"#), Ask::WindowDrag);
        assert_eq!(read_ask(r#"{"op":"nav","to":"library"}"#), Ask::Nav("library".into()));
        assert_eq!(read_ask(r#"{"op":"download.cancel","id":7}"#), Ask::DownloadCancel(7));
    }

    #[test]
    fn a_value_with_a_quote_in_it_arrives_whole() {
        // What `JSON.stringify` writes for a title carrying a quote, a backslash
        // and a newline. Reading up to the first quote cut it short, which is
        // how a rename could quietly lose half a name.
        let body = r#"{"op":"library.rename","id":3,"title":"a \"b\" \\c\nd"}"#;
        assert_eq!(
            read_ask(body),
            Ask::LibraryRename { id: 3, title: "a \"b\" \\c\nd".to_string() }
        );
    }

    #[test]
    fn a_character_written_by_its_number_is_put_back_together() {
        let body = r#"{"op":"library.rename","id":1,"title":"\u0061\u0041b \ud83c\udf0a"}"#;
        let wave = char::from_u32(0x1_f30a).expect("a character");
        assert_eq!(
            read_ask(body),
            Ask::LibraryRename { id: 1, title: format!("aAb {wave}") }
        );
        // A half with nothing after it is a question mark, not the character
        // that happened to follow it.
        let lone = r#"{"op":"library.rename","id":1,"title":"\ud83cX"}"#;
        assert_eq!(
            read_ask(lone),
            Ask::LibraryRename { id: 1, title: "\u{fffd}X".to_string() }
        );
    }

    #[test]
    fn a_line_separator_cannot_end_the_script_it_travels_in() {
        assert_eq!(escape("a\u{2028}b"), "a\\u2028b");
        assert_eq!(escape("a\u{2029}b"), "a\\u2029b");
    }

    #[test]
    fn a_korean_title_is_not_disturbed_on_the_way_through() {
        let body = r#"{"op":"library.rename","id":1,"title":"제주도 3박4일"}"#;
        assert_eq!(
            read_ask(body),
            Ask::LibraryRename { id: 1, title: "제주도 3박4일".to_string() }
        );
    }

    #[test]
    fn an_ask_this_build_does_not_know_is_kept_whole() {
        let body = r#"{"op":"future.thing","x":1}"#;
        assert_eq!(read_ask(body), Ask::Unknown(body.to_string()));
    }

    #[test]
    fn the_shells_own_files_are_served_and_nothing_else_is() {
        let (status, mime, body) = serve("shard://shard.localhost/index.html");
        assert_eq!(status, 200);
        assert!(mime.starts_with("text/html"));
        assert!(!body.is_empty());

        assert_eq!(serve("shard://shard.localhost/app.css").0, 200);
        assert_eq!(serve("shard://shard.localhost/app.js").0, 200);
        // The root is the page, so a bare address opens the shell.
        assert_eq!(serve("shard://shard.localhost/").0, 200);
        // Anything else is not ours to hand out.
        assert_eq!(serve("shard://shard.localhost/../../secret.txt").0, 404);
        assert_eq!(serve("shard://shard.localhost/app.js/../../x").0, 404);
    }

    #[test]
    fn a_query_string_does_not_hide_the_file_being_asked_for() {
        assert_eq!(serve("shard://shard.localhost/app.css?v=2").0, 200);
    }

    #[test]
    fn a_player_asking_for_nothing_in_particular_gets_the_opening_slice() {
        assert_eq!(wanted_range(None, 10_000_000), (0, SLICE - 1));
        // A short file is handed over whole, and the last byte is the last byte.
        assert_eq!(wanted_range(None, 500), (0, 499));
    }

    #[test]
    fn a_seek_is_answered_from_where_it_asks_and_no_further_than_a_slice() {
        let len = 10_000_000;
        assert_eq!(
            wanted_range(Some("bytes=5000000-"), len),
            (5_000_000, 5_000_000 + SLICE - 1)
        );
        assert_eq!(wanted_range(Some("bytes=100-999"), len), (100, 999));
        // Never past the end of the file.
        assert_eq!(wanted_range(Some("bytes=9999000-99999999"), len), (9_999_000, len - 1));
        // One byte, which is how a player checks whether ranges are answered.
        assert_eq!(wanted_range(Some("bytes=0-0"), len), (0, 0));
    }

    #[test]
    fn the_end_of_a_file_can_be_asked_for_from_the_end() {
        // `bytes=-500`: the last 500 bytes, which is where some containers keep
        // the index a player needs before it can seek at all.
        assert_eq!(wanted_range(Some("bytes=-500"), 10_000), (9_500, 9_999));
        // A list of ranges is answered with the first of them.
        assert_eq!(wanted_range(Some("bytes=0-99,200-299"), 10_000), (0, 99));
    }

    #[test]
    fn a_file_is_reached_by_a_number_and_never_by_its_path() {
        let path = std::path::Path::new("C:/Videos/Shard/노래.webm");
        let id = register_media(path);
        assert_eq!(register_media(path), id);
        assert_eq!(media_path(id).as_deref(), Some(path));
        assert!(media_path(id + 9_999).is_none());
    }

    #[test]
    fn a_file_that_is_renamed_keeps_the_number_the_player_is_holding() {
        let was = std::path::Path::new("C:/Videos/Shard/before.webm");
        let now = std::path::Path::new("C:/Videos/Shard/after.webm");
        let id = register_media(was);
        remember_moved(was, now);
        assert_eq!(media_path(id).as_deref(), Some(now));
    }

    #[test]
    fn what_a_file_is_comes_from_what_it_is_called() {
        assert_eq!(media_mime(std::path::Path::new("a.webm")), "video/webm");
        assert_eq!(media_mime(std::path::Path::new("a.mp4")), "video/mp4");
        assert_eq!(media_mime(std::path::Path::new("a.m4a")), "audio/mp4");
        assert_eq!(media_mime(std::path::Path::new("a.zip")), "application/octet-stream");
    }

    #[test]
    fn a_files_lasting_name_is_the_same_one_next_time() {
        let path = std::path::Path::new("C:/Videos/Shard/노래.webm");
        assert_eq!(durable_key(path), durable_key(path));
        assert_ne!(durable_key(path), durable_key(std::path::Path::new("C:/Videos/Shard/x.webm")));
    }

    #[test]
    fn text_on_its_way_into_a_script_cannot_end_it_early() {
        assert_eq!(escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(escape("two\nlines"), "two\\nlines");
    }
}
