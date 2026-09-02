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
    @State private var renaming: Bookmark?
    @State private var renameText = ""
    @State private var editingHomepage = false
    @State private var homepageText = ""
    @State private var editingTiles = false
    @State private var jiggle = false
    @State private var editing = ""
    /// The user's homepage: where the home button goes, and the first page opened.
    @AppStorage("shard.homepage") private var homepage = "https://m.youtube.com"

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
    // 재서명(자체 서명) 시트 — 우회 토글이 뜰 때 함께 나오는 모래시계로 연다.
    @State private var showResign = false
    // 현재 서명의 남은 유효일수 — 모래시계에 표시한다(≤3일이면 앰버).
    @State private var signDaysLeft: Int? = nil

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
            VStack(spacing: 0) {
                addressPanel
                // Autocomplete: matches from bookmarks + visited hosts as you type.
                if showAddress, !editing.isEmpty, editing != model.address {
                    let matches = addressSuggestions
                    if !matches.isEmpty {
                        VStack(spacing: 0) {
                            ForEach(matches, id: \.url) { m in
                                Button { model.load(m.url); showAddress = false; showStart = false } label: {
                                    HStack(spacing: 10) {
                                        Image(systemName: m.bookmarked ? "star.fill" : "clock")
                                            .font(.system(size: 12)).foregroundColor(.muted).frame(width: 16)
                                        VStack(alignment: .leading, spacing: 1) {
                                            Text(m.title).font(.subheadline).foregroundColor(.onSurface).lineLimit(1)
                                            Text(m.url).font(.caption2).foregroundColor(.muted).lineLimit(1)
                                        }
                                        Spacer()
                                    }
                                    .padding(.horizontal, 16).padding(.vertical, 9).contentShape(Rectangle())
                                }
                                .buttonStyle(.plain)
                                Divider().background(Color.toolbar).padding(.leading, 42)
                            }
                        }
                        .background(Color.surface)
                    }
                }
            }
            .offset(y: (showAddress || showStart) ? 0 : -400)
            .opacity((showAddress || showStart) ? 1 : 0)
            .allowsHitTesting(showAddress || showStart)
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

            if editingHomepage { homepageEditor.transition(.opacity) }

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
            bookmarks.recordVisit(url, title: model.pageTitle)
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
            if model.address.isEmpty { model.load(homepage) }
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
    // 모래시계 색: 서명 만료 임박(≤3일, ㉮ 자동 갱신 임계와 동일)이면 앰버로 경고.
    private var hourglassColor: Color {
        if let d = signDaysLeft, d <= 3 { return .accent }
        return .onSurface
    }

    // 남은 서명일수를 '모래 양'으로: 무료 개발 인증서 유효기간(7일) 대비 비율(0~1).
    // 이 비율만큼 모래시계 안을 아래에서 채워 모래가 줄어드는 것처럼 보이게 한다.
    private var sandFraction: CGFloat {
        guard let d = signDaysLeft else { return 0 }
        return min(max(CGFloat(d) / 7.0, 0), 1)
    }

    private var addressPanel: some View {
        HStack(spacing: 10) {
            if engineRevealed {
                // The bypass ON/OFF, lit amber when the engine is on — boxed and set
                // off by a divider so it reads as separate from the home/star row.
                Image(systemName: "power")
                    .foregroundColor(model.engineOn ? .accent : .muted)
                    .frame(width: 32, height: 30)
                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(model.engineOn ? Color.accent : Color.toolbar, lineWidth: 1))
                    .contentShape(Rectangle())
                    .onTapGesture { model.toggleEngine() }
                Divider().frame(height: 22).background(Color.toolbar)
            }
            // Home: tap → the set homepage; hold → make the current page the homepage.
            tapHoldIcon(showStart ? "house.fill" : "house", color: .onSurface, tap: {
                withAnimation { showStart = false }
                model.load(homepage)
                showAddress = false
            }, hold: {
                homepageText = homepage      // prefill the field with the current home
                editingHomepage = true
            })
            // Star: tap → the favorites (bookmarks) page; hold → add the current page.
            // Filled white when the current page is already bookmarked.
            tapHoldIcon(bookmarks.isBookmarked(model.address) ? "star.fill" : "star",
                        color: showStart ? .accent : (bookmarks.isBookmarked(model.address) ? .white : .onSurface),
                        tap: { withAnimation { showStart.toggle() } },
                        hold: {
                            bookmarks.toggle(url: model.address, title: model.pageTitle)
                            banner = bookmarks.isBookmarked(model.address) ? "즐겨찾기에 추가했습니다" : "즐겨찾기에서 뺐습니다"
                        })

            URLField(text: $editing) { model.load(editing); showAddress = false; showStart = false }
                .frame(height: 22)   // a UITextField wrapper has no height of its
                                     // own, so without this it stretched the panel
                                     // to the whole screen.
                .padding(.horizontal, 12).padding(.vertical, 8)
                .background(Color.chrome)
                .clipShape(Capsule())
                .onChange(of: model.address) { editing = $0 }

            iconButton(landscape ? "rotate.left" : "rotate.right") { rotate() }

            // 자체 서명(재서명) — 주소창 맨 오른쪽. 전원 버튼과 대칭으로 구분선을 왼쪽에 둔다
            // (전원은 왼쪽 끝+오른쪽 구분선, 모래시계는 오른쪽 끝+왼쪽 구분선).
            // 남은 유효일수를 '모래시계 안 모래 양'으로 표현한다(숫자 없음): 흐린 윤곽 위에
            // 잔량 비율(무료 7일 대비)만큼 아래에서 채운 색 모래를 겹친다. ≤3일이면 앰버. 탭=재서명 시트.
            if engineRevealed {
                Divider().frame(height: 22).background(Color.toolbar)
                // 실제 모래시계처럼: 남은 일수가 '윗칸 모래'(7일=위 가득/아래 빔), 지난 만큼 아래로 쌓인다.
                // SF Symbol은 위/아래 반칸 두 단계뿐이라 양 조절이 안 돼(사용자 지적) 직접 그린다.
                HourglassSand(fraction: sandFraction, sand: hourglassColor, frameColor: .muted)
                    .frame(width: 32, height: 30)               // 전원 버튼과 같은 상자
                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(hourglassColor == .accent ? Color.accent : Color.toolbar, lineWidth: 1))
                    .contentShape(Rectangle())
                    .onTapGesture { showResign = true }
            }
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .background(Color.surface)
        // Right-swipe reveals the bypass toggle; left-swipe closes the panel.
        .gesture(
            DragGesture(minimumDistance: 24).onEnded { v in
                guard abs(v.translation.width) > 40, abs(v.translation.height) < 60 else { return }
                if v.translation.width > 0 { withAnimation { engineRevealed = true } }
                // Don't swipe the bar away while the favorites page is up — the home/
                // star buttons on it are the only way back off that page.
                else if !showStart { showAddress = false }
            }
        )
        .sheet(isPresented: $showResign) { ResignView() }
        .onAppear { if signDaysLeft == nil { signDaysLeft = SigningInfo.daysLeft() } }
    }

    /// A small dialog for the homepage URL. Uses URLField so it grabs focus and
    /// selects the whole (prefilled) address, ready to be typed over at once.
    private var homepageEditor: some View {
        ZStack {
            Color.black.opacity(0.45).ignoresSafeArea().onTapGesture { editingHomepage = false }
            VStack(alignment: .leading, spacing: 14) {
                Text("홈페이지").font(.headline).foregroundColor(.onSurface)
                Text("홈 버튼을 누르면 이동할 주소").font(.caption).foregroundColor(.muted)
                URLField(text: $homepageText, autofocus: true) { saveHomepage() }
                    .frame(height: 22).padding(.horizontal, 12).padding(.vertical, 10)
                    .background(Color.chrome).clipShape(Capsule())
                HStack {
                    Button("취소") { editingHomepage = false }.foregroundColor(.muted)
                    Spacer()
                    Button("저장") { saveHomepage() }.foregroundColor(.accent).font(.body.weight(.semibold))
                }
            }
            .padding(20).background(Color.surface).clipShape(RoundedRectangle(cornerRadius: 16))
            .padding(28)
        }
    }

    private func saveHomepage() {
        let v = homepageText.trimmingCharacters(in: .whitespaces)
        if !v.isEmpty { homepage = WebModel.normalize(v) }
        editingHomepage = false
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

    /// An icon that acts on tap AND on a long press — used for home (go / set-home)
    /// and star (open favorites / add-current). A plain Button can't carry both, so
    /// this drives the gestures on a bare image.
    private func tapHoldIcon(_ name: String, color: Color, tap: @escaping () -> Void, hold: @escaping () -> Void) -> some View {
        Image(systemName: name)
            .foregroundColor(color)
            .frame(width: 28, height: 30)
            .contentShape(Rectangle())
            .onTapGesture { tap() }
            .onLongPressGesture(minimumDuration: 0.4) { hold() }
    }

    private func banner(_ text: String) -> some View {
        Text(text)
            .font(.callout).padding(12)
            .background(.ultraThinMaterial)
            .cornerRadius(10).padding(.bottom, 16)
            .onAppear { DispatchQueue.main.asyncAfter(deadline: .now() + 2.2) { banner = nil } }
    }

    struct Suggestion { let title: String; let url: String; let bookmarked: Bool }

    /// Address-bar autocomplete: bookmarks first, then visited hosts, matched against
    /// what has been typed (host or title contains it). Deduplicated by URL, few shown.
    private var addressSuggestions: [Suggestion] {
        let q = editing.lowercased().trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return [] }
        var out: [Suggestion] = []
        var seen = Set<String>()
        for b in bookmarks.bookmarks where b.url.lowercased().contains(q) || b.title.lowercased().contains(q) {
            if seen.insert(b.url).inserted { out.append(Suggestion(title: b.title, url: b.url, bookmarked: true)) }
        }
        for (host, _) in bookmarks.visits.sorted(by: { $0.value > $1.value }) where host.lowercased().contains(q) {
            let url = "https://\(host)"
            if seen.insert(url).inserted { out.append(Suggestion(title: host, url: url, bookmarked: false)) }
        }
        return Array(out.prefix(6))
    }

    // MARK: start page (home)

    /// The home page over the web: a single scrolling ROW of favorite tiles up top,
    /// and the visit history below as the only VERTICALLY scrolling area.
    private var startPage: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                section("즐겨찾기")
                Spacer()
                if editingTiles {
                    Button("완료") { withAnimation { editingTiles = false } }
                        .font(.subheadline).foregroundColor(.accent)
                }
            }
            // Clear the address bar that floats over the top, so the header shows and
            // the tiles' delete badge (which sits above each tile) is not clipped.
            .padding(.horizontal, 16).padding(.top, 54).padding(.bottom, 2)

            if bookmarks.bookmarks.isEmpty {
                Text("주소창의 ⭐를 눌러 즐겨찾기를 추가하세요.")
                    .font(.caption).foregroundColor(.muted).padding(.horizontal, 16).padding(.bottom, 8)
            } else {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 14) {
                        ForEach(bookmarks.bookmarks) { b in bookmarkTile(b) }
                    }
                    .padding(.horizontal, 16).padding(.top, 4).padding(.bottom, 12)
                }
            }

            let freq = bookmarks.frequent(limit: 20)
            if !freq.isEmpty {
                section("자주 방문")
                    .padding(.horizontal, 16).padding(.top, 8).padding(.bottom, 6)
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 14) {
                        ForEach(freq, id: \.host) { f in
                            frequentTile(host: f.host, url: f.url)
                        }
                    }
                    .padding(.horizontal, 16).padding(.bottom, 12)
                }
            }

            Divider().background(Color.toolbar)

            HStack {
                section("방문기록")
                Spacer()
                if !bookmarks.history.isEmpty {
                    Button("전체 지우기") { bookmarks.clearHistory() }
                        .font(.subheadline).foregroundColor(.muted)
                }
            }
            .padding(.horizontal, 16).padding(.top, 12).padding(.bottom, 2)

            if bookmarks.history.isEmpty {
                Text("방문기록이 없습니다.").font(.callout).foregroundColor(.muted)
                    .frame(maxWidth: .infinity, alignment: .center).padding(.top, 40)
                Spacer()
            } else {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(bookmarks.history) { h in historyRow(h) }
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.surface.ignoresSafeArea())
        // A tap on empty page space leaves edit mode (the home-screen way).
        .contentShape(Rectangle())
        .onTapGesture { if editingTiles { withAnimation { editingTiles = false } } }
        .onChange(of: editingTiles) { on in
            if on { withAnimation(.easeInOut(duration: 0.13).repeatForever(autoreverses: true)) { jiggle = true } }
            else { jiggle = false }
        }
        .onChange(of: bookmarks.bookmarks.count) { c in if c == 0 { editingTiles = false } }
        .onChange(of: showStart) { shown in if !shown { editingTiles = false } }
        .alert("이름 바꾸기", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("이름", text: $renameText)
            Button("확정") { if let b = renaming { bookmarks.rename(b, to: renameText) }; renaming = nil }
            Button("취소", role: .cancel) { renaming = nil }
        }
    }

    private func section(_ title: String) -> some View {
        Text(title).font(.headline).foregroundColor(.onSurface)
    }

    /// A small favorite tile: tap opens (or renames in edit mode), long-press starts
    /// edit mode (jiggle + a delete badge), like a home-screen icon.
    private func bookmarkTile(_ b: Bookmark) -> some View {
        VStack(spacing: 5) {
            favicon(URL(string: b.url)?.host ?? b.url)
                .frame(width: 54, height: 54)
                .clipShape(RoundedRectangle(cornerRadius: 13, style: .continuous))
            Text(b.title).font(.caption2).foregroundColor(.onSurface)
                .lineLimit(1).truncationMode(.tail).frame(width: 66)
        }
        .frame(width: 68)
        .contentShape(Rectangle())
        .onTapGesture {
            if editingTiles { renaming = b; renameText = b.title }
            else { model.load(b.url); showStart = false; showAddress = false }
        }
        .rotationEffect(.degrees(editingTiles ? (jiggle ? 2 : -2) : 0))
        .overlay(alignment: .topLeading) {
            if editingTiles {
                Button { bookmarks.remove(b) } label: {
                    Image(systemName: "minus.circle.fill")
                        .font(.system(size: 20)).symbolRenderingMode(.palette)
                        .foregroundStyle(.white, .black.opacity(0.6))
                }
                .offset(x: -4, y: -4)
            }
        }
        .onLongPressGesture { withAnimation { editingTiles = true } }
    }

    /// A most-visited tile — same look and edit behaviour as a favorite. Long-press
    /// jiggles; the delete badge removes it from "자주 방문" (a hidden host, so it does
    /// not come back on the next visit).
    private func frequentTile(host: String, url: String) -> some View {
        VStack(spacing: 5) {
            favicon(host)
                .frame(width: 54, height: 54)
                .clipShape(RoundedRectangle(cornerRadius: 13, style: .continuous))
            Text(host.replacingOccurrences(of: "www.", with: "")).font(.caption2).foregroundColor(.onSurface)
                .lineLimit(1).truncationMode(.tail).frame(width: 66)
        }
        .frame(width: 68)
        .contentShape(Rectangle())
        .onTapGesture {
            if editingTiles { return }   // in edit mode a tap must not open the site
            model.load(url); showStart = false; showAddress = false
        }
        .rotationEffect(.degrees(editingTiles ? (jiggle ? 2 : -2) : 0))
        .overlay(alignment: .topLeading) {
            if editingTiles {
                Button { withAnimation { bookmarks.hideFrequent(host) } } label: {
                    Image(systemName: "minus.circle.fill")
                        .font(.system(size: 20)).symbolRenderingMode(.palette)
                        .foregroundStyle(.white, .black.opacity(0.6))
                }
                .offset(x: -4, y: -4)
            }
        }
        .onLongPressGesture { withAnimation { editingTiles = true } }
    }

    /// One history row: opens on tap; the trailing ✕ removes just that entry.
    private func historyRow(_ h: Bookmark) -> some View {
        HStack(spacing: 8) {
            Button { model.load(h.url); showStart = false; showAddress = false } label: {
                HStack(spacing: 10) {
                    favicon(URL(string: h.url)?.host ?? h.url)
                        .frame(width: 26, height: 26).clipShape(RoundedRectangle(cornerRadius: 6))
                    VStack(alignment: .leading, spacing: 1) {
                        Text(h.title).font(.subheadline).foregroundColor(.onSurface).lineLimit(1)
                        Text(h.url).font(.caption2).foregroundColor(.muted).lineLimit(1)
                    }
                    Spacer()
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            Button { bookmarks.removeHistory(h) } label: {
                Image(systemName: "xmark").font(.system(size: 12, weight: .semibold))
                    .foregroundColor(.muted).padding(8)
            }
        }
        .padding(.horizontal, 16).padding(.vertical, 5)
        .overlay(Divider().background(Color.toolbar), alignment: .bottom)
    }

    /// The site's own icon, from Google's favicon service (reachable without the
    /// bypass), falling back to a colored initial while it loads or if it fails.
    @ViewBuilder private func favicon(_ host: String) -> some View {
        let letter = String(host.replacingOccurrences(of: "www.", with: "").prefix(1)).uppercased()
        let fallback = Text(letter.isEmpty ? "?" : letter)
            .font(.system(size: 30, weight: .semibold)).foregroundColor(.white)
            .frame(maxWidth: .infinity, maxHeight: .infinity).background(tileColor(host))
        if let u = URL(string: "https://www.google.com/s2/favicons?domain=\(host)&sz=64") {
            AsyncImage(url: u) { phase in
                if let img = phase.image {
                    img.resizable().scaledToFit().padding(14)
                        .frame(maxWidth: .infinity, maxHeight: .infinity).background(Color.chrome)
                } else {
                    fallback
                }
            }
        } else {
            fallback
        }
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

// 남은 서명일수를 **원래 모래시계 아이콘 모양 그대로** 두고 그 안의 모래 양만 조절해 표현한다.
// SF Symbol의 반쪽 채움 변형(tophalf/bottomhalf.filled)을 잔량만큼만 마스크로 드러낸다 —
// 윗칸 모래가 남은 비율(7일=위 가득/아래 빔), 시간이 지나면 아래로 쌓인다. ≤3일 앰버(sand 색).
// (직접 그린 삼각형은 투박하다는 지적으로 폐기.) fraction 0…1 = 남은 비율.
struct HourglassSand: View {
    var fraction: CGFloat
    var sand: Color
    var frameColor: Color

    var body: some View {
        let f = min(max(fraction, 0), 1)
        return ZStack {
            // 1) 원래 아이콘(윤곽) — 흐리게, 항상 전체 모양.
            symbol("hourglass").foregroundColor(frameColor)
            // 2) 윗칸 남은 모래: 위 반쪽 채운 아이콘을, 표면(위에서 (1-f)/2 지점) 아래로만 드러냄.
            symbol("hourglass.tophalf.filled").foregroundColor(sand)
                .mask(revealBelow((1 - f) / 2))
            // 3) 아랫칸 쌓인 모래: 아래 반쪽 채운 아이콘을, 표면(위에서 (1+f)/2 지점) 아래로만 드러냄.
            symbol("hourglass.bottomhalf.filled").foregroundColor(sand)
                .mask(revealBelow((1 + f) / 2))
        }
    }

    private func symbol(_ name: String) -> some View {
        Image(systemName: name).font(.system(size: 18, weight: .regular))
    }

    // 위에서 `top` 비율만큼 비우고(투명) 그 아래를 드러내는(불투명) 마스크. 심볼은 반쪽만 그려져
    // 있어 반대편은 어차피 비므로, 표면 아래를 통째로 드러내도 해당 반칸만 채워진다.
    private func revealBelow(_ top: CGFloat) -> some View {
        GeometryReader { geo in
            VStack(spacing: 0) {
                Spacer(minLength: 0).frame(height: geo.size.height * top)
                Rectangle()
            }
        }
    }
}
