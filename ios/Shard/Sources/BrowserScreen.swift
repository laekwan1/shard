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

    @State private var landscape = false

    var body: some View {
        ZStack(alignment: .top) {
            Color.surface.ignoresSafeArea()
            WebViewContainer(model: model).ignoresSafeArea(edges: .bottom)

            // Centre bands, so the screen edges are left for the web view's own
            // back/forward swipe: a right-drag left-of-centre opens the address
            // panel, a left-drag right-of-centre opens the library.
            GeometryReader { geo in
                let w = geo.size.width
                Color.clear.frame(width: w * 0.2).position(x: w * 0.36, y: geo.size.height / 2)
                    .contentShape(Rectangle())
                    .gesture(bandGesture(openAddress: true))
                Color.clear.frame(width: w * 0.2).position(x: w * 0.64, y: geo.size.height / 2)
                    .contentShape(Rectangle())
                    .gesture(bandGesture(openAddress: false))
            }

            floatingDownload

            if showAddress {
                addressPanel.transition(.move(edge: .top))
            }
            if let banner = banner {
                self.banner(banner).frame(maxHeight: .infinity, alignment: .bottom)
            }
        }
        .onAppear {
            model.onLongPressVideo = { askAndDownload() }
            model.onNavigated = { withAnimation { showAddress = false } }
            if model.address.isEmpty { model.load("https://m.youtube.com") }
        }
        .confirmationDialog("받을 화질을 고르세요", isPresented: $showQualities, titleVisibility: .visible) {
            ForEach(qualities) { row in
                Button("\(row.label) — \(row.detail)") { startYouTube(row) }
            }
            Button("취소", role: .cancel) {}
        }
    }

    private func bandGesture(openAddress: Bool) -> some Gesture {
        DragGesture(minimumDistance: 24)
            .onEnded { v in
                guard abs(v.translation.width) > 45, abs(v.translation.height) < 70 else { return }
                if openAddress, v.translation.width > 0 {
                    withAnimation(.easeOut(duration: 0.18)) { showAddress = true }
                } else if !openAddress, v.translation.width < 0 {
                    openLibrary()
                }
            }
    }

    // A small download control floating over the page: reliable where a
    // long-press is not (YouTube claims the long-press for its own 2× and the
    // system claims it for copy/look-up). It saves whatever the page is playing.
    private var floatingDownload: some View {
        Button { askAndDownload() } label: {
            Image(systemName: asking ? "arrow.down.circle" : "arrow.down.to.line")
                .font(.title2).foregroundColor(.white)
                .frame(width: 46, height: 46)
                .background(Color.accent.opacity(0.9)).clipShape(Circle())
                .shadow(radius: 3)
                .overlay { if asking { ProgressView().tint(.white) } }
        }
        .disabled(asking)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomTrailing)
        .padding(.trailing, 16).padding(.bottom, 24)
    }

    private var addressPanel: some View {
        HStack(spacing: 10) {
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
