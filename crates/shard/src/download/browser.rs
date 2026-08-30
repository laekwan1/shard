//! The download window.
//!
//! Shard's engine works on packets and never sees a page. That is fine for
//! making a site reachable and useless for saving what it plays: a media
//! request needs the address, the headers and — for YouTube — a whole captured
//! request that only a running page can produce.
//!
//! So this opens a window with a browser in it. Not a browser we ship: Windows
//! already has WebView2, the same engine Edge runs on, and this binds to it.
//!
//! It runs on its own thread with its own message loop. The settings window is
//! an egui application that owns the main thread's event loop, and two event
//! loops in one thread is not a thing Windows offers. A window belongs to the
//! thread that created it, so giving this one its own thread is the whole of
//! the arrangement — no locking, no shared state, just messages in and out.

use anyhow::{anyhow, Result};
use raw_window_handle::{HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
use std::num::NonZeroIsize;
use std::sync::mpsc::{self, Receiver, Sender};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::Graphics::Gdi::{CreateSolidBrush, InvalidateRect};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// What the page tells the app about.
#[derive(Debug, Clone)]
pub enum Event {
    /// A page finished loading, or navigated within itself.
    Navigated(String),
    /// The back/forward list changed — real navigation, SPA pushState, or a GoBack/GoForward.
    /// This is what the toolbar arrows key off, since the navigation handler misses the last
    /// two.
    HistoryChanged,
    /// The page answered a request for what it can offer.
    Offer(String),
    /// The window was closed.
    Closed,
}

/// What the app asks the window to do.
#[derive(Debug, Clone)]
pub enum Command {
    Navigate(String),
    /// Run a script in the page — how the app asks a page what it has.
    Evaluate(String),
    /// Bring the address bar down, or send it away.
    Toolbar(bool),
    /// What a tab should say, and which tab said it.
    ///
    /// Carrying the tab is the whole point. Every page keeps reporting its
    /// title while it is open, background ones included, and applying whatever
    /// arrived to whichever tab happened to be in front meant a tab in the back
    /// renamed the one in front every couple of seconds.
    TabTitle(u64, String),
    /// A page's background, for its own tab to match.
    PageColour(u64, u32),
    /// Where a tab is, so switching to it shows its address and not the last
    /// one typed anywhere.
    TabUrl(u64, String),
    /// Open another tab at this address.
    NewTab(String),
    /// Nothing to do in the loop; the caller raises the window itself.
    Focus,
    Close,
}

/// A running download window.
pub struct Window {
    commands: Sender<Command>,
    /// Posted to the window's thread so it wakes and drains `commands`.
    hwnd: isize,
}

impl Window {
    pub fn send(&self, command: Command) -> Result<()> {
        self.commands.send(command).map_err(|_| anyhow!("창이 닫혔습니다"))?;
        // The message loop is asleep until something arrives for it.
        unsafe { PostMessageW(self.hwnd as HWND, WM_APP_COMMAND, 0, 0) };
        Ok(())
    }

    pub fn navigate(&self, url: &str) -> Result<()> {
        self.send(Command::Navigate(url.into()))
    }

    /// Bring the window forward. Fails once its thread has gone, which is how
    /// the caller learns the window was closed without watching for the event.
    pub fn focus(&self) -> Result<()> {
        if self.commands.send(Command::Focus).is_err() {
            return Err(anyhow!("창이 닫혔습니다"));
        }
        let hwnd = self.hwnd as HWND;
        if unsafe { IsWindow(hwnd) } == 0 {
            return Err(anyhow!("창이 닫혔습니다"));
        }
        unsafe {
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        }
        Ok(())
    }
}

/// Where the home button goes.
const HOME: &str = "https://www.google.com/";

/// Injected into every page so the frame can hear from it.
///
/// The frame cannot see a click on the page: the web view is a child window and
/// takes the mouse before its parent is told anything. So the page says when it
/// was clicked, and the address bar goes away — which is how a browser behaves
/// and is not otherwise reachable from here.
pub(crate) const PAGE_HOOKS: &str = r#"
(function () {
  if (window.__shardFrame) return;
  window.__shardFrame = true;
  // Every report names the tab it came from. The id is injected ahead of this
  // script, one value per page, and it is a string so the reader on the other
  // side needs no second way of pulling a field out.
  function say(what) {
    try {
      what.tab = String(window.__shardTab || 0);
      window.ipc.postMessage(JSON.stringify(what));
    } catch (e) {}
  }
  // The frame cannot see the pointer over the page, so the page says when it
  // arrives — that is what "the mouse left the tabs" means from up there.
  // The page is no longer asked to dismiss anything: the address is a row of
  // the window rather than something resting on the page, so a click here means
  // only what it means to the page.
  function title() { if (document.hidden) return; say({ frame: 'title', text: document.title || location.host }); }
  // The page's own background, so the tab above it can be the same colour.
  function colour() {
    if (document.hidden) return;   // hidden tab: skip the layout read (battery)
    try {
      var seen = '';
      var nodes = [document.body, document.documentElement];
      for (var i = 0; i < nodes.length; i++) {
        if (!nodes[i]) continue;
        var found = getComputedStyle(nodes[i]).backgroundColor;
        if (found && found !== 'rgba(0, 0, 0, 0)' && found !== 'transparent') {
          seen = found;
          break;
        }
      }
      // A page that sets nothing is drawn on white, so that is what it is —
      // reporting nothing left the frame dark against a white page.
      var parts = /(\d+),\s*(\d+),\s*(\d+)/.exec(seen);
      var rgb = parts ? parts[1] + ',' + parts[2] + ',' + parts[3] : '255,255,255';
      say({ frame: 'colour', text: rgb });
    } catch (e) {}
  }
  // A click anywhere counts, embedded players included, so the dismissal above
  // is left alone. What the page *is* — its title and its colour — belongs to
  // the top frame only. This script is injected into every frame, so an advert
  // in an iframe was reporting its own background every three seconds and
  // winning: the tab followed the page, then turned black and stayed there.
  // Where the tab is. Reported from the page rather than from the navigation
  // handler: that handler fires for every page in the window, so a background
  // tab loading something wrote its address into the bar belonging to the tab
  // in front. A page can only ever report its own.
  var told = '';
  function here() {
    if (document.hidden) return;
    if (location.href === told) return;
    told = location.href;
    // A new page (or a single-page move to a new video): let the recorder mark
    // the next videoplayback again, so the spinner is measured per video, not once.
    window.__shardSabrMarked = false;
    say({ frame: 'url', text: told });
  }
  if (window.top === window) {
    window.addEventListener('load', colour);
    setTimeout(colour, 900);
    setInterval(colour, 3000);
    document.addEventListener('DOMContentLoaded', title);
    window.addEventListener('load', title);
    setTimeout(title, 800);
    // Diagnostic: the moment the HTML is parsed, timestamped by Rust. Its gap from
    // "nav:" is the page-load time; the gap from there to "page mark: sabr" is the
    // player start. Splits the spinner into network vs player. Remove once settled.
    document.addEventListener('DOMContentLoaded', function () { say({ mark: 'dom' }); });
    // Single-page sites change their title, and their address, without loading
    // anything.
    setInterval(title, 2000);
    here();
    setInterval(here, 1000);
  }
})();
"#;

/// Posted when a command is waiting, since a message loop cannot poll.
const WM_APP_COMMAND: u32 = WM_APP + 1;

const BAR_CLASS: &[u16] = &[
    b'S' as u16, b'h' as u16, b'a' as u16, b'r' as u16, b'd' as u16, b'B' as u16, b'a' as u16,
    b'r' as u16, 0,
];

const CLASS: &[u16] = &[
    b'S' as u16, b'h' as u16, b'a' as u16, b'r' as u16, b'd' as u16, b'W' as u16, b'e' as u16,
    b'b' as u16, 0,
];

/// Open the window on a thread of its own.
///
/// `at_document_start` is injected into every page before the page's own
/// scripts run. That timing is not a nicety: the first attempt injected on load
/// instead, by which point YouTube's player had already taken its own reference
/// to `fetch`, so replacing the global changed nothing and the request went
/// unseen.
pub fn open(
    start_url: &str,
    at_document_start: &str,
    title: &str,
) -> Result<(Window, Receiver<Event>)> {
    let (commands, command_rx) = mpsc::channel::<Command>();
    let (events, event_rx) = mpsc::channel::<Event>();
    let (ready, ready_rx) = mpsc::channel::<Result<isize, String>>();

    let start_url = start_url.to_string();
    let script = at_document_start.to_string();
    let title = title.to_string();

    std::thread::Builder::new().name("shard-browser".into()).spawn(move || {
        match run(&start_url, &script, &title, command_rx, events.clone(), &ready) {
            Ok(()) => {}
            Err(e) => {
                // If it failed before reporting readiness the caller is still
                // waiting; if after, the channel is closed and this is a no-op.
                let _ = ready.send(Err(e.to_string()));
            }
        }
        // Logged because closing this window has, more than once, taken the
        // whole program with it — and knowing whether this thread ended first
        // or last is the difference between two very different causes.
        tracing::info!("browser thread ending");
        let _ = events.send(Event::Closed);
    })?;

    match ready_rx.recv() {
        Ok(Ok(hwnd)) => Ok((Window { commands, hwnd }, event_rx)),
        Ok(Err(e)) => Err(anyhow!(e)),
        Err(_) => Err(anyhow!("창을 열지 못했습니다")),
    }
}

fn run(
    start_url: &str,
    at_document_start: &str,
    title: &str,
    commands: Receiver<Command>,
    events: Sender<Event>,
    ready: &Sender<Result<isize, String>>,
) -> Result<()> {
    let hwnd = create_window(title)?;

    let mut startup = String::from(PAGE_HOOKS);
    startup.push_str("\n");
    startup.push_str(at_document_start);

    let id = claim_tab_id();
    let first = new_view(hwnd, start_url, &startup, &events, id)?;
    VIEWS.with(|cell| cell.borrow_mut().push(first));
    TABS.with(|cell| cell.borrow_mut().push(Tab::new(id)));
    // After the first page, so it sits above it. A child made before its
    // siblings starts underneath them, and the toolbar that appeared behind the
    // page looked like a toolbar that never appeared at all.
    create_toolbar(hwnd)?;

    fit(hwnd);
    unsafe { ShowWindow(hwnd, SW_SHOW) };
    // Again after it is on screen: the attributes set before a window is shown
    // are the ones Windows is most willing to forget.
    dark_title_bar(hwnd);
    ready.send(Ok(hwnd as isize)).ok();

    // A plain message loop. `GetMessageW` blocks until something arrives, so
    // an idle window costs nothing.
    let mut message = unsafe { std::mem::zeroed::<MSG>() };
    loop {
        let got = unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
        if got <= 0 {
            break;
        }
        let count = VIEWS.with(|cell| cell.borrow().len());
        let active = ACTIVE.with(|cell| cell.get()).min(count.saturating_sub(1));
        match message.message {
            WM_APP_COMMAND => {
                while let Ok(command) = commands.try_recv() {
                    match command {
                        Command::Navigate(url) => {
                            VIEWS.with(|cell| {
                                if let Some(view) = cell.borrow().get(active) {
                                    let _ = view.load_url(&url);
                                }
                            });
                        }
                        Command::Evaluate(script) => {
                            VIEWS.with(|cell| {
                                if let Some(view) = cell.borrow().get(active) {
                                    let _ = view.evaluate_script(&script);
                                }
                            });
                        }
                        Command::Toolbar(down) => set_toolbar(down),
                        Command::TabTitle(id, text) => {
                            let changed = TABS.with(|cell| {
                                let mut held = cell.borrow_mut();
                                match held.iter_mut().find(|tab| tab.id == id) {
                                    Some(tab) if tab.title != text => {
                                        tab.title = text;
                                        true
                                    }
                                    _ => false,
                                }
                            });
                            if changed {
                                unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
                            }
                        }
                        Command::PageColour(id, colour) => {
                            let changed = TABS.with(|cell| {
                                let mut held = cell.borrow_mut();
                                match held.iter_mut().find(|tab| tab.id == id) {
                                    Some(tab) if tab.colour != colour => {
                                        tab.colour = colour;
                                        true
                                    }
                                    _ => false,
                                }
                            });
                            if changed {
                                // The frame follows the tab in front; a tab in
                                // the back still repaints, because its own tab
                                // carries its colour.
                                repaint_all(hwnd);
                            }
                        }
                        Command::TabUrl(id, url) => {
                            let front = TABS.with(|cell| {
                                let mut held = cell.borrow_mut();
                                let Some(at) = held.iter().position(|tab| tab.id == id) else {
                                    return false;
                                };
                                held[at].url = url.clone();
                                at == active
                            });
                            // Only the tab being looked at writes the address,
                            // and not while it is being typed into — a page
                            // settling on its final address a second later
                            // would otherwise wipe out a half-typed one.
                            let edit = ADDRESS.with(|cell| cell.get());
                            let typing = !edit.is_null()
                                && unsafe {
                                    windows_sys::Win32::UI::Input::KeyboardAndMouse::GetFocus()
                                } == edit;
                            if front && !typing {
                                show_address(&url);
                            }
                        }
                        Command::NewTab(url) => {
                            let id = claim_tab_id();
                            if let Ok(view) = new_view(hwnd, &url, &startup, &events, id) {
                                let last = VIEWS.with(|cell| {
                                    cell.borrow_mut().push(view);
                                    cell.borrow().len() - 1
                                });
                                TABS.with(|cell| {
                                    let mut tab = Tab::new(id);
                                    tab.url = url.clone();
                                    cell.borrow_mut().push(tab);
                                });
                                ACTIVE.with(|cell| cell.set(last));
                                show_address(&url);
                                repaint_all(hwnd);
                            }
                        }
                        Command::Focus => {}
                        // Asked to close rather than closing here: the pages
                        // have to be let go before the window goes, and that
                        // ordering lives in one place.
                        Command::Close => unsafe {
                            PostMessageW(hwnd, WM_CLOSE, 0, 0);
                        },
                    }
                }
            }

            WM_SIZE_REQUEST => fit(hwnd),

            WM_NAVIGATE => {
                if let Some(url) = typed_address() {
                    VIEWS.with(|cell| {
                        if let Some(view) = cell.borrow().get(active) {
                            let _ = view.load_url(&url);
                        }
                    });
                }
            }

            WM_BROWSE => {
                VIEWS.with(|cell| {
                    let held = cell.borrow();
                    let Some(view) = held.get(active) else { return };
                    if message.wParam == BROWSE_HOME {
                        let _ = view.load_url(HOME);
                    } else {
                        // Through the page's own history rather than a browser
                        // API: it is one line, it is what the buttons mean, and
                        // it behaves the same on a site that rewrites its own
                        // history as on one that does not.
                        let script = match message.wParam {
                            BROWSE_BACK => "history.back()",
                            BROWSE_FORWARD => "history.forward()",
                            _ => "location.reload()",
                        };
                        let _ = view.evaluate_script(script);
                    }
                });
            }

            WM_TOOLBAR => set_toolbar(message.wParam != 0),

            WM_TAB => {
                let which = message.lParam as usize;
                match message.wParam {
                    TAB_SELECT => {
                        if which < count && which != active {
                            ACTIVE.with(|cell| cell.set(which));
                            // The tab brings its own address and its own colour
                            // with it. Without this the bar still held whatever
                            // was last typed anywhere, which read as one tab
                            // having taken another's address.
                            let url = TABS.with(|cell| {
                                cell.borrow().get(which).map(|tab| tab.url.clone())
                            });
                            show_address(url.as_deref().unwrap_or(""));
                            // The bar is a window of its own and repaints only
                            // when it is told to. Repainting just the frame
                            // left the address box in the previous tab's colour
                            // while everything around it had changed.
                            repaint_all(hwnd);
                        }
                        // Choosing a tab is asking what it is showing, so the
                        // address comes with it rather than needing a second
                        // move of the pointer.
                        set_toolbar(true);
                    }
                    TAB_CLOSE => {
                        // The last tab is not closed: a browser with no page in
                        // it is a window with nothing to do, and the window's
                        // own close button is right there.
                        if count > 1 && which < count {
                            VIEWS.with(|cell| {
                                cell.borrow_mut().remove(which);
                            });
                            TABS.with(|cell| {
                                cell.borrow_mut().remove(which);
                            });
                            let next = if active >= which && active > 0 { active - 1 } else { active };
                            let next = next.min(count - 2);
                            ACTIVE.with(|cell| cell.set(next));
                            let url =
                                TABS.with(|cell| cell.borrow().get(next).map(|t| t.url.clone()));
                            show_address(url.as_deref().unwrap_or(""));
                            repaint_all(hwnd);
                        }
                    }
                    TAB_NEW => {
                        let id = claim_tab_id();
                        if let Ok(view) = new_view(hwnd, HOME, &startup, &events, id) {
                            let last = VIEWS.with(|cell| {
                                cell.borrow_mut().push(view);
                                cell.borrow().len() - 1
                            });
                            TABS.with(|cell| cell.borrow_mut().push(Tab::new(id)));
                            ACTIVE.with(|cell| cell.set(last));
                            show_address("");
                            repaint_all(hwnd);
                        }
                    }
                    _ => {}
                }
            }

            _ => unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            },
        }
    }
    Ok(())
}

