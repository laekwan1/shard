import SwiftUI
import AVFoundation
import MediaPlayer
import MobileVLCKit

/// Drives one libVLC player and publishes what the controls need. libVLC plays
/// everything the engine writes — mp4, and the mkv/webm (VP9/Opus) AVPlayer
/// refuses — so the library needs one player, not a per-format guess.
final class VLCController: NSObject, ObservableObject, VLCMediaPlayerDelegate {
    let player = VLCMediaPlayer()
    @Published var position: Float = 0
    @Published var isPlaying = false
    @Published var elapsed = "0:00"
    @Published var duration = "0:00"
    @Published var rate: Float = 1
    @Published var muted = false
    var scrubbing = false
    var onEnded: (() -> Void)?
    /// Lock-screen / headset next & previous reach the playlist through these.
    var onRemoteNext: (() -> Void)?
    var onRemotePrev: (() -> Void)?
    var nowPlayingTitle = ""

    func toggleMute() {
        muted.toggle()
        player.audio?.isMuted = muted
    }

    private var currentURL: URL?
    private var pendingSeek: Float?
    private static let rates: [Float] = [1, 1.25, 1.5, 2, 0.5, 0.75]

    override init() {
        super.init()
        player.delegate = self
        let session = AVAudioSession.sharedInstance()
        try? session.setCategory(.playback, mode: .moviePlayback)
        try? session.setActive(true)
        setupRemoteCommands()
    }

    deinit {
        // Without this a controller that goes out of view kept its player alive
        // and playing in the background — pick another file and two (then three)
        // were heard at once. Stop it, and drop the remote-command handlers so
        // the lock screen does not talk to a dead player.
        player.stop()
        let center = MPRemoteCommandCenter.shared()
        for c in [center.playCommand, center.pauseCommand, center.togglePlayPauseCommand,
                  center.nextTrackCommand, center.previousTrackCommand, center.changePlaybackPositionCommand] {
            c.removeTarget(nil)
        }
    }

    /// Whether a file is loaded, so the library can show the stage again after
    /// coming back from the background instead of losing it.
    var hasMedia: Bool { player.media != nil }

    /// Lock screen and headset controls, so a video listened to like music is
    /// controlled like music.
    private func setupRemoteCommands() {
        let center = MPRemoteCommandCenter.shared()
        center.playCommand.addTarget { [weak self] _ in self?.player.play(); return .success }
        center.pauseCommand.addTarget { [weak self] _ in self?.player.pause(); return .success }
        center.togglePlayPauseCommand.addTarget { [weak self] _ in self?.toggle(); return .success }
        center.nextTrackCommand.addTarget { [weak self] _ in self?.onRemoteNext?(); return .success }
        center.previousTrackCommand.addTarget { [weak self] _ in self?.onRemotePrev?(); return .success }
        center.changePlaybackPositionCommand.addTarget { [weak self] event in
            guard let self = self,
                  let e = event as? MPChangePlaybackPositionCommandEvent,
                  let length = self.player.media?.length.intValue, length > 0 else { return .commandFailed }
            self.seek(to: Float(e.positionTime * 1000 / Double(length)))
            return .success
        }
    }

    private func updateNowPlaying() {
        var info: [String: Any] = [MPMediaItemPropertyTitle: nowPlayingTitle]
        if let length = player.media?.length.intValue, length > 0 {
            info[MPMediaItemPropertyPlaybackDuration] = Double(length) / 1000
        }
        info[MPNowPlayingInfoPropertyElapsedPlaybackTime] = Double(player.time.intValue) / 1000
        info[MPNowPlayingInfoPropertyPlaybackRate] = player.isPlaying ? Double(rate) : 0
        MPNowPlayingInfoCenter.default().nowPlayingInfo = info
    }

    func attach(to view: UIView) { player.drawable = view }

    func open(_ url: URL) {
        savePosition()
        currentURL = url
        player.media = VLCMedia(url: url)
        let saved = UserDefaults.standard.float(forKey: "pos:\(url.lastPathComponent)")
        pendingSeek = saved > 0.01 && saved < 0.98 ? saved : nil
        player.play()
        player.rate = rate
    }

