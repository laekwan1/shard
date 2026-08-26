import SwiftUI
import WebKit
import Network
import UIKit

/// The page. Owns the WKWebView so navigation state lives in one observable
/// place the UI binds to. Media is not tracked continuously any more — the app
/// asks the page for an offer (Ask.js) at the moment the user wants to download.
final class WebModel: NSObject, ObservableObject, WKNavigationDelegate, WKScriptMessageHandler {
    @Published var address: String = ""
    @Published var pageTitle: String = ""
    @Published var canGoBack = false
    @Published var canGoForward = false
    @Published var isLoading = false
    /// Page load progress 0…1, for the thin bar that shows whether a page is still
    /// connecting or done — asked for after a blocked site left only a white screen
    /// with no sign of whether it was still trying.
    @Published var progress: Double = 0
    /// The download button on a video was pressed, with its bottom-right corner
    /// (in web points) so the list can drop right under it.
    var onDownloadRequest: ((_ right: CGFloat, _ bottom: CGFloat) -> Void)?
    /// A quality row in the in-page list was chosen (its itag).
    var onPick: ((UInt32) -> Void)?

    /// Render the quality list in the page, under the button that asked for it.
    func sendQualities(_ json: String) {
        webView.evaluateJavaScript("window.__shardQualities(\(json))", completionHandler: nil)
    }

    /// Pause anything the page is playing — called when the library opens over
    /// it, so the page's sound does not run under the library's player.
    func pauseWebVideos() {
        webView.evaluateJavaScript(
            "document.querySelectorAll('video').forEach(function(v){try{v.pause()}catch(e){}})",
            completionHandler: nil)
    }
    /// The `Cookie:` header the page would send for `urlString`, pulled from the
    /// web view's own cookie store — the download engine runs outside the browser,
    /// so a site that gates its media behind a session cookie (pornhub) needs this
    /// handed over, the same way the Android app shares its WebView cookies.
    func cookieHeader(for urlString: String) async -> String {
        guard let host = URL(string: urlString)?.host else { return "" }
        let store = webView.configuration.websiteDataStore.httpCookieStore
        let all: [HTTPCookie] = await withCheckedContinuation { cont in
            store.getAllCookies { cont.resume(returning: $0) }
        }
        let pairs = all
            .filter { cookie in
                let d = cookie.domain.hasPrefix(".") ? String(cookie.domain.dropFirst()) : cookie.domain
                return host == d || host.hasSuffix("." + d)
            }
            .map { "\($0.name)=\($0.value)" }
        return pairs.joined(separator: "; ")
    }

    /// The page's own User-Agent, handed to the download engine so its requests match
    /// the browser session — some CDNs (pornhub's) refuse a request whose UA does not
    /// look like the browser that got the signed URL. The Android app passes this too.
    func userAgent() async -> String {
        let ua = try? await webView.evaluateJavaScript("navigator.userAgent")
        return (ua as? String) ?? ""
    }

    /// Mark whether the browser is the visible screen. While it is not (the library
    /// is up), Capture.js cancels any full screen the page video tries to enter —
    /// otherwise the library's forced landscape sent a playing pornhub video to
    /// native full screen, hijacking the library's own.
    func setBrowserActive(_ on: Bool) {
        // Set the flag AND (when going inactive) pause videos — in the main frame
        // and every child frame. pornhub plays inside an <iframe>, and a main-frame-
        // only call left that video running, so it still hijacked full screen. The
        // relay postMessage reaches each frame's Capture.js, which sets its own flag.
        let js = """
        (function(on){
          try { window.__shardBrowserActive = on; } catch(e) {}
          if (!on) { try { document.querySelectorAll('video').forEach(function(v){ try{v.pause()}catch(e){} }); } catch(e) {} }
          try { for (var i=0;i<window.frames.length;i++){ try{ window.frames[i].postMessage({__shard:'active', on:on}, '*'); }catch(e){} } } catch(e) {}
        })(\(on ? "true" : "false"));
        """
        webView.evaluateJavaScript(js, completionHandler: nil)
    }
    /// Called when the page starts navigating, so the address panel can retreat.
    var onNavigated: (() -> Void)?
    /// Set when we reload for our own reasons (engine toggle), so the reload does
    /// not retreat the address panel — the user just tapped a control on it and
    /// expects it to stay put.
    private var suppressNavigatedClose = false
    /// Centre swipes: a rightward one opens the address panel, a leftward one the
    /// library. Bound to gesture recognizers on the web view itself so the page
    /// keeps its own taps and scrolls.
    var onSwipeAddress: (() -> Void)?
    var onSwipeLibrary: (() -> Void)?
    /// The page was tapped — used to retreat the address panel.
    var onWebTap: (() -> Void)?
    /// True while a web video is playing full screen, so the download button hides.
    @Published var videoFullscreen = false
    /// A web video started (true) or stopped (false) playing — so the library's
    /// player can pause and resume around it.
    var onWebPlaying: ((Bool) -> Void)?
    /// Whether the bypass engine is on (the local proxy is running and the
    /// WebView is pointed at it).
    @Published var engineOn = false

