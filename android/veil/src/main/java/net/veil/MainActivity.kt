package net.veil

import android.content.Context
import android.widget.EditText
import androidx.core.content.edit
import com.google.android.material.dialog.MaterialAlertDialogBuilder
import net.sw.browser.BrowserActivity
import net.sw.browser.EngineControl
import net.sw.browser.ProxyRoute
// Library resources are not folded into the app's R class, so the shared
// strings have to be named where they live.
import net.sw.browser.R as CoreR

/**
 * Veil: the browser with something else between it and the internet.
 *
 * Two modes, because they answer different questions.
 *
 * **Tor** is the anonymity one. Traffic passes through three relays run by
 * strangers who each know only one hop, so no single party can connect the
 * request to the person. The cost is speed, and that plenty of sites refuse
 * Tor exit addresses outright.
 *
 * **A server** is the private one. It hides the destination from the network
 * and the address from the site, at full speed — but the server is registered
 * to its owner, so it relocates rather than anonymises. It is also what works
 * when Tor is blocked.
 */
class MainActivity : BrowserActivity() {

    private enum class Mode { TOR, SERVER }

    private var port = 0

    /**
     * Off when the app opens.
     *
     * Tunnelling is something to reach for, not something to be doing by
     * default — and with Tor it is a minute of waiting nobody asked for.
     */
    override val autoStart: Boolean get() = false

    override fun proxyKind(): ProxyRoute.Kind =
        if (mode() == Mode.TOR) ProxyRoute.Kind.SOCKS else ProxyRoute.Kind.HTTP

    override fun onStart() {
        super.onStart()
        setSettingsAction { chooseMode() }
        if (mode() == Mode.SERVER && savedLink().isBlank()) chooseMode()
    }

    // ---- engine ------------------------------------------------------------

    override fun startEngine(): Int = when (mode()) {
        Mode.TOR -> startTor()
        Mode.SERVER -> startServer()
    }

    /**
     * Tor needs nothing started here — Orbot is already a running process.
     * What is needed is proof that it is carrying traffic before the browser is
     * pointed at it, or every page simply fails.
     */
    private fun startTor(): Int {
        if (TorEngine.isRunning()) {
            port = TorEngine.SOCKS_PORT
            return port
        }
        // Bootstrapping is slow enough to need saying so, and slow enough that
        // it cannot block the screen — so the answer comes back later.
        toast(getString(R.string.tor_starting))
        preparing = true
        startTorInBackground()
        return TOR_STARTING
    }

    /**
     * Bring tor up off the main thread and switch on when it is ready.
     *
     * A first connection to the network takes tens of seconds. Holding the UI
     * for that would look like a hang, and returning "started" before the port
     * is open would point the browser at nothing.
     */
    private fun startTorInBackground() {
        Thread {
            val ok = TorEngine.start(applicationContext)
            runOnUiThread {
                preparing = false
                if (ok) {
                    setEngine(true)
                } else {
                    MaterialAlertDialogBuilder(this)
                        .setTitle(R.string.tor_not_running_title)
                        .setMessage(getString(R.string.tor_failed, TorEngine.lastError))
                        .setPositiveButton(CoreR.string.close, null)
                        .show()
                }
            }
        }.apply { isDaemon = true }.start()
    }

    private fun startServer(): Int {
        val link = savedLink()
        if (link.isBlank()) {
            chooseMode()
            return NO_SERVER
        }
        // A previous run's engine may still be up. The app can be closed
        // without this process ending, and the engine lives in the process —
        // so starting again met "already running", the start failed, and the
        // switch sat at off while traffic was still being tunnelled.
        if (Native.isRunning()) Native.stop()
        // Port 0 asks the OS for a free one, so two runs never collide.
        val bound = Native.start(link, 0)
        if (bound > 0) port = bound
        return bound
    }

    override fun stopEngine() {
        when (mode()) {
            Mode.TOR -> TorEngine.stop(applicationContext)
            Mode.SERVER -> Native.stop()
        }
        port = 0
    }

    /**
     * Whether tor is still coming up.
     *
     * Bootstrapping takes tens of seconds and says nothing while it does, so
     * the status line carries it — otherwise the only signal is a switch that
     * has stayed off for no visible reason.
     */
    private var preparing = false

