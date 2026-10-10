// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The Wrapper's page. Tells wrapper.rs where the hole is whenever the
// page's size changes, and runs the player bar under it: what's playing in
// the Toaster's zone, transport, progress, zone picker and volume. Same
// Rust commands and events as the Toaster's Playing tab.

import { albumHue, albumPalette, paintColors } from "./album-colors.js";

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

const darkScheme = matchMedia("(prefers-color-scheme: dark)");

let zoneList = [];
let zone = null; // the Toaster's zone, from Rust
let artKey = null;
let draggingSeek = false;
let draggingVolume = false;
let colorsMode = "default"; // "default" or "album", from Settings

// ===== Helpers =============================================================

function send(command, args) {
  return invoke(command, args).catch((err) => console.error(command, err));
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
}

// ===== The hole ============================================================

// The hole's rectangle in CSS pixels, relative to the page (which fills the
// window's client area); Rust turns it into real pixels.
function reportHole() {
  const rect = hole.getBoundingClientRect();
  invoke("wrapper_hole", {
    x: rect.left,
    y: rect.top,
    width: rect.width,
    height: rect.height,
  }).catch(() => {});
}

new ResizeObserver(reportHole).observe(hole);
reportHole();

// Rust's one-line status: in the title bar when something's off, and in
// the middle while Roon isn't there yet (starting, or not found)
async function refreshStatus() {
  try {
    const s = await invoke("wrapper_status");
    const fine = s.open && s.roonFound;
    status.hidden = fine || !s.note;
    status.textContent = s.note;
    holeNote.textContent = s.roonFound ? "" : s.note || "Looking for Roon...";
  } catch {
    status.hidden = true;
  }
}
refreshStatus();
setInterval(refreshStatus, 1000);

// ===== Rendering ===========================================================

function render(payload) {
  zoneList = payload.zones;
  zone = payload.zones.find((z) => z.zoneId === payload.selectedZoneId) ?? null;
  const np = zone?.nowPlaying;

  zoneName.textContent = zone ? zone.name : "No zone";

  if (!zone) {
    showText("Roon: Toasted", "No zone", "");
  } else if (!np) {
    showText("Nothing playing", zone.name, "");
  } else {
    showText(np.title, np.artist, np.album);
  }
  showArt(np?.imageKey ?? null);

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

  renderOutput(zone?.outputs[0]);
}

function showText(t, a, b) {
  title.textContent = t;
  artist.textContent = a;
  album.textContent = b;
  $("#info").title = [t, a, b].filter(Boolean).join(" • ");
}

function showArt(imageKey) {
  if (imageKey === artKey) return;
  artKey = imageKey;
  art.hidden = !imageKey;
  artMissing.hidden = !!imageKey;
  if (imageKey) art.src = convertFileSrc(imageKey, "roonimg") + "?width=200&height=200";
  else art.removeAttribute("src");
  applyColors();
}

// Art that fails to load gets the logo stand-in
art.addEventListener("error", () => {
  art.hidden = true;
  artMissing.hidden = false;
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
  if (colorsMode === "album" && key) {
    try {
      hue = await albumHue(key);
    } catch (err) {
      console.warn("album colors", err);
    }
    if (key !== artKey) return; // the art changed meanwhile
  }
  paintColors(document.documentElement, hue ? albumPalette(hue, darkScheme.matches) : {});
}

function applySettings(settings) {
  const colorsChanged = settings.toasterColors !== colorsMode;
  colorsMode = settings.toasterColors;
  if (colorsChanged) applyColors();
}

// ===== Controls ============================================================

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

// Art or text: open the Toaster
$("#info").addEventListener("click", () => send("open_window", { window: "toaster" }));

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
}

function closeZoneMenu() {
  zoneMenu.hidden = true;
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
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") closeZoneMenu();
});

// ===== Window buttons ======================================================

$("#win-min").addEventListener("click", () => appWindow.minimize());
$("#win-max").addEventListener("click", () => appWindow.toggleMaximize());
$("#win-close").addEventListener("click", () => appWindow.close());
$("#detach").addEventListener("click", () => send("wrapper_put_back"));

// No right-click menu
document.addEventListener("contextmenu", (event) => event.preventDefault());

// ===== Start ===============================================================

// Load the current state once, then follow live updates from Rust
invoke("get_settings").then(applySettings);
listen("settings-changed", (event) => applySettings(event.payload));
invoke("roon_zones").then(render);
listen("roon-zones", (event) => render(event.payload));
listen("roon-seek", (event) => {
  if (zone && event.payload.zoneId === zone.zoneId) updatePosition(event.payload.seekPosition);
});
darkScheme.addEventListener("change", applyColors);
