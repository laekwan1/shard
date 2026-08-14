package net.sw.browser

import java.io.ByteArrayOutputStream
import java.io.OutputStream

/**
 * YouTube's server-driven delivery, spoken from this side.
 *
 * YouTube no longer hands out an address per stream. Every format is listed and
 * none of them can be fetched — measured: 403 with the address as given, 403
 * with the throttling parameter stripped, and the player's own code no longer
 * even contains the function that used to rewrite it. What the player does
 * instead is POST a description of its situation to one endpoint and let the
 * server decide which format, and which seconds of it, to send back.
 *
 * So that is what this does. The request is not built from nothing: the page
 * makes a real one while the video plays, and it is captured and used as a
 * template. That matters more than it sounds — the template carries the server
 * configuration and the bot-check token, both opaque and neither reproducible
 * from outside a browser. Everything this file changes is the part that says
 * *what to send*: which formats are on the menu, and how far along playback is.
 *
 * The reply is a stream of length-prefixed parts rather than a file, so the
 * media has to be picked out of it and reassembled per format.
 *
 * This will break. It is a private protocol with no promises attached, and the
 * field numbers below were read off a live request rather than a specification.
 * The cost of it breaking is that YouTube downloads stop working until the
 * numbers are read again; nothing else in the app depends on this.
 */
object Sabr {

    /** What the page captured: one real request, ready to be rewritten. */
    data class Template(val url: String, val body: ByteArray)

    /** A format as the player response describes it. */
    data class Track(val itag: Int, val lastModified: Long, val xtags: String, val bytes: Long)

    /** Fields inside the request that this code understands. */
    private const val CLIENT_STATE = 1
    private const val SELECTED_FORMATS = 2
    private const val BUFFERED_RANGES = 3
    private const val AUDIO_CHOICES = 16
    private const val VIDEO_CHOICES = 17

    /** Inside ClientAbrState. */
    private const val VIEWPORT_WIDTH = 18
    private const val VIEWPORT_HEIGHT = 19
    private const val PLAYER_TIME_MS = 28

    /**
     * Replies that add nothing before the download is abandoned.
     *
     * The server answers a position in the video, and near the end it answers
     * with what it has already sent. Three of those in a row means finished or
     * refused, and either way there is nothing more to ask for.
     */
    private const val STALLS_BEFORE_GIVING_UP = 3

    /** When a reply does not say what it covered. */
    private const val STEP_WHEN_UNSAID = 5_000L

    /**
     * A hard ceiling on requests for one download.
     *
     * Not a limit anything real should reach — a four-hour film at ten seconds
     * a reply is under fifteen hundred — but a runaway loop here is thousands
     * of requests a minute at YouTube, which is worth making impossible rather
     * than unlikely.
     */
    private const val MAX_REQUESTS = 4_000

    /** Parts of the reply. */
    private const val PART_MEDIA_HEADER = 20
    private const val PART_MEDIA = 21

    /** Inside a media header. */
    private const val HEADER_ID = 1
    private const val HEADER_ITAG = 3
    private const val HEADER_START_BYTE = 6
    private const val HEADER_SEQUENCE = 9
    private const val HEADER_DURATION_MS = 12

