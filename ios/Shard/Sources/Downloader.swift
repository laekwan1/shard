import Foundation

/// One YouTube quality row, as the Rust core lists them.
struct YtRow: Identifiable, Decodable {
    let itag: UInt32
    let label: String
    let detail: String
    var id: UInt32 { itag }
    var isAudioOnly: Bool { itag == UInt32.max }
}

/// A running save. Holds the cancel flag the C ABI polls; a pointer to it is
/// handed across the boundary as `ctx`, and the C progress callback reaches its
/// sink through it too.
final class DownloadTask {
    var onProgress: (UInt64, UInt64) -> Void = { _, _ in }
    private let lock = NSLock()
    private var _cancelled = false
    var cancelled: Bool {
        lock.lock(); defer { lock.unlock() }
        return _cancelled
    }
    func cancel() {
        lock.lock(); _cancelled = true; lock.unlock()
    }
}

enum DownloadError: Error, LocalizedError {
    case failed(String)
    var errorDescription: String? { if case .failed(let m) = self { return m }; return nil }
}

/// Drives the Rust fetch-and-mux engine. All work happens off the main thread;
/// progress is delivered back on the main thread for the UI.
enum Downloader {
    /// Where saved files land: the app's Documents folder, which is what the
    /// Files app shows for this app.
    static var saveDirectory: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    }

    /// The quality rows for a captured YouTube offer, or nil if it is not a
    /// YouTube offer / cannot be read.
    static func youtubeQualities(_ offerJSON: String) -> [YtRow]? {
        guard let raw = offerJSON.withCString({ shard_youtube_qualities($0) }) else { return nil }
        let json = String(cString: raw)
        shard_string_free(raw)
        guard let data = json.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              (obj["ok"] as? Bool) == true,
              let rows = obj["rows"] as? [[String: Any]] else { return nil }
        return rows.compactMap { row in
            guard let itag = (row["itag"] as? NSNumber)?.uint32Value,
                  let label = row["label"] as? String,
                  let detail = row["detail"] as? String else { return nil }
            return YtRow(itag: itag, label: label, detail: detail)
        }
    }

    /// Download a YouTube format (or audio only) from a captured offer.
    static func runYouTube(
        _ offerJSON: String, itag: UInt32, task: DownloadTask,
        progress: @escaping (UInt64, UInt64) -> Void
    ) async throws -> URL {
        try await run(task: task, progress: progress) { progressCb, cancelCb, ctx in
            offerJSON.withCString { json in
                Downloader.saveDirectory.path.withCString { dir in
                    shard_download_youtube(json, itag, dir, progressCb, cancelCb, ctx)
                }
            }
        }
    }

    /// Download a plain URL — an HLS playlist or a progressive file.
    static func runURL(
        _ url: String, isHLS: Bool, referer: String, title: String,
        task: DownloadTask, progress: @escaping (UInt64, UInt64) -> Void
    ) async throws -> URL {
        try await run(task: task, progress: progress) { progressCb, cancelCb, ctx in
            url.withCString { u in
                referer.withCString { ref in
                    Downloader.saveDirectory.path.withCString { dir in
                        title.withCString { t in
                            isHLS
                                ? shard_download_hls(u, ref, dir, t, progressCb, cancelCb, ctx)
                                : shard_download_direct(u, ref, dir, t, progressCb, cancelCb, ctx)
                        }
                    }
                }
            }
        }
    }

    /// Shared runner: set up the callbacks, run `call` off the main thread, and
    /// turn its JSON result into a URL or an error.
    private static func run(
        task: DownloadTask,
        progress: @escaping (UInt64, UInt64) -> Void,
        call: @escaping (ShardProgress, ShardCancel, UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
    ) async throws -> URL {
        task.onProgress = { done, total in
            DispatchQueue.main.async { progress(done, total) }
        }
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let ctx = Unmanaged.passUnretained(task).toOpaque()
                let progressCb: ShardProgress = { ctx, done, total in
                    guard let ctx = ctx else { return }
                    Unmanaged<DownloadTask>.fromOpaque(ctx).takeUnretainedValue().onProgress(done, total)
                }
                let cancelCb: ShardCancel = { ctx in
                    guard let ctx = ctx else { return 0 }
                    return Unmanaged<DownloadTask>.fromOpaque(ctx).takeUnretainedValue().cancelled ? 1 : 0
                }
                guard let raw = call(progressCb, cancelCb, ctx) else {
                    continuation.resume(throwing: DownloadError.failed("다운로드에 실패했습니다"))
                    return
                }
                let json = String(cString: raw)
                shard_string_free(raw)
                switch parseResult(json) {
                case .success(let path): continuation.resume(returning: URL(fileURLWithPath: path))
                case .failure(let error): continuation.resume(throwing: error)
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
        return .failure(.failed((obj["error"] as? String) ?? "다운로드에 실패했습니다"))
    }
}

/// The minimal offer fields the app needs to decide how to download.
struct Offer: Decodable {
    let formats: [Fmt]?
    let media: String?
    let hls: String?
    let referer: String?
    let title: String?
    let thumb: String?
    struct Fmt: Decodable { let itag: Int? }

    var isYouTube: Bool { !(formats?.isEmpty ?? true) }
}
