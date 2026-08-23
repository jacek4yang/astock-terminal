//! Strongly-typed tool system over the deterministic Rust engines.
//!
//! Every tool returns a [`ToolResult`] whose `summary_json` is the compact
//! payload the LLM sees; the full payload is persisted to
//! `storage.tool_cache` under `cache_key` and can be drilled into with the
//! `get_cached_detail` tool. Tool dispatch is read-through cached with a
//! per-tool TTL, so identical calls within the TTL never hit the network or
//! the engines twice.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{Datelike, FixedOffset, Timelike, Utc};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use astock_core::{
    Adjust, DataQualitySummary, DatasetKind, KlinePeriod, QualityFlag, QualityFlagCode,
};
use astock_fundamental::FundamentalClient;
use astock_graph::GraphStore;
use astock_market_data::{DataProvider, FinanceNewsProvider, IwencaiOpenApi, JoinQuantProvider};
use astock_minimax::MinimaxClient;
use astock_minimax::ToolSpec;
use astock_security::ToolPermissionDomain;
use astock_storage::{QualityObservation, Storage, ToolCacheEntry};

use crate::error::{AgentError, Result};

/// One currently active unit inside a long-running tool.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolWorkItem {
    /// User-facing stock/code/item label.
    pub label: String,
    /// Current deterministic processing stage.
    pub stage: String,
}

/// Structured, non-sensitive progress emitted by a long-running tool.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolProgressDetail {
    pub completed: usize,
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub cache_hits: usize,
    /// Number of upstream data rows successfully ingested in this run.
    pub records: usize,
    pub active: Vec<ToolWorkItem>,
    /// Bounded recent failures for diagnosis; never contains credentials.
    pub recent_errors: Vec<String>,
}

/// Synchronous event sink; tools only publish compact snapshots and never
/// wait for the UI while doing market-data work.
pub type ToolProgressReporter = Arc<dyn Fn(ToolProgressDetail) + Send + Sync>;

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
    /// 可选的聚宽研究环境；显式调用、低频串行，不进入普通行情自动切换链。
    pub joinquant: Option<Arc<JoinQuantProvider>>,
    /// MiniMax Coding Plan 官方联网搜索入口；与聊天复用同一安全密钥客户端。
    pub minimax_search: Option<Arc<MinimaxClient>>,
    /// 公共财经快讯聚合器（无凭据），用于发现事件线索。
    pub finance_news: Option<Arc<FinanceNewsProvider>>,
    /// 可选问财官方接口，用于个股公告、新闻和结构化事件补证。
    pub iwencai: Option<Arc<IwencaiOpenApi>>,
    /// Per-invocation progress sink installed by the orchestrator.
    pub progress: Option<ToolProgressReporter>,
}

impl ToolContext {
    /// Market + storage only; graph and fundamental stay unavailable.
    pub fn new(market: Arc<dyn DataProvider>, storage: Storage) -> Self {
        ToolContext {
            market,
            storage,
            graph: None,
            fundamental: None,
            joinquant: None,
            minimax_search: None,
            finance_news: None,
            iwencai: None,
            progress: None,
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

    /// Attach the explicit, credential-gated JoinQuant research channel.
    pub fn with_joinquant(mut self, joinquant: Option<Arc<JoinQuantProvider>>) -> Self {
        self.joinquant = joinquant;
        self
    }

    /// Attach MiniMax's official Coding Plan web search capability.
    pub fn with_minimax_search(mut self, client: Option<Arc<MinimaxClient>>) -> Self {
        self.minimax_search = client;
        self
    }

    /// Attach public headlines plus the optional official iwencai evidence source.
    pub fn with_news_sources(
        mut self,
        finance_news: Option<Arc<FinanceNewsProvider>>,
        iwencai: Option<Arc<IwencaiOpenApi>>,
    ) -> Self {
        self.finance_news = finance_news;
        self.iwencai = iwencai;
        self
    }

    /// Attach a lightweight progress sink for one tool invocation.
    pub fn with_progress_reporter(mut self, reporter: ToolProgressReporter) -> Self {
        self.progress = Some(reporter);
        self
    }

    /// Publish a compact snapshot if the host requested detailed progress.
    pub fn report_progress(&self, detail: ToolProgressDetail) {
        if let Some(reporter) = &self.progress {
            reporter(detail);
        }
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

    /// Side-effect domain used by the immutable Agent authorization gate and
    /// audit log. Existing tools default to read-only network access; any
    /// future external write tool must opt in explicitly.
    fn permission_domain(&self) -> ToolPermissionDomain {
        ToolPermissionDomain::ReadOnlyNetwork
    }

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

    /// Tool specs in registry order, restricted to the names enabled for one
    /// task. Keeping registry order stable preserves the provider's prompt
    /// cache prefix for identical tool configurations.
    pub fn specs_for(&self, enabled: Option<&[String]>) -> Vec<ToolSpec> {
        self.tools
            .iter()
            .filter(|tool| enabled.is_none_or(|names| names.iter().any(|name| name == tool.name())))
            .map(|tool| {
                ToolSpec::function(tool.name(), tool.description(), tool.parameters_schema())
            })
            .collect()
    }

    /// Registered names in their stable prompt order.
    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|tool| tool.name()).collect()
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn AgentTool>> {
        self.tools.iter().find(|t| t.name() == name).cloned()
    }

    pub fn permission_domain(&self, name: &str) -> Option<ToolPermissionDomain> {
        self.get(name).map(|tool| tool.permission_domain())
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
                    let mut result = ToolResult {
                        summary_json: env.summary,
                        full_json: env.full,
                        cache_key,
                        source: env.source,
                        fetched_at: env.fetched_at,
                    };
                    attach_and_observe_quality(name, &args, &mut result, ctx, None, true).await;
                    return Ok(result);
                }
            }
        }

