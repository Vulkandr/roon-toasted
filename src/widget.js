// The taskbar widget's page (the window itself is placed by widget.rs).
// Shows what's playing in the Toaster's zone; the buttons control it, and
// clicking the art or text opens the Toaster.

import { albumHue, albumPalette, paintColors } from "./album-colors.js";

// Tauri's JavaScript API (available because withGlobalTauri is on)
const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (selector) => document.querySelector(selector);

const widget = $("#widget");
const art = $("#art");
const artMissing = $("#art-missing");
const title = $("#title");
const artist = $("#artist");
const previousBtn = $("#previous");
const playPauseBtn = $("#play-pause");
const nextBtn = $("#next");

const darkScheme = matchMedia("(prefers-color-scheme: dark)");

let zone = null; // the Toaster's zone, from Rust
let artKey = null;
let lines = "two"; // "one" or "two", from Settings
let colorsMode = "default"; // "default" or "album", from Settings

function send(command, args) {
  return invoke(command, args).catch((err) => console.error(command, err));
}

function render(payload) {
  zone = payload.zones.find((z) => z.zoneId === payload.selectedZoneId) ?? null;
  const np = zone?.nowPlaying;

  if (!zone) {
    showText("Roon: Toasted", "No zone");
  } else if (!np) {
    showText("Nothing playing", zone.name);
  } else {
    showText(np.title, np.artist);
  }
  showArt(np?.imageKey ?? null);

  previousBtn.disabled = !zone?.canPrevious;
  nextBtn.disabled = !zone?.canNext;
  playPauseBtn.disabled = !zone || (!zone.canPlay && !zone.canPause);
  playPauseBtn.classList.toggle("playing", zone?.state === "playing");
}

let fullTitle = "";
let fullArtist = "";

function showText(t, a) {
  fullTitle = t;
  fullArtist = a;
  layoutText();
}

// Two lines: title above artist. One line: "Title • Artist"
function layoutText() {
  widget.classList.toggle("one-line", lines === "one");
  widget.classList.toggle("two-lines", lines !== "one");
  title.textContent = lines === "one" && fullArtist ? `${fullTitle} • ${fullArtist}` : fullTitle;
  artist.textContent = fullArtist;
  $("#info").title = fullArtist ? `${fullTitle} • ${fullArtist}` : fullTitle;
}

function showArt(imageKey) {
  if (imageKey === artKey) return;
  artKey = imageKey;
  art.hidden = !imageKey;
  artMissing.hidden = !!imageKey;
  if (imageKey) art.src = convertFileSrc(imageKey, "roonimg") + "?width=96&height=96";
  else art.removeAttribute("src");
  applyColors();
}

// Art that fails to load gets the logo stand-in
art.addEventListener("error", () => {
  art.hidden = true;
  artMissing.hidden = false;
});

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
  lines = settings.widgetLines;
  layoutText();
  if (colorsChanged) applyColors();
}

// Buttons
previousBtn.addEventListener("click", () => {
  if (zone) send("roon_control", { zoneId: zone.zoneId, action: "previous" });
});
playPauseBtn.addEventListener("click", () => {
  if (zone) send("roon_control", { zoneId: zone.zoneId, action: "playpause" });
});
nextBtn.addEventListener("click", () => {
  if (zone) send("roon_control", { zoneId: zone.zoneId, action: "next" });
});

// Art or text: open the Toaster
$("#info").addEventListener("click", () => send("open_window", { window: "toaster" }));

// No right-click menu
document.addEventListener("contextmenu", (event) => event.preventDefault());

// Load the current state once, then follow live updates from Rust
invoke("get_settings").then(applySettings);
listen("settings-changed", (event) => applySettings(event.payload));
invoke("roon_zones").then(render);
listen("roon-zones", (event) => render(event.payload));
darkScheme.addEventListener("change", applyColors);
