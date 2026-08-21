import Foundation

/// A media URL a page is playing, as captured by the injected script.
struct MediaCandidate: Identifiable, Equatable {
    let id = UUID()
    let url: String
    let isHLS: Bool
    let referer: String
    let title: String
}

/// One running save. Holds the progress sink and the cancel flag the C ABI
/// polls; a pointer to it is handed across the boundary as `ctx`.
final class DownloadTask {
    let candidate: MediaCandidate
    var onProgress: (UInt64, UInt64) -> Void = { _, _ in }
    private let cancelledFlag = NSLock.init()
    private var _cancelled = false

    init(_ candidate: MediaCandidate) { self.candidate = candidate }

    var cancelled: Bool {
        cancelledFlag.lock(); defer { cancelledFlag.unlock() }
        return _cancelled
    }
    func cancel() {
        cancelledFlag.lock(); _cancelled = true; cancelledFlag.unlock()
    }
}

enum DownloadError: Error, LocalizedError {
    case failed(String)
    var errorDescription: String? { if case .failed(let m) = self { return m }; return nil }
}

/// Drives the Rust fetch-and-mux engine. All work happens off the main thread;
/// progress is delivered back on the main thread for the UI.
enum Downloader {
    /// Where saved files land: the app's Documents folder, which is the folder
    /// the Files app shows for this app (see Info.plist LSSupportsOpeningDocumentsInPlace).
    static var saveDirectory: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    }

    /// Run a download. Returns the saved file path, or throws with a Korean
    /// message. `progress` is called on the main thread.
    static func run(
        _ task: DownloadTask,
        progress: @escaping (UInt64, UInt64) -> Void
    ) async throws -> URL {
        task.onProgress = { done, total in
            DispatchQueue.main.async { progress(done, total) }
        }
        let candidate = task.candidate
        let dir = saveDirectory.path

        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let ctx = Unmanaged.passUnretained(task).toOpaque()
                // Non-capturing C callbacks: they recover the task from ctx.
                let progressCb: ShardProgress = { ctx, done, total in
                    guard let ctx = ctx else { return }
                    Unmanaged<DownloadTask>.fromOpaque(ctx)
                        .takeUnretainedValue().onProgress(done, total)
                }
                let cancelCb: ShardCancel = { ctx in
                    guard let ctx = ctx else { return 0 }
                    return Unmanaged<DownloadTask>.fromOpaque(ctx)
                        .takeUnretainedValue().cancelled ? 1 : 0
                }

                let raw: UnsafeMutablePointer<CChar>? = candidate.url.withCString { url in
                    candidate.referer.withCString { referer in
                        dir.withCString { into in
                            candidate.title.withCString { title in
                                if candidate.isHLS {
                                    return shard_download_hls(url, referer, into, title,
                                                              progressCb, cancelCb, ctx)
                                } else {
                                    return shard_download_direct(url, referer, into, title,
                                                                 progressCb, cancelCb, ctx)
                                }
                            }
                        }
                    }
                }

                guard let raw = raw else {
                    continuation.resume(throwing: DownloadError.failed("다운로드에 실패했습니다"))
                    return
                }
                let json = String(cString: raw)
                shard_string_free(raw)

                switch parseResult(json) {
                case .success(let path):
                    continuation.resume(returning: URL(fileURLWithPath: path))
                case .failure(let error):
                    continuation.resume(throwing: error)
                }
            }
        }
    }

    /// Parse `{"ok":true,"path":"..."}` / `{"ok":false,"error":"..."}`.
    private static func parseResult(_ json: String) -> Result<String, DownloadError> {
        guard let data = json.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return .failure(.failed("결과를 읽지 못했습니다"))
        }
        if let ok = obj["ok"] as? Bool, ok, let path = obj["path"] as? String {
            return .success(path)
        }
        let message = (obj["error"] as? String) ?? "다운로드에 실패했습니다"
        return .failure(.failed(message))
    }
}
