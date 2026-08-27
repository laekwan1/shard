import SwiftUI
import AVFoundation
import MediaPlayer
import MobileVLCKit

/// Drives one libVLC player and publishes what the controls need. libVLC plays
/// everything the engine writes — mp4, and the mkv/webm (VP9/Opus) AVPlayer
/// refuses — so the library needs one player, not a per-format guess.
/// The fast-changing playback numbers, split off the controller so the seek bar
/// can observe them without the whole library re-rendering four times a second
/// (which made the folder dialogs flicker). Only the thin seek row watches this.
final class PlayerUI: ObservableObject {
    @Published var position: Float = 0
    @Published var elapsed = "0:00"
    @Published var duration = "0:00"
}

final class VLCController: NSObject, ObservableObject, VLCMediaPlayerDelegate {
    // Bluetooth crackle on every FRESH libVLC audio output (track change / seek /
    // replay) while an Apple Watch adds jitter to the A2DP link: a continuously-running
    // output stays clean, a re-primed one crackles as libVLC continuously micro-corrects
    // its output clock. Two levers, both best-effort (ignored if a module is absent):
    //   --no-audio-time-stretch : stop the pitch-preserving stretcher from continuously
    //                             resampling to chase the jittery clock.
    //   --audio-resampler=speex_resampler : a decent bundled resampler instead of the
    //                             default "ugly" linear one (SoXR was ignored — likely
    //                             not in this MobileVLCKit build).
    let player = VLCMediaPlayer(options: ["--no-audio-time-stretch",
                                          "--audio-resampler=speex_resampler"])
    let ui = PlayerUI()

    // Two backends: AVPlayer for the formats it can open (mp4/mov/m4a/mp3/…),
    // which — unlike libVLC — starts without the hardware "텁" pop; libVLC for
    // everything AVPlayer cannot (mkv/webm, VP9/Opus). The controls call the same
    // methods; each one branches on `backend`.
    enum Backend { case vlc, av }
    private(set) var backend: Backend = .vlc
    let av = AVPlayer()
    private var avTimeObs: Any?
    private var avEndObs: NSObjectProtocol?
    private var avStatusObs: NSKeyValueObservation?
    private var avControlObs: NSKeyValueObservation?
    private static let avExts: Set<String> =
        ["mp4", "m4v", "mov", "m4a", "mp3", "aac", "wav", "caf", "aif", "aiff"]
    private func prefersAV(_ url: URL) -> Bool { Self.avExts.contains(url.pathExtension.lowercased()) }

    /// The playing video's pixel size, for the library's fullscreen orientation
    /// decision — from whichever backend is in use.
    var videoSize: CGSize {
        backend == .av ? (av.currentItem?.presentationSize ?? .zero) : player.videoSize
    }
    /// Forwarders so the rest of the code reads/writes these as before, but the
    /// publishing happens on `ui`, not on the controller the library observes.
    var position: Float { get { ui.position } set { ui.position = newValue } }
    var elapsed: String { get { ui.elapsed } set { ui.elapsed = newValue } }
    var duration: String { get { ui.duration } set { ui.duration = newValue } }
    @Published var isPlaying = false
    @Published var rate: Float = 1
    @Published var muted = false
    /// True while the stream is opening/buffering, so the stage can show a spinner
    /// in the middle instead of a blank picture.
    @Published var buffering = false
    /// True briefly while the interface is rotating for full screen, so the stage
    /// can hide the (squishing) video behind black until the rotation settles.
    @Published var settling = false
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
        av.isMuted = muted
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
    /// Whether the player was genuinely playing (not intentionally paused) when an
    /// audio interruption began — gates the auto-resume on its end.
    private var wasPlayingAtInterruption = false
    /// Set when playback was paused on purpose (by the user, or by us for a web
    /// video), so an interruption's end does not resume it behind their back.
    private var userPaused = false
    private var tick = 0
    private var openGen = 0
    /// True from open() until the first frame of the new stream plays, so the audio
    /// stays muted across the swap glitch and comes on exactly with the picture.
    private var pendingUnmute = false
    /// Video-only brightness (1 = full), kept on the controller so it survives the
    /// stage view being replaced when it goes full screen and back.
    @Published var brightness: Double = 1

    override init() {
        super.init()
        player.delegate = self
        let session = AVAudioSession.sharedInstance()
        // .allowBluetoothA2DP makes the music-quality Bluetooth profile explicit and
        // never asks for the call profile (HFP), so a call that flipped the earbuds to
        // HFP is more likely to fall back to A2DP for us rather than stay crackly.
        try? session.setCategory(.playback, mode: .default, options: [.allowBluetoothA2DP])
        // Match the hardware sample rate AND the IO buffer to the OUTPUT device (see
        // the method) instead of always pinning 48k / a tight buffer — the pin crackled
        // over Bluetooth and the tight buffer underran over a jittery BT link.
        configureForCurrentRoute()
        try? session.setActive(true)
        setupRemoteCommands()
        // Re-match the rate whenever the route changes — the moment earbuds connect is
        // when we must adopt their rate, or the next track restart crackles.
        NotificationCenter.default.addObserver(
            self, selector: #selector(handleRouteChange(_:)),
            name: AVAudioSession.routeChangeNotification, object: nil)
        // A phone call (or any interruption) pauses us and, on iOS, tears the
        // audio route down. libVLC did not recover on its own — playback stayed
        // frozen and no later file would start — so on the interruption's end we
        // reload the file at the same spot and reactivate the session ourselves.
        NotificationCenter.default.addObserver(
            self, selector: #selector(handleInterruption(_:)),
            name: AVAudioSession.interruptionNotification, object: nil)
    }

