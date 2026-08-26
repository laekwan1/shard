import SwiftUI

/// A saved site. `url` is the identity — the same page bookmarked twice is one.
struct Bookmark: Codable, Identifiable, Equatable {
    let url: String
    var title: String
    var id: String { url }
}

/// Bookmarks the user pinned, and how often each host was visited — the two things
/// the start page's tiles are built from. Persisted in UserDefaults as JSON, small
/// enough that a plain read/write on every change is fine.
@MainActor
final class BookmarksStore: ObservableObject {
    @Published private(set) var bookmarks: [Bookmark] = []
    /// host → visit count, for the "자주 방문" tiles.
    @Published private(set) var visits: [String: Int] = [:]

    private let bookmarksKey = "shard.bookmarks"
    private let visitsKey = "shard.visits"

    init() { load() }

    func isBookmarked(_ url: String) -> Bool { bookmarks.contains { $0.url == url } }

    /// Add the page if new, remove it if already saved (the star toggles).
    func toggle(url: String, title: String) {
        guard !url.isEmpty, url != "about:blank" else { return }
        if let i = bookmarks.firstIndex(where: { $0.url == url }) {
            bookmarks.remove(at: i)
        } else {
            bookmarks.insert(Bookmark(url: url, title: title.isEmpty ? host(url) : title), at: 0)
        }
        save()
    }

    func remove(_ b: Bookmark) {
        bookmarks.removeAll { $0.url == b.url }
        save()
    }

    /// Count a page load toward its host's "자주 방문" tally.
    func recordVisit(_ url: String) {
        let h = host(url)
        guard !h.isEmpty else { return }
        visits[h, default: 0] += 1
        save()
    }

    /// The most-visited hosts, minus any already bookmarked (a bookmark tile already
    /// covers it), newest-weighted only by count.
    func frequent(limit: Int) -> [(host: String, url: String)] {
        let pinned = Set(bookmarks.map { host($0.url) })
        return visits
            .filter { $0.value >= 2 && !pinned.contains($0.key) }
            .sorted { $0.value > $1.value }
            .prefix(limit)
            .map { (host: $0.key, url: "https://\($0.key)") }
    }

    private func host(_ url: String) -> String {
        URL(string: url)?.host ?? url
    }

    private func save() {
        let d = UserDefaults.standard
        if let b = try? JSONEncoder().encode(bookmarks) { d.set(b, forKey: bookmarksKey) }
        if let v = try? JSONEncoder().encode(visits) { d.set(v, forKey: visitsKey) }
    }

    private func load() {
        let d = UserDefaults.standard
        if let b = d.data(forKey: bookmarksKey), let list = try? JSONDecoder().decode([Bookmark].self, from: b) {
            bookmarks = list
        }
        if let v = d.data(forKey: visitsKey), let map = try? JSONDecoder().decode([String: Int].self, from: v) {
            visits = map
        }
    }
}
