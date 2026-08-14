package net.sw.browser

/**
 * Who the request should look like it is coming from.
 *
 * Media CDNs check this. Measured against one: the same segment URL returns
 * 410 with no headers, 404 with only a user agent, and 200 with a `Referer`
 * naming the page. The player sends those headers as a matter of course; a
 * download started from outside the page has to be told to.
 *
 * Captured headers are preferred where they exist — they are what the page
 * actually sent — and this fills the gaps, which on a site whose media never
 * reaches the interceptor means filling all of them.
 */
data class PageContext(
    /** The page the video is playing on. */
    val referer: String = "",
    /** The browser's own user agent, so the request matches the session. */
    val userAgent: String = "",
) {
    /** `headers` with the missing essentials filled in. */
    fun applyTo(headers: Map<String, String>): Map<String, String> {
        val merged = headers.toMutableMap()
        if (referer.isNotBlank() && merged.keys.none { it.equals("Referer", true) }) {
            merged["Referer"] = referer
        }
        if (userAgent.isNotBlank() && merged.keys.none { it.equals("User-Agent", true) }) {
            merged["User-Agent"] = userAgent
        }
        if (merged.keys.none { it.equals("Accept", true) }) {
            merged["Accept"] = "*/*"
        }
        return merged
    }

    companion object {
        /** The page currently open, kept here so both the chooser and the
         *  downloader see the same thing without threading it through every
         *  call. Written on the main thread when a page loads. */
        @Volatile
        var current: PageContext = PageContext()
    }
}
