import SwiftUI

/// The browser: a thin top bar and the page. Downloads are handed to the shared
/// store and run in parallel; the library slides in from the right, or from a
/// swipe on the right edge.
struct BrowserScreen: View {
    @ObservedObject var downloads: DownloadsStore
    var onWebPlaying: (Bool) -> Void = { _ in }
    /// True while the library is shown over the browser — then page videos are
    /// paused and blocked from entering full screen (see WebModel.setBrowserActive).
    var libraryVisible: Bool = false
    var openLibrary: () -> Void

    @StateObject private var model = WebModel()
    @StateObject private var bookmarks = BookmarksStore()
    @State private var showStart = false
    @State private var editing = ""

    @State private var qualities: [YtRow] = []
    @State private var pendingOffer = ""
    @State private var pendingTitle = ""
    @State private var pendingThumb = ""
    @State private var pendingHLSReferer = ""
    @State private var pendingHLSCookie = ""
    @State private var pendingHLSUA = ""
    @State private var pendingHLSHeaders = ""
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
                // Replaced when the engine toggles (the web view is rebuilt on a
                // fresh session); the id change makes SwiftUI swap in the new view.
                .id(model.generation)

            // The start page (home): bookmarks + most-visited tiles, over the web.
            if showStart {
                startPage
                    .transition(.opacity)
            }

            // Always mounted and slid by an offset — a conditional `if` with a
            // .transition popped away on close no matter how the flag was animated.
            // An offset is a plain layout move, so open and close are the same
            // slide. allowsHitTesting keeps the hidden panel from eating top taps.
            addressPanel
                .offset(y: showAddress ? 0 : -200)
                .opacity(showAddress ? 1 : 0)
                .allowsHitTesting(showAddress)
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