/// One page, as a child of the frame.
///
/// Reachable from the shell as well: the one window builds its tabs with the
/// same call, so the hooks a page is fitted with — and the SABR capture that
/// rides on them — are the same ones, not a second copy that has to be kept in
/// step.
pub(crate) fn new_view(
    hwnd: HWND,
    url: &str,
    startup: &str,
    events: &Sender<Event>,
    id: u64,
) -> Result<wry::WebView> {
    let host = Host(hwnd);
    let ipc = events.clone();
    let navigated = events.clone();
    // The tab's id, ahead of everything else, so the hooks below it can name
    // themselves in every message they send.
    let startup = format!("window.__shardTab = {id};\n{startup}");
    let view = wry::WebViewBuilder::new()
        .with_url(url)
        .with_initialization_script(&startup)
        .with_ipc_handler(move |request| {
            let _ = ipc.send(Event::Offer(request.into_body()));
        })
        .with_navigation_handler(move |url| {
            // The address is not written from here. This fires for whichever
            // page is loading, which is not necessarily the page being looked
            // at, and a tab loading in the background was rewriting the address
            // of the tab in front. The page reports its own instead.
            let _ = navigated.send(Event::Navigated(url));
            true
        })
        // DevTools on so the site view can be inspected too (F12 / Inspect).
        .with_devtools(true)
        .build_as_child(&host)
        .map_err(|e| anyhow!("WebView2를 시작하지 못했습니다: {e}"))?;

    // WebView2's HistoryChanged fires on EVERY change to the back/forward list — a real
    // navigation, an SPA pushState (YouTube shorts stepping through videos), and our own
    // GoBack/GoForward — which is exactly when the toolbar arrows must be re-checked. The
    // navigation handler above catches none of the last two, so relying on it left the arrows
    // stale (back never lit on shorts; forward never lit after going back).
    {
        use wry::WebViewExtWindows;
        let hist = events.clone();
        let handler = webview2_com::HistoryChangedEventHandler::create(Box::new(move |_, _| {
            let _ = hist.send(Event::HistoryChanged);
            Ok(())
        }));
        let mut token = 0i64;
        if let Err(e) = unsafe { view.webview().add_HistoryChanged(&handler, &mut token) } {
            tracing::warn!("could not watch history for back/forward state: {e}");
        }
    }

    // Block ad/tracker HOSTS at the network level — the same list iOS blocks
    // (WKContentRuleList) and Android blocks in shouldInterceptRequest. The DOM
    // skipAds only clicks a skip button after an ad has started; this stops the ad
    // request itself. Never the video CDN (googlevideo.com) or youtube.com, so a
    // pattern that stops matching brings ads back without breaking playback.
    install_ad_block(&view);

    Ok(view)
}

/// The ad/tracker host and path fragments to block — identical to iOS/Android.
const AD_PATTERNS: [&str; 9] = [
    "doubleclick.net",
    "googlesyndication.com",
    "googleadservices.com",
    "google-analytics.com",
    "googletagservices.com",
    "googletagmanager.com",
    "/pagead/",
    "/ptracking",
    "/api/stats/ads",
];

fn is_ad_url(url: &str) -> bool {
    AD_PATTERNS.iter().any(|p| url.contains(p))
}

/// Whether ad requests are actually failed. The filter and handler are always
/// installed; this decides, per request, whether to block — so the setting takes
/// effect on tabs already open, not only new ones. On by default.
static AD_BLOCK_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_ad_block(on: bool) {
    AD_BLOCK_ON.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn is_ad_block_on() -> bool {
    AD_BLOCK_ON.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether AD_STRIP (the player-response strip that removes the video ad) is
/// injected into new tabs. Separate from the 403 block because this is the part
/// YouTube detects and penalises with a delay. Injected at document-start, so it
/// only takes effect on tabs opened after a change — the 403 flag above is the
/// live one.
static AD_STRIP_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_ad_strip(on: bool) {
    AD_STRIP_ON.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn is_ad_strip_on() -> bool {
    AD_STRIP_ON.load(std::sync::atomic::Ordering::Relaxed)
}

/// Attach a WebView2 WebResourceRequested filter that fails ad requests with an
/// empty 403, so they never load. Best-effort: a failure to install just leaves
/// the DOM skipAds as the only defense, as before.
fn install_ad_block(view: &wry::WebView) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL, ICoreWebView2_2,
    };
    use webview2_com::WebResourceRequestedEventHandler;
    use windows::core::{w, Interface, HSTRING};
    use wry::WebViewExtWindows;

    let wv = view.webview();
    // Filter by ad pattern, not "*". A "*" filter routes EVERY request — scripts,
    // images, and above all the video's own media stream — through this host
    // callback. That delayed the page and, worse, the video start (the long
    // spinner), and because the stream was held back the SABR request the recorder
    // waits for arrived late too, so a download begun during the spinner got no
    // audio and failed. Registering one filter per ad pattern means the callback
    // fires only for the handful of URLs that might be ads; the video stream is
    // never touched. Wrapped in '*…*' because the filter matches the whole URL.
    unsafe {
        for pattern in AD_PATTERNS {
            let filter = HSTRING::from(format!("*{pattern}*"));
            if let Err(e) =
                wv.AddWebResourceRequestedFilter(&filter, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)
            {
                tracing::warn!("could not set ad-block filter {pattern}: {e}");
            }
        }
    }
    let handler = WebResourceRequestedEventHandler::create(Box::new(move |wv, args| {
        let (Some(wv), Some(args)) = (wv, args) else { return Ok(()) };
        unsafe {
            let request = args.Request()?;
            let mut uri = windows::core::PWSTR::null();
            request.Uri(&mut uri)?;
            let url = uri.to_string().unwrap_or_default();
            if AD_BLOCK_ON.load(std::sync::atomic::Ordering::Relaxed) && is_ad_url(&url) {
                // An empty 403 in place of the ad. The response is built from the
                // environment, reached through ICoreWebView2_2. No content stream.
                if let Ok(env) = wv.cast::<ICoreWebView2_2>().and_then(|w2| w2.Environment()) {
                    let no_content: Option<&windows::Win32::System::Com::IStream> = None;
                    if let Ok(resp) = env.CreateWebResourceResponse(no_content, 403, w!("Blocked"), w!("")) {
                        let _ = args.SetResponse(&resp);
                    }
                }
            }
        }
        Ok(())
    }));
    let mut token = 0i64;
    if let Err(e) = unsafe { wv.add_WebResourceRequested(&handler, &mut token) } {
        tracing::warn!("could not install ad block: {e}");
    }
}

/// The next unused tab id.
fn claim_tab_id() -> u64 {
    NEXT_TAB_ID.with(|cell| {
        let id = cell.get();
        cell.set(id + 1);
        id
    })
}

/// Posted by the window procedure when the frame changed size.
const WM_SIZE_REQUEST: u32 = WM_APP + 2;
/// Posted when the address bar was submitted.
const WM_NAVIGATE: u32 = WM_APP + 3;
/// Posted when a navigation button was pressed.
const WM_BROWSE: u32 = WM_APP + 4;
const BROWSE_BACK: WPARAM = 1;
const BROWSE_FORWARD: WPARAM = 2;
const BROWSE_RELOAD: WPARAM = 3;
const BROWSE_HOME: WPARAM = 4;
/// Posted to bring the address toolbar down or send it away.
const WM_TOOLBAR: u32 = WM_APP + 5;
/// Posted to switch to a tab, close one, or open one.
const WM_TAB: u32 = WM_APP + 6;
const TAB_SELECT: WPARAM = 1;
const TAB_CLOSE: WPARAM = 2;
const TAB_NEW: WPARAM = 3;

/// Sent when the pointer leaves the window.
///
/// Spelled out because the binding crate does not export it, and a name it does
/// not know becomes a pattern that matches everything — which silently ate
/// every message handled after it, buttons and resizing included.
const MOUSE_LEFT_WINDOW: u32 = 0x02A3;

/// The title band: logo, name, tabs, close. A standard caption's height.
const BAR_HEIGHT: i32 = 40;

/// The toolbar that drops below it, holding the address.
///
/// Hidden until the pointer reaches the tabs, because a window opened to save
/// one video spends most of its life not being typed into, and the page is
/// what the window is for.
const TOOLBAR_HEIGHT: i32 = 46;

/// The close button's box: a square in the middle of the band rather than a
/// strip its full height, which reads as a button instead of as a column.
const CLOSE_BOX: i32 = 26;
/// How far the close button sits from the right edge.
const CLOSE_INSET: i32 = 10;
/// One navigation button: back, forward, reload, home.
const NAV_BOX: i32 = 30;

/// A tab's width and the gap around the strip.
const TAB_WIDTH: i32 = 190;

/// Room kept for the program's name before the tabs begin.
const NAME_WIDTH: i32 = 104;

/// The corner radius everything the frame draws shares.
///
/// The window's outline, the page inside it and the tab that joins them are one
/// object with one shape; three different radii read as three things stacked.
const RADIUS: i32 = 10;

/// How wide the grab strip along the window's edges is.
///
/// Only as wide as a pointer needs to find it. It used to be wide enough to
/// hide the page's square corners behind, which is no longer its job — the page
/// windows are cut to a rounded shape themselves now — and a visible margin
/// between the frame and what it holds is a gap with nothing in it.
const RESIZE_EDGE: i32 = 4;

/// Windows' own dark caption, so the band reads as a title bar rather than as
/// something a program drew. COLORREF is 0x00BBGGRR.
const CAPTION: u32 = 0x0020_2020;
/// The page area behind the web view.
const SURFACE: u32 = 0x0020_2020;
const TOOLBAR_BG: u32 = 0x0032_2E2C;
const FIELD: u32 = 0x0038_3838;
/// Shard's cyan, as the tray draws it.
const ACCENT: u32 = 0x00EE_D322;
const TEXT: u32 = 0x00F0_F0F0;

thread_local! {
    /// The address bar, reachable from the window procedure without a lookup.
    static ADDRESS: std::cell::Cell<HWND> = const { std::cell::Cell::new(std::ptr::null_mut()) };
    /// Which button the pointer is over, for its lit state.
    static HOVERED: std::cell::Cell<Hot> = const { std::cell::Cell::new(Hot::None) };
    /// Whether the address toolbar is down.
    static TOOLBAR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// What the tab says, which is the page's own title.
    static TAB_TITLE: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    /// The page's own background, so the tab it belongs to matches it.
    static PAGE_COLOUR: std::cell::Cell<u32> = const { std::cell::Cell::new(TOOLBAR_BG) };
    /// The toolbar's own window, which floats over the page.
    static BAR: std::cell::Cell<HWND> = const { std::cell::Cell::new(std::ptr::null_mut()) };
    /// What each tab is, in the order they are drawn.
    ///
    /// Parallel to `VIEWS` by position; the page inside a tab knows the tab by
    /// its `id`, which does not move when a tab to its left is closed.
    static TABS: std::cell::RefCell<Vec<Tab>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Hands out tab ids. Never reused, so a page cannot report against a tab
    /// that closed and have it land on whatever took its place.
    static NEXT_TAB_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
    /// The pages themselves.
    ///
    /// Held here rather than in the message loop so the window procedure can
    /// lay them out. Dragging a window's edge puts Windows into a loop of its
    /// own, and anything posted during it is dispatched to the procedure and
    /// consumed there — a message asking the loop to re-lay the page never
    /// arrived, so the page stayed the size it was until something else moved
    /// it. Everything here runs on one thread, so this is a place to put them
    /// rather than a thing to guard.
    static VIEWS: std::cell::RefCell<Vec<wry::WebView>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static ACTIVE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static ORIGINAL_EDIT_PROC: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    static BRUSHES: std::cell::Cell<(isize, isize)> = const { std::cell::Cell::new((0, 0)) };
    /// The address field's background, and the colour it was made for.
    ///
    /// Kept rather than made each time it is asked for: the control asks on
    /// every repaint, and a brush built per paint is a handle leaked per paint.
    static FIELD_BRUSH: std::cell::Cell<(u32, isize)> = const { std::cell::Cell::new((0, 0)) };
}

/// The button under the pointer, if any.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hot {
    None,
    Back,
    Forward,
    Reload,
    Home,
    Close,
    NewTab,
    Tab(usize),
    TabClose(usize),
}

