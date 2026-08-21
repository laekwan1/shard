import SwiftUI

@main
struct ShardApp: App {
    var body: some Scene {
        WindowGroup {
            TabView {
                BrowserView()
                    .tabItem { Label("브라우저", systemImage: "globe") }
                LibraryView()
                    .tabItem { Label("보관함", systemImage: "square.stack") }
            }
        }
    }
}