    /// Scrubbing the desktop's way: while dragging, only the knob and the clock
    /// move — the player is not touched. It seeks once, on release. That is the
    /// change made on PC after live-seeking-per-pixel proved unpleasant: no
    /// storm of seeks, so none of the overlapping audio bursts a drag produced.
    func beginScrub() { scrubbing = true }

    /// While dragging: move the shown position only. No seek.
    func previewSeek(_ p: Float) { position = max(0, min(1, p)) }

    /// Let go: seek once to the final spot.
    func endScrub(_ p: Float) {
        scrubbing = false
        player.position = max(0, min(1, p))
    }

    func toggle() {
        if player.state == .ended {
            // A finished player will not resume on play() — put it back to the
            // start and go, so the button replays instead of doing nothing.
            player.position = 0
            player.play()
        } else {
            player.isPlaying ? player.pause() : player.play()
        }
    }
    func seek(to p: Float) { player.position = max(0, min(1, p)) }
    func jump(_ seconds: Int32) { seconds < 0 ? player.jumpBackward(-seconds) : player.jumpForward(seconds) }
    func rewindStep() { player.jumpBackward(1) }

    /// libVLC volume runs 0–200; we drive 0–1 from the drag.
    var volume: Float {
        get { Float(player.audio?.volume ?? 100) / 200 }
        set { player.audio?.volume = Int32(max(0, min(1, newValue)) * 200) }
    }

    func cycleRate() {
        rate = Self.rates[(Self.rates.firstIndex(of: rate).map { $0 + 1 } ?? 0) % Self.rates.count]
        player.rate = rate
    }
    func holdRate(_ value: Float?) { player.rate = value ?? rate }

    func stop() { savePosition(); player.stop() }

    private func savePosition() {
        guard let url = currentURL, player.position > 0.01 else { return }
        UserDefaults.standard.set(player.position, forKey: "pos:\(url.lastPathComponent)")
    }

    func mediaPlayerTimeChanged(_ notification: Notification) {
        if let seek = pendingSeek, player.isSeekable {
            player.position = seek
            pendingSeek = nil
        }
        if !scrubbing { position = player.position }
        isPlaying = player.isPlaying
        elapsed = Self.clock(player.time.intValue)
        if let length = player.media?.length.intValue, length > 0 { duration = Self.clock(length) }
        updateNowPlaying()
    }

    func mediaPlayerStateChanged(_ notification: Notification) {
        isPlaying = player.isPlaying
        updateNowPlaying()
        if player.state == .ended { onEnded?() }
    }

    private static func clock(_ ms: Int32) -> String {
        let total = Int(max(0, ms)) / 1000
        let s = total % 60, m = (total / 60) % 60, h = total / 3600
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, s) : String(format: "%d:%02d", m, s)
    }
}

/// The bare drawing surface libVLC renders into.
private struct VLCSurface: UIViewRepresentable {
    let controller: VLCController
    func makeUIView(context: Context) -> UIView {
        let view = UIView(); view.backgroundColor = .black
        controller.attach(to: view)
        return view
    }
    // Re-point the player at whichever surface is on screen. Without this the
    // full-screen surface (a different view) stayed black — the player was still
    // drawing into the windowed one.
    func updateUIView(_ uiView: UIView, context: Context) {
        controller.attach(to: uiView)
    }
}

/// A brief on-screen gauge for a side drag (brightness or sound).
private struct Gauge: View {
    let icon: String
    let value: Double
    var body: some View {
        VStack(spacing: 8) {
            Image(systemName: icon).font(.title2)
            ProgressView(value: value).frame(width: 90).tint(.white)
        }
        .padding(16).background(.ultraThinMaterial).cornerRadius(12).foregroundColor(.white)
    }
}

