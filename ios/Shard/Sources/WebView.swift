import SwiftUI
import WebKit

/// The page. Owns the WKWebView so navigation state lives in one observable
/// place the UI binds to. Media is not tracked continuously any more — the app
/// asks the page for an offer (Ask.js) at the moment the user wants to download.
final class WebModel: NSObject, ObservableObject, WKNavigationDelegate, WKScriptMessageHandler, WKUIDelegate {
    @Published var address: String = ""
    @Published var pageTitle: String = ""
    @Published var canGoBack = false
    @Published var canGoForward = false
    @Published var isLoading = false
    /// Called when a video is long-pressed, so the browser can offer to download.
    var onLongPressVideo: (() -> Void)?

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
        view.uiDelegate = self
        view.allowsBackForwardNavigationGestures = true
        // No system link preview / callout menu — long-press is ours, for
        // downloading, not iOS's open-link/copy/share sheet.
        view.allowsLinkPreview = false
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
        guard message.name == "shard",
              let body = message.body as? [String: Any],
              body["type"] as? String == "longpress" else { return }
        onLongPressVideo?()
    }

    // MARK: WKUIDelegate — suppress the long-press link menu

    func webView(_ webView: WKWebView,
                 contextMenuConfigurationForElement elementInfo: WKContextMenuElementInfo,
                 completionHandler: @escaping (UIContextMenuConfiguration?) -> Void) {
        completionHandler(nil)
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
