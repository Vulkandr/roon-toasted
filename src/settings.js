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
  lastStatus = status;
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

// ===== Search, Manual and Diagnostics ======================================

const searchButton = document.querySelector("#core-search");
const manualButton = document.querySelector("#core-manual");
const manualPanel = document.querySelector("#manual-panel");
const manualInput = document.querySelector("#manual-address");
const manualPort = document.querySelector("#manual-port");
const manualSave = document.querySelector("#manual-save");
const manualClear = document.querySelector("#manual-clear");
const manualNote = document.querySelector("#manual-note");
const manualError = document.querySelector("#manual-error");
const diagButton = document.querySelector("#diag-toggle");
const diagPanel = document.querySelector("#diag-panel");
const diagText = document.querySelector("#diag-text");
const diagCopy = document.querySelector("#diag-copy");

let lastStatus = null;
let diagTimer = null;

// Opens or closes a slide-down panel and keeps its button's state in step
function setOpen(panel, button, open) {
  panel.classList.toggle("open", open);
  panel.inert = !open;
  button.setAttribute("aria-expanded", String(open));
}

// Search: asks the app to look for Cores again right now
searchButton.addEventListener("click", async () => {
  coreError.textContent = "";
  searchButton.disabled = true;
  searchButton.textContent = "Searching...";
  try {
    await invoke("roon_search_again");
  } catch (err) {
    coreError.textContent = err;
  }
  // Answers come back within a second or two; give it a moment before
  // allowing another search
  setTimeout(() => {
    searchButton.disabled = false;
    searchButton.textContent = "Search";
    if (diagPanel.classList.contains("open")) refreshDiagnostics();
  }, 3000);
});

// What the Manual panel says about the saved address
function manualText(diag) {
  if (!diag.manualCore) {
    return "Use this if searching can't find your Core. Leave the port as is unless you have a custom port.";
  }
  if (diag.manualAnswered) return "A Core answered the search at this address.";
  const where = `${diag.manualCore}:${diag.manualPort}`;
  switch (diag.direct) {
    case "connecting": return `No answer to the search. Connecting to ${where} directly...`;
    case "connected": return `Connected directly to ${where}.`;
    case "failed": return `Couldn't connect to ${where}. Check the address and port, and that Roon is running there. Retrying every 15 seconds.`;
    default: return "No answer from this address yet.";
  }
}

async function refreshManual() {
  try {
    const diag = await invoke("roon_diagnostics");
    manualNote.textContent = manualText(diag);
    return diag;
  } catch (err) {
    manualError.textContent = err;
    return null;
  }
}

// Manual: type in the Core's address
manualButton.addEventListener("click", async () => {
  const open = !manualPanel.classList.contains("open");
  setOpen(manualPanel, manualButton, open);
  if (!open) return;
  manualError.textContent = "";
  const diag = await refreshManual();
  if (diag) {
    manualInput.value = diag.manualCore ?? "";
    manualPort.value = String(diag.manualPort || 9330);
  }
  manualInput.focus();
});

// The direct connection's state arrives with every status update
listen("roon-status", () => {
  if (manualPanel.classList.contains("open")) refreshManual();
});

async function saveManual(address, port) {
  manualError.textContent = "";
  try {
    await invoke("roon_set_manual_core", { address, port });
  } catch (err) {
    manualError.textContent = err;
    return false;
  }
  return true;
}

manualSave.addEventListener("click", async () => {
  const address = manualInput.value.trim();
  if (!address) {
    manualError.textContent = "Enter the Core's IPv4 address, like 192.168.1.20.";
    return;
  }
  const portText = manualPort.value.trim() || "9330";
  const port = Number(portText);
  if (!/^\d+$/.test(portText) || port < 1 || port > 65535) {
    manualError.textContent = "The port must be a number between 1 and 65535 (9330 unless you changed it in Roon).";
    return;
  }
  if (!(await saveManual(address, port))) return;
  manualPort.value = String(port);
  manualNote.textContent = `Asking ${address}...`;
  // The search answer, if there is one, arrives within a second or two; if
  // not, the direct connection starts after 5 seconds and reports through
  // the status updates
  setTimeout(refreshManual, 3000);
});

manualClear.addEventListener("click", async () => {
  if (!(await saveManual(null, null))) return;
  manualInput.value = "";
  manualPort.value = "9330";
  manualNote.textContent = "Manual address removed.";
});

for (const field of [manualInput, manualPort]) {
  field.addEventListener("keydown", (event) => {
    if (event.key === "Enter") manualSave.click();
  });
}

// Diagnostics: what the search has been doing, as plain text (also what Copy gives)
function ago(secs) {
  if (secs === null || secs === undefined) return "never";
  if (secs < 60) return `${secs} s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)} min ago`;
  return `${Math.floor(secs / 3600)} h ago`;
}

function diagnosticsText(diag) {
  const status = lastStatus;
  const lines = [`Roon: Toasted ${diag.appVersion}`];
  if (status) {
    lines.push(`Connection: ${status.state}`);
    lines.push(`Cores found: ${status.available.length}`);
  }
  lines.push(`Searches sent: ${diag.searches} (last ${ago(diag.lastSearchSecsAgo)})`);
  lines.push(
    diag.lastReplyFrom
      ? `Last answer: from ${diag.lastReplyFrom}, ${ago(diag.lastReplySecsAgo)}`
      : "Last answer: none yet",
  );
  lines.push(
    diag.knownHosts.length
      ? `Asked directly: ${diag.knownHosts.join(", ")}`
      : "Asked directly: nothing saved",
  );
  if (diag.manualCore) {
    lines.push(
      `Manual address: ${diag.manualCore}:${diag.manualPort} `
        + `(search ${diag.manualAnswered ? "answered" : "not answered"}, direct connection ${diag.direct})`,
    );
  }
  lines.push("");
  lines.push(`Network adapters searched: ${diag.adapters.length}`);
  for (const adapter of diag.adapters) {
    lines.push(
      `  ${adapter.ip}  sent ${adapter.queriesSent}, answers ${adapter.replies}`,
    );
  }
  if (diag.otherReplies > 0) {
    lines.push(`  other (shared sockets)  answers ${diag.otherReplies}`);
  }
  return lines.join("\n");
}

