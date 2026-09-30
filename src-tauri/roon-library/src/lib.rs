// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Heart the track a Roon zone is playing, and add it (or its album) to the
//! library, over Roon's internal client protocol (TCP 9332).
//!
//! Unofficial and experimental: Roon changes this protocol whenever they like,
//! so any Roon update can break it. Roon's official extension API cannot do
//! either of these things, which is the only reason this exists.
//!
//! ```no_run
//! # async fn demo() -> Result<(), roon_library::LibraryError> {
//! use roon_library::{ClientOptions, LibraryClient, AddMode};
//! let roon = LibraryClient::connect(ClientOptions::new("192.168.0.68", "fafb763c-9ad0-4f07-887d-44e19b8374e0")).await?;
//! let zone = "160135c1c87491c726d7a10a9e088d25718a";
//! let track = roon.set_favorite(zone, true, Some(AddMode::Track)).await?; // adds first if needed
//! assert!(track.favorite);
//! roon.close();
//! # Ok(()) }
//! ```

mod client;
pub mod connection;
pub mod frame;
pub mod graph;
pub mod remoting;
pub mod wire;

pub use client::{read_state, AddMode, AlbumInfo, AlbumTrack, ArtistAlbum, ClientOptions, LibraryClient, LibraryError, LibraryEvent, TrackInfo};
pub use connection::{broker_id_from_core_id, DEFAULT_PORT};