/// The player stage: windowed 16:9 above the list, or full screen. Carries the
/// Android gestures — tap toggles the bar, double-tap seeks by the third of the
/// picture, hold runs 2× on the right and rewinds on the left, a side drag sets
/// brightness (left) and sound (right) in full screen, a swipe up expands and
/// down collapses, left stops and right leaves to the web.
struct PlayerStage: View {
    @ObservedObject var controller: VLCController
    let title: String
    @Binding var fullscreen: Bool
    var onStop: () -> Void
    var onPullToWeb: () -> Void
    var onPrev: () -> Void
    var onNext: () -> Void
    var hasPrev: Bool
    var hasNext: Bool
    var isMusic: Bool = false

    @State private var showControls = true
    @State private var rewindTimer: Timer?
    @State private var hideWork: DispatchWorkItem?
    @State private var gauge: (icon: String, value: Double)?
    @State private var zoom: CGFloat = 1

    /// True while a brightness/sound drag is showing its gauge, so the collapse
    /// swipe does not fire mid-adjust.
    private var gaugeActive: Bool { gauge != nil }
    @State private var dragStartBrightness: CGFloat = 0
    @State private var dragStartVolume: Float = 0

    var body: some View {
        ZStack {
            Color.black
            VLCSurface(controller: controller)
                .scaleEffect(zoom)
                .gesture(
                    MagnificationGesture()
                        .onChanged { zoom = min(3, max(1, $0)) }
                        .onEnded { _ in if zoom < 1.1 { withAnimation { zoom = 1 } } }
                )
            if isMusic {
                Image(systemName: "music.note")
                    .font(.system(size: 64)).foregroundColor(.white.opacity(0.5))
            }
            thirds
            if let g = gauge { Gauge(icon: g.icon, value: g.value) }
            if showControls { controlsOverlay.transition(.opacity) }
        }
        .clipped()
        .simultaneousGesture(swipeGesture)
        .simultaneousGesture(sideDragGesture)
        .onChange(of: showControls) { shown in if shown { scheduleAutoHide() } }
        .onChange(of: fullscreen) { fs in applyOrientation(fs) }
        .onAppear { scheduleAutoHide() }
        .onDisappear { Orientation.shared.free() }
    }

    /// Full screen turns a wide video on its side, like the phone; a tall one
    /// (a short) stays upright. Leaving full screen frees rotation again.
    private func applyOrientation(_ fs: Bool) {
        guard fs else { Orientation.shared.free(); return }
        let size = controller.player.videoSize
        if size.width > size.height {
            Orientation.shared.lock(.landscapeRight, to: .landscapeRight)
        } else {
            Orientation.shared.lock(.portrait, to: .portrait)
        }
    }

