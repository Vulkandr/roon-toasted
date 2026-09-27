// ===== 1. Setup ===========================================================

import { albumHue, albumPalette, paintColors } from "./album-colors.js";

// Tauri's JavaScript API (available because withGlobalTauri is on)
const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (selector) => document.querySelector(selector);

const zoneName = $("#zone-name");
const nothingPlaying = $("#nothing-playing");
const nowPlaying = $("#now-playing");
const art = $("#art");
const artMissing = $("#art-missing");
const title = $("#title");
const artist = $("#artist");
const album = $("#album");
const seek = $("#seek");
const elapsed = $("#elapsed");
const length = $("#length");
const shuffleBtn = $("#shuffle");
const previousBtn = $("#previous");
const playPauseBtn = $("#play-pause");
const nextBtn = $("#next");
const repeatBtn = $("#repeat");
const radioBtn = $("#radio");
const queueText = $("#queue-text");
const queueChip = $("#queue-chip");
const outputBar = $("#output-bar");
const outputName = $("#output-name");
const muteBtn = $("#mute");
const volume = $("#volume");
const volumeValue = $("#volume-value");
const zoneButton = $("#zone-button");
const outputZone = $("#output-zone");
const zoneMenu = $("#zone-menu");
const searchInput = $("#search-input");
const queueSummary = $("#queue-summary");
const queueRepeatBtn = $("#queue-repeat");
const queueShuffleBtn = $("#queue-shuffle");
const queueEmpty = $("#queue-empty");
const queueNowLabel = $("#queue-now-label");
const queueList = $("#queue-list");
const queueMore = $("#queue-more");
const queueRadio = $("#queue-radio");

// What the page currently knows
let zone = null; // the selected zone's data, from Rust
let artKey = null; // image key currently shown, so art only reloads on change
let draggingSeek = false;
let draggingVolume = false;
let zoneList = []; // every zone, for the zone picker
let lastSearch = ""; // what was last searched (the Search tab uses this later)
let queue = { zoneId: null, items: [] }; // the followed zone's queue (first 100), from Rust

// ===== 2. Helpers =========================================================

// 188 -> "3:08"
function formatTime(seconds) {
  if (seconds == null || isNaN(seconds)) return "0:00";
  const s = Math.max(0, Math.floor(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

// Long durations, like Roon's queue total: 3:08, 1:04:07, 15:04:59:16 (days first)
function formatLongTime(seconds) {
  const s = Math.max(0, Math.floor(seconds ?? 0));
  const days = Math.floor(s / 86400);
  const hours = Math.floor((s % 86400) / 3600);
  const minutes = Math.floor((s % 3600) / 60);
  const secs = s % 60;
  const two = (n) => String(n).padStart(2, "0");
  if (days > 0) return `${days}:${two(hours)}:${two(minutes)}:${two(secs)}`;
  if (hours > 0) return `${hours}:${two(minutes)}:${two(secs)}`;
  return `${minutes}:${two(secs)}`;
}

// Updates a slider's purple fill (the --fill variable in toaster.css)
function setFill(slider) {
  const min = Number(slider.min);
  const max = Number(slider.max);
  const pct = max > min ? ((Number(slider.value) - min) / (max - min)) * 100 : 0;
  slider.style.setProperty("--fill", `${pct}%`);
}

// Calls a Rust command; failures are logged instead of breaking the page
function send(command, args) {
  return invoke(command, args).catch((err) => console.error(command, err));
}

// Makes an element with a class and optional text: el("div", "row-title", "Hello")
function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text != null) node.textContent = text;
  return node;
}

// A picture from Roon at a given size; lazy, so it loads when scrolled into view
// If it can't be loaded (Roon has no picture: the Core answers with an error),
// it's swapped for `fallback()`, or the logo stand-in when none is given.
function roonImage(imageKey, size, className, fallback) {
  const img = el("img", className);
  img.src = convertFileSrc(imageKey, "roonimg") + `?width=${size}&height=${size}`;
  img.loading = "lazy";
  img.alt = "";
  const replacement = fallback ?? (() => artPlaceholder(className));
  img.addEventListener("error", () => img.replaceWith(replacement()), { once: true });
  return img;
}

// Stand-in for missing art: the app's logo, tinted to suit light or dark mode
function artPlaceholder(className) {
  return el("div", className ? `${className} art-placeholder` : "art-placeholder");
}

// Stand-in for an artist without a picture: initials in a circle, like Roon
// (the logo stand-in when the name has no letters or digits, e.g. "†††")
function initialsArt(name, className) {
  const initials = initialsOf(name);
  if (!initials) return artPlaceholder(className);
  return el("div", className ? `${className} art-initials` : "art-initials", initials);
}

// Roon-style initials: first letters of the first and last word ("Abhay
// Chauhan" -> AC), skipping A/An/The ("A Very Old Ghost Behind the Farm" -> VF);
// digits and letters count as separate words ("3FORCE" -> 3F); a single word
// gives its first and last letter ("Abort" -> AT)
function initialsOf(name) {
  const words = name
    .replace(/[^\p{L}\p{N}\s]/gu, " ")
    .split(/\s+|(?<=\p{N})(?=\p{L})|(?<=\p{L})(?=\p{N})/u)
    .filter(Boolean);
  const kept = words.filter((word) => !["a", "an", "the"].includes(word.toLowerCase()));
  const use = kept.length > 0 ? kept : words;
  if (use.length === 0) return "";
  if (use.length === 1) {
    const letters = Array.from(use[0]);
    return (letters[0] + (letters.length > 1 ? letters[letters.length - 1] : "")).toUpperCase();
  }
  return (Array.from(use[0])[0] + Array.from(use[use.length - 1])[0]).toUpperCase();
}

// ===== 3. Tabs ============================================================

function showTab(name) {
  document.querySelectorAll(".tab").forEach((tab) => {
    tab.classList.toggle("active", tab.dataset.tab === name);
  });
  document.querySelectorAll(".page").forEach((page) => {
    page.classList.toggle("active", page.id === `tab-${name}`);
  });
}

document.querySelectorAll(".tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    showTab(tab.dataset.tab);
    // Search tab with nothing searched yet: ready to type right away
    if (tab.dataset.tab === "search" && !lastSearch) searchInput.focus();
    if (tab.dataset.tab === "browse") openBrowse();
  });
});

