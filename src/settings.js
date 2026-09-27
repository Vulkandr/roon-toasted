// Tauri's JavaScript API (available because withGlobalTauri is on in tauri.conf.json)
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// The page elements, found by their ids in settings.html
const dot = document.querySelector("#status-dot");
const coreName = document.querySelector("#core-name");
const coreDetail = document.querySelector("#core-detail");
const coreList = document.querySelector("#core-list");
const coreError = document.querySelector("#core-error");

// Friendly wording for each connection state
const STATE_TEXT = {
  connected: "Connected",
  searching: "Searching...",
  reconnecting: "Reconnecting...",
};

// Updates the whole Roon Core section to match a status from Rust
function render(status) {
  // Dot color comes from the .connected / .searching / .reconnecting CSS classes
  dot.className = `dot ${status.state}`;

  // The paired Core's details, if it's been seen on the network
  const paired =
    status.connected ??
    status.available.find((core) => core.coreId === status.pairedCoreId);

  if (paired) {
    coreName.textContent = paired.name;
    coreDetail.textContent = `${STATE_TEXT[status.state]} · ${paired.host}`;
  } else if (status.pairedCoreId) {
    coreName.textContent = "Looking for your Roon Core...";
    coreDetail.textContent = "It isn't answering on the network right now.";
  } else {
    coreName.textContent = "No Roon Core paired yet";
    coreDetail.textContent =
      'Enable "Roon: Toasted" in Roon > Settings > Extensions.';
  }

  // Rebuild the Available Cores list from scratch
  coreList.replaceChildren();

  if (status.available.length === 0) {
    const empty = document.createElement("li");
    empty.className = "muted";
    empty.textContent = "No Cores found on the network yet.";
    coreList.append(empty);
  }

  for (const core of status.available) {
    const row = document.createElement("li");

    const info = document.createElement("div");
    const name = document.createElement("div");
    name.textContent = core.name;
    const host = document.createElement("div");
    host.className = "muted";
    host.textContent = core.host;
    info.append(name, host);

    const button = document.createElement("button");
    const inUse = core.coreId === status.pairedCoreId;
    button.textContent = inUse ? "In use" : status.pairedCoreId ? "Switch" : "Use";
    button.disabled = inUse;
    button.addEventListener("click", () => switchTo(core));

    row.append(info, button);
    coreList.append(row);
  }
}

// Asks Rust to switch Cores. On success the app restarts itself.
async function switchTo(core) {
  coreError.textContent = "";
  try {
    await invoke("switch_core", { coreId: core.coreId });
  } catch (err) {
    coreError.textContent = err;
  }
}

// Load the current status once, then stay live with updates from Rust
invoke("roon_status").then(render);
listen("roon-status", (event) => render(event.payload));
// ===== Toaster window size ================================================

const toasterWindow = document.querySelector("#toaster-window");

// Switch on = "remember" (Last Position), off = "default"
function showSettings(settings) {
  toasterWindow.checked = settings.toasterWindow === "remember";
}

async function setToasterWindow(remember) {
  toasterWindow.checked = remember;
  try {
    await invoke("update_settings", {
      changes: { toasterWindow: remember ? "remember" : "default" },
    });
  } catch (err) {
    // Saving failed: put the switch back to what's actually saved
    console.error(err);
    showSettings(await invoke("get_settings"));
  }
}

toasterWindow.addEventListener("change", () => setToasterWindow(toasterWindow.checked));

// Clicking either side's label picks that side
document.querySelector(".label-off").addEventListener("click", () => setToasterWindow(false));
document.querySelector(".label-on").addEventListener("click", () => setToasterWindow(true));

// Load the saved settings, then stay in sync with any changes
invoke("get_settings").then(showSettings);
listen("settings-changed", (event) => showSettings(event.payload));

// ===== Toaster zoom =======================================================

const zoomHotkeys = document.querySelector("#zoom-hotkeys");
const zoomGroup = document.querySelector("#zoom-group");
const zoomNow = document.querySelector("#zoom-now");
const zoomReset = document.querySelector("#zoom-reset");

