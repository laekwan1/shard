package net.sw.browser

import android.content.Context
import android.net.Uri
import java.io.File
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.Executors

/**
 * Fetches a captured stream to storage, through the same engine the browser
 * uses.
 *
 * Going through the local proxy matters: a download is just another connection,
 * and if it left the app directly it would be blocked exactly like the page
 * was. Routing it back through the engine means whatever made the page load is
 * also what makes the file arrive.
 */
class Downloader(private val context: Context) {

    /** Progress in bytes, plus a fraction when the total is knowable. */
    data class Progress(val bytes: Long, val fraction: Float?)

    class Failed(message: String, cause: Throwable? = null) : Exception(message, cause)

    /**
     * Download [media], reporting progress. Returns where the file landed.
     *
     * Runs on the calling thread and blocks; call it off the main thread.
     */
    fun fetch(
        media: Media,
        cancelled: () -> Boolean = { false },
        onProgress: (Progress) -> Unit,
    ): String {
        this.cancelled = cancelled
        return when (media.kind) {
            Media.Kind.FILE -> fetchFile(media, onProgress)
            Media.Kind.HLS -> fetchHls(media, onProgress)
            Media.Kind.PAIR -> fetchPair(media, onProgress)
            Media.Kind.SABR -> fetchSabr(media, onProgress)
        }
    }

    /**
     * Asked between requests and between chunks.
     *
     * Held on the downloader rather than threaded through every method: it is
     * the same answer everywhere and passing it down five call sites would say
     * nothing the name does not.
     */
    private var cancelled: () -> Boolean = { false }

    /** What the download is busy with, for something to say while it waits. */
    enum class Stage { FETCHING, JOINING }

    var onStage: (Stage) -> Unit = {}

    /** Thrown when the user stopped it, so the caller can tell that apart. */
    class Cancelled : Exception("취소했습니다")

    private fun stopIfCancelled() {
        if (cancelled()) throw Cancelled()
    }

    /**
     * YouTube: two conversations rather than two downloads.
     *
     * The video and the audio are pulled separately even though one request
     * returns both interleaved, because the server decides how much of each to
     * send and asking for one at a time is what keeps the two from arriving at
     * different points in the video. The result is the same pair of files the
     * muxer already knows how to join.
     */
    private fun fetchSabr(media: Media, onProgress: (Progress) -> Unit): String {
        val request = media.sabr ?: throw Failed("YouTube 요청 정보가 없습니다")
        val scratch = File(context.cacheDir, "mux").apply { mkdirs() }
        val videoFile = File(scratch, "v-${System.nanoTime()}")
        val audioFile = File(scratch, "a-${System.nanoTime()}")
        val merged = File(scratch, "m-${System.nanoTime()}")

        try {
            if (media.audioOnly) {
                val audioBytes = media.expectedAudioBytes.coerceAtLeast(1).toFloat()
                val (_, gotAudio) = Sabr.fetch(
                    request.template, request.video, request.audio, request.decoy,
                    videoFile, audioFile,
                    onBytes = { _, a -> onProgress(Progress(a, a / audioBytes)) },
                    cancelled = cancelled,
                    audioOnly = true,
                )
                complain("음악", gotAudio, request.audio.bytes)
                onStage(Stage.JOINING)
                return saveAudio(media, audioFile)
            }

            val videoBytes = media.expectedVideoBytes.coerceAtLeast(1)
            val audioBytes = media.expectedAudioBytes.coerceAtLeast(1)
            val whole = (videoBytes + audioBytes).toFloat()

            // One conversation, both streams: a reply carries video and audio
            // interleaved, so asking twice would download each of them twice.
            val (gotVideo, gotAudio) = Sabr.fetch(
                request.template, request.video, request.audio, request.decoy,
                videoFile, audioFile,
                onBytes = { v, a -> onProgress(Progress(v + a, (v + a) / whole)) },
                cancelled = cancelled,
            )
            // A short stream is a broken file, and one that plays for a while
            // before stopping is worse than one that never appears — so this
            // fails rather than saving what arrived. The server decides what to
            // send, and it can decide to send something else entirely.
            complain("영상", gotVideo, request.video.bytes)
            complain("음성", gotAudio, request.audio.bytes)

            onStage(Stage.JOINING)
            return joinInto(media, videoFile, audioFile)
        } finally {
            listOf(videoFile, audioFile, merged).forEach { it.delete() }
        }
    }