// ===== 4. Toaster modes, Escape, settings cog =============================

// Rust tells us how the Toaster was opened: "player" or "search".
// Player mode (Player hotkey, tray, launching the app again): the Playing tab.
// Search mode: Search tab, bar focused, old text selected so typing replaces it.
// Either way, search results and other tabs' pages are kept as they were.
listen("toaster-open", (event) => {
  if (event.payload === "search") {
    showTab("search");
    searchInput.focus();
    searchInput.select();
  } else {
    showTab("playing");
  }
});

// Enter in the search bar runs the search (results show in the Search tab)
searchInput.addEventListener("keydown", (event) => {
  if (event.key !== "Enter" || event.ctrlKey) return; // Ctrl + Enter resets the zoom
  const text = searchInput.value.trim();
  if (text) runSearch(text);
});

// Escape, in order: close whichever menu is open, clear the search bar if
// you're typing in it, otherwise hide the Toaster
document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape") return;

  if (!actionMenu.hidden) {
    closeActionMenu();
    return;
  }

  if (!zoneMenu.hidden) {
    closeZoneMenu();
    return;
  }

  if (document.activeElement === searchInput && searchInput.value) {
    event.preventDefault();
    searchInput.value = "";
    return;
  }

  send("hide_window");
});

$("#open-settings").addEventListener("click", () => {
  send("open_window", { window: "settings" });
});

// Window bar: × hides to the tray (like Escape), □ maximizes or restores
const appWindow = window.__TAURI__.window.getCurrentWindow();
const maximizeBtn = $("#win-maximize");

$("#win-close").addEventListener("click", () => send("hide_window"));
maximizeBtn.addEventListener("click", () => appWindow.toggleMaximize());

// Shows the restore icon while maximized (checked whenever the size changes)
async function updateMaximizeButton() {
  const maximized = await appWindow.isMaximized();
  maximizeBtn.classList.toggle("maximized", maximized);
  maximizeBtn.title = maximized ? "Restore" : "Maximize";
}
window.addEventListener("resize", updateMaximizeButton);
updateMaximizeButton();

// ===== 5. Rendering =======================================================

function render(payload) {
      zoneList = payload.zones;
  zone = payload.zones.find((z) => z.zoneId === payload.selectedZoneId) ?? null;

  if (!zone) {
    zoneName.textContent = "No zone";
    nothingPlaying.hidden = false;
    nowPlaying.hidden = true;
    outputBar.hidden = true;
    return;
  }

  zoneName.textContent = zone.name;
  const np = zone.nowPlaying;
  nothingPlaying.hidden = np != null;
  nowPlaying.hidden = np == null;

  if (np) {
    // Album art, only reloaded when the image actually changes
    if (np.imageKey !== artKey) {
      artKey = np.imageKey;
      if (artKey) {
        art.src = convertFileSrc(artKey, "roonimg") + "?width=1200&height=1200";
      } else {
        art.removeAttribute("src");
        showArt(false);
      }
      applyAlbumColors();
    }

    title.textContent = np.title;
    artist.textContent = np.artist;
    album.textContent = np.album;

    seek.max = np.length ?? 0;
    seek.disabled = !zone.canSeek || !np.length;
    length.textContent = formatTime(np.length);
    updatePosition(zone.seekPosition);
  }

  // Which buttons Roon currently allows
  previousBtn.disabled = !zone.canPrevious;
  nextBtn.disabled = !zone.canNext;
  playPauseBtn.disabled = !zone.canPlay && !zone.canPause;
  playPauseBtn.classList.toggle("playing", zone.state === "playing");

  shuffleBtn.classList.toggle("on", zone.shuffle);
  repeatBtn.classList.toggle("on", zone.loopMode !== "disabled");
  repeatBtn.classList.toggle("one", zone.loopMode === "loop_one");
  radioBtn.classList.toggle("on", zone.autoRadio);
  // Queue chip lit like Roon Radio, except on Repeat One (the queue won't move on)
  queueChip.classList.toggle("on", zone.loopMode !== "loop_one");
  queueShuffleBtn.classList.toggle("on", zone.shuffle);
  queueRepeatBtn.classList.toggle("on", zone.loopMode !== "disabled");
  queueRepeatBtn.classList.toggle("one", zone.loopMode === "loop_one");
  queueRadio.checked = zone.autoRadio;

  const queued = zone.queueItemsRemaining ?? 0;
  queueText.textContent = `${queued} in queue`;
    const trackWord = queued === 1 ? "track" : "tracks";
  queueSummary.textContent = queued
    ? `${queued} ${trackWord} remaining (${formatLongTime(zone.queueTimeRemaining)})`
    : "";
  updateQueueMore();

  renderOutput(zone.outputs[0]);
}

// The album art, or the logo stand-in when there's none (or it fails to load)
function showArt(hasArt) {
  art.hidden = !hasArt;
  artMissing.hidden = hasArt;
}
art.addEventListener("load", () => showArt(true));
art.addEventListener("error", () => showArt(false));

function updatePosition(seconds) {
  if (draggingSeek) return; // don't fight the user's drag
  seek.value = seconds ?? 0;
  elapsed.textContent = formatTime(seconds);
  setFill(seek);
}

function renderOutput(output) {
  outputBar.hidden = !output;
  if (!output) return;

  outputName.textContent = output.name;
  const vol = output.volume;
  const fixed = vol == null;
  volume.hidden = fixed;
  muteBtn.hidden = fixed;

  if (fixed) {
    volumeValue.textContent = "Fixed volume";
    return;
  }

  muteBtn.classList.toggle("muted", vol.isMuted);
  if (!draggingVolume) {
    volume.min = vol.min;
    volume.max = vol.max;
    volume.step = vol.step || 1;
    volume.value = vol.value;
    setFill(volume);
    showVolume(vol.value, vol.kind);
  }
}

function showVolume(value, kind) {
  volumeValue.textContent = kind === "db" ? `${value} dB` : `${value}`;
}

