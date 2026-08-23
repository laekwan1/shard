import SwiftUI

/// What happens when a track ends — the same choices the Android app keeps under
/// the gear: stop, go on to the next, or shuffle. Plus whether sound keeps
/// playing in the background.
enum PlaybackEnd: String { case stop, next, shuffle }

final class PlaybackPrefs: ObservableObject {
    @AppStorage("backgroundPlayback") var background = false

    // A real @Published, not a computed view over @AppStorage: inside an
    // ObservableObject the @AppStorage wrapper does not reliably fire
    // objectWillChange, so the shelf's end-mode read stale — shuffle behaved like
    // sequential because `end` never actually reported .shuffle to the player.
    @Published var end: PlaybackEnd {
        didSet { UserDefaults.standard.set(end.rawValue, forKey: "playbackEnd") }
    }

    init() {
        let raw = UserDefaults.standard.string(forKey: "playbackEnd")
        end = raw.flatMap(PlaybackEnd.init(rawValue:)) ?? .stop
    }

    /// Toggle a mode, turning it back to stop if it was already on — matching the
    /// desktop and phone, where pressing the lit one clears it.
    func toggleEnd(_ want: PlaybackEnd) {
        end = end == want ? .stop : want
    }
}
