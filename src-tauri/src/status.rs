//! Status check for other apps on this PC, mainly the Roon: Dialed Up
//! Stream Deck plugin, which shows a status light for Roon: Toasted.
//!
//! While the app runs it answers `GET http://127.0.0.1:58421/status` with:
//!
//! ```json
//! { "app": "Roon: Toasted", "version": "0.9.0",
//!   "roon": "connected", "core": "Roon Optimized Core Kit" }
//! ```
//!
//! - `roon`: "connected", "searching" (looking for the Core, or waiting to be
//!   enabled in Roon) or "reconnecting" (lost the Core, retrying)
//! - `core`: the Core's name once connected, otherwise null
//!
//! No answer means Roon: Toasted isn't running. It only listens on this PC's
//! own address (never the network), only answers that one question, and
//! can't be told to do anything. It also refuses requests that don't name
//! this PC as the host, so a web page can't read it by pointing a made-up
//! domain name at this PC ("DNS rebinding"). There's no CORS header either,
//! so pages in a browser can't read the answer at all; the plugin asks from
//! its Node side.
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

async fn answer(app: &AppHandle, mut stream: TcpStream) -> std::io::Result<()> {
    // Read the request's header (all we need; requests have no body)
    let mut request = Vec::new();
    let mut chunk = [0u8; 1024];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < 8192 {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
    }

    let (status, body) = match route(&String::from_utf8_lossy(&request)) {
        Route::Status => ("200 OK", status_json(app)),
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
    NotFound,
    Forbidden,
}

/// What a request asks for: only `GET /status`, addressed to this PC.
fn route(request: &str) -> Route {
    let mut lines = request.lines();
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default();
    let path = first.next().unwrap_or_default();
    let path = path.split('?').next().unwrap_or_default();

    let local_host = lines
        .filter_map(|line| line.split_once(':'))
        .any(|(name, value)| name.trim().eq_ignore_ascii_case("host") && is_this_pc(value.trim()));
    if !local_host {
        return Route::Forbidden;
    }
    if method == "GET" && path == "/status" {
        Route::Status
    } else {
        Route::NotFound
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
    })
}

#[cfg(test)]
mod tests {
    use super::{route, Route};

    #[test]
    fn requests() {
        let get = |path: &str, host: &str| format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n\r\n");
        assert_eq!(route(&get("/status", "127.0.0.1:58421")), Route::Status);
        assert_eq!(route(&get("/status?x=1", "localhost:58421")), Route::Status);
        assert_eq!(route(&get("/status", "LOCALHOST")), Route::Status);
        assert_eq!(route(&get("/", "127.0.0.1:58421")), Route::NotFound);
        assert_eq!(route("POST /status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"), Route::NotFound);
        // A web page using a made-up domain that points at this PC
        assert_eq!(route(&get("/status", "evil.example:58421")), Route::Forbidden);
        assert_eq!(route("GET /status HTTP/1.1\r\n\r\n"), Route::Forbidden);
    }
}
