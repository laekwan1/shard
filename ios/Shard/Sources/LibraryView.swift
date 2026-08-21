import SwiftUI
import AVKit

/// A saved file on disk.
struct SavedItem: Identifiable {
    let id = UUID()
    let url: URL
    var name: String { url.lastPathComponent }
    var size: Int64 {
        (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize).flatMap { Int64($0) } ?? 0
    }
}

/// The files the downloader has written, played in place with AVPlayer.
struct LibraryView: View {
    @State private var items: [SavedItem] = []
    @State private var playing: URL?

    var body: some View {
        NavigationView {
            Group {
                if items.isEmpty {
                    Text("아직 저장한 영상이 없습니다")
                        .foregroundColor(.secondary)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    List {
                        ForEach(items) { item in
                            Button {
                                playing = item.url
                            } label: {
                                HStack {
                                    Image(systemName: "play.rectangle.fill")
                                        .foregroundColor(.accentColor)
                                    VStack(alignment: .leading) {
                                        Text(item.name).lineLimit(1)
                                        Text(sizeText(item.size))
                                            .font(.caption).foregroundColor(.secondary)
                                    }
                                }
                            }
                        }
                        .onDelete(perform: delete)
                    }
                }
            }
            .navigationTitle("보관함")
            .onAppear(perform: reload)
        }
        .sheet(item: Binding(
            get: { playing.map { PlayerItem(url: $0) } },
            set: { playing = $0?.url }
        )) { item in
            VideoPlayer(player: AVPlayer(url: item.url))
                .ignoresSafeArea()
        }
    }

    private func reload() {
        let dir = Downloader.saveDirectory
        let files = (try? FileManager.default.contentsOfDirectory(
            at: dir, includingPropertiesForKeys: [.fileSizeKey], options: [.skipsHiddenFiles]
        )) ?? []
        let media = ["mp4", "m4v", "mov", "webm", "mkv", "m4a", "mp3"]
        items = files
            .filter { media.contains($0.pathExtension.lowercased()) }
            .map { SavedItem(url: $0) }
            .sorted { $0.name < $1.name }
    }

    private func delete(at offsets: IndexSet) {
        for index in offsets {
            try? FileManager.default.removeItem(at: items[index].url)
        }
        reload()
    }

    private func sizeText(_ bytes: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }
}

/// Identifiable wrapper so a URL can drive a `.sheet(item:)`.
private struct PlayerItem: Identifiable {
    let url: URL
    var id: String { url.absoluteString }
}
