# Vendored copy of roon-api

This folder is a local copy of the `roon-api` crate, version 0.5.3, from
https://github.com/shin1ohno/roon-rs (Copyright (c) 2026 shin1ohno), used under
the MIT license (see LICENSE-MIT; the crate is dual-licensed MIT OR Apache-2.0,
see LICENSE-APACHE).

`src-tauri/Cargo.toml` swaps it in for the crates.io version with a
`[patch.crates-io]` entry.

## Changes from the published 0.5.3

1. `src/transport.rs`, `change_settings()`: the settings (shuffle, loop,
   auto_radio) are now sent as top-level fields next to `zone_or_output_id`,
   matching RoonLabs' official node-roon-api-transport. The published version
   wraps them in a `"settings"` object, which Roon ignores, so shuffle, repeat
   and Roon Radio changes silently did nothing.
2. `src/browse.rs`: added fields Roon sends but the published structs dropped:
   `BrowseList.subtitle`, and `BrowseResult.message` / `BrowseResult.is_error`
   (the text Roon returns when an action answers with a message, e.g. an error).
3. `src/transport.rs`: added queue support, which the published version doesn't
   have: `subscribe_queue()`, `unsubscribe_queue()`, `play_from_here()`, and the
   `QueueItem` / `QueueEvent` / `QueueChange` types (exported from lib.rs),
   following RoonLabs' node-roon-api-transport.
4. `Cargo.toml`: removed the example, test and dev-dependency entries, since
   those files aren't included here.
5. `src/client.rs` and `src/connection.rs`: the automatic reconnect after
   `connect()` can be pointed at a new address with the new
   `RoonClient::set_core_address(host, port)`. The published version keeps
   retrying the address it first connected to forever, so a Core that got a
   new IP address (DHCP, a router change) was never found again until the app
   restarted. A waiting reconnect tries a new address right away.

If a future roon-api release includes these fixes (and anything added below),
remove this folder and the `[patch.crates-io]` entry and go back to the
published crate.
