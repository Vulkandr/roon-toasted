// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The Changelog window: shows changelog.json, one card per version, newest
// first. The installed version gets a tag. Each entry is [lead, text]:
// the lead in bold, then the text.

const appWindow = window.__TAURI__.window.getCurrentWindow();
const versions = document.querySelector("#versions");

document.querySelector("#win-close").addEventListener("click", () => appWindow.close());
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") appWindow.close();
});
// No right-click menu
document.addEventListener("contextmenu", (event) => event.preventDefault());

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text != null) node.textContent = text;
  return node;
}

// "2026-10-01" -> "October 1, 2026" (written out, so no time zone shifts it)
function longDate(iso) {
  const [year, month, day] = (iso || "").split("-").map(Number);
  if (!year || !month || !day) return "";
  const months = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
  return `${months[month - 1]} ${day}, ${year}`;
}

function render(entries, installed) {
  versions.replaceChildren(
    ...entries.map((entry) => {
      const card = element("section", "card version");
      const head = element("div", "version-head");
      head.append(element("h2", null, `Version ${entry.version}`));
      const date = longDate(entry.date);
      if (date) head.append(element("span", "version-date", date));
      if (entry.version === installed) {
        card.classList.add("installed");
        head.append(element("span", "version-tag", "Installed"));
      }
      card.append(head);
      for (const section of entry.sections || []) {
        card.append(element("h3", null, section.title));
        const list = element("ul");
        for (const [lead, text] of section.items || []) {
          const item = element("li");
          if (lead) item.append(element("strong", null, lead), text ? ": " : "");
          if (text) item.append(text);
          list.append(item);
        }
        card.append(list);
      }
      return card;
    }),
  );
}

async function load() {
  const installed = await window.__TAURI__.app.getVersion().catch(() => "");
  try {
    const response = await fetch("changelog.json", { cache: "no-store" });
    render(await response.json(), installed);
  } catch (err) {
    console.error(err);
    versions.replaceChildren(element("p", "changelog-error", "The changelog couldn't be loaded."));
  }
}

load();
