package net.sw.browser

import android.net.Uri

/** A downloadable stream spotted while a page loaded. */
data class Media(
    val url: String,
    val kind: Kind,
    /** Page title at the moment it appeared, used to name the file. */
    val title: String,
    /** Headers the page itself sent. Many hosts reject a request without the
     *  matching Referer, so the capture has to carry them along. */
    val headers: Map<String, String>,
    /**
     * The audio stream, when the video arrives without one.
     *
     * YouTube sends the two separately above 360p. Carrying the pair together
     * keeps them from being chosen independently and mismatched.
     */
    val audioUrl: String? = null,
    val expectedVideoBytes: Long = 0,
    val expectedAudioBytes: Long = 0,
    /**
     * How to ask YouTube for this, since there is no address to fetch.
     *
     * Carried on the download rather than looked up later because the template
     * is only obtainable from a page that is still playing the video, and by
     * the time a queued download runs the user has usually moved on.
     */
    val sabr: SabrRequest? = null,
    /**
     * Keep only the sound, saved as a music file rather than a video.
     *
     * The video half of the request is left out, and what comes back goes to
     * the phone's Music folder as an .m4a instead of being joined into a clip.
     */
    val audioOnly: Boolean = false,
    /** A thumbnail URL to save as the song's cover art, when the page had one. */
    val cover: String? = null,
) {
    /** A YouTube download: a captured request plus the pair to pull through it. */
    data class SabrRequest(
        val template: Sabr.Template,
        val video: Sabr.Track,
        val audio: Sabr.Track,
        /** Any other format: named as "already playing" so these two are not. */
        val decoy: Sabr.Track,
    )

    enum class Kind {
        FILE,
        HLS,
        /** A video stream and an audio stream that have to be joined. */
        PAIR,
        /** YouTube: the server decides what to send, so it has to be asked. */
        SABR,
    }

    /**
     * The name the server gave the file, when there is one.
     *
     * Preferred over the page title for naming a download: the main resource is
     * intercepted before the page reports its title, so at that moment the
     * title still belongs to the *previous* page. The filename is right there
     * in the URL and is never stale.
     */
    val fileName: String?
        get() = Uri.parse(url).lastPathSegment
            ?.takeIf { it.substringAfterLast('.', "").length in 2..4 }

    val label: String
        get() = when {
            kind == Kind.HLS -> "$title  ·  스트림"
            fileName != null -> fileName!!
            else -> title
        }
}

/**
 * Watches requests going through the web view and remembers the ones that look
 * like video.
 *
 * A page never announces "here is the video" — it just fetches it. So the only
 * reliable way to offer a download is to notice the fetch as it happens, which
 * is what makes an in-app browser worth having: outside it, those URLs are
 * invisible.
 */
class MediaCatcher {

    private val found = LinkedHashMap<String, Media>()

    val items: List<Media>
        get() = synchronized(found) { found.values.reversed() }

    val count: Int
        get() = synchronized(found) { found.size }

    fun clear() = synchronized(found) { found.clear() }

    /** Record [url] if it looks like a video. Returns true if it was new. */
    fun offer(url: String, title: String, headers: Map<String, String>): Boolean {
        val kind = classify(url) ?: return false
        val key = dedupeKey(url)
        return synchronized(found) {
            if (found.containsKey(key)) false
            else {
                found[key] = Media(url, kind, title.ifBlank { "영상" }, headers)
                true
            }
        }
    }

    private companion object {
        val FILE_EXTENSIONS = listOf(".mp4", ".webm", ".m4v", ".mov", ".mkv", ".mp3", ".m4a")

        /**
         * Playlists and whole files are worth offering; the segments a playlist
         * points at are not — they are fragments of something already listed,
         * and a page can fetch hundreds of them.
         */
        fun classify(url: String): Media.Kind? {
            val path = (Uri.parse(url).path ?: "").lowercase()
            return when {
                path.endsWith(".m3u8") -> Media.Kind.HLS
                FILE_EXTENSIONS.any { path.endsWith(it) } -> Media.Kind.FILE
                // YouTube-style delivery has no extension at all.
                path.contains("videoplayback") -> Media.Kind.FILE
                else -> null
            }
        }

        /**
         * Signed media URLs carry expiring tokens and byte ranges in the query,
         * so the same video reappears under many URLs. Keying on the path alone
         * collapses them into one entry.
         */
        fun dedupeKey(url: String): String {
            val uri = Uri.parse(url)
            return "${uri.host}${uri.path}"
        }
    }
}
