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
    /// Set when the stream reaches its end. A finished libVLC player will not
    /// resume on play() — its state may read .ended or .stopped — so the replay
    /// path keys off this flag rather than guessing the state.
    private var reachedEnd = false
    var onEnded: (() -> Void)?
    /// Lock-screen / headset next & previous reach the playlist through these.
    var onRemoteNext: (() -> Void)?
    var onRemotePrev: (() -> Void)?
    var nowPlayingTitle = ""

    func toggleMute() {
        muted.toggle()
        player.audio?.isMuted = muted
    }

    /// The one surface VLC draws into, reparented between windowed and full
    /// screen so its render layer is never torn down.
    lazy var hostView: PlayerHostView = {
        let v = PlayerHostView(); v.controller = self; v.backgroundColor = .black
        return v
    }()

    /// The file currently loaded, so the library can put its stage back when you
    /// return to it. Published so the stage appears the instant playback starts —
    /// which lets it travel with the library's slide-in instead of popping in.
    @Published private(set) var currentURL: URL?
    private static let rates: [Float] = [1, 1.25, 1.5, 2, 0.5, 0.75]
    /// A seek to apply once the media is playing again — used to resume at the
    /// same spot after a phone-call interruption reloads the file.
    private var pendingSeek: Float?
    private var interruptedAt: Float?

    override init() {
        super.init()
        player.delegate = self
        let session = AVAudioSession.sharedInstance()
        try? session.setCategory(.playback, mode: .moviePlayback)
        try? session.setActive(true)
        setupRemoteCommands()
        // A phone call (or any interruption) pauses us and, on iOS, tears the
        // audio route down. libVLC did not recover on its own — playback stayed
        // frozen and no later file would start — so on the interruption's end we
        // reload the file at the same spot and reactivate the session ourselves.
        NotificationCenter.default.addObserver(
            self, selector: #selector(handleInterruption(_:)),
            name: AVAudioSession.interruptionNotification, object: nil)
    }

    @objc private func handleInterruption(_ note: Notification) {
        guard let info = note.userInfo,
              let raw = info[AVAudioSessionInterruptionTypeKey] as? UInt,
              let type = AVAudioSession.InterruptionType(rawValue: raw) else { return }
        switch type {
        case .began:
            interruptedAt = player.position
        case .ended:
            try? AVAudioSession.sharedInstance().setActive(true)
            // Reload from the remembered spot: a plain play() left the player
            // wedged after the call ended.
            if let url = currentURL {
                let at = interruptedAt ?? player.position
                pendingSeek = at
                open(url)
            }
            interruptedAt = nil
        @unknown default: break
        }
    }

    deinit {
        // Without this a controller that goes out of view kept its player alive
        // and playing in the background — pick another file and two (then three)
        // were heard at once. Stop it, and drop the remote-command handlers so
        // the lock screen does not talk to a dead player.
        player.stop()
        NotificationCenter.default.removeObserver(self)
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
        reachedEnd = false
        // Stop before loading new media, always. A player left in .ended (or an
        // error) state wedged when handed new media — the replayed file, and
        // then every file after it, refused to start until the app was killed.
        // A clean stop first is what makes reuse reliable.
        player.stop()
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
        // Seeking to exactly 1.0 lands on end-of-stream, which VLC treats as
        // "finished" and snaps back — cap just short so the far end is reachable.
        player.position = max(0, min(0.999, p))
    }

    func toggle() {
        // A finished player will not resume on play(); reloading the file is the
        // reliable way to replay from the start.
        if reachedEnd || player.state == .ended || (!player.isPlaying && player.position > 0.995) {
            if let url = currentURL { open(url) }
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
    func setRate(_ r: Float) { rate = r; player.rate = r }
    func holdRate(_ value: Float?) { player.rate = value ?? rate }

    func stop() { player.stop(); currentURL = nil }

    func mediaPlayerTimeChanged(_ notification: Notification) {
        // Apply a resume-seek once the reloaded file is actually running.
        if let p = pendingSeek, player.isPlaying, player.position > 0 {
            player.position = p; pendingSeek = nil
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
        if player.state == .ended { reachedEnd = true; onEnded?() }
    }

    private static func clock(_ ms: Int32) -> String {
        let total = Int(max(0, ms)) / 1000
        let s = total % 60, m = (total / 60) % 60, h = total / 3600
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, s) : String(format: "%d:%02d", m, s)
    }
}

/// The drawing surface libVLC renders into. It attaches the player once it has a
/// real size — attaching at make-time (a zero-size view) left the picture black.
final class PlayerHostView: UIView {
    weak var controller: VLCController?
    private var attached = false
    override func layoutSubviews() {
        super.layoutSubviews()
        if !attached, bounds.width > 1, bounds.height > 1 {
            controller?.attach(to: self)
            attached = true
        }
    }
}

/// Reparents the controller's ONE persistent host view into whatever container
/// SwiftUI makes (windowed or full screen). Moving the same view keeps VLC's
/// render layer alive — recreating the surface on every window/full-screen swap
/// is what turned the picture black.
private struct VLCSurface: UIViewRepresentable {
    let controller: VLCController
    func makeUIView(context: Context) -> UIView {
        let container = UIView()
        container.backgroundColor = .black
        let host = controller.hostView
        host.translatesAutoresizingMaskIntoConstraints = false
        host.removeFromSuperview()
        container.addSubview(host)
        NSLayoutConstraint.activate([
            host.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            host.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            host.topAnchor.constraint(equalTo: container.topAnchor),
            host.bottomAnchor.constraint(equalTo: container.bottomAnchor),
        ])
        return container
    }
    func updateUIView(_ uiView: UIView, context: Context) {}
}

/// A slim seek bar with a small round handle — SwiftUI's Slider thumb cannot be
/// resized, and its default read as an oversized oval.
struct SeekSlider: View {
    @Binding var value: Double            // 0…1
    var onBegin: () -> Void
    var onScrub: (Double) -> Void
    var onEnd: (Double) -> Void
    @State private var dragging = false

    var body: some View {
        GeometryReader { geo in
            let w = max(geo.size.width, 1)
            ZStack(alignment: .leading) {
                Capsule().fill(Color.white.opacity(0.3)).frame(height: 3)
                Capsule().fill(Color.accent).frame(width: w * CGFloat(value), height: 3)
                Circle().fill(Color.accent)
                    .frame(width: 9, height: 9)
                    .offset(x: w * CGFloat(value) - 4.5)
            }
            .frame(maxHeight: .infinity)
            .contentShape(Rectangle())
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { g in
                        if !dragging { dragging = true; onBegin() }
                        onScrub(min(1, max(0, Double(g.location.x / w))))
                    }
                    .onEnded { g in
                        dragging = false
                        onEnd(min(1, max(0, Double(g.location.x / w))))
                    }
            )
        }
        .frame(height: 22)
    }
}

