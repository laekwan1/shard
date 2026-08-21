import SwiftUI

/// The browser tab: address bar, the page, and a download control that asks the
/// page what it has the moment it is pressed.
struct BrowserView: View {
    @StateObject private var model = WebModel()
    @State private var editing = ""

    // The YouTube quality picker.
    @State private var showQualities = false
    @State private var qualities: [YtRow] = []
    @State private var pendingOffer = ""

    // Download progress for the active save.
    @State private var task: DownloadTask?
    @State private var progressText = ""
    @State private var fraction: Double = 0
    @State private var banner: String?
    @State private var asking = false

    var body: some View {
        VStack(spacing: 0) {
            addressBar
            ZStack(alignment: .bottom) {
                WebViewContainer(model: model)
                if let banner = banner { self.banner(banner) }
                if task != nil { progressBar }
            }
        }
        .onAppear { if model.address.isEmpty { model.load("https://www.youtube.com") } }
        .confirmationDialog("받을 화질을 고르세요", isPresented: $showQualities, titleVisibility: .visible) {
            ForEach(qualities) { row in
                Button("\(row.label) — \(row.detail)") { startYouTube(row.itag) }
            }
            Button("취소", role: .cancel) {}
        }
    }

    private var addressBar: some View {
        HStack(spacing: 12) {
            Button { model.goBack() } label: { Image(systemName: "chevron.left") }
                .disabled(!model.canGoBack)
            Button { model.goForward() } label: { Image(systemName: "chevron.right") }
                .disabled(!model.canGoForward)

            TextField("주소 또는 검색", text: $editing)
                .textFieldStyle(.roundedBorder)
                .autocapitalization(.none)
                .disableAutocorrection(true)
                .keyboardType(.webSearch)
                .onSubmit { model.load(editing) }
                .onChange(of: model.address) { editing = $0 }

            if model.isLoading {
                Button { model.reload() } label: { Image(systemName: "xmark") }
            } else {
                Button { model.reload() } label: { Image(systemName: "arrow.clockwise") }
            }

            // Always tappable: pressing it asks the page what it has right now.
            Button { askAndDownload() } label: {
                if asking {
                    ProgressView()
                } else {
                    Image(systemName: "arrow.down.circle")
                }
            }
            .disabled(task != nil || asking)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }

    private var progressBar: some View {
        VStack(spacing: 6) {
            HStack {
                Text(progressText).font(.caption).lineLimit(1)
                Spacer()
                Button("취소") { task?.cancel() }.font(.caption)
            }
            ProgressView(value: fraction)
        }
        .padding(12)
        .background(.ultraThinMaterial)
    }

    private func banner(_ text: String) -> some View {
        Text(text)
            .font(.callout).padding(12)
            .background(.ultraThinMaterial)
            .cornerRadius(10).padding(.bottom, 16)
            .onAppear {
                DispatchQueue.main.asyncAfter(deadline: .now() + 2.5) { banner = nil }
            }
    }

    /// Ask the page for an offer, then act on it: a YouTube offer opens the
    /// quality picker; a plain media/HLS offer starts straight away.
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
            if offer.isYouTube {
                guard let rows = Downloader.youtubeQualities(json), !rows.isEmpty else {
                    banner = "화질을 찾지 못했습니다"
                    return
                }
                qualities = rows
                pendingOffer = json
                showQualities = true
            } else if let hls = offer.hls, !hls.isEmpty {
                startURL(hls, isHLS: true, referer: offer.referer ?? "", title: offer.title ?? "video")
            } else if let media = offer.media, !media.isEmpty {
                startURL(media, isHLS: false, referer: offer.referer ?? "", title: offer.title ?? "video")
            } else {
                banner = "감지된 미디어가 없습니다. 영상을 재생한 뒤 다시 눌러 보세요."
            }
        }
    }

    private func startYouTube(_ itag: UInt32) {
        let offer = pendingOffer
        beginTask { task, report in
            try await Downloader.runYouTube(offer, itag: itag, task: task, progress: report)
        }
    }

    private func startURL(_ url: String, isHLS: Bool, referer: String, title: String) {
        beginTask { task, report in
            try await Downloader.runURL(url, isHLS: isHLS, referer: referer, title: title,
                                        task: task, progress: report)
        }
    }

    /// Shared progress plumbing for any download.
    private func beginTask(_ run: @escaping (DownloadTask, @escaping (UInt64, UInt64) -> Void) async throws -> URL) {
        let task = DownloadTask()
        self.task = task
        progressText = "준비 중…"
        fraction = 0
        Task {
            do {
                let saved = try await run(task) { done, total in
                    if total > 0 {
                        fraction = Double(done) / Double(total)
                        progressText = "\(byteText(done)) / \(byteText(total))"
                    } else {
                        progressText = byteText(done)
                    }
                }
                self.task = nil
                banner = "저장됨: \(saved.lastPathComponent)"
            } catch {
                self.task = nil
                banner = error.localizedDescription
            }
        }
    }

    private func byteText(_ b: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(b), countStyle: .file)
    }
}
