package net.sw.browser

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * What the address bar makes of what is typed into it.
 *
 * Where the browser opens and where a search goes are two separate questions,
 * and one URL used to answer both of them. The browser now opens on YouTube —
 * it exists to save video — while typed words are still answered by Google.
 * Nothing on screen distinguishes the two, so they are easy to "tidy" back
 * together.
 */
class AddressBarTest {

    @Test
    fun `words with a space are a search rather than an address`() {
        assertFalse(BrowserActivity.looksLikeAddress("shard test search"))
        assertFalse(BrowserActivity.looksLikeAddress("what is a dpi"))
    }

    @Test
    fun `a single word with no dot is a search rather than a host`() {
        assertFalse(BrowserActivity.looksLikeAddress("youtube"))
    }

    @Test
    fun `a dot and no space is a host`() {
        assertTrue(BrowserActivity.looksLikeAddress("m.youtube.com"))
        assertTrue(BrowserActivity.looksLikeAddress("  example.com/a  "))
    }

    @Test
    fun `something already carrying its scheme is left alone`() {
        assertEquals("https://example.com/a", BrowserActivity.asUrl("https://example.com/a"))
        assertEquals("http://example.com", BrowserActivity.asUrl("  http://example.com  "))
    }
}
