package net.sw.browser

import java.net.HttpURLConnection
import java.net.URL

/**
 * Turning "the user pressed on this video" into a list of things to download.
 *
 * The hard part is not fetching, it is *identification*, and the evidence comes
 * from two places that each miss what the other sees:
 *
 * - The request interceptor sees what JavaScript fetched — playlists, preview
 *   clips, adverts — but never what the media element fetched. WebView routes
 *   media through its own stack, so a site that streams an MP4 straight into
 *   the player is invisible to it.
 * - The page's own Resource Timing sees everything, media included, which is
 *   the only way those URLs can be found at all.
 *
 * Neither says which one is the feature. Size does: the feature is the big one,
 * adverts and previews are one or two orders of magnitude smaller, and the
 * byte-sized files are not video. So every candidate is asked how large it is
 * and the list is offered largest first, with the size written next to it —
 * which is also the answer to "let me choose the quality", because for a plain
 * file the size *is* the quality.
 */
object Qualities {

    data class Option(
        /** The line the user reads: a resolution, or a filename. */
        val title: String,
        /** The line under it: size, bitrate, kind. */
        val detail: String,
        val url: String,
        val kind: Media.Kind,
        val headers: Map<String, String>,
        /** Bytes, or -1 when the server would not say. */
        val size: Long = -1,
        /** Bits per second, for a stream whose size has to be worked out. */
        val bitrate: Long = 0,
        /** True when [size] came from duration times bitrate, not from a server. */
        val estimated: Boolean = false,
        /** Set when the video has no sound of its own and needs joining. */
        val audioUrl: String? = null,
        val videoBytes: Long = 0,
        val audioBytes: Long = 0,
        /** The name to save under, when the page supplied one. */
        val pageTitle: String = "",
        /** Set for YouTube, where a download is a conversation and not a URL. */
        val sabr: Media.SabrRequest? = null,
        /** The sound on its own, saved as a music file. */
        val audioOnly: Boolean = false,
        /** A thumbnail to stand in as the song's cover, when there is one. */
        val cover: String? = null,
    )

    /** Below this a file is a preview or a stub, not something to watch. */
    private const val MIN_VIDEO_BYTES = 2 * 1024 * 1024L

    /** Probing costs a request each, and beyond this the list is unusable. */
    private const val MAX_CANDIDATES = 10

    /**
     * What can be downloaded for [target], largest first.
     *
     * Blocking: does network work, so call it off the main thread. Throws with
     * a readable message rather than returning nothing, so a failure can be
     * shown instead of looking like "no video here".
     */
    /**
     * YouTube's list, already paired with an audio track and sized.
     *
     * Separate from the rest because nothing has to be inferred: the page
     * states every format, its exact byte count and its codec, so the list is
     * built rather than guessed at.
     */
    /** Codec preference for a video track: AV1 (best quality-per-byte) beats H.264 (widest
     *  hardware decode) beats VP9. Unknown codecs sort last so a known one always wins. */
    private fun codecRank(codec: String): Int = when (codec) {
        "AV1" -> 0
        "H.264" -> 1
        "VP9" -> 2
        else -> 3
    }