/// A brief on-screen gauge for a side drag (brightness or sound).
private struct Gauge: View {
    let icon: String
    let value: Double
    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: icon).font(.system(size: 13))
            Capsule().fill(Color.white.opacity(0.3)).frame(width: 70, height: 3)
                .overlay(alignment: .leading) {
                    Capsule().fill(Color.white).frame(width: 70 * max(0, min(1, value)), height: 3)
                }
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .background(Color.black.opacity(0.4)).clipShape(Capsule()).foregroundColor(.white)
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
    @State private var holdVolume = false
    @State private var showRatePicker = false
    @State private var showVolumeBar = false
    /// The volume bar's shown value, kept locally so the thumb follows the finger
    /// instantly instead of waiting on libVLC's slower read-back.
    @State private var volDisplay: Double = 0
    /// True while a bar control is being dragged, so the 3-second auto-hide does
    /// not pull the bar away mid-adjustment.
    @State private var interacting = false

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
        .onChange(of: fullscreen) { fs in applyOrientation(fs) }
        // When playback stops or reaches the end, surface the bar so the play
        // button is right there — otherwise a finished video sat with no controls.
        .onChange(of: controller.isPlaying) { playing in if !playing { showBar() } }
        .onAppear { showBar() }
        .onDisappear { Orientation.shared.free() }
    }

    /// Full screen lays a wide video on its side; a tall one (a short) stays up.
    /// Leaving full screen turns back to portrait, then frees rotation again.
    private func applyOrientation(_ fs: Bool) {
        if fs {
            // Default to landscape at once (most videos are wide). videoSize is
            // often 0 the instant full screen opens, so decide again a beat later
            // once a frame has arrived — only a clearly tall video (a short) is
            // flipped back to portrait.
            let decide = {
                let s = controller.player.videoSize
                if s.height > s.width, s.width > 0 {
                    Orientation.shared.lock(.portrait, to: .portrait)
                } else {
                    Orientation.shared.lock(.landscapeRight, to: .landscapeRight)
                }
            }
            Orientation.shared.lock(.landscapeRight, to: .landscapeRight)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.35, execute: decide)
        } else {
            Orientation.shared.lock(.portrait, to: .portrait)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { Orientation.shared.free() }
        }
    }

    /// Tap toggles the bar — up if hidden, away if shown.
    private func toggleBar() {
        // A tap while a picker is open just dismisses the picker (and keeps the
        // bar) — it should not take a second tap, nor hide the whole bar.
        if showRatePicker || showVolumeBar {
            showRatePicker = false; showVolumeBar = false; showBar(); return
        }
        if showControls {
            hideWork?.cancel()
            withAnimation(.easeOut(duration: 0.12)) { showControls = false }
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
        // Windowed: a hold brings up the sound, adjusted by moving the finger up
        // and down (see the drag handler). Full screen keeps the 2×/rewind hold.
        if !fullscreen {
            holdVolume = active
            if active { startVolume = controller.volume }
            else { DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { gauge = nil } }
            return
        }
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
            let step = Float(-dy / 160)                     // from the current value
            // A windowed hold turns the finger's up/down into volume anywhere —
            // a shorter reach than brightness so it responds quickly.
            if holdVolume {
                let v = max(0, min(1, startVolume + Float(-dy / 100)))
                controller.volume = v
                gauge = ("speaker.wave.2", Double(v)); return
            }
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
        withAnimation(.easeOut(duration: 0.12)) { showControls = true }
        hideWork?.cancel()
        let work = DispatchWorkItem {
            // Do not hide while a control is still under the finger.
            if interacting { return }
            withAnimation { showControls = false }
        }
        hideWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 3, execute: work)
    }

    /// Keep the bar up without arming the auto-hide — used while dragging a
    /// control, so it stays until the drag ends and showBar() re-arms the timer.
    private func keepBar() {
        hideWork?.cancel()
        withAnimation(.easeOut(duration: 0.12)) { showControls = true }
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

    /// The top safe-area inset, so the title clears the status bar / notch when a
    /// short is played full screen (portrait) — otherwise it ran into the clock
    /// and battery.
    private var topInset: CGFloat {
        UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            .first(where: { $0.activationState == .foregroundActive })?
            .keyWindow?.safeAreaInsets.top ?? 0
    }

    private var controlsOverlay: some View {
        VStack(spacing: 0) {
            HStack {
                Text(title).font(.subheadline).foregroundColor(.white).lineLimit(1).shadow(radius: 2)
                Spacer()
                closeButton
            }
            .padding(.horizontal, 12)
            .padding(.top, fullscreen ? topInset + 6 : 10)
            .padding(.bottom, 8)
            Spacer()
            // Right-aligned and flush to the transport (spacing 0) so a picker
            // reads as attached right above the sound / speed buttons it belongs to.
            VStack(alignment: .trailing, spacing: 0) {
                if showRatePicker { ratePicker }
                if showVolumeBar { volumeBar }
                transport
            }
        }
    }

    /// A white ✕ with only a hairline dark edge on the glyph itself.
    private var closeButton: some View {
        Button { fullscreen ? (fullscreen = false) : onStop() } label: {
            Image(systemName: fullscreen ? "arrow.down.right.and.arrow.up.left" : "xmark")
                .font(.system(size: 17, weight: .semibold))
                .foregroundColor(.white)
                .shadow(color: .black.opacity(0.5), radius: 0.5)
                .frame(width: 30, height: 30)
        }
    }

    private var ratePicker: some View {
        HStack(spacing: 6) {
            ForEach([Float(0.5), 0.75, 1, 1.25, 1.5, 2], id: \.self) { r in
                Button { controller.setRate(r); showRatePicker = false; keepBar() } label: {
                    Text(String(format: "%g×", r)).font(.system(size: 13, weight: .semibold))
                        .padding(.horizontal, 10).padding(.vertical, 6)
                        .background(controller.rate == r ? Color.accent : Color.clear)
                        .foregroundColor(controller.rate == r ? .onAccent : .white)
                        .clipShape(Capsule())
                }
            }
        }
        .padding(6).background(Color.chrome).clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .padding(.trailing, 12).padding(.bottom, 6)
    }

    private var volumeBar: some View {
        HStack(spacing: 10) {
            Image(systemName: "speaker.fill").font(.system(size: 12)).foregroundColor(.white)
            SeekSlider(
                // Local display for an instant thumb; shows 0 while muted, and any
                // drag lifts the mute so the two never disagree.
                value: Binding(get: { controller.muted ? 0 : volDisplay }, set: { _ in }),
                onBegin: { interacting = true; keepBar() },
                onScrub: { v in
                    if controller.muted { controller.toggleMute() }
                    volDisplay = v; controller.volume = Float(v); keepBar()
                },
                onEnd: { _ in interacting = false; showBar() })
            Image(systemName: "speaker.wave.3.fill").font(.system(size: 12)).foregroundColor(.white)
        }
        .frame(width: 220)
        .padding(.horizontal, 14).padding(.vertical, 8)
        .background(Color.chrome).clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .padding(.trailing, 12).padding(.bottom, 6)
    }

    private var transport: some View {
        VStack(spacing: 10) {
            // Top row: the seek bar only.
            HStack(spacing: 6) {
                // Fixed width so the seek bar does not grow/shrink as the elapsed
                // time gains or loses a digit (0:09 → 0:10 → 1:00:00).
                Text(controller.elapsed).font(.system(size: 10)).foregroundColor(.white)
                    .monospacedDigit().frame(width: 46, alignment: .trailing)
                SeekSlider(
                    value: Binding(get: { Double(controller.position) },
                                   set: { controller.previewSeek(Float($0)) }),
                    onBegin: { controller.beginScrub(); interacting = true; keepBar() },
                    onScrub: { controller.previewSeek(Float($0)); keepBar() },
                    onEnd: { controller.endScrub(Float($0)); interacting = false; showBar() }
                )
                Text(controller.duration).font(.system(size: 10)).foregroundColor(.white)
                    .monospacedDigit().frame(width: 46, alignment: .leading)
            }
            // Bottom row: transport on the left (wide), sound/speed/full-screen right.
            HStack {
                HStack(spacing: 28) {
                    Button { onPrev() } label: { Image(systemName: "backward.end.fill").font(.system(size: 15)) }.disabled(!hasPrev)
                    Button { controller.toggle() } label: {
                        Image(systemName: controller.isPlaying ? "pause.fill" : "play.fill").font(.system(size: 19))
                    }
                    Button { onNext() } label: { Image(systemName: "forward.end.fill").font(.system(size: 15)) }.disabled(!hasNext)
                }
                .padding(.leading, 10)      // off the very left edge — the back button was hard to hit
                Spacer()
                HStack(spacing: 20) {
                    Button { controller.toggleMute() } label: {
                        Image(systemName: controller.muted ? "speaker.slash.fill" : "speaker.wave.2.fill").font(.system(size: 15))
                    }
                    // highPriority, not simultaneous: when the hold is recognized
                    // it cancels the button's tap, so releasing after a hold does
                    // not also mute / step the rate. A short tap still fails the
                    // long press and runs the button normally.
                    .highPriorityGesture(LongPressGesture(minimumDuration: 0.3).onEnded { _ in
                        volDisplay = Double(controller.volume)
                        showVolumeBar.toggle(); showRatePicker = false; keepBar()
                    })
                    Button { controller.cycleRate() } label: {
                        // Fixed width so 1× ↔ 1.25× does not shove the buttons
                        // beside it as the rate changes.
                        Text(String(format: "%g×", controller.rate)).font(.system(size: 14, weight: .bold))
                            .frame(width: 42)
                    }
                    .highPriorityGesture(LongPressGesture(minimumDuration: 0.3).onEnded { _ in
                        showRatePicker.toggle(); showVolumeBar = false; keepBar()
                    })
                    Button { fullscreen.toggle() } label: {
                        Image(systemName: fullscreen ? "arrow.down.right.and.arrow.up.left"
                                                      : "arrow.up.left.and.arrow.down.right").font(.system(size: 15))
                    }
                }
                .padding(.trailing, 10)     // off the very right edge, to match the left cluster
            }
            .foregroundColor(.white)
        }
        .padding(.horizontal, 12).padding(.top, 8)
        .padding(.bottom, fullscreen ? 30 : 16)   // room under the buttons; more in full screen for a short's edge
        .background(Color.black.opacity(0.3))
    }
}
