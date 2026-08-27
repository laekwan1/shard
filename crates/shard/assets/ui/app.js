// The shell's front end.
//
// It draws, and it asks. Everything it cannot do itself — moving the window,
// switching the engine on, reading a folder, laying a browsing tab underneath
// this strip — is a message to Rust, and everything Rust has to say comes back
// through `window.__shard.push`. Two channels, one shape, no other coupling.

const send = (op, args) =>
  window.ipc.postMessage(JSON.stringify({ op, ...(args || {}) }));

// ---- the window's own bar --------------------------------------------------

// A press anywhere on the empty stretch of the bar drags the window; the system
// takes over from there, so snapping and the double-click to maximise are the
// ones the user already knows.
const drag = document.getElementById("drag");
drag.addEventListener("mousedown", (e) => {
  if (e.button !== 0) return;
  // The second press of a double press: hand it to the window rather than
  // starting a drag, which would swallow it.
  if (e.detail === 2) {
    send("window.maximise");
    return;
  }
  send("window.drag");
});
drag.addEventListener("dblclick", () => send("window.maximise"));

for (const button of document.querySelectorAll("#frame-buttons button")) {
  button.addEventListener("click", () => send("window." + button.dataset.window));
}

// ---- the window's own edges -------------------------------------------------
//
// This page fills the window, and a child window takes the pointer wherever it
// reaches, so the frame never hears a press on its own edge. Within a few pixels
// of one the press is reported instead, and Windows carries on with the resize
// exactly as if the edge had been grabbed — which is what lets the page be laid
// out flush to the frame rather than a few pixels inside it.

const GRIP = 6;
const EDGES = ["t", "b", "l", "r", "tl", "tr", "bl", "br"];

// Set by Rust: a window filling the screen has no edge outside it, and offering
// to pull one is offering something that cannot happen.
let zoomed = false;

function edgeAt(e) {
  if (zoomed) return "";
  // With a site in front this page is only the strip at the top, so its own
  // bottom is not the window's — unless something is playing, which gives it
  // the whole window again. Reading it as the window's bottom would have made
  // the row under the tabs resize the window downwards.
  const toTheFloor = !browsing || !player.hidden;
  let at = "";
  if (e.clientY <= GRIP) at += "t";
  else if (toTheFloor && e.clientY >= window.innerHeight - GRIP) at += "b";
  if (e.clientX <= GRIP) at += "l";
  else if (e.clientX >= window.innerWidth - GRIP) at += "r";
  return at;
}

document.addEventListener("mousemove", (e) => {
  const at = edgeAt(e);
  // On the root, where a rule can reach every element under the pointer: the
  // shape has to be the edge's whatever it is passing over.
  for (const edge of EDGES) {
    document.documentElement.classList.toggle("grip-" + edge, edge === at);
  }
});

document.addEventListener(
  "mousedown",
  (e) => {
    if (e.button !== 0) return;
    const at = edgeAt(e);
    if (!at) return;
    e.preventDefault();
    e.stopPropagation();
    send("window.resize", { edge: at });
  },
  true
);

// ---- where we are ----------------------------------------------------------

const views = ["home", "library", "settings"];
let here = "home";

function show(where) {
  here = where;
  // The player belongs to the library, but what it is playing does not stop
  // because somewhere else was opened: away from the library it shrinks to a
  // strip along the bottom instead of covering the screen that was asked for.
  if (typeof syncPlayer === "function") syncPlayer();
  for (const name of views) {
    document.getElementById(name).hidden = name !== where;
  }
  // Read on the way in, so what was saved while this screen was closed — a
  // download that finished a moment ago — is already there.
  if (where === "library") send("library.list", { kind: shelf.kind });
  if (where === "settings") send("settings.read");
  // Rust decides what the window looks like around it: the browsing tabs are
  // its children, not ours, so it is told which view is up.
  send("nav", { to: where });
}

// ---- tabs and the address row ----------------------------------------------
//
// The strip shows what this window holds: our own screens as one tab, and every
// site being browsed as another. The sites themselves are laid out by Rust
// underneath — a page cannot draw another page — so this only says which one
// should be in front.

const tabsEl = document.getElementById("tabs");
const newTab = document.getElementById("newtab");
const addressRow = document.getElementById("address");
const urlBox = document.getElementById("url");
let browsing = false;

function paintTabs(message) {
  const list = message.list || [];
  const at = message.at;
  browsing = at !== null && at !== undefined;
  tabsEl.textContent = "";
  addressRow.hidden = !browsing;

  // Our own screens, as the first tab. It says what it opens and opens it:
  // reading the tab as "wherever you already were" made it do nothing at all
  // from the home screen, which is where it is pressed from most.
  // A site in front means the player cannot have the screen either.
  if (typeof syncPlayer === "function") syncPlayer();

  const ours = document.createElement("button");
  // Lit when the library is what is up — not whenever no site is being browsed,
  // which had it looking pressed from the home screen it does not belong to.
  ours.className = "tab" + (!browsing && here === "library" ? " on" : "");
  ours.innerHTML = '<span class="label">보관함</span>';
  ours.addEventListener("click", () => show("library"));
  tabsEl.appendChild(ours);

  // The way to open another page stays whether or not there are any: closing
  // the last tab used to take the button with it, leaving no way back.
  newTab.hidden = false;

  list.forEach((tab, index) => {
    const button = document.createElement("button");
    button.className = "tab" + (index === at ? " on" : "");
    const label = document.createElement("span");
    label.className = "label";
    label.textContent = tab.title || "새 탭";
    const shut = document.createElement("span");
    shut.className = "x";
    shut.textContent = "✕";
    shut.addEventListener("click", (e) => {
      e.stopPropagation();
      send("tab.shut", { at: index });
    });
    button.append(label, shut);
    button.addEventListener("click", () => send("tab.pick", { at: index }));
    tabsEl.appendChild(button);
    if (index === at && document.activeElement !== urlBox) urlBox.value = tab.url || "";
  });
}

newTab.addEventListener("click", () => send("tab.new", { url: "" }));

for (const button of document.querySelectorAll("#address button")) {
  button.addEventListener("click", () =>
    send("steer", { what: button.dataset.steer, url: "" })
  );
}

urlBox.addEventListener("keydown", (e) => {
  if (e.key !== "Enter") return;
  const typed = urlBox.value.trim();
  if (!typed) return;
  // A word is a search; anything with a dot or a scheme is an address.
  const url = /^[a-z]+:\/\//i.test(typed)
    ? typed
    : /\.\w{2,}(\/|$)/.test(typed)
      ? "https://" + typed
      : "https://www.google.com/search?q=" + encodeURIComponent(typed);
  send("steer", { what: "go", url });
  urlBox.blur();
});

// Whether the program has the token to run the engine. The home screen is shown
// either way — it is where downloads report their progress — but its engine
// switch and settings are hidden without the token, since neither can do
// anything then.
let elevated = false;

// The wordmark always opens home; downloads live there, so it must be reachable
// even in an unelevated run.
document.getElementById("chip").addEventListener("click", () => show("home"));

