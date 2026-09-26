//! Zones, now playing, and playback controls for the UI.
//!
//! While connected, every zone's state is kept here and sent to the pages:
//! - Command `roon_zones` returns `{ zones, selectedZoneId }`.
//! - Event `roon-zones` fires with the same shape whenever a zone changes.
//! - Event `roon-seek` fires about once a second per playing zone with
//!   `{ zoneId, seekPosition }` (kept separate so progress ticks don't
//!   resend everything).
//!
//! Album art is served through the app's own `roonimg` image route, so pages
//! never need the Core's address. In JavaScript:
//! `convertFileSrc(imageKey, "roonimg") + "?width=600&height=600"`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use roon_api::zone::LoopMode;
use roon_api::{
    ControlAction, Core, ImageOptions, MuteAction, PlayState, SeekMode, Transport, VolumeMode,
    Zone, ZoneEvent,
};
use serde::Serialize;
use tauri::http::{Request, Response};
use tauri::{AppHandle, Emitter, Manager, State, UriSchemeResponder};

use crate::settings;

// ---------------------------------------------------------------------------
// What the UI receives
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneView {
    pub zone_id: String,
    pub name: String,
    /// "playing", "paused", "loading" or "stopped"
    pub state: &'static str,
    pub now_playing: Option<NowPlayingView>,
    /// Seconds into the current track.
    pub seek_position: Option<f64>,
    pub shuffle: bool,
    /// "disabled", "loop" (repeat all) or "loop_one" (repeat track)
    pub loop_mode: &'static str,
    /// Roon Radio: keep playing similar music when the queue runs out.
    pub auto_radio: bool,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_next: bool,
    pub can_previous: bool,
    pub can_seek: bool,
    pub queue_items_remaining: Option<u32>,
    /// Seconds of music left in the whole queue.
    pub queue_time_remaining: Option<f64>,
    pub outputs: Vec<OutputView>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NowPlayingView {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub image_key: Option<String>,
    /// Track length in seconds.
    pub length: Option<f64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputView {
    pub output_id: String,
    pub name: String,
    /// None when Roon can't control this output's volume (fixed volume).
    pub volume: Option<VolumeView>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeView {
    /// "number" or "db" (how Roon wants the value shown)
    pub kind: String,
    pub min: f64,
    pub max: f64,
    pub value: f64,
    pub step: f64,
    pub is_muted: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZonesPayload {
    /// Sorted by name.
    pub zones: Vec<ZoneView>,
    /// The zone the Toaster should show: the user's pick if it exists,
    /// otherwise the first playing zone, otherwise the first zone.
    pub selected_zone_id: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SeekPayload {
    zone_id: String,
    seek_position: Option<f64>,
}

fn play_state(state: PlayState) -> &'static str {
    match state {
        PlayState::Playing => "playing",
        PlayState::Paused => "paused",
        PlayState::Loading => "loading",
        PlayState::Stopped => "stopped",
    }
}

fn zone_view(zone: &Zone) -> ZoneView {
    let now_playing = zone.now_playing.as_ref().map(|np| {
        // Roon sends the same info at three detail levels; use the richest one available.
        let (title, artist, album) = if let Some(three) = &np.three_line {
            (
                three.line1.clone(),
                three.line2.clone().unwrap_or_default(),
                three.line3.clone().unwrap_or_default(),
            )
        } else if let Some(two) = &np.two_line {
            (two.line1.clone(), two.line2.clone().unwrap_or_default(), String::new())
        } else {
            (np.one_line.line1.clone(), String::new(), String::new())
        };
        NowPlayingView {
            title,
            artist,
            album,
            image_key: np.image_key.clone(),
            length: np.length,
        }
    });

    let (shuffle, loop_mode, auto_radio) = match &zone.settings {
        Some(s) => (
            s.shuffle,
            match s.r#loop {
                LoopMode::Loop => "loop",
                LoopMode::LoopOne => "loop_one",
                LoopMode::Disabled => "disabled",
            },
            s.auto_radio,
        ),
        None => (false, "disabled", false),
    };

    ZoneView {
        zone_id: zone.zone_id.clone(),
        name: zone.display_name.clone(),
        state: play_state(zone.state),
        seek_position: zone
            .seek_position
            .or_else(|| zone.now_playing.as_ref().and_then(|np| np.seek_position)),
        now_playing,
        shuffle,
        loop_mode,
        auto_radio,
        can_play: zone.is_play_allowed,
        can_pause: zone.is_pause_allowed,
        can_next: zone.is_next_allowed,
        can_previous: zone.is_previous_allowed,
        can_seek: zone.is_seek_allowed,
        queue_items_remaining: zone.queue_items_remaining,
        queue_time_remaining: zone.queue_time_remaining,
        outputs: zone
            .outputs
            .iter()
            .map(|o| OutputView {
                output_id: o.output_id.clone(),
                name: o.display_name.clone(),
                volume: o.volume.as_ref().map(|v| VolumeView {
                    kind: v.volume_type.clone(),
                    min: v.min,
                    max: v.max,
                    value: v.value,
                    step: v.step,
                    is_muted: v.is_muted.unwrap_or(false),
                }),
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Inner {
    core: Option<Core>,
    zones: BTreeMap<String, ZoneView>,
    /// The user's explicit pick (saved in settings.json).
    picked_zone_id: Option<String>,
    /// The zone queue.rs is currently following.
    queue_zone_id: Option<String>,
}

/// Zone state, stored in Tauri's managed state.
///
/// Registered with the app before any window exists (so a page asking for
/// zones at startup always gets an answer); `init()` connects it to the app
/// and loads the saved zone pick a moment later.
#[derive(Default)]
pub struct Zones {
    inner: Mutex<Inner>,
    settings_path: OnceLock<PathBuf>,
    app: OnceLock<AppHandle>,
}

impl Zones {
    pub fn init(&self, app: &AppHandle) {
        let _ = self.app.set(app.clone());
        if let Some(path) = settings::path(app) {
            let picked = settings::load(&path).selected_zone_id;
            self.inner.lock().unwrap().picked_zone_id = picked;
            let _ = self.settings_path.set(path);
        }
    }

    fn payload(inner: &Inner) -> ZonesPayload {
        let mut zones: Vec<ZoneView> = inner.zones.values().cloned().collect();
        zones.sort_by_key(|z| z.name.to_lowercase());

        let selected_zone_id = inner
            .picked_zone_id
            .clone()
            .filter(|id| inner.zones.contains_key(id))
            .or_else(|| {
                zones
                    .iter()
                    .find(|z| z.state == "playing")
                    .or(zones.first())
                    .map(|z| z.zone_id.clone())
            });

        ZonesPayload {
            zones,
            selected_zone_id,
        }
    }

    pub fn payload_now(&self) -> ZonesPayload {
        Self::payload(&self.inner.lock().unwrap())
    }

    fn update(&self, change: impl FnOnce(&mut Inner)) {
        let (payload, queue_change) = {
            let mut inner = self.inner.lock().unwrap();
            change(&mut inner);
            let payload = Self::payload(&inner);
            // Point the queue at the selected zone whenever that changes.
            let queue_change = if payload.selected_zone_id != inner.queue_zone_id {
                inner.queue_zone_id = payload.selected_zone_id.clone();
                Some((inner.core.clone(), payload.selected_zone_id.clone()))
            } else {
                None
            };
            (payload, queue_change)
        };
        if let Some(app) = self.app.get() {
            let _ = app.emit("roon-zones", &payload);
            if let Some((core, zone_id)) = queue_change {
                crate::queue::follow(app, core, zone_id);
            }
        }
    }

    /// The connected Core, if any (used by browse.rs).
    pub fn core(&self) -> Option<Core> {
        self.inner.lock().unwrap().core.clone()
    }

    fn transport(&self) -> Result<Transport, String> {
        self.inner
            .lock()
            .unwrap()
            .core
            .as_ref()
            .map(|core| core.transport())
            .ok_or_else(|| "Not connected to Roon.".to_string())
    }
}

/// Called when a Core connects (or reconnects): starts following its zones.
pub fn attach(app: &AppHandle, core: Core) {
    let zones = app.state::<Zones>();
    zones.update(|inner| {
        inner.core = Some(core.clone());
        inner.zones.clear();
    });
    // The zones were just cleared, so the queue stopped following; forget which
    // zone it followed so the first zone update re-subscribes on the new connection.
    zones.inner.lock().unwrap().queue_zone_id = None;
    tauri::async_runtime::spawn(watch(app.clone(), core));
}

/// Called when the connection drops: zones are unknown until it's back.
pub fn detach(app: &AppHandle) {
    app.state::<Zones>().update(|inner| {
        inner.core = None;
        inner.zones.clear();
    });
}

async fn watch(app: AppHandle, core: Core) {
    let mut events = match core.transport().subscribe_zones().await {
        Ok(rx) => rx,
        Err(e) => {
            eprintln!("[roon] couldn't subscribe to zones: {e}");
            return;
        }
    };
    let zones = app.state::<Zones>();

    while let Some(event) = events.recv().await {
        match event {
            ZoneEvent::Initial(list) => {
                println!("[roon] {} zone(s) available", list.len());
                zones.update(|inner| {
                    inner.zones = list.iter().map(|z| (z.zone_id.clone(), zone_view(z))).collect();
                });
            }
            ZoneEvent::Added(list) | ZoneEvent::Changed(list) => {
                zones.update(|inner| {
                    for z in &list {
                        inner.zones.insert(z.zone_id.clone(), zone_view(z));
                    }
                });
            }
            ZoneEvent::Removed(ids) => {
                zones.update(|inner| {
                    for id in &ids {
                        inner.zones.remove(id);
                    }
                });
            }
            ZoneEvent::Seeked(seeks) => {
                // Keep stored positions fresh without resending every zone.
                {
                    let mut inner = zones.inner.lock().unwrap();
                    for s in &seeks {
                        if let Some(z) = inner.zones.get_mut(&s.zone_id) {
                            z.seek_position = s.seek_position;
                        }
                    }
                }
                for s in seeks {
                    let _ = app.emit(
                        "roon-seek",
                        SeekPayload {
                            zone_id: s.zone_id,
                            seek_position: s.seek_position,
                        },
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Commands the UI can call
// ---------------------------------------------------------------------------

/// All zones plus which one the Toaster should show.
#[tauri::command]
pub fn roon_zones(zones: State<'_, Zones>) -> ZonesPayload {
    zones.payload_now()
}

/// Makes a zone the one the Toaster controls, and remembers it.
#[tauri::command]
pub fn roon_select_zone(zones: State<'_, Zones>, zone_id: String) -> Result<(), String> {
    if let Some(path) = zones.settings_path.get() {
        let mut saved = settings::load(path);
        saved.selected_zone_id = Some(zone_id.clone());
        settings::save(path, &saved).map_err(|e| format!("Couldn't save the zone: {e}"))?;
    }
    zones.update(|inner| inner.picked_zone_id = Some(zone_id));
    Ok(())
}

/// Playback: "play", "pause", "playpause", "stop", "next" or "previous".
#[tauri::command]
pub async fn roon_control(
    zones: State<'_, Zones>,
    zone_id: String,
    action: String,
) -> Result<(), String> {
    let action = match action.as_str() {
        "play" => ControlAction::Play,
        "pause" => ControlAction::Pause,
        "playpause" => ControlAction::PlayPause,
        "stop" => ControlAction::Stop,
        "next" => ControlAction::Next,
        "previous" => ControlAction::Previous,
        other => return Err(format!("Unknown action: {other}")),
    };
    zones
        .transport()?
        .control(&zone_id, action)
        .await
        .map_err(|e| e.to_string())
}

/// Jumps to a position (in seconds) in the current track.
#[tauri::command]
pub async fn roon_seek(zones: State<'_, Zones>, zone_id: String, seconds: f64) -> Result<(), String> {
    zones
        .transport()?
        .seek(&zone_id, SeekMode::Absolute, seconds.max(0.0).round() as i64)
        .await
        .map_err(|e| e.to_string())
}

/// Shuffle, repeat ("disabled", "loop", "loop_one") and Roon Radio.
/// Leave out (or pass null for) anything that shouldn't change.
#[tauri::command]
pub async fn roon_zone_settings(
    zones: State<'_, Zones>,
    zone_id: String,
    shuffle: Option<bool>,
    loop_mode: Option<String>,
    auto_radio: Option<bool>,
) -> Result<(), String> {
    if let Some(mode) = &loop_mode {
        if !matches!(mode.as_str(), "disabled" | "loop" | "loop_one") {
            return Err(format!("Unknown repeat mode: {mode}"));
        }
    }
    zones
        .transport()?
        .change_settings(&zone_id, shuffle, loop_mode.as_deref(), auto_radio)
        .await
        .map_err(|e| e.to_string())
}

/// Sets an output's volume to an exact value (within its min/max).
#[tauri::command]
pub async fn roon_set_volume(
    zones: State<'_, Zones>,
    output_id: String,
    value: f64,
) -> Result<(), String> {
    zones
        .transport()?
        .change_volume(&output_id, VolumeMode::Absolute, value)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn roon_mute(zones: State<'_, Zones>, output_id: String, muted: bool) -> Result<(), String> {
    let action = if muted { MuteAction::Mute } else { MuteAction::Unmute };
    zones
        .transport()?
        .mute(&output_id, action)
        .await
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Album art: the `roonimg` image route
// ---------------------------------------------------------------------------

/// Serves `roonimg://localhost/<image_key>?width=..&height=..` (on Windows the
/// webview spells it `http://roonimg.localhost/...`) by fetching the image from
/// the connected Core. Image keys never change, so responses cache for a year.
pub fn image_request(app: &AppHandle, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let core = app
        .try_state::<Zones>()
        .and_then(|z| z.inner.lock().unwrap().core.clone());
    let key = request.uri().path().trim_start_matches('/').to_string();
    let query = request.uri().query().unwrap_or("").to_string();

    tauri::async_runtime::spawn(async move {
        let reply = |status: u16, content_type: &str, body: Vec<u8>| {
            Response::builder()
                .status(status)
                .header("Content-Type", content_type)
                .header("Cache-Control", "max-age=31536000")
                .header("Access-Control-Allow-Origin", "*")
                .body(body)
                .unwrap()
        };

        // Image keys are plain letters/numbers; refuse anything else.
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            responder.respond(reply(400, "text/plain", b"bad image key".to_vec()));
            return;
        }
        let Some(core) = core else {
            responder.respond(reply(503, "text/plain", b"not connected to Roon".to_vec()));
            return;
        };

        let mut opts = ImageOptions {
            format: Some("image/jpeg".into()),
            ..Default::default()
        };
        for pair in query.split('&') {
            match pair.split_once('=') {
                Some(("width", v)) => opts.width = v.parse().ok().map(|n: u32| n.min(2048)),
                Some(("height", v)) => opts.height = v.parse().ok().map(|n: u32| n.min(2048)),
                Some(("scale", v)) if matches!(v, "fit" | "fill" | "stretch") => {
                    opts.scale = Some(v.into())
                }
                _ => {}
            }
        }
        // Roon needs both dimensions and a scale mode whenever a size is given.
        if opts.width.is_some() || opts.height.is_some() {
            opts.width = opts.width.or(opts.height);
            opts.height = opts.height.or(opts.width);
            opts.scale.get_or_insert_with(|| "fit".into());
        }

        match core.image().get_image(&key, &opts).await {
            Ok(bytes) => responder.respond(reply(200, "image/jpeg", bytes)),
            Err(e) => responder.respond(reply(502, "text/plain", e.to_string().into_bytes())),
        }
    });
}
