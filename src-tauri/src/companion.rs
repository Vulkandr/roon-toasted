// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The phone companion app's way in: Roon ARC as a living queue zone.
//!
//! Roon keeps ARC's playback to ARC (the Core sends no zone for it to any
//! other client), so the companion app on the phone reads ARC's state from
//! its media session and tells Toasted, and Toasted shows ARC as one more
//! zone on the Living Queue page (Off or Hide Current).
//!
//! Toasted listens on the network, port 58422:
//!
//! - UDP: a datagram `ROON-TOASTED?` is answered with
//!   `{ "app": "Roon: Toasted", "name": <this PC's name>, "port": 58422 }`,
//!   so the companion finds Toasted on the same Wi-Fi by itself.
//! - TCP, HTTP with JSON bodies:
//!   - `GET /companion/hello`: the same as the UDP answer (no pairing
//!     needed; for typing an address by hand, as a Tailscale one).
//!   - `POST /companion/pair` `{ "code": "482917", "device": "Galaxy S24" }`:
//!     the six digits shown on the Living Queue page. Answers
//!     `{ "id", "token", "name" }`; the token goes with every later request
//!     as `Authorization: Bearer <token>`. A code is good for ten minutes and
//!     one phone; five wrong tries and it is replaced.
//!   - `POST /companion/report` `{ "arc": { "state", "title", "artist",
//!     "album", "positionMs", "durationMs" } | null, "livingQueue": bool? }`:
//!     what ARC is doing (sent while it plays, and now and then when not).
//!     `livingQueue`, when given, is the phone's own switch, and changes the
//!     setting. Answers `{ "livingQueue": "off" | "hide", "on": bool,
//!     "home": { "zone", "title", "artist", "seconds" } | null }`: the
//!     setting, whether the living queue is on in Toasted at all, and what
//!     is playing at home.
//!
//! Paired phones are kept in `companion-devices.json` (each with its token);
//! on the Living Queue page each is a zone with the id `arc:<device id>`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

pub const PORT: u16 = 58422;
const CODE_LIFE: Duration = Duration::from_secs(600);
const WRONG_TRIES: u32 = 5;
/// A phone not heard from for this long is shown as away.
const ONLINE: Duration = Duration::from_secs(30);
const MAX_REQUEST: usize = 64 * 1024;

/// A paired phone, as kept on disk.
#[derive(Clone, Serialize, Deserialize)]
struct Device {
    id: String,
    name: String,
    token: String,
    paired: u64,
}

/// What a phone last said about ARC.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ArcState {
    /// "playing", "paused", "buffering", "stopped" or "none".
    pub state: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub position_ms: i64,
    pub duration_ms: i64,
}

struct Heard {
    at: Instant,
    /// When it was, for the page ("last seen").
    unix: u64,
    arc: Option<ArcState>,
}

#[derive(Default)]
struct Pairing {
    code: Option<(String, Instant)>,
    wrong: u32,
}

static PAIRING: Mutex<Option<Pairing>> = Mutex::new(None);
static HEARD: Mutex<Option<HashMap<String, Heard>>> = Mutex::new(None);
/// Things for a phone to do to ARC ("pause"), handed over with the answer
/// to its next report (every second while ARC plays).
static COMMANDS: Mutex<Option<HashMap<String, Vec<String>>>> = Mutex::new(None);
/// The paired phones' names, by device id (read from disk once, kept up to date).
static NAMES: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// ARC on one phone, as the living queue sees it.
#[derive(Clone)]
pub struct ArcNow {
    /// `arc:<device id>`.
    pub zone_id: String,
    pub name: String,
    pub online: bool,
    pub state: String,
    pub title: String,
    pub artist: String,
    /// The position at `at`, when the report came.
    pub position_ms: i64,
    pub at: Instant,
}

impl ArcNow {
    pub fn playing(&self) -> bool {
        self.online && self.state == "playing"
    }

    /// Where ARC is (was) at `when`, in seconds: carried on from the report while it plays.
    pub fn second_at(&self, when: Instant) -> f64 {
        let mut ms = self.position_ms as f64;
        if self.state == "playing" {
            ms += when.saturating_duration_since(self.at).as_millis() as f64;
        }
        ms / 1000.0
    }
}

