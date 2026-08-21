// Media capture for the iOS shell.
//
// WKWebView cannot intercept a page's sub-requests the way Android's
// shouldInterceptRequest does, so we watch from inside the page instead: wrap
// fetch and XHR, and sweep the DOM. Anything that looks like a playlist or a
// video file is reported to the native side, which offers it for download.
//
// This is the weaker half of the port (a pure-native <video src> with only
// range requests can slip past), but it catches HLS and most streaming sites.
(function () {
  "use strict";
  if (window.__shardCapture) return;
  window.__shardCapture = true;

  var seen = Object.create(null);

  function post(url, kind) {
    if (!url || typeof url !== "string") return;
    // Absolute-ise against the page so a relative playlist URL is usable.
    try { url = new URL(url, location.href).href; } catch (e) { return; }
    if (!/^https?:/i.test(url)) return;
    var key = kind + "\n" + url;
    if (seen[key]) return;
    seen[key] = true;
    try {
      window.webkit.messageHandlers.shard.postMessage({
        type: "media",
        url: url,
        kind: kind,
        title: document.title || location.hostname
      });
    } catch (e) {}
  }

  // Classify a URL by extension. HLS playlists win over plain files because a
  // site usually offers both and the playlist carries every quality.
  function classify(url) {
    if (!url) return null;
    var u = url.split("?")[0].split("#")[0].toLowerCase();
    if (u.indexOf(".m3u8") !== -1) return "hls";
    if (/\.(mp4|m4v|mov|webm|mkv)$/.test(u)) return "file";
    return null;
  }

  function consider(url) {
    var kind = classify(url);
    if (kind) post(url, kind);
  }

  // fetch
  var origFetch = window.fetch;
  if (origFetch) {
    window.fetch = function (input) {
      try { consider(typeof input === "string" ? input : (input && input.url)); } catch (e) {}
      return origFetch.apply(this, arguments);
    };
  }

  // XHR
  var origOpen = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (method, url) {
    try { consider(url); } catch (e) {}
    return origOpen.apply(this, arguments);
  };

  // <video>/<source> src set directly, plus a periodic DOM sweep for players
  // that write the URL into an attribute we never saw requested.
  function sweep() {
    try {
      var vids = document.querySelectorAll("video, source, video source");
      for (var i = 0; i < vids.length; i++) {
        consider(vids[i].currentSrc || vids[i].src);
      }
      // Playlist URLs embedded in the page's own HTML (many sites inline them).
      var html = document.documentElement ? document.documentElement.innerHTML : "";
      var m = html.match(/https?:[^\s"'<>\\]+\.m3u8[^\s"'<>]*/g);
      if (m) for (var j = 0; j < m.length; j++) post(m[j], "hls");
    } catch (e) {}
  }

  var sweeps = 0;
  var timer = setInterval(function () {
    sweep();
    if (++sweeps > 40) clearInterval(timer); // ~20s, then rely on fetch/XHR
  }, 500);
  document.addEventListener("DOMContentLoaded", sweep);
})();