    /// Match the audio session's preferred rate to the OUTPUT device so a per-track
    /// audio-unit restart never resamples. Forcing 48kHz is right for the built-in
    /// speaker and wired output — it stops the 44.1k↔48k route "텁" between tracks —
    /// but a Bluetooth A2DP link runs at its own rate (often 44.1kHz), and that same
    /// pin made every track change / stop-restart crackle (지지직): the restart
    /// renegotiated 48k against the BT link and glitched. On a BT route we instead
    /// adopt the rate the link already settled on. This is why starting a song and
    /// THEN connecting the earbuds sounded clean — now later tracks keep that rate too.
    /// Whether the last configure saw a Bluetooth output. Used to skip touching the
    /// LIVE session when nothing relevant changed.
    private var configuredForBluetooth: Bool?

    private func configureForCurrentRoute() {
        let session = AVAudioSession.sharedInstance()
        let bt: Set<AVAudioSession.Port> = [.bluetoothA2DP, .bluetoothLE, .bluetoothHFP]
        let onBluetooth = session.currentRoute.outputs.contains { bt.contains($0.portType) }
        // Only reconfigure when the output actually crossed the BT boundary. An Apple
        // Watch connecting to CONTROL playback — and, worse, a Watch workout — fires a
        // storm of route-change pings that do NOT change the output; setting the
        // preferred rate/buffer on the running session for each one glitched the audio.
        // (The 지지직 during a workout is mostly the Watch↔phone link saturating the one
        // shared Bluetooth radio that A2DP earbuds also use — RF contention the app
        // cannot remove — but at least we stop adding our own glitch on top.)
        if configuredForBluetooth == onBluetooth { return }
        configuredForBluetooth = onBluetooth
        // Always request 48kHz — NOT the BT link's 44.1kHz. Our music is opus, which
        // ALWAYS decodes at 48kHz; if the session runs at 44.1k, libVLC opens its audio
        // unit at 44.1k and does its own 48k→44.1k resample, whose default resampler
        // crackled over Bluetooth on every fresh track. At 48k libVLC needs no resample
        // and CoreAudio converts 48k→the BT link cleanly. (This is why a song already
        // playing when the earbuds connect stayed clean — its unit was built at 48k —
        // while the next track, rebuilt at the 44.1k I had adopted, crackled.)
        try? session.setPreferredSampleRate(48000)
        // A jittery BT link (worst when relayed via the Watch) underran a tight buffer
        // and crackled; give BT a roomier buffer, keep the tight one for wired/built-in.
        try? session.setPreferredIOBufferDuration(onBluetooth ? 0.04 : 0.02)
    }

    @objc private func handleRouteChange(_ note: Notification) {
        routeChangeCount += 1
        configureForCurrentRoute()
        updateDiagnostics()
    }

    // MARK: audio diagnostics (temporary, on-screen readout)
    // A live readout of the audio session so a crackle can be diagnosed on the device
    // without a Mac console: what route is in use, at what rate/buffer, how many
    // channels, which backend, whether another app holds audio, and how many
    // route-change / interruption events fired.
    @Published var diagRoute = ""
    @Published var diagDetail = ""
    private var routeChangeCount = 0
    private var interruptionCount = 0

    func updateDiagnostics() {
        let s = AVAudioSession.sharedInstance()
        let out = s.currentRoute.outputs.first
        let name: String
        switch out?.portType {
        case .some(.bluetoothA2DP): name = "BT-A2DP"
        case .some(.bluetoothHFP):  name = "BT-HFP"
        case .some(.bluetoothLE):   name = "BT-LE"
        case .some(.headphones):    name = "Wired"
        case .some(.builtInSpeaker): name = "Speaker"
        case .some(.airPlay):       name = "AirPlay"
        case .some(.carAudio):      name = "Car"
        default: name = out?.portType.rawValue ?? "?"
        }
        // Assign only on change — these are observed by the whole library, and a
        // 4×/sec publish would re-render it (the flicker we work to avoid elsewhere).
        let r = "\(name)  \(Int(s.sampleRate))Hz  \(Int(s.ioBufferDuration * 1000))ms  ch\(s.outputNumberOfChannels)"
        let d = "\(backend == .av ? "AV" : "VLC")  other:\(s.isOtherAudioPlaying ? "Y" : "N")  rc:\(routeChangeCount)  int:\(interruptionCount)"
        if r != diagRoute { diagRoute = r }
        if d != diagDetail { diagDetail = d }
    }

