import SwiftUI
import MobileVLCKit

/// Thumbnails and durations for saved files, made by libVLC so they work for
/// every container the engine writes (mkv/webm included, which AVAsset cannot
/// thumbnail). Generated once per file and cached; anything that fails falls
/// back to no image and no badge, so the list is never blocked on it.
@MainActor
final class MediaProbe: NSObject, ObservableObject, VLCMediaThumbnailerDelegate {
    struct Result { var image: UIImage?; var duration: String? }

    @Published private var cache: [URL: Result] = [:]
    /// Thumbnailers in flight, kept alive and mapped back to their file.
    private var inFlight: [ObjectIdentifier: (url: URL, thumb: VLCMediaThumbnailer)] = [:]
    private var requested: Set<URL> = []

    /// The cached result, kicking off generation the first time it is asked for.
    func result(for url: URL) -> Result? {
        if let hit = cache[url] { return hit }
        if !requested.contains(url) {
            requested.insert(url)
            start(url)
        }
        return nil
    }

    private func start(_ url: URL) {
        let media = VLCMedia(url: url)
        guard let thumb = VLCMediaThumbnailer(media: media, andDelegate: self) else {
            cache[url] = Result(image: nil, duration: nil)
            return
        }
        thumb.snapshotPosition = 0.1
        inFlight[ObjectIdentifier(thumb)] = (url, thumb)
        thumb.fetchThumbnail()
    }

    private func finish(_ thumb: VLCMediaThumbnailer, image: UIImage?) {
        let key = ObjectIdentifier(thumb)
        guard let (url, _) = inFlight[key] else { return }
        inFlight[key] = nil
        var duration: String?
        if let length = thumb.media?.length.intValue, length > 0 {
            duration = Self.clock(length)
        }
        cache[url] = Result(image: image, duration: duration)
    }

    nonisolated func mediaThumbnailer(_ thumbnailer: VLCMediaThumbnailer, didFinishThumbnail thumbnail: CGImage) {
        let image = UIImage(cgImage: thumbnail)
        Task { @MainActor in self.finish(thumbnailer, image: image) }
    }

    nonisolated func mediaThumbnailerDidTimeOut(_ thumbnailer: VLCMediaThumbnailer) {
        Task { @MainActor in self.finish(thumbnailer, image: nil) }
    }

    private static func clock(_ ms: Int32) -> String {
        let total = Int(max(0, ms)) / 1000
        let s = total % 60, m = (total / 60) % 60, h = total / 3600
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, s) : String(format: "%d:%02d", m, s)
    }
}