        let started = Instant::now();
        let execution = tool.execute(args.clone(), ctx).await;
        let mut result = match execution {
            Ok(result) => result,
            Err(error) => {
                observe_failure(
                    name,
                    &args,
                    ctx,
                    started.elapsed().as_millis() as u64,
                    &error,
                )
                .await;
                return Err(error);
            }
        };
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
        attach_and_observe_quality(
            name,
            &args,
            &mut result,
            ctx,
            Some(started.elapsed().as_millis() as u64),
            false,
        )
        .await;
        Ok(result)
    }
}

fn dataset_for_tool(name: &str, args: &Value) -> DatasetKind {
    match name {
        "get_quote" | "get_market_breadth" | "search_stock" | "get_watchlist" => {
            DatasetKind::RealtimeQuote
        }
        "get_kline" => match args.get("period").and_then(Value::as_str).unwrap_or("day") {
            "week" | "weekly" | "w" => DatasetKind::WeeklyKline,
            "month" | "monthly" => DatasetKind::MonthlyKline,
            value if value.contains('m') => DatasetKind::IntradayMinute,
            _ => DatasetKind::DailyKline,
        },
        "compute_indicators" | "run_full_analysis" | "run_chanlun" | "compare_stocks"
        | "scan_market" | "get_market_regime" => DatasetKind::DailyKline,
        "get_fund_flow" => DatasetKind::FundFlow,
        "get_fundamentals" | "run_joinquant_research" => DatasetKind::Fundamentals,
        "run_valuation" => DatasetKind::Valuation,
        "research_news" => DatasetKind::News,
        "research_disclosures" | "research_global_transmission" | "analyze_event_price_in" => {
            DatasetKind::Announcement
        }
        "search_web" => DatasetKind::SearchDiscovery,
        "fetch_source_document" | "read_document" | "compare_source_evidence" => {
            DatasetKind::Announcement
        }
        "get_industry_chain" | "run_supply_chain_shock" | "build_relationship_graph" => {
            DatasetKind::KnowledgeGraph
        }
        "run_backtest" | "iterate_strategy" => DatasetKind::Backtest,
        _ => DatasetKind::Other,
    }
}

fn entity_key(args: &Value) -> Option<String> {
    ["symbol", "code", "subject", "query", "cache_key"]
        .iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str).map(str::to_string))
        .or_else(|| {
            args.get("symbols")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .take(20)
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .filter(|value| !value.is_empty())
        })
}

fn count_missing(value: &Value) -> u32 {
    match value {
        Value::Null => 1,
        Value::Array(values) => values.iter().map(count_missing).sum(),
        Value::Object(values) => values
            .iter()
            .filter(|(key, _)| key.as_str() != "data_quality")
            .map(|(_, value)| count_missing(value))
            .sum(),
        _ => 0,
    }
}

fn count_conflicts(value: &Value) -> u32 {
    match value {
        Value::Array(values) => values.iter().map(count_conflicts).sum(),
        Value::Object(values) => {
            let local = values
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| {
                    matches!(status, "conflict" | "incompatible_contract" | "冲突")
                }) as u32;
            let declared = values
                .get("conflict_count")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
            local
                .saturating_add(declared)
                .saturating_add(values.values().map(count_conflicts).sum())
        }
        _ => 0,
    }
}

fn age_secs(fetched_at: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(fetched_at)
        .ok()
        .map(|time| Utc::now().timestamp().saturating_sub(time.timestamp()) as u64)
}

fn in_china_trading_session() -> bool {
    let china = FixedOffset::east_opt(8 * 3_600).expect("valid China offset");
    let now = Utc::now().with_timezone(&china);
    if now.weekday().number_from_monday() > 5 {
        return false;
    }
    let minutes = now.hour() * 60 + now.minute();
    (570..=690).contains(&minutes) || (780..=900).contains(&minutes)
}

