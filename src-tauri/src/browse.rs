// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Browse and search: Roon's library as lists the pages can navigate.
//!
//! Roon keeps the navigation position on the Core, separately for each
//! "session" name, so Browse and Search each use their own session and keep
//! their place independently (e.g. `session: "browse"` and `session: "search"`).
//!
//! Pages navigate by **path**: the list of items clicked from the top, each
//! step given as `{ index, title }` (its position in that list and its name,
//! so a moved or missing item is caught instead of opening the wrong thing).
//! This makes back/forward simple for the page: going back is the same path
//! minus its last step. Rust remembers where each session is, so a step
//! forward or back costs one call to the Core, and anything else (a jump, or a
//! session that got out of step) is rebuilt from the top.
//!
//! - `roon_browse_path` goes to a path and returns that list (or runs the
//!   action at the end of it: Play Now, Queue, ...). Also used for paging.
//! - `roon_search` runs a search and returns Roon's top hit plus the first few
//!   results of each group (Artists, Albums, ...), for the Search tab.
//! - `roon_browse` / `roon_browse_more`: the original key-based commands.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use roon_api::{Browse, BrowseItem, BrowseOptions, BrowseResult, LoadOptions, LoadResult};
use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::sync::Mutex as AsyncMutex;

use crate::zones::Zones;

/// Items per page when a list is opened or paged through.
const PAGE_SIZE: u32 = 100;

/// Results per group on the search results page.
const SEARCH_PREVIEW: u32 = 5;

/// Hierarchies the pages may use (Roon's "settings" hierarchy is left out on purpose).
const HIERARCHIES: [&str; 8] = [
    "browse",
    "playlists",
    "internet_radio",
    "albums",
    "artists",
    "genres",
    "composers",
    "search",
];

