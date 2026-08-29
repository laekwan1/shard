package net.sw.browser

import android.content.Context
import android.net.Uri
import org.json.JSONArray
import org.json.JSONObject

/**
 * The browser's home: the sites the user pinned and the ones they visit most.
 *
 * The desktop build keeps the same three lists in its config; the phone keeps
 * them in its own preferences rather than in the native engine's config file,
 * so a page opened here does not have to cross the C ABI to be remembered. The
 * start page ([START_URL]) reads [homeJson] over the `Shard` bridge and calls
 * back to open a tile or drop one.
 *
 * Frequency is counted per host, not per page: "자주 방문" answers which sites you
 * keep going to, and a hundred YouTube videos are one place, not a hundred.
 */
class Favorites(context: Context) {

    private val prefs = context.applicationContext.getSharedPreferences("favorites", Context.MODE_PRIVATE)

    // ---- the pinned sites --------------------------------------------------

    /** Pin the page, or unpin it if it was already pinned. Returns the new state. */
    fun toggle(url: String, title: String): Boolean {
        if (url.isBlank() || !url.startsWith("http")) return false
        val marks = bookmarks()
        val at = marks.indexOfFirst { it.url == url }
        if (at >= 0) {
            marks.removeAt(at)
        } else {
            // Newest first, so the most recently pinned reads at the top.
            marks.add(0, Mark(url, title.ifBlank { hostOf(url) }))
        }
        saveBookmarks(marks)
        return at < 0
    }

    fun isPinned(url: String): Boolean = bookmarks().any { it.url == url }

    // ---- what gets visited -------------------------------------------------

    /**
     * Note that a page was reached. Counts the host, and remembers the newest
     * pages so a later feature could show them; the start page only uses the
     * counts. The start page itself and other non-web addresses are not places.
     */
    fun recordVisit(url: String) {
        if (!url.startsWith("http")) return
        val host = hostOf(url)
        if (host.isBlank()) return
        val visits = visits()
        visits.put(host, visits.optInt(host, 0) + 1)
        prefs.edit().putString(KEY_VISITS, visits.toString()).apply()
    }

    /** Drop one "자주 방문" tile for good: the host is remembered as hidden. */
    fun hide(host: String) {
        val h = hostOf(host).ifBlank { host.trim().lowercase() }
        if (h.isBlank()) return
        val visits = visits()
        visits.remove(h)
        prefs.edit().putString(KEY_VISITS, visits.toString()).apply()
        val hidden = hidden()
        if (!hidden.contains(h)) {
            hidden.add(h)
            prefs.edit().putString(KEY_HIDDEN, JSONArray(hidden).toString()).apply()
        }
    }

    // ---- what the start page draws -----------------------------------------

    /**
     * The tiles, as the start page reads them: the pinned sites, then the eight
     * most-visited hosts. YouTube is always offered unless it is hidden or
     * already pinned — the browser exists to save from it, so a blank start page
     * would hide its one sure destination.
     */
    fun homeJson(): String {
        val marks = bookmarks()
        val bookmarksJson = JSONArray()
        for (m in marks) {
            bookmarksJson.put(JSONObject().put("url", m.url).put("title", m.title))
        }

        val hidden = hidden()
        val pinnedHosts = marks.map { hostOf(it.url) }.toSet()
        val visits = visits()
        val ranked = visits.keys().asSequence()
            .filter { it !in hidden }
            .map { it to visits.optInt(it, 0) }
            .sortedWith(compareByDescending<Pair<String, Int>> { it.second }.thenBy { it.first })
            .toList()

        val frequent = JSONArray()
        val seen = HashSet<String>()
        // The mobile site's own host, so the always-offered tile is the same place
        // a real visit records — otherwise "youtube.com" (the default) and
        // "m.youtube.com" (what actually gets opened and counted) would both show.
        // The label stays the clean "youtube.com".
        val youtube = "m.youtube.com"
        if (youtube !in hidden && youtube !in pinnedHosts) {
            frequent.put(
                JSONObject()
                    .put("url", "https://m.youtube.com/")
                    .put("title", "youtube.com")
                    .put("host", youtube)
            )
            seen.add(youtube)
        }
        for ((host, _) in ranked) {
            if (frequent.length() >= 8) break
            if (host in seen || host in pinnedHosts) continue
            frequent.put(tile("https://$host/", host))
            seen.add(host)
        }

        return JSONObject().put("bookmarks", bookmarksJson).put("frequent", frequent).toString()
    }

    private fun tile(url: String, host: String): JSONObject =
        JSONObject().put("url", url).put("title", host).put("host", host)

    // ---- storage -----------------------------------------------------------

    private data class Mark(val url: String, val title: String)

    private fun bookmarks(): MutableList<Mark> {
        val raw = prefs.getString(KEY_BOOKMARKS, null) ?: return mutableListOf()
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

    private fun saveBookmarks(marks: List<Mark>) {
        val arr = JSONArray()
        for (m in marks) arr.put(JSONObject().put("url", m.url).put("title", m.title))
        prefs.edit().putString(KEY_BOOKMARKS, arr.toString()).apply()
    }

    private fun visits(): JSONObject =
        runCatching { JSONObject(prefs.getString(KEY_VISITS, "{}")!!) }.getOrDefault(JSONObject())

    private fun hidden(): MutableList<String> {
        val out = mutableListOf<String>()
        runCatching {
            val arr = JSONArray(prefs.getString(KEY_HIDDEN, "[]"))
            for (i in 0 until arr.length()) out.add(arr.getString(i))
        }
        return out
    }

    /** Host without a leading www., lowercased — the key frequency is counted by. */
    private fun hostOf(url: String): String {
        val host = runCatching { Uri.parse(url).host }.getOrNull() ?: return ""
        return host.removePrefix("www.").lowercase()
    }

    companion object {
        /** The local speed-dial page, shown when the browser has no site in front. */
        const val START_URL = "file:///android_asset/start.html"

        /** The bridge name the start page reaches this through. */
        const val BRIDGE = "Shard"

        private const val KEY_BOOKMARKS = "bookmarks"
        private const val KEY_VISITS = "visits"
        private const val KEY_HIDDEN = "hidden"
    }
}