// ===== 6. Controls ========================================================

playPauseBtn.addEventListener("click", () => {
  if (zone) send("roon_control", { zoneId: zone.zoneId, action: "playpause" });
});
previousBtn.addEventListener("click", () => {
  if (zone) send("roon_control", { zoneId: zone.zoneId, action: "previous" });
});
nextBtn.addEventListener("click", () => {
  if (zone) send("roon_control", { zoneId: zone.zoneId, action: "next" });
});

shuffleBtn.addEventListener("click", () => {
  if (zone) send("roon_zone_settings", { zoneId: zone.zoneId, shuffle: !zone.shuffle });
});

// Repeat cycles like Roon: off -> repeat all -> repeat one -> off
const NEXT_LOOP = { disabled: "loop", loop: "loop_one", loop_one: "disabled" };
repeatBtn.addEventListener("click", () => {
  if (zone) send("roon_zone_settings", { zoneId: zone.zoneId, loopMode: NEXT_LOOP[zone.loopMode] });
});

radioBtn.addEventListener("click", () => {
  if (zone) send("roon_zone_settings", { zoneId: zone.zoneId, autoRadio: !zone.autoRadio });
});

// The "N in queue" chip opens the Queue tab
queueChip.addEventListener("click", () => showTab("queue"));

muteBtn.addEventListener("click", () => {
  const output = zone?.outputs[0];
  if (output?.volume) {
    send("roon_mute", { outputId: output.outputId, muted: !output.volume.isMuted });
  }
});

// ===== 7. Seeking and volume ==============================================

// Progress bar: preview while dragging, seek when released
seek.addEventListener("input", () => {
  draggingSeek = true;
  elapsed.textContent = formatTime(Number(seek.value));
  setFill(seek);
});
seek.addEventListener("change", () => {
  draggingSeek = false;
  if (zone) send("roon_seek", { zoneId: zone.zoneId, seconds: Number(seek.value) });
});

// Volume: live updates while dragging (at most ~7 per second), final one on release
let lastVolumeSend = 0;
function sendVolume() {
  const output = zone?.outputs[0];
  if (!output?.volume) return;
  lastVolumeSend = Date.now();
  send("roon_set_volume", { outputId: output.outputId, value: Number(volume.value) });
}

volume.addEventListener("input", () => {
  draggingVolume = true;
  setFill(volume);
  showVolume(Number(volume.value), zone?.outputs[0]?.volume?.kind);
  if (Date.now() - lastVolumeSend > 150) sendVolume();
});
volume.addEventListener("change", () => {
  sendVolume();
  draggingVolume = false;
});

// ===== 8. Zone picker =====================================================

// Checkmark next to the zone you're on (Lucide "check")
const CHECK_ICON =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"/></svg>';

let zoneMenuButton = null; // which button opened the menu

// One row per zone; clicking a row switches to that zone
function fillZoneMenu() {
  zoneMenu.replaceChildren();
  for (const z of zoneList) {
    const option = document.createElement("button");
    option.className = "zone-option";
    const name = document.createElement("span");
    name.textContent = z.name;
    option.append(name);

    if (z.zoneId === zone?.zoneId) {
      option.classList.add("selected");
      option.insertAdjacentHTML("beforeend", CHECK_ICON);
    }

    option.addEventListener("click", () => {
      closeZoneMenu();
      if (z.zoneId !== zone?.zoneId) send("roon_select_zone", { zoneId: z.zoneId });
    });
    zoneMenu.append(option);
  }
}

// Opens under the header button, or above the footer button
function openZoneMenu(button) {
  if (zoneList.length === 0) return;
  fillZoneMenu();
  zoneMenu.hidden = false;
  zoneMenuButton = button;

  const rect = button.getBoundingClientRect();
  const left = Math.min(rect.left, window.innerWidth - zoneMenu.offsetWidth - 12);
  zoneMenu.style.left = `${Math.max(12, left)}px`;

  if (button === outputZone) {
    zoneMenu.style.top = "";
    zoneMenu.style.bottom = `${window.innerHeight - rect.top + 6}px`;
  } else {
    zoneMenu.style.bottom = "";
    zoneMenu.style.top = `${rect.bottom + 6}px`;
  }

  // Long list: scroll so the current zone is visible
  zoneMenu.querySelector(".selected")?.scrollIntoView({ block: "nearest" });
}

function closeZoneMenu() {
  zoneMenu.hidden = true;
  zoneMenuButton = null;
}

// Same button again closes it; the other zone button moves it there
function toggleZoneMenu(button) {
  const wasOpenHere = !zoneMenu.hidden && zoneMenuButton === button;
  closeZoneMenu();
  if (!wasOpenHere) openZoneMenu(button);
}

zoneButton.addEventListener("click", () => toggleZoneMenu(zoneButton));
outputZone.addEventListener("click", () => toggleZoneMenu(outputZone));

// Clicking anywhere else, resizing, or leaving the window closes it
document.addEventListener("pointerdown", (event) => {
  if (zoneMenu.hidden) return;
  if (zoneMenu.contains(event.target)) return;
  if (zoneButton.contains(event.target) || outputZone.contains(event.target)) return;
  closeZoneMenu();
});
window.addEventListener("resize", closeZoneMenu);
window.addEventListener("blur", closeZoneMenu);

// ===== 9. Queue ===========================================================

// Play icon over the art when hovering an upcoming track (Lucide "play")
const PLAY_ICON =
  '<svg viewBox="0 0 24 24" fill="currentColor" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="6 3 20 12 6 21 6 3"/></svg>';

