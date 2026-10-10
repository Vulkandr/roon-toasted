// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The Roon panel's page (prototype). Tells roonhost.rs where the hole is
// whenever the page's size changes, and shows what Rust says is going on.

const { invoke } = window.__TAURI__.core;
const appWindow = window.__TAURI__.window.getCurrentWindow();

const $ = (selector) => document.querySelector(selector);
const hole = $("#hole");
const status = $("#status");

// The hole's rectangle in CSS pixels, relative to the page (which fills the
// window's client area); Rust turns it into real pixels.
function reportHole() {
  const rect = hole.getBoundingClientRect();
  invoke("roonhost_hole", {
    x: rect.left,
    y: rect.top,
    width: rect.width,
    height: rect.height,
  }).catch(() => {});
}

new ResizeObserver(reportHole).observe(hole);
reportHole();

async function refreshStatus() {
  try {
    const s = await invoke("roonhost_status");
    status.textContent = s.note || (s.attached ? "Attached." : "Not attached.");
    status.style.color = s.attached && s.roonFound ? "" : "var(--warn)";
  } catch {
    status.textContent = "No status.";
  }
}
refreshStatus();
setInterval(refreshStatus, 1000);

$("#win-min").addEventListener("click", () => appWindow.minimize());
$("#win-max").addEventListener("click", () => appWindow.toggleMaximize());
$("#win-close").addEventListener("click", () => appWindow.close());
$("#detach").addEventListener("click", () => appWindow.close());
