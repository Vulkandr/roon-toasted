//! Roon connection.
//!
//! Finds Roon Cores on the network, connects to exactly one of them, and keeps
//! a live status (connected Core + every Core on the network) that the UI can
//! read with the `roon_status` command or follow with the `roon-status` event.
//! Zones and playback live in zones.rs, which this hands each connection to.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use roon_api::{ApiError, FileStateStore, RoonClient, RoonClientBuilder, RoonEvent, StateStore};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

/// A Core seen on the network. Cores that stop answering for this long drop off the list.
const CORE_EXPIRY: Duration = Duration::from_secs(150);

// ---------------------------------------------------------------------------
// Status shared with the UI
// ---------------------------------------------------------------------------

/// One Roon Core, as shown in Settings.
#[derive(Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CoreInfo {
    pub core_id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Copy, Serialize, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionState {
    /// Looking for the Core (or waiting for it to be enabled in Roon).
    Searching,
    Connected,
    /// Was connected, lost it, retrying automatically.
    Reconnecting,
}

/// What `roon_status` returns and the `roon-status` event carries.
#[derive(Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RoonStatus {
    pub state: ConnectionState,
    /// The Core this app is set to use (remembered from the last pairing), even
    /// before it's been found on the network. None until the first pairing.
    pub paired_core_id: Option<String>,
    /// The Core this app is paired with (filled in once it's been seen).
    pub connected: Option<CoreInfo>,
    /// Every Core currently answering on the network, including the connected one.
    pub available: Vec<CoreInfo>,
}

struct Inner {
    state: ConnectionState,
    paired_core_id: Option<String>,
    connected: Option<CoreInfo>,
    seen: HashMap<String, (CoreInfo, Instant)>,
    last_sent: Option<RoonStatus>,
}

/// Handle to the Roon status, stored in Tauri's managed state.
pub struct Roon {
    inner: Mutex<Inner>,
    state_path: PathBuf,
    app: AppHandle,
}

impl Roon {
    fn snapshot(inner: &Inner) -> RoonStatus {
        let mut available: Vec<CoreInfo> = inner.seen.values().map(|(c, _)| c.clone()).collect();
        available.sort_by_key(|c| c.name.to_lowercase());
        RoonStatus {
            state: inner.state,
            paired_core_id: inner.paired_core_id.clone(),
            connected: inner.connected.clone(),
            available,
        }
    }

    pub fn status(&self) -> RoonStatus {
        Self::snapshot(&self.inner.lock().unwrap())
    }

    /// Applies a change, then tells the UI (and the dev terminal) if anything actually changed.
    fn update(&self, change: impl FnOnce(&mut Inner)) {
        let status = {
            let mut inner = self.inner.lock().unwrap();
            change(&mut inner);
            let status = Self::snapshot(&inner);
            if inner.last_sent.as_ref() == Some(&status) {
                return;
            }
            inner.last_sent = Some(status.clone());
            status
        };
        print_status(&status);
        let _ = self.app.emit("roon-status", &status);
    }

    fn core_seen(&self, core: CoreInfo) {
        self.update(|inner| {
            if inner.connected.as_ref().is_some_and(|c| c.core_id == core.core_id) {
                inner.connected = Some(core.clone());
            }
            inner.seen.insert(core.core_id.clone(), (core, Instant::now()));
        });
    }

    fn remove_stale_cores(&self) {
        self.update(|inner| inner.seen.retain(|_, (_, last)| last.elapsed() < CORE_EXPIRY));
    }
}

fn print_status(status: &RoonStatus) {
    let current = match &status.connected {
        Some(c) => format!("{:?}, {} at {}", status.state, c.name, c.host),
        None => format!("{:?}", status.state),
    };
    let others: Vec<&str> = status
        .available
        .iter()
        .filter(|c| status.paired_core_id.as_ref() != Some(&c.core_id))
        .map(|c| c.name.as_str())
        .collect();
    if others.is_empty() {
        println!("[roon] status: {current}");
    } else {
        println!("[roon] status: {current} | other Cores: {}", others.join(", "));
    }
}

