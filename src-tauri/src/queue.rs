// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The selected zone's play queue, for the Queue tab.
//!
//! Automatically follows whichever zone the Toaster is showing: zones.rs calls
//! `follow()` whenever the selected zone or the connection changes, and this
//! drops the old subscription and subscribes to the new zone's queue.
//!
//! - Command `roon_queue` returns `{ zoneId, items }`.
//! - Event `roon-queue` fires with the same shape whenever the queue changes.
//! - Command `roon_play_from_here` jumps playback to a queue item.
//!
//! Only the first `MAX_ITEMS` tracks are followed (a shuffled library can queue
//! thousands); the zone's `queueItemsRemaining` / `queueTimeRemaining` give the
//! full totals.

use std::sync::{Mutex, OnceLock};

use roon_api::{Core, QueueChange, QueueEvent, QueueItem};
use serde::Serialize;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::zones::Zones;

/// How many queue items are followed and shown.
const MAX_ITEMS: u32 = 100;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItemView {
    /// Pass to `roon_play_from_here` to jump to this track.
    pub queue_item_id: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub image_key: Option<String>,
    /// Track length in seconds.
    pub length: Option<f64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuePayload {
    /// The zone this queue belongs to (None when no zone is selected).
    pub zone_id: Option<String>,
    pub items: Vec<QueueItemView>,
}

fn item_view(item: QueueItem) -> QueueItemView {
    // Same three detail levels as now playing; use the richest available.
    let (title, artist, album) = if let Some(three) = item.three_line {
        (
            three.line1,
            three.line2.unwrap_or_default(),
            three.line3.unwrap_or_default(),
        )
    } else if let Some(two) = item.two_line {
        (two.line1, two.line2.unwrap_or_default(), String::new())
    } else {
        (item.one_line.line1, String::new(), String::new())
    };
    QueueItemView {
        queue_item_id: item.queue_item_id,
        title,
        artist,
        album,
        image_key: item.image_key,
        length: item.length,
    }
}

#[derive(Default)]
struct Inner {
    zone_id: Option<String>,
    items: Vec<QueueItemView>,
    /// Bumped on every follow(), so a stale task can't touch a newer queue.
    generation: u64,
    /// The current subscription: its task, key, and the Core it's on.
    task: Option<JoinHandle<()>>,
    key: Option<u32>,
    core: Option<Core>,
}

/// Queue state, stored in Tauri's managed state (registered before any window opens).
#[derive(Default)]
pub struct Queue {
    inner: Mutex<Inner>,
    app: OnceLock<AppHandle>,
}

impl Queue {
    pub fn init(&self, app: &AppHandle) {
        let _ = self.app.set(app.clone());
    }

    fn payload(inner: &Inner) -> QueuePayload {
        QueuePayload {
            zone_id: inner.zone_id.clone(),
            items: inner.items.clone(),
        }
    }

    fn emit(&self, payload: &QueuePayload) {
        if let Some(app) = self.app.get() {
            let _ = app.emit("roon-queue", payload);
        }
    }
}

/// Starts following `zone_id`'s queue on `core`, or stops following when
/// either is None. Replaces any previous subscription.
pub fn follow(app: &AppHandle, core: Option<Core>, zone_id: Option<String>) {
    let queue = app.state::<Queue>();

    // Swap in the new zone, keeping the old subscription's details to clean up.
    let (generation, old_task, old_key, old_core, payload) = {
        let mut inner = queue.inner.lock().unwrap();
        inner.generation += 1;
        inner.zone_id = zone_id.clone();
        inner.items.clear();
        let old_task = inner.task.take();
        let old_key = inner.key.take();
        let old_core = std::mem::replace(&mut inner.core, core.clone());
        (inner.generation, old_task, old_key, old_core, Queue::payload(&inner))
    };
    queue.emit(&payload);

    // Stop the old subscription (on the Core it was made on).
    if let Some(task) = old_task {
        task.abort();
    }
    if let (Some(key), Some(old_core)) = (old_key, old_core) {
        tauri::async_runtime::spawn(async move {
            let _ = old_core.transport().unsubscribe_queue(key).await;
        });
    }

    let (Some(core), Some(zone_id)) = (core, zone_id) else {
        return;
    };

    let app = app.clone();
    let task = tauri::async_runtime::spawn(async move {
        let queue = app.state::<Queue>();
        let (key, mut events) = match core.transport().subscribe_queue(&zone_id, MAX_ITEMS).await {
            Ok(sub) => sub,
            Err(e) => {
                eprintln!("[roon] couldn't subscribe to the queue: {e}");
                return;
            }
        };

        // Remember the key so this subscription can be stopped later,
        // unless the zone already changed while subscribing.
        let still_current = {
            let mut inner = queue.inner.lock().unwrap();
            if inner.generation == generation {
                inner.key = Some(key);
                true
            } else {
                false
            }
        };
        if !still_current {
            let _ = core.transport().unsubscribe_queue(key).await;
            return;
        }

        while let Some(event) = events.recv().await {
            let payload = {
                let mut inner = queue.inner.lock().unwrap();
                if inner.generation != generation {
                    return;
                }
                match event {
                    QueueEvent::Initial(items) => {
                        inner.items = items.into_iter().map(item_view).collect();
                    }
                    QueueEvent::Changed(changes) => {
                        for change in changes {
                            match change {
                                QueueChange::Insert { index, items } => {
                                    let at = index.min(inner.items.len());
                                    let new = items.into_iter().map(item_view);
                                    inner.items.splice(at..at, new);
                                }
                                QueueChange::Remove { index, count } => {
                                    let start = index.min(inner.items.len());
                                    let end = (index + count).min(inner.items.len());
                                    inner.items.drain(start..end);
                                }
                            }
                        }
                    }
                }
                Queue::payload(&inner)
            };
            queue.emit(&payload);
        }
    });

    let mut inner = queue.inner.lock().unwrap();
    if inner.generation == generation {
        inner.task = Some(task);
    } else {
        task.abort();
    }
}

// ---------------------------------------------------------------------------
// Commands the UI can call
// ---------------------------------------------------------------------------

/// The followed zone's queue (first 100 items).
#[tauri::command]
pub fn roon_queue(queue: State<'_, Queue>) -> QueuePayload {
    Queue::payload(&queue.inner.lock().unwrap())
}

/// Starts playing the zone's queue from the given item.
#[tauri::command]
pub async fn roon_play_from_here(
    zones: State<'_, Zones>,
    zone_id: String,
    queue_item_id: u64,
) -> Result<(), String> {
    let core = zones.core().ok_or("Not connected to Roon.")?;
    core.transport()
        .play_from_here(&zone_id, queue_item_id)
        .await
        .map_err(|e| e.to_string())
}
