# Vendored copy of roon-sood

This folder is a local copy of the `roon-sood` crate, version 0.5.3, from
https://github.com/shin1ohno/roon-rs (Copyright (c) 2026 shin1ohno), used under
the MIT license (see LICENSE-MIT; the crate is dual-licensed MIT OR Apache-2.0,
see LICENSE-APACHE). It finds Roon Cores on the network.

`src-tauri/Cargo.toml` swaps it in for the crates.io version with a
`[patch.crates-io]` entry.

## Changes from the published 0.5.3

1. `src/discovery.rs`, `get_local_ipv4_addrs()`: doesn't run `hostname -I` on
   Windows. Windows' hostname.exe has no `-I` option (it only prints an error,
   so the command never found any addresses there), and starting a console
   program from a windowed app flashes a console window on screen, every
   5 seconds while discovery runs. Other systems are unchanged.
2. `Cargo.toml`: removed the dev-dependency entry (proptest), since this copy
   isn't tested on its own.

If a future roon-sood release fixes this, remove this folder and its
`[patch.crates-io]` entry and go back to the published crate.
