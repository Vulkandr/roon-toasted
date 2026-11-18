// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Tauri's JavaScript API (available because withGlobalTauri is on in tauri.conf.json)
import { albumHue, albumPalette, customPalette, paintColors } from "./album-colors.js";
import { closeColorPopover, cssToHex, openColorPopover } from "./color-picker.js";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ===== Switches as pills ===================================================
//
// Every on/off switch shows as two pills, like Colors. The checkbox stays
// (hidden) and is still what each setting's code reads and sets: a pill sets
// it and fires its change event, and setting it from code moves the pills.

const CHECKED = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "checked");

function pillSwitch(group) {
  const input = group.querySelector('input[type="checkbox"]');
  if (!input) return;
  const pills = document.createElement("div");
  pills.className = "seg-pills";
  const pill = (label, value) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "seg-pill";
    button.textContent = label.textContent;
    button.addEventListener("click", () => {
      if (input.checked === value || input.disabled) return;
      input.checked = value;
      input.dispatchEvent(new Event("change", { bubbles: true }));
    });
    pills.append(button);
    return button;
  };
  const off = pill(group.querySelector(".label-off"), false);
  const on = pill(group.querySelector(".label-on"), true);
  // (the "on" side, in the accent when it's chosen, as the switch was)
  on.classList.add("on-side");
  // A choice between two things (Left or Right, Single Song or Whole
  // Album: anything whose first side isn't "Off") has no off side, so
  // whichever is chosen is in the accent
  if (off.textContent.trim().toLowerCase() !== "off") off.classList.add("on-side");
  const show = () => {
    off.classList.toggle("selected", !input.checked);
    on.classList.toggle("selected", input.checked);
  };
  Object.defineProperty(input, "checked", {
    configurable: true,
    get() {
      return CHECKED.get.call(this);
    },
    set(value) {
      CHECKED.set.call(this, value);
      show();
    },
  });
  input.addEventListener("change", show);
  group.classList.add("as-pills");
  group.append(pills);
  show();
}

document.querySelectorAll(".switch-group").forEach(pillSwitch);

// The page elements, found by their ids in settings.html
const dot = document.querySelector("#status-dot");
const coreName = document.querySelector("#core-name");
const coreDetail = document.querySelector("#core-detail");
const coreList = document.querySelector("#core-list");
// ===== Pages ===============================================================
//
// One page shows at a time, picked from the list on the left. The page
// that was open comes back the next time Settings opens (kept in this
// window's own storage: it's only a convenience). Another window can ask
// for a page with a "settings-page" event (its name: "general", "core",
// "keys", "toaster", "wrapper", "toasts" or "widget"; "library" is on the
// Roon Core page).

const pageLinks = [...document.querySelectorAll(".page-link")];
const pages = [...document.querySelectorAll(".page")];
const PAGE_KEY = "settings-page";
// The user picked a page themselves (so a Core that isn't paired yet no
// longer pulls them to its page)
let pagePicked = false;

function showPage(name, picked = false) {
  if (name === "library") name = "core";
  if (!pages.some((page) => page.dataset.page === name)) name = "general";
  if (picked) pagePicked = true;
  for (const page of pages) page.hidden = page.dataset.page !== name;
  for (const link of pageLinks) link.classList.toggle("current", link.dataset.page === name);
  document.querySelector(".page-scroll").scrollTop = 0;
  try {
    localStorage.setItem(PAGE_KEY, name);
  } catch {
    // (no storage: the page just isn't remembered)
  }
}

for (const link of pageLinks) {
  link.addEventListener("click", () => showPage(link.dataset.page, true));
}

{
  let last = null;
  try {
    last = localStorage.getItem(PAGE_KEY);
  } catch {
    // (no storage)
  }
  showPage(last || "general");
}
listen("settings-page", (event) => showPage(String(event.payload), true));

// A page's entry in the list glows while something on that page does
// (something new since an update, or the Roon Core card waiting for a Core)
function showPageGlows() {
  for (const link of pageLinks) {
    const page = pages.find((p) => p.dataset.page === link.dataset.page);
    link.classList.toggle("is-new", !!page?.querySelector(".is-new, .card.attention"));
  }
}

// ===== Profile =============================================================
//
// A Roon account can have several profiles: the library is shared, hearts,
// bans and listening history are kept per profile. Toasted uses one
// (library.rs), shown at the top of the list; with more than one, a click
// opens the others under it. Picking one changes where Toasted's hearts go.
// It doesn't change the profile Roon's own app is on, and the list says so.

const navProfile = document.querySelector("#nav-profile");
const navProfileButton = document.querySelector("#nav-profile-button");
const navProfileList = document.querySelector("#nav-profile-list");
let profileState = { profiles: [], current: null };

// A profile's picture in its round frame, or its initial without one
function paintProfilePic(frame, profile) {
  const initial = () => {
    frame.replaceChildren();
    frame.textContent = (profile?.name || "?").trim().charAt(0).toUpperCase();
  };
  if (!profile?.imageKey) return initial();
  const img = document.createElement("img");
  img.alt = "";
  img.addEventListener("error", initial);
  img.src = `${window.__TAURI__.core.convertFileSrc(profile.imageKey, "roonimg")}?width=80&height=80&src=broker&square=1&v=2`;
  frame.replaceChildren(img);
}