// Shard's mark, the same shape the chip and the tray draw — used in the engine
// switch's place when the engine is out of reach.
const SHARD_MARK =
  '<svg viewBox="-1 -1 2 2" aria-hidden="true"><mask id="powercut">' +
  '<rect x="-.72" y="-.72" width="1.44" height="1.44" rx=".22" fill="#fff"/>' +
  '<line x1="-1.5" y1="-1.5" x2="1.5" y2="1.5" stroke="#000" stroke-width=".22"/></mask>' +
  '<rect x="-.72" y="-.72" width="1.44" height="1.44" rx=".22" fill="currentColor" mask="url(#powercut)"/></svg>';

// Reflect whether the engine is reachable. Elevated: the real switch and the
// settings button. Unelevated: the switch's place holds the Shard mark with a
// light that breathes on and off, and pressing it relaunches elevated; settings
// is hidden, since nothing it sets can take effect without the engine.
// A file whose row we are waiting for, to play the moment the shelf it is on
// has loaded — the home logo asked for the last thing watched.
let pendingLast = null;

// Bring back the last video or song. The shelf it is on may not be loaded yet,
// so the key is held and the row played once the list arrives (see paintFiles).
function playLast() {
  const raw = localStorage.getItem("last");
  if (!raw) return;
  let last;
  try {
    last = JSON.parse(raw);
  } catch (e) {
    return;
  }
  if (!last || !last.key) return;
  pendingLast = last.key;
  show("library");
  const already = shelf.items.find((i) => i.key === last.key);
  if (shelf.kind === last.kind && already) {
    pendingLast = null;
    play(already);
  } else {
    const button = document.querySelector('.shelf[data-shelf="' + last.kind + '"]');
    if (button && shelf.kind !== last.kind) button.click();
    else send("library.list", { kind: last.kind });
  }
}

function applyElevation() {
  const power = document.getElementById("power");
  document.querySelector('[data-go="settings"]').hidden = !elevated;
  if (elevated) {
    power.classList.remove("locked");
    return;
  }
  power.classList.add("locked");
  power.innerHTML = SHARD_MARK;
  power.title = "마지막 재생";
}
for (const button of document.querySelectorAll("#go button")) {
  button.addEventListener("click", () => {
    const to = button.dataset.go;
    if (to === "browser") send("nav", { to: "browser" });
    else show(to);
  });
}

// ---- the engine ------------------------------------------------------------

const power = document.getElementById("power");
const headline = document.getElementById("headline");
const detail = document.getElementById("detail");

power.addEventListener("click", () => {
  // Without the token the mark stands in for the switch, and pressing it plays
  // the last thing watched rather than toggling an engine that is not there.
  // Elevation is the tray's job now, not this button's.
  if (power.classList.contains("locked")) {
    playLast();
    return;
  }
  power.classList.add("busy");
  send("engine.toggle");
});

const note = document.getElementById("note");

let engineOn = false;

function paintEngine(state) {
  const on = !!state.running;
  engineOn = on;
  if (typeof markProbe === "function") markProbe();
  power.setAttribute("aria-pressed", String(on));
  power.classList.remove("busy");
  // Without the token there is no engine to be off, so the "꺼짐" heading is
  // dropped; the line under it — the invitation to press — stays.
  headline.textContent = elevated ? state.headline : "";
  // The same four readings the drawn window had: trouble in red, running with a
  // caveat in amber, running in green, off in grey.
  headline.className = state.kind || "idle";
  detail.textContent = state.detail || "";
  note.textContent = state.note || "";
}

// ---- downloads -------------------------------------------------------------

const downloads = document.getElementById("downloads");

function paintDownloads(list) {
  downloads.textContent = "";
  for (const item of list) {
    const row = document.createElement("div");
    row.className = "download";

    const name = document.createElement("span");
    name.className = "name";
    name.textContent = item.title;

    const track = document.createElement("span");
    track.className = "track";
    const fill = document.createElement("span");
    fill.className = "fill";
    fill.style.width = Math.round((item.fraction || 0) * 100) + "%";
    track.appendChild(fill);

    const percent = document.createElement("span");
    percent.className = "percent";
    percent.textContent = Math.round((item.fraction || 0) * 100) + "%";

    const stop = document.createElement("button");
    stop.className = "stop";
    stop.title = "받기 취소";
    stop.textContent = "✕";
    stop.addEventListener("click", () => send("download.cancel", { id: item.id }));

    row.append(name, track, percent, stop);
    downloads.appendChild(row);
  }
}

// ---- the library -----------------------------------------------------------

const shelf = { kind: "video", folder: null, items: [], folders: [] };

const foldersEl = document.getElementById("folders");
const filesEl = document.getElementById("files");

// Files marked to be moved together, the batch a drag is carrying, and the last
// row clicked, so Shift can take the run up to it.
const selected = new Set();
let dragBatch = [];
let lastClickedId = null;

// Light the rows that are marked, from the set, without rebuilding the list.
function paintSelection() {
  for (const row of filesEl.querySelectorAll(".file")) {
    row.classList.toggle("selected", selected.has(Number(row.dataset.id)));
  }
}
const emptyEl = document.getElementById("empty");

for (const button of document.querySelectorAll(".shelf")) {
  button.addEventListener("click", () => {
    if (shelf.kind === button.dataset.shelf) return;
    shelf.kind = button.dataset.shelf;
    shelf.folder = null;
    for (const other of document.querySelectorAll(".shelf")) {
      other.classList.toggle("on", other === button);
    }
    // The pill slides on this class; the buttons themselves only recolour.
    document.getElementById("shelves").classList.toggle("music", shelf.kind === "music");
    send("library.list", { kind: shelf.kind });
  });
}

function paintLibrary(message) {
  shelf.items = message.items || [];
  shelf.folders = message.folders || [];
  if (shelf.folder && !shelf.folders.includes(shelf.folder)) shelf.folder = null;
  paintFolders();
  paintFiles();
  paintQueue();
  // The home logo asked for the last thing watched, and the shelf it is on has
  // just arrived: play it now that its row exists.
  if (pendingLast) {
    const found = shelf.items.find((i) => i.key === pendingLast);
    pendingLast = null;
    if (found) play(found);
  }
}

function paintFolders() {
  foldersEl.textContent = "";
  const tab = (label, name) => {
    const button = document.createElement("button");
    button.className = "folder" + (shelf.folder === name ? " on" : "");
    button.textContent = label;
    button.addEventListener("click", () => {
      shelf.folder = shelf.folder === name ? null : name;
      paintFolders();
      paintFiles();
    });
    // A folder is somewhere to put things, so it takes what is dropped on it.
    button.addEventListener("dragover", (e) => {
      e.preventDefault();
      button.classList.add("over");
    });
    button.addEventListener("dragleave", () => button.classList.remove("over"));
    button.addEventListener("drop", (e) => {
      e.preventDefault();
      button.classList.remove("over");
      const id = Number(e.dataTransfer.getData("text/plain"));
      // The whole marked set moves when the dropped file is one of them.
      const ids = dragBatch.length && dragBatch.includes(id) ? dragBatch : [id];
      for (const one of ids) if (one) send("library.folder", { id: one, folder: name || "" });
      selected.clear();
      dragBatch = [];
    });
    // A folder made by hand can be unmade the same way. "전체" is not a folder
    // and has nothing to offer.
    if (name) {
      button.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        openMenu(e.clientX, e.clientY, { folder: name });
      });
    }
    foldersEl.appendChild(button);
  };

  tab("홈", null);   // the top level, not "everything" — see shown()
  for (const name of shelf.folders) tab(name, name);

  const add = document.createElement("button");
  add.className = "folder add";
  add.innerHTML =
    '<svg viewBox="0 0 12 12"><path d="M6 2.4v7.2M2.4 6h7.2" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>';
  add.title = "새 폴더";
  add.addEventListener("click", askFolderName);
  foldersEl.appendChild(add);
}

