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
  // search suggestions. Logged-out recent searches live on YouTube's server (keyed
  // by the visitor cookie), not in localStorage — so clearing the row on screen is
  // not enough; it has to invoke YouTube's own remove so it stays gone. We only add
  // our button to rows that actually carry a native remove affordance (that is what
  // marks a history entry vs a fresh autocomplete guess), and clicking ours clicks
  // that native control. DEFENSIVE: if the markup changes and no remove control is
  // found, we add nothing and normal search is untouched.
  // Terms the user deleted this session, hidden on every pass so a row YouTube
  // re-sends stays gone even when there is no server-side remove to call.
  var hiddenSearches = {};

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

  // The rows of previous searches. YouTube's search box did not render a native
  // remove control here (the earlier version keyed on that and so showed nothing),
  // and the rows come through as plain blue links, so we take them broadly: every
  // option / search-results link inside the suggestions area. Guarded to when the
  // input is EMPTY — that is when the box shows PREVIOUS searches rather than
  // autocomplete guesses for what is being typed, so we never offer to "delete"
  // a live suggestion.
  function searchRows() {
    var inp = document.querySelector('input[name="search_query"], input.ytSearchboxComponentInput, input[type="search"]');
    if (!inp || inp.value.trim() !== '') return [];
    var box = document.querySelector('.ytSearchboxComponentSuggestionsContainer, #i0[role="listbox"], [role="listbox"]');
    var scope = box || document;
    var rows = [];
    scope.querySelectorAll('[role="option"]').forEach(function (o) { rows.push(o); });
    if (!rows.length) {
      scope.querySelectorAll('a[href*="search_query="]').forEach(function (a) {
        // Prefer a list-item wrapper so our button sits at the row's edge.
        var row = a.closest('li, [role="option"]') || a;
        if (rows.indexOf(row) < 0) rows.push(row);
      });
    }
    return rows;
  }

  function rowText(row) {
    return (row.textContent || '').replace(/삭제\s*$/, '').trim();
  }

  function decorateHistory() {
    try {
      var rows = searchRows();
      for (var i = 0; i < rows.length; i++) {
        var row = rows[i];
        // Already hidden this session -> keep it hidden.
        if (hiddenSearches[rowText(row)]) { row.style.display = 'none'; continue; }
        if (row.querySelector(':scope > .shard-del')) continue;
        var d = document.createElement('span');
        d.className = 'shard-del';
        d.textContent = '삭제';
        d.style.cssText =
          'position:absolute;right:10px;top:50%;transform:translateY(-50%);z-index:2147483647;' +
          'color:#ff5252;text-decoration:underline;font:600 13px system-ui;cursor:pointer;padding:6px 8px;';
        (function (row) {
          d.addEventListener('click', function (e) {
            e.preventDefault(); e.stopPropagation();
            hiddenSearches[rowText(row)] = 1;          // stays gone even if re-sent
            var rm = nativeRemove(row);                 // real server delete if offered
            if (rm) { try { rm.click(); } catch (err) {} }
            try { row.style.display = 'none'; } catch (e2) {}
          }, true);
        })(row);
        try {
          var cs = getComputedStyle(row);
          if (cs.position === 'static') row.style.position = 'relative';
          row.style.paddingRight = '56px';
        } catch (e) {}
        row.appendChild(d);
      }
    } catch (e) {}
  }
  try {
    new MutationObserver(function () { decorateHistory(); }).observe(document.documentElement, { childList: true, subtree: true });
  } catch (e) {}
  setInterval(decorateHistory, 400);
})();
