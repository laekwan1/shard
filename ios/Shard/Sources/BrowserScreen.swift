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

    var body: some View {
        ZStack(alignment: .top) {
            Color.surface.ignoresSafeArea()
            WebViewContainer(model: model).ignoresSafeArea(edges: .bottom)

            // Edge zones: swipe in from the left for the address panel, from the
            // right for the library — the phone app's two swipes.
            HStack {
                edge(open: true)
                Spacer()
                edge(open: false)
            }

            if showAddress {
                addressPanel.transition(.move(edge: .top))
            }
            if asking {
                ProgressView().tint(.accent).padding(8)
                    .background(.ultraThinMaterial).clipShape(Circle())
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top).padding(.top, 6)
            }
            if let banner = banner {
                self.banner(banner).frame(maxHeight: .infinity, alignment: .bottom)
            }
        }
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

    private func edge(open address: Bool) -> some View {
        Color.clear
            .frame(width: 22)
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 20)
                    .onEnded { v in
                        guard abs(v.translation.width) > 40, abs(v.translation.height) < 70 else { return }
                        if address, v.translation.width > 0 {
                            withAnimation(.easeOut(duration: 0.18)) { showAddress = true }
                        } else if !address, v.translation.width < 0 {
                            openLibrary()
                        }
                    }
            )
    }

    private var addressPanel: some View {
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
                .onSubmit { model.load(editing); withAnimation { showAddress = false } }
                .onChange(of: model.address) { editing = $0 }

            iconButton(model.isLoading ? "xmark" : "arrow.clockwise") { model.reload() }
            iconButton("square.stack") { openLibrary() }
            iconButton("chevron.up") { withAnimation { showAddress = false } }
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