function showProfiles(state) {
  profileState = state?.profiles ? state : { profiles: [], current: null };
  const profile = profileState.profiles.find((p) => p.id === profileState.current);
  const several = profileState.profiles.length > 1;
  navProfile.hidden = !profile;
  navProfileList.hidden = true;
  navProfile.classList.toggle("pickable", several);
  if (!profile) return;
  paintProfilePic(document.querySelector("#nav-profile-pic"), profile);
  document.querySelector("#nav-profile-name").textContent = profile.name;
  document.querySelector("#nav-profile-note").textContent = "Roon profile";
  navProfileButton.title = several
    ? "The Roon profile Toasted uses: hearts, bans and history go to it. Click to pick another"
    : "The Roon profile Toasted uses: hearts, bans and history go to it";
  navProfileList.replaceChildren();
  for (const p of profileState.profiles) {
    const option = document.createElement("button");
    option.type = "button";
    option.className = `nav-profile-option${p.id === profileState.current ? " current" : ""}`;
    const pic = document.createElement("span");
    pic.className = "profile-pic";
    paintProfilePic(pic, p);
    const name = document.createElement("span");
    name.textContent = p.name;
    option.append(pic, name);
    option.addEventListener("click", async () => {
      navProfileList.hidden = true;
      if (p.id === profileState.current) return;
      try {
        showProfiles(await invoke("library_set_profile", { id: p.id }));
      } catch (err) {
        console.error(err);
      }
    });
    navProfileList.append(option);
  }
  const note = document.createElement("p");
  note.className = "nav-profile-list-note";
  note.textContent = "Changes the profile Toasted uses. Roon's own app keeps the profile it's on.";
  navProfileList.append(note);
}

navProfileButton.addEventListener("click", () => {
  if (profileState.profiles.length > 1) navProfileList.hidden = !navProfileList.hidden;
});

const readProfiles = () =>
  invoke("library_profiles")
    .then(showProfiles)
    .catch((err) => console.error(err));
readProfiles();
listen("library-profiles", (event) => showProfiles(event.payload));
// (Hearts & Library switched off, or its link lost: no profile to show)
listen("library-status", readProfiles);

const coreCard = document.querySelector(".core-card");
const navCoreDot = document.querySelector("#nav-core-dot");
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
  // (the same light on the Roon Core page's entry in the list, with the
  // state in words as its tooltip)
  navCoreDot.className = `dot nav-dot ${status.state}`;
  navCoreDot.title = STATE_TEXT[status.state] ?? "";

  // No Core paired yet: the card glows slowly, to say "start here"
  // (the "Enable ..." line glows in step with it)
  coreCard.classList.toggle("attention", !status.pairedCoreId);
  coreDetail.classList.toggle("attention", !status.pairedCoreId);
  // (and Settings opens on its page, unless a page was picked by hand)
  showPageGlows();
  if (!status.pairedCoreId && !pagePicked) showPage("core");

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
// ===== Toaster on/off =====================================================
//
// Off for people who only use the Wrapper (or just toasts and the widget):
// its own options (Open at Launch, Window Size, Zoom) dim and its Open Toaster hotkey goes
// away. Search stays while the Wrapper is on (it then opens Roon's own
// search). The General card's settings are for the whole app.

const toasterEnabled = document.querySelector("#toaster-enabled");

const toasterAtLaunch = document.querySelector("#toaster-at-launch");

function showToaster(settings) {
  toasterEnabled.checked = settings.toasterEnabled;
  toasterAtLaunch.checked = settings.toasterAtLaunch;
  document.querySelectorAll(".toaster-option").forEach((option) => {
    option.classList.toggle("disabled", !settings.toasterEnabled);
  });
  document.querySelectorAll(".toaster-hotkey-row").forEach((row) => {
    row.hidden = !settings.toasterEnabled;
  });
  // No hotkeys left to show (the Toaster and the Wrapper both off)
  const none = !settings.toasterEnabled && !settings.wrapperEnabled;
  document.querySelector("#hotkeys-group").hidden = none;
  document.querySelector("#hotkeys-none").hidden = !none;
  document.querySelector("#hotkey-list").hidden = none;
  document.querySelector("#search-hotkey-row").hidden = none;
  showSearchOpens(settings);
}

// ===== Where Search goes ==================================================
//
// Roon's own search (in the Wrapper) or the Toaster's Search tab. A choice
// only while both are on; otherwise the switch shows whichever is, locked.

const searchOpens = document.querySelector("#search-opens");
const searchOpensGroup = document.querySelector("#search-opens-group");

function showSearchOpens(settings) {
  const both = settings.toasterEnabled && settings.wrapperEnabled;
  const inRoon = settings.wrapperEnabled && (!settings.toasterEnabled || settings.searchOpens === "roon");
  // Checked = Toaster (the right-hand label)
  searchOpens.checked = !inRoon;
  searchOpensGroup.classList.toggle("disabled", !both);
  searchOpensGroup.title = both
    ? "Where the Search hotkey and Stream Deck search open"
    : `Search opens ${inRoon ? "Roon" : "the Toaster"} while only it is on`;
}

async function setSearchOpens(toaster) {
  searchOpens.checked = toaster;
  try {
    await invoke("update_settings", { changes: { searchOpens: toaster ? "toaster" : "roon" } });
  } catch (err) {
    console.error(err);
    showToaster(await invoke("get_settings"));
  }
}

searchOpens.addEventListener("change", () => setSearchOpens(searchOpens.checked));
searchOpensGroup.querySelector(".label-off").addEventListener("click", () => setSearchOpens(false));
searchOpensGroup.querySelector(".label-on").addEventListener("click", () => setSearchOpens(true));