    /**
     * Pull one format to [sink], from the beginning to the end of the stream.
     *
     * [want] is the format; [other] is the one the request must also mention.
     * Both are named because the server picks from what it is offered, and
     * offering a single track makes it send a track nobody asked to keep —
     * naming both and discarding the other half is what actually holds the
     * choice steady.
     *
     * Blocks. Returns the number of bytes written.
     */
    fun fetch(
        template: Template,
        video: Track,
        audio: Track,
        decoy: Track,
        videoFile: java.io.File,
        audioFile: java.io.File,
        onBytes: (Long, Long) -> Unit,
        cancelled: () -> Boolean,
        audioOnly: Boolean = false,
    ): Pair<Long, Long> {
        var playerTimeMs = 0L
        var stalled = 0
        var videoDone = 0L
        var audioDone = 0L

        java.io.RandomAccessFile(videoFile, "rw").use { videoOut ->
            java.io.RandomAccessFile(audioFile, "rw").use { audioOut ->
                var requests = 0
                val heldVideo = Held(video)
                val heldAudio = Held(audio)
                while (!cancelled() && requests++ < MAX_REQUESTS) {
                    val beforeVideo = videoDone
                    val beforeAudio = audioDone
                    val held = if (audioOnly) listOf(heldAudio) else listOf(heldVideo, heldAudio)
                    val reply = post(
                        template.url,
                        build(template.body, video, audio, decoy, playerTimeMs, held, audioOnly),
                    )
                    // Only the audio is asked for and only the audio is written
                    // when the video half is left out. The video counters stay at
                    // zero, which the stall and completion checks below already
                    // read as "not wanted" rather than "stuck".
                    val videoRuns = if (audioOnly) Runs(emptyList(), 0L, 0L) else runs(reply, video.itag)
                    val audioRuns = runs(reply, audio.itag)
                    val gotVideo = write(videoRuns, videoOut, videoDone)
                    val gotAudio = write(audioRuns, audioOut, audioDone)
                    videoDone = gotVideo.contiguous
                    audioDone = gotAudio.contiguous
                    if (!audioOnly) heldVideo.add(videoRuns)
                    heldAudio.add(audioRuns)

                    // Stalling is measured by whether the files grew, not by
                    // whether the reply was empty. A reply that repeats bytes
                    // already held is not empty and not progress, and the first
                    // version of this loop answered one by asking again — four
                    // and a half thousand times, for a video it never finished.
                    // Either stream failing to grow is a stall, not just both —
                    // one standing still is a gap that will not close, and
                    // letting the other carry the loop hides it. A stream that
                    // has all its bytes is finished, not stuck.
                    val videoStuck = !audioOnly &&
                        videoDone == beforeVideo && !whole(videoDone, video.bytes)
                    val audioStuck = audioDone == beforeAudio && !whole(audioDone, audio.bytes)
                    if (videoStuck || audioStuck) {
                        if (++stalled >= STALLS_BEFORE_GIVING_UP) break
                    } else {
                        stalled = 0
                        onBytes(videoDone, audioDone)
                    }

                    // How far to jump for the next request: the shorter of what
                    // the two streams covered, never the longer.
                    //
                    // They are cut differently — video around five seconds a
                    // piece, audio around ten — so a reply covers more of one
                    // than the other. Advancing by the longer skips the tail of
                    // the shorter, and the gap is never filled because nothing
                    // ever asks for that moment again. Measured before this
                    // line was written: audio kept arriving while video sat at
                    // six per cent, which is exactly what a gap looks like from
                    // the outside.
                    val covered = listOfNotNull(
                        gotVideo.durationMs.takeIf { !audioOnly && it > 0 },
                        gotAudio.durationMs.takeIf { it > 0 },
                    ).minOrNull() ?: 0L
                    playerTimeMs += if (covered > 0) covered else STEP_WHEN_UNSAID
                    val videoWhole = audioOnly || whole(videoDone, video.bytes)
                    if (videoWhole && whole(audioDone, audio.bytes)) break
                }
            }
        }
        return videoDone to audioDone
    }

    /** Whether a stream has all of its bytes, when its size is known at all. */
    private fun whole(got: Long, expected: Long) = expected > 0 && got >= expected

    private class Written(val contiguous: Long, val empty: Boolean, val durationMs: Long)

    /**
     * Place a reply's runs in the file where they belong.
     *
     * Written by offset rather than appended: the server answers a request for
     * a moment in time, and the runs it sends can overlap what is already here
     * or sit ahead of a gap. Appending them produced a file no player opened.
     */
    private fun write(runs: Runs, out: java.io.RandomAccessFile, from: Long): Written {
        var contiguous = from
        runs.pieces.sortedBy { it.at }.forEach { piece ->
            out.seek(piece.at)
            out.write(piece.bytes)
            if (piece.at <= contiguous) contiguous = maxOf(contiguous, piece.at + piece.bytes.size)
        }
        return Written(contiguous, runs.pieces.isEmpty(), runs.durationMs)
    }


