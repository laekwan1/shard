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

    enum Status: Equatable { case running, finished, failed(String), cancelled }

    var fraction: Double { total > 0 ? Double(done) / Double(total) : 0 }
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
        run: @escaping (DownloadTask, @escaping (UInt64, UInt64) -> Void) async throws -> URL
    ) {
        let task = DownloadTask()
        let download = Download(title: title, task: task)
        items.insert(download, at: 0)
        let id = download.id

        Task {
            do {
                let saved = try await run(task) { [weak self] done, total in
                    self?.update(id) { $0.done = done; $0.total = total }
                }
                update(id) { $0.status = .finished; $0.savedName = saved.lastPathComponent }
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
