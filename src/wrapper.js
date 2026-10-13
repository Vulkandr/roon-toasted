// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The Wrapper's page. Tells wrapper.rs where the hole is (and which of our
// menus and panels sit over it) whenever that changes, and runs the player
// bar under it: what's playing in the Toaster's zone, hearts, transport,
// progress, queue, zone picker and volume. Same Rust commands and events as
// the Toaster. Now Playing (the "stage") covers Roon with big art and the
// track's text, laid out however the user likes.

import { albumHue, albumPalette, paintColors } from "./album-colors.js";
import { ICON_PACKS } from "./icons.js";
import { WaveBar } from "./bars.js";

// Tauri's JavaScript API (available because withGlobalTauri is on)
const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const appWindow = window.__TAURI__.window.getCurrentWindow();

const $ = (selector) => document.querySelector(selector);

const hole = $("#hole");
const holeNote = $("#hole-note");
const status = $("#status");
const art = $("#art");
const artMissing = $("#art-missing");
const title = $("#title");
const trackText = $("#info .text");
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
const zoneButton = $("#zone-button");
const zoneName = $("#zone-name");
const zoneMenu = $("#zone-menu");
const muteBtn = $("#mute");
const volume = $("#volume");
const volumeValue = $("#volume-value");
const libraryBtn = $("#library-btn");
const queueBtn = $("#queue-btn");
const queuePanel = $("#queue-panel");
const queueSummary = $("#queue-summary");
const queueList = $("#queue-list");
const queueMore = $("#queue-more");
const queueEmpty = $("#queue-empty");
const stage = $("#stage");
const stageBg = $("#stage-bg");
const stageCanvas = $("#stage-canvas");
const stageArt = $("#stage-art");
const stageArtMissing = $("#stage-art-missing");
const stageEditBtn = $("#stage-edit");
const stageUnsaved = $("#stage-unsaved");
const stageEditor = $("#stage-editor");
const guideV = $("#guide-v");
const guideH = $("#guide-h");

const darkScheme = matchMedia("(prefers-color-scheme: dark)");

let zoneList = [];
let zone = null; // the Toaster's zone, from Rust
let artKey; // the art showing (undefined at first, so the first render always draws it)
let draggingSeek = false;
let draggingVolume = false;
let colorsMode = "default"; // "default" or "album", from Settings
let addMode = "track"; // what "add to library" adds, from Settings
let queue = { zoneId: null, items: [] };

// ===== Helpers =============================================================

