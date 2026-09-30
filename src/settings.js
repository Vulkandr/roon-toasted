// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Tauri's JavaScript API (available because withGlobalTauri is on in tauri.conf.json)
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// The page elements, found by their ids in settings.html
const dot = document.querySelector("#status-dot");
const coreName = document.querySelector("#core-name");
const coreDetail = document.querySelector("#core-detail");
const coreList = document.querySelector("#core-list");
const coreCard = document.querySelector(".core-card");
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

  // No Core paired yet: the card glows slowly, to say "start here"
  // (the "Enable ..." line glows in step with it)
  coreCard.classList.toggle("attention", !status.pairedCoreId);
  coreDetail.classList.toggle("attention", !status.pairedCoreId);

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
    // With no Core paired and only one on the network, there is nothing to
    // choose: it just needs enabling in Roon, so Use (which restarts the app)
    // is greyed out
    const onlyChoice = !status.pairedCoreId && status.available.length === 1;
    button.disabled = inUse || onlyChoice;
    if (onlyChoice) button.title = "Enable \"Roon: Toasted\" in Roon > Settings > Extensions instead.";
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

// ===== Auto-Hide (hides on click away) ====================================

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

// ===== Toasts =============================================================
//
// The now-playing toast: on/off, which corner, how long it stays, and a Test
// button (which shows one even while the Toaster is open).

const toastsEnabled = document.querySelector("#toasts-enabled");
const toastsGroup = document.querySelector("#toasts-group");
const toastOptions = document.querySelector("#toast-options");
const corners = document.querySelectorAll(".corner");
const toastSeconds = document.querySelector("#toast-seconds");
const toastSecondsValue = document.querySelector("#toast-seconds-value");
const monitorRow = document.querySelector("#monitor-row");
const toastMonitor = document.querySelector("#toast-monitor");
let savedMonitor = null; // id of the chosen monitor; null = Primary Monitor

function showToasts(settings) {
  toastsEnabled.checked = settings.toastsEnabled;
  toastOptions.classList.toggle("disabled", !settings.toastsEnabled);
  for (const corner of corners) {
    corner.classList.toggle("selected", corner.dataset.corner === settings.toastPosition);
  }
  toastSeconds.value = settings.toastSeconds;
  toastSecondsValue.textContent = `${settings.toastSeconds} s`;
  monitorRow.classList.toggle("disabled", !settings.toastsEnabled);
  savedMonitor = settings.toastMonitor;
  selectMonitor();
}

// Picks the saved monitor in the list (reloading the list if it isn't there)
function selectMonitor() {
  const value = savedMonitor ?? "";
  if ([...toastMonitor.options].some((option) => option.value === value)) {
    toastMonitor.value = value;
  } else {
    loadMonitors();
  }
}

// Fills the Monitor list: Primary Monitor first, then each monitor left to
// right, as "DELL U2720Q, 2560 x 1440 (primary)"
async function loadMonitors() {
  const monitors = await invoke("list_monitors");
  const options = [new Option("Primary Monitor", "")];
  for (const monitor of monitors) {
    const primary = monitor.primary ? " (primary)" : "";
    const label = `${monitor.name}, ${monitor.width} x ${monitor.height}${primary}`;
    options.push(new Option(label, monitor.id));
  }
  // The chosen monitor is unplugged: say so (toasts use the primary meanwhile)
  if (savedMonitor && !monitors.some((monitor) => monitor.id === savedMonitor)) {
    options.push(new Option("Not connected (using the primary monitor)", savedMonitor));
  }
  toastMonitor.replaceChildren(...options);
  toastMonitor.value = savedMonitor ?? "";
}

async function saveToasts(changes) {
  try {
    await invoke("update_settings", { changes });
  } catch (err) {
    // Saving failed: show what's actually saved
    console.error(err);
    showToasts(await invoke("get_settings"));
  }
}

function setToastsEnabled(on) {
  toastsEnabled.checked = on;
  saveToasts({ toastsEnabled: on });
}

toastsEnabled.addEventListener("change", () => setToastsEnabled(toastsEnabled.checked));
toastsGroup.querySelector(".label-off").addEventListener("click", () => setToastsEnabled(false));
toastsGroup.querySelector(".label-on").addEventListener("click", () => setToastsEnabled(true));