    override fun idleStatus(): String =
        if (preparing) getString(R.string.tor_preparing) else super.idleStatus()

    override fun engineStatus(): String = when (mode()) {
        Mode.TOR -> getString(R.string.status_tor)
        Mode.SERVER ->
            if (Native.isRunning()) getString(R.string.status_connected)
            else getString(R.string.status_stopped)
    }

    override fun bytesReceived(): Long =
        if (mode() == Mode.SERVER) Native.stats().bytesDown else 0

    override fun startFailureMessage(code: Int): String = when (code) {
        // The dialog or the toast has already said why, or the answer is still
        // on its way from the background.
        NO_SERVER, NO_TOR, TOR_STARTING -> ""
        else -> Native.reasonFor(code)
    }

    /** Holds only what it needs, so the notification can outlive this screen. */
    override fun engineControl(): EngineControl.Control {
        val link = savedLink()
        val viaTor = mode() == Mode.TOR
        val app = applicationContext
        return object : EngineControl.Control {
            override fun isOn() = if (viaTor) TorEngine.isRunning() else Native.isRunning()
            override fun setOn(on: Boolean) {
                if (viaTor) {
                    // Starting tor blocks on the network, and this can be
                    // called from the notification on the main thread.
                    if (on) Thread { TorEngine.start(app) }.start() else TorEngine.stop(app)
                } else {
                    if (on) Native.start(link, 0) else Native.stop()
                }
            }
        }
    }

    // ---- settings ----------------------------------------------------------

    private fun chooseMode() {
        val labels = arrayOf(
            getString(R.string.mode_tor),
            getString(R.string.mode_server),
        )
        MaterialAlertDialogBuilder(this)
            .setTitle(R.string.mode_title)
            .setItems(labels) { _, which ->
                when (which) {
                    0 -> switchTo(Mode.TOR)
                    1 -> askForLink()
                }
            }
            .setNegativeButton(CoreR.string.close, null)
            .show()
    }

    private fun switchTo(mode: Mode) {
        prefs().edit { putString(KEY_MODE, mode.name) }
        if (engineOn) setEngine(false)
        setEngine(true)
    }

    private fun askForLink() {
        val field = EditText(this).apply {
            setText(savedLink())
            hint = getString(R.string.link_hint)
            setSingleLine(false)
            maxLines = 4
        }
        MaterialAlertDialogBuilder(this)
            .setTitle(R.string.server_title)
            .setMessage(R.string.server_help)
            .setView(field)
            .setPositiveButton(R.string.save) { _, _ -> saveLink(field.text.toString().trim()) }
            .setNegativeButton(CoreR.string.close, null)
            .show()
    }

    /**
     * Store a link only if it parses.
     *
     * Saving a bad one would leave the app failing to start on every launch
     * with nothing to say; reporting it here points at the thing to fix while
     * the user still has it in front of them.
     */
    private fun saveLink(link: String) {
        if (link.isBlank()) return
        Native.describe(link)
            .onSuccess { description ->
                prefs().edit {
                    putString(KEY_LINK, link)
                    putString(KEY_MODE, Mode.SERVER.name)
                }
                toast(getString(R.string.server_saved, description))
                if (engineOn) setEngine(false)
                setEngine(true)
            }
            .onFailure { toast(getString(R.string.bad_link, it.message ?: "")) }
    }

    private fun prefs() = getSharedPreferences("veil", Context.MODE_PRIVATE)

    /** Tor by default: anonymity is what this app is for. */
    private fun mode(): Mode =
        runCatching { Mode.valueOf(prefs().getString(KEY_MODE, Mode.TOR.name)!!) }.getOrDefault(Mode.TOR)

    private fun savedLink() = prefs().getString(KEY_LINK, "").orEmpty()

    private companion object {
        const val KEY_LINK = "link"
        const val KEY_MODE = "mode"
        const val NO_SERVER = -100
        const val NO_TOR = -101

        /** Tor is coming up; the answer arrives from the background thread. */
        const val TOR_STARTING = -102
    }
}
