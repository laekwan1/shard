import SwiftUI
import MobileVLCKit

enum MediaKind: String { case video, music }

/// A saved file, with what the list needs to draw it.
struct Item: Identifiable, Hashable {
    let url: URL
    var id: URL { url }
    var name: String { url.deletingPathExtension().lastPathComponent }
    var ext: String { url.pathExtension.lowercased() }
    /// The folder it sits in, or nil for the top level.
    let folder: String?
    var size: Int64 {
        (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize).map(Int64.init) ?? 0
    }

    static let musicExts: Set<String> = ["m4a", "mp3", "weba", "opus", "aac", "oga", "wav"]
    var kind: MediaKind { Item.musicExts.contains(ext) ? .music : .video }
}

/// The saved files, grouped into folders, plus the operations the library needs.
/// Files live under Documents: the top level and one directory per folder, so
/// "folders" are real directories the Files app shows too.
@MainActor
final class LibraryStore: ObservableObject {
    @Published var items: [Item] = []
    /// The folder being viewed, or nil for the top level.
    @Published var current: String?
    /// Which shelf is shown — video or music.
    @Published var kind: MediaKind = .video

    /// Folders shown for the current shelf: video and music keep separate lists,
    /// so a folder made on one does not appear on the other. A folder is listed
    /// if it holds a file of this kind or was created for this kind.
    var folders: [String] {
        let withFiles = Set(items.filter { $0.kind == kind && $0.folder != nil }.compactMap { $0.folder })
        return Array(withFiles.union(remembered(kind))).sorted()
    }

    private func remembered(_ kind: MediaKind) -> Set<String> {
        Set(UserDefaults.standard.stringArray(forKey: "folders_\(kind.rawValue)") ?? [])
    }
    private func remember(_ name: String, _ kind: MediaKind, add: Bool) {
        var set = remembered(kind)
        if add { set.insert(name) } else { set.remove(name) }
        UserDefaults.standard.set(Array(set), forKey: "folders_\(kind.rawValue)")
    }

    private var root: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    }

    private static let mediaExts: Set<String> =
        ["mp4", "m4v", "mov", "webm", "mkv", "m4a", "mp3", "opus", "aac", "wav", "weba", "oga"]

    func reload() {
        let fm = FileManager.default
        let keys: [URLResourceKey] = [.isDirectoryKey, .contentModificationDateKey, .creationDateKey]
        var found: [Item] = []

        let top = (try? fm.contentsOfDirectory(at: root, includingPropertiesForKeys: keys,
                                               options: [.skipsHiddenFiles])) ?? []
        for url in top {
            let isDir = (try? url.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) ?? false
            if isDir {
                let inside = (try? fm.contentsOfDirectory(at: url, includingPropertiesForKeys: keys,
                                                          options: [.skipsHiddenFiles])) ?? []
                for f in inside where Self.mediaExts.contains(f.pathExtension.lowercased()) {
                    found.append(Item(url: f, folder: url.lastPathComponent))
                }
            } else if Self.mediaExts.contains(url.pathExtension.lowercased()) {
                found.append(Item(url: url, folder: nil))
            }
        }
        // Newest first: what was just downloaded should sit at the top.
        items = found.sorted { Self.modified($0.url) > Self.modified($1.url) }
    }

    private static func modified(_ url: URL) -> Date {
        let v = try? url.resourceValues(forKeys: [.contentModificationDateKey, .creationDateKey])
        return v?.contentModificationDate ?? v?.creationDate ?? .distantPast
    }

    /// The items shown for the current folder and shelf — music gets folders too.
    var visible: [Item] { items.filter { $0.folder == current && $0.kind == kind } }

    func createFolder(_ name: String) {
        let clean = name.trimmingCharacters(in: .whitespaces)
        guard !clean.isEmpty else { return }
        try? FileManager.default.createDirectory(at: root.appendingPathComponent(clean),
                                                 withIntermediateDirectories: true)
        remember(clean, kind, add: true)   // this shelf's folder, not the other's
        reload()
    }

    /// Move a file into a folder (nil = back to the top level).
    func move(_ item: Item, to folder: String?) {
        let dir = folder.map { root.appendingPathComponent($0) } ?? root
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let target = dir.appendingPathComponent(item.url.lastPathComponent)
        try? FileManager.default.moveItem(at: item.url, to: target)
        reload()
    }

    func rename(_ item: Item, to newName: String) {
        let clean = newName.trimmingCharacters(in: .whitespaces)
        guard !clean.isEmpty else { return }
        let target = item.url.deletingLastPathComponent()
            .appendingPathComponent(clean)
            .appendingPathExtension(item.ext)
        try? FileManager.default.moveItem(at: item.url, to: target)
        reload()
    }

    func delete(_ item: Item) {
        try? FileManager.default.removeItem(at: item.url)
        reload()
    }

    func renameFolder(_ from: String, to: String) {
        let clean = to.trimmingCharacters(in: .whitespaces)
        guard !clean.isEmpty, clean != from else { return }
        let src = root.appendingPathComponent(from)
        let dst = root.appendingPathComponent(clean)
        try? FileManager.default.moveItem(at: src, to: dst)
        remember(from, kind, add: false); remember(clean, kind, add: true)
        if current == from { current = clean }
        reload()
    }

    /// Delete a folder. `withContents` false moves its files up to the top level
    /// first (folder gone, files kept); true removes the folder and everything
    /// in it.
    /// Whether the folder holds any file of the shelf being shown — used to hide
    /// "전체삭제" when there is nothing of this kind to delete.
    func folderHasContents(_ name: String) -> Bool {
        items.contains { $0.folder == name && $0.kind == kind }
    }

    private static func kindOf(_ url: URL) -> MediaKind {
        Item.musicExts.contains(url.pathExtension.lowercased()) ? .music : .video
    }

    /// Delete a folder — but only for the shelf you are on. A folder is one real
    /// directory shared by both shelves (video and music can each hold files of
    /// the same folder name), so deleting must touch only THIS kind's files:
    /// wiping the whole directory took the other shelf's files with it and left a
    /// ghost folder behind on that shelf. The directory itself is removed only
    /// once no media of either kind remains in it.
    func deleteFolder(_ name: String, withContents: Bool) {
        let fm = FileManager.default
        let dir = root.appendingPathComponent(name)
        let inside = (try? fm.contentsOfDirectory(
            at: dir, includingPropertiesForKeys: nil, options: [.skipsHiddenFiles])) ?? []
        let mine = inside.filter { Self.mediaExts.contains($0.pathExtension.lowercased())
                                    && Self.kindOf($0) == kind }
        if withContents {
            for f in mine { try? fm.removeItem(at: f) }
        } else {
            for f in mine {
                try? fm.moveItem(at: f, to: root.appendingPathComponent(f.lastPathComponent))
            }
        }
        remember(name, kind, add: false)
        // Drop the directory only when nothing of either kind is left in it.
        let remaining = (try? fm.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil,
                                                     options: [.skipsHiddenFiles])) ?? []
        if remaining.filter({ Self.mediaExts.contains($0.pathExtension.lowercased()) }).isEmpty {
            try? fm.removeItem(at: dir)
        }
        if current == name { current = nil }
        reload()
    }
}
