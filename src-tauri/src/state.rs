//! Shared application state managed by Tauri.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use astock_fundamental::FundamentalClient;
use astock_graph::GraphStore;
use astock_market_data::{EastMoneyF10, MarketData};
use astock_minimax::MinimaxClient;
use astock_storage::{Storage, StorageConfig};
use astock_trading_rules::RuleSet;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::commands::scan::ScanHit;
use crate::error::CmdError;

/// Live snapshot of the market scan, returned by `scan_status`.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ScanSnapshot {
    /// Whether a scan task is currently running.
    pub running: bool,
    /// Stocks processed so far.
    pub done: u32,
    /// Total stocks to process this run.
    pub total: u32,
    /// Symbol most recently completed.
    pub current_symbol: String,
    /// Kept buy-signal hits, unsorted while the scan runs.
    pub results: Vec<ScanHit>,
}

/// Mutable scan coordination state: status snapshot + cancellation.
pub struct ScanState {
    /// Status snapshot polled by `scan_status` and streamed via events.
    pub snapshot: Mutex<ScanSnapshot>,
    /// Cancellation token of the running scan, if any.
    pub cancel: Mutex<Option<CancellationToken>>,
}

/// Pollable background-backtest snapshot. Results remain available when the
/// user navigates away from the lab and returns later in the same app run.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BacktestSnapshot {
    pub job_id: Option<String>,
    pub status: String,
    pub phase: String,
    pub progress: Option<u8>,
    pub started_at: Option<i64>,
    pub updated_at: i64,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
}

impl Default for BacktestSnapshot {
    fn default() -> Self {
        Self {
            job_id: None,
            status: "idle".into(),
            phase: "尚未运行".into(),
            progress: None,
            started_at: None,
            updated_at: 0,
            result: None,
            error: None,
        }
    }
}

pub struct BacktestState {
    pub snapshot: Mutex<BacktestSnapshot>,
    pub cancel: Mutex<Option<CancellationToken>>,
}

/// Detailed, pollable formal-disclosure synchronization state. It survives
/// page switches and exposes enough information to diagnose slow providers.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DisclosureSyncSnapshot {
    pub job_id: Option<String>,
    pub running: bool,
    pub status: String,
    pub phase: String,
    pub progress: u8,
    pub current_provider: String,
    pub current_item: String,
    pub discovered: u32,
    pub normalized: u32,
    pub inserted: u32,
    pub deduplicated: u32,
    pub primary_verified: u32,
    pub needs_review: u32,
    pub failures: u32,
    pub estimated_remaining_seconds: Option<u32>,
    pub recent_logs: Vec<String>,
    pub started_at: Option<i64>,
    pub updated_at: i64,
    pub error: Option<String>,
}

impl Default for DisclosureSyncSnapshot {
    fn default() -> Self {
        Self {
            job_id: None,
            running: false,
            status: "idle".into(),
            phase: "尚未同步".into(),
            progress: 0,
            current_provider: String::new(),
            current_item: String::new(),
            discovered: 0,
            normalized: 0,
            inserted: 0,
            deduplicated: 0,
            primary_verified: 0,
            needs_review: 0,
            failures: 0,
            estimated_remaining_seconds: None,
            recent_logs: Vec::new(),
            started_at: None,
            updated_at: 0,
            error: None,
        }
    }
}

pub struct DisclosureSyncState {
    pub snapshot: Mutex<DisclosureSyncSnapshot>,
    pub cancel: Mutex<Option<CancellationToken>>,
}

impl Default for DisclosureSyncState {
    fn default() -> Self {
        Self {
            snapshot: Mutex::new(DisclosureSyncSnapshot::default()),
            cancel: Mutex::new(None),
        }
    }
}

impl Default for BacktestState {
    fn default() -> Self {
        Self {
            snapshot: Mutex::new(BacktestSnapshot::default()),
            cancel: Mutex::new(None),
        }
    }
}

impl Default for ScanState {
    fn default() -> Self {
        ScanState {
            snapshot: Mutex::new(ScanSnapshot::default()),
            cancel: Mutex::new(None),
        }
    }
}

impl ScanState {
    /// Whether a scan is currently running.
    pub fn is_running(&self) -> bool {
        self.snapshot
            .lock()
            .expect("scan snapshot poisoned")
            .running
    }
}