            // A thin load bar at the very top: shows a page is still connecting, and
            // vanishes when it is done — so a blocked site's white screen is not a
            // mystery (still trying vs. finished/failed).
            if model.isLoading && model.progress < 1 {
                GeometryReader { g in
                    Rectangle().fill(Color.accent)
                        .frame(width: g.size.width * CGFloat(max(0.05, model.progress)), height: 2.5)
                        .animation(.easeOut(duration: 0.2), value: model.progress)
                }
                .frame(height: 2.5)
                .frame(maxHeight: .infinity, alignment: .top)
                .ignoresSafeArea(edges: .top)
            }
        }
        .animation(.easeInOut(duration: 0.34), value: showAddress)
        .onChange(of: showAddress) { shown in
            if !shown {
                engineRevealed = false
                // Closing the panel drops any half-typed, not-entered text and shows
                // the real current URL again. (Doing this on load-start instead broke a
                // NEW navigation: it overwrote the address the user just entered with the
                // previous page's URL before the new one committed/failed.)
                editing = model.address
            }
        }
        .onChange(of: model.address) { url in
            bookmarks.recordVisit(url)
            if !url.isEmpty && url != "about:blank" { showStart = false }
        }
        .onChange(of: libraryVisible) { visible in
            // Behind the library: pause page videos and block them from grabbing
            // full screen when the library forces landscape.
            model.setBrowserActive(!visible)
            if visible { model.pauseWebVideos() }
        }
        // Web video full-screen rotation is left to iOS. Forcing landscape here
        // kept corrupting the window geometry on exit (page and library came back
        // cropped/zoomed and stuck), and no restore sequence reliably undid it — so
        // it is off. The user can use the address-bar rotate button for a web video.
        .onAppear {
            // No per-call withAnimation: the panel's slide is driven by the
            // .animation(value: showAddress) on the ZStack, so open and close use
            // the exact same curve — close was popping when a caller changed the
            // flag without wrapping it.
            model.onNavigated = { showAddress = false }
            model.onSwipeAddress = { showAddress = true }
            model.onSwipeLibrary = {
                // If the address panel is open, the first left-swipe just puts it
                // away; the next one opens the library.
                if showAddress { showAddress = false }
                else { model.pauseWebVideos(); openLibrary() }
            }
            model.onWebTap = { showAddress = false }
            model.onWebPlaying = { onWebPlaying($0) }
            model.installAdBlock()
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
            // Prefer the page's thumbnail; if it was empty, build a clean one from
            // the video id so a cover is still fetched.
            if let t = offer.thumb, !t.isEmpty {
                pendingThumb = t
            } else if let id = offer.videoId, !id.isEmpty {
                pendingThumb = "https://i.ytimg.com/vi/\(id)/maxresdefault.jpg"
            } else {
                pendingThumb = ""
            }
            if offer.isYouTube {
                guard let rows = Downloader.youtubeQualities(json), !rows.isEmpty else {
                    banner = "화질을 찾지 못했습니다"; return
                }
                banner = nil
                qualities = rows
                withAnimation { showList = true }
            } else if let hlsURL = hlsURL(offer) {
                // (No reload/re-capture: pornhub's signed URL is valid for ~2 hours,
                // so it was never an expiry problem — the reload just reset the page
                // and annoyed. The real gap vs Android was a missing User-Agent, now
                // passed below.)
                // Offer the same quality list YouTube gets. If the master has
                // variants, show them; otherwise (a plain media playlist, or the
                // fetch failed) fall back to downloading it directly as before —
                // so this never blocks a download that used to work.
                pendingHLSReferer = offer.referer ?? ""
                pendingHLSCookie = await model.cookieHeader(for: hlsURL)
                pendingHLSUA = await model.userAgent()
                pendingHLSHeaders = headerString(offer.headers)
                // ALWAYS show a list — the user picks, even when there is only one
                // quality (never auto-save). If the manifest has no variants (a plain
                // media playlist, or it could not be read), offer the stream itself
                // as a single "다운로드" row so it is still a deliberate choice.
                var rows = await Downloader.hlsQualities(hlsURL, referer: pendingHLSReferer, cookie: pendingHLSCookie, ua: pendingHLSUA, extra: pendingHLSHeaders) ?? []
                if rows.isEmpty {
                    rows = [YtRow(itag: 0, label: "다운로드", detail: "", url: hlsURL)]
                }
                banner = nil
                qualities = rows
                withAnimation { showList = true }
            } else if let media = offer.media, !media.isEmpty {
                // A direct (progressive) file — an ad, a plain <video src>. Show it in
                // the list too (a single row) so it is a deliberate pick, not a silent
                // auto-download, the same as every other source.
                pendingHLSReferer = offer.referer ?? ""
                pendingHLSCookie = await model.cookieHeader(for: media)
                pendingHLSUA = await model.userAgent()
                pendingHLSHeaders = headerString(offer.headers)
                banner = nil
                qualities = [YtRow(itag: 0, label: "다운로드", detail: "", url: media, isHLS: false)]
                withAnimation { showList = true }
            } else {
                banner = "감지된 미디어가 없습니다. 영상을 재생한 뒤 다시 눌러 보세요."
            }
        }
    }

    /// The HLS manifest to use, if any. Prefer the dedicated `hls` field, but also
    /// accept an `.m3u8` that a site put in `media` — pornhub hands its master
    /// playlist there, which the old code fed to the progressive downloader and so
    /// saved a broken file instead of offering the quality list.
    private func hlsURL(_ offer: Offer) -> String? {
        if let hls = offer.hls, !hls.isEmpty { return hls }
        if let media = offer.media, media.contains(".m3u8") { return media }
        return nil
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
            iconButton(showStart ? "house.fill" : "house") { withAnimation { showStart.toggle() } }

            URLField(text: $editing) { model.load(editing); showAddress = false; showStart = false }
                .frame(height: 22)   // a UITextField wrapper has no height of its
                                     // own, so without this it stretched the panel
                                     // to the whole screen.
                .padding(.horizontal, 12).padding(.vertical, 8)
                .background(Color.chrome)
                .clipShape(Capsule())
                .onChange(of: model.address) { editing = $0 }

            // Star: bookmark / un-bookmark the current page.
            Button { bookmarks.toggle(url: model.address, title: model.pageTitle) } label: {
                Image(systemName: bookmarks.isBookmarked(model.address) ? "star.fill" : "star")
                    .foregroundColor(bookmarks.isBookmarked(model.address) ? .accent : .onSurface)
            }
            .frame(width: 26)

            iconButton(landscape ? "rotate.left" : "rotate.right") { rotate() }
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .background(Color.surface)
        // Right-swipe reveals the bypass toggle; left-swipe closes the panel.
        .gesture(
            DragGesture(minimumDistance: 24).onEnded { v in
                guard abs(v.translation.width) > 40, abs(v.translation.height) < 60 else { return }
                if v.translation.width > 0 { withAnimation { engineRevealed = true } }
                else { showAddress = false }
            }
        )
    }

    private func rotate() {
        // Decide from the ACTUAL current orientation, not a stored flag — the flag
        // fell out of sync after the library forced portrait, so the first press
        // did nothing and it took two to turn.
        let isLandscape = UIScreen.main.bounds.width > UIScreen.main.bounds.height
        if isLandscape {
            landscape = false
            Orientation.shared.lock(.portrait, to: .portrait)
        } else {
            landscape = true
            Orientation.shared.lock(.landscapeRight, to: .landscapeRight)
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

    // MARK: start page (home)

    /// A full page of tiles over the web: bookmarks first, then most-visited. Tapping
    /// one opens it; long-pressing a bookmark offers to remove it.
    private var startPage: some View {
        let cols = [GridItem(.adaptive(minimum: 96, maximum: 140), spacing: 14)]
        return ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                if !bookmarks.bookmarks.isEmpty {
                    section("북마크")
                    LazyVGrid(columns: cols, spacing: 14) {
                        ForEach(bookmarks.bookmarks) { b in
                            tile(title: b.title, url: b.url)
                                .contextMenu {
                                    Button(role: .destructive) { bookmarks.remove(b) } label: {
                                        Label("삭제", systemImage: "trash")
                                    }
                                }
                        }
                    }
                }
                let freq = bookmarks.frequent(limit: 12)
                if !freq.isEmpty {
                    section("자주 방문")
                    LazyVGrid(columns: cols, spacing: 14) {
                        ForEach(freq, id: \.host) { f in tile(title: f.host, url: f.url) }
                    }
                }
                if bookmarks.bookmarks.isEmpty && freq.isEmpty {
                    Text("별을 눌러 북마크를 추가하면 여기에 모입니다.")
                        .font(.callout).foregroundColor(.muted)
                        .frame(maxWidth: .infinity, alignment: .center).padding(.top, 80)
                }
            }
            .padding(20)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.surface.ignoresSafeArea())
    }

    private func section(_ title: String) -> some View {
        Text(title).font(.headline).foregroundColor(.onSurface)
    }

    private func tile(title: String, url: String) -> some View {
        let host = URL(string: url)?.host ?? url
        let letter = String(host.replacingOccurrences(of: "www.", with: "").prefix(1)).uppercased()
        return Button {
            model.load(url); showStart = false; showAddress = false
        } label: {
            VStack(spacing: 6) {
                Text(letter.isEmpty ? "?" : letter)
                    .font(.system(size: 30, weight: .semibold)).foregroundColor(.white)
                    .frame(width: 64, height: 64)
                    .background(tileColor(host)).clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
                Text(title).font(.caption).foregroundColor(.onSurface)
                    .lineLimit(1).truncationMode(.tail)
            }
            .frame(maxWidth: .infinity)
        }
        .buttonStyle(.plain)
    }

    /// A stable color per host, from a simple hash — so a site keeps its tile color.
    private func tileColor(_ host: String) -> Color {
        let hues: [Double] = [0.02, 0.09, 0.13, 0.33, 0.53, 0.58, 0.75, 0.83, 0.92]
        let h = abs(host.hashValue) % hues.count
        return Color(hue: hues[h], saturation: 0.55, brightness: 0.72)
    }

    private let listWidth: CGFloat = 264

    private var qualityList: some View {
        // Scrollable + height-capped: with a codec per resolution the list can be
        // taller than the screen, and the bottom rows were unreachable.
        ScrollView {
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
        }
        .frame(width: listWidth)
        .frame(maxHeight: UIScreen.main.bounds.height * 0.6)
        .background(Color.chrome)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14).stroke(Color.toolbar, lineWidth: 0.5))
        .shadow(color: .black.opacity(0.4), radius: 12, y: 4)
    }

    private func startYouTube(_ row: YtRow) {
        // An HLS variant row carries a URL instead of an itag: download that
        // rendition directly, the same list UI, a different source.
        if let variant = row.url {
            withAnimation { showList = false }
            let label = row.isHLS ? "\(pendingTitle) · \(row.label)" : pendingTitle
            startURL(variant, isHLS: row.isHLS, referer: pendingHLSReferer, cookie: pendingHLSCookie,
                     ua: pendingHLSUA, extra: pendingHLSHeaders, title: label)
            return
        }
        withAnimation { showList = false }
        let offer = pendingOffer
        let label = row.isAudioOnly ? "\(pendingTitle) (음악)" : "\(pendingTitle) · \(row.label)"
        // Save the YouTube thumbnail for a video too, not just music: VLC frequently
        // times out thumbnailing an AV1 frame, so the tile needs a reliable cover to
        // fall back to instead of a blank film icon.
        let cover = pendingThumb
        downloads.start(title: label, cover: cover) { task, report in
            try await Downloader.runYouTube(offer, itag: row.itag, task: task, progress: report)
        }
        banner = "다운로드를 시작했습니다"
    }

    /// The captured player headers as "Name: Value" lines for the download engine.
    private func headerString(_ headers: [String: String]?) -> String {
        headers?.map { "\($0.key): \($0.value)" }.joined(separator: "\n") ?? ""
    }

    private func startURL(_ url: String, isHLS: Bool, referer: String, cookie: String = "", ua: String = "", extra: String = "", title: String) {
        // HLS now reports bytes (run_hls estimates the total), so it shows MB and a
        // real speed just like the others — no segment-count mode needed.
        downloads.start(title: title) { task, report in
            try await Downloader.runURL(url, isHLS: isHLS, referer: referer, cookie: cookie, ua: ua,
                                        extra: extra, title: title, task: task, progress: report)
        }
        banner = "다운로드를 시작했습니다"
    }
}