// One row: art, title and artist, length. Upcoming rows play from there on click.
function queueRow(item, isNow) {
  const row = document.createElement("li");
  row.className = isNow ? "queue-item now" : "queue-item";

  const artBox = document.createElement("div");
  artBox.className = "queue-art";
  artBox.append(item.imageKey ? roonImage(item.imageKey, 96) : artPlaceholder());
  if (!isNow) {
    const play = document.createElement("span");
    play.className = "queue-play";
    play.innerHTML = PLAY_ICON;
    artBox.append(play);
  }

  const text = document.createElement("div");
  text.className = "queue-text";
  const titleEl = document.createElement("div");
  titleEl.className = "queue-item-title";
  titleEl.textContent = item.title;
  const artistEl = document.createElement("div");
  artistEl.className = "queue-item-artist";
  artistEl.textContent = item.artist;
  text.append(titleEl, artistEl);

  const lengthEl = document.createElement("span");
  lengthEl.className = "queue-length";
  lengthEl.textContent = item.length ? formatTime(item.length) : "";

  row.append(artBox, text, lengthEl);

  if (!isNow) {
    row.addEventListener("click", () => {
      send("roon_play_from_here", { zoneId: queue.zoneId, queueItemId: item.queueItemId });
    });
  }
  return row;
}

// Rebuilds the list; the first item is the playing track
function renderQueue(payload) {
  queue = payload;
  const items = payload.items;
  queueEmpty.hidden = items.length > 0;
  queueNowLabel.hidden = items.length === 0;
  queueList.replaceChildren(...items.map((item, i) => queueRow(item, i === 0)));
  updateQueueMore();
}

// "+ N more tracks not shown" when the queue is longer than the 100 shown
function updateQueueMore() {
  const total = zone?.queueItemsRemaining ?? 0;
  const notShown = total - queue.items.length;
  queueMore.hidden = notShown <= 0;
  queueMore.textContent = `+ ${notShown.toLocaleString()} more tracks not shown`;
}

// Repeat and shuffle work exactly like the Playing tab's buttons
queueRepeatBtn.addEventListener("click", () => repeatBtn.click());
queueShuffleBtn.addEventListener("click", () => shuffleBtn.click());

// Roon Radio switch: send the new on/off state
queueRadio.addEventListener("change", () => {
  if (zone) send("roon_zone_settings", { zoneId: zone.zoneId, autoRadio: queueRadio.checked });
});

// ===== 10. Library pages (Search and Browse) ==============================
//
// A tab that shows Roon's library has a "navigator": the pages visited (for
// back/forward), which one is showing, and where to draw it. A page is either
// search results or a Roon page reached by a path (the items clicked from the
// top, each as { index, title }).

const main = $("main");

// Small arrow on rows that open another page (Lucide "chevron-right")
const CHEVRON_ICON =
  '<svg class="row-chevron" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m9 18 6-6-6-6"/></svg>';

// Search groups drawn as round pictures or square covers; the rest are rows
const ROUND_GROUPS = ["Artists", "Composers"];
const COVER_GROUPS = ["Albums"];

// Waits for a Roon request, showing the floating spinner if it takes more
// than a moment (quick ones don't flicker). Several can overlap.
const loadingEl = $("#loading");
let loadingCount = 0;

async function withSpinner(promise) {
  loadingCount++;
  const timer = setTimeout(() => {
    if (loadingCount > 0) loadingEl.hidden = false;
  }, 150);
  try {
    return await promise;
  } finally {
    clearTimeout(timer);
    loadingCount--;
    if (loadingCount === 0) loadingEl.hidden = true;
  }
}

// --- Moving between pages ---

// Goes to a new page (and drops any "forward" pages, like a browser)
function navGo(nav, entry) {
  saveScroll(nav);
  nav.history.splice(nav.position + 1);
  nav.history.push(entry);
  nav.position = nav.history.length - 1;
  navShow(nav);
}

function navBack(nav) {
  if (nav.position <= 0) return;
  saveScroll(nav);
  nav.position--;
  navShow(nav);
}

function navForward(nav) {
  if (nav.position >= nav.history.length - 1) return;
  saveScroll(nav);
  nav.position++;
  navShow(nav);
}

// Remembers how far down the page was scrolled, for coming back to it
function saveScroll(nav) {
  const entry = nav.history[nav.position];
  if (entry) entry.scroll = main.scrollTop;
}

// Shows a message ("Searching…", errors) instead of a page
function showStatus(nav, text) {
  nav.view.replaceChildren();
  nav.status.textContent = text;
  nav.status.hidden = false;
}

// Draws the current page, fetching it from Rust first
async function navShow(nav) {
  const entry = nav.history[nav.position];
  const ticket = ++nav.ticket; // a newer navigation makes this one's answer stale
  nav.page = null;
  nav.backBtn.disabled = nav.position <= 0;
  nav.forwardBtn.disabled = nav.position >= nav.history.length - 1;
  nav.titleEl.textContent = entry.title;

  try {
    if (entry.kind === "results") {
      // Results are kept, so going back to them doesn't search again
      if (!entry.data) {
        showStatus(nav, "Searching…");
        entry.data = await withSpinner(invoke("roon_search", { query: entry.query }));
      }
      if (ticket !== nav.ticket) return;
      renderResults(nav, entry.data);
    } else if (entry.kind === "home") {
      // The Browse home list is kept too
      if (!entry.data) entry.data = await withSpinner(loadHome(nav));
      if (ticket !== nav.ticket) return;
      renderHome(nav, entry.data);
    } else {
      const page = await withSpinner(
        invoke("roon_browse_path", {
          session: nav.session,
          hierarchy: nav.hierarchy,
          input: entry.input,
          path: entry.path,
          zoneId: zone?.zoneId ?? null,
        }),
      );
      if (ticket !== nav.ticket) return;
      renderPage(nav, entry, page);
    }
    main.scrollTop = entry.scroll ?? 0;
  } catch (err) {
    if (ticket === nav.ticket) showStatus(nav, String(err));
  }
}

// What clicking an item does, going by Roon's hint: "list" opens its page,
// "action_list" shows its actions (Play Now, ...), "action" runs it.
// input and basePath say where the item was listed.
function openItem(nav, input, basePath, item, index, event) {
  const path = [...basePath, { index, title: item.title }];
  if (item.hint === "list") {
    navGo(nav, { kind: "page", input, path, title: item.title });
  } else if (item.hint === "action_list") {
    showActions(nav, input, path, event);
  } else if (item.hint === "action") {
    runAction(nav, input, path);
  }
}

// --- Building items ---

