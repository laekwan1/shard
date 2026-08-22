// Document-start capture for the iOS shell — the same idea as the desktop's
// RECORDER (crates/shard/src/download/youtube.rs).
//
// WKWebView cannot watch the network, so we watch from inside the page: wrap
// fetch and XHR before the page's own scripts take their references. Two things
// are kept for the download to use later (read back on demand by Ask.js):
//   window.__shardSabr  — YouTube's captured `videoplayback` POST (url + body)
//   window.__shardMedia — the last progressive file and every .m3u8 seen
//
// Nothing is posted from here; the app runs Ask.js when the user asks to
// download, which reads these globals plus the player response.
(function () {
  "use strict";
  if (window.__shardSabrReady) return;
  window.__shardSabrReady = true;
  window.__shardSabr = null;
  window.__shardMedia = window.__shardMedia || { mp4: "", m3u8: "", list: [] };

  function noteMedia(url) {
    try {
      if (typeof url !== "string") return;
      var bare = url.split("?")[0].toLowerCase();
      if (bare.indexOf(".m3u8") >= 0) {
        if (!window.__shardMedia.m3u8) window.__shardMedia.m3u8 = url;
        if (window.__shardMedia.list.indexOf(url) < 0) window.__shardMedia.list.push(url);
      } else if (/\.mp4$/.test(bare) || /\.mp4\//.test(bare)) {
        window.__shardMedia.mp4 = url;
      }
    } catch (e) {}
  }

  var original = window.fetch;
  if (original) {
    window.fetch = function (input, init) {
      try {
        var isRequest = typeof Request !== "undefined" && input instanceof Request;
        var url = isRequest ? input.url : String(input);
        noteMedia(url);
        if (url.indexOf("videoplayback") >= 0) {
          var method = (init && init.method) || (isRequest ? input.method : "GET");
          if (method === "POST") {
            // Read the body through a clone; reading it directly would consume
            // the request the player is about to send.
            var source = init && init.body != null ? null : isRequest ? input.clone() : null;
            var bytes = source ? source.arrayBuffer() : Promise.resolve(init.body);
            Promise.resolve(bytes)
              .then(function (raw) {
                var u8 =
                  raw instanceof ArrayBuffer
                    ? new Uint8Array(raw)
                    : raw instanceof Uint8Array
                    ? raw
                    : null;
                if (!u8) return;
                var s = "";
                for (var i = 0; i < u8.length; i++) s += String.fromCharCode(u8[i]);
                window.__shardSabr = { url: url, body: btoa(s) };
              })
              .catch(function () {});
          }
        }
      } catch (e) {}
      return original.apply(this, arguments);
    };
  }

  // XHR too: hls.js and many players fetch over XMLHttpRequest, not fetch.
  try {
    var openOriginal = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function (method, url) {
      try {
        noteMedia(String(url));
      } catch (e) {}
      return openOriginal.apply(this, arguments);
    };
  } catch (e) {}

  // Keep video inline. To capture YouTube's request the video has to play, but
  // iOS otherwise throws it into its own fullscreen player, which the user then
  // has to dismiss before downloading. Marking every <video> playsinline keeps
  // it on the page (paired with allowsInlineMediaPlayback on the native side).
  function keepInline() {
    try {
      var vids = document.getElementsByTagName("video");
      for (var i = 0; i < vids.length; i++) {
        vids[i].setAttribute("playsinline", "");
        vids[i].setAttribute("webkit-playsinline", "");
        vids[i].playsInline = true;
      }
    } catch (e) {}
  }
  document.addEventListener("DOMContentLoaded", keepInline);
  setInterval(keepInline, 1000);

  // Kill the iOS callout (open link / copy / share) on long-press, so a hold on
  // a video is ours to act on, not the system's.
  try {
    var style = document.createElement("style");
    // Everything: the callout (copy / look up / translate) and text selection
    // both hijack a long-press, and this is a browser for saving video, not
    // reading. Inputs keep selection so an address or search box still works.
    style.textContent =
      "* { -webkit-touch-callout: none !important; -webkit-user-select: none !important; }" +
      "input, textarea, [contenteditable] { -webkit-user-select: text !important; }";
    (document.head || document.documentElement).appendChild(style);
  } catch (e) {}

  // Long-press a video to download it, the way the phone app does. Both the
  // touch timer and the contextmenu can fire for one press, so a short debounce
  // keeps it to a single message.
  var lastPress = 0;
  function askDownload(x, y) {
    var now = Date.now();
    if (now - lastPress < 1000) return;
    var el = document.elementFromPoint(x, y);
    var onVideo = false;
    while (el) {
      if (el.tagName === "VIDEO") { onVideo = true; break; }
      el = el.parentElement;
    }
    if (!onVideo) return;
    lastPress = now;
    try {
      window.webkit.messageHandlers.shard.postMessage({ type: "longpress" });
    } catch (e) {}
  }

  var pressTimer = null;
  document.addEventListener(
    "touchstart",
    function (e) {
      var t = e.touches[0];
      if (!t) return;
      var x = t.clientX, y = t.clientY;
      clearTimeout(pressTimer);
      pressTimer = setTimeout(function () { askDownload(x, y); }, 550);
    },
    true
  );
  ["touchend", "touchmove", "touchcancel", "scroll"].forEach(function (name) {
    document.addEventListener(name, function () { clearTimeout(pressTimer); }, true);
  });
})();
