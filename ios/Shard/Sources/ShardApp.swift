import SwiftUI

@main
struct ShardApp: App {
    @StateObject private var downloads = DownloadsStore()

    var body: some Scene {
        WindowGroup {
            TabView {
                BrowserView(downloads: downloads)
                    .tabItem { Label("브라우저", systemImage: "globe") }
                DownloadsView(store: downloads)
                    .tabItem { Label("다운로드", systemImage: "arrow.down.circle") }
                    .badge(downloads.activeCount)
                LibraryView()
                    .tabItem { Label("보관함", systemImage: "square.stack") }
            }
        }
    }
}
