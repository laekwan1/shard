package net.shard

import org.json.JSONObject

/**
 * The Rust engine.
 *
 * Everything that decides *how* traffic is treated lives on the other side of
 * this boundary and is shared with the desktop build — the ClientHello parser,
 * the domain rules, the desync itself. Kotlin only supplies the things Android
 * alone can provide: the TUN interface, the UI, and the lifecycle.
 */
object Native {

    init {
        System.loadLibrary("shard_mobile")
        initLogging()
    }

    /** Send the engine's tracing output to logcat under the tag `shard`. */
    external fun initLogging()

    /** Bound port, or a negative error code. */
    external fun start(configDir: String?, port: Int): Int

    external fun stop()

    external fun isRunning(): Boolean

    /** Counters as JSON, released on the Rust side after copying. */
    external fun statsJson(): String

    data class Stats(
        val connections: Long = 0,
        val desynced: Long = 0,
        val passedThrough: Long = 0,
        val failed: Long = 0,
        val bytesUp: Long = 0,
        val bytesDown: Long = 0,
    )

    /** Parse the counters, tolerating anything unexpected rather than crashing. */
    fun stats(): Stats = try {
        val json = JSONObject(statsJson())
        Stats(
            connections = json.optLong("connections"),
            desynced = json.optLong("desynced"),
            passedThrough = json.optLong("passedThrough"),
            failed = json.optLong("failed"),
            bytesUp = json.optLong("bytesUp"),
            bytesDown = json.optLong("bytesDown"),
        )
    } catch (e: Exception) {
        Stats()
    }

    /** Human-readable byte count for the UI. */
    fun formatBytes(bytes: Long): String {
        val units = listOf("GB" to (1L shl 30), "MB" to (1L shl 20), "KB" to (1L shl 10))
        for ((unit, scale) in units) {
            if (bytes >= scale) return String.format("%.1f %s", bytes.toDouble() / scale, unit)
        }
        return "$bytes B"
    }
}
