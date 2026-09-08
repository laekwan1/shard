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
    // 앱 수준 전용 재서명 인스턴스 — 시트의 ResignModel과 별개로, 실행 시 만료 임박하면 조용히 자동
    // 갱신하는 데만 쓴다(시트를 안 열어도 돌아야 하므로 여기 둔다). 동시 서명은 사실상 안 겹치고
    // (자동은 .active 순간에만, 수동은 시트를 직접 열어야) 겹쳐도 RSD 터널 점유로 하나가 에러날 뿐 무해.
    @StateObject private var resignAuto = ResignModel()
    @State private var showLibrary = false
    // 앱이 실제로 OS 백그라운드(홈 버튼·잠금)로 들어가는 순간을 잡으려는 것. 오디오 세션이 .playback +
    // audio 백그라운드 모드라, 아무 처리도 안 하면 '백그라운드 재생'이 꺼져 있어도 계속 재생된다(사용자 지적).
    @Environment(\.scenePhase) private var scenePhase

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
                }, libraryVisible: showLibrary) {
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
        .onAppear { SystemVolume.shared.attach() }
        // '백그라운드 재생'이 꺼져 있으면 앱이 백그라운드로 들어갈 때 **종료처리(stop)**한다. pause만 하면
        // 소리는 멎어도 잠금화면/알림창에 재생 컨트롤(Now Playing)이 남는데, 사용자는 "백그라운드 재생일 때만
        // 컨트롤이 뜨고 아니면 그냥 종료"를 원했다 — stop()이 오디오 세션을 내리고 Now Playing을 지워 컨트롤을
        // 없앤다. 설정 ON이면 그대로 둬(재생·컨트롤 유지) 백그라운드 재생이 된다.
        .onChange(of: scenePhase) { phase in
            if phase == .background && !prefs.background {
                player.stop()
            }
            // 활성화(콜드런치·포그라운드 복귀) 때 만료 임박하면 조용히 자동 재서명(팝업 없이, 다음
            // 콜드런치에 적용 · VPN 켜져 있고 재생 안 할 때만 · 하루 1회). 모르는 사람도 그냥 앱을 쓰면
            // 서명이 알아서 갱신되도록.
            if phase == .active {
                resignAuto.autoRenewIfNeeded(nothingPlaying: !player.isPlaying)
            }
        }
    }
}