/// Every phone that has been heard from since Toasted started, with what
/// ARC was last doing.
pub fn arcs() -> Vec<ArcNow> {
    let names = NAMES.lock().unwrap().clone().unwrap_or_default();
    let heard = HEARD.lock().unwrap();
    let Some(heard) = heard.as_ref() else { return Vec::new() };
    heard
        .iter()
        .filter(|(id, _)| names.contains_key(*id))
        .map(|(id, h)| {
            let arc = h.arc.clone().unwrap_or_default();
            ArcNow {
                zone_id: format!("arc:{id}"),
                name: names.get(id).cloned().unwrap_or_default(),
                online: h.at.elapsed() < ONLINE,
                state: if arc.title.is_empty() { "none".into() } else { arc.state },
                title: arc.title,
                artist: arc.artist,
                position_ms: arc.position_ms,
                at: h.at,
            }
        })
        .collect()
}

/// Has a phone do something to ARC ("play", "pause", "playpause", "next",
/// "previous", "seek:<ms>"): handed over at once to the phone's waiting
/// request (`/companion/wait`), or with its next report.
pub fn command(zone_id: &str, what: &str) {
    let Some(id) = zone_id.strip_prefix("arc:") else { return };
    let mut all = COMMANDS.lock().unwrap();
    let list = all.get_or_insert_with(HashMap::new).entry(id.to_string()).or_default();
    // (one pause is enough; a seek replaces an older one)
    if what.starts_with("seek:") {
        list.retain(|w| !w.starts_with("seek:"));
    } else if what == "pause" || what == "play" {
        list.retain(|w| w != "pause" && w != "play" && w != "playpause");
    }
    list.push(what.to_string());
}

fn take_commands(device: &str) -> Vec<String> {
    COMMANDS.lock().unwrap().as_mut().and_then(|m| m.remove(device)).unwrap_or_default()
}

/// Album art for ARC's song, looked up in Roon by title and artist (ARC's own
/// art lives on the phone): the image key, once found.
static ART: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);

fn art_for(app: &AppHandle, title: &str, artist: &str) -> Option<String> {
    if title.is_empty() {
        return None;
    }
    let key = format!("{}\u{1}{}", title.to_lowercase(), artist.to_lowercase());
    {
        let mut art = ART.lock().unwrap();
        let map = art.get_or_insert_with(HashMap::new);
        if let Some(found) = map.get(&key) {
            return found.clone();
        }
        map.insert(key.clone(), None);
    }
    let Some(client) = app.state::<crate::library::Library>().client() else { return None };
    let (app, title, artist) = (app.clone(), title.to_string(), artist.to_string());
    tauri::async_runtime::spawn(async move {
        let first = artist.split([',', '/', '&']).next().unwrap_or("").trim().to_string();
        let Ok(hits) = client.search(format!("{title} {first}").trim(), 15).await else { return };
        let found = hits
            .tracks
            .into_iter()
            .find(|t| t.title.trim().eq_ignore_ascii_case(title.trim()) && (first.is_empty() || t.artist.to_lowercase().contains(&first.to_lowercase())))
            .and_then(|t| t.image_key);
        if found.is_some() {
            ART.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, found);
            push_zones(&app);
        }
    });
    None
}

