//! Strongly-typed tool system over the deterministic Rust engines.
//!
//! Every tool returns a [`ToolResult`] whose `summary_json` is the compact
//! payload the LLM sees; the full payload is persisted to
//! `storage.tool_cache` under `cache_key` and can be drilled into with the
//! `get_cached_detail` tool. Tool dispatch is read-through cached with a
//! per-tool TTL, so identical calls within the TTL never hit the network or
//! the engines twice.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

use astock_core::{Adjust, KlinePeriod};
use astock_fundamental::FundamentalClient;
use astock_graph::GraphStore;
use astock_market_data::DataProvider;
use astock_minimax::ToolSpec;
use astock_storage::{Storage, ToolCacheEntry};

use crate::error::{AgentError, Result};

/// Shared context handed to every tool execution.
///
/// `market` is the `DataProvider` trait seam (`MarketData` in production, a
/// canned mock in tests) so tool logic is testable without network access.
/// `graph`/`fundamental` are `Option` so tests and partial setups still work;
/// tools that need them return a clean "capability unavailable" error when
/// they are `None`.
#[derive(Clone)]
pub struct ToolContext {
    /// Market-data provider composite.
    pub market: Arc<dyn DataProvider>,
    /// Local persistence (tool cache, conversations, agent tasks).
    pub storage: Storage,
    /// Supply-chain knowledge graph (industry chain / event propagation).
    pub graph: Option<GraphStore>,
    /// Fundamental-data client (EastMoney F10 bundle + analytics).
    pub fundamental: Option<Arc<FundamentalClient>>,
}

impl ToolContext {
    /// Market + storage only; graph and fundamental stay unavailable.
    pub fn new(market: Arc<dyn DataProvider>, storage: Storage) -> Self {
        ToolContext {
            market,
            storage,
            graph: None,
            fundamental: None,
        }
    }

    /// Attach the graph / fundamental capabilities.
    pub fn with_engines(
        mut self,
        graph: Option<GraphStore>,
        fundamental: Option<Arc<FundamentalClient>>,
    ) -> Self {
        self.graph = graph;
        self.fundamental = fundamental;
        self
    }
}

/// Outcome of one tool execution.
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// Compact payload shown to the LLM.
    pub summary_json: Value,
    /// Full payload, persisted to `tool_cache` when present.
    pub full_json: Option<Value>,
    /// Cache key (tool + args hash) under which the full payload is stored.
    pub cache_key: String,
    /// Upstream data source ("tencent" / "sina" / "eastmoney" / "engine" ...).
    pub source: String,
    /// Fetch time of the underlying data, RFC 3339.
    pub fetched_at: String,
}

/// A strongly-typed tool the agent may call.
#[async_trait]
pub trait AgentTool: Send + Sync {
    /// Stable tool name (snake_case), used in tool calls and cache keys.
    fn name(&self) -> &'static str;

    /// One-line Chinese description shown to the model.
    fn description(&self) -> &'static str;

    /// JSON Schema of the arguments object (schemars-derived).
    fn parameters_schema(&self) -> Value;

    /// TTL for the read-through result cache, in seconds.
    fn cache_ttl_secs(&self) -> i64 {
        300
    }

    /// Whether dispatch may serve/store this tool's results from the cache.
    fn cacheable(&self) -> bool {
        true
    }

    /// Run the tool with JSON arguments.
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult>;
}

/// Envelope persisted in `tool_cache.result_json`, allowing a cache hit to
/// reconstruct both the summary and the full payload without re-executing.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CacheEnvelope {
    pub(crate) summary: Value,
    pub(crate) full: Option<Value>,
    pub(crate) source: String,
    pub(crate) fetched_at: String,
}

/// A set of tools plus dispatch with read-through caching.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Arc<Vec<Arc<dyn AgentTool>>>,
}

impl ToolRegistry {
    /// Build a registry from tool instances.
    pub fn new(tools: Vec<Arc<dyn AgentTool>>) -> Self {
        ToolRegistry {
            tools: Arc::new(tools),
        }
    }

