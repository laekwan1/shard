import SwiftUI

/// The browser: a thin top bar and the page. Downloads are handed to the shared
/// store and run in parallel; the library slides in from the right, or from a
/// swipe on the right edge.
struct BrowserScreen: View {
    @ObservedObject var downloads: DownloadsStore
    var openLibrary: () -> Void

    @StateObject private var model = WebModel()
    @State private var editing = ""

    @State private var showQualities = false
    @State private var qualities: [YtRow] = []
    @State private var pendingOffer = ""
    @State private var pendingTitle = ""
    @State private var banner: String?
    @State private var asking = false
    @State private var showAddress = false
    // The bypass toggle is hidden until asked for: two more right-swipes on the
    // open address panel reveal it.
    @State private var engineSwipes = 0
    @State private var engineRevealed = false

    @State private var landscape = false

    var body: some View {
        ZStack(alignment: .top) {
            Color.surface.ignoresSafeArea()
            // The page keeps clear of the home-indicator area at the bottom, so a
            // player's own seek bar sitting right at the screen edge (YouTube
            // Shorts) is reachable instead of under the indicator. The centre
            // swipes for address/library are recognizers on the web view itself
            // (see WebViewContainer) so they never swallow the page's touches.
            WebViewContainer(model: model)
                .overlay(alignment: .topTrailing) { topDownload }

            if showAddress {
                addressPanel.transition(.move(edge: .top))
            }
            if let banner = banner {
                self.banner(banner).frame(maxHeight: .infinity, alignment: .bottom)
            }
        }
        .onChange(of: showAddress) { shown in
            if !shown { engineSwipes = 0; engineRevealed = false }
        }
        .onAppear {
            model.onLongPressVideo = { askAndDownload() }
            model.onNavigated = { withAnimation { showAddress = false } }
            model.onSwipeAddress = { withAnimation(.easeOut(duration: 0.18)) { showAddress = true } }
            model.onSwipeLibrary = { openLibrary() }
            if model.address.isEmpty { model.load("https://m.youtube.com") }
        }
    }

    // A small control at the screen's top-right that drops its quality list
    // right underneath. A soft square, translucent grey, and half the size it
    // was. Hidden while a web video is full screen; the long-press and the
    // system menu are untouched.
    private var topDownload: some View {
        VStack(alignment: .trailing, spacing: 6) {
            if !model.videoFullscreen {
                Button { toggleDownload() } label: {
                    ZStack {
                        RoundedRectangle(cornerRadius: 7, style: .continuous)
                            .stroke(Color.white.opacity(0.65), lineWidth: 1.5)
                            .frame(width: 24, height: 24)
                        if asking { ProgressView().tint(.white).scaleEffect(0.6) }
                        else {
                            Image(systemName: "arrow.down")
                                .font(.system(size: 13, weight: .semibold))
                                .foregroundColor(.white.opacity(0.85))
                        }
                    }
                }
                .disabled(asking)

                if showQualities && !qualities.isEmpty {
                    VStack(spacing: 0) {
                        ForEach(qualities) { row in
                            Button { startYouTube(row); showQualities = false } label: {
                                HStack {
                                    Text(row.label).bold()
                                    Spacer()
                                    Text(row.detail).font(.caption).foregroundColor(.muted)
                                }
                                .padding(.horizontal, 12).padding(.vertical, 10)
                            }
                            .foregroundColor(.onSurface)
                            Divider().background(Color.toolbar)
                        }
                    }
                    .background(Color.chrome)
                    .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                    .frame(width: 250).shadow(radius: 6)
                }
            }
        }
        .padding(.top, 6).padding(.trailing, 10)
    }

    private func toggleDownload() {
        if showQualities { showQualities = false } else { askAndDownload() }
    }

    private var addressPanel: some View {
        HStack(spacing: 10) {
            if engineRevealed {
                // The bypass ON/OFF, lit amber when the engine is on.
                Button { model.toggleEngine() } label: {
                    Image(systemName: "power").foregroundColor(model.engineOn ? .accent : .muted)
                }
                .frame(width: 24)
            }
            iconButton("chevron.left", enabled: model.canGoBack) { model.goBack() }
            iconButton("chevron.right", enabled: model.canGoForward) { model.goForward() }

            URLField(text: $editing) { model.load(editing); withAnimation { showAddress = false } }
                .padding(.horizontal, 12).padding(.vertical, 7)
                .background(Color.chrome)
                .clipShape(Capsule())
                .onChange(of: model.address) { editing = $0 }

            iconButton(landscape ? "rotate.left" : "rotate.right") { rotate() }
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .background(Color.surface)
        // Two more right-swipes here reveal the bypass toggle.
        .gesture(
            DragGesture(minimumDistance: 24).onEnded { v in
                guard v.translation.width > 40, abs(v.translation.height) < 60 else { return }
                engineSwipes += 1
                if engineSwipes >= 2 { withAnimation { engineRevealed = true } }
            }
        )
    }

    private func rotate() {
        landscape.toggle()
        if landscape {
            Orientation.shared.lock(.landscapeRight, to: .landscapeRight)
        } else {
            Orientation.shared.lock(.portrait, to: .portrait)
        }
    }

    private func iconButton(_ name: String, enabled: Bool = true, _ tap: @escaping () -> Void) -> some View {
        Button(action: tap) {
            Image(systemName: name).foregroundColor(enabled ? .onSurface : .muted)
        }
        .disabled(!enabled)
        .frame(width: 26)
    }

    private func banner(_ text: String) -> some View {
        Text(text)
            .font(.callout).padding(12)
            .background(.ultraThinMaterial)
            .cornerRadius(10).padding(.bottom, 16)
            .onAppear { DispatchQueue.main.asyncAfter(deadline: .now() + 2.2) { banner = nil } }
    }

    private func askAndDownload() {
        asking = true
        Task {
            defer { asking = false }
            guard let json = await model.offer(),
                  let data = json.data(using: .utf8),
                  let offer = try? JSONDecoder().decode(Offer.self, from: data) else {
                banner = "감지된 미디어가 없습니다"
                return
            }
            let title = offer.title ?? model.pageTitle
            if offer.isYouTube {
                guard let rows = Downloader.youtubeQualities(json), !rows.isEmpty else {
                    banner = "화질을 찾지 못했습니다"
                    return
                }
                qualities = rows
                pendingOffer = json
                pendingTitle = title
                showQualities = true
            } else if let hls = offer.hls, !hls.isEmpty {
                startURL(hls, isHLS: true, referer: offer.referer ?? "", title: title)
            } else if let media = offer.media, !media.isEmpty {
                startURL(media, isHLS: false, referer: offer.referer ?? "", title: title)
            } else {
                banner = "감지된 미디어가 없습니다. 영상을 재생한 뒤 다시 눌러 보세요."
            }
        }
    }

    private func startYouTube(_ row: YtRow) {
        let offer = pendingOffer
        let label = row.isAudioOnly ? "\(pendingTitle) (음악)" : "\(pendingTitle) · \(row.label)"
        downloads.start(title: label) { task, report in
            try await Downloader.runYouTube(offer, itag: row.itag, task: task, progress: report)
        }
        banner = "다운로드를 시작했습니다"
    }

    private func startURL(_ url: String, isHLS: Bool, referer: String, title: String) {
        downloads.start(title: title) { task, report in
            try await Downloader.runURL(url, isHLS: isHLS, referer: referer, title: title,
                                        task: task, progress: report)
        }
        banner = "다운로드를 시작했습니다"
    }
}
