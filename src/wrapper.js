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
const wrappedBtn = $("#wrapped-btn");
const wrappedOpen = $("#wrapped-open");
const wrappedArrow = $("#wrapped-arrow");
const wrappedPageBtn = $("#wrapped-page-btn");
const wrappedPageOpen = $("#wrapped-page-open");
const wrappedPageArrow = $("#wrapped-page-arrow");
const wrappedSide = $("#wrapped-side");
const wrappedSideBtn = $("#wrapped-side-btn");
const wrapped = $("#wrapped");
const wrappedTitle = $("#wrapped-title");
const wrappedSub = $("#wrapped-sub");
const wrappedBody = $("#wrapped-body");
const wrappedBack = $("#wrapped-back");

const darkScheme = matchMedia("(prefers-color-scheme: dark)");

let zoneList = [];
let zone = null; // the Toaster's zone, from Rust
let ratingReady = false; // the Discover thumbs are set up (renderRating)
let artKey; // the art showing (undefined at first, so the first render always draws it)
let draggingSeek = false;
let draggingVolume = false;
let colorsMode = "default"; // "default" or "album", from Settings
let addMode = "track"; // what "add to library" adds, from Settings
let queue = { zoneId: null, items: [] };
// Wrapped (the menu over Roon's logo and the page over Roon's content):
// Roon's sidebar width in real pixels (null = collapsed, undefined = no
// Roon), its grey, which page Roon is on (roonnav.rs), and our page stack
let sidebarPx;
let sidebarColor = null;
let headerColor = null; // Roon's own header background, behind the content-header button
let sidebarSeen = false; // whether skin.rs has reported a sidebar at least once
let roonNav = { page: "", albumId: null, lastAlbumId: null, available: false, navCount: 0 };
let wrappedStack = [];
// The pages gone back from (the mouse's forward button, newest last)
let wrappedForward = [];

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
// queue panel, the Wrapped button, menu and page), which has to stay part
// of the window. Rust turns them into real pixels.
function reportHole() {
  const rect = hole.getBoundingClientRect();
  const covers = [];
  const radii = []; // a cover's rounded corners are cut too, so Roon shows there
  for (const element of [zoneMenu, queuePanel, stage, wrappedBtn, wrappedPageBtn, wrappedSide, wrapped]) {
    if (element.hidden) continue;
    const r = element.getBoundingClientRect();
    // The cut-out has hard edges, so the buttons keep theirs square and paint
    // the surroundings of their rounded faces in Roon's own colors instead
    const radius = parseFloat(getComputedStyle(element).borderTopLeftRadius) || 0;
    covers.push([r.left, r.top, r.width, r.height]);
    radii.push(radius);
  }
  invoke("wrapper_hole", {
    x: rect.left,
    y: rect.top,
    width: rect.width,
    height: rect.height,
    covers,
    radii,
  }).catch(() => {});
}

const holeWatcher = new ResizeObserver(reportHole);
holeWatcher.observe(hole);
holeWatcher.observe(zoneMenu);
holeWatcher.observe(queuePanel);
holeWatcher.observe(stage);
holeWatcher.observe(wrappedBtn);
holeWatcher.observe(wrappedPageBtn);
holeWatcher.observe(wrappedSide);
holeWatcher.observe(wrapped);
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
    setSidebar(fine ? s.sidebar : undefined, s.sidebarColor, fine, s.headerColor);
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
  noteZoneSongs(payload.zones);
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
  queuePanel.classList.toggle("paused", zone?.state !== "playing");
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
  // Wrapped's title and buttons (its own control, inside the piece)
  wrapped: { kind: "auto", node: "#wrapped-piece", text: true, icons: true },
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
    wrapped: { visible: true, x: 0.015, y: 0.03, size: 0.0173, font: DEFAULT_FONT, ...NO_TEXT_EDGE, ...NO_ICON_EDGE },
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
  // The Wrapped buttons over Roon have no place over Now Playing
  layoutWrapped();
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
  // (the Discover thumbs move between the info and Wrapped pieces)
  if (ratingReady) renderRating();
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
// In the player bar the sliders stretch with the window (narrow windows
// shorten them), and the canvas behind a drawn bar has to follow
new ResizeObserver(() => {
  if (stage.hidden) fitWaves();
}).observe($(".player"));

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
  // (the accent fill up to the value, as the player's sliders have)
  for (const range of [textEdgeSize, iconEdgeSize, labelScale]) setFill(range);
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
for (const range of [textEdgeSize, iconEdgeSize, labelScale]) range.addEventListener("input", () => setFill(range));
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
  renderRating();
  renderQuality();
  renderFormat();
  syncWaveform();
  markWrappedPlaying();
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

// ----- Rating the Discover lists' songs (discover.rs) -----
//
// While a song from 🌟 Discover Weekly or 💎 Library Rediscover plays,
// thumbs sit above its title: up makes its artist a starting point for the
// next lists, down keeps it (and what sounds like it) out. A heart on such
// a song counts as a milder thumbs up.

let discoverRatings = { songs: [], votes: {} };
const rateBox = $("#rate");
const RATE_HELP = rateBox.title;
// (and in Now Playing, under the Wrapped piece's buttons, with why the
// song is there; the info piece's thumbs step aside while it shows)
const pieceRate = $("#wrapped-piece-rate");
const pieceWhy = pieceRate.querySelector(".wrapped-piece-why");
// (a Deep Dive's song gets its own line there: the Deep Dive and why)
const pieceDive = $("#wrapped-piece-dive");
const pieceDiveWhy = pieceDive.querySelector(".wrapped-piece-why");

// Wrapped Discovery (Wrapped's Settings): off, the star, the thumbs and
// the Discover page go away (the html element's "no-discovery" class)
async function discoveryEnabled() {
  if (!wrappedSettings) wrappedSettings = await invoke("wrapped_settings_get").catch(() => null);
  paintPieceDive();
  return wrappedSettings?.discovery !== false;
}

async function loadDiscoverRatings() {
  const on = await discoveryEnabled();
  document.documentElement.classList.toggle("no-discovery", !on);
  if (!on) {
    discoverRatings = { songs: [], votes: {} };
    renderRating();
    markDiscoverFresh();
    return;
  }
  try {
    discoverRatings = await invoke("wrapped_discover_ratings");
  } catch {
    return;
  }
  renderRating();
  markDiscoverFresh();
}

// The star's dot: a list built since the Discover page was last opened
const discoverBtn = $("#discover-btn");
function markDiscoverFresh() {
  let seen = 0;
  try {
    seen = Number(localStorage.getItem("wrapped-discover-seen")) || 0;
  } catch {}
  const newest = Math.max(0, ...(discoverRatings.newest || []));
  const fresh = newest > seen;
  discoverBtn.classList.toggle("fresh", fresh);
  document.querySelector(".wrapped-piece-btn.discover")?.classList.toggle("fresh", fresh);
}
function discoverSeen() {
  try {
    localStorage.setItem("wrapped-discover-seen", String(Date.now()));
  } catch {}
  markDiscoverFresh();
}
discoverBtn.addEventListener("click", () => openWrappedPage({ page: "discover" }));
// Switched in Toasted's Settings window (or here): the star, thumbs and
// menu follow, and an open Settings or Discover page redraws
listen("wrapped-settings-changed", (event) => {
  const was = wrappedSettings?.discovery !== false;
  wrappedSettings = event.payload;
  paintPieceDive();
  if ((wrappedSettings?.discovery !== false) === was) return;
  loadDiscoverRatings();
  if (wrapped.hidden) return;
  if (wrappedStack.at(-1)?.page === "settings") redrawSettingsInPlace();
  else if (["discover", "home"].includes(wrappedStack.at(-1)?.page)) renderWrapped();
});

// The song before the one playing, per zone: zone id -> { key, cur, before }
// ({ title, artist } each), for telling a list's song played from the list
// apart from the same song played from anywhere else
const zoneSongs = new Map();
function noteZoneSongs(zones) {
  for (const z of zones) {
    const np = z.nowPlaying;
    if (!np?.title) continue;
    const key = `${np.title}|${np.artist || ""}|${np.album || ""}`;
    const was = zoneSongs.get(z.zoneId);
    if (was?.key === key) continue;
    zoneSongs.set(z.zoneId, { key, cur: { title: np.title, artist: np.artist || "" }, before: was ? was.cur : null });
  }
}

// The list song playing now, or null: by Roon's track id, else by title
// and artist (a stream may come back under another id). Only when it's
// played from its list: the song before or after it is on the same list.
// The same song in a Deep Dive or any other playlist is ordinary listening
// (the history has it), with no thumbs and nothing counted for the lists.
function discoverSongNow() {
  const track = zone ? libraryTracks[zone.zoneId] : null;
  const np = zone?.nowPlaying;
  if (!np || !discoverRatings.songs.length) return null;
  const id = track?.trackId != null ? String(track.trackId) : null;
  const isIt = (s, title, artist) => !!s.title && s.title === title && (artist || "").toLowerCase().includes(s.artist.toLowerCase());
  const here = discoverRatings.songs.filter((s) => (id && s.trackId === id) || isIt(s, track?.title || np.title, np.artist));
  if (!here.length) return null;
  const near = [zoneSongs.get(zone.zoneId)?.before];
  if (queue?.zoneId === zone.zoneId && queue.items?.[0]?.title === np.title) near.push(queue.items[1]);
  const sameList = (s, n) => discoverRatings.songs.some((o) => o !== s && o.which === s.which && o.builtAt === s.builtAt && isIt(o, n.title, n.artist));
  return here.find((s) => near.some((n) => n && sameList(s, n))) || null;
}

// The Deep Dives made so far (dive.rs, the last 12), for the player
let diveLists = [];
async function loadDiveLists() {
  try {
    diveLists = (await invoke("wrapped_dive_status")).dives || [];
  } catch {
    return;
  }
  renderDivePiece();
}

// The Deep Dive song playing now ({ dive, song }), or null: by title and
// artist, and only when it's played from its playlist (the song before or
// after it is on the same Deep Dive), as with the Discover lists' songs
function diveSongNow() {
  const np = zone?.nowPlaying;
  if (!np || !diveLists.length) return null;
  const track = libraryTracks[zone.zoneId];
  const lower = (t) => (t || "").toLowerCase();
  const isIt = (s, title, artist) => !!s.title && lower(s.title) === lower(title) && lower(artist).includes(lower(s.artist));
  const near = [zoneSongs.get(zone.zoneId)?.before];
  if (queue?.zoneId === zone.zoneId && queue.items?.[0]?.title === np.title) near.push(queue.items[1]);
  for (const dive of diveLists) {
    const song = (dive.songs || []).find((s) => isIt(s, track?.title || np.title, np.artist));
    if (!song) continue;
    if (near.some((n) => n && dive.songs.some((o) => o !== song && isIt(o, n.title, n.artist)))) return { dive, song };
  }
  return null;
}

// Under the Wrapped piece's buttons, while a Deep Dive's song plays: "From
// your Deep Dive", its name (opens it on Deep Dive's page) and why the
// song is there, the same short line as on the page. No thumbs: Deep Dive
// songs are ordinary listening.
function renderDivePiece() {
  const found = diveSongNow();
  const key = found ? `${found.dive.builtAt}|${found.song.title}|${found.song.artist}` : "";
  if (pieceDive.dataset.key === key) return;
  pieceDive.dataset.key = key;
  pieceDive.hidden = !found;
  // (its size changed: the pieces placed again)
  if (!stage.hidden) requestAnimationFrame(() => layoutStage());
  if (!found) return;
  const { dive, song } = found;
  const lead = document.createElement("div");
  lead.className = "wrapped-piece-why-lead";
  lead.textContent = `From your Deep Dive · ${song.inLibrary ? "in your library" : "new to you"}`;
  const big = document.createElement("div");
  big.className = "wrapped-piece-why-names";
  const name = document.createElement("button");
  name.className = "wrapped-piece-link";
  name.textContent = dive.name;
  name.title = "Open it on Deep Dive's page";
  name.addEventListener("click", async (e) => {
    e.stopPropagation();
    if (editing) return;
    await closeStage();
    if (!stage.hidden) return;
    deepDiveTab = "dive";
    diveOpen.add(dive.builtAt);
    openWrappedPage({ page: "deepdive" });
  });
  big.append(name);
  pieceDiveWhy.replaceChildren(lead, big);
  if (song.why) {
    const line = document.createElement("div");
    line.className = "wrapped-piece-why-text";
    line.textContent = song.why;
    pieceDiveWhy.append(line);
  }
}

let heartSent = "";
function renderRating() {
  // (and the Deep Dive line next to it, which follows the same changes)
  renderDivePiece();
  const song = discoverSongNow();
  const inPiece = !stage.hidden && layout?.pieces?.wrapped?.visible !== false;
  rateBox.hidden = !song || inPiece;
  pieceRate.hidden = !song;
  if (!song) return;
  const key = song.artist.toLowerCase();
  const mine = discoverRatings.votes[key];
  for (const btn of document.querySelectorAll(".rate-btn[data-vote]")) {
    btn.classList.toggle("on", Number(btn.dataset.vote) === mine?.vote);
  }
  rateBox.dataset.which = song.which;
  // The reason: a small lead line, then the artists of yours it came from
  // large (links to their Wrapped pages); for a Rediscover song, its lead
  // ("In your library, never played") on top
  pieceWhy.replaceChildren();
  const names = song.because || [];
  let reason = song.reason || "";
  if (names.length) {
    const first = reason.indexOf(names[0]);
    const lead = document.createElement("div");
    lead.className = "wrapped-piece-why-lead";
    lead.textContent = (first > 0 ? reason.slice(0, first) : "Suggested because you listened to ").trim();
    const big = document.createElement("div");
    big.className = "wrapped-piece-why-names";
    names.forEach((name, i) => {
      if (i) big.append(" and ");
      const link = document.createElement("button");
      link.className = "wrapped-piece-link";
      link.textContent = name;
      link.title = `Open ${name} in Wrapped`;
      link.addEventListener("click", async (e) => {
        e.stopPropagation();
        if (editing) return;
        // (exact name first; then letters only, "Septic Flesh" being
        // Roon's "Septicflesh")
        let handle = await roonArtistHandle({ name });
        if (!handle) {
          const plain = (t) => t.toLowerCase().replace(/[^a-z0-9]/g, "");
          try {
            const hits = await invoke("library_search", { query: name });
            handle = hits.artists.find((a) => plain(a.name) === plain(name))?.handle || null;
          } catch {}
        }
        if (!handle) return wrappedToastStage(`Roon didn't find ${name}.`);
        await closeStage();
        if (stage.hidden) openWrappedPage({ page: "artist", handle });
      });
      big.append(link);
    });
    pieceWhy.append(lead, big);
    if (song.why) {
      const line = document.createElement("div");
      line.className = "wrapped-piece-why-text";
      line.textContent = song.why;
      pieceWhy.append(line);
    }
  } else if (reason) {
    const lead = document.createElement("div");
    lead.className = "wrapped-piece-why-lead";
    lead.textContent = reason;
    pieceWhy.append(lead);
  }
  pieceWhy.hidden = !song.reason;
  rateBox.title = song.reason ? `${song.reason}.\n\n${RATE_HELP}` : RATE_HELP;
  // A heart on a list song counts too (once per change)
  const hearted = !!(zone && libraryTracks[zone.zoneId]?.favorite);
  const state = `${key}|${hearted}`;
  if (hearted !== !!mine?.heart && heartSent !== state) {
    heartSent = state;
    invoke("wrapped_discover_vote", { artist: song.artist, heart: hearted })
      .then((r) => {
        discoverRatings = r;
      })
      .catch(() => {});
  }
}

async function rateNow(value) {
  const song = discoverSongNow();
  if (!song) return;
  const current = discoverRatings.votes[song.artist.toLowerCase()]?.vote || 0;
  // (the same thumb again takes it back)
  const vote = current === value ? 0 : value;
  try {
    discoverRatings = await invoke("wrapped_discover_vote", { artist: song.artist, vote });
  } catch (err) {
    return wrapperToast(String(err));
  }
  renderRating();
  wrapperToast(
    vote === 1
      ? `More like ${song.artist} in your next lists`
      : vote === -1
        ? `No more ${song.artist}, or bands that sound like them`
        : `Rating for ${song.artist} taken back`,
  );
}

for (const btn of document.querySelectorAll(".rate-btn[data-vote]")) {
  btn.addEventListener("click", (e) => {
    e.stopPropagation();
    rateNow(Number(btn.dataset.vote));
  });
}
ratingReady = true;

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
  const was = libraryReady;
  libraryReady = status.state === "ready";
  if (!libraryReady) {
    libraryTracks = {}; // states are only valid per connection
    waveforms = {};
    formats = {};
  }
  renderLibrary();
  // A Wrapped page waiting for the link (or showing handles from a dropped one)
  if (was !== libraryReady && !wrapped.hidden) renderWrapped();
  // The queue panel's played songs, ready before it's first opened; the
  // Discover lists' songs, for the thumbs
  if (libraryReady && !was) {
    loadQueueHistory(true);
    loadDiscoverRatings();
    loadDiveLists();
  }
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
  } else {
    // (moving bars on the playing song, as Roon has)
    const eq = document.createElement("span");
    eq.className = "queue-eq";
    eq.innerHTML = "<i></i><i></i><i></i>";
    artBox.append(eq);
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

// A song already played (from the Core's own queue; the Roon API's queue
// starts at the playing one)
function playedRow(song) {
  const row = queueRow(
    { title: song.title, artist: song.artist, imageKey: song.imageKey, length: song.lengthSeconds },
    false,
  );
  row.className = "queue-item played";
  // A copy without the Roon API's play-from-here (it only knows the songs
  // still to come); the Core's own, by item id, takes its place
  const copy = row.cloneNode(true);
  if (song.itemId != null) {
    copy.classList.add("playable");
    copy.addEventListener("click", () => {
      invoke("library_queue_play_from", { zoneId: zone.zoneId, itemId: song.itemId }).catch((err) => wrapperToast(String(err)));
    });
  } else {
    copy.querySelector(".queue-play")?.remove();
  }
  return copy;
}

// Where the zone's current queue began (queue.rs notes it when a queue is
// started or replaced): the played part goes back that far, not through
// everything Roon ever played
async function queueStart() {
  try {
    return (await invoke("roon_queue_start", { zoneId: zone.zoneId })) ?? null;
  } catch {
    return null;
  }
}

// The played part of the queue (library.rs, queue_songs), fetched when the
// playing song changes and once the library link is up, whether the panel
// is open or not (it takes a moment, so it's ready by the time it opens),
// and again when the panel opens
let queueHistory = [];
let historyFor = "";
let historyZone = "";
let historyTimer = null;
let scrolledFor = "";
function loadQueueHistory(force = false) {
  clearTimeout(historyTimer);
  historyTimer = setTimeout(async () => {
    if (!zone || !libraryReady) return;
    const key = `${zone.zoneId}|${queue?.items?.[0]?.queueItemId ?? ""}`;
    if (!force && key === historyFor) return;
    historyFor = key;
    // (another zone's played songs never show under this one's)
    if (historyZone !== zone.zoneId) {
      historyZone = zone.zoneId;
      queueHistory = [];
    }
    try {
      // (a queue begun in Roon, with no start noted: its last 30 items)
      const songs = await invoke("library_queue_songs", { zoneId: zone.zoneId, start: await queueStart(), recent: 30 });
      queueHistory = songs.filter((s) => s.played);
    } catch (err) {
      console.warn("library_queue_songs", err);
      queueHistory = [];
    }
    if (queue) renderQueue(queue, true);
  }, 400);
}

// Songs jumped over before the playing one fold into one block, as Roon's
// "N skipped items": a count tile and Expand / Collapse, the songs inside
// the same block (one background, a rail down its side) when open
const openSkips = new Set(); // folds opened, by their first song
function playedRows(songs) {
  const rows = [];
  for (let i = 0; i < songs.length; ) {
    if (!songs[i].skipped) {
      rows.push(playedRow(songs[i]));
      i += 1;
      continue;
    }
    let end = i;
    while (end < songs.length && songs[end].skipped) end += 1;
    const run = songs.slice(i, end);
    const foldKey = `${run[0].itemId ?? ""}|${run[0].title}`;
    const showSkipped = openSkips.has(foldKey);
    const group = document.createElement("li");
    group.className = showSkipped ? "queue-skip-group open" : "queue-skip-group";

    const head = document.createElement("div");
    head.className = "queue-skip-head";
    const count = document.createElement("span");
    count.className = "queue-skip-count";
    count.textContent = run.length;
    const text = document.createElement("div");
    text.className = "queue-text";
    const label = document.createElement("div");
    label.className = "queue-item-title";
    label.textContent = `${run.length} skipped song${run.length === 1 ? "" : "s"}`;
    const toggle = document.createElement("div");
    toggle.className = "queue-item-artist";
    toggle.textContent = showSkipped ? "Collapse ▴" : "Expand ▾";
    text.append(label, toggle);
    head.append(count, text);
    head.addEventListener("click", () => {
      if (showSkipped) openSkips.delete(foldKey);
      else openSkips.add(foldKey);
      renderQueue(queue, true);
    });
    group.append(head);

    if (showSkipped) {
      const list = document.createElement("ol");
      list.className = "queue-skip-list";
      list.append(...run.map(playedRow));
      group.append(list);
    }
    rows.push(group);
    i = end;
  }
  return rows;
}

function renderQueue(payload, fromHistory = false) {
  queue = payload;
  // The player bar shows the queue's first track while nothing is loaded
  if (lastZones && zone && !zone.nowPlaying) render(lastZones);
  queueEmpty.hidden = payload.items.length > 0 || queueHistory.length > 0;
  const rows = playedRows(queueHistory);
  let nowLabel = null;
  if (rows.length && payload.items.length) {
    nowLabel = document.createElement("li");
    nowLabel.className = "queue-now-label";
    nowLabel.textContent = "Now playing";
    rows.push(nowLabel);
  }
  rows.push(...payload.items.map((item, i) => queueRow(item, i === 0)));
  queueList.replaceChildren(...rows);
  updateQueueMore();
  // The playing song near the top with the two rows before it in view (as
  // Roon's queue keeps the skipped block above "Now playing"), the older
  // ones above to scroll up to; once per song, so looking back isn't undone
  const key = `${zone?.zoneId}|${payload.items[0]?.queueItemId ?? ""}|${queueHistory.length}`;
  if (nowLabel && key !== scrolledFor) {
    scrolledFor = key;
    const scroller = queuePanel.querySelector(".queue-scroll");
    let top = nowLabel;
    for (let n = 0; n < 2 && top.previousElementSibling; n += 1) top = top.previousElementSibling;
    scroller.scrollTop += top.getBoundingClientRect().top - scroller.getBoundingClientRect().top - 4;
  }
  if (!fromHistory) loadQueueHistory();
  // (the next song tells a list's song played from its list)
  if (ratingReady) renderRating();
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
  if (!open) hideQueueSave();
  else {
    // What's already fetched shows (and scrolls into place) right away;
    // a fresh copy follows
    scrolledFor = "";
    if (queue) renderQueue(queue, true);
    loadQueueHistory(true);
  }
  reportHole();
}

queueBtn.addEventListener("click", () => setQueueOpen(queuePanel.hidden));
$("#queue-close").addEventListener("click", () => setQueueOpen(false));

// A date for playlist names ("Oct 17, 1:55 AM")
function playlistStamp() {
  return new Date().toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
}

// The whole queue (played songs too) as a Roon playlist (library.rs),
// under a name asked for first; left empty, the dated default
const queueSave = $("#queue-save");
const queueSaveForm = $("#queue-save-form");
const queueSaveName = $("#queue-save-name");
function hideQueueSave() {
  queueSaveForm.hidden = true;
}
queueSave.addEventListener("click", () => {
  if (!zone) return wrapperToast("Pick a zone first.");
  queueSaveName.value = "";
  queueSaveName.placeholder = `${WRAPPED_PREFIX}Queue, ${playlistStamp()}`;
  queueSaveForm.hidden = false;
  queueSaveName.focus();
});
$("#queue-save-cancel").addEventListener("click", hideQueueSave);
queueSaveName.addEventListener("keydown", (event) => {
  if (event.key === "Escape") {
    event.stopPropagation();
    hideQueueSave();
  }
});
queueSaveForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  if (!zone) return;
  const name = queueSaveName.value.trim() || queueSaveName.placeholder;
  hideQueueSave();
  queueSave.disabled = true;
  queueSave.textContent = "Saving...";
  try {
    const made = await invoke("library_save_queue", { zoneId: zone.zoneId, name, start: await queueStart() });
    wrapperToast(`Saved "${made.name}"${made.tracks != null ? ` (${made.tracks} tracks)` : ""}`);
  } catch (err) {
    console.error("library_save_queue", err);
    wrapperToast(typeof err === "string" ? err : "Roon didn't save the queue.");
  } finally {
    queueSave.disabled = false;
    queueSave.textContent = "Save as playlist";
  }
});

// Roon's own queue instead of ours: our panel and Now Playing step aside
// (Roon's player closes along with ours first), then Ctrl+E goes to Roon
$("#queue-roon").addEventListener("click", async () => {
  setQueueOpen(false);
  await closeStage();
  const shown = await invoke("wrapper_roon_queue").catch(() => false);
  if (!shown) wrapperToast("Roon couldn't be brought to the front for its queue.");
});

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
// Escape: the zone menu first, then the Wrapped menu, the queue panel, Now
// Playing's editing, Now Playing itself, then the Wrapped page
document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape") return;
  if (dialogOpen()) cancelDialog();
  else if (!zoneMenu.hidden) closeZoneMenu();
  else if (!wrappedSide.hidden) setWrappedSide(false);
  else if (!queueSaveForm.hidden) hideQueueSave();
  else if (!queuePanel.hidden) setQueueOpen(false);
  else if (editing) finishEditing();
  else if (!stage.hidden) closeStage();
  else if (!wrapped.hidden) setWrappedOpen(false);
});

// ===== Wrapped =============================================================

// The Wrapped button sits over Roon's logo, at the top-left of Roon's
// sidebar (Roon's logo is drawn at a fixed spot in its sidebar, so the
// button lands on it at any window size), and opens the Wrapped menu. The
// Wrapped page covers Roon's content area, from the sidebar's right edge to
// the window's edge, between the title bar and the player bar. Both are
// covers (see reportHole), so Roon shows and gets clicks everywhere else.
//
// Where Roon's sidebar ends comes from skin.rs (it measures it to lay its
// color filter over it). That filter also lies over our button, since the
// button is in the sidebar part of the hole, and recolors whatever is under
// it, so the button is drawn in Roon's own sidebar colors (its grey, its
// text grey, its purple) and comes out in ours, like the rest of the
// sidebar. With the sidebar collapsed there's no filter and the button is
// drawn in our colors, right of Roon's hamburger.

// Roon's logo spot and its collapsed-sidebar hamburger, in Roon's own
// pixels (CSS px at 100%) from the top-left of its window's contents
const ROON_LOGO = { x: 14, y: 16, width: 100, height: 36 };
// Roon's content header (the row with the back and forward arrows): where a
// button fits after the forward arrow, and, with the sidebar collapsed,
// after the hamburger (Home has no arrows then) or after all of them
const ROON_HEADER = { y: 12, height: 38, afterBack: 124, afterHamburger: 56, afterBoth: 172 };
// Roon's sidebar width while skin.rs hasn't measured it yet (Roon collapses
// it by itself in windows narrower than about 1400)
const ROON_SIDEBAR = 248;
const ROON_COLLAPSES_BELOW = 1400;
// Roon's text grey and purple in its sidebar (skin.rs maps these to ours)
const ROON_TEXT = "rgb(168, 168, 168)";
const ROON_BRIGHT = "rgb(230, 230, 230)";
const ROON_ACCENT = "rgb(106, 110, 217)";

// Rust's status (once a second): Roon's sidebar width and grey. The button
// shows while Roon is in the Wrapper. A sidebar that was there and isn't
// for a moment is usually Roon repainting (a resize, a page change), so
// "collapsed" only counts once it has lasted a second.
// During a resize Roon repaints at its new size and the measurement finds
// nothing for as long as the resize goes on, so "collapsed" also waits
// until a moment after the last resize.
const SIDEBAR_GONE_AFTER = 1500;
let sidebarGoneTimer = null;
let lastResizeAt = 0;
let roonHere = false;
window.addEventListener("resize", () => {
  lastResizeAt = Date.now();
});
function setSidebar(px, color, here, header) {
  if (here !== roonHere) {
    roonHere = here;
    layoutWrapped();
  }
  if (String(header) !== String(headerColor)) {
    headerColor = header ?? null;
    layoutWrapped();
  }
  if (px === null && typeof sidebarPx === "number") {
    if (!sidebarGoneTimer) sidebarGoneTimer = setTimeout(sidebarGone, SIDEBAR_GONE_AFTER);
    return;
  }
  if (sidebarGoneTimer) {
    clearTimeout(sidebarGoneTimer);
    sidebarGoneTimer = null;
  }
  applySidebar(px, color);
}

function sidebarGone() {
  const sinceResize = Date.now() - lastResizeAt;
  if (sinceResize < SIDEBAR_GONE_AFTER) {
    sidebarGoneTimer = setTimeout(sidebarGone, SIDEBAR_GONE_AFTER - sinceResize);
    return;
  }
  sidebarGoneTimer = null;
  applySidebar(null, null);
}

function applySidebar(px, color) {
  const changed = px !== sidebarPx || String(color) !== String(sidebarColor);
  sidebarPx = px;
  sidebarColor = color ?? null;
  if (changed) layoutWrapped();
}

// Roon's sidebar width in CSS px: measured, or guessed from the window
// width until it is; null while collapsed
function sidebarWidth() {
  if (sidebarPx === undefined) return null;
  if (sidebarPx === null) {
    const holeWidth = hole.getBoundingClientRect().width;
    return sidebarColor === null && holeWidth >= ROON_COLLAPSES_BELOW && !sidebarSeen ? ROON_SIDEBAR : null;
  }
  return sidebarPx / (window.devicePixelRatio || 1);
}

function layoutWrapped() {
  const h = hole.getBoundingClientRect();
  if (sidebarPx !== null && sidebarPx !== undefined) sidebarSeen = true;
  const width = sidebarWidth();
  const collapsed = width === null;
  const filtered = !collapsed && sidebarColor !== null;
  const here = wrappedHere();

  // The sidebar button: over the logo while the sidebar is there (it goes
  // with the sidebar when Roon collapses it); neither button while Now
  // Playing covers Roon
  const overRoon = roonHere && stage.hidden;
  wrappedBtn.hidden = !overRoon || collapsed;
  wrappedBtn.style.left = `${h.left + ROON_LOGO.x}px`;
  wrappedBtn.style.top = `${h.top + ROON_LOGO.y}px`;
  wrappedBtn.style.height = `${ROON_LOGO.height}px`;
  wrappedBtn.style.minWidth = `${ROON_LOGO.width}px`;
  // In Roon's colors under the filter (see above), ours otherwise
  wrappedBtn.classList.toggle("through-filter", filtered);
  if (filtered) {
    const [r, g, b] = sidebarColor;
    wrappedBtn.style.setProperty("--roon-bg", `rgb(${r}, ${g}, ${b})`);
    wrappedBtn.style.setProperty("--roon-hover", `rgb(${r + 14}, ${g + 14}, ${b + 14})`);
    wrappedBtn.style.setProperty("--roon-text", ROON_TEXT);
    wrappedBtn.style.setProperty("--roon-bright", ROON_BRIGHT);
    wrappedBtn.style.setProperty("--roon-accent", ROON_ACCENT);
  }

  // The content-header button: on Roon's album and artist pages it opens
  // that page in Wrapped; with the sidebar collapsed it stands in for the
  // sidebar button on every page. It sits after Roon's back arrow (and
  // hamburger), in the header row that doesn't scroll.
  const contentLeft = h.left + (collapsed ? 0 : width);
  // Not while the Wrapped page is open (it would sit over the page's own
  // header, and "open this album" is moot then)
  const showPage = overRoon && wrapped.hidden && (collapsed || here !== null);
  wrappedPageBtn.hidden = !showPage;
  wrappedPageOpen.textContent = here ? (here.find ? "Find artist in Wrapped" : `Open ${here.page} in Wrapped`) : "Wrapped";
  const offset = !collapsed ? ROON_HEADER.afterBack : roonNav.page === "home" || !roonNav.page ? ROON_HEADER.afterHamburger : ROON_HEADER.afterBoth;
  wrappedPageBtn.style.left = `${contentLeft + offset}px`;
  wrappedPageBtn.style.top = `${h.top + ROON_HEADER.y}px`;
  wrappedPageBtn.style.height = `${ROON_HEADER.height}px`;
  // Roon's own background around the rounded corners (the window's cut-out
  // for the button is square; painted in Roon's color, the corners outside
  // the rounded face can't be told from Roon)
  wrappedPageBtn.style.setProperty("--surround", headerColor ? `rgb(${headerColor.join(", ")})` : "transparent");

  // (with the sidebar collapsed, the page makes room for the side menu)
  wrapped.style.left = `${contentLeft + (collapsed && !wrappedSide.hidden ? ROON_SIDEBAR : 0)}px`;
  wrappedBtn.classList.toggle("page-on", !wrapped.hidden);
  placeWrappedSide();
  // A move alone doesn't reach the hole's ResizeObserver (same size, new
  // place), so the covers are reported here
  reportHole();
}
new ResizeObserver(layoutWrapped).observe(hole);

// ----- The menu -----

// Deep Dive's mark: a brain with a magnifying glass in front of it. Each
// copy gets its own clip id (a page shows several)
const DIVE_SVG = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><defs><clipPath id="dive-cut"><path clip-rule="evenodd" d="M-2 -2h28v28H-2z M16.5 10.2a6.3 6.3 0 1 0 0 12.6a6.3 6.3 0 1 0 0-12.6z"/></clipPath></defs><g clip-path="url(#dive-cut)"><g transform="scale(0.75)" stroke-width="2.67"><path d="M12 5a3 3 0 1 0-5.997.125 4 4 0 0 0-2.526 5.77 4 4 0 0 0 .556 6.588A4 4 0 1 0 12 18Z"/><path d="M12 5a3 3 0 1 1 5.997.125 4 4 0 0 1 2.526 5.77 4 4 0 0 1-.556 6.588A4 4 0 1 1 12 18Z"/><path d="M15 13a4.5 4.5 0 0 1-3-4 4.5 4.5 0 0 1-3 4"/><path d="M17.599 6.5a3 3 0 0 0 .399-1.375"/><path d="M6.003 5.125A3 3 0 0 0 6.401 6.5"/><path d="M3.477 10.896a4 4 0 0 1 .585-.396"/><path d="M19.938 10.5a4 4 0 0 1 .585.396"/><path d="M6 18a4 4 0 0 1-1.967-.516"/><path d="M19.967 17.484A4 4 0 0 1 18 18"/></g></g><circle cx="16.5" cy="16.5" r="4.3"/><path d="m22 22-2.4-2.4"/></svg>';
let diveIconN = 0;
function diveIcon() {
  return DIVE_SVG.replace(/dive-cut/g, `dive-cut-${++diveIconN}`);
}

