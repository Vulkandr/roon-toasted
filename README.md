# Roon: Toasted

A Roon companion for Windows that lives in the tray. It works on its own,
no Stream Deck needed, and pairs with **Roon: Dialed Up** for Stream Deck
if you have one.

- **The Toaster:** now playing, queue, browse and search for any zone, with
  playback, volume and zone controls. Opens from the tray, a hotkey, or a
  `roon-toasted://` link.
- **Now-playing toasts** when the track changes, in the corner and on the
  monitor you choose.
- **Global hotkeys:** Ctrl + Alt + Z opens (or hides) the Toaster,
  Ctrl + Alt + S opens it on Search. Both can be changed or turned off.
- **Taskbar widget** (experimental): a small player on the taskbar.
- **Album Art colors:** the Toaster, toasts and widget can take their colors
  from the album that's playing.
- Tray quick settings, Start with Windows, zoom, and light/dark themes.

## Setup

1. Install Roon: Toasted and start it. It appears in the tray.
2. In Roon, go to **Settings > Extensions** and enable **Roon: Toasted**.
3. Double-click the tray icon (or press Ctrl + Alt + Z) to open the Toaster.

## Links for other apps

Other apps (like the Stream Deck plugin) can open the Toaster with these links:

| Link | What it does |
|---|---|
| `roon-toasted://toggle` | Opens the Toaster, or hides it if it's open and in front |
| `roon-toasted://open` | Opens the Toaster on the Playing tab |
| `roon-toasted://search` | Opens the Toaster on Search, ready to type |

The app registers these links for the current user every time it starts.

## Status check for other apps

While Roon: Toasted is running, `http://127.0.0.1:58421/status` (this PC only)
answers with JSON, so other apps can tell it's running:

```json
{ "app": "Roon: Toasted", "version": "0.9.0", "roon": "connected", "core": "Roon Optimized Core Kit" }
```

`roon` is `connected`, `searching` or `reconnecting`; `core` is the Core's name
once connected, otherwise `null`. No answer means it isn't running.

## Building

Needs [Rust](https://rustup.rs), [Node.js](https://nodejs.org) and the
Microsoft C++ Build Tools.

```
npm install
npm run tauri dev      # run it while developing
npm run tauri build    # build the installer (src-tauri/target/release/bundle/nsis)
```

App icons are generated from `branding/app-icon-1024.png` with `npx tauri icon`;
`src-tauri/icons/icon.ico` and `tray.ico` also use the simplified drawing
(`branding/app-icon-small-1024.png`, `branding/logo-mask-small-1024.png`) at
small sizes.

## Credits

- Built with [Tauri](https://tauri.app) (MIT or Apache-2.0).
- Roon connection: [roon-api and roon-sood](https://github.com/shin1ohno/roon-rs)
  by shin1ohno (MIT or Apache-2.0). Patched copies of both are included in
  `src-tauri/vendor`; see `VENDORED.md` in each folder for the changes.
- Icons in the interface: [Lucide](https://lucide.dev) (ISC license).
- Fonts: [Gloock](https://fonts.google.com/specimen/Gloock),
  [Lato](https://fonts.google.com/specimen/Lato) and
  [Noto Sans](https://fonts.google.com/noto/specimen/Noto+Sans), all under the
  SIL Open Font License 1.1 (license files in `src/assets/fonts`), packaged by
  [Fontsource](https://fontsource.org).

Roon is a trademark of Roon Labs LLC. Roon: Toasted is an independent project,
not made or endorsed by Roon Labs.