/// Where everything sits, in client coordinates.
struct Band {
    /// The title band's height — tabs, logo, close.
    height: i32,
    /// The top of the address row — one line below the tabs, so the frame keeps
    /// the row between them to draw an edge on.
    bar_top: i32,
    /// Where the page starts: one line below the address row, for the same
    /// reason. Both edges live on rows no child window covers.
    page_top: i32,
    /// The toolbar's height, whether or not it is on screen.
    toolbar: i32,
    logo: windows_sys::Win32::Foundation::RECT,
    tabs: Vec<windows_sys::Win32::Foundation::RECT>,
    /// The small cross inside each tab.
    tab_closes: Vec<windows_sys::Win32::Foundation::RECT>,
    new_tab: windows_sys::Win32::Foundation::RECT,
    close: windows_sys::Win32::Foundation::RECT,
}

/// One tab: what it is called, what colour its page is, and where it is.
///
/// The page reports against `id` rather than against a position, because
/// positions shift as tabs are closed and a report in flight would then land on
/// the wrong tab.
#[derive(Clone)]
struct Tab {
    id: u64,
    title: String,
    colour: u32,
    url: String,
}

impl Tab {
    fn new(id: u64) -> Self {
        Tab { id, title: String::new(), colour: TOOLBAR_BG, url: String::new() }
    }
}

/// Bring the frame's colour back in line with whichever tab is in front.
///
/// Kept as a cached value rather than read from the tabs at every paint because
/// the toolbar is a separate window that needs the same answer and has no
/// business reaching into the tab list to get it.
/// Redraw the frame, lay the pages out again, and redraw the address bar.
///
/// The bar is a separate window, so it repaints only when asked. Everything
/// that changes the colour or the contents of a tab has to ask both, and doing
/// that in one place is what stops the two drifting apart.
fn repaint_all(hwnd: HWND) {
    refresh_page_colour();
    fit(hwnd);
    unsafe { InvalidateRect(hwnd, std::ptr::null(), 1) };
    let bar = BAR.with(|cell| cell.get());
    if !bar.is_null() {
        unsafe { InvalidateRect(bar, std::ptr::null(), 1) };
    }
}

fn refresh_page_colour() {
    let active = ACTIVE.with(|cell| cell.get());
    let colour = TABS.with(|cell| cell.borrow().get(active).map(|tab| tab.colour));
    PAGE_COLOUR.with(|cell| cell.set(colour.unwrap_or(TOOLBAR_BG)));
}

/// Where the toolbar's own parts sit, in the toolbar's coordinates.
struct Bar {
    back: windows_sys::Win32::Foundation::RECT,
    forward: windows_sys::Win32::Foundation::RECT,
    reload: windows_sys::Win32::Foundation::RECT,
    home: windows_sys::Win32::Foundation::RECT,
    field: windows_sys::Win32::Foundation::RECT,
}

fn scale_of(hwnd: HWND) -> i32 {
    unsafe { GetDpiForWindow(hwnd) }.max(96) as i32
}

fn band(hwnd: HWND) -> Band {
    let mut rect = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::RECT>() };
    unsafe { GetClientRect(hwnd, &mut rect) };
    let count = TABS.with(|cell| cell.borrow().len());
    band_of((rect.right - rect.left).max(0), scale_of(hwnd), count)
}

/// The band, from a width and a scale rather than from a window.
///
/// Split out so the layout — and everything drawn from it — can be worked out
/// without a window existing, which is what lets the frame be drawn into a
/// bitmap and looked at.
fn band_of(width: i32, scale: i32, tabs: usize) -> Band {
    let at = |size: i32| (size * scale / 96).max(12);

    let height = at(BAR_HEIGHT);
    let toolbar = at(TOOLBAR_HEIGHT);
    let logo_size = at(14);
    let close_box = at(CLOSE_BOX);
    let close_inset = at(CLOSE_INSET);
    let new_tab = at(20);
    let tab_top = at(5);

    let middle = |y: i32, h: i32, x: i32, size: i32| windows_sys::Win32::Foundation::RECT {
        left: x,
        top: y + (h - size.min(h)) / 2,
        right: x + size,
        bottom: y + (h - size.min(h)) / 2 + size.min(h),
    };

    // Tabs begin where the name ends and share what room is left, narrowing as
    // more are opened rather than running off the end of the window.
    let count = tabs.max(1);
    let strip_left = at(10) + logo_size + at(8) + at(NAME_WIDTH);
    let strip_right = layout_close_left(width, close_inset, close_box) - new_tab - at(10);
    let room = (strip_right - strip_left).max(at(80));
    let tab_width = (room / count as i32).min(at(TAB_WIDTH)).max(at(60));

    let mut tabs = Vec::with_capacity(count);
    let mut tab_closes = Vec::with_capacity(count);
    for index in 0..count {
        let left = strip_left + tab_width * index as i32;
        let tab = windows_sys::Win32::Foundation::RECT {
            left,
            top: tab_top,
            right: left + tab_width - at(2),
            bottom: height,
        };
        let cross = at(16);
        tab_closes.push(middle(tab_top, height - tab_top, tab.right - cross - at(6), cross));
        tabs.push(tab);
    }
    let after = tabs.last().map(|t| t.right).unwrap_or(strip_left) + at(4);

    Band {
        height,
        bar_top: height + 1,
        page_top: height + 1 + toolbar + 1,
        toolbar,
        logo: middle(0, height, at(10), logo_size),
        // Centred on the tab's own height rather than the band's: the tab does
        // not start at the top of the band, so centring on the band left the
        // plus sitting above the row it belongs to.
        new_tab: middle(tab_top, height - tab_top, after, new_tab),
        close: middle(0, height, layout_close_left(width, close_inset, close_box), close_box),
        tabs,
        tab_closes,
    }
}

fn layout_close_left(width: i32, inset: i32, box_size: i32) -> i32 {
    width - inset - box_size
}

fn bar_layout(hwnd: HWND) -> Bar {
    let mut rect = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::RECT>() };
    unsafe { GetClientRect(hwnd, &mut rect) };
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    let scale = scale_of(hwnd);
    let at = |size: i32| (size * scale / 96).max(12);

    let nav = at(NAV_BOX);
    let first = at(6);
    let middle = |x: i32, size: i32| windows_sys::Win32::Foundation::RECT {
        left: x,
        top: (height - size) / 2,
        right: x + size,
        bottom: (height - size) / 2 + size,
    };

    let field_left = first + nav * 4 + at(6);
    let field_height = (height * 62 / 100).max(20);
    let field_top = (height - field_height) / 2;

    Bar {
        back: middle(first, nav),
        forward: middle(first + nav, nav),
        reload: middle(first + nav * 2, nav),
        home: middle(first + nav * 3, nav),
        field: windows_sys::Win32::Foundation::RECT {
            left: field_left,
            top: field_top,
            right: (width - at(10)).max(field_left + at(80)),
            bottom: field_top + field_height,
        },
    }
}

/// The button under the pointer, if any. Tabs report their own index.
fn hot_at(layout: &Band, x: i32, y: i32) -> Hot {
    let inside = |r: &windows_sys::Win32::Foundation::RECT| {
        x >= r.left && x < r.right && y >= r.top && y < r.bottom
    };
    if inside(&layout.close) {
        return Hot::Close;
    }
    if inside(&layout.new_tab) {
        return Hot::NewTab;
    }
    for (index, cross) in layout.tab_closes.iter().enumerate() {
        if inside(cross) {
            return Hot::TabClose(index);
        }
    }
    for (index, tab) in layout.tabs.iter().enumerate() {
        if inside(tab) {
            return Hot::Tab(index);
        }
    }
    Hot::None
}

/// Lay out the toolbar and the page.
///
/// The toolbar is a window of its own sitting over the page rather than a strip
/// drawn above it. Anything the frame paints goes behind its child windows, so
/// a drawn toolbar could never appear over the web view — and pushing the page
/// down to make room moved the whole thing every time the pointer touched a
/// tab.
fn fit(hwnd: HWND) {
    let active = ACTIVE.with(|cell| cell.get());
    VIEWS.with(|cell| {
        let views = cell.borrow();
        lay_out(hwnd, &views, active.min(views.len().saturating_sub(1)));
    });
}