    /// Turn the DPI/SNI-bypass engine on or off. Starts the local proxy and aims
    /// the WebView at it (iOS 17+, where WKWebView takes a proxy); reloads so the
    /// change takes effect.
    func toggleEngine() {
        let current = webView.url
        if engineOn {
            shard_stop()
            proxyPort = 0
            engineOn = false
            // Turning OFF must block a blocked site AT ONCE. Removing the proxy alone
            // was not enough: the TLS connections already opened THROUGH the bypass
            // stay alive in the network process and get reused, so the site kept
            // loading (the desync only matters at the first handshake). A whole new
            // data store is a new network context — every warm connection is dropped —
            // so the reload goes out on fresh connections that DPI blocks again. This
            // also clears the session (sites log out); the user chose that for a hard off.
            rebuildWebView(store: WKWebsiteDataStore.nonPersistent(), reload: current)
        } else {
            let bound = shard_start(nil, 0)
            guard bound > 0 else { return }
            proxyPort = UInt16(bound)
            engineOn = true
            rebuildWebView(store: WKWebsiteDataStore.default(), reload: current)
        }
    }

    /// The bound port of the local bypass proxy, or 0 when the engine is off. Kept
    /// so a freshly rebuilt web view can be pointed at it.
    private var proxyPort: UInt16 = 0

    /// Swap in a brand-new WKWebView on `store`, dropping the old session and all its
    /// connections, then load `reload` (the current page). Bumping `generation` makes
    /// the SwiftUI container replace the displayed view.
    private func rebuildWebView(store: WKWebsiteDataStore, reload url: URL?) {
        // Detach the old view cleanly (it otherwise keeps `self` alive through its
        // message handler).
        webView.configuration.userContentController.removeAllScriptMessageHandlers()
        webView.navigationDelegate = nil
        urlObservation = nil

        webView = makeWebView(store: store)
        generation += 1
        installAdBlock()          // the fresh view has no rules yet; re-apply them
        suppressNavigatedClose = true
        if let url {
            webView.load(URLRequest(url: url))
        } else if let u = URL(string: Self.normalize(address)) {
            webView.load(URLRequest(url: u))
        }
    }

    private func makeWebView(store: WKWebsiteDataStore) -> WKWebView {
        let controller = WKUserContentController()
        if let js = Self.script("Capture") {
            controller.addUserScript(
                WKUserScript(source: js, injectionTime: .atDocumentStart, forMainFrameOnly: false)
            )
        }
        controller.add(self, name: "shard")
        let config = WKWebViewConfiguration()
        config.userContentController = controller
        config.allowsInlineMediaPlayback = true
        config.websiteDataStore = store
        // Point the session at the bypass proxy when the engine is on (iOS 17+).
        if proxyPort > 0, #available(iOS 17.0, *),
           let p = NWEndpoint.Port(rawValue: proxyPort) {
            let endpoint = NWEndpoint.hostPort(host: "127.0.0.1", port: p)
            store.proxyConfigurations = [ProxyConfiguration(httpCONNECTProxy: endpoint)]
        }

        // Let SwiftUI size it (frame .zero at birth). An earlier attempt to start it
        // at UIScreen.main.bounds "to fix first-load width" instead made pages render
        // zoomed/cut everywhere (the internal layout width no longer matched the
        // displayed frame), so it is reverted.
        let view = WKWebView(frame: .zero, configuration: config)
        view.navigationDelegate = self
        view.allowsBackForwardNavigationGestures = true
        // Report dark to pages (prefers-color-scheme: dark), so Google and other
        // sites match the app's dark theme.
        view.overrideUserInterfaceStyle = .dark
        // Track the URL directly, not just via didFinish: YouTube/xvideos navigate
        // by pushState (SPA), which fires no navigation callback — so the address
        // bar kept showing the URL from the first load. KVO on `url` catches those.
        urlObservation = view.observe(\.url, options: [.new]) { [weak self] _, _ in
            DispatchQueue.main.async { self?.sync() }
        }
        progressObservation = view.observe(\.estimatedProgress, options: [.new]) { [weak self] wv, _ in
            DispatchQueue.main.async { self?.progress = wv.estimatedProgress }
        }
        return view
    }