// One item as a tile or a row, opening it when clicked.
// look.shape: "round" / "square" (tiles) or "row"; look.art: pictures on rows
function itemNode(nav, input, basePath, item, index, look) {
  if (item.hint === "header") return el("li", "row header", item.title);
  const node = look.shape === "row" ? rowNode(item, look.art) : tileNode(item, look.shape === "round");
  node.addEventListener("click", (event) => openItem(nav, input, basePath, item, index, event));
  return node;
}

function tileNode(item, round) {
  const tile = el("button", round ? "tile round" : "tile");
  const artBox = el("div", "tile-art");
  // Artists without a picture get initials; anything else gets the logo stand-in
  const fallback = round ? () => initialsArt(item.title) : () => artPlaceholder();
  artBox.append(item.imageKey ? roonImage(item.imageKey, 400, undefined, fallback) : fallback());
  tile.append(artBox, el("div", "tile-title", item.title), el("div", "tile-sub", item.subtitle ?? ""));
  return tile;
}

function rowNode(item, showArt) {
  const row = el("li", "row");
  if (showArt) {
    row.append(item.imageKey ? roonImage(item.imageKey, 88, "row-art") : artPlaceholder("row-art"));
  }
  const text = el("div", "row-text");
  text.append(el("div", "row-title", item.title));
  if (item.subtitle) text.append(el("div", "row-sub", item.subtitle));
  row.append(text);
  if (item.hint === "list") row.insertAdjacentHTML("beforeend", CHEVRON_ICON);
  return row;
}

// A set of items: tiles in a grid, or rows in a list. entries: [item, index] pairs
function itemsBlock(nav, input, basePath, entries, look) {
  const block = look.shape === "row" ? el("ol", "row-list") : el("div", "tile-grid");
  for (const [item, index] of entries) {
    block.append(itemNode(nav, input, basePath, item, index, look));
  }
  return block;
}

// --- Search results page ---

function renderResults(nav, data) {
  if (data.topHits.length === 0 && data.sections.length === 0) {
    showStatus(nav, `No results for "${data.query}".`);
    return;
  }
  nav.status.hidden = true;
  const parts = data.topHits.map((hit) => topHitCard(nav, data.query, hit));
  for (const section of data.sections) parts.push(resultGroup(nav, data.query, section));
  nav.view.replaceChildren(...parts);
}

// Roon's top hit as a bigger card (round picture for an artist, e.g. "12 Albums")
function topHitCard(nav, query, hit) {
  const card = el("button", "top-hit");
  const isArtist = /\d+ Albums?$/.test(hit.subtitle ?? "");
  const artClass = isArtist ? "top-hit-art round" : "top-hit-art";
  const fallback = isArtist ? () => initialsArt(hit.title, artClass) : () => artPlaceholder(artClass);
  card.append(hit.imageKey ? roonImage(hit.imageKey, 200, artClass, fallback) : fallback());
  const text = el("div", "top-hit-text");
  text.append(el("div", "top-hit-label", "Top result"), el("div", "top-hit-title", hit.title));
  if (hit.subtitle) text.append(el("div", "top-hit-sub", hit.subtitle));
  card.append(text);
  card.addEventListener("click", (event) => openItem(nav, query, [], hit, hit.index, event));
  return card;
}

// One group (Artists, Albums, ...): heading, "See all", and its first results
function resultGroup(nav, query, section) {
  const group = el("section", "result-group");
  const groupStep = { index: section.index, title: section.title };

  const head = el("div", "group-head");
  const seeAll = el("button", "see-all", `See all ${section.count.toLocaleString()}`);
  seeAll.addEventListener("click", () => {
    navGo(nav, { kind: "page", input: query, path: [groupStep], title: section.title });
  });
  head.append(el("h3", "group-title", section.title), seeAll);

  let shape = "row";
  if (ROUND_GROUPS.includes(section.title)) shape = "round";
  if (COVER_GROUPS.includes(section.title)) shape = "square";
  const entries = section.items.map((item) => [item, item.index]);
  const art = section.items.some((item) => item.imageKey);
  group.append(head, itemsBlock(nav, query, [groupStep], entries, { shape, art }));
  return group;
}

// --- Roon pages (album, artist, "See all", menus) ---

function renderPage(nav, entry, page) {
  if (page.action !== "list" || !page.list) {
    showStatus(nav, page.message ?? "Nothing to show here.");
    return;
  }
  nav.status.hidden = true;
  const list = page.list;
  const entries = page.items.map((item, i) => [item, page.offset + i]);

  // Roon's "Play Album" / "Play Artist" item becomes the header's Play button
  const first = entries[0]?.[0];
  const play = first?.hint === "action_list" && first.title.startsWith("Play") ? entries.shift() : null;
  const isPerson = /^Play (Artist|Composer)/.test(first?.title ?? "");

  const parts = [];
  if (list.imageKey || play) {
    parts.push(pageHead(nav, entry, list, play, isPerson));
  } else {
    parts.push(listHead(list));
  }

  // Albums and artists (mostly pictures, opening pages) as tiles like Roon's
  // grid, the rest as rows. Rows skip pictures on pages with their own (an
  // album's tracks share its cover). Missing pictures get the stand-in.
  const withArt = entries.filter(([item]) => item.imageKey).length;
  const tiles =
    entries.length > 0 && entries.every(([item]) => item.hint === "list") && withArt >= entries.length / 2;
  const look = {
    shape: tiles ? (ROUND_GROUPS.includes(list.title) ? "round" : "square") : "row",
    art: !list.imageKey && entries.some(([item]) => item.imageKey),
  };
  const block = itemsBlock(nav, entry.input, entry.path, entries, look);
  parts.push(block);
  nav.view.replaceChildren(...parts);

  // Remembered for loading the rest of long lists while scrolling
  nav.page = { entry, block, look, loaded: page.offset + page.items.length, total: list.count };
}

// The top of a list page without art, like Roon's "My Artists / 1959 artists"
const COUNT_NOUNS = ["Artists", "Albums", "Tracks", "Composers", "Tags", "Playlists", "Genres"];

function listHead(list) {
  const head = el("div", "list-head");
  head.append(el("div", "list-title", list.title));
  const noun = COUNT_NOUNS.includes(list.title) ? list.title.toLowerCase() : "items";
  head.append(el("div", "list-count", list.subtitle ?? `${list.count.toLocaleString()} ${noun}`));
  return head;
}