function askFolderName() {
  const box = document.createElement("input");
  box.className = "rename";
  box.placeholder = "폴더 이름";
  foldersEl.appendChild(box);
  box.focus();
  const done = (make) => {
    const name = box.value.trim();
    box.remove();
    if (make && name) send("library.newFolder", { kind: shelf.kind, name });
  };
  box.addEventListener("keydown", (e) => {
    if (e.key === "Enter") done(true);
    if (e.key === "Escape") done(false);
  });
  box.addEventListener("blur", () => done(true));
}

function shown() {
  // The home view (no folder chosen) shows only the TOP LEVEL — files not inside any
  // folder — the way the phone does it, not every file at once. A chosen folder shows
  // just its own files.
  return shelf.folder === null
    ? shelf.items.filter((i) => !i.folder)
    : shelf.items.filter((i) => i.folder === shelf.folder);
}

function paintFiles() {
  filesEl.textContent = "";
  const list = shown();
  emptyEl.hidden = list.length > 0;
  emptyEl.textContent =
    shelf.kind === "music"
      ? "받은 음악이 없습니다."
      : "받은 영상이 없습니다.";

  for (const item of list) {
    const row = document.createElement("div");
    row.className = "file";
    row.dataset.id = String(item.id);
    // Held down, then moved: the browser starts the drag itself once the
    // button has been pressed on the row, so nothing moves on a passing hover.
    if (selected.has(item.id)) row.classList.add("selected");
    // Selection, the way a file manager does it: a plain click picks this one
    // alone, Ctrl adds or removes one, Shift takes the run between the last
    // click and this. Playing is the go button or a double click, so a single
    // click is free for this.
    row.addEventListener("click", (e) => {
      if (e.target.closest(".go")) return;
      const ids = shown().map((i) => i.id);
      if (e.shiftKey && lastClickedId != null) {
        const a = ids.indexOf(lastClickedId);
        const b = ids.indexOf(item.id);
        if (a >= 0 && b >= 0) {
          if (!e.ctrlKey) selected.clear();
          const [lo, hi] = a < b ? [a, b] : [b, a];
          for (let i = lo; i <= hi; i++) selected.add(ids[i]);
        }
      } else if (e.ctrlKey || e.metaKey) {
        if (selected.has(item.id)) selected.delete(item.id);
        else selected.add(item.id);
        lastClickedId = item.id;
      } else {
        selected.clear();
        selected.add(item.id);
        lastClickedId = item.id;
      }
      paintSelection();
    });
    row.draggable = true;
    row.addEventListener("dragstart", (e) => {
      // Drag the whole marked set when the dragged file is one of them; a lone
      // file drags on its own.
      dragBatch = selected.has(item.id) && selected.size > 1 ? [...selected] : [item.id];
      e.dataTransfer.setData("text/plain", String(item.id));
      e.dataTransfer.effectAllowed = "move";
      for (const other of filesEl.querySelectorAll(".file")) other.classList.remove("held");
      row.classList.add("held");
    });
    row.addEventListener("dragend", () => {
      row.classList.remove("held");
      clearMarks();
    });

    // Dropped on a folder, a file goes into it; dropped between two rows, it
    // takes that place in the list. Which of the two it will be is said while
    // the row is still being carried: the line under the pointer, or the folder
    // lighting up.
    row.addEventListener("dragover", (e) => {
      e.preventDefault();
      const box = row.getBoundingClientRect();
      const above = e.clientY < box.top + box.height / 2;
      clearMarks();
      row.classList.add(above ? "above" : "below");
    });
    row.addEventListener("drop", (e) => {
      e.preventDefault();
      const above = row.classList.contains("above");
      clearMarks();
      const id = Number(e.dataTransfer.getData("text/plain"));
      if (id && id !== item.id) rearrange(id, item.id, above);
    });

    // A frame out of the film at the head of its row. Music has no picture of
    // its own here, so only the video shelf carries one.
    // The picture out of the file when it carries one, a frame out of the film
    // when it does not. Nothing is kept beside the file either way.
    const shot = document.createElement("span");
    shot.className = "shot";
    if (item.cover) shot.style.backgroundImage = "url(/cover/" + item.cover + ")";
    else if (shelf.kind === "video") wantShot(item, shot);
    else shot.classList.add("none");

    const title = document.createElement("span");
    title.className = "title";
    title.textContent = item.title;

    const facts = document.createElement("span");
    facts.className = "facts";
    facts.textContent = [item.size, item.age].filter(Boolean).join("  ·  ");

    const go = document.createElement("button");
    go.className = "go";
    go.title = "재생";
    go.innerHTML = '<svg viewBox="0 0 12 12"><path d="M3 2v8l7-4z" fill="currentColor"/></svg>';
    go.addEventListener("click", () => play(item));

    row.append(shot);
    row.append(title, facts, go);
    row.addEventListener("dblclick", () => play(item));
    row.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      openMenu(e.clientX, e.clientY, { file: item, row });
    });
    filesEl.appendChild(row);
  }
}

function clearMarks() {
  for (const row of filesEl.querySelectorAll(".above, .below")) {
    row.classList.remove("above", "below");
  }
}

// Put one file before or after another, and tell Rust the whole arrangement.
//
// The whole shelf, not the part being shown: a list narrowed to one folder is
// still a slice of one order, and sending only the slice would forget where
// everything else stood.
function rearrange(moved, target, above) {
  const from = shelf.items.findIndex((i) => i.id === moved);
  if (from < 0) return;
  const [carried] = shelf.items.splice(from, 1);
  let to = shelf.items.findIndex((i) => i.id === target);
  if (to < 0) to = shelf.items.length;
  else if (!above) to += 1;
  shelf.items.splice(to, 0, carried);
  paintFiles();
  send("library.order", {
    kind: shelf.kind,
    keys: shelf.items.map((i) => i.key).join("\n"),
  });
}

// ---- a frame out of each film ----------------------------------------------
//
// Taken by playing the file itself and drawing one frame onto a canvas: there is
// no picture stored beside a video, and asking for one would mean shipping a
// decoder when the window already has the platform's. Kept afterwards under the
// file's own name, so it is done once however often the shelf is opened.
//
// One at a time. Each still costs a seek and a decode, and a shelf of forty
// videos all doing that at once is forty files being read at the same moment.

const wanted = [];
let taking = false;

function wantShot(item, box) {
  const kept = localStorage.getItem("shot:" + item.key);
  if (kept) {
    box.style.backgroundImage = "url(" + kept + ")";
    return;
  }
  wanted.push({ item, box });
  takeShots();
}