    /// Fade the bar out after a few idle seconds, like the phone player.
    private func scheduleAutoHide() {
        hideWork?.cancel()
        let work = DispatchWorkItem { withAnimation { showControls = false } }
        hideWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 4, execute: work)
    }

    // Thirds, like the phone: double-tap the left/right third seeks ±3s (the
    // middle only toggles the bar); a hold runs 2× on the right and rewinds on
    // the left, and the middle is left alone.
    private var thirds: some View {
        HStack(spacing: 0) {
            column(seek: -3, hold: .rewind)
            column(seek: 0, hold: .none)
            column(seek: 3, hold: .fast)
        }
    }

    private enum Hold { case rewind, fast, none }

    private func column(seek: Int32, hold: Hold) -> some View {
        Color.clear
            .contentShape(Rectangle())
            .onTapGesture(count: 2) {
                if seek != 0 { controller.jump(seek) } else { withAnimation { showControls.toggle() } }
            }
            .onTapGesture { withAnimation { showControls.toggle() } }
            .onLongPressGesture(minimumDuration: 0.35, pressing: { pressing in
                switch hold {
                case .rewind: pressing ? startRewind() : stopRewind()
                case .fast: controller.holdRate(pressing ? 2.0 : nil)
                case .none: break
                }
            }, perform: {})
    }

    private func startRewind() {
        rewindTimer?.invalidate()
        rewindTimer = Timer.scheduledTimer(withTimeInterval: 0.15, repeats: true) { _ in
            controller.rewindStep()
        }
    }
    private func stopRewind() { rewindTimer?.invalidate(); rewindTimer = nil }

    // Only up and down on the picture: up to full screen, down to the window.
    // Left/right are gone — sideways is for brightness and sound, and leaving to
    // the web is a swipe on the list, not the picture.
    private var swipeGesture: some Gesture {
        DragGesture(minimumDistance: 40)
            .onEnded { v in
                guard abs(v.translation.height) > abs(v.translation.width) else { return }
                if v.translation.height < -50 { fullscreen = true }
                else if v.translation.height > 50, !gaugeActive { fullscreen = false }
            }
    }

    // A slow vertical drag sets sound on the right (always) and brightness on the
    // left (full screen only), with a gauge while it moves.
    private var sideDragGesture: some Gesture {
        DragGesture(minimumDistance: 8)
            .onChanged { v in
                guard abs(v.translation.height) > abs(v.translation.width) else { return }
                let onLeft = v.startLocation.x < UIScreen.main.bounds.width / 2
                if onLeft && !fullscreen { return }
                if gauge == nil {
                    dragStartBrightness = UIScreen.main.brightness
                    dragStartVolume = controller.volume
                }
                let delta = Float(-v.translation.height / 200)
                if onLeft {
                    let b = max(0, min(1, dragStartBrightness + CGFloat(delta)))
                    UIScreen.main.brightness = b
                    gauge = ("sun.max", Double(b))
                } else {
                    let vol = max(0, min(1, dragStartVolume + delta))
                    controller.volume = vol
                    gauge = ("speaker.wave.2", Double(vol))
                }
            }
            .onEnded { _ in
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { gauge = nil }
            }
    }

    private var controlsOverlay: some View {
        VStack {
            HStack {
                Text(title).font(.subheadline).foregroundColor(.white).lineLimit(1)
                    .shadow(radius: 2)
                Spacer()
                Button { fullscreen ? (fullscreen = false) : onStop() } label: {
                    Image(systemName: fullscreen ? "arrow.down.right.and.arrow.up.left" : "xmark")
                        .font(.subheadline.bold()).foregroundColor(.white)
                        .frame(width: 30, height: 30)
                        .background(Color.black.opacity(0.4))
                        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                }
            }
            .padding(10)
            Spacer()
            transport
        }
    }

    private var transport: some View {
        VStack(spacing: 6) {
            // Seek above the buttons, the way the phone bar is laid out.
            HStack(spacing: 8) {
                Text(controller.elapsed).font(.caption2).foregroundColor(.white).monospacedDigit()
                Slider(
                    value: Binding(
                        get: { Double(controller.position) },
                        set: { controller.previewSeek(Float($0)) }
                    ),
                    in: 0...1,
                    onEditingChanged: { editing in
                        editing ? controller.beginScrub() : controller.endScrub(controller.position)
                    }
                ).tint(.accent)
                Text(controller.duration).font(.caption2).foregroundColor(.white).monospacedDigit()
            }
            HStack(spacing: 18) {
                Button { onPrev() } label: { Image(systemName: "backward.end.fill") }
                    .disabled(!hasPrev)
                Button { controller.toggle() } label: {
                    Image(systemName: controller.isPlaying ? "pause.fill" : "play.fill").font(.title2)
                }
                Button { onNext() } label: { Image(systemName: "forward.end.fill") }
                    .disabled(!hasNext)
                Spacer()
                Button { controller.toggleMute() } label: {
                    Image(systemName: controller.muted ? "speaker.slash.fill" : "speaker.wave.2.fill")
                }
                Button { controller.cycleRate() } label: {
                    Text(String(format: "%g×", controller.rate)).font(.subheadline.bold())
                }
                Button { fullscreen.toggle() } label: {
                    Image(systemName: fullscreen ? "arrow.down.right.and.arrow.up.left"
                                                  : "arrow.up.left.and.arrow.down.right")
                }
            }
            .foregroundColor(.white)
        }
        .padding(10)
        .background(.ultraThinMaterial)
    }
}
