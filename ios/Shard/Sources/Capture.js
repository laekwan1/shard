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
  // Request headers the PAGE'S player set on its media fetches (hls.js etc.). The
  // browser's own headers (Cookie/UA/Referer) are NOT visible here — only what the
  // page passes to fetch/XHR — so this catches a custom auth token if the player
  // uses one. Native-HLS playback bypasses JS entirely and leaves this empty.
  window.__shardMedia.headers = window.__shardMedia.headers || {};
  function isMediaURL(url) {
    try {
      var bare = String(url).split("?")[0].toLowerCase();
      return bare.indexOf(".m3u8") >= 0 || bare.indexOf(".ts") >= 0 ||
             /\.mp4($|\/)/.test(bare) || bare.indexOf(".m4s") >= 0 || bare.indexOf("segment") >= 0;
    } catch (e) { return false; }
  }
  function noteHeaders(url, obj) {
    try {
      if (!obj || !isMediaURL(url)) return;
      var skip = /^(cookie|user-agent|referer|origin|host|accept-encoding|connection|content-length|sec-)/i;
      var each = function (name, value) {
        if (name && value != null && !skip.test(name)) window.__shardMedia.headers[name] = String(value);
      };
      if (typeof Headers !== "undefined" && obj instanceof Headers) obj.forEach(function (v, k) { each(k, v); });
      else if (Array.isArray(obj)) obj.forEach(function (p) { each(p[0], p[1]); });
      else Object.keys(obj).forEach(function (k) { each(k, obj[k]); });
    } catch (e) {}
  }

  function noteMedia(url) {
    try {
      if (typeof url !== "string") return;
      var bare = url.split("?")[0].toLowerCase();
      if (bare.indexOf(".m3u8") >= 0) {
        // Keep the LATEST m3u8, not the first: these URLs are signed and expire, so
        // by download time an old one returned 410 ("조각을 받지 못했습니다"). The most
        // recent sighting is the freshest. Prefer a master over a media playlist,
        // but a newer master still wins.
        var isMaster = bare.indexOf("master") >= 0 || bare.indexOf("playlist") >= 0;
        var haveMaster = /master|playlist/i.test(window.__shardMedia.m3u8 || "");
        if (isMaster || !haveMaster) window.__shardMedia.m3u8 = url;
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
        if (init && init.headers) noteHeaders(url, init.headers);
        if (isRequest && input.headers) noteHeaders(url, input.headers);
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

  // XHR too: hls.js and many players fetch over XMLHttpRequest, not fetch. Remember
  // the URL on open and each setRequestHeader, then record them (a custom token the
  // player attaches to its segment requests).
  try {
    var openOriginal = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function (method, url) {
      try {
        this.__shardURL = String(url);
        noteMedia(this.__shardURL);
      } catch (e) {}
      return openOriginal.apply(this, arguments);
    };
    var setHeaderOriginal = XMLHttpRequest.prototype.setRequestHeader;
    XMLHttpRequest.prototype.setRequestHeader = function (name, value) {
      try {
        var h = {}; h[name] = value;
        noteHeaders(this.__shardURL || "", h);
      } catch (e) {}
      return setHeaderOriginal.apply(this, arguments);
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

  // NOTE: an earlier attempt to force a device-width viewport (rewriting the page's
  // <meta name=viewport> and re-asserting it from a MutationObserver) was removed —
  // it fought the page on every DOM change and drove a runaway zoom on ALL sites,
  // even pushing the app's own address bar off screen. The page's own viewport is
  // left alone. pornhub's zoomed video page is handled a gentler way if at all.

  // Tell the native side when a web video goes full screen, so the download
  // button (which belongs over the windowed page) can hide there.
  function reportFullscreen(on) {
    try {
      window.webkit.messageHandlers.shard.postMessage({ type: "fullscreen", on: !!on });
    } catch (e) {}
  }
  document.addEventListener("fullscreenchange", function () {
    reportFullscreen(document.fullscreenElement);
  }, true);
  document.addEventListener("webkitfullscreenchange", function () {
    reportFullscreen(document.webkitFullscreenElement);
  }, true);
  // iOS's own native video full screen fires these on the <video> element.
  document.addEventListener("webkitbeginfullscreen", function () { reportFullscreen(true); }, true);
  document.addEventListener("webkitendfullscreen", function () { reportFullscreen(false); }, true);

  // When the library screen is up (browser hidden behind it), the library forcing
  // landscape rotates this page too, and a playing site video (pornhub) then goes
  // to native full screen on its own — hijacking the library's own full screen. So
  // while the app marks the browser inactive, cancel any full screen the page tries
  // to enter and pause the video. Default active so normal browsing is untouched.
  if (window.__shardBrowserActive === undefined) window.__shardBrowserActive = true;
  function exitAnyFullscreen() {
    if (window.__shardBrowserActive) return;
    // Both the <video> native path (webkit*) AND the standard Fullscreen API a
    // site's own player may use on orientationchange.
    try { if (document.exitFullscreen) document.exitFullscreen(); } catch (e) {}
    try { if (document.webkitExitFullscreen) document.webkitExitFullscreen(); } catch (e) {}
    try {
      var vids = document.getElementsByTagName("video");
      for (var i = 0; i < vids.length; i++) {
        try { if (vids[i].webkitExitFullscreen) vids[i].webkitExitFullscreen(); } catch (e) {}
        try { vids[i].pause(); } catch (e) {}
      }
    } catch (e) {}
  }
  document.addEventListener("webkitbeginfullscreen", exitAnyFullscreen, true);
  document.addEventListener("fullscreenchange", exitAnyFullscreen, true);
  document.addEventListener("webkitfullscreenchange", exitAnyFullscreen, true);
  // Also keep the video from RESUMING while the browser is inactive — a play that
  // starts on the library's forced rotation is what iOS then throws to full screen,
  // so stopping the play stops the hijack at the source.
  document.addEventListener("play", function (e) {
    if (!window.__shardBrowserActive) { try { e.target.pause(); } catch (err) {} }
  }, true);
  // The native side reaches only the main frame; it relays the active flag to each
  // child frame here (pornhub's player lives in an iframe). Each frame runs its own
  // Capture.js, so this sets that frame's flag and pauses its own videos.
  window.addEventListener("message", function (e) {
    var d = e.data;
    if (d && d.__shard === "active") {
      window.__shardBrowserActive = !!d.on;
      if (!d.on) {
        try {
          var vids = document.getElementsByTagName("video");
          for (var i = 0; i < vids.length; i++) { try { vids[i].pause(); } catch (err) {} }
        } catch (err) {}
      }
    }
  }, false);

  // Tell the native side whether any web video is actually playing, so the
  // library's own player can step aside (pause) while a page video plays and
  // resume when it stops — otherwise the two fought over the audio route and the
  // background song went silent.
  var lastPlaying = null;
  function reportPlaying() {
    try {
      var vids = document.getElementsByTagName("video");
      var playing = false;
      for (var i = 0; i < vids.length; i++) {
        var v = vids[i];
        if (!v.paused && !v.ended && v.currentTime > 0 && v.readyState > 2) playing = true;
      }
      if (playing !== lastPlaying) {
        lastPlaying = playing;
        window.webkit.messageHandlers.shard.postMessage({ type: "webplaying", on: playing });
      }
    } catch (e) {}
  }
  document.addEventListener("play", reportPlaying, true);
  document.addEventListener("pause", reportPlaying, true);
  document.addEventListener("ended", reportPlaying, true);
  setInterval(reportPlaying, 800);

  // A download button over each video's top-right, and the quality list right
  // under it — rendered in the page so it sits on the actual video and a tap
  // anywhere else dismisses it, which a native overlay could not do.
  function send(msg) {
    try { window.webkit.messageHandlers.shard.postMessage(msg); } catch (e) {}
  }

  var buttons = [];   // {video, el}
  var anchor = null;  // button the open list belongs to
  var listEl = null;

  function makeButton() {
    var b = document.createElement("div");
    b.style.cssText =
      "position:fixed;width:26px;height:26px;z-index:2147483646;display:none;" +
      "border:1.5px solid rgba(255,255,255,0.7);border-radius:7px;" +
      "align-items:center;justify-content:center;cursor:pointer;" +
      "color:rgba(255,255,255,0.9);font:600 15px system-ui;background:transparent;";
    b.textContent = "↓"; // down arrow
    b.setAttribute("data-shard", "dl");
    b.addEventListener("click", function (e) {
      e.preventDefault(); e.stopPropagation();
      removeList();
      anchor = b;
      var r = b.getBoundingClientRect();
      // Send where the button is, so the native list can drop right under it.
      send({ type: "download", right: r.right, bottom: r.bottom });
    });
    document.documentElement.appendChild(b);
    return b;
  }

  function syncButtons() {
    var vids = Array.prototype.slice.call(document.getElementsByTagName("video"));
    vids.forEach(function (v) {
      if (!buttons.some(function (x) { return x.video === v; })) {
        buttons.push({ video: v, el: makeButton() });
      }
    });
    buttons.forEach(function (x) {
      var r = x.video.getBoundingClientRect();
      var visible = r.width > 60 && r.height > 60 && r.bottom > 0 && r.top < innerHeight;
      x.el.style.display = visible ? "flex" : "none";
      if (visible) {
        x.el.style.left = Math.min(innerWidth - 32, r.right - 32) + "px";
        x.el.style.top = Math.max(6, r.top + 6) + "px";
      }
    });
    if (listEl && anchor) positionList();
  }

  function removeList() {
    if (listEl) { listEl.remove(); listEl = null; }
    anchor = null;
  }
  function positionList() {
    if (!listEl || !anchor) return;
    var r = anchor.getBoundingClientRect();
    var w = 230;
    listEl.style.left = Math.max(8, Math.min(innerWidth - w - 8, r.right - w)) + "px";
    listEl.style.top = (r.bottom + 6) + "px";
    listEl.style.width = w + "px";
  }

  function esc(s) {
    return String(s).replace(/[&<>"]/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c];
    });
  }

  // Native calls this with the quality rows once it has parsed the page.
  window.__shardQualities = function (rows) {
    if (listEl) { listEl.remove(); listEl = null; }
    if (!anchor || !rows || !rows.length) return;
    listEl = document.createElement("div");
    listEl.style.cssText =
      "position:fixed;z-index:2147483647;background:#1a1a1d;border-radius:10px;" +
      "box-shadow:0 6px 20px rgba(0,0,0,0.5);overflow:hidden;font:14px system-ui;";
    rows.forEach(function (row, i) {
      var item = document.createElement("div");
      item.style.cssText =
        "padding:10px 12px;color:#e8e6e3;display:flex;justify-content:space-between;gap:10px;" +
        (i ? "border-top:1px solid #2c2e32;" : "");
      item.innerHTML =
        "<b>" + esc(row.label) + '</b><span style="color:#8a8a90">' + esc(row.detail) + "</span>";
      item.addEventListener("click", function (e) {
        e.stopPropagation();
        send({ type: "pick", itag: row.itag });
        removeList();
      });
      listEl.appendChild(item);
    });
    document.documentElement.appendChild(listEl);
    positionList();
  };

  // A tap anywhere but the list (or a download button) closes it.
  document.addEventListener("click", function (e) {
    if (listEl && !listEl.contains(e.target) && e.target.getAttribute("data-shard") !== "dl") {
      removeList();
    }
  }, true);

  setInterval(syncButtons, 400);
  window.addEventListener("scroll", syncButtons, true);
  window.addEventListener("resize", syncButtons, true);

  // Best-effort YouTube ad skipping. DEFENSIVE BY DESIGN: it only acts when an ad
  // is actually detected (the player's `ad-showing` class, or a skip button), and
  // everything is wrapped in try/catch. If YouTube changes its markup, this simply
  // does nothing — it never touches normal playback — so the worst case is that
  // ads come back, not that YouTube breaks.
  function skipAds() {
    try {
      var skip = document.querySelector(
        ".ytp-ad-skip-button, .ytp-ad-skip-button-modern, .ytp-skip-ad-button, .ytp-ad-skip-button-container button"
      );
      if (skip) { skip.click(); return; }
      var player = document.querySelector(".html5-video-player");
      if (player && player.classList.contains("ad-showing")) {
        // The ad plays in the same <video>; jumping it to the end gets past it,
        // then YouTube loads the real content. Only ever while `ad-showing`.
        var v = player.querySelector("video");
        if (v && isFinite(v.duration) && v.duration > 0) {
          v.currentTime = v.duration;
        }
      }
      // Close static overlay ads if their close button is present.
      var overlayClose = document.querySelector(".ytp-ad-overlay-close-button");
      if (overlayClose) { try { overlayClose.click(); } catch (e) {} }
    } catch (e) {}
  }
  setInterval(skipAds, 350);

  // A red, underlined "삭제" on the right of each PREVIOUS-SEARCH row in YouTube's
  // search suggestions. Deletion is made to stick by remembering the term (see the
  // localStorage list below) and hiding it ourselves — a refresh used to bring the
  // row back because YouTube re-sends logged-out recent searches from the server.
  // If YouTube ALSO exposes a native remove control we click it too, best-effort.
  function nativeRemove(opt) {
    var kids = opt.querySelectorAll('button, [role="button"], [aria-label], [class]');
    for (var i = 0; i < kids.length; i++) {
      var el = kids[i];
      var al = (el.getAttribute && el.getAttribute('aria-label')) || '';
      if (/remove|delete|삭제|제거/i.test(al)) return el;
      var cn = el.className;
      cn = (cn && cn.baseVal !== undefined) ? cn.baseVal : cn; // SVG className is an object
      if (/remove/i.test(String(cn || ''))) return el;
    }
    return null;
  }

  function rowText(row) { return (row.textContent || '').trim(); }

  // Deleted previous-searches, kept in localStorage so a delete STICKS across a
  // reload. YouTube's logged-out recent searches come back from its server (a
  // per-visitor cookie) after a refresh, and there is no reliable client remove —
  // so instead of fighting that, we remember what the user deleted and keep those
  // rows hidden ourselves. A term is un-deleted the moment the user searches it
  // again (below), so history never looks "stuck off" — that was the earlier bug.
  function loadDeleted() { try { return JSON.parse(localStorage.getItem('shardDeleted') || '[]'); } catch (e) { return []; } }
  function saveDeleted(a) { try { localStorage.setItem('shardDeleted', JSON.stringify(a.slice(-300))); } catch (e) {} }
  var deleted = loadDeleted();
  function isDeleted(t) { return deleted.indexOf(t) >= 0; }
  function markDeleted(t) { if (t && !isDeleted(t)) { deleted.push(t); saveDeleted(deleted); } }
  function unDelete(t) { var i = deleted.indexOf(t); if (i >= 0) { deleted.splice(i, 1); saveDeleted(deleted); } }
  // Any actual search (this results page's own query) un-deletes that term, so a
  // thing you deleted and then deliberately searched again is remembered normally.
  try {
    var q = new URLSearchParams(location.search).get('search_query');
    if (q) unDelete(q.trim());
  } catch (e) {}

  // The rows of previous searches inside YouTube's suggestions listbox. Only when
  // the input is EMPTY (that is when the box shows PREVIOUS searches, not
  // autocomplete for what is being typed). We take the listbox's options, or its
  // direct children / search links when it is not marked up with roles.
  function searchBox() {
    var inp = document.querySelector('input[name="search_query"], input.ytSearchboxComponentInput, input[type="search"]');
    if (!inp || inp.value.trim() !== '') return null;
    var box = document.querySelector('.ytSearchboxComponentSuggestionsContainer, #i0[role="listbox"], [role="listbox"]');
    if (!box || box.hidden) return null;
    return box;
  }
  function rowsIn(box) {
    var opts = box.querySelectorAll('[role="option"]');
    if (opts.length) return Array.prototype.slice.call(opts);
    var links = box.querySelectorAll('a[href*="search_query="]');
    if (links.length) return Array.prototype.slice.call(links).map(function (a) { return a.closest('li') || a; });
    // Last resort: the box's own children that carry text.
    return Array.prototype.slice.call(box.children).filter(function (c) { return rowText(c); });
  }

  // "삭제" is placed as a FIXED overlay OVER each row's right edge, NOT inside the
  // row. The previous version appended it inside the row's <a>, so a tap on it hit
  // the link and ran the search instead of deleting. A separate top-layer element
  // takes the tap itself, so the row underneath never activates.
  var delEls = []; // {row, el}
  function delFor(row) {
    for (var i = 0; i < delEls.length; i++) if (delEls[i].row === row) return delEls[i];
    var el = document.createElement('div');
    el.textContent = '삭제';
    el.setAttribute('data-shard', 'del');
    el.style.cssText =
      'position:fixed;z-index:2147483647;color:#ff5252;text-decoration:underline;' +
      'font:600 14px system-ui;padding:8px 10px;cursor:pointer;display:none;';
    // Intercept the tap on every path so the row cannot navigate; act on the tap's
    // end (touchend for the phone, click for a pointer).
    function kill(e) { e.preventDefault(); e.stopPropagation(); }
    ['touchstart', 'pointerdown', 'mousedown'].forEach(function (ev) { el.addEventListener(ev, kill, true); });
    function act(e) { kill(e); doDelete(row); }
    el.addEventListener('touchend', act, true);
    el.addEventListener('click', act, true);
    document.documentElement.appendChild(el);
    var rec = { row: row, el: el };
    delEls.push(rec);
    return rec;
  }
  // The element to actually hide: the row's outermost wrapper still inside the box.
  // Hiding just the matched inner element left an empty row (its <li>/wrapper kept
  // its height) — the "빈 행" that stayed behind.
  function topRow(box, row) {
    var e = row;
    while (e && e.parentElement && e.parentElement !== box) e = e.parentElement;
    return e || row;
  }
  function doDelete(row) {
    markDeleted(rowText(row));                       // remembered, so it stays gone on reload
    var rm = nativeRemove(row);                       // also use YouTube's own remove if it has one
    if (rm) { try { rm.click(); } catch (e) {} }
    var box = searchBox();
    try { (box ? topRow(box, row) : row).style.display = 'none'; } catch (e) {}
    setTimeout(syncDeletes, 60);
  }
  function syncDeletes() {
    try {
      var box = searchBox();
      var live = [];
      if (box) {
        // Pin the button to the LISTBOX's right edge, not the row's: a row is often
        // an inline element only as wide as its text, so "row.right" landed on top
        // of the query. The box spans the full width, so its edge is the real right.
        var boxRight = box.getBoundingClientRect().right;
        rowsIn(box).forEach(function (row) {
          // A previously-deleted term: keep the whole row hidden, no button for it.
          if (isDeleted(rowText(row))) {
            topRow(box, row).style.display = 'none';
            return;
          }
          var rec = delFor(row);
          var r = row.getBoundingClientRect();
          if (r.height > 0 && r.bottom > 0) {
            rec.el.style.display = 'block';
            rec.el.style.left = Math.max(0, boxRight - 58) + 'px';
            rec.el.style.top = (r.top + r.height / 2 - 16) + 'px';
          } else {
            rec.el.style.display = 'none';
          }
          live.push(rec);
        });
      }
      // Hide overlays whose row went away (input filled, box closed, list changed).
      delEls.forEach(function (rec) { if (live.indexOf(rec) < 0) rec.el.style.display = 'none'; });
    } catch (e) {}
  }
  try {
    new MutationObserver(function () { syncDeletes(); }).observe(document.documentElement, { childList: true, subtree: true });
  } catch (e) {}
  window.addEventListener('scroll', syncDeletes, true);
  window.addEventListener('resize', syncDeletes, true);
  setInterval(syncDeletes, 300);
})();
