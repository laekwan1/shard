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
        for candidate in candidates(urlString) {
            guard let url = URL(string: candidate) else { continue }
            if let data = try? await plainGet(url), data.count > 512 {
                save(key, data: data)
                return
            }
        }
    }

    /// The URLs to try, best quality first. For a YouTube thumbnail we can build
    /// clean, query-free ytimg URLs from the video id — the query-carrying URL the
    /// page hands out sometimes 404s on the maxres variant, so a clean
    /// `.../<id>/maxresdefault.jpg` is the reliable way to a 720p cover.
    private static func candidates(_ s: String) -> [String] {
        if let id = ytID(s) {
            return [
                "https://i.ytimg.com/vi/\(id)/maxresdefault.jpg",
                "https://i.ytimg.com/vi/\(id)/sddefault.jpg",
                "https://i.ytimg.com/vi/\(id)/hqdefault.jpg",
                s,
            ]
        }
        return [s]
    }

    /// The 11-char video id out of an `i.ytimg.com/vi/<id>/...` thumbnail URL.
    private static func ytID(_ s: String) -> String? {
        guard let range = s.range(of: "ytimg.com/vi/") else { return nil }
        let rest = s[range.upperBound...]
        let id = rest.prefix { $0 != "/" }
        return id.count >= 8 ? String(id) : nil
    }

    private static func plainGet(_ url: URL) async throws -> Data {
        let (data, response) = try await URLSession.shared.data(from: url)
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            throw URLError(.badServerResponse)
        }
        return data
    }
}
