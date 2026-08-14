package net.shard

import net.sw.browser.BrowserActivity
import net.sw.browser.EngineControl

/**
 * Shard: the browser with the desync engine underneath.
 *
 * The engine is the same Rust code the Windows build runs — same ClientHello
 * parser, same domain rules, same strategy model — reproduced with what a
 * socket can do rather than what a packet driver can. It costs no extra hop and
 * no measurable bandwidth, and provides no anonymity; that is Veil's job.
 */
class MainActivity : BrowserActivity() {

    // Port 0 asks the OS for a free one, so two runs never collide.
    override fun startEngine(): Int = Native.start(filesDir.absolutePath, 0)

    override fun stopEngine() = Native.stop()

    override fun engineStatus(): String = "우회 중"

    override fun bytesReceived(): Long = Native.stats().bytesDown

    /** Holds only the application context, so the notification can outlive us. */
    /**
     * Off when the app opens.
     *
     * Bypassing is something to reach for, not something to be doing by
     * default: an engine running unasked rewrites every connection the browser
     * makes, and a page that misbehaves because of it looks like a broken page.
     */
    override val autoStart: Boolean get() = false

    override fun engineControl(): EngineControl.Control {
        val files = applicationContext.filesDir.absolutePath
        return object : EngineControl.Control {
            override fun isOn() = Native.isRunning()
            override fun setOn(on: Boolean) {
                if (on) Native.start(files, 0) else Native.stop()
            }
        }
    }
}