function takeShots() {
  if (taking || !wanted.length) return;
  taking = true;
  const { item, box } = wanted.shift();
  const reader = document.createElement("video");
  reader.muted = true;
  reader.preload = "auto";

  let settled = false;
  const done = (picture) => {
    if (settled) return;
    settled = true;
    clearTimeout(watchdog);
    reader.removeAttribute("src");
    reader.load();
    if (picture) {
      // The store is small and a still is a couple of kilobytes; when it is
      // full, the picture is simply not kept and the row goes without.
      try {
        localStorage.setItem("shot:" + item.key, picture);
      } catch (e) {}
      if (box.isConnected) box.style.backgroundImage = "url(" + picture + ")";
    }
    taking = false;
    takeShots();
  };

  const draw = () => {
    try {
      const canvas = document.createElement("canvas");
      canvas.width = 88;
      canvas.height = 50;
      canvas.getContext("2d").drawImage(reader, 0, 0, canvas.width, canvas.height);
      done(canvas.toDataURL("image/jpeg", 0.62));
    } catch (e) {
      done(null);
    }
  };

  // A few seconds in, not the first frame: films open on black. Only when the
  // length is known and worth stepping into — asking for a time in a film whose
  // length is not a number never arrives anywhere, and the wait used to end in
  // nothing at all.
  reader.addEventListener("loadeddata", () => {
    const length = reader.duration;
    const at = Number.isFinite(length) && length > 6 ? Math.min(3, length / 3) : 0;
    if (at > 0.2) reader.currentTime = at;
    else draw();
  });
  reader.addEventListener("seeked", draw);
  reader.addEventListener("error", () => done(null));
  // Long enough for a large file to reach the place asked for, and if it has a
  // picture by then that picture is taken rather than nothing: seeking into a
  // film of several hundred megabytes is several reads from a disk.
  const watchdog = setTimeout(() => {
    if (reader.readyState >= 2) draw();
    else done(null);
  }, 20000);
  reader.src = "/media/" + item.id;
}

// ---- what a right-click offers ---------------------------------------------

const menu = document.getElementById("menu");
let menuFor = null;
let menuRow = null;
let menuFolder = null;

// One menu, three things it can be about: the file under the pointer, the folder
// tab under it, or the place in what is playing. What belongs to the others is
// put away rather than a second menu being built beside this one.
function openMenu(x, y, on) {
  menuFor = on.file || null;
  menuRow = on.row || null;
  menuFolder = on.folder || null;
  const about = on.spot ? "spot" : menuFolder ? "folder" : "file";
  for (const el of menu.querySelectorAll("[data-on]")) {
    el.hidden = el.dataset.on !== about;
  }
  if (menuFor) {
    menu.querySelector('[data-do="unfile"]').hidden = !menuFor.folder;
  }
  if (menuFolder) {
    // 전체삭제 only when the folder actually holds files of this shelf; an empty
    // folder has nothing to delete-all, so it shows only 폴더삭제.
    const hasFiles = shelf.items.some((i) => i.folder === menuFolder);
    menu.querySelector('[data-do="deleteFolderAll"]').hidden = !hasFiles;
  }
  menu.hidden = false;
  // Kept inside the window: a menu opened near the edge would otherwise run off.
  const box = menu.getBoundingClientRect();
  menu.style.left = Math.min(x, window.innerWidth - box.width - 6) + "px";
  menu.style.top = Math.min(y, window.innerHeight - box.height - 6) + "px";
}

function closeMenu() {
  menu.hidden = true;
  menuFor = null;
  menuRow = null;
  menuFolder = null;
}

document.addEventListener("click", (e) => {
  if (!menu.hidden && !menu.contains(e.target)) closeMenu();
});
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") closeMenu();
});

for (const button of menu.querySelectorAll("button")) {
  button.addEventListener("click", () => {
    const item = menuFor;
    const row = menuRow;
    const folder = menuFolder;
    closeMenu();
    if (button.dataset.do === "hold" || button.dataset.do === "forget") {
      if (!playing) return;
      if (button.dataset.do === "hold") {
        localStorage.setItem("at:" + playing.key, String(video.currentTime || 0));
        flash("여기서부터 다시 재생합니다");
      } else {
        localStorage.removeItem("at:" + playing.key);
        flash("처음부터 재생합니다");
      }
      return;
    }
    if (button.dataset.do === "dropFolder") {
      if (folder) send("library.dropFolder", { kind: shelf.kind, name: folder });
      return;
    }
    if (button.dataset.do === "deleteFolderAll") {
      // 전체삭제: the folder AND everything of this shelf inside it.
      if (folder) send("library.deleteFolder", { kind: shelf.kind, name: folder });
      return;
    }
    if (!item) return;
    switch (button.dataset.do) {
      case "rename":
        startRename(item, row);
        break;
      case "unfile":
        send("library.folder", { id: item.id, folder: "" });
        break;
      case "delete":
        send("library.delete", { id: item.id });
        break;
    }
  });
}

// Renaming happens where the name is, not in a window over it.
function startRename(item, row) {
  if (!row) return;
  const box = document.createElement("input");
  box.className = "rename";
  box.value = item.title;
  row.textContent = "";
  row.appendChild(box);
  box.focus();
  box.select();
  let settled = false;
  const done = (save) => {
    if (settled) return;
    settled = true;
    const title = box.value.trim();
    if (save && title && title !== item.title) {
      send("library.rename", { id: item.id, title });
    } else {
      paintFiles();
    }
  };
  box.addEventListener("keydown", (e) => {
    if (e.key === "Enter") done(true);
    if (e.key === "Escape") done(false);
  });
  box.addEventListener("blur", () => done(true));
}

// ---- the player ------------------------------------------------------------

const player = document.getElementById("player");
const video = document.getElementById("video");
const art = document.getElementById("art");
const scrub = document.getElementById("scrub");
const played = document.getElementById("played");
const knob = document.getElementById("knob");
const playButton = document.getElementById("play");
const clock = document.getElementById("clock");
const nowLine = document.getElementById("now");
const rateButton = document.getElementById("rate");
const muteButton = document.getElementById("mute");
const volume = document.getElementById("volume");

const PLAY = '<svg viewBox="0 0 16 16" width="15" height="15"><path d="M4 2.5v11l9-5.5z" fill="currentColor"/></svg>';
const PAUSE = '<svg viewBox="0 0 16 16" width="15" height="15"><rect x="4" y="2.6" width="3.2" height="10.8" rx="1" fill="currentColor"/><rect x="9" y="2.6" width="3.2" height="10.8" rx="1" fill="currentColor"/></svg>';
const LOUD = '<svg viewBox="0 0 16 16" width="14" height="14"><path d="M3 6h2.6L9 3v10L5.6 10H3z" fill="currentColor"/><path d="M11 5.6a3.4 3.4 0 0 1 0 4.8" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>';
const QUIET = '<svg viewBox="0 0 16 16" width="14" height="14"><path d="M3 6h2.6L9 3v10L5.6 10H3z" fill="currentColor"/><path d="M11 6l3 4M14 6l-3 4" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>';

const titleLine = document.getElementById("title");
const prevButton = document.getElementById("prev");
const nextButton = document.getElementById("next");

let playing = null;

// Every place the old rule wrote down by itself. None of them were asked for,
// and they are why files opened in the middle; the ones put down by hand from
// here on are kept.
if (localStorage.getItem("at:byhand") !== "1") {
  for (let i = localStorage.length - 1; i >= 0; i--) {
    const key = localStorage.key(i);
    if (key && key.startsWith("at:")) localStorage.removeItem(key);
  }
  localStorage.setItem("at:byhand", "1");
}