    /**
     * Join the two streams directly into their destination.
     *
     * The earlier version built the file in the cache and copied it across
     * afterwards, which on a large download was most of the wait after the
     * progress bar had already reached the end — the file existed, finished,
     * somewhere the user could not see it.
     */
    private fun joinInto(media: Media, videoFile: File, audioFile: File): String {
        val extension = Muxer.planExtension(videoFile, audioFile)
        val sink = Storage.createFile(context, nameFor(media, extension), mimeFor(extension))
        var ok = false
        try {
            Muxer.combineInto(videoFile, audioFile, sink.descriptor.fileDescriptor)
            ok = true
            return sink.where
        } finally {
            sink.close()
            // A failed join leaves a partial file where downloads are kept, and
            // a video that opens to nothing is worse than one that never
            // appeared.
            if (!ok) Storage.remove(context, sink)
        }
    }

    /**
     * Put a music-only stream in the Music folder as it is.
     *
     * The stream is already a container a player will open, so it is copied, not
     * rebuilt — nothing is re-encoded. Which container decides the name: an AAC
     * track is an .m4a every phone plays, but where a video offered only Opus
     * that track is WebM, and calling it .m4a would be a file that opens to
     * nothing. The bytes are read for what they are, the same judgement the
     * desktop build makes. It goes to the phone's Music folder, not beside the
     * videos, because that is where a music app looks.
     */
    private fun saveAudio(media: Media, audioFile: File): String {
        val webm = looksWebm(audioFile)
        val extension = if (webm) "webm" else "m4a"
        val mime = if (webm) "audio/webm" else "audio/mp4"
        val name = nameFor(media, extension)
        val sink = Storage.createMusicFile(context, name, mime)
        var ok = false
        try {
            java.io.FileInputStream(audioFile).use { input ->
                java.io.FileOutputStream(sink.descriptor.fileDescriptor).use { output ->
                    input.copyTo(output)
                }
            }
            ok = true
            // The song has no picture of its own, so the video's thumbnail stands
            // in as its cover in the library. Best effort — a cover that will not
            // fetch is a blank tile, not a failed download.
            media.cover?.takeIf { it.isNotBlank() }?.let { saveCover(name, it) }
            return sink.where
        } finally {
            sink.close()
            if (!ok) Storage.remove(context, sink)
        }
    }

    /**
     * Fetch the song's cover straight from the thumbnail host.
     *
     * A plain request, not the download proxy: the thumbnail lives on a public
     * CDN that needs no bypass and none of the page's headers, and going direct
     * is one less thing between the picture and the tile. Best effort — a cover
     * that will not come is a note on the tile, nothing worse.
     */
    private fun saveCover(name: String, url: String) {
        runCatching {
            val connection = (java.net.URL(url).openConnection() as HttpURLConnection).apply {
                instanceFollowRedirects = true
                connectTimeout = 10_000
                readTimeout = 10_000
            }
            val bytes = connection.inputStream.use { it.readBytes() }
            Covers.save(context, Covers.keyFor(name), bytes)
        }
    }

    /**
     * Whether an audio file is WebM rather than an MP4 container.
     *
     * A WebM (Matroska) file opens with the EBML marker; an MP4 does not. Four
     * bytes are enough to tell them apart, which is all the desktop build reads
     * for the same decision.
     */
    private fun looksWebm(file: File): Boolean = runCatching {
        java.io.FileInputStream(file).use { input ->
            val head = ByteArray(4)
            input.read(head) == 4 &&
                head[0] == 0x1A.toByte() && head[1] == 0x45.toByte() &&
                head[2] == 0xDF.toByte() && head[3] == 0xA3.toByte()
        }
    }.getOrDefault(false)

