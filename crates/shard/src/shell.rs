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
/// The colour behind the page: the caption's own, so the band reaches the frame
/// with nothing showing between them.
///
/// The web view is set four pixels in from every edge, because a child window
/// takes the pointer and the frame would never hear the drag that resizes it.
/// Those four pixels are this colour — near black, it read as a gap between the
/// window and its own contents.
const SURFACE: u32 = 0x0020_2020;

/// The timer that keeps the loop turning while nothing is being pressed.
const TICK_TIMER: usize = 1;

/// The shortest gap between two turns of the periodic work.
///
/// The timer above sets the pace while the window is idle; this is the ceiling
/// when it is not, and messages are pouring in from the pointer.
const BEAT: std::time::Duration = std::time::Duration::from_millis(120);

/// How much of the frame is kept clear of a browsed page, in pixels.
///
/// A child window takes the pointer wherever it reaches, so the frame cannot
/// hear a drag on its own edge. Our own screens answer that themselves — the
/// page reports the edge it was pressed on (see `Ask::WindowResize`) — so they
/// are laid out flush to the frame.
///
/// A site cannot. Its scrollbar is drawn by the browser rather than by the page,
/// and a press on one is not something any script is told about, so along the
/// right-hand edge of anything that scrolls there was no way to take hold of the
/// window at all. This much of the frame is kept back from a site for the window
/// itself to hear.
const RESIZE_EDGE: i32 = 4;

/// How tall the title strip is — a Windows caption's own height, since that is
/// what it stands in for. The page draws it; this is what the layout code has to
/// agree with, so it is stated once and read from both sides.
pub const BAR: i32 = 32;

/// How much of the window the page keeps while a site is being looked at: the
/// title strip with its tabs, and the address row under it. Everything below is
/// the page being browsed, which is a child web view of its own.
pub const CHROME: i32 = BAR + 46;

/// How tall the strip along the bottom is while something is playing and one of
/// the browsed pages is in front. The page draws it; this is the room kept for
/// it, and the two have to be the same number or it is drawn cut in half.
pub const NOW_PLAYING: i32 = 58;

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

/// The `dwData` tag on the `WM_COPYDATA` a second copy sends to hand the running
/// one a file path to play. A fixed private number, so a stray copy-data from
/// somewhere else is not mistaken for one of ours.
const SHARD_OPEN_FILE: usize = 0x5348_4446; // "SHDF"

/// `COPYDATASTRUCT`, defined here rather than imported: the windows-sys build in
/// use does not expose it, and it is three fields.
#[cfg(windows)]
#[repr(C)]
struct CopyData {
    kind: usize,
    len: u32,
    data: *const std::ffi::c_void,
}

/// A media file opened from outside the library, and the others in its folder —
/// so the panel can list them and the next one is one press away. Kept for
/// `/external` to serve by index and for the run loop to hand to the page.
struct External {
    files: Vec<std::path::PathBuf>,
    at: usize,
}

static EXTERNAL: std::sync::Mutex<Option<External>> = std::sync::Mutex::new(None);
static EXTERNAL_DIRTY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The extensions the folder scan treats as playable, matching the file types
/// the program registers itself for.
const MEDIA_EXTS: &[&str] = &[
    "mp4", "mkv", "webm", "mov", "m4v", "avi", "mp3", "m4a", "aac", "flac", "wav", "opus", "ogg",
];

fn is_media(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| MEDIA_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Remember a file to play, and gather the media beside it. The run loop picks
/// it up. Scanning one folder is a cheap directory read, no burden at start-up.
pub fn set_opened_file(path: std::path::PathBuf) {
    let mut files: Vec<std::path::PathBuf> = path
        .parent()
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .map(|entries| {
            entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| is_media(p)).collect()
        })
        .unwrap_or_default();
    if !files.iter().any(|p| p == &path) {
        files.push(path.clone());
    }
    files.sort();
    let at = files.iter().position(|p| p == &path).unwrap_or(0);
    if let Ok(mut held) = EXTERNAL.lock() {
        *held = Some(External { files, at });
    }
    EXTERNAL_DIRTY.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The path served at `/external`, by index or the current one.
fn external_path(index: Option<usize>) -> Option<std::path::PathBuf> {
    let held = EXTERNAL.lock().ok()?;
    let set = held.as_ref()?;
    set.files.get(index.unwrap_or(set.at)).cloned()
}

/// The opened file's siblings and which one is current, once, when one is
/// waiting — the JSON the page needs to draw the panel and start playing.
fn take_opened_file() -> Option<String> {
    if !EXTERNAL_DIRTY.swap(false, std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    let held = EXTERNAL.lock().ok()?;
    let set = held.as_ref()?;
    let names: Vec<String> = set
        .files
        .iter()
        .map(|p| p.file_stem().and_then(|s| s.to_str()).unwrap_or("video").to_string())
        .collect();
    let list = names.iter().map(|n| format!("\"{}\"", escape(n))).collect::<Vec<_>>().join(",");
    Some(format!(r#""at":{},"list":[{}]"#, set.at, list))
}

/// Hand a file path to the copy already running, so it plays it and this copy
/// can exit. Sent with `WM_COPYDATA` to the shell window, found by its class.
pub fn send_file_to_running_copy(path: &std::path::Path) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
        use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, SendMessageW, WM_COPYDATA};
        let hwnd: HWND = unsafe { FindWindowW(CLASS.as_ptr(), std::ptr::null()) };
        if hwnd.is_null() {
            // The other copy is not up yet — at least raise it.
            uikit::single::wake_the_running_copy(crate::config::APP_NAME);
            return;
        }
        let wide: Vec<u16> =
            path.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        let data = CopyData {
            kind: SHARD_OPEN_FILE,
            len: (wide.len() * 2) as u32,
            data: wide.as_ptr().cast(),
        };
        // Let the running copy take the foreground. Without this its
        // SetForegroundWindow is refused when its window is already open but
        // behind another — Windows only lets the process that currently owns the
        // foreground (this launcher, for the moment) hand it over.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
                windows_sys::Win32::UI::WindowsAndMessaging::ASFW_ANY,
            );
            SendMessageW(
                hwnd,
                WM_COPYDATA,
                0 as WPARAM,
                (&data as *const CopyData) as LPARAM,
            )
        };
    }
    #[cfg(not(windows))]
    let _ = path;
}

/// The media kinds Shard can open, offered to Windows as things it plays.
#[cfg(windows)]
const OPENABLE: &[&str] =
    &["mp4", "mkv", "webm", "mov", "m4v", "mp3", "m4a", "aac", "flac", "wav", "opus", "ogg"];

/// Make Shard a program these files can be opened with.
///
/// Under `HKCU\Software\Classes`, so it needs no elevation and touches only this
/// user's settings. It registers a ProgID and adds it to each extension's
/// "open with" list — which puts Shard in the *연결 프로그램* menu without
/// seizing the default. The user chooses to make it the default themselves, the
/// one change Windows will not let a program make for itself.
pub fn register_file_types() {
    #[cfg(windows)]
    {
        let Ok(exe) = std::env::current_exe() else { return };
        let exe = exe.to_string_lossy().to_string();
        let command = format!("\"{exe}\" \"%1\"");
        let icon = format!("\"{exe}\",0");
        reg_set("Software\\Classes\\Shard.Media", None, "Shard 미디어");
        reg_set("Software\\Classes\\Shard.Media\\DefaultIcon", None, &icon);
        reg_set("Software\\Classes\\Shard.Media\\shell\\open\\command", None, &command);
        for ext in OPENABLE {
            reg_set(&format!("Software\\Classes\\.{ext}\\OpenWithProgids"), Some("Shard.Media"), "");
        }
    }
}