function play(item) {
  // Back to a shelf item: leave the folder-opened mode so the panel is the
  // library again.
  externalItems = null;
  document.getElementById("queueShelves").style.display = "";
  // Remembered so the home logo can bring the last thing watched straight back.
  localStorage.setItem("last", JSON.stringify({ kind: shelf.kind, key: item.key }));
  playing = item;
  player.hidden = false;
  // The queue rides with the film — hidden until there is one to play, and its
  // marks redrawn so the row now playing shows it.
  document.getElementById("queue").hidden = false;
  queuePicked = item.id;
  const music = shelf.kind === "music";
  art.hidden = !music;
  // The picture the song was listed under, when the download kept one.
  art.style.backgroundImage = item.cover ? "url(/cover/" + item.cover + ")" : "";
  art.classList.toggle("covered", !!item.cover);
  video.style.visibility = music ? "hidden" : "visible";
  nowLine.textContent = item.title;
  titleLine.textContent = item.title;
  video.src = "/media/" + item.id;
  video.play().catch(() => {});
  // From the beginning, unless a place was put down by hand on this file.
  //
  // It used to remember where everything was left, always, and start there —
  // which meant nothing ever played from the start again and there was no
  // saying why a film opened where it did. Now it is asked for: right-click,
  // save the place, and only that file comes back to it.
  const at = Number(localStorage.getItem("at:" + item.key) || 0);
  if (at > 1) video.currentTime = at;
  syncPlayer();
  paintQueue();
}

// Files opened from outside the library — Shard being the program a video or a
// song opens with. Not shelf items, so they live in their own little list: the
// panel shows the folder they came from, and playing one points the player at
// `/external?i=`.
let externalItems = null;
let externalAt = 0;

function openExternal(list, at) {
  externalItems = list;
  externalAt = at;
  show("library");
  playExternalAt(at);
  // The panel lists the folder, so open it onto that.
  paintQueue();
}

function playExternalAt(index) {
  if (!externalItems || index < 0 || index >= externalItems.length) return;
  externalAt = index;
  playing = null;
  player.hidden = false;
  document.getElementById("queue").hidden = false;
  art.hidden = true;
  video.style.visibility = "visible";
  const name = externalItems[index];
  nowLine.textContent = name;
  titleLine.textContent = name;
  // A cache-buster after the index, so moving between files always reloads.
  video.src = "/external?i=" + index + "&t=" + Date.now();
  video.play().catch(() => {});
  syncPlayer();
  paintTime();
  paintQueue();
}

function shutPlayer() {
  video.pause();
  video.removeAttribute("src");
  video.load();
  player.hidden = true;
  playing = null;
  externalItems = null;
  document.getElementById("queueShelves").style.display = "";
  document.getElementById("queue").hidden = true;
  document.getElementById("queue").classList.remove("open");
  syncPlayer();
}

// Full height on the screen it belongs to, a strip along the bottom anywhere
// else. Rust is told either way: while a site is in front the strip is the one
// part of this page not covered by it, and the site has to be made shorter by
// exactly its height or it is drawn over.
function syncPlayer() {
  const own = !browsing && here === "library";
  player.classList.toggle("mini", !own);
  // The queue belongs to the full player over the library; away from it the
  // strip shows instead, so a queue left open is put away.
  if (!own) document.getElementById("queue").classList.remove("open");
  send("playing", { on: !player.hidden });
}

// The file before or after this one, in the order the shelf is showing them.
// Wrapping round at the ends: a shelf of songs is a loop, not a queue that runs
// out.
function step(by) {
  // A folder-opened file steps through that folder, not a shelf.
  if (externalItems) {
    const n = externalItems.length;
    playExternalAt((externalAt + by + n) % n);
    return;
  }
  if (!playing) return;
  const list = shown();
  const at = list.findIndex((i) => i.id === playing.id);
  if (at < 0) return;
  const found = list[(at + by + list.length) % list.length];
  if (found) play(found);
}

prevButton.addEventListener("click", () => step(-1));
nextButton.addEventListener("click", () => step(1));

// ---- how the shelf plays through -------------------------------------------
//
// Kept between runs: which way somebody listens is not something to set again
// every time the program is opened.

const onwardButton = document.getElementById("onward");
const shuffleButton = document.getElementById("shuffle");

// One of three things, not two switches that can both be on: in order, out of
// order, or neither — and neither means this file and then nothing.
let mode = localStorage.getItem("play:mode");
if (mode !== "order" && mode !== "shuffle") mode = "order";

function markModes() {
  onwardButton.classList.toggle("on", mode === "order");
  shuffleButton.classList.toggle("on", mode === "shuffle");
}

function setMode(want) {
  mode = mode === want ? "" : want;
  localStorage.setItem("play:mode", mode);
  markModes();
}

onwardButton.addEventListener("click", () => setMode("order"));
shuffleButton.addEventListener("click", () => setMode("shuffle"));
markModes();

// One of the others on the shelf, never the one just heard.
function another() {
  const list = shown().filter((i) => !playing || i.id !== playing.id);
  if (!list.length) return null;
  return list[Math.floor(Math.random() * list.length)];
}

document.getElementById("shut").addEventListener("click", shutPlayer);
playButton.addEventListener("click", () => (video.paused ? video.play() : video.pause()));
// A click on the picture no longer pauses. The whole picture is a press-and-
// hold surface now — the right half runs at 2×, the left half rewinds — and a
// click that also toggled play would fire on the way into every hold. Play and
// pause are the button and the spacebar; the picture is for the hold.
video.addEventListener("dblclick", (e) => {
  e.preventDefault();
  toggleFullscreen();
});
// Double-clicking the name on the bottom strip brings the picture up — the
// strip has no stage of its own, so this is the way from a name back to a
// screen. In the full player it goes the rest of the way, to full screen.
titleLine.addEventListener("dblclick", () => {
  if (player.classList.contains("mini")) show("library");
  else toggleFullscreen();
});
video.addEventListener("play", () => (playButton.innerHTML = PAUSE));
video.addEventListener("pause", () => (playButton.innerHTML = PLAY));
video.addEventListener("ended", () => {
  playButton.innerHTML = PLAY;
  if (!playing || !mode) return;
  // Out of order if that is what is set; otherwise down the list, stopping at
  // the end rather than going round — a list that never stops is a list nobody
  // asked to hear twice.
  if (mode === "shuffle") {
    const next = another();
    if (next) play(next);
    return;
  }
  const list = shown();
  const at = list.findIndex((i) => i.id === playing.id);
  if (at >= 0 && at + 1 < list.length) play(list[at + 1]);
});

// How often the line and the clock are redrawn while something plays.
//
// `timeupdate` fires whenever the player feels like it — four times a second on
// some files, dozens on others — and every one of them rewrote the clock and
// moved the knob. The reading only changes once a second, so most of that work
// produced the same pixels. A fixed beat also makes the knob travel evenly
// instead of in whatever steps the decoder happened to report.
const TICK_MS = 200;
let lastTick = 0;

