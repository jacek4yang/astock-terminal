//! Bounded deterministic research snapshots requested by the MoonBit Agent.
//!
//! The Agent owns tool selection and workflow order. This module only batches
//! existing Engine services and windows repetitive series so a single IPC
//! frame cannot become an unbounded database/data-provider transport.

use super::{Engine, ServiceError};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const MAX_EVIDENCE_FACTS: usize = 3_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EvidenceFact {
    pub evidence_id: String,
    pub path: String,
    pub value: Value,
    pub source: String,
    pub observed_at: Option<String>,
    pub source_version_id: Option<String>,
    pub quality_blocking: bool,
}

#[derive(Clone, Default)]
struct EvidenceMetadata {
    source: Option<String>,
    observed_at: Option<String>,
    source_version_id: Option<String>,
    quality_blocking: bool,
}

const NEWS_SOURCES: [&str; 12] = [
    "cls-telegraph",
    "cls-depth",
    "cls-hot",
    "jin10",
    "wallstreetcn-quick",
    "wallstreetcn-hot",
    "wallstreetcn-news",
    "mktnews-flash",
    "gelonghui",
    "fastbull-express",
    "fastbull-news",
    "xueqiu-hotstock",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrepareContextPayload {
    depth: String,
    #[serde(default)]
    capital: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SecurityContextPayload {
    symbols: Vec<String>,
    depth: String,
    tool_policy: String,
    #[serde(default)]
    analysis_modules: Vec<String>,
    benchmark: String,
    start: String,
    end: String,
}

const ADVANCED_ANALYSIS_MODULES: [&str; 5] = [
    "earnings_driver",
    "industry_graph",
    "relationship",
    "market_regime",
    "historical_backtest",
];

fn analysis_modules(
    tool_policy: &str,
    requested: Vec<String>,
) -> Result<Vec<String>, ServiceError> {
    let allowed = match tool_policy {
        "market" => &[][..],
        "evidence" => &ADVANCED_ANALYSIS_MODULES[..4],
        "auto" | "full" => &ADVANCED_ANALYSIS_MODULES[..],
        _ => {
            return Err(ServiceError::new(
                "invalid_agent_tool_policy",
                "Agent tool policy must be auto, market, evidence or full",
                false,
            ))
        }
    };
    let requested = if tool_policy == "full" {
        allowed.iter().map(|value| (*value).to_string()).collect()
    } else {
        requested
    };
    let mut unique = BTreeSet::new();
    for module in requested {
        if !allowed.contains(&module.as_str()) {
            return Err(ServiceError::new(
                "invalid_agent_analysis_module",
                format!("Agent requested an unavailable analysis module: {module}"),
                false,
            ));
        }
        unique.insert(module);
    }
    Ok(unique.into_iter().collect())
}

fn captured_module(result: Result<Value, ServiceError>) -> Value {
    match result {
        Ok(data) => json!({"ok": true, "data": data}),
        Err(error) => json!({
            "ok": false,
            "error": error.code,
            "message": error.message,
            "retryable": error.retryable,
            "quality_blocking": true,
        }),
    }
}

fn module_activity(module: &str, scope: &str, result: &Value) -> Value {
    json!({
        "module": module,
        "scope": scope,
        "status": if result.get("skipped").and_then(Value::as_bool) == Some(true) {
            "skipped"
        } else if result.get("ok").and_then(Value::as_bool) == Some(true) {
            "succeeded"
        } else {
            "failed"
        },
        "error": result.get("error").cloned().unwrap_or(Value::Null),
    })
}

fn exhaustive(depth: &str) -> Result<bool, ServiceError> {
    match depth {
        "fast" | "balanced" | "deep" => Ok(false),
        "exhaustive" => Ok(true),
        _ => Err(ServiceError::new(
            "invalid_agent_depth",
            "Agent research depth must be fast, balanced, deep or exhaustive",
            false,
        )),
    }
}

fn trim_head(rows: &mut Vec<Value>, limit: usize) {
    rows.truncate(limit);
}

fn trim_tail(rows: &mut Vec<Value>, limit: usize) {
    if rows.len() > limit {
        rows.drain(..rows.len() - limit);
    }
}

fn array_at_mut<'a>(value: &'a mut Value, path: &[&str]) -> Option<&'a mut Vec<Value>> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.as_object_mut()?.get_mut(*key)?;
    }
    cursor.as_array_mut()
}