    // ---- request -----------------------------------------------------------

    /**
     * Rewrite the captured request to ask for one pair of formats at one point
     * in the video.
     *
     * Fields the code has no use for are copied across untouched. That is
     * deliberate: the request carries a great deal that only the server
     * understands, and dropping a field because this file has no name for it
     * would be guessing.
     */
    private fun build(
        template: ByteArray,
        video: Track,
        audio: Track,
        decoy: Track,
        playerTimeMs: Long,
        held: List<Held>,
        audioOnly: Boolean = false,
    ): ByteArray {
        val out = ByteArrayOutputStream()
        val fields = split(template)

        val state = fields.firstOrNull { it.number == CLIENT_STATE }
        if (state != null) out.writeLengthField(CLIENT_STATE, playbackState(state.value, playerTimeMs))

        // A format that is deliberately *not* one of the two being downloaded.
        //
        // This field says what is already playing, and the server does not
        // resend the few hundred bytes of setup at the head of a stream it
        // believes the client already has. Naming the wanted format here — the
        // obvious thing to do — produced a file with every frame in it and no
        // way to open it. Naming something else leaves the target looking new.
        out.writeLengthField(SELECTED_FORMATS, formatId(decoy))

        // What has arrived so far.
        //
        // Without this the server sends the opening of the stream, then keeps
        // answering with the same seconds — it decides what comes next from
        // what the client says it is holding, and a client holding nothing is
        // always at the beginning. Measured: the download stopped at six per
        // cent and every further request repeated bytes already written.
        held.filter { it.durationMs > 0 }.forEach { track ->
            val range = ByteArrayOutputStream()
            range.writeLengthField(1, formatId(track.track))
            range.writeVarintField(2, 0)
            range.writeVarintField(3, track.durationMs)
            range.writeVarintField(4, 1)
            range.writeVarintField(5, track.lastSequence)
            out.writeLengthField(BUFFERED_RANGES, range.toByteArray())
        }

        // Everything else the template carried, except the two menus and the
        // ranges already held. Those are replaced below.
        fields.filter {
            it.number !in setOf(
                CLIENT_STATE, SELECTED_FORMATS, BUFFERED_RANGES, AUDIO_CHOICES, VIDEO_CHOICES,
            )
        }.forEach { out.write(it.raw) }

        // The menu, holding exactly what is wanted. The server picks from this,
        // so one entry each is what makes the choice stick — otherwise it
        // switches quality mid-download to suit a connection it is measuring,
        // which is right for playback and wrong for a file.
        out.writeLengthField(AUDIO_CHOICES, formatId(audio))
        // No video menu when only the sound is wanted. Leaving it out is what
        // makes the server send audio alone — with it in, video comes too and
        // is thrown away.
        if (!audioOnly) out.writeLengthField(VIDEO_CHOICES, formatId(video))
        return out.toByteArray()
    }

    /** ClientAbrState with the playback position moved and the viewport opened up. */
    private fun playbackState(original: ByteArray, playerTimeMs: Long): ByteArray {
        val out = ByteArrayOutputStream()
        split(original).forEach { field ->
            when (field.number) {
                PLAYER_TIME_MS -> out.writeVarintField(PLAYER_TIME_MS, playerTimeMs)
                // A small viewport is a request for a small picture: the server
                // answered a 640x360 window with 360p however the formats were
                // listed. This says the screen is large enough for any of them.
                VIEWPORT_WIDTH -> out.writeVarintField(VIEWPORT_WIDTH, 3840)
                VIEWPORT_HEIGHT -> out.writeVarintField(VIEWPORT_HEIGHT, 2160)
                else -> out.write(field.raw)
            }
        }
        return out.toByteArray()
    }

    private fun formatId(track: Track): ByteArray {
        val out = ByteArrayOutputStream()
        out.writeVarintField(1, track.itag.toLong())
        out.writeVarintField(2, track.lastModified)
        if (track.xtags.isNotEmpty()) out.writeLengthField(3, track.xtags.toByteArray())
        return out.toByteArray()
    }