    /**
     * Two streams that have to become one file.
     *
     * This is the price of YouTube sending video and audio separately, and also
     * the reason it is worth paying: separate streams mean the codec can be
     * chosen, and the efficient one is less than half the size of the
     * compatible one at the same resolution.
     *
     * Both are fetched to the app's own cache first. Muxing reads them back,
     * which needs real files — and a half-finished download must never be left
     * in the user's folder.
     */
    private fun fetchPair(media: Media, onProgress: (Progress) -> Unit): String {
        val audioUrl = media.audioUrl ?: throw Failed("음성 스트림이 없습니다")
        val scratch = File(context.cacheDir, "mux").apply { mkdirs() }
        val videoFile = File(scratch, "v-${System.nanoTime()}")
        val audioFile = File(scratch, "a-${System.nanoTime()}")
        val merged = File(scratch, "m-${System.nanoTime()}")

        try {
            // Video first and audio second, weighted by their sizes so the bar
            // does not stall at the end while the small track arrives.
            val videoBytes = media.expectedVideoBytes.coerceAtLeast(1)
            val audioBytes = media.expectedAudioBytes.coerceAtLeast(1)
            val whole = (videoBytes + audioBytes).toFloat()

            downloadTo(media.url, media.headers, videoFile) { done ->
                onProgress(Progress(done, done / whole))
            }
            downloadTo(audioUrl, media.headers, audioFile) { done ->
                onProgress(Progress(videoBytes + done, (videoBytes + done) / whole))
            }

            onStage(Stage.JOINING)
            return joinInto(media, videoFile, audioFile)
        } finally {
            listOf(videoFile, audioFile, merged).forEach { it.delete() }
        }
    }

    /** Refuse a stream that did not arrive whole. */
    private fun complain(what: String, got: Long, expected: Long) {
        android.util.Log.i("shard-sabr", "$what $got / $expected")
        if (got == 0L) throw Failed("${what} 데이터를 받지 못했습니다")
        if (expected > 0 && got < expected) {
            throw Failed("${what}이 중간에 끊겼습니다 (${got * 100 / expected}%)")
        }
    }

    /** Stream a URL straight to a file, reporting bytes as they land. */
    private fun downloadTo(url: String, headers: Map<String, String>, file: File, onBytes: (Long) -> Unit) {
        open(url, headers).inputStream.use { input ->
            file.outputStream().use { output -> copy(input, output) { onBytes(it) } }
        }
    }

    // ---- whole files -------------------------------------------------------

    private fun fetchFile(media: Media, onProgress: (Progress) -> Unit): String {
        val connection = open(media.url, media.headers)
        val total = connection.contentLengthLong.takeIf { it > 0 }
        val extension = extensionOf(media.url) ?: "mp4"
        val sink = Storage.create(context, nameFor(media, extension), mimeFor(extension))

        sink.use { out ->
            connection.inputStream.use { input ->
                copy(input, out.stream) { done ->
                    onProgress(Progress(done, total?.let { done.toFloat() / it }))
                }
            }
        }
        return sink.where
    }

    // ---- HLS ---------------------------------------------------------------

    /**
     * An HLS "video" is a playlist of segments. Downloading it means fetching
     * every segment and writing them end to end — the result is a single
     * transport stream, which players handle without any repackaging.
     */
    private fun fetchHls(media: Media, onProgress: (Progress) -> Unit): String {
        val playlistUrl = resolveVariant(media)
        val segments = segmentsOf(playlistUrl, media.headers)
        if (segments.isEmpty()) throw Failed("재생목록에 조각이 없습니다")

        val sink = Storage.create(context, nameFor(media, "ts"), "video/mp2t")
        var done = 0L
        val pool = Executors.newFixedThreadPool(PARALLEL)
        try {
            sink.use { out ->
                // Fetched a batch at a time and written in order. One at a time
                // spends most of its life waiting for the next connection to be
                // established — with a hundred segments that handshake cost is
                // the download. Writing stays sequential because the segments
                // are concatenated into one stream and order is the file.
                segments.chunked(PARALLEL).forEachIndexed { batchIndex, batch ->
                    val fetched = batch.map { url ->
                        pool.submit<ByteArray> { readWhole(url, media.headers) }
                    }
                    fetched.forEachIndexed { within, future ->
                        val bytes = runCatching { future.get() }
                            .getOrElse { throw Failed("조각을 받을 수 없습니다: ${it.cause?.message ?: it.message}") }
                        out.stream.write(bytes)
                        done += bytes.size
                        val position = batchIndex * PARALLEL + within + 1
                        onProgress(Progress(done, position.toFloat() / segments.size))
                    }
                }
            }
        } finally {
            pool.shutdownNow()
        }
        return sink.where
    }

