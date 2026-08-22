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

    var body: some View {
        VStack(spacing: 0) {
            topBar
            ZStack(alignment: .bottom) {
                WebViewContainer(model: model)
                if let banner = banner { self.banner(banner) }
            }
        }
        .background(Color.surface)
        .overlay(alignment: .trailing) { rightEdgeSwipe }
        .onAppear {
            model.onLongPressVideo = { askAndDownload() }
            if model.address.isEmpty { model.load("https://m.youtube.com") }
        }
        .confirmationDialog("받을 화질을 고르세요", isPresented: $showQualities, titleVisibility: .visible) {
            ForEach(qualities) { row in
                Button("\(row.label) — \(row.detail)") { startYouTube(row) }
            }
            Button("취소", role: .cancel) {}
        }
    }

    private var topBar: some View {
        HStack(spacing: 10) {
            iconButton("chevron.left", enabled: model.canGoBack) { model.goBack() }
            iconButton("chevron.right", enabled: model.canGoForward) { model.goForward() }

            TextField("주소 또는 검색", text: $editing)
                .textFieldStyle(.plain)
                .autocapitalization(.none)
                .disableAutocorrection(true)
                .keyboardType(.webSearch)
                .foregroundColor(.onSurface)
                .padding(.horizontal, 12).padding(.vertical, 7)
                .background(Color.chrome)
                .clipShape(Capsule())
                .onSubmit { model.load(editing) }
                .onChange(of: model.address) { editing = $0 }

            iconButton(model.isLoading ? "xmark" : "arrow.clockwise") { model.reload() }
            if asking { ProgressView().tint(.accent).frame(width: 26) }
            iconButton("square.stack") { openLibrary() }
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .background(Color.surface)
    }

    private func iconButton(_ name: String, enabled: Bool = true, _ tap: @escaping () -> Void) -> some View {
        Button(action: tap) {
            Image(systemName: name).foregroundColor(enabled ? .onSurface : .muted)
        }
        .disabled(!enabled)
        .frame(width: 26)
    }

    /// A slim strip down the right edge: dragging left from it opens the library.
    private var rightEdgeSwipe: some View {
        Color.clear
            .frame(width: 24)
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 20)
                    .onEnded { value in
                        if value.translation.width < -40 && abs(value.translation.height) < 60 {
                            openLibrary()
                        }
                    }
            )
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
