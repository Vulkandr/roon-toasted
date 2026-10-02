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
2. `src/discovery.rs`: sends the discovery query out of every IPv4 network
   adapter instead of only the one Windows picks. Upstream uses one send socket
   bound to 0.0.0.0, so with a VPN, Hyper-V, WSL or VirtualBox adapter present
   the query could leave on the wrong adapter and the Core was never found
   (until a reboot happened to change the choice). Now there is one send socket
   per adapter address (multicast egress set to that adapter, plus the
   255.255.255.255 and subnet-directed broadcasts), found with the `if-addrs`
   crate and re-checked every 5 seconds; the original OS-chosen socket is kept
   as well. Replies are read on each adapter's socket. New dependency:
   `if-addrs` in `Cargo.toml`.
   Also added (used by Settings > Roon Core): `SoodDiscovery::control()`, a
   cloneable handle with `search_now()`, `set_known_hosts()` (addresses asked
   directly, by unicast, on every search) and `diagnostics()` (searches sent,
   per-adapter queries and answers, last answer). `src/lib.rs` re-exports the
   new types.
3. `Cargo.toml`: added the `if-addrs` dependency (see 2); removed the dev-dependency entry (proptest), since this copy
   isn't tested on its own.

If a future roon-sood release fixes this, remove this folder and its
`[patch.crates-io]` entry and go back to the published crate.
