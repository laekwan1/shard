package net.veil

import org.json.JSONObject

/**
 * The tunnel.
 *
 * It runs inside this process. The first attempt shipped the sing-box binary
 * and launched it, which Android refuses — the core wants a netlink socket to
 * watch the network and an ordinary app is not allowed one, so it started and
 * exited immediately. A library needs no permission to exist.
 *
 * Everything that decides how traffic is treated is on the other side of this
 * boundary and shared with the desktop and the server, so a link that works on
 * the PC produces the same tunnel here.
 */
object Native {

    init {
        System.loadLibrary("veil_mobile")
        initLogging()
    }

    /** Send the tunnel's tracing output to logcat under the tag `veil`. */
    external fun initLogging()

    /** Bound port, or a negative code. */
    external fun start(link: String, port: Int): Int

    external fun stop()

    external fun isRunning(): Boolean

    external fun statsJson(): String

    /** A description of the link, or a string starting with `error:`. */
    external fun describeLink(link: String): String

    /** Why [start] refused, in words the user can act on. */
    fun reasonFor(code: Int): String = when (code) {
        BAD_LINK -> "링크를 읽을 수 없습니다"
        ALREADY_RUNNING -> "이미 실행 중입니다"
        else -> "터널을 시작할 수 없습니다"
    }

    fun describe(link: String): Result<String> {
        val text = describeLink(link)
        return if (text.startsWith("error:")) {
            Result.failure(IllegalArgumentException(text.removePrefix("error:").trim()))
        } else {
            Result.success(text)
        }
    }

    data class Stats(
        val connections: Long = 0,
        val tunnelled: Long = 0,
        val direct: Long = 0,
        val failed: Long = 0,
        val bytesUp: Long = 0,
        val bytesDown: Long = 0,
    )

    /** Parse the counters, tolerating anything unexpected rather than crashing. */
    fun stats(): Stats = try {
        val json = JSONObject(statsJson())
        Stats(
            connections = json.optLong("connections"),
            tunnelled = json.optLong("tunnelled"),
            direct = json.optLong("direct"),
            failed = json.optLong("failed"),
            bytesUp = json.optLong("bytesUp"),
            bytesDown = json.optLong("bytesDown"),
        )
    } catch (e: Exception) {
        Stats()
    }

    const val BAD_LINK = -1
    const val ALREADY_RUNNING = -2
}
