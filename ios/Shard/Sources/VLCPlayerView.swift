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

    /// The file currently loaded, so the library can put its stage back when you
    /// return to it.
    private(set) var currentURL: URL?
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

    func attach(to view: UIView) {
        // Only when it actually changed — re-pointing the drawable on every
        // SwiftUI update flickered the picture to black.
        if (player.drawable as? UIView) !== view { player.drawable = view }
    }

    func open(_ url: URL) {
        currentURL = url
        player.media = VLCMedia(url: url)
        player.play()
        player.rate = rate
    }

    func pause() { player.pause() }

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

    func stop() { player.stop(); currentURL = nil }

    func mediaPlayerTimeChanged(_ notification: Notification) {
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
    /// A brief double-tap flash: (rightward, seconds).
    @State private var seekFlash: (right: Bool, secs: Int)?

    private enum DragMode { case brightness, volume, swipe, none }
    @State private var dragMode: DragMode = .none
    @State private var startBrightness: CGFloat = 0
    @State private var startVolume: Float = 0

    var body: some View {
        ZStack {
            Color.black
            VLCSurface(controller: controller).scaleEffect(zoom)
            if isMusic {
                Image(systemName: "music.note").font(.system(size: 64)).foregroundColor(.white.opacity(0.4))
            }
            PlayerGestures(
                onTap: { toggleBar() },
                onDoubleTap: { x, w in doubleTap(x, w) },
                onHold: { active, x, w in hold(active, x, w) },
                onVerticalDrag: { phase, sx, w, dy in vdrag(phase, sx, w, dy) }
            )
            .gesture(
                MagnificationGesture()
                    .onChanged { zoom = min(3, max(1, $0)) }
                    .onEnded { _ in if zoom < 1.1 { withAnimation { zoom = 1 } } }
            )
            if let f = seekFlash { seekFlashView(f) }
            if let g = gauge { Gauge(icon: g.icon, value: g.value) }
            if showControls { controlsOverlay.transition(.opacity) }
        }
        .clipped()
        .onAppear { showBar() }
    }

    /// Tap toggles the bar — up if hidden, away if shown.
    private func toggleBar() {
        if showControls {
            hideWork?.cancel()
            withAnimation { showControls = false }
        } else {
            showBar()
        }
    }

    // MARK: gesture handlers

    private func doubleTap(_ x: CGFloat, _ w: CGFloat) {
        let third = w / 3
        if x < third {
            controller.jump(-3); flash(right: false)
        } else if x > third * 2 {
            controller.jump(3); flash(right: true)
        } else {
            showBar()
        }
    }

    private func flash(right: Bool) {
        seekFlash = (right, 3)
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { seekFlash = nil }
    }

    private func hold(_ active: Bool, _ x: CGFloat, _ w: CGFloat) {
        if active { showBar() }
        if x > w / 2 {
            controller.holdRate(active ? 2.0 : nil)       // right: 2×
        } else {
            active ? startRewind() : stopRewind()          // left: rewind
        }
    }

    private func vdrag(_ phase: PlayerGestures.Phase, _ startX: CGFloat, _ w: CGFloat, _ dy: CGFloat) {
        switch phase {
        case .began:
            if startX < w * 0.2 && fullscreen {
                dragMode = .brightness; startBrightness = UIScreen.main.brightness
            } else if startX > w * 0.8 {
                dragMode = .volume; startVolume = controller.volume
            } else {
                dragMode = .swipe
            }
        case .changed:
            let step = Float(-dy / 260)                     // from the current value
            if dragMode == .brightness {
                let b = max(0, min(1, startBrightness + CGFloat(step)))
                UIScreen.main.brightness = b; gauge = ("sun.max", Double(b))
            } else if dragMode == .volume {
                let v = max(0, min(1, startVolume + step)); controller.volume = v
                gauge = ("speaker.wave.2", Double(v))
            }
        case .ended:
            if dragMode == .swipe {
                if dy < -60 { fullscreen = true }
                else if dy > 60 { fullscreen = false }
            } else {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { gauge = nil }
            }
            dragMode = .none
        }
    }

    private func startRewind() {
        rewindTimer?.invalidate()
        rewindTimer = Timer.scheduledTimer(withTimeInterval: 0.15, repeats: true) { _ in
            controller.rewindStep()
        }
    }
    private func stopRewind() { rewindTimer?.invalidate(); rewindTimer = nil }

    /// Show the bar and keep it up for three idle seconds, refreshed by any
    /// interaction that calls this.
    private func showBar() {
        withAnimation { showControls = true }
        hideWork?.cancel()
        let work = DispatchWorkItem { withAnimation { showControls = false } }
        hideWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 3, execute: work)
    }

    // MARK: overlays

    /// A YouTube-style double-tap cue: a triangle with "3s" inside a translucent
    /// circle, on the side that was tapped.
    private func seekFlashView(_ f: (right: Bool, secs: Int)) -> some View {
        HStack {
            if f.right { Spacer() }
            ZStack {
                Circle().fill(Color.black.opacity(0.45)).frame(width: 74, height: 74)
                VStack(spacing: 2) {
                    Image(systemName: f.right ? "forward.fill" : "backward.fill").font(.title3)
                    Text("\(f.secs)초").font(.caption2)
                }.foregroundColor(.white)
            }
            .padding(.horizontal, 40)
            if !f.right { Spacer() }
        }
    }

    private var controlsOverlay: some View {
        VStack {
            HStack {
                Text(title).font(.subheadline).foregroundColor(.white).lineLimit(1).shadow(radius: 2)
                Spacer()
                Button { fullscreen ? (fullscreen = false) : onStop() } label: {
                    Image(systemName: fullscreen ? "arrow.down.right.and.arrow.up.left" : "xmark")
                        .font(.system(size: 15, weight: .bold)).foregroundColor(.white)
                        .frame(width: 30, height: 30)
                        .overlay(Circle().stroke(Color.black.opacity(0.55), lineWidth: 1.5))
                }
            }
            .padding(10)
            Spacer()
            transport
        }
    }

    private var transport: some View {
        VStack(spacing: 8) {
            // Seek, with the sound control to its right.
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
                        showBar()
                    }
                ).tint(.accent)
                Text(controller.duration).font(.caption2).foregroundColor(.white).monospacedDigit()
                Button { controller.toggleMute() } label: {
                    Image(systemName: controller.muted ? "speaker.slash.fill" : "speaker.wave.2.fill")
                        .font(.subheadline)
                }
            }
            // Speed at the far left; transport centred; full-screen at the right.
            HStack(spacing: 22) {
                Button { controller.cycleRate() } label: {
                    Text(String(format: "%g×", controller.rate)).font(.subheadline.bold())
                }
                Spacer()
                Button { onPrev() } label: { Image(systemName: "backward.end.fill") }.disabled(!hasPrev)
                Button { controller.toggle() } label: {
                    Image(systemName: controller.isPlaying ? "pause.fill" : "play.fill").font(.title3)
                }
                Button { onNext() } label: { Image(systemName: "forward.end.fill") }.disabled(!hasNext)
                Spacer()
                Button { fullscreen.toggle() } label: {
                    Image(systemName: fullscreen ? "arrow.down.right.and.arrow.up.left"
                                                  : "arrow.up.left.and.arrow.down.right")
                }
            }
            .foregroundColor(.white)
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .background(.ultraThinMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .padding(.horizontal, 8).padding(.bottom, 8)
    }
}
