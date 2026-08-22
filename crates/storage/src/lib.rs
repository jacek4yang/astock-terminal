//! Tiered local persistence for the A-share terminal.
//!
//! Three tiers:
//! - **SQLite** (`meta.db`) for metadata/state: securities, settings,
//!   watchlists, agent conversations, tool cache, reports, predictions.
//!   Accessed through a dedicated blocking thread (see [`Storage::run`]).
//! - **Parquet** time-series cache under `timeseries/` for kline bars and
//!   fund flow, partitioned by `{symbol}/{period}/{adjust}.parquet`.
//! - **LRU memory cache** for hot values.
//!
//! ```no_run
//! use astock_storage::{Storage, StorageConfig};
//! # async fn demo() -> astock_storage::Result<()> {
//! let storage = Storage::open(StorageConfig::default())?;
//! storage.settings_set("theme", "dark").await?;
//! assert_eq!(storage.settings_get("theme").await?, Some("dark".into()));
//! # Ok(())
//! # }
//! ```

mod config;
mod db;
mod error;
mod maintenance;
mod memcache;
mod timeseries;

pub use config::StorageConfig;
pub use error::{Error, Result};
pub use maintenance::{disk_free_bytes, CacheStats, CleanupPolicy, CleanupReport};
pub use memcache::MemCache;
pub use timeseries::{BarRow, FundFlowRow};

use std::path::PathBuf;
use std::sync::Arc;

use chrono::NaiveDate;
use rusqlite::{params, Connection};

use astock_core::{Market, SecurityMasterRecord};

use db::{now_secs, Db};
use timeseries::TimeSeriesStore;

fn enum_token(value: &impl serde::Serialize) -> Result<String> {
    Ok(serde_json::to_string(value)?.trim_matches('"').to_string())
}

fn parse_enum<T: serde::de::DeserializeOwned>(token: &str) -> Result<T> {
    Ok(serde_json::from_str(&format!("\"{token}\""))?)
}

fn parse_market(token: &str) -> Result<Market> {
    match token.to_ascii_uppercase().as_str() {
        "SH" => Ok(Market::SH),
        "SZ" => Ok(Market::SZ),
        "BJ" => Ok(Market::BJ),
        _ => Err(Error::Invalid(format!("unknown security market {token}"))),
    }
}

/// A row of the `tool_cache` table.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCacheEntry {
    /// Cache key (primary key).
    pub cache_key: String,
    /// Tool that produced the result.
    pub tool: String,
    /// JSON-encoded tool parameters.
    pub params_json: String,
    /// JSON-encoded tool result.
    pub result_json: String,
    /// Optional upstream data version stamp.
    pub data_version: Option<String>,
    /// Creation time, unix seconds.
    pub created_at: i64,
    /// Time-to-live in seconds.
    pub ttl_seconds: i64,
    /// Last access time, unix seconds.
    pub accessed_at: i64,
}

impl ToolCacheEntry {
    /// Whether this entry is expired at `now` (unix seconds).
    pub fn is_expired(&self, now: i64) -> bool {
        now >= self.created_at + self.ttl_seconds
    }
}

/// A `kv` settings-table row with its write timestamp (column added by
/// migration v5; rows predating v5 report `fetched_at = 0`).
#[derive(Debug, Clone, PartialEq)]
pub struct KvEntry {
    /// Setting key (primary key).
    pub key: String,
    /// Setting value.
    pub value: String,
    /// Write time, unix seconds (0 = unknown, written before migration v5).
    pub fetched_at: i64,
}

/// A watchlist row.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchlistItem {
    /// Watchlist group name.
    pub group_name: String,
    /// Security code.
    pub code: String,
    /// When it was added, unix seconds.
    pub added_at: i64,
    /// Whether the item is pinned to the top.
    pub pinned: bool,
}

/// A persisted agent chat message.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    /// Message id (caller-provided, e.g. UUID).
    pub id: String,
    /// Owning conversation id.
    pub conversation_id: String,
    /// Role: "user" | "assistant" | "tool" | ...
    pub role: String,
    /// Message text.
    pub content: String,
    /// JSON-encoded tool calls, if any.
    pub tool_calls: Option<String>,
    /// Creation time, unix seconds.
    pub created_at: i64,
}

/// A persisted agent conversation (chat thread header).
#[derive(Debug, Clone, PartialEq)]
pub struct Conversation {
    /// Conversation id (equals the agent task id).
    pub id: String,
    /// Optional title (the agent stores the task kind here).
    pub title: Option<String>,
    /// Creation time, unix seconds.
    pub created_at: i64,
}

/// A tracked prediction (thesis → outcome lifecycle).
#[derive(Debug, Clone, PartialEq)]
pub struct Prediction {
    /// Prediction id (caller-provided).
    pub id: String,
    /// Symbol the prediction is about.
    pub symbol: String,
    /// Thesis text.
    pub thesis: String,
    /// Expected outcome description.
    pub expectation: Option<String>,
    /// Subjective probability, 0..=1.
    pub probability: Option<f64>,
    /// Time horizon, e.g. "1m", "6m".
    pub horizon: Option<String>,
    /// What would invalidate the thesis.
    pub invalidation: Option<String>,
    /// JSON snapshot of the inputs used.
    pub snapshot_json: Option<String>,
    /// Creation time, unix seconds.
    pub created_at: i64,
    /// Review outcome, set by [`Storage::predictions_review`].
    pub outcome: Option<String>,
    /// Review time, unix seconds.
    pub reviewed_at: Option<i64>,
}

/// A stored report.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// Report id (caller-provided).
    pub id: String,
    /// Report kind, e.g. "daily", "deep-dive".
    pub kind: String,
    /// Title.
    pub title: String,
    /// JSON-encoded report content.
    pub content_json: String,
    /// Creation time, unix seconds.
    pub created_at: i64,
}

/// A persisted agent workflow task (suspend/resume state).
#[derive(Debug, Clone, PartialEq)]
pub struct AgentTask {
    /// Task id (caller-provided).
    pub id: String,
    /// Task kind, e.g. "analysis".
    pub kind: String,
    /// Lifecycle status: "running" | "suspended" | "completed" | "failed" | "cancelled".
    pub status: String,
    /// JSON-encoded workflow state (task spec, round counter, evidence).
    pub state_json: String,
    /// Creation time, unix seconds.
    pub created_at: i64,
    /// Last update time, unix seconds.
    pub updated_at: i64,
}

/// One metadata-only event in the append-only Agent tool audit trail.
///
/// Request/response bodies, raw arguments, provider errors and credentials
/// are intentionally absent. `args_fingerprint` is a one-way digest used to
/// correlate retries without retaining sensitive input.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentToolAudit {
    /// SQLite sequence id; `None` before insertion.
    pub id: Option<i64>,
    /// Durable Agent task id.
    pub task_id: String,
    /// Stable tool-call id within the task.
    pub call_id: String,
    /// Registered tool name.
    pub tool: String,
    /// Immutable permission domain assigned by the tool registry.
    pub permission_domain: String,
    /// Invocation origin, e.g. `model_plan` or `explicit_user`.
    pub origin: String,
    /// One-way digest of canonicalized arguments.
    pub args_fingerprint: String,
    /// Lifecycle event: requested|denied|succeeded|failed|invalid_arguments.
    pub event: String,
    /// Runtime when this is a terminal event.
    pub elapsed_ms: Option<i64>,
    /// Event time, unix seconds.
    pub created_at: i64,
}

/// A supply-chain graph node row (migration v3).
#[derive(Debug, Clone, PartialEq)]
pub struct GraphNodeRow {
    /// Node id (caller-provided, e.g. "company:600519").
    pub id: String,
    /// Node kind: company|product|segment|material|commodity|industry|region|policy.
    pub kind: String,
    /// Display name (Chinese).
    pub name: String,
    /// Listed-company code for kind=company, else None.
    pub code: Option<String>,
    /// Free-form JSON metadata.
    pub meta_json: String,
    /// Creation time, unix seconds.
    pub created_at: i64,
    /// Last update time, unix seconds.
    pub updated_at: i64,
}

/// A supply-chain graph edge row (migration v3). `id` is None before insert.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphEdgeRow {
    /// Row id (autoincrement; None for a not-yet-inserted edge).
    pub id: Option<i64>,
    /// Source node id.
    pub src: String,
    /// Destination node id.
    pub dst: String,
    /// Relation: supplies|customer_of|competes|substitutes|exposed_to|
    /// belongs_to|produces|consumes.
    pub relation: String,
    /// Relation strength 0..=1 (heuristic, e.g. revenue share of the link).
    pub weight: f64,
    /// Provenance: human-readable source name (e.g. "公司年报2024").
    pub source_name: String,
    /// Provenance: public URL backing the relation.
    pub source_url: String,
    /// Confidence 0..=1 (source-backed only; see astock-graph docs).
    pub confidence: f64,
    /// Valid-from, unix seconds (0 = unknown/always).
    pub valid_from: i64,
    /// Valid-to, unix seconds; None = still valid.
    pub valid_to: Option<i64>,
    /// Creation time, unix seconds.
    pub created_at: i64,
    /// Last update time, unix seconds.
    pub updated_at: i64,
}