async function setToasterEnabled(on) {
  toasterEnabled.checked = on;
  try {
    await invoke("update_settings", { changes: { toasterEnabled: on } });
  } catch (err) {
    console.error(err);
    showToaster(await invoke("get_settings"));
  }
}

toasterEnabled.addEventListener("change", () => setToasterEnabled(toasterEnabled.checked));

async function setToasterAtLaunch(on) {
  toasterAtLaunch.checked = on;
  try {
    await invoke("update_settings", { changes: { toasterAtLaunch: on } });
  } catch (err) {
    console.error(err);
    showToaster(await invoke("get_settings"));
  }
}
toasterAtLaunch.addEventListener("change", () => setToasterAtLaunch(toasterAtLaunch.checked));
const toasterLaunchGroup = document.querySelector("#toaster-launch-group");
toasterLaunchGroup.querySelector(".label-off").addEventListener("click", () => setToasterAtLaunch(false));
toasterLaunchGroup.querySelector(".label-on").addEventListener("click", () => setToasterAtLaunch(true));
const toasterGroup = document.querySelector("#toaster-group");
toasterGroup.querySelector(".label-off").addEventListener("click", () => setToasterEnabled(false));
toasterGroup.querySelector(".label-on").addEventListener("click", () => setToasterEnabled(true));

invoke("get_settings").then(showToaster);
listen("settings-changed", (event) => showToaster(event.payload));

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
const windowGroup = document.querySelector("#window-group");
windowGroup.querySelector(".label-off").addEventListener("click", () => setToasterWindow(false));
windowGroup.querySelector(".label-on").addEventListener("click", () => setToasterWindow(true));

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

// ===== Artists (all of them, or just the main one) =======================
//
// Some tracks list dozens of supporting artists. Main shows only the first
// one Roon lists, everywhere (Toaster, Wrapper, toasts, widget, queue).

const mainArtistOnly = document.querySelector("#main-artist-only");
const artistsGroup = document.querySelector("#artists-group");

function showMainArtistOnly(settings) {
  mainArtistOnly.checked = settings.mainArtistOnly;
}

async function setMainArtistOnly(on) {
  mainArtistOnly.checked = on;
  try {
    await invoke("update_settings", { changes: { mainArtistOnly: on } });
  } catch (err) {
    console.error(err);
    showMainArtistOnly(await invoke("get_settings"));
  }
}

mainArtistOnly.addEventListener("change", () => setMainArtistOnly(mainArtistOnly.checked));
artistsGroup.querySelector(".label-off").addEventListener("click", () => setMainArtistOnly(false));
artistsGroup.querySelector(".label-on").addEventListener("click", () => setMainArtistOnly(true));

invoke("get_settings").then(showMainArtistOnly);
listen("settings-changed", (event) => showMainArtistOnly(event.payload));

// ===== Toaster colors =====================================================

const colorsPills = document.querySelector("#colors-pills");
const customPill = colorsPills.querySelector('[data-colors="custom"]');
const darkScheme = matchMedia("(prefers-color-scheme: dark)");
let colorsNow = "default";
let accentNow = "#7b5fc7";
let backgroundNow = null; // Custom's background (null: tinted from the accent)
// What the window itself is painted in: those three, unless the Roon
// profile in use has colors of its own (settings.rs works that out)
let shown = { colors: "default", accent: "#7b5fc7", background: null };

// Settings takes the same colors as the Toaster: Custom's, or with Album
// Art the colors of the album playing in the selected zone (the app's own
// while nothing is playing there)
let artKey = null; // the selected zone's album art
async function paintSettings() {
  const key = artKey;
  let hue = null;
  if (shown.colors === "album" && key) {
    try {
      hue = await albumHue(key);
    } catch (err) {
      console.warn("album colors", err);
    }
    // (the art or the setting changed meanwhile: that call wins)
    if (key !== artKey || shown.colors !== "album") return;
  }
  paintColors(
    document.documentElement,
    hue ? albumPalette(hue, darkScheme.matches) : shown.colors === "custom" ? customPalette(shown.accent, shown.background, darkScheme.matches) : {},
  );
}
darkScheme.addEventListener("change", paintSettings);

function showZoneArt(payload) {
  const zone = payload?.zones?.find((z) => z.zoneId === payload.selectedZoneId);
  const key = zone?.nowPlaying?.imageKey ?? null;
  if (key === artKey) return;
  artKey = key;
  paintSettings();
}
invoke("roon_zones").then(showZoneArt).catch((err) => console.error(err));
listen("roon-zones", (event) => showZoneArt(event.payload));

function showColors(settings) {
  // (the colors in use: the shared ones, or the own ones of a Roon profile
  // set to Custom in Wrapped's Settings. A change made here goes to
  // whichever those are; settings.rs sees to that)
  colorsNow = settings.shownColors ?? settings.toasterColors;
  accentNow = settings.shownAccent || settings.toasterAccent || accentNow;
  backgroundNow = (settings.shownColors ? settings.shownBackground : settings.toasterBackground) ?? null;
  for (const pill of colorsPills.children) pill.classList.toggle("selected", pill.dataset.colors === colorsNow);
  shown = { colors: colorsNow, accent: accentNow, background: backgroundNow };
  customPill.style.setProperty("--dot", accentNow);
  paintSettings();
}

async function saveColors(changes) {
  try {
    showColors(await invoke("update_settings", { changes }));
  } catch (err) {
    console.error(err);
    showColors(await invoke("get_settings"));
  }
}