function showZoom(settings) {
  zoomHotkeys.checked = settings.zoomHotkeys;
  zoomNow.textContent = `${Math.round(settings.zoom * 100)}%`;
  zoomReset.disabled = settings.zoom === 1;
}

async function saveZoomSetting(changes) {
  try {
    await invoke("update_settings", { changes });
  } catch (err) {
    // Saving failed: show what's actually saved
    console.error(err);
    showZoom(await invoke("get_settings"));
  }
}

function setZoomHotkeys(on) {
  zoomHotkeys.checked = on;
  saveZoomSetting({ zoomHotkeys: on });
}

zoomHotkeys.addEventListener("change", () => setZoomHotkeys(zoomHotkeys.checked));
zoomGroup.querySelector(".label-off").addEventListener("click", () => setZoomHotkeys(false));
zoomGroup.querySelector(".label-on").addEventListener("click", () => setZoomHotkeys(true));
zoomReset.addEventListener("click", () => saveZoomSetting({ zoom: 1 }));

invoke("get_settings").then(showZoom);
listen("settings-changed", (event) => showZoom(event.payload));

// ===== Hide on click away =================================================

const hideOnBlur = document.querySelector("#hide-on-blur");
const blurGroup = document.querySelector("#blur-group");

function showHideOnBlur(settings) {
  hideOnBlur.checked = settings.hideOnBlur;
}

async function setHideOnBlur(on) {
  hideOnBlur.checked = on;
  try {
    await invoke("update_settings", { changes: { hideOnBlur: on } });
  } catch (err) {
    console.error(err);
    showHideOnBlur(await invoke("get_settings"));
  }
}

hideOnBlur.addEventListener("change", () => setHideOnBlur(hideOnBlur.checked));
blurGroup.querySelector(".label-off").addEventListener("click", () => setHideOnBlur(false));
blurGroup.querySelector(".label-on").addEventListener("click", () => setHideOnBlur(true));

invoke("get_settings").then(showHideOnBlur);
listen("settings-changed", (event) => showHideOnBlur(event.payload));

// ===== Toaster colors =====================================================

const toasterColors = document.querySelector("#toaster-colors");
const colorsGroup = document.querySelector("#colors-group");

// Switch on = "album" (Album Art), off = "default"
function showColors(settings) {
  toasterColors.checked = settings.toasterColors === "album";
}

async function setColors(album) {
  toasterColors.checked = album;
  try {
    await invoke("update_settings", { changes: { toasterColors: album ? "album" : "default" } });
  } catch (err) {
    console.error(err);
    showColors(await invoke("get_settings"));
  }
}

toasterColors.addEventListener("change", () => setColors(toasterColors.checked));
colorsGroup.querySelector(".label-off").addEventListener("click", () => setColors(false));
colorsGroup.querySelector(".label-on").addEventListener("click", () => setColors(true));

invoke("get_settings").then(showColors);
listen("settings-changed", (event) => showColors(event.payload));

// ===== Window bar =========================================================

// × closes Settings (back to the Toaster if it's open), □ maximizes or restores
const appWindow = window.__TAURI__.window.getCurrentWindow();
const maximizeBtn = document.querySelector("#win-maximize");

document.querySelector("#win-close").addEventListener("click", () => appWindow.close());
maximizeBtn.addEventListener("click", () => appWindow.toggleMaximize());

// Shows the restore icon while maximized (checked whenever the size changes)
async function updateMaximizeButton() {
  const maximized = await appWindow.isMaximized();
  maximizeBtn.classList.toggle("maximized", maximized);
  maximizeBtn.title = maximized ? "Restore" : "Maximize";
}
window.addEventListener("resize", updateMaximizeButton);
updateMaximizeButton();

// ===== Hotkeys ============================================================
//
// Click a hotkey, then press the new combination: it needs Ctrl and/or Alt
// plus a key. While recording, all hotkeys are paused, so pressing the current
// one doesn't open or hide the Toaster. Escape or clicking elsewhere cancels.

