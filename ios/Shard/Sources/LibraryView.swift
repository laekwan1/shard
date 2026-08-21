import SwiftUI

/// The saved files: folder chips across the top, a list with thumbnails and
/// length badges below, and a player that walks the current folder.
struct LibraryView: View {
    @StateObject private var store = LibraryStore()
    @StateObject private var probe = MediaProbe()

    @State private var playingIndex: Int?
    @State private var showNewFolder = false
    @State private var newFolder = ""
    @State private var renaming: Item?
    @State private var renameText = ""

    var body: some View {
        NavigationView {
            VStack(spacing: 0) {
                folderBar
                if store.visible.isEmpty {
                    Text("아직 저장한 항목이 없습니다")
                        .foregroundColor(.secondary)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    list
                }
            }
            .navigationTitle(store.current ?? "보관함")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                Button { showNewFolder = true } label: { Image(systemName: "folder.badge.plus") }
            }
            .onAppear { store.reload() }
        }
        .fullScreenCover(item: Binding(
            get: { playingIndex.map { PlayIndex(value: $0) } },
            set: { playingIndex = $0?.value }
        )) { start in
            VLCPlayerScreen(
                playlist: store.visible.map { $0.url },
                start: start.value,
                onClose: { playingIndex = nil }
            )
        }
        .alert("새 폴더", isPresented: $showNewFolder) {
            TextField("폴더 이름", text: $newFolder)
            Button("만들기") { store.createFolder(newFolder); newFolder = "" }
            Button("취소", role: .cancel) {}
        }
        .alert("이름 바꾸기", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("새 이름", text: $renameText)
            Button("바꾸기") { if let item = renaming { store.rename(item, to: renameText) }; renaming = nil }
            Button("취소", role: .cancel) { renaming = nil }
        }
    }

    private var folderBar: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                chip("전체", active: store.current == nil) { store.current = nil }
                ForEach(store.folders, id: \.self) { folder in
                    chip(folder, active: store.current == folder) { store.current = folder }
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
        }
    }

    private func chip(_ label: String, active: Bool, tap: @escaping () -> Void) -> some View {
        Button(action: tap) {
            Text(label)
                .font(.subheadline)
                .padding(.horizontal, 14).padding(.vertical, 6)
                .background(active ? Color.accentColor : Color(.secondarySystemBackground))
                .foregroundColor(active ? .white : .primary)
                .clipShape(Capsule())
        }
    }

    private var list: some View {
        List {
            ForEach(Array(store.visible.enumerated()), id: \.element.id) { index, item in
                Button { playingIndex = index } label: { row(item) }
                    .buttonStyle(.plain)
                    .contextMenu { menu(item) }
            }
        }
        .listStyle(.plain)
    }

    private func row(_ item: Item) -> some View {
        HStack(spacing: 12) {
            ZStack(alignment: .bottomTrailing) {
                thumbnail(item)
                if let duration = probe.result(for: item.url)?.duration {
                    Text(duration)
                        .font(.system(size: 10, weight: .semibold))
                        .padding(.horizontal, 4).padding(.vertical, 1)
                        .background(Color.black.opacity(0.7))
                        .foregroundColor(.white)
                        .cornerRadius(3)
                        .padding(3)
                }
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(item.name).lineLimit(2)
                Text(ByteCountFormatter.string(fromByteCount: item.size, countStyle: .file))
                    .font(.caption).foregroundColor(.secondary)
            }
            Spacer()
        }
        .padding(.vertical, 4)
    }

    private func thumbnail(_ item: Item) -> some View {
        Group {
            if let image = probe.result(for: item.url)?.image {
                Image(uiImage: image).resizable().aspectRatio(contentMode: .fill)
            } else {
                ZStack {
                    Color(.secondarySystemBackground)
                    Image(systemName: "play.rectangle").foregroundColor(.secondary)
                }
            }
        }
        .frame(width: 96, height: 54)
        .clipShape(RoundedRectangle(cornerRadius: 6))
    }

    @ViewBuilder
    private func menu(_ item: Item) -> some View {
        Button { renaming = item; renameText = item.name } label: { Label("이름 바꾸기", systemImage: "pencil") }
        Menu {
            if item.folder != nil {
                Button { store.move(item, to: nil) } label: { Label("최상위로", systemImage: "arrow.up") }
            }
            ForEach(store.folders.filter { $0 != item.folder }, id: \.self) { folder in
                Button { store.move(item, to: folder) } label: { Label(folder, systemImage: "folder") }
            }
        } label: { Label("폴더로 이동", systemImage: "folder") }
        Button(role: .destructive) { store.delete(item) } label: { Label("삭제", systemImage: "trash") }
    }
}

/// Identifiable wrapper so an index can drive `.fullScreenCover(item:)`.
private struct PlayIndex: Identifiable { let value: Int; var id: Int { value } }