/// ARC on each phone that can be reached, as a zone for the player and the
/// zone picker.
fn arc_zones(app: &AppHandle) -> Vec<crate::zones::ZoneView> {
    let names = NAMES.lock().unwrap().clone().unwrap_or_default();
    let heard: Vec<(String, Option<ArcState>, Instant)> = {
        let heard = HEARD.lock().unwrap();
        heard.as_ref().map(|m| m.iter().filter(|(_, h)| h.at.elapsed() < ONLINE).map(|(id, h)| (id.clone(), h.arc.clone(), h.at)).collect()).unwrap_or_default()
    };
    heard
        .into_iter()
        .filter_map(|(id, arc, at)| {
            let name = names.get(&id)?;
            let arc = arc.unwrap_or_default();
            let state: &'static str = match arc.state.as_str() {
                "playing" => "playing",
                "paused" => "paused",
                "buffering" => "loading",
                _ => "stopped",
            };
            let has_song = !arc.title.is_empty();
            let mut ms = arc.position_ms as f64;
            if state == "playing" {
                ms += at.elapsed().as_millis() as f64;
            }
            Some(crate::zones::ZoneView {
                zone_id: format!("arc:{id}"),
                name: format!("ARC on {name}"),
                state,
                now_playing: has_song.then(|| crate::zones::NowPlayingView {
                    title: arc.title.clone(),
                    artist: arc.artist.clone(),
                    artist_full: arc.artist.clone(),
                    album: arc.album.clone(),
                    image_key: art_for(app, &arc.title, &arc.artist),
                    length: (arc.duration_ms > 0).then(|| arc.duration_ms as f64 / 1000.0),
                }),
                seek_position: has_song.then_some(ms / 1000.0),
                shuffle: false,
                loop_mode: "disabled",
                auto_radio: false,
                can_play: has_song,
                can_pause: has_song,
                can_next: has_song,
                can_previous: has_song,
                can_seek: has_song,
                queue_items_remaining: None,
                queue_time_remaining: None,
                outputs: Vec::new(),
            })
        })
        .collect()
}

fn push_zones(app: &AppHandle) {
    app.state::<crate::zones::Zones>().set_arcs(arc_zones(app));
}

fn remember_names(devices: &[Device]) {
    *NAMES.lock().unwrap() = Some(devices.iter().map(|d| (d.id.clone(), d.name.clone())).collect());
}

/// A paired phone, for the Living Queue pages.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    /// The zone id it goes by in the living queue settings: `arc:<id>`.
    pub zone_id: String,
    pub name: String,
    pub online: bool,
    /// Unix seconds it was last heard from; 0 for never.
    pub last_seen: u64,
    pub arc: Option<ArcState>,
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).expect("the system's random numbers");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn pc_name() -> String {
    std::env::var("COMPUTERNAME").ok().filter(|n| !n.is_empty()).unwrap_or_else(|| "this PC".into())
}

fn devices_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("companion-devices.json"))
}

