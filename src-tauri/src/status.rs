// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Status check and library controls for other apps on this PC, mainly the
//! Roon: Dialed Up Stream Deck plugin.
//!
//! While the app runs it answers `GET http://127.0.0.1:58421/status` with:
//!
//! ```json
//! { "app": "Roon: Toasted", "version": "1.1.1",
//!   "roon": "connected", "core": "Roon Optimized Core Kit", "library": "ready" }
//! ```
//!
//! - `roon`: "connected", "searching" (looking for the Core, or waiting to be
//!   enabled in Roon) or "reconnecting" (lost the Core, retrying)
//! - `core`: the Core's name once connected, otherwise null
//! - `library`: the hearts / add-to-library feature (see library.rs): "off",
//!   "connecting", "ready", "unsupported" or "unavailable"
//!
//! Hearts and add-to-library (experimental; only while `library` is "ready"):
//!
//! - `GET /library?zone=<zoneId>` -> `{ "zoneId": ..., "track": {...} | null }`
//!   with the track's `title`, `inLibrary`, `favorite`, `album`, ...
//! - `POST /library/heart` with `{ "zoneId": "...", "favorite": true }`
//! - `POST /library/add` with `{ "zoneId": "...", "mode": "track" | "album" }` (mode optional)
//!
//! Both POSTs answer with the confirmed track, or 409 with `{ "error": ... }`
//! when the feature is off / not connected and 400 for a bad request.
//!
//! No answer means Roon: Toasted isn't running. It only listens on this PC's
//! own address (never the network). It refuses requests that don't name this
//! PC as the host, so a web page can't reach it by pointing a made-up domain
//! name at this PC ("DNS rebinding"), and there's no CORS header, so pages in
//! a browser can't read answers at all; the plugin asks from its Node side.
//!
//! If the port is taken (another program, or the previous copy still closing)
//! it tries again every 30 seconds.

use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The port the status check answers on. Well away from Roon's own ports
/// (9003, 9100 to 9200, 9330 to 9339).
pub const STATUS_PORT: u16 = 58421;

/// Starts listening (in setup); runs for the life of the app.
pub fn start(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match TcpListener::bind(("127.0.0.1", STATUS_PORT)).await {
                Ok(listener) => serve(&app, listener).await,
                Err(e) => eprintln!(
                    "[status] can't listen on 127.0.0.1:{STATUS_PORT} ({e}), trying again in 30 s"
                ),
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
}

async fn serve(app: &AppHandle, listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    // A client that never finishes its request is dropped
                    let _ = tokio::time::timeout(Duration::from_secs(3), answer(&app, stream)).await;
                });
            }
            // Usually a client that gave up; don't spin if it keeps happening
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

/// Largest request accepted (headers plus body).
const MAX_REQUEST: usize = 16 * 1024;

async fn answer(app: &AppHandle, mut stream: TcpStream) -> std::io::Result<()> {
    // Read the header, then as much body as Content-Length announces
    let mut request = Vec::new();
    let mut chunk = [0u8; 1024];
    let header_end = loop {
        if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
            break Some(pos + 4);
        }
        if request.len() >= MAX_REQUEST {
            break None;
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break None;
        }
        request.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&request[..header_end.unwrap_or(request.len())]).into_owned();
    let wanted = content_length(&head).min(MAX_REQUEST);
    if let Some(start) = header_end {
        while request.len() - start < wanted {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
        }
    }
    let body_bytes = header_end.map(|start| &request[start..]).unwrap_or(&[]);

    let (status, body) = match route(&head) {
        Route::Status => ("200 OK", status_json(app)),
        Route::LibraryTrack(zone_id) => library_track(app, zone_id),
        Route::LibraryHeart => library_change(app, body_bytes, true).await,
        Route::LibraryAdd => library_change(app, body_bytes, false).await,
        Route::NotFound => ("404 Not Found", json!({ "error": "not found" })),
        Route::Forbidden => ("403 Forbidden", json!({ "error": "forbidden" })),
    };
    let body = body.to_string();
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await
}

#[derive(Debug, PartialEq)]
enum Route {
    Status,
    /// `GET /library?zone=<zoneId>`
    LibraryTrack(String),
    /// `POST /library/heart`
    LibraryHeart,
    /// `POST /library/add`
    LibraryAdd,
    NotFound,
    Forbidden,
}

/// What a request asks for, if it is addressed to this PC.
fn route(request: &str) -> Route {
    let mut lines = request.lines();
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default();
    let full_path = first.next().unwrap_or_default();
    let (path, query) = full_path.split_once('?').unwrap_or((full_path, ""));

    let local_host = lines
        .filter_map(|line| line.split_once(':'))
        .any(|(name, value)| name.trim().eq_ignore_ascii_case("host") && is_this_pc(value.trim()));
    if !local_host {
        return Route::Forbidden;
    }
    match (method, path) {
        ("GET", "/status") => Route::Status,
        ("GET", "/library") => match query_value(query, "zone") {
            Some(zone) if !zone.is_empty() => Route::LibraryTrack(zone),
            _ => Route::NotFound,
        },
        ("POST", "/library/heart") => Route::LibraryHeart,
        ("POST", "/library/add") => Route::LibraryAdd,
        _ => Route::NotFound,
    }
}