/// A persisted graph event row (migration v3).
#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    /// Event id (caller-provided).
    pub id: String,
    /// Event kind, e.g. "commodity_price" | "policy" | "accident".
    pub kind: String,
    /// Human-readable title.
    pub title: String,
    /// Subject: graph node id, company code, or node name.
    pub subject: String,
    /// Event magnitude (e.g. 0.10 for +10%); None when not quantified.
    pub magnitude: Option<f64>,
    /// Direction: +1 up/positive, -1 down/negative.
    pub direction: i64,
    /// When the event occurred, unix seconds.
    pub occurred_at: i64,
    /// Provenance: source name.
    pub source_name: String,
    /// Provenance: source URL.
    pub source_url: String,
    /// Lifecycle status: "new" | "processed" | "archived".
    pub status: String,
    /// Creation time, unix seconds.
    pub created_at: i64,
}

/// A `corporate_actions` row (migration v4): one corporate action with
/// fetch provenance. Ratios are per share (10送10 -> `bonus_share = 1.0`);
/// see `docs/data-foundation-v2.md` §Schema 变更.
#[derive(Debug, Clone, PartialEq)]
pub struct CorporateActionRow {
    /// Security code (bare 6 digits).
    pub code: String,
    /// Ex-dividend / ex-rights date.
    pub ex_date: NaiveDate,
    /// Announcement date, when known (strict PIT uses it).
    pub notice_date: Option<NaiveDate>,
    /// Pre-tax cash dividend per share.
    pub cash_div: f64,
    /// Bonus + capitalisation shares per share.
    pub bonus_share: f64,
    /// Rights-issue shares per share.
    pub rights_ratio: f64,
    /// Rights-issue price per new share, when known.
    pub rights_price: Option<f64>,
    /// Data source label, e.g. "eastmoney".
    pub source: String,
    /// Provenance URL.
    pub source_url: String,
    /// Fetch time, unix seconds.
    pub fetched_at: i64,
}

impl From<&CorporateActionRow> for astock_core::CorporateAction {
    fn from(row: &CorporateActionRow) -> Self {
        astock_core::CorporateAction {
            ex_date: row.ex_date,
            notice_date: row.notice_date,
            cash_div: row.cash_div,
            bonus_share: row.bonus_share,
            rights_ratio: row.rights_ratio,
            rights_price: row.rights_price,
        }
    }
}

/// Output of [`Storage::load_bars_adjusted`]: adjusted bars plus the
/// data-quality warnings from the adjustment engine.
#[derive(Debug, Clone, PartialEq)]
pub struct AdjustedBars {
    /// Adjusted bars (provenance fields carried over from the raw rows).
    pub bars: Vec<BarRow>,
    /// Degraded/skipped corporate actions.
    pub warnings: Vec<astock_core::AdjustWarning>,
}

struct Inner {
    config: StorageConfig,
    db: Db,
    ts: TimeSeriesStore,
    tool_mem: MemCache<ToolCacheEntry>,
    bars_mem: MemCache<Arc<Vec<BarRow>>>,
}

/// Top-level storage handle. Cheap to clone, `Send + Sync`.
///
/// SQLite work runs on a dedicated blocking thread; Parquet IO runs on
/// `tokio::task::spawn_blocking`. Dropping the last handle shuts the worker
/// thread down.
#[derive(Clone)]
pub struct Storage {
    inner: Arc<Inner>,
}

impl Storage {
    /// Open storage at `config.base_dir` (created if missing; never panics on
    /// absent directories) and run pending migrations.
    pub fn open(config: StorageConfig) -> Result<Storage> {
        std::fs::create_dir_all(&config.base_dir)?;
        std::fs::create_dir_all(config.timeseries_dir())?;
        let db = Db::open(&config.meta_db_path())?;
        let ts = TimeSeriesStore::new(config.timeseries_dir());
        Ok(Storage {
            inner: Arc::new(Inner {
                tool_mem: MemCache::new(config.mem_cache_entries),
                bars_mem: MemCache::new(config.mem_cache_entries),
                config,
                db,
                ts,
            }),
        })
    }

    /// The configuration this storage was opened with.
    pub fn config(&self) -> &StorageConfig {
        &self.inner.config
    }