// ---------------------------------------------------------------------------
// What the pages get
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListView {
    pub title: String,
    pub subtitle: Option<String>,
    /// Total number of items in the list (may be more than one page).
    pub count: u32,
    /// How deep this list is (0 = the top of the hierarchy).
    pub level: u32,
    pub image_key: Option<String>,
    pub hint: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemView {
    pub title: String,
    pub subtitle: Option<String>,
    /// Pass back to `roon_browse` to open or run this item.
    pub item_key: Option<String>,
    pub image_key: Option<String>,
    /// Roon's hint for what the item is:
    /// "list" (opens another list), "action" (does something, e.g. Play Now),
    /// "action_list" (opens a list of actions), "header" (a section heading)
    pub hint: Option<String>,
    /// For items that ask for text, like the search box inside Roon's library.
    pub input_prompt: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseView {
    /// "list" (show `list` + `items`), "message" (show `message`),
    /// "none" (done, nothing to show), or "replace_item" / "remove_item".
    pub action: String,
    pub list: Option<ListView>,
    pub items: Vec<ItemView>,
    /// Position of the first item in `items` within the whole list.
    pub offset: u32,
    pub message: Option<String>,
    pub is_error: bool,
    /// For replace_item: the updated item.
    pub item: Option<ItemView>,
}

/// A search result with its position in its list (for building paths).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchItem {
    #[serde(flatten)]
    pub item: ItemView,
    pub index: u32,
}

/// One group of search results, e.g. Albums.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchSection {
    /// "Artists", "Albums", ...
    pub title: String,
    /// Position of the group in the results list: the first path step for
    /// "See all" and for opening any of its results.
    pub index: u32,
    /// How many results the group has in total.
    pub count: u32,
    /// The first few results.
    pub items: Vec<SearchItem>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchView {
    pub query: String,
    /// Roon's best matches, shown above the groups (usually one).
    pub top_hits: Vec<SearchItem>,
    pub sections: Vec<SearchSection>,
}

/// One step of a path: which item was opened from a list.
#[derive(Clone, PartialEq, Deserialize)]
pub struct Step {
    pub index: u32,
    pub title: String,
}

/// Roon marks up names in subtitles as links, like "[[17431674|Kiefer]]"
/// (its own apps show "Kiefer" as a link to the artist). Keeps just the names:
/// "[[1|Tinashe]], [[2|Kiefer]]" -> "Tinashe, Kiefer". Only used for subtitles:
/// titles have to stay exactly as Roon sends them, since paths match on them.
fn plain_text(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        plain.push_str(&rest[..start]);
        let link = &rest[start + 2..];
        let Some(end) = link.find("]]") else {
            // Not really a link: keep it as it is
            plain.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let inside = &link[..end];
        plain.push_str(inside.split_once('|').map_or(inside, |(_, name)| name));
        rest = &link[end + 2..];
    }
    plain.push_str(rest);
    plain
}

fn subtitle_text(subtitle: Option<String>) -> Option<String> {
    subtitle
        .map(|s| plain_text(&s))
        .filter(|s| !s.is_empty())
}

fn item_view(item: BrowseItem) -> ItemView {
    ItemView {
        title: item.title,
        subtitle: subtitle_text(item.subtitle),
        item_key: item.item_key,
        image_key: item.image_key,
        hint: item.hint,
        input_prompt: item.input_prompt.and_then(|p| p.prompt),
    }
}

fn result_view(result: BrowseResult) -> BrowseView {
    BrowseView {
        action: result.action,
        list: None,
        items: Vec::new(),
        offset: 0,
        message: result.message,
        is_error: result.is_error.unwrap_or(false),
        item: result.item.map(item_view),
    }
}

/// Roon's search groups look like `{ title: "Albums", subtitle: "47 Results",
/// hint: "list" }` with no image. Returns the result count for those.
fn group_count(item: &BrowseItem) -> Option<u32> {
    if item.hint.as_deref() != Some("list") || item.image_key.is_some() {
        return None;
    }
    let subtitle = item.subtitle.as_deref()?;
    let number = subtitle
        .strip_suffix(" Results")
        .or_else(|| subtitle.strip_suffix(" Result"))?;
    number.replace(',', "").trim().parse().ok()
}

// ---------------------------------------------------------------------------
// Where each session is
// ---------------------------------------------------------------------------

/// A session's known position: the path it was last moved to.
#[derive(Clone)]
struct Located {
    hierarchy: String,
    input: Option<String>,
    steps: Vec<Step>,
    /// Roon's level for the top list and after each step (a step can go more
    /// than one level deep when an in-between page is skipped). Used to pop
    /// back the right amount and check it landed right.
    levels: Vec<u32>,
}

type Slot = Arc<AsyncMutex<Option<Located>>>;

/// Positions of the browse sessions, stored in Tauri's managed state.
/// Each session has its own lock, so two requests never move it at once.
#[derive(Default)]
pub struct BrowseState {
    sessions: Mutex<HashMap<String, Slot>>,
}

impl BrowseState {
    fn slot(&self, session: &str) -> Slot {
        self.sessions
            .lock()
            .unwrap()
            .entry(session.to_string())
            .or_default()
            .clone()
    }

    /// Forgets every session's position. Called on each (re)connection,
    /// because the Core starts its sessions fresh.
    pub fn reset(&self) {
        self.sessions.lock().unwrap().clear();
    }
}

/// Talks to Roon for one session.
struct Nav<'a> {
    browse: &'a Browse,
    session: &'a str,
    hierarchy: &'a str,
    zone_id: Option<String>,
}

impl Nav<'_> {
    async fn browse(&self, opts: BrowseOptions) -> Result<BrowseResult, String> {
        self.browse
            .browse(BrowseOptions {
                hierarchy: Some(self.hierarchy.into()),
                multi_session_key: Some(self.session.into()),
                zone_or_output_id: self.zone_id.clone(),
                ..opts
            })
            .await
            .map_err(|e| e.to_string())
    }

    async fn load(&self, offset: u32, count: u32) -> Result<LoadResult, String> {
        self.browse
            .load(LoadOptions {
                hierarchy: Some(self.hierarchy.into()),
                multi_session_key: Some(self.session.into()),
                offset: Some(offset),
                count: Some(count.clamp(1, 500)),
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())
    }

    /// Back to the top (with `input`, this runs a search).
    async fn root(&self, input: Option<String>) -> Result<BrowseResult, String> {
        self.browse(BrowseOptions {
            pop_all: Some(true),
            input,
            ..Default::default()
        })
        .await
    }

    async fn pop(&self, levels: u32) -> Result<BrowseResult, String> {
        self.browse(BrowseOptions {
            pop_levels: Some(levels),
            ..Default::default()
        })
        .await
    }

    /// Opens (or runs) a step's item in the current list.
    async fn open(&self, step: &Step) -> Result<BrowseResult, String> {
        let key = self.find_key(step).await?;
        self.browse(BrowseOptions {
            item_key: Some(key),
            ..Default::default()
        })
        .await
    }

    /// Opens a step's item like Roon's own app does: some items (e.g. an
    /// album opened from search) lead to an in-between page holding only the
    /// item itself, with the real page one level further. Those are opened
    /// straight through.
    async fn open_page(&self, step: &Step) -> Result<BrowseResult, String> {
        let mut result = self.open(step).await?;
        for _ in 0..3 {
            let title = match &result.list {
                Some(list) if result.action == "list" && list.count == 1 => list.title.clone(),
                _ => break,
            };
            let page = self.load(0, 1).await?;
            let Some(item) = page.items.into_iter().next() else {
                break;
            };
            if item.hint.as_deref() != Some("list") || item.title != title {
                break;
            }
            let Some(key) = item.item_key else {
                break;
            };
            result = self
                .browse(BrowseOptions {
                    item_key: Some(key),
                    ..Default::default()
                })
                .await?;
        }
        Ok(result)
    }

    /// The item key for a step: the item at its index if the name matches,
    /// otherwise the first item with that name.
    async fn find_key(&self, step: &Step) -> Result<String, String> {
        let at_index = self.load(step.index, 1).await?;
        if let Some(item) = at_index.items.into_iter().next() {
            if item.title == step.title {
                if let Some(key) = item.item_key {
                    return Ok(key);
                }
            }
        }
        let all = self.load(0, 500).await?;
        all.items
            .into_iter()
            .find(|item| item.title == step.title)
            .and_then(|item| item.item_key)
            .ok_or_else(|| format!("\"{}\" isn't available anymore.", step.title))
    }
}

