package net.sw.browser

import android.content.Context
import android.net.Uri
import org.json.JSONArray
import org.json.JSONObject

/**
 * The browser's home and favorites, kept the same shape iOS keeps them
 * (BookmarksStore + the "shard.homepage" setting): the sites the user pinned, the
 * pages they have visited (newest first), and the one address the home button
 * goes to. Persisted in SharedPreferences as JSON — small enough to read and
 * write whole on every change.
 *
 * The favorites page ([homeJson]) shows two sections, exactly as iOS does minus
 * the "자주 방문" tiles the user asked to drop here: 즐겨찾기 (the pins) and
 * 방문기록 (the history). The `Shard` bridge reads this and calls back to open a
 * row, remove one, or clear the history.
 */
class Favorites(context: Context) {

    private val prefs = context.applicationContext.getSharedPreferences("favorites", Context.MODE_PRIVATE)

    // ---- the homepage ------------------------------------------------------

    /** Where the home button goes and the first page opened. iOS's default too. */
    fun homepage(): String = prefs.getString(KEY_HOMEPAGE, DEFAULT_HOME) ?: DEFAULT_HOME

    fun setHomepage(url: String) {
        val v = normalize(url)
        if (v.isNotBlank()) prefs.edit().putString(KEY_HOMEPAGE, v).apply()
    }

    // ---- the pinned sites --------------------------------------------------

    /** Pin the page, or unpin it if already pinned (the star toggles). */
    fun toggle(url: String, title: String): Boolean {
        if (url.isBlank() || !url.startsWith("http")) return false
        val marks = bookmarks()
        val at = marks.indexOfFirst { it.url == url }
        if (at >= 0) {
            marks.removeAt(at)
        } else {
            marks.add(0, Mark(url, title.ifBlank { hostOf(url) }))
        }
        saveBookmarks(marks)
        return at < 0
    }

    fun isBookmarked(url: String): Boolean = bookmarks().any { it.url == url }

    fun removeBookmark(url: String) {
        val marks = bookmarks()
        if (marks.removeAll { it.url == url }) saveBookmarks(marks)
    }

    // ---- what gets visited (history) ---------------------------------------

    /**
     * Note a page load: push it onto the history (deduped, newest first, capped),
     * the way iOS's recordVisit does. The start page and other non-web addresses
     * are not places.
     */
    fun recordVisit(url: String, title: String) {
        if (!url.startsWith("http")) return
        val hist = history()
        hist.removeAll { it.url == url }
        hist.add(0, Mark(url, title.ifBlank { hostOf(url) }))
        while (hist.size > 60) hist.removeAt(hist.size - 1)
        saveHistory(hist)
    }

    fun removeHistory(url: String) {
        val hist = history()
        if (hist.removeAll { it.url == url }) saveHistory(hist)
    }

    fun clearHistory() {
        prefs.edit().remove(KEY_HISTORY).apply()
    }

    // ---- what the favorites page draws -------------------------------------

    /** The two sections the start page reads: the pins and the history. */
    fun homeJson(): String {
        val marks = JSONArray()
        for (m in bookmarks()) {
            marks.put(JSONObject().put("url", m.url).put("title", m.title))
        }
        val hist = JSONArray()
        for (m in history()) {
            hist.put(JSONObject().put("url", m.url).put("title", m.title))
        }
        return JSONObject().put("bookmarks", marks).put("history", hist).toString()
    }

    // ---- storage -----------------------------------------------------------

    private data class Mark(val url: String, val title: String)

    private fun bookmarks(): MutableList<Mark> = readList(KEY_BOOKMARKS)
    private fun history(): MutableList<Mark> = readList(KEY_HISTORY)

    private fun readList(key: String): MutableList<Mark> {
        val raw = prefs.getString(key, null) ?: return mutableListOf()
        val out = mutableListOf<Mark>()
        runCatching {
            val arr = JSONArray(raw)
            for (i in 0 until arr.length()) {
                val o = arr.getJSONObject(i)
                out.add(Mark(o.optString("url"), o.optString("title")))
            }
        }
        return out
    }

    private fun saveBookmarks(marks: List<Mark>) = writeList(KEY_BOOKMARKS, marks)
    private fun saveHistory(hist: List<Mark>) = writeList(KEY_HISTORY, hist)

    private fun writeList(key: String, list: List<Mark>) {
        val arr = JSONArray()
        for (m in list) arr.put(JSONObject().put("url", m.url).put("title", m.title))
        prefs.edit().putString(key, arr.toString()).apply()
    }

    /** Host without a leading www., lowercased — the fallback title/label. */
    private fun hostOf(url: String): String {
        val host = runCatching { Uri.parse(url).host }.getOrNull() ?: return ""
        return host.removePrefix("www.").lowercase()
    }

    /** Turn what the user typed into a URL, the way WebModel.normalize does on iOS. */
    private fun normalize(input: String): String {
        val t = input.trim()
        if (t.isEmpty()) return ""
        if (t.contains("://")) return t
        // A single word with a dot is a host; anything else is a search.
        return if (Regex("^[^\\s]+\\.[^\\s]{2,}").containsMatchIn(t) && !t.contains(' ')) {
            "https://$t"
        } else {
            "https://www.google.com/search?q=" + Uri.encode(t)
        }
    }

    companion object {
        /** The local favorites page, shown as an overlay when the star is tapped. */
        const val START_URL = "file:///android_asset/start.html"

        /** The bridge name the favorites page reaches this through. */
        const val BRIDGE = "Shard"

        /** iOS's default homepage. */
        const val DEFAULT_HOME = "https://m.youtube.com"

        private const val KEY_BOOKMARKS = "bookmarks"
        private const val KEY_HISTORY = "history"
        private const val KEY_HOMEPAGE = "homepage"
    }
}
