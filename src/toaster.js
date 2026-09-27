// ===== 1. Setup ===========================================================

// Tauri's JavaScript API (available because withGlobalTauri is on)
const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (selector) => document.querySelector(selector);

const zoneName = $("#zone-name");
const nothingPlaying = $("#nothing-playing");
const nowPlaying = $("#now-playing");
const art = $("#art");
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
  });
});

// ===== 4. Toaster modes, Escape, settings cog =============================

// Rust tells us how the Toaster was opened: "player" or "search".
// Search mode: Search tab, bar focused, old text selected so typing replaces it.
listen("toaster-open", (event) => {
  if (event.payload === "search") {
    showTab("search");
    searchInput.focus();
    searchInput.select();
  }
});

// Enter in the search bar opens the Search tab (results come in a later step)
searchInput.addEventListener("keydown", (event) => {
  if (event.key !== "Enter") return;
  const text = searchInput.value.trim();
  if (!text) return;
  lastSearch = text;
  showTab("search");
});

// Escape, in order: close the zone picker if it's open, clear the search bar
// if you're typing in it, otherwise hide the Toaster
document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape") return;

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
      }
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
  if (item.imageKey) {
    const img = document.createElement("img");
    img.src = convertFileSrc(item.imageKey, "roonimg") + "?width=96&height=96";
    img.loading = "lazy";
    img.alt = "";
    artBox.append(img);
  }
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

// ===== 10. Start ===========================================================

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