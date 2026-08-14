package net.sw.browser

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class HlsTest {

    private val master = """
        #EXTM3U
        #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360
        360/index.m3u8
        #EXT-X-STREAM-INF:BANDWIDTH=5200000,RESOLUTION=1920x1080
        1080/index.m3u8
        #EXT-X-STREAM-INF:BANDWIDTH=2400000,RESOLUTION=1280x720
        https://cdn.example.com/720/index.m3u8
    """.trimIndent()

    @Test
    fun `a master playlist is recognised and a media one is not`() {
        assertTrue(Hls.isMaster(master))
        assertFalse(Hls.isMaster("#EXTM3U\n#EXTINF:10,\nseg1.ts\n"))
    }

    @Test
    fun `variants come back highest quality first`() {
        val variants = Hls.variants(master, "https://v.example.com/hls/master.m3u8")

        assertEquals(listOf(1080, 720, 360), variants.map { it.height })
        assertEquals(5_200_000L, variants[0].bandwidth)
    }

    @Test
    fun `relative variant urls resolve against the playlist and absolute ones are left alone`() {
        val variants = Hls.variants(master, "https://v.example.com/hls/master.m3u8")

        assertEquals("https://v.example.com/hls/1080/index.m3u8", variants[0].url)
        assertEquals("https://cdn.example.com/720/index.m3u8", variants[1].url)
    }

    @Test
    fun `a variant label names the quality a user would recognise`() {
        val variants = Hls.variants(master, "https://v.example.com/hls/master.m3u8")
        assertEquals("1080p · 5.2 Mbps", variants[0].label)
    }

    @Test
    fun `a duplicated rendition is offered once`() {
        // Playlists repeat a rendition per audio group; listing it twice would
        // look like two different choices that download the same thing.
        val duplicated = master + "\n#EXT-X-STREAM-INF:BANDWIDTH=5200000,RESOLUTION=1920x1080\n1080/index.m3u8"
        val variants = Hls.variants(duplicated, "https://v.example.com/hls/master.m3u8")
        assertEquals(3, variants.size)
    }

    @Test
    fun `an attribute line with no url after it is skipped`() {
        // Truncated playlists happen; pairing the line with a later unrelated
        // url would download the wrong rendition.
        val truncated = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\n"
        assertTrue(Hls.variants(truncated, "https://v.example.com/master.m3u8").isEmpty())
    }

    @Test
    fun `segments are returned in order with comments dropped`() {
        val media = """
            #EXTM3U
            #EXT-X-TARGETDURATION:10
            #EXTINF:10.0,
            seg1.ts
            #EXTINF:10.0,
            seg2.ts
            #EXT-X-ENDLIST
        """.trimIndent()

        assertEquals(
            listOf("https://v.example.com/hls/seg1.ts", "https://v.example.com/hls/seg2.ts"),
            Hls.segments(media, "https://v.example.com/hls/index.m3u8"),
        )
    }

    @Test
    fun `a playlist url is recognised even with a query string`() {
        assertTrue(Hls.isPlaylist("https://v.example.com/master.m3u8"))
        assertTrue(Hls.isPlaylist("https://v.example.com/master.m3u8?token=abc"))
        assertFalse(Hls.isPlaylist("https://v.example.com/video.mp4"))
    }
}
