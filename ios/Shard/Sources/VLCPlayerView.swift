import SwiftUI
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
    /// True while the user drags the scrubber, so ticks do not fight the drag.
    var scrubbing = false
    private var started = false

    func open(_ url: URL, into view: UIView) {
        guard !started else { return }
        started = true
        player.delegate = self
        player.drawable = view
        player.media = VLCMedia(url: url)
        player.play()
    }

    func mediaPlayerTimeChanged(_ notification: Notification) {
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

    func toggle() { player.isPlaying ? player.pause() : player.play() }
    func seek(to p: Float) { player.position = p }
    func stop() { player.stop() }

    /// Milliseconds to m:ss (or h:mm:ss past an hour).
    private static func clock(_ ms: Int32) -> String {
        let total = Int(max(0, ms)) / 1000
        let s = total % 60, m = (total / 60) % 60, h = total / 3600
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, s) : String(format: "%d:%02d", m, s)
    }
}

/// The bare drawing surface libVLC renders into.
private struct VLCSurface: UIViewRepresentable {
    let controller: VLCController
    let url: URL
    func makeUIView(context: Context) -> UIView {
        let view = UIView()
        view.backgroundColor = .black
        controller.open(url, into: view)
        return view
    }
    func updateUIView(_ uiView: UIView, context: Context) {}
    static func dismantleUIView(_ uiView: UIView, coordinator: ()) {}
}

/// A player with controls: play/pause, a scrubber with times, and a close
/// button. Tapping the picture toggles the controls.
struct VLCPlayerScreen: View {
    let url: URL
    var onClose: () -> Void
    @StateObject private var controller = VLCController()
    @State private var showControls = true

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            VLCSurface(controller: controller, url: url)
                .ignoresSafeArea()
                .onTapGesture { withAnimation { showControls.toggle() } }

            if showControls {
                VStack {
                    HStack {
                        Spacer()
                        Button { controller.stop(); onClose() } label: {
                            Image(systemName: "xmark.circle.fill")
                                .font(.title).foregroundColor(.white.opacity(0.9))
                        }
                    }
                    .padding()
                    Spacer()
                    controls
                }
                .transition(.opacity)
            }
        }
        .onDisappear { controller.stop() }
    }

    private var controls: some View {
        HStack(spacing: 14) {
            Button { controller.toggle() } label: {
                Image(systemName: controller.isPlaying ? "pause.fill" : "play.fill")
                    .font(.title2).foregroundColor(.white)
            }
            Text(controller.elapsed).font(.caption).foregroundColor(.white).monospacedDigit()
            Slider(
                value: Binding(
                    get: { Double(controller.position) },
                    set: { controller.position = Float($0) }
                ),
                in: 0...1,
                onEditingChanged: { editing in
                    controller.scrubbing = editing
                    if !editing { controller.seek(to: controller.position) }
                }
            )
            .tint(.white)
            Text(controller.duration).font(.caption).foregroundColor(.white).monospacedDigit()
        }
        .padding()
        .background(.ultraThinMaterial)
    }
}
