import SwiftUI
import WebKit
import Network

/// The page. Owns the WKWebView so navigation state lives in one observable
/// place the UI binds to. Media is not tracked continuously any more — the app
/// asks the page for an offer (Ask.js) at the moment the user wants to download.
final class WebModel: NSObject, ObservableObject, WKNavigationDelegate, WKScriptMessageHandler {
    @Published var address: String = ""
    @Published var pageTitle: String = ""
    @Published var canGoBack = false
    @Published var canGoForward = false
    @Published var isLoading = false
    /// The download button on a video was pressed — the browser fetches the
    /// quality rows and hands them back with `sendQualities`.
    var onDownloadRequest: (() -> Void)?
    /// A quality row in the in-page list was chosen (its itag).
    var onPick: ((UInt32) -> Void)?

    /// Render the quality list in the page, under the button that asked for it.
    func sendQualities(_ json: String) {
        webView.evaluateJavaScript("window.__shardQualities(\(json))", completionHandler: nil)
    }
    /// Called when the page starts navigating, so the address panel can retreat.
    var onNavigated: (() -> Void)?
    /// Centre swipes: a rightward one opens the address panel, a leftward one the
    /// library. Bound to gesture recognizers on the web view itself so the page
    /// keeps its own taps and scrolls.
    var onSwipeAddress: (() -> Void)?
    var onSwipeLibrary: (() -> Void)?
    /// True while a web video is playing full screen, so the download button hides.
    @Published var videoFullscreen = false
    /// Whether the bypass engine is on (the local proxy is running and the
    /// WebView is pointed at it).
    @Published var engineOn = false

    /// Turn the DPI/SNI-bypass engine on or off. Starts the local proxy and aims
    /// the WebView at it (iOS 17+, where WKWebView takes a proxy); reloads so the
    /// change takes effect.
    func toggleEngine() {
        if engineOn {
            shard_stop()
            setProxy(port: 0)
            engineOn = false
        } else {
            let bound = shard_start(nil, 0)
            guard bound > 0 else { return }
            setProxy(port: UInt16(bound))
            engineOn = true
        }
        webView.reload()
    }

    private func setProxy(port: UInt16) {
        guard #available(iOS 17.0, *) else { return }
        let store = webView.configuration.websiteDataStore
        if port > 0, let p = NWEndpoint.Port(rawValue: port) {
            let endpoint = NWEndpoint.hostPort(host: "127.0.0.1", port: p)
            store.proxyConfigurations = [ProxyConfiguration(httpCONNECTProxy: endpoint)]
        } else {
            store.proxyConfigurations = []
        }
    }

    lazy var webView: WKWebView = {
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

        let view = WKWebView(frame: .zero, configuration: config)
        view.navigationDelegate = self
        view.allowsBackForwardNavigationGestures = true
        return view
    }()

    private static func script(_ name: String) -> String? {
        guard let url = Bundle.main.url(forResource: name, withExtension: "js"),
              let text = try? String(contentsOf: url, encoding: .utf8) else { return nil }
        return text
    }

    // MARK: navigation

    func load(_ text: String) {
        guard let url = URL(string: Self.normalize(text)) else { return }
        webView.load(URLRequest(url: url))
    }

    /// Turn what the user typed into a URL: a bare query becomes a search, a
    /// host without scheme gets https.
    static func normalize(_ text: String) -> String {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty { return "about:blank" }
        if trimmed.contains(" ") || !trimmed.contains(".") {
            let q = trimmed.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? trimmed
            return "https://duckduckgo.com/?q=\(q)"
        }
        if trimmed.hasPrefix("http://") || trimmed.hasPrefix("https://") { return trimmed }
        return "https://\(trimmed)"
    }

    func goBack() { webView.goBack() }
    func goForward() { webView.goForward() }
    func reload() { webView.reload() }

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
        case "download": onDownloadRequest?()
        case "pick": if let itag = (body["itag"] as? NSNumber)?.uint32Value { onPick?(itag) }
        case "fullscreen": videoFullscreen = (body["on"] as? Bool) ?? false
        default: break
        }
    }

    // MARK: WKNavigationDelegate

    func webView(_ webView: WKWebView, didStartProvisionalNavigation navigation: WKNavigation!) {
        isLoading = true
        onNavigated?()
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
        return view
    }

    func updateUIView(_ uiView: WKWebView, context: Context) {}

    final class Coordinator: NSObject, UIGestureRecognizerDelegate {
        let model: WebModel
        weak var refresh: UIRefreshControl?
        init(_ model: WebModel) { self.model = model }

        @objc func reload() {
            model.reload()
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { self.refresh?.endRefreshing() }
        }

        @objc func swiped(_ g: UISwipeGestureRecognizer) {
            guard let view = g.view else { return }
            let x = g.location(in: view).x
            let w = view.bounds.width
            // The outer fifths are left for the web view's own back/forward
            // swipe; the centre is ours.
            guard x > w * 0.2, x < w * 0.8 else { return }
            if g.direction == .right { model.onSwipeAddress?() }
            else { model.onSwipeLibrary?() }
        }

        // Ride alongside the web view's own gestures rather than blocking them.
        func gestureRecognizer(_ g: UIGestureRecognizer,
                               shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool { true }
    }
}
