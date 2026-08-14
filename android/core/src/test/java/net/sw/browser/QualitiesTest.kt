package net.sw.browser

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Which video a long press refers to.
 *
 * [Qualities.forTarget] itself fetches playlists, so only the selection is
 * tested here — but selection is where the mistake was: pressing one video and
 * being offered every clip on the page.
 */
class QualitiesTest {

    private fun media(url: String) = Media(url, Media.Kind.FILE, "", emptyMap())

    /** Adverts are removed before narrowing, so this is what survives it. */
    private val onPage = listOf(
        media("https://cdn.example.com/main-feature.mp4"),   // newest first
        media("https://cdn.example.com/thumbnail-clip.mp4"),
    )

    @Test
    fun `a video that names its own url offers only that url`() {
        val target = VideoHook.VideoTarget("https://cdn.example.com/chosen.mp4", 1920, 1080)

        val candidates = Qualities.candidatesFor(target, onPage)
        assertEquals(1, candidates.size)
        assertEquals("https://cdn.example.com/chosen.mp4", candidates[0].url)
    }

    @Test
    fun `a private player keeps every capture as a candidate`() {
        // Media Source Extensions leaves a blob with nothing behind it, so the
        // element names nothing. Narrowing to one here picks whatever loaded
        // last, which is usually the advert — size is what tells them apart,
        // and that is measured later.
        val target = VideoHook.VideoTarget("blob:https://example.com/9f2c", 1920, 1080)
        assertEquals(onPage.size, Qualities.candidatesFor(target, onPage).size)
    }

    @Test
    fun `what the page reports comes before what the interceptor caught`() {
        // The interceptor cannot see media requests at all, so its entries are
        // usually previews; the page's own list is where a directly-streamed
        // file appears, and it has to be considered first.
        val target = VideoHook.VideoTarget(
            src = "blob:https://example.com/9f2c",
            width = 1920,
            height = 1080,
            seen = listOf("https://cdn.example.com/older.mp4", "https://cdn.example.com/feature-1080.mp4"),
        )

        val candidates = Qualities.candidatesFor(target, onPage)
        // Reported newest-last, so the newest is considered first.
        assertEquals("https://cdn.example.com/feature-1080.mp4", candidates[0].url)
        assertEquals("https://cdn.example.com/older.mp4", candidates[1].url)
        assertEquals(onPage.size + 2, candidates.size)
    }

    @Test
    fun `a url reported by both sources is offered once`() {
        val target = VideoHook.VideoTarget(
            src = "",
            width = 0,
            height = 0,
            seen = listOf("https://cdn.example.com/main-feature.mp4"),
        )
        assertEquals(onPage.size, Qualities.candidatesFor(target, onPage).size)
    }

    @Test
    fun `an id from the pressed element narrows the list to that video`() {
        // The point of the whole exercise: pressing one video on a page of many
        // must offer that one, not the page.
        val onListing = listOf(
            media("https://cdn.example.com/1196777/video_1080p.mp4"),
            media("https://cdn.example.com/9900011/video_1080p.mp4"),
            media("https://ads.example.com/preroll.mp4"),
        )

        val narrowed = Qualities.narrowTo(listOf("1196777"), emptyList(), onListing)
        assertEquals(1, narrowed.size)
        assertEquals("https://cdn.example.com/1196777/video_1080p.mp4", narrowed[0].url)
    }

    @Test
    fun `several renditions of the same video all survive narrowing`() {
        val sameVideo = listOf(
            media("https://cdn.example.com/1196777/1080p.mp4"),
            media("https://cdn.example.com/1196777/720p.mp4"),
            media("https://cdn.example.com/other/1080p.mp4"),
        )
        assertEquals(2, Qualities.narrowTo(listOf("1196777"), emptyList(), sameVideo).size)
    }

    @Test
    fun `title words are used only when no id matches`() {
        val onListing = listOf(
            media("https://cdn.example.com/1196777/video.mp4"),
            media("https://cdn.example.com/beautiful-sunset-clip/1080p.mp4"),
        )

        // The id wins outright when it matches.
        assertEquals(
            "https://cdn.example.com/1196777/video.mp4",
            Qualities.narrowTo(listOf("1196777"), listOf("sunset"), onListing)[0].url,
        )
        // With no matching id, the slug is the next best evidence.
        assertEquals(
            "https://cdn.example.com/beautiful-sunset-clip/1080p.mp4",
            Qualities.narrowTo(listOf("0000000"), listOf("sunset"), onListing)[0].url,
        )
    }

    @Test
    fun `an id that matches nothing leaves the list alone`() {
        // Plenty of sites name media with no relation to the markup. Offering
        // everything is recoverable; offering nothing is a dead end.
        assertEquals(onPage.size, Qualities.narrowTo(listOf("nope"), emptyList(), onPage).size)
        assertEquals(onPage.size, Qualities.narrowTo(emptyList(), emptyList(), onPage).size)
    }

    @Test
    fun `known advert patterns are dropped`() {
        for (url in listOf(
            "https://ads.example.com/x.mp4",
            "https://cdn.example.com/ads/preroll.mp4",
            "https://www.trafficjunky.com/spot.mp4",
            "https://x.doubleclick.net/v.mp4",
            "https://cdn.example.com/preroll/intro.mp4",
        )) {
            assertTrue("should be an advert: $url", Qualities.isAdvert(url))
        }
    }

    @Test
    fun `content is not mistaken for an advert`() {
        // The filter runs before size is measured, so a false positive here
        // silently removes the video the user asked for.
        for (url in listOf(
            "https://cdn.example.com/1196777/video_1080p.mp4",
            "https://v.example.com/hls/master.m3u8",
            "https://cdn.example.com/uploads/download-day.mp4",
        )) {
            assertFalse("should not be an advert: $url", Qualities.isAdvert(url))
        }
    }

    @Test
    fun `nothing captured and nothing named offers nothing`() {
        val candidates = Qualities.candidatesFor(VideoHook.VideoTarget("", 0, 0), emptyList())
        assertEquals(0, candidates.size)
    }

    @Test
    fun `a relative or non-http source is not treated as a url`() {
        // `hasUsableSrc` guards the fetch; anything else has to fall through to
        // the captured list or it would be fetched as a literal.
        for (src in listOf("blob:x", "data:video/mp4;base64,AAAA", "//cdn.example.com/x.mp4", "")) {
            val candidates = Qualities.candidatesFor(VideoHook.VideoTarget(src, 0, 0), onPage)
            assertEquals("failed for $src", onPage.size, candidates.size)
        }
    }
}
