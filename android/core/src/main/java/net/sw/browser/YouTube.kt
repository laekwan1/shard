package net.sw.browser

import org.json.JSONObject

/**
 * YouTube, which needs a different approach from every other site.
 *
 * Elsewhere the problem is finding the video's address. Here there is no
 * address to find. Measured on a live page: the formats are all listed with
 * URLs, every one of those URLs answers 403, removing the throttling parameter
 * does not help, and the player's own code no longer contains the function that
 * used to rewrite it. The desktop page has gone further and lists forty formats
 * with no URL at all.
 *
 * What the player does instead is described in [Sabr]: it POSTs its situation
 * to one endpoint and the server decides what to send. This file's job is to
 * hand the Android side the two things it cannot obtain on its own — a real
 * request to use as a template, and the list of formats with the identifiers
 * that request needs to name one.
 *
 * The template has to be captured rather than built, because it carries a
 * server configuration blob and a bot-check token that only a running page can
 * produce. That is also why this only works after the video has played for a
 * moment: until the player has fetched something, there is nothing to copy.
 */
object YouTube {

    /** One downloadable stream as the page describes it. */
    data class Format(
        val itag: Int,
        val mimeType: String,
        /** "1080p60" for video, blank for audio. */
        val quality: String,
        val bitrate: Int,
        val bytes: Long,
        /** Identifies this exact encoding to the server; a format is the pair. */
        val lastModified: Long,
        val xtags: String,
        val hasVideo: Boolean,
        val hasAudio: Boolean,
        /** "ko", "en" — blank on a video with one soundtrack. */
        val audioLanguage: String = "",
        /** What the site calls it: "한국어", "English (original)". */
        val audioName: String = "",
        /** The track the video plays when nothing is chosen. */
        val audioDefault: Boolean = false,
    ) {
        /** "AV1", "VP9", "H.264", "Opus", "AAC" — what the size hinges on. */
        val codec: String
            get() = when {
                mimeType.contains("av01") -> "AV1"
                mimeType.contains("vp9") || mimeType.contains("vp09") -> "VP9"
                mimeType.contains("avc1") -> "H.264"
                mimeType.contains("opus") -> "Opus"
                mimeType.contains("mp4a") -> "AAC"
                mimeType.contains("ec-3") -> "EAC3"
                mimeType.contains("ac-3") -> "AC3"
                else -> ""
            }

        val container: String
            get() = if (mimeType.startsWith("audio")) {
                if (codec == "Opus") "webm" else "m4a"
            } else {
                if (mimeType.contains("webm")) "webm" else "mp4"
            }

        fun asTrack() = Sabr.Track(itag, lastModified, xtags, bytes)
    }

    /** Everything the page had to offer. */
    data class Offer(
        val template: Sabr.Template?,
        val formats: List<Format>,
        val title: String,
        /** The video's thumbnail, for standing in as a song's cover art. */
        val cover: String = "",
        /** Set when the page could be read but had not played anything yet. */
        val notPlayedYet: Boolean,
    )

    fun isWatchPage(url: String): Boolean =
        url.contains("youtube.com/watch") || url.contains("youtu.be/") ||
            url.contains("youtube.com/shorts/") || url.contains("m.youtube.com/watch")

    /** Parse what [SCRIPT] sent back. */
    fun parse(payload: String): Offer {
        val json = runCatching { JSONObject(payload) }.getOrNull()
            ?: return Offer(null, emptyList(), "", notPlayedYet = false)

        val formats = buildList {
            val array = json.optJSONArray("formats")
            for (i in 0 until (array?.length() ?: 0)) {
                val f = array!!.optJSONObject(i) ?: continue
                val mime = f.optString("mimeType")
                add(
                    Format(
                        itag = f.optInt("itag"),
                        mimeType = mime,
                        quality = f.optString("quality"),
                        bitrate = f.optInt("bitrate"),
                        bytes = f.optString("bytes").toLongOrNull() ?: -1,
                        lastModified = f.optString("lastModified").toLongOrNull() ?: 0,
                        xtags = f.optString("xtags"),
                        audioLanguage = f.optString("audioLanguage"),
                        audioName = f.optString("audioName"),
                        audioDefault = f.optBoolean("audioDefault"),
                        hasVideo = mime.startsWith("video"),
                        hasAudio = mime.startsWith("audio") || f.optBoolean("progressive"),
                    )
                )
            }
        }

        val url = json.optString("templateUrl")
        val body = json.optString("templateBody")
        val template = if (url.isNotBlank() && body.isNotBlank()) {
            runCatching { Sabr.Template(url, android.util.Base64.decode(body, android.util.Base64.DEFAULT)) }
                .getOrNull()
        } else {
            null
        }

        return Offer(
            template = template,
            formats = formats,
            title = json.optString("title"),
            cover = json.optString("thumbnail"),
            notPlayedYet = template == null && formats.isNotEmpty(),
        )
    }

