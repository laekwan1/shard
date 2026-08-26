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
    /// Whether the library is the shown screen. It is kept mounted and slid by an
    /// offset (see ShardApp), so this drives the per-open restore that used to
    /// live in onAppear.
    var visible: Bool
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
    @State private var showFolderPick = false   // card shifted left
    @State private var showFolderPanel = false  // folder list revealed (a beat later)
    /// Direction of the last shelf switch, so the list slides in the way the
    /// finger went: to music (a left swipe) the new list enters from the right.
    @State private var toMusic = true
    @State private var orientGen = 0
    @State private var dragging: Item?
    @Namespace private var shelfNS

    var body: some View {
        ZStack {
            VStack(spacing: 0) {
                header
                shelfSwitch
                    .padding(.bottom, 12)   // wider gap above the folders…
                // A separate view (not this body) so the four-times-a-second player
                // updates do not re-render the folder chips — their context menus
                // flickered, and the rename/delete dialogs with them.
                FolderBar(store: store,
                          onRename: { renamingFolder = $0; folderRenameText = $0 },
                          onDelete: { deletingFolder = $0 },
                          onNewFolder: { showNewFolder = true },
                          onDrop: { drop($0, to: $1) })
                if player.currentURL != nil && !fullscreen {
                    stage.aspectRatio(16.0 / 9.0, contentMode: .fit)
                        .background(Color.black)
                        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                        .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous).stroke(Color.toolbar, lineWidth: 1))
                        .padding(.horizontal, 10).padding(.bottom, 6)
                }
                if !downloads.items.isEmpty { downloadsStrip }
                if store.visible.isEmpty {
                    empty
                } else {
                    // Slide the list the way the switch went: to music (a left
                    // fling) it enters from the right, the old leaves left — the
                    // segmented pill slides under the chosen tab at the same time.
                    list.id(store.kind).transition(.asymmetric(
                        insertion: .move(edge: toMusic ? .trailing : .leading),
                        removal: .move(edge: toMusic ? .leading : .trailing)
                    ))
                }
            }
            if player.currentURL != nil && fullscreen {
                stage.ignoresSafeArea().zIndex(2)
            }
            if let item = fileMenu {
                Color.black.opacity(0.35).ignoresSafeArea()
                    .onTapGesture { fileMenu = nil; showFolderPick = false; showFolderPanel = false }
                    .zIndex(6)
                fileMenuCard(item).zIndex(7)
            }
            if showNewFolder {
                Color.black.opacity(0.35).ignoresSafeArea()
                    .onTapGesture { showNewFolder = false; newFolder = "" }
                    .zIndex(4)
                newFolderCard.zIndex(5)
            }
            // Rename dialogs as custom cards (not system alerts) so a tap outside
            // cancels them, the way new-folder does.
            if renaming != nil {
                Color.black.opacity(0.35).ignoresSafeArea()
                    .onTapGesture { renaming = nil }.zIndex(8)
                renameCard(title: "이름 바꾸기", text: $renameText,
                           confirm: { if let it = renaming { store.rename(it, to: renameText) }; renaming = nil },
                           cancel: { renaming = nil }).zIndex(9)
            }
            if renamingFolder != nil {
                Color.black.opacity(0.35).ignoresSafeArea()
                    .onTapGesture { renamingFolder = nil }.zIndex(8)
                renameCard(title: "폴더 이름 바꾸기", text: $folderRenameText,
                           confirm: { if let f = renamingFolder { store.renameFolder(f, to: folderRenameText) }; renamingFolder = nil },
                           cancel: { renamingFolder = nil }).zIndex(9)
            }
            if let f = deletingFolder {
                Color.black.opacity(0.35).ignoresSafeArea()
                    .onTapGesture { deletingFolder = nil }.zIndex(8)
                deleteFolderCard(f).zIndex(9)
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
        .onChange(of: store.visible.count) { _ in store.shuffleQueue = [] }
        // Coming back to the library, put the last-played file's stage back where
        // it was — the player kept running (or stayed paused) in the meantime, so
        // it just needs the stage drawn onto it again.
        // A new track while full screen (the next one after a short ends) may have
        // a different shape — re-decide orientation once it starts.
        .onChange(of: player.currentURL) { _ in
            if fullscreen { DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { lockForVideo() } }
        }
        .onChange(of: visible) { shown in
            if !shown && fullscreen { fullscreen = false; Orientation.shared.free(); player.settling = false }
            // Each time the library opens, put the stage back on whatever is
            // playing. Runs on open (not once at mount) because the screen stays
            // mounted and only slides in and out.
            guard shown else { return }
            if let url = player.currentURL,
               let item = store.items.first(where: { $0.url == url }) {
                // Only reassign when different: setting store.kind changes the
                // list's .id and re-inserts it (an un-animated pop) while the whole
                // screen is already sliding in — which looked like the list "just
                // appearing" instead of travelling with the slide.
                if store.kind != item.kind { store.kind = item.kind }
                if store.current != item.folder { store.current = item.folder }
                currentIndex = store.visible.firstIndex(where: { $0.url == url })
            }
        }
    }

    /// The file's own menu, with a folder panel that slides out to the right for
    /// "move to folder" — half-screen at most, not a full system sheet.
    /// Where a file can be moved: the storage root (only if it is not already
    /// there) plus every folder except the one it is in. Empty means nowhere to
    /// move, so the "move" row is hidden.
    private func moveTargets(_ item: Item) -> [String?] {
        var targets: [String?] = []
        if item.folder != nil { targets.append(nil) }          // 저장소
        targets += store.folders.filter { $0 != item.folder }
        return targets
    }

    /// Two-step so the card slides left first, then the folder list slides out
    /// from where the row was — instead of both moving at once.
    private func toggleFolderMove() {
        // easeOut, not spring — the spring's overshoot read as the card stuttering
        // ("툭툭"). The panel then reveals in place (scale+fade from its top-left),
        // not sliding across.
        let move = Animation.easeOut(duration: 0.22)
        if !showFolderPick {
            withAnimation(move) { showFolderPick = true }               // card slides left first
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.22) {
                withAnimation(.easeOut(duration: 0.2)) { showFolderPanel = true }
            }
        } else {
            withAnimation(.easeOut(duration: 0.18)) { showFolderPanel = false }  // panel goes first
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.18) {
                withAnimation(move) { showFolderPick = false }          // then card returns
            }
        }
    }

    private func fileMenuCard(_ item: Item) -> some View {
        let targets = moveTargets(item)
        // The card is anchored (centred) and the folder panel is an OVERLAY, so
        // opening it never resizes or re-centres the card — that shift was the
        // "ghost / jumps right" the panel used to do. The panel is offset to sit
        // just right of the card, its top aligned to the move row.
        return VStack(spacing: 0) {
            menuRow("이름 바꾸기", "pencil") {
                renaming = item; renameText = item.name; fileMenu = nil
            }
            if !targets.isEmpty {
                Divider().background(Color.toolbar)
                Button { toggleFolderMove() } label: {
                    HStack(spacing: 10) {
                        Image(systemName: "folder").frame(width: 18)   // stays a folder, does not become the arrow
                        Text("폴더로 이동").font(.subheadline)
                        Spacer()
                        // The chevron appears in step with the folder panel (not the
                        // card's earlier slide), and goes away when it closes.
                        if showFolderPanel {
                            Image(systemName: "chevron.right").font(.caption).foregroundColor(.muted)
                                .transition(.opacity)
                        }
                    }
                    .foregroundColor(.onSurface)
                    .padding(.horizontal, 14).padding(.vertical, 12)
                    .contentShape(Rectangle())
                }
            }
            Divider().background(Color.toolbar)
            menuRow("삭제", "trash", tint: .red) { store.delete(item); fileMenu = nil; verifyPlaying() }
        }
        .frame(width: 200)
        .background(Color.chrome)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(alignment: .topLeading) {
            if showFolderPanel && !targets.isEmpty {
                VStack(spacing: 0) {
                    ForEach(Array(targets.enumerated()), id: \.offset) { i, folder in
                        if i > 0 { Divider().background(Color.toolbar) }
                        menuRow(folder ?? "저장소", folder == nil ? "house.fill" : "folder") {
                            store.move(item, to: folder); fileMenu = nil; showFolderPick = false; showFolderPanel = false
                        }
                    }
                }
                .frame(width: 170)
                .background(Color.chrome)
                .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
                .shadow(radius: 12)
                // Opens rightward from the move row: offset right of the card, top
                // aligned to that row (row height ≈ 45 with its divider).
                .offset(x: 208, y: 42)
                // Appears in place (grows from its top-left corner + fades) rather
                // than sliding across, so it reads as opening where it sits.
                .transition(.scale(scale: 0.85, anchor: .topLeading).combined(with: .opacity))
            }
        }
        .shadow(radius: 16)
        // Slide the card left as the panel opens, so the pair stays centred and
        // the panel does not run off the right edge — this is also the "move row
        // slides left while the folders slide out right" the design calls for.
        .offset(x: showFolderPick ? -90 : 0)
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

    /// A filled, full-width dialog button so the choices read as buttons, not
    /// plain text.
    private func dialogButton(_ title: String, fill: Color, text: Color,
                              action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title).font(.subheadline.weight(.semibold))
                .foregroundColor(text)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 11)
                .background(fill)
                .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        }
    }

    private func deleteFolderCard(_ folder: String) -> some View {
        VStack(spacing: 14) {
            Text("폴더 삭제").font(.headline)
            Text("'전체삭제'는 폴더와 안의 파일을 모두 지웁니다. '폴더삭제'는 폴더만 지우고 파일은 저장소로 옮깁니다.")
                .font(.caption).foregroundColor(.muted).multilineTextAlignment(.center)
            VStack(spacing: 8) {
                if store.folderHasContents(folder) {
                    dialogButton("전체삭제", fill: .red, text: .white) {
                        store.deleteFolder(folder, withContents: true); deletingFolder = nil; verifyPlaying()
                    }
                }
                dialogButton("폴더삭제", fill: Color.toolbar, text: .onSurface) {
                    store.deleteFolder(folder, withContents: false); deletingFolder = nil; verifyPlaying()
                }
                dialogButton("취소", fill: Color.toolbar, text: .muted) { deletingFolder = nil }
            }
        }
        .padding(18)
        .frame(width: 300)
        .background(Color.chrome)
        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        .shadow(radius: 20)
    }

    private func renameCard(title: String, text: Binding<String>,
                            confirm: @escaping () -> Void, cancel: @escaping () -> Void) -> some View {
        VStack(spacing: 14) {
            Text(title).font(.headline)
            TextField("새 이름", text: text)
                .textFieldStyle(.roundedBorder)
                .autocapitalization(.none)
            HStack(spacing: 10) {
                dialogButton("취소", fill: Color.toolbar, text: .muted, action: cancel)
                dialogButton("바꾸기", fill: .accent, text: .onAccent, action: confirm)
            }
        }
        .padding(18)
        .frame(width: 280)
        .background(Color.chrome)
        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        .shadow(radius: 20)
    }

    private var newFolderCard: some View {
        VStack(spacing: 14) {
            Text("새 폴더").font(.headline)
            TextField("폴더 이름", text: $newFolder)
                .textFieldStyle(.roundedBorder)
                .autocapitalization(.none)
            HStack(spacing: 10) {
                dialogButton("취소", fill: Color.toolbar, text: .muted) { showNewFolder = false; newFolder = "" }
                dialogButton("만들기", fill: .accent, text: .onAccent) {
                    store.createFolder(newFolder); newFolder = ""; showNewFolder = false
                }
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
            onPrev: { playPrev() },
            onNext: { playNext() },
            hasPrev: playingList().count > 1,
            hasNext: playingList().count > 1,
            // The stage reflects the PLAYING file's kind, not the shelf on view —
            // switching to the video shelf while a song plays must not blank the
            // stage to a video surface (it showed black).
            isMusic: store.items.first(where: { $0.url == player.currentURL })?.kind == .music,
            onEnterFullscreen: { enterFullscreen() },
            onExitFullscreen: { exitFullscreen() }
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
        start(store.visible[index])
    }

    /// Start a specific file. All playback goes through here so next/previous and
    /// end-of-track work off the PLAYING file's own shelf+folder, not whatever the
    /// library happens to be showing — switching the view while something plays no
    /// longer breaks the "play next" that follows.
    private func start(_ item: Item, record: Bool = true) {
        // Remember what we were on, so "previous" returns the real prior track
        // (important for shuffle, where the sequential neighbour is meaningless).
        if record, let cur = player.currentURL, cur != item.url {
            store.history.append(cur)
            if store.history.count > 200 { store.history.removeFirst() }
        }
        currentIndex = store.visible.firstIndex(where: { $0.url == item.url })
        player.onEnded = { advanceOnEnd() }
        player.nowPlayingTitle = item.name
        player.onRemoteNext = { playNext() }
        player.onRemotePrev = { playPrev() }
        player.open(item.url)
    }

    /// The list playback moves through: the shelf+folder the playing file belongs
    /// to. `store.items` is already sorted newest-first, and filtering keeps that
    /// order, so it matches what `visible` would show for that context.
    private func playingList() -> [Item] {
        guard let url = player.currentURL,
              let cur = store.items.first(where: { $0.url == url }) else { return store.visible }
        return store.items.filter { $0.kind == cur.kind && $0.folder == cur.folder }
    }
    private func playingIndex() -> Int? {
        guard let url = player.currentURL else { return nil }
        return playingList().firstIndex(where: { $0.url == url })
    }

    /// Manual "next": follows the end-mode — random when shuffle is on, otherwise
    /// the next in order (wrapping).
    private func playNext() {
        let list = playingList()
        guard let url = player.currentURL, let i = list.firstIndex(where: { $0.url == url }) else { return }
        if prefs.end == .shuffle { shuffleNext(list, current: url) }
        else { start(list[(i + 1) % list.count]) }
    }
    /// Manual "previous": the track actually played before this one (from history);
    /// falls back to the sequential previous when there is no history.
    private func playPrev() {
        let list = playingList()
        guard !list.isEmpty else { return }
        // Only go back to a track in the SAME shelf+folder as what is playing now —
        // the history stack mixes kinds, so without this, "previous" during a video
        // could jump back to a song you had played earlier.
        while let prev = store.history.popLast() {
            if let item = list.first(where: { $0.url == prev }) { start(item, record: false); return }
        }
        guard let i = playingIndex() else { return }
        start(list[(i - 1 + list.count) % list.count], record: false)
    }

    /// Pick the next shuffle track from a queue that holds every other track once;
    /// refilled (reshuffled, current excluded) only when it empties — so nothing
    /// repeats until the whole list has played.
    private func shuffleNext(_ list: [Item], current: URL) {
        let urls = Set(list.map { $0.url })
        store.shuffleQueue.removeAll { !urls.contains($0) || $0 == current }
        if store.shuffleQueue.isEmpty {
            store.shuffleQueue = list.map { $0.url }.filter { $0 != current }.shuffled()
        }
        guard let nextURL = store.shuffleQueue.first,
              let item = list.first(where: { $0.url == nextURL }) else { return }
        store.shuffleQueue.removeFirst()
        start(item)
    }

    private func stopPlayer() {
        player.stop()
        currentIndex = nil
        if fullscreen { fullscreen = false; Orientation.shared.free() }
        player.settling = false
    }

    /// Lock the interface to the playing video's orientation (landscape for a wide
    /// video, portrait for a short), behind the black cover.
    private func lockForVideo() {
        let s = player.videoSize
        let portrait = s.height > s.width && s.width > 0
        Orientation.shared.lock(portrait ? .portrait : .landscapeRight,
                                to: portrait ? .portrait : .landscapeRight)
    }

    /// Enter full screen: raise the stage, rotate behind a black cover so the
    /// rotation itself is never seen (it just appears already turned), then reveal.
    private func enterFullscreen() {
        orientGen += 1
        let gen = orientGen
        player.settling = true
        fullscreen = true
        // Rotate just after the black cover is painted (rotating in the same runloop
        // showed the windowed squish first), with one retry for reliability.
        let s = player.videoSize
        let willRotate = !(s.height > s.width && s.width > 0)   // wide → landscape turn
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) { if gen == orientGen { lockForVideo() } }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.26) { if gen == orientGen { lockForVideo() } }
        // Clear the black as soon as it can: a portrait video does not rotate, so
        // reveal quickly; a landscape one waits just long enough for the turn.
        let reveal = willRotate ? 0.4 : 0.16
        DispatchQueue.main.asyncAfter(deadline: .now() + reveal) { if gen == orientGen { player.settling = false } }
    }

    /// Exit full screen: rotate back to portrait FIRST (still full screen, black
    /// covering), THEN shrink to the window — so the list is never glimpsed while
    /// the screen is still turning.
    private func exitFullscreen() {
        orientGen += 1
        let gen = orientGen
        let s = player.videoSize
        let wasLandscape = !(s.height > s.width && s.width > 0)   // needs to turn back
        player.settling = true
        Orientation.shared.lock(.portrait, to: .portrait)
        DispatchQueue.main.asyncAfter(deadline: .now() + (wasLandscape ? 0.34 : 0.14)) {
            guard gen == orientGen else { return }
            fullscreen = false
            Orientation.shared.free()
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
                if gen == orientGen { player.settling = false }
            }
        }
    }

    /// After a delete, if the file playing is no longer on disk, stop — a folder
    /// deleted out from under a playing file otherwise kept sounding.
    private func verifyPlaying() {
        guard let url = player.currentURL else { return }
        if !store.items.contains(where: { $0.url == url }) { stopPlayer() }
    }

    private func advanceOnEnd() {
        let list = playingList()
        guard let url = player.currentURL, let i = list.firstIndex(where: { $0.url == url }),
              !list.isEmpty else { return }
        switch prefs.end {
        case .next:  start(list[(i + 1) % list.count])   // loop to the top after the last
        case .shuffle: shuffleNext(list, current: url)
        case .stop:  break
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
            // The lit pill is a single shared shape that slides under the chosen
            // tab (matchedGeometryEffect), the way the Android segmented control
            // moves its thumb — instead of one tab's fill blinking off and the
            // other's on.
            .background(
                ZStack {
                    if on {
                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .fill(Color.chrome)
                            .matchedGeometryEffect(id: "shelfPill", in: shelfNS)
                    }
                }
            )
            .foregroundColor(on ? .onSurface : .muted)
        }
    }

    /// Switch shelf, remembering the direction so the list slides accordingly.
    private func setKind(_ kind: MediaKind) {
        guard store.kind != kind else { return }
        toMusic = (kind == .music)
        store.shuffleQueue = []
        withAnimation(.spring(response: 0.34, dampingFraction: 0.86)) { store.kind = kind }
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
            .contentShape(Rectangle())                 // make the empty area swipeable
            .simultaneousGesture(shelfSwitchGesture)    // so an empty shelf can still switch
    }

    /// The horizontal fling that switches shelves — shared so it works on the list
    /// AND on an empty shelf (it used to live only on the list, so a video shelf
    /// with no items could not be swiped away).
    private var shelfSwitchGesture: some Gesture {
        DragGesture(minimumDistance: 40)
            .onEnded { value in
                // Clearly horizontal only, so ordinary up/down scrolling is never
                // read as a shelf switch.
                guard abs(value.translation.width) > 80,
                      abs(value.translation.width) > abs(value.translation.height) * 2.5 else { return }
                if value.translation.width > 0 {
                    if store.kind == .music { setKind(.video) } else { close() }
                } else {
                    if store.kind == .video { setKind(.music) }
                }
            }
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
                        // Double-tap opens the file's menu; a single tap plays. The
                        // menu moved off long-press because a hold is now the drag
                        // (onDrag) that carries the row onto a folder chip.
                        .onTapGesture(count: 2) { fileMenu = item; showFolderPick = false; showFolderPanel = false }
                        .onTapGesture { play(at: index) }
                        .opacity(dragging?.url == item.url ? 0.4 : 1)
                        .onDrag { dragging = item; return NSItemProvider(object: item.url as NSURL) }
                        // Dropping onto another row reorders; dragging up onto a folder
                        // chip still moves to that folder (that drop lives on the chip).
                        .onDrop(of: [.fileURL], delegate: RowReorderDrop(item: item, dragging: $dragging, store: store))
                    Divider().background(Color.toolbar)
                }
            }
        }
        // Horizontal fling switches shelves (right: music→video then leave to web;
        // left: video→music) — works while a video plays too.
        .simultaneousGesture(shelfSwitchGesture)
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
            // Music first tries its saved cover: the thumbnailer hands back a
            // black frame for an audio file, which would otherwise mask the cover.
            if let image = (item.kind == .music ? (cover(item) ?? probe.result(for: item.url)?.image)
                                                : (probe.result(for: item.url)?.image ?? cover(item))) {
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

    /// The cover saved at download time. Music: for a bare .m4a with no embedded
    /// art. Video: the YouTube thumbnail, used as a fallback because VLC often
    /// times out thumbnailing an AV1 frame (software decode) and returns nothing —
    /// so a saved cover is what keeps most video tiles from showing a blank film icon.
    private func cover(_ item: Item) -> UIImage? {
        Covers.load(Covers.keyFor(item.url.lastPathComponent))
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

/// Live-reorder: as a dragged row hovers over another, the store swaps their order.
/// Dropping onto a folder chip is a SEPARATE drop (on the chip) that moves to a
/// folder — this one only fires over list rows, so the two do not fight.
private struct RowReorderDrop: DropDelegate {
    let item: Item
    @Binding var dragging: Item?
    let store: LibraryStore
    func dropUpdated(info: DropInfo) -> DropProposal? { DropProposal(operation: .move) }
    func dropEntered(info: DropInfo) {
        guard let d = dragging, d.url != item.url else { return }
        withAnimation(.easeInOut(duration: 0.15)) { store.moveItem(d, over: item) }
    }
    func performDrop(info: DropInfo) -> Bool { dragging = nil; return true }
}

/// The folder chips row, split out of LibraryScreen so it observes only the store
/// — the player's frequent updates no longer re-render it, which is what made the
/// chip context menus (and the rename/delete dialogs off them) flicker mid-play.
private struct FolderBar: View {
    @ObservedObject var store: LibraryStore
    var onRename: (String) -> Void
    var onDelete: (String) -> Void
    var onNewFolder: () -> Void
    var onDrop: ([NSItemProvider], String?) -> Bool

    var body: some View {
        HStack(spacing: 8) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    chip(active: store.current == nil, tap: { store.current = nil }) {
                        Image(systemName: "house.fill").font(.subheadline)
                    }
                    .onDrop(of: [.fileURL], isTargeted: nil) { onDrop($0, nil) }
                    ForEach(store.folders, id: \.self) { folder in
                        chip(active: store.current == folder, tap: { store.current = folder }) {
                            Text(folder).font(.subheadline)
                        }
                        .onDrop(of: [.fileURL], isTargeted: nil) { onDrop($0, folder) }
                        .contextMenu {
                            Button { onRename(folder) } label: { Label("이름 바꾸기", systemImage: "pencil") }
                            Button(role: .destructive) { onDelete(folder) } label: {
                                Label("폴더 삭제", systemImage: "trash")
                            }
                        }
                    }
                }
                .padding(.leading, 16)
            }
            Button(action: onNewFolder) {
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
}