    fun forYouTube(offer: YouTube.Offer, title: String): List<Option> {
        val template = offer.template ?: return emptyList()
        if (YouTube.bestAudio(offer.formats) == null) return emptyList()

        // One row per resolution, carrying the BEST codec of it: AV1 > H.264 > VP9.
        // YouTube offers each resolution in up to three codecs; showing all three made
        // the reader compare the same resolution three times. We keep the highest-priority
        // one that exists and drop the rest (if AV1 1080p exists, its H.264/VP9 siblings are
        // not listed). AV1 first for quality-per-byte, H.264 next for the widest hardware
        // decode, VP9 last. Bytes break a tie only within one codec. (bestAudio above already
        // guarantees the list is non-empty, so no codec ever empties it.)
        val tracks = YouTube.videoTracks(offer.formats)
            .groupBy { it.quality }
            .values
            .map { sameResolution ->
                sameResolution.minWithOrNull(
                    compareBy({ codecRank(it.codec) }, { it.bytes.coerceAtLeast(0) }))!!
            }
            .sortedByDescending { YouTube.heightOf(it.quality) }
        val options = tracks.mapNotNull { video ->
            val audio = YouTube.bestAudio(offer.formats, video.container) ?: return@mapNotNull null
            val total = video.bytes.coerceAtLeast(0) + audio.bytes.coerceAtLeast(0)
            val decoy = tracks.firstOrNull { it.itag != video.itag } ?: audio
            Option(
                title = video.quality.ifBlank { "영상" },
                detail = listOf(video.codec, audio.codec).filter { it.isNotBlank() }.joinToString(" + "),
                url = "",
                kind = Media.Kind.SABR,
                headers = emptyMap(),
                size = if (total > 0) total else -1,
                videoBytes = video.bytes.coerceAtLeast(0),
                audioBytes = audio.bytes.coerceAtLeast(0),
                pageTitle = title,
                sabr = Media.SabrRequest(template, video.asTrack(), audio.asTrack(), decoy.asTrack()),
            )
        }

        // Music on its own, at the top, at the best quality a phone can play.
        // The video half of the request is left out; the audio is its own file.
        val music = YouTube.bestMusicAudio(offer.formats)?.let { audio ->
            val decoy = tracks.firstOrNull()?.asTrack()
                ?: offer.formats.firstOrNull { it.itag != audio.itag }?.asTrack()
                ?: audio.asTrack()
            val bytes = audio.bytes.coerceAtLeast(0)
            Option(
                title = "음악만 저장",
                detail = listOfNotNull(
                    audio.codec.takeIf { it.isNotBlank() },
                    (audio.bitrate / 1000).takeIf { it > 0 }?.let { "${it}k" },
                ).joinToString(" · "),
                url = "",
                kind = Media.Kind.SABR,
                headers = emptyMap(),
                size = if (bytes > 0) bytes else -1,
                audioBytes = bytes,
                pageTitle = title,
                // The audio fills the video slot too; it is unused when only the
                // sound is fetched, and a non-null track is simpler than an
                // optional one everywhere it is read.
                sabr = Media.SabrRequest(template, audio.asTrack(), audio.asTrack(), decoy),
                audioOnly = true,
                cover = offer.cover.ifBlank { null },
            )
        }

        return (listOfNotNull(music) + options).map { describe(it) }
    }

    fun forTarget(target: VideoHook.VideoTarget, captured: List<Media>): List<Option> {
        val headers = captured.firstOrNull()?.headers ?: emptyMap()

        // What the page declares wins outright — the player is reading the same
        // list, so there is nothing left to infer.
        val expanded = if (target.declared.isNotEmpty()) {
            target.declared
                .filterNot { isAdvert(it.url) }
                .take(MAX_CANDIDATES)
                .flatMap { declared ->
                    expand(Media(declared.url, kindOf(declared.url), "", headers)).map { option ->
                        if (declared.quality.isNotBlank()) option.copy(title = "${declared.quality}p")
                        else option
                    }
                }
                .distinctBy { it.url }
        } else {
            candidatesFor(target, captured)
                .take(MAX_CANDIDATES)
                .flatMap { expand(it) }
                .distinctBy { it.url }
        }

        val sized = expanded.map { option ->
            if (option.kind == Media.Kind.HLS) option else option.copy(size = sizeOf(option))
        }
        val measured = estimateStreamSizes(sized)

        // Keep what is plausibly the feature. If that leaves nothing, show
        // everything rather than an empty list — being wrong about the
        // threshold should not look like finding nothing.
        val worthwhile = measured.filter { it.kind == Media.Kind.HLS || it.size < 0 || it.size >= MIN_VIDEO_BYTES }
        val chosen = worthwhile.ifEmpty { measured }

        return chosen.sortedByDescending { it.size }.map { describe(it) }
    }

    /**
     * Every URL that could be the pressed video, best evidence first.
     *
     * A named `src` settles it. Otherwise the page's own list comes before the
     * interceptor's, because the interceptor cannot see media requests at all
     * and its entries are usually previews.
     */
    fun candidatesFor(target: VideoHook.VideoTarget, captured: List<Media>): List<Media> {
        if (target.hasUsableSrc) {
            return listOf(Media(target.src, kindOf(target.src), "", headersFor(target.src, captured)))
        }
        val headers = captured.firstOrNull()?.headers ?: emptyMap()
        val fromPage = target.seen.asReversed().map { Media(it, kindOf(it), "", headers) }
        val all = (fromPage + captured).distinctBy { it.url }.filterNot { isAdvert(it.url) }

        return narrowTo(target.hints, target.titleWords, all)
    }