fn load_devices(app: &AppHandle) -> Vec<Device> {
    devices_path(app).and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_devices(app: &AppHandle, devices: &[Device]) -> Result<(), String> {
    remember_names(devices);
    let path = devices_path(app).ok_or("Couldn't find the app data folder.")?;
    let bytes = serde_json::to_vec_pretty(devices).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

fn zone_id(device: &Device) -> String {
    format!("arc:{}", device.id)
}

/// The paired phones and what each last said (for the Living Queue pages).
pub fn devices(app: &AppHandle) -> Vec<DeviceView> {
    if preview_soon() {
        return Vec::new();
    }
    let heard = HEARD.lock().unwrap();
    load_devices(app)
        .iter()
        .map(|d| {
            let h = heard.as_ref().and_then(|m| m.get(&d.id));
            DeviceView {
                zone_id: zone_id(d),
                name: d.name.clone(),
                online: h.is_some_and(|h| h.at.elapsed() < ONLINE),
                last_seen: h.map(|h| h.unix).unwrap_or(0),
                arc: h.and_then(|h| h.arc.clone()),
            }
        })
        .collect()
}

fn tell_page(app: &AppHandle) {
    let _ = app.emit("companion-devices", devices(app));
}

/// Whether phones can be paired: dev builds only while the companion app is unreleased ("Coming soon" on the
/// Living Queue pages). A release still serves phones paired before.
pub fn open() -> bool {
    cfg!(debug_assertions) && !preview_soon()
}

/// Dev switch: the pages as a new user of a release sees them (no pairing, no phones listed; phones paired
/// before still work). On while a file named `companion-soon` is in the app data folder, or with
/// TOASTED_COMPANION_SOON=1 set.
fn preview_soon() -> bool {
    std::env::var_os("TOASTED_COMPANION_SOON").is_some() || SOON_FILE.lock().unwrap().as_ref().is_some_and(|f| f.exists())
}

/// Where the dev switch's file goes (set in start).
static SOON_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Starts listening (in setup); runs for the life of the app.
pub fn start(app: &AppHandle) {
    let paired = load_devices(app);
    remember_names(&paired);
    *SOON_FILE.lock().unwrap() = app.path().app_data_dir().ok().map(|d| d.join("companion-soon"));
    if cfg!(debug_assertions) {
        println!("[companion] pairing {} (dev build{})", if open() { "open" } else { "shown as Coming soon" }, if preview_soon() { ", the Coming soon preview is on" } else { "" });
    }
    if !open() && paired.is_empty() {
        // (nothing to listen for: no phone can pair, so no network listener, and no firewall question)
        println!("[companion] coming soon: not listening (no phone paired)");
        return;
    }
    let tcp = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match TcpListener::bind(("0.0.0.0", PORT)).await {
                Ok(listener) => {
                    println!("[companion] listening on port {PORT}");
                    serve(&tcp, listener).await
                }
                Err(e) => println!("[companion] can't listen on port {PORT} ({e}), trying again in 30 s"),
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
    tauri::async_runtime::spawn(async move {
        loop {
            match UdpSocket::bind(("0.0.0.0", PORT)).await {
                Ok(socket) => {
                    let mut buf = [0u8; 256];
                    while let Ok((n, from)) = socket.recv_from(&mut buf).await {
                        if buf[..n].starts_with(b"ROON-TOASTED?") {
                            let _ = socket.send_to(hello().to_string().as_bytes(), from).await;
                        }
                    }
                }
                Err(e) => println!("[companion] can't listen for phones on UDP port {PORT} ({e}), trying again in 30 s"),
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
    // (the page's "away" lines, as phones go quiet)
    let ticker = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut was: Vec<bool> = Vec::new();
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let now: Vec<bool> = devices(&ticker).iter().map(|d| d.online).collect();
            if now != was {
                was = now;
                tell_page(&ticker);
                push_zones(&ticker);
            }
        }
    });
}

fn hello() -> serde_json::Value {
    json!({ "app": "Roon: Toasted", "name": pc_name(), "port": PORT, "version": env!("CARGO_PKG_VERSION") })
}

async fn serve(app: &AppHandle, listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((stream, from)) => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    // (long enough for a phone's waiting request, 25 s)
                    let _ = tokio::time::timeout(Duration::from_secs(30), answer(&app, stream, from)).await;
                });
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).filter_map(|l| l.split_once(':')).find(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim())
}

async fn answer(app: &AppHandle, mut stream: TcpStream, from: std::net::SocketAddr) -> std::io::Result<()> {
    let mut request = Vec::new();
    let mut chunk = [0u8; 2048];
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
    let wanted = header(&head, "content-length").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0).min(MAX_REQUEST);
    if let Some(start) = header_end {
        while request.len() - start < wanted {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
        }
    }
    let body = header_end.map(|start| &request[start..]).unwrap_or(&[]);
    let mut first = head.lines().next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default();
    let path = first.next().unwrap_or_default().split('?').next().unwrap_or_default();
    let token = header(&head, "authorization").and_then(|v| v.strip_prefix("Bearer ")).map(str::trim).unwrap_or("");

    let (status, reply) = match (method, path) {
        ("GET", "/companion/hello") => ("200 OK", hello()),
        ("POST", "/companion/pair") => pair(app, body, from),
        ("POST", "/companion/report") => report(app, body, token),
        ("GET", "/companion/wait") => wait(app, token).await,
        _ => ("404 Not Found", json!({ "error": "not found" })),
    };
    let reply = reply.to_string();
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        reply.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(reply.as_bytes()).await?;
    stream.shutdown().await
}

#[derive(Deserialize)]
struct PairRequest {
    code: String,
    #[serde(default)]
    device: String,
}

