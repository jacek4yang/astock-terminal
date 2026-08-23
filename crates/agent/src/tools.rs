//! Runtime-hardened facade for deterministic Agent tools.
//!
//! The implementation from the previous release is retained verbatim in
//! `tools_legacy.rs`. This facade adds two production safeguards without
//! changing the public tool contract:
//!
//! - canonical cache arguments plus per-key single-flight coalescing, so
//!   concurrent identical requests execute upstream work only once;
//! - bounded, tool-class-aware execution budgets, so a permanently stalled
//!   provider becomes a normal tool error that the orchestrator can feed back
//!   to the model instead of leaving the whole Agent run stuck forever.

#[path = "tools_legacy.rs"]
mod legacy;

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::error::{AgentError, Result};

pub use legacy::{
    parse_adjust, parse_args, parse_period, schema_value, AgentTool, ToolContext,
    ToolProgressDetail, ToolProgressReporter, ToolResult, ToolWorkItem,
};
pub(crate) use legacy::{now_secs, CacheEnvelope};

/// A tool registry with cache-key normalization, request coalescing and a
/// final safety deadline around every deterministic tool invocation.
#[derive(Clone)]
pub struct ToolRegistry {
    inner: legacy::ToolRegistry,
    flights: Arc<DashMap<String, Arc<Mutex<()>>>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl ToolRegistry {
    /// Build a registry from tool instances.
    pub fn new(tools: Vec<Arc<dyn AgentTool>>) -> Self {
        Self {
            inner: legacy::ToolRegistry::new(tools),
            flights: Arc::new(DashMap::new()),
        }
    }

    /// OpenAI-style tool specs in stable registry order.
    pub fn specs(&self) -> Vec<astock_minimax::ToolSpec> {
        self.inner.specs()
    }

    /// Tool specs restricted to one run's allowlist, retaining stable order.
    pub fn specs_for(&self, enabled: Option<&[String]>) -> Vec<astock_minimax::ToolSpec> {
        self.inner.specs_for(enabled)
    }

    /// Registered names in their stable prompt order.
    pub fn names(&self) -> Vec<&'static str> {
        self.inner.names()
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn AgentTool>> {
        self.inner.get(name)
    }

    pub fn permission_domain(
        &self,
        name: &str,
    ) -> Option<astock_security::ToolPermissionDomain> {
        self.inner.permission_domain(name)
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Dispatch a tool through the durable read-through cache.
    ///
    /// Identical calls share one per-key mutex. The first caller performs the
    /// upstream work; waiters re-check the persistent cache after the leader
    /// completes. The timeout is deliberately generous and is only a final
    /// deadlock guard—normal provider-level retries and progress reporting
    /// remain inside each tool.
    pub async fn dispatch(&self, name: &str, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args = canonicalize_cache_args(args);
        let cache_key = legacy::tool_cache_key(name, &args);
        let gate = self
            .flights
            .entry(cache_key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();

        let queued_at = Instant::now();
        let guard = gate.lock().await;
        let queue_wait = queued_at.elapsed();
        if queue_wait >= Duration::from_secs(1) {
            tracing::debug!(
                tool = name,
                cache_key = %cache_key,
                wait_ms = queue_wait.as_millis() as u64,
                "coalesced duplicate Agent tool request"
            );
        }

        let budget = tool_runtime_budget(name);
        let outcome = tokio::time::timeout(budget, self.inner.dispatch(name, args, ctx)).await;
        drop(guard);

        // Remove an idle flight entry. If another waiter already cloned the
        // gate its strong count is greater than the map + this local handle,
        // so the entry remains until the final waiter completes.
        if Arc::strong_count(&gate) <= 2 {
            self.flights.remove(&cache_key);
        }

        match outcome {
            Ok(result) => result,
            Err(_) => Err(AgentError::Tool {
                tool: name.to_string(),
                msg: format!(
                    "运行超过安全上限 {} 秒，已取消该数据源调用；主 Agent 应继续使用其他已成功证据并明确标注此项缺失",
                    budget.as_secs()
                ),
            }),
        }
    }
}

/// Canonicalize semantically equivalent argument envelopes before hashing.
///
/// This intentionally performs only transformations that are safe for the
/// current typed tools: object `null` is equivalent to an omitted `Option`,
/// surrounding whitespace is insignificant, and documented period/adjustment
/// aliases map to their canonical values. Array order is preserved because it
/// can be meaningful for comparison and source-priority tools.
pub fn canonicalize_cache_args(value: Value) -> Value {
    fn normalize(value: Value, key: Option<&str>) -> Value {
        match value {
            Value::Object(fields) => {
                let mut normalized = serde_json::Map::new();
                for (field, value) in fields {
                    if value.is_null() {
                        continue;
                    }
                    normalized.insert(field.clone(), normalize(value, Some(&field)));
                }
                Value::Object(normalized)
            }
            Value::Array(values) => Value::Array(
                values
                    .into_iter()
                    .map(|value| normalize(value, key))
                    .collect(),
            ),
            Value::String(value) => Value::String(normalize_string(key, &value)),
            other => other,
        }
    }

    normalize(value, None)
}

fn normalize_string(key: Option<&str>, value: &str) -> String {
    let trimmed = value.trim();
    match key.unwrap_or_default() {
        "symbol" | "code" => trimmed
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>()
            .to_ascii_uppercase(),
        "period" => match trimmed.to_ascii_lowercase().as_str() {
            "d" | "daily" => "day".to_string(),
            "w" | "weekly" => "week".to_string(),
            "m" | "monthly" => "month".to_string(),
            "min1" => "1m".to_string(),
            "min5" => "5m".to_string(),
            "min15" => "15m".to_string(),
            "min30" => "30m".to_string(),
            "min60" => "60m".to_string(),
            other => other.to_string(),
        },
        "adjust" => match trimmed.to_ascii_lowercase().as_str() {
            "" | "raw" => "none".to_string(),
            other => other.to_string(),
        },
        _ => trimmed.to_string(),
    }
}

/// Deterministic cache key for a canonical `(tool, args)` pair.
pub fn tool_cache_key(tool: &str, args: &Value) -> String {
    legacy::tool_cache_key(tool, &canonicalize_cache_args(args.clone()))
}

/// Final deadlock guard by workload class.
///
/// These are not expected-duration estimates. They are intentionally much
/// larger than normal runtimes and only ensure that a broken upstream cannot
/// occupy an Agent run forever.
pub fn tool_runtime_budget(name: &str) -> Duration {
    let seconds = match name {
        "get_quote" | "search_stock" | "get_watchlist" | "get_cached_detail" => 90,
        "get_kline"
        | "compute_indicators"
        | "get_fund_flow"
        | "get_market_breadth"
        | "get_market_regime" => 180,
        "run_full_analysis"
        | "run_chanlun"
        | "get_fundamentals"
        | "analyze_earnings_drivers"
        | "run_valuation"
        | "get_industry_chain"
        | "compare_stocks"
        | "run_joinquant_research" => 360,
        "search_web"
        | "fetch_source_document"
        | "read_document"
        | "compare_source_evidence"
        | "research_news"
        | "research_gold_market"
        | "research_disclosures"
        | "research_global_transmission"
        | "analyze_event_price_in"
        | "research_supply_chain_relations" => 600,
        "scan_market"
        | "run_supply_chain_shock"
        | "build_relationship_graph"
        | "query_graph_as_of"
        | "run_quant_research"
        | "run_backtest"
        | "iterate_strategy" => 1_200,
        _ => 300,
    };
    Duration::from_secs(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{EchoTool, NoopMarket};
    use astock_storage::{Storage, StorageConfig};
    use serde_json::json;
    use std::sync::atomic::Ordering;

    #[test]
    fn cache_arguments_are_canonical_but_preserve_array_order() {
        let a = canonicalize_cache_args(json!({
            "symbol": " sh 600519 ",
            "period": "DAILY",
            "adjust": "raw",
            "optional": null,
            "symbols": [" 600519 ", "000001"]
        }));
        let b = canonicalize_cache_args(json!({
            "symbols": ["600519", "000001"],
            "adjust": "none",
            "period": "day",
            "symbol": "SH600519"
        }));
        assert_eq!(a, b);

        let reversed = canonicalize_cache_args(json!({
            "symbols": ["000001", "600519"],
            "adjust": "none",
            "period": "day",
            "symbol": "SH600519"
        }));
        assert_ne!(a, reversed);
    }

    #[test]
    fn expensive_tools_receive_larger_deadlock_budgets() {
        assert!(tool_runtime_budget("scan_market") > tool_runtime_budget("get_quote"));
        assert!(tool_runtime_budget("research_news") > tool_runtime_budget("get_kline"));
    }

    #[tokio::test]
    async fn concurrent_identical_calls_are_single_flighted() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let echo = Arc::new(EchoTool::new());
        let registry = ToolRegistry::new(vec![echo.clone()]);
        let ctx = ToolContext::new(Arc::new(NoopMarket), storage);

        let calls = (0..16).map(|_| {
            registry.dispatch("echo", json!({"text": " same request "}), &ctx)
        });
        let results = futures::future::join_all(calls).await;
        assert!(results.iter().all(|result| result.is_ok()));
        assert_eq!(
            echo.calls.load(Ordering::SeqCst),
            1,
            "only the single-flight leader may execute upstream work"
        );
        assert!(results
            .iter()
            .all(|result| result.as_ref().unwrap().summary_json["echo"] == json!("same request")));
    }
}