    /**
     * Whether this URL is an advert rather than the page's video.
     *
     * Pattern matching, so it is a filter and not a guarantee — an advert
     * served from the site's own CDN under an innocuous path is
     * indistinguishable from content, and only its size gives it away. What
     * this does remove is the obvious majority.
     */
    fun isAdvert(url: String): Boolean {
        val lower = url.lowercase()
        return AD_MARKERS.any { lower.contains(it) }
    }

    private val AD_MARKERS = listOf(
        "/ads/", "/ad/", "ads.", "adserver", "adservice", "/advert", "/banner",
        "doubleclick", "googlesyndication", "trafficjunky", "exoclick", "juicyads",
        "popads", "propellerads", "/preroll", "/midroll", "/promo/", "/sponsor",
    )

    /**
     * Keep only the URLs belonging to the video that was pressed.
     *
     * Identifiers first: a numeric video id in the markup usually appears in
     * the media URL too, and that match is exact. Title words are the fallback
     * for sites that build URLs out of a slug — weaker, so they are only
     * consulted when the ids matched nothing.
     *
     * When neither matches, the whole list stands. Plenty of sites name media
     * with no relation to the markup, and offering everything is recoverable
     * where offering nothing is a dead end.
     */
    fun narrowTo(hints: List<String>, titleWords: List<String>, candidates: List<Media>): List<Media> {
        fun matching(tokens: List<String>) = candidates.filter { media ->
            val url = media.url.lowercase()
            tokens.any { url.contains(it) }
        }

        if (hints.isNotEmpty()) {
            matching(hints).takeIf { it.isNotEmpty() }?.let { return it }
        }
        if (titleWords.isNotEmpty()) {
            matching(titleWords).takeIf { it.isNotEmpty() }?.let { return it }
        }
        return candidates
    }

    /**
     * Work out how large the streams are.
     *
     * A playlist states a bitrate but never a byte count, so the size comes
     * from duration times bitrate. Duration is the same for every rendition of
     * one video, so it is fetched once and applied to all of them — the
     * alternative is a request per quality, over a tunnel, before the list can
     * even be shown.
     */
    private fun estimateStreamSizes(options: List<Option>): List<Option> {
        val streams = options.filter { it.kind == Media.Kind.HLS && it.size < 0 && it.bitrate > 0 }
        if (streams.isEmpty()) return options

        val seconds = runCatching {
            val text = fetchText(streams.first().url, streams.first().headers)
            // A master points at the real playlist; follow it once.
            val playlist = if (Hls.isMaster(text)) {
                val first = Hls.variants(text, streams.first().url).firstOrNull() ?: return options
                fetchText(first.url, streams.first().headers)
            } else {
                text
            }
            Hls.durationSeconds(playlist)
        }.getOrDefault(0.0)

        if (seconds <= 0) return options
        return options.map { option ->
            if (option.kind == Media.Kind.HLS && option.size < 0 && option.bitrate > 0) {
                option.copy(size = (seconds * option.bitrate / 8).toLong(), estimated = true)
            } else {
                option
            }
        }
    }

    /** Fill in the two lines the dialog shows. */
    private fun describe(option: Option): Option {
        val parts = mutableListOf<String>()
        if (option.size > 0) {
            parts += if (option.estimated) "약 ${formatSize(option.size)}" else formatSize(option.size)
        }
        if (option.detail.isNotBlank()) parts += option.detail
        parts += when {
            option.audioOnly -> "음악"
            option.kind == Media.Kind.HLS -> "스트리밍"
            else -> "파일"
        }
        return option.copy(detail = parts.joinToString("  ·  "))
    }

    private fun formatSize(bytes: Long): String = when {
        bytes >= 1L shl 30 -> "%.1f GB".format(bytes.toDouble() / (1L shl 30))
        bytes >= 1L shl 20 -> "%.0f MB".format(bytes.toDouble() / (1L shl 20))
        else -> "%.0f KB".format(bytes.toDouble() / 1024)
    }

