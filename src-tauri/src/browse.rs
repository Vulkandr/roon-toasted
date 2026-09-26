//! Browse and search: Roon's library as lists the pages can navigate.
//!
//! Roon keeps the navigation position on the Core, separately for each
//! "session" name, so Browse and Search each use their own session and keep
//! their place independently (e.g. `session: "browse"` and `session: "search"`).
//!
//! - `roon_browse` opens an item, goes back, searches, or runs an action
//!   (Play Now, Queue, ...) on the selected zone. When the result is a list,
//!   its first page of items comes back with it.
//! - `roon_browse_more` loads further pages of the current list.

use roon_api::{Browse, BrowseItem, BrowseOptions, LoadOptions};
use serde::Serialize;
use tauri::State;

use crate::zones::Zones;

/// Items per page when a list is opened or paged through.
const PAGE_SIZE: u32 = 100;

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

fn item_view(item: BrowseItem) -> ItemView {
    ItemView {
        title: item.title,
        subtitle: item.subtitle.filter(|s| !s.is_empty()),
        item_key: item.item_key,
        image_key: item.image_key,
        hint: item.hint,
        input_prompt: item.input_prompt.and_then(|p| p.prompt),
    }
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
    let result = browse
        .load(LoadOptions {
            hierarchy: Some(hierarchy.into()),
            multi_session_key: Some(session.into()),
            offset: Some(offset),
            count: Some(count.clamp(1, 500)),
            ..Default::default()
        })
        .await
        .map_err(|e| e.to_string())?;

    let list = result.list.map(|l| ListView {
        title: l.title,
        subtitle: l.subtitle.filter(|s| !s.is_empty()),
        count: l.count,
        level: l.level,
        image_key: l.image_key,
        hint: l.hint,
    });
    Ok((list, result.items.into_iter().map(item_view).collect(), result.offset))
}

/// Opens, goes back, searches, or runs an action.
///
/// - `itemKey`: open/run that item (from a previous result)
/// - `popAll`: go back to the top of the hierarchy
/// - `popLevels`: go back that many levels (1 = the back button)
/// - `input`: text for items that take input; with the "search" hierarchy and
///   `popAll: true`, this runs a search
/// - `zoneId`: the zone actions like Play Now apply to
/// - `refresh`: reload the current list in place
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn roon_browse(
    zones: State<'_, Zones>,
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

    let result = browse
        .browse(BrowseOptions {
            hierarchy: Some(hierarchy.clone()),
            multi_session_key: Some(session.clone()),
            item_key,
            pop_all,
            pop_levels,
            input,
            zone_or_output_id: zone_id,
            refresh_list: refresh,
            ..Default::default()
        })
        .await
        .map_err(|e| e.to_string())?;

    let mut view = BrowseView {
        action: result.action.clone(),
        list: None,
        items: Vec::new(),
        offset: 0,
        message: result.message,
        is_error: result.is_error.unwrap_or(false),
        item: result.item.map(item_view),
    };

    if result.action == "list" {
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