    /// The live web view. Replaced whole on an engine toggle (see rebuildWebView).
    private(set) lazy var webView: WKWebView = makeWebView(store: .default())
    private var urlObservation: NSKeyValueObservation?
    private var progressObservation: NSKeyValueObservation?
    /// Bumped whenever the web view is replaced, so the SwiftUI container swaps it in.
    @Published var generation = 0

    private static func script(_ name: String) -> String? {
        guard let url = Bundle.main.url(forResource: name, withExtension: "js"),
              let text = try? String(contentsOf: url, encoding: .utf8) else { return nil }
        return text
    }

    // MARK: navigation

    /// The address the user last asked to open, so a failed load keeps the bar on it
    /// instead of snapping back to the previous page.
    private var lastRequested = ""

    func load(_ text: String) {
        guard let url = URL(string: Self.normalize(text)) else { return }
        lastRequested = url.absoluteString
        webView.load(URLRequest(url: url))
    }

    /// Turn what the user typed into a URL: a bare query becomes a search, a
    /// host without scheme gets https.
    static func normalize(_ text: String) -> String {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty { return "about:blank" }
        if trimmed.contains(" ") || !trimmed.contains(".") {
            let q = trimmed.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? trimmed
            return "https://www.google.com/search?q=\(q)"
        }
        if trimmed.hasPrefix("http://") || trimmed.hasPrefix("https://") { return trimmed }
        return "https://\(trimmed)"
    }

    func goBack() { webView.goBack() }
    func goForward() { webView.goForward() }
    func reload() { webView.reload() }

    /// Block ad/tracker HOSTS only — never the video CDN (googlevideo.com), the
    /// API, or youtube.com itself. So if these ever stop matching, ads return but
    /// nothing about YouTube breaks. Compiled once and attached to the content
    /// controller; it applies to every navigation after.
    func installAdBlock() {
        let rules = """
        [
          {"trigger":{"url-filter":"doubleclick\\\\.net"},"action":{"type":"block"}},
          {"trigger":{"url-filter":"googlesyndication\\\\.com"},"action":{"type":"block"}},
          {"trigger":{"url-filter":"googleadservices\\\\.com"},"action":{"type":"block"}},
          {"trigger":{"url-filter":"google-analytics\\\\.com"},"action":{"type":"block"}},
          {"trigger":{"url-filter":"googletagservices\\\\.com"},"action":{"type":"block"}},
          {"trigger":{"url-filter":"googletagmanager\\\\.com"},"action":{"type":"block"}},
          {"trigger":{"url-filter":"/pagead/"},"action":{"type":"block"}},
          {"trigger":{"url-filter":"/ptracking"},"action":{"type":"block"}},
          {"trigger":{"url-filter":"/api/stats/ads"},"action":{"type":"block"}}
        ]
        """
        WKContentRuleListStore.default()?.compileContentRuleList(
            forIdentifier: "shard-adblock", encodedContentRuleList: rules) { [weak self] list, _ in
            guard let self = self, let list = list else { return }
            self.webView.configuration.userContentController.add(list)
        }
    }

    /// Ask the current page what it has to offer — runs Ask.js and returns the
    /// offer JSON (formats for YouTube, or a media/hls URL otherwise).
    func offer() async -> String? {
        guard let js = Self.script("Ask") else { return nil }
        return await withCheckedContinuation { continuation in
            webView.evaluateJavaScript(js) { result, _ in
                continuation.resume(returning: result as? String)
            }
        }
    }