const WRAPPED_ICONS = {
  // (Lucide "clock-plus" and "tag")
  listenlater: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 6v6l3.644 1.822"/><path d="M16 19h6"/><path d="M19 16v6"/><path d="M21.92 13.267a10 10 0 1 0-8.653 8.653"/></svg>',
  tag: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12.586 2.586A2 2 0 0 0 11.172 2H4a2 2 0 0 0-2 2v7.172a2 2 0 0 0 .586 1.414l8.704 8.704a2.426 2.426 0 0 0 3.42 0l6.58-6.58a2.426 2.426 0 0 0 0-3.42z"/><circle cx="7.5" cy="7.5" r=".5" fill="currentColor"/></svg>',
  // (Lucide "tags")
  genres: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m15 5 6.3 6.3a2.4 2.4 0 0 1 0 3.4L17 19"/><path d="M9.586 5.586A2 2 0 0 0 8.172 5H3a1 1 0 0 0-1 1v5.172a2 2 0 0 0 .586 1.414L8.29 18.29a2.426 2.426 0 0 0 3.42 0l3.58-3.58a2.426 2.426 0 0 0 0-3.42z"/><circle cx="6.5" cy="9.5" r=".5" fill="currentColor"/></svg>',
  track: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/></svg>',
  get deepdive() {
    return diveIcon();
  },
  // (Lucide "sparkles")
  discover: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z"/><path d="M20 3v4"/><path d="M22 5h-4"/><path d="M4 17v2"/><path d="M5 18H3"/></svg>',
  radio: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4.9 19.1C1 15.2 1 8.8 4.9 4.9"/><path d="M7.8 16.2c-2.3-2.3-2.3-6.1 0-8.5"/><circle cx="12" cy="12" r="2"/><path d="M16.2 7.8c2.3 2.3 2.3 6.1 0 8.5"/><path d="M19.1 4.9C23 8.8 23 15.2 19.1 19.1"/></svg>',
  home: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 21v-8a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v8"/><path d="M3 10a2 2 0 0 1 .709-1.528l7-5.999a2 2 0 0 1 2.582 0l7 5.999A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/></svg>',
  album: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><circle cx="12" cy="12" r="3"/></svg>',
  artist: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"/><circle cx="12" cy="7" r="4"/></svg>',
  search: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/></svg>',
  playlists: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15V6"/><path d="M18.5 18a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5Z"/><path d="M12 12H3"/><path d="M16 6H3"/><path d="M12 18H3"/></svg>',
  settings: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="21" x2="14" y1="4" y2="4"/><line x1="10" x2="3" y1="4" y2="4"/><line x1="21" x2="12" y1="12" y2="12"/><line x1="8" x2="3" y1="12" y2="12"/><line x1="21" x2="16" y1="20" y2="20"/><line x1="12" x2="3" y1="20" y2="20"/><line x1="14" x2="14" y1="2" y2="6"/><line x1="8" x2="8" y1="10" y2="14"/><line x1="16" x2="16" y1="18" y2="22"/></svg>',
  history: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/><path d="M12 7v5l4 2"/></svg>',
  close: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></svg>',
};

// What Roon's current page would open in Wrapped: the album Roon shows,
// or (on an artist page) the artist of the last album it showed
// Roon's page title read off the screen, searched; an empty search when
// nothing could be read
async function wrappedFindOnScreen() {
  let read = null;
  try {
    read = await invoke("wrapper_read_page_name");
  } catch (err) {
    console.warn("read page name", err);
  }
  const name = read?.name || "";
  if (!name) wrappedToast("Couldn't read the artist's name off Roon's page; search for it here.");
  if (name) {
    // An exact hit goes straight to the artist; otherwise the results
    try {
      const hits = await invoke("library_search", { query: name });
      const hit = hits.artists.find((a) => a.name.toLowerCase() === name.toLowerCase());
      if (hit) return openWrappedPage({ page: "artist", handle: hit.handle });
    } catch (err) {
      console.warn("find on screen", err);
    }
  }
  openWrappedPage({ page: "search", query: name });
}

function wrappedHere() {
  if (roonNav.page === "album" && roonNav.albumId != null) {
    return { page: "album", albumId: roonNav.albumId, label: "This album", icon: "album" };
  }
  // An artist page: known when Roon reached it from an album (roonnav.rs
  // keeps that album); otherwise the log names nobody, so Wrapped offers
  // its search
  if (roonNav.page === "artist") {
    if (roonNav.albumId != null) return { page: "artist", albumId: roonNav.albumId, label: "This artist", icon: "artist" };
    // Otherwise by the banner Roon fetched for the page (a library
    // artist's, or a performer seen this session); the artist page resolves
    // it, and falls back to reading the name off Roon's screen
    if (roonNav.imageKeys?.length) return { page: "artist", imageKeys: roonNav.imageKeys.slice(), label: "This artist", icon: "artist" };
    return { page: "search", label: "Find this artist", icon: "search", find: true };
  }
  return null;
}

// ----- The side menu -----
// Wrapped's own version of Roon's sidebar: its pages as real buttons (so
// it's told from Roon's at a glance), dropped down from the Wrapped
// button's arrow and hidden by it again; independent of the page (it
// opens over Roon's sidebar with the page closed, beside the page with it
// open)

function wrappedSideGroups() {
  const browse = [{ page: "home", label: "Home", icon: "home" }];
  const here = wrappedHere();
  if (here) browse.push(here);
  if (wrappedSettings?.discovery !== false) browse.push({ page: "discover", label: "Discover", icon: "discover" });
  // (with tier 2, Radio lives inside Deep Dive)
  browse.push(tier2Ready() ? { page: "deepdive", label: "Deep Dive", icon: "deepdive" } : { page: "radio", label: "Radio", icon: "radio" });
  browse.push({ page: "history", label: "History", icon: "history" });
  browse.push({ page: "settings", label: "Settings", icon: "settings" });
  const library = [
    { page: "genres", label: "Genres", icon: "genres" },
    { page: "artists", label: "Artists", icon: "artist" },
    { page: "albums", label: "Albums", icon: "album" },
    { page: "tracks", label: "Tracks", icon: "track" },
    { page: "playlists", label: "Playlists", icon: "playlists" },
    { page: "listenlater", label: "Listen Later", icon: "listenlater" },
    { page: "tags", label: "Tags", icon: "tag" },
  ];
  return [
    ["Wrapped Features", browse],
    ["Your library", library],
  ];
}

function fillWrappedSide() {
  wrappedSide.replaceChildren();
  const current = wrapped.hidden ? null : wrappedStack.at(-1)?.page;
  for (const [caption, items] of wrappedSideGroups()) {
    const head = document.createElement("div");
    head.className = "wrapped-side-caption";
    head.textContent = caption;
    wrappedSide.append(head);
    for (const item of items) {
      const option = document.createElement("button");
      option.className = `wrapped-side-btn${item.page === current ? " selected" : ""}`;
      option.insertAdjacentHTML("beforeend", WRAPPED_ICONS[item.icon]);
      const label = document.createElement("span");
      label.textContent = item.label;
      option.append(label);
      option.addEventListener("click", () => {
        if (item.find) wrappedFindOnScreen();
        else openWrappedPage(item);
      });
      wrappedSide.append(option);
    }
  }
  // Roon's playlists, like Roon's sidebar lists them (Wrapped's own first,
  // as the Playlists page does); one plays on click
  const lists = document.createElement("div");
  lists.className = "wrapped-side-lists";
  wrappedSide.append(lists);
  if (libraryReady) {
    const fill = ++wrappedSideFill;
    invoke("library_playlists")
      .then((list) => {
        if (fill !== wrappedSideFill || !list.length) return;
        const head = document.createElement("div");
        head.className = "wrapped-side-caption";
        head.textContent = "Playlists";
        lists.append(head);
        const sorted = [...list.filter(isWrappedPlaylist), ...list.filter((p) => !isWrappedPlaylist(p))];
        for (const p of sorted) {
          const option = document.createElement("button");
          option.className = "wrapped-side-btn wrapped-side-list";
          option.insertAdjacentHTML("beforeend", WRAPPED_ICONS.playlists);
          const label = document.createElement("span");
          label.textContent = p.name;
          option.append(label);
          option.title = p.name;
          if (current === "playlist" && wrappedStack.at(-1)?.handle === p.handle) option.classList.add("selected");
          option.addEventListener("click", () => openWrappedPage({ page: "playlist", handle: p.handle, name: p.name }));
          lists.append(option);
        }
      })
      .catch(() => {});
  }
  if (!wrapped.hidden) {
    const close = document.createElement("button");
    close.className = "wrapped-side-btn wrapped-side-close";
    close.insertAdjacentHTML("beforeend", WRAPPED_ICONS.close);
    const label = document.createElement("span");
    label.textContent = "Close Wrapped";
    close.append(label);
    close.addEventListener("click", () => setWrappedOpen(false));
    wrappedSide.append(close);
  }
}
let wrappedSideFill = 0; // (a fill's playlists are dropped when the menu was filled again meanwhile)

// Over Roon's sidebar, under the Wrapped button (as wide as the sidebar).
// With the sidebar collapsed: under Roon's header row while the page is
// closed, and beside the page (which moves over for it) while it's open
function placeWrappedSide() {
  if (wrappedSide.hidden) return;
  const h = hole.getBoundingClientRect();
  const width = sidebarWidth();
  const collapsed = width === null;
  wrappedSide.style.left = `${h.left}px`;
  wrappedSide.style.width = `${collapsed ? ROON_SIDEBAR : width}px`;
  // Over the sidebar it shows through skin.rs's color filter, like the
  // Wrapped button: drawn in Roon's own colors so the filter turns them
  // into ours (our colors would come out shifted)
  const filtered = !collapsed && sidebarColor !== null;
  wrappedSide.classList.toggle("through-filter", filtered);
  if (filtered) {
    const [r, g, b] = sidebarColor;
    wrappedSide.style.setProperty("--roon-bg", `rgb(${r}, ${g}, ${b})`);
    wrappedSide.style.setProperty("--roon-card", `rgb(${r + 10}, ${g + 10}, ${b + 10})`);
    wrappedSide.style.setProperty("--roon-hover", `rgb(${r + 22}, ${g + 22}, ${b + 22})`);
    wrappedSide.style.setProperty("--roon-border", `rgb(${r + 30}, ${g + 30}, ${b + 30})`);
    wrappedSide.style.setProperty("--roon-text", ROON_TEXT);
    wrappedSide.style.setProperty("--roon-bright", ROON_BRIGHT);
    wrappedSide.style.setProperty("--roon-accent", ROON_ACCENT);
  }
  const top = !collapsed ? ROON_LOGO.y + ROON_LOGO.height + 6 : wrapped.hidden ? ROON_HEADER.y + ROON_HEADER.height + 6 : 0;
  wrappedSide.style.top = `${h.top + top}px`;
}

function setWrappedSide(open) {
  if (open) fillWrappedSide();
  wrappedSide.hidden = !open;
  wrappedBtn.classList.toggle("on", open);
  wrappedPageBtn.classList.toggle("on", open);
  wrappedSideBtn.classList.toggle("on", open);
  layoutWrapped();
}

function toggleWrappedSide() {
  setWrappedSide(wrappedSide.hidden);
}

// A split button: the name opens the Wrapped page (and closes it again),
// the arrow drops the side menu down (and hides it again); a right-click
// anywhere on it does the same
wrappedOpen.addEventListener("click", () => setWrappedOpen(wrapped.hidden));
wrappedArrow.addEventListener("click", toggleWrappedSide);
wrappedBtn.addEventListener("contextmenu", (event) => {
  event.preventDefault();
  toggleWrappedSide();
});
// The content-header button: the name opens Roon's current album or artist
// in Wrapped (or the page itself when Roon is elsewhere), the arrow the
// side menu
wrappedPageOpen.addEventListener("click", () => {
  const here = wrappedHere();
  if (here?.find) wrappedFindOnScreen();
  else if (here) openWrappedPage(here);
  else setWrappedOpen(wrapped.hidden);
});
wrappedPageArrow.addEventListener("click", toggleWrappedSide);
wrappedPageBtn.addEventListener("contextmenu", (event) => {
  event.preventDefault();
  toggleWrappedSide();
});
// The page's own menu button (for when Roon's sidebar, and the button over
// it, are out of the picture)
wrappedSideBtn.addEventListener("click", toggleWrappedSide);

// ----- The page -----

// Closing keeps the page stack: "Wrapped" opens it where it was
function setWrappedOpen(open) {
  if (open && !wrappedStack.length) wrappedStack = [{ page: "home" }];
  wrapped.hidden = !open;
  if (open) renderWrapped();
  else discoverBtn.classList.remove("on");
  layoutWrapped();
  reportHole();
}

// Opens a page on top of the stack (back goes to the one before)
function openWrappedPage(entry) {
  const top = wrappedStack.at(-1);
  const same =
    top &&
    top.page === entry.page &&
    top.albumId === entry.albumId &&
    top.handle === entry.handle &&
    top.name === entry.name &&
    top.performerId === entry.performerId &&
    top.album === entry.album &&
    (entry.imageKeys == null || top.imageKeys?.join() === entry.imageKeys.join());
  if (same) {
    if (wrapped.hidden) setWrappedOpen(true);
    return;
  }
  // Home is single: going there again goes back to it
  if (entry.page === "home") wrappedStack = [];
  // (Deep Dive always opens on Deep Dive; Radio is the other tab)
  if (entry.page === "deepdive") deepDiveTab = "dive";
  wrappedForward = [];
  wrappedStack.push({
    page: entry.page,
    albumId: entry.albumId,
    performerId: entry.performerId,
    handle: entry.handle,
    query: entry.query,
    name: entry.name,
    album: entry.album,
    albumHandle: entry.albumHandle,
    imageKeys: entry.imageKeys,
  });
  wrapped.hidden = false;
  renderWrapped();
  layoutWrapped();
  reportHole();
}

// Back: the page before; from the first page, back to Roon (the pages
// stay, so Forward or the Wrapped button opens it where it was)
function wrappedBackPage() {
  if (wrapped.hidden) return;
  if (wrappedStack.length <= 1) return setWrappedOpen(false);
  wrappedForward.push(wrappedStack.pop());
  renderWrapped();
}

function wrappedForwardPage() {
  if (wrapped.hidden) {
    if (wrappedStack.length) setWrappedOpen(true);
    return;
  }
  const next = wrappedForward.pop();
  if (!next) return;
  wrappedStack.push(next);
  renderWrapped();
}

// The mouse's back and forward buttons (the webview would otherwise try
// to navigate the page itself)
for (const type of ["mousedown", "mouseup", "auxclick"]) {
  document.addEventListener(type, (event) => {
    if (event.button !== 3 && event.button !== 4) return;
    event.preventDefault();
    if (type !== "mouseup") return;
    if (event.button === 3) wrappedBackPage();
    else wrappedForwardPage();
  });
}

function renderWrapped() {
  discoverBtn.classList.toggle("on", !wrapped.hidden && wrappedStack.at(-1)?.page === "discover");
  const entry = wrappedStack.at(-1);
  if (!entry) return;
  wrappedBack.hidden = wrappedStack.length <= 1;
  fillWrappedNav();
  if (!wrappedSide.hidden) fillWrappedSide();
  // (the bar shows the search this page is, else nothing)
  if (document.activeElement !== wrappedBarInput) wrappedBarInput.value = entry.page === "search" ? entry.query || "" : "";
  wrappedTrackRows = [];
  wrappedAlbumTracks = [];
  wrappedBody.replaceChildren();
  wrappedBody.scrollTop = 0;
  wrappedTicket++;
  const page = WRAPPED_PAGES[entry.page] || WRAPPED_PAGES.home;
  Promise.resolve(page(entry)).catch((err) => console.error("wrapped page", err));
}

function wrappedHeading(title, sub) {
  wrappedTitle.textContent = title;
  wrappedSub.textContent = sub || "";
  wrappedSub.hidden = !sub;
  // (the whole line on hover, when the bar cuts it short)
  wrappedTitle.title = title || "";
  wrappedSub.title = sub || "";
}

function wrappedNote(text, className = "wrapped-note") {
  const p = document.createElement("p");
  p.className = className;
  p.textContent = text;
  return p;
}

function wrappedTile(label, text, onClick) {
  const tile = document.createElement("button");
  tile.className = "wrapped-tile";
  const head = document.createElement("div");
  head.className = "wrapped-tile-title";
  head.textContent = label;
  const body = document.createElement("div");
  body.className = "wrapped-tile-text";
  body.textContent = text;
  tile.append(head, body);
  if (onClick) tile.addEventListener("click", onClick);
  else tile.disabled = true;
  return tile;
}

// ----- Pieces the pages are built from -----

const HEART_ICONS =
  '<svg class="icon-add" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14"/><path d="M12 5v14"/></svg>' +
  '<svg class="icon-heart" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M2 9.5a5.5 5.5 0 0 1 9.591-3.676.56.56 0 0 0 .818 0A5.49 5.49 0 0 1 22 9.5c0 2.29-1.5 4-3 5.5l-5.492 5.313a2 2 0 0 1-3 .019L5 15c-1.5-1.5-3-3.2-3-5.5"/></svg>';
const WRAPPED_PLAY_ICON = '<svg viewBox="0 0 24 24" fill="currentColor" stroke="none"><path d="M6 4.5v15a1 1 0 0 0 1.5.87l13-7.5a1 1 0 0 0 0-1.74l-13-7.5A1 1 0 0 0 6 4.5z"/></svg>';
// The Wrapped radio mark (a broadcast icon): on every track, album and artist
const RADIO_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4.9 19.1C1 15.2 1 8.8 4.9 4.9"/><path d="M7.8 16.2c-2.3-2.3-2.3-6.1 0-8.5"/><circle cx="12" cy="12" r="2"/><path d="M16.2 7.8c2.3 2.3 2.3 6.1 0 8.5"/><path d="M19.1 4.9C23 8.8 23 15.2 19.1 19.1"/></svg>';

// Art from the Core's image service (the roonimg scheme, see zones.rs),
// or initials in a colored square while there's none
function wrappedArt(imageKey, size, name, round) {
  const box = document.createElement("div");
  box.className = `wrapped-art${round ? " round" : ""}`;
  if (imageKey) {
    const img = document.createElement("img");
    img.loading = "lazy";
    img.alt = "";
    // (A broker key: the Library link's, not the official API's; zones.rs)
    img.src = `${convertFileSrc(imageKey, "roonimg")}?width=${size}&height=${size}&src=broker`;
    img.addEventListener("error", () => img.replaceWith(wrappedInitials(name)));
    box.append(img);
  } else {
    box.append(wrappedInitials(name));
  }
  return box;
}

function wrappedInitials(name) {
  const el = document.createElement("div");
  el.className = "wrapped-initials";
  // (each word's first letter or digit: an emoji or a sign in front is
  // skipped, not cut in half)
  const firsts = (name || "")
    .split(/\s+/)
    .map((w) => w.match(/[\p{L}\p{N}]/u)?.[0])
    .filter(Boolean);
  el.textContent = firsts.slice(0, 2).map((c) => c.toUpperCase()).join("") || "♪";
  return el;
}

// Heart / add-to-library button in the player bar's style: "+" while the
// thing isn't in the library, a heart once it is (filled when hearted).
// `change(favorite)` returns the new state (or throws).
function wrappedHeart(state, change) {
  const btn = document.createElement("button");
  btn.className = "icon-btn library";
  btn.innerHTML = HEART_ICONS;
  const paint = (st) => {
    btn.classList.toggle("in-library", !!st.inLibrary);
    btn.classList.toggle("on", !!st.favorite);
    btn.title = !st.inLibrary ? "Add to library" : st.favorite ? "Remove heart" : "Heart";
  };
  paint(state);
  btn.addEventListener("click", async (event) => {
    event.stopPropagation();
    btn.classList.add("busy");
    try {
      const next = await change(!(state.inLibrary && state.favorite));
      if (next) {
        state = next;
        paint(state);
      }
    } catch (err) {
      console.error("heart", err);
    } finally {
      btn.classList.remove("busy");
    }
  });
  return btn;
}

function wrappedIconButton(icon, title, onClick, className = "") {
  const btn = document.createElement("button");
  btn.className = `icon-btn ${className}`.trim();
  btn.innerHTML = icon;
  btn.title = title;
  if (onClick) {
    btn.addEventListener("click", (event) => {
      event.stopPropagation();
      onClick();
    });
  } else {
    btn.disabled = true;
  }
  return btn;
}

// The radio button: Roon Radio on the zone, started from this song, album
// or artist (`seeds`: { tracks, albums, artists, genres, libraryOnly }, handles)
function wrappedRadio(what, seeds) {
  const btn = wrappedIconButton(RADIO_ICON, `Wrapped radio from this ${what}`, () => startRoonRadio(what, seeds, btn), "wrapped-radio");
  return btn;
}

async function startRoonRadio(what, seeds, btn) {
  if (!zone) return wrappedToast("Pick a zone first.");
  btn.classList.add("busy");
  try {
    await invoke("library_play_radio", { zoneId: zone.zoneId, seeds });
    wrappedToast(`Radio from this ${what} on ${zone.name}`);
  } catch (err) {
    console.error("library_play_radio", err);
    wrappedToast(typeof err === "string" ? err : "Roon didn't start the radio.");
  } finally {
    btn.classList.remove("busy");
  }
}

// ----- Deep Dive (with tier 2; Radio without it) -----
// The picks it works from (songs, albums, artists; up to DIVE_MAX), what's
// typed, which tab is open and which pick the Radio tab starts from
const DIVE_MAX = 5;
let diveSeeds = [];
let diveText = "";
// Where the songs come from: "new", "library" or "both" (the toggle under
// the words; remembered)
let diveSource = "both";
try {
  diveSource = ["new", "library", "both"].includes(localStorage.getItem("wrapped-dive-source")) ? localStorage.getItem("wrapped-dive-source") : "both";
} catch {}
let deepDiveTab = "dive";
let diveRadioAt = -1;

// Tier 2 set up: a key, or Ollama with a model
function tier2Ready() {
  const s = wrappedSettings;
  if (!s) return false;
  if ((s.llmProvider || "anthropic") === "ollama") return ollamaState ? !!ollamaState.modelReady : !!s.llmModel;
  return !!s.llmKey;
}

// Where the radio button was: with tier 2, Deep Dive's button (adds this
// song, album or artist to the picks and opens Deep Dive); without it,
// the radio button as it was
function wrappedDive(seed, radio) {
  if (!tier2Ready()) return radio();
  const what = seed.kind === "song" ? "song" : seed.kind;
  return wrappedIconButton(diveIcon(), `Deep Dive from this ${what}`, () => divePickAdd(seed), "wrapped-radio wrapped-dive");
}

// Adds a pick to Deep Dive and opens it
function divePickAdd(seed) {
  const key = (s) => `${s.kind}|${s.title}|${s.artist || ""}`.toLowerCase();
  if (!diveSeeds.some((s) => key(s) === key(seed))) {
    if (diveSeeds.length >= DIVE_MAX) wrappedToast(`Deep Dive takes up to ${DIVE_MAX} picks: remove one first.`);
    else diveSeeds.push(seed);
  }
  diveRadioAt = -1;
  openWrappedPage({ page: "deepdive" });
}

// The play choices for a song or an album (Roon's: Play Now, Add Next,
// Queue, Start Radio), with Deep Dive: a menu by `anchor`.
// `what`: "song" or "album"; `handle` its Roon handle; `seed` the Deep
// Dive pick (null for none); `radio` the radio seeds.
const MENU_ICONS = {
  play: WRAPPED_PLAY_ICON,
  next: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 14l5-5-5-5"/><path d="M4 20v-7a4 4 0 0 1 4-4h12"/></svg>',
  queue: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h13"/><path d="M3 12h13"/><path d="M3 18h9"/><path d="m16 16 3 3 3-3"/><path d="M19 10v9"/></svg>',
};
function wrappedPlayMenu(anchor, { what, handle, seed, radio }) {
  closePlayMenu();
  const menu = document.createElement("div");
  menu.className = "wrapped-popmenu";
  const item = (icon, label, run) => {
    const btn = document.createElement("button");
    btn.className = "wrapped-popmenu-item";
    btn.insertAdjacentHTML("beforeend", icon);
    const text = document.createElement("span");
    text.textContent = label;
    btn.append(text);
    btn.addEventListener("click", (event) => {
      event.stopPropagation();
      closePlayMenu();
      run();
    });
    menu.append(btn);
  };
  const play = (how) => {
    if (!zone) return wrappedToast("Pick a zone first.");
    const cmd = what === "album" ? "library_play_album" : "library_play_track";
    invoke(cmd, { zoneId: zone.zoneId, handle, how }).catch((err) => wrappedToast(String(err)));
  };
  item(MENU_ICONS.play, "Play Now", () => play("now"));
  item(MENU_ICONS.next, "Add Next", () => play("next"));
  item(MENU_ICONS.queue, "Queue", () => play("queue"));
  if (radio) item(RADIO_ICON, "Start Radio", () => startRoonRadio(what, radio, anchor));
  if (seed && tier2Ready()) item(diveIcon(), "Deep Dive", () => divePickAdd(seed));
  wrapped.append(menu);
  // (under the anchor, kept inside the page)
  const a = anchor.getBoundingClientRect();
  const w = wrapped.getBoundingClientRect();
  const m = menu.getBoundingClientRect();
  let left = a.left - w.left;
  let top = a.bottom - w.top + 4;
  if (left + m.width > w.width - 8) left = Math.max(8, w.width - 8 - m.width);
  if (top + m.height > w.height - 8) top = Math.max(8, a.top - w.top - m.height - 4);
  menu.style.left = `${left}px`;
  menu.style.top = `${top}px`;
  menu.querySelector("button")?.focus({ preventScroll: true });
}

function closePlayMenu() {
  wrapped.querySelector(".wrapped-popmenu")?.remove();
}
document.addEventListener("pointerdown", (event) => {
  const menu = wrapped.querySelector(".wrapped-popmenu");
  if (menu && !menu.contains(event.target)) closePlayMenu();
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") closePlayMenu();
});

// A line at the bottom of the page for a few seconds
let wrappedToastTimer = null;
function wrappedToast(text) {
  let toast = wrapped.querySelector(".wrapped-toast");
  if (!toast) {
    toast = document.createElement("div");
    toast.className = "wrapped-toast";
    wrapped.append(toast);
  }
  toast.textContent = text;
  toast.classList.add("show");
  clearTimeout(wrappedToastTimer);
  wrappedToastTimer = setTimeout(() => toast.classList.remove("show"), 3000);
}

function wrappedSection(title) {
  const h = document.createElement("h3");
  h.className = "wrapped-section";
  h.textContent = title;
  return h;
}

// A tile for an album or artist (art, title, a line under it)
function wrappedCard(item, onClick) {
  const tile = document.createElement("button");
  tile.className = `wrapped-card${item.round ? " round" : ""}`;
  tile.append(wrappedArt(item.imageKey, 400, item.title, item.round));
  const title = document.createElement("div");
  title.className = "wrapped-card-title";
  title.textContent = item.title;
  tile.append(title);
  if (item.sub) {
    const sub = document.createElement("div");
    sub.className = "wrapped-card-sub";
    sub.textContent = item.sub;
    tile.append(sub);
  }
  if (item.why) tile.title = item.why;
  tile.addEventListener("click", onClick);
  return tile;
}

function wrappedGrid(items, onClick) {
  const grid = document.createElement("div");
  grid.className = "wrapped-cards";
  grid.append(...items.map((item) => wrappedCard(item, () => onClick(item))));
  return grid;
}

// The Wrapped pages run on the Hearts & Library link (Roon's internal
// protocol); without it there's nothing to show
function wrappedNeedsLibrary() {
  if (libraryReady) return false;
  wrappedBody.append(
    wrappedNote("Wrapped reads your library through the Hearts & Library link, which is off or not connected."),
  );
  const open = document.createElement("button");
  open.className = "wrapped-button";
  open.textContent = "Open Settings";
  open.addEventListener("click", () => send("open_window", { window: "settings" }));
  wrappedBody.append(open);
  return true;
}

// A page's fetches are dropped when another page has taken over meanwhile
let wrappedTicket = 0;

// The album page's track rows, with their ids, so the playing mark follows
// the zone (renderLibrary calls markWrappedPlaying on every track change)
let wrappedTrackRows = [];
let wrappedAlbumTracks = []; // the album page's tracks, for "save as playlist"
function markWrappedPlaying() {
  if (!wrappedTrackRows.length) return;
  // (The zone's track carries its ids as strings, the album's rows as numbers)
  const track = zone ? libraryTracks[zone.zoneId] : null;
  const ids = track ? [track.trackId, track.libraryTrackId].filter((id) => id != null).map(String) : [];
  for (const { row, ids: own } of wrappedTrackRows) {
    row.classList.toggle("playing", own.some((id) => ids.includes(String(id))));
  }
}

function wrappedFailed(err) {
  wrappedBody.replaceChildren(wrappedNote(typeof err === "string" ? err : "Roon didn't answer.", "wrapped-note error"));
}

// ----- The pages -----

const WRAPPED_PAGES = {
  // Home: artists for you (new to you, by what you play), new albums from
  // the artists you listen to, and from those suggested (home.rs)
  home() {
    wrappedHeading("Wrapped", "Playlists, radio and discovery, built on your Roon library");
    const here = wrappedHere();
    if (here) {
      const open = document.createElement("button");
      open.className = "wrapped-button small wrapped-here";
      open.innerHTML = `${WRAPPED_ICONS[here.icon] || ""}<span>${here.label}</span>`;
      open.title = "Open what Roon is showing now";
      open.addEventListener("click", () => (here.find ? wrappedFindOnScreen() : openWrappedPage(here)));
      wrappedBody.append(open);
    }
    if (wrappedNeedsLibrary()) return;
    const holder = document.createElement("div");
    holder.className = "wrapped-home";
    wrappedBody.append(holder);
    wrappedHome(holder, ++wrappedTicket);
  },

  // An album: by the Core's id (from Roon's log) or by a session handle
  async album(entry) {
    wrappedHeading("Album", "");
    if (wrappedNeedsLibrary()) return;
    const ticket = ++wrappedTicket;
    wrappedBody.append(wrappedNote("Loading..."));
    let card;
    try {
      card = entry.handle
        ? await invoke("library_album_card", { handle: entry.handle })
        : await invoke("library_album_by_id", { albumId: entry.albumId });
    } catch (err) {
      if (ticket === wrappedTicket) wrappedFailed(err);
      return;
    }
    if (ticket !== wrappedTicket) return;
    entry.handle = card.handle;
    wrappedHeading(card.title, card.artist);
    wrappedBody.replaceChildren(albumHeader(card));
    wrappedFacts("album", card, ticket);
    // (The first of several credited artists)
    const mainArtist = (card.artist || "").split(/ \/ |, /)[0];
    if (mainArtist) wrappedOutsideTags(wrappedBody.querySelector(".wrapped-genres.outside"), mainArtist, ticket, card.title);
    const list = document.createElement("div");
    list.className = "wrapped-tracks";
    list.append(wrappedNote("Loading tracks..."));
    wrappedBody.append(list);
    let tracks;
    try {
      tracks = await invoke("library_album_tracks_for", { zoneId: zone?.zoneId ?? "", handle: card.handle });
    } catch (err) {
      if (ticket === wrappedTicket) list.replaceChildren(wrappedNote(String(err), "wrapped-note error"));
      return;
    }
    if (ticket !== wrappedTicket) return;
    wrappedAlbumTracks = tracks;
    list.replaceChildren(...trackRows(tracks, card));
    wrappedTrackRows = [...list.querySelectorAll(".wrapped-track")].map((row, i) => ({
      row,
      ids: [tracks[i].trackId, tracks[i].libraryTrackId].filter((id) => id != null),
    }));
    markWrappedPlaying();
    const total = tracks.reduce((sum, t) => sum + (t.lengthSeconds || 0), 0);
    const facts = [card.year, `${tracks.length} track${tracks.length === 1 ? "" : "s"}`, total ? formatLongTime(total) : null]
      .filter(Boolean)
      .join(" · ");
    wrappedBody.querySelector(".wrapped-facts").textContent = facts;
    const extras = document.createElement("div");
    extras.className = "wrapped-extras";
    wrappedBody.append(extras);
    await wrappedAlbumExtras(extras, card, ticket);
  },

  // An artist: by a handle, or by the album Roon showed last (its main artist)
  async artist(entry) {
    wrappedHeading("Artist", "");
    if (wrappedNeedsLibrary()) return;
    const ticket = ++wrappedTicket;
    wrappedBody.append(wrappedNote("Loading..."));
    let card;
    try {
      if (!entry.handle && !entry.performerId && entry.imageKeys) {
        const found = await invoke("wrapped_artist_by_images", { keys: entry.imageKeys });
        if (!found) {
          // Nobody by those pictures: the name off Roon's screen (ocr.rs),
          // into Wrapped's search
          wrappedStack.pop();
          await wrappedFindOnScreen();
          return;
        }
        if (found.handle) entry.handle = found.handle;
        else entry.performerId = found.performerId;
      }
      if (!entry.handle && entry.performerId) {
        card = await invoke("library_artist_by_id", { performerId: entry.performerId });
        entry.handle = card.handle;
      } else if (!entry.handle) {
        const album = await invoke("library_album_by_id", { albumId: entry.albumId });
        if (!album.performerHandle) throw "Roon didn't say who that album's artist is.";
        entry.handle = album.performerHandle;
      }
      card ||= await invoke("library_artist_card", { handle: entry.handle });
    } catch (err) {
      if (ticket === wrappedTicket) wrappedFailed(err);
      return;
    }
    if (ticket !== wrappedTicket) return;
    wrappedHeading(card.name, card.albumCount ? `${card.albumCount} album${card.albumCount === 1 ? "" : "s"}` : "");
    wrappedBody.replaceChildren(artistHeader(card));
    wrappedFacts("artist", card, ticket);
    wrappedOutsideTags(wrappedBody.querySelector(".wrapped-genres.outside"), card.name, ticket);
    // Roon's picks for the artist (similar artists, and the albums it would
    // start you on, which order the discography by Popularity) load meanwhile
    const picks = invoke("library_artist_extras", { handle: card.handle }).then(
      (x) => ({ x }),
      (err) => ({ err }),
    );
    const holder = document.createElement("div");
    holder.append(wrappedNote("Loading albums..."));
    wrappedBody.append(holder);
    let albums;
    try {
      albums = await invoke("library_artist_albums_for", { handle: card.handle, zoneId: zone?.zoneId ?? null });
    } catch (err) {
      if (ticket === wrappedTicket) holder.replaceChildren(wrappedNote(String(err), "wrapped-note error"));
      return;
    }
    if (ticket !== wrappedTicket) return;
    holder.replaceChildren();
    if (albums.length) wrappedDiscography(holder, albums, picks);
    else holder.append(wrappedNote("Roon lists no albums for this artist."));
    const extras = document.createElement("div");
    extras.className = "wrapped-extras";
    wrappedBody.append(extras);
    await wrappedArtistExtras(extras, picks, card, ticket);
  },

  // Search: what the bar's search found (the bar is the input)
  async search(entry) {
    const query = (entry.query || "").trim();
    wrappedHeading("Search", query ? `"${query}" in your library, Qobuz and TIDAL` : "Artists, albums and tracks, from Roon's own search");
    if (wrappedNeedsLibrary()) return;
    const results = document.createElement("div");
    results.className = "wrapped-results";
    wrappedBody.append(results);
    if (!query) {
      results.append(wrappedNote("Type in the bar above to search."));
      return;
    }
    const ticket = wrappedTicket;
    results.replaceChildren(wrappedNote("Searching..."));
    let hits;
    try {
      hits = await invoke("library_search", { query });
    } catch (err) {
      if (ticket === wrappedTicket) results.replaceChildren(wrappedNote(String(err), "wrapped-note error"));
      return;
    }
    if (ticket !== wrappedTicket) return;
    results.replaceChildren(...searchResults(hits, query));
  },

  // Deep Dive: a playlist made to order from the picks (songs, albums,
  // artists from their brain buttons) and what you ask for; the Radio tab
  // is the radio page for one of the picks. Without tier 2: the radio page.
  deepdive() {
    if (!tier2Ready()) return WRAPPED_PAGES.radio({});
    wrappedHeading("Deep Dive", "A playlist made to order from what you pick and what you ask for");
    if (wrappedNeedsLibrary()) return;
    const tabs = document.createElement("div");
    tabs.className = "wrapped-tabs dive-tabs";
    for (const [value, label] of [
      ["dive", "Deep Dive"],
      ["radio", "Radio"],
    ]) {
      const tab = document.createElement("button");
      tab.className = `wrapped-tab${deepDiveTab === value ? " selected" : ""}`;
      tab.textContent = label;
      tab.addEventListener("click", () => {
        if (deepDiveTab === value) return;
        deepDiveTab = value;
        renderWrapped();
      });
      tabs.append(tab);
    }
    wrappedBody.append(tabs);
    if (deepDiveTab === "radio") return diveRadioTab();
    wrappedBody.append(divePicks());
    const check = soundCheckBox();
    if (check) wrappedBody.append(check);
    wrappedBody.append(diveAsk());
    wrappedBody.append(diveMade());
  },

  // Roon Radio seeded with an artist (or album) and the artists that share
  // its sound; without one, a search for it first
  radio(entry) {
    if (!entry.name) {
      wrappedHeading("Radio", "Pick the artist to start from");
      if (wrappedNeedsLibrary()) return;
      return radioPickSeed();
    }
    wrappedHeading("Radio", entry.album ? `From ${entry.album} by ${entry.name}` : `From ${entry.name}`);
    if (wrappedNeedsLibrary()) return;
    wrappedRadioPage(entry, ++wrappedTicket);
  },

  // The weekly playlists (discover.rs): this week's large, last weeks'
  // two smaller above it, and Build now for each
  async discover() {
    wrappedHeading("Discover", "Built every Monday from your listening; the last two weeks are kept");
    if (!(await discoveryEnabled())) {
      wrappedBody.append(wrappedNote("Wrapped Discovery is off. Turn it on in Wrapped's Settings."));
      const go = document.createElement("button");
      go.className = "wrapped-button";
      go.textContent = "Open Settings";
      go.addEventListener("click", () => openWrappedPage({ page: "settings" }));
      wrappedBody.append(go);
      return;
    }
    discoverSeen();
    if (wrappedNeedsLibrary()) return;
    const ticket = ++wrappedTicket;
    wrappedBody.append(wrappedNote("Loading..."));
    let status;
    try {
      status = await invoke("wrapped_discover_status");
    } catch (err) {
      if (ticket === wrappedTicket) wrappedFailed(err);
      return;
    }
    if (ticket !== wrappedTicket) return;
    wrappedBody.replaceChildren();
    const lists = document.createElement("div");
    lists.className = "discover-lists";
    lists.append(
      discoverColumn("weekly", "🌟 Discover Weekly", "Artists you don't have and have never played, found through the listeners of what you've played most lately. One song each.", status.weekly, status.building, status.weeklyError, status),
      discoverColumn("rediscover", "💎 Library Rediscover", "Forgotten gems: old favorites you haven't played in a year or more, and a third from artists in your library you've never played.", status.rediscover, status.building, status.rediscoverError),
    );
    wrappedBody.append(lists, discoverResetRow(!!status.building));
  },

  async playlists() {
    wrappedHeading("Playlists", "Roon's playlists; the ones Wrapped made first");
    if (wrappedNeedsLibrary()) return;
    const ticket = ++wrappedTicket;
    wrappedBody.append(wrappedNote("Loading..."));
    let list;
    try {
      list = await invoke("library_playlists");
    } catch (err) {
      if (ticket === wrappedTicket) wrappedFailed(err);
      return;
    }
    if (ticket !== wrappedTicket) return;
    wrappedBody.replaceChildren(wrappedCorner("Ban list", () => openWrappedPage({ page: "banlist" })));
    const mine = list.filter(isWrappedPlaylist);
    const others = list.filter((p) => !isWrappedPlaylist(p));
    if (mine.length) {
      wrappedBody.append(wrappedSection("Made by Wrapped"), playlistRows(mine, true));
    } else {
      wrappedBody.append(wrappedNote("Wrapped hasn't made any playlists yet. On an album page, the list button saves the album as one."));
    }
    if (others.length) wrappedBody.append(wrappedSection("Your other playlists"), playlistRows(others, false));
  },

  // The ban list: artists never suggested again (a thumbs down on a
  // Discover song, or the X on a Home card)
  async banlist() {
    wrappedHeading("Ban list", "Artists Wrapped never suggests: not on Home, not by Discover, Deep Dive or the radio");
    wrappedBody.append(wrappedCorner("Playlists", () => openWrappedPage({ page: "playlists" })));
    const ticket = ++wrappedTicket;
    let list;
    try {
      list = await invoke("wrapped_ban_list");
    } catch (err) {
      if (ticket === wrappedTicket) wrappedFailed(err);
      return;
    }
    if (ticket !== wrappedTicket) return;
    const holder = document.createElement("div");
    wrappedBody.append(holder);
    const paint = (list) => {
      holder.replaceChildren();
      if (!list.length) return holder.append(wrappedNote("Nobody's banned. The X on an artist or album on Home adds one; so does a thumbs down on a Discover song."));
      const rows = document.createElement("div");
      rows.className = "wrapped-ban-rows";
      for (const b of list) {
        const row = document.createElement("div");
        row.className = "wrapped-ban-row";
        const name = document.createElement("span");
        name.className = "wrapped-ban-name";
        name.textContent = b.name;
        const when = document.createElement("span");
        when.className = "wrapped-ban-when";
        when.textContent = b.at ? historyWhen(b.at) : "";
        const x = document.createElement("button");
        x.className = "wrapped-button small";
        x.textContent = "Unban";
        x.addEventListener("click", async () => {
          try {
            paint(await invoke("wrapped_ban_remove", { artist: b.name }));
            wrappedToast(`${b.name} can be suggested again.`);
          } catch (err) {
            wrappedToast(String(err));
          }
        });
        row.append(name, when, x);
        rows.append(row);
      }
      holder.append(rows);
    };
    paint(list);
  },
};

