// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Shows what every zone is playing with its heart / library state, then
//! (optionally) hearts, un-hearts or adds the track in one zone.
//!
//!   ROON_HOST=192.168.0.68 ROON_CORE_ID=<core id> cargo run --example heart_now_playing
//!   ... heart <zoneId>            heart what that zone is playing (adds a single track first if needed)
//!   ... unheart <zoneId>
//!   ... add <zoneId> [track|album]
//!   ... heartalbum <zoneId> | unheartalbum <zoneId>
//!   ... albums <zoneId>          the playing artist's discography (with handles)
//!   ... albumtracks <zoneId> <title>      (title: part of a discography album's title)
//!   ... addalbum <zoneId> <title> | heartalbumh <zoneId> <title> | unheartalbumh <zoneId> <title>
//!   ... watch                    keep running and print changes

use roon_library::{AddMode, ClientOptions, LibraryClient, LibraryEvent, TrackInfo};

fn show(t: &Option<TrackInfo>) -> String {
    match t {
        None => "(nothing loaded)".into(),
        Some(t) => {
            let mut s = format!(
                "\"{}\"  library={}  heart={}  source={}",
                t.title,
                if t.in_library { "yes" } else { "no" },
                if t.favorite { "yes" } else { "no" },
                t.source.unwrap_or("?")
            );
            if let Some(a) = &t.album {
                s += &format!(
                    "  album=\"{}\" (library={}, heart={})",
                    a.title,
                    if a.in_library { "yes" } else { "no" },
                    if a.favorite { "yes" } else { "no" }
                );
            }
            s
        }
    }
}

/// The handle of the first discography album whose title contains `title` (case-insensitive).
async fn find_album(roon: &LibraryClient, zone: &str, title: &str) -> String {
    let albums = roon.artist_albums(zone).await.expect("discography");
    let wanted = title.to_lowercase();
    let a = albums
        .iter()
        .find(|a| a.title.to_lowercase().contains(&wanted))
        .expect("no album with that title in the discography");
    println!("album \"{}\" [{}] library={} heart={}", a.title, a.group, a.in_library, a.favorite);
    a.handle.clone()
}