// The top of an album or artist page: art, title, subtitle, and Roon's split
// Play button ("Play now", with ▾ for the rest of Roon's options)
function pageHead(nav, entry, list, play, isPerson) {
  const head = el("div", "page-head");
  if (list.imageKey) {
    const artClass = isPerson ? "page-art round" : "page-art";
    const fallback = isPerson ? () => initialsArt(list.title, artClass) : undefined;
    head.append(roonImage(list.imageKey, 400, artClass, fallback));
  }
  const info = el("div", "page-info");
  info.append(el("div", "page-title", list.title));
  if (list.subtitle) info.append(el("div", "page-sub", list.subtitle));

  if (play) {
    const [item, index] = play;
    const playPath = [...entry.path, { index, title: item.title }];

    const playBtn = el("button", "play-btn");
    playBtn.innerHTML = PLAY_ICON;
    playBtn.append("Play now");
    playBtn.addEventListener("click", (event) => playNow(nav, entry.input, playPath, event));

    const moreBtn = el("button", "play-more");
    moreBtn.title = "More options";
    moreBtn.innerHTML = CHEVRON_DOWN_ICON;
    moreBtn.addEventListener("click", () => {
      const rect = moreBtn.getBoundingClientRect();
      showActions(nav, entry.input, playPath, { clientX: rect.left, clientY: rect.bottom + 6 });
    });

    const split = el("div", "play-split");
    split.append(playBtn, moreBtn);
    info.append(split);
  }
  head.append(info);
  return head;
}

// --- Actions: Play Now, Add Next, Queue, Start Radio, ... ---

const actionMenu = $("#action-menu");

// Down arrow on the Play button (Lucide "chevron-down")
const CHEVRON_DOWN_ICON =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg>';

// An icon for each of Roon's actions, by name (Lucide)
const ACTION_ICONS = {
  "Play Now": PLAY_ICON,
  "Add Next":
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 7v6h-6"/><path d="M3 17a9 9 0 0 1 9-9 9 9 0 0 1 6 2.3l3 2.7"/></svg>',
  Queue:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M11 12H3"/><path d="M16 6H3"/><path d="M16 18H3"/><path d="M18 9v6"/><path d="M21 12h-6"/></svg>',
  "Start Radio":
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4.9 19.1C1 15.2 1 8.8 4.9 4.9"/><path d="M7.8 16.2c-2.3-2.3-2.3-6.1 0-8.5"/><circle cx="12" cy="12" r="2"/><path d="M16.2 7.8c2.3 2.3 2.3 6.1 0 8.5"/><path d="M19.1 4.9C23 8.8 23 15.1 19.1 19"/></svg>',
  Shuffle:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m18 14 4 4-4 4"/><path d="m18 2 4 4-4 4"/><path d="M2 18h1.973a4 4 0 0 0 3.3-1.7l5.454-8.6a4 4 0 0 1 3.3-1.7H22"/><path d="M2 6h1.972a4 4 0 0 1 3.6 2.2"/><path d="M22 18h-6.041a4 4 0 0 1-3.3-1.8l-.359-.45"/></svg>',
};

// Asks Roon which actions an item has: [item, index] pairs
async function fetchActions(nav, input, path) {
  const result = await invoke("roon_browse_path", {
    session: nav.session,
    hierarchy: nav.hierarchy,
    input,
    path,
    zoneId: zone?.zoneId ?? null,
  });
  if (result.action !== "list") return [];
  return result.items.map((item, i) => [item, result.offset + i]);
}

// Opens the action menu for an item, at the mouse (or at a given spot)
async function showActions(nav, input, path, event) {
  let actions;
  try {
    actions = await withSpinner(fetchActions(nav, input, path));
  } catch (err) {
    console.error("actions", err);
    return;
  }
  if (actions.length === 0) return;

  actionMenu.replaceChildren();
  for (const [item, index] of actions) {
    const option = el("button", "action-option");
    const icon = el("span", "action-icon");
    icon.innerHTML = ACTION_ICONS[item.title] ?? "";

    // Queue shows how long the queue already is, like Roon
    let label = item.title;
    if (item.title === "Queue" && zone?.queueTimeRemaining) {
      label += ` (${formatLongTime(zone.queueTimeRemaining)})`;
    }
    option.append(icon, el("span", null, label));

    const actionPath = [...path, { index, title: item.title }];
    option.addEventListener("click", () => {
      closeActionMenu();
      if (item.hint === "action_list") {
        showActions(nav, input, actionPath, event); // a submenu
      } else {
        runAction(nav, input, actionPath);
      }
    });
    actionMenu.append(option);
  }

  // Place it at the spot, kept inside the window (above the spot if no room below)
  actionMenu.hidden = false;
  const left = Math.min(event.clientX, window.innerWidth - actionMenu.offsetWidth - 8);
  let top = event.clientY;
  if (top + actionMenu.offsetHeight > window.innerHeight - 8) top -= actionMenu.offsetHeight;
  actionMenu.style.left = `${Math.max(8, left)}px`;
  actionMenu.style.top = `${Math.max(8, top)}px`;
}

function closeActionMenu() {
  actionMenu.hidden = true;
}

// Runs an action on the selected zone
async function runAction(nav, input, path) {
  try {
    const result = await invoke("roon_browse_path", {
      session: nav.session,
      hierarchy: nav.hierarchy,
      input,
      path,
      zoneId: zone?.zoneId ?? null,
    });
    if (result.isError) console.warn("Roon:", result.message);
  } catch (err) {
    console.error("action", err);
  }
}

// The big Play button: runs Roon's "Play Now" directly (or shows the menu if
// there isn't one)
async function playNow(nav, input, path, event) {
  try {
    const actions = await withSpinner(fetchActions(nav, input, path));
    const found = actions.find(([item]) => item.title.toLowerCase() === "play now");
    if (found) {
      const [item, index] = found;
      runAction(nav, input, [...path, { index, title: item.title }]);
    } else {
      showActions(nav, input, path, event);
    }
  } catch (err) {
    console.error("play", err);
  }
}

