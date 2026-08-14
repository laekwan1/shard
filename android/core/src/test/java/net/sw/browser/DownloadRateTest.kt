package net.sw.browser

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Whether the speed shown is the speed the line is delivering.
 *
 * It was not. The rate was worked out between one progress callback and the
 * next, and those arrive a chunk at a time — often a few milliseconds apart.
 * Over a gap that short what gets measured is how fast a chunk leaves the
 * socket's buffer, and a gap of "1 ms" that was really 1.4 ms reads forty per
 * cent high before anything else happens. A 21 Mbit line carries 2.6 MB/s at
 * most; the display was saying 3 to 5.
 */
class DownloadRateTest {

    private fun job() = Downloads.Job(1, "test")

    /** One megabyte a second, delivered evenly, reads as one megabyte a second. */
    @Test
    fun `an even megabyte per second reads as one`() {
        val job = job()
        val chunk = 1L shl 20
        var at = 1_000L
        var bytes = 0L
        job.advance(0, 0, at)
        repeat(10) {
            at += 1_000
            bytes += chunk
            job.advance(bytes, 0, at)
        }
        assertEquals(chunk.toDouble(), job.rate, chunk * 0.05)
    }

    /**
     * The same megabyte, delivered in bursts a few milliseconds apart, still
     * reads as one megabyte a second — not as the speed of a burst.
     */
    @Test
    fun `bursts do not read faster than the line carrying them`() {
        val job = job()
        val second = 1L shl 20
        var at = 1_000L
        var bytes = 0L
        // Ten seconds of traffic: each second is one megabyte, arriving as
        // sixteen chunks in the first 80 ms and nothing for the rest of it.
        repeat(10) {
            repeat(16) {
                at += 5
                bytes += second / 16
                job.advance(bytes, 0, at)
            }
            at += 920
            job.advance(bytes, 0, at)
        }
        assertTrue(
            "a burst was reported as the line speed: ${job.rate} B/s against ${second} B/s",
            job.rate <= second * 1.25,
        )
    }

    /** A stream that restarts reports fewer bytes; that is not negative speed. */
    @Test
    fun `a restarted stream does not produce a negative rate`() {
        val job = job()
        var at = 1_000L
        job.advance(0, 0, at)
        at += 1_000
        job.advance(1L shl 20, 0, at)
        val before = job.rate
        at += 1_000
        job.advance(0, 0, at)
        assertEquals(before, job.rate, 0.001)
        assertTrue(job.rate > 0)
    }

    /** Progress itself is recorded every time, whatever the rate is doing. */
    @Test
    fun `progress is recorded on every report`() {
        val job = job()
        job.advance(0, 100, 1_000)
        job.advance(40, 100, 1_010)
        assertEquals(40L, job.done)
        assertEquals(40, job.percent!!)
    }
}
