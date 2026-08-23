import SwiftUI
import UniformTypeIdentifiers

/// The library, over the browser: a header with the video/music switch, folder
/// chips, and the list — the shape of the Android app's library. Playing a row
/// opens the player over the current shelf.
struct LibraryScreen: View {
    @ObservedObject var store: LibraryStore
    @ObservedObject var downloads: DownloadsStore
    @ObservedObject var prefs: PlaybackPrefs
    @ObservedObject var player: VLCController
    var close: () -> Void

    @StateObject private var probe = MediaProbe()
    @State private var currentIndex: Int?
    @State private var fullscreen = false
    @State private var showNewFolder = false
    @State private var newFolder = ""
    @State private var renaming: Item?
    @State private var renameText = ""
    @State private var renamingFolder: String?
    @State private var folderRenameText = ""
    @State private var deletingFolder: String?
    @State private var showSettings = false
    @State private var fileMenu: Item?
    @State private var showFolderPick = false
    /// Direction of the last shelf switch, so the list slides in the way the
    /// finger went: to music (a left swipe) the new list enters from the right.
    @State private var toMusic = true

    var body: some View {
        ZStack {
            VStack(spacing: 0) {
                header
                shelfSwitch
                    .padding(.bottom, 12)   // wider gap above the folders…
                folderBar
                if currentIndex != nil && !fullscreen {
                    stage.aspectRatio(16.0 / 9.0, contentMode: .fit)
                        .background(Color.black)
                        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                        .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous).stroke(Color.toolbar, lineWidth: 1))
                        .padding(.horizontal, 10).padding(.bottom, 6)
                        .transition(.move(edge: .trailing).combined(with: .opacity))
                }
                if !downloads.items.isEmpty { downloadsStrip }
                if store.visible.isEmpty {
                    empty
                } else {
                    // Slide in the direction of the switch: to music the new list
                    // enters from the right and the old leaves to the left.
                    list.id(store.kind).transition(.asymmetric(
                        insertion: .move(edge: toMusic ? .trailing : .leading),
                        removal: .move(edge: toMusic ? .leading : .trailing)
                    ))
                }
            }
            if currentIndex != nil && fullscreen {
                stage.ignoresSafeArea().zIndex(2)
            }
            if let item = fileMenu {
                Color.black.opacity(0.35).ignoresSafeArea()
                    .onTapGesture { fileMenu = nil; showFolderPick = false }
                    .zIndex(6)
                fileMenuCard(item).zIndex(7)
            }
            if showNewFolder {
                Color.black.opacity(0.35).ignoresSafeArea()
                    .onTapGesture { showNewFolder = false; newFolder = "" }
                    .zIndex(4)
                newFolderCard.zIndex(5)
            }
            if showSettings {
                Color.black.opacity(0.001).ignoresSafeArea()
                    .onTapGesture { showSettings = false }
                    .zIndex(2.5)
                settingsPanel
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
                    .padding(.top, 48).padding(.trailing, 12)
                    .zIndex(3)
            }
        }
        .background(Color.surface.ignoresSafeArea())
        .foregroundColor(.onSurface)
        // A finished download adds a file; reloading when the downloads list
        // changes makes it appear without leaving and coming back.
        .onChange(of: downloads.items.count) { _ in store.reload() }
        // Coming back to the library, put the last-played file's stage back where
        // it was — the player kept running (or stayed paused) in the meantime, so
        // it just needs the stage drawn onto it again.
        .onAppear {
            if currentIndex == nil, let url = player.currentURL,
               let item = store.items.first(where: { $0.url == url }) {
                store.kind = item.kind
                store.current = item.folder
                withAnimation(.easeOut(duration: 0.25)) {
                    currentIndex = store.visible.firstIndex(where: { $0.url == url })
                }
            }
        }
        .alert("이름 바꾸기", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            TextField("새 이름", text: $renameText)
            Button("바꾸기") { if let item = renaming { store.rename(item, to: renameText) }; renaming = nil }
            Button("취소", role: .cancel) { renaming = nil }
        }
        .alert("폴더 이름 바꾸기", isPresented: Binding(get: { renamingFolder != nil }, set: { if !$0 { renamingFolder = nil } })) {
            TextField("새 이름", text: $folderRenameText)
            Button("바꾸기") { if let f = renamingFolder { store.renameFolder(f, to: folderRenameText) }; renamingFolder = nil }
            Button("취소", role: .cancel) { renamingFolder = nil }
        }
        .confirmationDialog("폴더 삭제", isPresented: Binding(get: { deletingFolder != nil }, set: { if !$0 { deletingFolder = nil } }), titleVisibility: .visible) {
            Button("전체삭제", role: .destructive) {
                if let f = deletingFolder { store.deleteFolder(f, withContents: true) }; deletingFolder = nil
                verifyPlaying()
            }
            Button("폴더삭제") {
                if let f = deletingFolder { store.deleteFolder(f, withContents: false) }; deletingFolder = nil
                verifyPlaying()
            }
            Button("취소", role: .cancel) { deletingFolder = nil }
        } message: {
            Text("'전체삭제'는 폴더와 안의 파일을 모두 지웁니다. '폴더삭제'는 폴더만 지우고 파일은 저장소로 옮깁니다.")
        }
    }

    /// The file's own menu, with a folder panel that slides out to the right for
    /// "move to folder" — half-screen at most, not a full system sheet.
    private func fileMenuCard(_ item: Item) -> some View {
        HStack(alignment: .top, spacing: 10) {
            VStack(spacing: 0) {
                menuRow("이름 바꾸기", "pencil") {
                    renaming = item; renameText = item.name; fileMenu = nil
                }
                Divider().background(Color.toolbar)
                menuRow("폴더로 이동", showFolderPick ? "chevron.right" : "folder") {
                    withAnimation { showFolderPick.toggle() }
                }
                Divider().background(Color.toolbar)
                menuRow("삭제", "trash", tint: .red) { store.delete(item); fileMenu = nil; verifyPlaying() }
            }
            .frame(width: 190)
            .background(Color.chrome)
            .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))

            if showFolderPick {
                // Height follows the number of folders — no fixed box.
                VStack(spacing: 0) {
                    menuRow("저장소", "house.fill") { store.move(item, to: nil); fileMenu = nil }
                    ForEach(store.folders.filter { $0 != item.folder }, id: \.self) { folder in
                        Divider().background(Color.toolbar)
                        menuRow(folder, "folder") { store.move(item, to: folder); fileMenu = nil }
                    }
                }
                .frame(width: 160)
                .background(Color.chrome)
                .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
                .transition(.move(edge: .leading).combined(with: .opacity))
            }
        }
        .shadow(radius: 16)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func menuRow(_ label: String, _ icon: String, tint: Color = .onSurface, tap: @escaping () -> Void) -> some View {
        Button(action: tap) {
            HStack(spacing: 10) {
                Image(systemName: icon).frame(width: 18)
                Text(label).font(.subheadline)
                Spacer()
            }
            .foregroundColor(tint)
            .padding(.horizontal, 14).padding(.vertical, 12)
            .contentShape(Rectangle())
        }
    }

    private var newFolderCard: some View {
        VStack(spacing: 14) {
            Text("새 폴더").font(.headline)
            TextField("폴더 이름", text: $newFolder)
                .textFieldStyle(.roundedBorder)
                .autocapitalization(.none)
            HStack(spacing: 10) {
                Button("취소") { showNewFolder = false; newFolder = "" }
                    .frame(maxWidth: .infinity)
                Button {
                    store.createFolder(newFolder); newFolder = ""; showNewFolder = false
                } label: {
                    Text("만들기").bold().frame(maxWidth: .infinity)
                }
                .foregroundColor(.accent)
            }
        }
        .padding(18)
        .frame(width: 280)
        .background(Color.chrome)
        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        .shadow(radius: 20)
    }

    /// Playback settings, the phone's way: each mode lit accent when on, grey
    /// when off — a plain panel, not a system menu (which flickered).
    private var settingsPanel: some View {
        VStack(spacing: 0) {
            settingRow("순서대로", "arrow.right.to.line", on: prefs.end == .next) { prefs.toggleEnd(.next) }
            Divider().background(Color.toolbar)
            settingRow("무작위", "shuffle", on: prefs.end == .shuffle) { prefs.toggleEnd(.shuffle) }
            Divider().background(Color.toolbar)
            settingRow("백그라운드 재생", "moon.fill", on: prefs.background) { prefs.background.toggle() }
        }
        .frame(width: 180)
        .background(Color.chrome)
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        .shadow(radius: 8)
    }

    private func settingRow(_ label: String, _ icon: String, on: Bool, tap: @escaping () -> Void) -> some View {
        Button(action: tap) {
            HStack(spacing: 10) {
                // Only the icon changes colour when on; the label stays readable.
                Image(systemName: icon).foregroundColor(on ? .accent : .muted)
                Text(label).font(.subheadline).foregroundColor(.onSurface)
                Spacer()
            }
            .padding(.horizontal, 14).padding(.vertical, 11)
            .contentShape(Rectangle())
        }
    }

    private var stage: some View {
        PlayerStage(
            controller: player,
            // Title comes from the file actually playing, not visible[index]:
            // currentIndex points into the current shelf, so switching shelves
            // while music plays otherwise showed the top video's name.
            title: store.items.first(where: { $0.url == player.currentURL })?.name ?? "",
            fullscreen: $fullscreen,
            onStop: { stopPlayer() },
            onPullToWeb: { stopPlayer(); close() },
            onPrev: { if let i = currentIndex, i > 0 { play(at: i - 1) } },
            onNext: { if let i = currentIndex, i + 1 < store.visible.count { play(at: i + 1) } },
            hasPrev: (currentIndex ?? 0) > 0,
            hasNext: (currentIndex ?? Int.max) + 1 < store.visible.count,
            isMusic: store.kind == .music
        )
    }

    /// A row dropped on a folder chip moves that file into the folder (nil =
    /// the top level).
    private func drop(_ providers: [NSItemProvider], to folder: String?) -> Bool {
        guard let provider = providers.first else { return false }
        provider.loadObject(ofClass: NSURL.self) { object, _ in
            guard let url = (object as? NSURL) as URL? else { return }
            DispatchQueue.main.async {
                if let item = store.items.first(where: { $0.url == url }) { store.move(item, to: folder) }
            }
        }
        return true
    }

    private func play(at index: Int) {
        guard store.visible.indices.contains(index) else { return }
        currentIndex = index
        player.onEnded = { advanceOnEnd() }
        player.nowPlayingTitle = store.visible[index].name
        player.onRemoteNext = { if let i = currentIndex, i + 1 < store.visible.count { play(at: i + 1) } }
        player.onRemotePrev = { if let i = currentIndex, i > 0 { play(at: i - 1) } }
        player.open(store.visible[index].url)
    }

    private func stopPlayer() {
        player.stop()
        currentIndex = nil
        fullscreen = false
    }

    /// After a delete, if the file playing is no longer on disk, stop — a folder
    /// deleted out from under a playing file otherwise kept sounding.
    private func verifyPlaying() {
        guard let url = player.currentURL else { return }
        if !store.items.contains(where: { $0.url == url }) { stopPlayer() }
    }

    private func advanceOnEnd() {
        guard let i = currentIndex else { return }
        switch prefs.end {
        case .next:
            if i + 1 < store.visible.count { play(at: i + 1) } else { stopPlayer() }
        case .shuffle:
            guard store.visible.count > 1 else { return }
            var n = i
            while n == i { n = Int.random(in: 0..<store.visible.count) }
            play(at: n)
        case .stop:
            break
        }
    }

    private var header: some View {
        HStack {
            Button(action: close) { Image(systemName: "chevron.left").font(.title3) }
            Spacer()
            Text(store.current ?? "보관함").font(.headline)
            Spacer()
            Button { showSettings.toggle() } label: { Image(systemName: "gearshape").font(.title3) }
        }
        .padding(.horizontal, 16).padding(.vertical, 12)
    }

    private var shelfSwitch: some View {
        HStack(spacing: 0) {
            shelfTab(.video, "비디오", "film")
            shelfTab(.music, "음악", "music.note")
        }
        .padding(3)
        .background(Color.toolbar)
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .padding(.horizontal, 16)
    }

    private func shelfTab(_ kind: MediaKind, _ label: String, _ icon: String) -> some View {
        let on = store.kind == kind
        return Button { setKind(kind) } label: {
            HStack(spacing: 6) {
                Image(systemName: icon)
                Text(label).bold()
            }
            .font(.subheadline)
            .padding(.vertical, 7)
            .frame(maxWidth: .infinity)
            .background(on ? Color.chrome : Color.clear)
            .foregroundColor(on ? .onSurface : .muted)
            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        }
    }

    /// Switch shelf, remembering the direction so the list slides accordingly.
    private func setKind(_ kind: MediaKind) {
        guard store.kind != kind else { return }
        toMusic = (kind == .music)
        withAnimation(.easeInOut(duration: 0.2)) { store.kind = kind }
    }

    private var folderBar: some View {
        HStack(spacing: 8) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    // The top level is home — a house, like the desktop's storage tab.
                    chip(active: store.current == nil, tap: { store.current = nil }) {
                        Image(systemName: "house.fill").font(.subheadline)
                    }
                    .onDrop(of: [.fileURL], isTargeted: nil) { drop($0, to: nil) }
                    ForEach(store.folders, id: \.self) { folder in
                        chip(active: store.current == folder, tap: { store.current = folder }) {
                            Text(folder).font(.subheadline)
                        }
                        .onDrop(of: [.fileURL], isTargeted: nil) { drop($0, to: folder) }
                        .contextMenu {
                            Button { renamingFolder = folder; folderRenameText = folder } label: {
                                Label("이름 바꾸기", systemImage: "pencil")
                            }
                            Button(role: .destructive) { deletingFolder = folder } label: {
                                Label("폴더 삭제", systemImage: "trash")
                            }
                        }
                    }
                }
                .padding(.leading, 16)
            }
            // The make-folder button stays pinned at the right, not among the tabs.
            Button { showNewFolder = true } label: {
                Image(systemName: "folder.badge.plus").foregroundColor(.muted).padding(8)
            }
            .padding(.trailing, 12)
        }
        .padding(.bottom, 4)
    }

    private func chip<Content: View>(
        active: Bool, tap: @escaping () -> Void, @ViewBuilder label: () -> Content
    ) -> some View {
        Button(action: tap) {
            label()
                .padding(.horizontal, 14).padding(.vertical, 6)
                .background(active ? Color.accent : Color.toolbar)
                .foregroundColor(active ? .onAccent : .onSurface)
                .clipShape(RoundedRectangle(cornerRadius: 9, style: .continuous))
        }
    }

    private var downloadsStrip: some View {
        VStack(spacing: 6) {
            ForEach(downloads.items.prefix(3)) { item in
                VStack(alignment: .leading, spacing: 2) {
                    HStack {
                        Text(item.title).font(.caption).lineLimit(1)
                        Spacer()
                        switch item.status {
                        case .running:
                            Button { downloads.cancel(item.id) } label: {
                                Image(systemName: "xmark.circle").font(.caption)
                            }
                        case .finished:
                            Image(systemName: "checkmark.circle.fill").font(.caption).foregroundColor(.green)
                        case .cancelled:
                            Text("취소됨").font(.caption2).foregroundColor(.muted)
                        case .failed(let message):
                            Text(message).font(.caption2).foregroundColor(.red).lineLimit(1)
                        }
                    }
                    if item.status == .running {
                        ProgressView(value: item.fraction).tint(.accent)
                        Text(item.detail).font(.caption2).foregroundColor(.muted).lineLimit(1)
                    }
                }
            }
        }
        .padding(.horizontal, 16).padding(.vertical, 6)
        .background(Color.chrome)
    }

    private var empty: some View {
        Text(store.kind == .music ? "저장한 음악이 없습니다" : "저장한 영상이 없습니다")
            .foregroundColor(.muted)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var list: some View {
        ScrollView {
            LazyVStack(spacing: 0) {
                ForEach(Array(store.visible.enumerated()), id: \.element.id) { index, item in
                    row(item)
                        // A tap plays; a long-press opens the file's own menu (a
                        // custom one, so "move to folder" can show the folders in
                        // a panel beside it rather than a giant system sheet).
                        .contentShape(Rectangle())
                        .onTapGesture { play(at: index) }
                        .onLongPressGesture(minimumDuration: 0.4) {
                            fileMenu = item; showFolderPick = false
                        }
                    Divider().background(Color.toolbar)
                }
            }
        }
        // Horizontal fling on the list. Right: switch music→video, then a second
        // right leaves to the web. Left: switch video→music. Works while a video
        // plays too (the player has its own gestures, on the picture itself).
        .simultaneousGesture(
            DragGesture(minimumDistance: 40)
                .onEnded { value in
                    // Clearly horizontal only, so ordinary up/down scrolling is
                    // never read as a shelf switch.
                    guard abs(value.translation.width) > 80,
                          abs(value.translation.width) > abs(value.translation.height) * 2.5 else { return }
                    if value.translation.width > 0 {
                        if store.kind == .music { setKind(.video) }
                        else { close() }
                    } else {
                        if store.kind == .video { setKind(.music) }
                    }
                }
        )
    }

    private func row(_ item: Item) -> some View {
        HStack(spacing: 12) {
            ZStack(alignment: .bottomTrailing) {
                thumbnail(item)
                if let duration = probe.result(for: item.url)?.duration {
                    Text(duration)
                        .font(.system(size: 10, weight: .semibold))
                        .padding(.horizontal, 4).padding(.vertical, 1)
                        .background(Color.black.opacity(0.7)).foregroundColor(.white)
                        .cornerRadius(3).padding(3)
                }
            }
            VStack(alignment: .leading, spacing: 3) {
                Text(item.name).foregroundColor(.onSurface).lineLimit(2)
                Text(describe(item)).font(.caption).foregroundColor(.muted)
            }
            Spacer()
        }
        .padding(.horizontal, 16).padding(.vertical, 8)
        .contentShape(Rectangle())
    }

    /// Size · age — the folder is the chip you are already in, so it is not
    /// repeated on every row.
    private func describe(_ item: Item) -> String {
        "\(ByteCountFormatter.string(fromByteCount: item.size, countStyle: .file)) · \(age(item.url))"
    }

    private func age(_ url: URL) -> String {
        guard let date = try? url.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate
        else { return "" }
        let days = Calendar.current.dateComponents([.day], from: date, to: Date()).day ?? 0
        switch days {
        case 0: return "오늘"
        case 1: return "어제"
        default: return "\(days)일 전"
        }
    }

    private func thumbnail(_ item: Item) -> some View {
        // Music files often carry embedded cover art, which the thumbnailer
        // returns just like a video frame — so try the image first for both, and
        // fall back to a kind icon.
        Group {
            if let image = probe.result(for: item.url)?.image ?? cover(item) {
                Image(uiImage: image).resizable().aspectRatio(contentMode: .fill)
            } else {
                ZStack {
                    Color.toolbar
                    Image(systemName: item.kind == .music ? "music.note" : "film")
                        .foregroundColor(.muted)
                }
            }
        }
        .frame(width: 104, height: 58)
        .clipShape(RoundedRectangle(cornerRadius: 6))
    }

    /// The song cover saved at download time, for music files whose own bytes
    /// carry no embedded art (a bare .m4a). Video tiles use the frame thumbnail.
    private func cover(_ item: Item) -> UIImage? {
        guard item.kind == .music else { return nil }
        return Covers.load(Covers.keyFor(item.url.lastPathComponent))
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