// Clicking elsewhere, resizing, or leaving the window closes the menu
document.addEventListener("pointerdown", (event) => {
  if (!actionMenu.hidden && !actionMenu.contains(event.target)) closeActionMenu();
});
window.addEventListener("resize", closeActionMenu);
window.addEventListener("blur", closeActionMenu);

// --- Long lists: load the next 100 when scrolled near the bottom ---

async function loadMore(nav) {
  const page = nav.page;
  if (!page || page.busy || page.loaded >= page.total) return;
  if (main.scrollTop + main.clientHeight < main.scrollHeight - 800) return;

  page.busy = true;
  try {
    const more = await withSpinner(
      invoke("roon_browse_path", {
        session: nav.session,
        hierarchy: nav.hierarchy,
        input: page.entry.input,
        path: page.entry.path,
        zoneId: zone?.zoneId ?? null,
        offset: page.loaded,
      }),
    );
    if (nav.page !== page) return; // moved to another page meanwhile
    more.items.forEach((item, i) => {
      const node = itemNode(nav, page.entry.input, page.entry.path, item, more.offset + i, page.look);
      page.block.append(node);
    });
    page.loaded = more.items.length ? more.offset + more.items.length : page.total;
  } catch (err) {
    console.error("load more", err);
  } finally {
    page.busy = false;
  }
}

// ===== 11. Search tab =====================================================

const searchNav = {
  session: "search",
  hierarchy: "search",
  backBtn: $("#search-back"),
  forwardBtn: $("#search-forward"),
  titleEl: $("#search-nav-title"),
  view: $("#search-view"),
  status: $("#search-status"),
  history: [], // pages visited, for back/forward
  position: -1, // which of them is showing
  ticket: 0, // bumps on every navigation, so a slow old answer is ignored
  page: null, // the Roon page showing, for loading more of long lists
};

// Runs a search: a new results page in the Search tab
function runSearch(text) {
  lastSearch = text;
  showTab("search");
  $("#search-hint").hidden = true;
  navGo(searchNav, { kind: "results", query: text, title: `Results for "${text}"` });
}

searchNav.backBtn.addEventListener("click", () => navBack(searchNav));
searchNav.forwardBtn.addEventListener("click", () => navForward(searchNav));

// --- Shared by the library tabs (Search and Browse) ---

// The navigator of the tab that's showing, if it's a library tab
function activeNav() {
  if ($("#tab-search").classList.contains("active")) return searchNav;
  if ($("#tab-browse").classList.contains("active")) return browseNav;
  return null;
}

// Mouse side buttons: back (button 3) and forward (button 4)
document.addEventListener("mouseup", (event) => {
  if (event.button !== 3 && event.button !== 4) return;
  event.preventDefault();
  const nav = activeNav();
  if (!nav) return;
  if (event.button === 3) navBack(nav);
  else navForward(nav);
});

// Scrolling closes the action menu and loads more of long lists
main.addEventListener("scroll", () => {
  closeActionMenu();
  const nav = activeNav();
  if (nav) loadMore(nav);
});

// ===== 12. Browse tab =====================================================
//
// Starts on a home list like Roon's sidebar, built from what the Core lists:
// everything inside Library (except Search), then everything else at the top
// (except Library and Settings). Streaming services (Qobuz, TIDAL, ...) only
// appear when they're linked in Roon.

const browseNav = {
  session: "browse",
  hierarchy: "browse",
  backBtn: $("#browse-back"),
  forwardBtn: $("#browse-forward"),
  titleEl: $("#browse-nav-title"),
  view: $("#browse-view"),
  status: $("#browse-status"),
  history: [], // pages visited, for back/forward
  position: -1, // which of them is showing
  ticket: 0, // bumps on every navigation, so a slow old answer is ignored
  page: null, // the Roon page showing, for loading more of long lists
};

// Roon entries left off the home list
const HOME_SKIP = ["Library", "Settings", "Search"];

// Home list icons, by Roon's name (Lucide); streaming services get a cloud
const CLOUD_ICON =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9Z"/></svg>';
const HOME_ICONS = {
  artists:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="8" r="5"/><path d="M20 21a8 8 0 0 0-16 0"/></svg>',
  albums:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M6 12c0-1.7.7-3.2 1.8-4.2"/><circle cx="12" cy="12" r="2"/><path d="M18 12c0 1.7-.7 3.2-1.8 4.2"/></svg>',
  tracks:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/></svg>',
  composers:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20.24 12.24a6 6 0 0 0-8.49-8.49L5 10.5V19h8.5z"/><line x1="16" x2="2" y1="8" y2="22"/><line x1="17.5" x2="9" y1="15" y2="15"/></svg>',
  tags:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12.586 2.586A2 2 0 0 0 11.172 2H4a2 2 0 0 0-2 2v7.172a2 2 0 0 0 .586 1.414l8.704 8.704a2.426 2.426 0 0 0 3.42 0l6.58-6.58a2.426 2.426 0 0 0 0-3.42z"/><circle cx="7.5" cy="7.5" r=".5" fill="currentColor"/></svg>',
  playlists:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15V6"/><path d="M18.5 18a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5Z"/><path d="M12 12H3"/><path d="M16 6H3"/><path d="M12 18H3"/></svg>',
  genres:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M8.3 10a.7.7 0 0 1-.626-1.079L11.4 3a.7.7 0 0 1 1.198-.043L16.3 8.9a.7.7 0 0 1-.572 1.1Z"/><rect x="3" y="14" width="7" height="7" rx="1"/><circle cx="17.5" cy="17.5" r="3.5"/></svg>',
  "my live radio":
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4.9 19.1C1 15.2 1 8.8 4.9 4.9"/><path d="M7.8 16.2c-2.3-2.3-2.3-6.1 0-8.5"/><circle cx="12" cy="12" r="2"/><path d="M16.2 7.8c2.3 2.3 2.3 6.1 0 8.5"/><path d="M19.1 4.9C23 8.8 23 15.1 19.1 19"/></svg>',
};