fn lay_out(hwnd: HWND, views: &[wry::WebView], active: usize) {
    let mut rect = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::RECT>() };
    if unsafe { GetClientRect(hwnd, &mut rect) } == 0 {
        return;
    }
    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    let layout = band(hwnd);

    let bar = BAR.with(|cell| cell.get());
    if !bar.is_null() {
        // A row of the window, the full width of it, between the tabs and the
        // page. Full width because it is part of the frame now rather than
        // something resting on the page; the box inside it is what is inset.
        unsafe {
            let mut had = std::mem::zeroed::<windows_sys::Win32::Foundation::RECT>();
            GetWindowRect(bar, &mut had);
            let mut origin = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
            windows_sys::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut origin);
            if had.left != origin.x
                || had.top != origin.y + layout.bar_top
                || had.right - had.left != width
                || had.bottom - had.top != layout.toolbar
            {
                MoveWindow(bar, 0, layout.bar_top, width, layout.toolbar, 1);
            }
            place_address(bar);
        }
    }

    // A strip is kept clear of the web view down each side and along the
    // bottom. Not decoration: a child window takes the mouse before its parent
    // sees it, so wherever the web view reaches an edge the parent is never
    // asked where the pointer is and the window cannot be resized there.
    let edge = if unsafe { IsZoomed(hwnd) } != 0 { 0 } else { RESIZE_EDGE };
    for (index, view) in views.iter().enumerate() {
        if index == active {
            let _ = view.set_bounds(wry::Rect {
                position: wry::dpi::PhysicalPosition::new(edge, layout.page_top).into(),
                size: wry::dpi::PhysicalSize::new(
                    (width - edge * 2).max(0) as u32,
                    (height - layout.page_top - edge).max(0) as u32,
                )
                .into(),
            });
            let _ = view.set_visible(true);
        } else {
            // Hidden rather than destroyed: a tab keeps its page, its history
            // and whatever it was playing, which is the whole point of tabs.
            let _ = view.set_visible(false);
        }
    }
}

/// Put the address control inside the toolbar's rounded box.
fn place_address(bar: HWND) {
    let edit = ADDRESS.with(|cell| cell.get());
    if edit.is_null() {
        return;
    }
    let layout = bar_layout(bar);
    // The control is the height of its own text, and centred in the box.
    //
    // Not because that is tidy — because it is the only thing that works. A
    // single-line edit draws its text against the top of whatever height it is
    // given, and it declines to move its formatting rectangle; both are asked
    // of a real control in the tests at the foot of this file. So a control
    // filling the box would strand the address against the top of it.
    //
    // What that leaves is a margin above and below the text that belongs to the
    // bar rather than to the control. The bar hands it back: it shows a caret
    // over the whole box and sends clicks there on to the control.
    let box_height = layout.field.bottom - layout.field.top;
    let text_height = font_height(edit).clamp(12, box_height);
    let top = layout.field.top + (box_height - text_height) / 2;
    let pad = (box_height / 4).max(6);
    unsafe {
        MoveWindow(
            edit,
            layout.field.left + pad,
            top,
            (layout.field.right - layout.field.left - pad * 2).max(0),
            text_height,
            1,
        );
    }
}

/// The height of one line in a control's own font.
fn font_height(control: HWND) -> i32 {
    use windows_sys::Win32::Graphics::Gdi::{GetTextMetricsW, SelectObject, TEXTMETRICW};
    unsafe {
        let dc = windows_sys::Win32::Graphics::Gdi::GetDC(control);
        if dc.is_null() {
            return 16;
        }
        // WM_GETFONT. A control that was never given one answers null, and the
        // system font is what it is drawing with in that case.
        let font = SendMessageW(control, 0x0031, 0, 0);
        let old = if font != 0 { SelectObject(dc, font as _) } else { std::ptr::null_mut() };
        let mut metrics = std::mem::zeroed::<TEXTMETRICW>();
        let ok = GetTextMetricsW(dc, &mut metrics);
        if !old.is_null() {
            SelectObject(dc, old);
        }
        windows_sys::Win32::Graphics::Gdi::ReleaseDC(control, dc);
        if ok == 0 {
            16
        } else {
            (metrics.tmHeight + metrics.tmExternalLeading).max(12)
        }
    }
}

/// Put the keyboard in the address field.
///
/// The bar is a top-level window of its own, and a control only receives keys
/// when the top-level window holding it is the active one. Setting the focus
/// without settling that first is what left the address showing a caret and a
/// selection while every key went to the frame behind it — which is how it
/// looked when the bar accepted typing as it appeared and stopped the moment it
/// was clicked, because by then the frame had been activated in between.
fn focus_address(bar: HWND, select_all: bool) {
    let edit = ADDRESS.with(|cell| cell.get());
    if edit.is_null() {
        return;
    }
    let _ = bar;
    unsafe {
        // Nothing is activated and nothing is brought to the front. The bar and
        // the frame share a thread, so this moves the thread's focus, and the
        // keyboard follows the focus inside whichever thread is in front. The
        // frame stays the active window throughout.
        SetFocus(edit);
        // EM_SETSEL. Everything, when the bar has just come down and typing
        // should replace the address; an empty range at the end when it was
        // clicked into, because that is an intent to edit rather than replace.
        let (from, to): (WPARAM, LPARAM) = if select_all { (0, -1) } else { (-1i32 as WPARAM, -1) };
        SendMessageW(edit, 0x00B1, from, to);
    }
}

/// Kept because the page and the tabs still say when they think the address
/// should come and go — and the answer is now that it never does.
///
/// It came down on a hover and went away again on anything at all, and the gap
/// between those two was often shorter than the reach for it. A browser's
/// address is simply there.
fn set_toolbar(_down: bool) {}

/// Draw the frame: title band, tabs, and the window's border.
///
/// Everything a title bar normally provides is drawn here, because taking the
/// caption is what makes room for tabs in it. The colours follow the page, so
/// the tab and the thing it holds read as one surface.
fn paint(hwnd: HWND) {
    use windows_sys::Win32::Graphics::Gdi::*;

    let mut paint = unsafe { std::mem::zeroed::<PAINTSTRUCT>() };
    let dc = unsafe { BeginPaint(hwnd, &mut paint) };
    if dc.is_null() {
        return;
    }
    let mut client = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::RECT>() };
    unsafe { GetClientRect(hwnd, &mut client) };

    draw_frame(dc, &client, &Frame::of(hwnd));

    unsafe { EndPaint(hwnd, &paint) };
}

/// Everything the frame's appearance depends on.
///
/// Gathered into one value so the drawing below can be run against a bitmap
/// with no window behind it. What the band looks like is then a question that
/// can be asked and answered without anything appearing on screen — which is
/// the only way a line that kept being painted where something else covered it
/// was ever going to be caught.
struct Frame {
    layout: Band,
    page: u32,
    scale: i32,
    hovered: Hot,
    active: usize,
    tabs: Vec<Tab>,
}

impl Frame {
    fn of(hwnd: HWND) -> Self {
        Frame {
            layout: band(hwnd),
            page: PAGE_COLOUR.with(|cell| cell.get()),
            scale: scale_of(hwnd),
            hovered: HOVERED.with(|cell| cell.get()),
            active: ACTIVE.with(|cell| cell.get()),
            tabs: TABS.with(|cell| cell.borrow().clone()),
        }
    }
}

/// Draw the band, the tabs and the line between them and the page.
fn draw_frame(
    dc: windows_sys::Win32::Graphics::Gdi::HDC,
    client: &windows_sys::Win32::Foundation::RECT,
    frame: &Frame,
) {
    use windows_sys::Win32::Graphics::Gdi::*;

    let layout = &frame.layout;
    let page = frame.page;
    let ink = readable_on(page);
    let client = *client;

    unsafe {
        let caption = CreateSolidBrush(CAPTION);
        let mut top = client;
        top.bottom = layout.height;
        FillRect(dc, &top, caption);
        DeleteObject(caption as _);

        let surface = CreateSolidBrush(page);
        let mut rest = client;
        rest.top = layout.page_top;
        FillRect(dc, &rest, surface);
        DeleteObject(surface as _);
    }

    // No border is drawn here. The window is cut to a rounded shape and the
    // desktop manager draws the outline of that shape; a line painted just
    // inside it made a second edge a pixel away from the first, which is the
    // stray line that showed around the window.

    let hovered = frame.hovered;
    unsafe { SetBkMode(dc, TRANSPARENT as i32) };

    // The mark, from the same idea as the tray icon: a rounded square cut by a
    // diagonal gap — a packet split in two.
    unsafe {
        let fill = CreateSolidBrush(ACCENT);
        let pen = CreatePen(PS_SOLID, 1, ACCENT);
        let old_brush = SelectObject(dc, fill as _);
        let old_pen = SelectObject(dc, pen as _);
        let r = layout.logo;
        let round = (r.right - r.left) / 3;
        RoundRect(dc, r.left, r.top, r.right, r.bottom, round, round);
        SelectObject(dc, old_brush);
        SelectObject(dc, old_pen);
        DeleteObject(fill as _);
        DeleteObject(pen as _);

        let cut = CreatePen(PS_SOLID, ((r.right - r.left) / 5).max(2), CAPTION);
        let old_cut = SelectObject(dc, cut as _);
        MoveToEx(dc, r.left - 1, r.bottom + 1, std::ptr::null_mut());
        LineTo(dc, r.right + 1, r.top - 1);
        SelectObject(dc, old_cut);
        DeleteObject(cut as _);
    }

    // One face for everything the band says. Selected once and put back at the
    // end, because a device context handed a font keeps it.
    let height = text_height(frame.scale);
    let label_font = ui_font(height, false);
    // The window's own name is set smaller than the tabs. It never changes and
    // it is read once; a tab's title is the thing being looked for, and at the
    // same size the name competed with every one of them.
    let name_font = ui_font((height * 4 / 5).max(10), true);
    let previous_font = unsafe { SelectObject(dc, name_font as _) };

    let mut name: Vec<u16> = "Shard Browser".encode_utf16().collect();
    name.push(0);
    let first_tab = layout.tabs.first().map(|t| t.left).unwrap_or(client.right);
    let mut where_ = windows_sys::Win32::Foundation::RECT {
        left: layout.logo.right + 8,
        top: 0,
        right: (first_tab - 8).max(layout.logo.right + 8),
        bottom: layout.height,
    };
    unsafe {
        SetTextColor(dc, TEXT);
        DrawTextW(
            dc,
            name.as_mut_ptr(),
            -1,
            &mut where_,
            DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS,
        );
    }

    // No line is drawn round the page: the window's own edge is the system's
    // now, and a second one just inside it read as a box within a box. What
    // does get a line is where the band meets the page — the same line the tabs
    // are drawn with, so the whole frame is edged by one stroke.
    let scale = frame.scale;
    let radius = (RADIUS * scale / 96).max(6);
    let line: i32 = 1;
    let edge_colour = blend(page, readable_on(page), 0.28);
    // The line between the band and the page is drawn after the tabs, below.
    //
    // Twice it was drawn before them and twice it disappeared: on the boundary
    // itself the web view covered it, because a child window is placed starting
    // at exactly that row and covers whatever its parent painted there; a row
    // higher, each tab's own fill covered the stretch it sat on, which is the
    // line arriving in pieces.
    let divider = layout.page_top - line;

    let active = frame.active;
    let held_tabs = &frame.tabs;
    for (index, tab) in layout.tabs.iter().enumerate() {
        let front = index == active;
        let hovered_tab = matches!(hovered, Hot::Tab(at) | Hot::TabClose(at) if at == index);
        // Each tab carries the colour of the page inside it, not the colour of
        // whichever page is in front. The one in front matches the surface
        // below it, so the two join; the rest sit back in the caption, tinted
        // by their own page and lifting under the pointer.
        let mine = held_tabs.get(index).map(|t| t.colour).unwrap_or(page);
        let fill_colour = if front {
            page
        } else if hovered_tab {
            blend(CAPTION, mine, 0.4)
        } else {
            blend(CAPTION, mine, 0.15)
        };
        unsafe {
            let fill = CreateSolidBrush(fill_colour);
            // The one in front is outlined in the same line that goes round the
            // page, so the two are one shape. The others are separated from the
            // band by a fainter line — enough to see where a tab ends without
            // claiming the attention the front one has.
            let outline = if front {
                edge_colour
            } else {
                blend(CAPTION, readable_on(page), 0.18)
            };
            let pen = CreatePen(PS_SOLID, line, outline);
            let old_brush = SelectObject(dc, fill as _);
            let old_pen = SelectObject(dc, pen as _);
            let round = radius * 2;
            // Drawn past the bottom of the band so the foot of the tab is
            // outside the window and only its sides and top show a line —
            // which is what opens it into the page below.
            RoundRect(dc, tab.left, tab.top, tab.right, tab.bottom + round, round, round);
            SelectObject(dc, old_brush);
            SelectObject(dc, old_pen);
            DeleteObject(fill as _);
            DeleteObject(pen as _);
        }

        let held = held_tabs.get(index).map(|t| t.title.clone()).unwrap_or_default();
        let text = if held.is_empty() { "새 탭".to_string() } else { held };
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0);
        let cross = layout.tab_closes.get(index);
        let mut inside = windows_sys::Win32::Foundation::RECT {
            // Clear of the tab's rounded corner rather than against it. At the
            // old inset the first letter sat inside the curve.
            left: tab.left + (15 * scale / 96).max(12),
            top: tab.top,
            right: cross.map(|c| c.left - 4).unwrap_or(tab.right - 12),
            bottom: tab.bottom,
        };
        unsafe {
            SelectObject(dc, label_font as _);
            SetTextColor(dc, if front { ink } else { TEXT });
            DrawTextW(
                dc,
                wide.as_mut_ptr(),
                -1,
                &mut inside,
                DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS,
            );
        }

        // A tab's own cross, shown on the one in front and under the pointer —
        // a row of crosses on every tab is noise to read past.
        if let Some(cross) = cross {
            if (front || hovered_tab) && layout.tabs.len() > 1 {
                glyph_in(
                    dc,
                    cross,
                    Glyph::TabClose,
                    hovered == Hot::TabClose(index),
                    if front { ink } else { TEXT },
                );
            }
        }
    }

    // The edge the tabs stand on, opened under the one in front.
    //
    // Same colour and same weight as a tab's own outline, so the outline of the
    // tab in front runs straight out into it on both sides — which is what
    // makes a tab and the thing below it read as one surface rather than two.
    unsafe {
        let pen = CreatePen(PS_SOLID, line, edge_colour);
        let old = SelectObject(dc, pen as _);
        MoveToEx(dc, 0, layout.height, std::ptr::null_mut());
        LineTo(dc, client.right, layout.height);
        SelectObject(dc, old);
        DeleteObject(pen as _);
    }
    if let Some(tab) = layout.tabs.get(active) {
        let mouth = windows_sys::Win32::Foundation::RECT {
            left: tab.left + line,
            top: layout.height,
            right: tab.right - line,
            bottom: layout.height + line,
        };
        unsafe {
            let cover = CreateSolidBrush(page);
            FillRect(dc, &mouth, cover);
            DeleteObject(cover as _);
        }
    }

    // The one line the frame draws: where the window ends and the page begins.
    //
    // It runs the whole width with nothing cut out of it. The tab in front no
    // longer meets it — it meets the address row directly above it, and the two
    // are the same colour, which is what joins them. Before, the tab sat
    // straight on the page and this line had to be broken to let it through;
    // breaking it is what left it looking unfinished.
    unsafe {
        let pen = CreatePen(PS_SOLID, line, edge_colour);
        let old = SelectObject(dc, pen as _);
        MoveToEx(dc, 0, divider, std::ptr::null_mut());
        LineTo(dc, client.right, divider);
        SelectObject(dc, old);
        DeleteObject(pen as _);
    }
    glyph(dc, &layout.new_tab, Glyph::Plus, hovered == Hot::NewTab);
    glyph(dc, &layout.close, Glyph::Close, hovered == Hot::Close);

    unsafe {
        SelectObject(dc, previous_font);
        DeleteObject(label_font as _);
        DeleteObject(name_font as _);
    }
}