/// State managed by Tauri and injected into every command.
///
/// `market` and `scan` are `Arc` so the scan task can own clones; the
/// MiniMax client is built lazily behind a lock because it requires the
/// API key from the OS credential store.
pub struct AppState {
    /// Composite market-data facade (kline failover + EastMoney endpoints).
    pub market: Arc<MarketData>,
    /// Fundamental-data client (EastMoney F10), sharing the market stack's
    /// HTTP client and TTL cache. Behind `Arc` so the agent `ToolContext`
    /// can share the same client.
    pub fundamental: Arc<FundamentalClient>,
    /// Supply-chain knowledge graph over the shared storage (seeded at
    /// startup; see [`AppState::init`]).
    pub graph: GraphStore,
    /// Tiered local persistence (SQLite + parquet + mem cache).
    pub storage: Storage,
    /// Trading-calendar / fee / board rules.
    pub rules: RuleSet,
    /// Lazily constructed MiniMax client (requires a stored API key).
    /// Behind `Arc` so agent tasks can share it as a `ChatBackend`.
    pub minimax: RwLock<Option<Arc<MinimaxClient>>>,
    /// Scan coordination state.
    pub scan: Arc<ScanState>,
    /// Background backtest coordination state.
    pub backtest: Arc<BacktestState>,
    /// Background formal-disclosure synchronization and diagnostics.
    pub disclosure_sync: Arc<DisclosureSyncState>,
    /// Live agent event-forwarder tasks, keyed by task id. Entries are
    /// removed when the event stream ends (Completed / Failed / Suspended)
    /// or on `agent_cancel`.
    pub agent_handles: Arc<Mutex<HashMap<String, tauri::async_runtime::JoinHandle<()>>>>,
}

impl AppState {
    /// Build the state at app startup: open storage (honoring a previously
    /// persisted custom data directory), the market-data stack and the
    /// trading rules. The MiniMax client is *not* built here — see
    /// [`AppState::ensure_minimax`].
    pub fn init() -> Result<Self, CmdError> {
        let mut storage = Storage::open(StorageConfig::default())
            .map_err(|e| CmdError::new("storage", format!("open storage: {e}")))?;

        // Honor a custom data directory persisted by `set_data_dir`.
        let configured =
            tauri::async_runtime::block_on(storage.settings_get("data_dir")).unwrap_or(None);
        if let Some(dir) = configured {
            let dir = dir.trim().to_string();
            if !dir.is_empty() && storage.base_dir().to_string_lossy() != dir {
                tracing::info!(data_dir = %dir, "using persisted custom data directory");
                storage = Storage::open(StorageConfig::with_base_dir(&dir))
                    .map_err(|e| CmdError::new("storage", format!("open data dir {dir}: {e}")))?;
            }
        }

        let rules = RuleSet::load(None)
            .map_err(|e| CmdError::new("rules", format!("load trading rules: {e}")))?;

        // Inject persisted provider credentials into the process environment
        // *before* building the market-data stack: the optional providers
        // (tushare / iwencai / joinquant / socks5 proxy) capture their env
        // vars at construction time. Values are never logged.
        tauri::async_runtime::block_on(
            crate::commands::settings::load_provider_credentials_into_env(&storage),
        );

        let market = Arc::new(MarketData::with_storage(storage.clone()));
        match tauri::async_runtime::block_on(storage.securities_list()) {
            Ok(records) => market.security_master.merge_records(records),
            Err(error) => tracing::warn!(%error, "failed to load cached security master"),
        }
        if let Err(error) =
            tauri::async_runtime::block_on(storage.securities_upsert(market.security_master.all()))
        {
            tracing::warn!(%error, "failed to persist security-master bootstrap records");
        }
        let f10 = Arc::new(EastMoneyF10::new(market.http.clone(), market.cache.clone()));

        // Supply-chain graph over the shared storage; seed the built-in
        // industry-chain graph on first run. Seeding is best-effort: a
        // failure here must not block app startup (graph commands degrade
        // to empty results and the agent tools report it).
        let graph = GraphStore::new(storage.clone());
        let seed_report = tauri::async_runtime::block_on(async {
            let seed = astock_graph::seed_if_empty(&graph).await;
            let nodes = graph.all_nodes().await.map(|n| n.len());
            let edges = graph.all_edges().await.map(|e| e.len());
            (seed, nodes, edges)
        });
        match seed_report {
            (Ok(seed), Ok(nodes), Ok(edges)) => {
                tracing::info!(
                    skipped = seed.skipped,
                    seeded_nodes = seed.nodes,
                    seeded_edges = seed.edges,
                    total_nodes = nodes,
                    total_edges = edges,
                    "supply-chain graph ready"
                );
            }
            (seed, nodes, edges) => {
                tracing::warn!(
                    seed = ?seed.map(|s| (s.skipped, s.nodes, s.edges)),
                    nodes = ?nodes,
                    edges = ?edges,
                    "graph seeding/count failed (non-fatal)"
                );
            }
        }

        Ok(AppState {
            market,
            fundamental: Arc::new(FundamentalClient::new(f10)),
            graph,
            storage,
            rules,
            minimax: RwLock::new(None),
            scan: Arc::new(ScanState::default()),
            backtest: Arc::new(BacktestState::default()),
            disclosure_sync: Arc::new(DisclosureSyncState::default()),
            agent_handles: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Ensure the MiniMax client exists: build it from the keyring key on
    /// first use. Returns `Ok(false)` when no key has been stored.
    pub async fn ensure_minimax(&self) -> Result<bool, CmdError> {
        if self.minimax.read().await.is_some() {
            return Ok(true);
        }
        let key = astock_minimax::KeyStore::new().load_key()?;
        match key {
            None => Ok(false),
            Some(key) => {
                let mut guard = self.minimax.write().await;
                if guard.is_none() {
                    *guard = Some(Arc::new(MinimaxClient::new(key)));
                }
                Ok(true)
            }
        }
    }
}