    /**
     * The soundtrack to save, chosen the way the desktop chooses it.
     *
     * Language first, quality second. The two used to be run together, and the
     * result was a Korean video saved with its English dub because that track
     * happened to be a few kilobits smaller — a worse answer to a question
     * nobody asked. Which language it is and how good it is are separate
     * questions, and the first one has to be settled before the second is even
     * interesting.
     *
     * [language] defaults to the phone's own, which is the right guess when
     * there is nothing else to go on.
     */
    fun bestAudio(
        formats: List<Format>,
        forContainer: String? = null,
        language: String = deviceLanguage(),
    ): Format? {
        val all = formats.filter { it.hasAudio && !it.hasVideo && it.lastModified > 0 }
        if (all.isEmpty()) return null

        // Language, in order of how well it is known: what was asked for, then
        // what the video plays by itself, then anything at all.
        val wanted = if (language.isBlank()) emptyList()
                     else all.filter { it.audioLanguage.startsWith(language) }
        val spoken = all.filter { it.audioDefault }
        val candidates = wanted.ifEmpty { spoken }.ifEmpty { all }

        // Tracks the page marks with extra tags are alternate mixes and are
        // taken only when they are all there is.
        val plain = candidates.filter { it.xtags.isEmpty() }.ifEmpty { candidates }

        // Matched to the video's container, because joining the two is done by
        // Android's own muxer and it will not put Opus into an MP4. Opus is the
        // smaller track and worth having wherever the video is WebM; where it
        // is not, AAC costs a few megabytes and actually produces a file.
        val usable = when (forContainer) {
            "mp4" -> plain.filter { it.codec == "AAC" }
            "webm" -> plain.filter { it.codec == "Opus" }
            else -> plain
        }.ifEmpty { plain }

        return usable.filter { it.bitrate in 64_000..145_000 }.minByOrNull { it.bytes.coerceAtLeast(0) }
            ?: usable.minByOrNull { it.bitrate }
    }

    /**
     * The best-quality track for a music file, in the format phones play.
     *
     * AAC in an m4a, not the smaller, higher-rated Opus: a Galaxy and an iPhone
     * both open AAC in their own music app, and an iPhone will not play Opus at
     * all. Highest bitrate rather than smallest — the opposite of the choice
     * made for a video's soundtrack, because a file kept as music is worth the
     * few extra megabytes.
     */
    fun bestMusicAudio(formats: List<Format>, language: String = deviceLanguage()): Format? {
        val all = formats.filter { it.hasAudio && !it.hasVideo && it.lastModified > 0 }
        if (all.isEmpty()) return null
        val wanted = if (language.isBlank()) emptyList()
                     else all.filter { it.audioLanguage.startsWith(language) }
        val spoken = all.filter { it.audioDefault }
        val candidates = wanted.ifEmpty { spoken }.ifEmpty { all }
        val plain = candidates.filter { it.xtags.isEmpty() }.ifEmpty { candidates }
        val aac = plain.filter { it.codec == "AAC" }.ifEmpty { plain }
        return aac.maxByOrNull { it.bitrate }
    }

    /** The phone's language as a bare code: "ko", "en". */
    fun deviceLanguage(): String =
        runCatching { java.util.Locale.getDefault().language.orEmpty() }.getOrDefault("")

    /** Every language a video offers, for anything that wants to list them. */
    fun languages(formats: List<Format>): List<Pair<String, String>> =
        formats.filter { it.hasAudio && !it.hasVideo && it.audioLanguage.isNotBlank() }
            .distinctBy { it.audioLanguage }
            .map { it.audioLanguage to it.audioName }

    /**
     * Video tracks, smallest first within each resolution.
     *
     * Sorted by resolution descending then size ascending, so the efficient
     * codec sits at the top of each group and the saving is visible rather than
     * something the user has to know about.
     */
    fun videoTracks(formats: List<Format>): List<Format> =
        formats.filter { it.hasVideo && !it.hasAudio && it.lastModified > 0 && it.xtags.isEmpty() }
            .sortedWith(compareByDescending<Format> { heightOf(it.quality) }.thenBy { it.bytes })

    /** The number in "1080p60", for ordering. */
    fun heightOf(quality: String): Int =
        Regex("(\\d{3,4})p").find(quality)?.groupValues?.get(1)?.toIntOrNull() ?: 0

    /** Name the bridge exposes to the page. */
    const val BRIDGE = "ShardYouTube"

