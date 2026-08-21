import SwiftUI
import AVFoundation
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
    var scrubbing = false

    private var currentURL: URL?
    private var pendingSeek: Float?
    private static let rates: [Float] = [1, 1.25, 1.5, 2, 0.5, 0.75]

    override init() {
        super.init()
        player.delegate = self
        // Keep sound going when the screen locks or the app backgrounds, so a
        // video can be listened to like music. moviePlayback mode is the one
        // meant for a video player and cuts the start/seek clicks a bare
        // .playback session let through.
        let session = AVAudioSession.sharedInstance()
        try? session.setCategory(.playback, mode: .moviePlayback)
        try? session.setActive(true)
    }

    func attach(to view: UIView) { player.drawable = view }

    /// Load and play a file, restoring where it was left off.
    func open(_ url: URL) {
        savePosition()
        currentURL = url
        player.media = VLCMedia(url: url)
        let saved = UserDefaults.standard.float(forKey: "pos:\(url.lastPathComponent)")
        pendingSeek = saved > 0.01 && saved < 0.98 ? saved : nil
        player.play()
        player.rate = rate
    }

    func toggle() { player.isPlaying ? player.pause() : player.play() }
    func seek(to p: Float) { player.position = p }

    /// Step back in time for the rewind hold. Jumping by whole seconds off the
    /// clock, rather than nudging the 0–1 position, avoids the little
    /// forward-then-back oscillation that made a short stretch repeat.
    func rewindStep() {
        player.jumpBackward(1)
    }

    func cycleRate() {
        let next = Self.rates[(Self.rates.firstIndex(of: rate).map { $0 + 1 } ?? 0) % Self.rates.count]
        rate = next
        player.rate = next
    }

    /// A temporary rate for a press-and-hold; pass nil to restore the chosen one.
    func holdRate(_ value: Float?) {
        player.rate = value ?? rate
    }

    func stop() {
        savePosition()
        player.stop()
    }

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
        if let length = player.media?.length.intValue, length > 0 {
            duration = Self.clock(length)
        }
    }

    func mediaPlayerStateChanged(_ notification: Notification) {
        isPlaying = player.isPlaying
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
        let view = UIView()
        view.backgroundColor = .black
        controller.attach(to: view)
        return view
    }
    func updateUIView(_ uiView: UIView, context: Context) {}
}

/// A player over a playlist (the current folder): play/pause, a scrubber with
/// times, speed, previous/next, and press-and-hold on the picture — the right
/// half runs at 2×, the left half rewinds — matching the desktop and phone
/// players. Position is remembered per file.
struct VLCPlayerScreen: View {
    let playlist: [URL]
    let start: Int
    var onClose: () -> Void

    @StateObject private var controller = VLCController()
    @State private var index: Int = 0
    @State private var showControls = true
    @State private var rewindTimer: Timer?

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            VLCSurface(controller: controller).ignoresSafeArea()
            holdSurfaces
            if showControls { overlay.transition(.opacity) }
        }
        .onAppear {
            index = min(max(0, start), max(0, playlist.count - 1))
            if playlist.indices.contains(index) { controller.open(playlist[index]) }
        }
        .onDisappear { controller.stop() }
    }

    /// Two invisible halves: hold right for 2×, hold left to rewind. A single
    /// tap toggles the controls.
    private var holdSurfaces: some View {
        HStack(spacing: 0) {
            Color.clear.contentShape(Rectangle())
                .onTapGesture { withAnimation { showControls.toggle() } }
                .onLongPressGesture(minimumDuration: 0.35, pressing: { pressing in
                    if pressing { startRewind() } else { stopRewind() }
                }, perform: {})
            Color.clear.contentShape(Rectangle())
                .onTapGesture { withAnimation { showControls.toggle() } }
                .onLongPressGesture(minimumDuration: 0.35, pressing: { pressing in
                    controller.holdRate(pressing ? 2.0 : nil)
                }, perform: {})
        }
    }

    private func startRewind() {
        rewindTimer?.invalidate()
        rewindTimer = Timer.scheduledTimer(withTimeInterval: 0.15, repeats: true) { _ in
            controller.rewindStep()
        }
    }
    private func stopRewind() {
        rewindTimer?.invalidate()
        rewindTimer = nil
    }

    private var overlay: some View {
        VStack {
            HStack {
                Spacer()
                Button { controller.stop(); onClose() } label: {
                    Image(systemName: "xmark.circle.fill").font(.title).foregroundColor(.white.opacity(0.9))
                }
            }
            .padding()
            Spacer()
            transport
        }
    }

    private var transport: some View {
        VStack(spacing: 10) {
            HStack(spacing: 20) {
                Button { step(-1) } label: { Image(systemName: "backward.end.fill") }
                    .disabled(index <= 0)
                Button { controller.toggle() } label: {
                    Image(systemName: controller.isPlaying ? "pause.fill" : "play.fill").font(.title)
                }
                Button { step(1) } label: { Image(systemName: "forward.end.fill") }
                    .disabled(index >= playlist.count - 1)
                Spacer()
                Button { controller.cycleRate() } label: {
                    Text(String(format: "%g×", controller.rate)).font(.subheadline.bold())
                }
            }
            .foregroundColor(.white)

            HStack(spacing: 10) {
                Text(controller.elapsed).font(.caption).foregroundColor(.white).monospacedDigit()
                Slider(
                    value: Binding(get: { Double(controller.position) },
                                   set: { controller.position = Float($0) }),
                    in: 0...1,
                    onEditingChanged: { editing in
                        controller.scrubbing = editing
                        if !editing { controller.seek(to: controller.position) }
                    }
                ).tint(.white)
                Text(controller.duration).font(.caption).foregroundColor(.white).monospacedDigit()
            }
        }
        .padding()
        .background(.ultraThinMaterial)
    }

    private func step(_ delta: Int) {
        let next = index + delta
        guard playlist.indices.contains(next) else { return }
        index = next
        controller.open(playlist[next])
    }
}