for (const corner of corners) {
  corner.addEventListener("click", () => saveToasts({ toastPosition: corner.dataset.corner }));
}

// The number follows the slider while dragging; it's saved on release
toastSeconds.addEventListener("input", () => {
  toastSecondsValue.textContent = `${toastSeconds.value} s`;
});
toastSeconds.addEventListener("change", () => saveToasts({ toastSeconds: Number(toastSeconds.value) }));

toastMonitor.addEventListener("change", () => saveToasts({ toastMonitor: toastMonitor.value || null }));

// Monitors may have been plugged in or out while Settings was closed
window.addEventListener("focus", loadMonitors);

document.querySelector("#toast-test").addEventListener("click", () => invoke("toast_test"));

invoke("get_settings").then(showToasts);
listen("settings-changed", (event) => showToasts(event.payload));

// ===== Taskbar widget (experimental) ======================================
//
// On/off, side (left or right end of the taskbar), one or two lines of text,
// and Position / Width sliders that move the widget live while dragging.

const widgetEnabled = document.querySelector("#widget-enabled");
const widgetSide = document.querySelector("#widget-side");
const widgetLines = document.querySelector("#widget-lines");
const widgetPosition = document.querySelector("#widget-position");
const widgetWidth = document.querySelector("#widget-width");
const widgetPositionValue = document.querySelector("#widget-position-value");
const widgetWidthValue = document.querySelector("#widget-width-value");

function showWidget(settings) {
  widgetEnabled.checked = settings.widgetEnabled;
  widgetSide.checked = settings.widgetSide === "right";
  widgetLines.checked = settings.widgetLines === "two";
  document.querySelectorAll(".widget-option").forEach((option) => {
    option.classList.toggle("disabled", !settings.widgetEnabled);
  });
  // Don't fight the slider being dragged
  if (!dragging) {
    widgetPosition.value = settings.widgetPosition;
    widgetWidth.value = settings.widgetWidth;
  }
  widgetPositionValue.textContent = `${widgetPosition.value} px`;
  widgetWidthValue.textContent = `${widgetWidth.value} px`;
}

async function saveWidget(changes) {
  try {
    await invoke("update_settings", { changes });
  } catch (err) {
    console.error(err);
    showWidget(await invoke("get_settings"));
  }
}

// Switches (the side labels pick that side, like the others)
function widgetSwitch(input, group, change) {
  input.addEventListener("change", () => saveWidget(change(input.checked)));
  const labels = document.querySelector(group);
  labels.querySelector(".label-off").addEventListener("click", () => saveWidget(change(false)));
  labels.querySelector(".label-on").addEventListener("click", () => saveWidget(change(true)));
}
widgetSwitch(widgetEnabled, "#widget-group", (on) => ({ widgetEnabled: on }));
widgetSwitch(widgetSide, "#side-group", (right) => ({ widgetSide: right ? "right" : "left" }));
widgetSwitch(widgetLines, "#lines-group", (two) => ({ widgetLines: two ? "two" : "one" }));

// Sliders: save (and so move the widget) at most every 50 ms while dragging
let dragging = false;
let pending = null;
let saveTimer = null;

function saveSoon(changes) {
  pending = { ...pending, ...changes };
  if (saveTimer) return;
  saveTimer = setTimeout(() => {
    const changes = pending;
    pending = null;
    saveTimer = null;
    saveWidget(changes);
  }, 50);
}

for (const [slider, name, label] of [
  [widgetPosition, "widgetPosition", widgetPositionValue],
  [widgetWidth, "widgetWidth", widgetWidthValue],
]) {
  slider.addEventListener("input", () => {
    dragging = true;
    label.textContent = `${slider.value} px`;
    saveSoon({ [name]: Number(slider.value) });
  });
  slider.addEventListener("change", () => {
    dragging = false;
    saveSoon({ [name]: Number(slider.value) });
  });
}

invoke("get_settings").then(showWidget);
listen("settings-changed", (event) => showWidget(event.payload));

// ===== Hearts & Library ====================================================
//
// On/off and what "add to library" adds. The line underneath says whether the
// Core accepts the connection (this uses a part of Roon that isn't officially
// open to apps, so it can stop working after a Roon update).

const libraryEnabled = document.querySelector("#library-enabled");
const libraryMode = document.querySelector("#library-mode");
const libraryDetail = document.querySelector("#library-detail");

