import SwiftUI

/// The browser: a thin top bar and the page. Downloads are handed to the shared
/// store and run in parallel; the library slides in from the right, or from a
/// swipe on the right edge.
struct BrowserScreen: View {
    @ObservedObject var downloads: DownloadsStore
    var openLibrary: () -> Void

    @StateObject private var model = WebModel()
    @State private var editing = ""

    @State private var qualities: [YtRow] = []
    @State private var pendingOffer = ""
    @State private var pendingTitle = ""
    @State private var pendingThumb = ""
    @State private var banner: String?
    @State private var showAddress = false
    @State private var showList = false
    @State private var listAnchor: CGPoint = .zero
    // The bypass toggle is hidden until asked for: a right-swipe on the open
    // address panel reveals it.
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

            if showAddress {
                // Animate on the value, not only on each caller's withAnimation,
                // so the panel slides back up on close exactly as it slid down on
                // open — some close paths were tearing it off with no animation.
                addressPanel.transition(.move(edge: .top))
            }
            if showList {
                Color.black.opacity(0.001).ignoresSafeArea()
                    .onTapGesture { withAnimation { showList = false } }
                qualityList
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                    .padding(.leading, max(8, min(listAnchor.x - listWidth, UIScreen.main.bounds.width - listWidth - 8)))
                    .padding(.top, listAnchor.y + 4)
            }
            if let banner = banner {
                self.banner(banner).frame(maxHeight: .infinity, alignment: .bottom)
            }
        }
        .animation(.easeInOut(duration: 0.22), value: showAddress)
        .onChange(of: showAddress) { shown in
            if !shown { engineRevealed = false }
        }
        .onAppear {
            model.onNavigated = { withAnimation { showAddress = false } }
            model.onSwipeAddress = { withAnimation(.easeOut(duration: 0.18)) { showAddress = true } }
            model.onSwipeLibrary = {
                // If the address panel is open, the first left-swipe just puts it
                // away; the next one opens the library.
                if showAddress { withAnimation(.easeOut(duration: 0.18)) { showAddress = false } }
                else { model.pauseWebVideos(); openLibrary() }
            }
            model.onWebTap = { withAnimation(.easeOut(duration: 0.15)) { showAddress = false } }
            model.onDownloadRequest = { right, bottom in
                listAnchor = CGPoint(x: right, y: bottom)
                requestDownload()
            }
            model.onPick = { itag in pick(itag) }
            if model.address.isEmpty { model.load("https://m.youtube.com") }
        }
    }

    /// A video's download button was pressed: ask the page, then either drop the
    /// quality list into the page (YouTube) or start a plain download.
    private func requestDownload() {
        // Immediate, synchronous feedback so it is clear the button reached the
        // app even before the page is asked.
        banner = "다운로드 확인 중…"
        Task { @MainActor in
            guard let json = await model.offer(),
                  let data = json.data(using: .utf8),
                  let offer = try? JSONDecoder().decode(Offer.self, from: data) else {
                banner = "감지된 미디어가 없습니다"; return
            }
            pendingOffer = json
            pendingTitle = offer.title ?? model.pageTitle
            pendingThumb = offer.thumb ?? ""
            if offer.isYouTube {
                guard let rows = Downloader.youtubeQualities(json), !rows.isEmpty else {
                    banner = "화질을 찾지 못했습니다"; return
                }
                banner = nil
                qualities = rows
                withAnimation { showList = true }
            } else if let hls = offer.hls, !hls.isEmpty {
                startURL(hls, isHLS: true, referer: offer.referer ?? "", title: pendingTitle)
            } else if let media = offer.media, !media.isEmpty {
                startURL(media, isHLS: false, referer: offer.referer ?? "", title: pendingTitle)
            } else {
                banner = "감지된 미디어가 없습니다. 영상을 재생한 뒤 다시 눌러 보세요."
            }
        }
    }

    private func pick(_ itag: UInt32) {
        guard let row = qualities.first(where: { $0.itag == itag }) else { return }
        startYouTube(row)
    }

    // A small control at the screen's top-right that drops its quality list
    // right underneath. A soft square, translucent grey, and half the size it
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
                .frame(height: 22)   // a UITextField wrapper has no height of its
                                     // own, so without this it stretched the panel
                                     // to the whole screen.
                .padding(.horizontal, 12).padding(.vertical, 8)
                .background(Color.chrome)
                .clipShape(Capsule())
                .onChange(of: model.address) { editing = $0 }

            iconButton(landscape ? "rotate.left" : "rotate.right") { rotate() }
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .background(Color.surface)
        // Right-swipe reveals the bypass toggle; left-swipe closes the panel.
        .gesture(
            DragGesture(minimumDistance: 24).onEnded { v in
                guard abs(v.translation.width) > 40, abs(v.translation.height) < 60 else { return }
                if v.translation.width > 0 { withAnimation { engineRevealed = true } }
                else { withAnimation(.easeOut(duration: 0.18)) { showAddress = false } }
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

    private let listWidth: CGFloat = 264

    private var qualityList: some View {
        VStack(spacing: 0) {
            ForEach(Array(qualities.enumerated()), id: \.element.id) { i, row in
                Button { startYouTube(row) } label: {
                    HStack(spacing: 10) {
                        Image(systemName: row.isAudioOnly ? "music.note" : "film")
                            .font(.system(size: 13)).foregroundColor(.accent).frame(width: 18)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(row.label).font(.subheadline.weight(.semibold))
                            Text(row.detail).font(.caption2).foregroundColor(.muted)
                        }
                        Spacer()
                    }
                    .padding(.horizontal, 14).padding(.vertical, 13)
                    .contentShape(Rectangle())
                }
                .foregroundColor(.onSurface)
                if i < qualities.count - 1 { Divider().background(Color.toolbar).padding(.leading, 42) }
            }
        }
        .frame(width: listWidth)
        .background(Color.chrome)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14).stroke(Color.toolbar, lineWidth: 0.5))
        .shadow(color: .black.opacity(0.4), radius: 12, y: 4)
    }

    private func startYouTube(_ row: YtRow) {
        withAnimation { showList = false }
        let offer = pendingOffer
        let label = row.isAudioOnly ? "\(pendingTitle) (음악)" : "\(pendingTitle) · \(row.label)"
        // Only music tiles need a stand-in cover; a video makes its own thumbnail.
        let cover = row.isAudioOnly ? pendingThumb : nil
        downloads.start(title: label, cover: cover) { task, report in
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
