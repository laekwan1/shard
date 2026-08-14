package net.sw.browser

import java.net.URL

/**
 * HLS playlists.
 *
 * A streaming "video" is a playlist of segments, and a *master* playlist is a
 * list of those playlists at different bitrates. That list is where the quality
 * choice comes from — the page's own player picks one silently, and this is
 * what lets the user pick instead.
 */
object Hls {

    /** One rendition from a master playlist. */
    data class Variant(
        val url: String,
        /** Pixels, when the playlist declares them. */
        val width: Int = 0,
        val height: Int = 0,
        /** Bits per second, when declared. */
        val bandwidth: Long = 0,
    ) {
        /** What the user sees in the list. */
        val label: String
            get() {
                val quality = if (height > 0) "${height}p" else "화질 미상"
                val rate = if (bandwidth > 0) " · %.1f Mbps".format(bandwidth / 1_000_000.0) else ""
                return quality + rate
            }
    }

    fun isPlaylist(url: String): Boolean = url.substringBefore('?').endsWith(".m3u8", ignoreCase = true)

    /** True when `text` lists renditions rather than segments. */
    fun isMaster(text: String): Boolean = text.contains("#EXT-X-STREAM-INF")

    /**
     * Renditions in a master playlist, highest quality first.
     *
     * A rendition line and its URL are on separate lines, so the two have to be
     * read together; an attribute line with nothing after it is skipped rather
     * than paired with the wrong URL.
     */
    fun variants(text: String, baseUrl: String): List<Variant> {
        val lines = text.lines()
        val found = mutableListOf<Variant>()

        lines.forEachIndexed { index, line ->
            if (!line.startsWith("#EXT-X-STREAM-INF")) return@forEachIndexed
            val target = lines.drop(index + 1).firstOrNull { it.isNotBlank() && !it.startsWith("#") }
                ?: return@forEachIndexed

            val resolution = Regex("RESOLUTION=(\\d+)x(\\d+)").find(line)
            found += Variant(
                url = resolve(baseUrl, target.trim()),
                width = resolution?.groupValues?.get(1)?.toIntOrNull() ?: 0,
                height = resolution?.groupValues?.get(2)?.toIntOrNull() ?: 0,
                bandwidth = Regex("BANDWIDTH=(\\d+)").find(line)?.groupValues?.get(1)?.toLongOrNull() ?: 0,
            )
        }

        // Distinct by URL: a playlist often lists the same rendition twice with
        // different audio groups, and offering it twice is just confusing.
        return found
            .distinctBy { it.url }
            .sortedWith(compareByDescending<Variant> { it.height }.thenByDescending { it.bandwidth })
    }

    /**
     * How long a media playlist runs, in seconds.
     *
     * The playlist never states a byte count, but duration times bitrate is a
     * good enough figure to choose by — and choosing is the only thing the
     * number is for.
     */
    fun durationSeconds(text: String): Double =
        Regex("#EXTINF:([\\d.]+)").findAll(text)
            .mapNotNull { it.groupValues[1].toDoubleOrNull() }
            .sum()

    /** Segment URLs from a media playlist, in play order. */
    fun segments(text: String, baseUrl: String): List<String> =
        text.lines()
            .map { it.trim() }
            .filter { it.isNotEmpty() && !it.startsWith("#") }
            .map { resolve(baseUrl, it) }

    /** Resolve a possibly relative playlist entry against the playlist's URL. */
    fun resolve(base: String, reference: String): String = try {
        URL(URL(base), reference).toString()
    } catch (e: Exception) {
        reference
    }
}