function send(command, args) {
  return invoke(command, args).catch((err) => console.error(command, err));
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

// 188 -> "3:08"
function formatTime(seconds) {
  if (seconds == null || isNaN(seconds)) return "0:00";
  const s = Math.max(0, Math.floor(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

// Updates a slider's accent fill (the --fill variable in wrapper.css)
function setFill(slider) {
  const min = Number(slider.min);
  const max = Number(slider.max);
  const pct = max > min ? ((Number(slider.value) - min) / (max - min)) * 100 : 0;
  slider.style.setProperty("--fill", `${pct}%`);
  barChanged(slider);
}

// ===== The hole ============================================================

// The hole's rectangle in CSS pixels, relative to the page (which fills the
// window's client area), plus whatever we show over it (the zone menu, the
// queue panel), which has to stay part of the window. Rust turns them into
// real pixels.
function reportHole() {
  const rect = hole.getBoundingClientRect();
  const covers = [zoneMenu, queuePanel, stage]
    .filter((element) => !element.hidden)
    .map((element) => {
      const r = element.getBoundingClientRect();
      return [r.left, r.top, r.width, r.height];
    });
  invoke("wrapper_hole", {
    x: rect.left,
    y: rect.top,
    width: rect.width,
    height: rect.height,
    covers,
  }).catch(() => {});
}

const holeWatcher = new ResizeObserver(reportHole);
holeWatcher.observe(hole);
holeWatcher.observe(zoneMenu);
holeWatcher.observe(queuePanel);
holeWatcher.observe(stage);
reportHole();

// Rust's one-line status: in the title bar when something's off, and in
// the middle while Roon isn't there yet (starting, or not found)
// Roon couldn't be started (Rust gave up after a few tries): Now Playing
// opens if there's a zone to show, otherwise the note says so, large
let lostHandled = false;
async function refreshStatus() {
  try {
    const s = await invoke("wrapper_status");
    const fine = s.open && s.roonFound;
    status.hidden = fine || !s.note;
    status.textContent = s.roonLost ? "Couldn't open Roon" : s.note;
    holeNote.textContent = s.roonFound ? "" : s.note || "Looking for Roon...";
    holeNote.classList.toggle("lost", !s.roonFound && !!s.roonLost);
    if (s.roonLost && !lostHandled) {
      lostHandled = true;
      if (zone) setStageOpen(true);
    } else if (!s.roonLost) {
      lostHandled = false;
    }
  } catch {
    status.hidden = true;
  }
}
refreshStatus();
setInterval(refreshStatus, 1000);

// ===== Rendering ===========================================================

let lastZones = null; // the last zones payload, to render again when the queue comes in

function render(payload) {
  lastZones = payload;
  zoneList = payload.zones;
  zone = payload.zones.find((z) => z.zoneId === payload.selectedZoneId) ?? null;
  const np = zone?.nowPlaying;
  // Stopped with nothing loaded (Roon's output came back after Roon was
  // restarted, say), but a queue waiting: what Play will start, as Roon
  // itself shows it
  const next = !np && zone && queue.zoneId === zone.zoneId ? queue.items[0] : null;

  zoneName.textContent = zone ? zone.name : "No zone";

  // Roon's "On" / "Performed by" labels in Now Playing are for tracks only
  trackText.classList.toggle("track", !!np);
  if (!zone) {
    showText("Roon: Toasted", "No zone", "");
  } else if (np) {
    showText(np.title, np.artist, np.album);
  } else if (next) {
    showText(next.title, next.artist, next.album);
  } else {
    showText("Nothing playing", zone.name, "");
  }
  showArt(np?.imageKey ?? next?.imageKey ?? null);

  seek.max = np?.length ?? 0;
  seek.disabled = !zone?.canSeek || !np?.length;
  length.textContent = formatTime(np?.length);
  updatePosition(np ? zone.seekPosition : 0);

  previousBtn.disabled = !zone?.canPrevious;
  nextBtn.disabled = !zone?.canNext;
  playPauseBtn.disabled = !zone || (!zone.canPlay && !zone.canPause);
  playPauseBtn.classList.toggle("playing", zone?.state === "playing");
  shuffleBtn.disabled = !zone;
  repeatBtn.disabled = !zone;
  radioBtn.disabled = !zone;
  shuffleBtn.classList.toggle("on", !!zone?.shuffle);
  repeatBtn.classList.toggle("on", !!zone && zone.loopMode !== "disabled");
  repeatBtn.classList.toggle("one", zone?.loopMode === "loop_one");
  radioBtn.classList.toggle("on", !!zone?.autoRadio);

  const queued = zone?.queueItemsRemaining ?? 0;
  queueSummary.textContent = queued
    ? `${queued} ${queued === 1 ? "track" : "tracks"} remaining (${formatLongTime(zone.queueTimeRemaining)})`
    : "";
  updateQueueMore();

  renderOutput(zone?.outputs[0]);
  renderLibrary();
  for (const wave of Object.values(waves)) wave.setPlaying(zone?.state === "playing");
}

function showText(t, a, b) {
  title.textContent = t;
  artist.textContent = a;
  album.textContent = b;
}

function showArt(imageKey) {
  if (imageKey === artKey) return;
  artKey = imageKey;
  for (const [img, missing, size] of [
    [art, artMissing, 200],
    [stageArt, stageArtMissing, 1200],
  ]) {
    img.hidden = !imageKey;
    missing.hidden = !!imageKey;
    if (imageKey) img.src = convertFileSrc(imageKey, "roonimg") + `?width=${size}&height=${size}`;
    else img.removeAttribute("src");
  }
  stageBg.style.backgroundImage = imageKey ? `url("${stageArt.src}")` : "";
  applyColors();
}

// Art that fails to load gets the logo stand-in
art.addEventListener("error", () => {
  art.hidden = true;
  artMissing.hidden = false;
});
stageArt.addEventListener("error", () => {
  stageArt.hidden = true;
  stageArtMissing.hidden = false;
});

function updatePosition(seconds) {
  if (draggingSeek) return; // don't fight the user's drag
  seek.value = seconds ?? 0;
  elapsed.textContent = formatTime(seconds);
  setFill(seek);
}

function renderOutput(output) {
  const vol = output?.volume;
  const fixed = vol == null;
  volume.hidden = fixed;
  muteBtn.hidden = fixed;
  if (!output) {
    volumeValue.textContent = "";
    return;
  }
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

// Album colors, when the Toaster uses them
async function applyColors() {
  const key = artKey;
  let hue = null;
  // Worked out even when the app isn't on album colors: Now Playing's bars
  // can use it on their own
  if (key) {
    try {
      hue = await albumHue(key);
    } catch (err) {
      console.warn("album colors", err);
    }
    if (key !== artKey) return; // the art changed meanwhile
  }
  const palette = hue ? albumPalette(hue, darkScheme.matches) : null;
  albumAccent = palette?.["--accent"] ?? null;
  paintColors(document.documentElement, colorsMode === "album" && palette ? palette : {});
  layoutStage();
}

function applySettings(settings) {
  const colorsChanged = settings.toasterColors !== colorsMode;
  colorsMode = settings.toasterColors;
  addMode = settings.libraryAddMode;
  rememberPlayer = settings.wrapperRememberPlayer !== false;
  if (colorsChanged) applyColors();
  renderLibrary();
  loadLayout(settings.wrapperStage);
  restorePlayer();
}

// ===== Remember Player =====================================================
//
// Now Playing comes back as it was left, even after a restart (Settings >
// The Wrapper > Remember Player). Off, it's closed whenever the Wrapper
// closes, so it opens on Roon's page, as Roon itself does.

const PLAYER_OPEN_KEY = "wrapper.playerOpen";
let rememberPlayer = true;
let playerRestored = false;

function savePlayerOpen(open) {
  try {
    localStorage.setItem(PLAYER_OPEN_KEY, open ? "1" : "0");
  } catch {}
}

// Once, when the page starts: Now Playing back open if it was left open
function restorePlayer() {
  if (playerRestored) return;
  playerRestored = true;
  let wasOpen = false;
  try {
    wasOpen = localStorage.getItem(PLAYER_OPEN_KEY) === "1";
  } catch {}
  if (rememberPlayer && wasOpen) setStageOpen(true);
}

listen("wrapper-parked", () => {
  if (!rememberPlayer && !stage.hidden && !editing) setStageOpen(false);
});

// ===== Controls ============================================================

// Play or pause outright (not "playpause"): a stopped zone, with its
// output just back, starts with Play
playPauseBtn.addEventListener("click", () => {
  if (zone) send("roon_control", { zoneId: zone.zoneId, action: zone.state === "playing" ? "pause" : "play" });
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

// Art or text: Now Playing
$("#info").addEventListener("click", () => (stage.hidden ? setStageOpen(true) : closeStage()));

// In Now Playing the track text links to Roon's pages, like in Roon's own
// player: the click goes to the same link in Roon's player screen (kept open
// behind ours, see wrapper.rs), Roon goes to the page, and ours closes to
// show it
let linking = false;
for (const [link, which] of [[title, "title"], [album, "album"], [artist, "artist"]]) {
  link.addEventListener("click", async (event) => {
    if (stage.hidden || editing || !trackText.classList.contains("track")) return;
    event.stopPropagation();
    // One at a time: more clicks while one is on its way do nothing
    if (linking) return;
    linking = true;
    try {
      // Ours stays open if the link wasn't found on Roon's player screen
      const clicked = await invoke("wrapper_roon_link", { which });
      if (clicked) await dropStage();
    } catch (err) {
      console.error("wrapper_roon_link", err);
    } finally {
      linking = false;
    }
  });
}

// ===== Now Playing (the stage) =============================================
//
// Covers Roon and the player bar with big album art and the bar's own
// controls (the track's text, transport, seek, zone and volume), which move
// into the stage while it's open and back to the bar when it closes. Each
// is a "piece" with its own place, size and visibility. "Edit layout" lets
// the user drag pieces around, resize them by their handles, hide them, set
// text alignment, the volume's direction and the background; the layout is
// saved in Settings (wrapperStage).
//
// A piece's place is a fraction of the stage's free space (0 = against the
// left/top edge, 0.5 = centered, 1 = against the right/bottom edge), so
// centered things stay centered and bottom things stay at the bottom at
// any window size. Sizes are fractions too: text and control size of the
// stage's height, widths of its width, the art (always square) of its
// shorter side, the volume's length of its height when vertical and width
// when horizontal.

const PIECES = {
  art: { kind: "square" },
  info: { kind: "box", node: ".text", text: true },
  transport: { kind: "auto", node: ".transport", icons: true },
  seek: { kind: "box", node: ".progress", text: true, bar: true },
  // (the light leaves the zone's row for a piece of its own; listed first so
  // it's taken out before the row moves)
  quality: { kind: "auto", node: "#quality", text: true },
  zone: { kind: "auto", node: "#zone-row", text: true, icons: true },
  volume: { kind: "bar", node: ".volume-row", text: true, icons: true, bar: true },
};
const DEFAULT_ICONS = "lucide";
// The app's own accent (wrapper.css), for bars set to the app's color
const APP_ACCENT = "#7b5fc7";
let albumAccent = null; // the playing album's accent, whatever the app's Colors setting
// Fonts shipped with the app (fonts.css), offered before the PC's own
const BUNDLED_FONTS = ["Lato", "Inter", "Montserrat", "Oswald", "Bebas Neue", "Playfair Display", "Gloock", "JetBrains Mono"];
const DEFAULT_FONT = "Lato";
const PIECE_IDS = Object.keys(PIECES);
const LAYOUT_VERSION = 2;
// A black edge on a piece's text or icons: "none", "stroke" (an outline)
// or "glow", and its thickness in px
const NO_TEXT_EDGE = { textEdge: "none", textEdgeSize: 2 };
const NO_ICON_EDGE = { iconEdge: "none", iconEdgeSize: 2 };
const DEFAULT_LAYOUT = {
  barStyle: true, // the player bar uses these styles too (not the places)
  background: "art", // or "plain"
  invertToggles: false, // "Invert colors": what's white takes the accent, and what has the accent turns white
  pieces: {
    art: { visible: true, x: 0.5, y: 0.15, size: 0.83 },
    info: { visible: true, x: 0.02, y: 0.97, w: 0.3, size: 0.0173, align: "left", lines: 1, font: DEFAULT_FONT, ...NO_TEXT_EDGE },
    transport: { visible: true, x: 0.5, y: 0.942, size: 0.0173, icons: DEFAULT_ICONS, play: "circle", ...NO_ICON_EDGE },
    seek: { visible: true, x: 0.5, y: 0.988, w: 0.39, size: 0.0173, font: DEFAULT_FONT, bar: "line", mirror: true, side: "top", labelScale: 1, ...NO_TEXT_EDGE },
    // anchor "transport": sits just right of the controls, whatever the
    // window's size, until it's moved (then null, and x / y count)
    quality: { visible: true, anchor: "transport", x: 0.87, y: 0.925, size: 0.016, format: false, formatSide: "right", font: DEFAULT_FONT, ...NO_TEXT_EDGE },
    zone: { visible: true, x: 0.985, y: 0.975, size: 0.024, font: DEFAULT_FONT, icons: DEFAULT_ICONS, ...NO_TEXT_EDGE, ...NO_ICON_EDGE },
    volume: { visible: true, x: 0.99, y: 0.45, len: 0.78, size: 0.0173, vertical: true, font: DEFAULT_FONT, icons: DEFAULT_ICONS, bar: "line", mirror: true, side: "top", labelScale: 1, ...NO_TEXT_EDGE, ...NO_ICON_EDGE },
  },
};
const MIN_ART = 48; // px
const MIN_TEXT_SIZE = 9; // px
const MIN_LENGTH = 60; // px, the narrowest a width or length can be dragged
const MAX_SIZE = { square: 1, box: 0.3, auto: 0.15, bar: 0.15 }; // of the stage
const SNAP = 7; // px

// ----- Profiles -----
//
// A profile is a saved layout with a name. Presets ship with the app and
// can't be overwritten (changes to one are saved as a new profile); the
// user's own can be saved over, renamed and deleted. Settings keep the
// working layout (with any unsaved changes), which profile it came from,
// and the user's profiles.
const PRESETS = [
  { id: "default", name: "Default", layout: DEFAULT_LAYOUT },
  // Vulk's: a mirrored wave seek under the transport, wave volume
  { id: "waves", name: "Waves", layout: {
      background: "art",
      barStyle: true,
      pieces: {
        art: {
          size: 0.83,
          visible: true,
          x: 0.5,
          y: 0.15
        },
        info: {
          align: "left",
          font: "Lato",
          lines: 1,
          size: 0.0173,
          visible: true,
          w: 0.2633,
          x: 0.0191,
          y: 0.9703
        },
        seek: {
          bar: "wave",
          font: "Lato",
          mirror: true,
          side: "top",
          size: 0.0138,
          visible: true,
          w: 0.4418,
          x: 0.5,
          y: 0.9891
        },
        transport: {
          icons: "lucide",
          play: "circle",
          size: 0.0173,
          visible: true,
          x: 0.5,
          y: 0.9289
        },
        volume: {
          bar: "wave",
          font: "Lato",
          icons: "lucide",
          len: 0.7361,
          mirror: true,
          side: "top",
          size: 0.013,
          vertical: true,
          visible: true,
          x: 0.99,
          y: 0.4398
        },
        zone: {
          font: "Lato",
          icons: "lucide",
          size: 0.024,
          visible: true,
          x: 0.985,
          y: 0.975
        }
      }
    } },
  // Vulk's: bigger art and text, Phosphor icons, a one-sided wave seek
  // along the bottom edge
  { id: "waves-large", name: "Waves Large", layout: {
      background: "art",
      barStyle: true,
      pieces: {
        art: {
          size: 0.9106,
          visible: true,
          x: 0.5,
          y: 0.0496
        },
        info: {
          align: "left",
          font: "Inter",
          lines: 2,
          size: 0.0249,
          visible: true,
          w: 0.359,
          x: 0.0238,
          y: 0.9374
        },
        seek: {
          bar: "wave",
          font: "Inter",
          mirror: false,
          side: "top",
          size: 0.0133,
          visible: true,
          w: 0.9953,
          x: 0.5,
          y: 1
        },
        transport: {
          icons: "phosphor",
          play: "plain",
          size: 0.0173,
          visible: true,
          x: 0.5,
          y: 0.9725
        },
        volume: {
          bar: "wave",
          font: "Inter",
          icons: "phosphor",
          len: 0.8374,
          mirror: true,
          side: "top",
          size: 0.0173,
          vertical: true,
          visible: true,
          x: 0.99,
          y: 0.3227
        },
        zone: {
          font: "Inter",
          icons: "phosphor",
          size: 0.024,
          visible: true,
          x: 0.9894,
          y: 0.9499
        }
      }
    } },
];
let profiles = []; // the user's: { id, name, layout }
let activeProfile = "default";

const pieces = Object.fromEntries(PIECE_IDS.map((id) => [id, stage.querySelector(`[data-piece="${id}"]`)]));
// The player bar's controls that become pieces (the same elements move)
const CONTROLS = Object.fromEntries(PIECE_IDS.filter((id) => PIECES[id].node).map((id) => [id, $(PIECES[id].node)]));
let layout = structuredClone(DEFAULT_LAYOUT);
let editing = false;
let selected = null; // the piece being edited
let drag = null; // a move or resize in progress
const parked = {}; // where each moved control came from, to put it back

const clamp = (v, lo, hi) => Math.min(Math.max(v, lo), hi);
// Where a piece sits as a fraction of the free space, and back
const toFraction = (pos, total, extent) => (total > extent ? clamp(pos / (total - extent), 0, 1) : 0);
const fromFraction = (fraction, total, extent) => Math.round(fraction * Math.max(0, total - extent));

// Resize handles: corners on every piece (scale it), sides where there's a
// width or length to change
function makeHandles(id) {
  const el = pieces[id];
  for (const old of el.querySelectorAll(".handle")) old.remove();
  const p = layout.pieces[id];
  const kind = PIECES[id].kind;
  // Seek and volume: the ends change the length, the sides the thickness
  const dirs = PIECES[id].bar ? ["n", "s", "w", "e"] : ["nw", "ne", "sw", "se"];
  if (!PIECES[id].bar && kind === "box") dirs.push("w", "e");
  for (const dir of dirs) {
    const handle = document.createElement("span");
    handle.className = `handle ${dir}`;
    handle.dataset.dir = dir;
    el.append(handle);
  }
}
PIECE_IDS.forEach(makeHandles);

function setStageOpen(open) {
  if (open === !stage.hidden) return;
  savePlayerOpen(open);
  if (open) {
    // The bar's controls come in
    for (const id of PIECE_IDS) {
      const { node } = PIECES[id];
      if (!node) continue;
      const control = $(node);
      const placeholder = document.createComment(`${id} is in Now Playing`);
      control.replaceWith(placeholder);
      pieces[id].querySelector(".piece-content").append(control);
      parked[id] = { control, placeholder };
    }
    // Like Roon: the heart sits right after the title
    const heartHome = document.createComment("the heart is by the title");
    libraryBtn.replaceWith(heartHome);
    const titleRow = document.createElement("div");
    titleRow.className = "title-row";
    title.replaceWith(titleRow);
    titleRow.append(title, libraryBtn);
    parked.heart = { heartHome, titleRow };
  } else {
    if (editing) setEditing(false);
    dropWaves();
    if (parked.heart) {
      parked.heart.titleRow.replaceWith(title);
      parked.heart.heartHome.replaceWith(libraryBtn);
      delete parked.heart;
    }
    for (const id of PIECE_IDS) {
      const home = parked[id];
      if (!home) continue;
      home.placeholder.replaceWith(home.control);
      delete parked[id];
    }
  }
  stage.hidden = !open;
  $("#stage-minimize").hidden = !open;
  // Roon's own player screen opens and closes along (wrapper.rs), so its
  // links are there for ours
  send("wrapper_roon_player", { open });
  layoutStage();
  applyIcons();
  reportHole();
}

// ----- Icon packs -----
//
// Each icon's <svg> in the player bar is Lucide's; while Now Playing is
// open, a piece's pack (ICON_PACKS, icons.js) replaces what's inside it.
// Which svg is which: by its button and class.

const ICON_SLOTS = [
  ["transport", "#shuffle svg", "shuffle"],
  ["transport", "#previous svg", "previous"],
  ["transport", "#play-pause .icon-play", "play"],
  ["transport", "#play-pause .icon-pause", "pause"],
  ["transport", "#next svg", "next"],
  ["transport", "#repeat .icon-repeat", "repeat"],
  ["transport", "#repeat .icon-repeat-one", "repeatOne"],
  ["transport", "#radio svg", "radio"],
  ["transport", "#queue-btn svg", "queue"],
  ["transport", "#library-btn .icon-add", "add"],
  ["transport", "#library-btn .icon-heart", "heart"],
  ["zone", "#zone-button svg", "zone"],
  ["volume", "#mute .icon-volume", "volume"],
  ["volume", "#mute .icon-muted", "muted"],
].map(([piece, selector, role]) => {
  const svg = $(selector);
  // Lucide, as it is in the page
  const original = { viewBox: svg.getAttribute("viewBox"), body: svg.innerHTML, kind: "stroke" };
  return { piece, svg, role, original, showing: null };
});

function applyIcons() {
  const styled = barStyled();
  const heartOn = libraryBtn.classList.contains("on");
  for (const slot of ICON_SLOTS) {
    const pack = styled ? layout.pieces[slot.piece].icons || DEFAULT_ICONS : DEFAULT_ICONS;
    let role = slot.role;
    if (role === "heart" && heartOn && pack !== DEFAULT_ICONS) role = "heartFill";
    const key = `${pack}/${role}`;
    if (slot.showing === key) continue;
    slot.showing = key;
    const icon = ICON_PACKS[pack]?.icons[role] ?? slot.original;
    slot.svg.setAttribute("viewBox", icon.viewBox);
    slot.svg.innerHTML = icon.body;
    if (icon.kind === "stroke") {
      slot.svg.setAttribute("fill", "none");
      slot.svg.setAttribute("stroke", "currentColor");
      slot.svg.setAttribute("stroke-width", "2");
      slot.svg.setAttribute("stroke-linecap", "round");
      slot.svg.setAttribute("stroke-linejoin", "round");
    } else {
      slot.svg.setAttribute("fill", "currentColor");
      slot.svg.setAttribute("stroke", "none");
    }
    // Lucide's play/pause are filled outlines
    if (icon === slot.original && (role === "play" || role === "pause")) slot.svg.setAttribute("fill", "currentColor");
  }
}

$("#stage-close").addEventListener("click", () => closeStage());
$("#stage-minimize").addEventListener("click", () => closeStage());

// Puts every piece where the layout says, in pixels: first its size, then
// (knowing how big it came out) its place
function layoutStage() {
  renderFormat();
  styleControls();
  const W = stageCanvas.clientWidth;
  const H = stageCanvas.clientHeight;
  if (!W || !H) {
    fitWaves(); // the player bar's, if it's styled
    return;
  }
  const base = Math.min(W, H);
  stage.classList.toggle("art-bg", layout.background === "art");
  for (const id of PIECE_IDS) {
    const p = layout.pieces[id];
    const el = pieces[id];
    const { kind } = PIECES[id];
    el.classList.toggle("off", !p.visible);
    if (kind === "square") {
      const size = Math.round(p.size * base);
      el.style.width = `${size}px`;
      el.style.height = `${size}px`;
    } else {
      el.style.fontSize = `${Math.max(MIN_TEXT_SIZE, p.size * H).toFixed(1)}px`;
      if (kind === "box") {
        el.style.width = `${Math.round(p.w * W)}px`;
        el.style.height = "";
      } else if (kind === "bar") {
        el.classList.toggle("vertical", !!p.vertical);
        el.style.width = p.vertical ? "" : `${Math.round(p.len * W)}px`;
        el.style.height = p.vertical ? `${Math.round(p.len * H)}px` : "";
      } else {
        el.style.width = "";
        el.style.height = "";
      }
      if (id === "info") {
        el.style.textAlign = p.align;
        el.dataset.align = p.align;
        el.style.setProperty("--lines", p.lines);
      }
      if (PIECES[id].icons) el.dataset.icons = p.icons || DEFAULT_ICONS;
      // A bar's text and icons (times, level, mute) can be sized apart
      // from the bar itself, which follows the piece's size
      if (PIECES[id].bar) el.style.setProperty("--label-scale", p.labelScale || 1);
    }
    el.style.left = `${fromFraction(p.x, W, el.offsetWidth)}px`;
    el.style.top = `${fromFraction(p.y, H, el.offsetHeight)}px`;
  }
  // Pieces anchored beside another (the Quality light by the controls)
  for (const id of PIECE_IDS) {
    const to = layout.pieces[id].anchor;
    if (!to || !layout.pieces[to]?.visible) continue;
    const el = pieces[id];
    const by = pieces[to];
    const gap = Math.round(layout.pieces[id].size * H * 0.4);
    el.style.left = `${clamp(by.offsetLeft + by.offsetWidth + gap, 0, Math.max(0, W - el.offsetWidth))}px`;
    el.style.top = `${clamp(by.offsetTop + Math.round((by.offsetHeight - el.offsetHeight) / 2), 0, Math.max(0, H - el.offsetHeight))}px`;
  }
  fitWaves();
}

// ----- Styles on the controls -----
//
// Fonts, the play button's look and the bars' looks and colors go on the
// controls themselves (not the pieces), so they come along to the player
// bar when Now Playing closes, if the layout says so (barStyle).

function barStyled() {
  return !stage.hidden || layout.barStyle !== false;
}

// The CSS for a black edge: on text, a ring of sharp shadows (an outline)
// or soft ones (a glow); on icons, the same as filters
function textEdge(kind, size) {
  const n = Number(size) || 2;
  if (kind === "stroke") {
    const ring = [];
    for (let i = 0; i < 16; i++) {
      const a = (i * Math.PI) / 8;
      ring.push(`${(Math.cos(a) * n).toFixed(2)}px ${(Math.sin(a) * n).toFixed(2)}px 0 #000`);
    }
    return ring.join(", ");
  }
  if (kind === "glow") return `0 0 ${n}px #000, 0 0 ${n}px #000, 0 0 ${n * 2}px #000`;
  return "";
}

function iconEdge(kind, size) {
  const n = Number(size) || 2;
  // Each shadow is cast by everything before it, so the four together
  // ring the icon all the way round
  if (kind === "stroke") return `drop-shadow(${n}px 0 0 #000) drop-shadow(-${n}px 0 0 #000) drop-shadow(0 ${n}px 0 #000) drop-shadow(0 -${n}px 0 #000)`;
  if (kind === "glow") return `drop-shadow(0 0 ${n}px #000) drop-shadow(0 0 ${n}px #000)`;
  return "none";
}

function styleControls() {
  const on = barStyled();
  document.documentElement.classList.toggle("invert-colors", on && !!layout.invertToggles);
  for (const [id, control] of Object.entries(CONTROLS)) {
    const p = layout.pieces[id];
    const spec = PIECES[id];
    if (spec.text) control.style.textShadow = on ? textEdge(p.textEdge, p.textEdgeSize) : "";
    if (spec.icons) control.style.setProperty("--icon-edge", on ? iconEdge(p.iconEdge, p.iconEdgeSize) : "none");
    // The heart sits by the title in Now Playing, but it's one of the
    // controls' icons
    if (id === "transport") libraryBtn.style.setProperty("--icon-edge", on ? iconEdge(p.iconEdge, p.iconEdgeSize) : "none");
    if (spec.text) control.style.fontFamily = on ? fontStack(p.font) : "";
    if (spec.bar) {
      if (on) {
        control.dataset.bar = barOf(id);
        // The canvas styles share their slider rules (wrapper.css)
        if (CANVAS_BARS.includes(control.dataset.bar)) control.dataset.canvasBar = "";
        else delete control.dataset.canvasBar;
        control.style.setProperty("--accent", barColor(p).color);
      } else {
        delete control.dataset.bar;
        delete control.dataset.canvasBar;
        control.style.removeProperty("--accent");
      }
    }
    if (id === "transport") {
      if (on) control.dataset.play = p.play || "circle";
      else delete control.dataset.play;
    }
  }
}

// ----- Bars -----
//
// The seek and volume sliders' looks (`bar`): Line, Thick, Hairline and
// Segments are CSS on the slider itself (wrapper.css, by the piece's
// data-bar); Wave and the other drawn styles are painted on a canvas
// behind it (bars.js).

const waves = {}; // piece id -> WaveBar while that piece is drawn on a canvas
// The bar styles drawn on a canvas (bars.js); the rest are CSS
// The seek bar's Waveform looks (bars.js painters), drawn from Roon's own
// waveform of the playing track
// Any of them can be Alive too (`waveAlive`: bouncing gently near the
// playhead) and colored by loudness (`waveLoud`)
const WAVEFORM_LOOKS = {
  roon: "roonwave",
  pills: "wfpills",
  shape: "wfshape",
  neon: "wfneon",
  led: "wfled",
  art: "wfart",
};
// The piece's look, if it's one we have ("alive" and "<look>-alive" were
// briefly separate looks while trying them out)
function waveLookOf(p) {
  const look = p.waveLook === "alive" ? "roon" : (p.waveLook || "").replace(/-alive$/, "");
  return WAVEFORM_LOOKS[look] ? look : "roon";
}
const waveAliveOf = (p) => !!p.waveAlive || p.waveLook === "alive" || !!p.waveLook?.endsWith("-alive");
// Looks Loudness doesn't change (Album art shows the cover)
const LOUDNESS_FIXED = ["art"];
const CANVAS_BARS = ["wave", "spectrum", "beam", "glow", "dots", "ribbon", "neon", "mountain", "particles", "plasma", "art", ...Object.values(WAVEFORM_LOOKS)];

// The style a bar is drawn in: with Waveform on and a waveform for what's
// playing, the picked waveform look; otherwise the Bar choice (which is
// also the waveform's fallback)
function barOf(id) {
  const p = layout.pieces[id];
  if (id === "seek" && p.waveform && currentWaveform()) return WAVEFORM_LOOKS[waveLookOf(p)];
  return p.bar || "line";
}
const BAR_INPUTS = { seek, volume };

// A bar's color: the app's, the playing album's, or one the user picked.
// Not picked yet: whatever the app's Colors setting is.
function barColor(p) {
  const mode = p.color || (colorsMode === "album" ? "album" : "app");
  const color = mode === "custom" ? p.customColor || APP_ACCENT : mode === "album" ? albumAccent || APP_ACCENT : APP_ACCENT;
  return { mode, color };
}

function fitWaves() {
  const open = !stage.hidden;
  const H = stageCanvas.clientHeight;
  for (const id of ["seek", "volume"]) {
    const p = layout.pieces[id];
    const { mode, color } = barColor(p);
    // In the player bar the sliders are always shown, horizontal, at the
    // bar's own size
    const style = barOf(id);
    const want = barStyled() && (!open || p.visible) && CANVAS_BARS.includes(style);
    if (!want) {
      waves[id]?.remove();
      delete waves[id];
      continue;
    }
    const input = BAR_INPUTS[id];
    waves[id] ??= new WaveBar(input.parentElement, input);
    waves[id].fit({
      style,
      // Roon's waveform, for the Waveform looks
      waveform: Object.values(WAVEFORM_LOOKS).includes(style) ? currentWaveform() : null,
      // Waveform colors by loudness, and the Alive versions
      loudColor: id === "seek" && !!p.waveLoud,
      alive: id === "seek" && waveAliveOf(p),
      // The cover, for the Album art style (the small one is plenty)
      art: artKey && !art.hidden ? art.src : null,
      vertical: open && id === "volume" && !!p.vertical,
      // One-sided waves are for edges; in the player bar it's mirrored
      mirror: !open || p.mirror !== false,
      side: p.side || "top",
      // The app's look: the rainbow for Wave, blue-purple for the rest
      rainbow: mode === "app",
      color,
      // The playhead follows what's playing (or the picked color)
      dot: mode === "custom" ? color : albumAccent || APP_ACCENT,
      fontSize: open ? Math.max(MIN_TEXT_SIZE, p.size * H) : parseFloat(getComputedStyle(input.parentElement).fontSize) || 13,
      // The times' and knob's scale, so the wave's ends line up with them
      labelScale: open ? p.labelScale || 1 : 1,
    });
    waves[id].setPlaying(zone?.state === "playing");
  }
}

function dropWaves() {
  for (const id of Object.keys(waves)) {
    waves[id].remove();
    delete waves[id];
  }
}

// A slider's value changed (see setFill)
function barChanged(slider) {
  const id = slider === seek ? "seek" : slider === volume ? "volume" : null;
  if (id) waves[id]?.draw();
}

new ResizeObserver(layoutStage).observe(stageCanvas);

// A layout from Settings or a profile: each piece's saved values over the
// defaults, so pieces or fields added later just get theirs
function normalizeLayout(saved) {
  const out = structuredClone(DEFAULT_LAYOUT);
  if (!saved || typeof saved !== "object") return out;
  if (typeof saved.barStyle === "boolean") out.barStyle = saved.barStyle;
  if (typeof saved.invertToggles === "boolean") out.invertToggles = saved.invertToggles;
  if (saved.background === "art") out.background = "art";
  const savedPieces = saved.pieces;
  if (savedPieces && typeof savedPieces === "object") {
    for (const id of PIECE_IDS) {
      if (savedPieces[id] && typeof savedPieces[id] === "object") Object.assign(out.pieces[id], savedPieces[id]);
    }
  }
  return out;
}

// JSON with sorted keys, to compare layouts whatever order things were set in
function stable(value) {
  if (Array.isArray(value)) return `[${value.map(stable).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((k) => `${JSON.stringify(k)}:${stable(value[k])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function findProfile(id) {
  return PRESETS.find((p) => p.id === id) ?? profiles.find((p) => p.id === id) ?? null;
}

function isPreset(id) {
  return PRESETS.some((p) => p.id === id);
}

// The active profile as saved
function profileLayout(id = activeProfile) {
  return normalizeLayout(findProfile(id)?.layout);
}

function isDirty() {
  return stable(layout) !== stable(profileLayout());
}

// The settings from disk: the working layout, its profile, the profiles.
// A layout saved by an older version of this page starts over.
let savedLayoutJson = "";
function loadLayout(saved) {
  const current = saved?.version === LAYOUT_VERSION;
  const incomingProfiles = current && Array.isArray(saved.profiles)
    ? saved.profiles
        .filter((p) => p && typeof p.id === "string" && typeof p.name === "string")
        .map((p) => ({ id: p.id, name: p.name, layout: normalizeLayout(p.layout) }))
    : [];
  const incoming = current ? normalizeLayout(saved) : structuredClone(DEFAULT_LAYOUT);
  const json = stable({ incoming, incomingProfiles, profile: saved?.profile });
  if (json === savedLayoutJson) return;
  if (editing) return; // what's being edited here is newer
  profiles = incomingProfiles;
  activeProfile = current && findProfile(saved.profile) ? saved.profile : "default";
  layout = incoming;
  savedLayoutJson = json;
  PIECE_IDS.forEach(makeHandles);
  layoutStage();
  applyIcons();
  fillProfiles();
  showUnsaved();
}

let saveTimer;
function scheduleSave() {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(saveLayout, 400);
  showUnsaved();
}

function saveLayout() {
  clearTimeout(saveTimer);
  const json = stable({ incoming: layout, incomingProfiles: profiles, profile: activeProfile });
  if (json === savedLayoutJson) return;
  savedLayoutJson = json;
  send("update_settings", {
    changes: { wrapperStage: { version: LAYOUT_VERSION, ...layout, profile: activeProfile, profiles } },
  });
}

// ----- Profile controls -----

const profileSelect = $("#editor-profile-select");
const unsavedTag = $("#editor-unsaved");
const profileSaveBtn = $("#profile-save");
const profileSaveAsBtn = $("#profile-save-as");
const profileRenameBtn = $("#profile-rename");
const profileDeleteBtn = $("#profile-delete");

function fillProfiles() {
  const group = (label, list) => {
    const g = document.createElement("optgroup");
    g.label = label;
    for (const p of list) {
      const option = document.createElement("option");
      option.value = p.id;
      option.textContent = p.name;
      g.append(option);
    }
    return g;
  };
  const groups = [group("Presets", PRESETS)];
  if (profiles.length) groups.push(group("Mine", profiles));
  profileSelect.replaceChildren(...groups);
  profileSelect.value = activeProfile;
  showUnsaved();
}

function showUnsaved() {
  const dirty = isDirty();
  const own = !isPreset(activeProfile);
  unsavedTag.hidden = !dirty;
  // Outside the editor: the "Unsaved Style" tag by the pencil
  stageUnsaved.hidden = !dirty || editing;
  profileSaveBtn.hidden = !own;
  profileSaveBtn.disabled = !dirty;
  // The button that would keep the changes glows: Save for your own
  // profiles, Save as new for a preset (those can't be overwritten)
  profileSaveBtn.classList.toggle("unsaved", dirty && own);
  profileSaveAsBtn.classList.toggle("unsaved", dirty && !own);
  profileRenameBtn.hidden = !own;
  profileDeleteBtn.hidden = !own;
}

// Puts a profile's layout in place
function useProfile(id) {
  activeProfile = id;
  layout = profileLayout(id);
  PIECE_IDS.forEach(makeHandles);
  applyIcons();
  fillProfiles();
  changed();
  saveLayout();
}

function saveProfile() {
  const profile = profiles.find((p) => p.id === activeProfile);
  if (!profile) return;
  profile.layout = structuredClone(layout);
  saveLayout();
  showUnsaved();
}

const sameName = (a, b) => a.trim().toLowerCase() === b.trim().toLowerCase();

// Asks for a profile name until it's usable. A preset's name is taken; one
// of the user's own (other than `own`) can be overwritten if they say so.
// Resolves to { name, replace } (replace = the profile to overwrite), or
// null if cancelled.
async function askName(text, suggestion, action, own = null) {
  let input = suggestion;
  let note = "";
  for (;;) {
    const answer = await ask({
      text: note ? `${note} ${text}` : text,
      input,
      buttons: [{ label: "Cancel", value: null }, { label: action, value: "ok", primary: true }],
    });
    const name = answer.text?.trim();
    if (answer.value !== "ok" || !name) return null;
    // The user's own profiles first, so one that shares a preset's name
    // can still be overwritten
    const existing = profiles.find((p) => p !== own && sameName(p.name, name));
    if (!existing) {
      if (!PRESETS.some((p) => sameName(p.name, name))) return { name, replace: null };
      note = `"${name}" already exists as a preset, and presets can't be overwritten.`;
      input = name;
      continue;
    }
    const overwrite = await ask({
      text: `"${existing.name}" already exists. Overwrite it?`,
      buttons: [{ label: "Cancel", value: null }, { label: "Overwrite", value: "ok", primary: true }],
    });
    if (overwrite.value === "ok") return { name: existing.name, replace: existing };
    input = name;
    note = "";
  }
}

async function saveAsNew() {
  const base = findProfile(activeProfile)?.name ?? "My layout";
  const chosen = await askName("Name the new profile:", isPreset(activeProfile) ? `My ${base}` : `${base} copy`, "Save");
  if (!chosen) return false;
  if (chosen.replace) {
    chosen.replace.layout = structuredClone(layout);
    activeProfile = chosen.replace.id;
  } else {
    const id = `p${Date.now().toString(36)}`;
    profiles.push({ id, name: chosen.name, layout: structuredClone(layout) });
    activeProfile = id;
  }
  fillProfiles();
  saveLayout();
  return true;
}

// Unsaved changes, before something that would drop them: true when it's
// fine to go on (saved, or the user chose to discard), false to stay
async function settleChanges(question) {
  if (!isDirty()) return true;
  const name = findProfile(activeProfile)?.name ?? "this profile";
  const own = !isPreset(activeProfile);
  const buttons = [{ label: "Keep editing", value: null }, { label: "Discard", value: "discard" }, { label: "Save as new", value: "new", primary: !own }];
  if (own) buttons.push({ label: "Save", value: "save", primary: true });
  const answer = await ask({
    text: own
      ? `You changed "${name}". ${question}`
      : `You changed the "${name}" preset. Presets can't be overwritten, so save your changes as a new profile?`,
    buttons,
  });
  if (answer.value === "save") saveProfile();
  else if (answer.value === "new") return saveAsNew();
  else if (answer.value === "discard") useProfile(activeProfile);
  return answer.value != null;
}

profileSelect.addEventListener("change", async () => {
  const wanted = profileSelect.value;
  profileSelect.value = activeProfile;
  if (wanted === activeProfile) return;
  if (!(await settleChanges("Save your changes before switching?"))) return;
  useProfile(wanted);
});
profileSaveBtn.addEventListener("click", saveProfile);
profileSaveAsBtn.addEventListener("click", saveAsNew);
profileRenameBtn.addEventListener("click", async () => {
  const profile = profiles.find((p) => p.id === activeProfile);
  if (!profile) return;
  const chosen = await askName("Rename this profile:", profile.name, "Rename", profile);
  if (!chosen) return;
  // Taking another profile's name replaces that profile
  if (chosen.replace) profiles = profiles.filter((p) => p !== chosen.replace);
  profile.name = chosen.name;
  fillProfiles();
  saveLayout();
});
profileDeleteBtn.addEventListener("click", async () => {
  const profile = profiles.find((p) => p.id === activeProfile);
  if (!profile) return;
  const answer = await ask({
    text: `Delete "${profile.name}"? This can't be undone.`,
    buttons: [{ label: "Cancel", value: null }, { label: "Delete", value: "ok", primary: true }],
  });
  if (answer.value !== "ok") return;
  profiles = profiles.filter((p) => p !== profile);
  useProfile("default");
});

// ----- Questions (a small dialog over Now Playing) -----

const dialog = $("#stage-dialog");
const dialogText = $("#dialog-text");
const dialogInput = $("#dialog-input");
const dialogButtons = $("#dialog-buttons");
let dialogDone = null;

// Asks something; resolves to { value, text } (value null = cancelled)
function ask({ text, input = null, buttons }) {
  dialogDone?.({ value: null, text: null });
  return new Promise((resolve) => {
    dialogText.textContent = text;
    dialogInput.hidden = input == null;
    dialogInput.value = input ?? "";
    dialogButtons.replaceChildren(
      ...buttons.map((b) => {
        const button = document.createElement("button");
        button.className = b.primary ? "editor-btn primary" : "editor-btn";
        button.textContent = b.label;
        button.addEventListener("click", () => finish(b.value));
        return button;
      }),
    );
    const finish = (value) => {
      dialog.hidden = true;
      dialogDone = null;
      resolve({ value, text: dialogInput.value });
    };
    dialogDone = (result) => {
      dialog.hidden = true;
      dialogDone = null;
      resolve(result);
    };
    dialog.hidden = false;
    if (input != null) {
      dialogInput.focus();
      dialogInput.select();
    } else {
      dialogButtons.lastElementChild?.focus();
    }
    dialogInput.onkeydown = (event) => {
      if (event.key !== "Enter") return;
      // Without this the same key press would also click whatever the next
      // question focuses (an Overwrite button, say)
      event.preventDefault();
      finish(buttons.find((b) => b.primary)?.value ?? null);
    };
  });
}

function dialogOpen() {
  return !dialog.hidden;
}

function cancelDialog() {
  dialogDone?.({ value: null, text: null });
}

// A piece's font, falling back to the app's own if it's missing on this PC
function fontStack(name) {
  const safe = String(name || DEFAULT_FONT).replace(/["\\]/g, "");
  return `"${safe}", "Lato", "Noto Sans", "Segoe UI", system-ui, sans-serif`;
}

// The font list: bundled fonts, then the PC's, each shown in itself
const fontSelect = $("#editor-font-select");
let fontsLoaded = false;
async function loadFonts() {
  if (fontsLoaded) return;
  fontsLoaded = true;
  let installed = [];
  try {
    installed = await invoke("list_fonts");
  } catch (err) {
    console.warn("fonts", err);
  }
  const group = (label, names) => {
    const g = document.createElement("optgroup");
    g.label = label;
    for (const name of names) {
      const option = document.createElement("option");
      option.value = name;
      option.textContent = name;
      option.style.fontFamily = fontStack(name);
      g.append(option);
    }
    return g;
  };
  fontSelect.replaceChildren(
    group("Bundled", BUNDLED_FONTS),
    group("Installed", installed.filter((name) => !BUNDLED_FONTS.includes(name))),
  );
  refreshEditor();
}

// ----- Edit layout -----

function setEditing(on) {
  if (on === editing) return;
  editing = on;
  document.documentElement.classList.toggle("stage-editing", on);
  stageEditor.hidden = !on;
  stageEditor.style.left = "";
  stageEditor.style.top = "";
  stageEditor.style.transform = "";
  stageEditBtn.classList.toggle("on", on);
  stageEditBtn.title = on ? "Done editing" : "Edit layout";
  showUnsaved();
  select(null);
  if (on) {
    loadFonts();
    fillProfiles();
  } else {
    cancelDialog();
    saveLayout();
  }
}

// Done (or anything else that ends editing): unsaved changes stay as they
// are, in use, with "Unsaved Style" by the pencil until they're saved or
// discarded (only switching profiles asks, since that would drop them)
async function finishEditing() {
  setEditing(false);
  return true;
}

// Closing Now Playing while editing just ends editing, too
async function closeStage() {
  if (editing && !(await finishEditing())) return;
  await dropStage();
}

// Closes Now Playing once Roon's own player behind it is closed and done
// animating (wrapper.rs, at most about a second), so Roon's page and its
// recolored sidebar are already in place when ours goes
let dropping = false;
async function dropStage() {
  if (stage.hidden || dropping) return;
  dropping = true;
  try {
    await invoke("wrapper_roon_close");
  } catch (err) {
    console.error("wrapper_roon_close", err);
  } finally {
    dropping = false;
  }
  setStageOpen(false);
}

stageEditBtn.addEventListener("click", () => (editing ? finishEditing() : setEditing(true)));
stageUnsaved.addEventListener("click", () => setEditing(true));

// The toolbar can be dragged by its grip, so it's never in the way of a
// piece (it goes back to the top center each time editing starts)
const editorGrip = $("#editor-grip");
editorGrip.addEventListener("pointerdown", (event) => {
  if (event.button !== 0) return;
  event.preventDefault();
  const box = stageEditor.getBoundingClientRect();
  const area = stage.getBoundingClientRect();
  const grab = { x: event.clientX - box.left, y: event.clientY - box.top };
  const move = (e) => {
    const left = clamp(e.clientX - grab.x - area.left, 0, area.width - box.width);
    const top = clamp(e.clientY - grab.y - area.top, 0, area.height - box.height);
    stageEditor.style.left = `${left}px`;
    stageEditor.style.top = `${top}px`;
    stageEditor.style.transform = "none";
  };
  const up = () => {
    editorGrip.removeEventListener("pointermove", move);
    editorGrip.removeEventListener("pointerup", up);
    editorGrip.removeEventListener("pointercancel", up);
  };
  editorGrip.setPointerCapture(event.pointerId);
  editorGrip.addEventListener("pointermove", move);
  editorGrip.addEventListener("pointerup", up);
  editorGrip.addEventListener("pointercancel", up);
});
$("#editor-done").addEventListener("click", finishEditing);
// Reset: back to the profile as it was last saved
$("#editor-reset").addEventListener("click", () => useProfile(activeProfile));

// After any change from the toolbar or keyboard
function changed() {
  layoutStage();
  refreshEditor();
  scheduleSave();
}

function select(id) {
  selected = id;
  for (const pid of PIECE_IDS) pieces[pid].classList.toggle("selected", pid === id);
  refreshEditor();
}

// The toolbar: which pieces show, the background, and the selected
// piece's own options (text alignment and lines, the volume's direction)
const editorText = $("#editor-text");
const editorFont = $("#editor-font");
const editorIcons = $("#editor-icons");
const editorQuality = $("#editor-quality");
const qualityFormatCheck = $("#editor-quality-format");
const formatSide = $("#editor-format-side");
const editorPlay = $("#editor-play");
const editorBar = $("#editor-bar");
const barSelect = $("#editor-bar-select");
const editorWave = $("#editor-wave");
const editorWaveform = $("#editor-waveform");
const waveformCheck = $("#editor-waveform-check");
const waveformLook = $("#editor-waveform-look");
const waveformLoud = $("#editor-waveform-loud");
const waveformLoudLabel = $("#editor-waveform-loud-label");
const waveformAlive = $("#editor-waveform-alive");
const waveformAliveLabel = $("#editor-waveform-alive-label");
const colorInput = $("#editor-color");
const mirrorCheck = $("#editor-mirror");
const editorSide = $("#editor-side");
const iconsSelect = $("#editor-icons-select");
for (const [key, label] of [[DEFAULT_ICONS, "Lucide"], ...Object.entries(ICON_PACKS).map(([k, p]) => [k, p.label])]) {
  const option = document.createElement("option");
  option.value = key;
  option.textContent = label;
  iconsSelect.append(option);
}
const editorVolume = $("#editor-volume");
const linesSelect = $("#editor-lines");
const editorTextEdge = $("#editor-text-edge");
const textEdgeSelect = $("#editor-text-edge-select");
const textEdgeSize = $("#editor-text-edge-size");
const textEdgeValue = $("#editor-text-edge-value");
const textEdgeAll = $("#editor-text-edge-all");
const editorIconEdge = $("#editor-icon-edge");
const iconEdgeSelect = $("#editor-icon-edge-select");
const iconEdgeSize = $("#editor-icon-edge-size");
const iconEdgeValue = $("#editor-icon-edge-value");
const iconEdgeAll = $("#editor-icon-edge-all");
const invertCheck = $("#editor-invert");
const editorLabels = $("#editor-labels");
const labelScale = $("#editor-label-scale");
const labelScaleValue = $("#editor-label-scale-value");
function refreshEditor() {
  stageEditor.classList.toggle("piece-selected", !!selected);
  showUnsaved();
  for (const chip of stageEditor.querySelectorAll(".chip[data-show]")) {
    chip.classList.toggle("on", layout.pieces[chip.dataset.show].visible);
  }
  for (const chip of stageEditor.querySelectorAll(".chip[data-bg]")) {
    chip.classList.toggle("on", chip.dataset.bg === layout.background);
  }
  barStyleCheck.checked = layout.barStyle !== false;
  const text = selected === "info" ? layout.pieces.info : null;
  editorText.hidden = !text;
  for (const button of editorText.querySelectorAll(".align-btn")) {
    button.classList.toggle("on", !!text && button.dataset.align === text.align);
  }
  if (text) linesSelect.value = String(text.lines);
  const withFont = selected && PIECES[selected].text ? layout.pieces[selected] : null;
  editorFont.hidden = !withFont;
  if (withFont) {
    const name = withFont.font || DEFAULT_FONT;
    // A font that isn't in the list (uninstalled since): shown anyway
    if (![...fontSelect.options].some((o) => o.value === name)) {
      const option = document.createElement("option");
      option.value = name;
      option.textContent = name;
      fontSelect.append(option);
    }
    fontSelect.value = name;
  }
  const withIcons = selected && PIECES[selected].icons ? layout.pieces[selected] : null;
  editorIcons.hidden = !withIcons;
  editorQuality.hidden = selected !== "quality";
  qualityFormatCheck.checked = !!layout.pieces.quality.format;
  // The side: greyed out (still in place) while File format is off
  formatSide.classList.toggle("disabled", !qualityFormatCheck.checked);
  for (const chip of formatSide.querySelectorAll(".chip")) {
    chip.disabled = !qualityFormatCheck.checked;
    chip.classList.toggle("on", chip.dataset.formatSide === (layout.pieces.quality.formatSide || "right"));
  }
  if (withIcons) iconsSelect.value = withIcons.icons || DEFAULT_ICONS;
  const textEdgeOf = selected && PIECES[selected].text ? layout.pieces[selected] : null;
  editorTextEdge.hidden = !textEdgeOf;
  if (textEdgeOf) {
    textEdgeSelect.value = textEdgeOf.textEdge || "none";
    textEdgeSize.value = textEdgeOf.textEdgeSize || 2;
    textEdgeValue.textContent = `${textEdgeSize.value} px`;
    textEdgeSize.disabled = textEdgeSelect.value === "none";
  }
  const iconEdgeOf = selected && PIECES[selected].icons ? layout.pieces[selected] : null;
  editorIconEdge.hidden = !iconEdgeOf;
  if (iconEdgeOf) {
    iconEdgeSelect.value = iconEdgeOf.iconEdge || "none";
    iconEdgeSize.value = iconEdgeOf.iconEdgeSize || 2;
    iconEdgeValue.textContent = `${iconEdgeSize.value} px`;
    iconEdgeSize.disabled = iconEdgeSelect.value === "none";
  }
  invertCheck.checked = !!layout.invertToggles;
  const withLabels = selected && PIECES[selected].bar ? layout.pieces[selected] : null;
  editorLabels.hidden = !withLabels;
  if (withLabels) {
    labelScale.value = Math.round((withLabels.labelScale || 1) * 100);
    labelScaleValue.textContent = `${labelScale.value}%`;
  }
  const withBar = selected && PIECES[selected].bar ? layout.pieces[selected] : null;
  editorBar.hidden = !withBar;
  editorWaveform.hidden = selected !== "seek";
  if (withBar) {
    barSelect.value = withBar.bar || "line";
    const { mode, color } = barColor(withBar);
    for (const chip of editorBar.querySelectorAll(".chip[data-color]")) chip.classList.toggle("on", chip.dataset.color === mode);
    // The picker and Top / Bottom stay in place, greyed out while they don't
    // apply, so nothing on the line moves when they come into play
    colorInput.disabled = mode !== "custom";
    colorInput.value = toHex(color);
    // (Waveform: the seek bar's real waveform, drawn on the canvas too)
    const waveform = selected === "seek" && !!withBar.waveform;
    editorWave.hidden = !CANVAS_BARS.includes(barSelect.value) && !waveform;
    waveformCheck.checked = waveform;
    waveformLook.classList.toggle("off", !waveform);
    waveformLook.value = waveLookOf(withBar);
    // Loudness: with the picker; greyed out for the looks it doesn't change
    waveformLoudLabel.classList.toggle("off", !waveform);
    const fixed = LOUDNESS_FIXED.includes(waveformLook.value);
    waveformLoudLabel.classList.toggle("disabled", fixed);
    waveformLoud.disabled = fixed;
    waveformLoud.checked = !!withBar.waveLoud;
    waveformAliveLabel.classList.toggle("off", !waveform);
    waveformAlive.checked = waveAliveOf(withBar);
    mirrorCheck.checked = withBar.mirror !== false;
    editorSide.classList.toggle("disabled", mirrorCheck.checked);
    for (const chip of editorSide.querySelectorAll(".chip")) chip.disabled = mirrorCheck.checked;
    const vertical = selected === "volume" && withBar.vertical;
    for (const chip of editorSide.querySelectorAll(".chip")) {
      chip.classList.toggle("on", chip.dataset.side === (withBar.side || "top"));
      chip.textContent = chip.dataset.side === "top" ? (vertical ? "Left" : "Top") : vertical ? "Right" : "Bottom";
    }
  }
  editorPlay.hidden = selected !== "transport";
  for (const chip of editorPlay.querySelectorAll(".chip")) {
    chip.classList.toggle("on", selected === "transport" && chip.dataset.play === (layout.pieces.transport.play || "circle"));
  }
  const bar = selected === "volume" ? layout.pieces.volume : null;
  editorVolume.hidden = !bar;
  for (const chip of editorVolume.querySelectorAll(".chip")) {
    chip.classList.toggle("on", !!bar && chip.dataset.vertical === String(!!bar.vertical));
  }
}

for (const chip of stageEditor.querySelectorAll(".chip[data-show]")) {
  chip.addEventListener("click", () => {
    const p = layout.pieces[chip.dataset.show];
    p.visible = !p.visible;
    changed();
  });
}
for (const chip of stageEditor.querySelectorAll(".chip[data-bg]")) {
  chip.addEventListener("click", () => {
    layout.background = chip.dataset.bg;
    changed();
  });
}
for (const button of editorText.querySelectorAll(".align-btn")) {
  button.addEventListener("click", () => {
    layout.pieces.info.align = button.dataset.align;
    changed();
  });
}
// "All": the choice goes to every piece of that kind
const fontAll = $("#editor-font-all");
const barStyleCheck = $("#editor-barstyle");
barStyleCheck.addEventListener("change", () => {
  layout.barStyle = barStyleCheck.checked;
  changed();
  applyIcons();
});
const iconsAll = $("#editor-icons-all");
fontSelect.addEventListener("change", () => {
  if (!selected || !PIECES[selected].text) return;
  for (const id of fontAll.checked ? PIECE_IDS.filter((id) => PIECES[id].text) : [selected]) {
    layout.pieces[id].font = fontSelect.value;
  }
  changed();
});
// Text and icon edges: the kind and the thickness, to this piece or (All)
// every piece with text or icons
function setEdge(kind, value) {
  const type = kind === "text" ? "text" : "icons";
  if (!selected || !PIECES[selected][type]) return;
  const all = (kind === "text" ? textEdgeAll : iconEdgeAll).checked;
  for (const id of all ? PIECE_IDS.filter((id) => PIECES[id][type]) : [selected]) {
    Object.assign(layout.pieces[id], value);
  }
  changed();
}
textEdgeSelect.addEventListener("change", () => setEdge("text", { textEdge: textEdgeSelect.value, textEdgeSize: Number(textEdgeSize.value) }));
textEdgeSize.addEventListener("input", () => setEdge("text", { textEdge: textEdgeSelect.value, textEdgeSize: Number(textEdgeSize.value) }));
iconEdgeSelect.addEventListener("change", () => setEdge("icon", { iconEdge: iconEdgeSelect.value, iconEdgeSize: Number(iconEdgeSize.value) }));
iconEdgeSize.addEventListener("input", () => setEdge("icon", { iconEdge: iconEdgeSelect.value, iconEdgeSize: Number(iconEdgeSize.value) }));
labelScale.addEventListener("input", () => {
  if (!selected || !PIECES[selected].bar) return;
  layout.pieces[selected].labelScale = Number(labelScale.value) / 100;
  changed();
});
// Double-click: back to 100%
labelScale.addEventListener("dblclick", () => {
  if (!selected || !PIECES[selected].bar) return;
  layout.pieces[selected].labelScale = 1;
  changed();
});
invertCheck.addEventListener("change", () => {
  layout.invertToggles = invertCheck.checked;
  changed();
});
iconsSelect.addEventListener("change", () => {
  if (!selected || !PIECES[selected].icons) return;
  for (const id of iconsAll.checked ? PIECE_IDS.filter((id) => PIECES[id].icons) : [selected]) {
    layout.pieces[id].icons = iconsSelect.value;
  }
  applyIcons();
  changed();
});
barSelect.addEventListener("change", () => {
  if (!selected || !PIECES[selected].bar) return;
  layout.pieces[selected].bar = barSelect.value;
  changed();
});
for (const chip of editorBar.querySelectorAll(".chip[data-color]")) {
  chip.addEventListener("click", () => {
    if (!selected || !PIECES[selected].bar) return;
    const p = layout.pieces[selected];
    p.color = chip.dataset.color;
    if (p.color === "custom" && !p.customColor) p.customColor = toHex(albumAccent || APP_ACCENT);
    changed();
  });
}
colorInput.addEventListener("input", () => {
  if (!selected || !PIECES[selected].bar) return;
  layout.pieces[selected].customColor = colorInput.value;
  layoutStage();
  scheduleSave();
});
for (const chip of formatSide.querySelectorAll(".chip")) {
  chip.addEventListener("click", () => {
    layout.pieces.quality.formatSide = chip.dataset.formatSide;
    changed();
  });
}
qualityFormatCheck.addEventListener("change", () => {
  if (selected !== "quality") return;
  layout.pieces.quality.format = qualityFormatCheck.checked;
  changed();
});
waveformCheck.addEventListener("change", () => {
  if (selected !== "seek") return;
  layout.pieces.seek.waveform = waveformCheck.checked;
  changed();
});
waveformLook.addEventListener("change", () => {
  if (selected !== "seek") return;
  const p = layout.pieces.seek;
  p.waveAlive = waveAliveOf(p); // (an old "-alive" look keeps its Alive)
  p.waveLook = waveformLook.value;
  changed();
});
waveformAlive.addEventListener("change", () => {
  if (selected !== "seek") return;
  const p = layout.pieces.seek;
  p.waveLook = waveLookOf(p);
  p.waveAlive = waveformAlive.checked;
  changed();
});
waveformLoud.addEventListener("change", () => {
  if (selected !== "seek") return;
  layout.pieces.seek.waveLoud = waveformLoud.checked;
  changed();
});
mirrorCheck.addEventListener("change", () => {
  if (!selected || !PIECES[selected].bar) return;
  layout.pieces[selected].mirror = mirrorCheck.checked;
  changed();
});
for (const chip of editorSide.querySelectorAll(".chip")) {
  chip.addEventListener("click", () => {
    if (!selected || !PIECES[selected].bar) return;
    layout.pieces[selected].side = chip.dataset.side;
    changed();
  });
}

// Any CSS color as #rrggbb (for the color picker)
function toHex(color) {
  const probe = document.createElement("canvas").getContext("2d");
  probe.fillStyle = "#000";
  probe.fillStyle = color;
  const value = probe.fillStyle;
  if (value.startsWith("#")) return value;
  const m = value.match(/[\d.]+/g) || [0, 0, 0];
  return "#" + m.slice(0, 3).map((n) => Math.round(Number(n)).toString(16).padStart(2, "0")).join("");
}
for (const chip of editorPlay.querySelectorAll(".chip")) {
  chip.addEventListener("click", () => {
    layout.pieces.transport.play = chip.dataset.play;
    changed();
  });
}
linesSelect.addEventListener("change", () => {
  layout.pieces.info.lines = Number(linesSelect.value);
  changed();
});
for (const chip of editorVolume.querySelectorAll(".chip")) {
  chip.addEventListener("click", () => {
    layout.pieces.volume.vertical = chip.dataset.vertical === "true";
    makeHandles("volume");
    changed();
  });
}

// Pressing a piece selects it and starts a move (or a resize, from a handle)
for (const id of PIECE_IDS) {
  pieces[id].addEventListener("pointerdown", (event) => {
    if (!editing || event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    select(id);
    const handle = event.target.closest(".handle");
    const el = pieces[id];
    drag = {
      id,
      mode: handle ? handle.dataset.dir : "move",
      x0: event.clientX,
      y0: event.clientY,
      start: { ...layout.pieces[id], px: el.offsetLeft, py: el.offsetTop, pw: el.offsetWidth, ph: el.offsetHeight },
    };
    stageCanvas.setPointerCapture(event.pointerId);
  });
}
// Pressing the empty stage deselects
stageCanvas.addEventListener("pointerdown", (event) => {
  if (editing && event.button === 0) select(null);
});
stageCanvas.addEventListener("pointermove", (event) => {
  if (drag) dragTo(event.clientX, event.clientY);
});
stageCanvas.addEventListener("pointerup", endDrag);
stageCanvas.addEventListener("pointercancel", endDrag);
stageCanvas.addEventListener("lostpointercapture", endDrag);

function dragTo(cx, cy) {
  const W = stageCanvas.clientWidth;
  const H = stageCanvas.clientHeight;
  const base = Math.min(W, H);
  const dx = cx - drag.x0;
  const dy = cy - drag.y0;
  const p = layout.pieces[drag.id];
  const s = drag.start;
  const { kind } = PIECES[drag.id];
  const el = pieces[drag.id];

  if (drag.mode === "move") {
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    let x = s.px + dx;
    let y = s.py + dy;
    ({ x, y } = snapTo(x, y, w, h, W, H, drag.id));
    if (dx || dy) p.anchor = null; // moved by hand: its own place from now on
    p.x = toFraction(clamp(x, 0, Math.max(0, W - w)), W, w);
    p.y = toFraction(clamp(y, 0, Math.max(0, H - h)), H, h);
    layoutStage();
    return;
  }

  // A resize: the dragged edge or corner follows the mouse, the opposite
  // one stays put
  const sx = drag.mode.includes("e") ? 1 : drag.mode.includes("w") ? -1 : 0;
  const sy = drag.mode.includes("s") ? 1 : drag.mode.includes("n") ? -1 : 0;
  let width = s.pw;
  let height = s.ph;
  if (PIECES[drag.id].bar) {
    const alongX = !(drag.id === "volume" && p.vertical);
    const acrossDrag = (sx !== 0) !== alongX;
    if (!acrossDrag && sx !== 0) {
      // Its length, across the screen
      width = clamp(s.pw + sx * dx, MIN_LENGTH, W);
      if (kind === "box") p.w = width / W;
      else p.len = width / W;
    } else if (!acrossDrag) {
      // Its length, up the screen
      height = clamp(s.ph + sy * dy, MIN_LENGTH, H);
      p.len = height / H;
    } else {
      // Its thickness only: the bar gets taller (or thinner) while its
      // length stays, and its times or value and icon keep their size
      // (the "Text & icons" scale makes up for it, within its range); the
      // far side stays put
      const extent = sx !== 0 ? s.pw : s.ph;
      const moved = sx !== 0 ? sx * dx : sy * dy;
      const sizePx = s.size * H;
      let factor = clamp((extent + moved) / extent, MIN_TEXT_SIZE / sizePx, (H * MAX_SIZE[kind]) / sizePx);
      factor = Math.min(factor, alongX ? H / s.ph : W / s.pw);
      p.size = s.size * factor;
      p.labelScale = clamp((s.labelScale || 1) / factor, 0.5, 2);
      layoutStage();
      // Place it from its new size, keeping the side opposite the handle
      const nw = el.offsetWidth;
      const nh = el.offsetHeight;
      const left = alongX ? s.px : sx < 0 ? s.px + s.pw - nw : s.px;
      const top = alongX ? (sy < 0 ? s.py + s.ph - nh : s.py) : s.py;
      p.x = toFraction(clamp(left, 0, Math.max(0, W - nw)), W, nw);
      p.y = toFraction(clamp(top, 0, Math.max(0, H - nh)), H, nh);
      layoutStage();
      refreshEditor();
      return;
    }
  } else if (sx !== 0 && sy !== 0) {
    // A corner scales the whole piece, by whichever way it was dragged more
    const fx = (s.pw + sx * dx) / s.pw;
    const fy = (s.ph + sy * dy) / s.ph;
    let factor = Math.abs(fx - 1) >= Math.abs(fy - 1) ? fx : fy;
    const sizePx = kind === "square" ? s.size * base : s.size * H;
    const minPx = kind === "square" ? MIN_ART : MIN_TEXT_SIZE;
    const maxPx = (kind === "square" ? base : H) * MAX_SIZE[kind];
    factor = clamp(factor, minPx / sizePx, maxPx / sizePx);
    // And it has to stay inside the stage
    factor = Math.min(factor, W / s.pw, H / s.ph);
    p.size = s.size * factor;
    if (kind === "box") p.w = s.w * factor;
    if (kind === "bar") p.len = s.len * factor;
    width = s.pw * factor;
    height = s.ph * factor;
  } else if (sx !== 0) {
    width = clamp(s.pw + sx * dx, MIN_LENGTH, W);
    if (kind === "box") p.w = width / W;
    if (kind === "bar") p.len = width / W;
  } else {
    height = clamp(s.ph + sy * dy, MIN_LENGTH, H);
    p.len = height / H;
  }
  const left = clamp(sx < 0 ? s.px + s.pw - width : s.px, 0, Math.max(0, W - width));
  const top = clamp(sy < 0 ? s.py + s.ph - height : s.py, 0, Math.max(0, H - height));
  p.x = toFraction(left, W, width);
  p.y = toFraction(top, H, height);
  layoutStage();
}

// Snaps a moving piece to the stage's center and to the other visible
// pieces' edges and centers, showing a line where it snapped
function snapTo(x, y, w, h, W, H, id) {
  const xs = [{ at: W / 2, of: "center" }];
  const ys = [{ at: H / 2, of: "center" }];
  for (const other of PIECE_IDS) {
    if (other === id || !layout.pieces[other].visible) continue;
    const el = pieces[other];
    const l = el.offsetLeft;
    const t = el.offsetTop;
    xs.push({ at: l, of: "edge" }, { at: l + el.offsetWidth, of: "edge" }, { at: l + el.offsetWidth / 2, of: "center" });
    ys.push({ at: t, of: "edge" }, { at: t + el.offsetHeight, of: "edge" }, { at: t + el.offsetHeight / 2, of: "center" });
  }
  const snapAxis = (pos, extent, lines) => {
    let best = null;
    for (const line of lines) {
      // Our edges to their lines, our center to their centers
      const candidates = [line.at, line.at - extent];
      if (line.of === "center") candidates.push(line.at - extent / 2);
      for (const to of candidates) {
        const distance = Math.abs(pos - to);
        if (distance <= SNAP && (!best || distance < best.distance)) best = { to, line: line.at, distance };
      }
    }
    return best;
  };
  const sx = snapAxis(x, w, xs);
  const sy = snapAxis(y, h, ys);
  showGuide(guideV, sx ? sx.line : null, "left");
  showGuide(guideH, sy ? sy.line : null, "top");
  return { x: sx ? sx.to : x, y: sy ? sy.to : y };
}

function showGuide(guide, at, side) {
  guide.classList.toggle("show", at != null);
  if (at != null) guide.style[side] = `${Math.round(at)}px`;
}

function endDrag(event) {
  if (!drag) return;
  if (event.type !== "lostpointercapture") {
    try {
      stageCanvas.releasePointerCapture(event.pointerId);
    } catch {
      // already released
    }
  }
  drag = null;
  showGuide(guideV, null, "left");
  showGuide(guideH, null, "top");
  scheduleSave();
}

// Arrow keys nudge the selected piece (Shift: by 10)
document.addEventListener("keydown", (event) => {
  if (!editing || !selected || !event.key.startsWith("Arrow")) return;
  if (["INPUT", "SELECT", "TEXTAREA"].includes(document.activeElement?.tagName)) return;
  event.preventDefault();
  const W = stageCanvas.clientWidth;
  const H = stageCanvas.clientHeight;
  const step = event.shiftKey ? 10 : 1;
  const p = layout.pieces[selected];
  const el = pieces[selected];
  const dx = { ArrowLeft: -step, ArrowRight: step }[event.key] ?? 0;
  const dy = { ArrowUp: -step, ArrowDown: step }[event.key] ?? 0;
  p.anchor = null;
  p.x = toFraction(clamp(el.offsetLeft + dx, 0, Math.max(0, W - el.offsetWidth)), W, el.offsetWidth);
  p.y = toFraction(clamp(el.offsetTop + dy, 0, Math.max(0, H - el.offsetHeight)), H, el.offsetHeight);
  layoutStage();
  scheduleSave();
});

// ===== Hearts & Library (experimental) =====================================
//
// Same as the Toaster's: a "+" adds the playing track to the library (single
// song or whole album, per Settings), then it's a heart. Rust keeps the state
// live (`library-track`) and says whether the feature is on and connected
// (`library-status`).

let libraryReady = false;
let libraryTracks = {}; // zoneId -> the zone's track state (null = loading)
// zoneId -> { key, data }: the last waveform Roon sent for the zone's track.
// Kept while that track stays loaded, in case Roon drops it on pause.
let waveforms = {};
let shownWaveform = null;

const trackKey = (t) => (t ? t.trackId || t.title : null);

// Roon's waveform of what the shown zone is playing (Hearts & Library),
// or null
function currentWaveform() {
  if (!libraryReady || !zone?.nowPlaying) return null;
  const held = waveforms[zone.zoneId];
  const track = libraryTracks[zone.zoneId];
  if (!held || (track && trackKey(track) !== held.key)) return null;
  return held.data;
}

// Picks up new waveforms; redraws the seek bar when its waveform comes,
// goes or changes
function syncWaveform() {
  for (const [zoneId, track] of Object.entries(libraryTracks)) {
    if (track?.waveform) waveforms[zoneId] = { key: trackKey(track), data: track.waveform };
  }
  const now = currentWaveform();
  if (now === shownWaveform) return;
  shownWaveform = now;
  if (!layout.pieces.seek.waveform) return;
  styleControls();
  fitWaves();
}
let libraryBusy = false;

// The audio quality dot left of the zone icon (the track state carries the
// zone's signal path source, grey while paused or stopped)
const quality = $("#quality");
const qualityDot = quality.querySelector(".quality-dot");
const QUALITY_NAMES = { low: "Low Quality", cd: "CD Quality", hires: "Hi-Res" };

function renderQuality() {
  // Always there while connected; grey when there's nothing to read
  const connected = libraryReady && zone != null;
  const playing = connected && zone.nowPlaying ? libraryTracks[zone.zoneId]?.quality : null;
  const kind = playing && QUALITY_NAMES[playing.class] ? playing.class : "none";
  const wasHidden = quality.hidden;
  quality.hidden = !connected;
  qualityDot.classList.remove("low", "cd", "hires", "none");
  qualityDot.classList.add(kind);
  quality.dataset.tip = kind === "none" ? "No signal" : QUALITY_NAMES[kind];
  // The zone piece in Now Playing changed width
  if (wasHidden !== quality.hidden && !stage.hidden) layoutStage();
}

// The playing file's format next to the light: Now Playing only, with the
// Quality piece's File format on. Kept while paused (Roon clears the
// signal path then) as long as the same track is loaded.
const qualityFormat = $("#quality-format");
let formats = {}; // zoneId -> { key, text }

// "FLAC 44.1kHz 16bit 2ch", "MP3 320kbps 44.1kHz 2ch", "DSF 2.8MHz 1bit 2ch"
function formatText(q) {
  const parts = [];
  if (q.format) parts.push(q.format.toUpperCase());
  if (q.class === "low" && q.bitRate) parts.push(`${q.bitRate > 10000 ? Math.round(q.bitRate / 1000) : q.bitRate}kbps`);
  if (q.sampleRate) {
    parts.push(q.sampleRate >= 1e6 ? `${+(q.sampleRate / 1e6).toFixed(1)}MHz` : `${+(q.sampleRate / 1000).toFixed(1)}kHz`);
  }
  if (q.bitsPerSample && q.class !== "low") parts.push(`${q.bitsPerSample}bit`);
  if (q.channels) parts.push(`${q.channels}ch`);
  return parts.join(" ");
}

function renderFormat() {
  let text = "";
  if (!stage.hidden && layout.pieces.quality.format && libraryReady && zone?.nowPlaying) {
    const track = libraryTracks[zone.zoneId];
    const live = track?.quality ? formatText(track.quality) : "";
    if (live) formats[zone.zoneId] = { key: trackKey(track), text: live };
    const held = formats[zone.zoneId];
    if (held && (!track || trackKey(track) === held.key)) text = held.text;
  }
  quality.dataset.formatSide = layout.pieces.quality.formatSide || "right";
  if (qualityFormat.textContent !== text) qualityFormat.textContent = text;
  qualityFormat.hidden = !text;
  // (it hangs off the light, so the piece's size doesn't change with it)
}

function renderLibrary() {
  renderQuality();
  renderFormat();
  syncWaveform();
  const show = libraryReady && zone != null && zone.nowPlaying != null;
  libraryBtn.classList.toggle("spacer", !show);
  if (!show) return;

  const track = libraryTracks[zone.zoneId];
  const known = track != null;
  libraryBtn.classList.toggle("in-library", known && track.inLibrary);
  libraryBtn.classList.toggle("on", known && track.favorite);
  applyIcons();
  libraryBtn.classList.toggle("busy", libraryBusy);
  libraryBtn.disabled = !known;
  if (!known) {
    libraryBtn.title = "Waiting for Roon...";
  } else if (!track.inLibrary) {
    libraryBtn.title = addMode === "album" ? "Add album to library" : "Add to library";
  } else {
    libraryBtn.title = track.favorite ? "Remove heart" : "Heart";
  }
  // Rust only pushes states it has seen change; ask for this zone's if it's new
  if (!(zone.zoneId in libraryTracks)) {
    const zoneId = zone.zoneId;
    libraryTracks[zoneId] = null;
    invoke("library_track", { zoneId })
      .then((t) => {
        libraryTracks[zoneId] = t;
        renderLibrary();
      })
      .catch(() => delete libraryTracks[zoneId]);
  }
}

libraryBtn.addEventListener("click", async () => {
  const track = zone ? libraryTracks[zone.zoneId] : null;
  if (!zone || !track || libraryBusy) return;
  const zoneId = zone.zoneId;
  libraryBusy = true;
  renderLibrary();
  try {
    const result = track.inLibrary
      ? await invoke("library_heart", { zoneId, favorite: !track.favorite })
      : await invoke("library_add", { zoneId });
    libraryTracks[zoneId] = result;
  } catch (err) {
    console.error("library", err);
  } finally {
    libraryBusy = false;
    renderLibrary();
  }
});

function setLibraryStatus(status) {
  libraryReady = status.state === "ready";
  if (!libraryReady) {
    libraryTracks = {}; // states are only valid per connection
    waveforms = {};
    formats = {};
  }
  renderLibrary();
}

// ===== Queue panel =========================================================
//
// The Toaster's zone's queue (first 100 tracks); clicking a track plays from
// there. Opens over the right side of Roon.

// Play icon over the art when hovering an upcoming track (Lucide "play")
const PLAY_ICON =
  '<svg viewBox="0 0 24 24" fill="currentColor" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polygon points="6 3 20 12 6 21 6 3"/></svg>';

function queueRow(item, isNow) {
  const row = document.createElement("li");
  row.className = isNow ? "queue-item now" : "queue-item";

  const artBox = document.createElement("div");
  artBox.className = "queue-art";
  if (item.imageKey) {
    const img = document.createElement("img");
    img.alt = "";
    img.loading = "lazy";
    img.src = convertFileSrc(item.imageKey, "roonimg") + "?width=96&height=96";
    artBox.append(img);
  } else {
    const missing = document.createElement("div");
    missing.className = "art-placeholder";
    artBox.append(missing);
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

function renderQueue(payload) {
  queue = payload;
  // The player bar shows the queue's first track while nothing is loaded
  if (lastZones && zone && !zone.nowPlaying) render(lastZones);
  queueEmpty.hidden = payload.items.length > 0;
  queueList.replaceChildren(...payload.items.map((item, i) => queueRow(item, i === 0)));
  updateQueueMore();
}

// "+ N more tracks not shown" when the queue is longer than the 100 shown
function updateQueueMore() {
  const total = zone?.queueItemsRemaining ?? 0;
  const notShown = total - queue.items.length;
  queueMore.hidden = notShown <= 0;
  queueMore.textContent = `+ ${notShown.toLocaleString()} more tracks not shown`;
}

function setQueueOpen(open) {
  queuePanel.hidden = !open;
  queueBtn.classList.toggle("on", open);
  reportHole();
}

queueBtn.addEventListener("click", () => setQueueOpen(queuePanel.hidden));
$("#queue-close").addEventListener("click", () => setQueueOpen(false));

// ===== Zone picker =========================================================

// Checkmark next to the zone you're on (Lucide "check")
const CHECK_ICON =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"/></svg>';

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

// Opens above the zone button, right-aligned with it
function openZoneMenu() {
  if (zoneList.length === 0) return;
  fillZoneMenu();
  zoneMenu.hidden = false;
  const rect = zoneButton.getBoundingClientRect();
  const left = Math.max(12, rect.right - zoneMenu.offsetWidth);
  zoneMenu.style.left = `${left}px`;
  zoneMenu.style.bottom = `${window.innerHeight - rect.top + 6}px`;
  zoneMenu.querySelector(".selected")?.scrollIntoView({ block: "nearest" });
  reportHole();
}

function closeZoneMenu() {
  if (zoneMenu.hidden) return;
  zoneMenu.hidden = true;
  reportHole();
}

zoneButton.addEventListener("click", () => {
  if (zoneMenu.hidden) openZoneMenu();
  else closeZoneMenu();
});

// Clicking anywhere else, resizing, or leaving the window closes it
document.addEventListener("pointerdown", (event) => {
  if (zoneMenu.hidden) return;
  if (zoneMenu.contains(event.target) || zoneButton.contains(event.target)) return;
  closeZoneMenu();
});
window.addEventListener("resize", closeZoneMenu);
window.addEventListener("blur", closeZoneMenu);
// Escape: the zone menu first, then the queue panel, then Now Playing's
// editing, then Now Playing itself
document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape") return;
  if (dialogOpen()) cancelDialog();
  else if (!zoneMenu.hidden) closeZoneMenu();
  else if (!queuePanel.hidden) setQueueOpen(false);
  else if (editing) finishEditing();
  else if (!stage.hidden) closeStage();
});

// ===== Window buttons ======================================================

// The colors Roon's sidebar copy is painted in (skin.rs): the panel's
// background and the app's accent, as the browser works them out (album
// colors included); sent when they change. The accent is read off a hidden
// probe, not a real button: the play button can be plain (no background),
// which read as black and turned the sidebar's highlight black.
let sentColors = "";
const accentProbe = document.createElement("span");
accentProbe.style.cssText = "position: absolute; width: 0; height: 0; visibility: hidden; background: var(--accent);";
document.querySelector(".panel").append(accentProbe);
function sendColors() {
  const rgb = (value) => (value.match(/[\d.]+/g) || []).slice(0, 3).map((n) => Math.round(Number(n)));
  const bg = rgb(getComputedStyle(document.querySelector(".panel")).backgroundColor);
  const accent = rgb(getComputedStyle(accentProbe).backgroundColor);
  if (bg.length < 3 || accent.length < 3) return;
  const key = `${bg}|${accent}`;
  if (key === sentColors) return;
  sentColors = key;
  invoke("wrapper_colors", { bg, accent }).catch(() => {});
}
setInterval(sendColors, 400);
sendColors();

// Square corners and no border while maximized (wrapper.rs squares the
// window's shape then too)
async function updateMaximized() {
  document.documentElement.classList.toggle("maximized", await appWindow.isMaximized());
}
appWindow.onResized(updateMaximized);
updateMaximized();

// Square corners too while Zonekeeper has the Wrapper in one of its zones
// (zonekeeper.rs): it squares other windows' corners through Windows, which
// doesn't reach ours, since the page draws them
function setZoneSquared(on) {
  document.documentElement.classList.toggle("zone-squared", on === true);
}
invoke("zone_squared").then(setZoneSquared).catch(() => {});
listen("zone-squared", (event) => setZoneSquared(event.payload));

$("#win-min").addEventListener("click", () => appWindow.minimize());
$("#win-max").addEventListener("click", () => appWindow.toggleMaximize());
$("#win-close").addEventListener("click", () => appWindow.close());
$("#detach").addEventListener("click", () => send("wrapper_put_back"));
$("#open-settings").addEventListener("click", () => send("open_window", { window: "settings" }));

// No right-click menu
document.addEventListener("contextmenu", (event) => event.preventDefault());

// ===== Start ===============================================================

// Search set to Roon (the Search hotkey or a Stream Deck button): Now
// Playing goes first, so Roon's search box isn't hidden behind it
listen("wrapper-search", async () => {
  if (!stage.hidden) await closeStage();
  if (!stage.hidden) return; // still editing (asked to save first)
  invoke("wrapper_roon_search").catch((err) => console.error("wrapper_roon_search", err));
});

// Load the current state once, then follow live updates from Rust
invoke("get_settings").then(applySettings);
listen("settings-changed", (event) => applySettings(event.payload));
invoke("roon_zones").then(render);
listen("roon-zones", (event) => render(event.payload));
listen("roon-seek", (event) => {
  if (zone && event.payload.zoneId === zone.zoneId) updatePosition(event.payload.seekPosition);
});
darkScheme.addEventListener("change", applyColors);
invoke("library_status").then(setLibraryStatus);
listen("library-status", (event) => setLibraryStatus(event.payload));
listen("library-track", (event) => {
  libraryTracks[event.payload.zoneId] = event.payload.track;
  renderLibrary();
});
invoke("roon_queue").then(renderQueue);
listen("roon-queue", (event) => renderQueue(event.payload));
