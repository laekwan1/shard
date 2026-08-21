import SwiftUI

@main
struct ShardApp: App {
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
    @State private var showLibrary = false

    var body: some View {
        ZStack {
            Color.surface.ignoresSafeArea()

            BrowserScreen(downloads: downloads) {
                library.reload()
                withAnimation(.easeOut(duration: 0.22)) { showLibrary = true }
            }

            if showLibrary {
                LibraryScreen(store: library, downloads: downloads, prefs: prefs) {
                    withAnimation(.easeIn(duration: 0.18)) { showLibrary = false }
                }
                .transition(.move(edge: .trailing))
                .zIndex(1)
            }
        }
        .tint(.accent)
    }
}
