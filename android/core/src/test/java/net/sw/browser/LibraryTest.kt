package net.sw.browser

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * How the library names and finds its folders.
 *
 * Only the naming is tested here — listing and moving go through the media
 * store, which does not exist off a phone. Naming is where a mistake would be
 * quiet and permanent: a folder called "a/b" would not be a folder called
 * "a/b", it would be a file put somewhere nobody asked for.
 */
class LibraryTest {

    @Test
    fun `a folder name cannot carry a path separator`() {
        assertEquals("ab", Library.clean("a/b"))
        assertEquals("ab", Library.clean("a\\b"))
        assertEquals("music", Library.clean("  music  "))
    }

    @Test
    fun `a folder name cannot carry characters the file system refuses`() {
        assertEquals("what", Library.clean("wh<a>t?"))
        assertEquals("", Library.clean("///"))
    }

    @Test
    fun `a long name is cut rather than refused`() {
        val long = "가".repeat(80)
        assertEquals(40, Library.clean(long).length)
    }

    @Test
    fun `the top of the library and a folder inside it have different paths`() {
        assertEquals("Movies/Shard/", Library.relativePath(""))
        assertEquals("Movies/Shard/clips/", Library.relativePath("clips"))
    }

    @Test
    fun `the folder is read back out of the path it was written to`() {
        assertEquals("", Library.folderOf("Movies/Shard/"))
        assertEquals("clips", Library.folderOf("Movies/Shard/clips/"))
        // What the media store actually hands back has no leading slash and
        // sometimes no trailing one either.
        assertEquals("clips", Library.folderOf("Movies/Shard/clips"))
    }

    @Test
    fun `a path outside the library has no folder`() {
        // The old download location is now outside the library's marker too.
        assertEquals("", Library.folderOf("Download/Shard/"))
        assertEquals("", Library.folderOf("Pictures/"))
    }
}
