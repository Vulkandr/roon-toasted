// The now-playing toast (the window itself is managed by toast.rs).
//
// Rust sends a track with "toast-show". The page fills itself in, waits a
// moment for the album art (and its colors), then asks Rust to put the window
// in its corner and show it ("toast_present"). A line along the bottom counts
// down the time; when it runs out the toast fades and hides ("toast_hide").

import { albumHue, albumPalette, paintColors } from "./album-colors.js";

// Tauri's JavaScript API (available because withGlobalTauri is on)
const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (selector) => document.querySelector(selector);

const toast = $("#toast");
const art = $("#art");
const artMissing = $("#art-missing");
const label = $("#label");
const title = $("#title");
const artist = $("#artist");
const album = $("#album");
const bar = $("#bar");

const darkScheme = matchMedia("(prefers-color-scheme: dark)");

// How long to wait for the art before showing the toast anyway
const ART_WAIT_MS = 800;

let current = 0; // goes up with every toast, so older, slower work gives up
let onScreen = false;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// Resolves once the image has loaded or failed
function artReady() {
  if (art.complete) return Promise.resolve();
  return new Promise((resolve) => {
    art.addEventListener("load", resolve, { once: true });
    art.addEventListener("error", resolve, { once: true });
  });
}

function showArt(imageKey) {
  art.hidden = !imageKey;
  artMissing.hidden = !!imageKey;
  if (imageKey) art.src = convertFileSrc(imageKey, "roonimg") + "?width=200&height=200";
  else art.removeAttribute("src");
}

// Art that fails to load gets the logo stand-in
art.addEventListener("error", () => {
  art.hidden = true;
  artMissing.hidden = false;
});

async function show(track) {
  const id = ++current;

  label.textContent = track.zoneName ? `Now Playing · ${track.zoneName}` : "Now Playing";
  title.textContent = track.title;
  artist.textContent = track.artist;
  album.textContent = track.album;
  showArt(track.imageKey);

  // Wait (briefly) for the art, and for its colors when those are on
  let palette = {};
  const waits = [];
  if (track.imageKey) {
    waits.push(artReady());
    if (track.colors === "album") {
      waits.push(
        albumHue(track.imageKey)
          .then((hue) => (palette = albumPalette(hue, darkScheme.matches)))
          .catch((err) => console.warn("album colors", err)),
      );
    }
  }
  await Promise.race([Promise.all(waits), sleep(ART_WAIT_MS)]);
  if (id !== current) return;
  paintColors(document.documentElement, palette);

  // Restart the countdown
  toast.classList.remove("counting");
  bar.style.animationDuration = `${track.seconds}s`;

  await invoke("toast_present");
  if (id !== current) return;
  onScreen = true;
  void toast.offsetWidth; // lets the countdown start over from full
  toast.classList.add("shown", "counting");
}

// Fades out, then hides the window
async function close() {
  if (!onScreen) return;
  const id = ++current;
  onScreen = false;
  toast.classList.remove("shown");
  await sleep(200);
  if (id !== current) return; // a new toast came in meanwhile
  toast.classList.remove("counting");
  await invoke("toast_hide");
}

listen("toast-show", (event) => show(event.payload));

// Time's up
bar.addEventListener("animationend", close);

// Click: open the Toaster (on the Playing tab) and close the toast
toast.addEventListener("click", () => {
  invoke("open_window", { window: "toaster" });
  close();
});

// x or right-click: just close it
$("#dismiss").addEventListener("click", (event) => {
  event.stopPropagation();
  close();
});
document.addEventListener("contextmenu", (event) => {
  event.preventDefault();
  close();
});
