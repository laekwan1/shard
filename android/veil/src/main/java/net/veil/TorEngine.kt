package net.veil

import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.ServiceConnection
import android.os.IBinder
import androidx.localbroadcastmanager.content.LocalBroadcastManager
import org.torproject.jni.TorService
import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * Tor, inside the app.
 *
 * Orbot was the first attempt and it asked too much: recent versions no longer
 * expose the SOCKS port to other apps at all, so "install this other program
 * and find a setting that has been removed" was the whole experience.
 *
 * This runs the same tor the Guardian Project ships to Orbot, in this process.
 * Nothing about the anonymity changes by moving it here — that comes from the
 * network and from behaving like everyone else, not from which app started the
 * daemon. What does change is that there is one app instead of two.
 */
object TorEngine {

    /** Where tor listens once it is up. The library's default. */
    const val SOCKS_PORT = 9050

    /** Bootstrapping over a slow link genuinely takes this long. */
    private const val START_TIMEOUT_SECONDS = 90L

    @Volatile
    private var connection: ServiceConnection? = null

    @Volatile
    var lastError: String = ""
        private set

    /**
     * Whether the port was open last time anything looked.
     *
     * Cached because this is read from the main thread — the status line asks
     * every second — and Android kills any thread that opens a socket there.
     * The probe itself only ever runs in the background.
     */
    @Volatile
    private var portOpen = false

    fun isRunning(): Boolean = portOpen

    /** Opens a socket: never call this from the main thread. */
    private fun probe(): Boolean {
        portOpen = runCatching {
            Socket().use { it.connect(InetSocketAddress("127.0.0.1", SOCKS_PORT), 1200) }
        }.isSuccess
        return portOpen
    }

    /**
     * Start tor and wait for it to be usable.
     *
     * Blocking on purpose: the caller has to know whether to point the browser
     * at the port, and "probably up" is not something to hand a browser.
     */
    fun start(context: Context): Boolean {
        if (probe()) return true
        lastError = ""

        val ready = CountDownLatch(1)
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context?, intent: Intent?) {
                when (intent?.getStringExtra(TorService.EXTRA_STATUS)) {
                    TorService.STATUS_ON -> ready.countDown()
                    TorService.STATUS_OFF -> {
                        lastError = "tor가 종료되었습니다"
                        ready.countDown()
                    }
                }
            }
        }
        val broadcasts = LocalBroadcastManager.getInstance(context.applicationContext)
        broadcasts.registerReceiver(receiver, IntentFilter(TorService.ACTION_STATUS))

        val binding = object : ServiceConnection {
            override fun onServiceConnected(name: ComponentName?, binder: IBinder?) = Unit
            override fun onServiceDisconnected(name: ComponentName?) = Unit
        }

        return try {
            context.applicationContext.bindService(
                Intent(context.applicationContext, TorService::class.java),
                binding,
                Context.BIND_AUTO_CREATE,
            )
            connection = binding

            // The socket decides. The broadcast is watched too, but it is
            // sent within the process that sends it — and tor runs in one of
            // its own now — so waiting for it would mean waiting the whole
            // timeout every time. A port that is open is proof either way.
            waitForPort(ready)
        } catch (e: Exception) {
            lastError = e.message.orEmpty()
            false
        } finally {
            runCatching { broadcasts.unregisterReceiver(receiver) }
        }
    }

    private fun waitForPort(ready: CountDownLatch): Boolean {
        val steps = (START_TIMEOUT_SECONDS * 1000 / POLL_MS).toInt()
        repeat(steps) {
            if (probe()) return true
            // A daemon that has said it stopped is not going to open a port.
            if (ready.await(POLL_MS, TimeUnit.MILLISECONDS) && lastError.isNotEmpty()) {
                return false
            }
        }
        if (lastError.isEmpty()) lastError = "tor가 시간 안에 준비되지 않았습니다"
        return false
    }

    /** How often the port is tried while waiting for the daemon. */
    private const val POLL_MS = 500L

    fun stop(context: Context) {
        connection?.let { runCatching { context.applicationContext.unbindService(it) } }
        connection = null
        portOpen = false
    }
}