/// Write one `REG_SZ` under `HKEY_CURRENT_USER`, creating the key if need be.
#[cfg(windows)]
fn reg_set(subkey: &str, value: Option<&str>, data: &str) -> bool {
    use windows_sys::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
    let sub: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let val: Option<Vec<u16>> = value.map(|v| v.encode_utf16().chain(std::iter::once(0)).collect());
    let dat: Vec<u16> = data.encode_utf16().chain(std::iter::once(0)).collect();
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            val.as_ref().map(|v| v.as_ptr()).unwrap_or(std::ptr::null()),
            REG_SZ,
            dat.as_ptr().cast(),
            (dat.len() * 2) as u32,
        )
    };
    status == 0
}

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
    /// Whether something is playing. Beside the others so the window procedure
    /// keeps the room for the strip while the window is being dragged.
    static PLAYING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Extra chrome height (in page units) the bookmarks bar takes under the
    /// address row — 0 when it is not shown. The page reports it so the site below
    /// starts under the bar rather than beneath it.
    static EXTRA_CHROME: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
    /// The window the shell's page is drawn in — a child of ours, taken while it
    /// was the only one. Needed to say which of two overlapping children is in
    /// front, which nothing in the web view binding can be asked.
    static SHELL_WINDOW: std::cell::Cell<HWND> = const { std::cell::Cell::new(std::ptr::null_mut()) };
    /// Whether that has been said already.
    static SUNK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
    // Nothing is kept back from the window while it fills the screen: there is
    // no edge to take hold of.
    let edge = if unsafe { IsZoomed(hwnd) } != 0 { 0 } else { RESIZE_EDGE };
    let width = (rect.right - rect.left).max(0) as u32;
    let height = (rect.bottom - rect.top).max(0) as u32;
    let showing = SHOWING.with(|cell| cell.get());
    // The address row plus, when the bookmarks bar is shown, its height too — so
    // the site starts under the bar, not behind it.
    let chrome = chrome_height(hwnd) + scaled(hwnd, EXTRA_CHROME.with(|cell| cell.get()));
    // Room kept along the bottom for the strip that says what is playing. The
    // page draws it, and the page is underneath the site being browsed, so the
    // site has to be made shorter or the strip is simply covered up.
    let bar = if PLAYING.with(|cell| cell.get()) { scaled(hwnd, NOW_PLAYING) } else { 0 };

    // With a site in front the page keeps only its chrome — the tabs and the
    // address row — and the site fills what is left. With one of our own screens
    // up, the page is the whole window.
    //
    // Except while something is playing: then the page also has the strip along
    // the bottom, so it is given the whole window and the site is laid over its
    // middle. Two children of one window overlapping is only safe if the order
    // is known, so the page is put at the back of it (see [`sink_shell`]).
    let ours = match (showing.is_some(), bar > 0) {
        (true, false) => (chrome as u32).min(height),
        _ => height,
    };
    if showing.is_some() && bar > 0 {
        sink_shell();
    }
    SHELL.with(|cell| {
        if let Some(view) = cell.borrow().as_ref() {
            let _ = view.set_bounds(wry::Rect {
                position: wry::dpi::PhysicalPosition::new(0, 0).into(),
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
                // Below the site sits the playing strip when it is up, or the
                // bottom resize grip when it is not. It was always both —
                // `bar + edge` — so while something played there was a band of
                // the page's own background, the width of the grip, showing
                // between the site and the strip. Maximised the grip is zero and
                // it vanished, which is why it only showed in a windowed size.
                // The strip is flush against the site now: no grip under it.
                let below = if bar > 0 { bar } else { edge };
                let _ = view.set_bounds(wry::Rect {
                    position: wry::dpi::PhysicalPosition::new(edge, chrome).into(),
                    size: wry::dpi::PhysicalSize::new(
                        width.saturating_sub((edge * 2) as u32),
                        height.saturating_sub((chrome + below) as u32),
                    )
                    .into(),
                });
            }
        }
    });
}