// A button in the page's top right corner (the Playlists page's "Ban
// list", the ban list's "Playlists")
function wrappedCorner(label, onClick) {
  const row = document.createElement("div");
  row.className = "wrapped-corner";
  const btn = document.createElement("button");
  btn.className = "wrapped-button small";
  btn.textContent = label;
  btn.addEventListener("click", onClick);
  row.append(btn);
  return row;
}

// ----- Roon's picks (the "one box" calls its own pages are built from) -----

const albumItem = (a) => ({
  handle: a.handle,
  title: a.title,
  sub: [a.artist, a.year, a.inLibrary ? "In library" : null].filter(Boolean).join(" · "),
  imageKey: a.imageKey,
});
const artistItem = (p) => ({ handle: p.handle, title: p.name, sub: "", imageKey: p.imageKey, round: true });
const openAlbum = (item) => openWrappedPage({ page: "album", handle: item.handle });
const openArtist = (item) => openWrappedPage({ page: "artist", handle: item.handle });

// A titled row of cards, or nothing when the list is empty
function wrappedPicks(holder, title, items, onClick) {
  if (!items.length) return false;
  holder.append(wrappedSection(title), wrappedGrid(items, onClick));
  return true;
}

// The "Similar by" row: Roon's own picks, or artists that share this one's
// genre, style or scene tags, or its era, among the artists you've played
// (tags.rs). The default pill is a Wrapped setting.
async function wrappedSimilarBlock(holder, subject, roonRows, ticket) {
  wrappedSettings ||= await invoke("wrapped_settings_get").catch(() => ({ similarBy: "roon" }));
  if (ticket !== wrappedTicket) return;
  const block = document.createElement("div");
  block.className = "wrapped-similar";
  const head = document.createElement("div");
  head.className = "wrapped-similar-head";
  const title = wrappedSection("Similar artists");
  const pills = document.createElement("div");
  pills.className = "wrapped-tabs small";
  const scopes = document.createElement("div");
  scopes.className = "wrapped-tabs small scope";
  const body = document.createElement("div");
  head.append(title, pills, scopes);
  block.append(head, body);
  holder.append(block);
  let current = wrappedSettings.similarBy || "roon";
  let scope = wrappedSettings.similarScope || "library";
  const show = async () => {
    for (const pill of pills.children) pill.classList.toggle("selected", pill.dataset.by === current);
    for (const pill of scopes.children) pill.classList.toggle("selected", pill.dataset.scope === scope);
    scopes.hidden = current === "roon";
    body.replaceChildren();
    if (current === "roon") {
      if (roonRows.length) body.append(wrappedGrid(roonRows.map(artistItem), openArtist));
      else body.append(wrappedNote("Roon has no similar artists for this one."));
      return;
    }
    const by = current;
    const mine = scope;
    body.append(wrappedNote(scope === "discover" ? "Asking Last.fm for neighbours..." : "Comparing tags..."));
    let answer;
    let status;
    try {
      [answer, status] = await Promise.all([
        invoke("wrapped_similar", { artist: subject.artist, album: subject.album || null, by, scope, limit: 30 }),
        invoke("wrapped_tags_status").catch(() => null),
      ]);
    } catch (err) {
      if (ticket === wrappedTicket) body.replaceChildren(wrappedNote(String(err), "wrapped-note error"));
      return;
    }
    if (ticket !== wrappedTicket || current !== by || scope !== mine) return;
    body.replaceChildren();
    if (!status?.library) {
      body.append(wrappedNote("The library hasn't been read yet; Library and Discover need it. Read it in Wrapped's settings."));
    }
    const grid = document.createElement("div");
    const pendingNote = document.createElement("div");
    body.append(grid, pendingNote);
    const paint = (rows, pending) => {
      grid.replaceChildren();
      if (rows.length) {
        const cards = wrappedGrid(
          rows.map((r) => ({ row: r, title: r.name, sub: r.why.slice(0, 3).join(" · "), imageKey: r.imageKey, round: true })),
          (item) => openFromHistory("artist", item.row),
        );
        grid.append(cards);
        wrappedFillPhotos(rows, cards, ticket);
      } else if (!pending && answer.subjectEmpty) {
        // Nothing to compare: say so instead of blaming the library
        const who = subject.album ? `${subject.album} or ${subject.artist}` : subject.artist;
        const missing = {
          genre: "Last.fm has no genre tags",
          blend: "Last.fm has no genre tags",
          style: "Last.fm has no style, sound or theme words (like atmospheric, melodic or viking)",
          scene: "Last.fm has no scene or country tags",
          era: "MusicBrainz has no start year",
        }[by];
        grid.append(wrappedNote(`${missing} for ${who}, so there is nothing to match on.${by === "genre" || by === "blend" ? "" : " Try Genre."}`));
      } else if (!pending) {
        grid.append(
          wrappedNote(
            `No tagged artist ${scope === "discover" ? "outside" : "in"} your library shares this one's ${{ era: "era and genre", blend: "genre and style" }[by] || by} tags yet.`,
          ),
        );
      }
      pendingNote.replaceChildren();
      if (pending) {
        const n = wrappedNote(`Tagging ${pending} more neighbour${pending === 1 ? "" : "s"} from Last.fm...`);
        n.classList.add("small");
        pendingNote.append(n);
      }
    };
    paint(answer.rows, answer.pending);
    if (answer.pending) {
      // More rows arrive as the neighbours get tagged, five at a time
      const unlisten = await listen("wrapped-similar", (event) => {
        if (ticket !== wrappedTicket || current !== by || scope !== mine || event.payload.key !== answer.key) {
          if (ticket !== wrappedTicket) unlisten();
          return;
        }
        paint(event.payload.rows, event.payload.pending);
        if (!event.payload.pending) unlisten();
      });
    }
    if (status && status.tagged < status.artists) {
      const note = wrappedNote(
        `${status.tagged.toLocaleString()} of the ${status.artists.toLocaleString()} artists you've played are tagged${status.filling ? " (tagging now)" : "; tag the rest in Wrapped's settings"}.`,
      );
      note.classList.add("small");
      body.append(note);
    }
  };
  for (const [by, label] of SIMILAR_BY) {
    const pill = document.createElement("button");
    pill.className = "wrapped-tab";
    pill.dataset.by = by;
    pill.textContent = label;
    pill.addEventListener("click", () => {
      current = by;
      show();
    });
    pills.append(pill);
  }
  for (const [value, label] of SIMILAR_SCOPES) {
    const pill = document.createElement("button");
    pill.className = "wrapped-tab";
    pill.dataset.scope = value;
    pill.textContent = label;
    pill.addEventListener("click", () => {
      scope = value;
      show();
    });
    scopes.append(pill);
  }
  await show();
}

async function wrappedAlbumExtras(holder, card, ticket) {
  holder.append(wrappedNote("Looking for similar albums..."));
  let x;
  try {
    x = await invoke("library_album_extras", { handle: card.handle });
  } catch (err) {
    if (ticket === wrappedTicket) holder.replaceChildren(wrappedNote(String(err), "wrapped-note error"));
    return;
  }
  if (ticket !== wrappedTicket) return;
  holder.replaceChildren();
  const mainArtist = (card.artist || "").split(/ \/ |, /)[0];
  await wrappedSimilarBlock(holder, { artist: mainArtist, album: card.title }, x.similarArtists, ticket);
  if (ticket !== wrappedTicket) return;
  wrappedPicks(holder, "Similar albums", x.similar.map(albumItem), openAlbum);
  wrappedPicks(holder, "Recommended from this album", x.recommended.map(albumItem), openAlbum);
}

// (Roon's recommended and latest albums for an artist are its own albums, so
// they order the discography above instead of repeating it here)
async function wrappedArtistExtras(holder, picks, card, ticket) {
  holder.append(wrappedNote("Looking for similar artists..."));
  const { x, err } = await picks;
  if (ticket !== wrappedTicket) return;
  if (err) {
    holder.replaceChildren(wrappedNote(String(err), "wrapped-note error"));
    return;
  }
  holder.replaceChildren();
  await wrappedSimilarBlock(holder, { artist: card.name }, x.similar, ticket);
}

// ----- The Wrapped radio: Roon Radio (PlaySwim) seeded with the artists
// that share an artist's sound by its tags (tags.rs), not Roon's own guess
// from one artist -----

const RADIO_BY = [
  ["blend", "Genre + Style"],
  ["genre", "Genre"],
  ["style", "Style"],
  ["scene", "Scene"],
  ["roon", "Roon's similar"],
];
const RADIO_COUNTS = [5, 10, 20, 30, 50];
const RADIO_PER_ARTIST = [1, 2, 3, 5];
// Remembered between pages and runs (a convenience, so storage may fail)
let radioPrefs = { by: "blend", scope: "library", count: 20, perArtist: 3, includeLive: false, libraryOnly: false, savePlaylist: false };
try {
  radioPrefs = { ...radioPrefs, ...JSON.parse(localStorage.getItem("wrapped-radio") || "{}") };
} catch {}
if (!RADIO_COUNTS.includes(radioPrefs.count)) radioPrefs.count = 20;
if (!RADIO_PER_ARTIST.includes(radioPrefs.perArtist)) radioPrefs.perArtist = 3;
function saveRadioPrefs() {
  try {
    localStorage.setItem("wrapped-radio", JSON.stringify(radioPrefs));
  } catch {}
}

// The button on artist and album pages: opens the radio page for them
function wrappedRadioBuilder(what, entry) {
  return wrappedIconButton(RADIO_ICON, `Wrapped radio from this ${what}`, () => {
    if (!entry.name) return wrappedToast("Roon didn't say who the artist is.");
    openWrappedPage({ page: "radio", ...entry });
  }, "wrapped-radio");
}

// Deep Dive's picks, each with an x, and a [+] after them: back to where
// you came from to pick another (the brain button there adds it)
function divePicks() {
  const box = document.createElement("section");
  box.className = "dive-picks";
  const head = document.createElement("div");
  head.className = "dive-picks-head";
  const title = document.createElement("h4");
  title.textContent = diveSeeds.length ? `Your picks (${diveSeeds.length} of ${DIVE_MAX})` : "Your picks";
  head.append(title);
  box.append(head);
  const row = document.createElement("div");
  row.className = "dive-pick-row";
  const kinds = { song: "Song", album: "Album", artist: "Artist" };
  diveSeeds.forEach((seed, i) => {
    const pick = document.createElement("div");
    pick.className = "dive-pick";
    const art = wrappedArt(seed.imageKey, 96, seed.title, seed.kind === "artist");
    art.classList.add("small");
    const text = document.createElement("div");
    text.className = "dive-pick-text";
    const name = document.createElement("span");
    name.className = "dive-pick-title";
    name.textContent = seed.title;
    const sub = document.createElement("span");
    sub.className = "dive-pick-sub";
    sub.textContent = [kinds[seed.kind], seed.kind === "artist" ? null : seed.artist].filter(Boolean).join(" · ");
    text.append(name, sub);
    const remove = document.createElement("button");
    remove.className = "discover-ask-x";
    remove.innerHTML = CLOSE_ICON;
    remove.setAttribute("aria-label", `Take ${seed.title} out`);
    remove.addEventListener("click", () => {
      diveSeeds.splice(i, 1);
      renderWrapped();
    });
    pick.append(art, text, remove);
    row.append(pick);
  });
  if (diveSeeds.length < DIVE_MAX) {
    const add = document.createElement("button");
    add.className = "dive-add";
    add.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14"/><path d="M12 5v14"/></svg>';
    add.title = "Pick another: back to where you were, then its brain button";
    add.setAttribute("aria-label", "Pick another");
    // (nothing picked yet: the bar's search, to find the first; after a
    // pick, back to where it came from)
    add.addEventListener("click", () => {
      const before = wrappedStack.at(-2);
      if (diveSeeds.length && before && before.page !== "deepdive") wrappedBackPage();
      else wrappedBarInput.focus();
    });
    row.append(add);
  }
  box.append(row);
  if (!diveSeeds.length) {
    box.append(wrappedNote(`Pick up to ${DIVE_MAX} songs, albums or artists with the brain button next to them, or just describe the playlist below.`, "wrapped-note small"));
  }
  return box;
}

// What the playlist should be, in your own words. Make the playlist starts
// a talk with tier 2 (diveChatPanel): a question at a time, yours answered
// too; once it's settled, the talk goes away and the playlist is made
let diveSlot = null; // where the talk shows, under the words
let diveGo = null; // Make the playlist (hidden while talking)
function diveAsk() {
  const box = document.createElement("section");
  box.className = "dive-ask";
  const label = document.createElement("h4");
  label.textContent = "What should it be?";
  const input = document.createElement("textarea");
  input.className = "wrapped-search dive-input";
  input.rows = 3;
  input.maxLength = 500;
  input.placeholder = diveSeeds.length ? "e.g. the guitars of these, but heavier and from the 90s" : "e.g. the heaviest death metal, songs I've never heard";
  input.value = diveText;
  const go = document.createElement("button");
  go.className = "wrapped-play dive-go";
  go.innerHTML = `${diveIcon()}<span>Make the playlist</span>`;
  const paint = () => {
    go.disabled = !diveText.trim() && !diveSeeds.length;
    go.title = go.disabled ? "Pick something, or say what the playlist should be" : "";
  };
  input.addEventListener("input", () => {
    diveText = input.value;
    paint();
  });
  paint();
  go.addEventListener("click", () => {
    diveChat = { turns: [], plan: null, waiting: false, error: "", draft: "" };
    diveTalk();
  });
  // Where from: the toggle, so the talk needn't ask
  const from = document.createElement("div");
  from.className = "dive-from";
  const fromLabel = document.createElement("span");
  fromLabel.className = "dive-from-label";
  fromLabel.textContent = "Songs from";
  const fromTabs = document.createElement("div");
  fromTabs.className = "wrapped-tabs small";
  from.append(fromLabel, fromTabs);
  const chips = [];
  for (const [value, text] of [["new", "New to you"], ["library", "Your library"], ["both", "Both"]]) {
    const chip = document.createElement("button");
    chip.className = `wrapped-tab${diveSource === value ? " selected" : ""}`;
    chip.textContent = text;
    chip.addEventListener("click", () => {
      diveSource = value;
      try {
        localStorage.setItem("wrapped-dive-source", value);
      } catch {}
      chips.forEach((c) => c.classList.toggle("selected", c === chip));
      if (diveChat?.plan) {
        diveChat.plan.source = value;
        diveChatRedraw();
      }
    });
    chips.push(chip);
    fromTabs.append(chip);
  }
  const slot = document.createElement("div");
  slot.className = "dive-chat-slot";
  diveSlot = slot;
  diveGo = go;
  box.append(label, input, from, go, slot);
  diveChatRedraw();
  return box;
}

// The picks as dive.rs takes them
function divePicksPayload() {
  return diveSeeds.map((s) => ({
    kind: s.kind,
    handle: s.handle,
    title: s.title,
    artist: s.artist || "",
    album: s.album || "",
    albumHandle: s.albumHandle || null,
    artistHandle: (s.kind === "artist" ? s.handle : s.radio?.handle) || null,
    imageKey: s.imageKey || null,
  }));
}

// The talk before building: { turns: [{ from: "listener" | "dive" |
// "note", text }], plan (what tier 2 last said is settled), waiting, error,
// draft, asking (its question waits for you: the bubble flashes), follows
// (the Deep Dive it's about, when taken up again: its build time) }
let diveChat = null;
// The turns tier 2 reads (the page's own notes left out)
const diveHistory = (chat) => chat.turns.filter((t) => t.from !== "note");

// A Deep Dive at work (tier 2 thinking, or the playlist being made): Roon's
// own page changes don't close Wrapped meanwhile
let diveBuilding = false;
function diveHolding() {
  return diveBuilding || !!diveChat?.waiting;
}

// Deep Dive back in front when it needs you or is done: its page, Wrapped
// opened on it from anywhere else (`render`: drawn again when it's already
// showing); the taskbar button flashes when the window is minimized or
// behind another
function diveToFront(render = false) {
  const showing = !wrapped.hidden && wrappedStack.at(-1)?.page === "deepdive";
  if (!showing || deepDiveTab !== "dive" || render) {
    deepDiveTab = "dive";
    if (showing) renderWrapped();
    else openWrappedPage({ page: "deepdive" });
  }
  if (document.hidden || !document.hasFocus()) invoke("wrapper_attention").catch(() => {});
}
const DIVE_SOURCES = { library: "from your library", new: "new to you", both: "your library and new to you" };

function diveChatRedraw() {
  if (diveGo) diveGo.hidden = !!diveChat;
  if (!diveSlot?.isConnected && diveSlot?.parentNode == null) return;
  diveSlot.replaceChildren(...(diveChat ? [diveChatPanel()] : []));
}

// Tier 2's next turn (its opening question when nothing's been said)
async function diveTalk() {
  const chat = diveChat;
  if (!chat) return;
  chat.waiting = true;
  chat.error = "";
  chat.asking = false;
  diveChatRedraw();
  try {
    const reply = await invoke("wrapped_dive_chat", { picks: divePicksPayload(), text: diveText, source: diveSource, history: diveHistory(chat), follows: chat.follows ?? null });
    if (diveChat !== chat) return;
    chat.waiting = false;
    chat.plan = { summary: reply.summary, source: diveSource, count: reply.count, perArtist: reply.perArtist };
    if (reply.say) chat.turns.push({ from: "dive", text: reply.say });
    if (reply.done) return diveBuild(reply.say);
    chat.asking = true;
  } catch (err) {
    if (diveChat !== chat) return;
    chat.waiting = false;
    chat.error = String(err);
  }
  diveChatRedraw();
  diveToFront();
}

function diveChatPanel() {
  const chat = diveChat;
  const panel = document.createElement("section");
  panel.className = "dive-chat";
  const log = document.createElement("div");
  log.className = "dive-chat-log";
  const message = (from, text, asking = false) => {
    if (from === "note") return log.append(wrappedNote(text, "wrapped-note small dive-note"));
    const row = document.createElement("div");
    row.className = `dive-msg from-${from}${asking ? " asking" : ""}`;
    if (from === "dive") row.insertAdjacentHTML("beforeend", diveIcon());
    const bubble = document.createElement("div");
    bubble.className = "dive-msg-text";
    if (text == null) bubble.innerHTML = '<span class="dive-typing"><i></i><i></i><i></i></span>';
    else bubble.textContent = text;
    row.append(bubble);
    log.append(row);
  };
  // (its question waiting for an answer flashes until you start on one)
  chat.turns.forEach((t, i) => message(t.from, t.text, chat.asking && !chat.waiting && i === chat.turns.length - 1 && t.from === "dive"));
  if (chat.waiting) message("dive", null);
  panel.append(log);
  if (chat.error) panel.append(wrappedNote(chat.error, "wrapped-note error small"));
  if (chat.plan) {
    const p = chat.plan;
    panel.append(wrappedNote(`So far: ${DIVE_SOURCES[p.source] || DIVE_SOURCES.both} · ${p.count} songs · ${p.perArtist} per artist`, "wrapped-note small dive-chat-plan"));
  }
  const row = document.createElement("div");
  row.className = "dive-chat-row";
  const input = document.createElement("textarea");
  input.className = "wrapped-search dive-chat-input";
  input.rows = 2;
  input.maxLength = 600;
  input.placeholder = "Answer, ask what it means, or say more...";
  input.value = chat.draft || "";
  input.disabled = chat.waiting;
  const stopAsking = () => {
    if (!chat.asking) return;
    chat.asking = false;
    log.querySelector(".dive-msg.asking")?.classList.remove("asking");
  };
  input.addEventListener("input", () => {
    chat.draft = input.value;
    stopAsking();
  });
  input.addEventListener("pointerdown", stopAsking);
  const send = document.createElement("button");
  send.className = "wrapped-button";
  send.textContent = chat.error && !chat.draft ? "Try again" : "Send";
  send.disabled = chat.waiting;
  const sendNow = () => {
    const text = input.value.trim();
    if (chat.waiting) return;
    if (text) chat.turns.push({ from: "listener", text });
    else if (!chat.error) return input.focus();
    chat.draft = "";
    diveTalk();
  };
  send.addEventListener("click", sendNow);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      sendNow();
    }
  });
  row.append(input, send);
  const actions = document.createElement("div");
  actions.className = "dive-chat-actions";
  const make = document.createElement("button");
  make.className = "wrapped-play dive-go small";
  make.innerHTML = `${diveIcon()}<span>Make it now</span>`;
  make.title = "Skip the rest of the questions and make it with what's settled";
  make.disabled = chat.waiting;
  make.addEventListener("click", () => diveBuild());
  const over = document.createElement("button");
  over.className = "dive-chat-over";
  over.textContent = "Start over";
  over.addEventListener("click", () => {
    diveChat = null;
    diveChatRedraw();
  });
  actions.append(make, over);
  panel.append(row, actions);
  requestAnimationFrame(() => {
    log.scrollTop = log.scrollHeight;
    if (!chat.waiting && panel.isConnected) input.focus({ preventScroll: true });
  });
  return panel;
}

// Makes the Deep Dive from what the talk settled, with the builder's
// progress over the page; the talk comes back if it fails
async function diveBuild(said) {
  const chat = diveChat;
  const plan = { ...(chat?.plan || { summary: "", count: 30, perArtist: 1 }), source: diveSource };
  const history = chat ? diveHistory(chat) : [];
  const follows = chat?.follows ?? null;
  diveChat = null;
  diveChatRedraw();
  diveBuilding = true;
  wrappedBusy(said || "Making your Deep Dive...");
  const unlisten = await listen("wrapped-dive", (event) => {
    if (event.payload.text) wrappedBusy(event.payload.text);
  });
  try {
    const status = await invoke("wrapped_dive_build", { picks: divePicksPayload(), text: diveText, plan, history, follows });
    diveLists = status.dives;
    renderDivePiece();
    const made = status.dives[0];
    // (the new one opens on its songs)
    if (made) diveOpen.add(made.builtAt);
    wrappedToast(`Made ${made?.name || "your Deep Dive"}.`);
  } catch (err) {
    wrappedToast(String(err));
    if (chat) {
      chat.error = `It didn't get made: ${err}`;
      diveChat = chat;
    }
  } finally {
    diveBuilding = false;
    unlisten();
    wrappedBusy(null);
    // (back on Deep Dive, wherever you went meanwhile)
    diveToFront(true);
  }
}

// "Talk it over": a Deep Dive made earlier taken up again: its picks, its
// words and its talk back, and tier 2 asks how it turned out; what you say
// of its songs (the ones that hit the mark, the misses) leads the next one
function diveTakeUp(d) {
  diveSeeds = (d.pickList || []).map((p) => ({
    kind: p.kind,
    handle: p.handle,
    title: p.title,
    artist: p.artist,
    album: p.album,
    albumHandle: p.albumHandle,
    imageKey: p.imageKey,
    radio: p.kind === "artist" ? { name: p.title, handle: p.handle } : p.kind === "album" ? { name: p.artist, handle: p.artistHandle, album: p.title, albumHandle: p.handle } : undefined,
  }));
  diveText = d.request || "";
  const p = d.plan;
  if (["new", "library", "both"].includes(p?.source)) {
    diveSource = p.source;
    try {
      localStorage.setItem("wrapped-dive-source", diveSource);
    } catch {}
  }
  diveChat = {
    turns: [
      ...(d.talk || []),
      { from: "note", text: `Made: ${d.name} · ${d.songs.length} songs` },
      { from: "dive", text: "How did it turn out? Tell me which songs hit the mark and which missed, and I'll dig deeper from there." },
    ],
    plan: p ? { summary: p.summary || "", source: diveSource, count: p.count || 30, perArtist: p.perArtist || 1 } : null,
    waiting: false,
    error: "",
    draft: "",
    asking: true,
    follows: d.builtAt,
  };
  deepDiveTab = "dive";
  renderWrapped();
  requestAnimationFrame(() => wrappedBody.querySelector(".dive-chat")?.scrollIntoView({ block: "center", behavior: "smooth" }));
}

// Sound check, tier 3's test on Deep Dive's page: with a song and an
// album picked, the album's songs ranked by how close they sound to the
// song, from their previews (sound.rs). Nothing is made or played. Kept
// while you move around, until the picks change: { key (the picks), running,
// done, total, result, error }
let soundCheck = null;
let soundCheckLabel = null;