fn pair(app: &AppHandle, body: &[u8], from: std::net::SocketAddr) -> (&'static str, serde_json::Value) {
    let Ok(request) = serde_json::from_slice::<PairRequest>(body) else {
        return ("400 Bad Request", json!({ "error": "expected { code, device }" }));
    };
    {
        let mut pairing = PAIRING.lock().unwrap();
        let p = pairing.get_or_insert_with(Pairing::default);
        let good = p.code.as_ref().is_some_and(|(code, made)| made.elapsed() < CODE_LIFE && *code == request.code.trim().replace(' ', ""));
        if !good {
            p.wrong += 1;
            if p.wrong >= WRONG_TRIES {
                p.code = None;
                p.wrong = 0;
                println!("[companion] {WRONG_TRIES} wrong pairing codes (the last from {}); the code was dropped", from.ip());
            }
            return ("403 Forbidden", json!({ "error": "That code isn't right, or it has run out. Make a new one on the Living Queue page in Toasted." }));
        }
        p.code = None;
        p.wrong = 0;
    }
    let name = request.device.trim();
    let device = Device {
        id: random_hex(4),
        name: if name.is_empty() { "Phone".into() } else { name.chars().take(60).collect() },
        token: random_hex(24),
        paired: now_unix(),
    };
    let mut all = load_devices(app);
    all.push(device.clone());
    if let Err(e) = save_devices(app, &all) {
        return ("500 Internal Server Error", json!({ "error": e }));
    }
    println!("[companion] \"{}\" paired from {}", device.name, from.ip());
    // (it takes part as Hide Current from the start: pairing it is asking for it)
    set_mode(app, &device, true);
    tell_page(app);
    ("200 OK", json!({ "id": device.id, "token": device.token, "name": pc_name() }))
}