    /** A playlist expands into its renditions; anything else is one option. */
    private fun expand(media: Media): List<Option> {
        if (media.kind != Media.Kind.HLS) {
            val resolution = Regex("(\\d{3,4})p").find(media.url)?.groupValues?.get(1)
            val title = resolution?.let { "${it}p" } ?: nameOf(media.url)
            return listOf(Option(title, "", media.url, Media.Kind.FILE, media.headers))
        }

        // The rate is often written into the URL — `1080P_4000K_…` — which
        // saves fetching the playlist just to learn it.
        val declaredRate = Regex("_(\\d+)K").find(media.url)?.groupValues?.get(1)
            ?.toLongOrNull()?.times(1000) ?: 0

        // Even when the playlist cannot be read, the rate from the URL is
        // something to show — an entry with nothing next to it is not a choice.
        val plain = {
            Option(
                title = nameOf(media.url),
                detail = if (declaredRate > 0) "%.1f Mbps".format(declaredRate / 1_000_000.0) else "",
                url = media.url,
                kind = Media.Kind.HLS,
                headers = media.headers,
                bitrate = declaredRate,
            )
        }

        val text = runCatching { fetchText(media.url, media.headers) }.getOrNull() ?: return listOf(plain())
        if (!Hls.isMaster(text)) return listOf(plain())

        return Hls.variants(text, media.url)
            .map { variant ->
                Option(
                    title = if (variant.height > 0) "${variant.height}p" else nameOf(variant.url),
                    detail = if (variant.bandwidth > 0)
                        "%.1f Mbps".format(variant.bandwidth / 1_000_000.0) else "",
                    url = variant.url,
                    kind = Media.Kind.HLS,
                    headers = media.headers,
                    bitrate = if (variant.bandwidth > 0) variant.bandwidth else declaredRate,
                )
            }
            .ifEmpty { listOf(plain()) }
    }

    private fun nameOf(url: String): String {
        val segment = android.net.Uri.parse(url).lastPathSegment.orEmpty()
        return segment.takeIf { it.isNotBlank() }?.take(38) ?: "영상"
    }

    /**
     * How large the file is, without downloading it.
     *
     * HEAD first; many media hosts answer it with nothing useful, so a one-byte
     * ranged GET is the fallback — `Content-Range` carries the total length,
     * and a server that streams video always supports ranges.
     */
    private fun sizeOf(option: Option): Long {
        runCatching {
            val head = open(option.url, option.headers, method = "HEAD")
            val length = head.contentLengthLong
            head.disconnect()
            if (length > 0) return length
        }
        runCatching {
            val probe = open(option.url, option.headers, method = "GET", range = "bytes=0-0")
            val total = probe.getHeaderField("Content-Range")?.substringAfterLast('/')?.toLongOrNull()
            probe.disconnect()
            if (total != null && total > 0) return total
        }
        return -1
    }

    private fun kindOf(url: String) =
        if (Hls.isPlaylist(url)) Media.Kind.HLS else Media.Kind.FILE

    /**
     * Headers the page used for this URL, if the interceptor saw it.
     *
     * Many hosts refuse a request without the matching Referer, so a URL taken
     * from the page but fetched bare would 403 where the page succeeded.
     */
    private fun headersFor(url: String, captured: List<Media>): Map<String, String> =
        captured.firstOrNull { it.url == url }?.headers
            ?: captured.firstOrNull()?.headers
            ?: emptyMap()

    private fun open(
        url: String,
        headers: Map<String, String>,
        method: String = "GET",
        range: String? = null,
    ): HttpURLConnection {
        val connection = URL(url).openConnection(ProxyRoute.forDownloads()) as HttpURLConnection
        connection.requestMethod = method
        PageContext.current.applyTo(headers).forEach { (name, value) ->
            if (!name.equals("Host", true) && !name.equals("Range", true) &&
                !name.equals("Accept-Encoding", true)
            ) {
                connection.setRequestProperty(name, value)
            }
        }
        range?.let { connection.setRequestProperty("Range", it) }
        connection.connectTimeout = 8_000
        connection.readTimeout = 8_000
        connection.instanceFollowRedirects = true
        return connection
    }

    private fun fetchText(url: String, headers: Map<String, String>): String =
        open(url, headers).inputStream.use { it.readBytes().toString(Charsets.UTF_8) }
}