    // MARK: WKScriptMessageHandler

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        guard message.name == "shard", let body = message.body as? [String: Any] else { return }
        switch body["type"] as? String {
        case "download":
            let right = (body["right"] as? NSNumber)?.doubleValue ?? 0
            let bottom = (body["bottom"] as? NSNumber)?.doubleValue ?? 0
            onDownloadRequest?(CGFloat(right), CGFloat(bottom))
        case "pick": if let itag = (body["itag"] as? NSNumber)?.uint32Value { onPick?(itag) }
        case "fullscreen": videoFullscreen = (body["on"] as? Bool) ?? false
        case "webplaying": onWebPlaying?((body["on"] as? Bool) ?? false)
        default: break
        }
    }

    // MARK: WKNavigationDelegate

    func webView(_ webView: WKWebView, didStartProvisionalNavigation navigation: WKNavigation!) {
        isLoading = true
        if suppressNavigatedClose { suppressNavigatedClose = false }
        else { onNavigated?() }
        sync()
    }
    func webView(_ webView: WKWebView, didCommit navigation: WKNavigation!) {
        // Reflect the URL as soon as the new page commits, not only on finish — a
        // reload/redirect updates the bar sooner and more reliably.
        sync()
    }
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        isLoading = false
        pageTitle = webView.title ?? ""
        sync()
    }
    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        isLoading = false
        sync()
    }
    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        // A load that never commits (an HTTPS site blocked by SNI reset) left a blank
        // white screen with no word why. Show a Chrome-style "cannot connect" page at
        // the attempted address so it reads like an ordinary browser. (An HTTP block
        // that redirects to the government warning page SUCCEEDS instead and shows
        // through untouched.) Not for a plain cancel (-999), a normal interrupted load.
        isLoading = false
        let ns = error as NSError
        if ns.code == NSURLErrorCancelled { sync(); return }
        let failed = (ns.userInfo[NSURLErrorFailingURLStringErrorKey] as? String) ?? lastRequested
        let host = URL(string: failed)?.host ?? failed
        let page = """
        <html><head><meta name="viewport" content="width=device-width, initial-scale=1"></head>
        <body style="margin:0;background:#141414;color:#e8e6e3;font:15px -apple-system;\
        display:flex;align-items:center;justify-content:center;min-height:100vh">
        <div style="max-width:460px;padding:28px">
        <div style="font-size:40px;margin-bottom:8px">⚠️</div>
        <p style="font-size:18px;font-weight:600;margin:0 0 8px">이 사이트에 연결할 수 없습니다</p>
        <p style="color:#9a9a9a;word-break:break-all;margin:0 0 14px"><b>\(host)</b> 의 연결이 거부되었습니다.</p>
        <p style="color:#7a7a7a;font-size:13px;margin:0">ERR_CONNECTION_CLOSED · 차단되었거나 응답이 없습니다. 우회 전원을 켜고 다시 시도해 보세요.</p>
        </div></body></html>
        """
        webView.loadHTMLString(page, baseURL: URL(string: failed))
        address = failed
    }

    private func sync() {
        address = webView.url?.absoluteString ?? address
        canGoBack = webView.canGoBack
        canGoForward = webView.canGoForward
    }
}

/// Hosts the model's WKWebView inside SwiftUI, with pull-to-refresh — a drag
/// down from the top of the page reloads it, like the phone app.
struct WebViewContainer: UIViewRepresentable {
    let model: WebModel

    func makeCoordinator() -> Coordinator { Coordinator(model) }

