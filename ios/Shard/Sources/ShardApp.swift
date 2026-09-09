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
    // 자동 재서명 공유 인스턴스를 **관찰**한다 — 포그라운드 '재서명 필요' 알림창과 자동 재서명 완료 후
    // '재시작' 팝업을 시트가 닫힌 상태에서도 루트에서 띄우려면 여기서 바인딩해야 한다.
    @ObservedObject private var autoResign = ResignModel.shared
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
                }, libraryVisible: showLibrary, prefs: prefs) {
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

                // 자동(포그라운드) 재서명이 도는 동안 스피너 — '재서명 필요' 확인 ~ '앱을 다시 시작해 주세요'
                // 사이(요청). 진행 중임을 알려 그 사이 백그라운드로 가지 않게 한다(가면 재서명이 끊길 수 있음).
                // 수동(시트)은 자체 버튼 표시가 있으니 여기선 자동(shared.running)만 덮는다.
                if autoResign.running {
                    ZStack {
                        Color.black.opacity(0.45).ignoresSafeArea()
                        VStack(spacing: 12) {
                            ProgressView().scaleEffect(1.4).tint(.white)
                            Text("재서명 중…").font(.callout).foregroundColor(.white)
                        }
                        .padding(24)
                        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 16))
                    }
                    .zIndex(20)
                }
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
            // '재서명 필요' 알림창이 떠 있는데 확인 없이 홈으로 가면(요청) 동의로 보고 백그라운드로 조용히
            // 재서명한다. 팝업이 안 떠 있으면 no-op.
            if phase == .background {
                autoResign.confirmRenewFromBackground()
            }
            // 활성화(콜드런치·포그라운드 복귀) 때: 포그라운드는 만료 급할 때(≤1일) '재서명 필요' 알림창을
            // 띄우고, 정기 갱신은 새벽 BGTask가 조용히 한다. 그리고 만료 하루 전 로컬 알림을 (재)예약해
            // 앱을 안 열어도 알림이 오게 한다.
            if phase == .active {
                autoResign.autoRenewIfNeeded(nothingPlaying: !player.isPlaying)
                autoResign.scheduleExpiryReminder()
                // 2단계 자체 업데이트: Veil 마커에 새 버전이 있으면 미서명 ipa를 받아 재서명·설치한다.
                // update_url.txt(인프라)가 없으면 조용히 넘어가 — 켜기 전엔 아무 일도 안 한다.
                Task { await autoResign.checkForUpdate() }
            }
        }
        // 포그라운드 만료 임박 '재서명 필요' 알림창(요청): 확인만, 누르면 저장된 계정으로 재서명 시작 →
        // 완료 시 아래 '재시작' 팝업이 뜬다. 자동 재서명은 시트가 닫혀 있어도 떠야 하므로 루트에 둔다.
        .alert("재서명이 필요합니다", isPresented: $autoResign.showRenewPrompt) {
            Button("확인") { autoResign.confirmRenew() }
        } message: {
            Text("서명 만료가 임박했습니다. 확인을 누르면 지금 재서명합니다.")
        }
        // 자동(포그라운드) 재서명이 스테이징을 마치면 뜨는 재시작 팝업 — 수동 재서명의 것(시트)과 별개로,
        // 자동은 공유 인스턴스라 여기 루트에 바인딩해야 뜬다. 확인 → 종료(다음 실행 때 새 서명 적용).
        .alert("앱을 다시 시작해 주세요", isPresented: $autoResign.showRestartAlert) {
            Button("확인") { exit(0) }
        }
        // 자동(포그라운드) 재서명이 실패하면 이유를 보여준다 — 예전엔 errorText가 시트에만 떠(자동은 시트가
        // 닫힘) 실패가 조용히 묻혔고, 그래서 "재설치도 스탬프 갱신도 안 된다"의 원인이 안 보였다.
        .alert("재서명 실패", isPresented: Binding(
            get: { autoResign.errorText != nil },
            set: { if !$0 { autoResign.errorText = nil } }
        )) {
            Button("확인") { }
        } message: {
            Text(autoResign.errorText ?? "")
        }
    }
}