    @objc private func handleInterruption(_ note: Notification) {
        guard let info = note.userInfo,
              let raw = info[AVAudioSessionInterruptionTypeKey] as? UInt,
              let type = AVAudioSession.InterruptionType(rawValue: raw) else { return }
        interruptionCount += 1
        switch type {
        case .began:
            interruptedAt = player.position
            wasPlayingAtInterruption = isPlaying && !userPaused
            updateDiagnostics()
        case .ended:
            let session = AVAudioSession.sharedInstance()
            // Only the post-phone-CALL case needs the heavy renegotiation: a call can
            // leave the earbuds stuck on the call profile (HFP, 16kHz) and libVLC
            // wedged, so there a deactivate→reactivate cycle kicks them back to A2DP.
            // But doing that teardown on EVERY interruption end was itself the glitch —
            // an Apple Watch (control or workout) fires interruptions with the route
            // still fine, and tearing the session down/up each time crackled. So gate
            // the heavy path on the route ACTUALLY being HFP; otherwise just reactivate
            // and resume in place.
            let onHFP = session.currentRoute.outputs.contains { $0.portType == .bluetoothHFP }
            if onHFP {
                try? session.setActive(false, options: .notifyOthersOnDeactivation)
                try? session.setCategory(.playback, mode: .default, options: [.allowBluetoothA2DP])
                configuredForBluetooth = nil            // force the rate/buffer re-apply
                configureForCurrentRoute()
            }
            try? session.setActive(true)
            updateDiagnostics()
            // Only auto-resume if we were actually playing when interrupted AND had
            // not intentionally paused. A web video playing over us fires this pair
            // too — without the guard, pausing the library for a web video and then
            // stopping it (or locking the screen) resumed playback on its own.
            guard wasPlayingAtInterruption, !userPaused else { interruptedAt = nil; return }
            if backend == .av {
                avPlay()
            } else if onHFP, let url = currentURL {
                // A call wedges libVLC — reload from the remembered spot. A light
                // interruption does not, so a plain resume avoids the glitchy reload.
                pendingSeek = interruptedAt ?? player.position
                open(url)
            } else {
                player.play()
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
        teardownAV()
        NotificationCenter.default.removeObserver(self)
        let center = MPRemoteCommandCenter.shared()
        for c in [center.playCommand, center.pauseCommand, center.togglePlayPauseCommand,
                  center.nextTrackCommand, center.previousTrackCommand, center.changePlaybackPositionCommand] {
            c.removeTarget(nil)
        }
    }

    /// Whether a file is loaded, so the library can show the stage again after
    /// coming back from the background instead of losing it.
    var hasMedia: Bool { backend == .av ? av.currentItem != nil : player.media != nil }

    /// Lock screen and headset controls, so a video listened to like music is
    /// controlled like music.
    private func setupRemoteCommands() {
        let center = MPRemoteCommandCenter.shared()
        center.playCommand.addTarget { [weak self] _ in self?.resume(); return .success }
        center.pauseCommand.addTarget { [weak self] _ in self?.pause(); return .success }
        center.togglePlayPauseCommand.addTarget { [weak self] _ in self?.toggle(); return .success }
        center.nextTrackCommand.addTarget { [weak self] _ in self?.onRemoteNext?(); return .success }
        center.previousTrackCommand.addTarget { [weak self] _ in self?.onRemotePrev?(); return .success }
        center.changePlaybackPositionCommand.addTarget { [weak self] event in
            guard let self = self, let e = event as? MPChangePlaybackPositionCommandEvent else { return .commandFailed }
            if self.backend == .av {
                self.av.seek(to: CMTime(seconds: e.positionTime, preferredTimescale: 600))
            } else if let length = self.player.media?.length.intValue, length > 0 {
                self.seek(to: Float(e.positionTime * 1000 / Double(length)))
            } else { return .commandFailed }
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
        userPaused = false
        buffering = true
        reachedEnd = false
        position = 0          // jump the bar to the start at once, not a beat later
        openGen += 1
        // Reclaim the audio session from whatever held it — notably Apple Music, which
        // an Apple Watch keeps bound as the Now Playing app. Activating our non-mixing
        // .playback session interrupts it so we OWN the output outright; playing while
        // Music merely sat paused left two sessions coexisting and the Bluetooth output
        // crackled. Re-match the route's rate/buffer right after, then read diagnostics.
        try? AVAudioSession.sharedInstance().setActive(true)
        configureForCurrentRoute()
        updateDiagnostics()

        if prefersAV(url) {
            backend = .av
            player.stop()                 // make sure libVLC is not also sounding
            openAV(url)
            return
        }

        backend = .vlc
        teardownAV()                      // stop/detach AVPlayer
        // MUTE (not just volume-0) across the whole transition and hold it a beat
        // into playback: the volume resets per media, but the mute flag is what
        // actually silences the new stream's first buffer — the moment the "텁" pop
        // fires. Unmuting too early (on the first frame) let the pop through, so it
        // is released a short time after playback is up (see timeChanged).
        let s = player.state
        let needStop = s == .ended || s == .error || s == .stopped
        player.audio?.isMuted = true
        if needStop { player.stop() }
        pendingUnmute = true
        player.media = makeMedia(url)
        player.audio?.isMuted = true
        player.play()
        player.rate = rate
        scheduleWatchdog(url)
    }

    // MARK: AVPlayer backend

    private func openAV(_ url: URL) {
        // Swap item → item WITHOUT niling first: replaceCurrentItem(nil) deactivated
        // the audio output and the next item re-activated it, which is the route
        // "텁" pop (it happened on m4a music too, proving it was not libVLC). A
        // direct item→item swap is seamless.
        removeAVObservers()
        let item = AVPlayerItem(url: url)
        // Sample-accurate volume ramp over the first 60ms: whatever the "텁" is in
        // the opening samples (encoder-delay priming, a DC step), a fade masks it —
        // and an AVAudioMix ramp applies to the signal itself, independent of the
        // system volume, so it actually lands (unlike libVLC's late audio object).
        if let track = item.asset.tracks(withMediaType: .audio).first {
            let params = AVMutableAudioMixInputParameters(track: track)
            params.setVolumeRamp(fromStartVolume: 0, toEndVolume: 1,
                                 timeRange: CMTimeRange(start: .zero,
                                                        duration: CMTime(seconds: 0.06, preferredTimescale: 600)))
            let mix = AVMutableAudioMix()
            mix.inputParameters = [params]
            item.audioMix = mix
        }
        av.replaceCurrentItem(with: item)
        av.isMuted = muted
        av.volume = 1
        av.play()
        av.rate = rate
        hostView.showAV(av)               // route the picture through the AVPlayerLayer
        addAVObservers(item)
    }

    private func addAVObservers(_ item: AVPlayerItem) {
        avTimeObs = av.addPeriodicTimeObserver(
            forInterval: CMTime(value: 1, timescale: 4), queue: .main) { [weak self] _ in self?.avTick() }
        avEndObs = NotificationCenter.default.addObserver(
            forName: .AVPlayerItemDidPlayToEndTime, object: item, queue: .main) { [weak self] _ in
                self?.reachedEnd = true; self?.isPlaying = false; self?.onEnded?()
            }
        // Buffering: waiting-to-play shows the spinner; playing clears it.
        avControlObs = av.observe(\.timeControlStatus, options: [.new]) { [weak self] p, _ in
            guard let self = self, self.backend == .av else { return }
            if p.timeControlStatus == .playing { self.buffering = false }
        }
    }

    // Play/pause are instant — a de-click fade did not remove the pop (it is not a
    // signal-path click) and only added a hair of lag.
    private func avPlay() { av.volume = 1; av.play(); av.rate = rate }
    private func avPause() { av.pause() }

    private func removeAVObservers() {
        if let o = avTimeObs { av.removeTimeObserver(o); avTimeObs = nil }
        if let o = avEndObs { NotificationCenter.default.removeObserver(o); avEndObs = nil }
        avStatusObs = nil
        avControlObs = nil
    }

    private func avTick() {
        guard backend == .av, let item = av.currentItem else { return }
        if av.timeControlStatus == .playing { buffering = false }
        let cur = item.currentTime().seconds
        let dur = item.duration.seconds
        if !scrubbing, cur.isFinite {
            if dur.isFinite, dur > 0 {
                let p = Float(cur / dur)
                if abs(p - position) > 0.0008 { position = p }
            }
            let e = Self.clock(Int32(cur * 1000))
            if e != elapsed { elapsed = e }
        }
        if dur.isFinite, dur > 0 {
            let d = Self.clock(Int32(dur * 1000))
            if d != duration { duration = d }
        }
        // Treat "waiting to play" (buffering) as playing, so the pause icon does not
        // flip back to play for a beat right after a tap.
        let playing = av.timeControlStatus != .paused
        if isPlaying != playing { isPlaying = playing }
        updateDiagnostics()   // publishes only on change; keeps the readout live
        updateNowPlayingAV(cur: cur, dur: dur)
    }

    private func updateNowPlayingAV(cur: Double, dur: Double) {
        var info: [String: Any] = [MPMediaItemPropertyTitle: nowPlayingTitle]
        if dur.isFinite, dur > 0 { info[MPMediaItemPropertyPlaybackDuration] = dur }
        info[MPNowPlayingInfoPropertyElapsedPlaybackTime] = cur
        info[MPNowPlayingInfoPropertyPlaybackRate] = av.rate == 0 ? 0 : Double(rate)
        MPNowPlayingInfoCenter.default().nowPlayingInfo = info
    }

    private func teardownAV() {
        removeAVObservers()
        av.pause()
        av.replaceCurrentItem(with: nil)
        hostView.showAV(nil)
    }

    private func makeMedia(_ url: URL) -> VLCMedia {
        let media = VLCMedia(url: url)
        media.addOption(":file-caching=100")   // shorter startup buffer → faster start
        media.addOption(":no-audio-time-stretch")            // see player init — BT crackle
        media.addOption(":audio-resampler=speex_resampler")  // see player init — BT crackle
        return media
    }

    /// If the fast (no-stop) swap failed to start within ~1.6s, hard-reset once —
    /// this is what recovers the intermittent "next plays nothing but the title".
    private func scheduleWatchdog(_ url: URL) {
        openGen += 1
        let gen = openGen
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.6) { [weak self] in
            guard let self = self, gen == self.openGen, self.currentURL == url else { return }
            if !self.player.isPlaying && self.player.time.intValue == 0 {
                self.player.stop()
                self.player.media = self.makeMedia(url)
                self.pendingUnmute = true
                self.player.audio?.isMuted = true
                self.player.play()
                self.player.rate = self.rate
            }
        }
    }

    func pause() {
        backend == .av ? avPause() : player.pause()
        userPaused = true
    }
    /// Resume after we paused for a web video — reactivate the session first, since
    /// the web video may have taken the audio route.
    func resume() {
        try? AVAudioSession.sharedInstance().setActive(true)
        backend == .av ? avPlay() : player.play()
        userPaused = false
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
        let clamped = max(0, min(0.999, p))
        if backend == .av {
            avSeekFraction(clamped)
        } else {
            // Seeking to exactly 1.0 lands on end-of-stream, which VLC treats as
            // "finished" and snaps back — cap just short so the far end is reachable.
            player.position = clamped
        }
    }

    private func avSeekFraction(_ p: Float) {
        guard let item = av.currentItem else { return }
        let dur = item.duration.seconds
        guard dur.isFinite, dur > 0 else { return }
        av.seek(to: CMTime(seconds: dur * Double(p), preferredTimescale: 600),
                toleranceBefore: .zero, toleranceAfter: .zero)
    }

    func toggle() {
        if backend == .av {
            if reachedEnd || (av.currentItem.map { $0.duration.seconds.isFinite && $0.currentTime().seconds >= $0.duration.seconds - 0.3 } ?? false) {
                reachedEnd = false; av.seek(to: .zero); avPlay(); userPaused = false; isPlaying = true
            } else if av.timeControlStatus == .playing {
                // Only pause when it is actually playing. While it is buffering
                // (.waitingToPlayAtSpecifiedRate) the old code treated a tap as
                // "pause", so it took two taps to get playback going.
                avPause(); userPaused = true; isPlaying = false
            } else {
                avPlay(); userPaused = false; isPlaying = true
            }
            return
        }
        // A finished player will not resume on play(); reloading the file is the
        // reliable way to replay from the start.
        if reachedEnd || player.state == .ended || (!player.isPlaying && player.position > 0.995) {
            if let url = currentURL { open(url) }
        } else if player.isPlaying {
            player.pause()
            userPaused = true
            isPlaying = false     // flip the button at once; VLC can lag the state event
        } else {
            player.play()
            userPaused = false
            isPlaying = true
        }
    }
    func seek(to p: Float) {
        if backend == .av { avSeekFraction(max(0, min(1, p))) } else { player.position = max(0, min(1, p)) }
    }
    func jump(_ seconds: Int32) {
        if backend == .av {
            let t = (av.currentItem?.currentTime().seconds ?? 0) + Double(seconds)
            av.seek(to: CMTime(seconds: max(0, t), preferredTimescale: 600))
        } else {
            seconds < 0 ? player.jumpBackward(-seconds) : player.jumpForward(seconds)
        }
    }
    func rewindStep() { backend == .av ? jump(-1) : player.jumpBackward(1) }

    /// libVLC volume runs 0–200; we drive 0–1 from the drag. The last set value is
    /// remembered as the fade-in target so a ramp lands on what the user chose.
    private var targetVolume: Int32 = 100
    var volume: Float {
        get { Float(player.audio?.volume ?? 100) / 200 }
        set {
            let raw = Int32(max(0, min(1, newValue)) * 200)
            player.audio?.volume = raw
            targetVolume = raw
        }
    }

    private func applyRate(_ r: Float) {
        if backend == .av {
            // Setting AVPlayer.rate also starts playback; only change it while playing.
            if av.timeControlStatus != .paused { av.rate = r }
        } else {
            player.rate = r
        }
    }
    func cycleRate() {
        rate = Self.rates[(Self.rates.firstIndex(of: rate).map { $0 + 1 } ?? 0) % Self.rates.count]
        applyRate(rate)
    }
    func setRate(_ r: Float) { rate = r; applyRate(r) }
    func holdRate(_ value: Float?) { applyRate(value ?? rate) }

    func stop() {
        player.stop()
        teardownAV()
        currentURL = nil
        // Release the audio session so other devices/apps can take the Bluetooth route
        // back — holding it active kept the earbuds bound to us, which is why the Watch
        // could not grab them and Apple Music would not play while Shard was open.
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    func mediaPlayerTimeChanged(_ notification: Notification) {
        // Time is advancing → real playback, so any spinner comes down. This is
        // the reliable "actually playing" signal (a state change to .playing does
        // not always arrive).
        // First real frame of a freshly-opened stream: unmute (audio starts
        // priming) but KEEP the black cover a bit longer, then reveal picture and
        // sound together — libVLC's audio trails the first video frame by up to
        // ~0.5s, so revealing on the frame alone showed a silent picture. The extra
        // hold also keeps the spinner visible instead of a one-frame flash.
        if pendingUnmute, player.isPlaying {
            pendingUnmute = false
            buffering = false          // reveal the picture the instant it is running
            player.audio?.isMuted = muted
        } else if buffering, player.isPlaying {
            // Buffering that was NOT an open (e.g. after a seek) — clear it once
            // frames flow again, or the spinner spun forever over black.
            buffering = false
        }
        // Apply a resume-seek once the reloaded file is actually running.
        if let p = pendingSeek, player.isPlaying, player.position > 0 {
            player.position = p; pendingSeek = nil
        }
        // Assign only when the value actually changed. Re-assigning an identical
        // @Published value still fires objectWillChange, which re-rendered the
        // whole library four times a second and made the folder rename/delete
        // dialogs flicker while something played. A small position threshold keeps
        // the seek bar smooth without a publish on every sub-pixel tick.
        tick &+= 1
        if !scrubbing, tick % 2 == 0 {   // ~2×/sec is smooth enough for a thin bar
            let p = player.position
            if abs(p - position) > 0.0008 { position = p }
        }
        if isPlaying != player.isPlaying { isPlaying = player.isPlaying }
        if tick % 4 == 0 { updateDiagnostics() }   // ~2×/sec; publishes only on change
        let e = Self.clock(player.time.intValue)
        if e != elapsed { elapsed = e }
        if let length = player.media?.length.intValue, length > 0 {
            let d = Self.clock(length)
            if d != duration { duration = d }
        }
        updateNowPlaying()
    }

    func mediaPlayerStateChanged(_ notification: Notification) {
        isPlaying = player.isPlaying
        updateNowPlaying()
        // Do NOT raise the cover on .opening/.buffering here — that also fired for
        // a mid-playback seek/jump, blacking out the picture with a spinner while
        // the sound kept going. The cover is driven only by open() (a real track
        // change) and cleared on the first frame; a seek just re-buffers silently.
        if player.state == .ended || player.state == .error { buffering = false }
        // Keep the (late-created) new audio object MUTED at every state update until
        // we release it after playback is up — this is what actually stops the "텁"
        // pop on the first buffer.
        if pendingUnmute { player.audio?.isMuted = true }
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
    /// AVPlayer draws into this sublayer; when nil/hidden, libVLC draws into the
    /// view itself. The two never play at once.
    private let avLayer = AVPlayerLayer()

    override init(frame: CGRect) {
        super.init(frame: frame)
        avLayer.videoGravity = .resizeAspect
        avLayer.isHidden = true
        layer.addSublayer(avLayer)
    }
    required init?(coder: NSCoder) { fatalError() }

    /// Show (or hide) the AVPlayer picture on top of the libVLC surface.
    func showAV(_ player: AVPlayer?) {
        avLayer.player = player
        avLayer.isHidden = (player == nil)
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        avLayer.frame = bounds
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

/// The seek bar plus its two time labels. Observes only `PlayerUI`, so it is the
/// one thing that re-renders as playback advances.
private struct SeekRow: View {
    @ObservedObject var ui: PlayerUI
    var onBegin: () -> Void
    var onScrub: (Double) -> Void
    var onEnd: (Double) -> Void
    var body: some View {
        HStack(spacing: 6) {
            // Fixed width so the bar does not grow/shrink as the elapsed time gains or
            // loses a digit; wide enough for an hours clock (1:23:45), and lineLimit(1)
            // + a scale floor keep it to ONE line instead of wrapping to two.
            Text(ui.elapsed).font(.system(size: 10)).foregroundColor(.white)
                .monospacedDigit().lineLimit(1).minimumScaleFactor(0.7)
                .frame(width: 50, alignment: .trailing)
            SeekSlider(value: Binding(get: { Double(ui.position) }, set: { onScrub($0) }),
                       onBegin: onBegin, onScrub: onScrub, onEnd: onEnd)
            Text(ui.duration).font(.system(size: 10)).foregroundColor(.white)
                .monospacedDigit().lineLimit(1).minimumScaleFactor(0.7)
                .frame(width: 50, alignment: .leading)
        }
    }
}

/// A brief on-screen gauge for a side drag (brightness or sound).
private struct Gauge: View {
    let icon: String
    let value: Double
    var body: some View {
        // Vertical: the finger moves up/down to set it, so the gauge fills bottom-up
        // to match the gesture.
        VStack(spacing: 6) {
            Capsule().fill(Color.white.opacity(0.3)).frame(width: 3, height: 60)
                .overlay(alignment: .bottom) {
                    Capsule().fill(Color.white).frame(width: 3, height: 60 * max(0, min(1, value)))
                }
            Image(systemName: icon).font(.system(size: 11))
        }
        .padding(.horizontal, 9).padding(.vertical, 10)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .foregroundColor(.white)
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
    // Full-screen enter/exit go through the library, which rotates behind a black
    // cover so the squish/animation is never seen and the exit rotates BEFORE it
    // shrinks (so the list is not glimpsed mid-turn).
    var onEnterFullscreen: () -> Void = {}
    var onExitFullscreen: () -> Void = {}

    @State private var showControls = true
    @State private var rewindTimer: Timer?
    @State private var hideWork: DispatchWorkItem?
    @State private var gauge: (icon: String, value: Double)?
    @State private var zoom: CGFloat = 1
    /// A brief double-tap flash: (rightward, seconds).
    @State private var seekFlash: (right: Bool, secs: Int)?

    /// The saved cover for the song now playing, so a music stage shows art.
    private var musicCover: UIImage? {
        guard isMusic, let url = controller.currentURL else { return nil }
        return Covers.load(Covers.keyFor(url.lastPathComponent))
    }

    private enum DragMode { case brightness, volume, swipe, none }
    @State private var dragMode: DragMode = .none
    @State private var startBrightness: Double = 1
    @State private var startVolume: Float = 0
    /// True while a hold (2×/rewind) is down, so the vertical-drag
    /// brightness/volume is suppressed for its duration.
    @State private var holdActive = false
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
                // A song has no picture of its own — show the saved cover, large,
                // and fall back to a note only when there is none.
                if let cover = musicCover {
                    Image(uiImage: cover).resizable().aspectRatio(contentMode: .fit)
                } else {
                    Image(systemName: "music.note").font(.system(size: 64)).foregroundColor(.white.opacity(0.4))
                }
            }
            // Video-only brightness — dim the picture, never the phone screen.
            if controller.brightness < 1 {
                Color.black.opacity(1 - controller.brightness).allowsHitTesting(false)
            }
            // Cover the surface with black+spinner while a new stream is opening
            // AND for a short hold after playback starts (see VLCController) — this
            // hides the old frame flash and lets the audio catch the picture.
            if controller.buffering && !isMusic {
                Color.black
                ProgressView().progressViewStyle(.circular).tint(.white).scaleEffect(1.3)
            }
            // An opaque black cover over the video WHILE the interface rotates, so
            // the mid-rotation squish is never seen. A plain conditional (not an
            // opacity animation) means it snaps in instantly with no fade.
            if controller.settling { Color.black }
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
            // Hide the controls while the interface is rotating — otherwise they
            // were seen jumping from the windowed/portrait insets to the landscape
            // ones as the black cover cleared. They reappear already in place.
            if showControls && !controller.settling { controlsOverlay.transition(.opacity) }
            // TEMP audio diagnostics — a live readout of the route/rate/buffer so a
            // Bluetooth crackle can be pinned down on the device. Always visible (not
            // gated by the controls) so it can be read while a song plays. Remove once
            // the crackle is understood.
            VStack(alignment: .leading, spacing: 1) {
                Text(controller.diagRoute)
                Text(controller.diagDetail)
            }
            .font(.system(size: 10, weight: .semibold, design: .monospaced))
            .foregroundColor(.yellow)
            .padding(5)
            .background(Color.black.opacity(0.6))
            .cornerRadius(5)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .padding(.top, fullscreen ? topInset + 8 : 44).padding(.leading, 10)
            .allowsHitTesting(false)
            // Above the controls: the popup + its full-screen dismiss catcher.
            pickerLayer
        }
        .clipped()
        // When playback stops or reaches the end, surface the bar so the play
        // button is right there — otherwise a finished video sat with no controls.
        .onChange(of: controller.isPlaying) { playing in if !playing { showBar() } }
        .onAppear { showBar() }
        // Orientation is NOT driven from here: entering full screen swaps this view
        // for a fresh instance born with fullscreen==true, so onChange never fires
        // and onDisappear of the old instance would free the lock the new one just
        // set. LibraryScreen owns the lock instead (it persists across the swap).
    }

    /// Tap toggles the bar — up if hidden, away if shown.
    private func toggleBar() {
        // A tap while a picker is open just dismisses the picker (keeping the bar).
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
        // A hold runs 2× on the right, rewind on the left — in both windowed and
        // full screen. While it is down, the vertical-drag brightness/volume is
        // suppressed (see vdrag), so the two no longer fire together.
        holdActive = active
        if x > w / 2 {
            controller.holdRate(active ? 2.0 : nil)
        } else {
            active ? startRewind() : stopRewind()
        }
    }

    private func vdrag(_ phase: PlayerGestures.Phase, _ startX: CGFloat, _ w: CGFloat, _ dy: CGFloat) {
        switch phase {
        case .began:
            if startX < w * 0.2 {
                dragMode = .brightness; startBrightness = controller.brightness
            } else if startX > w * 0.8 {
                dragMode = .volume; startVolume = SystemVolume.shared.level
            } else {
                dragMode = .swipe
            }
        case .changed:
            // A hold owns the gesture (2×/rewind) — do not also adjust
            // brightness/volume while it is down.
            if holdActive { return }
            let step = Float(-dy / 160)                     // from the current value
            if dragMode == .brightness {
                let b = max(0.1, min(1, startBrightness + Double(step)))
                controller.brightness = b; gauge = ("sun.max", b)
            } else if dragMode == .volume {
                let v = max(0, min(1, startVolume + step)); SystemVolume.shared.set(v)
                gauge = ("speaker.wave.2", Double(v))
            }
        case .ended:
            if holdActive {
                // The hold is driving 2×/rewind — never fire the swipe toggle.
                dragMode = .none
            } else if dragMode == .swipe {
                if dy < -42 { if !fullscreen { onEnterFullscreen() } }
                else if dy > 42 { if fullscreen { onExitFullscreen() } }
                dragMode = .none
            } else {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { gauge = nil }
                dragMode = .none
            }
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
            // Take any open picker down with the bar, so it does not reappear when
            // the bar comes back on the next tap.
            showRatePicker = false; showVolumeBar = false
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
    private var safeInsets: UIEdgeInsets {
        UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            .first(where: { $0.activationState == .foregroundActive })?
            .keyWindow?.safeAreaInsets ?? .zero
    }
    private var topInset: CGFloat { safeInsets.top }
    /// How far to pull the controls in from the edges in full screen: clear of the
    /// notch/home-indicator on the sides, plus enough to sit around the seek bar's
    /// span rather than jammed against the very edge.
    private var sideInset: CGFloat { max(safeInsets.left, safeInsets.right) + 12 }
    /// Whether the screen is currently wider than tall — a landscape full screen
    /// wants the controls pulled in more; a portrait one (a short) wants the seek
    /// bar to run nearly full width.
    private var screenLandscape: Bool {
        UIScreen.main.bounds.width > UIScreen.main.bounds.height
    }
    private var seekInset: CGFloat { fullscreen ? (screenLandscape ? 34 : 6) : 0 }
    // A touch inside the time labels so the outer buttons do not stick out past
    // them.
    private var buttonInset: CGFloat { fullscreen ? (screenLandscape ? 46 : 24) : 10 }

    private var controlsOverlay: some View {
        VStack(spacing: 0) {
            HStack {
                Text(title).font(.subheadline).foregroundColor(.white).lineLimit(1).shadow(radius: 2)
                Spacer()
                closeButton
            }
            .padding(.leading, fullscreen ? safeInsets.left + 16 : 12)
            .padding(.trailing, fullscreen ? safeInsets.right + 16 : 12)
            .padding(.top, fullscreen ? max(topInset - 12, 2) : 10)
            .padding(.bottom, 8)
            Spacer()
            transport
        }
    }

    /// A white ✕ with only a hairline dark edge on the glyph itself.
    private var closeButton: some View {
        // Always an ✕ — in full screen it drops back to the window, windowed it
        // stops. The collapse-arrows glyph was not wanted.
        Button { fullscreen ? onExitFullscreen() : onStop() } label: {
            Image(systemName: "xmark")
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
        .padding(6).background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .padding(.trailing, 2)
    }

    private var volumeBar: some View {
        HStack(spacing: 10) {
            Image(systemName: "speaker.fill").font(.system(size: 12)).foregroundColor(.white)
            SeekSlider(
                // Drives the PHONE's volume (synced with the hardware buttons), not
                // libVLC's own gain. Local display for an instant thumb; shows 0
                // while muted, and any drag lifts the mute.
                value: Binding(get: { controller.muted ? 0 : volDisplay }, set: { _ in }),
                onBegin: { interacting = true; keepBar() },
                onScrub: { v in
                    if controller.muted { controller.toggleMute() }
                    volDisplay = v; SystemVolume.shared.set(Float(v)); keepBar()
                },
                onEnd: { _ in interacting = false; showBar() })
            Image(systemName: "speaker.wave.3.fill").font(.system(size: 12)).foregroundColor(.white)
        }
        .frame(width: 220)
        .padding(.horizontal, 14).padding(.vertical, 8)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .padding(.trailing, 2)
    }

    private var transport: some View {
        VStack(spacing: fullscreen ? 14 : 10) {
            // Top row: the seek bar only — its own subview so it (and only it)
            // re-renders as the position ticks, leaving the library still. It spans
            // the full width (long); only the buttons below are pulled inward.
            SeekRow(ui: controller.ui,
                    onBegin: { controller.beginScrub(); interacting = true; keepBar() },
                    onScrub: { controller.previewSeek(Float($0)); keepBar() },
                    onEnd: { controller.endScrub(Float($0)); interacting = false; showBar() })
                .padding(.horizontal, seekInset)
            // Bottom row: transport on the left (wide), sound/speed/full-screen right.
            HStack {
                HStack(spacing: fullscreen ? 44 : 28) {
                    Button { onPrev(); showBar() } label: { Image(systemName: "backward.end.fill").font(.system(size: 15)) }.disabled(!hasPrev)
                    Button { controller.toggle(); showBar() } label: {
                        Image(systemName: controller.isPlaying ? "pause.fill" : "play.fill").font(.system(size: 19))
                            .animation(nil, value: controller.isPlaying)   // switch icon instantly, no morph
                    }
                    Button { onNext(); showBar() } label: { Image(systemName: "forward.end.fill").font(.system(size: 15)) }.disabled(!hasNext)
                }
                .padding(.leading, buttonInset)   // in to meet the seek bar
                Spacer()
                HStack(spacing: fullscreen ? 28 : 20) {
                    Button { controller.toggleMute(); showBar() } label: {
                        Image(systemName: controller.muted ? "speaker.slash.fill" : "speaker.wave.2.fill").font(.system(size: 15))
                    }
                    // highPriority, not simultaneous: when the hold is recognized
                    // it cancels the button's tap, so releasing after a hold does
                    // not also mute / step the rate. A short tap still fails the
                    // long press and runs the button normally.
                    .highPriorityGesture(LongPressGesture(minimumDuration: 0.3).onEnded { _ in
                        volDisplay = Double(SystemVolume.shared.level)
                        showVolumeBar.toggle(); showRatePicker = false; showBar()
                    })
                    Button { controller.cycleRate(); showBar() } label: {
                        // Fixed width so 1× ↔ 1.25× does not shove the buttons
                        // beside it as the rate changes.
                        Text(String(format: "%g×", controller.rate)).font(.system(size: 14, weight: .bold))
                            .frame(width: 42)
                    }
                    .highPriorityGesture(LongPressGesture(minimumDuration: 0.3).onEnded { _ in
                        showRatePicker.toggle(); showVolumeBar = false; showBar()
                    })
                    Button { fullscreen ? onExitFullscreen() : onEnterFullscreen() } label: {
                        Image(systemName: fullscreen ? "arrow.down.right.and.arrow.up.left"
                                                      : "arrow.up.left.and.arrow.down.right").font(.system(size: 15))
                    }
                }
                .padding(.trailing, buttonInset)  // in to meet the seek bar
            }
            .foregroundColor(.white)
        }
        // Windowed uses a small side inset so the seek bar runs nearly the whole
        // width; full screen keeps a modest edge margin.
        .padding(.horizontal, fullscreen ? 12 : 8)
        // Full screen: a small top inset (thin band above the seek bar) but a
        // larger bottom inset so the seek bar and buttons sit higher, off the very
        // edge — the band above the seek is what was overlapping the video.
        .padding(.top, fullscreen ? 0 : 8)
        .padding(.bottom, fullscreen ? 26 : 16)
        .background(Color.black.opacity(0.3))
    }

    /// The rate/volume popup as a TOP-LEVEL layer with a full-screen catcher below
    /// it, so a tap anywhere but the popup dismisses it (the popup sits above the
    /// catcher and stays usable). The buttons underneath are untouched when it is
    /// closed.
    @ViewBuilder private var pickerLayer: some View {
        if showRatePicker || showVolumeBar {
            Color.clear.contentShape(Rectangle())
                .onTapGesture { showRatePicker = false; showVolumeBar = false; showBar() }
            VStack(alignment: .trailing, spacing: 6) {
                if showRatePicker { ratePicker }
                if showVolumeBar { volumeBar }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomTrailing)
            .padding(.trailing, fullscreen ? sideInset : 12)
            .padding(.bottom, fullscreen ? 64 : 50)   // just above the buttons, as before
        }
    }
}
