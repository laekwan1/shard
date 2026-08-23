import SwiftUI

@main
struct ShardApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    var body: some Scene {
        WindowGroup {
            RootView().preferredColorScheme(.dark)
        }
    }
}

/// The browser is the app; the library slides in over it, the way the Android
/// app is built — one screen, not a set of tabs.
struct RootView: View {
    @StateObject private var downloads = DownloadsStore()
    @StateObject private var library = LibraryStore()
    @StateObject private var prefs = PlaybackPrefs()
    // One player for the whole app, owned here — so it is never duplicated when
    // the library view comes and goes, which was stacking playback.
    @StateObject private var player = VLCController()
    @State private var showLibrary = false

    var body: some View {
        GeometryReader { geo in
            ZStack {
                Color.surface.ignoresSafeArea()

                BrowserScreen(downloads: downloads, onWebPlaying: { on in
                    // Only when the browser is the visible screen: a background page
                    // video reporting itself was pausing the library's own playback
                    // (a track paused ~1s in; full screen dropped to pause on exit).
                    guard !showLibrary else { return }
                    if on && player.isPlaying { player.pause() }
                }) {
                    // If the browser was turned to landscape (address rotate button),
                    // force portrait before the library slides in — free() alone left
                    // it sideways, so lock portrait, then free once it has turned.
                    Orientation.shared.lock(.portrait, to: .portrait)
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { Orientation.shared.free() }
                    library.reload()
                    // Point the library at the playing file's shelf/folder BEFORE it
                    // slides in, so the list is not re-inserted mid-slide (which read
                    // as the list popping in while the rest slid).
                    if let url = player.currentURL,
                       let item = library.items.first(where: { $0.url == url }) {
                        library.kind = item.kind
                        library.current = item.folder
                    }
                    withAnimation(.easeOut(duration: 0.24)) { showLibrary = true }
                }

                // The library is kept mounted and slid with an offset rather than
                // inserted with a .transition: a SwiftUI transition snapshots the
                // view, and the live VLC surface cannot be snapshotted — so the
                // video popped into place while the rest slid in. An offset is a
                // real layout move, so the video travels with the list.
                LibraryScreen(store: library, downloads: downloads, prefs: prefs,
                              player: player, visible: showLibrary) {
                    withAnimation(.easeIn(duration: 0.2)) { showLibrary = false }
                    // Leaving the library stops the player unless background play
                    // is on — otherwise the sound kept going after the screen was
                    // gone.
                    if !prefs.background { player.stop() }
                }
                .offset(x: showLibrary ? 0 : geo.size.width + geo.safeAreaInsets.trailing)
                .zIndex(1)
            }
        }
        .tint(.accent)
        .onAppear { SystemVolume.shared.attach(); AudioKeepAlive.shared.start() }
    }
}