video.addEventListener("timeupdate", () => {
  // Not while the line is being dragged: the picture belongs to the hand then,
  // and letting playback write over it makes the knob jump back under the
  // pointer.
  if (scrubbing) return;
  const now = performance.now();
  if (now - lastTick < TICK_MS) return;
  lastTick = now;
  paintTime();
});

function paintTime() {
  const length = video.duration || 0;
  const at = video.currentTime || 0;
  markScrub(length ? at / length : 0);
  clock.textContent = say(at) + " / " + say(length);
}

function say(seconds) {
  seconds = Math.max(0, Math.floor(seconds || 0));
  const m = Math.floor(seconds / 60);
  const s = String(seconds % 60).padStart(2, "0");
  const h = Math.floor(m / 60);
  return h ? `${h}:${String(m % 60).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

// Anywhere along the line, and dragging along it scrubs.
//
// The line follows the hand at once; the film only moves when the hand is let
// go. Every move of the pointer used to set the play position, and every one of
// those makes the player throw away what it had and ask for the file again from
// a new place — hundreds of times across one drag. That is what made dragging
// feel like it was catching: the picture was seeking to somewhere the pointer
// had already left.
let scrubbing = false;

function markScrub(part) {
  const percent = Math.min(1, Math.max(0, part)) * 100;
  played.style.width = percent + "%";
  knob.style.left = percent + "%";
}

function partAt(e) {
  const box = scrub.getBoundingClientRect();
  return Math.min(1, Math.max(0, (e.clientX - box.left) / box.width));
}

scrub.addEventListener("mousedown", (e) => {
  if (!video.duration) return;
  scrubbing = true;
  let part = partAt(e);
  const show = (at) => {
    part = at;
    markScrub(part);
    clock.textContent = say(part * video.duration) + " / " + say(video.duration);
  };
  show(part);
  const move = (m) => show(partAt(m));
  const up = () => {
    document.removeEventListener("mousemove", move);
    document.removeEventListener("mouseup", up);
    scrubbing = false;
    video.currentTime = part * video.duration;
  };
  document.addEventListener("mousemove", move);
  document.addEventListener("mouseup", up);
});

function seekStep(far) {
  const length = video.duration || 0;
  const share = length / (far ? 5 : 20);
  return Math.min(Math.max(share, far ? 15 : 3), far ? 120 : 30);
}

const RATES = [1, 1.25, 1.5, 2, 0.5, 0.75];
let rateAt = 0;

function setRate(rate) {
  rateAt = Math.max(0, RATES.indexOf(rate));
  video.playbackRate = rate;
  rateButton.textContent = rate + "×";
}

rateButton.addEventListener("click", () => setRate(RATES[(rateAt + 1) % RATES.length]));

// Every speed at once, for when stepping round to the one wanted is the long
// way there.
const ratesMenu = document.getElementById("rates");
rateButton.addEventListener("contextmenu", (e) => {
  e.preventDefault();
  ratesMenu.textContent = "";
  for (const rate of [0.5, 0.75, 1, 1.25, 1.5, 2]) {
    const row = document.createElement("button");
    row.textContent = rate + "×";
    if (rate === RATES[rateAt]) row.className = "on";
    row.addEventListener("click", () => {
      setRate(rate);
      ratesMenu.hidden = true;
    });
    ratesMenu.appendChild(row);
  }
  ratesMenu.hidden = false;
  const box = ratesMenu.getBoundingClientRect();
  const at = rateButton.getBoundingClientRect();
  ratesMenu.style.left = Math.min(at.left, window.innerWidth - box.width - 6) + "px";
  ratesMenu.style.top = Math.max(6, at.top - box.height - 6) + "px";
});
document.addEventListener("click", (e) => {
  if (!ratesMenu.hidden && !ratesMenu.contains(e.target)) ratesMenu.hidden = true;
});

muteButton.innerHTML = LOUD;
muteButton.addEventListener("click", () => {
  video.muted = !video.muted;
  muteButton.innerHTML = video.muted ? QUIET : LOUD;
});
volume.addEventListener("input", () => setVolume(Number(volume.value)));

// A word on the picture, said and gone.
function flash(text) {
  nowLine.textContent = text;
  clearTimeout(flash.timer);
  flash.timer = setTimeout(() => {
    nowLine.textContent = playing ? playing.title : "";
  }, 1600);
}

// Where a place is put down, and taken back.
document.getElementById("stage").addEventListener("contextmenu", (e) => {
  if (!playing) return;
  e.preventDefault();
  openMenu(e.clientX, e.clientY, { spot: true });
});

function toggleFullscreen() {
  if (document.fullscreenElement) document.exitFullscreen();
  else document.getElementById("stage").requestFullscreen().catch(() => {});
}
document.getElementById("full").addEventListener("click", toggleFullscreen);

// ---- the queue: a small library on the player's right edge -----------------
//
// A hover held on the tab, or a click, opens it; leaving the whole thing closes
// it. Inside is the same shelf state the library shows, so switching video/music
// here switches it there too — there is one library, not a copy of it. A single
// click on a row marks it; a double plays it, the same as the list downstairs.
const queue = document.getElementById("queue");
const queuePanel = document.getElementById("queuePanel");
const queueList = document.getElementById("queueList");
const queueTab = document.getElementById("queueTab");
let queueHoverTimer = null;
let queuePicked = null;

function markQueueShelf() {
  for (const b of document.querySelectorAll(".qshelf")) {
    b.classList.toggle("on", b.dataset.shelf === shelf.kind);
  }
}

function paintQueue() {
  markQueueShelf();
  queueList.textContent = "";
  // Opened from a folder rather than a shelf: the panel lists that folder, and a
  // press plays the file beside the one on screen.
  if (externalItems) {
    document.getElementById("queueShelves").style.display = "none";
    externalItems.forEach((name, index) => {
      const row = document.createElement("button");
      row.className = "qrow" + (index === externalAt ? " playing" : "");
      row.textContent = name;
      row.title = name;
      row.addEventListener("click", () => playExternalAt(index));
      queueList.appendChild(row);
    });
    return;
  }
  document.getElementById("queueShelves").style.display = "";
  for (const item of shown()) {
    const row = document.createElement("button");
    row.className =
      "qrow" +
      (playing && item.id === playing.id ? " playing" : "") +
      (queuePicked === item.id ? " picked" : "");
    row.textContent = item.title;
    row.title = item.title;
    row.addEventListener("click", () => {
      queuePicked = item.id;
      paintQueue();
    });
    row.addEventListener("dblclick", () => play(item));
    queueList.appendChild(row);
  }
}

// The mini switch drives the real one, so the list below and the panel here are
// never two different shelves.
for (const b of document.querySelectorAll(".qshelf")) {
  b.addEventListener("click", () => {
    if (shelf.kind === b.dataset.shelf) return;
    document.querySelector('.shelf[data-shelf="' + b.dataset.shelf + '"]').click();
  });
}

function openQueue() {
  queue.classList.add("open");
  paintQueue();
}
function closeQueue() {
  queue.classList.remove("open");
}

queueTab.addEventListener("click", () =>
  queue.classList.contains("open") ? closeQueue() : openQueue(),
);
// Rest on the tab for a moment and it opens on its own — the promised hover.
queueTab.addEventListener("mouseenter", () => {
  queueHoverTimer = setTimeout(openQueue, 1000);
});
queueTab.addEventListener("mouseleave", () => clearTimeout(queueHoverTimer));
// Leaving the panel closes it, so it is not left hanging over the picture.
queue.addEventListener("mouseleave", () => {
  clearTimeout(queueHoverTimer);
  closeQueue();
});
// A press inside the panel is for its rows, not the hold-to-scrub picture under
// it, and a wheel there scrolls the list rather than moving the sound.
queuePanel.addEventListener("mousedown", (e) => e.stopPropagation());
queuePanel.addEventListener("touchstart", (e) => e.stopPropagation(), { passive: true });
queuePanel.addEventListener("wheel", (e) => e.stopPropagation());
// The tab is not inside the panel, so it needs the same guard of its own.
queueTab.addEventListener("mousedown", (e) => e.stopPropagation());
queueTab.addEventListener("touchstart", (e) => e.stopPropagation(), { passive: true });

// ---- press-and-hold on the picture: right runs fast, left rewinds ----------
//
// The gesture every long-form player has settled on. Which half decides the
// direction, so it works the same whether the picture is full screen or a
// pane in the library. A hold is a mouse held down here, a touch held down on
// the phone; a quick press does nothing, since a click no longer pauses.
const stage = document.getElementById("stage");
let heldRate = null;
let rewindTimer = null;
// Whether the film was paused when a left-rewind began, kept out here because a
// timer id is a number in a browser and will not hold a property of its own.
let rewindWasPaused = false;

function startHold(clientX) {
  if (!video.duration) return;
  const box = stage.getBoundingClientRect();
  const third = box.width / 3;
  const x = clientX - box.left;
  if (x > third * 2) {
    // Right: run at double speed until let go.
    heldRate = video.playbackRate;
    video.playbackRate = 2;
    flash("2× 재생");
  } else if (x < third) {
    // Left: rewind continuously — about four seconds a second — until let go.
    if (rewindTimer) return;
    rewindWasPaused = video.paused;
    video.pause();
    rewindTimer = setInterval(() => {
      video.currentTime = Math.max(0, video.currentTime - 0.2);
      paintTime();
      if (video.currentTime <= 0) endHold();
    }, 50);
    flash("◀◀ 되감기");
  }
}

function endHold() {
  if (heldRate !== null) {
    video.playbackRate = heldRate;
    heldRate = null;
  }
  if (rewindTimer) {
    clearInterval(rewindTimer);
    rewindTimer = null;
    // Leave it as it was found: playing on if it was playing, still if it was
    // paused — a rewind should not start playback nobody asked to resume.
    if (!rewindWasPaused) video.play().catch(() => {});
  }
}

stage.addEventListener("mousedown", (e) => {
  if (e.button !== 0) return;
  startHold(e.clientX);
});
stage.addEventListener("mouseup", () => endHold());
stage.addEventListener("mouseleave", () => endHold());
// Touch, for the same picture on a touch screen.
stage.addEventListener("touchstart", (e) => {
  if (e.touches.length === 1) startHold(e.touches[0].clientX);
}, { passive: true });
stage.addEventListener("touchend", () => endHold());
stage.addEventListener("touchcancel", () => endHold());

// ---- the sound follows the wheel over the picture --------------------------
stage.addEventListener("wheel", (e) => {
  e.preventDefault();
  // Up is louder. A notch is 5%, the same step the arrow keys take.
  const next = Math.min(1, Math.max(0, video.volume - Math.sign(e.deltaY) * 0.05));
  setVolume(next);
}, { passive: false });

// One place sets the sound, so the slider, the wheel, the keys and the saved
// value never disagree. The level is remembered across sessions — a player
// that forgets how loud you had it is a player you set every time.
function setVolume(v) {
  video.volume = v;
  video.muted = false;
  volume.value = String(v);
  muteButton.innerHTML = v ? LOUD : QUIET;
  localStorage.setItem("volume", String(v));
}
{
  const saved = Number(localStorage.getItem("volume"));
  if (localStorage.getItem("volume") !== null) setVolume(saved);
}

// The keys every player has, while one is open.
document.addEventListener("keydown", (e) => {
  if (player.hidden) return;
  if (e.target instanceof HTMLInputElement) return;
  switch (e.key) {
    case " ":
      e.preventDefault();
      video.paused ? video.play() : video.pause();
      break;
    // A twentieth of what is playing, rather than a fixed three seconds — that
    // is a third of a short clip and nothing at all in a two-hour film. Shift
    // takes a fifth. Bounded so a very long file does not jump minutes and a
    // very short one still moves.
    case "ArrowRight":
      video.currentTime += seekStep(e.shiftKey);
      paintTime();
      break;
    case "ArrowLeft":
      video.currentTime -= seekStep(e.shiftKey);
      paintTime();
      break;
    case "Home":
      e.preventDefault();
      video.currentTime = 0;
      video.play().catch(() => {});
      break;
    case "PageUp":
      e.preventDefault();
      step(-1);
      break;
    case "PageDown":
      e.preventDefault();
      step(1);
      break;
    case "ArrowUp":
      setVolume(Math.min(1, video.volume + 0.05));
      break;
    case "ArrowDown":
      setVolume(Math.max(0, video.volume - 0.05));
      break;
    // Enter and F both toggle full screen — Enter because it is the largest key
    // and the one a hand rests near, F because every other player uses it.
    case "Enter":
    case "f":
    case "F":
      e.preventDefault();
      toggleFullscreen();
      break;
    case "m":
    case "M":
      video.muted = !video.muted;
      muteButton.innerHTML = video.muted ? QUIET : LOUD;
      break;
    case "Escape":
      if (!document.fullscreenElement) shutPlayer();
      break;
    default:
      // A number is a jump to that tenth of the film: 1 → 10%, 5 → 50%, 0 → 0.
      if (e.key >= "0" && e.key <= "9" && video.duration) {
        video.currentTime = video.duration * (Number(e.key) / 10);
        paintTime();
      }
  }
});

playButton.innerHTML = PLAY;

// ---- settings --------------------------------------------------------------
//
// Nothing here knows what any particular setting means. Rust describes them —
// a name, a label, and what sort of thing each is — and this draws whatever it
// is handed, sending back the name and the new value. Adding a setting is a line
// in Rust, not a control here as well.

const settingsEl = document.getElementById("settings");

function paintSettings(message) {
  settingsEl.textContent = "";
  settingsEl.appendChild(settingsBar());
  for (const group of message.groups || []) {
    const section = document.createElement("section");
    section.className = "group";

    const title = document.createElement("h2");
    title.textContent = group.title;
    section.appendChild(title);

    for (const item of group.items) {
      const row = document.createElement("div");
      // A handful of answers fits beside the name; more than that goes under it.
      const wide = item.kind === "choice" && (item.options || []).length > 3;
      row.className =
        "setting" + (item.kind === "lines" ? " tall" : "") + (wide ? " wide" : "");

      const label = document.createElement("div");
      label.className = "what";
      const name = document.createElement("span");
      name.textContent = item.label;
      const help = document.createElement("small");
      help.textContent = item.help || "";
      label.append(name, help);

      row.append(label, control(item));
      section.appendChild(row);
    }
    settingsEl.appendChild(section);
  }
  settingsEl.appendChild(probeBlock());
}

// ---- finding out what is blocked -------------------------------------------
//
// Not a way of connecting: a way of asking. It tries the strategies in turn
// against one name and keeps the one that gets through, which is what the
// automatic learning does by itself for sites visited often enough — this is for
// the one that is not.

const probeLines = [];
let probeRunning = false;
let probeBox = null;
let probeGo = null;

function probeBlock() {
  const block = document.createElement("section");
  block.className = "group";
  block.id = "probe";

  const title = document.createElement("h2");
  title.textContent = "차단 검사";
  const what = document.createElement("small");
  what.className = "what-for";
  what.textContent =
    "평소 접속만 해도 자동 학습이 같은 일을 합니다. 여기는 직접 확인하고 싶을 때 — 막히는 게 이름인지, 손잡기인지, 주소인지까지 말해 줍니다.";

  const ask = document.createElement("div");
  ask.className = "ask";
  probeBox = document.createElement("input");
  probeBox.spellcheck = false;
  probeBox.placeholder = "example.com";
  probeGo = document.createElement("button");
  probeGo.className = "go";
  probeGo.textContent = "탐색 시작";

  const start = () => {
    const host = probeBox.value.trim();
    if (!host || probeRunning || !engineOn) return;
    probeLines.length = 0;
    probeRunning = true;
    paintProbe();
    send("probe.start", { host });
  };
  probeGo.addEventListener("click", start);
  probeBox.addEventListener("keydown", (e) => {
    if (e.key === "Enter") start();
  });

  ask.append(probeBox, probeGo);
  const lines = document.createElement("div");
  lines.className = "lines";
  block.append(title, what, ask, lines);
  setTimeout(markProbe, 0);
  return block;
}

function markProbe() {
  if (!probeGo) return;
  probeGo.disabled = probeRunning || !engineOn;
  probeGo.textContent = probeRunning ? "탐색 중…" : engineOn ? "탐색 시작" : "엔진을 켜세요";
}

function paintProbe() {
  const box = document.querySelector("#probe .lines");
  if (!box) return;
  box.textContent = "";
  for (const line of probeLines) {
    const row = document.createElement("div");
    row.className = line.ok === true ? "good" : line.ok === false ? "bad" : "";
    const mark = document.createElement("span");
    mark.className = "mark";
    mark.textContent = line.ok === true ? "✓" : line.ok === false ? "✕" : "·";
    const text = document.createElement("span");
    text.textContent = line.text;
    row.append(mark, text);
    box.appendChild(row);
  }
  box.scrollTop = box.scrollHeight;
  markProbe();
}

// What to do with the screen rather than with any one setting: read the log,
// put everything back, or say that this is done with.
//
// Settings are written the moment they are changed, so nothing is waiting on the
// last of these — it is there because a screen with no way out of it leaves the
// question of whether anything was actually saved.
function settingsBar() {
  const bar = document.createElement("div");
  bar.className = "settings-bar";

  const logs = document.createElement("button");
  logs.textContent = "로그 보기";
  logs.addEventListener("click", () => send("logs.open"));

  const done = document.createElement("button");
  done.className = "ok";
  done.textContent = "확인";
  done.addEventListener("click", () => {
    done.classList.add("done");
    done.textContent = "저장됨";
    setTimeout(() => show("home"), 260);
  });

  bar.append(logs, resetButton(), done);
  return bar;
}

// The way back, for a setting changed by mistake. Asked twice rather than
// through a dialog: the second press is the confirmation, and it forgets it was
// asked if nothing follows.
function resetButton() {
  const button = document.createElement("button");
  button.className = "reset";
  button.textContent = "설정 초기화";
  let asked = false;
  let forget = 0;
  button.addEventListener("click", () => {
    if (asked) {
      clearTimeout(forget);
      asked = false;
      button.classList.remove("sure");
      button.textContent = "설정 초기화";
      send("settings.reset");
      return;
    }
    asked = true;
    button.classList.add("sure");
    button.textContent = "한 번 더 누르면 처음 상태로";
    forget = setTimeout(() => {
      asked = false;
      button.classList.remove("sure");
      button.textContent = "설정 초기화";
    }, 4000);
  });
  return button;
}

function control(item) {
  const set = (value) => send("settings.set", { key: item.key, value: String(value) });

  if (item.kind === "toggle") {
    const button = document.createElement("button");
    button.className = "switch" + (item.value ? " on" : "");
    button.setAttribute("aria-pressed", String(!!item.value));
    button.innerHTML = "<span></span>";
    button.addEventListener("click", () => {
      const on = button.classList.toggle("on");
      button.setAttribute("aria-pressed", String(on));
      set(on);
    });
    return button;
  }

  if (item.kind === "choice") {
    const wrap = document.createElement("div");
    wrap.className = "choice";
    for (const option of item.options) {
      const button = document.createElement("button");
      button.textContent = option.label;
      button.className = option.name === item.value ? "on" : "";
      button.addEventListener("click", () => {
        for (const other of wrap.children) other.className = "";
        button.className = "on";
        set(option.name);
      });
      wrap.appendChild(button);
    }
    return wrap;
  }

  if (item.kind === "number") {
    const box = document.createElement("input");
    box.type = "number";
    box.value = item.value;
    box.min = item.min;
    box.max = item.max;
    box.className = "box short";
    box.addEventListener("change", () => set(box.value));
    return box;
  }

  if (item.kind === "lines") {
    const box = document.createElement("textarea");
    box.value = item.value || "";
    box.rows = 4;
    box.spellcheck = false;
    box.className = "box";
    box.addEventListener("change", () => set(box.value));
    return box;
  }

  const box = document.createElement("input");
  box.value = item.value || "";
  box.spellcheck = false;
  box.className = "box";
  box.addEventListener("change", () => set(box.value));
  return box;
}

// ---- what Rust says --------------------------------------------------------

window.__shard = {
  push(message) {
    switch (message.t) {
      case "engine":
        paintEngine(message);
        break;
      case "downloads":
        paintDownloads(message.list || []);
        break;
      case "library":
        paintLibrary(message);
        break;
      case "tabs":
        paintTabs(message);
        break;
      case "settings":
        paintSettings(message);
        break;
      case "probe":
        if (message.clear) probeLines.length = 0;
        for (const line of message.add || []) probeLines.push(line);
        probeRunning = !!message.running;
        paintProbe();
        break;
      case "frame":
        zoomed = !!message.zoomed;
        if (zoomed) {
          for (const edge of EDGES) document.documentElement.classList.remove("grip-" + edge);
        }
        break;
      case "saved":
        // Something new landed on a shelf. Read it again if the library is
        // being looked at; otherwise it will be read on the way in.
        if (here === "library") send("library.list", { kind: shelf.kind });
        break;
      case "external":
        // A file opened from outside the library — a double-click on a video or
        // a song, with the others in its folder. It plays on its own, and the
        // panel lists the folder so the next one is one press away.
        openExternal(message.list || [], message.at || 0);
        break;
      case "caps":
        elevated = !!message.elevated;
        applyElevation();
        break;
    }
  },
};

// Ask for the state once the page is up: the shell is built before this runs,
// so nothing has been sent yet.
send("ready");

// Land on home: downloads report there, so it has to be the first thing seen
// even in an unelevated run where its switch is hidden.
show("home");