function showLibrary(settings) {
  libraryEnabled.checked = settings.libraryControls;
  libraryMode.checked = settings.libraryAddMode === "album";
  document.querySelectorAll(".library-option").forEach((option) => {
    option.classList.toggle("disabled", !settings.libraryControls);
  });
}

const LIBRARY_TEXT = {
  off: "Off",
  connecting: "Connecting to the Roon Core...",
  ready: "Ready: the Playing tab has the button.",
  unsupported: "Not available with this Roon version.",
  unavailable: "Waiting for the Roon Core...",
};

function showLibraryStatus(status) {
  let text = LIBRARY_TEXT[status.state] ?? status.state;
  if (status.reason && status.state !== "ready" && status.state !== "off") {
    text = `${LIBRARY_TEXT[status.state] ?? ""} ${status.reason}`.trim();
  }
  libraryDetail.textContent = text;
  libraryDetail.classList.toggle("ready", status.state === "ready");
  libraryDetail.classList.toggle("problem", status.state === "unsupported");
}

async function saveLibrary(changes) {
  try {
    await invoke("update_settings", { changes });
  } catch (err) {
    console.error(err);
    showLibrary(await invoke("get_settings"));
  }
}

function librarySwitch(input, group, change) {
  input.addEventListener("change", () => saveLibrary(change(input.checked)));
  const labels = document.querySelector(group);
  labels.querySelector(".label-off").addEventListener("click", () => saveLibrary(change(false)));
  labels.querySelector(".label-on").addEventListener("click", () => saveLibrary(change(true)));
}
librarySwitch(libraryEnabled, "#library-group", (on) => ({ libraryControls: on }));
librarySwitch(libraryMode, "#library-mode-group", (album) => ({ libraryAddMode: album ? "album" : "track" }));

invoke("get_settings").then(showLibrary);
listen("settings-changed", (event) => showLibrary(event.payload));
invoke("library_status").then(showLibraryStatus);
listen("library-status", (event) => showLibraryStatus(event.payload));

// ===== Reset on slider numbers ============================================
//
// The number next to a slider (e.g. "59 px"): hovering it shows a red
// "Reset" when the slider isn't at its default, and clicking puts the
// default back. Each number says which slider and setting it belongs to.

document.querySelectorAll(".reset-value").forEach((button) => {
  const slider = document.querySelector(button.dataset.slider);
  const atDefault = () => Number(slider.value) === Number(button.dataset.default);
  const showNumber = () => {
    button.classList.remove("reset");
    button.textContent = `${slider.value} ${button.dataset.unit}`;
  };

  button.addEventListener("mouseenter", () => {
    if (atDefault()) return;
    button.classList.add("reset");
    button.textContent = "Reset";
  });
  button.addEventListener("mouseleave", showNumber);
  button.addEventListener("click", async () => {
    if (atDefault()) return;
    slider.value = button.dataset.default;
    showNumber();
    try {
      await invoke("update_settings", {
        changes: { [button.dataset.setting]: Number(button.dataset.default) },
      });
    } catch (err) {
      console.error(err);
    }
  });
});

// ===== Start with Windows =================================================

const startWithWindows = document.querySelector("#start-with-windows");
const autostartGroup = document.querySelector("#autostart-group");

function showStartWithWindows(settings) {
  startWithWindows.checked = settings.startWithWindows;
}

async function setStartWithWindows(on) {
  startWithWindows.checked = on;
  try {
    await invoke("update_settings", { changes: { startWithWindows: on } });
  } catch (err) {
    console.error(err);
    showStartWithWindows(await invoke("get_settings"));
  }
}

startWithWindows.addEventListener("change", () => setStartWithWindows(startWithWindows.checked));
autostartGroup.querySelector(".label-off").addEventListener("click", () => setStartWithWindows(false));
autostartGroup.querySelector(".label-on").addEventListener("click", () => setStartWithWindows(true));

invoke("get_settings").then(showStartWithWindows);
listen("settings-changed", (event) => showStartWithWindows(event.payload));


// The footer link opens in the browser, not in this window
document.querySelector("#plugin-link").addEventListener("click", (event) => {
  event.preventDefault();
  invoke("plugin:opener|open_url", { url: event.currentTarget.href });
});
