import SwiftUI
import WebKit

/// The page and everything captured from it. Owns the WKWebView so navigation
/// state and the media candidates live in one observable place the UI binds to.
final class WebModel: NSObject, ObservableObject, WKScriptMessageHandler, WKNavigationDelegate {
    @Published var address: String = ""
    @Published var pageTitle: String = ""
    @Published var canGoBack = false
    @Published var canGoForward = false
    @Published var isLoading = false
    /// Distinct media URLs the current page has revealed. Newest first.
    @Published var candidates: [MediaCandidate] = []

    lazy var webView: WKWebView = {
        let controller = WKUserContentController()
        if let js = Self.captureScript() {
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

    private static func captureScript() -> String? {
        guard let url = Bundle.main.url(forResource: "Capture", withExtension: "js"),
              let text = try? String(contentsOf: url, encoding: .utf8) else { return nil }
        return text
    }

    // MARK: navigation

    func load(_ text: String) {
        let target = Self.normalize(text)
        guard let url = URL(string: target) else { return }
        candidates = []
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

    // MARK: captured media

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        guard message.name == "shard",
              let body = message.body as? [String: Any],
              body["type"] as? String == "media",
              let url = body["url"] as? String,
              let kind = body["kind"] as? String else { return }
        let candidate = MediaCandidate(
            url: url,
            isHLS: kind == "hls",
            referer: webView.url?.absoluteString ?? "",
            title: (body["title"] as? String) ?? pageTitle
        )
        if !candidates.contains(where: { $0.url == candidate.url }) {
            // HLS first, then files — the playlist is what carries every quality.
            candidates.insert(candidate, at: 0)
            candidates.sort { ($0.isHLS ? 0 : 1) < ($1.isHLS ? 0 : 1) }
        }
    }

    // MARK: WKNavigationDelegate

    func webView(_ webView: WKWebView, didStartProvisionalNavigation navigation: WKNavigation!) {
        isLoading = true
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

/// Hosts the model's WKWebView inside SwiftUI.
struct WebViewContainer: UIViewRepresentable {
    let model: WebModel
    func makeUIView(context: Context) -> WKWebView { model.webView }
    func updateUIView(_ uiView: WKWebView, context: Context) {}
}
