// Built on demand, when the user asks to download: read the player response and
// the globals Capture.js filled, and return the offer as a JSON string. Adapted
// from the desktop's ASK (crates/shard/src/download/youtube.rs) — the one change
// is that it returns the JSON (for WKWebView's evaluateJavaScript) instead of
// posting it over an IPC channel.
(function () {
  function player() {
    var byId =
      document.querySelector("#movie_player") || document.querySelector(".html5-video-player");
    if (byId && typeof byId.getPlayerResponse === "function") return byId;
    var videos = document.getElementsByTagName("video");
    for (var i = 0; i < videos.length; i++) {
      var node = videos[i],
        depth = 0;
      while (node && depth++ < 12) {
        if (typeof node.getPlayerResponse === "function") return node;
        node = node.parentElement;
      }
    }
    return byId || null;
  }

  function response() {
    var p = player();
    if (p && typeof p.getPlayerResponse === "function") {
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

  function thumb(data) {
    try {
      var all = data.videoDetails.thumbnail.thumbnails || [];
      var best = null;
      for (var i = 0; i < all.length; i++) {
        if (!best || (all[i].width || 0) > (best.width || 0)) best = all[i];
      }
      return best && best.url ? String(best.url) : "";
    } catch (e) {
      return "";
    }
  }

  function fallback() {
    var media = window.__shardMedia || { mp4: "", m3u8: "", list: [] };
    media.list = media.list || [];
    try {
      var raw =
        document.documentElement.innerHTML.match(
          /https?:[^\s"'<>\\]+(?:\\\/[^\s"'<>\\]+)*\.m3u8[^\s"'<>]*/g
        ) || [];
      for (var i = 0; i < raw.length; i++) {
        var u = raw[i].replace(/\\\//g, "/");
        if (media.list.indexOf(u) < 0) media.list.push(u);
        if (!media.m3u8) media.m3u8 = u;
      }
    } catch (e) {}
    var mp4 = media.mp4 || "";
    var hls = media.m3u8 || "";
    if (!mp4) {
      var vids = document.getElementsByTagName("video");
      for (var i = 0; i < vids.length; i++) {
        var src = vids[i].currentSrc || vids[i].src || "";
        if (src && src.indexOf("blob:") !== 0 && src.split("?")[0].toLowerCase().indexOf(".mp4") >= 0) {
          mp4 = src;
          break;
        }
      }
    }
    var picture = "";
    var og = document.querySelector('meta[property="og:image"]');
    if (og) picture = og.getAttribute("content") || "";
    return {
      formats: [],
      media: mp4,
      hls: hls,
      hlsList: (media.list || []).join("\n"),
      referer: location.href,
      title: document.title || "",
      thumb: picture,
      reason: mp4 || hls ? "" : player() ? "no-streams" : "no-player"
    };
  }

  var data = response();
  if (!data || !data.streamingData) {
    return JSON.stringify(fallback());
  }

  var out = [];
  var lists = [data.streamingData.formats || [], data.streamingData.adaptiveFormats || []];
  for (var l = 0; l < lists.length; l++) {
    for (var i = 0; i < lists[l].length; i++) {
      var f = lists[l][i];
      out.push({
        itag: f.itag,
        mimeType: f.mimeType || "",
        quality: f.qualityLabel || f.audioQuality || "",
        bitrate: f.bitrate || 0,
        bytes: String(f.contentLength || "0"),
        lastModified: String(f.lastModified || "0"),
        durationMs: String(f.approxDurationMs || 0),
        xtags: f.xtags || "",
        audioLanguage: f.audioTrack && f.audioTrack.id ? String(f.audioTrack.id).split(".")[0] : "",
        audioName: (f.audioTrack && f.audioTrack.displayName) || "",
        audioDefault: !!(f.audioTrack && f.audioTrack.audioIsDefault)
      });
    }
  }

  var captured = window.__shardSabr;
  return JSON.stringify({
    formats: out,
    media: "",
    hls: "",
    hlsList: "",
    referer: location.href,
    title: (data.videoDetails || {}).title || "",
    thumb: thumb(data),
    videoId: (data.videoDetails || {}).videoId || "",
    templateUrl: captured ? captured.url : "",
    templateBody: captured ? captured.body : "",
    reason: captured ? "" : "not-played"
  });
})();