// ---------------------------------------------------------------------------
// Commands the UI can call
// ---------------------------------------------------------------------------

/// Current connection status and Core list.
#[tauri::command]
pub fn roon_status(roon: State<'_, Roon>) -> RoonStatus {
    roon.status()
}

/// Makes a different Core the paired one, then restarts the app to connect to it.
///
/// roon-api's reconnect loop can't be stopped once it starts, so a quick restart
/// is the clean way to let go of the old Core. This is an async command on
/// purpose: restarting from a background thread lets Tauri shut down fully first
/// (releasing the single-instance lock) before the new copy starts.
#[tauri::command]
pub async fn switch_core(roon: State<'_, Roon>, core_id: String) -> Result<(), String> {
    let status = roon.status();
    if status.connected.as_ref().is_some_and(|c| c.core_id == core_id) {
        return Ok(()); // Already using it.
    }
    let Some(core) = status.available.iter().find(|c| c.core_id == core_id) else {
        return Err("That Core isn't on the network right now.".into());
    };

    FileStateStore::new(roon.state_path.clone())
        .save_paired_core_id(Some(&core_id))
        .map_err(|e| format!("Couldn't save the new Core: {e}"))?;
    println!("[roon] switching to \"{}\", restarting...", core.name);
    roon.app.restart();
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

/// Sets up the Roon status and starts connecting in the background. Returns immediately.
pub fn start(app: &AppHandle) {
    // Pairing approval is saved here so Roon only asks once:
    // %APPDATA%\com.vulkan.roon-toasted\roon-state.json on Windows.
    let state_path = match app.path().app_data_dir() {
        Ok(dir) => dir.join("roon-state.json"),
        Err(e) => {
            eprintln!("[roon] couldn't find the app data folder: {e}");
            return;
        }
    };
    app.manage(Roon {
        inner: Mutex::new(Inner {
            state: ConnectionState::Searching,
            paired_core_id: None,
            connected: None,
            seen: HashMap::new(),
            last_sent: None,
        }),
        state_path: state_path.clone(),
        app: app.clone(),
    });

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = run(app, state_path).await {
            eprintln!("[roon] connection stopped: {e}");
        }
    });
}