function soundCheckBox() {
  if (!soundState?.ready) return null;
  const key = JSON.stringify(diveSeeds.map((s) => [s.kind, s.title, s.artist || ""]));
  if (soundCheck && soundCheck.key !== key && !soundCheck.running) soundCheck = null;
  const songs = diveSeeds.filter((s) => s.kind === "song");
  const albums = diveSeeds.filter((s) => s.kind === "album");
  if (!soundCheck && (!songs.length || !albums.length)) return null;
  const box = document.createElement("section");
  box.className = "sound-check";
  const head = document.createElement("div");
  head.className = "sound-check-head";
  const title = document.createElement("h4");
  title.textContent = "Sound check";
  const run = document.createElement("button");
  run.className = "wrapped-button small";
  run.textContent = soundCheck?.running ? "Listening..." : soundCheck?.result || soundCheck?.error ? "Run again" : "Run";
  run.disabled = !!soundCheck?.running || !songs.length || !albums.length;
  run.addEventListener("click", () => runSoundCheck(key));
  head.append(title, run);
  if (soundCheck && !soundCheck.running && (soundCheck.result || soundCheck.error)) {
    const close = document.createElement("button");
    close.className = "discover-ask-x";
    close.innerHTML = CLOSE_ICON;
    close.setAttribute("aria-label", "Close the Sound check");
    close.addEventListener("click", () => {
      soundCheck = { key, closed: true };
      soundCheckRedraw();
    });
    head.append(close);
  }
  box.append(head);
  const quoted = (list, many) => (list.length === 1 ? `"${list[0].title}"` : many);
  if (!soundCheck || soundCheck.closed) {
    box.append(
      wrappedNote(
        `Tier 3's test: the songs of ${quoted(albums, "the picked albums")} ranked by how close they sound to ${quoted(songs, "the picked songs")}, from your own files where Roon has them, else their 30-second previews. Nothing is made or played.`,
        "wrapped-note small",
      ),
    );
    return box;
  }
  if (soundCheck.running) {
    soundCheckLabel = wrappedNote("", "wrapped-note small sound-check-progress");
    box.append(soundCheckLabel);
    paintSoundCheck();
  }
  if (soundCheck.error) box.append(wrappedNote(soundCheck.error, "wrapped-note small error"));
  const r = soundCheck.result;
  if (r) {
    const rows = document.createElement("ul");
    rows.className = "sound-check-rows";
    const row = (song, rank, top) => {
      const li = document.createElement("li");
      li.className = `sound-check-row${song.heard ? "" : " missing"}${top ? " top" : ""}`;
      const place = document.createElement("span");
      place.className = "sound-check-rank";
      place.textContent = rank;
      const name = document.createElement("span");
      name.className = "sound-check-title";
      name.textContent = song.title;
      name.title = `${song.artist} - ${song.title}${song.file ? " (your file, all of it)" : song.from ? ` (${song.from}'s preview)` : ""}`;
      const bar = document.createElement("span");
      bar.className = "sound-check-bar";
      const value = document.createElement("span");
      value.className = "sound-check-value";
      if (song.closeness != null) {
        // (0.6 and under shows empty: alike songs sit around 0.85 to 0.95)
        const fill = document.createElement("i");
        fill.style.width = `${Math.max(0, Math.min(1, (song.closeness - 0.6) / 0.4)) * 100}%`;
        bar.append(fill);
        value.textContent = song.closeness.toFixed(3);
      }
      const styles = document.createElement("span");
      styles.className = "sound-check-styles";
      styles.textContent = song.heard ? song.styles.join(", ") : "No file or preview found";
      // What was heard: your file (all of the song) or its preview
      const source = document.createElement("span");
      source.className = `sound-check-source${song.file ? " file" : ""}`;
      source.textContent = song.file ? "your file" : song.heard ? "preview" : "";
      li.append(place, name, bar, value, styles, source);
      return li;
    };
    const group = (heading, list, extra = "") => {
      const g = document.createElement("div");
      g.className = `sound-check-group${extra}`;
      const h = document.createElement("h5");
      h.textContent = heading;
      g.append(h, list.children.length ? list : wrappedNote("Roon didn't list its songs.", "wrapped-note small"));
      box.append(g);
    };
    const picked = document.createElement("ul");
    picked.className = "sound-check-rows";
    for (const s of r.picks) picked.append(row({ ...s, closeness: null }, "", false));
    group(r.picks.length === 1 ? "Your song, as it sounds" : "Your songs, as they sound", picked, " picks");
    for (const a of r.albums) {
      const list = rows.cloneNode(false);
      a.songs.forEach((s, i) => list.append(row(s, s.heard ? `${i + 1}` : "-", i === 0 && s.heard)));
      group(`${a.title} by ${a.artist}, closest first`, list);
    }
    const n = r.heardNow;
    const heard = [...r.picks, ...r.albums.flatMap((a) => a.songs)].filter((s) => s.heard).length;
    const files = r.files ? ` · ${r.files} of the ${heard} from your own files` : "";
    box.append(
      wrappedNote(
        `${n ? `${n} song${n === 1 ? "" : "s"} heard just now` : "Nothing new heard"} · ${Math.round(r.seconds)} s${files}. Closeness is the artist model's: 1 is alike, around 0.3 nothing alike.`,
        "wrapped-note small",
      ),
    );
  }
  return box;
}

function paintSoundCheck() {
  if (!soundCheck?.running || !soundCheckLabel?.isConnected) return;
  const { done, total } = soundCheck;
  soundCheckLabel.textContent = total ? `Listening: ${done} of ${total} song${total === 1 ? "" : "s"}...` : "Finding your files and the previews...";
}

// The Sound check drawn again where it is (when Deep Dive is showing)
function soundCheckRedraw() {
  const old = wrappedBody.querySelector(".sound-check");
  if (wrapped.hidden || wrappedStack.at(-1)?.page !== "deepdive" || deepDiveTab !== "dive") return;
  const fresh = soundCheckBox();
  if (old && fresh) old.replaceWith(fresh);
  else if (old) old.remove();
  else if (fresh) wrappedBody.querySelector(".dive-picks")?.after(fresh);
}

async function runSoundCheck(key) {
  if (soundCheck?.running) return;
  const mine = { key, running: true, done: 0, total: 0, result: null, error: "" };
  soundCheck = mine;
  soundCheckRedraw();
  const unlisten = await listen("sound-check", (event) => {
    if (soundCheck !== mine) return;
    mine.done = event.payload.done;
    mine.total = event.payload.total;
    paintSoundCheck();
  });
  try {
    mine.result = await invoke("sound_check", { picks: divePicksPayload() });
  } catch (err) {
    mine.error = String(err);
  } finally {
    mine.running = false;
    unlisten();
    if (soundCheck === mine) soundCheckRedraw();
  }
}

// The Deep Dives made so far (newest first): Play, the songs and why
const diveOpen = new Set();
function diveMade() {
  const box = document.createElement("section");
  box.className = "dive-made";
  const ticket = wrappedTicket;
  invoke("wrapped_dive_status")
    .then((status) => {
      diveLists = status.dives;
      if (ticket !== wrappedTicket || !status.dives.length) return;
      const head = document.createElement("h4");
      head.textContent = "Made so far";
      box.append(head);
      for (const d of status.dives) {
        const card = document.createElement("div");
        card.className = `dive-made-card${diveOpen.has(d.builtAt) ? " open" : ""}`;
        const top = document.createElement("div");
        top.className = "dive-made-top";
        const text = document.createElement("div");
        text.className = "dive-made-text";
        const name = document.createElement("span");
        name.className = "dive-made-name";
        name.textContent = d.name;
        const sub = document.createElement("span");
        sub.className = "dive-made-sub";
        const date = new Date(d.builtAt).toLocaleDateString(undefined, { month: "short", day: "numeric" });
        sub.textContent = [`${d.songs.length} songs`, date, d.request ? `"${d.request}"` : d.picks.join(", ")].filter(Boolean).join(" · ");
        text.append(name, sub);
        top.append(text);
        const talk = document.createElement("button");
        talk.className = "wrapped-button small dive-talk";
        talk.textContent = "Talk it over";
        talk.title = "Tell it which songs hit the mark and which missed, and it digs deeper from there";
        talk.addEventListener("click", (e) => {
          e.stopPropagation();
          diveTakeUp(d);
        });
        top.append(talk);
        if (d.playlist) {
          const play = document.createElement("button");
          play.className = "wrapped-play discover-play small";
          play.innerHTML = `${WRAPPED_PLAY_ICON}<span>Play</span>`;
          play.addEventListener("click", (e) => {
            e.stopPropagation();
            if (!zone) return wrappedToast("Pick a zone first.");
            invoke("library_play_playlist", { zoneId: zone.zoneId, handle: d.playlist.handle, clear: true })
              .then(() => wrappedToast(`Playing ${d.name} on ${zone.name}`))
              .catch((err) => wrappedToast(String(err)));
          });
          top.append(play);
        }
        const forget = document.createElement("button");
        forget.className = "discover-ask-x";
        forget.innerHTML = CLOSE_ICON;
        forget.title = "Take it off this page";
        forget.setAttribute("aria-label", "Take it off this page");
        forget.addEventListener("click", async (e) => {
          e.stopPropagation();
          // Off the page, and the Roon playlist with it or not: asked first
          const choice = await wrappedConfirm({
            title: `Take "${d.name}" off this page?`,
            text: d.playlist ? "The playlist in Roon can stay, or go with it. Deleting it from Roon can't be undone." : "Its playlist isn't in Roon any more.",
            actions: d.playlist
              ? [
                  { value: "delete", label: "Delete the playlist too", kind: "danger" },
                  { value: "keep", label: "Keep the playlist" },
                  { value: null, label: "Cancel" },
                ]
              : [
                  { value: "keep", label: "Take it off" },
                  { value: null, label: "Cancel" },
                ],
          });
          if (!choice) return;
          try {
            if (choice === "delete") await invoke("library_delete_playlist", { handle: d.playlist.handle });
            diveLists = (await invoke("wrapped_dive_forget", { builtAt: d.builtAt })).dives;
            renderDivePiece();
            card.remove();
            if (choice === "delete") wrappedToast(`Deleted "${d.name}" from Roon.`);
          } catch (err) {
            wrappedToast(String(err));
          }
        });
        top.append(forget);
        const list = document.createElement("ol");
        list.className = "dive-made-songs";
        for (const s of d.songs) {
          const li = document.createElement("li");
          const what = document.createElement("span");
          what.className = "dive-made-song";
          what.textContent = `${s.artist} - ${s.title}`;
          const why = document.createElement("span");
          why.className = "dive-made-why";
          // (with tier 3: how close it sounds to the picks, from listening)
          const sound = s.sound != null ? `sounds ${s.sound.toFixed(2)} like your picks` : null;
          why.textContent = [s.inLibrary ? "In your library" : "New to you", s.album, s.why, sound].filter(Boolean).join(" · ");
          li.append(what, why);
          list.append(li);
        }
        top.addEventListener("click", () => {
          if (diveOpen.has(d.builtAt)) diveOpen.delete(d.builtAt);
          else diveOpen.add(d.builtAt);
          card.classList.toggle("open");
        });
        card.append(top, list);
        box.append(card);
      }
    })
    .catch(() => {});
  return box;
}

// The Radio tab: the radio page for one of the picks (the newest unless
// another is chosen); a song plays Roon Radio from itself; no picks, a
// search for an artist (which becomes a pick)
function diveRadioTab() {
  if (!diveSeeds.length) {
    wrappedBody.append(wrappedNote("Start the radio from an artist, or pick songs, albums or artists with their brain buttons.", "wrapped-note small"));
    return radioPickSeed((item) => {
      diveSeeds.push({ kind: "artist", handle: item.handle, title: item.title, imageKey: item.imageKey, radio: { name: item.title, handle: item.handle } });
      renderWrapped();
    });
  }
  const at = diveRadioAt >= 0 && diveRadioAt < diveSeeds.length ? diveRadioAt : diveSeeds.length - 1;
  if (diveSeeds.length > 1) {
    const pills = radioPills(
      diveSeeds.map((s, i) => [String(i), `${s.title} (${s.kind})`]),
      (v) => Number(v) === at,
      (v) => {
        diveRadioAt = Number(v);
        renderWrapped();
      },
    );
    const from = radioControl("Start from", pills.row);
    from.classList.add("dive-radio-from");
    wrappedBody.append(from);
  }
  const seed = diveSeeds[at];
  if (seed.kind === "song") {
    const title = document.createElement("div");
    title.className = "wrapped-radio-subject";
    title.textContent = [seed.title, seed.artist].filter(Boolean).join(" · ");
    const start = document.createElement("button");
    start.className = "wrapped-play wrapped-radio-start";
    start.innerHTML = `${RADIO_ICON}<span>Start radio</span>`;
    start.addEventListener("click", () => startRoonRadio("song", { tracks: [seed.handle] }, start));
    wrappedBody.append(title, wrappedNote("Roon Radio from this song.", "wrapped-note small"), start);
    return;
  }
  if (!seed.radio?.name) return wrappedBody.append(wrappedNote("Roon didn't say who the artist is.", "wrapped-note error"));
  wrappedRadioPage(seed.radio, ++wrappedTicket);
}

// A Roon handle for an artist row (Similar rows carry a performer id or
// nothing): by id, else Roon's search, exact name only. A name can be
// several artists: Last.fm's page for a name lumps them together, and
// Roon's first hit may be another one (Absent in Body's neighbour "Kingdom"
// came out as the electronic producer). With `like` (the starting artist's
// Roon genres) a name match counts only if it shares one of them; one
// Roon has no genres for is taken when nothing better turns up.
const roonNameHits = new Map(); // lower-cased name -> exact-name ArtistCards
async function roonArtistHandle(row, like = []) {
  if (row.handle) return row.handle;
  if (row.performerId) {
    try {
      const card = await invoke("library_artist_by_id", { performerId: row.performerId });
      if (card?.handle) return (row.handle = card.handle);
    } catch {}
  }
  const key = row.name.toLowerCase();
  let hits = roonNameHits.get(key);
  if (hits === undefined) {
    try {
      const found = await invoke("library_search", { query: row.name });
      hits = found.artists.filter((a) => a.name.toLowerCase() === key);
    } catch {
      hits = [];
    }
    roonNameHits.set(key, hits);
  }
  if (!like.length) return hits[0] ? (row.handle = hits[0].handle) : null;
  const wanted = new Set(like.map((g) => g.toLowerCase()));
  let unknown = null;
  for (const hit of hits) {
    const genres = await roonGenres(hit.handle);
    if (genres.some((g) => wanted.has(g.toLowerCase()))) return (row.handle = hit.handle);
    if (!genres.length && !unknown) unknown = hit;
  }
  if (hits.length && !unknown) console.info(`[radio] left out ${row.name}: Roon's ${row.name} shares no genre with the starting artist`);
  return unknown ? (row.handle = unknown.handle) : null;
}

// Roon's genres for an artist (its Performer's), once per handle
const roonGenreCache = new Map();
function roonGenres(handle) {
  if (!handle) return Promise.resolve([]);
  if (!roonGenreCache.has(handle)) {
    roonGenreCache.set(
      handle,
      invoke("library_artist_facts", { handle })
        .then((facts) => facts?.genres || [])
        .catch(() => []),
    );
  }
  return roonGenreCache.get(handle);
}

// No artist yet: Roon's search, artists only (`onPick`: what a click does;
// the radio page for the artist by default)
function radioPickSeed(onPick = (item) => openWrappedPage({ page: "radio", name: item.title, handle: item.handle })) {
  const input = document.createElement("input");
  input.className = "wrapped-search";
  input.type = "search";
  input.placeholder = "An artist to start the radio from";
  const results = document.createElement("div");
  results.className = "wrapped-results";
  wrappedBody.append(input, results);
  let timer = null;
  const run = async () => {
    const query = input.value.trim();
    if (!query) return results.replaceChildren();
    const ticket = ++wrappedTicket;
    results.replaceChildren(wrappedNote("Searching..."));
    let hits;
    try {
      hits = await invoke("library_search", { query });
    } catch (err) {
      if (ticket === wrappedTicket) results.replaceChildren(wrappedNote(String(err), "wrapped-note error"));
      return;
    }
    if (ticket !== wrappedTicket || input.value.trim() !== query) return;
    results.replaceChildren();
    if (!wrappedPicks(results, "Artists", hits.artists.map(artistItem), onPick)) {
      results.append(wrappedNote("No artists by that name."));
    }
  };
  input.addEventListener("input", () => {
    clearTimeout(timer);
    timer = setTimeout(run, 350);
  });
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      clearTimeout(timer);
      run();
    }
  });
  setTimeout(() => input.focus(), 50);
}

// A row of pills; `selected(value)` marks them, `pick(value)` on a click
function radioPills(options, selected, pick) {
  const row = document.createElement("div");
  row.className = "wrapped-tabs small";
  const paint = () => {
    for (const pill of row.children) pill.classList.toggle("selected", selected(pill.dataset.value));
  };
  for (const [value, label] of options) {
    const pill = document.createElement("button");
    pill.className = "wrapped-tab";
    pill.dataset.value = value;
    pill.textContent = label;
    pill.addEventListener("click", () => {
      pick(value);
      paint();
    });
    row.append(pill);
  }
  paint();
  return { row, paint };
}

function radioControl(label, control) {
  const line = document.createElement("div");
  line.className = "wrapped-radio-line";
  const name = document.createElement("span");
  name.className = "wrapped-radio-label";
  name.textContent = label;
  line.append(name, control);
  return line;
}

async function wrappedRadioPage(entry, ticket) {
  const subject = { artist: entry.name, album: entry.album || null };
  const head = document.createElement("div");
  head.className = "wrapped-radio-head";
  const controls = document.createElement("div");
  controls.className = "wrapped-radio-controls";
  const start = document.createElement("button");
  start.className = "wrapped-play wrapped-radio-start";
  start.innerHTML = `${RADIO_ICON}<span>Start radio</span>`;
  const status = document.createElement("div");
  status.className = "wrapped-note small";
  const seedsHead = wrappedSection("Seeds");
  const seeds = document.createElement("div");
  wrappedBody.append(head, controls, start, status, seedsHead, seeds);

  // The artist (and album) it starts from
  const title = document.createElement("div");
  title.className = "wrapped-radio-subject";
  title.textContent = entry.album ? `${entry.album} · ${entry.name}` : entry.name;
  head.append(title);

  let rows = [];
  const left = new Set(); // names clicked out of the seeds
  let load = 0;
  const discover = () => radioPrefs.by !== "roon" && radioPrefs.scope === "discover";

  const libraryPill = document.createElement("button");
  const livePill = document.createElement("button");
  const savePill = document.createElement("button");
  const paintSwitches = () => {
    livePill.classList.toggle("selected", radioPrefs.includeLive);
    savePill.classList.toggle("selected", radioPrefs.savePlaylist);
    libraryPill.classList.toggle("selected", radioPrefs.libraryOnly && !discover());
    libraryPill.disabled = discover();
    libraryPill.title = discover() ? "Discover seeds are outside your library" : "Play only tracks in your library";
  };
  for (const [pill, label, key, tip] of [
    [livePill, "Include live recordings", "includeLive", "Live albums and songs are left out unless this is on"],
    [libraryPill, "Library tracks only", "libraryOnly", ""],
    [savePill, "Save as playlist", "savePlaylist", "Also keep the mix as a Roon playlist"],
  ]) {
    pill.className = "wrapped-tab";
    pill.textContent = label;
    if (tip) pill.title = tip;
    pill.addEventListener("click", () => {
      radioPrefs[key] = !radioPrefs[key];
      saveRadioPrefs();
      paintSwitches();
      paintStatus();
    });
  }
  const switches = document.createElement("div");
  switches.className = "wrapped-tabs small";
  switches.append(livePill, libraryPill, savePill);

  const by = radioPills(RADIO_BY, (v) => v === radioPrefs.by, (v) => {
    radioPrefs.by = v;
    saveRadioPrefs();
    scopeLine.hidden = v === "roon";
    paintSwitches();
    fill();
  });
  const scope = radioPills(SIMILAR_SCOPES, (v) => v === radioPrefs.scope, (v) => {
    radioPrefs.scope = v;
    saveRadioPrefs();
    paintSwitches();
    fill();
  });
  const count = radioPills(RADIO_COUNTS.map((n) => [String(n), String(n)]), (v) => Number(v) === radioPrefs.count, (v) => {
    radioPrefs.count = Number(v);
    saveRadioPrefs();
    fill();
  });
  const perArtist = radioPills(RADIO_PER_ARTIST.map((n) => [String(n), String(n)]), (v) => Number(v) === radioPrefs.perArtist, (v) => {
    radioPrefs.perArtist = Number(v);
    saveRadioPrefs();
    paintStatus();
  });
  const scopeLine = radioControl("From", scope.row);
  scopeLine.hidden = radioPrefs.by === "roon";
  controls.append(radioControl("Match by", by.row), scopeLine, radioControl("Artists", count.row), radioControl("Songs each", perArtist.row), radioControl("Play", switches));
  paintSwitches();

  const chosen = () => rows.filter((r) => !left.has(r.name));
  // Several artists go to Roon's radio as "these artists only": the Core
  // queues their songs, shuffled (client.rs, RadioSeeds). Roon's own radio
  // from one artist is in Roon already, so it isn't offered here.
  const paintStatus = () => {
    const n = chosen().length;
    const on = zone ? ` on ${zone.name}` : "";
    const each = radioPrefs.perArtist;
    const top = each === 1 ? "The top song" : `Up to ${each} top songs`;
    const live = radioPrefs.includeLive ? "" : ", no live recordings";
    if (discover()) {
      status.textContent = n
        ? `${top} by each of the ${n} artist${n === 1 ? "" : "s"} below (none by ${entry.name}), one per album${live}, mixed so no artist plays twice in a row${on}. Click an artist to leave it out.`
        : "No artists left to play; Discover leaves out the one it starts from.";
      return;
    }
    status.textContent = n
      ? `${top} by ${entry.name} and each of the ${n} artist${n === 1 ? "" : "s"} below, one per album${live}, mixed so no artist plays twice in a row${on}. Click an artist to leave it out.`
      : `${top} by ${entry.name} alone${live}${on}.`;
  };
  const paint = (list, pending) => {
    rows = list;
    seeds.replaceChildren();
    if (rows.length) {
      const cards = wrappedGrid(
        rows.map((r) => ({ row: r, title: r.name, sub: (r.why || []).slice(0, 3).join(" · "), imageKey: r.imageKey, round: true })),
        (item) => {
          if (left.has(item.row.name)) left.delete(item.row.name);
          else left.add(item.row.name);
          paintLeft();
          paintStatus();
        },
      );
      seeds.append(cards);
      wrappedFillPhotos(rows, cards, ticket);
      paintLeft();
    } else if (!pending) {
      seeds.append(wrappedNote("No artists to add; the radio would start from this one alone. Try another Match by or From."));
    }
    if (pending) seeds.append(wrappedNote(`Tagging ${pending} more neighbour${pending === 1 ? "" : "s"} from Last.fm...`, "wrapped-note small"));
    paintStatus();
  };
  const paintLeft = () => {
    const tiles = seeds.querySelectorAll(".wrapped-card");
    rows.forEach((r, i) => tiles[i]?.classList.toggle("left-out", left.has(r.name)));
  };

  const fill = async () => {
    const mine = ++load;
    seeds.replaceChildren(wrappedNote(radioPrefs.by === "roon" ? "Asking Roon..." : discover() ? "Asking Last.fm for neighbours..." : "Comparing tags..."));
    try {
      if (radioPrefs.by === "roon") {
        const x = entry.albumHandle
          ? await invoke("library_album_extras", { handle: entry.albumHandle })
          : await invoke("library_artist_extras", { handle: entry.handle });
        if (mine !== load || ticket !== wrappedTicket) return;
        const banned = new Set((await invoke("wrapped_ban_list").catch(() => [])).map((b) => b.name.toLowerCase()));
        const list = ((entry.albumHandle ? x.similarArtists : x.similar) || []).filter((p) => !banned.has((p.name || "").toLowerCase()));
        return paint(list.slice(0, radioPrefs.count).map((p) => ({ name: p.name, handle: p.handle, imageKey: p.imageKey, performerId: p.performerId, why: [] })), 0);
      }
      const answer = await invoke("wrapped_similar", {
        ...subject,
        by: radioPrefs.by,
        scope: radioPrefs.scope,
        limit: radioPrefs.count,
        neighboursOnly: true,
      });
      if (mine !== load || ticket !== wrappedTicket) return;
      if (!answer.rows.length && !answer.pending && answer.subjectEmpty) {
        rows = [];
        seeds.replaceChildren(wrappedNote(`Last.fm has nothing for ${entry.name} to match ${radioPrefs.by === "blend" ? "genres" : radioPrefs.by} on. Try Genre or Roon's similar.`));
        return paintStatus();
      }
      paint(answer.rows, answer.pending);
      if (!answer.byListeners) {
        seeds.prepend(
          wrappedNote(`Last.fm lists no listener neighbours for ${entry.name}, so these are matched by tags alone.`, "wrapped-note small"),
        );
      }
      if (answer.pending) {
        const unlisten = await listen("wrapped-similar", (event) => {
          if (mine !== load || ticket !== wrappedTicket) return unlisten();
          if (event.payload.key !== answer.key) return;
          paint(event.payload.rows, event.payload.pending);
          if (!event.payload.pending) unlisten();
        });
      }
    } catch (err) {
      if (mine === load && ticket === wrappedTicket) seeds.replaceChildren(wrappedNote(String(err), "wrapped-note error"));
    }
  };

  start.addEventListener("click", async () => {
    if (!zone) return wrappedToast("Pick a zone first.");
    start.disabled = true;
    start.classList.add("busy");
    // The big spinner over the page while the seeds are found in Roon and
    // Roon gathers their songs (up to 50 lookups, then the queue)
    const picked = chosen();
    wrappedBusy(`Finding ${picked.length ? `${picked.length + 1} artists` : entry.name} in Roon...`);
    try {
      // (five lookups at a time, in the seeds' order; artists found by name
      // must share a Roon genre with the starting one)
      const like = await roonGenres(entry.handle);
      const found = new Array(picked.length).fill(null);
      let done = 0;
      for (let i = 0; i < picked.length; i += 5) {
        await Promise.all(
          picked.slice(i, i + 5).map(async (row, j) => {
            found[i + j] = await roonArtistHandle(row, like);
            done += 1;
            wrappedBusy(`Finding artists in Roon... ${done} of ${picked.length}`);
          }),
        );
      }
      const handles = found.filter(Boolean);
      const count = handles.length + (discover() ? 0 : 1);
      wrappedBusy(`Picking songs from ${count} artist${count === 1 ? "" : "s"}...`);
      // (Discover leaves the starting artist out: only artists you don't have)
      const artists = [...new Set([discover() ? null : entry.handle, ...handles].filter(Boolean))];
      if (!artists.length) throw "Roon didn't find any of these artists.";
      // Wrapped picks the songs itself (client.rs, play_artist_mix): Roon's
      // own radio takes several artists only as "these artists only" and
      // then queues all their songs, whole albums of one artist at a time
      const played = await invoke("library_play_artist_mix", {
        zoneId: zone.zoneId,
        artists,
        options: {
          perArtist: radioPrefs.perArtist,
          includeLive: radioPrefs.includeLive,
          libraryOnly: radioPrefs.libraryOnly && !discover(),
        },
      });
      const missed = picked.length - handles.length;
      // The same mix kept as a Roon playlist, when asked
      let saved = null;
      let unsaved = null;
      if (radioPrefs.savePlaylist) {
        wrappedBusy("Saving the mix as a playlist...");
        const name = `${WRAPPED_PREFIX}Radio: ${entry.name}${discover() ? " (Discover)" : ""}, ${playlistStamp()}`;
        try {
          saved = await invoke("library_create_playlist", { name, tracks: played.tracks });
        } catch (err) {
          console.error("radio playlist", err);
          unsaved = `not saved as a playlist: ${typeof err === "string" ? err : "Roon said no"}`;
        }
      }
      const notes = [
        missed ? `${missed} not found in Roon` : null,
        played.liveSkipped ? `${played.liveSkipped} live left out` : null,
        saved ? `saved as "${saved.name}"` : null,
        unsaved,
      ].filter(Boolean);
      wrappedToast(
        `Radio on ${zone.name}: ${played.songs} songs by ${played.artists} artist${played.artists === 1 ? "" : "s"}${notes.length ? ` (${notes.join(", ")})` : ""}`,
      );
    } catch (err) {
      console.error("radio", err);
      wrappedToast(typeof err === "string" ? err : "Roon didn't start the radio.");
    } finally {
      wrappedBusy(null);
      start.disabled = false;
      start.classList.remove("busy");
    }
  });

  fill();
}

// ----- The artist page's discography, sorted and filtered like Roon's -----

const ALBUM_SORTS = [
  ["date", "Date"],
  ["popular", "Popularity"],
  ["title", "Title"],
];
// Newest, most popular, A to Z first
const ALBUM_SORT_DESC = { date: true, popular: true, title: false };
const ALBUM_GROUPS = [
  ["main", "Albums"],
  ["other", "Singles & EPs"],
  ["appearance", "Appears on"],
];
// Remembered between pages and runs (a convenience, so storage may fail)
let albumView = { sort: "date", desc: true, library: false, favorites: false };
try {
  albumView = { ...albumView, ...JSON.parse(localStorage.getItem("wrapped-album-view") || "{}") };
} catch {}
function saveAlbumView() {
  try {
    localStorage.setItem("wrapped-album-view", JSON.stringify(albumView));
  } catch {}
}

// "15 August 2025", "August 2025" or "2025", as Roon writes them
function releasedText(a) {
  const [y, m, d] = (a.released || "").split("-").map(Number);
  if (y && m && d) return new Date(y, m - 1, d).toLocaleDateString(undefined, { day: "numeric", month: "long", year: "numeric" });
  if (y && m) return new Date(y, m - 1, 1).toLocaleDateString(undefined, { month: "long", year: "numeric" });
  return a.year ? String(a.year) : "";
}

const albumKey = (a) => (a.albumId != null ? `id:${a.albumId}` : `t:${a.title.toLowerCase().replace(/[^\p{L}\p{N}]/gu, "")}`);

// Sorts a copy of `list`. Popularity is Roon's own score when the Core sends
// one, else the order of the albums Roon recommends for the artist, the rest
// after them newest first.
function sortAlbums(list, sort, desc, recommended) {
  const dateOf = (a) => a.released || (a.year ? `${a.year}-00-00` : "");
  const byDate = (a, b) => dateOf(a).localeCompare(dateOf(b));
  const scored = list.some((a) => a.popularity != null);
  const rank = new Map(recommended.map((a, i) => [albumKey(a), recommended.length - i]));
  const popularity = (a) => (scored ? a.popularity ?? -1 : rank.get(albumKey(a)) ?? 0);
  const compare = {
    date: byDate,
    popular: (a, b) => popularity(a) - popularity(b) || byDate(a, b),
    title: (a, b) => a.title.localeCompare(b.title, undefined, { sensitivity: "base", numeric: true }),
  }[sort];
  const known = (a) => (sort === "date" ? Boolean(dateOf(a)) : true);
  const sorted = list.filter(known).sort((a, b) => (desc ? -compare(a, b) : compare(a, b)));
  // (Undated albums last either way)
  return [...sorted, ...list.filter((a) => !known(a))];
}

function wrappedDiscography(holder, albums, picks) {
  const tools = document.createElement("div");
  tools.className = "wrapped-album-tools";
  const filters = document.createElement("div");
  filters.className = "wrapped-tabs small";
  const sorts = document.createElement("div");
  sorts.className = "wrapped-tabs small sorts";
  tools.append(filters, sorts);
  const lists = document.createElement("div");
  holder.append(tools, lists);
  let recommended = [];
  const render = () => {
    for (const pill of filters.children) pill.classList.toggle("selected", Boolean(albumView[pill.dataset.filter]));
    for (const pill of sorts.children) {
      const on = pill.dataset.sort === albumView.sort;
      pill.classList.toggle("selected", on);
      pill.textContent = pill.dataset.label + (on ? (albumView.desc ? " ↓" : " ↑") : "");
    }
    const shown = albums.filter((a) => (!albumView.library || a.inLibrary) && (!albumView.favorites || a.favorite));
    lists.replaceChildren();
    for (const [group, label] of ALBUM_GROUPS) {
      const list = shown.filter((a) => a.group === group);
      if (!list.length) continue;
      const items = sortAlbums(list, albumView.sort, albumView.desc, recommended).map((a) => ({
        handle: a.handle,
        title: a.title,
        sub: [releasedText(a), a.inLibrary ? "In library" : null, a.favorite ? "♥" : null].filter(Boolean).join(" · "),
        imageKey: a.imageKey,
      }));
      lists.append(wrappedSection(`${label} (${list.length})`), wrappedGrid(items, openAlbum));
    }
    if (!lists.children.length) {
      lists.append(wrappedNote(`None of this artist's albums are ${albumView.library ? "in your library" : ""}${albumView.library && albumView.favorites ? " and " : ""}${albumView.favorites ? "hearted" : ""}.`));
    }
  };
  for (const [key, label] of [
    ["library", "In library"],
    ["favorites", "♥ Favorites"],
  ]) {
    const pill = document.createElement("button");
    pill.className = "wrapped-tab";
    pill.dataset.filter = key;
    pill.textContent = label;
    pill.addEventListener("click", () => {
      albumView[key] = !albumView[key];
      saveAlbumView();
      render();
    });
    filters.append(pill);
  }
  for (const [key, label] of ALBUM_SORTS) {
    const pill = document.createElement("button");
    pill.className = "wrapped-tab";
    pill.dataset.sort = key;
    pill.dataset.label = label;
    pill.addEventListener("click", () => {
      // The selected one flips its direction
      albumView.desc = albumView.sort === key ? !albumView.desc : ALBUM_SORT_DESC[key];
      albumView.sort = key;
      saveAlbumView();
      render();
    });
    sorts.append(pill);
  }
  render();
  // Roon's recommended albums arrive with its other picks
  picks.then(({ x }) => {
    if (!x?.recommended?.length || !holder.isConnected) return;
    recommended = x.recommended;
    if (albumView.sort === "popular") render();
  });
}

// The Home page's rows, from the last build; while a build runs, its
// progress, and the rows again when it's done ("wrapped-home" events)
let homeUnlisten = null;
async function wrappedHome(holder, ticket, refresh = false) {
  if (homeUnlisten) {
    homeUnlisten();
    homeUnlisten = null;
  }
  let status;
  try {
    status = await invoke("wrapped_home", { refresh });
  } catch (err) {
    if (ticket === wrappedTicket) holder.replaceChildren(wrappedNote(String(err), "wrapped-note error"));
    return;
  }
  if (ticket !== wrappedTicket) return;
  paintHome(holder, status, ticket);
  // (a build's progress and end, a ban, a daily look for albums: all
  // come as events while the page shows)
  homeUnlisten = await listen("wrapped-home", (event) => {
    if (ticket !== wrappedTicket) {
      homeUnlisten?.();
      homeUnlisten = null;
      return;
    }
    paintHome(holder, event.payload, ticket);
  });
}

function paintHome(holder, status, ticket) {
  // (the progress line only changes while the rows stay)
  let note = holder.querySelector(".wrapped-home-progress");
  if (status.building) {
    if (!note) {
      note = wrappedNote("", "wrapped-note small wrapped-home-progress");
      holder.prepend(note);
    }
    note.textContent = status.progress || "Looking at what you listen to...";
    if (holder.querySelector(".wrapped-home-rows")) return;
  } else {
    note?.remove();
  }
  const rows = document.createElement("div");
  rows.className = "wrapped-home-rows";
  const empty = !status.artists.length && !status.newFromFavorites.length && !status.newFromSuggested.length;
  if (status.error && empty) rows.append(wrappedNote(status.error, "wrapped-note error"));
  else if (empty && !status.building) {
    rows.append(wrappedNote(wrappedSettings?.lastfmKey ? "Nothing to show yet: play some music and come back." : "Artists for you need a Last.fm key (Settings, tier 1); new albums come from your listening history."));
  }
  const albumItem = (a) => ({
    albumId: a.albumId,
    performerId: a.performerId,
    artist: a.artist,
    title: a.title,
    sub: `${a.artist} · ${homeDate(a.released)}`,
    imageKey: a.imageKey,
    why: a.why,
  });
  const openHomeAlbum = (item) => openWrappedPage({ page: "album", albumId: item.albumId });
  const ban = (artist) => async () => {
    const choice = await wrappedConfirm({
      title: `Add ${artist} to the ban list?`,
      text: "Never suggested again: not here, not by Discover, Deep Dive or the radio. The ban list is on the Playlists page.",
      actions: [
        { value: "ban", label: "Add to ban list", kind: "danger" },
        { value: null, label: "Cancel" },
      ],
    });
    if (choice !== "ban") return;
    try {
      await invoke("wrapped_ban_add", { artist });
      wrappedToast(`${artist} is on the ban list.`);
    } catch (err) {
      wrappedToast(String(err));
    }
  };
  if (status.artists.length) {
    rows.append(
      homeRow(
        "artists",
        "Artists for you",
        status.artists.map((a) => ({ performerId: a.performerId, title: a.name, artist: a.name, sub: a.seeds?.length ? `Like ${a.seeds.slice(0, 2).join(", ")}` : "", why: a.because, imageKey: a.imageKey, round: true })),
        (item) => openWrappedPage({ page: "artist", performerId: item.performerId }),
        ban,
      ),
    );
  }
  if (status.newFromFavorites.length) rows.append(homeRow("favorites", "New albums from artists you listen to", status.newFromFavorites.map(albumItem), openHomeAlbum, ban));
  if (status.newFromSuggested.length) rows.append(homeRow("suggested", "New albums from artists you may like", status.newFromSuggested.map(albumItem), openHomeAlbum, ban));
  const foot = document.createElement("div");
  foot.className = "wrapped-home-foot";
  if (status.builtAt) {
    foot.append(wrappedNote(`Put together ${historyWhen(status.builtAt)}. Artists new to you that sound like your favorites over all time and your library (by tags, not the last few weeks like Discover), and albums from the last year.`, "wrapped-note small"));
  }
  if (!status.building) {
    const again = document.createElement("button");
    again.className = "wrapped-button small";
    again.textContent = "Refresh";
    again.addEventListener("click", () => wrappedHome(holder, ticket, true));
    foot.append(again);
  }
  rows.append(foot);
  holder.querySelector(".wrapped-home-rows")?.remove();
  holder.append(rows);
}

// A Home row: one row of cards, as many as fit the width; its title opens
// it to three rows, and "Keep looking" three more at a time. Each card's
// X (on hover) bans the artist. `open` is remembered per row.
const homeOpen = {};
function homeRow(id, title, items, onOpen, ban) {
  const box = document.createElement("section");
  box.className = "wrapped-home-row";
  const head = document.createElement("button");
  head.className = "wrapped-home-head";
  head.innerHTML = `<span>${title}</span><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg>`;
  head.title = "Show more";
  const grid = document.createElement("div");
  grid.className = "wrapped-cards";
  const more = document.createElement("button");
  more.className = "wrapped-button wrapped-home-more";
  more.textContent = "Keep looking";
  box.append(head, grid, more);
  let rows = homeOpen[id] || 1;
  let cols = 0;
  const paint = () => {
    const width = grid.clientWidth;
    if (!width) return;
    const fit = Math.max(1, Math.floor((width + 14) / (150 + 14)));
    const shown = Math.min(items.length, fit * rows);
    if (fit === cols && grid.childElementCount === shown) return;
    cols = fit;
    grid.replaceChildren(
      ...items.slice(0, shown).map((item) => {
        const wrap = document.createElement("div");
        wrap.className = "wrapped-card-wrap";
        wrap.append(wrappedCard(item, () => onOpen(item)));
        if (item.artist) {
          const x = document.createElement("button");
          x.className = "discover-ask-x wrapped-card-ban";
          x.innerHTML = CLOSE_ICON;
          x.title = "Add to ban list";
          x.setAttribute("aria-label", `Add ${item.artist} to the ban list`);
          x.addEventListener("click", (event) => {
            event.stopPropagation();
            ban(item.artist)();
          });
          wrap.append(x);
        }
        return wrap;
      }),
    );
    box.classList.toggle("open", rows > 1);
    more.hidden = rows <= 1 || shown >= items.length;
    head.title = rows > 1 ? "Show one row" : "Show more";
  };
  head.addEventListener("click", () => {
    rows = rows > 1 ? 1 : 3;
    homeOpen[id] = rows;
    paint();
  });
  more.addEventListener("click", () => {
    rows += 3;
    homeOpen[id] = rows;
    paint();
  });
  new ResizeObserver(paint).observe(grid);
  requestAnimationFrame(paint);
  return box;
}

// "2026-09-12" as "Sep 12, 2026" ("2026" alone when only the year is known)
function homeDate(d) {
  const [y, m, day] = (d || "").split("-").map(Number);
  if (!y) return "";
  if (!m) return String(y);
  const name = new Date(Date.UTC(y, m - 1, day || 1)).toLocaleDateString(undefined, { month: "short", day: day ? "numeric" : undefined, year: "numeric", timeZone: "UTC" });
  return name;
}

// Roon's play history, from Wrapped's own copy of it (wrapped.rs keeps it
// on disk and syncs it from the Core): the newest plays, and the most
// played artists, albums and tracks over a period
const HISTORY_PERIODS = [
  [28, "4 weeks"],
  [91, "3 months"],
  [365, "A year"],
  [null, "All time"],
];
let historyStatus = null;

function historyProgress(holder, p) {
  const n = (x) => x.toLocaleString();
  if (p.error) holder.replaceChildren(wrappedNote(p.error, "wrapped-note error"));
  else if (p.done) holder.replaceChildren();
  else if (!p.wanted) holder.replaceChildren(wrappedNote("Checking Roon for new plays..."));
  else holder.replaceChildren(wrappedNote(`Reading your history from Roon: ${n(p.loaded)} of ${n(p.wanted)} plays...`));
}

// ----- The library's artists and albums (the copy Settings reads) -----

