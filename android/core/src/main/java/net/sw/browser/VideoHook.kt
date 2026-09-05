package net.sw.browser

import android.webkit.JavascriptInterface
import org.json.JSONObject

/**
 * Long-press on a video, reported back from the page.
 *
 * Two problems are solved here, and the second is the one that matters.
 *
 * Android's `HitTestResult` cannot see a `<video>` element — it reports images
 * and links and nothing else — so the page has to be asked which element was
 * pressed. And the listener cannot go on the video itself: every real player
 * covers it with its own controls, which swallow the touch. So the press is
 * caught on the document and matched to a video by where it landed.
 *
 * The second problem: `shouldInterceptRequest` never sees what the media
 * element fetches. WebView hands media requests to its own stack, so a site
 * whose player streams an MP4 directly leaves no trace in the interceptor — the
 * only URLs it catches are the little preview clips that JavaScript fetched.
 * The page itself does know, though: every request appears in Resource Timing,
 * media included. So the page is asked for that list too.
 */
class VideoHook(
    private val onLongPress: (VideoTarget) -> Unit,
    private val onSeekTouch: (Boolean) -> Unit = {},
    private val onSeekBand: (Float, Float) -> Unit = { _, _ -> },
) {

    /** What the page found under the finger. */
    data class VideoTarget(
        /** Playable URL, or empty when the player feeds the element privately. */
        val src: String,
        val width: Int,
        val height: Int,
        /** URLs the page fetched that look like media, newest last. */
        val seen: List<String> = emptyList(),
        /**
         * Identifiers scraped from around the pressed element — the poster
         * image, the link it sits inside, its own attributes.
         *
         * A `blob:` element will not say what it is playing, but the markup
         * around it almost always carries the video's id, and the media URL
         * carries the same id. Matching the two is what turns "everything this
         * page fetched" into "the one that was pressed".
         */
        val hints: List<String> = emptyList(),
        /** Words from the video's title, for sites that put a slug in the URL. */
        val titleWords: List<String> = emptyList(),
        /** The video's title as written on the page, for naming the file. */
        val title: String = "",
        /**
         * Media the page publishes about itself, with the quality it claims.
         *
         * The strongest evidence there is: the player is reading from the same
         * list. Sites expose it either as schema.org `VideoObject` markup or as
         * the player's own configuration object. When this is present nothing
         * else needs guessing at.
         */
        val declared: List<Declared> = emptyList(),
    ) {
        /** A player using Media Source Extensions has no URL worth fetching. */
        val hasUsableSrc: Boolean
            get() = src.startsWith("http://") || src.startsWith("https://")
    }

    @JavascriptInterface
    fun onVideoLongPress(payload: String?) {
        val target = runCatching { parse(payload.orEmpty()) }.getOrNull() ?: return
        onLongPress(target)
    }

    // The page's own seek bar reports while a finger is on it, so the browser's edge-swipe
    // (open the library / the address panel) does not ALSO fire from a scrub and yank a screen up
    // mid-seek (user hit: dragging the web seek bar opened the library/address bar). The page's
    // stopPropagation only stops DOM bubbling — the native gesture detector still sees the touch,
    // so the page has to tell us out of band. Runs on the JS thread; the flag it sets is @Volatile.
    @JavascriptInterface
    fun seekTouch(active: Boolean) {
        onSeekTouch(active)
    }

    // The seek bar reports its vertical band (top + height, in CSS px, viewport-relative) so the
    // native side knows where it is BEFORE a finger lands — no race with the async touchstart above.
    // A press whose Y falls in this band (converted to screen px) is a scrub, so the edge-swipe is
    // held off. top < 0 means the bar is hidden. Runs on the JS thread; fields it feeds are @Volatile.
    @JavascriptInterface
    fun seekBand(topCss: Float, heightCss: Float) {
        onSeekBand(topCss, heightCss)
    }

    private fun parse(payload: String): VideoTarget {
        val json = JSONObject(payload)
        return VideoTarget(
            src = json.optString("src"),
            width = json.optInt("width"),
            height = json.optInt("height"),
            seen = json.stringList("seen"),
            hints = json.stringList("hints"),
            titleWords = json.stringList("titleWords"),
            title = json.optString("title"),
            declared = json.declaredList(),
        )
    }

    /** One entry from the page's own media list. */
    data class Declared(val url: String, val quality: String)

    private fun JSONObject.declaredList(): List<Declared> {
        val array = optJSONArray("declared") ?: return emptyList()
        return buildList {
            for (i in 0 until array.length()) {
                val entry = array.optJSONObject(i) ?: continue
                val url = entry.optString("url")
                if (url.isNotBlank()) add(Declared(url, entry.optString("quality")))
            }
        }
    }

    private fun JSONObject.stringList(key: String): List<String> {
        val array = optJSONArray(key) ?: return emptyList()
        return buildList {
            for (i in 0 until array.length()) {
                array.optString(i).takeIf { it.isNotBlank() }?.let { add(it) }
            }
        }
    }

    companion object {
        /** Name the page sees. */
        const val BRIDGE = "ShardMedia"

        /**
         * Injected after every page load.
         *
         * Guarded against running twice. Re-scanning for videos is unnecessary
         * because the hit test happens at press time, so one added a second
         * later is found without an observer.
         */
        val SCRIPT = """
            (function () {
              if (window.__shardMediaHook) return;
              window.__shardMediaHook = true;

              var timer = null;
              var MEDIA = /\.(m3u8|mpd|mp4|m4v|webm|mkv|mov|ts)(\?|${'$'})|videoplayback|\/master|\/manifest/i;

              // An empty or placeholder src resolves to the page's own address,
              // which is a document and not a video. Everything offered has to
              // clear the media test and not be the page itself.
              function usable(u) {
                if (!u) return false;
                if (u.indexOf('blob:') === 0 || u.indexOf('data:') === 0) return false;
                if (u === location.href || u.split('#')[0] === location.href.split('#')[0]) return false;
                return MEDIA.test(u);
              }

              function sourceOf(v) {
                if (usable(v.currentSrc)) return v.currentSrc;
                if (usable(v.src)) return v.src;
                var sources = v.querySelectorAll('source[src]');
                for (var i = 0; i < sources.length; i++) {
                  if (usable(sources[i].src)) return sources[i].src;
                }
                return '';
              }

              // Everything the page has fetched that looks like media. This is
              // the only place a directly-streamed MP4 shows up: the native
              // interceptor never sees requests the media element makes.
              function seenMedia(v) {
                var urls = [];
                try {
                  var sources = v.querySelectorAll('source[src]');
                  for (var i = 0; i < sources.length; i++) {
                    if (usable(sources[i].src)) urls.push(sources[i].src);
                  }
                } catch (e) {}
                try {
                  var entries = performance.getEntriesByType('resource');
                  for (var j = 0; j < entries.length; j++) {
                    if (usable(entries[j].name)) urls.push(entries[j].name);
                  }
                } catch (e) {}
                return urls;
              }

              // Which video, if any, is under this point. Checking rectangles
              // rather than elementFromPoint is what sees through the controls
              // players lay over the top.
              function videoAt(x, y) {
                var videos = document.getElementsByTagName('video');
                for (var i = 0; i < videos.length; i++) {
                  var r = videos[i].getBoundingClientRect();
                  if (r.width > 0 && x >= r.left && x <= r.right && y >= r.top && y <= r.bottom) {
                    return videos[i];
                  }
                }
                return null;
              }

              // What the page says about its own video, which is what the
              // player itself is reading. Two well-established shapes: the
              // schema.org markup search engines index, and the configuration
              // object players hang off `window`.
              function declaredMedia() {
                var out = [];
                try {
                  var blocks = document.querySelectorAll('script[type="application/ld+json"]');
                  for (var i = 0; i < blocks.length; i++) {
                    var data = JSON.parse(blocks[i].textContent);
                    var items = [].concat(data);
                    for (var j = 0; j < items.length; j++) {
                      var it = items[j];
                      if (it && it.contentUrl && usable(it.contentUrl)) {
                        out.push({ url: it.contentUrl, quality: '' });
                      }
                    }
                  }
                } catch (e) {}

                try {
                  for (var k in window) {
                    if (k.indexOf('flashvars') !== 0 && k.indexOf('playerConfig') !== 0) continue;
                    var config = window[k];
                    var list = config && config.mediaDefinitions;
                    if (!list || !list.length) continue;
                    for (var m = 0; m < list.length; m++) {
                      var d = list[m];
                      if (!d || !d.videoUrl || !usable(d.videoUrl)) continue;
                      // Some entries carry an array of qualities rather than
                      // one — those are the master playlist, not a rendition.
                      out.push({
                        url: d.videoUrl,
                        quality: (typeof d.quality === 'string' || typeof d.quality === 'number')
                          ? String(d.quality) : ''
                      });
                    }
                  }
                } catch (e) {}
                return out;
              }

              // Anything around the element that might carry the video's id:
              // the poster image, the link it sits in, its own attributes.
              function hintsFor(v) {
                var text = [];
                try {
                  if (v.poster) text.push(v.poster);
                  if (v.id) text.push(v.id);
                  for (var i = 0; i < v.attributes.length; i++) {
                    var a = v.attributes[i];
                    if (a.name.indexOf('data-') === 0) text.push(a.value);
                  }
                  var node = v, depth = 0;
                  while (node && depth++ < 6) {
                    if (node.tagName === 'A' && node.href) { text.push(node.href); break; }
                    if (node.dataset) {
                      for (var k in node.dataset) text.push(node.dataset[k]);
                    }
                    node = node.parentElement;
                  }
                } catch (e) {}

                // Ids, not words: long digit runs and long hex tokens.
                var tokens = [], seenToken = {};
                for (var j = 0; j < text.length; j++) {
                  var found = String(text[j]).match(/[0-9]{6,}|[a-f0-9]{12,}/gi) || [];
                  for (var m = 0; m < found.length; m++) {
                    var t = found[m].toLowerCase();
                    if (!seenToken[t]) { seenToken[t] = 1; tokens.push(t); }
                  }
                }
                return tokens;
              }

              // What this video is called, nearest source first. The link it
              // sits in names it on a listing page; the heading names it on a
              // watch page; the document title is the last resort.
              function titleFor(v) {
                var candidates = [];
                try {
                  if (v.title) candidates.push(v.title);
                  if (v.getAttribute('aria-label')) candidates.push(v.getAttribute('aria-label'));
                  var node = v, depth = 0;
                  while (node && depth++ < 6) {
                    if (node.tagName === 'A') {
                      if (node.title) candidates.push(node.title);
                      if (node.textContent) candidates.push(node.textContent);
                      break;
                    }
                    node = node.parentElement;
                  }
                  var h1 = document.querySelector('h1');
                  if (h1) candidates.push(h1.textContent);
                  var og = document.querySelector('meta[property="og:title"]');
                  if (og) candidates.push(og.getAttribute('content'));
                } catch (e) {}
                candidates.push(document.title);

                for (var i = 0; i < candidates.length; i++) {
                  var t = String(candidates[i] || '').replace(/\s+/g, ' ').trim();
                  if (t.length >= 3 && t.length <= 140) return t;
                }
                return '';
              }

              // Slug words, for the sites that build media URLs out of them.
              function wordsOf(title) {
                var parts = title.toLowerCase().split(/[^a-z0-9]+/);
                var words = [];
                for (var i = 0; i < parts.length && words.length < 4; i++) {
                  if (parts[i].length >= 5) words.push(parts[i]);
                }
                return words;
              }

              // Report ONE video through the bridge — the shared body of the long-press and
              // the per-video download button below.
              function reportVideo(v) {
                if (!v) return;
                var title = titleFor(v);
                ${BRIDGE}.onVideoLongPress(JSON.stringify({
                  src: sourceOf(v),
                  width: v.videoWidth || 0,
                  height: v.videoHeight || 0,
                  seen: seenMedia(v),
                  hints: hintsFor(v),
                  titleWords: wordsOf(title),
                  title: title,
                  declared: declaredMedia()
                }));
              }

              // A download button over each video's top-right — the same affordance PC and
              // iOS give, so a video is saved without discovering the long-press. Buttons are
              // position:fixed and synced to each video's rectangle; a tap reports THAT video
              // through the bridge and the native quality sheet takes over.
              var dlButtons = [];   // {video, el}
              function makeDlButton() {
                var b = document.createElement('div');
                b.setAttribute('data-shard', 'dl');
                b.style.cssText =
                  'position:fixed;width:26px;height:26px;z-index:2147483646;display:none;' +
                  'align-items:center;justify-content:center;cursor:pointer;' +
                  'color:rgba(255,255,255,0.92);font:600 15px system-ui;' +
                  'border:1.5px solid rgba(255,255,255,0.7);border-radius:7px;' +
                  'background:rgba(0,0,0,0.35);';
                b.textContent = '↓';
                b.addEventListener('click', function (e) {
                  e.preventDefault(); e.stopPropagation();
                  reportVideo(b.__video);
                });
                document.documentElement.appendChild(b);
                return b;
              }
              // Keep YouTube's seek bar visible while playing — YouTube fades its bottom bar
              // (class ytp-autohide) during playback, so a video page looked like a bare
              // picture cut into the list below; the bar only returned on pause. CSS-only
              // override (opacity), so it cannot break the player and no-ops if the markup
              // changes.
              function keepYtControls() {
                try {
                  if (location.hostname.indexOf('youtube.com') < 0) return;
                  if (document.getElementById('shard-yt-style')) return;
                  var head = document.head || document.documentElement;
                  if (!head) return;
                  var s = document.createElement('style');
                  s.id = 'shard-yt-style';
                  s.textContent =
                    '.html5-video-player.ytp-autohide .ytp-chrome-bottom{opacity:1 !important;}' +
                    '.ytp-chrome-bottom{opacity:1 !important;}';
                  head.appendChild(s);
                } catch (e) {}
              }

              function syncDlButtons() {
                // Skip the per-video DOM work while the page is hidden (backgrounded, or the
                // library/player over it) — this 400ms timer otherwise woke the CPU forever on
                // every page, which shows up as battery. Resumes when the page is shown again.
                if (document.hidden) return;
                keepYtControls();
                var vids = document.getElementsByTagName('video');
                for (var i = 0; i < vids.length; i++) {
                  var v = vids[i], has = false;
                  for (var j = 0; j < dlButtons.length; j++) { if (dlButtons[j].video === v) { has = true; break; } }
                  if (!has) { var el = makeDlButton(); el.__video = v; dlButtons.push({ video: v, el: el }); }
                }
                for (var k = 0; k < dlButtons.length; k++) {
                  var x = dlButtons[k];
                  if (!x.video.isConnected) { x.el.style.display = 'none'; continue; }
                  var r = x.video.getBoundingClientRect();
                  var visible = r.width > 60 && r.height > 60 && r.bottom > 0 && r.top < innerHeight;
                  x.el.style.display = visible ? 'flex' : 'none';
                  if (visible) {
                    x.el.style.left = Math.min(innerWidth - 32, r.right - 32) + 'px';
                    x.el.style.top = Math.max(6, r.top + 6) + 'px';
                  }
                }
              }
              setInterval(syncDlButtons, 400);
              window.addEventListener('scroll', syncDlButtons, true);

              // Best-effort YouTube ad skipping — parity with iOS. Defensive: only acts when
              // an ad is actually shown (skip button, or the player's ad-showing class), all in
              // try/catch, so if YouTube changes markup it just does nothing.
              function skipAds() {
                if (document.hidden) return;
                try {
                  var skip = document.querySelector(
                    '.ytp-ad-skip-button, .ytp-ad-skip-button-modern, .ytp-skip-ad-button, .ytp-ad-skip-button-container button');
                  if (skip) { skip.click(); return; }
                  var player = document.querySelector('.html5-video-player');
                  if (player && player.classList.contains('ad-showing')) {
                    var v = player.querySelector('video');
                    if (v && isFinite(v.duration) && v.duration > 0) v.currentTime = v.duration;
                  }
                  var overlayClose = document.querySelector('.ytp-ad-overlay-close-button');
                  if (overlayClose) { try { overlayClose.click(); } catch (e) {} }
                } catch (e) {}
              }
              setInterval(skipAds, 350);
              window.addEventListener('resize', syncDlButtons, true);

              var lastFire = 0;
              function fire(x, y) {
                // A touch long-press makes the WebView fire both our touchstart
                // timer and a contextmenu event, so fire() was called twice and
                // two quality dialogs stacked. Swallow a second call within a
                // second of the first.
                var now = Date.now();
                if (now - lastFire < 1000) return;
                lastFire = now;
                reportVideo(videoAt(x, y));
              }

              function cancel() {
                if (timer) { clearTimeout(timer); timer = null; }
              }

              document.addEventListener('touchstart', function (e) {
                var t = e.touches[0];
                if (!t) return;
                var x = t.clientX, y = t.clientY;
                cancel();
                timer = setTimeout(function () { timer = null; fire(x, y); }, 550);
              }, true);

              ['touchend', 'touchmove', 'touchcancel', 'scroll'].forEach(function (name) {
                document.addEventListener(name, cancel, true);
              });

              // A mouse or a stylus produces a context menu instead.
              document.addEventListener('contextmenu', function (e) {
                if (videoAt(e.clientX, e.clientY)) { e.preventDefault(); fire(e.clientX, e.clientY); }
              }, true);
            })();

            // A thin seek bar pinned to the bottom edge of the playing video —
            // always visible, dragged to scrub. Mobile YouTube (m.youtube.com)
            // renders its own controls lazily and hides them, so there was no way
            // to scrub without first tapping; this is our own, on any site.
            (function () {
              if (window.__shardSeek) return;
              window.__shardSeek = true;
              var bar, played, dragging = false, vid = null;
              function build() {
                if (bar) return;
                bar = document.createElement('div');
                bar.style.cssText = 'position:fixed;left:0;height:18px;z-index:2147483000;display:none;touch-action:none;background:transparent;';
                var track = document.createElement('div');
                track.style.cssText = 'position:absolute;left:0;right:0;bottom:0;height:3px;background:rgba(255,255,255,.28);';
                played = document.createElement('div');
                played.style.cssText = 'position:absolute;left:0;bottom:0;height:3px;width:0;background:#ff0033;';
                bar.appendChild(track); bar.appendChild(played);
                (document.body || document.documentElement).appendChild(bar);
                var seek = function (cx) {
                  if (!vid || !isFinite(vid.duration) || vid.duration <= 0) return;
                  var r = bar.getBoundingClientRect();
                  var f = Math.min(1, Math.max(0, (cx - r.left) / r.width));
                  vid.currentTime = f * vid.duration;
                  played.style.width = (f * 100) + '%';
                };
                var release = function () { dragging = false; try { ${BRIDGE}.seekTouch(false); } catch (x) {} };
                bar.addEventListener('touchstart', function (e) { dragging = true; try { ${BRIDGE}.seekTouch(true); } catch (x) {} seek(e.touches[0].clientX); e.preventDefault(); e.stopPropagation(); }, { passive: false });
                bar.addEventListener('touchmove', function (e) { if (dragging) { seek(e.touches[0].clientX); e.preventDefault(); e.stopPropagation(); } }, { passive: false });
                bar.addEventListener('touchend', function (e) { release(); e.stopPropagation(); }, { passive: false });
                bar.addEventListener('touchcancel', function () { release(); }, { passive: false });
              }
              // Follow the video's bottom edge in REAL TIME while the page scrolls — on every
              // animation frame, not the 250ms tick. The tick alone lagged behind a smooth scroll,
              // so the position:fixed bar left a ghost trail (user hit; iOS fixed it the same way,
              // per-frame). place() just moves the bar to the video's current rect right now.
              function place() {
                if (!vid || !bar) return;
                var r = vid.getBoundingClientRect();
                bar.style.top = (r.bottom - 18) + 'px';
                bar.style.left = r.left + 'px';
                bar.style.width = r.width + 'px';
                // Tell native where the scrub band is (a bit taller than the visible 18px line so a
                // finger a hair above still counts), so the edge-swipe is held off there. CSS px.
                try { ${BRIDGE}.seekBand(r.bottom - 34, 48); } catch (x) {}
              }
              var rafPending = false;
              window.addEventListener('scroll', function () {
                if (rafPending) return;
                rafPending = true;
                requestAnimationFrame(function () { rafPending = false; place(); });
              }, true);
              function pick() {
                var best = null, area = 0, vids = document.getElementsByTagName('video');
                for (var i = 0; i < vids.length; i++) {
                  var v = vids[i], r = v.getBoundingClientRect(), a = r.width * r.height;
                  if (a > area && r.width > 120 && r.height > 80 && r.bottom > 0 && r.top < innerHeight) { best = v; area = a; }
                }
                return best;
              }
              function tick() {
                try {
                  build();
                  vid = pick();
                  if (!vid) { bar.style.display = 'none'; try { ${BRIDGE}.seekBand(-1, 0); } catch (x) {} return; }
                  var r = vid.getBoundingClientRect();
                  bar.style.display = 'block';
                  bar.style.left = r.left + 'px';
                  bar.style.width = r.width + 'px';
                  bar.style.top = (r.bottom - 18) + 'px';
                  try { ${BRIDGE}.seekBand(r.bottom - 34, 48); } catch (x) {}
                  if (!dragging && isFinite(vid.duration) && vid.duration > 0) {
                    played.style.width = ((vid.currentTime / vid.duration) * 100) + '%';
                  }
                } catch (e) {}
              }
              setInterval(tick, 250);
            })();
        """.trimIndent()
    }
}
