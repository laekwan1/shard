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

// ---- where we are ----------------------------------------------------------

const views = ["home", "library", "settings"];
let here = "home";

function show(where) {
  // The player covers the whole window, so it goes with the screen it belongs
  // to; left up, it hid whatever was navigated to and kept playing out of reach.
  if (where !== "library" && typeof shutPlayer === "function") shutPlayer();
  here = where;
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

document.getElementById("chip").addEventListener("click", () => show("home"));
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
  power.classList.add("busy");
  send("engine.toggle");
});

const note = document.getElementById("note");

function paintEngine(state) {
  const on = !!state.running;
  power.setAttribute("aria-pressed", String(on));
  power.classList.remove("busy");
  headline.textContent = state.headline;
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
const emptyEl = document.getElementById("empty");

for (const button of document.querySelectorAll(".shelf")) {
  button.addEventListener("click", () => {
    if (shelf.kind === button.dataset.shelf) return;
    shelf.kind = button.dataset.shelf;
    shelf.folder = null;
    for (const other of document.querySelectorAll(".shelf")) {
      other.classList.toggle("on", other === button);
    }
    send("library.list", { kind: shelf.kind });
  });
}

function paintLibrary(message) {
  shelf.items = message.items || [];
  shelf.folders = message.folders || [];
  if (shelf.folder && !shelf.folders.includes(shelf.folder)) shelf.folder = null;
  paintFolders();
  paintFiles();
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
      if (id) send("library.folder", { id, folder: name || "" });
    });
    foldersEl.appendChild(button);
  };

  tab("전체", null);
  for (const name of shelf.folders) tab(name, name);

  const add = document.createElement("button");
  add.className = "folder add";
  add.textContent = "＋";
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
  return shelf.folder === null
    ? shelf.items
    : shelf.items.filter((i) => i.folder === shelf.folder);
}

function paintFiles() {
  filesEl.textContent = "";
  const list = shown();
  emptyEl.hidden = list.length > 0;
  emptyEl.textContent =
    shelf.kind === "music"
      ? "받은 음악이 없습니다. 음악만 저장하면 여기에 모입니다."
      : "저장한 영상이 없습니다. 브라우저에서 영상을 받으면 여기에 모입니다.";

  for (const item of list) {
    const row = document.createElement("div");
    row.className = "file";
    // Held down, then moved: the browser starts the drag itself once the
    // button has been pressed on the row, so nothing moves on a passing hover.
    row.draggable = true;
    row.addEventListener("dragstart", (e) => {
      e.dataTransfer.setData("text/plain", String(item.id));
      e.dataTransfer.effectAllowed = "move";
      row.classList.add("held");
    });
    row.addEventListener("dragend", () => row.classList.remove("held"));

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

    row.append(title, facts, go);
    row.addEventListener("dblclick", () => play(item));
    row.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      openMenu(e.clientX, e.clientY, item, row);
    });
    filesEl.appendChild(row);
  }
}

// ---- what a right-click offers ---------------------------------------------

const menu = document.getElementById("menu");
let menuFor = null;
let menuRow = null;

function openMenu(x, y, item, row) {
  menuFor = item;
  menuRow = row;
  menu.hidden = false;
  menu.querySelector('[data-do="unfile"]').hidden = !item.folder;
  // Kept inside the window: a menu opened near the edge would otherwise run off.
  const box = menu.getBoundingClientRect();
  menu.style.left = Math.min(x, window.innerWidth - box.width - 6) + "px";
  menu.style.top = Math.min(y, window.innerHeight - box.height - 6) + "px";
}