// A list that fills in as you scroll (a library holds thousands):
// `make` turns an item into its element
function wrappedLazyList(items, make, className, batch = 240) {
  const holder = document.createElement("div");
  const grid = document.createElement("div");
  grid.className = className;
  const more = document.createElement("div");
  more.className = "wrapped-more-sentinel";
  let shown = 0;
  const fill = () => {
    const next = items.slice(shown, shown + batch);
    grid.append(...next.map(make));
    shown += next.length;
    if (shown >= items.length) more.remove();
  };
  fill();
  holder.append(grid, more);
  if (shown < items.length) {
    const watcher = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) fill();
        if (shown >= items.length) watcher.disconnect();
      },
      { root: wrappedBody, rootMargin: "600px" },
    );
    watcher.observe(more);
  }
  return holder;
}

function wrappedLazyGrid(items, onClick) {
  return wrappedLazyList(items, (item) => wrappedCard(item, () => onClick(item)), "wrapped-cards");
}

// The library read from Roon on first use (Settings' "Read the library"),
// with its progress; `done` runs once it is there
function wrappedLibraryRead(ticket, done) {
  const note = wrappedNote("Wrapped hasn't read your library yet.");
  const read = document.createElement("button");
  read.className = "wrapped-button";
  read.textContent = "Read the library";
  const progress = document.createElement("div");
  read.addEventListener("click", async () => {
    try {
      await invoke("wrapped_library_sync");
      read.disabled = true;
      progress.replaceChildren(wrappedNote("Reading..."));
    } catch (err) {
      wrappedToast(String(err));
    }
  });
  wrappedBody.append(note, read, progress);
  listen("wrapped-library", (event) => {
    if (ticket !== wrappedTicket) return;
    const p = event.payload;
    if (p.error) wrappedToast(p.error);
    if (p.done) done();
    else progress.replaceChildren(wrappedNote(`Reading: ${p.read.toLocaleString()} of ${p.total.toLocaleString()} ${p.what || "artists"}...`));
  }).then((unlisten) => {
    const stop = setInterval(() => {
      if (ticket !== wrappedTicket) {
        unlisten();
        clearInterval(stop);
      }
    }, 2000);
  });
}

// A filter box over a list, matching as you type
function wrappedFilter(placeholder, onChange) {
  const input = document.createElement("input");
  input.className = "wrapped-search wrapped-filter";
  input.type = "search";
  input.placeholder = placeholder;
  input.autocomplete = "off";
  input.spellcheck = false;
  let timer = null;
  input.addEventListener("input", () => {
    clearTimeout(timer);
    timer = setTimeout(() => onChange(input.value.trim().toLowerCase()), 150);
  });
  return input;
}

WRAPPED_PAGES.artists = async function (entry) {
  wrappedHeading("Artists", "Every artist in your library");
  if (wrappedNeedsLibrary()) return;
  const ticket = ++wrappedTicket;
  let artists;
  try {
    artists = await invoke("wrapped_library_artists");
  } catch (err) {
    if (ticket === wrappedTicket) wrappedFailed(err);
    return;
  }
  if (ticket !== wrappedTicket) return;
  if (!artists.length) return wrappedLibraryRead(ticket, renderWrapped);
  artists.sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: "base", numeric: true }));
  wrappedHeading("Artists", `${artists.length.toLocaleString()} artists in your library`);
  const holder = document.createElement("div");
  const show = (filter) => {
    const list = filter ? artists.filter((a) => a.name.toLowerCase().includes(filter)) : artists;
    holder.replaceChildren(
      wrappedLazyGrid(
        list.map((a) => ({
          title: a.name,
          sub: a.albumCount ? `${a.albumCount} album${a.albumCount === 1 ? "" : "s"}` : "",
          imageKey: a.imageKey,
          round: true,
          performerId: a.performerId,
        })),
        (a) => openWrappedPage({ page: "artist", performerId: a.performerId, name: a.title }),
      ),
    );
    if (!list.length) holder.replaceChildren(wrappedNote("No artist by that name."));
  };
  entry.filter ||= "";
  const filter = wrappedFilter("Filter artists", (text) => {
    entry.filter = text;
    show(text);
  });
  filter.value = entry.filter;
  wrappedBody.append(filter, holder);
  show(entry.filter);
};

WRAPPED_PAGES.albums = async function (entry) {
  wrappedHeading("Albums", "Every album in your library");
  if (wrappedNeedsLibrary()) return;
  const ticket = ++wrappedTicket;
  let albums;
  try {
    albums = await invoke("wrapped_library_albums");
  } catch (err) {
    if (ticket === wrappedTicket) wrappedFailed(err);
    return;
  }
  if (ticket !== wrappedTicket) return;
  if (!albums.length) return wrappedLibraryRead(ticket, renderWrapped);
  const byName = (a, b) => a.localeCompare(b, undefined, { sensitivity: "base", numeric: true });
  const sorts = {
    artist: (a, b) => byName(a.artist, b.artist) || (a.year || 0) - (b.year || 0) || byName(a.title, b.title),
    title: (a, b) => byName(a.title, b.title) || byName(a.artist, b.artist),
    year: (a, b) => (b.year || 0) - (a.year || 0) || byName(a.artist, b.artist) || byName(a.title, b.title),
  };
  entry.sort ||= "artist";
  entry.filter ||= "";
  wrappedHeading("Albums", `${albums.length.toLocaleString()} albums in your library`);
  const tabs = document.createElement("div");
  tabs.className = "wrapped-tabs small";
  const holder = document.createElement("div");
  const show = () => {
    for (const tab of tabs.querySelectorAll(".wrapped-tab")) tab.classList.toggle("selected", tab.dataset.sort === entry.sort);
    const f = entry.filter;
    const list = (f ? albums.filter((a) => a.title.toLowerCase().includes(f) || a.artist.toLowerCase().includes(f)) : albums.slice()).sort(sorts[entry.sort]);
    holder.replaceChildren(
      wrappedLazyGrid(
        list.map((a) => ({
          title: a.title,
          sub: [a.artist, a.year].filter(Boolean).join(" \u00b7 "),
          imageKey: a.imageKey,
          albumId: a.albumId,
        })),
        (a) => openWrappedPage({ page: "album", albumId: a.albumId }),
      ),
    );
    if (!list.length) holder.replaceChildren(wrappedNote("No album by that name."));
  };
  for (const [sort, label] of [
    ["artist", "By artist"],
    ["title", "By title"],
    ["year", "By year"],
  ]) {
    const tab = document.createElement("button");
    tab.className = "wrapped-tab";
    tab.dataset.sort = sort;
    tab.textContent = label;
    tab.addEventListener("click", () => {
      entry.sort = sort;
      show();
    });
    tabs.append(tab);
  }
  const filter = wrappedFilter("Filter albums", (text) => {
    entry.filter = text;
    show();
  });
  filter.value = entry.filter;
  const row = document.createElement("div");
  row.className = "wrapped-tabs-row";
  row.append(tabs, filter);
  wrappedBody.append(row, holder);
  show();
};

// A library track's handle for this session (the copy keeps ids), then
// `then` with it; says so when Roon can't find the track
async function withTrackHandle(track, then) {
  if (track.handle == null && track.trackId == null) return wrappedToast("Roon gave no id for that track.");
  try {
    // (a list read this session carries its handle already)
    const handle = track.handle ?? (await invoke("library_track_handle", { trackId: track.trackId }));
    await then(handle);
  } catch (err) {
    wrappedToast(String(err));
  }
}

// A song's row (the Tracks page, playlists, tags, Listen Later). `opts`:
// `play` replaces what the play button does, `extra(row)` adds buttons at
// the end, `menu` adds to the song's menu
function libraryTrackRow(t, opts) {
  opts = opts && typeof opts === "object" ? opts : {};
  const row = document.createElement("div");
  row.className = "wrapped-track wrapped-library-track";
  row.append(
    wrappedIconButton(WRAPPED_PLAY_ICON, opts.playTitle || "Play now", () => {
      if (!zone) return wrappedToast("Pick a zone first.");
      if (opts.play) return opts.play(t);
      withTrackHandle(t, (handle) => send("library_play_track", { zoneId: zone.zoneId, handle }));
    }, "wrapped-track-play"),
  );
  row.append(wrappedArt(t.imageKey, 80, t.album || t.title));
  const title = document.createElement("span");
  title.className = "wrapped-track-title link";
  title.textContent = t.title;
  title.addEventListener("click", (event) => {
    event.stopPropagation();
    withTrackHandle(t, (handle) =>
      wrappedPlayMenu(title, {
        what: "song",
        handle,
        seed: { kind: "song", handle, title: t.title, artist: (t.artist || "").split(/ \/ |, /)[0], album: t.album || "", imageKey: t.imageKey },
        radio: { tracks: [handle] },
      }),
    );
  });
  const artist = document.createElement("span");
  artist.className = `wrapped-track-by${t.performerId != null ? " link" : ""}`;
  artist.textContent = t.artist;
  if (t.performerId != null) artist.addEventListener("click", () => openWrappedPage({ page: "artist", performerId: t.performerId, name: t.artist }));
  const album = document.createElement("span");
  album.className = `wrapped-track-on${t.albumId != null ? " link" : ""}`;
  album.textContent = t.album;
  if (t.albumId != null) album.addEventListener("click", () => openWrappedPage({ page: "album", albumId: t.albumId }));
  // The heart works on the copy's state and asks Roon for the handle on a
  // click (the row then remembers Roon's answer)
  const heart = wrappedHeart({ inLibrary: true, favorite: t.favorite }, async (favorite) => {
    const handle = t.handle ?? (await invoke("library_track_handle", { trackId: t.trackId }));
    const next = await invoke("library_track_heart", { handle, favorite });
    t.favorite = next.favorite;
    return { inLibrary: true, favorite: next.favorite };
  });
  const length = document.createElement("span");
  length.className = "wrapped-track-length";
  length.textContent = t.lengthSeconds ? formatTime(t.lengthSeconds) : "";
  // Date added: Roon's, or the first Last.fm scrobble when that's earlier
  // (marked); "Unknown" when neither says
  const added = document.createElement("span");
  added.className = "wrapped-track-added";
  if (t.addedAt) {
    added.textContent = new Date(t.addedAt).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
    if (t.addedFrom === "lastfm") {
      const tag = document.createElement("span");
      tag.className = "wrapped-track-added-from";
      tag.textContent = "Last.fm";
      added.append(tag);
      added.title = "First scrobbled on Last.fm (earlier than Roon's import date)";
    } else added.title = "Added to Roon";
  } else {
    added.textContent = "Unknown";
    added.classList.add("unknown");
  }
  const year = document.createElement("span");
  year.className = "wrapped-track-year";
  year.textContent = t.year || "";
  row.append(title, artist, album, year, added, heart, length);
  if (opts.extra) opts.extra(row);
  wholeRowOpens(row, title);
  return row;
}

// A click anywhere on a song's row (not on its buttons or links) does what
// a click on its title does: the song's menu
function wholeRowOpens(row, title) {
  row.classList.add("clickable");
  row.addEventListener("click", (event) => {
    if (event.target.closest("button, a, input, .link")) return;
    event.stopPropagation();
    title.click();
  });
}

// The Tracks page's sort, direction and hearts switch, kept between visits
const TRACKS_VIEW_KEY = "wrapped-tracks-view";
function loadTracksView() {
  try {
    return JSON.parse(localStorage.getItem(TRACKS_VIEW_KEY)) || {};
  } catch {
    return {};
  }
}
function saveTracksView(entry) {
  try {
    localStorage.setItem(TRACKS_VIEW_KEY, JSON.stringify({ sort: entry.sort, dir: entry.dir, hearts: entry.hearts }));
  } catch {}
}

WRAPPED_PAGES.tracks = async function (entry) {
  wrappedHeading("Tracks", "Every track in your library");
  if (wrappedNeedsLibrary()) return;
  const ticket = ++wrappedTicket;
  let tracks;
  try {
    tracks = await invoke("wrapped_library_tracks");
  } catch (err) {
    if (ticket === wrappedTicket) wrappedFailed(err);
    return;
  }
  if (ticket !== wrappedTicket) return;
  if (!tracks.length) return wrappedLibraryRead(ticket, renderWrapped);
  const byName = (a, b) => a.localeCompare(b, undefined, { sensitivity: "base", numeric: true });
  // Each ascending (A to Z, oldest first); a direction flips it. Tracks
  // without a year or date stay at the end either way.
  const last = (key) => (a, b) => (a[key] ? 0 : 1) - (b[key] ? 0 : 1);
  const sorts = {
    title: (a, b) => byName(a.title, b.title) || byName(a.artist, b.artist),
    artist: (a, b) => byName(a.artist, b.artist) || (a.year || 0) - (b.year || 0) || byName(a.album, b.album) || byName(a.title, b.title),
    album: (a, b) => byName(a.album, b.album) || byName(a.artist, b.artist) || byName(a.title, b.title),
    year: (a, b) => (a.year || 0) - (b.year || 0) || byName(a.artist, b.artist) || byName(a.album, b.album),
    added: (a, b) => (a.addedAt || 0) - (b.addedAt || 0) || byName(a.artist, b.artist) || byName(a.album, b.album),
    length: (a, b) => (a.lengthSeconds || 0) - (b.lengthSeconds || 0) || byName(a.title, b.title),
  };
  const missing = { year: last("year"), added: last("addedAt") };
  // How each sort starts: names A to Z, years and dates newest first
  const firstDir = { artist: "up", title: "up", album: "up", year: "down", added: "down", length: "down" };
  const saved = loadTracksView();
  entry.sort ||= sorts[saved.sort] ? saved.sort : "artist";
  entry.dir ||= saved.dir === "up" || saved.dir === "down" ? saved.dir : firstDir[entry.sort];
  entry.hearts ??= !!saved.hearts;
  entry.filter ||= "";
  const hearted = tracks.filter((t) => t.favorite).length;
  const total = tracks.length.toLocaleString();
  wrappedHeading("Tracks", hearted ? `${hearted.toLocaleString()} hearted tracks, of ${total} tracks in your library` : `${total} tracks in your library`);
  // Column headers, like Roon's: a click sorts by the column, again
  // reverses it (the arrow says which way)
  const header = document.createElement("div");
  header.className = "wrapped-track wrapped-library-track wrapped-track-header";
  const spacer = (el) => {
    el.classList.add("wrapped-header-spacer");
    el.setAttribute("aria-hidden", "true");
    el.tabIndex = -1;
    return el;
  };
  header.append(spacer(wrappedIconButton(WRAPPED_PLAY_ICON, "", () => {}, "wrapped-track-play")), spacer(wrappedArt(null, 80, "")));
  const columns = [
    ["title", "Track", "wrapped-track-title"],
    ["artist", "Artist", "wrapped-track-by"],
    ["album", "Album", "wrapped-track-on"],
    ["year", "Year", "wrapped-track-year"],
    ["added", "Date added", "wrapped-track-added"],
    [null, "", "heart"],
    ["length", "Length", "wrapped-track-length"],
  ];
  for (const [sort, label, className] of columns) {
    if (!sort) {
      header.append(spacer(wrappedHeart({ inLibrary: true, favorite: false }, async () => ({}))));
      continue;
    }
    const cell = document.createElement("button");
    cell.className = `${className} wrapped-sort-head`;
    cell.dataset.sort = sort;
    const text = document.createElement("span");
    text.textContent = label;
    const dir = document.createElement("span");
    dir.className = "wrapped-sort-dir";
    cell.append(text, dir);
    cell.addEventListener("click", () => {
      if (entry.sort === sort) entry.dir = entry.dir === "up" ? "down" : "up";
      else {
        entry.sort = sort;
        entry.dir = firstDir[sort];
      }
      saveTracksView(entry);
      show();
    });
    header.append(cell);
  }
  const holder = document.createElement("div");
  // Only the hearted ones (as the copy last saw them)
  const hearts = document.createElement("button");
  hearts.className = "wrapped-hearts-filter";
  hearts.innerHTML = HEART_ICONS;
  hearts.title = "Only hearted tracks";
  const show = () => {
    for (const cell of header.querySelectorAll(".wrapped-sort-head")) {
      const on = cell.dataset.sort === entry.sort;
      cell.classList.toggle("selected", on);
      cell.querySelector(".wrapped-sort-dir").textContent = on ? (entry.dir === "up" ? "\u25B4" : "\u25BE") : "";
      cell.title = on ? "Click again to reverse" : `Sort by ${cell.textContent.toLowerCase()}`;
    }
    hearts.classList.toggle("on", entry.hearts);
    const f = entry.filter;
    let list = entry.hearts ? tracks.filter((t) => t.favorite) : tracks.slice();
    if (f) list = list.filter((t) => t.title.toLowerCase().includes(f) || t.artist.toLowerCase().includes(f) || t.album.toLowerCase().includes(f));
    const by = sorts[entry.sort];
    const flip = entry.dir === "down" ? -1 : 1;
    const end = missing[entry.sort];
    list.sort((a, b) => (end && end(a, b)) || flip * by(a, b));
    holder.replaceChildren(wrappedLazyList(list, libraryTrackRow, "wrapped-tracks", 150));
    if (!list.length) holder.replaceChildren(wrappedNote(entry.hearts ? "No hearted track matches." : "No track by that name."));
  };
  hearts.addEventListener("click", () => {
    entry.hearts = !entry.hearts;
    saveTracksView(entry);
    show();
  });
  const filter = wrappedFilter("Filter tracks", (text) => {
    entry.filter = text;
    show();
  });
  filter.value = entry.filter;
  const row = document.createElement("div");
  row.className = "wrapped-tabs-row";
  row.append(hearts, filter);
  wrappedBody.append(row, header, holder);
  show();
};


// ----- A playlist, Tags, Listen Later (Roon's own lists) -----

// A song list's "Play" from its start
function wrappedPlayAll(label, run) {
  const btn = document.createElement("button");
  btn.className = "wrapped-button small wrapped-play-all";
  btn.innerHTML = `${WRAPPED_PLAY_ICON}<span>${label}</span>`;
  btn.addEventListener("click", async () => {
    if (!zone) return wrappedToast("Pick a zone first.");
    btn.disabled = true;
    try {
      await run();
    } catch (err) {
      wrappedToast(String(err));
    } finally {
      btn.disabled = false;
    }
  });
  return btn;
}

WRAPPED_PAGES.playlist = async function (entry) {
  const name = (entry.name || "Playlist").startsWith(WRAPPED_PREFIX) ? entry.name.slice(WRAPPED_PREFIX.length) : entry.name || "Playlist";
  wrappedHeading(name, "Playlist");
  if (wrappedNeedsLibrary()) return;
  const ticket = ++wrappedTicket;
  wrappedBody.append(wrappedNote("Loading..."));
  let songs;
  let card = null;
  try {
    [songs, card] = await Promise.all([
      invoke("library_playlist_tracks", { handle: entry.handle }),
      invoke("library_playlists").then((all) => all.find((p) => p.handle === entry.handle) || null).catch(() => null),
    ]);
  } catch (err) {
    if (ticket === wrappedTicket) wrappedFailed(err);
    return;
  }
  if (ticket !== wrappedTicket) return;
  const editable = card ? !card.readOnly : true;
  const paint = () => {
    const length = songs.reduce((sum, s) => sum + (s.lengthSeconds || 0), 0);
    wrappedHeading(name, `${songs.length.toLocaleString()} song${songs.length === 1 ? "" : "s"}${length ? `, ${formatLongTime(length)}` : ""}`);
    wrappedBody.replaceChildren();
    const actions = document.createElement("div");
    actions.className = "wrapped-actions";
    actions.append(
      wrappedPlayAll("Play", async () => {
        await invoke("library_play_playlist_from", { zoneId: zone.zoneId, handle: entry.handle, item: null });
        wrappedToast(`Playing ${name} on ${zone.name}`);
      }),
    );
    wrappedBody.append(actions);
    if (!songs.length) return wrappedBody.append(wrappedNote("This playlist is empty."));
    // Play goes on through the playlist from the song; X takes it out
    const remove = (song) =>
      wrappedIconButton(CLOSE_ICON, "Remove from this playlist", async () => {
        try {
          songs = await invoke("library_playlist_remove", { handle: entry.handle, items: [song.itemHandle] });
          wrappedToast(`Took "${song.title}" out of ${name}`);
          if (ticket === wrappedTicket) paint();
        } catch (err) {
          wrappedToast(String(err));
        }
      }, "wrapped-row-remove");
    wrappedBody.append(
      wrappedLazyList(
        songs,
        (song) =>
          libraryTrackRow(song, {
            playTitle: "Play from here",
            play: () =>
              invoke("library_play_playlist_from", { zoneId: zone.zoneId, handle: entry.handle, item: song.itemHandle })
                .then(() => wrappedToast(`Playing ${name} from "${song.title}"`))
                .catch((err) => wrappedToast(String(err))),
            extra: editable ? (row) => row.append(remove(song)) : null,
          }),
        "wrapped-tracks",
        150,
      ),
    );
  };
  paint();
};

WRAPPED_PAGES.tags = async function () {
  wrappedHeading("Tags", "Your Roon tags");
  if (wrappedNeedsLibrary()) return;
  const ticket = ++wrappedTicket;
  wrappedBody.append(wrappedNote("Loading..."));
  let tags;
  try {
    tags = await invoke("library_tags");
  } catch (err) {
    if (ticket === wrappedTicket) wrappedFailed(err);
    return;
  }
  if (ticket !== wrappedTicket) return;
  wrappedBody.replaceChildren();
  wrappedHeading("Tags", `${tags.length.toLocaleString()} tag${tags.length === 1 ? "" : "s"}`);
  if (!tags.length) return wrappedBody.append(wrappedNote("You have no tags in Roon yet."));
  wrappedBody.append(
    wrappedGrid(
      tags.map((t) => ({ title: t.name, sub: t.count != null ? `${t.count.toLocaleString()} item${t.count === 1 ? "" : "s"}` : "", imageKey: t.imageKey, handle: t.handle })),
      (t) => openWrappedPage({ page: "tag", handle: t.handle, name: t.title }),
    ),
  );
};

// The albums, artists and songs of a list (a tag, Listen Later), each kind
// its own section; `remove` (optional) puts an X on each
function wrappedMixedSections(albums, artists, tracks, remove) {
  const out = [];
  const withX = (card, item, kind) => {
    if (!remove) return card;
    const wrap = document.createElement("div");
    wrap.className = "wrapped-card-wrap";
    wrap.append(card);
    const x = document.createElement("button");
    x.className = "discover-ask-x wrapped-card-ban";
    x.innerHTML = CLOSE_ICON;
    x.title = remove.title;
    x.addEventListener("click", (event) => {
      event.stopPropagation();
      remove.run(kind, item, wrap);
    });
    wrap.append(x);
    return wrap;
  };
  if (albums.length) {
    out.push(wrappedSection(`Albums (${albums.length})`));
    const grid = document.createElement("div");
    grid.className = "wrapped-cards";
    for (const a of albums) {
      grid.append(withX(wrappedCard({ title: a.title, sub: [a.artist, a.year].filter(Boolean).join(" \u00b7 "), imageKey: a.imageKey }, () => openWrappedPage(a.albumId != null ? { page: "album", albumId: a.albumId } : { page: "album", handle: a.handle })), a, "album"));
    }
    out.push(grid);
  }
  if (artists.length) {
    out.push(wrappedSection(`Artists (${artists.length})`));
    const grid = document.createElement("div");
    grid.className = "wrapped-cards";
    for (const a of artists) {
      grid.append(withX(wrappedCard({ title: a.name, sub: a.albumCount ? `${a.albumCount} albums` : "", imageKey: a.imageKey, round: true }, () => openWrappedPage({ page: "artist", handle: a.handle })), a, "artist"));
    }
    out.push(grid);
  }
  if (tracks.length) {
    out.push(wrappedSection(`Songs (${tracks.length})`));
    const list = document.createElement("div");
    list.className = "wrapped-tracks";
    for (const t of tracks) {
      list.append(
        libraryTrackRow(t, {
          extra: remove
            ? (row) =>
                row.append(
                  wrappedIconButton(CLOSE_ICON, remove.title, () => remove.run("track", t, row), "wrapped-row-remove"),
                )
            : null,
        }),
      );
    }
    out.push(list);
  }
  return out;
}

WRAPPED_PAGES.tag = async function (entry) {
  wrappedHeading(entry.name || "Tag", "Tag");
  if (wrappedNeedsLibrary()) return;
  const ticket = ++wrappedTicket;
  wrappedBody.append(wrappedNote("Loading..."));
  let tag;
  try {
    tag = await invoke("library_tag_contents", { handle: entry.handle });
  } catch (err) {
    if (ticket === wrappedTicket) wrappedFailed(err);
    return;
  }
  if (ticket !== wrappedTicket) return;
  wrappedBody.replaceChildren();
  if (tag.smart) return wrappedBody.append(wrappedNote("This is a smart tag (rules, not a list): Wrapped can't read those yet. Roon's Tags page shows it."));
  const n = tag.albums.length + tag.artists.length + tag.tracks.length;
  wrappedHeading(entry.name || tag.name, `${n.toLocaleString()} item${n === 1 ? "" : "s"}${tag.other ? `, and ${tag.other} of other kinds` : ""}`);
  if (!n) return wrappedBody.append(wrappedNote(tag.other ? "This tag holds only things Wrapped doesn't show yet (works, playlists)." : "Nothing in this tag."));
  wrappedBody.append(...wrappedMixedSections(tag.albums, tag.artists, tag.tracks, null));
};

WRAPPED_PAGES.listenlater = async function () {
  wrappedHeading("Listen Later", "What you put aside in Roon");
  if (wrappedNeedsLibrary()) return;
  const ticket = ++wrappedTicket;
  wrappedBody.append(wrappedNote("Loading..."));
  let list;
  try {
    list = await invoke("library_listen_later");
  } catch (err) {
    if (ticket === wrappedTicket) wrappedFailed(err);
    return;
  }
  if (ticket !== wrappedTicket) return;
  wrappedBody.replaceChildren();
  wrappedHeading("Listen Later", `${list.length.toLocaleString()} waiting, newest first`);
  if (!list.length) return wrappedBody.append(wrappedNote("Nothing in Listen Later. In Roon, an album's, artist's or song's menu has Listen Later."));
  const albums = list.filter((e) => e.album).map((e) => e.album);
  const artists = list.filter((e) => e.artist).map((e) => e.artist);
  const tracks = list.filter((e) => e.track).map((e) => e.track);
  wrappedBody.append(
    ...wrappedMixedSections(albums, artists, tracks, {
      title: "Remove from Listen Later",
      run: async (kind, item, el) => {
        try {
          await invoke("library_listen_later_remove", { kind, handle: item.handle });
          // (an emptied section goes, heading and all)
          const parent = el.parentElement;
          el.remove();
          if (parent && !parent.children.length) {
            if (parent.previousElementSibling?.classList.contains("wrapped-section")) parent.previousElementSibling.remove();
            parent.remove();
          }
          wrappedToast(`Took "${item.title || item.name}" out of Listen Later`);
        } catch (err) {
          wrappedToast(String(err));
        }
      },
    }),
  );
};

// ----- Our genres: your library by the tags Wrapped keeps (tags.rs) -----

const GENRE_KINDS = [
  ["genre", "Genres"],
  ["style", "Styles"],
  ["scene", "Scenes"],
];

WRAPPED_PAGES.genres = async function (entry) {
  wrappedHeading("Genres", "Your library by Wrapped's own tags, not Roon's");
  if (wrappedNeedsLibrary()) return;
  const ticket = ++wrappedTicket;
  let rows;
  try {
    rows = await invoke("wrapped_genres");
  } catch (err) {
    if (ticket === wrappedTicket) wrappedFailed(err);
    return;
  }
  if (ticket !== wrappedTicket) return;
  if (!rows.length) {
    wrappedBody.append(wrappedNote("Nothing tagged yet: Wrapped builds its genres from the tags it fetches for your artists. Read the library and start the tagging in Settings."));
    const open = document.createElement("button");
    open.className = "wrapped-button";
    open.textContent = "Open Settings";
    open.addEventListener("click", () => openWrappedPage({ page: "settings" }));
    wrappedBody.append(open);
    return;
  }
  entry.kind ||= "genre";
  entry.filter ||= "";
  const tabs = document.createElement("div");
  tabs.className = "wrapped-tabs small";
  const holder = document.createElement("div");
  const show = () => {
    for (const tab of tabs.querySelectorAll(".wrapped-tab")) tab.classList.toggle("selected", tab.dataset.kind === entry.kind);
    const f = entry.filter;
    const list = rows.filter((r) => r.kind === entry.kind && (!f || r.name.includes(f)));
    const kind = GENRE_KINDS.find(([k]) => k === entry.kind)[1].toLowerCase();
    wrappedHeading("Genres", `${list.length.toLocaleString()} ${f ? "matching " : ""}${kind} across your library, by Wrapped's own tags`);
    holder.replaceChildren(
      wrappedLazyGrid(
        list.map((r) => ({ title: r.name, sub: `${r.artists} artist${r.artists === 1 ? "" : "s"}`, imageKey: r.imageKey, why: r.examples.join(", "), name: r.name })),
        (r) => openWrappedPage({ page: "ourgenre", name: r.name }),
      ),
    );
    if (!list.length) holder.replaceChildren(wrappedNote("Nothing by that name."));
  };
  for (const [kind, label] of GENRE_KINDS) {
    const tab = document.createElement("button");
    tab.className = "wrapped-tab";
    tab.dataset.kind = kind;
    tab.textContent = label;
    tab.addEventListener("click", () => {
      entry.kind = kind;
      show();
    });
    tabs.append(tab);
  }
  const filter = wrappedFilter("Filter", (text) => {
    entry.filter = text;
    show();
  });
  filter.value = entry.filter;
  const row = document.createElement("div");
  row.className = "wrapped-tabs-row";
  row.append(tabs, filter);
  wrappedBody.append(row, holder);
  show();
};

// One of our genres: the library's artists that carry the tag
WRAPPED_PAGES.ourgenre = async function (entry) {
  wrappedHeading(entry.name, "Artists in your library with this tag");
  if (wrappedNeedsLibrary()) return;
  const ticket = ++wrappedTicket;
  let artists;
  try {
    artists = await invoke("wrapped_genre_artists", { name: entry.name });
  } catch (err) {
    if (ticket === wrappedTicket) wrappedFailed(err);
    return;
  }
  if (ticket !== wrappedTicket) return;
  wrappedHeading(entry.name, `${artists.length.toLocaleString()} artist${artists.length === 1 ? "" : "s"} in your library with this tag, the strongest first`);
  if (!artists.length) return wrappedBody.append(wrappedNote("Nobody in your library carries this tag."));
  const actions = document.createElement("div");
  actions.className = "wrapped-actions";
  const radio = document.createElement("button");
  radio.className = "wrapped-button small";
  radio.textContent = "Shuffle";
  radio.title = "Roon shuffles the songs of this tag's strongest artists (up to 12)";
  radio.addEventListener("click", async () => {
    try {
      const ids = artists.slice(0, 12).filter((a) => a.performerId != null);
      const handles = [];
      for (const a of ids) {
        const card = await invoke("library_artist_by_id", { performerId: a.performerId });
        handles.push(card.handle);
      }
      if (!handles.length) return wrappedToast("Roon gave no ids for these artists.");
      // (several artists need limitToSeed: the Core then queues their own
      // songs, shuffled; without it Roon's radio takes one artist only)
      await startRoonRadio("genre", { artists: handles, limitToSeed: true, libraryOnly: true }, radio);
    } catch (err) {
      wrappedToast(String(err));
    }
  });
  actions.append(radio);
  wrappedBody.append(actions);
  wrappedBody.append(
    wrappedLazyGrid(
      artists.map((a) => ({
        title: a.name,
        sub: a.albumCount ? `${a.albumCount} album${a.albumCount === 1 ? "" : "s"}` : "",
        imageKey: a.imageKey,
        round: true,
        performerId: a.performerId,
      })),
      (a) => openWrappedPage({ page: "artist", performerId: a.performerId, name: a.title }),
    ),
  );
};

WRAPPED_PAGES.history = async function (entry) {
  wrappedHeading("History", "What Roon has logged for your profile");
  if (wrappedNeedsLibrary()) return;
  const ticket = ++wrappedTicket;
  try {
    historyStatus = await invoke("wrapped_history_status");
  } catch (err) {
    if (ticket === wrappedTicket) wrappedFailed(err);
    return;
  }
  if (ticket !== wrappedTicket) return;
  wrappedBody.replaceChildren();
  const tabs = document.createElement("div");
  tabs.className = "wrapped-tabs";
  const progress = document.createElement("div");
  const holder = document.createElement("div");
  const views = [
    ["recent", "Recent"],
    ["artists", "Top artists"],
    ["albums", "Top albums"],
    ["tracks", "Top tracks"],
  ];
  entry.view ||= "recent";
  entry.days ??= 91;
  const heading = () => {
    const n = historyStatus.plays.toLocaleString();
    const when = historyStatus.syncedAt ? `, read ${historyWhen(historyStatus.syncedAt)}` : "";
    const scrobbles = historyStatus.scrobbles ? ` and ${historyStatus.scrobbles.toLocaleString()} Last.fm scrobbles from before Roon` : "";
    wrappedHeading("History", historyStatus.plays ? `${n} Roon plays${historyStatus.complete ? "" : " so far"}${scrobbles}${when}` : "Not read from Roon yet");
  };
  const show = async () => {
    for (const tab of tabs.querySelectorAll(".wrapped-tab[data-view]")) tab.classList.toggle("selected", tab.dataset.view === entry.view);
    heading();
    holder.replaceChildren();
    if (!historyStatus.plays) {
      if (!historyStatus.syncing) holder.append(wrappedNote("Nothing read yet."));
      return;
    }
    if (entry.view === "recent") {
      const plays = await invoke("wrapped_history_recent", { limit: 500 });
      if (ticket !== wrappedTicket || entry.view !== "recent") return;
      holder.replaceChildren(historyRows(plays));
    } else {
      const periods = document.createElement("div");
      periods.className = "wrapped-tabs small";
      for (const [days, label] of HISTORY_PERIODS) {
        const pill = document.createElement("button");
        pill.className = `wrapped-tab${entry.days === days ? " selected" : ""}`;
        pill.textContent = label;
        pill.addEventListener("click", () => {
          entry.days = days;
          show();
        });
        periods.append(pill);
      }
      const rows = await invoke("wrapped_history_top", { what: entry.view, days: entry.days, limit: 60 });
      if (ticket !== wrappedTicket || entry.view === "recent") return;
      holder.replaceChildren(periods, historyTop(rows, entry.view));
    }
  };
  for (const [view, label] of views) {
    const tab = document.createElement("button");
    tab.className = "wrapped-tab";
    tab.dataset.view = view;
    tab.textContent = label;
    tab.addEventListener("click", () => {
      entry.view = view;
      show();
    });
    tabs.append(tab);
  }
  const sync = async (full) => {
    try {
      historyStatus = await invoke("wrapped_history_sync", { full });
    } catch (err) {
      progress.replaceChildren(wrappedNote(String(err), "wrapped-note error"));
      return;
    }
    historyProgress(progress, { loaded: 0, wanted: historyStatus.complete ? 0 : historyStatus.total || 0, done: false });
  };
  const reload = document.createElement("button");
  reload.className = "wrapped-button small reload";
  reload.textContent = historyStatus.plays ? "Check for new plays" : "Read from Roon";
  reload.addEventListener("click", () => sync(false));
  const tabsRow = document.createElement("div");
  tabsRow.className = "wrapped-tabs-row";
  tabsRow.append(tabs, reload);
  wrappedBody.append(tabsRow, progress, holder);
  // Progress of a sync (started here or elsewhere), while this page shows
  const unlisten = await listen("wrapped-history", (event) => {
    if (ticket !== wrappedTicket) return unlisten();
    historyProgress(progress, event.payload);
    if (event.payload.done) {
      if (event.payload.status) historyStatus = event.payload.status;
      if (event.payload.added != null) {
        const n = event.payload.added;
        wrappedToast(n ? `${n.toLocaleString()} new play${n === 1 ? "" : "s"}` : "Nothing new since the last read");
      }
      reload.textContent = "Check for new plays";
      show();
    }
  });
  if (historyStatus.syncing) historyProgress(progress, { loaded: 0, wanted: historyStatus.complete ? 0 : historyStatus.total || 0, done: false });
  else if (!historyStatus.plays) sync(false);
  show();
};

