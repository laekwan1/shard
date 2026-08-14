package net.sw.browser

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Which soundtrack gets saved.
 *
 * The mistake this pins down: a Korean video saved with its English dub,
 * because the two questions — which language, and how good — were being
 * answered together and the English track happened to be a few kilobits
 * smaller. Language is settled first now, and only then is quality compared,
 * which is the rule the desktop already followed.
 */
class YouTubeAudioTest {

    private fun audio(
        itag: Int,
        codec: String,
        bitrate: Int,
        language: String = "",
        default: Boolean = false,
        bytes: Long = bitrate * 100L,
    ) = YouTube.Format(
        itag = itag,
        mimeType = if (codec == "Opus") "audio/webm; codecs=\"opus\"" else "audio/mp4; codecs=\"mp4a.40.2\"",
        quality = "",
        bitrate = bitrate,
        bytes = bytes,
        lastModified = 1,
        xtags = "",
        hasVideo = false,
        hasAudio = true,
        audioLanguage = language,
        audioName = language,
        audioDefault = default,
    )

    /** English is the original and smaller; Korean is what was asked for. */
    private val dubbed = listOf(
        audio(140, "AAC", 128_000, language = "en", default = true, bytes = 1_000_000),
        audio(141, "AAC", 128_000, language = "ko", bytes = 1_400_000),
        audio(251, "Opus", 96_000, language = "en", default = true, bytes = 800_000),
        audio(252, "Opus", 96_000, language = "ko", bytes = 1_100_000),
    )

    @Test
    fun `the language asked for wins over the smaller track`() {
        val chosen = YouTube.bestAudio(dubbed, language = "ko")
        assertEquals("ko", chosen?.audioLanguage)
    }

    @Test
    fun `a language the video does not have falls back to the one it plays`() {
        val chosen = YouTube.bestAudio(dubbed, language = "ja")
        assertEquals("en", chosen?.audioLanguage)
    }

    @Test
    fun `quality is compared only within the chosen language`() {
        // Both Korean tracks are larger than both English ones, so a rule that
        // compared size first would never reach either of them.
        val chosen = YouTube.bestAudio(dubbed, language = "ko")
        assertEquals("ko", chosen?.audioLanguage)
        assertEquals(96_000, chosen?.bitrate)
    }

    @Test
    fun `the container the muxer needs still decides the codec`() {
        // Android's muxer refuses Opus in an MP4, so a video in that container
        // takes the AAC track — but still the Korean one.
        val chosen = YouTube.bestAudio(dubbed, forContainer = "mp4", language = "ko")
        assertEquals("ko", chosen?.audioLanguage)
        assertEquals("AAC", chosen?.codec)
    }

    @Test
    fun `a video with one soundtrack is unaffected`() {
        val plain = listOf(
            audio(140, "AAC", 128_000),
            audio(251, "Opus", 96_000),
        )
        assertEquals(251, YouTube.bestAudio(plain, language = "ko")?.itag)
    }
}