/// Black or white, whichever can be read on `colour`.
fn readable_on(colour: u32) -> u32 {
    let red = colour & 0xff;
    let green = (colour >> 8) & 0xff;
    let blue = (colour >> 16) & 0xff;
    // Weighted the way an eye weights them, not evenly.
    let light = (red * 299 + green * 587 + blue * 114) / 1000;
    if light > 140 {
        0x0020_2020
    } else {
        0x00F0_F0F0
    }
}

fn blend(from: u32, to: u32, amount: f32) -> u32 {
    let mix = |shift: u32| {
        let a = ((from >> shift) & 0xff) as f32;
        let b = ((to >> shift) & 0xff) as f32;
        ((a + (b - a) * amount) as u32).min(255) << shift
    };
    mix(0) | mix(8) | mix(16)
}

/// Paint the toolbar: its background, the address box, and its buttons.
fn paint_bar(hwnd: HWND) {
    use windows_sys::Win32::Graphics::Gdi::*;

    let layout = bar_layout(hwnd);
    let page = PAGE_COLOUR.with(|cell| cell.get());
    let ink = readable_on(page);
    let mut paint = unsafe { std::mem::zeroed::<PAINTSTRUCT>() };
    let screen = unsafe { BeginPaint(hwnd, &mut paint) };
    if screen.is_null() {
        return;
    }
    let mut client = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::RECT>() };
    unsafe { GetClientRect(hwnd, &mut client) };

    // Drawn into memory first and copied across in one go. Painting straight to
    // the screen shows every step of it, which is what the flicker was.
    let dc = unsafe { CreateCompatibleDC(screen) };
    let surface =
        unsafe { CreateCompatibleBitmap(screen, client.right.max(1), client.bottom.max(1)) };
    let previous = unsafe { SelectObject(dc, surface as _) };

    unsafe {
        let fill = CreateSolidBrush(page);
        FillRect(dc, &client, fill);
        DeleteObject(fill as _);
        // No line across the bottom: the bar is a shape, and a line drawn to its
        // edges ran through the curve and read as a rule across it.
        //
        // The box is a rounded rectangle at the same radius as the bar holding
        // it. It was a stadium — arcs as tall as the box — which is where the
        // mismatched ends came from.
        let box_fill = CreateSolidBrush(blend(page, ink, 0.10));
        let edge = CreatePen(PS_SOLID, 1, blend(page, ink, 0.25));
        let old_brush = SelectObject(dc, box_fill as _);
        let old_pen = SelectObject(dc, edge as _);
        let radius = (RADIUS * scale_of(hwnd) / 96).max(6) * 2;
        RoundRect(
            dc,
            layout.field.left,
            layout.field.top,
            layout.field.right,
            layout.field.bottom,
            radius,
            radius,
        );
        SelectObject(dc, old_brush);
        SelectObject(dc, old_pen);
        DeleteObject(box_fill as _);
        DeleteObject(edge as _);
        SetBkMode(dc, TRANSPARENT as i32);
    }

    let hovered = HOVERED.with(|cell| cell.get());
    glyph_in(dc, &layout.back, Glyph::Back, hovered == Hot::Back, ink);
    glyph_in(dc, &layout.forward, Glyph::Forward, hovered == Hot::Forward, ink);
    glyph_in(dc, &layout.reload, Glyph::Reload, hovered == Hot::Reload, ink);
    glyph_in(dc, &layout.home, Glyph::Home, hovered == Hot::Home, ink);

    unsafe {
        BitBlt(screen, 0, 0, client.right, client.bottom, dc, 0, 0, SRCCOPY);
        SelectObject(dc, previous);
        DeleteObject(surface as _);
        DeleteDC(dc);
        EndPaint(hwnd, &paint);
    }
}

/// Which toolbar button covers a point.
fn bar_hot_at(layout: &Bar, x: i32, y: i32) -> Hot {
    let inside = |r: &windows_sys::Win32::Foundation::RECT| {
        x >= r.left && x < r.right && y >= r.top && y < r.bottom
    };
    if inside(&layout.back) {
        Hot::Back
    } else if inside(&layout.forward) {
        Hot::Forward
    } else if inside(&layout.reload) {
        Hot::Reload
    } else if inside(&layout.home) {
        Hot::Home
    } else {
        Hot::None
    }
}

/// A handle to the icon font, made once and kept.
///
/// Windows ships a font of interface icons, and its arrows and crosses are
/// drawn by people who do this for a living. Lines drawn by hand looked like
/// lines drawn by hand — no anti-aliasing, and a shape that has to be nudged
/// again at every scale.
fn icon_font(height: i32) -> windows_sys::Win32::Graphics::Gdi::HFONT {
    use windows_sys::Win32::Graphics::Gdi::*;
    let mut face: Vec<u16> = "Segoe Fluent Icons".encode_utf16().collect();
    face.resize(32, 0);
    let mut logfont: LOGFONTW = unsafe { std::mem::zeroed() };
    logfont.lfHeight = -height;
    logfont.lfWeight = FW_NORMAL as i32;
    logfont.lfCharSet = DEFAULT_CHARSET;
    logfont.lfQuality = CLEARTYPE_QUALITY;
    logfont.lfFaceName[..32].copy_from_slice(&face[..32]);
    let font = unsafe { CreateFontIndirectW(&logfont) };
    if !font.is_null() {
        return font;
    }
    // Windows 10 calls the same set by an older name.
    let mut older: Vec<u16> = "Segoe MDL2 Assets".encode_utf16().collect();
    older.resize(32, 0);
    logfont.lfFaceName[..32].copy_from_slice(&older[..32]);
    unsafe { CreateFontIndirectW(&logfont) }
}

/// The faces the interface asks for, best first.
///
/// The first three are what Korean product and marketing work is set in, and
/// they are the reason this list exists rather than a single name. None of them
/// ship with Windows, so the list ends with faces that always will: whatever is
/// installed is used, and nothing has to be bundled for the window to be
/// legible.
const UI_FACES: [&str; 6] = [
    "Pretendard Variable",
    "Pretendard",
    "SUIT",
    "NanumSquare",
    // Windows' own Korean and Latin interface faces.
    "Malgun Gothic",
    "Segoe UI",
];

/// How tall the interface's text is, before the screen's scaling is applied.
///
/// One number for everything, because two were the problem: the band scaled
/// with the screen and the address did not, so the further from 100% a display
/// ran the further apart the two drifted — the band growing, the address
/// staying put.
const UI_TEXT: i32 = 13;

/// That height on a particular screen.
fn text_height(scale: i32) -> i32 {
    (UI_TEXT * scale / 96).max(11)
}

/// A face from `UI_FACES`, at the height and weight asked for.
///
/// GDI does not fall back between families — a name it does not know is served
/// by whatever it likes — so each name is tried and the one that comes back is
/// checked against what was asked for.
fn ui_font(height: i32, bold: bool) -> windows_sys::Win32::Graphics::Gdi::HFONT {
    use windows_sys::Win32::Graphics::Gdi::*;

    let mut logfont: LOGFONTW = unsafe { std::mem::zeroed() };
    logfont.lfHeight = -height;
    logfont.lfWeight = if bold { FW_SEMIBOLD as i32 } else { FW_NORMAL as i32 };
    logfont.lfCharSet = DEFAULT_CHARSET;
    logfont.lfQuality = CLEARTYPE_QUALITY;

    for face in UI_FACES {
        let mut wide: Vec<u16> = face.encode_utf16().collect();
        wide.resize(32, 0);
        logfont.lfFaceName[..32].copy_from_slice(&wide[..32]);
        let font = unsafe { CreateFontIndirectW(&logfont) };
        if font.is_null() {
            continue;
        }
        if face_of(font).eq_ignore_ascii_case(face) {
            return font;
        }
        unsafe { DeleteObject(font as _) };
    }
    // Nothing matched, which cannot happen while Segoe UI exists — but a window
    // with no font at all is worse than a window with the wrong one.
    unsafe { CreateFontIndirectW(&logfont) }
}

/// What a font actually is, as opposed to what was asked for.
fn face_of(font: windows_sys::Win32::Graphics::Gdi::HFONT) -> String {
    use windows_sys::Win32::Graphics::Gdi::*;
    unsafe {
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        if dc.is_null() {
            return String::new();
        }
        let old = SelectObject(dc, font as _);
        let mut name = [0u16; 64];
        let read = GetTextFaceW(dc, name.len() as i32, name.as_mut_ptr());
        SelectObject(dc, old);
        DeleteDC(dc);
        let end = (read.max(1) - 1) as usize;
        String::from_utf16_lossy(&name[..end.min(name.len())])
    }
}

/// Which mark a button carries.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Glyph {
    Back,
    Forward,
    Reload,
    Home,
    Plus,
    Close,
    /// The same mark as `Close`, without the red: red means the window is
    /// about to go, and a tab going is not that.
    TabClose,
}

/// Draw one button: its lit background, then its mark.
fn glyph(
    dc: windows_sys::Win32::Graphics::Gdi::HDC,
    at: &windows_sys::Win32::Foundation::RECT,
    which: Glyph,
    lit: bool,
) {
    glyph_in(dc, at, which, lit, TEXT);
}

