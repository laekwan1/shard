import SwiftUI

/// One download's live state, shown in the downloads list.
struct Download: Identifiable {
    let id = UUID()
    let title: String
    var done: UInt64 = 0
    var total: UInt64 = 0
    var status: Status = .running
    let task: DownloadTask
    var savedName: String?
    /// Bytes per second, smoothed a little between samples.
    var rate: Double = 0
    var lastDone: UInt64 = 0
    var lastTime: Date = Date()
    /// HLS reports progress as segment COUNTS, not bytes — so the same numbers must
    /// not be run through ByteCountFormatter (that printed "5 bytes / 120 bytes").
    var countsSegments: Bool = false

    enum Status: Equatable { case running, finished, failed(String), cancelled }

    var fraction: Double { total > 0 ? Double(done) / Double(total) : 0 }

    /// "12.3 MB / 45.0 MB · 3.1 MB/s · 남은 10초", as far as it is known. For an HLS
    /// download the numbers are segment counts, shown as "12 / 120 조각".
    var detail: String {
        if countsSegments {
            var parts = [total > 0 ? "\(done) / \(total) 조각" : "\(done) 조각"]
            if rate > 0 && total > done {
                let secs = Int(Double(total - done) / rate)
                parts.append(secs >= 60 ? "남은 \(secs / 60)분" : "남은 \(secs)초")
            }
            return parts.joined(separator: " · ")
        }
        let f = ByteCountFormatter.string(fromByteCount: Int64(done), countStyle: .file)
        var parts = [total > 0 ? "\(f) / \(ByteCountFormatter.string(fromByteCount: Int64(total), countStyle: .file))" : f]
        if rate > 0 {
            parts.append(ByteCountFormatter.string(fromByteCount: Int64(rate), countStyle: .file) + "/s")
            if total > done {
                let secs = Int(Double(total - done) / rate)
                parts.append(secs >= 60 ? "남은 \(secs / 60)분" : "남은 \(secs)초")
            }
        }
        return parts.joined(separator: " · ")
    }
}

/// Every download at once — the engine runs them in parallel, so the UI stops
/// forcing one at a time. The store owns the list the downloads sheet renders
/// and the browser adds to.
@MainActor
final class DownloadsStore: ObservableObject {
    @Published var items: [Download] = []

    var activeCount: Int { items.filter { $0.status == .running }.count }

    /// Add a download and run it. `run` is the Downloader call for this job.
    func start(
        title: String,
        cover: String? = nil,
        segments: Bool = false,
        run: @escaping (DownloadTask, @escaping (UInt64, UInt64) -> Void) async throws -> URL
    ) {
        let task = DownloadTask()
        var download = Download(title: title, task: task)
        download.countsSegments = segments
        items.insert(download, at: 0)
        let id = download.id

        Task {
            do {
                let saved = try await run(task) { [weak self] done, total in
                    self?.update(id) { d in
                        // Rate from the gap since the last sample, at least a
                        // fifth of a second apart so a burst of callbacks does
                        // not read as an impossible speed.
                        let now = Date()
                        let dt = now.timeIntervalSince(d.lastTime)
                        if dt >= 0.2 {
                            let instant = Double(done - d.lastDone) / dt
                            d.rate = d.rate == 0 ? instant : d.rate * 0.6 + instant * 0.4
                            d.lastDone = done; d.lastTime = now
                        }
                        d.done = done; d.total = total
                    }
                }
                update(id) { $0.status = .finished; $0.savedName = saved.lastPathComponent }
                // Save the song's cover under the file's own name, so the library
                // tile can show it even though a music-only file carries no art.
                if let cover = cover, !cover.isEmpty {
                    await Covers.fetch(cover, key: Covers.keyFor(saved.lastPathComponent)) {
                        await Downloader.fetchViaEngine($0)
                    }
                }
            } catch {
                if task.cancelled {
                    update(id) { $0.status = .cancelled }
                } else {
                    update(id) { $0.status = .failed(error.localizedDescription) }
                }
            }
            // A finished download clears itself after a moment, so the progress
            // strip empties on its own rather than piling up completed rows.
            try? await Task.sleep(nanoseconds: 2_500_000_000)
            items.removeAll { $0.id == id && $0.status != .running }
        }
    }

    func cancel(_ id: UUID) {
        items.first(where: { $0.id == id })?.task.cancel()
    }

    /// Drop finished/failed/cancelled rows; running ones stay.
    func clearDone() {
        items.removeAll { $0.status != .running }
    }

    private func update(_ id: UUID, _ change: (inout Download) -> Void) {
        guard let i = items.firstIndex(where: { $0.id == id }) else { return }
        change(&items[i])
    }
}