    /**
     * Keeps a copy of the player's own delivery request.
     *
     * Installed as the page loads, because the request cannot be reconstructed
     * afterwards — it is a POST whose body is built by the player, and the
     * browser's resource log records only that a URL was fetched.
     *
     * The body has to be read through a clone. The player passes a `Request`
     * object rather than a URL and options, and reading a request's body
     * consumes it; cloning is what lets it be seen without stealing it from the
     * player that is about to send it.
     */
    val RECORDER = """
        (function () {
          // Injected twice into the same page, the second copy used to throw
          // away what the first had captured. On a site that changes video
          // without loading a document — Shorts — that is the only capture
          // there is, so the download button found nothing and asked the user
          // to play the video they were already watching.
          if (window.__shardSabrReady) return;
          window.__shardSabrReady = true;
          window.__shardSabr = null;

          var original = window.fetch;
          window.fetch = function (input, init) {
            try {
              var isRequest = (typeof Request !== 'undefined') && (input instanceof Request);
              var url = isRequest ? input.url : String(input);
              if (url.indexOf('videoplayback') >= 0) {
                var method = (init && init.method) || (isRequest ? input.method : 'GET');
                if (method === 'POST') {
                  var source = (init && init.body != null) ? null : (isRequest ? input.clone() : null);
                  var bytes = source ? source.arrayBuffer()
                                     : Promise.resolve(init.body);
                  Promise.resolve(bytes).then(function (raw) {
                    var u8 = raw instanceof ArrayBuffer ? new Uint8Array(raw)
                           : (raw instanceof Uint8Array ? raw : null);
                    if (!u8) return;
                    var s = '';
                    for (var i = 0; i < u8.length; i++) s += String.fromCharCode(u8[i]);
                    // Which video the player was asking for when it sent
                    // this. Shorts changes video without loading a document and
                    // fetches the next one before it is reached, so the last
                    // request captured is often not the one on screen — and a
                    // download built from it asks the server for a video nobody
                    // is watching, which it answers with nothing at all.
                    window.__shardSabr = {
                      url: url,
                      body: btoa(s),
                      where: location.pathname + location.search
                    };
                  }).catch(function () {});
                }
              }
            } catch (e) {}
            return original.apply(this, arguments);
          };
        })();
    """.trimIndent()

    /**
     * Reports the formats and the captured request.
     *
     * The player is asked for its response rather than `ytInitialPlayerResponse`
     * being read: that global describes whichever video the page was opened on,
     * and YouTube moves between videos without loading a page.
     */
    val SCRIPT = """
        (function () {
          function player() {
            var byId = document.querySelector('#movie_player') ||
                       document.querySelector('.html5-video-player');
            if (byId && typeof byId.getPlayerResponse === 'function') return byId;
            var videos = document.getElementsByTagName('video');
            for (var i = 0; i < videos.length; i++) {
              var node = videos[i], depth = 0;
              while (node && depth++ < 12) {
                if (typeof node.getPlayerResponse === 'function') return node;
                node = node.parentElement;
              }
            }
            return byId || null;
          }

          function response() {
            var p = player();
            if (p && typeof p.getPlayerResponse === 'function') {
              try {
                var live = p.getPlayerResponse();
                if (live && live.streamingData) return live;
              } catch (e) {}
            }
            if (window.ytInitialPlayerResponse && window.ytInitialPlayerResponse.streamingData) {
              return window.ytInitialPlayerResponse;
            }
            return null;
          }

          var data = response();
          if (!data || !data.streamingData) {
            ${BRIDGE}.onFormats(JSON.stringify({
              formats: [],
              reason: player() ? 'no-streams' : 'no-player'
            }));
            return;
          }

          var out = [];
          var lists = [data.streamingData.formats || [], data.streamingData.adaptiveFormats || []];
          for (var l = 0; l < lists.length; l++) {
            for (var i = 0; i < lists[l].length; i++) {
              var f = lists[l][i];
              out.push({
                itag: f.itag,
                mimeType: f.mimeType || '',
                quality: f.qualityLabel || f.audioQuality || '',
                bitrate: f.bitrate || 0,
                bytes: f.contentLength || '-1',
                lastModified: f.lastModified || '0',
                xtags: f.xtags || '',
                // Which language this sound is, what it is called, and whether
                // it is the one the video plays by itself. A video dubbed into
                // several languages lists one audio track per language, and
                // nothing else in the format distinguishes them.
                audioLanguage: (f.audioTrack && f.audioTrack.id ? String(f.audioTrack.id).split('.')[0] : ''),
                audioName: (f.audioTrack && f.audioTrack.displayName) || '',
                audioDefault: !!(f.audioTrack && f.audioTrack.audioIsDefault),
                progressive: l === 0
              });
            }
          }

          // Only used for the video it was captured on.
          var held = window.__shardSabr;
          var here = location.pathname + location.search;
          var captured = (held && held.where === here) ? held : null;
          var thumbs = ((data.videoDetails || {}).thumbnail || {}).thumbnails || [];
          ${BRIDGE}.onFormats(JSON.stringify({
            formats: out,
            title: (data.videoDetails || {}).title || '',
            thumbnail: thumbs.length ? thumbs[thumbs.length - 1].url : '',
            templateUrl: captured ? captured.url : '',
            templateBody: captured ? captured.body : ''
          }));
        })();
    """.trimIndent()
}
