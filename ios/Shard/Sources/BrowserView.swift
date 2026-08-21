import SwiftUI

/// The browser tab: address bar, the page, and a download control that lights
/// up once the page reveals something saveable.
struct BrowserView: View {
    @StateObject private var model = WebModel()
    @State private var editing = ""
    @State private var showCandidates = false

    // Download progress for the active save.
    @State private var task: DownloadTask?
    @State private var progressText = ""
    @State private var fraction: Double = 0
    @State private var banner: String?

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
        .confirmationDialog("받을 화질을 고르세요", isPresented: $showCandidates, titleVisibility: .visible) {
            ForEach(model.candidates) { candidate in
                Button(candidateLabel(candidate)) { start(candidate) }
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

            // The save button: enabled only when the page revealed media.
            Button {
                if model.candidates.count == 1 { start(model.candidates[0]) }
                else { showCandidates = true }
            } label: {
                Image(systemName: "arrow.down.circle\(model.candidates.isEmpty ? "" : ".fill")")
            }
            .disabled(model.candidates.isEmpty || task != nil)
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

    private func candidateLabel(_ c: MediaCandidate) -> String {
        (c.isHLS ? "스트리밍(HLS) — " : "파일 — ") + c.title
    }

    private func start(_ candidate: MediaCandidate) {
        let task = DownloadTask(candidate)
        self.task = task
        progressText = "준비 중…"
        fraction = 0
        Task {
            do {
                let saved = try await Downloader.run(task) { done, total in
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