function historyWhen(ms) {
  // (Floored, as Roon's own History page counts)
  const diff = Date.now() - ms;
  const min = Math.floor(diff / 60000);
  if (min < 1) return "just now";
  if (min < 60) return `${min} min ago`;
  const h = Math.floor(min / 60);
  if (h < 24) return `${h} h ago`;
  const d = Math.floor(h / 24);
  if (d < 7) return `${d} d ago`;
  return new Date(ms).toLocaleDateString(undefined, { month: "short", day: "numeric", year: d > 300 ? "numeric" : undefined });
}

// Opens an album or artist page from a play or a top row: by the Core's
// ids, which outlive the session (the handles don't)
// Roon's photo (and handle) for cards that only have a name, one search
// at a time in the background; what it finds is kept on the row
const roonArtistCache = new Map(); // lower-cased name -> ArtistCard or null
let photoQueue = Promise.resolve();
function wrappedFillPhotos(rows, cards, ticket) {
  const tiles = [...cards.querySelectorAll(".wrapped-card")];
  rows.forEach((row, i) => {
    if (row.imageKey && row.performerId) return;
    const tile = tiles[i];
    photoQueue = photoQueue.then(async () => {
      if (ticket !== wrappedTicket || !tile.isConnected) return;
      const key = row.name.toLowerCase();
      let hit = roonArtistCache.get(key);
      if (hit === undefined) {
        try {
          const hits = await invoke("library_search", { query: row.name });
          hit = hits.artists.find((a) => a.name.toLowerCase() === key) || null;
        } catch {
          hit = null;
        }
        roonArtistCache.set(key, hit);
      }
      if (!hit) return;
      row.handle = hit.handle;
      if (hit.performerId) row.performerId = hit.performerId;
      if (hit.imageKey && !row.imageKey) {
        row.imageKey = hit.imageKey;
        const art = tile.querySelector(".wrapped-art");
        if (art) art.replaceWith(wrappedArt(hit.imageKey, 400, row.name, true));
      }
    });
  });
}

async function openFromHistory(kind, row) {
  if (kind === "artist" && row.handle) return openWrappedPage({ page: "artist", handle: row.handle });
  if (kind === "artist" && row.performerId) return openWrappedPage({ page: "artist", performerId: row.performerId });
  if (row.albumId) return openWrappedPage({ page: kind, albumId: row.albumId });
  // Nothing of Roon's for it yet (a Discover pick): Roon's search finds it
  if (kind === "artist" && row.name) {
    wrappedToast(`Looking up ${row.name} in Roon...`);
    try {
      const hits = await invoke("library_search", { query: row.name });
      const hit = hits.artists.find((a) => a.name.toLowerCase() === row.name.toLowerCase()) || hits.artists[0];
      if (hit) return openWrappedPage({ page: "artist", handle: hit.handle });
    } catch (err) {
      console.warn("discover lookup", err);
    }
    return openWrappedPage({ page: "search", query: row.name });
  }
  wrappedToast("Roon didn't keep an id for that one.");
}

function historyRows(plays) {
  const rows = document.createElement("div");
  rows.className = "wrapped-tracks";
  for (const play of plays) {
    const row = document.createElement("div");
    row.className = "wrapped-track";
    const art = wrappedArt(play.imageKey, 88, play.album || play.title);
    art.classList.add("small");
    const text = document.createElement("button");
    text.className = "wrapped-track-text link";
    const title = document.createElement("span");
    title.className = "wrapped-track-title";
    title.textContent = play.title;
    const sub = document.createElement("span");
    sub.className = "wrapped-track-sub";
    sub.textContent = [play.artist, play.album].filter(Boolean).join(" · ");
    text.append(title, sub);
    text.addEventListener("click", () => openFromHistory("album", play));
    const when = document.createElement("span");
    when.className = "wrapped-track-length";
    const heard = play.secondsPlayed != null && play.lengthSeconds ? Math.min(100, Math.round((play.secondsPlayed / play.lengthSeconds) * 100)) : null;
    when.textContent = [historyWhen(play.playedAt), heard != null && heard < 90 ? `${heard}%` : null].filter(Boolean).join(" · ");
    row.append(art, text, when);
    rows.append(row);
  }
  if (!plays.length) rows.append(wrappedNote("Roon has no plays logged yet."));
  return rows;
}

function historyTop(rows, what) {
  if (!rows.length) return wrappedNote("Nothing played far enough to count in that time.");
  const items = rows.map((row) => {
    const times = `${row.plays} play${row.plays === 1 ? "" : "s"}`;
    if (what === "artists") return { row, title: row.name, sub: times, imageKey: row.imageKey, round: true };
    return { row, title: row.name, sub: `${row.artist} · ${times}`, imageKey: row.imageKey };
  });
  return wrappedGrid(items, (item) => openFromHistory(what === "artists" ? "artist" : "album", item.row));
}

// Wrapped's own settings: the Last.fm key, how "Similar by" starts out,
// and the state of Wrapped's records (history copy, tag cache)
const SIMILAR_BY = [
  ["roon", "Roon"],
  ["genre", "Genre"],
  ["style", "Style"],
  ["blend", "Genre + Style"],
  ["scene", "Scene"],
  ["era", "Era"],
];
// Where the tag rows look: in the library, or only outside it
const SIMILAR_SCOPES = [
  ["library", "Library"],
  ["discover", "Discover"],
];

WRAPPED_PAGES.settings = async function () {
  wrappedHeading("Settings", "Wrapped's own settings");
  const ticket = ++wrappedTicket;
  let tagCount = 0;
  try {
    [wrappedSettings, historyStatus, tagCount] = await Promise.all([
      invoke("wrapped_settings_get"),
      invoke("wrapped_history_status").catch(() => null),
      invoke("wrapped_tags_count").catch(() => 0),
      fetchSoundState(),
    ]);
  } catch (err) {
    if (ticket === wrappedTicket) wrappedFailed(err);
    return;
  }
  if (ticket !== wrappedTicket) return;
  // Ollama chosen for tier 2: the page draws with what's known and asks
  // where the setup stands on the side, drawing again if that changed
  if (wrappedSettings?.discovery !== false && wrappedSettings?.llmProvider === "ollama") {
    if (!ollamaFetching && (!ollamaState || Date.now() - ollamaStateAt > 3000)) {
      fetchOllamaState().then((changed) => {
        if (changed) redrawSettingsInPlace();
      });
    }
  }
  wrappedBody.replaceChildren();
  // Two columns: general on the left; Wrapped Discovery, the keys and the
  // picking tiers on the right
  const columns = document.createElement("div");
  columns.className = "wrapped-settings-columns";
  const left = document.createElement("div");
  left.className = "wrapped-settings-column";
  const right = document.createElement("div");
  right.className = "wrapped-settings-column";
  columns.append(left, right);
  wrappedBody.append(columns);
  const save = async (changes) => {
    try {
      wrappedSettings = await invoke("wrapped_settings_update", { changes });
      wrappedToast("Saved");
    } catch (err) {
      wrappedToast(String(err));
    }
  };
  const field = (label, text, where = left) => {
    const block = document.createElement("div");
    block.className = "wrapped-setting";
    const h = document.createElement("div");
    h.className = "wrapped-setting-label";
    h.textContent = label;
    const p = document.createElement("div");
    p.className = "wrapped-setting-text";
    if (typeof text === "string") p.textContent = text;
    else p.append(...text);
    block.append(h, p);
    where.append(block);
    return block;
  };
  // A link that opens in the browser, not in this window
  const link = (href, label) => {
    const a = document.createElement("a");
    a.href = href;
    a.className = "wrapped-link-text";
    a.textContent = label;
    a.addEventListener("click", (event) => {
      event.preventDefault();
      invoke("plugin:opener|open_url", { url: href });
    });
    return a;
  };

  // Wrapped Discovery on or off: the weekly lists, the thumbs and the
  // zone watching behind them (the menu, pages and radio stay either way)
  const discovery = field(
    "Wrapped Discovery",
    "Discover Weekly and Library Rediscover, rating their songs, and \"Suggested because\" in the player. Off, none of it shows or runs; your ratings and the lists in Roon are kept.",
    right,
  );
  const onOff = document.createElement("div");
  onOff.className = "wrapped-tabs small";
  const discoveryOn = wrappedSettings.discovery !== false;
  for (const [value, label] of [
    [false, "Off"],
    [true, "On"],
  ]) {
    const pill = document.createElement("button");
    pill.className = `wrapped-tab${discoveryOn === value ? " selected" : ""}`;
    pill.textContent = label;
    pill.addEventListener("click", async () => {
      if (discoveryOn === value) return;
      await save({ discovery: value });
      loadDiscoverRatings();
      redrawSettingsInPlace();
    });
    onOff.append(pill);
  }
  discovery.append(onOff);

  // The Last.fm key: inside Tier 1 while Wrapped Discovery is on (it's what
  // switches tier 1 on), on its own otherwise (Similar and the radio use it
  // too)
  const keyWords = () => [
    'Tags like "melodic death metal" and "viking metal" come from Last.fm\'s listeners; Roon\'s own genres stop at "Death Metal". A key is free: ',
    link("https://www.last.fm/api/account/create", "create one here"),
    " (any name and description will do, ignore the rest; ",
    link("https://www.last.fm/api/accounts", "your existing keys"),
    "). Without one, only MusicBrainz is asked.",
  ];
  const lastfmRow = document.createElement("div");
  lastfmRow.className = "wrapped-setting-row";
  const input = document.createElement("input");
  input.className = "wrapped-search";
  input.type = "password";
  input.placeholder = "32 characters";
  input.value = wrappedSettings.lastfmKey || "";
  input.autocomplete = "off";
  const show = document.createElement("button");
  show.className = "wrapped-button";
  show.textContent = "Show";
  show.addEventListener("click", () => {
    input.type = input.type === "password" ? "text" : "password";
    show.textContent = input.type === "password" ? "Show" : "Hide";
  });
  const saveKey = document.createElement("button");
  saveKey.className = "wrapped-button";
  saveKey.textContent = "Save";
  saveKey.addEventListener("click", async () => {
    await save({ lastfmKey: input.value.trim() });
    // (the tiers' badges and cards follow the key)
    redrawSettingsInPlace();
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") saveKey.click();
  });
  lastfmRow.append(input, show, saveKey);
  if (!discoveryOn) {
    const key = field("Last.fm API key", keyWords(), right);
    // No key yet: the same slow glow as Toasted's Core card without a Core
    key.classList.toggle("attention", !wrappedSettings.lastfmKey);
    key.append(lastfmRow);
  }

  // A tier's setup, folding open and shut under a button: open while the
  // tier isn't working yet, folded once it is; kept as it was while the
  // page redraws
  // (`ready` null: not known yet, shut for now and decided when it is)
  const setupFold = (name, ready, ...content) => {
    if (settingsFolds[name] === undefined && ready !== null) settingsFolds[name] = !ready;
    const toggle = document.createElement("button");
    toggle.className = "wrapped-button small llm-setup-toggle";
    const fold = document.createElement("div");
    fold.className = `llm-fold${settingsFolds[name] ? " open" : ""}`;
    const inner = document.createElement("div");
    inner.className = "llm-fold-inner";
    inner.append(...content);
    fold.append(inner);
    const label = () => {
      toggle.textContent = settingsFolds[name] ? "Hide setup" : ready ? "Change setup" : "Set up";
      toggle.setAttribute("aria-expanded", settingsFolds[name] ? "true" : "false");
    };
    label();
    toggle.addEventListener("click", () => {
      settingsFolds[name] = !settingsFolds[name];
      fold.classList.toggle("open", settingsFolds[name]);
      label();
    });
    return [toggle, fold];
  };

  // How the Discover lists pick: three tiers, each adding to the one below
  if (discoveryOn) {
    const tiers = field(
      "How Discover picks",
      "Each level adds to the one below it, and your thumbs on the lists' songs count at every level.",
      right,
    );
    const tierCards = document.createElement("div");
    tierCards.className = "picking-tiers";
    // Each tier is a section; the ones switched on, from tier 1 down, share
    // one card (a line between them), the rest stand on their own
    const sections = [];
    const tier = (level, title, text, state, stateClass) => {
      const card = document.createElement("div");
      card.className = `picking-tier-section ${stateClass}`;
      sections.push({ el: card, on: stateClass === "on" });
      const head = document.createElement("div");
      head.className = "picking-tier-head";
      const name = document.createElement("span");
      name.className = "picking-tier-name";
      name.textContent = `Tier ${level}: ${title}`;
      const badge = document.createElement("span");
      badge.className = "picking-tier-state";
      badge.textContent = state;
      head.append(name, badge);
      card.append(head, wrappedNote(text, "wrapped-note small"));
      return card;
    };
    const hasKey = !!wrappedSettings.lastfmKey;
    const tagsCard = tier(
      1,
      "Tags",
      "Artists your favorites' listeners also play, kept to the ones whose Last.fm genre tags sound like the artist they came from. Needs a Last.fm key (free).",
      hasKey ? "Activated" : "Needs a key",
      hasKey ? "on" : "setup",
    );
    const lastfmBox = document.createElement("div");
    lastfmBox.className = "llm-setup";
    const lastfmSection = document.createElement("div");
    lastfmSection.className = "llm-section";
    const lastfmTitle = document.createElement("div");
    lastfmTitle.className = "llm-section-title";
    lastfmTitle.textContent = "Last.fm API key";
    const lastfmNote = document.createElement("p");
    lastfmNote.className = "wrapped-note small";
    lastfmNote.append(...keyWords());
    lastfmSection.append(lastfmTitle, lastfmNote, lastfmRow);
    lastfmBox.append(lastfmSection);
    // Your Last.fm account: its scrobbles reach back before Roon's history
    const userSection = document.createElement("div");
    userSection.className = "llm-section";
    const userTitle = document.createElement("div");
    userTitle.className = "llm-section-title";
    userTitle.textContent = "Last.fm username (optional)";
    const userNote = wrappedNote(
      "Roon's history only goes back as far as this Core's library. If you scrobble to Last.fm, your account's full history counts toward your favorites on Home (the scrobbles from before Roon's history starts). It's read once, then new ones daily.",
      "wrapped-note small",
    );
    const userRow = document.createElement("div");
    userRow.className = "wrapped-setting-row";
    const userInput = document.createElement("input");
    userInput.className = "wrapped-search";
    userInput.placeholder = "Your Last.fm username";
    userInput.value = wrappedSettings.lastfmUser || "";
    userInput.autocomplete = "off";
    const saveUser = document.createElement("button");
    saveUser.className = "wrapped-button";
    saveUser.textContent = "Save";
    const userState = wrappedNote("", "wrapped-note small");
    const paintScrobbles = (st) => {
      if (!st || !st.user) return (userState.textContent = "");
      const n = st.plays.toLocaleString();
      const from = st.oldest ? ` back to ${new Date(st.oldest).toLocaleDateString()}` : "";
      userState.textContent = st.error
        ? st.error
        : st.syncing
          ? `Reading ${st.user}'s scrobbles... ${n} so far${from}`
          : st.plays
            ? `${n} scrobbles of ${st.user}${from}${st.complete ? "" : " (not all read yet: it goes on by itself, or Save reads now)"}`
            : `Nothing read from ${st.user} yet.`;
    };
    saveUser.addEventListener("click", async () => {
      await save({ lastfmUser: userInput.value.trim() });
      if (userInput.value.trim()) paintScrobbles(await invoke("wrapped_scrobbles_sync", { again: false }).catch(() => null));
      else userState.textContent = "";
    });
    userInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") saveUser.click();
    });
    // (a read that stopped, or one to do over)
    const again = document.createElement("button");
    again.className = "wrapped-button";
    again.textContent = "Rescan";
    again.title = "Read the whole history from Last.fm again";
    again.addEventListener("click", async () => {
      if (!userInput.value.trim()) return;
      paintScrobbles(await invoke("wrapped_scrobbles_sync", { again: true }).catch(() => null));
    });
    userRow.append(userInput, saveUser, again);
    userSection.append(userTitle, userNote, userRow, userState);
    lastfmBox.append(userSection);
    invoke("wrapped_scrobbles_status").then(paintScrobbles).catch(() => {});
    scrobblesPaint = paintScrobbles;
    // Scan all: the history's new plays, the library's artists, then the
    // tags for them (the left column has each on its own)
    const [lastfmToggle, lastfmFold] = setupFold("lastfm", hasKey, lastfmBox);
    const scanRow = document.createElement("div");
    scanRow.className = "wrapped-setting-row tier-actions";
    const scan = document.createElement("button");
    scan.className = "wrapped-button small";
    scan.textContent = "Scan all";
    scan.title = "Read your new plays from Roon, read your library's artists again, then tag them (the tagging carries on in the background)";
    const scanText = document.createElement("span");
    scanText.className = "llm-test-out";
    const describe = async () => {
      const [h, l, t] = await Promise.all([
        invoke("wrapped_history_status").catch(() => null),
        invoke("wrapped_library_status").catch(() => null),
        invoke("wrapped_tags_status").catch(() => null),
      ]);
      const n = (x) => (x ?? 0).toLocaleString();
      return [h ? `${n(h.plays)} plays` : null, l ? `${n(l.artists)} library artists` : null, t ? `${n(t.tagged)} of ${n(t.artists)} tagged${t.filling ? " (tagging...)" : ""}` : null]
        .filter(Boolean)
        .join(" · ");
    };
    describe().then((text) => {
      if (!scan.disabled) scanText.textContent = text;
    });
    scan.addEventListener("click", async () => {
      scan.disabled = true;
      try {
        scanText.textContent = "Reading your new plays from Roon...";
        await wrappedScanStep("wrapped-history", () => invoke("wrapped_history_sync", { full: false }), (p) => {
          if (p.error) throw new Error(p.error);
          if (!p.done) scanText.textContent = `Reading your new plays from Roon... ${(p.loaded ?? 0).toLocaleString()}`;
          return p.done;
        });
        scanText.textContent = "Reading your library's artists...";
        await wrappedScanStep("wrapped-library", () => invoke("wrapped_library_sync"), (p) => {
          if (p.error) throw new Error(p.error);
          if (!p.done) scanText.textContent = `Reading your library's artists... ${(p.read ?? 0).toLocaleString()} of ${(p.total ?? 0).toLocaleString()}`;
          return p.done;
        });
        await invoke("wrapped_tags_fill", { stop: false });
        scanText.textContent = await describe();
        wrappedToast("Scanned. Tagging carries on in the background (Wrapped's Settings shows how far it got).");
      } catch (err) {
        scanText.textContent = String(err?.message || err);
      }
      scan.disabled = false;
    });
    scanRow.append(lastfmToggle, scan, scanText);
    tagsCard.append(scanRow, lastfmFold);
    // Ollama on this PC, in three steps (install, a model, a test), each
    // ticked off once it's done; ollama.rs does the work and reports back
    const ollamaSteps = () => {
      const st = ollamaState || {};
      const busy = ollamaRunning();
      const box = document.createElement("div");
      box.className = "ollama-steps";
      const step = (n, title, done, now, state) => {
        const el = document.createElement("div");
        el.className = `ollama-step ${done ? "done" : now ? "now" : "later"}`;
        const head = document.createElement("div");
        head.className = "ollama-step-head";
        const mark = document.createElement("span");
        mark.className = "ollama-step-mark";
        mark.textContent = done ? "\u2713" : String(n);
        const name = document.createElement("span");
        name.className = "ollama-step-title";
        name.textContent = title;
        head.append(mark, name);
        if (state) {
          const s = document.createElement("span");
          s.className = "ollama-step-state";
          s.textContent = state;
          head.append(s);
        }
        const body = document.createElement("div");
        body.className = "ollama-step-body";
        el.append(head, body);
        box.append(el);
        return body;
      };
      const button = (label, onClick, go = false) => {
        const b = document.createElement("button");
        b.className = `wrapped-button small${go ? " ollama-go" : ""}`;
        b.textContent = label;
        b.disabled = busy;
        b.addEventListener("click", onClick);
        return b;
      };
      const row = (...items) => {
        const r = document.createElement("div");
        r.className = "wrapped-setting-row ollama-row";
        r.append(...items);
        return r;
      };
      const note = (text) => wrappedNote(text, "wrapped-note small");
      // The running job's bar, with Stop while it downloads
      const progress = (job) => {
        const view = document.createElement("div");
        view.className = "ollama-progress";
        const track = document.createElement("div");
        track.className = "ollama-bar";
        const fill = document.createElement("div");
        fill.className = "ollama-bar-fill";
        track.append(fill);
        const label = document.createElement("span");
        label.className = "ollama-progress-label";
        const stop = document.createElement("button");
        stop.className = "wrapped-button small";
        stop.textContent = "Stop";
        stop.addEventListener("click", () => invoke("ollama_cancel"));
        view.append(track, row(label, stop));
        ollamaProgressViews[job] = { box: view, fill, label, stop };
        return view;
      };
      // How the step's last job ended, when it didn't work out
      const ended = (job) => {
        if (!ollamaJob || ollamaJob.job !== job || !["error", "stopped"].includes(ollamaJob.stage)) return null;
        return wrappedNote(ollamaJob.label, `wrapped-note small${ollamaJob.stage === "error" ? " ollama-error" : ""}`);
      };

      // 1. Ollama installed (Wrapped starts it when needed), or answering
      const up = !!st.running;
      // (Wrapped runs Ollama unless "Leave Ollama to me" is ticked)
      const auto = !wrappedSettings.ollamaManual && st.local !== false;
      const there = up || (auto && !!st.installed);
      // (not known yet: the first answer, or Windows' details, on the way)
      const unsure = !ollamaState || (!up && st.checking && !st.installed);
      const one = step(
        1,
        "Install Ollama",
        there,
        !there,
        up
          ? `Ollama ${st.version || ""} is running${st.local === false ? ` at ${st.url}` : ""}`
          : unsure
            ? "Checking..."
            : there
              ? "Installed. Wrapped starts it when needed"
              : st.installed
                ? "Installed, not running"
                : "",
      );
      if (!there && !unsure) {
        if (st.local === false) {
          one.append(note(`Nothing answers at ${st.url}. Check that Ollama is running on that computer and lets other computers in (OLLAMA_HOST set to 0.0.0.0 there).`));
        } else if (st.installed) {
          one.append(note("Ollama is on this PC but isn't running."));
          const start = button(
            "Start Ollama",
            async () => {
              start.disabled = true;
              start.textContent = "Starting...";
              try {
                await invoke("ollama_start");
                ollamaJob = null;
              } catch (err) {
                ollamaJob = { job: "install", stage: "error", label: String(err) };
              }
              refreshOllama();
            },
            true,
          );
          one.append(row(start));
        } else {
          one.append(note("Ollama runs the model on this PC: nothing leaves it, and there's no key or account. Pick the drive for Ollama and its models (each model is 5 to 20 GB):"));
          const drives = st.drives || [];
          if (!drives.some((d) => d.letter === ollamaDrive)) {
            ollamaDrive = drives.reduce((best, d) => (!best || d.freeGb > best.freeGb ? d : best), null)?.letter || null;
          }
          if (drives.length) {
            const pills = document.createElement("div");
            pills.className = "wrapped-tabs small ollama-pills";
            for (const d of drives) {
              const pill = document.createElement("button");
              pill.className = `wrapped-tab${d.letter === ollamaDrive ? " selected" : ""}`;
              pill.textContent = `${d.letter} ${ollamaGb(d.freeGb)} free`;
              if (d.label) pill.title = d.label;
              pill.disabled = busy;
              pill.addEventListener("click", () => {
                ollamaDrive = d.letter;
                redrawSettingsInPlace();
              });
              pills.append(pill);
            }
            one.append(pills);
          }
          if (ollamaDrive) {
            one.append(
              note(
                st.modelsDir
                  ? `Ollama goes in ${ollamaDrive}\\Ollama\\app. Models go where you've set them already (${st.modelsDir}).`
                  : `Ollama goes in ${ollamaDrive}\\Ollama\\app, its models in ${ollamaDrive}\\Ollama\\models.`,
              ),
            );
          }
          const install = button(
            "Download and install",
            () => {
              ollamaJob = { job: "install", stage: "download", label: "Downloading Ollama", done: 0, total: 0 };
              redrawSettingsInPlace();
              invoke("ollama_install", { drive: ollamaDrive }).catch(ollamaNotStarted);
            },
            true,
          );
          install.disabled = busy || !ollamaDrive;
          one.append(row(install));
          one.append(
            note(
              auto
                ? "Ollama's installer shows a small window of its own while it works. Then Wrapped closes Ollama again and starts it whenever it's needed, so there's nothing else to do."
                : "Ollama's installer shows a small window of its own while it works. When it's done, Ollama opens its own window asking you to create an account: none is needed, so choose \"No thanks, I'll use Ollama locally\" and close that window.",
            ),
          );
        }
      }
      if (!there && busy && ollamaJob.job === "install") one.append(progress("install"));
      const oneEnded = ended("install");
      if (oneEnded && !there) one.append(oneEnded);

      // 2. A model Ollama has, tier 2's choice (read from Ollama's folder
      // while it's closed)
      const ready = there && !!st.modelReady;
      const two = step(2, "Get a model", ready, there && !ready, ready ? st.model : "");
      if (there) {
        const have = st.models || [];
        if (have.length) {
          const menu = document.createElement("select");
          menu.className = "ollama-select";
          if (!ready) menu.append(new Option("Choose one...", ""));
          for (const m of have) {
            const same = m.name === st.model || m.name === `${st.model}:latest`;
            menu.append(new Option(`${m.name} (${ollamaGb(m.gb)})`, m.name, false, same));
          }
          menu.disabled = busy;
          menu.addEventListener("change", async () => {
            if (!menu.value) return;
            await save({ llmModel: menu.value });
            ollamaTest = null;
            refreshOllama();
          });
          two.append(row(labeled(ready ? "Using" : "Or use one Ollama has", menu)));
        }
        if ((!ready || ollamaMore) && st.checking) {
          two.append(note("Checking your graphics card..."));
        } else if (!ready || ollamaMore) {
          const sizes = st.sizes || [];
          if (!sizes.some((z) => z.name === ollamaPick)) ollamaPick = st.suggested || sizes[0]?.name;
          const gpu = st.gpu;
          two.append(
            note(
              st.local === false
                ? "Ollama is on another computer: pick a size that fits its graphics card's memory."
                : gpu && gpu.gb >= 7
                  ? `Your ${gpu.name} has ${ollamaGb(gpu.gb)} of memory, so ${st.suggested} is the one to get.`
                  : "No graphics card with enough memory was found, so the model will run on the processor and a build can take a long while. The smallest is suggested.",
            ),
          );
          const pills = document.createElement("div");
          pills.className = "wrapped-tabs small ollama-pills";
          for (const z of sizes) {
            const pill = document.createElement("button");
            pill.className = `wrapped-tab${z.name === ollamaPick ? " selected" : ""}`;
            pill.textContent = `${z.name} \u00b7 ${ollamaGb(z.downloadGb)}${z.name === st.suggested ? " (suggested)" : ""}`;
            if (st.local !== false) pill.title = z.fits ? "Fits your graphics card" : "More than your graphics card holds: it would run slowly";
            pill.disabled = busy;
            pill.addEventListener("click", () => {
              ollamaPick = z.name;
              redrawSettingsInPlace();
            });
            pills.append(pill);
          }
          two.append(pills);
          const get = button(
            `Download ${ollamaPick}`,
            () => {
              ollamaJob = { job: "pull", stage: "pull", label: `Downloading ${ollamaPick}`, done: 0, total: 0 };
              ollamaTest = null;
              ollamaMore = false;
              redrawSettingsInPlace();
              invoke("ollama_pull", { model: ollamaPick }).catch(ollamaNotStarted);
            },
            true,
          );
          get.disabled = busy || !ollamaPick;
          two.append(row(get));
        } else {
          const more = button("Download another size", () => {
            ollamaMore = true;
            redrawSettingsInPlace();
          });
          two.append(row(more));
        }
        if (busy && ollamaJob.job === "pull") two.append(progress("pull"));
        const twoEnded = ended("pull");
        if (twoEnded) two.append(twoEnded);
      }

      // 3. A test: the model answers
      const tried = ollamaTest && ollamaTest.model === st.model ? ollamaTest : null;
      const three = step(3, "Test", !!tried?.ok, ready && !tried?.ok, "");
      if (ready) {
        const out = document.createElement("span");
        out.className = "llm-test-out";
        const go = button(tried?.ok ? "Test again" : "Test", () => runOllamaTest(st.model), !tried?.ok);
        if (tried?.running) {
          go.disabled = true;
          out.textContent = up
            ? "Asking (the first answer loads the model into the graphics card, which can take a moment)..."
            : "Starting Ollama and asking (the first answer loads the model into the graphics card, which can take a moment)...";
        } else if (tried) {
          out.textContent = tried.text;
          if (!tried.ok) out.classList.add("ollama-error");
        }
        const r = row(go, out);
        r.classList.add("llm-test");
        three.append(r);
      }
      // (a job's bar starts from what's known so far)
      queueMicrotask(paintOllamaProgress);
      return box;
    };
    // Ollama on another computer (a server at home): its address
    // Who runs Ollama: Wrapped (unticked, the default), or the user
    const ollamaChoice = () => {
      const label = document.createElement("label");
      label.className = "ollama-manual";
      const tick = document.createElement("input");
      tick.type = "checkbox";
      tick.checked = !!wrappedSettings.ollamaManual;
      tick.disabled = ollamaRunning();
      tick.addEventListener("change", async () => {
        await save({ ollamaManual: tick.checked });
        refreshOllama();
      });
      const words = document.createElement("span");
      words.textContent = "Leave Ollama to me (Wrapped won't start or stop it)";
      label.append(tick, words);
      label.hidden = ollamaState?.local === false;
      return label;
    };
    const ollamaAddress = () => {
      const input = document.createElement("input");
      input.className = "wrapped-search";
      input.type = "text";
      input.autocomplete = "off";
      input.placeholder = "http://localhost:11434";
      input.value = wrappedSettings.llmUrl || "";
      const saveUrl = document.createElement("button");
      saveUrl.className = "wrapped-button";
      saveUrl.textContent = "Save";
      saveUrl.addEventListener("click", async () => {
        await save({ llmUrl: input.value.trim() });
        ollamaTest = null;
        refreshOllama();
      });
      input.addEventListener("keydown", (e) => {
        if (e.key === "Enter") saveUrl.click();
      });
      const r = document.createElement("div");
      r.className = "wrapped-setting-row llm-key-row";
      r.append(labeled("Ollama on another computer? Its address (optional)", input), saveUrl);
      return r;
    };
    // Tier 2: a service and its key (or Ollama's address), the model, Light
    // or Heavy, and a Test button
    const llmProvider = wrappedSettings.llmProvider || "anthropic";
    const onPc = llmProvider === "ollama";
    // (Ollama: answering, or installed for Wrapped to start, with the model)
    const ollamaThere = !!(ollamaState?.running || (!wrappedSettings.ollamaManual && ollamaState?.local !== false && ollamaState?.installed));
    const llmReady = onPc ? ollamaThere && !!ollamaState?.modelReady : !!wrappedSettings.llmKey;
    const llmCard = tier(
      2,
      "Language model",
      "Reads what's written about each pick (Roon's reviews and bios, Last.fm, Wikipedia, and Discogs' styles with a token) against the albums you played of the artist it came from, drops the ones that don't fit, and picks the song to open with. Light: tier 1 decides the order. Heavy: also brings in artists of its own (checked like the rest) and puts the list in order.",
      llmReady ? "Activated" : onPc ? (ollamaState ? "Needs setup" : "Checking") : "Needs a key",
      llmReady ? "on" : "setup",
    );
    const llmBox = document.createElement("div");
    llmBox.className = "llm-setup";
    const providers = [
      ["anthropic", "Anthropic"],
      ["openai", "OpenAI"],
      ["gemini", "Gemini"],
      ["ollama", "Ollama (on this PC)"],
    ];
    const defaults = { anthropic: "claude-haiku-5-5", openai: "gpt-4o-mini", gemini: "gemini-2.5-flash", ollama: "gemma4:12b" };
    const providerRow = document.createElement("div");
    providerRow.className = "wrapped-tabs small";
    for (const [value, label] of providers) {
      const pill = document.createElement("button");
      pill.className = `wrapped-tab${llmProvider === value ? " selected" : ""}`;
      pill.textContent = label;
      pill.addEventListener("click", async () => {
        if (llmProvider === value) return;
        // (lit at once; the card follows when the page is drawn again)
        for (const other of providerRow.children) other.classList.toggle("selected", other === pill);
        // (a model name only means something to its own service)
        await save({ llmProvider: value, llmModel: "" });
        redrawSettingsInPlace();
      });
      // (Ollama's status asks Windows about the graphics card and drives,
      // which takes a moment the first time: started on the way to a click)
      if (value === "ollama" && llmProvider !== "ollama") {
        pill.addEventListener("pointerenter", () => fetchOllamaState(), { once: true });
      }
      providerRow.append(pill);
    }
    // Two parts: the model (service, key, model, Light / Heavy, Test), and
    // the optional extra reading (Discogs), each with its own heading
    const section = (title, text) => {
      const box = document.createElement("div");
      box.className = "llm-section";
      const h = document.createElement("div");
      h.className = "llm-section-title";
      h.textContent = title;
      box.append(h);
      if (text) box.append(wrappedNote(text, "wrapped-note small"));
      return box;
    };
    // An input with its name above it
    const labeled = (label, input) => {
      const box = document.createElement("label");
      box.className = "llm-field";
      const name = document.createElement("span");
      name.className = "llm-field-label";
      name.textContent = label;
      box.append(name, input);
      return box;
    };
    const modelSection = section("Model", "The service that reads the picks, and your key for it.");
    modelSection.append(providerRow);
    llmBox.append(modelSection);
    const keyRow = document.createElement("div");
    keyRow.className = "wrapped-setting-row";
    const keyInput = document.createElement("input");
    keyInput.className = "wrapped-search";
    keyInput.autocomplete = "off";
    if (llmProvider === "ollama") {
      keyInput.type = "text";
      keyInput.placeholder = "http://localhost:11434";
      keyInput.value = wrappedSettings.llmUrl || "";
    } else {
      keyInput.type = "password";
      keyInput.placeholder = `${providers.find((p) => p[0] === llmProvider)[1]} API key`;
      keyInput.value = wrappedSettings.llmKey || "";
    }
    const modelInput = document.createElement("input");
    modelInput.className = "wrapped-search llm-model";
    modelInput.type = "text";
    modelInput.placeholder = `model (default ${defaults[llmProvider]})`;
    modelInput.value = wrappedSettings.llmModel || "";
    modelInput.autocomplete = "off";
    const saveLlm = document.createElement("button");
    saveLlm.className = "wrapped-button";
    saveLlm.textContent = "Save";
    saveLlm.addEventListener("click", async () => {
      const changes = { llmModel: modelInput.value.trim() };
      if (llmProvider === "ollama") changes.llmUrl = keyInput.value.trim();
      else changes.llmKey = keyInput.value.trim();
      await save(changes);
      redrawSettingsInPlace();
    });
    for (const input of [keyInput, modelInput]) {
      input.addEventListener("keydown", (e) => {
        if (e.key === "Enter") saveLlm.click();
      });
    }
    keyRow.classList.add("llm-key-row");
    keyRow.append(
      labeled(llmProvider === "ollama" ? "Ollama's address" : `${providers.find((p) => p[0] === llmProvider)[1]} API key`, keyInput),
      labeled("Model (optional)", modelInput),
      saveLlm,
    );
    if (onPc) modelSection.append(ollamaSteps(), ollamaChoice(), ollamaAddress());
    else modelSection.append(keyRow);
    if (llmProvider === "openai") {
      modelSection.append(wrappedNote("OpenAI hasn't been tested yet (no account to test with). If it misbehaves, please report it.", "wrapped-note small"));
    } else if (llmProvider === "gemini") {
      modelSection.append(wrappedNote("Gemini's free tier allows a few calls a minute, so a build takes longer, and Google may use free-tier prompts to train its models.", "wrapped-note small"));
    }
    // Discogs (optional): styles per album, chosen by the people who
    // catalogue them
    const discogsRow = document.createElement("div");
    discogsRow.className = "wrapped-setting-row";
    const discogsInput = document.createElement("input");
    discogsInput.className = "wrapped-search";
    discogsInput.type = "password";
    discogsInput.autocomplete = "off";
    discogsInput.placeholder = "not set";
    discogsInput.value = wrappedSettings.discogsToken || "";
    const saveDiscogs = document.createElement("button");
    saveDiscogs.className = "wrapped-button";
    saveDiscogs.textContent = "Save";
    saveDiscogs.addEventListener("click", () => save({ discogsToken: discogsInput.value.trim() }));
    discogsInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") saveDiscogs.click();
    });
    discogsRow.classList.add("llm-key-row");
    discogsRow.append(labeled("Discogs token", discogsInput), saveDiscogs);
    const discogsNote = document.createElement("p");
    discogsNote.className = "wrapped-note small";
    discogsNote.append(
      "Discogs lists styles for each album (\"Melodic Death Metal\" on one record, \"Power Metal\" on another), which helps pick the right era. A personal token is free: ",
      link("https://www.discogs.com/settings/developers", "generate one here"),
      " (tick \"I have read and agree to the Discogs API Terms of Use\" first, then Generate token, and paste it below).",
    );
    const readingSection = section("Extra reading (optional)", null);
    readingSection.append(discogsNote, discogsRow);
    const modeRow = document.createElement("div");
    modeRow.className = "wrapped-tabs small";
    const llmMode = wrappedSettings.llmMode === "heavy" ? "heavy" : "light";
    for (const [value, label, soon] of [
      ["light", "Light", false],
      ["heavy", "Heavy", false],
    ]) {
      const pill = document.createElement("button");
      pill.className = `wrapped-tab${llmMode === value ? " selected" : ""}`;
      pill.textContent = label;
      pill.disabled = soon;
      pill.addEventListener("click", async () => {
        if (llmMode === value) return;
        await save({ llmMode: value });
        redrawSettingsInPlace();
      });
      modeRow.append(pill);
    }
    const testRow = document.createElement("div");
    testRow.className = "wrapped-setting-row llm-test";
    const test = document.createElement("button");
    test.className = "wrapped-button small";
    test.textContent = "Test";
    test.disabled = !llmReady;
    const testOut = document.createElement("span");
    testOut.className = "llm-test-out";
    test.addEventListener("click", async () => {
      test.disabled = true;
      testOut.textContent = "Asking...";
      try {
        testOut.textContent = await invoke("wrapped_llm_test");
      } catch (err) {
        testOut.textContent = String(err);
      }
      test.disabled = false;
    });
    if (onPc) testRow.append(modeRow);
    else testRow.append(modeRow, test, testOut);
    modelSection.append(testRow);
    // Deep Dive's own model (optional): it reads far more per playlist, so
    // a bigger model pays off there
    const diveSection = section(
      "Deep Dive's model (optional)",
      onPc
        ? "Deep Dive reads a lot more per playlist than the weekly lists. Another model you've downloaded can do it (empty: the one above)."
        : "Deep Dive reads a lot more per playlist than the weekly lists, so a bigger model pays off there: on Anthropic, claude-sonnet-5-5 or claude-opus-5-5 know far more bands and albums than Haiku, at a few times the cost. Empty: the model above.",
    );
    const diveRow = document.createElement("div");
    diveRow.className = "wrapped-setting-row llm-key-row";
    const diveInput = document.createElement("input");
    diveInput.className = "wrapped-search";
    diveInput.type = "text";
    diveInput.autocomplete = "off";
    diveInput.placeholder = onPc ? "same as above" : "same as above, e.g. claude-sonnet-5-5";
    diveInput.value = wrappedSettings.diveModel || "";
    const saveDive = document.createElement("button");
    saveDive.className = "wrapped-button";
    saveDive.textContent = "Save";
    saveDive.addEventListener("click", async () => {
      await save({ diveModel: diveInput.value.trim() });
      wrappedToast(diveInput.value.trim() ? `Deep Dive uses ${diveInput.value.trim()}.` : "Deep Dive uses the model above.");
    });
    diveInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") saveDive.click();
    });
    diveRow.append(labeled("Model for Deep Dive", diveInput), saveDive);
    diveSection.append(diveRow);
    llmBox.append(diveSection);
    llmBox.append(readingSection);
    llmCard.append(...setupFold("llm", onPc && !ollamaState ? null : llmReady, llmBox));
    // Tier 3: two models to download once (sound.rs), then songs are heard
    // from your own files through Roon, else from their previews
    const soundOn = !!soundState?.ready;
    const soundCard = tier(
      3,
      "Sound",
      "Listens to the songs with two models that run on this PC, so songs are compared by how they actually sound, not only by what's written about them: all of each song from your own files, wherever Roon keeps them, and the rest from their 30-second previews (Deezer's, else Apple's). To try it: Sound check on Deep Dive's page, with a song and an album picked.",
      soundOn ? "Activated" : soundRunning() ? "Downloading" : soundState ? "Needs download" : "Checking",
      soundOn ? "on" : "setup",
    );
    const soundBox = document.createElement("div");
    soundBox.className = "llm-setup";
    const soundSection = document.createElement("div");
    soundSection.className = "llm-section";
    const soundTitle = document.createElement("div");
    soundTitle.className = "llm-section-title";
    soundTitle.textContent = "The models";
    const soundMb = Math.round((soundState?.bytes || 36664655) / 1e6);
    const soundAbout = document.createElement("p");
    soundAbout.className = "wrapped-note small";
    soundAbout.append(
      "Discogs-EffNet, from the Music Technology Group at Universitat Pompeu Fabra (",
      link("https://essentia.upf.edu/models.html#discogs-effnet", "Essentia's models"),
      `), trained on Discogs: one knows 400 styles of music, the other which artists a song sounds like. A one-time download of ${soundMb} MB. They're free for non-commercial use (CC BY-NC-SA 4.0), as Toasted is.`,
    );
    soundSection.append(soundTitle, soundAbout);
    if (soundRunning()) {
      // The bar while it downloads, with Stop
      const view = document.createElement("div");
      view.className = "ollama-progress";
      const track = document.createElement("div");
      track.className = "ollama-bar";
      const fill = document.createElement("div");
      fill.className = "ollama-bar-fill";
      track.append(fill);
      const label = document.createElement("span");
      label.className = "ollama-progress-label";
      const stop = document.createElement("button");
      stop.className = "wrapped-button small";
      stop.textContent = "Stop";
      stop.addEventListener("click", () => invoke("sound_cancel"));
      const stopRow = document.createElement("div");
      stopRow.className = "wrapped-setting-row ollama-row";
      stopRow.append(label, stop);
      view.append(track, stopRow);
      soundView = { box: view, fill, label, stop };
      soundSection.append(view);
      paintSoundProgress();
    } else if (soundOn) {
      const n = soundState.heard || 0;
      const files = soundState.files || 0;
      const heard = n
        ? `${n.toLocaleString()} song${n === 1 ? "" : "s"} heard so far${files ? ` (${files === n ? "all" : files.toLocaleString()} from your own files)` : ""}`
        : "No songs heard yet";
      soundSection.append(wrappedNote(`Downloaded and working. ${heard}; this PC hears ${soundState.workers} previews at a time.`, "wrapped-note small"));
    } else {
      if (soundJob?.stage === "error") soundSection.append(wrappedNote(soundJob.error, "wrapped-note small ollama-error"));
      const goRow = document.createElement("div");
      goRow.className = "wrapped-setting-row tier-actions";
      const go = document.createElement("button");
      go.className = "wrapped-button small";
      go.textContent = soundJob ? "Try again" : `Download (${soundMb} MB)`;
      go.addEventListener("click", () => {
        soundJob = { stage: "download", done: 0, total: 0, error: "" };
        redrawSettingsInPlace();
        // (its events report how it goes and how it ends)
        invoke("sound_download").catch((err) => {
          if (String(err).includes("already")) wrappedToast(String(err));
        });
      });
      goRow.append(go);
      soundSection.append(goRow);
    }
    soundBox.append(soundSection);
    soundCard.append(...setupFold("sound", soundState ? soundOn : null, soundBox));
    // The on ones from the top together, then one card each
    let i = 0;
    const group = [];
    while (i < sections.length && sections[i].on) group.push(sections[i++]);
    const cards = group.length ? [group] : [];
    for (; i < sections.length; i++) cards.push([sections[i]]);
    for (const members of cards) {
      const card = document.createElement("div");
      const on = members.every((m) => m.on);
      card.className = `picking-tier${on ? " on" : ""}${members.length > 1 ? " combined" : ""}`;
      if (!on) card.classList.add(members[0].el.classList.contains("setup") ? "setup" : "soon");
      card.append(...members.map((m) => m.el));
      tierCards.append(card);
    }
    tiers.append(tierCards);
  }

  const toasted = field("Toasted's settings", "The Roon Core, the Toaster, hotkeys, toasts and the Wrapper live in the app's own Settings window.");
  const openToasted = document.createElement("button");
  openToasted.className = "wrapped-button";
  openToasted.textContent = "Open Toasted's Settings";
  openToasted.addEventListener("click", () => send("open_window", { window: "settings" }));
  toasted.append(openToasted);
  // (above Similar by, at the top of the left column)
  left.prepend(toasted);

  const similar = field(
    "Similar by",
    'Which row the "Similar" section under artists and albums opens with. Roon is Roon\'s own picks; the others come from the tags.',
  );
  const pills = document.createElement("div");
  pills.className = "wrapped-tabs small";
  for (const [value, label] of SIMILAR_BY) {
    const pill = document.createElement("button");
    pill.className = `wrapped-tab${wrappedSettings.similarBy === value ? " selected" : ""}`;
    pill.textContent = label;
    pill.addEventListener("click", async () => {
      await save({ similarBy: value });
      for (const other of pills.children) other.classList.toggle("selected", other === pill);
    });
    pills.append(pill);
  }
  similar.append(pills);

  const scopeField = field(
    "Similar looks in",
    "Library shows artists you already have; Discover only artists you don't. The row has the switch too; this is where it starts.",
  );
  const scopePills = document.createElement("div");
  scopePills.className = "wrapped-tabs small";
  for (const [value, label] of SIMILAR_SCOPES) {
    const pill = document.createElement("button");
    pill.className = `wrapped-tab${(wrappedSettings.similarScope || "library") === value ? " selected" : ""}`;
    pill.textContent = label;
    pill.addEventListener("click", async () => {
      await save({ similarScope: value });
      for (const other of scopePills.children) other.classList.toggle("selected", other === pill);
    });
    scopePills.append(pill);
  }
  scopeField.append(scopePills);

  const n = (x) => (x ?? 0).toLocaleString();
  const copied = historyStatus
    ? `${n(historyStatus.plays)} plays copied from Roon's history${historyStatus.syncedAt ? `, read ${historyWhen(historyStatus.syncedAt)}` : ""}`
    : "History not read yet";
  const records = field(
    "Wrapped's records",
    `${copied}. ${n(tagCount)} artist${tagCount === 1 ? "" : "s"} in the tag cache. Both live in the app's data folder.`,
  );
  const libraryField = field(
    "Your library's artists",
    "The main artists in your Roon library, read once so Wrapped can tell what's in it and what isn't (Library and Discover). Read it again after adding a lot of music.",
  );
  const libRow = document.createElement("div");
  libRow.className = "wrapped-setting-row";
  const libButton = document.createElement("button");
  libButton.className = "wrapped-button";
  const libText = document.createElement("span");
  libText.className = "wrapped-setting-text inline";
  const showLibrary = (st) => {
    libButton.textContent = st.syncing ? "Reading..." : st.artists ? "Read again" : "Read the library";
    libButton.disabled = st.syncing;
    libText.textContent = st.artists
      ? `${st.artists.toLocaleString()} artists${st.albums ? `, ${st.albums.toLocaleString()} albums` : ""}${st.tracks ? `, ${st.tracks.toLocaleString()} tracks` : ""}${st.syncedAt ? `, read ${historyWhen(st.syncedAt)}` : ""}`
      : "Not read yet";
  };
  invoke("wrapped_library_status").then(showLibrary).catch(() => {});
  libButton.addEventListener("click", async () => {
    try {
      showLibrary(await invoke("wrapped_library_sync"));
    } catch (err) {
      wrappedToast(String(err));
    }
  });
  const unlistenLibrary = await listen("wrapped-library", (event) => {
    if (ticket !== wrappedTicket) return unlistenLibrary();
    const p = event.payload;
    if (p.error) wrappedToast(p.error);
    if (p.done) {
      invoke("wrapped_library_status").then(showLibrary).catch(() => {});
      // The library's artists join the ones to tag
      invoke("wrapped_tags_status").then(showTagging).catch(() => {});
    } else libText.textContent = `Reading: ${p.read.toLocaleString()} of ${p.total.toLocaleString()} ${p.what || "artists"}...`;
  });
  libRow.append(libButton, libText);
  libraryField.append(libRow);

  const tagging = field(
    "Tags for your artists",
    "The Similar by Genre, Style, Scene and Era rows compare tags, so the artists in your history and library need tagging first: one lookup a second, most played first, in the background. Stop and start it as you like; it carries on where it left off.",
  );
  const tagRow = document.createElement("div");
  tagRow.className = "wrapped-setting-row";
  const tagButton = document.createElement("button");
  tagButton.className = "wrapped-button";
  const tagText = document.createElement("span");
  tagText.className = "wrapped-setting-text inline";
  const showTagging = (st) => {
    tagButton.textContent = st.filling ? "Stop" : st.tagged >= st.artists ? "All tagged" : "Tag my artists";
    tagButton.disabled = !st.filling && st.tagged >= st.artists;
    tagText.textContent = `${st.tagged.toLocaleString()} of ${st.artists.toLocaleString()} tagged`;
  };
  invoke("wrapped_tags_status").then(showTagging).catch(() => {});
  tagButton.addEventListener("click", async () => {
    const st = await invoke("wrapped_tags_status");
    showTagging(await invoke("wrapped_tags_fill", { stop: st.filling }));
  });
  const unlistenTags = await listen("wrapped-tags", (event) => {
    if (ticket !== wrappedTicket) return unlistenTags();
    invoke("wrapped_tags_status").then(showTagging).catch(() => {});
  });
  tagRow.append(tagButton, tagText);
  tagging.append(tagRow);

  const actions = document.createElement("div");
  actions.className = "wrapped-setting-row";
  const reread = document.createElement("button");
  reread.className = "wrapped-button";
  reread.textContent = "Read the whole history again";
  reread.addEventListener("click", async () => {
    try {
      await invoke("wrapped_history_sync", { full: true });
      wrappedToast("Reading the history from Roon; the History page shows the progress.");
    } catch (err) {
      wrappedToast(String(err));
    }
  });
  actions.append(reread);
  records.append(actions);
};