async fn run(app: AppHandle, state_path: PathBuf) -> Result<(), ApiError> {
    if let Some(dir) = state_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    println!("[roon] pairing file: {}", state_path.display());

    // How the app introduces itself in Roon > Settings > Extensions.
    let version = app.package_info().version.to_string();
    let client = RoonClientBuilder::new(
        "com.vulkan.roon-toasted",
        "Roon: Toasted",
        &version,
        "Vulkandr",
        "",
    )
    .website("https://github.com/Vulkandr")
    .token_store(FileStateStore::new(state_path))
    .require_transport()
    .require_browse()
    .build()?;
    let client = Arc::new(client);

    // Connection events keep arriving for the life of the app (including
    // automatic reconnects), so they get their own listener.
    let mut events = client.events();
    let events_app = app.clone();
    tauri::async_runtime::spawn(async move {
        let roon = events_app.state::<Roon>();
        loop {
            match events.recv().await {
                Ok(RoonEvent::CorePaired(core)) => {
                    println!(
                        "[roon] connected to \"{}\" (Roon {})",
                        core.display_name(),
                        core.display_version()
                    );
                    let core_id = core.core_id().to_string();
                    let fallback_name = core.display_name().to_string();
                    roon.update(|inner| {
                        inner.state = ConnectionState::Connected;
                        inner.paired_core_id = Some(core_id.clone());
                        inner.connected = Some(match inner.seen.get(&core_id) {
                            Some((info, _)) => info.clone(),
                            None => CoreInfo {
                                core_id,
                                name: fallback_name,
                                host: String::new(),
                                port: 0,
                            },
                        });
                    });
                    // Each (re)connection gets its own zone watcher; the old one ends with its connection.
                    crate::zones::attach(&events_app, core);
                }
                Ok(RoonEvent::CoreLost { .. }) => {
                    roon.update(|inner| inner.state = ConnectionState::Reconnecting);
                    crate::zones::detach(&events_app);
                }
                Ok(_) => {}
                // Fell behind on events; skip ahead rather than stop.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });

    discover_and_connect(&app, &client).await
}

/// Keeps listening for Cores on the network for the life of the app (so the
/// "available" list stays current) and connects to exactly one of them.
///
/// Networks can have more than one Core answering (an old or idle install,
/// for example), so the app decides which one to use instead of retrying
/// every Core it hears from:
/// - Already paired: only that Core is used. Others are listed but left alone.
/// - Not paired yet: every Core found is tried at once, and whichever one the
///   user enables "Roon: Toasted" on first becomes the paired Core.
///
/// Once connected, roon-api's own reconnect (with backoff) keeps that one
/// connection alive; discovery only keeps the list up to date.
async fn discover_and_connect(app: &AppHandle, client: &Arc<RoonClient>) -> Result<(), ApiError> {
    let roon = app.state::<Roon>();
    let paired = client.pairing().paired_core_id().await;
    roon.update(|inner| inner.paired_core_id = paired.clone());
    match &paired {
        Some(id) => println!("[roon] looking for the paired Core ({id})..."),
        None => println!("[roon] searching the network for a Roon Core..."),
    }

    // Keep the handle: dropping it would stop discovery.
    let (_discovery, mut found) = roon_sood::SoodDiscovery::start().await?;
    let (connected_tx, mut connected_rx) = tokio::sync::mpsc::channel::<String>(1);
    let mut attempts: HashMap<String, JoinHandle<()>> = HashMap::new();
    let mut ignored: HashSet<String> = HashSet::new();
    let mut connected = false;
    let mut prune = tokio::time::interval(Duration::from_secs(30));

    loop {
        tokio::select! {
            Some(connected_id) = connected_rx.recv() => {
                // Cancel any attempts still waiting on other Cores.
                for (core_id, task) in attempts.drain() {
                    if core_id != connected_id {
                        task.abort();
                    }
                }
                connected = true;
            }

            _ = prune.tick() => roon.remove_stale_cores(),

            result = found.recv() => {
                let core = match result {
                    Ok(core) => core,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => return Err(ApiError::ConnectionClosed),
                };
                let name = core.name.clone().unwrap_or_else(|| "Roon Core".into());
                roon.core_seen(CoreInfo {
                    core_id: core.core_id.clone(),
                    name: name.clone(),
                    host: core.host.to_string(),
                    port: core.http_port,
                });

                // From here on it's only about connecting.
                if connected {
                    continue;
                }

                // Already paired with a different Core: leave this one alone.
                if paired.as_ref().is_some_and(|id| *id != core.core_id) {
                    if ignored.insert(core.core_id.clone()) {
                        println!(
                            "[roon] ignoring \"{name}\" at {} ({}), it isn't the paired Core",
                            core.host, core.core_id
                        );
                    }
                    continue;
                }

                // Discovery re-announces Cores every ~10 seconds; don't stack up attempts.
                if attempts.get(&core.core_id).is_some_and(|task| !task.is_finished()) {
                    continue;
                }
                let first_try = !attempts.contains_key(&core.core_id);
                if first_try {
                    println!("[roon] found \"{name}\" at {}, registering...", core.host);
                    if paired.is_none() {
                        println!("[roon] (if it stops here, enable \"Roon: Toasted\" in Roon > Settings > Extensions)");
                    }
                }

                let client = client.clone();
                let connected_tx = connected_tx.clone();
                let host = core.host.to_string();
                let port = core.http_port;
                let core_id = core.core_id.clone();
                let task = tokio::spawn(async move {
                    match client.connect(&host, port).await {
                        Ok(_) => {
                            let _ = connected_tx.send(core_id).await;
                        }
                        // Only report the first failure; retries happen quietly.
                        Err(e) if first_try => {
                            eprintln!("[roon] couldn't register with \"{name}\" at {host}: {e} (will keep trying)");
                        }
                        Err(_) => {}
                    }
                });
                attempts.insert(core.core_id.clone(), task);
            }
        }
    }
}