/// The same, in a stated colour — for buttons over a page whose shade is not
/// known until it says so.
fn glyph_in(
    dc: windows_sys::Win32::Graphics::Gdi::HDC,
    at: &windows_sys::Win32::Foundation::RECT,
    which: Glyph,
    lit: bool,
    ink: u32,
) {
    use windows_sys::Win32::Graphics::Gdi::*;

    if lit {
        let colour = if which == Glyph::Close { 0x0022_22C8 } else { 0x003F_3B39 };
        unsafe {
            let brush = CreateSolidBrush(colour);
            let pen = CreatePen(PS_SOLID, 1, colour);
            let old_brush = SelectObject(dc, brush as _);
            let old_pen = SelectObject(dc, pen as _);
            let round = ((at.bottom - at.top) / 3).max(3);
            RoundRect(dc, at.left, at.top, at.right, at.bottom, round, round);
            SelectObject(dc, old_brush);
            SelectObject(dc, old_pen);
            DeleteObject(brush as _);
            DeleteObject(pen as _);
        }
    }

    // Codepoints from Windows' own icon font, the ones its browser uses.
    let mark: u16 = match which {
        Glyph::Back => 0xE72B,
        Glyph::Forward => 0xE72A,
        Glyph::Reload => 0xE72C,
        Glyph::Home => 0xE80F,
        Glyph::Plus => 0xE710,
        Glyph::Close | Glyph::TabClose => 0xE8BB,
    };
    let size = ((at.bottom - at.top) * 45 / 100).max(8);
    let font = icon_font(size);
    let mut text = [mark, 0u16];
    let mut where_ = *at;

    unsafe {
        let old = SelectObject(dc, font as _);
        SetTextColor(dc, if lit && which == Glyph::Close { 0x00FF_FFFF } else { ink });
        DrawTextW(
            dc,
            text.as_mut_ptr(),
            1,
            &mut where_,
            DT_SINGLELINE | DT_CENTER | DT_VCENTER,
        );
        SelectObject(dc, old);
        DeleteObject(font as _);
    }
}

fn create_window(title: &str) -> Result<HWND> {
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(procedure),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance as _,
        // The icon the executable already carries, so the window, the taskbar
        // and the tray all show the same mark without a second copy of it.
        hIcon: unsafe { LoadIconW(instance as _, 1 as *const u16) },
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        hbrBackground: unsafe { CreateSolidBrush(SURFACE) } as _,
        lpszMenuName: std::ptr::null(),
        lpszClassName: CLASS.as_ptr(),
    };
    // Registering twice is not an error worth reporting: the second window of a
    // session finds the class already there.
    unsafe { RegisterClassW(&class) };

    let mut wide: Vec<u16> = title.encode_utf16().collect();
    wide.push(0);
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            CLASS.as_ptr(),
            wide.as_ptr(),
            // No caption in the style at all. With one, Windows keeps drawing
            // its own close button over the frame this code draws, and the two
            // sit on top of each other — pressing ours only revealed theirs.
            // Maximise stays because it is what makes a double-click on the
            // band maximise, which is the gesture that replaces the button.
            WS_POPUP | WS_THICKFRAME | WS_SYSMENU | WS_MAXIMIZEBOX | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1100,
            800,
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
    // Tell the window its frame changed, or the caption this code replaces
    // stays on screen until something else forces a recalculation — which is
    // why the first window opened with two title bars and only looked right
    // after being maximised once.
    unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
    Ok(hwnd)
}

/// Ask the desktop manager for a dark frame.
///
/// Without it the title bar stays white against a dark window, which reads as
/// two programs stuck together rather than one.
fn dark_title_bar(hwnd: HWND) {
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    const USE_DARK_MODE: u32 = 20;
    const CORNER_PREFERENCE: u32 = 33;
    const BORDER_COLOUR: u32 = 34;
    const ROUND: i32 = 2;

    let on: i32 = 1;
    let round: i32 = ROUND;
    // Left to the desktop manager rather than drawn: it rounds the window's
    // own outline, which includes the parts a program cannot paint.
    unsafe {
        DwmSetWindowAttribute(hwnd, USE_DARK_MODE, (&on as *const i32).cast(), 4);
        DwmSetWindowAttribute(hwnd, CORNER_PREFERENCE, (&round as *const i32).cast(), 4);
    }
    // The border itself is left to Windows, which is what the settings window
    // has and what this was trying to imitate. An imitation of a system edge is
    // never quite the system edge. The light frame that used to appear when the
    // window lost focus is stopped elsewhere — by answering the deactivation
    // notice without letting the frame be redrawn.
    let _ = BORDER_COLOUR;
}

/// The toolbar: a window of its own, floating over the page.
fn create_toolbar(parent: HWND) -> Result<()> {
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(bar_procedure),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance as _,
        hIcon: std::ptr::null_mut(),
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: BAR_CLASS.as_ptr(),
    };
    unsafe { RegisterClassW(&class) };

    let bar = unsafe {
        CreateWindowExW(
            0,
            BAR_CLASS.as_ptr(),
            std::ptr::null(),
            // A child of the frame, sitting in its own row above the page.
            //
            // It was a popup floating over the page, because a sibling of the
            // web view could not be kept on top of it — the documented
            // "airspace" limit of hosting a web view in a window. Solving it
            // that way cost more than it bought: a popup is a top-level window,
            // so it needed the activation to take the keyboard, and taking the
            // activation is what made a click on a tab land on nothing and the
            // address go dead a moment after appearing.
            //
            // Nothing overlaps now. The page starts below this row, so there is
            // no airspace to fight over, the frame keeps the activation, and
            // the focus moves here the way it moves between any two controls.
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
            0,
            0,
            0,
            0,
            parent,
            std::ptr::null_mut(),
            instance as _,
            std::ptr::null(),
        )
    };
    if bar.is_null() {
        return Err(anyhow!("주소창을 만들지 못했습니다"));
    }
    BAR.with(|cell| cell.set(bar));
    create_address_bar(bar)
}

unsafe extern "system" fn bar_procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => {
            paint_bar(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        // The bar holds the keyboard on behalf of the field inside it. The
        // control is only as tall as its text, so the rest of the box is this
        // window, and focus arriving here belongs one level down.
        WM_SETFOCUS => {
            let edit = ADDRESS.with(|cell| cell.get());
            if !edit.is_null() {
                unsafe { SetFocus(edit) };
            }
            0
        }

        // Clicked without being activated. The frame keeps the activation and
        // this window keeps the focus, which is the arrangement that lets the
        // address be typed into while the title bar behind it stays lit.
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,

        // A caret over the whole box, not only over the strip of it the control
        // occupies. The pointer showing an arrow over an address is the window
        // telling the truth about which window is underneath and the wrong
        // thing to say about what will happen if it is clicked.
        WM_SETCURSOR => {
            let mut where_ = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::POINT>() };
            let mut rect = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::RECT>() };
            unsafe {
                GetCursorPos(&mut where_);
                GetWindowRect(hwnd, &mut rect);
            }
            let x = where_.x - rect.left;
            let y = where_.y - rect.top;
            let field = bar_layout(hwnd).field;
            if x >= field.left && x < field.right && y >= field.top && y < field.bottom {
                unsafe { SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_IBEAM)) };
                return 1;
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_MOUSEMOVE => {
            let x = (lparam & 0xffff) as i16 as i32;
            let y = ((lparam >> 16) & 0xffff) as i16 as i32;
            let over = bar_hot_at(&bar_layout(hwnd), x, y);
            if HOVERED.with(|cell| cell.replace(over)) != over {
                unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
            }
            let mut track = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            unsafe { TrackMouseEvent(&mut track) };
            0
        }
        // Nothing is watched for here any more. The row is part of the window;
        // it does not come and go, so there is nothing for the pointer leaving
        // to mean.
        MOUSE_LEFT_WINDOW => {
            if HOVERED.with(|cell| cell.replace(Hot::None)) != Hot::None {
                unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
            }
            0
        }
        WM_LBUTTONDOWN => {
            let x = (lparam & 0xffff) as i16 as i32;
            let y = ((lparam >> 16) & 0xffff) as i16 as i32;
            let parent = unsafe { GetParent(hwnd) };
            let layout = bar_layout(hwnd);
            // Anywhere in the box counts as reaching for the address.
            //
            // The control is only as tall as its text, so the rest of the box
            // is this window, and a click landing there has to be handed on.
            let field = layout.field;
            let in_field = x >= field.left && x < field.right && y >= field.top && y < field.bottom;
            if in_field {
                focus_address(hwnd, false);
                return 0;
            }
            match bar_hot_at(&layout, x, y) {
                Hot::Back => unsafe { PostMessageW(parent, WM_BROWSE, BROWSE_BACK, 0) },
                Hot::Forward => unsafe { PostMessageW(parent, WM_BROWSE, BROWSE_FORWARD, 0) },
                Hot::Reload => unsafe { PostMessageW(parent, WM_BROWSE, BROWSE_RELOAD, 0) },
                Hot::Home => unsafe { PostMessageW(parent, WM_BROWSE, BROWSE_HOME, 0) },
                _ => 0,
            };
            0
        }
        WM_CTLCOLOREDIT => {
            let page = PAGE_COLOUR.with(|cell| cell.get());
            let ink = readable_on(page);
            // The same fill the pill around it is painted with. Windows erases
            // the control with the brush this returns — not with the colour
            // SetBkColor names — so a fixed brush left a dark rectangle lying
            // across a pale box, which read as a rule with the address on it.
            let want = blend(page, ink, 0.10);
            let brush = FIELD_BRUSH.with(|cell| {
                let (had, brush) = cell.get();
                if brush != 0 && had == want {
                    return brush;
                }
                if brush != 0 {
                    unsafe { windows_sys::Win32::Graphics::Gdi::DeleteObject(brush as _) };
                }
                let made = unsafe { CreateSolidBrush(want) } as isize;
                cell.set((want, made));
                made
            });
            unsafe {
                windows_sys::Win32::Graphics::Gdi::SetBkColor(wparam as _, want);
                windows_sys::Win32::Graphics::Gdi::SetTextColor(wparam as _, ink);
            }
            brush as LRESULT
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// The address bar: a plain edit control, themed by hand.
fn create_address_bar(parent: HWND) -> Result<()> {
    let class: Vec<u16> = "EDIT\0".encode_utf16().collect();
    let edit = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            std::ptr::null(),
            WS_CHILD | WS_VISIBLE | (ES_AUTOHSCROLL as u32) | (ES_LEFT as u32),
            0,
            0,
            0,
            0,
            parent,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if edit.is_null() {
        return Err(anyhow!("주소창을 만들지 못했습니다"));
    }
    // An edit control does not report Enter to its parent, so its procedure is
    // replaced with one that does and defers everything else to the original.
    // Through a function pointer first: casting the item itself to an integer
    // is what the lint objects to, and the pointer is what Windows wants.
    let replacement: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT =
        address_procedure;
    let original =
        unsafe { SetWindowLongPtrW(edit, GWLP_WNDPROC, replacement as *const () as isize) };
    ORIGINAL_EDIT_PROC.with(|cell| cell.set(original));
    ADDRESS.with(|cell| cell.set(edit));
    // The same face the band is set in. A control given no font draws in a
    // bitmap face from an older Windows, which is the one thing on screen that
    // would not match anything else on it.
    let font = ui_font(text_height(scale_of(parent)), false);
    // WM_SETFONT. The control keeps the handle, so it is not deleted here.
    unsafe { SendMessageW(edit, 0x0030, font as WPARAM, 1) };
    BRUSHES.with(|cell| {
        cell.set((unsafe { CreateSolidBrush(FIELD) } as isize, unsafe {
            CreateSolidBrush(SURFACE)
        } as isize))
    });
    Ok(())
}

unsafe extern "system" fn address_procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_CHAR && wparam == 13 {
        // Swallow the character too, or the control beeps at a key it has no
        // use for.
        unsafe { PostMessageW(GetParent(hwnd), WM_NAVIGATE, 0, 0) };
        return 0;
    }
    let original = ORIGINAL_EDIT_PROC.with(|cell| cell.get());
    unsafe { CallWindowProcW(std::mem::transmute(original), hwnd, message, wparam, lparam) }
}

/// What the user typed, turned into something loadable.
fn typed_address() -> Option<String> {
    let edit = ADDRESS.with(|cell| cell.get());
    if edit.is_null() {
        return None;
    }
    let length = unsafe { GetWindowTextLengthW(edit) };
    if length <= 0 {
        return None;
    }
    let mut buffer = vec![0u16; length as usize + 1];
    let read = unsafe { GetWindowTextW(edit, buffer.as_mut_ptr(), buffer.len() as i32) };
    let typed = String::from_utf16_lossy(&buffer[..read.max(0) as usize]).trim().to_string();
    if typed.is_empty() {
        return None;
    }
    Some(as_url(&typed))
}

/// A bare word is a search; anything with a dot in it is an address.
fn as_url(typed: &str) -> String {
    if typed.starts_with("http://") || typed.starts_with("https://") {
        return typed.to_string();
    }
    let looks_like_host = typed.contains('.') && !typed.contains(' ');
    if looks_like_host {
        format!("https://{typed}")
    } else {
        format!("https://www.google.com/search?q={}", encode(typed))
    }
}

fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn show_address(url: &str) {
    let edit = ADDRESS.with(|cell| cell.get());
    if edit.is_null() {
        return;
    }
    let mut wide: Vec<u16> = url.encode_utf16().collect();
    wide.push(0);
    unsafe { SetWindowTextW(edit, wide.as_ptr()) };
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        // Take the caption away and keep the resize border. What is left is a
        // window whose client area starts at the very top, which is the only
        // way Windows lets a program put anything in the title bar.
        WM_NCCALCSIZE if wparam != 0 => {
            let params = lparam as *mut NCCALCSIZE_PARAMS;
            let rect = unsafe { &mut (*params).rgrc[0] };
            // A maximised window is sized to the whole monitor, borders and
            // all; without this the edges — and the taskbar — end up underneath
            // it. The frame is only in the way when the window is maximised.
            if unsafe { IsZoomed(hwnd) } != 0 {
                let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
                let frame = unsafe {
                    GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi)
                        + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
                };
                rect.left += frame;
                rect.right -= frame;
                rect.bottom -= frame;
                rect.top += frame;
            }
            0
        }

        // Where the pointer is, in a window that has no frame of its own.
        WM_NCHITTEST => {
            let x = (lparam & 0xffff) as i16 as i32;
            let y = ((lparam >> 16) & 0xffff) as i16 as i32;
            let mut window = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::RECT>() };
            unsafe { GetWindowRect(hwnd, &mut window) };
            let edge = RESIZE_EDGE;

            if unsafe { IsZoomed(hwnd) } == 0 {
                let left = x < window.left + edge;
                let right = x >= window.right - edge;
                let top = y < window.top + edge;
                let bottom = y >= window.bottom - edge;
                let hit = match (top, bottom, left, right) {
                    (true, _, true, _) => HTTOPLEFT,
                    (true, _, _, true) => HTTOPRIGHT,
                    (_, true, true, _) => HTBOTTOMLEFT,
                    (_, true, _, true) => HTBOTTOMRIGHT,
                    (true, ..) => HTTOP,
                    (_, true, ..) => HTBOTTOM,
                    (_, _, true, _) => HTLEFT,
                    (_, _, _, true) => HTRIGHT,
                    _ => 0,
                };
                if hit != 0 {
                    return hit as LRESULT;
                }
            }

            let layout = band(hwnd);
            let client_x = x - window.left;
            let client_y = y - window.top;
            if client_y < layout.height {
                // The buttons are ordinary client area so their clicks and
                // their hover arrive as ordinary messages. Everything else in
                // the band drags the window, and on a double-click maximises
                // it — which is what replaces the maximise button.
                if hot_at(&layout, client_x, client_y) != Hot::None {
                    return HTCLIENT as LRESULT;
                }
                return HTCAPTION as LRESULT;
            }
            HTCLIENT as LRESULT
        }

        WM_PAINT => {
            paint(hwnd);
            0
        }

        // Windows repaints its own frame when a window gains or loses focus,
        // and puts back the light border each time. Setting the attribute once
        // at creation was not enough — it had to be said again every time.
        // Answered without letting Windows redraw the frame. A window with no
        // caption of its own gets a light one drawn over it the moment it stops
        // being the active window — and the address bar taking the keyboard is
        // exactly that moment, so the frame flashed white every time the bar
        // appeared. Saying the frame is still active leaves it as drawn.
        WM_NCACTIVATE => 1,

        // A click on this window while the address bar holds the keyboard has
        // to do two things at once: bring the frame forward and press what was
        // pressed. Left to the default, the click that changes the active
        // window is sometimes spent doing only that — which is why pressing a
        // tab appeared to do nothing until the pointer moved and a second
        // message arrived. MA_ACTIVATE says activate *and* deliver.
        WM_MOUSEACTIVATE => MA_ACTIVATE as LRESULT,

        WM_ACTIVATE => {
            dark_title_bar(hwnd);
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }

        // A drawn button is only distinguishable from a drawn label when it
        // reacts, so each lights up under the pointer.
        WM_MOUSEMOVE => {
            let x = (lparam & 0xffff) as i16 as i32;
            let y = ((lparam >> 16) & 0xffff) as i16 as i32;
            let layout = band(hwnd);
            // Reaching the tabs brings the address down. The pointer cannot
            // reach the page this way — the web view takes those messages — so
            // this only ever fires on the frame, which is what makes it a
            // gesture rather than an accident.
            let over = hot_at(&layout, x, y);
            // The tab is what carries the address, so the tab is what reveals
            // it — not the whole band, which put it on screen every time the
            // pointer passed the close button on its way somewhere else.
            //
            // It stays while the pointer is inside the toolbar, or reaching
            // down to type in it would dismiss it on the way.
            // Only the tab already in front opens the address. Passing over
            // the others lights them and nothing more — a pointer crossing the
            // strip on its way somewhere else should not change what is on
            // screen. A click is what selects, and selecting opens the address
            // by itself.
            let active = ACTIVE.with(|cell| cell.get());
            // Reaching a tab that is already in front brings the address down.
            // Nothing here puts it away again: the bar is its own window now,
            // sitting below the band with a gap, and the pointer has to cross
            // that gap to reach it. Hiding on the way out of the band dismissed
            // the bar just as the pointer set off towards it. The bar decides
            // when it is done — it hears its own mouse leaving — and so does
            // the page.
            let _ = active;
            if HOVERED.with(|cell| cell.replace(over)) != over {
                unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
                // Asked for, or leaving the window would leave a button lit.
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                unsafe { TrackMouseEvent(&mut track) };
            }
            0
        }

        MOUSE_LEFT_WINDOW => {
            if HOVERED.with(|cell| cell.replace(Hot::None)) != Hot::None {
                unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
            }
            0
        }

        WM_LBUTTONDOWN => {
            let x = (lparam & 0xffff) as i16 as i32;
            let y = ((lparam >> 16) & 0xffff) as i16 as i32;
            match hot_at(&band(hwnd), x, y) {
                Hot::Close => unsafe {
                    PostMessageW(hwnd, WM_CLOSE, 0, 0);
                },
                Hot::Back => unsafe {
                    PostMessageW(hwnd, WM_BROWSE, BROWSE_BACK, 0);
                },
                Hot::Forward => unsafe {
                    PostMessageW(hwnd, WM_BROWSE, BROWSE_FORWARD, 0);
                },
                Hot::Reload => unsafe {
                    PostMessageW(hwnd, WM_BROWSE, BROWSE_RELOAD, 0);
                },
                Hot::Home => unsafe {
                    PostMessageW(hwnd, WM_BROWSE, BROWSE_HOME, 0);
                },
                Hot::Tab(index) => unsafe {
                    PostMessageW(hwnd, WM_TAB, TAB_SELECT, index as LPARAM);
                },
                Hot::TabClose(index) => unsafe {
                    PostMessageW(hwnd, WM_TAB, TAB_CLOSE, index as LPARAM);
                },
                Hot::NewTab => unsafe {
                    PostMessageW(hwnd, WM_TAB, TAB_NEW, 0);
                },
                Hot::None => {}
            }
            0
        }
        WM_MOVE => {
            // A popup does not travel with the window that owns it.
            fit(hwnd);
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }

        WM_SIZE => {
            // Done here rather than by asking the loop: while an edge is being
            // dragged, Windows runs a loop of its own and swallows anything
            // posted during it.
            fit(hwnd);
            // Cutting the window to a rounded shape used to happen here. It
            // never reached the page — a child window keeps its own square
            // corners whatever its parent is cut to — and it stopped the window
            // being resized smaller than it started. The page is held inside
            // the frame instead, which is what rounds the corners now.
            unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
            // The web view is resized from the loop rather than here, because
            // this runs inside a dispatch and the loop is where it is owned.
            unsafe { PostMessageW(hwnd, WM_SIZE_REQUEST, 0, 0) };
            0
        }
        WM_CLOSE => {
            // The pages are let go first, while the window that holds them is
            // still standing. WebView2 wants its host alive when the controller
            // closes; destroying the window first left it running a callback
            // against a window that no longer existed, and the exception coming
            // back out of that callback took the whole program down —
            // 0xc000041d in EmbeddedBrowserWebView.dll, every time, which is
            // why closing this window also emptied the tray.
            VIEWS.with(|cell| cell.borrow_mut().clear());
            let bar = BAR.with(|cell| cell.replace(std::ptr::null_mut()));
            if !bar.is_null() {
                unsafe { DestroyWindow(bar) };
            }
            // The address field is the bar's own child and went with it.
            ADDRESS.with(|cell| cell.set(std::ptr::null_mut()));
            TOOLBAR.with(|cell| cell.set(false));
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// Lets wry attach to a window this module created.
struct Host(HWND);

impl HasWindowHandle for Host {
    fn window_handle(&self) -> Result<WindowHandle<'_>, raw_window_handle::HandleError> {
        let mut handle = Win32WindowHandle::new(
            NonZeroIsize::new(self.0 as isize).ok_or(raw_window_handle::HandleError::Unavailable)?,
        );
        handle.hinstance = NonZeroIsize::new(unsafe { GetModuleHandleW(std::ptr::null()) } as isize);
        // Safety: the window outlives the web view — it is destroyed only when
        // the loop that owns both has already ended.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(handle)) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How tall one line of a font is, without needing a control to ask through.
    fn line_height(font: windows_sys::Win32::Graphics::Gdi::HFONT) -> i32 {
        use windows_sys::Win32::Graphics::Gdi::*;
        unsafe {
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            if dc.is_null() {
                return 0;
            }
            let old = SelectObject(dc, font as _);
            let mut metrics = std::mem::zeroed::<TEXTMETRICW>();
            let ok = GetTextMetricsW(dc, &mut metrics);
            SelectObject(dc, old);
            DeleteDC(dc);
            if ok == 0 {
                0
            } else {
                metrics.tmHeight
            }
        }
    }

    /// Does a single-line edit control honour a formatting rectangle?
    ///
    /// It matters because the address is centred by moving that rectangle. The
    /// documentation says the message belongs to multiline controls, which
    /// would make the centring silently do nothing — so it is asked here rather
    /// than assumed, against a real control on the machine that will run it.
    #[test]
    fn a_single_line_edit_takes_a_formatting_rectangle() {
        const EM_GETRECT: u32 = 0x00B2;
        const EM_SETRECT: u32 = 0x00B3;

        let class: Vec<u16> = "EDIT\0".encode_utf16().collect();
        let edit = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                WS_POPUP | (ES_AUTOHSCROLL as u32) | (ES_LEFT as u32),
                0,
                0,
                300,
                40,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!edit.is_null(), "could not make an edit control to ask");

        let mut wanted = windows_sys::Win32::Foundation::RECT {
            left: 0,
            top: 12,
            right: 300,
            bottom: 28,
        };
        unsafe { SendMessageW(edit, EM_SETRECT, 0, &mut wanted as *mut _ as LPARAM) };

        let mut got = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::RECT>() };
        unsafe { SendMessageW(edit, EM_GETRECT, 0, &mut got as *mut _ as LPARAM) };
        unsafe { DestroyWindow(edit) };

        // It does not. The address is centred by giving the control the height
        // of its own text and centring the control, not by moving a rectangle
        // the control declines to move.
        assert_eq!(
            got.top, 0,
            "a single-line edit started honouring the formatting rectangle; the address could be \
             centred that way now, with the control filling its box"
        );
    }

    /// The band and the address are set in one face at one size.
    ///
    /// They were not: the band's size was multiplied by the screen's scaling
    /// and the address's was not, so on any display above 100% the band grew
    /// and the address stayed where it was.
    #[test]
    fn the_band_and_the_address_are_set_at_one_size() {
        let harness = Harness::open();
        let bar = harness.bar();
        let edit = ADDRESS.with(|cell| cell.get());

        // What the band draws its tab titles with.
        let band_font = ui_font(text_height(scale_of(bar)), false);
        let band = (face_of(band_font), line_height(band_font));
        unsafe { windows_sys::Win32::Graphics::Gdi::DeleteObject(band_font as _) };

        // What the address control was actually given. WM_GETFONT.
        let held = unsafe { SendMessageW(edit, 0x0031, 0, 0) } as _;
        let address = (face_of(held), line_height(held));

        assert_eq!(band.1, address.1, "the band and the address are different sizes");
        assert_eq!(band.0, address.0, "the band and the address are different faces");
    }

    /// A real frame and a real toolbar, driven by real messages.
    ///
    /// Everything below is asked of the same window procedures the program
    /// runs, on the same thread, with the same messages Windows would send.
    /// No web view is created — the frame does not need one to lay out its
    /// band, place its address or decide what a click landed on.
    struct Harness {
        frame: HWND,
    }

    impl Harness {
        fn open() -> Self {
            let frame = create_window("Shard Browser test").expect("frame");
            // One tab, so the band has something to draw and something to hit.
            TABS.with(|cell| {
                let mut held = cell.borrow_mut();
                held.clear();
                held.push(Tab::new(claim_tab_id()));
                held.push(Tab::new(claim_tab_id()));
            });
            ACTIVE.with(|cell| cell.set(0));
            create_toolbar(frame).expect("toolbar");
            unsafe { MoveWindow(frame, 0, 0, 1100, 800, 0) };
            // Laid out by the same code the window uses. Positioning it here
            // instead was how the test came to be measuring a row the program
            // would never have put there.
            lay_out(frame, &[], 0);
            Harness { frame }
        }

        fn bar(&self) -> HWND {
            BAR.with(|cell| cell.get())
        }

        /// Deliver a message the way Windows does — to the procedure, in place.
        fn send(&self, window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
            unsafe { SendMessageW(window, message, wparam, lparam) }
        }

        fn at(x: i32, y: i32) -> LPARAM {
            ((y as u32) << 16 | (x as u32 & 0xffff)) as LPARAM
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let bar = BAR.with(|cell| cell.replace(std::ptr::null_mut()));
            if !bar.is_null() {
                unsafe { DestroyWindow(bar) };
            }
            ADDRESS.with(|cell| cell.set(std::ptr::null_mut()));
            TOOLBAR.with(|cell| cell.set(false));
            TABS.with(|cell| cell.borrow_mut().clear());
            unsafe { DestroyWindow(self.frame) };
        }
    }

    /// Clicking the address box has to leave the keyboard in the address box.
    ///
    /// This is the one that kept coming back. The bar is a window of its own,
    /// and the field inside it is shorter than the box it is drawn in, so a
    /// click can land on the bar rather than on the control — and the keyboard
    /// then belongs to neither.
    #[test]
    fn clicking_the_address_puts_the_keyboard_in_it() {
        let harness = Harness::open();
        let bar = harness.bar();
        assert!(!bar.is_null(), "no toolbar");
        set_toolbar(true);

        let field = bar_layout(bar).field;
        let edit = ADDRESS.with(|cell| cell.get());

        // Deliberately above the control: the top of the box, which is the
        // strip that belongs to the bar rather than to the field.
        let x = (field.left + field.right) / 2;
        let y = field.top + 1;
        let mut edit_rect = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::RECT>() };
        unsafe { GetWindowRect(edit, &mut edit_rect) };
        let mut bar_rect = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::RECT>() };
        unsafe { GetWindowRect(bar, &mut bar_rect) };
        assert!(
            y < edit_rect.top - bar_rect.top,
            "the test is not aiming above the control any more; it is at y={y} and the control \
             starts at {}",
            edit_rect.top - bar_rect.top
        );

        harness.send(bar, WM_LBUTTONDOWN, 0, Harness::at(x, y));

        let focused = unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetFocus() };
        assert_eq!(
            focused, edit,
            "a click on the address box left the keyboard somewhere else"
        );
    }

    /// And a key pressed after that click has to arrive as text.
    #[test]
    fn typing_after_that_click_reaches_the_address() {
        let harness = Harness::open();
        let bar = harness.bar();
        set_toolbar(true);
        let field = bar_layout(bar).field;
        let edit = ADDRESS.with(|cell| cell.get());
        show_address("");
        harness.send(
            bar,
            WM_LBUTTONDOWN,
            0,
            Harness::at((field.left + field.right) / 2, field.top + 1),
        );

        // WM_CHAR is what a key becomes once translated, and it goes to
        // whichever window holds the focus.
        let focused = unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetFocus() };
        for ch in "shard".encode_utf16() {
            harness.send(focused, WM_CHAR, ch as WPARAM, 0);
        }

        let mut buffer = [0u16; 64];
        let read = unsafe { GetWindowTextW(edit, buffer.as_mut_ptr(), buffer.len() as i32) };
        let typed = String::from_utf16_lossy(&buffer[..read.max(0) as usize]);
        assert_eq!(typed, "shard", "what was typed did not reach the address");
    }

    /// The address row is part of the frame and does not overlap the page.
    ///
    /// It used to be a top-level window floating over the page, which is what
    /// made it need the activation to take the keyboard — and taking the
    /// activation is what made a click on a tab land on nothing and the address
    /// go dead a moment after it appeared. A child that shares the window can
    /// simply be focused, and a row that owns its own pixels can be drawn in.
    #[test]
    fn the_address_row_belongs_to_the_frame_and_clears_the_page() {
        let harness = Harness::open();
        let bar = harness.bar();

        let style = unsafe { GetWindowLongW(bar, GWL_STYLE) } as u32;
        assert!(style & WS_CHILD != 0, "the address row is not part of the frame");
        assert_eq!(
            unsafe { GetParent(bar) },
            harness.frame,
            "the address row is not held by the frame"
        );

        let layout = band(harness.frame);
        let mut rect = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::RECT>() };
        let mut origin = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
        unsafe {
            GetWindowRect(bar, &mut rect);
            windows_sys::Win32::Graphics::Gdi::ClientToScreen(harness.frame, &mut origin);
        }
        let bottom = rect.bottom - origin.y;
        assert!(
            bottom < layout.page_top,
            "the address row reaches {bottom}, into the page which starts at {}",
            layout.page_top
        );
    }

    /// One click on a tab selects it. Not the second, and not after moving.
    #[test]
    fn one_click_selects_a_tab() {
        let harness = Harness::open();
        let layout = band(harness.frame);
        let second = layout.tabs.get(1).copied().expect("two tabs");
        assert_eq!(ACTIVE.with(|cell| cell.get()), 0);

        let x = (second.left + second.right) / 2;
        let y = (second.top + second.bottom) / 2;
        // The frame reports this spot as ordinary client area, or the click
        // would begin a window drag instead of pressing anything.
        let mut window = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::RECT>() };
        unsafe { GetWindowRect(harness.frame, &mut window) };
        let hit = harness.send(
            harness.frame,
            WM_NCHITTEST,
            0,
            Harness::at(window.left + x, window.top + y),
        );
        assert_eq!(hit, HTCLIENT as LRESULT, "a tab is not treated as clickable");

        harness.send(harness.frame, WM_LBUTTONDOWN, 0, Harness::at(x, y));
        // The frame posts to itself and the loop acts on it, so the test plays
        // the part of the loop.
        pump(harness.frame);
        assert_eq!(
            ACTIVE.with(|cell| cell.get()),
            1,
            "one click on a tab did not select it"
        );
    }

    /// Run the posted messages the window procedure sent to itself.
    fn pump(frame: HWND) {
        let mut message = unsafe { std::mem::zeroed::<MSG>() };
        while unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            if message.message == WM_TAB && message.wParam == TAB_SELECT {
                let which = message.lParam as usize;
                let count = TABS.with(|cell| cell.borrow().len());
                if which < count {
                    ACTIVE.with(|cell| cell.set(which));
                }
                continue;
            }
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        let _ = frame;
    }

    /// Where does a single-line edit put its text when it is taller than that
    /// text — at the top, or in the middle?
    ///
    /// The answer decides whether the control can fill the box it is drawn in.
    /// If it centres, it can, and every click and every cursor over the box
    /// belongs to the control. If it does not, the control has to be the height
    /// of its text and the box has to work around it.
    #[test]
    fn where_a_tall_edit_draws_its_text() {
        const EM_POSFROMCHAR: u32 = 0x00D6;
        const TALL: i32 = 40;

        let class: Vec<u16> = "EDIT\0".encode_utf16().collect();
        let edit = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                WS_POPUP | (ES_AUTOHSCROLL as u32) | (ES_LEFT as u32),
                0,
                0,
                300,
                TALL,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!edit.is_null(), "could not make an edit control to ask");

        let mut text: Vec<u16> = "https://example.com".encode_utf16().collect();
        text.push(0);
        unsafe { SetWindowTextW(edit, text.as_ptr()) };

        let placed = unsafe { SendMessageW(edit, EM_POSFROMCHAR, 0, 0) };
        let y = ((placed >> 16) & 0xffff) as i16 as i32;
        let line = font_height(edit);
        unsafe { DestroyWindow(edit) };

        let centred = (TALL - line) / 2;
        assert!(
            y < centred / 2,
            "a tall single-line edit centres its text after all — it drew at y={y} in a control \
             {TALL} tall with a {line}-tall line, where centred would be {centred}. The control \
             can fill its box."
        );
    }
}

