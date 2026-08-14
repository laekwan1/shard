package net.sw.browser

import android.media.MediaCodec
import android.media.MediaExtractor
import android.media.MediaFormat
import android.media.MediaMuxer
import java.io.File
import java.nio.ByteBuffer

/**
 * Joining a video track and an audio track into one playable file.
 *
 * YouTube sends them separately above 360p, which is what makes choosing a
 * codec possible — and what makes this step necessary. Android does the work
 * itself: `MediaExtractor` reads each stream and `MediaMuxer` writes both into
 * one container. No library is added and the APK does not grow, which is the
 * whole reason for going this way rather than carrying a copy of FFmpeg.
 *
 * What the device will actually accept differs by version and by chip, so
 * nothing here assumes: the container is chosen from what the tracks are, and a
 * refusal is reported rather than papered over.
 */
object Muxer {

    class Failed(message: String, cause: Throwable? = null) : Exception(message, cause)

    /**
     * Write [video] and [audio] into [output] as one file.
     *
     * Returns the container that was used, so the caller can name the file
     * correctly — a WebM with an `.mp4` extension plays nowhere.
     */
    /**
     * Which container the pair will end up in, without writing anything.
     *
     * Asked before the destination is created, because the file's name has to
     * carry the right extension and the answer depends on the codecs. Opening
     * the two extractors twice costs a header read each; the copy this avoids
     * costs the whole file.
     */
    fun planExtension(video: File, audio: File): String {
        val videoExtractor = MediaExtractor().apply { setDataSource(video.absolutePath) }
        val audioExtractor = MediaExtractor().apply { setDataSource(audio.absolutePath) }
        try {
            val videoTrack = firstTrack(videoExtractor, "video/") ?: throw Failed("영상 트랙을 찾을 수 없습니다")
            val audioTrack = firstTrack(audioExtractor, "audio/") ?: throw Failed("음성 트랙을 찾을 수 없습니다")
            return containerFor(
                videoExtractor.getTrackFormat(videoTrack),
                audioExtractor.getTrackFormat(audioTrack),
            ).extension
        } finally {
            runCatching { videoExtractor.release() }
            runCatching { audioExtractor.release() }
        }
    }

    /** Join the pair straight into an already-open destination. */
    fun combineInto(video: File, audio: File, into: java.io.FileDescriptor): String =
        join(video, audio) { container -> MediaMuxer(into, container.output) }

    fun combine(video: File, audio: File, output: File): String =
        join(video, audio) { container -> MediaMuxer(output.absolutePath, container.output) }

    private inline fun join(video: File, audio: File, open: (Container) -> MediaMuxer): String {
        val videoExtractor = MediaExtractor().apply { setDataSource(video.absolutePath) }
        val audioExtractor = MediaExtractor().apply { setDataSource(audio.absolutePath) }

        try {
            val videoTrack = firstTrack(videoExtractor, "video/")
                ?: throw Failed("영상 트랙을 찾을 수 없습니다")
            val audioTrack = firstTrack(audioExtractor, "audio/")
                ?: throw Failed("음성 트랙을 찾을 수 없습니다")

            val videoFormat = videoExtractor.getTrackFormat(videoTrack)
            val audioFormat = audioExtractor.getTrackFormat(audioTrack)
            val container = containerFor(videoFormat, audioFormat)

            val muxer = open(container)
            try {
                videoExtractor.selectTrack(videoTrack)
                audioExtractor.selectTrack(audioTrack)
                val outVideo = muxer.addTrack(videoFormat)
                val outAudio = muxer.addTrack(audioFormat)
                muxer.start()

                copyTrack(videoExtractor, muxer, outVideo)
                copyTrack(audioExtractor, muxer, outAudio)

                muxer.stop()
            } catch (e: IllegalArgumentException) {
                // The muxer refuses formats it cannot write, and the message it
                // gives is not one to show a user.
                throw Failed("이 기기가 합칠 수 없는 형식입니다 (${shortName(videoFormat)} + ${shortName(audioFormat)})", e)
            } finally {
                runCatching { muxer.release() }
            }
            return container.extension
        } finally {
            runCatching { videoExtractor.release() }
            runCatching { audioExtractor.release() }
        }
    }

    private class Container(val output: Int, val extension: String)

    /**
     * Which container can hold both tracks.
     *
     * MP4 takes H.264 and AV1 with AAC; WebM takes VP9 and AV1 with Opus.
     * Choosing by what the tracks are rather than by a fixed preference is what
     * lets the smallest codec be used instead of the most compatible one.
     */
    private fun containerFor(video: MediaFormat, audio: MediaFormat): Container {
        val v = video.getString(MediaFormat.KEY_MIME).orEmpty()
        val a = audio.getString(MediaFormat.KEY_MIME).orEmpty()
        val webmVideo = v.contains("vp9") || v.contains("vp8")
        val webmAudio = a.contains("opus") || a.contains("vorbis")
        return if (webmVideo || webmAudio) {
            Container(MediaMuxer.OutputFormat.MUXER_OUTPUT_WEBM, "webm")
        } else {
            Container(MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4, "mp4")
        }
    }

    private fun shortName(format: MediaFormat): String =
        format.getString(MediaFormat.KEY_MIME).orEmpty().substringAfter('/')

    private fun firstTrack(extractor: MediaExtractor, prefix: String): Int? {
        for (i in 0 until extractor.trackCount) {
            val mime = extractor.getTrackFormat(i).getString(MediaFormat.KEY_MIME).orEmpty()
            if (mime.startsWith(prefix)) return i
        }
        return null
    }

    /** Copy every sample of the selected track, timestamps and flags intact. */
    private fun copyTrack(extractor: MediaExtractor, muxer: MediaMuxer, track: Int) {
        val buffer = ByteBuffer.allocate(BUFFER)
        val info = MediaCodec.BufferInfo()
        while (true) {
            val size = extractor.readSampleData(buffer, 0)
            if (size < 0) break
            info.offset = 0
            info.size = size
            info.presentationTimeUs = extractor.sampleTime
            info.flags = extractor.sampleFlags
            muxer.writeSampleData(track, buffer, info)
            extractor.advance()
        }
    }

    /** Large enough for a 4K keyframe; a smaller one throws mid-copy. */
    private const val BUFFER = 2 * 1024 * 1024
}