fn compact_dataset(value: &mut Value, key: &str, limit: usize) {
    let Some(dataset) = value
        .as_object_mut()
        .and_then(|root| root.get_mut("datasets"))
        .and_then(Value::as_object_mut)
        .and_then(|datasets| datasets.get_mut(key))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let Some(rows) = dataset.get_mut("rows").and_then(Value::as_array_mut) else {
        return;
    };
    let visible = rows.len().min(limit);
    rows.truncate(limit);
    dataset.insert("model_view_rows".into(), json!(visible));
}

fn compact_market_context(value: &mut Value, is_exhaustive: bool) {
    let limits = [
        ("billboard_7d", if is_exhaustive { 120 } else { 80 }),
        ("margin_daily", if is_exhaustive { 40 } else { 30 }),
        ("industry_boards", if is_exhaustive { 60 } else { 40 }),
        ("concept_boards", if is_exhaustive { 60 } else { 40 }),
        (
            "previous_limit_up_pool",
            if is_exhaustive { 160 } else { 100 },
        ),
        ("sub_new_pool", if is_exhaustive { 160 } else { 100 }),
    ];
    for (key, limit) in limits {
        compact_dataset(value, key, limit);
    }
}

fn compact_security_bundle(bundle: &mut Map<String, Value>, is_exhaustive: bool) {
    if let Some(market) = bundle.get_mut("market") {
        if let Some(rows) = array_at_mut(market, &["kline", "bars"]) {
            trim_tail(rows, if is_exhaustive { 250 } else { 180 });
        }
        if let Some(rows) = array_at_mut(market, &["fund_flow_30d"]) {
            trim_tail(rows, 30);
        }
    }
    if let Some(fundamentals) = bundle.get_mut("fundamentals") {
        for key in ["income", "balance", "cashflow", "indicators", "dividends"] {
            if let Some(rows) = array_at_mut(fundamentals, &[key]) {
                trim_tail(rows, 8);
            }
        }
        if let Some(rows) = array_at_mut(fundamentals, &["valuation_history"]) {
            trim_tail(rows, if is_exhaustive { 252 } else { 180 });
        }
    }
    if let Some(events) = bundle.get_mut("events") {
        let limits = [
            ("announcements_1y", if is_exhaustive { 160 } else { 100 }),
            ("cninfo_disclosures_1y", if is_exhaustive { 50 } else { 40 }),
            ("org_survey_2y", if is_exhaustive { 120 } else { 80 }),
            ("block_trade_1y", if is_exhaustive { 120 } else { 80 }),
        ];
        for (key, limit) in limits {
            compact_dataset(events, key, limit);
        }
        if let Some(datasets) = events.get("datasets").and_then(Value::as_object) {
            let keys = datasets.keys().cloned().collect::<Vec<_>>();
            for key in keys {
                if !limits.iter().any(|(known, _)| *known == key) {
                    compact_dataset(events, &key, if is_exhaustive { 120 } else { 80 });
                }
            }
        }
    }
    if let Some(news) = bundle.get_mut("news") {
        if let Some(rows) = array_at_mut(news, &["items"]) {
            trim_head(rows, if is_exhaustive { 30 } else { 20 });
        }
    }
    if let Some(reconciliation) = bundle.get_mut("reconciliation") {
        if let Some(rows) = array_at_mut(reconciliation, &["kline_close_checks"]) {
            trim_tail(rows, 20);
        }
    }
    if let Some(joinquant) = bundle.get_mut("joinquant") {
        for (key, limit) in [
            ("qfq_daily", if is_exhaustive { 500 } else { 250 }),
            ("benchmark_components", 500),
            ("macro_cpi", 24),
        ] {
            compact_dataset(joinquant, key, limit);
        }
    }
    if let Some(optional) = bundle.get_mut("optional_sources") {
        for (key, limit) in [
            ("tushare_raw_daily", if is_exhaustive { 500 } else { 250 }),
            ("tushare_daily_basic", if is_exhaustive { 500 } else { 250 }),
            (
                "tushare_adjustment_factors",
                if is_exhaustive { 500 } else { 250 },
            ),
            ("tushare_dividends", 120),
            ("sec_edgar_filings", 120),
        ] {
            compact_dataset(optional, key, limit);
        }
    }
    if let Some(analysis) = bundle.get_mut("advanced_analysis") {
        if let Some(rows) = array_at_mut(analysis, &["historical_backtest", "data", "equity_curve"])
        {
            trim_tail(rows, if is_exhaustive { 500 } else { 250 });
        }
        if let Some(rows) = array_at_mut(analysis, &["historical_backtest", "data", "trades_tail"])
        {
            trim_tail(rows, 50);
        }
    }
}