async function refreshDiagnostics() {
  try {
    diagText.textContent = diagnosticsText(await invoke("roon_diagnostics"));
  } catch (err) {
    diagText.textContent = String(err);
  }
}

diagButton.addEventListener("click", () => {
  const open = !diagPanel.classList.contains("open");
  setOpen(diagPanel, diagButton, open);
  clearInterval(diagTimer);
  if (open) {
    refreshDiagnostics();
    // Stays live while the panel is open
    diagTimer = setInterval(refreshDiagnostics, 2000);
  }
});

diagCopy.addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText(diagText.textContent);
    diagCopy.textContent = "Copied";
  } catch {
    diagCopy.textContent = "Couldn't copy";
  }
  setTimeout(() => (diagCopy.textContent = "Copy"), 1500);
});

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

// ===== Updates =============================================================
//
// The "Check for Updates" switch, a Check now / Install button, the state
// line and, when a new version is waiting, its release notes and the download
// bar. Rust does the work (update.rs) and reports through `update-status`.

const updatesEnabled = document.querySelector("#updates-enabled");
const updateAction = document.querySelector("#update-action");
const updateDetail = document.querySelector("#update-detail");
const updateProgress = document.querySelector("#update-progress");
const updateNotes = document.querySelector("#update-notes");
let updateStatus = null;

function showUpdates(settings) {
  updatesEnabled.checked = settings.checkForUpdates;
}

function showUpdateStatus(status) {
  updateStatus = status;
  const current = `Roon: Toasted ${status.currentVersion}`;
  let text = current;
  let button = "Check now";
  let enabled = true;
  let tone = "";
  let progress = null; // null hides the bar, "indeterminate" animates it, a number fills it
  switch (status.state) {
    case "off":
      text = `${current}. Automatic checks are off.`;
      break;
    case "idle":
      text = `${current}. Not checked yet.`;
      break;
    case "checking":
      text = "Checking for updates...";
      enabled = false;
      break;
    case "upToDate":
      text = `${current} is the latest version.`;
      break;
    case "available":
      text = `${status.version} is available (you have ${status.currentVersion}).`;
      button = `Install ${status.version}`;
      tone = "available";
      break;
    case "downloading":
      text = `Downloading ${status.version}...`;
      button = status.percent == null ? "Downloading..." : `Downloading ${status.percent}%`;
      enabled = false;
      progress = status.percent == null ? "indeterminate" : status.percent;
      break;
    case "installing":
      text = `Installing ${status.version}. Roon: Toasted will restart.`;
      button = "Installing...";
      enabled = false;
      progress = "indeterminate";
      break;
    case "failed":
      text = status.message;
      tone = "problem";
      break;
  }
  updateDetail.textContent = text;
  updateDetail.classList.toggle("available", tone === "available");
  updateDetail.classList.toggle("problem", tone === "problem");
  updateAction.textContent = button;
  updateAction.disabled = !enabled;
  updateProgress.hidden = progress === null;
  updateProgress.classList.toggle("indeterminate", progress === "indeterminate");
  updateProgress.querySelector(".bar").style.width = typeof progress === "number" ? `${progress}%` : "";
  const notes = status.state === "available" ? status.notes : null;
  updateNotes.hidden = !notes;
  updateNotes.textContent = notes ?? "";
}

updateAction.addEventListener("click", async () => {
  if (!updateStatus) return;
  try {
    if (updateStatus.state === "available") {
      await invoke("update_install");
    } else {
      showUpdateStatus(await invoke("update_check"));
    }
  } catch (err) {
    console.error(err);
    showUpdateStatus(await invoke("update_status"));
  }
});

librarySwitch(updatesEnabled, "#updates-group", (on) => ({ checkForUpdates: on }));
invoke("get_settings").then(showUpdates);
listen("settings-changed", (event) => showUpdates(event.payload));
invoke("update_status").then(showUpdateStatus);
listen("update-status", (event) => showUpdateStatus(event.payload));

// ===== Version / update in the title bar ===================================
//
// The middle of the title bar shows the version (quietly) or, once Rust's
// update check found a newer release, "Update available" in the accent
// color; clicking it opens Settings, which has the Install button.

const titlebarUpdate = document.querySelector("#titlebar-update");

function showTitlebarUpdate(status) {
  const available = status.state === "available";
  const busy = status.state === "downloading" || status.state === "installing";
  titlebarUpdate.querySelector("span").textContent = available
    ? "Update available"
    : busy
      ? "Updating..."
      : `v${status.currentVersion}`;
  titlebarUpdate.classList.toggle("available", available);
  titlebarUpdate.classList.add("shown");
  titlebarUpdate.title = available ? `Roon: Toasted ${status.version} is ready to install` : `Roon: Toasted ${status.currentVersion}`;
}

titlebarUpdate.addEventListener("click", () => {
  if (titlebarUpdate.classList.contains("available")) updateAction.click();
});
invoke("update_status").then(showTitlebarUpdate);
listen("update-status", (event) => showTitlebarUpdate(event.payload));


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