/// Put the shell's page behind every site being browsed.
///
/// They only overlap while the playing strip is up: the page needs the whole
/// window to put a strip along the bottom of it, and the site has to be over the
/// middle of that. Windows puts each new child in front of the ones before it,
/// which already has the tabs in front — this says so rather than relying on it,
/// because the way it fails is a blank page over the site being watched.
fn sink_shell() {
    // Once. This runs from the layout, which runs for every pixel a window is
    // dragged, and reordering the children of a window sixty times a second to
    // say what it already says is work that shows.
    if SUNK.with(|cell| cell.replace(true)) {
        return;
    }
    let shell = SHELL_WINDOW.with(|cell| cell.get());
    if shell.is_null() {
        return;
    }
    unsafe {
        SetWindowPos(shell, HWND_BOTTOM, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

/// How tall the page's chrome is in real pixels on this window's display.
///
/// The page lays its strip out in its own units and the browser scales them by
/// the display's zoom; the site underneath is placed in real pixels. Without
/// this the two disagreed on a display at 125% and the page overlapped the site.
fn chrome_height(hwnd: HWND) -> i32 {
    scaled(hwnd, CHROME)
}

/// A measurement the page lays out in its own units, in this display's pixels.
fn scaled(hwnd: HWND, units: i32) -> i32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let dpi = if dpi == 0 { 96 } else { dpi };
    (units * dpi as i32) / 96
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
    LibraryDropFolder { kind: String, name: String },
    /// Delete a folder AND everything of this shelf inside it (전체삭제).
    LibraryDeleteFolder { kind: String, name: String },
    /// The shelf, rearranged by hand: every file's key in its new order.
    LibraryOrder { kind: String, keys: String },
    /// Something is playing, or has stopped: the window keeps room along the
    /// bottom for the strip that says so.
    Playing(bool),
    /// The bookmarks bar appeared or went away: keep room for it under the address
    /// row so the site starts below it. The value is its height in page units.
    ChromeExtra(i32),
    /// Browsing: a tab to open, pick or drop, and where to point it.
    TabNew(String),
    TabPick(usize),
    TabShut(usize),
    Steer { what: String, url: String },
    /// The browser start page asked for its own contents — the pinned sites and
    /// the ones visited most, so it can draw the tiles.
    BrowserHome,
    /// Pin or unpin a page: the star on the address row. Toggles by URL, so a
    /// second press on a page already pinned removes it.
    BookmarkToggle { url: String, title: String },
    /// Drop one "자주 방문" tile for good — the host is remembered as hidden so
    /// the tile does not come back the next time the site is opened.
    FrequentHide(String),
    /// Make the page in front the homepage (a right-click on the home button).
    HomeSet(String),
    /// The favorites page's history: drop one entry, or clear it all.
    HistoryRemove(String),
    HistoryClear,
    /// Rename a bookmark (a right-click on the favorites page).
    BookmarkRename { url: String, title: String },
    /// Reorder bookmarks by dragging: move the one at `from` to sit at `to`. Same
    /// order backs the bookmarks bar and the favorites page, so both drag to it.
    BookmarkMove { from: usize, to: usize },
    /// A page answered the download panel — which quality was chosen.
    /// A row on the quality list. `anyway` is set when it was pressed past a
    /// warning that the file is already saved.
    Chose { itag: u64, anyway: bool },
    /// What a page can give, for the list to be built from.
    PageOffer(String),
    /// The settings: read them, or put one back.
    SettingsRead,
    /// Put every setting back to what it was on the first launch.
    SettingsReset,
    /// Open the folder the log is written into.
    LogsOpen,
    /// Find out whether a site is blocked, and what gets through to it.
    ProbeStart(String),
    SettingsSet { key: String, value: String },
    /// The window itself: what a page cannot do to the frame around it.
    WindowDrag,
    WindowMinimise,
    WindowMaximise,
    WindowClose,
    /// A press on one of the window's own edges, named by the page under the
    /// pointer: "t", "bl", "r" and so on.
    WindowResize(String),
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
        "library.order" => Ask::LibraryOrder {
            kind: field(body, "kind").unwrap_or_else(|| "video".into()),
            keys: field(body, "keys").unwrap_or_default(),
        },
        "library.dropFolder" => Ask::LibraryDropFolder {
            kind: field(body, "kind").unwrap_or_else(|| "video".into()),
            name: field(body, "name").unwrap_or_default(),
        },
        "library.deleteFolder" => Ask::LibraryDeleteFolder {
            kind: field(body, "kind").unwrap_or_else(|| "video".into()),
            name: field(body, "name").unwrap_or_default(),
        },
        // A plain boolean, not a string, so it is read as one.
        "playing" => Ask::Playing(body.contains(r#""on":true"#)),
        "chrome" => Ask::ChromeExtra(number(body, "extra").unwrap_or(0) as i32),
        "tab.new" => Ask::TabNew(field(body, "url").unwrap_or_default()),
        "tab.pick" => Ask::TabPick(number(body, "at").unwrap_or(0) as usize),
        "tab.shut" => Ask::TabShut(number(body, "at").unwrap_or(0) as usize),
        "browser.home" => Ask::BrowserHome,
        "bookmark.toggle" => Ask::BookmarkToggle {
            url: field(body, "url").unwrap_or_default(),
            title: field(body, "title").unwrap_or_default(),
        },
        "frequent.hide" => Ask::FrequentHide(field(body, "host").unwrap_or_default()),
        "home.set" => Ask::HomeSet(field(body, "url").unwrap_or_default()),
        "history.remove" => Ask::HistoryRemove(field(body, "url").unwrap_or_default()),
        "history.clear" => Ask::HistoryClear,
        "bookmark.rename" => Ask::BookmarkRename {
            url: field(body, "url").unwrap_or_default(),
            title: field(body, "title").unwrap_or_default(),
        },
        "bookmark.move" => Ask::BookmarkMove {
            from: number(body, "from").unwrap_or(0) as usize,
            to: number(body, "to").unwrap_or(0) as usize,
        },
        "steer" => Ask::Steer {
            what: field(body, "what").unwrap_or_else(|| "go".into()),
            url: field(body, "url").unwrap_or_default(),
        },
        "chose" => Ask::Chose { itag: number(body, "itag").unwrap_or(0), anyway: false },
        "settings.read" => Ask::SettingsRead,
        "settings.reset" => Ask::SettingsReset,
        "logs.open" => Ask::LogsOpen,
        "probe.start" => Ask::ProbeStart(field(body, "host").unwrap_or_default()),
        "settings.set" => Ask::SettingsSet {
            key: field(body, "key").unwrap_or_default(),
            value: field(body, "value").unwrap_or_default(),
        },
        "window.drag" => Ask::WindowDrag,
        "window.minimise" => Ask::WindowMinimise,
        "window.maximise" => Ask::WindowMaximise,
        "window.close" => Ask::WindowClose,
        "window.resize" => Ask::WindowResize(field(body, "edge").unwrap_or_default()),
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

    // The picture out of a file, for a row and for the screen a song plays on.
    //
    // Read from the front of the file rather than all of it: the header is where
    // this lives, and a song is megabytes of sound after it.
    if let Some(rest) = path.strip_prefix("/cover/") {
        let id = rest.split('/').next().unwrap_or("").parse::<u64>().unwrap_or(0);
        let Some(file) = media_path(id) else { return not_found() };
        // The whole file, not a 2 MB head: our writer puts the `moov` (which holds
        // the cover) at the END, so a music file bigger than that head had its
        // cover missed. A music file is a few MB, read once when the tile shows.
        let Ok(head) = std::fs::read(&file) else { return not_found() };
        // MP4/.m4a keep the cover in a `covr` box; an MP3 keeps it in an ID3 APIC
        // frame at the front. Try the box first, then the tag.
        let Some((picture, kind)) = crate::download::mp4::cover(&head)
            .or_else(|| crate::download::mp3::id3_cover(&head))
        else {
            return not_found();
        };
        return build
            .status(200)
            .header("Content-Type", if kind == "png" { "image/png" } else { "image/jpeg" })
            .header("Content-Length", picture.len().to_string())
            .header("Cache-Control", "no-store")
            .body(std::borrow::Cow::Owned(picture))
            .unwrap_or_else(|_| not_found());
    }

    // A file opened from outside the library — the one Explorer handed us when
    // Shard is the program a media file opens with. Served from the path kept in
    // `EXTERNAL`, with the same range handling a library file gets so seeking
    // works. The query after it is only a cache-buster.
    if path == "/external" || path.starts_with("/external?") {
        // `?i=N` names one of the folder's files; without it, the current one.
        let index = uri
            .split_once("i=")
            .and_then(|(_, rest)| rest.split(['&', '#']).next())
            .and_then(|n| n.parse::<usize>().ok());
        let Some(file) = external_path(index) else {
            return not_found();
        };
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
            let path = path_of(&uri);
            if !path.starts_with("/media/") && !path.starts_with("/cover/") {
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
        // DevTools on so the shell UI can be inspected with F12 / right-click
        // Inspect — release builds otherwise ship with it disabled.
        .with_devtools(true)
        .build_as_child(&host)
        .map_err(|e| anyhow!("WebView2를 시작하지 못했습니다: {e}"))?;

    // Taken while it is the only child there is, which is what makes it the one
    // that can be named later. See [`sink_shell`].
    SHELL_WINDOW.with(|cell| cell.set(unsafe { GetWindow(hwnd, GW_CHILD) }));

    let view = Rc::new(view);
    watch_process(&view, "shard://shard.localhost/index.html");
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
        beat: std::cell::Cell::new(std::time::Instant::now()),
        next_tab: std::cell::Cell::new(1),
        said_zoomed: std::cell::Cell::new(false),
        said_downloads: RefCell::new(String::new()),
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
    /// The engine switch, only present when the program runs elevated — the
    /// engine needs the driver, and the driver needs a token. Its label is the
    /// action, not the state: "ON" while stopped, "OFF" while running.
    toggle: Option<uikit::tray::MenuItem>,
    /// Offered only when *not* elevated: run a copy that is, so the engine
    /// becomes reachable. There is no engine UI at all without the token.
    elevate: Option<uikit::tray::MenuItem>,
    open: uikit::tray::MenuItem,
    quit: uikit::tray::MenuItem,
}

impl Tray {
    fn build(elevated: bool) -> Result<Self> {
        use uikit::tray::{Menu, MenuItem, PredefinedMenuItem};
        let events = uikit::tray::watch();
        let menu = Menu::new();

        // Elevated: the engine switch, named by its state. Unelevated: no engine
        // at all, only the way to become elevated — the bypass is hidden until
        // the program has the token to run it.
        let mut toggle = None;
        let mut elevate = None;
        if elevated {
            let item = MenuItem::new("ON", true, None);
            menu.append(&item)?;
            toggle = Some(item);
        } else {
            let item = MenuItem::new("관리자 권한으로 실행", true, None);
            menu.append(&item)?;
            elevate = Some(item);
        }

        let open = MenuItem::new("보관함 열기", true, None);
        let quit = MenuItem::new("종료", true, None);
        menu.append(&open)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&quit)?;
        let icon = uikit::tray::build("Shard", &uikit::icon::shard(false), menu)?;
        Ok(Self { icon, events, toggle, elevate, open, quit })
    }

    /// Show what the engine is doing, in the icon, the tooltip and the switch.
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
            .set_tooltip(Some(if running { "Shard — 우회 동작 중" } else { "Shard" }));
        if let Some(toggle) = &self.toggle {
            // The label is what a press will do: turn it ON while it is off,
            // turn it OFF while it is on.
            toggle.set_text(if running { "OFF" } else { "ON" });
        }
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
    //
    // `--engine` starts it too: that is the flag the unelevated copy passes when
    // it relaunches itself elevated to switch the engine on, so the elevated one
    // comes up with the engine already running rather than waiting for a second
    // press.
    let asked_engine = std::env::args().any(|arg| arg == "--engine");
    if asked_engine || core.borrow().shared.config.read().start_engine_on_launch {
        core.borrow_mut().start();
    }

    // The switch in the notification area. It is what lets the window be put
    // away while the bypass keeps running — closing the window is tidying it
    // out of the way, not stopping the program.
    let elevated = uikit::elevation::is_elevated();
    let tray = Tray::build(elevated)?;
    tray.follow(&core.borrow());

    // A probe in flight, if one has been started. Beside the downloads for the
    // same reason: it reports from a thread of its own, and the beat is what
    // carries anything said off-thread to the page.
    let probing: Rc<RefCell<Option<crossbeam_channel::Receiver<crate::prober::Progress>>>> =
        Rc::new(RefCell::new(None));

    let engine = core.clone();
    let jobs = saving.clone();
    let started = probing.clone();
    let shell = open("Shard", move |shell, ask| match ask {
        Ask::Ready => {
            // A page that has just loaded knows nothing, whatever was last sent
            // to the one before it.
            shell.forget_what_was_said();
            // Whether the engine is reachable at all — the home screen and its
            // switch only exist when the program has the token to run the driver.
            shell.tell(&format!(r#"{{"t":"caps","elevated":{elevated}}}"#));
            shell.say_engine(&engine.borrow());
            shell.say_downloads(&jobs.borrow());
            shell.say_tabs();
            // Sent up front, not only when the start page opens: the star on the
            // address row fills from this list, and it has to be right the moment
            // a tab is looked at, before the start page has ever been seen.
            shell.tell(&browser_home_json(&engine.borrow().shared.config.read().browser));
        }
        Ask::EngineToggle => {
            // The engine needs the WinDivert driver, and the driver needs an
            // elevated token; nothing else in the program does. So the program
            // runs unelevated, and the first time the engine is switched on
            // without a token it hands off to an elevated copy of itself that
            // starts the engine, rather than failing inside WinDivertOpen. The
            // claim is let go first so the elevated copy can take it, and taken
            // back if the prompt is declined.
            if !engine.borrow().running() && !uikit::elevation::is_elevated() {
                uikit::single::release();
                if uikit::elevation::relaunch_elevated("--engine") {
                    shell.quit();
                } else {
                    uikit::single::reclaim();
                    shell.say_engine(&engine.borrow());
                }
            } else {
                engine.borrow_mut().toggle();
                shell.say_engine(&engine.borrow());
            }
        }
        // Going to the browser opens a tab the first time and comes back to
        // what was left the rest of the time; going anywhere else hides it,
        // which is what gives the shell the whole window again.
        Ask::Nav(to) => {
            if to == "browser" {
                if shell.tab_count() == 0 {
                    shell.open_tab(&homepage(&engine.borrow()));
                } else {
                    shell.show_tab(Some(shell.tab_count() - 1));
                }
            } else {
                shell.show_tab(None);
            }
        }
        Ask::TabNew(url) => {
            let url = if url.is_empty() { "https://www.youtube.com/".to_string() } else { url };
            // Counted where a page is opened by hand, not on every heartbeat a tab
            // sends: "자주 방문" is the sites you go to, and one visit is one arrival,
            // not one second spent there. Persisted at once, the way settings are.
            {
                let core = engine.borrow();
                let mut cfg = core.shared.config.write();
                record_visit(&mut cfg.browser, &url);
            }
            engine.borrow().save_config();
            shell.open_tab(&url);
        }
        Ask::TabPick(at) => shell.show_tab(Some(at)),
        Ask::TabShut(at) => shell.close_tab(at),
        Ask::Steer { what, url } => shell.steer(&what, &url),
        // The start page asked for its tiles — the pinned sites and the ones
        // visited most. Read straight from the saved config.
        Ask::BrowserHome => {
            let json = browser_home_json(&engine.borrow().shared.config.read().browser);
            shell.tell(&json);
        }
        // The star: pin the page in front, or unpin it if it was already pinned.
        Ask::BookmarkToggle { url, title } => {
            if !url.is_empty() {
                {
                    let core = engine.borrow();
                    let mut cfg = core.shared.config.write();
                    let marks = &mut cfg.browser.bookmarks;
                    if let Some(pos) = marks.iter().position(|b| b.url == url) {
                        marks.remove(pos);
                    } else {
                        // Newest first, so the most recently pinned reads at the top.
                        marks.insert(0, crate::config::Bookmark {
                            url: url.clone(),
                            title: if title.trim().is_empty() { title_of(&url) } else { title },
                        });
                    }
                }
                engine.borrow().save_config();
            }
            let json = browser_home_json(&engine.borrow().shared.config.read().browser);
            shell.tell(&json);
        }
        // Drop one "자주 방문" tile and remember the host as hidden, so opening the
        // site again does not bring the tile straight back.
        Ask::FrequentHide(host) => {
            let host = crate::config::normalise_host(&host);
            if !host.is_empty() {
                {
                    let core = engine.borrow();
                    let mut cfg = core.shared.config.write();
                    cfg.browser.visits.remove(&host);
                    if !cfg.browser.hidden_frequent.iter().any(|h| h == &host) {
                        cfg.browser.hidden_frequent.push(host);
                    }
                }
                engine.borrow().save_config();
            }
            let json = browser_home_json(&engine.borrow().shared.config.read().browser);
            shell.tell(&json);
        }
        // Right-click on the home button: the page in front becomes the homepage.
        Ask::HomeSet(url) => {
            if url.starts_with("http") {
                {
                    let core = engine.borrow();
                    core.shared.config.write().browser.homepage = url;
                }
                engine.borrow().save_config();
            }
            let json = browser_home_json(&engine.borrow().shared.config.read().browser);
            shell.tell(&json);
        }
        Ask::HistoryRemove(url) => {
            {
                let core = engine.borrow();
                core.shared.config.write().browser.history.retain(|h| h.url != url);
            }
            engine.borrow().save_config();
            let json = browser_home_json(&engine.borrow().shared.config.read().browser);
            shell.tell(&json);
        }
        Ask::HistoryClear => {
            {
                let core = engine.borrow();
                core.shared.config.write().browser.history.clear();
            }
            engine.borrow().save_config();
            let json = browser_home_json(&engine.borrow().shared.config.read().browser);
            shell.tell(&json);
        }
        Ask::BookmarkRename { url, title } => {
            let title = title.trim().to_string();
            if !url.is_empty() && !title.is_empty() {
                {
                    let core = engine.borrow();
                    let mut cfg = core.shared.config.write();
                    if let Some(m) = cfg.browser.bookmarks.iter_mut().find(|m| m.url == url) {
                        m.title = title;
                    }
                }
                engine.borrow().save_config();
            }
            let json = browser_home_json(&engine.borrow().shared.config.read().browser);
            shell.tell(&json);
        }
        Ask::BookmarkMove { from, to } => {
            {
                let core = engine.borrow();
                let mut cfg = core.shared.config.write();
                let marks = &mut cfg.browser.bookmarks;
                // Bounds-checked: the page's indices could lag a concurrent change.
                if from < marks.len() && to < marks.len() && from != to {
                    let m = marks.remove(from);
                    marks.insert(to, m);
                }
            }
            engine.borrow().save_config();
            let json = browser_home_json(&engine.borrow().shared.config.read().browser);
            shell.tell(&json);
        }

        // The list of what can be saved, and the row that was pressed on it.
        Ask::PageOffer(payload) => {
            let script = jobs.borrow_mut().qualities(&payload);
            shell.tell_page(&script);
        }
        Ask::Chose { itag, anyway } => {
            let script = jobs.borrow_mut().begin(itag as u32, anyway);
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

        // Everything back to what it was on the first launch, except the one
        // thing that is not a setting: what has been learned about which sites
        // are blocked stays, because relearning it takes days of ordinary use.
        Ask::SettingsReset => {
            {
                let core = engine.borrow();
                let mut cfg = core.shared.config.write();
                let learned = std::mem::take(&mut cfg.overrides);
                *cfg = crate::config::Config::default();
                cfg.overrides = learned;
            }
            engine.borrow().save_config();
            engine.borrow_mut().restart_if_running();
            let json = crate::settings::as_json(&engine.borrow().shared.config.read());
            shell.tell(&json);
            shell.say_engine(&engine.borrow());
        }

        Ask::ProbeStart(host) => {
            let core = engine.borrow();
            if !core.running() {
                shell.tell(r#"{"t":"probe","add":[{"text":"엔진이 꺼져 있으면 탐색할 수 없습니다.","ok":false}],"running":false}"#);
                return;
            }
            // Accept a pasted address, not only a bare hostname.
            let host = crate::config::normalise_host(&host);
            if host.is_empty() {
                return;
            }
            *started.borrow_mut() = Some(crate::prober::spawn(core.shared.clone(), host));
            shell.tell(r#"{"t":"probe","add":[],"running":true,"clear":true}"#);
        }
        Ask::LogsOpen => {
            let logs = uikit::config::app_dir(crate::config::APP_NAME).join("logs");
            // The folder, not the file: a log being written to is held open, and
            // what opens it matters less than being able to find it.
            crate::library::reveal(&logs);
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
        Ask::LibraryDropFolder { kind, name } => {
            let shelf = shelf_of(&kind);
            match crate::library::drop_folder(shelf, &name) {
                Ok(moved) => tracing::info!("folder {name} removed; {moved} file(s) came out of it"),
                Err(e) => tracing::warn!("could not remove folder {name}: {e:#}"),
            }
            shell.say_library(shelf);
        }
        Ask::LibraryDeleteFolder { kind, name } => {
            let shelf = shelf_of(&kind);
            match crate::library::delete_folder(shelf, &name) {
                Ok(gone) => tracing::info!("folder {name} deleted with {gone} file(s)"),
                Err(e) => tracing::warn!("could not delete folder {name}: {e:#}"),
            }
            shell.say_library(shelf);
        }
        Ask::LibraryOrder { kind, keys } => {
            let shelf = shelf_of(&kind);
            let keys: Vec<String> =
                keys.lines().map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect();
            if let Err(e) = crate::library::set_order(shelf, &keys) {
                tracing::warn!("could not write the shelf's order: {e:#}");
            }
        }
        Ask::Playing(on) => shell.now_playing(on),
        Ask::ChromeExtra(extra) => {
            if EXTRA_CHROME.with(|cell| cell.get()) != extra {
                EXTRA_CHROME.with(|cell| cell.set(extra));
                shell.lay_out();
            }
        }
        other => tracing::info!("shell ask: {other:?}"),
    })?;

    // What the running downloads have to say, taken in on the same beat as the
    // window's own messages: they finish on their own threads, and the page
    // learns about it here.
    // The engine state the tray last drew, so a change made from the home
    // screen — which the tray does not hear about directly — is followed here
    // and the two never drift apart.
    let tray_shows = std::cell::Cell::new(core.borrow().running());
    shell.run(|shell| {
        // A file was double-clicked, on start-up or handed over by a second
        // copy while this one ran. Bring the window up and tell the page to play
        // it — the file itself is served from `/external`.
        if let Some(payload) = take_opened_file() {
            shell.show();
            shell.tell(&format!(r#"{{"t":"external",{payload}}}"#));
        }

        // Keep the tray in step with the engine however it was switched — the
        // page toggles it without the tray knowing, so the tray reads the truth
        // here rather than trusting its own last guess.
        let running = core.borrow().running();
        if running != tray_shows.get() {
            tray_shows.set(running);
            tray.follow(&core.borrow());
        }

        // The tray, first: it is how the window comes back once it has been put
        // away, so it has to be answered even while nothing else is happening.
        while let Ok(event) = tray.events.tray.try_recv() {
            // A double click, not a single one: one click on a tray icon is
            // how a menu is reached, and a window jumping up for it is a window
            // that appears when nothing was asked for.
            if uikit::tray::is_double_click(&event) {
                shell.show();
            }
        }
        while let Ok(event) = tray.events.menu.try_recv() {
            let id = event.id();
            if tray.toggle.as_ref().is_some_and(|t| id == t.id()) {
                core.borrow_mut().toggle();
                shell.say_engine(&core.borrow());
                tray.follow(&core.borrow());
            } else if tray.elevate.as_ref().is_some_and(|e| id == e.id()) {
                // Become elevated: let the claim go, start an elevated copy, and
                // step aside. If the prompt is declined nothing changes and the
                // claim is taken back.
                uikit::single::release();
                if uikit::elevation::relaunch_elevated("") {
                    shell.quit();
                } else {
                    uikit::single::reclaim();
                }
            } else if id == tray.open.id() {
                shell.show();
            } else if id == tray.quit.id() {
                shell.quit();
            }
        }

        let drained = saving.borrow_mut().drain();
        for (note, failed) in &drained.finished {
            // Written down either way. A download that failed says so on the
            // page it was started from and then is gone; without this there was
            // nothing left anywhere to say what had gone wrong.
            if *failed {
                tracing::warn!("download failed: {note}");
            } else {
                tracing::info!("download saved: {note}");
            }
            shell.tell_page(&if *failed {
                crate::download::youtube::say_script(note, true)
            } else {
                crate::download::youtube::flash_script(note)
            });
        }
        // Every beat, and sent only when it reads differently from last time.
        shell.say_downloads(&saving.borrow());

        // What the probe has to say since last time.
        let mut lines = Vec::new();
        let mut ended = false;
        if let Some(rx) = probing.borrow().as_ref() {
            while let Ok(progress) = rx.try_recv() {
                ended |= crate::prober::is_last(&progress);
                for (text, ok) in crate::prober::say(progress) {
                    lines.push(format!(
                        r#"{{"text":"{}","ok":{}}}"#,
                        crate::shell::escape(&text),
                        match ok {
                            Some(true) => "true",
                            Some(false) => "false",
                            None => "null",
                        }
                    ));
                }
            }
        }
        if !lines.is_empty() || ended {
            shell.tell(&format!(
                r#"{{"t":"probe","add":[{}],"running":{}}}"#,
                lines.join(","),
                !ended
            ));
        }
        if ended {
            *probing.borrow_mut() = None;
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

/// Notice when a page's own process dies, and put the page back.
///
/// A web view is two programs: this one, and the process drawing the page. When
/// the drawing side dies — a page that ran out of memory is the usual reason —
/// the view stays exactly where it was, blank, and there is nothing from here
/// that looks wrong. The runtime does say so, but wry does not pass it on, so
/// this asks the view's own interface to be told.
///
/// Held by a weak handle: the view owns the handler, so a strong one would be a
/// ring neither end could leave, and closing a tab would free nothing.
fn watch_process(view: &Rc<wry::WebView>, home: &str) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PROCESS_FAILED_KIND, COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED,
    };
    use wry::WebViewExtWindows;

    let weak = Rc::downgrade(view);
    let home = home.to_string();
    let handler = webview2_com::ProcessFailedEventHandler::create(Box::new(move |_, args| {
        let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND(-1);
        if let Some(args) = args.as_ref() {
            let _ = unsafe { args.ProcessFailedKind(&mut kind) };
        }
        // The browser process going takes every view in the program with it,
        // this one included, and nothing left here can draw. Said plainly in the
        // log rather than dressed up as a recovery that cannot work.
        if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED {
            tracing::error!("WebView2 browser process exited; Shard must be restarted");
            return Ok(());
        }
        let Some(view) = weak.upgrade() else { return Ok(()) };
        // Where it was, if it can still be asked; where it started, if not.
        let back = match view.url() {
            Ok(url) if !url.is_empty() && url != "about:blank" => url,
            _ => home.clone(),
        };
        tracing::warn!("page process failed (kind {}); reloading {back}", kind.0);
        if let Err(e) = view.load_url(&back) {
            tracing::error!("could not reload after a process failure: {e}");
        }
        Ok(())
    }));

    // Registered for as long as the view lives, so the token is not kept: there
    // is nothing later that would want to stop listening.
    let mut token = 0i64;
    if let Err(e) = unsafe { view.webview().add_ProcessFailed(&handler, &mut token) } {
        tracing::warn!("could not watch for page process failures: {e}");
    }
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

/// Note that a site was opened: bump its host's visit count and keep the newest
/// pages in history. Host granularity, because "자주 방문" answers "which sites",
/// not "which pages" — a hundred YouTube videos are one place you keep going.
fn record_visit(b: &mut crate::config::Browser, url: &str) {
    if !url.starts_with("http") {
        return; // about:blank and the like are not places.
    }
    let host = crate::config::normalise_host(url);
    if host.is_empty() {
        return;
    }
    *b.visits.entry(host).or_insert(0) += 1;
    // Newest first, without duplicates, capped — a long history is neither shown
    // nor useful, and rewriting the whole file on each open should stay cheap.
    b.history.retain(|h| h.url != url);
    b.history.insert(0, crate::config::Bookmark { url: url.to_string(), title: title_of(url) });
    b.history.truncate(60);
}

/// Where the browser opens and the home button goes — the saved homepage, or
/// YouTube when none is set (the browser's reason to exist).
fn homepage(core: &crate::core::EngineCore) -> String {
    let set = core.shared.config.read().browser.homepage.clone();
    if set.trim().is_empty() { "https://www.youtube.com/".to_string() } else { set }
}

/// Build the favorites page's data: the pinned sites, the visited pages (newest
/// first), and the saved homepage. Mirrors iOS/Android — bookmarks + 방문기록, no
/// "자주 방문" tiles.
fn browser_home_json(b: &crate::config::Browser) -> String {
    let bookmarks: Vec<String> = b
        .bookmarks
        .iter()
        .map(|m| format!(r#"{{"url":"{}","title":"{}"}}"#, escape(&m.url), escape(&m.title)))
        .collect();
    let history: Vec<String> = b
        .history
        .iter()
        .map(|m| format!(r#"{{"url":"{}","title":"{}"}}"#, escape(&m.url), escape(&m.title)))
        .collect();

    format!(
        r#"{{"t":"start","bookmarks":[{}],"history":[{}],"homepage":"{}"}}"#,
        bookmarks.join(","),
        history.join(","),
        escape(&b.homepage)
    )
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
    /// When the periodic work last ran, so it runs on a beat rather than once
    /// per window message. See `run`.
    beat: std::cell::Cell<std::time::Instant>,
    /// Whether the window was filling the screen last time the page was told.
    /// A window with no edge outside it has no edge to take hold of, and the
    /// page has no way of knowing that by itself.
    said_zoomed: std::cell::Cell<bool>,
    /// The number the next tab opened will carry. See [`Tab::id`].
    next_tab: std::cell::Cell<u64>,
    /// The last thing the download list was told, so an unchanged list is not
    /// sent again. Each send crosses into the page and makes it lay out a list
    /// that looks exactly as it already did.
    said_downloads: RefCell<String>,
}

/// One tab: the page, and what the strip needs to draw it.
pub struct Tab {
    /// Its own number for as long as it is open, which is what the page in it
    /// puts on everything it says. Not its place in the strip: that shifts when
    /// a tab to its left is closed, and reports would land on a stranger.
    id: u64,
    view: Rc<wry::WebView>,
    pub title: String,
    pub url: String,
}

impl Shell {
    /// Run until the window is closed, answering the page as it asks.
    ///
    /// `tick` is called on a beat — a few times a second, whether or not
    /// anything is being pressed — for the work that has to happen anyway:
    /// the tray, and what the running downloads have to say.
    ///
    /// On a beat rather than after every message. Windows sends one for every
    /// pixel the pointer moves, and the tick tells the page how the downloads
    /// stand; while something was downloading, moving the mouse across the
    /// window put hundreds of those a second into the page's own thread. That
    /// is what made the window crawl and the pointer flicker between shapes as
    /// soon as a download had been started.
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
            // What the page asked for, straight away: these arrive by hand and
            // waiting for the next beat to answer them would be felt.
            self.pump();
            if self.beat.get().elapsed() >= BEAT {
                self.beat.set(std::time::Instant::now());
                tick(self);
            }
        }
    }

    /// Deal with whatever the pages have said since last time.
    pub fn pump(&self) {
        // Whether the window still has an edge, when that changes. Read here
        // rather than announced from the places that maximise it: the system
        // does it too — a double click on the strip, a snap to the side of the
        // screen — and this is the one place that hears about all of them.
        let zoomed = unsafe { IsZoomed(self.hwnd) } != 0;
        if zoomed != self.said_zoomed.get() {
            self.said_zoomed.set(zoomed);
            self.tell(&format!(r#"{{"t":"frame","zoomed":{zoomed}}}"#));
        }

        // What the sites being browsed had to say first: an address that moved,
        // and the answers the download hooks send back.
        for event in self.page_events() {
            match event {
                // Nothing is written from here. This fires for whichever page
                // is loading, which is not necessarily the page in front, and
                // the report carries no way to tell which tab it came from —
                // taking it as the front tab's was the other half of the label
                // flickering between sites. Each page reports its own address
                // and names itself when it does.
                // The URL bar is each page's own business; nothing to do on a plain navigate.
                crate::download::browser::Event::Navigated(_) => {}
                // The back/forward list changed (navigation, shorts pushState, or a
                // GoBack/GoForward) — re-check the front tab's arrows so they grey out with
                // nowhere to go and light up once there is somewhere.
                crate::download::browser::Event::HistoryChanged => self.say_nav(),
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
                Ask::WindowResize(edge) => self.resize_from(&edge, true),
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
            // Whichever tab said it, not whichever tab is in front. Every page
            // reports its title and address a few times a second, so reading
            // these as the front tab's meant a page loading in the background
            // rewrote the label of the tab being looked at — the name flickering
            // between two sites.
            let said_by = field(payload, "tab").and_then(|id| id.parse::<u64>().ok());
            let Some(said_by) = said_by else { return };
            let changed = {
                let mut tabs = self.tabs.borrow_mut();
                let held = tabs.iter_mut().find(|tab| tab.id == said_by);
                match (frame.as_str(), held) {
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
            self.answer(Ask::Chose { itag: itag as u64, anyway: crate::downloads::forced(payload) });
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
            "{}\n{}\n{}\n{}",
            crate::download::browser::PAGE_HOOKS,
            crate::download::youtube::AD_STRIP,
            crate::download::youtube::RECORDER,
            crate::download::youtube::CONTROL,
        );
        // Never reused, so a report from a page that is closing cannot land on
        // the tab that took its place.
        let id = self.next_tab.get();
        self.next_tab.set(id + 1);
        match crate::download::browser::new_view(self.hwnd, url, &startup, &self.to_pages, id) {
            Ok(view) => {
                let view = Rc::new(view);
                watch_process(&view, url);
                TABS.with(|cell| cell.borrow_mut().push(view.clone()));
                self.tabs.borrow_mut().push(Tab {
                    id,
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
        self.say_nav();   // the arrows belong to the tab now in front
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

    /// Push whether the FRONT tab can step back or forward, so the toolbar can grey the
    /// arrows out when there is nowhere to go — they looked pressable with no history to
    /// walk. Re-queried on every navigation and tab switch; asking the front tab each time
    /// is correct even when a background tab is what navigated (its answer is unchanged).
    pub fn say_nav(&self) {
        use wry::WebViewExtWindows;
        let view = self
            .showing
            .get()
            .and_then(|at| self.tabs.borrow().get(at).map(|t| t.view.clone()));
        let (mut back, mut forward) = (false, false);
        if let Some(view) = view {
            let (mut b, mut f) = (windows::core::BOOL(0), windows::core::BOOL(0));
            unsafe {
                let wv = view.webview();
                let _ = wv.CanGoBack(&mut b);
                let _ = wv.CanGoForward(&mut f);
            }
            back = b.as_bool();
            forward = f.as_bool();
        }
        self.tell(&format!(r#"{{"t":"nav","back":{back},"forward":{forward}}}"#));
    }

    /// Do something to the tab in front: go somewhere, or step through history.
    pub fn steer(&self, what: &str, url: &str) {
        let view = {
            let tabs = self.tabs.borrow();
            self.showing.get().and_then(|at| tabs.get(at).map(|t| t.view.clone()))
        };
        let Some(view) = view else { return };
        use wry::WebViewExtWindows;
        match what {
            "go" => { let _ = view.load_url(url); }
            // Native WebView2 history rather than history.back()/forward() via script: the
            // script form did nothing on the pages that most need it — cross-origin
            // navigations (the JS history is per-origin) and SPA sites that rewrite their own
            // history. GoBack/GoForward walk the WebView's real back/forward list, which is
            // what the toolbar arrows are supposed to do.
            "back" => unsafe { let _ = view.webview().GoBack(); }
            "forward" => unsafe { let _ = view.webview().GoForward(); }
            _ => { let _ = view.reload(); }
        }
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
                    r#"{{"id":{},"key":"{}","title":"{}","folder":"{}","size":"{}","age":"{}","cover":{}}}"#,
                    register_media(&item.path),
                    // Something that means the same file next time the program
                    // runs: the number does not, and playback positions kept
                    // against it pointed at whatever took that number later.
                    crate::library::key(&item.path),
                    escape(&item.title),
                    escape(&item.folder),
                    escape(&crate::library::human(item.bytes)),
                    escape(&crate::library::age(item.saved_at)),
                    // The picture is inside the file, so it is the same number
                    // the file is asked for — under `/cover/` rather than
                    // `/media/`, which is what says to take it out.
                    if item.cover { register_media(&item.path) } else { 0 },
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

    /// Keep, or give back, the room along the bottom for the playing strip.
    pub fn now_playing(&self, on: bool) {
        if PLAYING.with(|cell| cell.get()) == on {
            return;
        }
        PLAYING.with(|cell| cell.set(on));
        self.lay_out();
    }

    /// Forget what the page has been told, so the next telling goes through.
    pub fn forget_what_was_said(&self) {
        self.said_downloads.borrow_mut().clear();
    }

    /// Tell the page what is being fetched, so the home screen can show it.
    pub fn say_downloads(&self, downloads: &crate::downloads::Downloads) {
        let rows: Vec<String> = downloads
            .list
            .iter()
            .map(|job| {
                format!(
                    r#"{{"id":{},"title":"{}","fraction":{:.4},"done":{},"total":{},"speed":{},"elapsed":{},"done_state":{}}}"#,
                    job.id,
                    escape(&job.title),
                    job.fraction(),
                    job.done,
                    job.total,
                    job.speed_bps(),
                    job.elapsed_ms,
                    matches!(job.state, crate::downloads::State::Done)
                )
            })
            .collect();
        let message = format!(r#"{{"t":"downloads","list":[{}]}}"#, rows.join(","));
        // Only when it reads differently. Nothing downloading means the same
        // empty list over and over, and a stalled one means the same numbers.
        if *self.said_downloads.borrow() == message {
            return;
        }
        *self.said_downloads.borrow_mut() = message.clone();
        // Written down when the list changes, which is rarely: whether the home
        // screen ever heard about a download is the first question when its bar
        // is not there, and it cannot be answered from this side otherwise.
        tracing::info!("downloads: {}", message);
        self.tell(&message);
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

    /// Hand a drag on the window's own edge to the system, the same way.
    ///
    /// The page says which edge because the page is what the pointer is over: a
    /// web view fills the window, and a window whose whole face is a child
    /// window never hears a press on its frame. Told which edge it was, this is
    /// the same gesture the system would have started itself.
    fn resize_from(&self, edge: &str, from_shell: bool) {
        // A window filling the screen has no edge to pull.
        if unsafe { IsZoomed(self.hwnd) } != 0 {
            return;
        }
        // The shell's page is only the strip at the top while a site is in
        // front — unless something is playing, which gives it the window again.
        // Its bottom is not the window's bottom then, and a press near it means
        // nothing about the frame.
        if from_shell
            && edge.starts_with('b')
            && self.showing.get().is_some()
            && !PLAYING.with(|cell| cell.get())
        {
            return;
        }
        let corner = match edge {
            "t" => HTTOP,
            "b" => HTBOTTOM,
            "l" => HTLEFT,
            "r" => HTRIGHT,
            "tl" => HTTOPLEFT,
            "tr" => HTTOPRIGHT,
            "bl" => HTBOTTOMLEFT,
            "br" => HTBOTTOMRIGHT,
            _ => return,
        };
        unsafe {
            ReleaseCapture();
            SendMessageW(self.hwnd, WM_NCLBUTTONDOWN, corner as usize, 0);
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
    // Let a lower-integrity copy reach this window. When Shard runs elevated,
    // Windows' message filter (UIPI) drops messages from an unelevated process
    // by default — so a media file double-clicked from Explorer, which starts
    // an unelevated copy, could not hand its path to the elevated one and a
    // second window opened instead. These two are the messages that handoff
    // uses: the file path, and the request to come to the front.
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            ChangeWindowMessageFilterEx, MSGFLT_ALLOW, WM_COPYDATA,
        };
        ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, std::ptr::null_mut());
        let wake = uikit::single::wake_message(crate::config::APP_NAME);
        ChangeWindowMessageFilterEx(hwnd, wake, MSGFLT_ALLOW, std::ptr::null_mut());
    }
    centre(hwnd);
    dark_title_bar(hwnd);
    // A beat of its own. The loop only turns when a message arrives, and a
    // download reports on another thread — without this the bar sat still and a
    // finished download went unnoticed until something was clicked.
    unsafe { SetTimer(hwnd, TICK_TIMER, 250, None) };
    Ok(hwnd)
}

/// Put the window in the middle of the screen it is opening on.
///
/// Windows only honours `CW_USEDEFAULT` for windows that have a caption of their
/// own to cascade. This one does not, so it was being put at the very corner of
/// the display every time it was launched.
///
/// The work area rather than the whole screen: the middle of the space a window
/// may actually use, which is not the middle of the glass when the taskbar is
/// along one side.
fn centre(hwnd: HWND) {
    let mut work = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    let got = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            (&mut work as *mut RECT).cast(),
            0,
        )
    };
    if got == 0 {
        return;
    }
    let mut window = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    unsafe { GetWindowRect(hwnd, &mut window) };
    let width = window.right - window.left;
    let height = window.bottom - window.top;
    // Never off the top or the left, however small the work area is: a title bar
    // above the screen cannot be taken hold of.
    let left = (work.left + (work.right - work.left - width) / 2).max(work.left);
    let top = (work.top + (work.bottom - work.top - height) / 2).max(work.top);
    unsafe {
        SetWindowPos(hwnd, std::ptr::null_mut(), left, top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
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
    // A second copy of the program, started while this one is in the tray, posts
    // this to ask us to come to the front instead of putting up a message box.
    // The id is handed out by RegisterWindowMessageW, so it is not a compile-time
    // constant and cannot sit in the match below; it is cached because a value
    // that never changes should not be re-registered on every message.
    static WAKE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let wake = *WAKE.get_or_init(|| uikit::single::wake_message(crate::config::APP_NAME));
    if message == wake {
        unsafe {
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
        }
        return 0;
    }

    // A second copy handed us a media file to play (a double-click while this
    // one was already running). Take the path out of the copy-data, remember it
    // for `/external`, and come to the front; the run loop plays it.
    if message == WM_COPYDATA {
        let data = lparam as *const CopyData;
        if !data.is_null() && unsafe { (*data).kind } == SHARD_OPEN_FILE {
            let bytes = unsafe { (*data).len } as usize;
            let ptr = unsafe { (*data).data } as *const u16;
            if !ptr.is_null() && bytes >= 2 {
                let units = std::slice::from_raw_parts(ptr, bytes / 2);
                let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
                let path = String::from_utf16_lossy(&units[..end]);
                set_opened_file(std::path::PathBuf::from(path));
                unsafe {
                    ShowWindow(hwnd, SW_SHOW);
                    SetForegroundWindow(hwnd);
                }
            }
        }
        return 1;
    }

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
    fn a_picture_inside_a_file_can_be_asked_for_on_its_own() {
        // The whole way through: a file with a cover in it, registered the way
        // the library registers one, and asked for down the address the page
        // uses. A row showed an empty frame when any step of this was wrong,
        // and nothing on screen said which.
        let dir = std::env::temp_dir().join("shard-cover-route");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("song.m4a");

        let mut fake = crate::download::mp4::tests_fixture();
        let picture = vec![0xff, 0xd8, 0xff, 0xe0, 9, 9, 9];
        fake = crate::download::mp4::with_cover(&fake, &picture, "jpg").expect("a header");
        std::fs::write(&file, &fake).expect("write");

        let id = register_media(&file);
        let answer = respond(&format!("shard://shard.localhost/cover/{id}"), None);
        assert_eq!(answer.status(), 200);
        assert_eq!(answer.headers()["Content-Type"], "image/jpeg");
        assert_eq!(answer.body().as_ref(), picture.as_slice());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_files_lasting_name_is_the_same_one_next_time() {
        // Kept beside the files it names now, so the shelf's own order and the
        // page's resume positions are written under one name.
        use crate::library::key;
        let path = std::path::Path::new("C:/Videos/Shard/노래.webm");
        assert_eq!(key(path), key(path));
        assert_ne!(key(path), key(std::path::Path::new("C:/Videos/Shard/x.webm")));
    }

    #[test]
    fn text_on_its_way_into_a_script_cannot_end_it_early() {
        assert_eq!(escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(escape("two\nlines"), "two\\nlines");
    }
}