// Custom's color picker, under the Custom pill: Accent or Background color.
// The window shows the colors while they're dragged, and they're saved when
// let go. The background follows the accent until it's picked.
function pickAccent() {
  const tinted = () => cssToHex(customPalette(accentNow, null, darkScheme.matches)["--bg"]);
  openColorPopover(customPill, accentNow, {
    tabs: [
      { key: "accent", label: "Accent", value: accentNow },
      {
        key: "background",
        label: "Background color",
        value: backgroundNow || tinted(),
        follows: !backgroundNow,
        followValue: tinted,
        followNote: "Following the accent",
        followLabel: "Follow the accent",
      },
    ],
    onInput: (hex, key) => {
      if (key === "accent") {
        accentNow = hex;
        customPill.style.setProperty("--dot", hex);
      } else backgroundNow = hex;
      paintSettings();
    },
    onChange: (hex, key) => saveColors(key === "accent" ? { toasterAccent: hex } : { toasterBackground: hex }),
  });
}

for (const pill of colorsPills.children) {
  pill.addEventListener("click", async () => {
    const value = pill.dataset.colors;
    if (value === "custom") pickAccent();
    else closeColorPopover();
    if (!pill.classList.contains("selected")) await saveColors({ toasterColors: value });
  });
}

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
  wrapper: document.querySelector("#hotkey-wrapper"),
};
const hotkeyError = document.querySelector("#hotkey-error");
const hotkeysEnabled = document.querySelector("#hotkeys-enabled");
const hotkeysGroup = document.querySelector("#hotkeys-group");
const mediaKeysNote = document.querySelector("#media-keys-note");
const mediaKeysError = document.querySelector("#media-keys-error");
let recording = null; // "player", "search" or "wrapper" while waiting for keys

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
  for (const which of ["player", "search", "wrapper"]) {
    const button = hotkeyButtons[which];
    if (recording === which) continue;
    button.textContent = prettyHotkey(status[which]);
    button.classList.toggle("off", !status[which]);
  }
  hotkeyError.textContent = [status.playerError, status.searchError, status.wrapperError]
    .filter(Boolean)
    .join(" ");
  mediaKeysNote.textContent = status.mediaNote || "";
  mediaKeysError.textContent = status.mediaError || "";
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
// (registering happens a moment after a setting changes, and again when
// the selected zone gains or loses its volume control)
listen("hotkeys-changed", () => {
  if (!recording) refreshHotkeys();
});

// ===== Media Keys =========================================================
//
// The keyboard's playback keys and its volume keys control the selected
// Roon zone, each with a switch of its own.

const mediaPlayback = document.querySelector("#media-keys-playback");
const mediaPlaybackGroup = document.querySelector("#media-playback-group");
const mediaVolume = document.querySelector("#media-keys-volume");
const mediaVolumeGroup = document.querySelector("#media-volume-group");

function showMediaKeys(settings) {
  mediaPlayback.checked = !!settings.mediaKeysPlayback;
  mediaVolume.checked = !!settings.mediaKeysVolume;
}

async function setMediaKeys(changes) {
  try {
    showMediaKeys(await invoke("update_settings", { changes }));
  } catch (err) {
    console.error(err);
    showMediaKeys(await invoke("get_settings"));
  }
}

mediaPlayback.addEventListener("change", () => setMediaKeys({ mediaKeysPlayback: mediaPlayback.checked }));
mediaPlaybackGroup.querySelector(".label-off").addEventListener("click", () => setMediaKeys({ mediaKeysPlayback: false }));
mediaPlaybackGroup.querySelector(".label-on").addEventListener("click", () => setMediaKeys({ mediaKeysPlayback: true }));
mediaVolume.addEventListener("change", () => setMediaKeys({ mediaKeysVolume: mediaVolume.checked }));
mediaVolumeGroup.querySelector(".label-off").addEventListener("click", () => setMediaKeys({ mediaKeysVolume: false }));
mediaVolumeGroup.querySelector(".label-on").addEventListener("click", () => setMediaKeys({ mediaKeysVolume: true }));

invoke("get_settings").then(showMediaKeys);
listen("settings-changed", (event) => showMediaKeys(event.payload));

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

// ===== The Wrapper (experimental) ==========================================
//
// On/off, opening at launch, and a button to open it. Its hotkey lives in
// the Toaster card's Hotkeys list and only shows while it's on.

const wrapperEnabled = document.querySelector("#wrapper-enabled");
const wrapperAtLaunch = document.querySelector("#wrapper-at-launch");
const wrapperRememberPlayer = document.querySelector("#wrapper-remember-player");
const wrapperOpen = document.querySelector("#wrapper-open");

function showWrapper(settings) {
  wrapperEnabled.checked = settings.wrapperEnabled;
  wrapperAtLaunch.checked = settings.wrapperAtLaunch;
  wrapperRememberPlayer.checked = settings.wrapperRememberPlayer;
  document.querySelectorAll(".wrapper-option").forEach((option) => {
    option.classList.toggle("disabled", !settings.wrapperEnabled);
  });
  wrapperOpen.disabled = !settings.wrapperEnabled;
  document.querySelector("#wrapper-hotkey-row").hidden = !settings.wrapperEnabled;
}

async function saveWrapper(changes) {
  try {
    await invoke("update_settings", { changes });
  } catch (err) {
    console.error(err);
    showWrapper(await invoke("get_settings"));
  }
}