fn quality_for_result(
    name: &str,
    dataset: DatasetKind,
    result: &ToolResult,
) -> (DataQualitySummary, u32, u32) {
    let value = result.full_json.as_ref().unwrap_or(&result.summary_json);
    let missing = count_missing(value);
    let conflicts = count_conflicts(value);
    let mut flags = Vec::new();
    if missing > 0 {
        flags.push(QualityFlag::warning(
            QualityFlagCode::Partial,
            None,
            format!("结果中有 {missing} 个空值；空值不会自动替换为零"),
        ));
    }
    if conflicts > 0 {
        flags.push(QualityFlag::blocking(
            QualityFlagCode::SourceConflict,
            None,
            format!("检测到 {conflicts} 个未解决的跨源冲突"),
        ));
    }
    if name == "run_valuation" {
        flags.push(QualityFlag::warning(
            QualityFlagCode::Unverified,
            None,
            "本次 Agent 估值未在同一工具内完成独立估值源对账，置信上限降为中等",
        ));
    }
    if matches!(name, "run_backtest" | "iterate_strategy") {
        flags.push(QualityFlag::warning(
            QualityFlagCode::Unverified,
            None,
            "历史输入序列未在本工具内逐字段完成跨源复核，不得仅凭最优回测给出高置信建议",
        ));
    }
    let age = age_secs(&result.fetched_at).unwrap_or_else(|| {
        flags.push(QualityFlag::warning(
            QualityFlagCode::Unverified,
            None,
            "上游结果没有可解析的抓取时间，无法证明实时性",
        ));
        0
    });
    (
        DataQualitySummary::evaluate(dataset, age, in_china_trading_session(), flags),
        missing,
        conflicts,
    )
}

fn insert_quality(value: &mut Value, quality: &Value) {
    if let Some(object) = value.as_object_mut() {
        object.insert("data_quality".into(), quality.clone());
    } else {
        let data = std::mem::take(value);
        *value = serde_json::json!({
            "data": data,
            "data_quality": quality,
        });
    }
}

async fn attach_and_observe_quality(
    name: &str,
    args: &Value,
    result: &mut ToolResult,
    ctx: &ToolContext,
    latency_ms: Option<u64>,
    cache_hit: bool,
) {
    let dataset = dataset_for_tool(name, args);
    let (summary, missing, conflicts) = quality_for_result(name, dataset, result);
    let quality_json = serde_json::to_value(&summary).unwrap_or(Value::Null);
    insert_quality(&mut result.summary_json, &quality_json);
    if let Some(full) = result.full_json.as_mut() {
        insert_quality(full, &quality_json);
    }
    let provider = if result.source.trim().is_empty() {
        "未声明来源".to_string()
    } else {
        result.source.clone()
    };
    let operation = if cache_hit {
        format!("{name}（缓存命中）")
    } else {
        name.to_string()
    };
    let _ = ctx
        .storage
        .quality_observation_add(QualityObservation {
            observation_id: None,
            dataset,
            provider,
            entity_key: entity_key(args),
            operation,
            success: true,
            latency_ms,
            summary,
            missing_fields: missing,
            conflicts,
            error_kind: None,
            recorded_at: now_secs(),
        })
        .await;
}

async fn observe_failure(
    name: &str,
    args: &Value,
    ctx: &ToolContext,
    latency_ms: u64,
    error: &AgentError,
) {
    let dataset = dataset_for_tool(name, args);
    let summary = DataQualitySummary::evaluate(
        dataset,
        0,
        in_china_trading_session(),
        vec![QualityFlag::warning(
            QualityFlagCode::Partial,
            None,
            "本次工具调用失败，未产生可用于结论的数据",
        )],
    );
    let _ = ctx
        .storage
        .quality_observation_add(QualityObservation {
            observation_id: None,
            dataset,
            provider: "调用失败".into(),
            entity_key: entity_key(args),
            operation: name.into(),
            success: false,
            latency_ms: Some(latency_ms),
            summary,
            missing_fields: 0,
            conflicts: 0,
            error_kind: Some(error.to_string()),
            recorded_at: now_secs(),
        })
        .await;
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
            joinquant: None,
            minimax_search: None,
            finance_news: None,
            iwencai: None,
            progress: None,
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
        assert_eq!(second.summary_json["echo"], json!("hi"));
        assert_eq!(
            second.summary_json["data_quality"]["freshness"],
            json!("expired")
        );
        assert_eq!(
            second.summary_json["data_quality"]["allow_deterministic_compute"],
            json!(false)
        );
        let observations = ctx
            .storage
            .quality_observations_recent(None, None, 10)
            .await
            .unwrap();
        assert_eq!(observations.len(), 2);
        assert!(observations
            .iter()
            .any(|item| item.operation.contains("缓存命中")));

        let missing = registry.dispatch("nope", json!({}), &ctx).await;
        assert!(matches!(missing, Err(AgentError::UnknownTool(_))));
    }
}