// Fetches the home list: [{ title, path }] in Roon's order
async function loadHome(nav) {
  const call = (path) =>
    invoke("roon_browse_path", {
      session: nav.session,
      hierarchy: nav.hierarchy,
      input: null,
      path,
      zoneId: zone?.zoneId ?? null,
    });
  const skip = (item) => HOME_SKIP.includes(item.title) || item.inputPrompt || item.hint !== "list";

  const top = await call([]);
  const links = [];
  for (const [i, item] of top.items.entries()) {
    const step = { index: top.offset + i, title: item.title };
    if (item.title === "Library") {
      const library = await call([step]);
      library.items.forEach((sub, j) => {
        if (!skip(sub)) {
          links.push({ title: sub.title, path: [step, { index: library.offset + j, title: sub.title }] });
        }
      });
    } else if (!skip(item)) {
      links.push({ title: item.title, path: [step] });
    }
  }
  return links;
}

// Draws the home list: one row per entry, icon + name
function renderHome(nav, links) {
  nav.status.hidden = true;
  const list = el("ol", "home-list");
  for (const link of links) {
    const row = el("li", "home-row");
    const icon = el("span", "home-icon");
    icon.innerHTML = HOME_ICONS[link.title.toLowerCase()] ?? CLOUD_ICON;
    row.append(icon, el("span", null, link.title));
    row.addEventListener("click", () => {
      navGo(nav, { kind: "page", input: null, path: link.path, title: link.title });
    });
    list.append(row);
  }
  nav.view.replaceChildren(list);
}

// Called when the Browse tab is opened: shows home the first time, and
// retries a home list that failed to load (e.g. before Roon was connected)
function openBrowse() {
  const entry = browseNav.history[browseNav.position];
  if (!entry) {
    navGo(browseNav, { kind: "home", title: "Browse" });
  } else if (entry.kind === "home" && !entry.data) {
    navShow(browseNav);
  }
}

browseNav.backBtn.addEventListener("click", () => navBack(browseNav));
browseNav.forwardBtn.addEventListener("click", () => navForward(browseNav));

// ===== 13. Zoom ===========================================================
//
// Ctrl + / Ctrl - (or Ctrl + mouse wheel) zoom the Toaster in 5% steps and
// Ctrl + Enter resets it to 100%, when "Zoom Hotkeys" is on in Settings. 100% is the
// normal size (Rust scales it by BASE_ZOOM underneath). The zoom is saved;
// Rust applies it (everything scales together: text, spacing, art).

const ZOOM_STEP = 0.05;
const ZOOM_MIN = 0.5; // same range as ZOOM_RANGE in settings.rs
const ZOOM_MAX = 1.8;
const zoomToast = $("#zoom-toast");
let appSettings = { zoom: 1, zoomHotkeys: true };
let zoomToastTimer = 0;

invoke("get_settings").then((settings) => (appSettings = settings));
listen("settings-changed", (event) => (appSettings = event.payload));

// A small pill at the bottom showing the new zoom
function showZoomToast() {
  zoomToast.textContent = `Zoom ${Math.round(appSettings.zoom * 100)}%`;
  zoomToast.hidden = false;
  clearTimeout(zoomToastTimer);
  zoomToastTimer = setTimeout(() => (zoomToast.hidden = true), 1500);
}

// step: +0.05 / -0.05, or null to reset to 100%
function changeZoom(step) {
  const target = step === null ? 1 : appSettings.zoom + step;
  const zoom = Math.round(Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, target)) * 100) / 100;
  appSettings.zoom = zoom; // right away, so quick repeated presses add up
  send("update_settings", { changes: { zoom } });
  showZoomToast();
}

document.addEventListener("keydown", (event) => {
  if (!event.ctrlKey || event.altKey || !appSettings.zoomHotkeys) return;
  let step;
  if (event.key === "=" || event.key === "+") step = ZOOM_STEP;
  else if (event.key === "-") step = -ZOOM_STEP;
  else if (event.key === "Enter") step = null;
  else return;
  event.preventDefault();
  changeZoom(step);
});

document.addEventListener(
  "wheel",
  (event) => {
    if (!event.ctrlKey || !appSettings.zoomHotkeys) return;
    event.preventDefault();
    changeZoom(event.deltaY < 0 ? ZOOM_STEP : -ZOOM_STEP);
  },
  { passive: false },
);

// ===== 14. Album colors ===================================================
//
// With Settings > Colors on "Album Art", the Toaster takes its accent color
// and a background tint from the album that's playing, and fades to the next
// album's colors (the fade itself is CSS: see @property in toaster.css).
// Art without a clear color (black and white, greys) gets a white accent.
// Finding the color is in album-colors.js, shared with the toast.

const darkScheme = matchMedia("(prefers-color-scheme: dark)");
let colorsMode = "default"; // "default" or "album", from Settings

// Sets the colors for the current album (or back to default)
async function applyAlbumColors() {
  const root = document.documentElement;
  const key = artKey;
  root.classList.toggle("album-colors", colorsMode === "album");

  let hue = null;
  if (colorsMode === "album" && key) {
    try {
      hue = await albumHue(key);
    } catch (err) {
      console.warn("album colors", err);
    }
    if (key !== artKey) return; // the art changed meanwhile; that call wins
  }

  paintColors(root, hue ? albumPalette(hue, darkScheme.matches) : {});
}

// Follow the Colors setting, and Windows switching between light and dark
invoke("get_settings").then((settings) => {
  colorsMode = settings.toasterColors;
  applyAlbumColors();
});
listen("settings-changed", (event) => {
  if (event.payload.toasterColors !== colorsMode) {
    colorsMode = event.payload.toasterColors;
    applyAlbumColors();
  }
});
darkScheme.addEventListener("change", applyAlbumColors);

// ===== 15. Start ===========================================================

// Load the current state once, then follow live updates from Rust
invoke("roon_zones").then(render);
listen("roon-zones", (event) => render(event.payload));
listen("roon-seek", (event) => {
  if (zone && event.payload.zoneId === zone.zoneId) {
    updatePosition(event.payload.seekPosition);
  }
});

invoke("roon_queue").then(renderQueue);
listen("roon-queue", (event) => renderQueue(event.payload));