#[cfg(test)]
mod appearance {
    use super::*;

    /// Draw the frame into a bitmap and hand back a way to read its pixels.
    ///
    /// No window is created and nothing appears on screen. The frame's
    /// appearance becomes something that can be asked about directly, which is
    /// what this had been missing: a line drawn where something else covered it
    /// is invisible on screen and obvious here.
    fn painted(width: i32, height: i32, frame: &Frame) -> (Vec<u32>, i32) {
        use windows_sys::Win32::Graphics::Gdi::*;

        let client = windows_sys::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        let mut pixels = vec![0u32; (width * height) as usize];
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            let dc = CreateCompatibleDC(screen);
            let surface = CreateCompatibleBitmap(screen, width, height);
            let previous = SelectObject(dc, surface as _);

            draw_frame(dc, &client, frame);

            for y in 0..height {
                for x in 0..width {
                    pixels[(y * width + x) as usize] = GetPixel(dc, x, y);
                }
            }

            SelectObject(dc, previous);
            DeleteObject(surface as _);
            DeleteDC(dc);
            ReleaseDC(std::ptr::null_mut(), screen);
        }
        (pixels, width)
    }

    fn frame_with(tabs: usize, active: usize, page: u32, width: i32, scale: i32) -> Frame {
        Frame {
            layout: band_of(width, scale, tabs),
            page,
            scale,
            hovered: Hot::None,
            active,
            tabs: (0..tabs).map(|i| Tab::new(i as u64 + 1)).collect(),
        }
    }

    /// The line between the window and the page runs the whole width, unbroken.
    ///
    /// It kept arriving in pieces. Drawn on the boundary itself the web view
    /// covered it; drawn a row higher each tab's own fill covered the stretch it
    /// sat on. Both were symptoms of the same thing — the frame trying to draw
    /// where it did not own the pixels — and the address row now sits between
    /// the tabs and the page, so this row belongs to the frame alone.
    #[test]
    fn the_line_under_the_frame_is_unbroken() {
        const WIDTH: i32 = 1100;
        const HEIGHT: i32 = 400;
        const PAGE: u32 = 0x00FF_FFFF;

        let frame = frame_with(3, 1, PAGE, WIDTH, 96);
        let (pixels, stride) = painted(WIDTH, HEIGHT, &frame);
        let divider = frame.layout.page_top - 1;

        for x in 0..WIDTH {
            assert_ne!(
                pixels[(divider * stride + x) as usize],
                PAGE,
                "the line is missing at x={x}"
            );
        }
    }

    /// The edge the tabs stand on is open under the tab in front, and drawn
    /// everywhere else — so the outline of that tab runs out into it.
    #[test]
    fn the_edge_under_the_tabs_is_open_under_the_front_one() {
        const WIDTH: i32 = 1100;
        const HEIGHT: i32 = 400;
        const PAGE: u32 = 0x00FF_FFFF;

        let frame = frame_with(3, 1, PAGE, WIDTH, 96);
        let (pixels, stride) = painted(WIDTH, HEIGHT, &frame);
        let row = frame.layout.height;
        let at = |x: i32| pixels[(row * stride + x) as usize];

        let middle = |t: &windows_sys::Win32::Foundation::RECT| (t.left + t.right) / 2;
        assert_eq!(
            at(middle(&frame.layout.tabs[1])),
            PAGE,
            "the tab in front is closed off from the row below it"
        );
        assert_ne!(at(middle(&frame.layout.tabs[0])), PAGE, "the edge breaks under a tab behind");
        assert_ne!(at(middle(&frame.layout.tabs[2])), PAGE, "the edge breaks under a tab behind");
        assert_ne!(at(4), PAGE, "no edge beside the tabs");
    }

    /// And it is one colour the whole way across.
    #[test]
    fn the_line_is_one_colour_across_its_length() {
        const WIDTH: i32 = 1100;
        const HEIGHT: i32 = 400;
        const PAGE: u32 = 0x0020_2020;

        let frame = frame_with(3, 0, PAGE, WIDTH, 96);
        let (pixels, stride) = painted(WIDTH, HEIGHT, &frame);
        let divider = frame.layout.page_top - 1;

        let shades: std::collections::BTreeSet<u32> =
            (0..WIDTH).map(|x| pixels[(divider * stride + x) as usize]).collect();
        assert_eq!(
            shades.len(),
            1,
            "the line is drawn in {} different shades along its length",
            shades.len()
        );
    }
}
