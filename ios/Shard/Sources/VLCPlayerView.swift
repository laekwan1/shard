import SwiftUI
import MobileVLCKit

/// A player backed by libVLC, so the library plays everything the engine can
/// write — mp4, but also the mkv/webm (VP9/Opus) that YouTube and some sites
/// give and that AVPlayer refuses. One player for every container means the
/// library never has to care what a file turned out to be.
struct VLCPlayerView: UIViewRepresentable {
    let url: URL

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> UIView {
        let container = UIView()
        container.backgroundColor = .black
        let player = context.coordinator.player
        player.drawable = container
        player.media = VLCMedia(url: url)
        player.play()
        return container
    }

    func updateUIView(_ uiView: UIView, context: Context) {}

    static func dismantleUIView(_ uiView: UIView, coordinator: Coordinator) {
        coordinator.player.stop()
    }

    /// Holds the player for the view's lifetime; stopping it on dismantle frees
    /// the decoder rather than leaving it running behind a dismissed sheet.
    final class Coordinator {
        let player = VLCMediaPlayer()
    }
}