function closeMenu() {
  menu.hidden = true;
  menuFor = null;
  menuRow = null;
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
    closeMenu();
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

let playing = null;

function play(item) {
  playing = item;
  player.hidden = false;
  const music = shelf.kind === "music";
  art.hidden = !music;
  video.style.visibility = music ? "hidden" : "visible";
  nowLine.textContent = item.title;
  video.src = "/media/" + item.id;
  video.play().catch(() => {});
  // Where it left off, remembered per file so a long video is not started over.
  const at = Number(localStorage.getItem("at:" + item.id) || 0);
  if (at > 5) video.currentTime = at;
}

function shutPlayer() {
  video.pause();
  video.removeAttribute("src");
  video.load();
  player.hidden = true;
  playing = null;
}

document.getElementById("shut").addEventListener("click", shutPlayer);
playButton.addEventListener("click", () => (video.paused ? video.play() : video.pause()));
video.addEventListener("click", () => (video.paused ? video.play() : video.pause()));
video.addEventListener("play", () => (playButton.innerHTML = PAUSE));
video.addEventListener("pause", () => (playButton.innerHTML = PLAY));
video.addEventListener("ended", () => {
  localStorage.removeItem("at:" + (playing && playing.id));
  playButton.innerHTML = PLAY;
});

video.addEventListener("timeupdate", () => {
  const length = video.duration || 0;
  const at = video.currentTime || 0;
  const part = length ? (at / length) * 100 : 0;
  played.style.width = part + "%";
  knob.style.left = part + "%";
  clock.textContent = say(at) + " / " + say(length);
  if (playing && at > 5) localStorage.setItem("at:" + playing.id, String(at));
});

function say(seconds) {
  seconds = Math.max(0, Math.floor(seconds || 0));
  const m = Math.floor(seconds / 60);
  const s = String(seconds % 60).padStart(2, "0");
  const h = Math.floor(m / 60);
  return h ? `${h}:${String(m % 60).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

// Anywhere along the line, and dragging along it scrubs.
function seekTo(e) {
  const box = scrub.getBoundingClientRect();
  const part = Math.min(1, Math.max(0, (e.clientX - box.left) / box.width));
  if (video.duration) video.currentTime = part * video.duration;
}
scrub.addEventListener("mousedown", (e) => {
  seekTo(e);
  const move = (m) => seekTo(m);
  const up = () => {
    document.removeEventListener("mousemove", move);
    document.removeEventListener("mouseup", up);
  };
  document.addEventListener("mousemove", move);
  document.addEventListener("mouseup", up);
});

const RATES = [1, 1.25, 1.5, 2, 0.5, 0.75];
let rateAt = 0;
rateButton.addEventListener("click", () => {
  rateAt = (rateAt + 1) % RATES.length;
  video.playbackRate = RATES[rateAt];
  rateButton.textContent = RATES[rateAt] + "×";
});

muteButton.innerHTML = LOUD;
muteButton.addEventListener("click", () => {
  video.muted = !video.muted;
  muteButton.innerHTML = video.muted ? QUIET : LOUD;
});
volume.addEventListener("input", () => {
  video.volume = Number(volume.value);
  video.muted = false;
  muteButton.innerHTML = video.volume ? LOUD : QUIET;
});

document.getElementById("full").addEventListener("click", () => {
  if (document.fullscreenElement) document.exitFullscreen();
  else document.getElementById("stage").requestFullscreen().catch(() => {});
});

// The keys every player has, while one is open.
document.addEventListener("keydown", (e) => {
  if (player.hidden) return;
  if (e.target instanceof HTMLInputElement) return;
  switch (e.key) {
    case " ":
      e.preventDefault();
      video.paused ? video.play() : video.pause();
      break;
    case "ArrowRight":
      video.currentTime += e.shiftKey ? 30 : 5;
      break;
    case "ArrowLeft":
      video.currentTime -= e.shiftKey ? 30 : 5;
      break;
    case "ArrowUp":
      video.volume = Math.min(1, video.volume + 0.05);
      volume.value = String(video.volume);
      break;
    case "ArrowDown":
      video.volume = Math.max(0, video.volume - 0.05);
      volume.value = String(video.volume);
      break;
    case "Escape":
      if (!document.fullscreenElement) shutPlayer();
      break;
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
  for (const group of message.groups || []) {
    const section = document.createElement("section");
    section.className = "group";

    const title = document.createElement("h2");
    title.textContent = group.title;
    section.appendChild(title);

    for (const item of group.items) {
      const row = document.createElement("div");
      row.className = "setting" + (item.kind === "lines" ? " tall" : "");

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
      case "saved":
        // Something new landed on a shelf. Read it again if the library is
        // being looked at; otherwise it will be read on the way in.
        if (here === "library") send("library.list", { kind: shelf.kind });
        break;
    }
  },
};

// Ask for the state once the page is up: the shell is built before this runs,
// so nothing has been sent yet.
send("ready");