    /// OpenAI-style tool specs for the chat request.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools
            .iter()
            .map(|t| ToolSpec::function(t.name(), t.description(), t.parameters_schema()))
            .collect()
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn AgentTool>> {
        self.tools.iter().find(|t| t.name() == name).cloned()
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Execute a tool by name with read-through caching.
    ///
    /// Cache hits (same tool + args within the TTL) skip execution entirely;
    /// misses execute and persist the envelope under the same key.
    pub async fn dispatch(&self, name: &str, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let tool = self
            .get(name)
            .ok_or_else(|| AgentError::UnknownTool(name.to_string()))?;
        let cache_key = tool_cache_key(name, &args);

        if tool.cacheable() {
            if let Some(entry) = ctx.storage.tool_cache_get(&cache_key).await? {
                if let Ok(env) = serde_json::from_str::<CacheEnvelope>(&entry.result_json) {
                    return Ok(ToolResult {
                        summary_json: env.summary,
                        full_json: env.full,
                        cache_key,
                        source: env.source,
                        fetched_at: env.fetched_at,
                    });
                }
            }
        }

        let mut result = tool.execute(args, ctx).await?;
        result.cache_key = cache_key.clone();

        if tool.cacheable() {
            let env = CacheEnvelope {
                summary: result.summary_json.clone(),
                full: result.full_json.clone(),
                source: result.source.clone(),
                fetched_at: result.fetched_at.clone(),
            };
            let now = now_secs();
            ctx.storage
                .tool_cache_put(ToolCacheEntry {
                    cache_key: cache_key.clone(),
                    tool: name.to_string(),
                    params_json: serde_json::to_string(&env.summary).unwrap_or_default(),
                    result_json: serde_json::to_string(&env)?,
                    data_version: None,
                    created_at: now,
                    ttl_seconds: tool.cache_ttl_secs(),
                    accessed_at: now,
                })
                .await?;
        }
        Ok(result)
    }
}

/// Deterministic cache key for `(tool, args)`: `tool:fnv1a64(canonical_json)`.
///
/// serde_json maps are B-tree backed by default, so equal argument sets hash
/// identically regardless of key order.
pub fn tool_cache_key(tool: &str, args: &Value) -> String {
    let canonical = serde_json::to_string(args).unwrap_or_default();
    format!("{tool}:{:016x}", fnv1a64(canonical.as_bytes()))
}

/// FNV-1a 64-bit: stable across processes and builds (unlike `DefaultHasher`).
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// JSON Schema for a schemars-derived arguments struct, minus the
/// `$schema`/`title` clutter providers do not need.
pub fn schema_value<T: JsonSchema>() -> Value {
    let mut v = serde_json::to_value(schemars::schema_for!(T)).unwrap_or(Value::Null);
    if let Some(obj) = v.as_object_mut() {
        obj.remove("$schema");
        obj.remove("title");
    }
    v
}

/// Deserialize tool arguments into a typed struct with a typed error.
pub fn parse_args<T: DeserializeOwned>(tool: &str, args: Value) -> Result<T> {
    serde_json::from_value(args).map_err(|e| AgentError::InvalidArgs {
        tool: tool.to_string(),
        msg: e.to_string(),
    })
}

/// Parse a kline period string: day/week/month/1m/5m/15m/30m/60m (default day).
pub fn parse_period(raw: Option<&str>) -> Result<KlinePeriod> {
    match raw.unwrap_or("day").to_ascii_lowercase().as_str() {
        "day" | "daily" | "d" => Ok(KlinePeriod::Day),
        "week" | "weekly" | "w" => Ok(KlinePeriod::Week),
        "month" | "monthly" | "m" => Ok(KlinePeriod::Month),
        "1m" | "min1" => Ok(KlinePeriod::Min1),
        "5m" | "min5" => Ok(KlinePeriod::Min5),
        "15m" | "min15" => Ok(KlinePeriod::Min15),
        "30m" | "min30" => Ok(KlinePeriod::Min30),
        "60m" | "min60" => Ok(KlinePeriod::Min60),
        other => Err(AgentError::InvalidArgs {
            tool: "period".to_string(),
            msg: format!("unknown period `{other}`"),
        }),
    }
}

/// Parse a price-adjustment string: none/qfq/hfq (default qfq).
pub fn parse_adjust(raw: Option<&str>) -> Result<Adjust> {
    match raw.unwrap_or("qfq").to_ascii_lowercase().as_str() {
        "none" | "raw" | "" => Ok(Adjust::None),
        "qfq" => Ok(Adjust::Qfq),
        "hfq" => Ok(Adjust::Hfq),
        other => Err(AgentError::InvalidArgs {
            tool: "adjust".to_string(),
            msg: format!("unknown adjust `{other}`"),
        }),
    }
}

/// Current unix time in seconds (chrono here has no `clock` feature).
pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cache_key_is_stable_and_order_insensitive() {
        let a = tool_cache_key("get_kline", &json!({"symbol": "600519", "count": 120}));
        let b = tool_cache_key("get_kline", &json!({"count": 120, "symbol": "600519"}));
        assert_eq!(a, b);
        assert!(a.starts_with("get_kline:"));
        let c = tool_cache_key("get_kline", &json!({"symbol": "000001", "count": 120}));
        assert_ne!(a, c);
    }

    #[test]
    fn period_and_adjust_parsing() {
        assert_eq!(parse_period(None).unwrap(), KlinePeriod::Day);
        assert_eq!(parse_period(Some("60m")).unwrap(), KlinePeriod::Min60);
        assert!(parse_period(Some("year")).is_err());
        assert_eq!(parse_adjust(None).unwrap(), Adjust::Qfq);
        assert_eq!(parse_adjust(Some("none")).unwrap(), Adjust::None);
        assert!(parse_adjust(Some("xxx")).is_err());
    }

    #[tokio::test]
    async fn dispatch_is_read_through_cached() {
        use crate::testing::{EchoTool, NoopMarket};
        use std::sync::atomic::Ordering;

        let dir = tempfile::tempdir().unwrap();
        let storage =
            Storage::open(astock_storage::StorageConfig::with_base_dir(dir.path())).unwrap();
        let echo = Arc::new(EchoTool::new());
        let registry = ToolRegistry::new(vec![echo.clone()]);
        let ctx = ToolContext {
            market: Arc::new(NoopMarket),
            storage,
            graph: None,
            fundamental: None,
        };
        let args = json!({"text": "hi"});
        let first = registry.dispatch("echo", args.clone(), &ctx).await.unwrap();
        let second = registry.dispatch("echo", args, &ctx).await.unwrap();
        assert_eq!(
            echo.calls.load(Ordering::SeqCst),
            1,
            "second call served from cache"
        );
        assert_eq!(first.cache_key, second.cache_key);
        assert!(first.cache_key.starts_with("echo:"));
        assert_eq!(second.summary_json, json!({"echo": "hi"}));

        let missing = registry.dispatch("nope", json!({}), &ctx).await;
        assert!(matches!(missing, Err(AgentError::UnknownTool(_))));
    }
}
