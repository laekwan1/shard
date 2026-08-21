import SwiftUI
import MobileVLCKit

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
}

/// The saved files, grouped into folders, plus the operations the library needs.
/// Files live under Documents: the top level and one directory per folder, so
/// "folders" are real directories the Files app shows too.
@MainActor
final class LibraryStore: ObservableObject {
    @Published var items: [Item] = []
    @Published var folders: [String] = []
    /// The folder being viewed, or nil for the top level.
    @Published var current: String?

    private var root: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    }

    private static let mediaExts: Set<String> =
        ["mp4", "m4v", "mov", "webm", "mkv", "m4a", "mp3", "opus", "aac", "wav"]

    func reload() {
        let fm = FileManager.default
        var found: [Item] = []
        var dirs: [String] = []

        // Top level: files and folder directories.
        let top = (try? fm.contentsOfDirectory(at: root, includingPropertiesForKeys: [.isDirectoryKey],
                                               options: [.skipsHiddenFiles])) ?? []
        for url in top {
            let isDir = (try? url.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) ?? false
            if isDir {
                dirs.append(url.lastPathComponent)
                let inside = (try? fm.contentsOfDirectory(at: url, includingPropertiesForKeys: nil,
                                                          options: [.skipsHiddenFiles])) ?? []
                for f in inside where Self.mediaExts.contains(f.pathExtension.lowercased()) {
                    found.append(Item(url: f, folder: url.lastPathComponent))
                }
            } else if Self.mediaExts.contains(url.pathExtension.lowercased()) {
                found.append(Item(url: url, folder: nil))
            }
        }
        items = found.sorted { $0.name < $1.name }
        folders = dirs.sorted()
    }

    /// The items shown for the current folder.
    var visible: [Item] { items.filter { $0.folder == current } }

    func createFolder(_ name: String) {
        let clean = name.trimmingCharacters(in: .whitespaces)
        guard !clean.isEmpty else { return }
        try? FileManager.default.createDirectory(at: root.appendingPathComponent(clean),
                                                 withIntermediateDirectories: true)
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
}