/// Puts a phone's ARC in (Hide Current) or out of the living queue.
fn set_mode(app: &AppHandle, device: &Device, on: bool) {
    let Some(path) = crate::settings::path(app) else { return };
    let settings = crate::settings::load(&path);
    let id = zone_id(device);
    let mut zones = settings.living_queue_zones.clone();
    let mut names = settings.living_queue_names.clone();
    if on {
        zones.insert(id.clone(), "hide".into());
        names.insert(id, device.name.clone());
    } else {
        zones.remove(&id);
        names.remove(&id);
    }
    if zones != settings.living_queue_zones {
        let _ = crate::settings::update_settings(app.clone(), json!({ "livingQueueZones": zones, "livingQueueNames": names }), Some(true));
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    #[serde(default)]
    arc: Option<ArcState>,
    #[serde(default)]
    living_queue: Option<bool>,
}

fn report(app: &AppHandle, body: &[u8], token: &str) -> (&'static str, serde_json::Value) {
    let Some(device) = load_devices(app).into_iter().find(|d| !token.is_empty() && d.token == token) else {
        return ("401 Unauthorized", json!({ "error": "This phone isn't paired with Toasted (any more). Pair it again." }));
    };
    let Ok(request) = serde_json::from_slice::<Report>(body) else {
        return ("400 Bad Request", json!({ "error": "expected { arc, livingQueue }" }));
    };
    if let Some(on) = request.living_queue {
        set_mode(app, &device, on);
    }
    let (changed, moved) = {
        let mut heard = HEARD.lock().unwrap();
        let map = heard.get_or_insert_with(HashMap::new);
        let before = map.get(&device.id).map(|h| (h.at.elapsed() < ONLINE, h.arc.as_ref().map(|a| (a.state.clone(), a.title.clone(), a.position_ms / 1000))));
        let after = (true, request.arc.as_ref().map(|a| (a.state.clone(), a.title.clone(), a.position_ms / 1000)));
        let song = |a: &Option<(String, String, i64)>| a.as_ref().map(|(s, t, _)| (s.clone(), t.clone()));
        let moved = before.as_ref().is_some_and(|b| b.0 == after.0 && song(&b.1) == song(&after.1));
        map.insert(device.id.clone(), Heard { at: Instant::now(), unix: now_unix(), arc: request.arc.clone() });
        (before.as_ref() != Some(&after), moved)
    };
    if changed {
        tell_page(app);
    }
    // ARC as a zone: a new song or state goes out whole, a position on its own
    if changed && moved {
        if let Some(arc) = &request.arc {
            app.state::<crate::zones::Zones>().arc_seek(&zone_id(&device), arc.position_ms as f64 / 1000.0);
        }
    } else if changed {
        push_zones(app);
    }
    let settings = crate::settings::path(app).map(|p| crate::settings::load(&p)).unwrap_or_default();
    let mode = settings.living_queue_zones.get(&zone_id(&device)).cloned().unwrap_or_else(|| "off".into());
    let commands = take_commands(&device.id);
    if !commands.is_empty() {
        println!("[companion] \"{}\" told: {}", device.name, commands.join(", "));
    }
    ("200 OK", json!({
        "livingQueue": mode,
        "on": settings.living_queue && settings.library_controls,
        "home": home(app, &settings),
        // (what a handoff to ARC carries on from: the zone here that leads, playing or paused)
        "leader": crate::livequeue::leader_now(),
        "commands": commands,
    }))
}

/// A phone's waiting request: answered as soon as there is something for it
/// to do to ARC, or after 25 s with nothing.
async fn wait(app: &AppHandle, token: &str) -> (&'static str, serde_json::Value) {
    let Some(device) = load_devices(app).into_iter().find(|d| !token.is_empty() && d.token == token) else {
        return ("401 Unauthorized", json!({ "error": "This phone isn't paired with Toasted (any more). Pair it again." }));
    };
    let began = Instant::now();
    while began.elapsed() < Duration::from_secs(25) {
        let commands = take_commands(&device.id);
        if !commands.is_empty() {
            println!("[companion] \"{}\" told at once: {}", device.name, commands.join(", "));
            return ("200 OK", json!({ "commands": commands }));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    ("200 OK", json!({ "commands": [] }))
}

/// What is playing at home: the first zone taking part that plays.
fn home(app: &AppHandle, settings: &crate::settings::AppSettings) -> serde_json::Value {
    let zones = app.state::<crate::zones::Zones>().payload_now().zones;
    zones
        .iter()
        .filter(|z| settings.living_queue_zones.contains_key(&z.zone_id))
        .find(|z| z.state == "playing")
        .and_then(|z| {
            let n = z.now_playing.as_ref()?;
            Some(json!({ "zone": z.name, "title": n.title, "artist": n.artist, "seconds": z.seek_position.unwrap_or(0.0).round() }))
        })
        .unwrap_or(serde_json::Value::Null)
}

/// The paired phones (the Living Queue pages).
#[tauri::command]
pub fn companion_devices(app: AppHandle) -> Vec<DeviceView> {
    devices(&app)
}

/// A pairing code for the Living Queue page: the one in force, or a new one
/// (`fresh`). Answers the code and the seconds it has left.
/// Whether this build pairs phones (see open()): the Living Queue pages show "Coming soon" otherwise.
#[tauri::command]
pub fn companion_open() -> bool {
    open()
}

#[tauri::command]
pub fn companion_pair_code(fresh: bool) -> serde_json::Value {
    let mut pairing = PAIRING.lock().unwrap();
    let p = pairing.get_or_insert_with(Pairing::default);
    let stale = p.code.as_ref().is_none_or(|(_, made)| made.elapsed() >= CODE_LIFE);
    if fresh || stale {
        let mut buf = [0u8; 4];
        getrandom::getrandom(&mut buf).expect("the system's random numbers");
        let n = u32::from_le_bytes(buf) % 1_000_000;
        p.code = Some((format!("{n:06}"), Instant::now()));
        p.wrong = 0;
    }
    let (code, made) = p.code.clone().unwrap();
    json!({ "code": code, "seconds": CODE_LIFE.saturating_sub(made.elapsed()).as_secs(), "pc": pc_name() })
}

/// Forgets a phone: it has to pair again.
#[tauri::command]
pub fn companion_forget(app: AppHandle, zone_id: String) -> Result<Vec<DeviceView>, String> {
    let mut all = load_devices(&app);
    let Some(at) = all.iter().position(|d| format!("arc:{}", d.id) == zone_id) else { return Ok(devices(&app)) };
    let device = all.remove(at);
    save_devices(&app, &all)?;
    set_mode(&app, &device, false);
    if let Some(m) = HEARD.lock().unwrap().as_mut() {
        m.remove(&device.id);
    }
    println!("[companion] \"{}\" forgotten", device.name);
    tell_page(&app);
    push_zones(&app);
    Ok(devices(&app))
}
