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