    private fun post(url: String, body: ByteArray): ByteArray {
        val connection = (java.net.URL(url).openConnection(ProxyRoute.forDownloads())
            as java.net.HttpURLConnection).apply {
            requestMethod = "POST"
            doOutput = true
            // Declare the length rather than letting the connection chunk the
            // body: this endpoint is not a general web server and a chunked
            // request is not what its own player sends.
            setFixedLengthStreamingMode(body.size)
            setRequestProperty("Content-Type", "application/x-protobuf")
            setRequestProperty("Origin", "https://www.youtube.com")
            setRequestProperty("Referer", "https://www.youtube.com/")
            PageContext.current.userAgent?.let { setRequestProperty("User-Agent", it) }
            connectTimeout = 15_000
            readTimeout = 60_000
        }
        connection.outputStream.use { it.write(body) }
        val code = connection.responseCode
        android.util.Log.i("shard-sabr", "post ${body.size}B -> $code")
        if (code !in 200..299) throw Downloader.Failed("서버가 $code 로 응답했습니다")
        return connection.inputStream.use { it.readBytes() }
    }

    // ---- reply -------------------------------------------------------------

    /** A run of bytes and where in the stream it belongs. */
    private class Piece(val at: Long, val bytes: ByteArray)

    private class Runs(val pieces: List<Piece>, val durationMs: Long, val lastSequence: Long)

    /** What has been received of one track, as the server needs it described. */
    private class Held(val track: Track) {
        var durationMs = 0L
        var lastSequence = 0L

        fun add(runs: Runs) {
            durationMs += runs.durationMs
            if (runs.lastSequence > lastSequence) lastSequence = runs.lastSequence
        }
    }

    /**
     * Take the media belonging to [itag] out of a reply.
     *
     * The reply interleaves both tracks, and each run of media bytes is
     * introduced by a header naming the format it belongs to. Anything with a
     * header for the other track — or with no header seen yet — is skipped, so
     * the audio arriving alongside a video download is simply dropped.
     */
    private fun runs(reply: ByteArray, itag: Int): Runs {
        val pieces = ArrayList<Piece>()
        // Headers are addressed by id, and the media parts that follow refer
        // back to them, so which track a run belongs to — and where it starts —
        // is only knowable by remembering the headers seen so far.
        val itagOfHeader = HashMap<Long, Int>()
        val startOfHeader = HashMap<Long, Long>()
        val filled = HashMap<Long, Long>()
        var covered = 0L
        var lastSequence = 0L
        var position = 0

        while (position < reply.size) {
            val type = readUmpVarint(reply, position) ?: break
            position = type.next
            val size = readUmpVarint(reply, position) ?: break
            position = size.next
            val start = position
            val end = start + size.value.toInt()
            if (end > reply.size || size.value < 0) break

            when (type.value.toInt()) {
                PART_MEDIA_HEADER -> {
                    var id = 0L
                    var headerItag = 0
                    var durationMs = 0L
                    var startByte = 0L
                    var sequence = 0L
                    split(reply.copyOfRange(start, end)).forEach { f ->
                        when (f.number) {
                            HEADER_ID -> id = f.asLong()
                            HEADER_ITAG -> headerItag = f.asLong().toInt()
                            HEADER_DURATION_MS -> durationMs = f.asLong()
                            HEADER_START_BYTE -> startByte = f.asLong()
                            HEADER_SEQUENCE -> sequence = f.asLong()
                        }
                    }
                    itagOfHeader[id] = headerItag
                    startOfHeader[id] = startByte
                    filled[id] = 0
                    if (headerItag == itag) {
                        covered += durationMs
                        if (sequence > lastSequence) lastSequence = sequence
                    }
                }

                PART_MEDIA -> {
                    // A media part opens with the id of the header describing
                    // it, and a run can be split across several parts, so each
                    // continues where the previous one for that header stopped.
                    val id = readUmpVarint(reply, start) ?: continue
                    if (itagOfHeader[id.value] == itag) {
                        val already = filled[id.value] ?: 0
                        val bytes = reply.copyOfRange(id.next, end)
                        pieces.add(Piece((startOfHeader[id.value] ?: 0) + already, bytes))
                        filled[id.value] = already + bytes.size
                    }
                }
            }
            position = end
        }
        return Runs(pieces, covered, lastSequence)
    }

