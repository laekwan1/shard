package net.sw.browser

import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicLong

/**
 * What is downloading, right now, for anything that wants to show it.
 *
 * Process-wide rather than owned by a screen: the work outlives the screen, and
 * both the browser panel and the notification have to read the same numbers or
 * they will disagree in front of the user.
 */
object Downloads {

    enum class State { RUNNING, DONE, FAILED }

    class Job(val id: Long, val name: String) {
        @Volatile var total: Long = -1
        @Volatile var done: Long = 0
        /** Bytes per second over the last sample. */
        @Volatile var rate: Double = 0.0
        @Volatile var state: State = State.RUNNING
        @Volatile var error: String = ""

        /**
         * Set when the user asks for this to stop.
         *
         * A flag rather than an interrupted thread: the download is a loop of
         * whole requests, and stopping between two of them leaves the partial
         * file in a known state to delete. Killing a thread mid-write does not.
         */
        @Volatile var cancelled: Boolean = false

        /**
         * True once the bytes are in and the two streams are being joined.
         *
         * Worth its own flag because the progress bar is already full by then:
         * without something to say, a download that is still working looks like
         * one that has stopped.
         */
        @Volatile var joining: Boolean = false

        /** 0..100, or null when the total is unknown. */
        val percent: Int?
            get() = if (total > 0) ((done * 100 / total).toInt()).coerceIn(0, 100) else null

        /** Seconds left at the current rate, or null when it cannot be known. */
        val secondsLeft: Long?
            get() {
                if (total <= 0 || rate <= 1.0) return null
                val remaining = total - done
                return if (remaining > 0) (remaining / rate).toLong() else 0
            }

        private var lastBytes = 0L
        private var lastAt = 0L

        /**
         * How long a stretch the rate is measured over.
         *
         * The rate used to be worked out between one progress callback and the
         * next, and those arrive a chunk at a time — often a few milliseconds
         * apart. Over a gap that short what is measured is how fast a chunk
         * comes out of the socket's buffer, not how fast the line is delivering
         * it; and a gap of "1 ms" that was really 1.4 ms reads forty per cent
         * high on its own. The two together are why a 21 Mbit line, which
         * cannot carry more than 2.6 MB/s, was being reported at 3 to 5.
         *
         * Half a second is long enough for the bursts to average out and short
         * enough that the number still follows what is happening.
         */
        private val OVER_MS = 500L

        /**
         * Record progress, and work out the rate over the last half second.
         *
         * [now] must come from a clock that only goes forwards —
         * `SystemClock.elapsedRealtime`, not the wall clock, which can be
         * adjusted underneath a download and take the rate with it.
         */
        fun advance(bytes: Long, total: Long, now: Long) {
            if (total > 0) this.total = total
            done = bytes
            if (lastAt == 0L) {
                lastAt = now
                lastBytes = bytes
                return
            }
            val span = now - lastAt
            if (span < OVER_MS) return

            // A stream that restarts reports fewer bytes than last time. That is
            // not negative speed; it is a measurement to throw away.
            val moved = bytes - lastBytes
            if (moved >= 0) {
                val sampled = moved * 1000.0 / span
                // Smoothed: even over half a second the number swings, and it is
                // being read by a person rather than a machine.
                rate = if (rate <= 0) sampled else rate * 0.6 + sampled * 0.4
            }
            lastBytes = bytes
            lastAt = now
        }
    }

    private val jobs = CopyOnWriteArrayList<Job>()
    private val nextId = AtomicLong(1)

    /** Newest last, which is the order they should be shown in. */
    val snapshot: List<Job> get() = jobs.toList()

    val running: List<Job> get() = jobs.filter { it.state == State.RUNNING }

    fun start(name: String): Job {
        val job = Job(nextId.getAndIncrement(), name)
        jobs.add(job)
        return job
    }

    fun finish(job: Job, error: String? = null) {
        job.state = if (error == null) State.DONE else State.FAILED
        job.error = error.orEmpty()
        if (error == null && job.total > 0) job.done = job.total
    }

    fun forget(job: Job) {
        jobs.remove(job)
    }

    /**
     * Ask a download to stop.
     *
     * The row disappears at once even though the worker may take a moment to
     * notice: it is between requests, and waiting for it would make the button
     * feel broken. Nothing is saved, so there is nothing left behind to explain.
     */
    fun cancel(job: Job) {
        job.cancelled = true
        jobs.remove(job)
    }

    /** Overall progress across everything running, for a single summary bar. */
    fun overallPercent(): Int? {
        val active = running
        if (active.isEmpty()) return null
        val known = active.filter { it.total > 0 }
        if (known.isEmpty()) return null
        val done = known.sumOf { it.done }
        val total = known.sumOf { it.total }
        return if (total > 0) ((done * 100 / total).toInt()).coerceIn(0, 100) else null
    }

    // ---- formatting --------------------------------------------------------

    fun bytes(value: Long): String = when {
        value >= 1L shl 30 -> "%.1f GB".format(value.toDouble() / (1L shl 30))
        value >= 1L shl 20 -> "%.0f MB".format(value.toDouble() / (1L shl 20))
        value >= 1024 -> "%.0f KB".format(value.toDouble() / 1024)
        else -> "$value B"
    }

    fun rate(bytesPerSecond: Double): String = when {
        bytesPerSecond >= 1 shl 20 -> "%.1f MB/s".format(bytesPerSecond / (1 shl 20))
        bytesPerSecond >= 1024 -> "%.0f KB/s".format(bytesPerSecond / 1024)
        else -> "—"
    }

    /** A duration in the terms someone waiting would use. */
    fun remaining(seconds: Long?): String = when {
        seconds == null -> "—"
        seconds >= 3600 -> "남은 %d시간 %d분".format(seconds / 3600, (seconds % 3600) / 60)
        seconds >= 60 -> "남은 %d분 %d초".format(seconds / 60, seconds % 60)
        else -> "남은 %d초".format(seconds)
    }
}