WRAPPED_PAGES.genre = async function (entry) {
  wrappedHeading(entry.name, "Genre");
  if (wrappedNeedsLibrary()) return;
  const ticket = ++wrappedTicket;
  wrappedBody.append(wrappedNote("Loading..."));
  let page;
  try {
    page = await invoke("library_genre_page", { name: entry.name });
  } catch (err) {
    if (ticket === wrappedTicket) wrappedFailed(err);
    return;
  }
  if (ticket !== wrappedTicket) return;
  wrappedBody.replaceChildren();
  const holder = document.createElement("div");
  wrappedBody.append(holder);
  const any = [
    wrappedPicks(holder, "Top artists", page.topArtists.map(artistItem), openArtist),
    wrappedPicks(holder, "Artists Roon recommends", page.recommendedArtists.map(artistItem), openArtist),
    wrappedPicks(holder, "Top albums", page.topAlbums.map(albumItem), openAlbum),
    wrappedPicks(holder, "Albums Roon recommends", page.recommendedAlbums.map(albumItem), openAlbum),
    wrappedPicks(holder, "New releases", page.newReleases.map(albumItem), openAlbum),
  ].some(Boolean);
  if (!any) holder.append(wrappedNote("Roon has nothing to show for this genre."));
};

// "Open in Roon": Roon's own search run for the name (wrapper.rs types it
// into Roon's search box); the page closes so Roon's results show. (The
// Core's share links were tried first: GetShareLink answers NotFound.)
const OPEN_IN_ROON_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 3h6v6"/><path d="M10 14 21 3"/><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/></svg>';
function wrappedOpenInRoon(text, pick) {
  return wrappedIconButton(OPEN_IN_ROON_ICON, "Open in Roon", async () => {
    // The page stays, with a spinner, while Roon is searched and the exact
    // result clicked behind it (read off the results page); then it closes
    // onto Roon's page (or its results, when nothing matched exactly)
    wrappedBusy(`Opening ${pick} in Roon...`);
    let picked = false;
    let failed = null;
    try {
      picked = await invoke("wrapper_open_in_roon", { text, pick });
    } catch (err) {
      failed = String(err);
    }
    wrappedBusy(null);
    if (failed) return wrappedToast(failed);
    setWrappedOpen(false);
    if (!picked) wrapperToast(`Roon's results for ${text}; nothing matched "${pick}" exactly.`);
  });
}

// A spinner over the Wrapped page while something takes a moment
// A question over the page, like a warning: its title, a line of text
// and the choices (`actions`: { value, label, kind "danger" }); resolves
// to the chosen value (null for Cancel, Escape or a click outside)
function wrappedConfirm({ title, text, actions }) {
  return new Promise((resolve) => {
    wrapped.querySelector(".wrapped-dialog-back")?.remove();
    const back = document.createElement("div");
    back.className = "wrapped-dialog-back";
    const box = document.createElement("div");
    box.className = "wrapped-dialog";
    box.setAttribute("role", "alertdialog");
    const head = document.createElement("div");
    head.className = "wrapped-dialog-title";
    head.textContent = title;
    box.append(head);
    if (text) box.append(wrappedNote(text, "wrapped-note small"));
    const row = document.createElement("div");
    row.className = "wrapped-dialog-actions";
    const finish = (value) => {
      document.removeEventListener("keydown", onKey, true);
      back.remove();
      resolve(value);
    };
    const onKey = (event) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        finish(null);
      }
    };
    let first = null;
    for (const a of actions) {
      const btn = document.createElement("button");
      btn.className = `wrapped-button${a.kind ? ` ${a.kind}` : ""}`;
      btn.textContent = a.label;
      btn.addEventListener("click", () => finish(a.value));
      row.append(btn);
      if (a.value == null) first = btn;
    }
    box.append(row);
    back.append(box);
    back.addEventListener("pointerdown", (event) => {
      if (event.target === back) finish(null);
    });
    document.addEventListener("keydown", onKey, true);
    wrapped.append(back);
    (first || row.lastChild)?.focus();
  });
}

function wrappedBusy(text) {
  let busy = wrapped.querySelector(".wrapped-busy");
  if (!text) {
    busy?.remove();
    return;
  }
  if (!busy) {
    busy = document.createElement("div");
    busy.className = "wrapped-busy";
    busy.innerHTML = '<div class="wrapped-spinner"></div><div class="wrapped-busy-text"></div>';
    wrapped.append(busy);
  }
  busy.querySelector(".wrapped-busy-text").textContent = text;
}

// The playlists Wrapped makes are named with this, so they're told apart
const WRAPPED_PREFIX = "Wrapped: ";
// The weekly ones (discover.rs) carry their own names, dated copies too
const WEEKLY_NAMES = ["🌟 Discover Weekly", "💎 Library Rediscover", "🧠 "];
const isWrappedPlaylist = (p) => p.name.startsWith(WRAPPED_PREFIX) || WEEKLY_NAMES.some((n) => p.name.startsWith(n));
const PLAYLIST_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M11 12H3"/><path d="M16 6H3"/><path d="M16 18H3"/><path d="M18 9v6"/><path d="M21 12h-6"/></svg>';
const TRASH_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/></svg>';

// One weekly list on the Discover page: the two dated copies small on
// top, the current one large with Play and Build now
function discoverColumn(which, title, blurb, built, building, failed, status) {
  const column = document.createElement("section");
  column.className = "discover-column";
  const head = document.createElement("div");
  head.className = "discover-head";
  const name = document.createElement("h3");
  name.textContent = title;
  head.append(name, wrappedNote(blurb, "wrapped-note small"));
  column.append(head);

  const [current, ...older] = built;
  if (older.length) {
    const row = document.createElement("div");
    row.className = "discover-old";
    for (const copy of older) row.append(discoverCard(copy, false, which));
    column.append(row);
  }
  if (current) column.append(discoverCard(current, true, which));
  else column.append(wrappedNote("Not built yet: the first one comes next Monday, or build it now.", "wrapped-note discover-empty"));

  const build = document.createElement("button");
  build.className = "wrapped-button discover-build";
  build.textContent = building === which ? "Building..." : current ? "Rebuild now" : "Build now";
  build.disabled = !!building;
  build.title = current ? "Build this week's list again (the one now becomes a dated copy)" : "Build the list now";
  build.addEventListener("click", () => discoverBuild(which, title));
  column.append(build);
  // (why the last build didn't make a new list; the details are in
  // discover-log.txt in the app data folder)
  if (failed && building !== which) column.append(wrappedNote(`The last build didn't finish: ${failed}`, "wrapped-note error"));
  if (status) column.append(discoverAskPanel(status, which));
  return column;
}

// ----- Fine Tuning: asking Discover Weekly for something -----
// What's typed so far, per list (kept while the page redraws)
const discoverAskText = { weekly: "", rediscover: "" };

const TUNE_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><defs><clipPath id="tune-cut"><path clip-rule="evenodd" d="M-2 -2h28v28H-2z M17 10.7a6.3 6.3 0 1 0 0 12.6a6.3 6.3 0 1 0 0-12.6z"/></clipPath></defs><g clip-path="url(#tune-cut)"><g transform="scale(0.75)" stroke-width="2.67"><path d="M12 5a3 3 0 1 0-5.997.125 4 4 0 0 0-2.526 5.77 4 4 0 0 0 .556 6.588A4 4 0 1 0 12 18Z"/><path d="M12 5a3 3 0 1 1 5.997.125 4 4 0 0 1 2.526 5.77 4 4 0 0 1-.556 6.588A4 4 0 1 1 12 18Z"/><path d="M15 13a4.5 4.5 0 0 1-3-4 4.5 4.5 0 0 1-3 4"/><path d="M17.599 6.5a3 3 0 0 0 .399-1.375"/><path d="M6.003 5.125A3 3 0 0 0 6.401 6.5"/><path d="M3.477 10.896a4 4 0 0 1 .585-.396"/><path d="M19.938 10.5a4 4 0 0 1 .585.396"/><path d="M6 18a4 4 0 0 1-1.967-.516"/><path d="M19.967 17.484A4 4 0 0 1 18 18"/></g></g><g transform="translate(11 11.05) scale(0.5)" stroke-width="4"><path d="M11.525 2.295a.53.53 0 0 1 .95 0l2.31 4.679a2.123 2.123 0 0 0 1.595 1.16l5.166.756a.53.53 0 0 1 .294.904l-3.736 3.638a2.123 2.123 0 0 0-.611 1.878l.882 5.14a.53.53 0 0 1-.771.56l-4.618-2.428a2.122 2.122 0 0 0-1.973 0L6.396 21.01a.53.53 0 0 1-.77-.56l.881-5.139a2.122 2.122 0 0 0-.611-1.879L2.16 9.795a.53.53 0 0 1 .294-.906l5.165-.755a2.122 2.122 0 0 0 1.597-1.16z"/></g></svg>';
const CLOSE_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></svg>';

// Fine Tuning, under Build now: what you'd like the next Discover Weekly
// to bring, in your own words; then the requests standing, each for the
// next list or, with Permanent, every one
function discoverAskPanel(status, which = "weekly") {
  const panel = document.createElement("section");
  panel.className = "discover-ask";
  const redraw = (next) => panel.replaceWith(discoverAskPanel(next, which));
  const rediscover = which === "rediscover";
  const requests = rediscover ? status.rediscoverRequests : status.requests;
  const button = (className, html, label, onClick) => {
    const b = document.createElement("button");
    b.className = className;
    b.innerHTML = html;
    if (label) b.setAttribute("aria-label", label);
    b.addEventListener("click", onClick);
    return b;
  };

  const head = document.createElement("div");
  head.className = "discover-ask-head";
  head.innerHTML = TUNE_ICON;
  const title = document.createElement("h4");
  title.textContent = "Fine Tuning";
  head.append(title);
  panel.append(
    head,
    wrappedNote(
      rediscover
        ? "Tell the next Library Rediscover what to dig up from your library, in your own words: a sound, an era, a mood. It takes a third of the list; say \"all\" or \"only\" and it takes the whole list."
        : "Tell the next Discover Weekly what you'd like, in your own words: more of a sound, less of something, an era.",
      "wrapped-note small",
    ),
  );

  const row = document.createElement("div");
  row.className = "discover-ask-row";
  const input = document.createElement("input");
  input.className = "wrapped-search";
  input.type = "text";
  input.maxLength = 300;
  input.placeholder = rediscover ? "e.g. doom metal I haven't heard in a long time" : "e.g. more atmospheric black metal with clean vocals";
  input.value = discoverAskText[which];
  input.addEventListener("input", () => (discoverAskText[which] = input.value));
  const add = document.createElement("button");
  add.className = "wrapped-button";
  add.textContent = "Add";
  const send = async () => {
    const text = input.value.trim();
    if (!text) return input.focus();
    add.disabled = true;
    try {
      const next = await invoke("wrapped_discover_request_add", { text, song: null, aspects: [], which });
      discoverAskText[which] = "";
      redraw(next);
    } catch (err) {
      wrappedToast(String(err));
      add.disabled = false;
    }
  };
  add.addEventListener("click", send);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      send();
    }
  });
  row.append(input, add);
  panel.append(row);
  if (!status.tier2) {
    panel.append(wrappedNote("Fine Tuning is read by tier 2, a language model (Wrapped's Settings). Until it's set up, what you add here waits.", "wrapped-note small"));
  }

  if (requests?.length) {
    const list = document.createElement("ul");
    list.className = "discover-requests";
    for (const r of requests) {
      const item = document.createElement("li");
      item.className = "discover-request";
      const text = document.createElement("div");
      text.className = "discover-request-text";
      text.textContent = r.said;
      const when = document.createElement("span");
      when.className = "discover-request-when";
      when.textContent = r.permanent ? "Every list" : "Next list";
      text.append(when);
      const keep = button(`discover-request-keep${r.permanent ? " on" : ""}`, "", null, async () => {
        try {
          redraw(await invoke("wrapped_discover_request_keep", { id: r.id, permanent: !r.permanent }));
        } catch (err) {
          wrappedToast(String(err));
        }
      });
      keep.textContent = "Permanent";
      keep.setAttribute("aria-pressed", r.permanent ? "true" : "false");
      keep.title = r.permanent ? "Kept for every list: click to use it for the next list only" : "Keep this for every list, not just the next one";
      const remove = button("discover-ask-x", CLOSE_ICON, "Remove this", async () => {
        try {
          redraw(await invoke("wrapped_discover_request_remove", { id: r.id }));
        } catch (err) {
          wrappedToast(String(err));
        }
      });
      item.append(text, keep, remove);
      list.append(item);
    }
    panel.append(list);
  }
  return panel;
}

// Which cards show their track list (by list and build time), kept while
// the page redraws
const discoverOpen = new Set();
const CHEVRON_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg>';

// A part of a card that folds away or out (grid rows 1fr / 0fr)
function discoverFold(className, ...children) {
  const fold = document.createElement("div");
  fold.className = `discover-fold ${className}`;
  const inner = document.createElement("div");
  inner.className = "discover-fold-inner";
  inner.append(...children);
  fold.append(inner);
  return fold;
}

// One list's card: this week's large (cover, artists, Play), the dated
// copies small. A click anywhere but Play swaps the cover and summary for
// the numbered track list, and back.
// A list's cover: Roon's own for the playlist, or (before Roon has made
// one) the first songs' album covers, four in a square like Roon's
function discoverCover(imageKey, covers, size, name) {
  if (imageKey || covers.length < 4) return wrappedArt(imageKey || covers[0], size, name);
  const box = document.createElement("div");
  box.className = "wrapped-art discover-mosaic";
  for (const key of covers.slice(0, 4)) {
    const img = document.createElement("img");
    img.loading = "lazy";
    img.alt = "";
    img.src = `${convertFileSrc(key, "roonimg")}?width=${Math.round(size / 2)}&height=${Math.round(size / 2)}&src=broker`;
    box.append(img);
  }
  return box;
}

function discoverCard(copy, large, which) {
  const key = `${which}-${copy.builtAt}`;
  const card = document.createElement("div");
  card.className = `discover-card${large ? " large" : ""}${discoverOpen.has(key) ? " open" : ""}`;
  card.tabIndex = 0;
  card.setAttribute("role", "button");
  card.setAttribute("aria-expanded", discoverOpen.has(key) ? "true" : "false");
  card.title = "Show the songs";
  const p = copy.playlist;
  const art = discoverCover(p?.imageKey, copy.covers || [], large ? 400 : 120, copy.name);
  const text = document.createElement("div");
  text.className = "discover-text";
  const head = document.createElement("div");
  head.className = "discover-card-head";
  const title = document.createElement("div");
  title.className = "discover-title";
  title.textContent = large ? `This week, ${copy.date}` : copy.date;
  const chevron = document.createElement("span");
  chevron.className = "discover-chevron";
  chevron.innerHTML = CHEVRON_ICON;
  head.append(title, chevron);
  const sub = document.createElement("div");
  sub.className = "discover-sub";
  const length = p?.lengthSeconds ? formatLongTime(p.lengthSeconds) : null;
  sub.textContent = [`${copy.songs} songs`, length, p ? null : "deleted in Roon"].filter(Boolean).join(" · ");
  text.append(head, sub);
  if (large && copy.artists.length) {
    const who = document.createElement("div");
    who.className = "discover-artists";
    const shown = copy.artists.slice(0, 8).join(", ");
    who.textContent = copy.artists.length > 8 ? `${shown} and ${copy.artists.length - 8} more` : shown;
    text.append(discoverFold("summary", who));
  }
  if (p) {
    const play = document.createElement("button");
    play.className = large ? "wrapped-play discover-play" : "wrapped-play discover-play small";
    play.innerHTML = `${WRAPPED_PLAY_ICON}<span>Play</span>`;
    play.title = `Play ${p.name}`;
    play.addEventListener("click", (e) => {
      e.stopPropagation();
      if (!zone) return wrappedToast("Pick a zone first.");
      invoke("library_play_playlist", { zoneId: zone.zoneId, handle: p.handle, clear: true })
        .then(() => wrappedToast(`Playing ${p.name} on ${zone.name}`))
        .catch((err) => wrappedToast(String(err)));
    });
    text.append(play);
  }
  // The track list: number, title, artist, and why it's there
  const list = document.createElement("ol");
  list.className = "discover-tracklist";
  (copy.tracks || []).forEach((t, i) => {
    const row = document.createElement("li");
    row.className = "discover-track";
    const num = document.createElement("span");
    num.className = "discover-track-num";
    num.textContent = `${i + 1}.`;
    const body = document.createElement("div");
    body.className = "discover-track-body";
    const name = document.createElement("div");
    name.className = "discover-track-title";
    name.textContent = t.title || "(untitled)";
    const who = document.createElement("div");
    who.className = "discover-track-artist";
    who.textContent = t.artist;
    body.append(name, who);
    const why = t.why || t.reason;
    if (why) {
      const line = document.createElement("div");
      line.className = "discover-track-why";
      line.textContent = why;
      line.title = why;
      body.append(line);
    }
    row.append(num, body);
    list.append(row);
  });
  if (!list.children.length) {
    const none = document.createElement("li");
    none.className = "discover-track empty";
    none.textContent = "No songs saved for this list.";
    list.append(none);
  }
  const face = large ? discoverFold("cover", art) : art;
  card.append(face, text, discoverFold("tracks", list));
  const toggle = () => {
    const open = !card.classList.contains("open");
    // (one open at a time in a column)
    if (open) {
      for (const other of card.closest(".discover-column")?.querySelectorAll(".discover-card.open") || []) {
        if (other !== card) other.dispatchEvent(new CustomEvent("discover-close"));
      }
    }
    card.classList.toggle("open", open);
    card.setAttribute("aria-expanded", open ? "true" : "false");
    card.title = open ? "Hide the songs" : "Show the songs";
    if (open) discoverOpen.add(key);
    else discoverOpen.delete(key);
  };
  card.addEventListener("discover-close", () => {
    card.classList.remove("open");
    card.setAttribute("aria-expanded", "false");
    card.title = "Show the songs";
    discoverOpen.delete(key);
  });
  card.addEventListener("click", (e) => {
    if (e.target.closest("button")) return;
    toggle();
  });
  card.addEventListener("keydown", (e) => {
    if (e.target !== card || (e.key !== "Enter" && e.key !== " ")) return;
    e.preventDefault();
    toggle();
  });
  return card;
}

// "Start fresh" under the lists: forgets ratings and skips and lets the
// songs and artists of the lists so far come back. Two clicks, no dialog.
function discoverResetRow(building) {
  const row = document.createElement("div");
  row.className = "discover-reset";
  const button = document.createElement("button");
  button.className = "wrapped-button small";
  button.textContent = "Start fresh";
  button.disabled = building;
  button.title = "Forget your ratings and skips, and let everything the lists have suggested come around again";
  const note = wrappedNote("Forgets your ratings and skips; past suggestions can come back. Your listening history stays.", "wrapped-note small");
  let armed = null;
  button.addEventListener("click", async () => {
    if (!armed) {
      button.textContent = "Click again to start fresh";
      armed = setTimeout(() => {
        armed = null;
        button.textContent = "Start fresh";
      }, 4000);
      return;
    }
    clearTimeout(armed);
    armed = null;
    button.disabled = true;
    try {
      await invoke("wrapped_discover_reset");
      wrappedToast("Started fresh. The next build picks from the whole pool again.");
      loadDiscoverRatings();
    } catch (err) {
      wrappedToast(String(err));
    }
    button.textContent = "Start fresh";
    button.disabled = false;
  });
  row.append(button, note);
  return row;
}

// Builds a list now, with the builder's progress over the page
async function discoverBuild(which, title) {
  wrappedBusy(`Building ${title}...`);
  const unlisten = await listen("wrapped-discover", (event) => {
    if (event.payload.which === which && event.payload.text) wrappedBusy(event.payload.text);
  });
  try {
    await invoke("wrapped_discover_build", { which, utcOffsetMinutes: -new Date().getTimezoneOffset() });
    wrappedToast(`${title} is ready.`);
  } catch (err) {
    wrappedToast(String(err));
  } finally {
    unlisten();
    wrappedBusy(null);
    loadDiscoverRatings();
    if (wrappedStack.at(-1)?.page === "discover" && !wrapped.hidden) renderWrapped();
  }
}