/// Moves the session to `steps` (from the top of `hierarchy`, with `input`
/// for searches). Returns Roon's result for the last move, or None when the
/// session was already there (a list).
async fn go_to(
    nav: &Nav<'_>,
    at: &mut Option<Located>,
    input: Option<String>,
    steps: Vec<Step>,
) -> Result<Option<BrowseResult>, String> {
    // Fast paths: already there, one step deeper, or some steps back.
    if let Some(current) = at.clone() {
        if current.hierarchy == nav.hierarchy && current.input == input {
            if current.steps == steps {
                return Ok(None);
            }
            let deeper = steps.len() == current.steps.len() + 1 && steps.starts_with(&current.steps);
            if deeper {
                if let Ok(result) = nav.open_page(steps.last().unwrap()).await {
                    *at = match &result.list {
                        Some(list) if result.action == "list" => {
                            let mut levels = current.levels.clone();
                            levels.push(list.level);
                            Some(Located {
                                steps,
                                levels,
                                ..current
                            })
                        }
                        _ => None,
                    };
                    return Ok(Some(result));
                }
            } else if current.steps.starts_with(&steps) {
                let expected = current.levels[steps.len()];
                let now = current.levels[current.steps.len()];
                if let Ok(result) = nav.pop(now.saturating_sub(expected)).await {
                    let landed = result.action == "list"
                        && result.list.as_ref().map(|l| l.level) == Some(expected);
                    if landed {
                        let levels = current.levels[..=steps.len()].to_vec();
                        *at = Some(Located {
                            steps,
                            levels,
                            ..current
                        });
                        return Ok(Some(result));
                    }
                }
            }
        }
    }

    // Otherwise (or if a fast path went wrong): from the top, one step at a time.
    *at = None;
    let mut result = nav.root(input.clone()).await?;
    let mut levels = vec![result.list.as_ref().map(|l| l.level).unwrap_or(0)];
    for step in &steps {
        if result.action != "list" {
            return Err("That page isn't available anymore.".into());
        }
        result = nav.open_page(step).await?;
        if let Some(list) = &result.list {
            levels.push(list.level);
        }
    }
    if result.action == "list" && levels.len() == steps.len() + 1 {
        *at = Some(Located {
            hierarchy: nav.hierarchy.to_string(),
            input,
            steps,
            levels,
        });
    }
    Ok(Some(result))
}

fn browse_service(zones: &Zones) -> Result<Browse, String> {
    zones
        .core()
        .map(|core| core.browse())
        .ok_or_else(|| "Not connected to Roon.".to_string())
}

fn check_hierarchy(hierarchy: &str) -> Result<(), String> {
    if HIERARCHIES.contains(&hierarchy) {
        Ok(())
    } else {
        Err(format!("Unknown hierarchy: {hierarchy}"))
    }
}

