import UIKit

/// Cover art for songs, kept in the app's own cache — a mirror of the Android
/// `Covers` object. A music-only download is a bare .m4a with no picture, so the
/// video's thumbnail is fetched at download time and remembered here, keyed by
/// the saved file's name. The library reads it back for the tile when the file
/// itself carries no embedded art. It is a cache, not a store: the worst a purge
/// costs is a blank tile.
enum Covers {
    private static var dir: URL {
        let base = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
        let d = base.appendingPathComponent("covers", isDirectory: true)
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    /// The key a song's cover is filed under: its name without the extension.
    static func keyFor(_ name: String) -> String { (name as NSString).deletingPathExtension }

    /// A stable hash so any title — Korean, emoji, punctuation — makes a safe
    /// filename. Swift's `hashValue` is randomized per run and would not survive a
    /// relaunch, so this rolls its own (djb2 over the UTF-8 bytes).
    private static func stableHash(_ s: String) -> String {
        var h: UInt64 = 5381
        for b in s.utf8 { h = (h &* 33) &+ UInt64(b) }
        return String(h, radix: 16)
    }

    private static func file(_ key: String) -> URL {
        dir.appendingPathComponent(stableHash(key) + ".jpg")
    }

    /// Store a fetched cover, downscaled: a tile never needs more than a few
    /// hundred pixels, and this caps what a huge source thumbnail would take.
    static func save(_ key: String, data: Data) {
        guard let full = UIImage(data: data) else { return }
        let side: CGFloat = 320
        let scale = min(1, side / max(full.size.width, full.size.height))
        let image: UIImage
        if scale < 1 {
            let size = CGSize(width: full.size.width * scale, height: full.size.height * scale)
            let renderer = UIGraphicsImageRenderer(size: size)
            image = renderer.image { _ in full.draw(in: CGRect(origin: .zero, size: size)) }
        } else {
            image = full
        }
        if let jpeg = image.jpegData(compressionQuality: 0.85) {
            try? jpeg.write(to: file(key))
        }
    }

    static func load(_ key: String) -> UIImage? {
        let f = file(key)
        guard FileManager.default.fileExists(atPath: f.path) else { return nil }
        return UIImage(contentsOfFile: f.path)
    }

    /// Fetch a thumbnail URL and file it under `key`. A plain request, not the
    /// download proxy: the thumbnail lives on a public host. YouTube's own thumb
    /// URLs are upgraded to maxresdefault (1280×720) so the tile is sharp rather
    /// than the small default the page hands out.
    static func fetch(_ urlString: String, key: String) async {
        let upgraded = upgradeToMaxRes(urlString)
        guard let url = URL(string: upgraded) else { return }
        if let data = try? await plainGet(url), !data.isEmpty {
            save(key, data: data)
            return
        }
        // maxresdefault does not exist for every video; fall back to what the
        // page actually offered.
        if upgraded != urlString, let url = URL(string: urlString),
           let data = try? await plainGet(url), !data.isEmpty {
            save(key, data: data)
        }
    }

    private static func plainGet(_ url: URL) async throws -> Data {
        let (data, response) = try await URLSession.shared.data(from: url)
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            throw URLError(.badServerResponse)
        }
        return data
    }

    /// `.../vi/<id>/hqdefault.jpg` → `.../vi/<id>/maxresdefault.jpg`, so the cover
    /// is at least 720p as asked. Only rewrites i.ytimg.com thumbnails; anything
    /// else is returned unchanged.
    private static func upgradeToMaxRes(_ s: String) -> String {
        guard s.contains("ytimg.com/vi/") else { return s }
        for name in ["hqdefault", "sddefault", "mqdefault", "default", "hq720"] {
            if s.contains("/\(name).jpg") {
                return s.replacingOccurrences(of: "/\(name).jpg", with: "/maxresdefault.jpg")
            }
        }
        return s
    }
}
