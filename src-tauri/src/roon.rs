//! Roon connection.
//!
//! Prototype stage: finds the Roon Core on the network, pairs with it, and
//! prints zones and track changes to the dev terminal. Nothing is shown in
//! the UI yet; this is only to prove the roon-api library works with Vulk's Core.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use roon_api::{
    ApiError, Core, FileStateStore, RoonClient, RoonClientBuilder, RoonEvent, Zone, ZoneEvent,
};
use tauri::{AppHandle, Manager};
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

/// Starts the Roon connection in the background. Returns immediately.
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
    let version = app.package_info().version.to_string();

    tauri::async_runtime::spawn(async move {
        if let Err(e) = run(state_path, version).await {
            eprintln!("[roon] connection stopped: {e}");
        }
    });
}

async fn run(state_path: PathBuf, version: String) -> Result<(), ApiError> {
    if let Some(dir) = state_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    println!("[roon] pairing file: {}", state_path.display());

    // How the app introduces itself in Roon > Settings > Extensions.
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
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(RoonEvent::CorePaired(core)) => {
                    println!(
                        "[roon] connected to \"{}\" (Roon {})",
                        core.display_name(),
                        core.display_version()
                    );
                    // Each (re)connection gets its own zone watcher; the old one ends with its connection.
                    tauri::async_runtime::spawn(watch_zones(core));
                }
                Ok(RoonEvent::CoreLost { .. }) => {
                    println!("[roon] lost the connection to the Core, reconnecting...");
                }
                Ok(_) => {}
                // Fell behind on events; skip ahead rather than stop.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });

    connect_to_one_core(&client).await
}

/// Finds Cores on the network and connects to exactly one of them.
///
/// Networks can have more than one Core answering (an old or idle install,
/// for example), so the app decides which one to use instead of retrying
/// every Core it hears from:
/// - Already paired: only that Core is used. Others are mentioned once and ignored.
/// - Not paired yet: every Core found is tried at once, and whichever one the
///   user enables "Roon: Toasted" on first becomes the paired Core.
///
/// Once connected, discovery stops and roon-api's own reconnect (with
/// backoff) keeps that one connection alive.
async fn connect_to_one_core(client: &Arc<RoonClient>) -> Result<(), ApiError> {
    let paired = client.pairing().paired_core_id().await;
    match &paired {
        Some(id) => println!("[roon] looking for the paired Core ({id})..."),
        None => println!("[roon] searching the network for a Roon Core..."),
    }

    let (discovery, mut found) = roon_sood::SoodDiscovery::start().await?;
    let (connected_tx, mut connected_rx) = tokio::sync::mpsc::channel::<String>(1);
    let mut attempts: HashMap<String, JoinHandle<()>> = HashMap::new();
    let mut ignored: HashSet<String> = HashSet::new();

    let connected_id = loop {
        tokio::select! {
            Some(core_id) = connected_rx.recv() => break core_id,
            result = found.recv() => {
                let core = match result {
                    Ok(core) => core,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => return Err(ApiError::ConnectionClosed),
                };
                let name = core.name.clone().unwrap_or_else(|| "a Roon Core".into());

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
    };

    // Cancel any attempts still waiting on other Cores, then stop searching.
    for (core_id, task) in attempts {
        if core_id != connected_id {
            task.abort();
        }
    }
    discovery.stop().await;
    Ok(())
}

/// Prints every zone once, then any track or play/pause change.
async fn watch_zones(core: Core) {
    let transport = core.transport();
    let mut zone_events = match transport.subscribe_zones().await {
        Ok(rx) => rx,
        Err(e) => {
            eprintln!("[roon] couldn't subscribe to zones: {e}");
            return;
        }
    };

    // Last printed status per zone, so repeated updates don't spam the terminal.
    let mut last_status: HashMap<String, String> = HashMap::new();

    while let Some(event) = zone_events.recv().await {
        match event {
            ZoneEvent::Initial(zones) => {
                println!("[roon] {} zone(s):", zones.len());
                for zone in &zones {
                    let status = zone_status(zone);
                    println!("[roon]   {}: {}", zone.display_name, status);
                    last_status.insert(zone.zone_id.clone(), status);
                }
            }
            ZoneEvent::Added(zones) | ZoneEvent::Changed(zones) => {
                for zone in &zones {
                    let status = zone_status(zone);
                    if last_status.get(&zone.zone_id) != Some(&status) {
                        println!("[roon] {}: {}", zone.display_name, status);
                        last_status.insert(zone.zone_id.clone(), status);
                    }
                }
            }
            ZoneEvent::Removed(zone_ids) => {
                for id in zone_ids {
                    last_status.remove(&id);
                    println!("[roon] a zone was removed ({id})");
                }
            }
            // Playback position ticks roughly every second; ignored for now.
            ZoneEvent::Seeked(_) => {}
        }
    }
}

/// One-line summary, e.g. "Playing | Song - Artist [has album art]".
fn zone_status(zone: &Zone) -> String {
    match &zone.now_playing {
        Some(np) => format!(
            "{:?} | {}{}",
            zone.state,
            np.one_line.line1,
            if np.image_key.is_some() { " [has album art]" } else { "" }
        ),
        None => format!("{:?} | nothing loaded", zone.state),
    }
}