async fn load_page(
    browse: &Browse,
    session: &str,
    hierarchy: &str,
    offset: u32,
    count: u32,
) -> Result<(Option<ListView>, Vec<ItemView>, u32), String> {
    let nav = Nav {
        browse,
        session,
        hierarchy,
        zone_id: None,
    };
    let result = nav.load(offset, count).await?;
    let list = result.list.map(|l| ListView {
        title: l.title,
        subtitle: subtitle_text(l.subtitle),
        count: l.count,
        level: l.level,
        image_key: l.image_key,
        hint: l.hint,
    });
    Ok((list, result.items.into_iter().map(item_view).collect(), result.offset))
}

// ---------------------------------------------------------------------------
// Commands the UI can call
// ---------------------------------------------------------------------------

/// Goes to a path and returns that list, or runs the action at its end.
///
/// - `session`: "search" or "browse" (each keeps its own place)
/// - `hierarchy`: "search" or "browse" (or another allowed hierarchy)
/// - `input`: the search text (search hierarchy only)
/// - `path`: `[{ index, title }, ...]`, the items opened from the top; `[]` is
///   the top itself (for search: the results list)
/// - `zoneId`: the zone actions like Play Now apply to
/// - `offset` / `count`: which items to return (paging; default the first 100)
///
/// A path ending on a track opens its actions (Play Now, Add Next, ...);
/// a path ending on one of those actions runs it (`action` is then "none"
/// or "message").
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn roon_browse_path(
    zones: State<'_, Zones>,
    state: State<'_, BrowseState>,
    session: String,
    hierarchy: String,
    input: Option<String>,
    path: Vec<Step>,
    zone_id: Option<String>,
    offset: Option<u32>,
    count: Option<u32>,
) -> Result<BrowseView, String> {
    check_hierarchy(&hierarchy)?;
    let browse = browse_service(&zones)?;
    let slot = state.slot(&session);
    let mut at = slot.lock().await;
    let nav = Nav {
        browse: &browse,
        session: &session,
        hierarchy: &hierarchy,
        zone_id,
    };

    let input = input.map(|text| text.trim().to_string()).filter(|t| !t.is_empty());
    let moved = match go_to(&nav, &mut at, input, path).await {
        Ok(moved) => moved,
        Err(e) => {
            *at = None;
            return Err(e);
        }
    };

    let mut view = match moved {
        Some(result) => result_view(result),
        None => BrowseView {
            action: "list".into(),
            list: None,
            items: Vec::new(),
            offset: 0,
            message: None,
            is_error: false,
            item: None,
        },
    };
    if view.action == "list" {
        let (list, items, first) = load_page(
            &browse,
            &session,
            &hierarchy,
            offset.unwrap_or(0),
            count.unwrap_or(PAGE_SIZE),
        )
        .await?;
        view.list = list;
        view.items = items;
        view.offset = first;
    }
    Ok(view)
}

/// Runs a search for the Search tab: Roon's top hit(s), then the first few
/// results of each group (Artists, Albums, Tracks, ...), fetched side by side.
/// Opening anything from here goes through `roon_browse_path` with
/// `hierarchy: "search"`, `input: query` and a path starting at the group's
/// (or top hit's) `index`.
#[tauri::command]
pub async fn roon_search(
    zones: State<'_, Zones>,
    state: State<'_, BrowseState>,
    query: String,
) -> Result<SearchView, String> {
    let query = query.trim().to_string();
    let mut view = SearchView {
        query: query.clone(),
        top_hits: Vec::new(),
        sections: Vec::new(),
    };
    if query.is_empty() {
        return Ok(view);
    }
    let browse = browse_service(&zones)?;

    // The results list, in a session of its own.
    let results = {
        let slot = state.slot("search-results");
        let mut at = slot.lock().await;
        *at = None;
        let nav = Nav {
            browse: &browse,
            session: "search-results",
            hierarchy: "search",
            zone_id: None,
        };
        let result = nav.root(Some(query.clone())).await?;
        if result.action != "list" {
            return Ok(view);
        }
        nav.load(0, PAGE_SIZE).await?
    };

    // Split it into top hits and groups, and fetch each group's first results
    // at the same time, each in its own session.
    let mut groups = Vec::new();
    for (i, item) in results.items.into_iter().enumerate() {
        let index = results.offset + i as u32;
        match group_count(&item) {
            Some(count) => {
                let step = Step {
                    index,
                    title: item.title.clone(),
                };
                let session = format!("search-group-{index}");
                let slot = state.slot(&session);
                let browse = browse.clone();
                let query = query.clone();
                let task = tauri::async_runtime::spawn(async move {
                    let mut at = slot.lock().await;
                    *at = None;
                    let nav = Nav {
                        browse: &browse,
                        session: &session,
                        hierarchy: "search",
                        zone_id: None,
                    };
                    nav.root(Some(query)).await?;
                    if nav.open(&step).await?.action != "list" {
                        return Ok(Vec::new());
                    }
                    let page = nav.load(0, SEARCH_PREVIEW).await?;
                    Ok::<_, String>(
                        page.items
                            .into_iter()
                            .enumerate()
                            .map(|(j, item)| SearchItem {
                                item: item_view(item),
                                index: page.offset + j as u32,
                            })
                            .collect(),
                    )
                });
                groups.push((item.title, index, count, task));
            }
            None => view.top_hits.push(SearchItem {
                item: item_view(item),
                index,
            }),
        }
    }

    for (title, index, count, task) in groups {
        // A group that fails to load still shows (with "See all").
        let items = match task.await {
            Ok(Ok(items)) => items,
            Ok(Err(e)) => {
                eprintln!("[roon] search group {title} failed: {e}");
                Vec::new()
            }
            Err(e) => {
                eprintln!("[roon] search group {title} failed: {e}");
                Vec::new()
            }
        };
        view.sections.push(SearchSection {
            title,
            index,
            count,
            items,
        });
    }
    Ok(view)
}