const hotkeyButtons = {
  player: document.querySelector("#hotkey-player"),
  search: document.querySelector("#hotkey-search"),
};
const hotkeyError = document.querySelector("#hotkey-error");
const hotkeysEnabled = document.querySelector("#hotkeys-enabled");
const hotkeysGroup = document.querySelector("#hotkeys-group");
let recording = null; // "player" or "search" while waiting for keys

// "Ctrl+Alt+R" -> "Ctrl + Alt + R"
function prettyHotkey(text) {
  if (!text) return "Off";
  return text
    .split("+")
    .map((part) => ({ Super: "Win", ArrowUp: "Up", ArrowDown: "Down", ArrowLeft: "Left", ArrowRight: "Right" })[part] ?? part)
    .join(" + ");
}

function showHotkeys(status) {
  hotkeysEnabled.checked = status.enabled;
  document.querySelector("#hotkey-list").classList.toggle("disabled", !status.enabled);
  for (const which of ["player", "search"]) {
    const button = hotkeyButtons[which];
    if (recording === which) continue;
    button.textContent = prettyHotkey(status[which]);
    button.classList.toggle("off", !status[which]);
  }
  hotkeyError.textContent = [status.playerError, status.searchError].filter(Boolean).join(" ");
}

async function refreshHotkeys() {
  showHotkeys(await invoke("hotkey_status"));
}

async function startRecording(which) {
  if (recording) await stopRecording();
  recording = which;
  hotkeyError.textContent = "";
  hotkeyButtons[which].textContent = "Press keys…";
  hotkeyButtons[which].classList.add("recording");
  await invoke("pause_hotkeys", { paused: true });
}

async function stopRecording() {
  const which = recording;
  recording = null;
  if (which) hotkeyButtons[which].classList.remove("recording");
  await invoke("pause_hotkeys", { paused: false });
  await refreshHotkeys();
}

// The key's name as the app stores it: KeyR -> R, Digit5 -> 5, others as is
// (F5, Space, ArrowUp, Numpad1, ...)
function keyName(code) {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  return code;
}

async function saveHotkey(which, shortcut) {
  try {
    showHotkeys(await invoke("set_hotkey", { which, shortcut }));
  } catch (err) {
    await refreshHotkeys();
    hotkeyError.textContent = err;
  }
}

document.addEventListener("keydown", async (event) => {
  if (!recording) return;
  event.preventDefault();
  if (event.key === "Escape") {
    await stopRecording();
    return;
  }
  // Wait for the main key after the modifiers
  if (["Control", "Alt", "Shift", "Meta", "AltGraph"].includes(event.key)) return;
  if (!event.ctrlKey && !event.altKey) {
    hotkeyError.textContent = "Use Ctrl and/or Alt with the key, so normal typing isn't affected.";
    return;
  }
  const parts = [];
  if (event.ctrlKey) parts.push("Ctrl");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  if (event.metaKey) parts.push("Super");
  parts.push(keyName(event.code));

  const which = recording;
  recording = null;
  hotkeyButtons[which].classList.remove("recording");
  await saveHotkey(which, parts.join("+"));
});

for (const [which, button] of Object.entries(hotkeyButtons)) {
  button.addEventListener("click", () => startRecording(which));
  // Clicking elsewhere cancels
  button.addEventListener("blur", () => {
    if (recording === which) stopRecording();
  });
}

document.querySelectorAll(".hotkey-clear").forEach((button) => {
  button.addEventListener("click", () => saveHotkey(button.dataset.which, null));
});

// The Hotkeys switch: off releases both combinations (they're kept for later)
async function setHotkeysEnabled(on) {
  hotkeysEnabled.checked = on;
  try {
    await invoke("update_settings", { changes: { hotkeysEnabled: on } });
  } catch (err) {
    console.error(err);
  }
  await refreshHotkeys();
}

hotkeysEnabled.addEventListener("change", () => setHotkeysEnabled(hotkeysEnabled.checked));
hotkeysGroup.querySelector(".label-off").addEventListener("click", () => setHotkeysEnabled(false));
hotkeysGroup.querySelector(".label-on").addEventListener("click", () => setHotkeysEnabled(true));

refreshHotkeys();
listen("settings-changed", () => refreshHotkeys());