    func makeUIView(context: Context) -> WKWebView {
        // Return the web view directly, as this always did before the zoom detour. A
        // run of "fixes" (screen-size initial frame, .frame(width:) clamps, an
        // Auto-Layout host that hugged the page's content) each INTRODUCED the very
        // width runaway they were meant to cure — YouTube, which was fine, started
        // rendering zoomed/cut only after them. The plain return is the version that
        // worked; the pornhub page's own zoom is a separate, page-side thing.
        let view = model.webView
        let refresh = UIRefreshControl()
        refresh.addTarget(context.coordinator, action: #selector(Coordinator.reload), for: .valueChanged)
        view.scrollView.refreshControl = refresh
        context.coordinator.refresh = refresh

        // Centre swipes for address/library, added to the web view rather than
        // an overlay so the page still gets its own taps and scrolls (the
        // overlay approach swallowed every touch). cancelsTouchesInView = false
        // and simultaneous recognition keep the web working underneath.
        for direction in [UISwipeGestureRecognizer.Direction.right, .left] {
            let swipe = UISwipeGestureRecognizer(target: context.coordinator,
                                                 action: #selector(Coordinator.swiped(_:)))
            swipe.direction = direction
            swipe.cancelsTouchesInView = false
            swipe.delegate = context.coordinator
            view.addGestureRecognizer(swipe)
        }
        let tap = UITapGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.tapped))
        tap.cancelsTouchesInView = false
        tap.delegate = context.coordinator
        view.addGestureRecognizer(tap)
        // A drag/scroll on the page also retreats the address panel.
        let pan = UIPanGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.panned(_:)))
        pan.cancelsTouchesInView = false
        pan.delegate = context.coordinator
        view.addGestureRecognizer(pan)
        return view
    }

    func updateUIView(_ uiView: WKWebView, context: Context) {}

    // Size to the PROPOSED size (the window), never to the web content. On iOS 16+
    // SwiftUI otherwise asks the WKWebView how big it wants to be and a wide page
    // answered wider than the screen — which fed back and grew the layout, cutting
    // the right (address bar included). Returning the proposal caps it at the window.
    // (Not called on iOS 15, which used the proposal already.)
    @available(iOS 16.0, *)
    func sizeThatFits(_ proposal: ProposedViewSize, uiView: WKWebView, context: Context) -> CGSize? {
        CGSize(width: proposal.width ?? UIScreen.main.bounds.width,
               height: proposal.height ?? UIScreen.main.bounds.height)
    }

    final class Coordinator: NSObject, UIGestureRecognizerDelegate {
        let model: WebModel
        weak var refresh: UIRefreshControl?
        init(_ model: WebModel) { self.model = model }

        @objc func reload() {
            model.reload()
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { self.refresh?.endRefreshing() }
        }

        @objc func tapped() { model.onWebTap?() }
        @objc func panned(_ g: UIPanGestureRecognizer) {
            // Only a vertical drag (a scroll) retreats the address panel. A
            // horizontal drag is an address/library swipe, which closes the panel
            // itself in the right order — letting the pan close it here first made
            // the swipe skip straight to the library.
            guard g.state == .began else { return }
            // Leave the bottom strip (Shorts progress bar) to the page.
            if let view = g.view, g.location(in: view).y > view.bounds.height * 0.85 { return }
            let v = g.velocity(in: g.view)
            if abs(v.y) > abs(v.x) { model.onWebTap?() }
        }

        @objc func swiped(_ g: UISwipeGestureRecognizer) {
            guard let view = g.view else { return }
            let p = g.location(in: view)
            let x = p.x, w = view.bounds.width
            // Leave the bottom strip to the page: that is where YouTube Shorts puts
            // its draggable progress bar, and our swipes were stealing it.
            guard p.y < view.bounds.height * 0.85 else { return }
            // Leave a margin at each edge for the web view's own back/forward swipe.
            // The RIGHT margin is a little wider (36pt): the far-right edge is "go
            // forward" in the page, and a too-close library pull kept triggering there.
            guard x > 24, x < w - 36 else { return }
            if g.direction == .right { model.onSwipeAddress?() }
            else { model.onSwipeLibrary?() }
        }

        // Ride alongside the web view's own gestures rather than blocking them.
        func gestureRecognizer(_ g: UIGestureRecognizer,
                               shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool { true }

        // Keep our recognizers out of the bottom strip so the page owns its own
        // controls there. (YouTube Shorts only exposes a draggable scrubber while
        // paused — that is the page's own behaviour, not something we can force.)
        func gestureRecognizer(_ g: UIGestureRecognizer,
                               shouldReceive touch: UITouch) -> Bool {
            guard let view = g.view else { return true }
            return touch.location(in: view).y < view.bounds.height * 0.88
        }
    }
}