function wrapperSwitch(input, group, change) {
  input.addEventListener("change", () => saveWrapper(change(input.checked)));
  const labels = document.querySelector(group);
  labels.querySelector(".label-off").addEventListener("click", () => saveWrapper(change(false)));
  labels.querySelector(".label-on").addEventListener("click", () => saveWrapper(change(true)));
}
wrapperSwitch(wrapperEnabled, "#wrapper-group", (on) => ({ wrapperEnabled: on }));
wrapperSwitch(wrapperAtLaunch, "#wrapper-launch-group", (on) => ({ wrapperAtLaunch: on }));
wrapperSwitch(wrapperRememberPlayer, "#wrapper-remember-group", (on) => ({ wrapperRememberPlayer: on }));

wrapperOpen.addEventListener("click", () => {
  invoke("open_window", { window: "wrapper" }).catch((err) => console.error(err));
});

invoke("get_settings").then(showWrapper);
listen("settings-changed", (event) => showWrapper(event.payload));

// Wrapped Discovery: one of Wrapped's own settings (wrapped-settings.json,
// so it goes with Wrapped), shown here too; Wrapped's Settings page has the
// same switch, and either one updates the other
const wrapperDiscovery = document.querySelector("#wrapper-discovery");
const showDiscovery = (wrapped) => {
  wrapperDiscovery.checked = wrapped?.discovery !== false;
};
async function saveDiscovery(on) {
  try {
    showDiscovery(await invoke("wrapped_settings_update", { changes: { discovery: on } }));
  } catch (err) {
    console.error(err);
    showDiscovery(await invoke("wrapped_settings_get").catch(() => null));
  }
}
wrapperDiscovery.addEventListener("change", () => saveDiscovery(wrapperDiscovery.checked));
{
  const labels = document.querySelector("#wrapper-discovery-group");
  labels.querySelector(".label-off").addEventListener("click", () => saveDiscovery(false));
  labels.querySelector(".label-on").addEventListener("click", () => saveDiscovery(true));
}
invoke("wrapped_settings_get").then(showDiscovery).catch(() => {});
listen("wrapped-settings-changed", (event) => showDiscovery(event.payload));

// ===== Hearts & Library ====================================================
//
// On/off and what "add to library" adds. The line underneath says whether the
// Core accepts the connection (this uses a part of Roon that isn't officially
// open to apps, so it can stop working after a Roon update).

const libraryEnabled = document.querySelector("#library-enabled");
const libraryMode = document.querySelector("#library-mode");
const banButton = document.querySelector("#ban-button");
const libraryDetail = document.querySelector("#library-detail");

function showLibrary(settings) {
  libraryEnabled.checked = settings.libraryControls;
  libraryMode.checked = settings.libraryAddMode === "album";
  banButton.checked = !!settings.banButton;
  document.querySelectorAll(".library-option").forEach((option) => {
    option.classList.toggle("disabled", !settings.libraryControls);
  });
}

const LIBRARY_TEXT = {
  off: "Off",
  connecting: "Connecting to the Roon Core...",
  ready: "Ready: the Playing tab has the button, and the quality light is on.",
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
librarySwitch(banButton, "#ban-button-group", (on) => ({ banButton: on }));

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
// Opening Settings checks again if the last check is old (10 minutes)
invoke("update_check_if_stale").then(showUpdateStatus);
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") invoke("update_check_if_stale").then(showUpdateStatus);
});
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
  // (the Updates card is on the General page)
  showPage("general", true);
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

// ===== Living Queue ========================================================
//
// The switch, and how each zone takes part: Off, Mirrored or Hide Current.
// The zones are Roon's, live; one that took part and isn't there right now
// (a phone whose Roon app is closed) stays listed under the name it had.

const lqSwitch = document.querySelector("#living-queue");
const lqStatus = document.querySelector("#lq-status");
const lqZones = document.querySelector("#lq-zones");
const lqError = document.querySelector("#lq-error");
const lqRequires = document.querySelector("#lq-requires");
const lqGroup = document.querySelector("#living-queue-group");
const LQ_MODES = [
  ["off", "Off", "This zone takes no part. Toasted never touches its queue."],
  [
    "mirrored",
    "Mirrored",
    "The zone always shows the song that's playing, ready to carry on. Its queue is replaced at every song change, which can keep the zone's device awake for as long as music plays on another zone that takes part.",
  ],
  [
    "hide",
    "Hide Current",
    "The zone is left alone, so its device can sleep. Once the music has moved on from the song it was left in, it shows a silent Living Queue track in its place. Press play on it and it jumps to the song and second you were at.",
  ],
];
let lqSettings = { on: false, zones: {}, names: {} };
let lqLive = [];
// The silent Living Queue track Hide Current shows (silenttrack.rs): "ready"
// once it's in Roon's library. Until then Hide Current can't be chosen, and
// when Toasted can't put the track there itself the page says how to.
let lqTrack = { state: "unknown", message: "", file: "" };
const lqTrackBox = document.querySelector("#lq-track");
const lqTrackMessage = document.querySelector("#lq-track-message");
const lqTrackFile = document.querySelector("#lq-track-file");
const lqTrackActions = document.querySelector("#lq-track-actions");
// Phones with the companion app: Roon ARC as a zone (companion.rs), shown
// under a divider, Off or Hide Current only.
let lqPhones = [];
let lqPairCode = null;
// (false until the app says this build pairs phones: dev builds only for now)
let lqPairing = false;
const lqPhonesBox = document.querySelector("#lq-phones");
const LQ_ARC_MODES = LQ_MODES_ARC();
function LQ_MODES_ARC() {
  return [
    ["off", "Off", "ARC takes no part."],
    [
      "hide",
      "Hide Current",
      "ARC is left alone. Tap the Living Queue playlist in ARC and it carries on at the song and second you were at. (Mirrored can't be offered for ARC: changing its queue from outside starts it playing.)",
    ],
  ];
}
const LQ_NO_TRACK = "Not available yet: Hide Current needs the silent Living Queue track in your Roon library. See the note at the top of this page.";