fn metadata_text(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
}

fn scalar_fact(value: &Value) -> bool {
    match value {
        Value::Number(_) | Value::Bool(_) => true,
        Value::String(text) => !text.is_empty() && text.chars().count() <= 160,
        _ => false,
    }
}

fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn collect_evidence_facts(
    value: &Value,
    path: &str,
    scope: &str,
    inherited: &EvidenceMetadata,
    facts: &mut Vec<EvidenceFact>,
) {
    if facts.len() >= MAX_EVIDENCE_FACTS {
        return;
    }
    match value {
        Value::Object(object) => {
            let mut metadata = inherited.clone();
            metadata.source =
                metadata_text(object, &["source", "provider", "provider_id"]).or(metadata.source);
            metadata.observed_at = metadata_text(
                object,
                &[
                    "retrieved_at",
                    "fetched_at",
                    "observed_at",
                    "published_at",
                    "trade_date",
                    "date",
                ],
            )
            .or(metadata.observed_at);
            metadata.source_version_id =
                metadata_text(object, &["source_version_id", "revision_id"])
                    .or(metadata.source_version_id);
            metadata.quality_blocking = metadata.quality_blocking
                || object
                    .get("blocking")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                || object
                    .get("quality_blocking")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            for (key, child) in object {
                if key == "evidence_registry" {
                    continue;
                }
                collect_evidence_facts(
                    child,
                    &format!("{path}/{}", escape_pointer(key)),
                    scope,
                    &metadata,
                    facts,
                );
                if facts.len() >= MAX_EVIDENCE_FACTS {
                    break;
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                let remaining_items = items.len() - index;
                let fair_share = (MAX_EVIDENCE_FACTS - facts.len()) / remaining_items.max(1);
                let before = facts.len();
                collect_evidence_facts(child, &format!("{path}/{index}"), scope, inherited, facts);
                facts.truncate((before + fair_share).min(MAX_EVIDENCE_FACTS));
                if facts.len() >= MAX_EVIDENCE_FACTS {
                    break;
                }
            }
        }
        _ if scalar_fact(value) => {
            let canonical = serde_json::to_string(value).unwrap_or_default();
            let digest = format!(
                "{:x}",
                Sha256::digest(format!("{scope}|{path}|{canonical}"))
            );
            facts.push(EvidenceFact {
                evidence_id: format!("evf_{}", &digest[..24]),
                path: path.to_owned(),
                value: value.clone(),
                source: inherited.source.clone().unwrap_or_else(|| "engine".into()),
                observed_at: inherited.observed_at.clone(),
                source_version_id: inherited
                    .source_version_id
                    .clone()
                    .or_else(|| Some(format!("field:{}", &digest[..24]))),
                quality_blocking: inherited.quality_blocking,
            });
        }
        _ => {}
    }
}

pub(super) fn attach_evidence_registry(payload: &mut Value, scope: &str) {
    let mut facts = Vec::new();
    collect_evidence_facts(payload, "", scope, &EvidenceMetadata::default(), &mut facts);
    let truncated = facts.len() == MAX_EVIDENCE_FACTS;
    if let Some(root) = payload.as_object_mut() {
        root.insert(
            "evidence_registry".into(),
            json!({
                "version": "astock-field-evidence/v1",
                "scope": scope,
                "facts": facts,
                "truncated": truncated,
            }),
        );
    }
}

pub(super) async fn prepare(
    engine: &Engine,
    payload: PrepareContextPayload,
) -> Result<Value, ServiceError> {
    let is_exhaustive = exhaustive(&payload.depth)?;
    let candidate_limit = if is_exhaustive { 80 } else { 50 };
    let news_limit = if is_exhaustive { 60 } else { 45 };
    let candidates_payload = match payload
        .capital
        .filter(|value| value.is_finite() && *value > 0.0)
    {
        Some(capital) => json!({"limit": candidate_limit, "max_lot_cost": capital * 0.8}),
        None => json!({"limit": candidate_limit}),
    };
    let (market_overview, market_context, global_context, market_news, candidates) = tokio::join!(
        engine.dispatch_internal("market.overview", json!({})),
        engine.dispatch_internal("research.market_context", json!({})),
        engine.dispatch_internal("research.global_context", json!({})),
        engine.dispatch_internal(
            "research.news",
            json!({"sources": NEWS_SOURCES, "limit": news_limit}),
        ),
        engine.dispatch_internal("research.market_candidates", candidates_payload),
    );
    let mut market_context = market_context?;
    compact_market_context(&mut market_context, is_exhaustive);
    let mut market_news = market_news?;
    let news_count = market_news
        .get("items")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if news_count < 10 {
        return Err(ServiceError::new(
            "insufficient_news_evidence",
            format!("有效资讯仅 {news_count} 条，低于 Agent 研究发布门槛 10 条"),
            true,
        ));
    }
    if let Some(root) = market_news.as_object_mut() {
        root.insert("requested_source_count".into(), json!(NEWS_SOURCES.len()));
        root.insert(
            "evidence_note".into(),
            json!("一次有界采集覆盖12类频道；重要判断仍须回链一级来源"),
        );
    }
    let mut payload = json!({
        "source": "engine_agent_prepare_aggregate",
        "retrieved_at": astock_core::time::utc_now(),
        "depth": payload.depth,
        "market_overview": market_overview?,
        "market_context": market_context,
        "global_context": global_context?,
        "market_news": market_news,
        "candidates": candidates?,
    });
    attach_evidence_registry(&mut payload, "agent_prepare_context");
    let encoded = serde_json::to_vec(&payload).map_err(super::serialize_error)?;
    if encoded.len() > astock_protocol::MAX_FRAME_BYTES - 512 * 1024 {
        return Err(ServiceError::new(
            "agent_context_frame_too_large",
            "有界市场研究快照超过安全 IPC 预算；请使用较低研究深度",
            false,
        ));
    }
    Ok(payload)
}

pub(super) async fn security(
    engine: &Engine,
    payload: SecurityContextPayload,
) -> Result<Value, ServiceError> {
    let is_exhaustive = exhaustive(&payload.depth)?;
    let selected_modules = analysis_modules(&payload.tool_policy, payload.analysis_modules)?;
    if payload.symbols.is_empty() || payload.symbols.len() > 5 {
        return Err(ServiceError::new(
            "invalid_research_symbol_count",
            "Agent security context requires between one and five symbols",
            false,
        ));
    }
    let mut unique = Vec::new();
    for symbol in payload.symbols {
        if symbol.len() != 6 || !symbol.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ServiceError::new(
                "invalid_research_symbol",
                format!("invalid A-share symbol: {symbol}"),
                false,
            ));
        }
        if !unique.contains(&symbol) {
            unique.push(symbol);
        }
    }
    let depth = payload.depth.clone();
    let tool_policy = payload.tool_policy;
    let benchmark = payload.benchmark;
    let start = payload.start;
    let end = payload.end;
    let selected_symbols = unique.clone();
    let bundles = join_all(unique.into_iter().map(|symbol| {
        let benchmark = benchmark.clone();
        let start = start.clone();
        let end = end.clone();
        let tool_policy = tool_policy.clone();
        let selected_modules = selected_modules.clone();
        async move {
            let count = if is_exhaustive { 500 } else { 250 };
            let (market, reconciliation) = tokio::join!(
                engine.dispatch_internal("market.security_snapshot", json!({"symbol": symbol, "period": "day", "adjust": "qfq", "count": count})),
                engine.dispatch_internal("research.data_reconcile", json!({"symbol": symbol})),
            );
            let (fundamentals, events, news) = if tool_policy == "market" {
                let skipped = json!({"ok": false, "skipped": true, "reason": "skipped_by_tool_policy"});
                (Ok(skipped.clone()), Ok(skipped.clone()), Ok(json!({"items": [], "skipped": true, "reason": "skipped_by_tool_policy"})))
            } else {
                tokio::join!(
                    engine.dispatch_internal("research.fundamentals", json!({"symbol": symbol})),
                    engine.dispatch_internal("research.security_events", json!({"symbol": symbol})),
                    engine.dispatch_internal("research.news", json!({"symbol": symbol, "keyword": symbol, "sources": NEWS_SOURCES, "limit": if is_exhaustive { 30 } else { 20 }})),
                )
            };
            let (joinquant, optional) = if tool_policy == "auto" || tool_policy == "full" {
                tokio::join!(
                    engine.dispatch_internal("research.joinquant_context", json!({"symbol": symbol, "benchmark": benchmark, "start": start, "end": end})),
                    engine.dispatch_internal("research.optional_sources", json!({"symbol": symbol, "start": start, "end": end})),
                )
            } else {
                let skipped = json!({
                    "configured": false,
                    "skipped": true,
                    "reason": "credentialed_sources_excluded_by_tool_policy",
                    "datasets": {},
                });
                (Ok(skipped.clone()), Ok(skipped))
            };
            let earnings_driver = async {
                if selected_modules.iter().any(|item| item == "earnings_driver") {
                    captured_module(
                        engine
                            .dispatch_internal(
                                "research.earnings_driver.tree",
                                json!({"symbol": symbol}),
                            )
                            .await,
                    )
                } else {
                    Value::Null
                }
            };
            let industry_graph = async {
                if selected_modules.iter().any(|item| item == "industry_graph") {
                    captured_module(
                        engine
                            .dispatch_internal(
                                "research.graph.subgraph",
                                json!({"symbol_or_node": symbol, "hops": 2}),
                            )
                            .await,
                    )
                } else {
                    Value::Null
                }
            };
            let historical_backtest = async {
                if selected_modules
                    .iter()
                    .any(|item| item == "historical_backtest")
                {
                    captured_module(
                        engine
                            .dispatch_internal(
                                "research.backtest.run",
                                json!({
                                    "symbol": symbol,
                                    "strategy": "ma_cross",
                                    "bars": if is_exhaustive { 1000 } else { 500 },
                                }),
                            )
                            .await,
                    )
                } else {
                    Value::Null
                }
            };
            let (earnings_driver, industry_graph, historical_backtest) = tokio::join!(
                earnings_driver,
                industry_graph,
                historical_backtest
            );
            let mut advanced_analysis = Map::new();
            for (module, result) in [
                ("earnings_driver", earnings_driver),
                ("industry_graph", industry_graph),
                ("historical_backtest", historical_backtest),
            ] {
                if !result.is_null() {
                    advanced_analysis.insert(module.to_string(), result);
                }
            }
            let mut bundle = Map::new();
            bundle.insert("symbol".into(), json!(symbol));
            bundle.insert("market".into(), market?);
            bundle.insert("fundamentals".into(), fundamentals?);
            bundle.insert("events".into(), events?);
            bundle.insert("news".into(), news?);
            bundle.insert("reconciliation".into(), reconciliation?);
            bundle.insert("joinquant".into(), joinquant?);
            bundle.insert("optional_sources".into(), optional?);
            bundle.insert(
                "advanced_analysis".into(),
                Value::Object(advanced_analysis),
            );
            compact_security_bundle(&mut bundle, is_exhaustive);
            Ok::<Value, ServiceError>(Value::Object(bundle))
        }
    }))
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    let relationship = if selected_modules.iter().any(|item| item == "relationship") {
        if selected_symbols.len() < 2 {
            json!({
                "ok": false,
                "skipped": true,
                "error": "relationship_requires_two_symbols",
                "message": "跨证券关系分析至少需要两个研究标的",
                "quality_blocking": false,
            })
        } else {
            captured_module(
                engine
                    .dispatch_internal(
                        "research.market.relationship",
                        json!({"symbols": selected_symbols, "window_days": 250}),
                    )
                    .await,
            )
        }
    } else {
        Value::Null
    };
    let market_regime = if selected_modules.iter().any(|item| item == "market_regime") {
        captured_module(
            engine
                .dispatch_internal("research.market.regime", json!({}))
                .await,
        )
    } else {
        Value::Null
    };
    let mut cross_security_analysis = Map::new();
    for (module, result) in [
        ("relationship", relationship),
        ("market_regime", market_regime),
    ] {
        if !result.is_null() {
            cross_security_analysis.insert(module.to_string(), result);
        }
    }
    let mut tool_activities = Vec::new();
    for bundle in &bundles {
        let scope = bundle
            .get("symbol")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if let Some(advanced) = bundle.get("advanced_analysis").and_then(Value::as_object) {
            for (module, result) in advanced {
                tool_activities.push(module_activity(module, scope, result));
            }
        }
    }
    for (module, result) in &cross_security_analysis {
        tool_activities.push(module_activity(module, "portfolio", result));
    }
    let mut payload = json!({
        "source": "engine_agent_security_aggregate",
        "retrieved_at": astock_core::time::utc_now(),
        "depth": depth,
        "analysis_modules": selected_modules,
        "securities": bundles,
        "cross_security_analysis": cross_security_analysis,
        "tool_activities": tool_activities,
    });
    attach_evidence_registry(&mut payload, "agent_security_context");
    let encoded = serde_json::to_vec(&payload).map_err(super::serialize_error)?;
    if encoded.len() > astock_protocol::MAX_FRAME_BYTES - 512 * 1024 {
        return Err(ServiceError::new(
            "agent_context_frame_too_large",
            "有界研究快照仍超过安全 IPC 预算；请减少证券数量或使用较低研究深度",
            false,
        ));
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_retains_recent_market_rows_and_dataset_quality() {
        let mut bundle = Map::from_iter([
            (
                "market".into(),
                json!({"kline": {"bars": [1,2,3,4]}, "fund_flow_30d": [1,2,3]}),
            ),
            (
                "events".into(),
                json!({"datasets": {"announcements_1y": {"ok": true, "rows": [1,2,3], "source": "CNInfo"}}}),
            ),
        ]);
        compact_security_bundle(&mut bundle, false);
        assert_eq!(bundle["market"]["kline"]["bars"], json!([1, 2, 3, 4]));
        assert_eq!(bundle["events"]["datasets"]["announcements_1y"]["ok"], true);
        assert_eq!(
            bundle["events"]["datasets"]["announcements_1y"]["model_view_rows"],
            3
        );
    }

    #[test]
    fn unknown_depth_is_rejected() {
        assert!(exhaustive("unbounded").is_err());
    }

    #[test]
    fn analysis_module_policy_is_closed_and_never_silently_escalates() {
        assert!(analysis_modules("market", vec![]).unwrap().is_empty());
        assert_eq!(
            analysis_modules(
                "evidence",
                vec!["industry_graph".into(), "earnings_driver".into()]
            )
            .unwrap(),
            vec!["earnings_driver", "industry_graph"]
        );
        assert_eq!(
            analysis_modules("full", vec![]).unwrap(),
            vec![
                "earnings_driver",
                "historical_backtest",
                "industry_graph",
                "market_regime",
                "relationship",
            ]
        );
        let error = analysis_modules("auto", vec!["place_order".into()]).unwrap_err();
        assert_eq!(error.code, "invalid_agent_analysis_module");
        assert!(analysis_modules("market", vec!["market_regime".into()]).is_err());
    }

    #[test]
    fn advanced_module_activity_keeps_failures_and_skips_visible() {
        let failed = captured_module(Err(ServiceError::new(
            "provider_unavailable",
            "upstream unavailable",
            true,
        )));
        assert_eq!(failed["ok"], false);
        assert_eq!(failed["quality_blocking"], true);
        assert_eq!(failed["retryable"], true);
        assert_eq!(
            module_activity("industry_graph", "300308", &failed)["status"],
            "failed"
        );

        let skipped = json!({
            "ok": false,
            "skipped": true,
            "error": "relationship_requires_two_symbols",
            "quality_blocking": false,
        });
        let activity = module_activity("relationship", "portfolio", &skipped);
        assert_eq!(activity["status"], "skipped");
        assert_eq!(activity["error"], "relationship_requires_two_symbols");
    }

    #[test]
    fn evidence_registry_is_stable_and_inherits_quality_metadata() {
        let mut payload = json!({
            "retrieved_at": "2026-08-25T01:00:00Z",
            "source": "provider-a",
            "dataset": {"blocking": true, "price": 12.34, "symbol": "000001"}
        });
        attach_evidence_registry(&mut payload, "test");
        let facts = payload["evidence_registry"]["facts"].as_array().unwrap();
        let price = facts
            .iter()
            .find(|fact| fact["path"] == "/dataset/price")
            .unwrap();
        assert_eq!(price["value"], json!(12.34));
        assert_eq!(price["source"], "provider-a");
        assert_eq!(price["observed_at"], "2026-08-25T01:00:00Z");
        assert_eq!(price["quality_blocking"], true);
        let first_id = price["evidence_id"].clone();
        attach_evidence_registry(&mut payload, "test");
        let repeated = payload["evidence_registry"]["facts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|fact| fact["path"] == "/dataset/price")
            .unwrap();
        assert_eq!(repeated["evidence_id"], first_id);
    }
}