    // ---- wire formats ------------------------------------------------------

    private class Field(val number: Int, val wire: Int, val raw: ByteArray, val value: ByteArray) {
        fun asLong(): Long = readVarint(value, 0)?.value ?: 0
    }

    /**
     * Break a protobuf message into its fields, keeping each one's original
     * bytes so it can be written back exactly as it came.
     */
    private fun split(buffer: ByteArray): List<Field> {
        val fields = ArrayList<Field>()
        var position = 0
        while (position < buffer.size) {
            val start = position
            val key = readVarint(buffer, position) ?: break
            position = key.next
            val number = (key.value ushr 3).toInt()
            val wire = (key.value and 7).toInt()
            if (number == 0) break

            val valueStart = position
            when (wire) {
                0 -> position = (readVarint(buffer, position) ?: break).next
                1 -> position += 8
                5 -> position += 4
                2 -> {
                    val length = readVarint(buffer, position) ?: break
                    position = length.next + length.value.toInt()
                }
                else -> return fields
            }
            if (position > buffer.size) break
            fields.add(
                Field(
                    number, wire,
                    buffer.copyOfRange(start, position),
                    // For a length-delimited field the payload is what follows
                    // its length, which is what callers actually want to read.
                    if (wire == 2) {
                        val length = readVarint(buffer, valueStart)!!
                        buffer.copyOfRange(length.next, position)
                    } else {
                        buffer.copyOfRange(valueStart, position)
                    },
                )
            )
        }
        return fields
    }

    private class Read(val value: Long, val next: Int)

    private fun readVarint(buffer: ByteArray, from: Int): Read? {
        var result = 0L
        var shift = 0
        var position = from
        while (position < buffer.size) {
            val byte = buffer[position++].toInt() and 0xff
            result = result or ((byte and 0x7f).toLong() shl shift)
            if (byte and 0x80 == 0) return Read(result, position)
            shift += 7
            if (shift > 63) return null
        }
        return null
    }

    /**
     * The reply uses a different variable-length integer from the request: the
     * top bits of the first byte say how many bytes follow, rather than each
     * byte carrying a continuation bit.
     */
    private fun readUmpVarint(buffer: ByteArray, from: Int): Read? {
        if (from >= buffer.size) return null
        val first = buffer[from].toInt() and 0xff
        fun at(offset: Int) = (buffer.getOrNull(from + offset)?.toInt() ?: 0) and 0xff
        return when {
            first < 0x80 -> Read(first.toLong(), from + 1)
            first < 0xc0 -> Read(((first and 0x3f) or (at(1) shl 6)).toLong(), from + 2)
            first < 0xe0 -> Read(((first and 0x1f) or (at(1) shl 5) or (at(2) shl 13)).toLong(), from + 3)
            first < 0xf0 ->
                Read(((first and 0x0f) or (at(1) shl 4) or (at(2) shl 12) or (at(3) shl 20)).toLong(), from + 4)
            else ->
                Read(
                    (at(1).toLong() or (at(2).toLong() shl 8) or (at(3).toLong() shl 16) or (at(4).toLong() shl 24)),
                    from + 5,
                )
        }
    }

    private fun OutputStream.writeVarint(value: Long) {
        var remaining = value
        do {
            var byte = (remaining and 0x7f).toInt()
            remaining = remaining ushr 7
            if (remaining != 0L) byte = byte or 0x80
            write(byte)
        } while (remaining != 0L)
    }

    private fun OutputStream.writeVarintField(number: Int, value: Long) {
        writeVarint((number.toLong() shl 3))
        writeVarint(value)
    }

    private fun OutputStream.writeLengthField(number: Int, payload: ByteArray) {
        writeVarint((number.toLong() shl 3) or 2)
        writeVarint(payload.size.toLong())
        write(payload)
    }
}