function lqStateLine(mode, zone) {
  if (!zone) return "Not available right now";
  if (mode === "off") return "Not taking part";
  if (zone.state === "playing") return "Playing";
  if (mode === "hide" && lqTrack.state !== "ready") return "Waiting for the Living Queue track";
  return mode === "mirrored" ? "Mirrored" : "Hidden until you press play";
}

function showLivingQueueZones() {
  lqZones.replaceChildren();
  lqZones.classList.toggle("off", !lqSettings.on);
  // (the zones Roon has, by name, then the ones that took part and are away)
  const rows = lqLive.map((zone) => ({ id: zone.zoneId, name: zone.name, zone }));
  // (ARC on a phone has its own place, under the divider)
  for (const id of Object.keys(lqSettings.zones).filter((id) => !id.startsWith("arc:"))) {
    if (!rows.some((row) => row.id === id)) rows.push({ id, name: lqSettings.names[id] || "A zone that's gone", zone: null });
  }
  if (!rows.length) {
    const empty = document.createElement("div");
    empty.className = "lq-empty";
    empty.textContent = "No zones yet. They show here once Roon is connected.";
    lqZones.append(empty);
    return;
  }
  for (const row of rows) {
    const mode = lqSettings.zones[row.id] || "off";
    const line = document.createElement("div");
    line.className = "lq-zone";
    line.classList.toggle("away", !row.zone);
    const text = document.createElement("div");
    text.className = "lq-zone-text";
    const name = document.createElement("div");
    name.className = "lq-zone-name";
    name.textContent = row.name;
    const state = document.createElement("div");
    state.className = "lq-zone-state";
    state.textContent = lqStateLine(mode, row.zone);
    text.append(name, state);
    const pills = document.createElement("div");
    pills.className = "seg-pills";
    for (const [value, label, hint] of LQ_MODES) {
      const pill = document.createElement("button");
      pill.type = "button";
      pill.className = "seg-pill";
      pill.textContent = label;
      pill.title = hint;
      // (taking part, in the accent, as a switch's "on" side)
      if (value !== "off") pill.classList.add("on-side");
      pill.classList.toggle("selected", value === mode);
      // (Hide Current, while its track isn't in the library: greyed, and it says why)
      const blocked = value === "hide" && lqSettings.on && lqTrack.state !== "ready";
      if (blocked) {
        pill.classList.add("unavailable");
        pill.title = LQ_NO_TRACK;
      }
      pill.addEventListener("click", () => {
        if (!blocked) setLivingQueueZone(row.id, row.name, value);
      });
      pills.append(pill);
    }
    line.append(text, pills);
    lqZones.append(line);
  }
}

function showLivingQueue(settings) {
  lqSettings = {
    on: !!settings.livingQueue,
    zones: settings.livingQueueZones || {},
    names: settings.livingQueueNames || {},
  };
  // (it runs on the Hearts & Library link: without it the switch is greyed and says so)
  const link = settings.libraryControls !== false;
  lqSettings.on = lqSettings.on && link;
  lqSwitch.checked = lqSettings.on;
  lqSwitch.disabled = !link;
  lqGroup.classList.toggle("needs-link", !link);
  lqRequires.hidden = link;
  showLivingQueueTrack();
}

async function saveLivingQueue(changes) {
  lqError.hidden = true;
  try {
    showLivingQueue(await invoke("update_settings", { changes, shared: true }));
  } catch (err) {
    console.error(err);
    lqError.textContent = String(err);
    lqError.hidden = false;
    showLivingQueue(await invoke("get_settings"));
  }
}

function setLivingQueueZone(id, name, mode) {
  const zones = { ...lqSettings.zones };
  const names = { ...lqSettings.names };
  if (mode === "off") {
    delete zones[id];
    delete names[id];
  } else {
    zones[id] = mode;
    names[id] = name;
  }
  saveLivingQueue({ livingQueueZones: zones, livingQueueNames: names });
}

lqSwitch.addEventListener("change", () => saveLivingQueue({ livingQueue: lqSwitch.checked }));

function showLivingQueueLive(payload) {
  // (ARC on a phone is in the player's zones too, but has its own place on this page, under the divider)
  lqLive = (payload?.zones || []).filter((z) => !z.zoneId.startsWith("arc:")).sort((a, b) => a.name.localeCompare(b.name));
  showLivingQueueZones();
}

// The line under the switch: what the living queue is doing (livequeue.rs, event living-queue-status)
function lqStatusParts(st) {
  const songs = (n) => `${n} song${n === 1 ? "" : "s"}`;
  if (st.state === "waiting") return [["Waiting for a zone taking part to play."]];
  if (st.state === "paused") return [["Paused on "], [st.leader, true], [`: ${st.title}. Press play on any zone taking part to carry on.`]];
  return [["Following "], [st.leader, true], [`: ${st.title}${st.after != null ? `, ${songs(st.after)} after it` : ""}`]];
}

function showLivingQueueStatus(st) {
  lqStatus.hidden = !st || st.state === "off";
  if (lqStatus.hidden) return;
  lqStatus.replaceChildren();
  for (const [text, bold] of lqStatusParts(st)) {
    const part = document.createElement(bold ? "b" : "span");
    part.textContent = text;
    lqStatus.append(part);
  }
  if (st.note) {
    const note = document.createElement("span");
    note.className = `lq-status-note${st.warn ? " warn" : ""}`;
    note.textContent = st.note;
    lqStatus.append(note);
  }
}