    /// Run a raw closure with the SQLite connection on the worker thread.
    /// Escape hatch for queries without a typed helper.
    pub async fn run<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        self.inner.db.run(f).await
    }

    /// Shut down the database worker thread (idempotent; also happens on drop).
    pub fn shutdown(&self) {
        self.inner.db.shutdown();
    }

    /// In-memory LRU cache for hot tool results.
    pub fn tool_mem_cache(&self) -> &MemCache<ToolCacheEntry> {
        &self.inner.tool_mem
    }

    // ------------------------------------------------------------------
    // canonical security master (migration v6)
    // ------------------------------------------------------------------

    /// Upsert a provider refresh as one transaction.
    pub async fn securities_upsert(&self, records: Vec<SecurityMasterRecord>) -> Result<()> {
        self.run(move |conn| {
            let tx = conn.transaction()?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO securities
                     (code,name,market,classify,updated_at,board,asset_type,
                      aliases_json,industry,concepts_json,region,source,source_url,
                      valid_from,valid_to,refreshed_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
                     ON CONFLICT(code) DO UPDATE SET
                       name=excluded.name, market=excluded.market,
                       classify=excluded.classify, updated_at=excluded.updated_at,
                       board=excluded.board, asset_type=excluded.asset_type,
                       aliases_json=excluded.aliases_json, industry=excluded.industry,
                       concepts_json=excluded.concepts_json, region=excluded.region,
                       source=excluded.source, source_url=excluded.source_url,
                       valid_from=excluded.valid_from, valid_to=excluded.valid_to,
                       refreshed_at=excluded.refreshed_at",
                )?;
                for record in records {
                    let refreshed_at = record.refreshed_at.timestamp();
                    stmt.execute(params![
                        record.code,
                        record.canonical_name,
                        record.market.to_string(),
                        enum_token(&record.board)?,
                        refreshed_at,
                        enum_token(&record.board)?,
                        enum_token(&record.asset_type)?,
                        serde_json::to_string(&record.aliases)?,
                        record.industry,
                        serde_json::to_string(&record.concepts)?,
                        record.region,
                        record.source,
                        record.source_url,
                        record.valid_from.map(|d| d.timestamp()),
                        record.valid_to.map(|d| d.timestamp()),
                        refreshed_at,
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Load the complete locally cached security master.
    pub async fn securities_list(&self) -> Result<Vec<SecurityMasterRecord>> {
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT code,name,market,board,asset_type,aliases_json,industry,
                        concepts_json,region,source,source_url,valid_from,valid_to,
                        refreshed_at
                 FROM securities ORDER BY code",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                    row.get::<_, Option<i64>>(12)?,
                    row.get::<_, i64>(13)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (
                    code,
                    canonical_name,
                    market,
                    board,
                    asset_type,
                    aliases_json,
                    industry,
                    concepts_json,
                    region,
                    source,
                    source_url,
                    valid_from,
                    valid_to,
                    refreshed_at,
                ) = row?;
                out.push(SecurityMasterRecord {
                    code,
                    canonical_name,
                    market: parse_market(&market)?,
                    board: parse_enum(&board)?,
                    asset_type: parse_enum(&asset_type)?,
                    aliases: serde_json::from_str(&aliases_json)?,
                    industry,
                    concepts: serde_json::from_str(&concepts_json)?,
                    region,
                    source,
                    source_url,
                    valid_from: valid_from.and_then(|ts| chrono::DateTime::from_timestamp(ts, 0)),
                    valid_to: valid_to.and_then(|ts| chrono::DateTime::from_timestamp(ts, 0)),
                    refreshed_at: chrono::DateTime::from_timestamp(refreshed_at, 0)
                        .unwrap_or(chrono::DateTime::UNIX_EPOCH),
                });
            }
            Ok(out)
        })
        .await
    }

    // ------------------------------------------------------------------
    // tool cache
    // ------------------------------------------------------------------

    /// Insert or replace a tool-cache entry and refresh the memory cache.
    pub async fn tool_cache_put(&self, entry: ToolCacheEntry) -> Result<()> {
        let row = entry.clone();
        self.run(move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO tool_cache
                 (cache_key, tool, params_json, result_json, data_version,
                  created_at, ttl_seconds, accessed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    row.cache_key,
                    row.tool,
                    row.params_json,
                    row.result_json,
                    row.data_version,
                    row.created_at,
                    row.ttl_seconds,
                    row.accessed_at,
                ],
            )?;
            Ok(())
        })
        .await?;
        self.inner.tool_mem.put(entry.cache_key.clone(), entry);
        Ok(())
    }

    /// Fetch a tool-cache entry. Returns `Ok(None)` when missing or expired
    /// (expired rows are left in place for [`Storage::cleanup`]). A hit bumps
    /// `accessed_at`.
    pub async fn tool_cache_get(&self, cache_key: &str) -> Result<Option<ToolCacheEntry>> {
        let now = now_secs();
        if let Some(entry) = self.inner.tool_mem.get(cache_key) {
            if !entry.is_expired(now) {
                return Ok(Some(entry));
            }
            self.inner.tool_mem.invalidate(cache_key);
        }
        let key = cache_key.to_string();
        let found = self
            .run(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT cache_key, tool, params_json, result_json, data_version,
                            created_at, ttl_seconds, accessed_at
                     FROM tool_cache WHERE cache_key = ?1",
                )?;
                let mut rows = stmt.query(params![key])?;
                let entry = match rows.next()? {
                    Some(row) => ToolCacheEntry {
                        cache_key: row.get(0)?,
                        tool: row.get(1)?,
                        params_json: row.get(2)?,
                        result_json: row.get(3)?,
                        data_version: row.get(4)?,
                        created_at: row.get(5)?,
                        ttl_seconds: row.get(6)?,
                        accessed_at: row.get(7)?,
                    },
                    None => return Ok(None),
                };
                drop(rows);
                drop(stmt);
                if !entry.is_expired(now) {
                    conn.execute(
                        "UPDATE tool_cache SET accessed_at = ?2 WHERE cache_key = ?1",
                        params![entry.cache_key, now],
                    )?;
                }
                Ok(Some(entry))
            })
            .await?;
        match found {
            Some(entry) if entry.is_expired(now) => Ok(None),
            Some(entry) => {
                self.inner
                    .tool_mem
                    .put(entry.cache_key.clone(), entry.clone());
                Ok(Some(entry))
            }
            None => Ok(None),
        }
    }

    // ------------------------------------------------------------------
    // settings (kv)
    // ------------------------------------------------------------------

    /// Read a setting; `Ok(None)` when absent.
    pub async fn settings_get(&self, key: &str) -> Result<Option<String>> {
        let key = key.to_string();
        self.run(move |conn| {
            let mut stmt = conn.prepare("SELECT value FROM kv WHERE key = ?1")?;
            let mut rows = stmt.query(params![key])?;
            Ok(match rows.next()? {
                Some(row) => Some(row.get(0)?),
                None => None,
            })
        })
        .await
    }

    /// Write a setting.
    pub async fn settings_set(&self, key: &str, value: &str) -> Result<()> {
        let (key, value) = (key.to_string(), value.to_string());
        self.run(move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO kv (key, value, updated_at, fetched_at)
                 VALUES (?1, ?2, ?3, ?3)",
                params![key, value, now_secs()],
            )?;
            Ok(())
        })
        .await
    }

    // ------------------------------------------------------------------
    // typed kv API (migration v5)
    // ------------------------------------------------------------------

    /// Write a kv entry, stamping `fetched_at` with the current time.
    pub async fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        let (key, value) = (key.to_string(), value.to_string());
        self.run(move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO kv (key, value, updated_at, fetched_at)
                 VALUES (?1, ?2, ?3, ?3)",
                params![key, value, now_secs()],
            )?;
            Ok(())
        })
        .await
    }

    /// Read a kv entry with its write timestamp; `Ok(None)` when absent.
    pub async fn kv_get(&self, key: &str) -> Result<Option<KvEntry>> {
        let key = key.to_string();
        self.run(move |conn| {
            let mut stmt = conn.prepare("SELECT key, value, fetched_at FROM kv WHERE key = ?1")?;
            let mut rows = stmt.query(params![key])?;
            Ok(match rows.next()? {
                Some(row) => Some(KvEntry {
                    key: row.get(0)?,
                    value: row.get(1)?,
                    fetched_at: row.get(2)?,
                }),
                None => None,
            })
        })
        .await
    }

    /// Delete a kv entry; returns whether it existed.
    pub async fn kv_delete(&self, key: &str) -> Result<bool> {
        let key = key.to_string();
        self.run(move |conn| {
            let n = conn.execute("DELETE FROM kv WHERE key = ?1", params![key])?;
            Ok(n > 0)
        })
        .await
    }

    /// List all kv entries whose key starts with `prefix`, ordered by key.
    pub async fn kv_list_prefix(&self, prefix: &str) -> Result<Vec<KvEntry>> {
        let prefix = prefix.to_string();
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT key, value, fetched_at FROM kv
                 WHERE substr(key, 1, length(?1)) = ?1 ORDER BY key ASC",
            )?;
            let rows = stmt.query_map(params![prefix], |row| {
                Ok(KvEntry {
                    key: row.get(0)?,
                    value: row.get(1)?,
                    fetched_at: row.get(2)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    // ------------------------------------------------------------------
    // watchlist
    // ------------------------------------------------------------------

    /// Add `code` to a watchlist group (no-op if already present).
    pub async fn watchlist_add(&self, group: &str, code: &str) -> Result<()> {
        let (group, code) = (group.to_string(), code.to_string());
        self.run(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO watchlist (group_name, code, added_at, pinned)
                 VALUES (?1, ?2, ?3, 0)",
                params![group, code, now_secs()],
            )?;
            Ok(())
        })
        .await
    }

    /// Remove `code` from a watchlist group; returns whether it existed.
    pub async fn watchlist_remove(&self, group: &str, code: &str) -> Result<bool> {
        let (group, code) = (group.to_string(), code.to_string());
        self.run(move |conn| {
            let n = conn.execute(
                "DELETE FROM watchlist WHERE group_name = ?1 AND code = ?2",
                params![group, code],
            )?;
            Ok(n > 0)
        })
        .await
    }

    /// Pin or unpin an item; returns whether the item exists.
    pub async fn watchlist_set_pinned(
        &self,
        group: &str,
        code: &str,
        pinned: bool,
    ) -> Result<bool> {
        let (group, code) = (group.to_string(), code.to_string());
        self.run(move |conn| {
            let n = conn.execute(
                "UPDATE watchlist SET pinned = ?3 WHERE group_name = ?1 AND code = ?2",
                params![group, code, pinned as i64],
            )?;
            Ok(n > 0)
        })
        .await
    }

    /// List a group, pinned first, then by insertion order.
    pub async fn watchlist_list(&self, group: &str) -> Result<Vec<WatchlistItem>> {
        let group = group.to_string();
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT group_name, code, added_at, pinned FROM watchlist
                 WHERE group_name = ?1 ORDER BY pinned DESC, added_at ASC",
            )?;
            let rows = stmt.query_map(params![group], |row| {
                Ok(WatchlistItem {
                    group_name: row.get(0)?,
                    code: row.get(1)?,
                    added_at: row.get(2)?,
                    pinned: row.get::<_, i64>(3)? != 0,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    // ------------------------------------------------------------------
    // conversations / messages
    // ------------------------------------------------------------------

    /// Create a conversation (no-op if the id already exists).
    pub async fn conversation_create(&self, id: &str, title: Option<&str>) -> Result<()> {
        let id = id.to_string();
        let title = title.map(str::to_string);
        self.run(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO conversations (id, title, created_at) VALUES (?1, ?2, ?3)",
                params![id, title, now_secs()],
            )?;
            Ok(())
        })
        .await
    }

    /// Append a message to a conversation.
    pub async fn conversation_append(&self, message: ChatMessage) -> Result<()> {
        self.run(move |conn| {
            conn.execute(
                "INSERT INTO messages
                 (id, conversation_id, role, content, tool_calls, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    message.id,
                    message.conversation_id,
                    message.role,
                    message.content,
                    message.tool_calls,
                    message.created_at,
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Load a conversation's messages in chronological order.
    pub async fn conversation_load(&self, conversation_id: &str) -> Result<Vec<ChatMessage>> {
        let id = conversation_id.to_string();
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, conversation_id, role, content, tool_calls, created_at
                 FROM messages WHERE conversation_id = ?1
                 ORDER BY created_at ASC, rowid ASC",
            )?;
            let rows = stmt.query_map(params![id], |row| {
                Ok(ChatMessage {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    tool_calls: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    /// Delete a conversation and all of its messages. Returns true when the
    /// conversation existed.
    pub async fn conversation_delete(&self, conversation_id: &str) -> Result<bool> {
        let id = conversation_id.to_string();
        self.run(move |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "DELETE FROM messages WHERE conversation_id = ?1",
                params![id],
            )?;
            let n = tx.execute("DELETE FROM conversations WHERE id = ?1", params![id])?;
            tx.commit()?;
            Ok(n > 0)
        })
        .await
    }

    /// List all conversations, most recently created first.
    pub async fn conversation_list(&self) -> Result<Vec<Conversation>> {
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, title, created_at FROM conversations
                 ORDER BY created_at DESC, rowid DESC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(Conversation {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    created_at: row.get(2)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    // ------------------------------------------------------------------
    // agent tasks
    // ------------------------------------------------------------------

    /// Insert or update an agent task; bumps `updated_at` (and `created_at`
    /// on first insert).
    pub async fn agent_task_save(&self, task: AgentTask) -> Result<()> {
        self.run(move |conn| {
            conn.execute(
                "INSERT INTO agent_tasks (id, kind, status, state_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET
                     kind = excluded.kind,
                     status = excluded.status,
                     state_json = excluded.state_json,
                     updated_at = excluded.updated_at",
                params![
                    task.id,
                    task.kind,
                    task.status,
                    task.state_json,
                    task.created_at,
                    task.updated_at,
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Load an agent task by id.
    pub async fn agent_task_get(&self, id: &str) -> Result<Option<AgentTask>> {
        let id = id.to_string();
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, kind, status, state_json, created_at, updated_at
                 FROM agent_tasks WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![id])?;
            Ok(match rows.next()? {
                Some(row) => Some(AgentTask {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    status: row.get(2)?,
                    state_json: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                }),
                None => None,
            })
        })
        .await
    }

    /// List all agent tasks, most recently updated first.
    pub async fn agent_task_list(&self) -> Result<Vec<AgentTask>> {
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, kind, status, state_json, created_at, updated_at
                 FROM agent_tasks ORDER BY updated_at DESC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(AgentTask {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    status: row.get(2)?,
                    state_json: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    /// Append a metadata-only Agent tool audit event.
    pub async fn agent_tool_audit_append(&self, audit: AgentToolAudit) -> Result<i64> {
        self.run(move |conn| {
            conn.execute(
                "INSERT INTO agent_tool_audit
                 (task_id, call_id, tool, permission_domain, origin,
                  args_fingerprint, event, elapsed_ms, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    audit.task_id,
                    audit.call_id,
                    audit.tool,
                    audit.permission_domain,
                    audit.origin,
                    audit.args_fingerprint,
                    audit.event,
                    audit.elapsed_ms,
                    audit.created_at,
                ],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await
    }

    /// List a task's tool audit events in execution order.
    pub async fn agent_tool_audit_list(&self, task_id: &str) -> Result<Vec<AgentToolAudit>> {
        let task_id = task_id.to_string();
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, task_id, call_id, tool, permission_domain, origin,
                        args_fingerprint, event, elapsed_ms, created_at
                 FROM agent_tool_audit WHERE task_id = ?1 ORDER BY id ASC",
            )?;
            let rows = stmt.query_map(params![task_id], |row| {
                Ok(AgentToolAudit {
                    id: row.get(0)?,
                    task_id: row.get(1)?,
                    call_id: row.get(2)?,
                    tool: row.get(3)?,
                    permission_domain: row.get(4)?,
                    origin: row.get(5)?,
                    args_fingerprint: row.get(6)?,
                    event: row.get(7)?,
                    elapsed_ms: row.get(8)?,
                    created_at: row.get(9)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    // ------------------------------------------------------------------
    // predictions
    // ------------------------------------------------------------------

    /// Insert a prediction (outcome fields are reset to NULL).
    pub async fn predictions_insert(&self, prediction: Prediction) -> Result<()> {
        self.run(move |conn| {
            conn.execute(
                "INSERT INTO predictions
                 (id, symbol, thesis, expectation, probability, horizon,
                  invalidation, snapshot_json, created_at, outcome, reviewed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, NULL)",
                params![
                    prediction.id,
                    prediction.symbol,
                    prediction.thesis,
                    prediction.expectation,
                    prediction.probability,
                    prediction.horizon,
                    prediction.invalidation,
                    prediction.snapshot_json,
                    prediction.created_at,
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Record the review outcome of a prediction; returns whether it exists.
    pub async fn predictions_review(&self, id: &str, outcome: &str) -> Result<bool> {
        let (id, outcome) = (id.to_string(), outcome.to_string());
        self.run(move |conn| {
            let n = conn.execute(
                "UPDATE predictions SET outcome = ?2, reviewed_at = ?3 WHERE id = ?1",
                params![id, outcome, now_secs()],
            )?;
            Ok(n > 0)
        })
        .await
    }

    /// Load a prediction by id.
    pub async fn predictions_get(&self, id: &str) -> Result<Option<Prediction>> {
        let id = id.to_string();
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, symbol, thesis, expectation, probability, horizon,
                        invalidation, snapshot_json, created_at, outcome, reviewed_at
                 FROM predictions WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![id])?;
            Ok(match rows.next()? {
                Some(row) => Some(Prediction {
                    id: row.get(0)?,
                    symbol: row.get(1)?,
                    thesis: row.get(2)?,
                    expectation: row.get(3)?,
                    probability: row.get(4)?,
                    horizon: row.get(5)?,
                    invalidation: row.get(6)?,
                    snapshot_json: row.get(7)?,
                    created_at: row.get(8)?,
                    outcome: row.get(9)?,
                    reviewed_at: row.get(10)?,
                }),
                None => None,
            })
        })
        .await
    }

    // ------------------------------------------------------------------
    // reports
    // ------------------------------------------------------------------

    /// Insert a report.
    pub async fn reports_insert(&self, report: Report) -> Result<()> {
        self.run(move |conn| {
            conn.execute(
                "INSERT INTO reports (id, kind, title, content_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    report.id,
                    report.kind,
                    report.title,
                    report.content_json,
                    report.created_at
                ],
            )?;
            Ok(())
        })
        .await
    }

    // ------------------------------------------------------------------
    // supply-chain graph (migration v3)
    // ------------------------------------------------------------------

    /// Insert or update a graph node; bumps `updated_at` on conflict.
    pub async fn graph_node_upsert(&self, node: GraphNodeRow) -> Result<()> {
        self.run(move |conn| {
            conn.execute(
                "INSERT INTO graph_nodes (id, kind, name, code, meta_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                     kind = excluded.kind,
                     name = excluded.name,
                     code = excluded.code,
                     meta_json = excluded.meta_json,
                     updated_at = excluded.updated_at",
                params![
                    node.id,
                    node.kind,
                    node.name,
                    node.code,
                    node.meta_json,
                    node.created_at,
                    node.updated_at,
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Load a graph node by id.
    pub async fn graph_node_get(&self, id: &str) -> Result<Option<GraphNodeRow>> {
        let id = id.to_string();
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, kind, name, code, meta_json, created_at, updated_at
                 FROM graph_nodes WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![id])?;
            Ok(match rows.next()? {
                Some(row) => Some(graph_node_from_row(row)?),
                None => None,
            })
        })
        .await
    }

    /// All graph nodes (for in-memory analytics).
    pub async fn graph_nodes_all(&self) -> Result<Vec<GraphNodeRow>> {
        self.run(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, kind, name, code, meta_json, created_at, updated_at
                 FROM graph_nodes ORDER BY id",
            )?;
            let rows = stmt.query_map([], graph_node_from_row)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    /// Insert or update a graph edge (keyed by (src, dst, relation)); bumps
    /// `updated_at` on conflict and keeps the original row id.
    pub async fn graph_edge_upsert(&self, edge: GraphEdgeRow) -> Result<()> {
        self.run(move |conn| {
            conn.execute(
                "INSERT INTO graph_edges
                     (src, dst, relation, weight, source_name, source_url,
                      confidence, valid_from, valid_to, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(src, dst, relation) DO UPDATE SET
                     weight = excluded.weight,
                     source_name = excluded.source_name,
                     source_url = excluded.source_url,
                     confidence = excluded.confidence,
                     valid_from = excluded.valid_from,
                     valid_to = excluded.valid_to,
                     updated_at = excluded.updated_at",
                params![
                    edge.src,
                    edge.dst,
                    edge.relation,
                    edge.weight,
                    edge.source_name,
                    edge.source_url,
                    edge.confidence,
                    edge.valid_from,
                    edge.valid_to,
                    edge.created_at,
                    edge.updated_at,
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// All graph edges (for in-memory analytics).
    pub async fn graph_edges_all(&self) -> Result<Vec<GraphEdgeRow>> {
        self.run(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, src, dst, relation, weight, source_name, source_url,
                        confidence, valid_from, valid_to, created_at, updated_at
                 FROM graph_edges ORDER BY id",
            )?;
            let rows = stmt.query_map([], graph_edge_from_row)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    /// Edges touching `id` in either direction, each paired with the node at
    /// the other end (orphan endpoints are skipped).
    pub async fn graph_neighbors(&self, id: &str) -> Result<Vec<(GraphEdgeRow, GraphNodeRow)>> {
        let id = id.to_string();
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT e.id, e.src, e.dst, e.relation, e.weight, e.source_name,
                        e.source_url, e.confidence, e.valid_from, e.valid_to,
                        e.created_at, e.updated_at,
                        n.id, n.kind, n.name, n.code, n.meta_json,
                        n.created_at, n.updated_at
                 FROM graph_edges e
                 JOIN graph_nodes n
                   ON n.id = CASE WHEN e.src = ?1 THEN e.dst ELSE e.src END
                 WHERE e.src = ?1 OR e.dst = ?1
                 ORDER BY e.id",
            )?;
            let rows = stmt.query_map(params![id], |row| {
                Ok((
                    GraphEdgeRow {
                        id: Some(row.get(0)?),
                        src: row.get(1)?,
                        dst: row.get(2)?,
                        relation: row.get(3)?,
                        weight: row.get(4)?,
                        source_name: row.get(5)?,
                        source_url: row.get(6)?,
                        confidence: row.get(7)?,
                        valid_from: row.get(8)?,
                        valid_to: row.get(9)?,
                        created_at: row.get(10)?,
                        updated_at: row.get(11)?,
                    },
                    GraphNodeRow {
                        id: row.get(12)?,
                        kind: row.get(13)?,
                        name: row.get(14)?,
                        code: row.get(15)?,
                        meta_json: row.get(16)?,
                        created_at: row.get(17)?,
                        updated_at: row.get(18)?,
                    },
                ))
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    /// Edges with either endpoint in `ids` (batch fetch for traversal).
    pub async fn graph_edges_from(&self, ids: &[String]) -> Result<Vec<GraphEdgeRow>> {
        let ids = ids.to_vec();
        self.run(move |conn| {
            let mut out = Vec::new();
            let mut stmt = conn.prepare(
                "SELECT id, src, dst, relation, weight, source_name, source_url,
                        confidence, valid_from, valid_to, created_at, updated_at
                 FROM graph_edges WHERE src = ?1 OR dst = ?1 ORDER BY id",
            )?;
            for id in &ids {
                let rows = stmt.query_map(params![id], graph_edge_from_row)?;
                for row in rows {
                    let edge = row?;
                    if !out.iter().any(|e: &GraphEdgeRow| e.id == edge.id) {
                        out.push(edge);
                    }
                }
            }
            Ok(out)
        })
        .await
    }

    /// Insert an event (id conflict keeps the original row).
    pub async fn event_insert(&self, event: EventRow) -> Result<()> {
        self.run(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO events
                     (id, kind, title, subject, magnitude, direction, occurred_at,
                      source_name, source_url, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    event.id,
                    event.kind,
                    event.title,
                    event.subject,
                    event.magnitude,
                    event.direction,
                    event.occurred_at,
                    event.source_name,
                    event.source_url,
                    event.status,
                    event.created_at,
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// List events, most recent first, capped at `limit`.
    pub async fn event_list(&self, limit: u32) -> Result<Vec<EventRow>> {
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, kind, title, subject, magnitude, direction, occurred_at,
                        source_name, source_url, status, created_at
                 FROM events ORDER BY occurred_at DESC, rowid DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], |row| {
                Ok(EventRow {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    title: row.get(2)?,
                    subject: row.get(3)?,
                    magnitude: row.get(4)?,
                    direction: row.get(5)?,
                    occurred_at: row.get(6)?,
                    source_name: row.get(7)?,
                    source_url: row.get(8)?,
                    status: row.get(9)?,
                    created_at: row.get(10)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    // ------------------------------------------------------------------
    // parquet time-series
    // ------------------------------------------------------------------

    /// Load cached bars for `(symbol, period, adjust)`; empty when absent.
    /// Results are served from the LRU memory cache when hot.
    pub async fn load_bars(&self, symbol: &str, period: &str, adjust: &str) -> Result<Vec<BarRow>> {
        let key = format!("{symbol}|{period}|{adjust}");
        if let Some(bars) = self.inner.bars_mem.get(&key) {
            return Ok((*bars).clone());
        }
        let ts = self.inner.ts.clone();
        let (symbol, period, adjust) = owned3(symbol, period, adjust);
        let bars = tokio::task::spawn_blocking(move || ts.load_bars(&symbol, &period, &adjust))
            .await
            .map_err(|_| Error::WorkerClosed)??;
        self.inner.bars_mem.put(key, Arc::new(bars.clone()));
        Ok(bars)
    }

    /// Incrementally merge new bars into the cache (keyed by date, new rows
    /// override old, output sorted); returns the merged row count.
    pub async fn merge_and_write_bars(
        &self,
        symbol: &str,
        period: &str,
        adjust: &str,
        new_bars: Vec<BarRow>,
    ) -> Result<usize> {
        let key = format!("{symbol}|{period}|{adjust}");
        let ts = self.inner.ts.clone();
        let (symbol, period, adjust) = owned3(symbol, period, adjust);
        let len = tokio::task::spawn_blocking(move || {
            ts.merge_and_write_bars(&symbol, &period, &adjust, &new_bars)
        })
        .await
        .map_err(|_| Error::WorkerClosed)??;
        self.inner.bars_mem.invalidate(&key);
        Ok(len)
    }

    /// Latest cached bar date, for incremental-update decisions.
    pub async fn last_bar_date(
        &self,
        symbol: &str,
        period: &str,
        adjust: &str,
    ) -> Result<Option<NaiveDate>> {
        let ts = self.inner.ts.clone();
        let (symbol, period, adjust) = owned3(symbol, period, adjust);
        tokio::task::spawn_blocking(move || ts.last_bar_date(&symbol, &period, &adjust))
            .await
            .map_err(|_| Error::WorkerClosed)?
    }

    /// Load the fund-flow series for `symbol`; empty when absent.
    pub async fn load_fund_flow(&self, symbol: &str) -> Result<Vec<FundFlowRow>> {
        let ts = self.inner.ts.clone();
        let symbol = symbol.to_string();
        tokio::task::spawn_blocking(move || ts.load_fund_flow(&symbol))
            .await
            .map_err(|_| Error::WorkerClosed)?
    }

    /// Merge fund-flow rows keyed by date; returns the merged row count.
    pub async fn merge_and_write_fund_flow(
        &self,
        symbol: &str,
        rows: Vec<FundFlowRow>,
    ) -> Result<usize> {
        let ts = self.inner.ts.clone();
        let symbol = symbol.to_string();
        tokio::task::spawn_blocking(move || ts.merge_and_write_fund_flow(&symbol, &rows))
            .await
            .map_err(|_| Error::WorkerClosed)?
    }

    // ------------------------------------------------------------------
    // corporate actions (migration v4, data-foundation-v2)
    // ------------------------------------------------------------------

    /// Upsert corporate actions in one transaction (keyed by
    /// `(code, ex_date)`); returns the number of rows written.
    pub async fn corp_actions_upsert_batch(&self, rows: Vec<CorporateActionRow>) -> Result<usize> {
        let n = rows.len();
        self.run(move |conn| {
            let tx = conn.unchecked_transaction()?;
            {
                let mut stmt = tx.prepare(
                    "INSERT OR REPLACE INTO corporate_actions
                     (code, ex_date, notice_date, cash_div, bonus_share,
                      rights_ratio, rights_price, source, source_url, fetched_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                )?;
                for row in &rows {
                    stmt.execute(params![
                        row.code,
                        row.ex_date.to_string(),
                        row.notice_date.map(|d| d.to_string()),
                        row.cash_div,
                        row.bonus_share,
                        row.rights_ratio,
                        row.rights_price,
                        row.source,
                        row.source_url,
                        row.fetched_at,
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
        Ok(n)
    }

    /// Load all corporate actions for `code`, ascending ex-date.
    pub async fn corp_actions_load(&self, code: &str) -> Result<Vec<CorporateActionRow>> {
        let code = code.to_string();
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT code, ex_date, notice_date, cash_div, bonus_share,
                        rights_ratio, rights_price, source, source_url, fetched_at
                 FROM corporate_actions WHERE code = ?1 ORDER BY ex_date ASC",
            )?;
            let rows = stmt.query_map(params![code], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, Option<f64>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (
                    code,
                    ex_date,
                    notice_date,
                    cash_div,
                    bonus_share,
                    rights_ratio,
                    rights_price,
                    source,
                    source_url,
                    fetched_at,
                ) = row?;
                out.push(CorporateActionRow {
                    code,
                    ex_date: parse_stored_date(&ex_date)?,
                    notice_date: notice_date.map(|s| parse_stored_date(&s)).transpose()?,
                    cash_div,
                    bonus_share,
                    rights_ratio,
                    rights_price,
                    source,
                    source_url,
                    fetched_at,
                });
            }
            Ok(out)
        })
        .await
    }

    /// Latest ex-date stored for `code`, for incremental fetches
    /// (pull actions with `ex_date >` this).
    pub async fn corp_actions_latest_date(&self, code: &str) -> Result<Option<NaiveDate>> {
        let code = code.to_string();
        self.run(move |conn| {
            let mut stmt =
                conn.prepare("SELECT MAX(ex_date) FROM corporate_actions WHERE code = ?1")?;
            let latest: Option<String> = stmt.query_row(params![code], |row| row.get(0))?;
            latest.map(|s| parse_stored_date(&s)).transpose()
        })
        .await
    }

    /// Load raw bars from the `raw` parquet partition, join them with the
    /// stored corporate actions, and apply the core adjustment engine
    /// (data-foundation-v2 §数据管线 step 3: runtime adjustment; the store
    /// itself stays raw-only).
    ///
    /// `anchor` is only used for `Adjust::Qfq` (hfq anchors at the last bar,
    /// raw is a passthrough). Volume/amount/turnover pass through unscaled,
    /// per the spec (only prices are adjusted).
    pub async fn load_bars_adjusted(
        &self,
        symbol: &str,
        period: &str,
        kind: astock_core::Adjust,
        anchor: NaiveDate,
    ) -> Result<AdjustedBars> {
        let raw = self.load_bars(symbol, period, "raw").await?;
        let action_rows = self.corp_actions_load(symbol).await?;
        let actions: Vec<astock_core::CorporateAction> =
            action_rows.iter().map(Into::into).collect();
        // BarRow mirrors core `Bar` field-by-field; the volume unit is
        // irrelevant here because adjustment only scales prices and the rows
        // are converted straight back.
        let core_bars: Vec<astock_core::Bar> = raw
            .iter()
            .map(|b| {
                let mut bar = astock_core::Bar::new(
                    b.date,
                    b.open,
                    b.close,
                    b.high,
                    b.low,
                    b.volume,
                    astock_core::VolumeUnit::Lots,
                );
                bar.amount = b.amount;
                bar.turnover = b.turnover;
                bar
            })
            .collect();
        let adjusted = astock_core::apply_adjustment(&core_bars, &actions, kind, anchor, None);
        let bars = raw
            .into_iter()
            .zip(adjusted.bars)
            .map(|(row, adj)| BarRow {
                open: adj.open,
                high: adj.high,
                low: adj.low,
                close: adj.close,
                ..row
            })
            .collect();
        Ok(AdjustedBars {
            bars,
            warnings: adjusted.warnings,
        })
    }

    // ------------------------------------------------------------------
    // disk management
    // ------------------------------------------------------------------

    /// On-disk size breakdown by category.
    pub async fn cache_stats(&self) -> Result<CacheStats> {
        let ts = self.inner.ts.clone();
        let db_path = self.inner.config.meta_db_path();
        let (kline_parquet_bytes, kline_parquet_files, sqlite_bytes) =
            tokio::task::spawn_blocking(move || {
                let files = ts.parquet_files();
                let size = maintenance::files_size(&files);
                let count = files.len() as u64;
                let db_size = maintenance::sqlite_file_size(&db_path)?;
                Ok::<_, Error>((size, count, db_size))
            })
            .await
            .map_err(|_| Error::WorkerClosed)??;
        let (tool_cache_rows, tool_cache_bytes, chat_bytes) = self
            .run(|conn| {
                let (rows, bytes): (i64, i64) = conn.query_row(
                    "SELECT COUNT(*), COALESCE(SUM(LENGTH(params_json) + LENGTH(result_json)), 0)
                     FROM tool_cache",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let chat: i64 = conn.query_row(
                    "SELECT COALESCE(SUM(LENGTH(content) + COALESCE(LENGTH(tool_calls), 0)), 0)
                     FROM messages",
                    [],
                    |row| row.get(0),
                )?;
                Ok((rows as u64, bytes as u64, chat as u64))
            })
            .await?;
        Ok(CacheStats {
            kline_parquet_bytes,
            kline_parquet_files,
            sqlite_bytes,
            tool_cache_rows,
            tool_cache_bytes,
            chat_bytes,
        })
    }

    /// Evict expired tool-cache rows, then least-recently-used parquet files,
    /// until the total size is under `policy.target_total_bytes`.
    pub async fn cleanup(&self, policy: CleanupPolicy) -> Result<CleanupReport> {
        let now = now_secs();
        let tool_cache_rows_deleted = self
            .run(move |conn| {
                let n = conn.execute(
                    "DELETE FROM tool_cache WHERE created_at + ttl_seconds <= ?1",
                    params![now],
                )?;
                Ok(n as u64)
            })
            .await?;
        self.inner.tool_mem.clear();

        let mut report = CleanupReport {
            tool_cache_rows_deleted,
            ..Default::default()
        };
        let stats = self.cache_stats().await?;
        if stats.total_bytes() <= policy.target_total_bytes {
            return Ok(report);
        }
        let bytes_to_free = stats.total_bytes() - policy.target_total_bytes;
        let ts = self.inner.ts.clone();
        let (deleted, freed) = tokio::task::spawn_blocking(move || {
            let files = ts.parquet_files();
            let evict = maintenance::select_evictions(&files, bytes_to_free);
            let mut freed = 0u64;
            let mut deleted = 0u64;
            for path in &evict {
                if let Ok(meta) = std::fs::metadata(path) {
                    if std::fs::remove_file(path).is_ok() {
                        freed += meta.len();
                        deleted += 1;
                    }
                }
            }
            (deleted, freed)
        })
        .await
        .map_err(|_| Error::WorkerClosed)?;
        report.parquet_files_deleted = deleted;
        report.bytes_freed = freed;
        self.inner.bars_mem.clear();
        Ok(report)
    }

    /// Free bytes on the volume holding `base_dir`; see [`disk_free_bytes`].
    pub fn disk_free_bytes(&self) -> Option<u64> {
        disk_free_bytes(&self.inner.config.base_dir)
    }

    /// Base directory of this storage.
    pub fn base_dir(&self) -> PathBuf {
        self.inner.config.base_dir.clone()
    }
}

fn owned3(a: &str, b: &str, c: &str) -> (String, String, String) {
    (a.to_string(), b.to_string(), c.to_string())
}

/// Parse a stored `YYYY-MM-DD` date (corporate_actions columns are TEXT).
fn parse_stored_date(s: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|e| Error::Invalid(format!("bad stored date {s:?}: {e}")))
}

/// Map a `graph_nodes` SELECT row (id, kind, name, code, meta_json,
/// created_at, updated_at) to a [`GraphNodeRow`].
fn graph_node_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphNodeRow> {
    Ok(GraphNodeRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        name: row.get(2)?,
        code: row.get(3)?,
        meta_json: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

/// Map a `graph_edges` SELECT row (id, src, dst, relation, weight,
/// source_name, source_url, confidence, valid_from, valid_to, created_at,
/// updated_at) to a [`GraphEdgeRow`].
fn graph_edge_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphEdgeRow> {
    Ok(GraphEdgeRow {
        id: Some(row.get(0)?),
        src: row.get(1)?,
        dst: row.get(2)?,
        relation: row.get(3)?,
        weight: row.get(4)?,
        source_name: row.get(5)?,
        source_url: row.get(6)?,
        confidence: row.get(7)?,
        valid_from: row.get(8)?,
        valid_to: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_storage() -> (tempfile::TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        (dir, storage)
    }

    fn bar(date: &str, close: f64) -> BarRow {
        BarRow {
            date: NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
            open: close - 0.5,
            high: close + 0.5,
            low: close - 1.0,
            close,
            volume: 1_000_000.0,
            amount: Some(close * 1_000_000.0),
            turnover: None,
            source: "test".into(),
            fetched_at: 1_700_000_000,
        }
    }

    fn cache_entry(key: &str, ttl: i64) -> ToolCacheEntry {
        ToolCacheEntry {
            cache_key: key.into(),
            tool: "search".into(),
            params_json: "{}".into(),
            result_json: "{\"hits\":[]}".into(),
            data_version: None,
            created_at: now_secs(),
            ttl_seconds: ttl,
            accessed_at: now_secs(),
        }
    }

    #[tokio::test]
    async fn security_master_roundtrip_preserves_identity_and_lineage() {
        let (_dir, storage) = test_storage();
        let mut record = SecurityMasterRecord::listed_stock("300308", "中际旭创", "tdx");
        record.refreshed_at =
            chrono::DateTime::from_timestamp(record.refreshed_at.timestamp(), 0).unwrap();
        record.aliases = vec!["中际旭创股份".into()];
        record.industry = Some("通信设备".into());
        record.concepts = vec!["光模块".into(), "CPO".into()];
        record.region = Some("山东".into());
        storage
            .securities_upsert(vec![record.clone()])
            .await
            .unwrap();

        let rows = storage.securities_list().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], record);
    }

    #[tokio::test]
    async fn tool_cache_ttl_semantics() {
        let (_dir, storage) = test_storage();
        storage
            .tool_cache_put(cache_entry("fresh", 3600))
            .await
            .unwrap();
        let hit = storage.tool_cache_get("fresh").await.unwrap().unwrap();
        assert_eq!(hit.tool, "search");
        // accessed_at is bumped to now on a db hit.
        assert!(hit.accessed_at >= hit.created_at);

        storage
            .tool_cache_put(cache_entry("stale", 0))
            .await
            .unwrap();
        assert!(storage.tool_cache_get("stale").await.unwrap().is_none());
        assert!(storage.tool_cache_get("missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn watchlist_crud() {
        let (_dir, storage) = test_storage();
        storage.watchlist_add("core", "600519").await.unwrap();
        storage.watchlist_add("core", "000001").await.unwrap();
        storage.watchlist_add("core", "600519").await.unwrap(); // idempotent
        assert!(storage
            .watchlist_set_pinned("core", "000001", true)
            .await
            .unwrap());

        let items = storage.watchlist_list("core").await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].code, "000001"); // pinned first
        assert!(items[0].pinned);
        assert_eq!(items[1].code, "600519");

        assert!(storage.watchlist_remove("core", "600519").await.unwrap());
        assert!(!storage.watchlist_remove("core", "600519").await.unwrap());
        assert_eq!(storage.watchlist_list("core").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn conversation_append_and_load() {
        let (_dir, storage) = test_storage();
        storage
            .conversation_create("c1", Some("demo"))
            .await
            .unwrap();
        for (i, role) in ["user", "assistant", "user"].iter().enumerate() {
            storage
                .conversation_append(ChatMessage {
                    id: format!("m{i}"),
                    conversation_id: "c1".into(),
                    role: role.to_string(),
                    content: format!("message {i}"),
                    tool_calls: None,
                    created_at: 1_700_000_000 + i as i64,
                })
                .await
                .unwrap();
        }
        let messages = storage.conversation_load("c1").await.unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[2].content, "message 2");
    }

    #[tokio::test]
    async fn conversation_list_returns_headers_newest_first() {
        let (_dir, storage) = test_storage();
        storage
            .conversation_create("c1", Some("demo"))
            .await
            .unwrap();
        storage.conversation_create("c2", None).await.unwrap();
        // INSERT OR IGNORE: re-creating keeps the original row.
        storage
            .conversation_create("c1", Some("dup"))
            .await
            .unwrap();

        let list = storage.conversation_list().await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "c2");
        assert_eq!(list[0].title, None);
        assert_eq!(list[1].id, "c1");
        assert_eq!(list[1].title.as_deref(), Some("demo"));
    }

    #[tokio::test]
    async fn predictions_lifecycle() {
        let (_dir, storage) = test_storage();
        storage
            .predictions_insert(Prediction {
                id: "p1".into(),
                symbol: "600519".into(),
                thesis: "strong Q3 earnings".into(),
                expectation: Some("+10% in 3m".into()),
                probability: Some(0.65),
                horizon: Some("3m".into()),
                invalidation: Some("earnings miss".into()),
                snapshot_json: Some("{\"close\":1800}".into()),
                created_at: now_secs(),
                outcome: None,
                reviewed_at: None,
            })
            .await
            .unwrap();
        let p = storage.predictions_get("p1").await.unwrap().unwrap();
        assert!(p.outcome.is_none());
        assert!(storage.predictions_review("p1", "hit").await.unwrap());
        let p = storage.predictions_get("p1").await.unwrap().unwrap();
        assert_eq!(p.outcome.as_deref(), Some("hit"));
        assert!(p.reviewed_at.is_some());
        assert!(!storage.predictions_review("nope", "hit").await.unwrap());
    }

    #[tokio::test]
    async fn agent_task_crud() {
        let (_dir, storage) = test_storage();
        let task = AgentTask {
            id: "t1".into(),
            kind: "analysis".into(),
            status: "running".into(),
            state_json: "{\"round\":0}".into(),
            created_at: now_secs(),
            updated_at: now_secs(),
        };
        storage.agent_task_save(task).await.unwrap();
        let loaded = storage.agent_task_get("t1").await.unwrap().unwrap();
        assert_eq!(loaded.status, "running");

        // Upsert updates status/state but keeps the row singular.
        let mut updated = loaded.clone();
        updated.status = "suspended".into();
        updated.state_json = "{\"round\":3}".into();
        updated.updated_at = now_secs();
        storage.agent_task_save(updated).await.unwrap();
        let loaded = storage.agent_task_get("t1").await.unwrap().unwrap();
        assert_eq!(loaded.status, "suspended");
        assert_eq!(loaded.state_json, "{\"round\":3}");

        assert!(storage.agent_task_get("missing").await.unwrap().is_none());
        let list = storage.agent_task_list().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "t1");
    }

    #[tokio::test]
    async fn agent_tool_audit_is_append_only_metadata() {
        let (_dir, storage) = test_storage();
        for event in ["requested", "succeeded"] {
            storage
                .agent_tool_audit_append(AgentToolAudit {
                    id: None,
                    task_id: "t1".into(),
                    call_id: "call-1".into(),
                    tool: "search_web".into(),
                    permission_domain: "read_only_network".into(),
                    origin: "model_plan".into(),
                    args_fingerprint: "sha256:opaque".into(),
                    event: event.into(),
                    elapsed_ms: (event == "succeeded").then_some(123),
                    created_at: now_secs(),
                })
                .await
                .unwrap();
        }
        let rows = storage.agent_tool_audit_list("t1").await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].event, "requested");
        assert_eq!(rows[1].elapsed_ms, Some(123));
        assert!(storage
            .agent_tool_audit_list("other")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn parquet_roundtrip_and_merge_dedupe() {
        let (_dir, storage) = test_storage();
        let first = vec![bar("2025-01-02", 10.0), bar("2025-01-03", 11.0)];
        let len = storage
            .merge_and_write_bars("600519", "day", "qfq", first)
            .await
            .unwrap();
        assert_eq!(len, 2);

        // Overlapping merge: 2025-01-03 is replaced, 2025-01-06 appended.
        let second = vec![bar("2025-01-03", 11.5), bar("2025-01-06", 12.0)];
        let len = storage
            .merge_and_write_bars("600519", "day", "qfq", second)
            .await
            .unwrap();
        assert_eq!(len, 3);

        let bars = storage.load_bars("600519", "day", "qfq").await.unwrap();
        assert_eq!(bars.len(), 3);
        assert_eq!(bars[0].date.to_string(), "2025-01-02");
        assert_eq!(bars[1].close, 11.5); // dedupe keeps the new row
        assert_eq!(bars[2].date.to_string(), "2025-01-06");
        assert_eq!(bars[0].amount, Some(10.0 * 1_000_000.0));
        assert_eq!(bars[0].turnover, None);
        assert_eq!(bars[0].source, "test");

        let last = storage.last_bar_date("600519", "day", "qfq").await.unwrap();
        assert_eq!(last.unwrap().to_string(), "2025-01-06");
        assert!(storage
            .last_bar_date("000001", "day", "qfq")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn fund_flow_roundtrip() {
        let (_dir, storage) = test_storage();
        let rows = vec![
            FundFlowRow {
                date: NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(),
                main_net_inflow: Some(1.2e8),
                super_large: Some(8e7),
                large: Some(4e7),
                medium: None,
                small: None,
                source: "test".into(),
                fetched_at: 1_700_000_000,
            },
            FundFlowRow {
                date: NaiveDate::from_ymd_opt(2025, 1, 3).unwrap(),
                main_net_inflow: Some(-5e7),
                super_large: None,
                large: None,
                medium: None,
                small: None,
                source: "test".into(),
                fetched_at: 1_700_000_000,
            },
        ];
        assert_eq!(
            storage
                .merge_and_write_fund_flow("600519", rows)
                .await
                .unwrap(),
            2
        );
        let loaded = storage.load_fund_flow("600519").await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].main_net_inflow, Some(1.2e8));
        assert_eq!(loaded[0].medium, None);
    }

    #[tokio::test]
    async fn cleanup_evicts_expired_rows_and_parquet() {
        let (_dir, storage) = test_storage();
        storage.tool_cache_put(cache_entry("old", 0)).await.unwrap();
        storage
            .tool_cache_put(cache_entry("new", 86_400))
            .await
            .unwrap();
        let bars: Vec<BarRow> = (1..=20)
            .map(|d| bar(&format!("2025-01-{d:02}"), 10.0 + d as f64))
            .collect();
        storage
            .merge_and_write_bars("600519", "day", "qfq", bars)
            .await
            .unwrap();

        let before = storage.cache_stats().await.unwrap();
        assert_eq!(before.tool_cache_rows, 2);
        assert_eq!(before.kline_parquet_files, 1);
        assert!(before.kline_parquet_bytes > 0);

        // Target zero: everything evictable must go.
        let report = storage
            .cleanup(CleanupPolicy {
                target_total_bytes: 0,
            })
            .await
            .unwrap();
        assert_eq!(report.tool_cache_rows_deleted, 1); // only the expired one
        assert_eq!(report.parquet_files_deleted, 1);
        assert!(report.bytes_freed > 0);

        let after = storage.cache_stats().await.unwrap();
        assert_eq!(after.tool_cache_rows, 1);
        assert_eq!(after.kline_parquet_files, 0);
        assert!(after.total_bytes() < before.total_bytes());
        assert!(storage
            .load_bars("600519", "day", "qfq")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn settings_roundtrip_and_stats() {
        let (_dir, storage) = test_storage();
        assert!(storage.settings_get("theme").await.unwrap().is_none());
        storage.settings_set("theme", "dark").await.unwrap();
        assert_eq!(
            storage.settings_get("theme").await.unwrap().as_deref(),
            Some("dark")
        );
        let stats = storage.cache_stats().await.unwrap();
        assert!(stats.sqlite_bytes > 0);
    }

    #[tokio::test]
    async fn kv_set_get_delete_roundtrip() {
        let (_dir, storage) = test_storage();
        assert!(storage
            .kv_get("provider.tushare_token")
            .await
            .unwrap()
            .is_none());

        let before = now_secs();
        storage
            .kv_set("provider.tushare_token", "abc123")
            .await
            .unwrap();
        let entry = storage
            .kv_get("provider.tushare_token")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.key, "provider.tushare_token");
        assert_eq!(entry.value, "abc123");
        assert!(entry.fetched_at >= before);

        // Overwrite replaces the value and re-stamps fetched_at.
        storage
            .kv_set("provider.tushare_token", "def456")
            .await
            .unwrap();
        let entry = storage
            .kv_get("provider.tushare_token")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.value, "def456");

        assert!(storage.kv_delete("provider.tushare_token").await.unwrap());
        assert!(!storage.kv_delete("provider.tushare_token").await.unwrap());
        assert!(storage
            .kv_get("provider.tushare_token")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn kv_list_prefix_filters_and_orders() {
        let (_dir, storage) = test_storage();
        storage
            .kv_set("provider.socks5", "127.0.0.1:1080")
            .await
            .unwrap();
        storage
            .kv_set("provider.jq_user", "13800000000")
            .await
            .unwrap();
        storage.kv_set("provider.jq_pwd", "c2VjcmV0").await.unwrap();
        storage.kv_set("theme", "dark").await.unwrap();
        // Prefix matching must be exact, not LIKE-wildcard-based.
        storage.kv_set("providerX.other", "nope").await.unwrap();

        let entries = storage.kv_list_prefix("provider.").await.unwrap();
        let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["provider.jq_pwd", "provider.jq_user", "provider.socks5"]
        );
        assert!(entries.iter().all(|e| e.fetched_at > 0));

        assert!(storage
            .kv_list_prefix("nonexistent.")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn graph_node_edge_crud() {
        let (_dir, storage) = test_storage();
        let node = |id: &str, kind: &str, name: &str, code: Option<&str>| GraphNodeRow {
            id: id.into(),
            kind: kind.into(),
            name: name.into(),
            code: code.map(str::to_string),
            meta_json: "{}".into(),
            created_at: now_secs(),
            updated_at: now_secs(),
        };
        storage
            .graph_node_upsert(node("commodity:copper", "commodity", "铜", None))
            .await
            .unwrap();
        storage
            .graph_node_upsert(node(
                "company:600362",
                "company",
                "江西铜业",
                Some("600362"),
            ))
            .await
            .unwrap();
        storage
            .graph_node_upsert(node(
                "company:600869",
                "company",
                "远东股份",
                Some("600869"),
            ))
            .await
            .unwrap();

        let loaded = storage
            .graph_node_get("company:600362")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.name, "江西铜业");
        assert_eq!(loaded.code.as_deref(), Some("600362"));
        assert!(storage.graph_node_get("missing").await.unwrap().is_none());

        // Upsert on conflict updates fields, keeps one row.
        let mut renamed = loaded.clone();
        renamed.name = "江西铜业股份".into();
        storage.graph_node_upsert(renamed).await.unwrap();
        assert_eq!(storage.graph_nodes_all().await.unwrap().len(), 3);
        assert_eq!(
            storage
                .graph_node_get("company:600362")
                .await
                .unwrap()
                .unwrap()
                .name,
            "江西铜业股份"
        );

        let edge = |src: &str, dst: &str, relation: &str| GraphEdgeRow {
            id: None,
            src: src.into(),
            dst: dst.into(),
            relation: relation.into(),
            weight: 0.8,
            source_name: "公司年报2024".into(),
            source_url: "https://example.com/report".into(),
            confidence: 0.85,
            valid_from: 0,
            valid_to: None,
            created_at: now_secs(),
            updated_at: now_secs(),
        };
        storage
            .graph_edge_upsert(edge("company:600362", "commodity:copper", "produces"))
            .await
            .unwrap();
        storage
            .graph_edge_upsert(edge("company:600869", "commodity:copper", "consumes"))
            .await
            .unwrap();

        // Upsert on (src, dst, relation) conflict updates, no duplicate.
        let mut stronger = edge("company:600362", "commodity:copper", "produces");
        stronger.weight = 0.9;
        storage.graph_edge_upsert(stronger).await.unwrap();
        let all = storage.graph_edges_all().await.unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[0].id.is_some());
        assert_eq!(
            all.iter()
                .find(|e| e.relation == "produces")
                .unwrap()
                .weight,
            0.9
        );

        // Neighbors of the commodity: both companies, other-end node joined.
        let neighbors = storage.graph_neighbors("commodity:copper").await.unwrap();
        assert_eq!(neighbors.len(), 2);
        let names: Vec<&str> = neighbors.iter().map(|(_, n)| n.name.as_str()).collect();
        assert!(names.contains(&"江西铜业股份"));
        assert!(names.contains(&"远东股份"));

        // Batch edge fetch dedupes shared edges.
        let batch = storage
            .graph_edges_from(&["commodity:copper".into(), "company:600362".into()])
            .await
            .unwrap();
        assert_eq!(batch.len(), 2);
    }

    #[tokio::test]
    async fn event_insert_and_list() {
        let (_dir, storage) = test_storage();
        let event = |id: &str, occurred_at: i64| EventRow {
            id: id.into(),
            kind: "commodity_price".into(),
            title: "铜价上涨10%".into(),
            subject: "commodity:copper".into(),
            magnitude: Some(0.10),
            direction: 1,
            occurred_at,
            source_name: "上海有色网".into(),
            source_url: "https://example.com/cu".into(),
            status: "new".into(),
            created_at: now_secs(),
        };
        storage
            .event_insert(event("e1", 1_700_000_000))
            .await
            .unwrap();
        storage
            .event_insert(event("e2", 1_700_100_000))
            .await
            .unwrap();
        // INSERT OR IGNORE: duplicate id keeps the original row.
        storage
            .event_insert(event("e1", 1_799_999_999))
            .await
            .unwrap();

        let list = storage.event_list(10).await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "e2"); // newest occurred_at first
        assert_eq!(list[1].id, "e1");
        assert_eq!(list[1].direction, 1);
        assert_eq!(list[1].magnitude, Some(0.10));

        let limited = storage.event_list(1).await.unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].id, "e2");
    }

    #[test]
    fn disk_free_bytes_is_sane() {
        let dir = tempfile::tempdir().unwrap();
        if let Some(free) = disk_free_bytes(dir.path()) {
            assert!(free > 0);
        }
        // None is the documented "unknown" fallback.
    }

    fn action_row(
        code: &str,
        ex_date: &str,
        cash_div: f64,
        bonus_share: f64,
    ) -> CorporateActionRow {
        CorporateActionRow {
            code: code.into(),
            ex_date: NaiveDate::parse_from_str(ex_date, "%Y-%m-%d").unwrap(),
            notice_date: None,
            cash_div,
            bonus_share,
            rights_ratio: 0.0,
            rights_price: None,
            source: "test".into(),
            source_url: "https://example.com/ca".into(),
            fetched_at: 1_700_000_000,
        }
    }

    #[tokio::test]
    async fn corp_actions_crud() {
        let (_dir, storage) = test_storage();
        assert!(storage
            .corp_actions_load("600519")
            .await
            .unwrap()
            .is_empty());
        assert!(storage
            .corp_actions_latest_date("600519")
            .await
            .unwrap()
            .is_none());

        let rows = vec![
            action_row("600519", "2025-06-25", 2.8, 0.0),
            action_row("600519", "2024-06-28", 3.0, 0.0),
            action_row("000001", "2025-05-10", 0.5, 0.0),
        ];
        assert_eq!(storage.corp_actions_upsert_batch(rows).await.unwrap(), 3);

        // Ascending ex-date, filtered by code.
        let loaded = storage.corp_actions_load("600519").await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].ex_date.to_string(), "2024-06-28");
        assert_eq!(loaded[1].ex_date.to_string(), "2025-06-25");
        assert_eq!(loaded[1].cash_div, 2.8);
        assert_eq!(loaded[1].source, "test");

        let latest = storage
            .corp_actions_latest_date("600519")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(latest.to_string(), "2025-06-25");

        // Upsert on (code, ex_date) conflict replaces the row.
        let mut updated = action_row("600519", "2025-06-25", 2.9, 0.0);
        updated.notice_date = Some(NaiveDate::from_ymd_opt(2025, 6, 10).unwrap());
        storage
            .corp_actions_upsert_batch(vec![updated])
            .await
            .unwrap();
        let loaded = storage.corp_actions_load("600519").await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[1].cash_div, 2.9);
        assert_eq!(loaded[1].notice_date.unwrap().to_string(), "2025-06-10");
    }

    /// End-to-end data-foundation-v2 read path: raw parquet rows + stored
    /// actions -> runtime qfq/hfq. Uses the 20 -> 10送10 -> 10 golden.
    #[tokio::test]
    async fn load_bars_adjusted_applies_core_engine() {
        let (_dir, storage) = test_storage();
        let raw = vec![
            bar("2025-01-06", 20.0),
            bar("2025-01-07", 10.0), // ex-date of a 10送10
            bar("2025-01-08", 10.5),
        ];
        storage
            .merge_and_write_bars("600519", "day", "raw", raw)
            .await
            .unwrap();
        storage
            .corp_actions_upsert_batch(vec![action_row("600519", "2025-01-07", 0.0, 1.0)])
            .await
            .unwrap();

        let anchor = NaiveDate::from_ymd_opt(2025, 1, 8).unwrap();
        let qfq = storage
            .load_bars_adjusted("600519", "day", astock_core::Adjust::Qfq, anchor)
            .await
            .unwrap();
        assert!(qfq.warnings.is_empty());
        let closes: Vec<f64> = qfq.bars.iter().map(|b| b.close).collect();
        assert_eq!(closes, vec![10.0, 10.0, 10.5]);
        // Provenance survives the conversion round-trip.
        assert_eq!(qfq.bars[0].source, "test");
        assert_eq!(qfq.bars[0].fetched_at, 1_700_000_000);
        // Volume is not adjusted (spec: only prices).
        assert_eq!(qfq.bars[0].volume, 1_000_000.0);

        let hfq = storage
            .load_bars_adjusted("600519", "day", astock_core::Adjust::Hfq, anchor)
            .await
            .unwrap();
        let closes: Vec<f64> = hfq.bars.iter().map(|b| b.close).collect();
        assert_eq!(closes, vec![20.0, 20.0, 21.0]);

        let raw_out = storage
            .load_bars_adjusted("600519", "day", astock_core::Adjust::None, anchor)
            .await
            .unwrap();
        let closes: Vec<f64> = raw_out.bars.iter().map(|b| b.close).collect();
        assert_eq!(closes, vec![20.0, 10.0, 10.5]);
    }
}