#[tokio::main]
async fn main() {
    let host = std::env::var("ROON_HOST").expect("set ROON_HOST");
    let core_id = std::env::var("ROON_CORE_ID").expect("set ROON_CORE_ID");
    let args: Vec<String> = std::env::args().skip(1).collect();

    let roon = match LibraryClient::connect(ClientOptions::new(host, core_id)).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAILED: {e}");
            std::process::exit(1);
        }
    };
    println!("connected, {} objects, zones: {}", roon.object_count(), roon.zone_ids().len());
    let mut zones = roon.zone_ids();
    zones.sort();
    for z in &zones {
        println!("* {z}: {}", show(&roon.track_for_zone(z)));
    }

    let result = match (args.first().map(String::as_str), args.get(1)) {
        (Some("heart"), Some(z)) => Some(roon.set_favorite(z, true, Some(AddMode::Track)).await),
        (Some("unheart"), Some(z)) => Some(roon.set_favorite(z, false, None).await),
        (Some("add"), Some(z)) => {
            let mode = if args.get(2).map(String::as_str) == Some("album") { AddMode::Album } else { AddMode::Track };
            Some(roon.add_to_library(z, mode).await)
        }
        (Some("tracks"), Some(z)) => {
            match roon.album_tracks(z).await {
                Ok(tracks) => {
                    for t in &tracks {
                        println!(
                            "  #{} \"{}\" handle={} library={} heart={}{}",
                            t.track_number.unwrap_or(0),
                            t.title,
                            t.handle,
                            if t.in_library { "yes" } else { "no" },
                            if t.favorite { "yes" } else { "no" },
                            if t.playing { "  <- playing" } else { "" }
                        );
                    }
                }
                Err(e) => eprintln!("FAILED: {e}"),
            }
            None
        }
        (Some("play"), Some(z)) => {
            let handle = args.get(2).expect("play <zone> <handle>");
            match roon.play_track(z, handle).await {
                Ok(()) => println!("-> playing"),
                Err(e) => eprintln!("FAILED: {e}"),
            }
            None
        }
        (Some("addtrack"), Some(_)) => {
            let handle = args.get(2).expect("addtrack <zone> <handle>");
            match roon.add_track(handle).await {
                Ok(t) => println!("-> \"{}\" library={} heart={}", t.title, t.in_library, t.favorite),
                Err(e) => eprintln!("FAILED: {e}"),
            }
            None
        }
        // tracktest <zone> <n> [play]: add track n of the playing album, heart it, un-heart it, optionally play it
        (Some("tracktest"), Some(z)) => {
            let n: i32 = args.get(2).and_then(|v| v.parse().ok()).expect("tracktest <zone> <n> [play]");
            let tracks = roon.album_tracks(z).await.expect("album tracks");
            let t = tracks.iter().find(|t| t.track_number == Some(n)).expect("no such track number");
            println!("track #{n} \"{}\" library={} heart={}", t.title, t.in_library, t.favorite);
            if args.get(3).map(String::as_str) == Some("play") {
                match roon.play_track(z, &t.handle).await {
                    Ok(()) => println!("-> play sent"),
                    Err(e) => eprintln!("play FAILED: {e}"),
                }
            }
            match roon.add_track(&t.handle).await {
                Ok(a) => println!("-> added: library={} heart={}", a.in_library, a.favorite),
                Err(e) => eprintln!("add FAILED: {e}"),
            }
            match roon.set_track_favorite(&t.handle, true).await {
                Ok(a) => println!("-> hearted: library={} heart={}", a.in_library, a.favorite),
                Err(e) => eprintln!("heart FAILED: {e}"),
            }
            match roon.set_track_favorite(&t.handle, false).await {
                Ok(a) => println!("-> un-hearted: library={} heart={}", a.in_library, a.favorite),
                Err(e) => eprintln!("unheart FAILED: {e}"),
            }
            None
        }
        (Some("albums"), Some(z)) => {
            let started = std::time::Instant::now();
            match roon.artist_albums(z).await {
                Ok(albums) => {
                    println!("{} albums in {} ms, {} objects now", albums.len(), started.elapsed().as_millis(), roon.object_count());
                    for a in &albums {
                        println!(
                            "  [{}] \"{}\" by {} handle={} library={} heart={}{}",
                            a.group,
                            a.title,
                            a.artist,
                            a.handle,
                            if a.in_library { "yes" } else { "no" },
                            if a.favorite { "yes" } else { "no" },
                            if a.playing { "  <- playing" } else { "" }
                        );
                    }
                }
                Err(e) => eprintln!("FAILED: {e}"),
            }
            None
        }
        (Some("albumtracks"), Some(z)) => {
            let handle = find_album(&roon, z, args.get(2).expect("albumtracks <zone> <title>")).await;
            match roon.album_tracks_for(z, &handle).await {
                Ok(tracks) => {
                    for t in &tracks {
                        println!("  #{} \"{}\" handle={} library={} heart={}", t.track_number.unwrap_or(0), t.title, t.handle, t.in_library, t.favorite);
                    }
                }
                Err(e) => eprintln!("FAILED: {e}"),
            }
            None
        }
        (Some("addalbum"), Some(z)) => {
            let handle = find_album(&roon, z, args.get(2).expect("addalbum <zone> <title>")).await;
            match roon.add_album_for(&handle).await {
                Ok(a) => println!("-> \"{}\" library={} heart={}", a.title, a.in_library, a.favorite),
                Err(e) => eprintln!("FAILED: {e}"),
            }
            None
        }
        (Some("heartalbumh"), Some(z)) | (Some("unheartalbumh"), Some(z)) => {
            let on = args[0] == "heartalbumh";
            let handle = find_album(&roon, z, args.get(2).expect("heartalbumh <zone> <title>")).await;
            match roon.set_album_favorite_for(&handle, on).await {
                Ok(a) => println!("-> \"{}\" library={} heart={}", a.title, a.in_library, a.favorite),
                Err(e) => eprintln!("FAILED: {e}"),
            }
            None
        }
        (Some("heartalbum"), Some(z)) => Some(roon.set_album_favorite(z, true).await),
        (Some("unheartalbum"), Some(z)) => Some(roon.set_album_favorite(z, false).await),
        (Some("watch"), _) => {
            println!("watching for changes, Ctrl+C to stop");
            let mut events = roon.events();
            while let Ok(ev) = events.recv().await {
                match ev {
                    LibraryEvent::Track { zone_id, track } => println!("* {zone_id}: {}", show(&track)),
                    LibraryEvent::Disconnected { reason } => {
                        println!("disconnected: {reason}");
                        break;
                    }
                }
            }
            None
        }
        _ => None,
    };
    match result {
        Some(Ok(t)) => println!("-> {}", show(&Some(t))),
        Some(Err(e)) => {
            eprintln!("FAILED: {e}");
            roon.close();
            std::process::exit(1);
        }
        None => {}
    }
    roon.close();
}