    /** One segment, whole, in memory — they are seconds of video, not files. */
    private fun readWhole(url: String, headers: Map<String, String>): ByteArray =
        open(url, headers).inputStream.use { it.readBytes() }

    /**
     * A master playlist lists renditions; take the best one.
     *
     * Normally the user has already chosen and this URL is a media playlist, in
     * which case nothing happens here. This is the path for a download started
     * without a choice — a direct link, or a page that offered only one.
     */
    private fun resolveVariant(media: Media): String {
        val text = textOf(media.url, media.headers)
        if (!Hls.isMaster(text)) return media.url
        return Hls.variants(text, media.url).firstOrNull()?.url ?: media.url
    }

    private fun segmentsOf(playlistUrl: String, headers: Map<String, String>): List<String> =
        Hls.segments(textOf(playlistUrl, headers), playlistUrl)

    // ---- plumbing ----------------------------------------------------------

    private fun open(url: String, headers: Map<String, String>): HttpURLConnection {
        val connection = URL(url).openConnection(ProxyRoute.forDownloads()) as HttpURLConnection
        // The page's own headers, minus the ones that describe a different
        // connection than this one, plus the ones a CDN insists on.
        PageContext.current.applyTo(headers).forEach { (name, value) ->
            if (!name.equals("Host", true) && !name.equals("Range", true) &&
                !name.equals("Accept-Encoding", true)
            ) {
                connection.setRequestProperty(name, value)
            }
        }
        connection.connectTimeout = 15_000
        connection.readTimeout = 30_000
        connection.instanceFollowRedirects = true
        val code = connection.responseCode
        if (code !in 200..299) throw Failed("서버가 $code 로 응답했습니다")
        return connection
    }

    private fun textOf(url: String, headers: Map<String, String>): String =
        open(url, headers).inputStream.use { it.readBytes().toString(Charsets.UTF_8) }

    private fun copy(from: InputStream, to: java.io.OutputStream, onWritten: (Long) -> Unit): Long {
        val buffer = ByteArray(64 * 1024)
        var written = 0L
        var sinceReport = 0L
        while (true) {
            stopIfCancelled()
            val n = from.read(buffer)
            if (n < 0) break
            to.write(buffer, 0, n)
            written += n
            sinceReport += n
            // Reporting every chunk would post thousands of UI updates a second.
            if (sinceReport >= 512 * 1024) {
                onWritten(written)
                sinceReport = 0
            }
        }
        onWritten(written)
        return written
    }

    /**
     * What to call the saved file.
     *
     * The title the page gave it, when there is one — `1196777_video.mp4` says
     * nothing about what is inside, and a folder of those is unusable. The
     * server's filename is the fallback for a file opened directly, where there
     * is no title to take.
     */
    private fun nameFor(media: Media, extension: String): String {
        val title = media.title.trim()
        if (title.isNotEmpty()) return Storage.nameFrom(title, extension)
        return media.fileName?.let { Storage.sanitise(it) }
            ?: Storage.nameFrom("video", extension)
    }

    private companion object {
        /**
         * Segments in flight at once.
         *
         * Four rather than more: measured against the tunnel, piling on
         * connections made an already-congested international path collapse
         * rather than go faster. Four fills a direct line without doing that.
         */
        const val PARALLEL = 4

        fun extensionOf(url: String): String? =
            Uri.parse(url).lastPathSegment?.substringAfterLast('.', "")?.takeIf { it.length in 2..4 }

        fun mimeFor(extension: String) = when (extension.lowercase()) {
            "mp4", "m4v" -> "video/mp4"
            "webm" -> "video/webm"
            "mkv" -> "video/x-matroska"
            "mp3" -> "audio/mpeg"
            "m4a" -> "audio/mp4"
            else -> "video/mp4"
        }
    }
}