function playlistRows(list, mine) {
  const rows = document.createElement("div");
  rows.className = "wrapped-tracks";
  for (const p of list) {
    const row = document.createElement("div");
    row.className = "wrapped-track";
    row.append(
      wrappedIconButton(WRAPPED_PLAY_ICON, "Play now", () => {
        if (!zone) return wrappedToast("Pick a zone first.");
        invoke("library_play_playlist", { zoneId: zone.zoneId, handle: p.handle })
          .then(() => wrappedToast(`Playing ${p.name} on ${zone.name}`))
          .catch((err) => wrappedToast(String(err)));
      }, "wrapped-track-play"),
    );
    const art = wrappedArt(p.imageKey, 88, p.name);
    art.classList.add("small");
    const text = document.createElement("span");
    text.className = "wrapped-track-text";
    const title = document.createElement("span");
    title.className = "wrapped-track-title";
    title.textContent = mine && p.name.startsWith(WRAPPED_PREFIX) ? p.name.slice(WRAPPED_PREFIX.length) : p.name;
    const sub = document.createElement("span");
    sub.className = "wrapped-track-sub";
    sub.textContent = [
      p.tracks != null ? `${p.tracks} track${p.tracks === 1 ? "" : "s"}` : null,
      p.lengthSeconds ? formatLongTime(p.lengthSeconds) : null,
      p.kind === "roon" ? null : p.kind,
    ]
      .filter(Boolean)
      .join(" · ");
    text.append(title, sub);
    title.classList.add("link");
    title.addEventListener("click", () => openWrappedPage({ page: "playlist", handle: p.handle, name: p.name }));
    row.append(art, text);
    if (mine && !p.readOnly) {
      const del = wrappedIconButton(TRASH_ICON, "Delete this playlist", async () => {
        if (del.dataset.armed !== "1") {
          // Click twice: the first arms it
          del.dataset.armed = "1";
          del.classList.add("armed");
          wrappedToast(`Click the bin again to delete "${title.textContent}"`);
          setTimeout(() => {
            delete del.dataset.armed;
            del.classList.remove("armed");
          }, 4000);
          return;
        }
        del.classList.add("busy");
        try {
          await invoke("library_delete_playlist", { handle: p.handle });
          row.remove();
          wrappedToast(`Deleted "${title.textContent}"`);
        } catch (err) {
          wrappedToast(String(err));
          del.classList.remove("busy");
        }
      }, "wrapped-delete");
      row.append(del);
    }
    rows.append(row);
  }
  return rows;
}

// The album page's top: art, title, artist (a link), facts, Play now, heart, radio
function albumHeader(card) {
  const head = document.createElement("div");
  head.className = "wrapped-album-head";
  head.append(wrappedArt(card.imageKey, 600, card.title));
  const info = document.createElement("div");
  info.className = "wrapped-album-info";
  const title = document.createElement("h2");
  title.className = "wrapped-album-title";
  title.textContent = card.title;
  const artist = document.createElement("button");
  artist.className = "wrapped-link";
  artist.textContent = card.artist;
  if (card.performerHandle) {
    artist.addEventListener("click", () => openWrappedPage({ page: "artist", handle: card.performerHandle }));
  } else {
    artist.disabled = true;
  }
  const facts = document.createElement("div");
  facts.className = "wrapped-facts";
  facts.textContent = card.year ? String(card.year) : "";
  const genres = document.createElement("div");
  genres.className = "wrapped-genres roon";
  const outside = document.createElement("div");
  outside.className = "wrapped-genres outside";
  const actions = document.createElement("div");
  actions.className = "wrapped-actions";
  // Play now, and the other ways to play it (Add Next, Queue, Start
  // Radio, Deep Dive) behind its arrow
  const radioSeed = { name: (card.artist || "").split(/ \/ |, /)[0], handle: card.performerHandle, album: card.title, albumHandle: card.handle };
  const split = document.createElement("div");
  split.className = "wrapped-split";
  const play = document.createElement("button");
  play.className = "wrapped-play";
  play.innerHTML = `${WRAPPED_PLAY_ICON}<span>Play now</span>`;
  play.addEventListener("click", () => {
    if (!zone) return wrappedToast("Pick a zone first.");
    send("library_play_album", { zoneId: zone.zoneId, handle: card.handle, how: "now" });
  });
  const more = document.createElement("button");
  more.className = "wrapped-play wrapped-split-more";
  more.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg>';
  more.title = "More ways to play it";
  more.addEventListener("click", (event) => {
    event.stopPropagation();
    wrappedPlayMenu(split, {
      what: "album",
      handle: card.handle,
      seed: { kind: "album", handle: card.handle, title: card.title, artist: radioSeed.name, imageKey: card.imageKey, radio: radioSeed },
      radio: { albums: [card.handle] },
    });
  });
  split.append(play, more);
  actions.append(
    split,
    wrappedHeart({ inLibrary: card.inLibrary, favorite: card.favorite }, async (favorite) => {
      const next = card.inLibrary || favorite
        ? await invoke("library_album_heart_for", { handle: card.handle, favorite })
        : await invoke("library_album_add_for", { handle: card.handle });
      return { inLibrary: next.inLibrary, favorite: next.favorite };
    }),
    wrappedDive({ kind: "album", handle: card.handle, title: card.title, artist: radioSeed.name, imageKey: card.imageKey, radio: radioSeed }, () => wrappedRadioBuilder("album", radioSeed)),
    wrappedOpenInRoon(`${card.artist} ${card.title}`.trim(), card.title),
    wrappedIconButton(PLAYLIST_ICON, "Save this album as a Wrapped playlist", async () => {
      if (!wrappedAlbumTracks.length) return wrappedToast("The tracks haven't loaded yet.");
      const name = `${WRAPPED_PREFIX}${card.title}`;
      try {
        const made = await invoke("library_create_playlist", { name, tracks: wrappedAlbumTracks.map((t) => t.handle) });
        wrappedToast(`Saved "${made.name}" (${made.tracks ?? wrappedAlbumTracks.length} tracks)`);
      } catch (err) {
        wrappedToast(String(err));
      }
    }, "wrapped-save"),
  );
  info.append(title, artist, facts, genres, outside, actions);
  head.append(info);
  return head;
}

function artistHeader(card) {
  const head = document.createElement("div");
  head.className = "wrapped-artist-head";
  head.append(wrappedArt(card.imageKey, 600, card.name, true));
  const info = document.createElement("div");
  info.className = "wrapped-album-info";
  const name = document.createElement("h2");
  name.className = "wrapped-album-title";
  name.textContent = card.name;
  const facts = document.createElement("div");
  facts.className = "wrapped-facts";
  const genres = document.createElement("div");
  genres.className = "wrapped-genres roon";
  const outside = document.createElement("div");
  outside.className = "wrapped-genres outside";
  const actions = document.createElement("div");
  actions.className = "wrapped-actions";
  const radio = { name: card.name, handle: card.handle };
  actions.append(
    wrappedDive({ kind: "artist", handle: card.handle, title: card.name, imageKey: card.imageKey, radio }, () => wrappedRadioBuilder("artist", radio)),
    wrappedOpenInRoon(card.name, card.name),
  );
  info.append(name, facts, genres, outside, actions);
  head.append(info);
  return head;
}

// Roon's genre list and where an artist is from, under the name
function wrappedGenreChips(holder, genres) {
  holder.replaceChildren();
  for (const genre of genres) {
    const chip = document.createElement("button");
    chip.className = "wrapped-genre";
    chip.textContent = genre;
    chip.addEventListener("click", () => openWrappedPage({ page: "genre", name: genre }));
    holder.append(chip);
  }
}

// The outside world's tags for an artist (Last.fm, MusicBrainz; tags.rs),
// under Roon's: genre, style and scene, each its own color
async function wrappedOutsideTags(holder, name, ticket, album) {
  let tags;
  try {
    tags = album ? await invoke("wrapped_album_tags", { artist: name, album }) : await invoke("wrapped_artist_tags", { name });
  } catch (err) {
    console.warn("tags", err);
    return;
  }
  if (ticket !== wrappedTicket) return;
  holder.replaceChildren();
  // Roon's own genres ("Pop/Rock, Heavy Metal") only when the outside has
  // nothing better
  const roon = wrappedBody.querySelector(".wrapped-genres.roon");
  if (roon) roon.hidden = tags.tags.length > 0;
  if (!tags.tags.length) {
    const note = document.createElement("span");
    note.className = "wrapped-genre-note";
    note.textContent =
      tags.fromLastfm || tags.fromMusicbrainz
        ? "No outside tags for this artist."
        : wrappedSettings?.lastfmKey
          ? "Couldn't reach Last.fm or MusicBrainz."
          : "Add a Last.fm key in Wrapped's settings for richer tags.";
    holder.append(note);
    return;
  }
  for (const tag of tags.tags) {
    const chip = document.createElement("button");
    chip.className = `wrapped-genre outside ${tag.kind}`;
    chip.textContent = tag.name;
    chip.title = `${tag.kind}, weight ${tag.weight}`;
    chip.addEventListener("click", () => openWrappedPage({ page: "search", query: tag.name }));
    holder.append(chip);
  }
}

let wrappedSettings = null;
// One step of Scan all: listens for `event`, starts the job, and resolves
// once `done(payload)` says it finished (a throw in it rejects), or after
// ten minutes
async function wrappedScanStep(event, start, done) {
  let unlisten = () => {};
  try {
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("That took too long; try again.")), 600_000);
      listen(event, (e) => {
        try {
          if (done(e.payload)) {
            clearTimeout(timer);
            resolve();
          }
        } catch (err) {
          clearTimeout(timer);
          reject(err);
        }
      }).then((u) => {
        unlisten = u;
        start().catch((err) => {
          clearTimeout(timer);
          reject(err);
        });
      });
    });
  } finally {
    unlisten();
  }
}

// The tiers' setups on the Settings page: open or folded, by name (unset:
// decided by whether the tier is working yet)
const settingsFolds = {};

// Tier 3's setup (sound.rs): whether the models are here, the download
// running or just ended (its "sound-download" events), and its bar on the
// Settings page, updated in place while it runs
let soundState = null;
let soundJob = null;
let soundView = null;
const soundRunning = () => !!(soundJob && ["download", "check"].includes(soundJob.stage));

function fetchSoundState() {
  return invoke("sound_status")
    .then((st) => {
      soundState = st;
      // (a download that was running before the page loaded)
      if (st.job && ["download", "check"].includes(st.job.stage) && !soundRunning()) soundJob = st.job;
      return st;
    })
    .catch((err) => {
      console.warn("sound status", err);
      return null;
    });
}

function paintSoundProgress() {
  if (!soundJob || !soundView?.box.isConnected) return;
  const { done = 0, total = 0, stage } = soundJob;
  const counting = stage === "download" && total > 0;
  soundView.box.classList.toggle("waiting", !counting);
  soundView.fill.style.width = counting ? `${Math.min(100, (done / total) * 100)}%` : "";
  const mb = (bytes) => `${(bytes / 1e6).toFixed(1)} MB`;
  soundView.label.textContent =
    stage === "check" ? "Loading the models once, to be sure they run here..." : counting ? `Downloading the models: ${mb(done)} of ${mb(total)}` : "Downloading the models...";
  soundView.stop.hidden = stage !== "download";
}

// The scrobbles' reading, for the Settings page's line about it
let scrobblesPaint = null;
listen("wrapped-scrobbles", (event) => scrobblesPaint?.(event.payload));

listen("sound-download", (event) => {
  soundJob = event.payload;
  paintSoundProgress();
  // (a download the page isn't showing yet: drawn with its bar)
  if (soundRunning() && !soundView?.box.isConnected) redrawSettingsInPlace();
  if (!soundRunning()) {
    if (soundJob.stage === "done") soundJob = null;
    fetchSoundState().then(() => redrawSettingsInPlace());
  }
});
fetchSoundState();

// Ollama's setup on the Settings page (tier 2 on this PC, ollama.rs): the
// last status, the job running or just ended (an install or a model
// download, as its "ollama-setup" events tell it), the drive and size
// picked, and the last test's answer
// (the last status is kept between runs, so the page opens as it was
// instead of "Checking" while the first answer comes)
let ollamaState = (() => {
  try {
    return JSON.parse(localStorage.getItem("ollamaState") || "null");
  } catch {
    return null;
  }
})();
let ollamaJob = null;
let ollamaDrive = null;
let ollamaPick = null;
let ollamaMore = false;
let ollamaTest = null;
// (when the status came, and the ask on its way)
let ollamaStateAt = 0;
let ollamaFetching = null;
// The progress lines on the page, by job, updated in place while it runs
const ollamaProgressViews = {};

const ollamaRunning = () => !!(ollamaJob && !["done", "stopped", "error"].includes(ollamaJob.stage));

function ollamaGb(gb) {
  return `${gb >= 100 ? Math.round(gb) : Math.round(gb * 10) / 10} GB`;
}

function paintOllamaProgress() {
  if (!ollamaJob) return;
  const view = ollamaProgressViews[ollamaJob.job];
  if (!view || !view.box.isConnected) return;
  view.box.hidden = !ollamaRunning();
  const { done = 0, total = 0, stage, label } = ollamaJob;
  const counting = total > 0;
  view.box.classList.toggle("waiting", !counting);
  view.fill.style.width = counting ? `${Math.min(100, (done / total) * 100)}%` : "";
  const gb = (bytes) => ollamaGb(bytes / 1024 ** 3);
  view.label.textContent = counting && label.startsWith("Downloading") ? `${label}: ${gb(done)} of ${gb(total)}` : `${label}...`;
  // (only a download can be stopped)
  view.stop.hidden = !(stage === "download" || (stage === "pull" && label.startsWith("Downloading")));
}

listen("ollama-setup", (event) => {
  ollamaJob = event.payload;
  paintOllamaProgress();
  // (a job the page isn't showing yet: drawn with its bar)
  if (ollamaRunning() && !ollamaProgressViews[ollamaJob.job]?.box.isConnected) redrawSettingsInPlace();
  if (["done", "stopped", "error"].includes(ollamaJob.stage)) {
    const pulled = ollamaJob.job === "pull" && ollamaJob.stage === "done";
    if (ollamaJob.stage === "done") ollamaJob = null;
    refreshOllama().then(() => {
      // A new model: straight on to the test
      if (pulled && ollamaState?.modelReady) runOllamaTest(ollamaState.model);
    });
  }
});

// Draws the Settings page again where it's scrolled to (when it's open);
// calls while it's drawing make one more draw after it
let settingsDrawing = null;
let settingsDrawAgain = false;
async function redrawSettingsInPlace() {
  if (wrapped.hidden || wrappedStack.at(-1)?.page !== "settings") return;
  if (settingsDrawing) {
    settingsDrawAgain = true;
    return settingsDrawing;
  }
  settingsDrawing = (async () => {
    do {
      settingsDrawAgain = false;
      const top = wrappedBody.scrollTop;
      await WRAPPED_PAGES.settings();
      wrappedBody.scrollTop = top;
    } while (settingsDrawAgain);
  })();
  try {
    await settingsDrawing;
  } finally {
    settingsDrawing = null;
  }
}

// Asks ollama.rs where the setup stands; true when that changed. `fresh`
// asks Windows again about the graphics card and drives, and waits for it
function fetchOllamaState(fresh = false) {
  if (ollamaFetching && !fresh) return ollamaFetching;
  const ask = invoke("ollama_status", { fresh })
    .then((st) => {
      const changed = JSON.stringify(st) !== JSON.stringify(ollamaState);
      ollamaState = st;
      ollamaStateAt = Date.now();
      paintPieceDive();
      try {
        localStorage.setItem("ollamaState", JSON.stringify({ ...st, job: null }));
      } catch {
        // (kept for this run only)
      }
      // (a job that was running before the page loaded)
      if (st.job && !ollamaRunning()) ollamaJob = st.job;
      return changed;
    })
    .catch((err) => {
      console.warn("ollama status", err);
      return false;
    })
    .finally(() => {
      if (ollamaFetching === ask) ollamaFetching = null;
    });
  ollamaFetching = ask;
  return ask;
}

async function refreshOllama() {
  await fetchOllamaState(true);
  await redrawSettingsInPlace();
}

// Windows' answer about the graphics card and drives came in
listen("ollama-info", async () => {
  if (await fetchOllamaState()) redrawSettingsInPlace();
});

async function runOllamaTest(model) {
  ollamaTest = { model, running: true, ok: false, text: "" };
  await redrawSettingsInPlace();
  try {
    ollamaTest = { model, ok: true, text: await invoke("wrapped_llm_test") };
  } catch (err) {
    ollamaTest = { model, ok: false, text: String(err) };
  }
  await redrawSettingsInPlace();
}

// A job that couldn't start (another one is running) says so; one that
// started reports through its events
function ollamaNotStarted(err) {
  if (String(err).startsWith("Something is already")) {
    ollamaJob = null;
    wrappedToast(String(err));
    redrawSettingsInPlace();
  }
}

async function wrappedFacts(kind, card, ticket) {
  let facts;
  try {
    facts = await invoke(kind === "artist" ? "library_artist_facts" : "library_album_facts", { handle: card.handle });
  } catch (err) {
    console.warn("facts", err);
    return;
  }
  if (ticket !== wrappedTicket) return;
  const genres = wrappedBody.querySelector(".wrapped-genres.roon");
  if (genres) wrappedGenreChips(genres, facts.genres);
  if (kind === "artist") {
    // (The birthplace names the country already, in its own way)
    const where = [facts.from || facts.country].filter(Boolean);
    const line = [where.length ? where.join(", ") : null, facts.activeSince ? `since ${facts.activeSince}` : null]
      .filter(Boolean)
      .join(" · ");
    const el = wrappedBody.querySelector(".wrapped-facts");
    if (el) el.textContent = line;
  }
}

// Track rows like Roon's: play, the Wrapped radio mark, number and title,
// heart, length
function trackRows(tracks, album = {}) {
  const multi = tracks.some((t) => (t.mediaNumber || 1) > 1);
  const rows = [];
  let media = null;
  for (const track of tracks) {
    if (multi && track.mediaNumber !== media) {
      media = track.mediaNumber;
      rows.push(wrappedSection(`Disc ${media}`));
    }
    const row = document.createElement("div");
    row.className = `wrapped-track${track.playing ? " playing" : ""}`;
    row.append(
      wrappedIconButton(WRAPPED_PLAY_ICON, "Play now", () => {
        if (zone) send("library_play_track", { zoneId: zone.zoneId, handle: track.handle });
      }, "wrapped-track-play"),
      wrappedDive(
        { kind: "song", handle: track.handle, title: track.title, artist: (album.artist || "").split(/ \/ |, /)[0], album: album.title || "", albumHandle: album.handle, imageKey: album.imageKey },
        () => wrappedRadio("song", { tracks: [track.handle] }),
      ),
    );
    const number = document.createElement("span");
    number.className = "wrapped-track-number";
    number.textContent = track.trackNumber ?? "";
    const title = document.createElement("span");
    title.className = "wrapped-track-title link";
    title.textContent = track.title;
    // (a click on the song: Play Now, Add Next, Queue, Start Radio, Deep Dive)
    title.addEventListener("click", (event) => {
      event.stopPropagation();
      wrappedPlayMenu(title, {
        what: "song",
        handle: track.handle,
        seed: { kind: "song", handle: track.handle, title: track.title, artist: (album.artist || "").split(/ \/ |, /)[0], album: album.title || "", albumHandle: album.handle, imageKey: album.imageKey },
        radio: { tracks: [track.handle] },
      });
    });
    row.append(number, title);
    wholeRowOpens(row, title);
    row.append(
      wrappedHeart({ inLibrary: track.inLibrary, favorite: track.favorite }, async (favorite) => {
        const next = track.inLibrary
          ? await invoke("library_track_heart", { handle: track.handle, favorite })
          : await invoke("library_track_add", { handle: track.handle });
        return { inLibrary: next.inLibrary, favorite: next.favorite };
      }),
    );
    const length = document.createElement("span");
    length.className = "wrapped-track-length";
    length.textContent = track.lengthSeconds ? formatTime(track.lengthSeconds) : "";
    row.append(length);
    rows.push(row);
  }
  return rows;
}

function searchResults(hits, query) {
  const out = [];
  if (hits.artists.length) {
    out.push(wrappedSection("Artists"));
    out.push(
      wrappedGrid(
        hits.artists.slice(0, 12).map((a) => ({ handle: a.handle, performerId: a.performerId, title: a.name, sub: a.albumCount ? `${a.albumCount} albums` : "", imageKey: a.imageKey, round: true })),
        (item) => {
          if (item.performerId != null) rememberSearch({ kind: "artist", performerId: item.performerId, name: item.title, imageKey: item.imageKey });
          else if (query) rememberSearch({ kind: "query", text: query });
          openWrappedPage({ page: "artist", handle: item.handle });
        },
      ),
    );
  }
  if (hits.albums.length) {
    out.push(wrappedSection("Albums"));
    out.push(
      wrappedGrid(
        hits.albums.slice(0, 18).map((a) => ({ handle: a.handle, albumId: a.albumId, title: a.title, artist: a.artist, sub: [a.artist, a.year].filter(Boolean).join(" · "), imageKey: a.imageKey })),
        (item) => {
          if (item.albumId != null) rememberSearch({ kind: "album", albumId: item.albumId, name: item.title, artist: item.artist, imageKey: item.imageKey });
          else if (query) rememberSearch({ kind: "query", text: query });
          openWrappedPage({ page: "album", handle: item.handle });
        },
      ),
    );
  }
  if (hits.tracks.length) {
    out.push(wrappedSection("Tracks"));
    const list = document.createElement("div");
    list.className = "wrapped-tracks";
    for (const t of hits.tracks.slice(0, 20)) {
      const row = document.createElement("div");
      row.className = "wrapped-track";
      row.append(
        wrappedIconButton(WRAPPED_PLAY_ICON, "Play now", () => {
          if (zone) send("library_play_track", { zoneId: zone.zoneId, handle: t.handle });
        }, "wrapped-track-play"),
        wrappedDive(
          { kind: "song", handle: t.handle, title: t.title, artist: (t.artist || "").split(/ \/ |, /)[0], album: t.album || "", albumHandle: t.albumHandle, imageKey: t.imageKey },
          () => wrappedRadio("song", { tracks: [t.handle] }),
        ),
      );
      const art = wrappedArt(t.imageKey, 88, t.album);
      art.classList.add("small");
      const text = document.createElement("span");
      text.className = "wrapped-track-text";
      const title = document.createElement("span");
      title.className = "wrapped-track-title";
      title.textContent = t.title;
      const sub = document.createElement("span");
      sub.className = "wrapped-track-sub";
      sub.textContent = [t.artist, t.album].filter(Boolean).join(" · ");
      text.append(title, sub);
      if (t.albumHandle) {
        text.classList.add("link");
        text.addEventListener("click", () => openWrappedPage({ page: "album", handle: t.albumHandle }));
      }
      row.append(art, text);
      row.append(
        wrappedHeart({ inLibrary: t.inLibrary, favorite: t.favorite }, async (favorite) => {
          const next = t.inLibrary
            ? await invoke("library_track_heart", { handle: t.handle, favorite })
            : await invoke("library_track_add", { handle: t.handle });
          return { inLibrary: next.inLibrary, favorite: next.favorite };
        }),
      );
      const length = document.createElement("span");
      length.className = "wrapped-track-length";
      length.textContent = t.lengthSeconds ? formatTime(t.lengthSeconds) : "";
      row.append(length);
      list.append(row);
    }
    out.push(list);
  }
  if (!out.length) out.push(wrappedNote("Nothing found."));
  return out;
}

$("#wrapped-close").addEventListener("click", () => setWrappedOpen(false));
wrappedBack.addEventListener("click", wrappedBackPage);

// ----- The bar: the pages, and the search with what was searched lately -----

const wrappedNav = $("#wrapped-nav");
const wrappedBarInput = $("#wrapped-bar-input");
const wrappedRecent = $("#wrapped-recent");

// The pages along the bar (the menu has them too, with Settings and Close)
function fillWrappedNav() {
  const items = [{ page: "home", label: "Home", icon: "home" }];
  if (wrappedSettings?.discovery !== false) items.push({ page: "discover", label: "Discover", icon: "discover" });
  items.push(tier2Ready() ? { page: "deepdive", label: "Deep Dive", icon: "deepdive" } : { page: "radio", label: "Radio", icon: "radio" });
  items.push({ page: "playlists", label: "Playlists", icon: "playlists" }, { page: "history", label: "History", icon: "history" }, { page: "settings", label: "Settings", icon: "settings" });
  const current = wrappedStack.at(-1)?.page;
  wrappedNav.replaceChildren();
  for (const item of items) {
    const link = document.createElement("button");
    link.className = `wrapped-nav-link${item.page === current ? " selected" : ""}`;
    link.insertAdjacentHTML("beforeend", WRAPPED_ICONS[item.icon]);
    const label = document.createElement("span");
    label.textContent = item.label;
    link.append(label);
    link.title = item.label;
    link.addEventListener("click", () => openWrappedPage(item));
    wrappedNav.append(link);
  }
}

// Searching from the bar opens the Search page with what's typed (and
// keeps it current while you type); Enter searches at once
let wrappedBarTimer = null;
function wrappedBarSearch(now) {
  clearTimeout(wrappedBarTimer);
  const go = () => {
    const query = wrappedBarInput.value.trim();
    if (!query) return;
    const top = wrappedStack.at(-1);
    if (top?.page === "search" && !wrapped.hidden) {
      if (top.query === query) return;
      top.query = query;
      renderWrapped();
    } else {
      openWrappedPage({ page: "search", query });
    }
  };
  if (now) go();
  else wrappedBarTimer = setTimeout(go, 400);
}
wrappedBarInput.addEventListener("input", () => {
  if (wrappedBarInput.value.trim()) hideWrappedRecent();
  else showWrappedRecent();
  wrappedBarSearch(false);
});
wrappedBarInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    event.preventDefault();
    const query = wrappedBarInput.value.trim();
    if (query) rememberSearch({ kind: "query", text: query });
    wrappedBarSearch(true);
    hideWrappedRecent();
  } else if (event.key === "Escape") {
    hideWrappedRecent();
    wrappedBarInput.blur();
  }
});
wrappedBarInput.addEventListener("focus", () => {
  if (!wrappedBarInput.value.trim()) showWrappedRecent();
});
document.addEventListener("pointerdown", (event) => {
  if (wrappedRecent.hidden) return;
  if (wrappedRecent.contains(event.target) || wrappedBarInput.contains(event.target)) return;
  hideWrappedRecent();
});

// What was searched for and opened from a search lately (newest first,
// 12 at most), kept on this PC
const RECENT_SEARCHES_KEY = "wrapped-recent-searches";
const RECENT_SEARCHES_MAX = 12;
function recentSearches() {
  try {
    const list = JSON.parse(localStorage.getItem(RECENT_SEARCHES_KEY) || "[]");
    return Array.isArray(list) ? list : [];
  } catch {
    return [];
  }
}
function saveRecentSearches(list) {
  try {
    localStorage.setItem(RECENT_SEARCHES_KEY, JSON.stringify(list.slice(0, RECENT_SEARCHES_MAX)));
  } catch {}
}
const recentKey = (r) => (r.kind === "query" ? `q:${r.text.toLowerCase()}` : r.kind === "artist" ? `a:${r.performerId}` : `l:${r.albumId}`);
function rememberSearch(entry) {
  const key = recentKey(entry);
  saveRecentSearches([entry, ...recentSearches().filter((r) => recentKey(r) !== key)]);
}
function forgetSearch(entry) {
  const key = recentKey(entry);
  saveRecentSearches(recentSearches().filter((r) => recentKey(r) !== key));
}

function hideWrappedRecent() {
  wrappedRecent.hidden = true;
}

function showWrappedRecent() {
  const list = recentSearches();
  wrappedRecent.replaceChildren();
  if (!list.length) return hideWrappedRecent();
  const head = document.createElement("div");
  head.className = "wrapped-recent-head";
  head.textContent = "Recent";
  wrappedRecent.append(head);
  for (const r of list) {
    const row = document.createElement("div");
    row.className = "wrapped-recent-row";
    row.tabIndex = 0;
    const text = document.createElement("div");
    text.className = "wrapped-recent-text";
    const name = document.createElement("span");
    name.className = "wrapped-recent-name";
    if (r.kind === "query") {
      const glass = document.createElement("span");
      glass.className = "wrapped-recent-glass";
      glass.innerHTML = WRAPPED_ICONS.search;
      row.append(glass);
      name.textContent = r.text;
      text.append(name);
    } else {
      row.append(wrappedArt(r.imageKey, 72, r.name, r.kind === "artist"));
      const kind = document.createElement("span");
      kind.className = "wrapped-recent-kind";
      kind.textContent = r.kind === "artist" ? "Artist" : r.artist ? `Album · ${r.artist}` : "Album";
      name.textContent = r.name;
      text.append(kind, name);
    }
    row.append(text);
    const open = () => {
      hideWrappedRecent();
      rememberSearch(r);
      if (r.kind === "query") {
        wrappedBarInput.value = r.text;
        wrappedBarSearch(true);
      } else if (r.kind === "artist") {
        openWrappedPage({ page: "artist", performerId: r.performerId });
      } else {
        openWrappedPage({ page: "album", albumId: r.albumId });
      }
    };
    row.addEventListener("click", open);
    row.addEventListener("keydown", (event) => {
      if (event.key === "Enter") open();
    });
    const x = document.createElement("button");
    x.className = "discover-ask-x";
    x.innerHTML = CLOSE_ICON;
    x.title = "Forget this";
    x.addEventListener("click", (event) => {
      event.stopPropagation();
      forgetSearch(r);
      showWrappedRecent();
      wrappedBarInput.focus();
    });
    row.append(x);
    wrappedRecent.append(row);
  }
  wrappedRecent.hidden = false;
}

// Now Playing's Wrapped piece: each button closes Now Playing and opens
// the Wrapped page (the playing track's artist comes from the library link)
// The third button: Deep Dive with tier 2, Roon Radio from the song without
// it (repainted when the settings or the local model change, and on hover)
function paintPieceDive() {
  const btn = document.querySelector('.wrapped-piece-btn[data-action="dive"]');
  if (!btn) return;
  const dive = tier2Ready();
  if (btn.dataset.mode === (dive ? "dive" : "radio")) return;
  btn.dataset.mode = dive ? "dive" : "radio";
  btn.innerHTML = dive ? diveIcon() : RADIO_ICON;
  btn.dataset.tip = dive ? "Deep Dive from this song" : "Start radio from this song";
}
paintPieceDive();
document.querySelector('.wrapped-piece-btn[data-action="dive"]')?.addEventListener("pointerenter", paintPieceDive);
for (const btn of document.querySelectorAll(".wrapped-piece-btn")) {
  btn.addEventListener("click", async (event) => {
    event.stopPropagation();
    if (editing) return;
    const action = btn.dataset.action;
    const track = zone ? libraryTracks[zone.zoneId] : null;
    if (action === "open") {
      await closeStage();
      if (stage.hidden) openWrappedPage(wrappedStack.at(-1) ?? { page: "home" });
      return;
    }
    if (action === "discover") {
      await closeStage();
      if (stage.hidden) openWrappedPage({ page: "discover" });
      return;
    }
    if (!libraryReady || !track) {
      wrappedToastStage("Wrapped needs the Hearts & Library link for this.");
      return;
    }
    if (action === "artist") {
      let card;
      try {
        card = await invoke("library_playing_album", { zoneId: zone.zoneId });
      } catch (err) {
        return wrappedToastStage(String(err));
      }
      if (!card.performerHandle) return wrappedToastStage("Roon didn't say who the artist is.");
      await closeStage();
      if (stage.hidden) openWrappedPage({ page: "artist", handle: card.performerHandle });
    } else if (action === "album") {
      let card;
      try {
        card = await invoke("library_playing_album", { zoneId: zone.zoneId });
      } catch (err) {
        return wrappedToastStage(String(err));
      }
      await closeStage();
      if (stage.hidden) openWrappedPage({ page: "album", handle: card.handle });
    } else if (action === "dive") {
      // The playing song into Deep Dive's picks, as its button on an album
      // page does
      const id = track.trackId ?? track.libraryTrackId;
      if (id == null) return wrappedToastStage("Roon gave no id for this song.");
      // Without tier 2: Roon Radio from the song, Now Playing stays
      if (!tier2Ready()) {
        paintPieceDive();
        try {
          const handle = await invoke("library_track_handle", { trackId: Number(id) });
          await invoke("library_play_radio", { zoneId: zone.zoneId, seeds: { tracks: [handle] } });
          wrappedToastStage(`Radio from "${track.title}" on ${zone.name}`);
        } catch (err) {
          wrappedToastStage(typeof err === "string" ? err : "Roon didn't start the radio.");
        }
        return;
      }
      let handle;
      let card = null;
      try {
        [handle, card] = await Promise.all([
          invoke("library_track_handle", { trackId: Number(id) }),
          invoke("library_playing_album", { zoneId: zone.zoneId }).catch(() => null),
        ]);
      } catch (err) {
        return wrappedToastStage(String(err));
      }
      const artist = (card?.artist || track.album?.artist || "").split(/ \/ |, /)[0];
      await closeStage();
      if (stage.hidden) divePickAdd({ kind: "song", handle, title: track.title, artist, album: card?.title || "", albumHandle: card?.handle, imageKey: card?.imageKey });
    }
  });
}

// A line at the bottom of Now Playing for a moment
let stageToastTimer = null;
function wrappedToastStage(text) {
  let toast = stage.querySelector(".wrapped-toast");
  if (!toast) {
    toast = document.createElement("div");
    toast.className = "wrapped-toast";
    stage.append(toast);
  }
  toast.textContent = text;
  toast.classList.add("show");
  clearTimeout(stageToastTimer);
  stageToastTimer = setTimeout(() => toast.classList.remove("show"), 3000);
}

// Which page Roon is on (roonnav.rs reads Roon's log): the menu's "This
// album" / "This artist" and the home page follow it
function setRoonNav(nav) {
  // Every navigation Roon logged counts, also to the page it's already on
  const moved = roonNav.navCount !== nav.navCount && roonNav.navCount !== 0;
  roonNav = nav;
  // Roon went somewhere (the sidebar, a link): the page gets out of the
  // way like any Roon page would; it remembers where it was for next time
  // (not while a Deep Dive is at work: it stays in front)
  if (moved && !wrapped.hidden && !diveHolding()) setWrappedOpen(false);
  layoutWrapped();
  if (!wrappedSide.hidden) fillWrappedSide();
  if (!wrapped.hidden && wrappedStack.at(-1)?.page === "home") renderWrapped();
}
invoke("wrapper_nav").then(setRoonNav).catch(() => {});
listen("roon-nav", (event) => setRoonNav(event.payload));
// A click that reached Roon through the hole (its sidebar, beside the
// Wrapped page): the page gets out of the way, whatever Roon does with it
listen("roon-click", () => {
  if (!wrapped.hidden) setWrappedOpen(false);
});

// Roon's own full screen mode breaks the page's layout, so wrapper.rs
// undoes it the moment Roon enters it (another F11); the page says so,
// wherever it is showing
listen("roon-fullscreen", () => {
  const text = "Roon's full screen is off while the Wrapper holds it. Maximize the Wrapper instead.";
  if (!wrapped.hidden) wrappedToast(text);
  else if (!stage.hidden) wrappedToastStage(text);
  else wrapperToast(text);
});

let wrapperToastTimer;
function wrapperToast(text) {
  let toast = document.querySelector("body > .wrapped-toast");
  if (!toast) {
    toast = document.createElement("div");
    toast.className = "wrapped-toast";
    document.body.append(toast);
  }
  toast.textContent = text;
  toast.classList.add("show");
  clearTimeout(wrapperToastTimer);
  wrapperToastTimer = setTimeout(() => toast.classList.remove("show"), 3000);
}

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
// The title bar's sliders open Wrapped's own Settings page (Toasted's
// settings are a button on that page)
$("#open-settings").addEventListener("click", async () => {
  // (the player steps aside first, like the Wrapped piece's buttons)
  if (!stage.hidden) {
    await closeStage();
    if (!stage.hidden) return;
  }
  openWrappedPage({ page: "settings" });
});

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
// (the build number, from Rust: only a real rebuild changes it)
invoke("wrapper_build")
  .then((n) => {
    $("#titlebar-build").textContent = `Build: ${n}`;
  })
  .catch(() => {});
listen("library-status", (event) => setLibraryStatus(event.payload));
listen("library-track", (event) => {
  libraryTracks[event.payload.zoneId] = event.payload.track;
  renderLibrary();
});
invoke("roon_queue").then(renderQueue);
listen("roon-queue", (event) => renderQueue(event.payload));
