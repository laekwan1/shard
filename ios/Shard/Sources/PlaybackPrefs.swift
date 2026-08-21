import SwiftUI

/// What happens when a track ends — the same choices the Android app keeps under
/// the gear: stop, go on to the next, or shuffle. Plus whether sound keeps
/// playing in the background.
enum PlaybackEnd: String { case stop, next, shuffle }

final class PlaybackPrefs: ObservableObject {
    @AppStorage("playbackEnd") private var endRaw = PlaybackEnd.stop.rawValue
    @AppStorage("backgroundPlayback") var background = false

    var end: PlaybackEnd {
        get { PlaybackEnd(rawValue: endRaw) ?? .stop }
        set { endRaw = newValue.rawValue; objectWillChange.send() }
    }

    /// Toggle a mode, turning it back to stop if it was already on — matching the
    /// desktop and phone, where pressing the lit one clears it.
    func toggleEnd(_ want: PlaybackEnd) {
        end = end == want ? .stop : want
    }
}
