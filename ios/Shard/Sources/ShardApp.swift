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
        ZStack {
            Color.surface.ignoresSafeArea()

            BrowserScreen(downloads: downloads) {
                library.reload()
                withAnimation(.easeOut(duration: 0.22)) { showLibrary = true }
            }

            if showLibrary {
                LibraryScreen(store: library, downloads: downloads, prefs: prefs, player: player) {
                    withAnimation(.easeIn(duration: 0.18)) { showLibrary = false }
                    // Leaving the library stops the player unless background play
                    // is on — otherwise the sound kept going after the screen was
                    // gone.
                    if !prefs.background { player.stop() }
                }
                .transition(.move(edge: .trailing))
                .zIndex(1)
            }
        }
        .tint(.accent)
    }
}