/// One value from a query string (`a=1&zone=abc`); zone ids are plain hex, so
/// no URL decoding is needed beyond that.
fn query_value(query: &str, name: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

fn content_length(head: &str) -> usize {
    head.lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0)
}

fn library_track(app: &AppHandle, zone_id: String) -> (&'static str, serde_json::Value) {
    let library = app.state::<crate::library::Library>();
    if library.status().state != crate::library::LibraryState::Ready {
        return ("409 Conflict", json!({ "error": "library controls are not ready", "library": crate::library::state_word(app) }));
    }
    match crate::library::library_track(library, zone_id.clone()) {
        Ok(track) => ("200 OK", json!({ "zoneId": zone_id, "track": track })),
        Err(e) => ("409 Conflict", json!({ "error": e })),
    }
}

/// `POST /library/heart` (`heart` = true) or `POST /library/add`.
async fn library_change(app: &AppHandle, body: &[u8], heart: bool) -> (&'static str, serde_json::Value) {
    let Ok(request) = serde_json::from_slice::<serde_json::Value>(body) else {
        return ("400 Bad Request", json!({ "error": "expected a JSON body" }));
    };
    let Some(zone_id) = request.get("zoneId").and_then(|v| v.as_str()).map(str::to_string) else {
        return ("400 Bad Request", json!({ "error": "zoneId is required" }));
    };
    if app.state::<crate::library::Library>().status().state != crate::library::LibraryState::Ready {
        return ("409 Conflict", json!({ "error": "library controls are not ready", "library": crate::library::state_word(app) }));
    }
    let result = if heart {
        let favorite = request.get("favorite").and_then(|v| v.as_bool()).unwrap_or(true);
        crate::library::library_heart(app.clone(), zone_id, favorite).await
    } else {
        let mode = match request.get("mode").and_then(|v| v.as_str()) {
            Some("track") => Some(roon_library::AddMode::Track),
            Some("album") => Some(roon_library::AddMode::Album),
            _ => None,
        };
        crate::library::library_add(app.clone(), zone_id, mode).await
    };
    match result {
        Ok(track) => ("200 OK", serde_json::to_value(track).unwrap_or(json!({}))),
        Err(e) => ("409 Conflict", json!({ "error": e })),
    }
}

/// "127.0.0.1:58421", "localhost:58421", with or without the port.
fn is_this_pc(host: &str) -> bool {
    let name = host.rsplit_once(':').map_or(host, |(name, _)| name);
    matches!(name.to_ascii_lowercase().as_str(), "127.0.0.1" | "localhost")
}

fn status_json(app: &AppHandle) -> serde_json::Value {
    let roon = app.state::<crate::roon::Roon>().status();
    let core = match roon.state {
        crate::roon::ConnectionState::Connected => roon.connected.map(|core| core.name),
        _ => None,
    };
    json!({
        "app": "Roon: Toasted",
        "version": app.package_info().version.to_string(),
        "roon": roon.state,
        "core": core,
        "library": crate::library::state_word(app),
    })
}

#[cfg(test)]
mod tests {
    use super::{content_length, route, Route};

    #[test]
    fn requests() {
        let get = |path: &str, host: &str| format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n\r\n");
        assert_eq!(route(&get("/status", "127.0.0.1:58421")), Route::Status);
        assert_eq!(route(&get("/status?x=1", "localhost:58421")), Route::Status);
        assert_eq!(route(&get("/status", "LOCALHOST")), Route::Status);
        assert_eq!(route(&get("/", "127.0.0.1:58421")), Route::NotFound);
        assert_eq!(route("POST /status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"), Route::NotFound);
        assert_eq!(route(&get("/library?zone=1601abcd", "127.0.0.1:58421")), Route::LibraryTrack("1601abcd".into()));
        assert_eq!(route(&get("/library", "127.0.0.1:58421")), Route::NotFound);
        assert_eq!(route("POST /library/heart HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\n\r\n{}"), Route::LibraryHeart);
        assert_eq!(route("POST /library/add HTTP/1.1\r\nHost: localhost\r\n\r\n"), Route::LibraryAdd);
        assert_eq!(route("GET /library/heart HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"), Route::NotFound);
        assert_eq!(content_length("POST /x HTTP/1.1\r\nContent-Length: 42\r\n\r\n"), 42);
        assert_eq!(content_length("GET /x HTTP/1.1\r\n\r\n"), 0);
        // A web page using a made-up domain that points at this PC
        assert_eq!(route(&get("/status", "evil.example:58421")), Route::Forbidden);
        assert_eq!(route("GET /status HTTP/1.1\r\n\r\n"), Route::Forbidden);
    }
}
