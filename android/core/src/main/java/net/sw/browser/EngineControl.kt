package net.sw.browser

/**
 * Turning the engine on and off from outside the screen.
 *
 * The notification needs to do it, and the notification outlives whatever
 * activity was on top when the download started. So the hook lives here, at
 * process scope, and is deliberately implemented against the application
 * context — anything holding an activity would keep a dead screen alive.
 */
object EngineControl {

    interface Control {
        fun isOn(): Boolean
        fun setOn(on: Boolean)
    }

    @Volatile
    var control: Control? = null

    fun isOn(): Boolean = control?.isOn() ?: false

    /**
     * How anything still playing is told to stop.
     *
     * The library holds a player that outlives its own screen — that is what
     * makes background sound possible — so nothing about a view can be relied
     * on to end it. Closing the app has to reach the player wherever it is, and
     * this is the one place both halves of the app can see.
     */
    @Volatile
    var stopPlayback: (() -> Unit)? = null

    // ---- library playback, shown in the same notification -------------------
    //
    // The engine switch and what the library is playing share one notification,
    // so the service that draws it needs to see the player, and the player needs
    // to tell the service when something changed. Both live here, at process
    // scope, for the same reason the switch does: the notification outlives the
    // screen the video started on.

    /** The title of what is playing, or null when nothing is. */
    @Volatile
    var playbackTitle: String? = null

    /** Whether it is playing rather than paused, for the button's icon. */
    @Volatile
    var playbackPlaying: Boolean = false

    /** Play or pause the library's video. */
    @Volatile
    var togglePlayback: (() -> Unit)? = null

    /** The service asks to be told when playback changed, so it can redraw. */
    @Volatile
    var onPlaybackChanged: (() -> Unit)? = null

    fun toggle() {
        val current = control ?: return
        current.setOn(!current.isOn())
    }
}
