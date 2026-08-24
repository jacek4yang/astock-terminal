//! Bounded deterministic research snapshots requested by the MoonBit Agent.
//!
//! The Agent owns tool selection and workflow order. This module only batches
//! existing Engine services and windows repetitive series so a single IPC
//! frame cannot become an unbounded database/data-provider transport.

use super::{Engine, ServiceError};
use futures::future::join_all;
use serde::Deserialize;
use serde_json::{json, Map, Value};

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
    benchmark: String,
    start: String,
    end: String,
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
    Ok(json!({
        "depth": payload.depth,
        "market_overview": market_overview?,
        "market_context": market_context,
        "global_context": global_context?,
        "market_news": market_news,
        "candidates": candidates?,
    }))
}

pub(super) async fn security(
    engine: &Engine,
    payload: SecurityContextPayload,
) -> Result<Value, ServiceError> {
    let is_exhaustive = exhaustive(&payload.depth)?;
    if !matches!(
        payload.tool_policy.as_str(),
        "auto" | "market" | "evidence" | "full"
    ) {
        return Err(ServiceError::new(
            "invalid_agent_tool_policy",
            "Agent tool policy must be auto, market, evidence or full",
            false,
        ));
    }
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
    let bundles = join_all(unique.into_iter().map(|symbol| {
        let benchmark = benchmark.clone();
        let start = start.clone();
        let end = end.clone();
        let tool_policy = tool_policy.clone();
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
            let mut bundle = Map::new();
            bundle.insert("symbol".into(), json!(symbol));
            bundle.insert("market".into(), market?);
            bundle.insert("fundamentals".into(), fundamentals?);
            bundle.insert("events".into(), events?);
            bundle.insert("news".into(), news?);
            bundle.insert("reconciliation".into(), reconciliation?);
            bundle.insert("joinquant".into(), joinquant?);
            bundle.insert("optional_sources".into(), optional?);
            compact_security_bundle(&mut bundle, is_exhaustive);
            Ok::<Value, ServiceError>(Value::Object(bundle))
        }
    }))
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    let payload = json!({"depth": depth, "securities": bundles});
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
}
