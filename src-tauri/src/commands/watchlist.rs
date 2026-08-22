//! Watchlist commands (docs/command-contract.md §自选股).

use serde::Serialize;
use tauri::State;

use crate::error::CmdError;
use crate::state::AppState;

use super::market::parse_symbol;

/// Contract watchlist row: `{group_name,code,name?,added_at,pinned}`.
/// `name` is not persisted by the storage layer, so it is always null here;
/// the UI resolves display names via `get_quote`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WatchlistEntry {
    /// Watchlist group name.
    pub group_name: String,
    /// Bare 6-digit code.
    pub code: String,
    /// Display name (not stored; always null).
    pub name: Option<String>,
    /// Added time, unix seconds.
    pub added_at: i64,
    /// Whether the item is pinned to the top.
    pub pinned: bool,
}

/// `watchlist_add` response.
#[derive(Debug, Serialize)]
pub struct WatchlistOkResponse {
    /// Always true on success.
    pub ok: bool,
}

/// `watchlist_remove` / `watchlist_pin` response: whether the row existed.
#[derive(Debug, Serialize)]
pub struct WatchlistRemovedResponse {
    /// Whether the entry existed and the operation applied.
    pub removed: bool,
}

/// List all watchlist entries across all groups (pinned first per group).
#[tauri::command(rename_all = "snake_case")]
pub async fn watchlist_list(state: State<'_, AppState>) -> Result<Vec<WatchlistEntry>, CmdError> {
    let rows = state
        .storage
        .run(|conn| {
            let mut stmt = conn.prepare(
                "SELECT group_name, code, added_at, pinned FROM watchlist
                 ORDER BY group_name ASC, pinned DESC, added_at ASC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(WatchlistEntry {
                    group_name: row.get(0)?,
                    code: row.get(1)?,
                    name: None,
                    added_at: row.get(2)?,
                    pinned: row.get::<_, i64>(3)? != 0,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await?;
    Ok(rows)
}

/// Add a code to a group (idempotent).
#[tauri::command(rename_all = "snake_case")]
pub async fn watchlist_add(
    state: State<'_, AppState>,
    code: String,
    group: String,
) -> Result<WatchlistOkResponse, CmdError> {
    let symbol = parse_symbol(&code)?;
    state.storage.watchlist_add(&group, symbol.code()).await?;
    Ok(WatchlistOkResponse { ok: true })
}

/// Remove a code from a group.
#[tauri::command(rename_all = "snake_case")]
pub async fn watchlist_remove(
    state: State<'_, AppState>,
    code: String,
    group: String,
) -> Result<WatchlistRemovedResponse, CmdError> {
    let symbol = parse_symbol(&code)?;
    let removed = state
        .storage
        .watchlist_remove(&group, symbol.code())
        .await?;
    Ok(WatchlistRemovedResponse { removed })
}

/// Pin or unpin an entry.
#[tauri::command(rename_all = "snake_case")]
pub async fn watchlist_pin(
    state: State<'_, AppState>,
    code: String,
    group: String,
    pinned: bool,
) -> Result<WatchlistRemovedResponse, CmdError> {
    let symbol = parse_symbol(&code)?;
    let removed = state
        .storage
        .watchlist_set_pinned(&group, symbol.code(), pinned)
        .await?;
    Ok(WatchlistRemovedResponse { removed })
}