/// Opens, goes back, searches, or runs an action, by item key.
///
/// - `itemKey`: open/run that item (from a previous result)
/// - `popAll`: go back to the top of the hierarchy
/// - `popLevels`: go back that many levels (1 = the back button)
/// - `input`: text for items that take input; with the "search" hierarchy and
///   `popAll: true`, this runs a search
/// - `zoneId`: the zone actions like Play Now apply to
/// - `refresh`: reload the current list in place
///
/// Don't mix this with `roon_browse_path` on the same session; if you do, the
/// session's remembered path is dropped and rebuilt on the next path call.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn roon_browse(
    zones: State<'_, Zones>,
    state: State<'_, BrowseState>,
    session: String,
    hierarchy: String,
    item_key: Option<String>,
    pop_all: Option<bool>,
    pop_levels: Option<u32>,
    input: Option<String>,
    zone_id: Option<String>,
    refresh: Option<bool>,
) -> Result<BrowseView, String> {
    check_hierarchy(&hierarchy)?;
    let browse = browse_service(&zones)?;
    let slot = state.slot(&session);
    let mut at = slot.lock().await;
    *at = None;

    let nav = Nav {
        browse: &browse,
        session: &session,
        hierarchy: &hierarchy,
        zone_id,
    };
    let result = nav
        .browse(BrowseOptions {
            item_key,
            pop_all,
            pop_levels,
            input,
            refresh_list: refresh,
            ..Default::default()
        })
        .await?;

    let mut view = result_view(result);
    if view.action == "list" {
        let (list, items, offset) = load_page(&browse, &session, &hierarchy, 0, PAGE_SIZE).await?;
        view.list = list;
        view.items = items;
        view.offset = offset;
    }
    Ok(view)
}

/// Loads another page of the current list (for lists longer than one page).
#[tauri::command]
pub async fn roon_browse_more(
    zones: State<'_, Zones>,
    session: String,
    hierarchy: String,
    offset: u32,
    count: Option<u32>,
) -> Result<BrowseView, String> {
    check_hierarchy(&hierarchy)?;
    let browse = browse_service(&zones)?;
    let (list, items, offset) = load_page(
        &browse,
        &session,
        &hierarchy,
        offset,
        count.unwrap_or(PAGE_SIZE),
    )
    .await?;
    Ok(BrowseView {
        action: "list".into(),
        list,
        items,
        offset,
        message: None,
        is_error: false,
        item: None,
    })
}

#[cfg(test)]
mod tests {
    use super::plain_text;

    #[test]
    fn roon_links_become_names() {
        assert_eq!(plain_text("[[17431674|Kiefer]]"), "Kiefer");
        assert_eq!(plain_text("[[1|Tinashe]], [[2|Kiefer]]"), "Tinashe, Kiefer");
        assert_eq!(plain_text("by [[5|Harry Connick, Jr.]] (2019)"), "by Harry Connick, Jr. (2019)");
        assert_eq!(plain_text("Plain Artist"), "Plain Artist");
        assert_eq!(plain_text("odd [[text"), "odd [[text");
        assert_eq!(plain_text("[[no id]]"), "no id");
    }
}