invoke("living_queue_status")
  .then(showLivingQueueStatus)
  .catch((err) => console.error(err));
listen("living-queue-status", (event) => showLivingQueueStatus(event.payload));

function lqClock(ms) {
  const s = Math.max(0, Math.round((ms || 0) / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

function lqPhoneLine(phone, mode) {
  const arc = phone.arc;
  const song = arc && arc.title ? `${arc.title} at ${lqClock(arc.positionMs)}` : "";
  if (!phone.online) {
    if (!phone.lastSeen) return "Not heard from yet. Open the companion app on the phone.";
    return song ? `Away, last on ${song}` : "Away";
  }
  if (mode === "hide" && lqTrack.state !== "ready") return "Waiting for the Living Queue track";
  if (!arc || !arc.title || arc.state === "none" || arc.state === "stopped") return "Connected, ARC isn't playing";
  const state = { playing: "Playing", paused: "Paused", buffering: "Loading" }[arc.state] || "Connected";
  return `${state}, ${song}`;
}

function showLivingQueuePhones() {
  lqPhonesBox.replaceChildren();
  lqPhonesBox.classList.toggle("off", !lqSettings.on);
  const divider = document.createElement("div");
  divider.className = "lq-divider";
  divider.textContent = lqPairing || lqPhones.length ? "Roon ARC, through the companion app" : "Roon ARC";
  lqPhonesBox.append(divider);
  for (const phone of lqPhones) {
    const mode = lqSettings.zones[phone.zoneId] === "hide" ? "hide" : "off";
    const line = document.createElement("div");
    line.className = "lq-zone";
    line.classList.toggle("away", !phone.online);
    const text = document.createElement("div");
    text.className = "lq-zone-text";
    const name = document.createElement("div");
    name.className = "lq-zone-name";
    name.textContent = `ARC on ${phone.name}`;
    const state = document.createElement("div");
    state.className = "lq-zone-state";
    state.textContent = lqPhoneLine(phone, mode);
    text.append(name, state);
    const side = document.createElement("div");
    side.style.display = "flex";
    side.style.alignItems = "center";
    const pills = document.createElement("div");
    pills.className = "seg-pills";
    for (const [value, label, hint] of LQ_ARC_MODES) {
      const pill = document.createElement("button");
      pill.type = "button";
      pill.className = "seg-pill";
      pill.textContent = label;
      pill.title = hint;
      if (value !== "off") pill.classList.add("on-side");
      pill.classList.toggle("selected", value === mode);
      const blocked = value === "hide" && lqSettings.on && lqTrack.state !== "ready";
      if (blocked) {
        pill.classList.add("unavailable");
        pill.title = LQ_NO_TRACK;
      }
      pill.addEventListener("click", () => {
        if (!blocked) setLivingQueueZone(phone.zoneId, phone.name, value);
      });
      pills.append(pill);
    }
    const forget = document.createElement("button");
    forget.type = "button";
    forget.className = "small lq-forget";
    forget.textContent = "Forget";
    forget.title = "Unpairs the phone. It has to be paired again to take part.";
    forget.addEventListener("click", async () => {
      try {
        lqPhones = await invoke("companion_forget", { zoneId: phone.zoneId });
        showLivingQueuePhones();
      } catch (err) {
        lqError.textContent = String(err);
        lqError.hidden = false;
      }
    });
    side.append(pills, forget);
    line.append(text, side);
    lqPhonesBox.append(line);
  }
  // Pairing: the code in force, or a new one when it has run out (dev builds only while the companion
  // app is unreleased: a release says "Coming soon", or nothing when a phone was paired before)
  if (!lqPairing) {
    if (!lqPhones.length) {
      const soon = document.createElement("div");
      soon.className = "lq-pair";
      soon.textContent = "Coming soon";
      lqPhonesBox.append(soon);
    }
    return;
  }
  const pair = document.createElement("div");
  pair.className = "lq-pair";
  const text = document.createElement("span");
  const code = document.createElement("span");
  code.className = "lq-pair-code";
  const fresh = document.createElement("button");
  fresh.type = "button";
  fresh.className = "small";
  fresh.textContent = "New code";
  fresh.addEventListener("click", () => lqShowPairCode(true));
  text.append(lqPhones.length ? "Pair another phone: in the companion app, enter " : "Pair a phone: in the companion app, enter ", code);
  pair.append(text, fresh);
  lqPhonesBox.append(pair);
  const shown = lqPairCode && lqPairCode.until > Date.now() ? lqPairCode.code : "";
  code.textContent = shown ? `${shown.slice(0, 3)} ${shown.slice(3)}` : "...";
  if (!shown) lqShowPairCode(false);
}

async function lqShowPairCode(fresh) {
  try {
    const got = await invoke("companion_pair_code", { fresh });
    lqPairCode = { code: got.code, until: Date.now() + got.seconds * 1000 };
    showLivingQueuePhones();
  } catch (err) {
    console.error(err);
  }
}

Promise.all([invoke("companion_devices"), invoke("companion_open").catch(() => false)])
  .then(([phones, open]) => {
    lqPhones = phones;
    lqPairing = !!open;
    showLivingQueuePhones();
  })
  .catch((err) => console.error(err));
listen("companion-devices", (event) => {
  lqPhones = event.payload || [];
  showLivingQueuePhones();
});

function showLivingQueueTrack(track) {
  lqTrack = track || lqTrack;
  const working = lqTrack.state === "working";
  const needed = lqTrack.state === "needed";
  lqTrackBox.hidden = !lqSettings.on || !(working || needed);
  lqTrackBox.classList.toggle("working", working);
  lqTrackMessage.textContent = lqTrack.message;
  lqTrackFile.textContent = lqTrack.file;
  lqTrackFile.hidden = !needed || !lqTrack.file;
  lqTrackActions.hidden = !needed;
  showLivingQueueZones();
  showLivingQueuePhones();
}

for (const [id, command] of [
  ["#lq-track-pick", "living_queue_track_pick"],
  ["#lq-track-check", "living_queue_track_check"],
]) {
  document.querySelector(id).addEventListener("click", async () => {
    lqError.hidden = true;
    try {
      showLivingQueueTrack(await invoke(command));
    } catch (err) {
      lqError.textContent = String(err);
      lqError.hidden = false;
    }
  });
}
document.querySelector("#lq-track-reveal").addEventListener("click", () => {
  invoke("living_queue_track_reveal").catch((err) => {
    lqError.textContent = String(err);
    lqError.hidden = false;
  });
});

invoke("living_queue_track_status").then(showLivingQueueTrack).catch((err) => console.error(err));
listen("living-queue-track", (event) => showLivingQueueTrack(event.payload));
invoke("get_settings").then(showLivingQueue);
listen("settings-changed", (event) => showLivingQueue(event.payload));
invoke("roon_zones").then(showLivingQueueLive).catch((err) => console.error(err));
listen("roon-zones", (event) => showLivingQueueLive(event.payload));

// ===== Skip Stuck Songs ====================================================

const skipStuck = document.querySelector("#skip-stuck");

function showSkipStuck(settings) {
  skipStuck.checked = settings.skipStuck !== false;
}

skipStuck.addEventListener("change", async () => {
  try {
    await invoke("update_settings", { changes: { skipStuck: skipStuck.checked } });
  } catch (err) {
    console.error(err);
    showSkipStuck(await invoke("get_settings"));
  }
});

invoke("get_settings").then(showSkipStuck);
listen("settings-changed", (event) => showSkipStuck(event.payload));

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


// ===== What's new ==========================================================
//
// The Changelog button (Updates card) opens the Changelog window. Things new
// since an update (data-new="name" in settings.html) glow like the Roon Core
// card does before a Core is paired, until the mouse goes over them once;
// the names seen are kept in the settings (seenNew).

document.querySelector("#open-changelog").addEventListener("click", () => {
  invoke("open_window", { window: "changelog" }).catch((err) => console.error(err));
});

function showNew(settings) {
  const seen = new Set(settings.seenNew || []);
  document.querySelectorAll("[data-new]").forEach((node) => {
    node.classList.toggle("is-new", !seen.has(node.dataset.new));
  });
  showPageGlows();
}

async function markSeen(name) {
  try {
    const settings = await invoke("get_settings");
    const seen = new Set(settings.seenNew || []);
    if (seen.has(name)) return;
    seen.add(name);
    await invoke("update_settings", { changes: { seenNew: [...seen] } });
  } catch (err) {
    console.error(err);
  }
}

document.querySelectorAll("[data-new]").forEach((node) => {
  node.addEventListener("mouseenter", () => {
    if (!node.classList.contains("is-new")) return;
    node.classList.remove("is-new");
    showPageGlows();
    markSeen(node.dataset.new);
  });
});

invoke("get_settings").then(showNew);
listen("settings-changed", (event) => showNew(event.payload));

// The footer link opens in the browser, not in this window
document.querySelector("#plugin-link").addEventListener("click", (event) => {
  event.preventDefault();
  invoke("plugin:opener|open_url", { url: event.currentTarget.href });
});

// ===== The window's height follows the page =================================
//
// The window is as tall as what it shows: the page that's picked (its
// cards, with any Legend that's open, and the footer's line), and never
// shorter than the list of pages on the left. It grows downward as a page
// gets longer and shrinks again when it gets shorter; lib.rs
// (`settings_fit_height`) keeps it on the screen, and past that the page
// scrolls as before. Not while maximized.

const fitNav = document.querySelector("#pages-nav");
const fitMain = document.querySelector("main");
const fitFooter = document.querySelector(".footer");
const fitBar = document.querySelector(".titlebar");
let fitQueued = false;
let fitLast = 0;
function fitHeight() {
  if (fitQueued) return;
  fitQueued = true;
  requestAnimationFrame(() => {
    fitQueued = false;
    // (the list's own height: its last entry's bottom, not its box, which
    // is as tall as the window)
    const last = fitNav.lastElementChild;
    const nav = last ? last.getBoundingClientRect().bottom - fitNav.getBoundingClientRect().top + fitNav.scrollTop + 12 : 0;
    const page = fitMain.offsetHeight + fitFooter.offsetHeight;
    const height = Math.ceil(fitBar.offsetHeight + Math.max(nav, page)) + 2;
    if (height === fitLast) return;
    fitLast = height;
    invoke("settings_fit_height", { height }).catch(() => {});
  });
}
new ResizeObserver(fitHeight).observe(fitMain);
new MutationObserver(fitHeight).observe(fitNav, { childList: true, subtree: true, attributes: true, attributeFilter: ["hidden", "class"] });
// (shown again, or moved to another screen: the same page may need another
// height there)
window.addEventListener("focus", () => {
  fitLast = 0;
  fitHeight();
});
fitHeight();
