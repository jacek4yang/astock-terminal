mod agent_context;
mod analysis;
mod backtest;
mod credentials;
mod data_quality;
mod data_root;
mod disclosure_sync;
mod event_analysis;
mod event_store;
mod global_sync;
mod graph_services;
mod market_regime;
mod news_center;
mod quant_research;
mod relation_extraction;
mod report_verification;
mod scan;
mod settings;

use astock_core::{Adjust, KlinePeriod, Symbol};
use astock_fundamental::{
    apply_driver_shocks, build_earnings_driver_tree, DriverShock, EarningsDriverTree,
    FundamentalClient, ShockBridge,
};
use astock_market_data::{DataProvider, EastMoneyF10, MarketData, FINANCE_NEWS_SOURCES};
use astock_protocol::{RequestEnvelope, ResponseEnvelope, PROTOCOL_VERSION};
use astock_source_verification::SourceVerifier;
use astock_storage::{CleanupPolicy, Storage};
use astock_trading_rules::RuleSet;
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime, TimeZone};
use data_root::DataRootDecision;
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct Engine {
    started_at_ms: u128,
    storage: Storage,
    market: Arc<MarketData>,
    fundamental: Arc<FundamentalClient>,
    graph: astock_graph::GraphStore,
    rules: RuleSet,
    scan: scan::ScanService,
    disclosure_sync: disclosure_sync::DisclosureSyncService,
    event_analysis: event_analysis::EventAnalysisService,
    relation_extraction: relation_extraction::RelationExtractionService,
    backtest: backtest::BacktestService,
    quant_research: quant_research::QuantResearchService,
    global_sync: global_sync::GlobalSyncService,
    provider_boot: credentials::BootStatus,
    credential_migration_error: Option<String>,
    data_root: DataRootDecision,
}

impl Engine {
    pub async fn initialize() -> Result<Self, String> {
        let (storage, data_root) = data_root::resolve_and_open().await?;
        let credential_migration_error = credentials::migrate_legacy(&storage)
            .await
            .err()
            .map(|error| error.message);
        let provider_boot = credentials::load_into_environment()
            .map_err(|error| format!("load provider credentials: {}", error.message))?;
        event_store::migrate(&storage)
            .await
            .map_err(|error| format!("migrate Agent event store: {error}"))?;
        let market = Arc::new(MarketData::with_storage(storage.clone()));
        let fundamental = Arc::new(FundamentalClient::new(Arc::new(EastMoneyF10::new(
            market.http.clone(),
            market.cache.clone(),
        ))));
        let graph = astock_graph::GraphStore::new(storage.clone());
        if let Err(error) = astock_graph::seed_if_empty(&graph).await {
            tracing::warn!(%error, "graph seed failed; graph services will expose partial coverage");
        }
        if let (Ok(Some(username)), Ok(Some(password))) = (
            joinquant_username_store().load_key(),
            joinquant_password_store().load_key(),
        ) {
            let _ = market
                .joinquant
                .configure(username.expose().to_string(), password.expose().to_string());
        }
        let rules = RuleSet::load(None).map_err(|error| format!("load A-share rules: {error}"))?;
        let quant_research = quant_research::QuantResearchService::restore(&storage)
            .await
            .map_err(|error| format!("restore Quant Lab jobs: {error}"))?;
        let backtest = backtest::BacktestService::restore(&storage)
            .await
            .map_err(|error| format!("restore backtest jobs: {error}"))?;
        if let Ok(records) = storage.securities_list().await {
            market.security_master.merge_records(records);
        }
        Ok(Self {
            started_at_ms: now_ms(),
            storage,
            market,
            fundamental,
            graph,
            rules,
            scan: scan::ScanService::default(),
            disclosure_sync: disclosure_sync::DisclosureSyncService::default(),
            event_analysis: event_analysis::EventAnalysisService::default(),
            relation_extraction: relation_extraction::RelationExtractionService::default(),
            backtest,
            quant_research,
            global_sync: global_sync::GlobalSyncService::default(),
            provider_boot,
            credential_migration_error,
            data_root,
        })
    }

    pub async fn dispatch(&self, request: &RequestEnvelope) -> ResponseEnvelope {
        if request.protocol_version != PROTOCOL_VERSION {
            return ResponseEnvelope::failure(
                request,
                "protocol_version_mismatch",
                format!(
                    "engine requires protocol {}, received {}",
                    PROTOCOL_VERSION, request.protocol_version
                ),
                false,
            );
        }
        match self.dispatch_inner(request).await {
            Ok(payload) => ResponseEnvelope::success(request, payload),
            Err(error) => {
                ResponseEnvelope::failure(request, error.code, error.message, error.retryable)
            }
        }
    }

    async fn dispatch_inner(&self, request: &RequestEnvelope) -> Result<Value, ServiceError> {
        match request.kind.as_str() {
            "system.handshake" => Ok(json!({
                "protocol_version": PROTOCOL_VERSION,
                "engine_version": ENGINE_VERSION,
                "capabilities": ["market", "research", "fundamentals", "valuation", "multi_source_news", "security_events", "global_context", "data_quality", "market_scan", "backtest_v2", "market_regime", "storage", "credentials", "agent_event_store_v2"],
                "max_frame_bytes": astock_protocol::MAX_FRAME_BYTES,
                "max_page_size": astock_protocol::MAX_PAGE_SIZE
            })),
            "diagnostics.status" => Ok(json!({
                "status": "ready",
                "pid": std::process::id(),
                "engine_version": ENGINE_VERSION,
                "protocol_version": PROTOCOL_VERSION,
                "uptime_ms": now_ms().saturating_sub(self.started_at_ms),
                "data_root": self.data_root,
                "provider_health": self.market.provider_health(),
                "credential_migration_error": self.credential_migration_error,
            })),
            "diagnostics.data_quality" => {
                let payload: data_quality::DataQualityQuery = decode_payload(&request.payload)?;
                data_quality::query(self, payload).await
            }
            "market.session" => Ok(market_session_payload(
                &self.rules,
                astock_core::time::now_china(),
            )),
            "market.overview" => {
                let fetched = self.market.market_breadth().await.map_err(upstream)?;
                let breadth = fetched.data;
                Ok(json!({
                    "breadth": {
                        "up": breadth.up,
                        "down": breadth.down,
                        "flat": breadth.flat,
                        "total": breadth.total,
                        "breadth_ratio": breadth.ratio()
                    },
                    "source": fetched.source,
                    "fetched_at": fetched.fetched_at,
                    "provider_health": self.market.provider_health()
                }))
            }
            "market.search" => {
                let payload: SearchPayload = decode_payload(&request.payload)?;
                let result = self
                    .market
                    .search(payload.keyword.trim())
                    .await
                    .map_err(upstream)?;
                Ok(json!({
                    "items": result.data,
                    "source": result.source,
                    "fetched_at": result.fetched_at,
                    "limit": astock_protocol::MAX_PAGE_SIZE
                }))
            }
            "market.quote" => {
                let payload: SymbolPayload = decode_payload(&request.payload)?;
                let symbol = parse_live_symbol(&payload.symbol)?;
                let result = self.market.quote(&symbol).await.map_err(upstream)?;
                Ok(json!({
                    "quote": result.data,
                    "source": result.source,
                    "fetched_at": result.fetched_at,
                    "stale": Value::Null,
                    "quality": Value::Null
                }))
            }
            "market.kline" => {
                let payload: KlinePayload = decode_payload(&request.payload)?;
                let symbol = parse_live_symbol(&payload.symbol)?;
                let period = parse_period(&payload.period)?;
                let adjust = parse_adjust(&payload.adjust)?;
                let count = payload.count.clamp(1, 10_000);
                let result = self
                    .market
                    .kline(&symbol, period, adjust, count)
                    .await
                    .map_err(upstream)?;
                Ok(json!({
                    "bars": result.data,
                    "source": result.source,
                    "fetched_at": result.fetched_at,
                    "stale": Value::Null,
                    "quality": Value::Null
                }))
            }
            "market.index_kline" => {
                let payload: IndexKlinePayload = decode_payload(&request.payload)?;
                let count = payload.count.clamp(1, 10_000);
                let result = self
                    .market
                    .index_kline(payload.secid.trim(), count)
                    .await
                    .map_err(upstream)?;
                Ok(json!({
                    "bars": result.data,
                    "source": result.source,
                    "fetched_at": result.fetched_at
                }))
            }
            "market.shares.page" => {
                let payload: SharesPagePayload = decode_payload(&request.payload)?;
                let cursor = payload.cursor.unwrap_or(0);
                let limit = payload
                    .limit
                    .unwrap_or(astock_protocol::MAX_PAGE_SIZE)
                    .clamp(1, astock_protocol::MAX_PAGE_SIZE);
                let fetched = self.market.all_a_shares().await.map_err(upstream)?;
                let snapshot_id = format!(
                    "market-shares:{}:{}",
                    fetched.source,
                    fetched.fetched_at.timestamp_millis()
                );
                if payload
                    .snapshot_id
                    .as_ref()
                    .is_some_and(|requested| requested != &snapshot_id)
                {
                    return Err(ServiceError::new(
                        "stale_snapshot",
                        "market snapshot changed; restart pagination",
                        true,
                    ));
                }
                if cursor > fetched.data.len() {
                    return Err(ServiceError::new(
                        "invalid_cursor",
                        format!(
                            "cursor {cursor} exceeds market row count {}",
                            fetched.data.len()
                        ),
                        false,
                    ));
                }
                let end = cursor.saturating_add(limit).min(fetched.data.len());
                let source = fetched.source.to_string();
                let fetched_at = fetched.fetched_at;
                let items = fetched.data[cursor..end]
                    .iter()
                    .map(|item| {
                        let identity = self.market.security_master.get(&item.code);
                        json!({
                            "code": item.code,
                            "name": identity.as_ref().map_or_else(
                                || item.name.clone(),
                                |row| row.canonical_name.clone()
                            ),
                            "market": identity.as_ref().map_or_else(
                                || "unknown".to_string(),
                                |row| row.market.to_string()
                            ),
                            "board": identity.as_ref().map_or_else(
                                || "other".to_string(),
                                |row| format!("{:?}", row.board).to_lowercase()
                            ),
                            "price": item.price,
                            "pct": item.pct,
                            "amount": item.amount,
                            "source": source,
                            "fetched_at": fetched_at
                        })
                    })
                    .collect::<Vec<_>>();
                self.storage
                    .securities_upsert(self.market.security_master.all())
                    .await
                    .map_err(storage)?;
                Ok(json!({
                    "items": items,
                    "cursor": cursor,
                    "next_cursor": (end < fetched.data.len()).then_some(end),
                    "total": fetched.data.len(),
                    "limit": limit,
                    "snapshot_id": snapshot_id,
                    "source_version_id": snapshot_id,
                    "source": fetched.source,
                    "fetched_at": fetched.fetched_at
                }))
            }
            "market.security_snapshot" => {
                let payload: KlinePayload = decode_payload(&request.payload)?;
                let symbol = parse_live_symbol(&payload.symbol)?;
                let period = parse_period(&payload.period)?;
                let adjust = parse_adjust(&payload.adjust)?;
                let count = payload.count.clamp(1, 10_000);
                let quote = self.market.quote(&symbol).await.map_err(upstream)?.data;
                let (kline_result, flow_result) = tokio::join!(
                    self.market.kline(&symbol, period, adjust, count),
                    self.market.fund_flow_daily(&symbol, 30)
                );
                let mut missing = Vec::new();
                let flows = match flow_result {
                    Ok(fetched) => Some(fetched.data),
                    Err(_) => {
                        missing.push("fund_flow_30d");
                        None
                    }
                };
                let fund_flow_30d = flows.as_deref().map(flow_rows);
                let (kline, signal, chanlun) = match kline_result {
                    Ok(fetched) if !fetched.data.is_empty() => {
                        let source = fetched.source.to_string();
                        let signal = analysis::signal(
                            &self.market,
                            &self.rules,
                            &symbol,
                            &fetched.data,
                            &quote,
                            flows.as_deref(),
                            &source,
                        )
                        .await;
                        let chanlun = match analysis::chanlun(&symbol, &fetched.data) {
                            Ok(value) => Some(value),
                            Err(_) => {
                                missing.push("chanlun_daily");
                                None
                            }
                        };
                        (
                            Some(json!({"bars": fetched.data, "source": source})),
                            Some(signal),
                            chanlun,
                        )
                    }
                    _ => {
                        missing.extend(["kline", "analysis", "chanlun_daily"]);
                        (None, None, None)
                    }
                };
                Ok(json!({
                    "quote": quote,
                    "kline": kline,
                    "fund_flow_30d": fund_flow_30d,
                    "analysis": signal,
                    "chanlun_daily": chanlun,
                    "missing": missing
                }))
            }
            "research.market_candidates" => {
                let payload: ResearchCandidatesPayload = decode_payload(&request.payload)?;
                let (shares, industry_rows) = tokio::join!(
                    self.market.all_a_shares(),
                    self.market.eastmoney.industry_map(),
                );
                let fetched = shares.map_err(upstream)?;
                // Industry enrichment is useful for portfolio concentration
                // checks but must not make the liquid-universe discovery path
                // unavailable. Its independent failure remains visible.
                let (industries, industry_enrichment) = match industry_rows {
                    Ok(rows) => (
                        rows.data
                            .into_iter()
                            .map(|row| (row.code, row.industry))
                            .collect::<BTreeMap<_, _>>(),
                        json!({
                            "ok": true,
                            "source": rows.source,
                            "fetched_at": rows.fetched_at,
                        }),
                    ),
                    Err(error) => (
                        BTreeMap::new(),
                        json!({
                            "ok": false,
                            "source": "EastMoney",
                            "error": error.to_string(),
                        }),
                    ),
                };
                let max_lot_cost = payload
                    .max_lot_cost
                    .filter(|value| value.is_finite() && *value > 0.0);
                let mut rows = fetched
                    .data
                    .into_iter()
                    .filter(|row| {
                        let price = row.price.unwrap_or_default();
                        let amount = row.amount.unwrap_or_default();
                        let name = row.name.trim().to_ascii_uppercase();
                        price > 0.0
                            && amount > 0.0
                            && !name.contains("ST")
                            && !name.contains('退')
                            && max_lot_cost.is_none_or(|budget| price * 100.0 <= budget)
                    })
                    .collect::<Vec<_>>();
                rows.sort_by(|left, right| {
                    right
                        .amount
                        .unwrap_or_default()
                        .total_cmp(&left.amount.unwrap_or_default())
                });
                let limit = payload.limit.unwrap_or(40).clamp(5, 100);
                let items = rows
                    .into_iter()
                    .take(limit)
                    .map(|row| {
                        let identity = self.market.security_master.get(&row.code);
                        let industry = industries
                            .get(&row.code)
                            .cloned()
                            .or_else(|| identity.as_ref().and_then(|value| value.industry.clone()));
                        json!({
                            "symbol": row.code,
                            "name": identity.as_ref().map_or(row.name, |value| value.canonical_name.clone()),
                            "market": identity.as_ref().map(|value| value.market.to_string()),
                            "board": identity.as_ref().map(|value| format!("{:?}", value.board).to_lowercase()),
                            "industry": industry,
                            "price": row.price,
                            "pct": row.pct,
                            "amount": row.amount,
                            "lot_cost": row.price.map(|price| (price * 10_000.0).round() / 100.0),
                        })
                    })
                    .collect::<Vec<_>>();
                Ok(json!({
                    "items": items,
                    "selection_rule": "active_a_share_sorted_by_turnover_amount_then_agent_review",
                    "source": fetched.source,
                    "fetched_at": fetched.fetched_at,
                    "industry_enrichment": industry_enrichment,
                    "excluded": ["missing_or_zero_quote", "missing_turnover", "risk_warning_name", "delisting_name", "lot_cost_above_budget"]
                }))
            }
            "research.fundamentals" => {
                let payload: SymbolPayload = decode_payload(&request.payload)?;
                let symbol = parse_live_symbol(&payload.symbol)?;
                let outcome = self.fundamental.bundle(&symbol).await;
                Ok(fundamental_research_payload(&symbol, outcome))
            }
            "research.earnings_driver.tree" => {
                let payload: SymbolPayload = decode_payload(&request.payload)?;
                let symbol = parse_live_symbol(&payload.symbol)?;
                let outcome = self.fundamental.bundle(&symbol).await;
                let tree = build_earnings_driver_tree(symbol.code(), &outcome.bundle, now_secs());
                persist_driver_tree(&self.storage, &tree).await?;
                serde_json::to_value(tree).map_err(invalid)
            }
            "research.earnings_driver.shock" => {
                let payload: EarningsDriverShockPayload = decode_payload(&request.payload)?;
                if payload.shocks.len() > 20 {
                    return Err(ServiceError::new(
                        "invalid_payload",
                        "单次最多计算 20 个冲击",
                        false,
                    ));
                }
                let symbol = parse_live_symbol(&payload.symbol)?;
                let outcome = self.fundamental.bundle(&symbol).await;
                let tree = build_earnings_driver_tree(symbol.code(), &outcome.bundle, now_secs());
                persist_driver_tree(&self.storage, &tree).await?;
                let bridge = apply_driver_shocks(&tree, &payload.shocks);
                persist_driver_shock_bridge(&self.storage, &bridge).await?;
                serde_json::to_value(bridge).map_err(invalid)
            }
            "research.earnings_driver.snapshot" => {
                let payload: EarningsDriverSnapshotPayload = decode_payload(&request.payload)?;
                let tree = load_driver_tree(&self.storage, payload.snapshot_id).await?;
                serde_json::to_value(tree).map_err(invalid)
            }
            "research.news" => {
                let payload: ResearchNewsPayload = decode_payload(&request.payload)?;
                let sources = payload.sources.unwrap_or_else(|| {
                    FINANCE_NEWS_SOURCES
                        .iter()
                        .map(|(id, _, _)| (*id).to_string())
                        .collect()
                });
                let symbol = match payload.symbol.as_deref() {
                    Some(value) => Some(parse_live_symbol(value)?.code().to_string()),
                    None => None,
                };
                let limit = payload.limit.unwrap_or(80).clamp(10, 160);
                // A slow discovery aggregator must never hold the whole Agent
                // run until its outer IPC deadline. Cut it off early, then
                // merge the immutable local archive and official disclosures.
                let live = tokio::time::timeout(
                    std::time::Duration::from_secs(45),
                    self.market.finance_news.research(
                        &sources,
                        symbol.as_deref(),
                        payload.keyword.as_deref(),
                        limit,
                    ),
                )
                .await;
                let mut items = Vec::new();
                let mut successful_sources = Vec::new();
                let mut stale_sources = Vec::new();
                let mut errors = Vec::new();
                match live {
                    Ok(Ok(batch)) => {
                        items.extend(batch.items.into_iter().map(normalized_news_item));
                        successful_sources.extend(batch.successful_sources);
                        stale_sources.extend(batch.stale_sources);
                        errors.extend(batch.errors);
                    }
                    Ok(Err(error)) => errors.push(format!("live_discovery: {error}")),
                    Err(_) => errors.push(
                        "live_discovery: 45s deadline exceeded; used durable/official fallbacks"
                            .to_string(),
                    ),
                }

                let archived = self
                    .storage
                    .news_archive_recent(limit.saturating_mul(8))
                    .await
                    .map_err(storage)?;
                let query = payload
                    .keyword
                    .as_deref()
                    .unwrap_or_default()
                    .to_lowercase();
                let symbol_filter = symbol.as_deref().unwrap_or_default();
                for row in archived {
                    let haystack = format!("{} {}", row.title, row.factual_summary).to_lowercase();
                    if (!query.is_empty() && !haystack.contains(&query))
                        || (!symbol_filter.is_empty()
                            && !haystack.contains(symbol_filter)
                            && !query.is_empty())
                    {
                        continue;
                    }
                    items.push(json!({
                        "document_id": row.document_id,
                        "revision_id": row.revision_id,
                        "source_id": row.source_id,
                        "source_name": row.source_name,
                        "provider_id": "durable_news_archive",
                        "title": row.title,
                        "summary": truncate_text(&row.factual_summary, 360),
                        "url": row.canonical_url,
                        "published_at": row.publish_time,
                        "important": false,
                        "trust_tier": "archived_revision",
                        "trust_tier_name": "不可变历史修订",
                        "independent_source_count": 1,
                        "old_republication": false,
                        "entity_links": [],
                        "last_observed_at": row.last_observed_at,
                    }));
                }
                if !items.is_empty() {
                    stale_sources.push("durable_news_archive".to_string());
                }

                let today = astock_core::time::now_china().date_naive();
                let begin = today - chrono::Duration::days(if symbol.is_some() { 365 } else { 14 });
                let official = tokio::time::timeout(
                    std::time::Duration::from_secs(35),
                    self.market.em_datacenter.notices(
                        symbol.as_deref(),
                        astock_market_data::providers::NoticeNode::All,
                        Some(begin),
                        Some(today),
                        if symbol.is_some() { 3 } else { 2 },
                    ),
                )
                .await;
                match official {
                    Ok(Ok(fetched)) => {
                        successful_sources.push("official-a-share-announcements".to_string());
                        items.extend(fetched.data.into_iter().map(|row| json!({
                            "document_id": format!("eastmoney-notice:{}", row.art_code),
                            "revision_id": null,
                            "source_id": "eastmoney-announcement-mirror",
                            "source_name": "沪深京上市公司公告镜像",
                            "provider_id": "official-a-share-announcements",
                            "title": row.title,
                            "summary": format!("{} · {}", row.stock_name, row.column_name),
                            "url": row.url,
                            "published_at": row.notice_date,
                            "important": true,
                            "trust_tier": "official_mirror",
                            "trust_tier_name": "公司公告发现层（需回链交易所原文）",
                            "independent_source_count": 1,
                            "old_republication": false,
                            "entity_links": [{ "code": row.stock_code, "name": row.stock_name }],
                        })));
                    }
                    Ok(Err(error)) => errors.push(format!("official_announcements: {error}")),
                    Err(_) => {
                        errors.push("official_announcements: 35s deadline exceeded".to_string())
                    }
                }
                // Preserve the provider's newest-first order. A BTreeMap here
                // would sort opaque document ids and could evict a newer
                // announcement merely because its key sorts later.
                let mut seen = HashSet::<String>::new();
                let mut deduplicated = Vec::<Value>::new();
                for item in items {
                    let key = item
                        .get("revision_id")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("document_id").and_then(Value::as_str))
                        .or_else(|| item.get("url").and_then(Value::as_str))
                        .unwrap_or("unknown")
                        .to_string();
                    if seen.insert(key) {
                        deduplicated.push(item);
                    }
                }
                let items = deduplicated.into_iter().take(limit).collect::<Vec<_>>();
                let successful_channels = items
                    .iter()
                    .filter_map(|item| item.get("source_id").and_then(Value::as_str))
                    .filter(|value| !value.is_empty())
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                Ok(json!({
                    "items": items,
                    "successful_sources": successful_sources,
                    "successful_channels": successful_channels,
                    "stale_sources": stale_sources,
                    "errors": errors,
                    "requested_source_count": sources.len(),
                    "symbol": symbol,
                    "evidence_note": "successful_sources统计采集Provider，successful_channels统计内容频道；无revision_id的实时条目只作发现证据，重要结论必须优先回链不可变归档、公告、交易所或公司原文"
                }))
            }
            "research.news.providers" => {
                let providers = self.market.finance_news.provider_health().await;
                serde_json::to_value(providers).map_err(serialize_error)
            }
            "research.news.center" => {
                let query: news_center::NewsCenterQuery = decode_payload(&request.payload)?;
                let page = news_center::query(self.storage.clone(), &self.rules, query)
                    .await
                    .map_err(|error| ServiceError::new("news_center", error, false))?;
                serde_json::to_value(page).map_err(serialize_error)
            }
            "research.news.provider.set" => {
                let payload: NewsProviderTogglePayload = decode_payload(&request.payload)?;
                let provider_id = payload.provider_id.trim();
                if provider_id.is_empty() || provider_id.len() > 128 {
                    return Err(ServiceError::new(
                        "invalid_payload",
                        "provider_id 必须是 1 至 128 个字符",
                        false,
                    ));
                }
                self.market
                    .finance_news
                    .set_provider_enabled(provider_id, payload.enabled)
                    .await
                    .map_err(|error| {
                        ServiceError::new(
                            format!("news_provider_{:?}", error.kind).to_ascii_lowercase(),
                            error.message,
                            error.retryable,
                        )
                    })?;
                Ok(Value::Null)
            }
            "research.news.archive.recent" => {
                let payload: LimitPayload = decode_payload(&request.payload)?;
                let rows = self
                    .storage
                    .news_archive_recent(payload.limit.unwrap_or(100).clamp(1, 500))
                    .await
                    .map_err(storage)?;
                serde_json::to_value(rows).map_err(serialize_error)
            }
            "research.news.archive.revisions" => {
                let payload: NewsDocumentPayload = decode_payload(&request.payload)?;
                let document_id = validate_news_identifier(&payload.document_id, "document_id")?;
                let rows = self
                    .storage
                    .news_archive_revisions(document_id)
                    .await
                    .map_err(storage)?;
                if rows.len() > 500 {
                    return Err(ServiceError::new(
                        "result_too_large",
                        "单一资讯文档的修订链超过 500 条，请先运行归档诊断",
                        false,
                    ));
                }
                serde_json::to_value(rows).map_err(serialize_error)
            }
            "research.news.archive.integrity" => {
                let status = self
                    .storage
                    .news_archive_integrity_check()
                    .await
                    .map_err(storage)?;
                Ok(Value::String(status))
            }
            "research.news.archive.observations" => {
                let payload: NewsObservationPayload = decode_payload(&request.payload)?;
                let provider_id = payload
                    .provider_id
                    .as_deref()
                    .map(|value| validate_news_identifier(value, "provider_id"))
                    .transpose()?;
                let rows = self
                    .storage
                    .news_ingest_observations(
                        provider_id,
                        payload.limit.unwrap_or(10).clamp(1, 500),
                    )
                    .await
                    .map_err(storage)?;
                serde_json::to_value(rows).map_err(serialize_error)
            }
            "research.news.user_state" => {
                let payload: NewsUserStatePayload = decode_payload(&request.payload)?;
                let document_id = validate_news_identifier(&payload.document_id, "document_id")?;
                let action = match payload.action.as_str() {
                    "read" => astock_storage::NewsUserAction::Read,
                    "pinned" => astock_storage::NewsUserAction::Pinned,
                    "favorite" => astock_storage::NewsUserAction::Favorite,
                    "ignored" => astock_storage::NewsUserAction::Ignored,
                    _ => {
                        return Err(ServiceError::new(
                            "invalid_payload",
                            "action 只能是 read/pinned/favorite/ignored",
                            false,
                        ))
                    }
                };
                let state = self
                    .storage
                    .news_user_state_set(document_id, action, payload.value)
                    .await
                    .map_err(storage)?;
                serde_json::to_value(state).map_err(serialize_error)
            }
            "research.news.clusters.list" => {
                let payload: LimitPayload = decode_payload(&request.payload)?;
                let rows = astock_news_intelligence::NewsEventClusterer::new(self.storage.clone())
                    .clusters_recent(payload.limit.unwrap_or(100).clamp(1, 500))
                    .await
                    .map_err(news_intelligence_error)?;
                serde_json::to_value(rows).map_err(serialize_error)
            }
            "research.news.clusters.detail" => {
                let payload: NewsClusterPayload = decode_payload(&request.payload)?;
                let cluster_id = validate_news_identifier(&payload.cluster_id, "cluster_id")?;
                let detail =
                    astock_news_intelligence::NewsEventClusterer::new(self.storage.clone())
                        .cluster_detail(cluster_id)
                        .await
                        .map_err(news_intelligence_error)?;
                serde_json::to_value(detail).map_err(serialize_error)
            }
            "research.news.clusters.merge" => {
                let payload: NewsClusterMergePayload = decode_payload(&request.payload)?;
                let from = validate_news_identifier(&payload.from_cluster_id, "from_cluster_id")?;
                let to = validate_news_identifier(&payload.to_cluster_id, "to_cluster_id")?;
                let reason = validate_review_reason(&payload.reason)?;
                if from == to {
                    return Err(ServiceError::new(
                        "invalid_payload",
                        "不能把事件簇合并到自身",
                        false,
                    ));
                }
                let detail =
                    astock_news_intelligence::NewsEventClusterer::new(self.storage.clone())
                        .manual_merge(from, to, reason)
                        .await
                        .map_err(news_intelligence_error)?;
                serde_json::to_value(detail).map_err(serialize_error)
            }
            "research.news.clusters.split" => {
                let payload: NewsClusterSplitPayload = decode_payload(&request.payload)?;
                let revision_id = validate_news_identifier(&payload.revision_id, "revision_id")?;
                let reason = validate_review_reason(&payload.reason)?;
                let detail =
                    astock_news_intelligence::NewsEventClusterer::new(self.storage.clone())
                        .manual_split(revision_id, reason)
                        .await
                        .map_err(news_intelligence_error)?;
                serde_json::to_value(detail).map_err(serialize_error)
            }
            "research.news.reviews.list" => {
                let payload: LimitPayload = decode_payload(&request.payload)?;
                let rows = astock_news_intelligence::NewsEventClusterer::new(self.storage.clone())
                    .pending_reviews(payload.limit.unwrap_or(100).clamp(1, 500))
                    .await
                    .map_err(news_intelligence_error)?;
                serde_json::to_value(rows).map_err(serialize_error)
            }
            "research.news.reviews.resolve" => {
                let payload: NewsReviewResolvePayload = decode_payload(&request.payload)?;
                let task_id = validate_news_identifier(&payload.task_id, "task_id")?;
                let conclusion_key =
                    validate_news_identifier(&payload.conclusion_key, "conclusion_key")?;
                let triggering_revision =
                    validate_news_identifier(&payload.triggering_revision, "triggering_revision")?;
                let resolved =
                    astock_news_intelligence::NewsEventClusterer::new(self.storage.clone())
                        .resolve_review(task_id, conclusion_key, triggering_revision)
                        .await
                        .map_err(news_intelligence_error)?;
                Ok(Value::Bool(resolved))
            }
            "research.entities.links" => {
                let payload: EntityLinksPayload = decode_payload(&request.payload)?;
                if payload.revision_ids.is_empty() || payload.revision_ids.len() > 100 {
                    return Err(ServiceError::new(
                        "invalid_payload",
                        "revision_ids 必须包含 1 至 100 个修订 ID",
                        false,
                    ));
                }
                let linker = astock_entity_linking::EntityLinker::new(self.storage.clone());
                let mut links = Vec::new();
                let mut seen = HashSet::new();
                for revision_id in payload.revision_ids {
                    let revision_id =
                        validate_news_identifier(&revision_id, "revision_id")?.to_string();
                    if seen.insert(revision_id.clone()) {
                        links.extend(
                            linker
                                .link_revision(&revision_id)
                                .await
                                .map_err(entity_linking_error)?,
                        );
                    }
                }
                serde_json::to_value(links).map_err(serialize_error)
            }
            "research.entities.reviews" => {
                let payload: LimitPayload = decode_payload(&request.payload)?;
                let rows = astock_entity_linking::EntityLinker::new(self.storage.clone())
                    .pending_reviews(payload.limit.unwrap_or(50).clamp(1, 500))
                    .await
                    .map_err(entity_linking_error)?;
                serde_json::to_value(rows).map_err(serialize_error)
            }
            "research.entities.resolve" => {
                let payload: EntityReviewResolvePayload = decode_payload(&request.payload)?;
                let link_id = validate_news_identifier(&payload.link_id, "link_id")?;
                let entity_id = payload
                    .entity_id
                    .as_deref()
                    .map(|value| validate_news_identifier(value, "entity_id"))
                    .transpose()?;
                let reason = validate_review_reason(&payload.reason)?;
                let resolved = astock_entity_linking::EntityLinker::new(self.storage.clone())
                    .resolve_review(link_id, entity_id, payload.accept, reason)
                    .await
                    .map_err(entity_linking_error)?;
                Ok(Value::Bool(resolved))
            }
            "research.events.analysis.start" => {
                let payload: event_analysis::EventAnalysisRequest =
                    decode_payload(&request.payload)?;
                let started = self
                    .event_analysis
                    .start(
                        self.storage.clone(),
                        self.market.clone(),
                        self.fundamental.clone(),
                        self.rules.clone(),
                        payload,
                    )
                    .await
                    .map_err(|error| ServiceError::new("event_analysis", error, false))?;
                serde_json::to_value(started).map_err(serialize_error)
            }
            "research.events.analysis.status" => {
                let payload: EventJobPayload = decode_payload(&request.payload)?;
                let snapshot = self
                    .event_analysis
                    .status(&payload.job_id)
                    .map_err(|error| ServiceError::new("event_job_not_found", error, false))?;
                serde_json::to_value(snapshot).map_err(serialize_error)
            }
            "research.events.analysis.cancel" => {
                let payload: EventJobPayload = decode_payload(&request.payload)?;
                let cancelled = self
                    .event_analysis
                    .cancel(&payload.job_id)
                    .map_err(|error| ServiceError::new("event_analysis", error, false))?;
                Ok(json!({ "cancelled": cancelled }))
            }
            "research.relations.extraction.start" => {
                let payload: relation_extraction::RelationExtractionRequest =
                    decode_payload(&request.payload)?;
                let started = self
                    .relation_extraction
                    .start(self.storage.clone(), payload)
                    .await
                    .map_err(|error| ServiceError::new("relation_extraction", error, false))?;
                serde_json::to_value(started).map_err(serialize_error)
            }
            "research.relations.extraction.status" => {
                let payload: RelationJobPayload = decode_payload(&request.payload)?;
                let snapshot = self
                    .relation_extraction
                    .status(&payload.job_id)
                    .map_err(|error| ServiceError::new("relation_job_not_found", error, false))?;
                serde_json::to_value(snapshot).map_err(serialize_error)
            }
            "research.relations.extraction.cancel" => {
                let payload: RelationJobPayload = decode_payload(&request.payload)?;
                let cancelled = self
                    .relation_extraction
                    .cancel(&payload.job_id)
                    .map_err(|error| ServiceError::new("relation_extraction", error, false))?;
                Ok(json!({ "cancelled": cancelled }))
            }
            "research.relations.reviews" => {
                let payload: RelationReviewPagePayload = decode_payload(&request.payload)?;
                let page =
                    astock_relation_extraction::RelationExtractionStore::new(self.storage.clone())
                        .review_page(
                            payload.status.as_deref(),
                            payload.document_kind,
                            payload.min_confidence_bps,
                            payload.page,
                            payload.page_size,
                        )
                        .await
                        .map_err(relation_extraction_error)?;
                serde_json::to_value(page).map_err(serialize_error)
            }
            "research.relations.review" => {
                let payload: astock_relation_extraction::RelationReviewRequest =
                    decode_payload(&request.payload)?;
                let result =
                    astock_relation_extraction::RelationExtractionStore::new(self.storage.clone())
                        .review(payload)
                        .await
                        .map_err(relation_extraction_error)?;
                serde_json::to_value(result).map_err(serialize_error)
            }
            "research.relations.retract" => {
                let payload: RelationRetractPayload = decode_payload(&request.payload)?;
                let result =
                    astock_relation_extraction::RelationExtractionStore::new(self.storage.clone())
                        .retract(&payload.candidate_id, &payload.reason)
                        .await
                        .map_err(relation_extraction_error)?;
                serde_json::to_value(result).map_err(serialize_error)
            }
            "research.graph.subgraph" => {
                let payload: GraphSubgraphPayload = decode_payload(&request.payload)?;
                graph_services::subgraph(
                    &self.graph,
                    &self.market,
                    &self.fundamental,
                    &self.storage,
                    &payload.symbol_or_node,
                    payload.hops,
                )
                .await
                .map_err(graph_service_error)
            }
            "research.graph.as_of" => {
                let payload: GraphAsOfPayload = decode_payload(&request.payload)?;
                graph_services::as_of(
                    &self.graph,
                    &self.market,
                    &self.fundamental,
                    &self.storage,
                    payload.business_time,
                    payload.knowledge_time,
                    payload.symbol_or_node.as_deref(),
                    payload.hops,
                )
                .await
                .map_err(graph_service_error)
            }
            "research.graph.history_bounds" => {
                let value = self
                    .graph
                    .graph_history_bounds()
                    .await
                    .map_err(graph_error)?;
                serde_json::to_value(value).map_err(serialize_error)
            }
            "research.graph.edge_timeline" => {
                let payload: GraphIdentityPayload = decode_payload(&request.payload)?;
                let identity_id = validate_news_identifier(&payload.identity_id, "identity_id")?;
                let value = self
                    .graph
                    .edge_timeline(identity_id)
                    .await
                    .map_err(graph_error)?;
                serde_json::to_value(value).map_err(serialize_error)
            }
            "research.graph.snapshot.get" => {
                let payload: GraphSnapshotPayload = decode_payload(&request.payload)?;
                let snapshot_id = validate_news_identifier(&payload.snapshot_id, "snapshot_id")?;
                let value = self
                    .graph
                    .graph_snapshot(snapshot_id)
                    .await
                    .map_err(graph_error)?;
                serde_json::to_value(value).map_err(serialize_error)
            }
            "research.graph.snapshot.diff" => {
                let payload: GraphSnapshotDiffPayload = decode_payload(&request.payload)?;
                for (field, value) in [
                    ("left_business_time", payload.left_business_time),
                    ("left_knowledge_time", payload.left_knowledge_time),
                    ("right_business_time", payload.right_business_time),
                    ("right_knowledge_time", payload.right_knowledge_time),
                ] {
                    if value <= 0 {
                        return Err(ServiceError::new(
                            "invalid_payload",
                            format!("{field} 必须是正的 Unix 秒时间戳"),
                            false,
                        ));
                    }
                }
                let value = self
                    .graph
                    .compare_graph_snapshots(
                        payload.left_business_time,
                        payload.left_knowledge_time,
                        payload.right_business_time,
                        payload.right_knowledge_time,
                    )
                    .await
                    .map_err(graph_error)?;
                serde_json::to_value(value).map_err(serialize_error)
            }
            "research.graph.shock" => {
                let payload: GraphShockPayload = decode_payload(&request.payload)?;
                graph_services::shock(
                    &self.graph,
                    &payload.subject,
                    &payload.direction,
                    payload.magnitude_pct,
                )
                .await
                .map_err(graph_service_error)
            }
            "research.market.relationship" => {
                let payload: MarketRelationshipPayload = decode_payload(&request.payload)?;
                graph_services::relationship(&self.market, &payload.symbols, payload.window_days)
                    .await
                    .map_err(graph_service_error)
            }
            "research.quant.start" => {
                let config: astock_quant::research::ResearchConfig =
                    decode_payload(&request.payload)?;
                let snapshot = self
                    .quant_research
                    .start(self.market.clone(), self.storage.clone(), config)
                    .await
                    .map_err(quant_research_error)?;
                serde_json::to_value(snapshot).map_err(serialize_error)
            }
            "research.quant.status" => {
                let payload: QuantJobStatusPayload = decode_payload(&request.payload)?;
                let snapshot = self
                    .quant_research
                    .status(payload.job_id.as_deref())
                    .map_err(quant_research_error)?;
                serde_json::to_value(snapshot).map_err(serialize_error)
            }
            "research.quant.cancel" => {
                let payload: QuantJobCancelPayload = decode_payload(&request.payload)?;
                let cancelled = self
                    .quant_research
                    .cancel(&payload.job_id)
                    .map_err(quant_research_error)?;
                Ok(Value::Bool(cancelled))
            }
            "research.quant.snapshots.get" => {
                let payload: QuantSnapshotPayload = decode_payload(&request.payload)?;
                let snapshot = quant_research::snapshot_get(&self.storage, &payload.snapshot_id)
                    .await
                    .map_err(quant_research_error)?;
                serde_json::to_value(snapshot).map_err(serialize_error)
            }
            "research.quant.snapshots.list" => {
                let payload: QuantSnapshotListPayload = decode_payload(&request.payload)?;
                quant_research::snapshot_list(&self.storage, payload.limit)
                    .await
                    .map_err(quant_research_error)
            }
            "research.backtest.strategies" => Ok(backtest::strategies()),
            "research.backtest.run" => {
                let payload: backtest::BacktestRequest = decode_payload(&request.payload)?;
                backtest::run(&self.market, &self.rules, payload)
                    .await
                    .map_err(backtest_error)
            }
            "research.backtest.start" => {
                let payload: backtest::BacktestRequest = decode_payload(&request.payload)?;
                self.backtest
                    .start(
                        self.market.clone(),
                        self.rules.clone(),
                        self.storage.clone(),
                        payload,
                    )
                    .await
                    .map_err(backtest_error)
            }
            "research.backtest.status" => {
                serde_json::to_value(self.backtest.status()).map_err(serialize_error)
            }
            "research.backtest.cancel" => Ok(json!({"cancelled": self.backtest.cancel()})),
            "research.market.regime" => market_regime::get(&self.market)
                .await
                .map_err(market_regime_error),
            "research.data_reconcile" => {
                let payload: SymbolPayload = decode_payload(&request.payload)?;
                let symbol = parse_live_symbol(&payload.symbol)?;
                let (tdx_quote, eastmoney_quote, tdx_bars, eastmoney_bars, tencent_bars, sina_bars) = tokio::join!(
                    self.market.tdx.quote(&symbol),
                    self.market.eastmoney.quote(&symbol),
                    self.market
                        .tdx
                        .kline(&symbol, KlinePeriod::Day, Adjust::None, 120),
                    self.market
                        .eastmoney
                        .kline(&symbol, KlinePeriod::Day, Adjust::None, 120),
                    self.market
                        .tencent
                        .kline(&symbol, KlinePeriod::Day, Adjust::None, 120),
                    self.market
                        .sina
                        .kline(&symbol, KlinePeriod::Day, Adjust::None, 120),
                );
                Ok(reconcile_market_sources(
                    symbol.code(),
                    tdx_quote,
                    eastmoney_quote,
                    tdx_bars,
                    eastmoney_bars,
                    tencent_bars,
                    sina_bars,
                ))
            }
            "research.quote_reconcile" => {
                let payload: SymbolPayload = decode_payload(&request.payload)?;
                data_quality::reconcile_quote(self, payload.symbol.trim()).await
            }
            "research.valuation_reconcile" => {
                let payload: SymbolPayload = decode_payload(&request.payload)?;
                data_quality::reconcile_valuation(self, payload.symbol.trim()).await
            }
            "research.joinquant_context" => {
                let payload: JoinQuantResearchPayload = decode_payload(&request.payload)?;
                let symbol = parse_live_symbol(&payload.symbol)?;
                if !self.market.joinquant.available() {
                    return Ok(json!({
                        "configured": false,
                        "symbol": symbol.code(),
                        "source": "JoinQuant",
                        "datasets": {},
                        "capabilities": ["qfq_daily", "valuation", "benchmark_components", "macro_cpi"],
                        "evidence_note": "聚宽未配置；没有用其他来源伪装聚宽数据。请在配置页通过掩码输入安全保存后重试"
                    }));
                }
                let start = parse_research_date(&payload.start, "start")?;
                let end = parse_research_date(&payload.end, "end")?;
                if start > end || (end - start).num_days() > 1_830 {
                    return Err(ServiceError::new(
                        "invalid_research_window",
                        "聚宽研究区间必须按时间正序且不超过5年",
                        false,
                    ));
                }
                let symbols = [symbol.clone()];
                let (daily, valuation, benchmark_components, macro_cpi) = tokio::join!(
                    self.market.joinquant.daily(&symbol, start, end),
                    self.market.joinquant.valuation(&symbols, end),
                    self.market
                        .joinquant
                        .index_components(payload.benchmark.trim(), end),
                    self.market.joinquant.macro_cpi(24),
                );
                Ok(json!({
                    "configured": true,
                    "symbol": symbol.code(),
                    "benchmark": payload.benchmark,
                    "start": start,
                    "end": end,
                    "retrieved_at": astock_core::time::utc_now(),
                    "source": "JoinQuant",
                    "datasets": {
                        "qfq_daily": bounded_plain_dataset(daily.map(|value| value.data), 1_250, "JoinQuant"),
                        "valuation": bounded_plain_dataset(valuation, 20, "JoinQuant"),
                        "benchmark_components": bounded_plain_dataset(benchmark_components, 500, "JoinQuant"),
                        "macro_cpi": bounded_plain_dataset(macro_cpi, 24, "JoinQuant"),
                    },
                    "evidence_note": "聚宽为用户凭据授权的显式低频研究源；前复权日线不得与不复权序列直接逐点比较，各子集独立保留失败状态"
                }))
            }
            "research.optional_sources" => {
                let payload: OptionalSourcesPayload = decode_payload(&request.payload)?;
                let symbol = parse_live_symbol(&payload.symbol)?;
                let start = parse_research_date(&payload.start, "start")?;
                let end = parse_research_date(&payload.end, "end")?;
                if start > end || (end - start).num_days() > 1_830 {
                    return Err(ServiceError::new(
                        "invalid_research_window",
                        "可选来源研究区间必须按时间正序且不超过5年",
                        false,
                    ));
                }
                let tushare_available = self.market.tushare.available();
                let iwencai_available = self.market.iwencai.available();
                let tushare_tier = if tushare_available {
                    Some(self.market.tushare.detect_tier().await)
                } else {
                    None
                };
                let tushare_pro = tushare_tier.as_ref().is_some_and(|tier| {
                    matches!(
                        tier,
                        Ok(astock_market_data::providers::TushareTier::Pro2000)
                    )
                });
                let sec_cik = payload
                    .sec_cik
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                let sec =
                    astock_market_data::providers::SecEdgarProvider::new(self.market.http.clone());
                let (
                    ts_daily,
                    ts_basic,
                    ts_adjustment,
                    ts_dividends,
                    iw_events,
                    iw_sectors,
                    sec_filings,
                ) = tokio::join!(
                    async {
                        // The free 120-point tier still exposes raw daily bars.
                        // Do not accidentally hide useful evidence behind the
                        // Pro-only adjustment/valuation capability gate.
                        if tushare_available {
                            self.market.tushare.daily(&symbol, start, end).await
                        } else {
                            Err(astock_core::DataError::NoProvider(
                                "tushare（未配置访问凭证）",
                            ))
                        }
                    },
                    async {
                        if tushare_pro {
                            self.market.tushare.daily_basic(&symbol, start, end).await
                        } else {
                            Err(tushare_pro_unavailable(&tushare_tier))
                        }
                    },
                    async {
                        if tushare_pro {
                            self.market.tushare.adj_factor(&symbol, start, end).await
                        } else {
                            Err(tushare_pro_unavailable(&tushare_tier))
                        }
                    },
                    async {
                        if tushare_pro {
                            self.market.tushare.dividend(&symbol).await
                        } else {
                            Err(tushare_pro_unavailable(&tushare_tier))
                        }
                    },
                    async {
                        if iwencai_available {
                            self.market.iwencai.stock_events(symbol.code()).await
                        } else {
                            Err(astock_core::DataError::NoProvider(
                                "iwencai（未配置访问凭证）",
                            ))
                        }
                    },
                    async {
                        if iwencai_available {
                            self.market.iwencai.sector_membership(symbol.code()).await
                        } else {
                            Err(astock_core::DataError::NoProvider(
                                "iwencai（未配置访问凭证）",
                            ))
                        }
                    },
                    async {
                        if let Some(cik) = sec_cik {
                            sec.submissions(cik).await
                        } else {
                            Err(astock_core::DataError::NoProvider(
                                "sec_edgar（本任务未提供CIK）",
                            ))
                        }
                    },
                );
                Ok(json!({
                    "symbol": symbol.code(),
                    "start": start,
                    "end": end,
                    "retrieved_at": astock_core::time::utc_now(),
                    "configured": {
                        "tushare": tushare_available,
                        "iwencai": iwencai_available,
                        "sec_edgar": std::env::var("ASTOCK_SEC_USER_AGENT").is_ok(),
                    },
                    "capabilities": {
                        "tushare_raw_daily": tushare_available,
                        "tushare_pro": tushare_pro,
                        "iwencai_research": iwencai_available,
                        "sec_edgar_requested": sec_cik.is_some(),
                    },
                    "tushare_tier": match tushare_tier {
                        Some(Ok(tier)) => json!({"ok": true, "tier": format!("{tier:?}")}),
                        Some(Err(error)) => json!({"ok": false, "tier": null, "error": error.to_string()}),
                        None => json!({"ok": false, "tier": null, "error": "未配置Tushare访问凭证"}),
                    },
                    "datasets": {
                        "tushare_raw_daily": bounded_research_dataset(ts_daily, 1_250),
                        "tushare_daily_basic": bounded_plain_dataset(ts_basic, 1_250, "Tushare Pro"),
                        "tushare_adjustment_factors": bounded_plain_dataset(ts_adjustment, 1_250, "Tushare Pro"),
                        "tushare_dividends": bounded_plain_dataset(ts_dividends, 500, "Tushare Pro"),
                        "iwencai_stock_events": research_value_dataset(iw_events, "iWencai OpenAPI"),
                        "iwencai_sector_membership": research_value_dataset(iw_sectors, "iWencai OpenAPI"),
                        "sec_edgar_filings": bounded_plain_dataset(sec_filings, 500, "SEC EDGAR"),
                    },
                    "evidence_note": "可选来源各自保留未配置、权限不足、失败、来源和抓取时点；Tushare原始日线不得与复权序列直接逐点比较；问财只作补充发现层；SEC仅在用户提供CIK且配置真实Fair Access身份时访问"
                }))
            }
            "market.order_book" => {
                let payload: SymbolPayload = decode_payload(&request.payload)?;
                let symbol = parse_live_symbol(&payload.symbol)?;
                let raw = self
                    .market
                    .tdx
                    .order_book(&symbol)
                    .await
                    .map_err(upstream)?;
                let levels = |rows: &[(f64, f64); 5]| {
                    rows.iter()
                        .enumerate()
                        .map(|(index, (price, volume))| {
                            json!({
                                "level": index + 1,
                                "price": price,
                                "volume": volume
                            })
                        })
                        .collect::<Vec<_>>()
                };
                Ok(json!({
                    "symbol": raw.code,
                    "server_time": raw.servertime,
                    "current_volume": raw.cur_vol,
                    "inner_volume": raw.s_vol,
                    "outer_volume": raw.b_vol,
                    "bids": levels(&raw.bid),
                    "asks": levels(&raw.ask),
                    "source": "tdx",
                    "fetched_at": astock_core::time::utc_now(),
                    "transaction_detail_available": false,
                    "limitation": "当前内置 TDX 协议层支持五档快照与分时，不支持逐笔成交；未使用虚构逐笔数据"
                }))
            }
            "market.minute" => {
                let payload: SymbolPayload = decode_payload(&request.payload)?;
                let symbol = parse_live_symbol(&payload.symbol)?;
                let fetched = self.market.minute(&symbol).await.map_err(upstream)?;
                Ok(json!({
                    "points": fetched.data.points.iter().map(|point| json!({
                        "time": point.time.format("%H:%M").to_string(),
                        "price": point.price,
                        "avg_price": point.avg_price,
                        "volume": point.volume
                    })).collect::<Vec<_>>(),
                    "pre_close": fetched.data.pre_close,
                    "name": fetched.data.name
                }))
            }
            "market.fund_flow.daily" => {
                let payload: FundFlowPayload = decode_payload(&request.payload)?;
                let symbol = parse_live_symbol(&payload.symbol)?;
                let fetched = self
                    .market
                    .fund_flow_daily(&symbol, payload.days.clamp(1, 500))
                    .await
                    .map_err(upstream)?;
                Ok(Value::Array(flow_rows(&fetched.data)))
            }
            "market.fund_flow.realtime" => {
                let payload: SymbolPayload = decode_payload(&request.payload)?;
                let symbol = parse_live_symbol(&payload.symbol)?;
                let fetched = self
                    .market
                    .fund_flow_realtime(&symbol)
                    .await
                    .map_err(upstream)?;
                let points = fetched
                    .data
                    .iter()
                    .map(|point| {
                        json!({
                            "time": point.time.format("%H:%M").to_string(),
                            "main_net": point.main_net,
                            "super_large_net": point.super_large_net,
                            "large_net": point.large_net,
                            "medium_net": point.medium_net,
                            "small_net": point.small_net
                        })
                    })
                    .collect::<Vec<_>>();
                let summary = points.last().cloned().unwrap_or_else(|| {
                    json!({
                        "main_net": 0.0,
                        "super_large_net": 0.0,
                        "large_net": 0.0,
                        "medium_net": 0.0,
                        "small_net": 0.0
                    })
                });
                Ok(json!({"points": points, "summary": summary}))
            }
            "analysis.chanlun.minute" => {
                let payload: SymbolPayload = decode_payload(&request.payload)?;
                let symbol = parse_live_symbol(&payload.symbol)?;
                let fetched = self.market.minute(&symbol).await.map_err(upstream)?;
                if fetched.data.points.is_empty() {
                    return Err(ServiceError::new(
                        "empty",
                        format!("{symbol} 当前没有可用分时数据（可能尚未开盘或已休市）"),
                        true,
                    ));
                }
                let times = fetched
                    .data
                    .points
                    .iter()
                    .map(|point| point.time.format("%H:%M").to_string())
                    .collect::<Vec<_>>();
                let prices = fetched
                    .data
                    .points
                    .iter()
                    .map(|point| point.price)
                    .collect::<Vec<_>>();
                let volumes = fetched
                    .data
                    .points
                    .iter()
                    .map(|point| point.volume)
                    .collect::<Vec<_>>();
                let result =
                    astock_chanlun::minute::analyze_chanlun_minute(&times, &prices, &volumes);
                Ok(astock_chanlun::minute::signals_to_dict(&result))
            }
            "research.market_context" => {
                let today = astock_core::time::now_china().date_naive();
                let trade_date = latest_trading_day_on_or_before(&self.rules, today);
                let billboard_start = trade_date - chrono::Duration::days(7);
                let (
                    limit_up,
                    previous_limit_up,
                    strong,
                    sub_new,
                    broken,
                    limit_down,
                    billboard,
                    margin,
                    industry_boards,
                    concept_boards,
                ) = tokio::join!(
                    self.market.em_datacenter.zt_pool(trade_date),
                    self.market.em_datacenter.prev_zt_pool(trade_date),
                    self.market.em_datacenter.strong_pool(trade_date),
                    self.market.em_datacenter.sub_new_pool(trade_date),
                    self.market.em_datacenter.broken_pool(trade_date),
                    self.market.em_datacenter.dt_pool(trade_date),
                    self.market
                        .em_datacenter
                        .billboard_detail(billboard_start, trade_date, 4),
                    self.market.em_datacenter.margin_daily(1),
                    self.market
                        .em_datacenter
                        .board_list(astock_market_data::providers::BoardKind::Industry),
                    self.market
                        .em_datacenter
                        .board_list(astock_market_data::providers::BoardKind::Concept),
                );
                Ok(json!({
                    "trade_date": trade_date,
                    "retrieved_at": astock_core::time::utc_now(),
                    "datasets": {
                        "limit_up_pool": bounded_research_dataset(limit_up, 200),
                        "previous_limit_up_pool": bounded_research_dataset(previous_limit_up, 200),
                        "strong_pool": bounded_research_dataset(strong, 200),
                        "sub_new_pool": bounded_research_dataset(sub_new, 200),
                        "broken_limit_pool": bounded_research_dataset(broken, 200),
                        "limit_down_pool": bounded_research_dataset(limit_down, 200),
                        "billboard_7d": bounded_research_dataset(billboard, 160),
                        "margin_daily": bounded_research_dataset(margin, 40),
                        "industry_boards": bounded_research_dataset(industry_boards, 80),
                        "concept_boards": bounded_research_dataset(concept_boards, 80),
                    },
                    "evidence_note": "东方财富数据中心市场环境包；各子集独立保留失败状态，池为空不等于接口失败"
                }))
            }
            "research.agent_prepare_context" => {
                let payload: agent_context::PrepareContextPayload =
                    decode_payload(&request.payload)?;
                agent_context::prepare(self, payload).await
            }
            "research.agent_security_context" => {
                let payload: agent_context::SecurityContextPayload =
                    decode_payload(&request.payload)?;
                agent_context::security(self, payload).await
            }
            "research.agent_report_verify" => {
                let payload: report_verification::VerifyReportPayload =
                    decode_payload(&request.payload)?;
                report_verification::verify(payload)
            }
            "research.market_pool" => {
                let payload: MarketPoolPayload = decode_payload(&request.payload)?;
                let date = payload
                    .date
                    .as_deref()
                    .map(|value| parse_research_date(value, "date"))
                    .transpose()?
                    .unwrap_or_else(|| {
                        latest_trading_day_on_or_before(
                            &self.rules,
                            astock_core::time::now_china().date_naive(),
                        )
                    });
                match parse_market_pool_kind(&payload.pool)? {
                    MarketPoolKind::LimitUp => research_rows_payload(
                        self.market
                            .em_datacenter
                            .zt_pool(date)
                            .await
                            .map_err(upstream)?,
                    ),
                    MarketPoolKind::PreviousLimitUp => research_rows_payload(
                        self.market
                            .em_datacenter
                            .prev_zt_pool(date)
                            .await
                            .map_err(upstream)?,
                    ),
                    MarketPoolKind::Strong => research_rows_payload(
                        self.market
                            .em_datacenter
                            .strong_pool(date)
                            .await
                            .map_err(upstream)?,
                    ),
                    MarketPoolKind::SubNew => research_rows_payload(
                        self.market
                            .em_datacenter
                            .sub_new_pool(date)
                            .await
                            .map_err(upstream)?,
                    ),
                    MarketPoolKind::BrokenLimit => research_rows_payload(
                        self.market
                            .em_datacenter
                            .broken_pool(date)
                            .await
                            .map_err(upstream)?,
                    ),
                    MarketPoolKind::LimitDown => research_rows_payload(
                        self.market
                            .em_datacenter
                            .dt_pool(date)
                            .await
                            .map_err(upstream)?,
                    ),
                }
            }
            "research.board.constituents" => {
                let payload: BoardConstituentsPayload = decode_payload(&request.payload)?;
                let board_code = parse_board_code(&payload.board_code)?;
                research_rows_payload(
                    self.market
                        .em_datacenter
                        .board_cons(&board_code, 5)
                        .await
                        .map_err(upstream)?,
                )
            }
            "research.disclosures.list" => {
                let mut query: astock_disclosure::DisclosureQuery =
                    decode_payload(&request.payload)?;
                query.page = query.page.max(1);
                query.page_size = query.page_size.clamp(1, 500);
                if let Some(code) = query.security_code.as_deref() {
                    Symbol::new(code).map_err(|error| {
                        ServiceError::new("invalid_payload", error.to_string(), false)
                    })?;
                }
                let page = astock_disclosure::DisclosureStore::new(self.storage.clone())
                    .query(query)
                    .await
                    .map_err(disclosure_error)?;
                serde_json::to_value(page).map_err(serialize_error)
            }
            "research.disclosures.detail" => {
                let payload: DisclosureDetailPayload = decode_payload(&request.payload)?;
                let disclosure_id = payload.disclosure_id.trim();
                if disclosure_id.is_empty() || disclosure_id.len() > 128 {
                    return Err(ServiceError::new(
                        "invalid_payload",
                        "disclosure_id 必须是 1 至 128 个字符",
                        false,
                    ));
                }
                let detail = astock_disclosure::DisclosureStore::new(self.storage.clone())
                    .detail(disclosure_id)
                    .await
                    .map_err(disclosure_error)?;
                serde_json::to_value(detail).map_err(serialize_error)
            }
            "research.disclosures.providers" => {
                let providers = disclosure_sync::provider_health(self.storage.clone())
                    .await
                    .map_err(|error| ServiceError::new("disclosure", error, true))?;
                serde_json::to_value(providers).map_err(serialize_error)
            }
            "research.disclosures.sync.start" => {
                let payload: disclosure_sync::DisclosureSyncRequest =
                    decode_payload(&request.payload)?;
                let started = self
                    .disclosure_sync
                    .start(self.market.clone(), self.storage.clone(), payload)
                    .await
                    .map_err(|error| {
                        let code = if error.contains("仍在后台运行") {
                            "already_running"
                        } else {
                            "invalid_payload"
                        };
                        ServiceError::new(code, error, code == "already_running")
                    })?;
                serde_json::to_value(started).map_err(serialize_error)
            }
            "research.disclosures.sync.status" => {
                serde_json::to_value(self.disclosure_sync.status()).map_err(serialize_error)
            }
            "research.disclosures.sync.cancel" => {
                Ok(json!({ "cancelled": self.disclosure_sync.cancel() }))
            }
            "research.global_context" => {
                let world_bank =
                    astock_market_data::providers::WorldBankProvider::new(self.market.http.clone());
                let (gold_market, primary_gold_news, inflation, gdp_growth, current_account) = tokio::join!(
                    self.market.global_assets.gold_snapshot(120),
                    self.market.global_assets.primary_gold_news(20),
                    world_bank.latest(&["CN", "US"], "FP.CPI.TOTL.ZG", 5),
                    world_bank.latest(&["CN", "US"], "NY.GDP.MKTP.KD.ZG", 5),
                    world_bank.latest(&["CN", "US"], "BN.CAB.XOKA.GD.ZS", 5),
                );
                Ok(json!({
                    "retrieved_at": astock_core::time::utc_now(),
                    "datasets": {
                        "gold_market": research_value_dataset(gold_market, "EastMoney/Yahoo Finance"),
                        "gold_primary_news": research_value_dataset(primary_gold_news, "World Gold Council/Shanghai Gold Exchange"),
                        "world_bank_inflation": bounded_plain_dataset(inflation, 20, "World Bank"),
                        "world_bank_gdp_growth": bounded_plain_dataset(gdp_growth, 20, "World Bank"),
                        "world_bank_current_account": bounded_plain_dataset(current_account, 20, "World Bank"),
                    },
                    "evidence_note": "全球上下文只提供跨市场背景，不把年度宏观数据伪装成实时信号；每个数据集独立保留来源、失败和观测期"
                }))
            }
            "research.global.sync.start" => {
                let payload: global_sync::GlobalSyncRequest = decode_payload(&request.payload)?;
                self.global_sync
                    .start(self.market.clone(), self.storage.clone(), payload)
                    .await
                    .map(|started| json!(started))
                    .map_err(|message| ServiceError::new("global_sync", message, false))
            }
            "research.global.sync.status" => Ok(json!(self.global_sync.status())),
            "research.global.sync.cancel" => Ok(json!({ "cancelled": self.global_sync.cancel() })),
            "research.global.providers" => global_sync::provider_health(self.storage.clone())
                .await
                .map(|providers| json!(providers))
                .map_err(|message| ServiceError::new("global_intelligence", message, false)),
            "research.global.documents" => {
                let payload: astock_global_intelligence::GlobalDocumentQuery =
                    decode_payload(&request.payload)?;
                astock_global_intelligence::GlobalStore::new(self.storage.clone())
                    .query_documents(payload)
                    .await
                    .map(|page| json!(page))
                    .map_err(|error| {
                        ServiceError::new("global_intelligence", error.to_string(), false)
                    })
            }
            "research.global.chains" => Ok(json!(
                astock_global_intelligence::global_a_share_golden_chains()
            )),
            "research.global.transmission" => {
                let payload: GlobalTransmissionPayload = decode_payload(&request.payload)?;
                if payload.root_entity_id.trim().is_empty() {
                    return Err(ServiceError::new(
                        "invalid_entity",
                        "root_entity_id must not be empty",
                        false,
                    ));
                }
                astock_global_intelligence::GlobalStore::new(self.storage.clone())
                    .transmission_paths(
                        payload.root_entity_id.trim(),
                        payload.as_of.unwrap_or_else(now_secs),
                        payload.max_depth.unwrap_or(6),
                    )
                    .await
                    .map(|paths| json!(paths))
                    .map_err(|error| {
                        ServiceError::new("global_intelligence", error.to_string(), false)
                    })
            }
            "research.security_events" => {
                let payload: SymbolPayload = decode_payload(&request.payload)?;
                let symbol = parse_live_symbol(&payload.symbol)?;
                let code = symbol.code();
                let today = astock_core::time::now_china().date_naive();
                let event_start = today - chrono::Duration::days(365);
                let research_start = today - chrono::Duration::days(730);
                let future_end = today + chrono::Duration::days(365);
                let cninfo_column = match symbol.market() {
                    astock_core::Market::SH => "sse",
                    astock_core::Market::SZ => "szse",
                    astock_core::Market::BJ => "bse",
                };
                let cninfo_range = format!("{event_start}~{today}");
                let cninfo = astock_market_data::providers::CninfoDisclosureProvider::new(
                    self.market.http.clone(),
                );
                let (
                    billboard,
                    block_trade,
                    org_survey,
                    holder_num,
                    earnings_predict,
                    unlocks,
                    suspensions,
                    notices,
                    cninfo_disclosures,
                ) = tokio::join!(
                    self.market.em_datacenter.billboard_detail_for_symbol(
                        code,
                        event_start,
                        today,
                        2,
                    ),
                    self.market
                        .em_datacenter
                        .block_trade_for_symbol(code, event_start, today, 2,),
                    self.market
                        .em_datacenter
                        .org_survey_for_symbol(code, research_start, 2,),
                    self.market.em_datacenter.holder_num_for_symbol(code, 2),
                    self.market
                        .em_datacenter
                        .earnings_predict_for_symbol(code, research_start, 2,),
                    self.market.em_datacenter.lift_stage_for_symbol(
                        code,
                        event_start,
                        future_end,
                        2,
                    ),
                    self.market
                        .em_datacenter
                        .suspensions_for_symbol(code, today, 1),
                    self.market.em_datacenter.notices(
                        Some(code),
                        astock_market_data::providers::NoticeNode::All,
                        Some(event_start),
                        Some(today),
                        3,
                    ),
                    cninfo.query_recent_for_stock(code, cninfo_column, Some(&cninfo_range), 2),
                );
                Ok(json!({
                    "symbol": code,
                    "as_of": today,
                    "retrieved_at": astock_core::time::utc_now(),
                    "datasets": {
                        "billboard_1y": bounded_research_dataset(billboard, 80),
                        "block_trade_1y": bounded_research_dataset(block_trade, 120),
                        "org_survey_2y": bounded_research_dataset(org_survey, 120),
                        "holder_num": bounded_research_dataset(holder_num, 80),
                        "earnings_predict_2y": bounded_research_dataset(earnings_predict, 80),
                        "unlocks_prev_next_1y": bounded_research_dataset(unlocks, 120),
                        "suspension_today": bounded_research_dataset(suspensions, 20),
                        "announcements_1y": bounded_research_dataset(notices, 240),
                        "cninfo_disclosures_1y": bounded_cninfo_dataset(cninfo_disclosures, 50),
                    },
                    "evidence_note": "个股事件包均在上游按证券代码过滤，避免全市场分页后本地筛选造成漏数；停复牌为当日截面；东方财富公告镜像用于发现，CNInfo数据集提供法定披露索引和PDF原文链接"
                }))
            }
            "research.sources.list" => {
                let payload: LimitPayload = decode_payload(&request.payload)?;
                let rows = SourceVerifier::new(self.storage.clone())
                    .recent_documents(payload.limit.unwrap_or(100).clamp(1, 500))
                    .await
                    .map_err(source_verification)?;
                serde_json::to_value(rows).map_err(invalid)
            }
            "research.sources.get" => {
                let payload: SourceVersionPayload = decode_payload(&request.payload)?;
                let row = SourceVerifier::new(self.storage.clone())
                    .read_document(&payload.source_version_id)
                    .await
                    .map_err(source_verification)?;
                serde_json::to_value(row).map_err(invalid)
            }
            "research.sources.fetch" => {
                let payload: UrlPayload = decode_payload(&request.payload)?;
                let row = SourceVerifier::new(self.storage.clone())
                    .fetch_source_document(payload.url.trim())
                    .await
                    .map_err(source_verification)?;
                serde_json::to_value(row).map_err(invalid)
            }
            "research.sources.compare" => {
                let payload: SourceComparePayload = decode_payload(&request.payload)?;
                let rows = SourceVerifier::new(self.storage.clone())
                    .compare_source_evidence(&payload.source_version_ids)
                    .await
                    .map_err(source_verification)?;
                serde_json::to_value(rows).map_err(invalid)
            }
            "workspace.watchlist.list" => {
                let payload: GroupPayload = decode_payload(&request.payload)?;
                let items = self
                    .storage
                    .watchlist_list(&payload.group)
                    .await
                    .map_err(storage)?;
                Ok(Value::Array(
                    items
                        .into_iter()
                        .map(|item| {
                            json!({
                                "group_name": item.group_name,
                                "code": item.code,
                                "added_at": item.added_at,
                                "pinned": item.pinned
                            })
                        })
                        .collect(),
                ))
            }
            "workspace.watchlist.add" => {
                let payload: WatchlistPayload = decode_payload(&request.payload)?;
                let symbol = parse_live_symbol(&payload.symbol)?;
                self.storage
                    .watchlist_add(&payload.group, symbol.code())
                    .await
                    .map_err(storage)?;
                Ok(json!({"ok": true}))
            }
            "workspace.watchlist.remove" => {
                let payload: WatchlistPayload = decode_payload(&request.payload)?;
                let symbol = Symbol::new(payload.symbol).map_err(invalid)?;
                let removed = self
                    .storage
                    .watchlist_remove(&payload.group, symbol.code())
                    .await
                    .map_err(storage)?;
                Ok(json!({"removed": removed}))
            }
            "workspace.watchlist.pin" => {
                let payload: WatchlistPinPayload = decode_payload(&request.payload)?;
                let symbol = parse_live_symbol(&payload.symbol)?;
                let updated = self
                    .storage
                    .watchlist_set_pinned(&payload.group, symbol.code(), payload.pinned)
                    .await
                    .map_err(storage)?;
                Ok(json!({"updated": updated}))
            }
            "credentials.status" => {
                let minimax = astock_minimax::KeyStore::new()
                    .load_key()
                    .map_err(|error| {
                        ServiceError::new("credential_store", error.to_string(), false)
                    })?
                    .is_some();
                let joinquant_user = joinquant_username_store()
                    .load_key()
                    .map_err(credential_store)?
                    .is_some();
                let joinquant_password = joinquant_password_store()
                    .load_key()
                    .map_err(credential_store)?
                    .is_some();
                let optional = credentials::status(self)?;
                Ok(json!({"providers": {
                    "minimax": minimax,
                    "joinquant": joinquant_user && joinquant_password && self.market.joinquant.available(),
                    "optional": optional
                }}))
            }
            "credentials.provider.set" => {
                let payload: credentials::ProviderCredentialPayload =
                    decode_payload(&request.payload)?;
                credentials::set(payload)
            }
            "credentials.provider.delete" => {
                let payload: credentials::ProviderIdPayload = decode_payload(&request.payload)?;
                credentials::delete(payload)
            }
            "credentials.minimax.set" => {
                let payload: MinimaxCredentialPayload = decode_payload(&request.payload)?;
                let key = payload.key.trim();
                if key.len() < 8 {
                    return Err(ServiceError::new(
                        "invalid_credential",
                        "MiniMax API Key 格式无效",
                        false,
                    ));
                }
                astock_minimax::KeyStore::new()
                    .store_key(&astock_minimax::SecretKey::new(key))
                    .map_err(|error| {
                        ServiceError::new("credential_store", error.to_string(), false)
                    })?;
                let verified = astock_minimax::KeyStore::new()
                    .load_key()
                    .map_err(credential_store)?
                    .is_some_and(|stored| stored.expose() == key);
                if !verified {
                    let _ = astock_minimax::KeyStore::new().delete_key();
                    return Err(ServiceError::new(
                        "credential_verification_failed",
                        "MiniMax Credential Manager read-back failed; the key was not retained",
                        false,
                    ));
                }
                Ok(json!({"stored": true}))
            }
            "credentials.minimax.delete" => {
                astock_minimax::KeyStore::new()
                    .delete_key()
                    .map_err(|error| {
                        ServiceError::new("credential_store", error.to_string(), false)
                    })?;
                Ok(json!({"deleted": true}))
            }
            "credentials.minimax.quota" => {
                let key = astock_minimax::KeyStore::new()
                    .load_key()
                    .map_err(credential_store)?
                    .ok_or_else(|| {
                        ServiceError::new("credential_missing", "尚未配置 MiniMax API Key", false)
                    })?;
                let quota = astock_minimax::MinimaxClient::new(key)
                    .quota()
                    .await
                    .map_err(|error| ServiceError::new("minimax_quota", error.to_string(), true))?;
                let fetched_at_ms = quota
                    .fetched_at
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                Ok(json!({
                    "fetched_at_ms": fetched_at_ms,
                    "models": quota.models.into_iter().map(|model| json!({
                        "model_name": model.model_name,
                        "interval_total": model.current_interval_total_count,
                        "interval_used": model.current_interval_usage_count,
                        "interval_remaining_percent": model.current_interval_remaining_percent,
                        "interval_reset_at_ms": model.end_time,
                        "weekly_total": model.current_weekly_total_count,
                        "weekly_used": model.current_weekly_usage_count,
                        "weekly_remaining_percent": model.current_weekly_remaining_percent,
                        "weekly_reset_at_ms": model.weekly_end_time
                    })).collect::<Vec<_>>()
                }))
            }
            "credentials.joinquant.set" => {
                let payload: JoinQuantCredentialPayload = decode_payload(&request.payload)?;
                let username = payload.username.trim();
                if username.is_empty()
                    || payload.password.len() < 6
                    || username.chars().any(char::is_control)
                    || payload.password.chars().any(char::is_control)
                {
                    return Err(ServiceError::new(
                        "invalid_credential",
                        "聚宽用户名或密码格式无效",
                        false,
                    ));
                }
                joinquant_username_store()
                    .store_key(&astock_minimax::SecretKey::new(username))
                    .map_err(credential_store)?;
                if let Err(error) = joinquant_password_store()
                    .store_key(&astock_minimax::SecretKey::new(&payload.password))
                {
                    let _ = joinquant_username_store().delete_key();
                    return Err(credential_store(error));
                }
                let verified = joinquant_username_store()
                    .load_key()
                    .map_err(credential_store)?
                    .is_some_and(|stored| stored.expose() == username)
                    && joinquant_password_store()
                        .load_key()
                        .map_err(credential_store)?
                        .is_some_and(|stored| stored.expose() == payload.password);
                if !verified {
                    let _ = joinquant_username_store().delete_key();
                    let _ = joinquant_password_store().delete_key();
                    return Err(ServiceError::new(
                        "credential_verification_failed",
                        "JoinQuant Credential Manager read-back failed; credentials were not retained",
                        false,
                    ));
                }
                if let Err(error) = self
                    .market
                    .joinquant
                    .configure(username.to_string(), payload.password)
                {
                    let _ = joinquant_username_store().delete_key();
                    let _ = joinquant_password_store().delete_key();
                    self.market.joinquant.clear_credentials();
                    return Err(upstream(error));
                }
                Ok(json!({"stored": true, "active": true}))
            }
            "credentials.joinquant.delete" => {
                joinquant_username_store()
                    .delete_key()
                    .map_err(credential_store)?;
                joinquant_password_store()
                    .delete_key()
                    .map_err(credential_store)?;
                self.market.joinquant.clear_credentials();
                Ok(json!({"deleted": true}))
            }
            "storage.cache.stats" => {
                let stats = self.storage.cache_stats().await.map_err(storage)?;
                Ok(cache_stats_json(stats, self.storage.disk_free_bytes()))
            }
            "storage.cache.cleanup" => {
                let payload: CacheCleanupPayload = decode_payload(&request.payload)?;
                let before = self.storage.cache_stats().await.map_err(storage)?;
                let target_total_bytes =
                    payload.target_mb.min(1_048_576).saturating_mul(1024 * 1024);
                let report = self
                    .storage
                    .cleanup(CleanupPolicy { target_total_bytes })
                    .await
                    .map_err(storage)?;
                let after = self.storage.cache_stats().await.map_err(storage)?;
                Ok(json!({
                    "before": cache_stats_json(before, self.storage.disk_free_bytes()),
                    "after": cache_stats_json(after, self.storage.disk_free_bytes()),
                    "report": {
                        "tool_cache_rows_deleted": report.tool_cache_rows_deleted,
                        "parquet_files_deleted": report.parquet_files_deleted,
                        "bytes_freed": report.bytes_freed
                    }
                }))
            }
            "storage.data_root.migrate" => {
                let payload: DataRootMigrationPayload = decode_payload(&request.payload)?;
                data_root::migrate(&self.storage, &payload.destination)
                    .await
                    .map(|outcome| json!(outcome))
                    .map_err(|message| {
                        ServiceError::new("data_root_migration_failed", message, false)
                    })
            }
            "storage.data_root.rollback" => data_root::rollback(&self.storage)
                .await
                .map(|outcome| json!(outcome))
                .map_err(|message| ServiceError::new("data_root_rollback_failed", message, false)),
            "quant.scan.start" => {
                let snapshot = self
                    .scan
                    .start(self.market.clone(), self.rules.clone())
                    .await
                    .map_err(|message| ServiceError::new("already_running", message, false))?;
                Ok(json!({ "started": true, "snapshot": snapshot }))
            }
            "quant.scan.status" => Ok(json!(self.scan.status().await)),
            "quant.scan.cancel" => Ok(json!({ "cancelled": self.scan.cancel().await })),
            "settings.agent_models.get" => Ok(json!(settings::get(&self.storage).await)),
            "settings.agent_models.set" => {
                let payload: settings::AgentModelRouting = decode_payload(&request.payload)?;
                Ok(json!(settings::set(&self.storage, payload).await?))
            }
            "agent.task.create" => {
                let payload: event_store::CreateTask = decode_payload(&request.payload)?;
                let inserted = event_store::create_task(&self.storage, payload)
                    .await
                    .map_err(event_store_error)?;
                Ok(json!({"inserted": inserted}))
            }
            "agent.event.append" => {
                let payload: event_store::AppendEvent = decode_payload(&request.payload)?;
                let inserted = event_store::append_event(&self.storage, payload)
                    .await
                    .map_err(event_store_error)?;
                Ok(json!({"inserted": inserted}))
            }
            "agent.checkpoint.put" => {
                let payload: event_store::PutCheckpoint = decode_payload(&request.payload)?;
                event_store::put_checkpoint(&self.storage, payload)
                    .await
                    .map_err(event_store_error)?;
                Ok(json!({"ok": true}))
            }
            "agent.effect.begin" => {
                let payload: event_store::BeginEffect = decode_payload(&request.payload)?;
                let inserted = event_store::begin_effect(&self.storage, payload)
                    .await
                    .map_err(event_store_error)?;
                Ok(json!({"inserted": inserted}))
            }
            "agent.effect.complete" => {
                let payload: event_store::CompleteEffect = decode_payload(&request.payload)?;
                event_store::complete_effect(&self.storage, payload)
                    .await
                    .map_err(event_store_error)?;
                Ok(json!({"ok": true}))
            }
            "agent.effect.list" => {
                let payload: TaskIdPayload = decode_payload(&request.payload)?;
                let effects = event_store::list_effects(&self.storage, payload.task_id)
                    .await
                    .map_err(event_store_error)?;
                Ok(json!({"items": effects}))
            }
            "agent.task.load" => {
                let payload: TaskIdPayload = decode_payload(&request.payload)?;
                let loaded = event_store::load_task(&self.storage, payload.task_id)
                    .await
                    .map_err(event_store_error)?;
                serde_json::to_value(loaded).map_err(invalid)
            }
            "agent.task.list" => {
                let payload: TaskListPayload = decode_payload(&request.payload)?;
                let tasks = event_store::list_tasks(
                    &self.storage,
                    payload.limit.unwrap_or(astock_protocol::MAX_PAGE_SIZE),
                )
                .await
                .map_err(event_store_error)?;
                Ok(json!({"items": tasks}))
            }
            "agent.conversation.save" => {
                let payload: event_store::SaveConversation = decode_payload(&request.payload)?;
                let conversation = event_store::save_conversation(&self.storage, payload)
                    .await
                    .map_err(event_store_error)?;
                serde_json::to_value(conversation).map_err(invalid)
            }
            "agent.conversation.load" => {
                let payload: event_store::ConversationId = decode_payload(&request.payload)?;
                let conversation =
                    event_store::load_conversation(&self.storage, payload.conversation_id)
                        .await
                        .map_err(event_store_error)?;
                serde_json::to_value(conversation).map_err(invalid)
            }
            "agent.conversation.list" => {
                let payload: event_store::ConversationList = decode_payload(&request.payload)?;
                let conversations = event_store::list_conversations(
                    &self.storage,
                    payload.limit.unwrap_or(astock_protocol::MAX_PAGE_SIZE),
                    payload.query,
                )
                .await
                .map_err(event_store_error)?;
                Ok(json!({"items": conversations}))
            }
            "agent.conversation.rename" => {
                let payload: event_store::RenameConversation = decode_payload(&request.payload)?;
                let conversation = event_store::rename_conversation(&self.storage, payload)
                    .await
                    .map_err(event_store_error)?;
                serde_json::to_value(conversation).map_err(invalid)
            }
            "agent.conversation.branch" => {
                let payload: event_store::BranchConversation = decode_payload(&request.payload)?;
                let conversation = event_store::branch_conversation(&self.storage, payload)
                    .await
                    .map_err(event_store_error)?;
                serde_json::to_value(conversation).map_err(invalid)
            }
            "agent.conversation.delete" => {
                let payload: event_store::ConversationId = decode_payload(&request.payload)?;
                let deleted =
                    event_store::delete_conversation(&self.storage, payload.conversation_id)
                        .await
                        .map_err(event_store_error)?;
                Ok(json!({"deleted": deleted}))
            }
            _ => Err(ServiceError::new(
                "unknown_request_kind",
                format!("unsupported Engine request kind: {}", request.kind),
                false,
            )),
        }
    }

    /// Invoke an existing deterministic Engine service while assembling one
    /// bounded Agent research snapshot. Boxing keeps the recursive dispatcher
    /// future finite; the two aggregate routes never call themselves.
    fn dispatch_internal<'a>(
        &'a self,
        kind: &'a str,
        payload: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, ServiceError>> + Send + 'a>> {
        let request = RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: format!("engine-internal-{kind}"),
            kind: kind.to_owned(),
            payload,
            deadline_ms: None,
            cancellation_id: None,
        };
        Box::pin(async move { self.dispatch_inner(&request).await })
    }
}

#[derive(Debug)]
struct ServiceError {
    code: String,
    message: String,
    retryable: bool,
}

impl ServiceError {
    fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchPayload {
    keyword: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SymbolPayload {
    symbol: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EarningsDriverShockPayload {
    symbol: String,
    shocks: Vec<DriverShock>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EarningsDriverSnapshotPayload {
    snapshot_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KlinePayload {
    symbol: String,
    period: String,
    adjust: String,
    count: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexKlinePayload {
    secid: String,
    count: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SharesPagePayload {
    #[serde(default)]
    cursor: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    snapshot_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FundFlowPayload {
    symbol: String,
    days: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResearchCandidatesPayload {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    max_lot_cost: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResearchNewsPayload {
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    keyword: Option<String>,
    #[serde(default)]
    sources: Option<Vec<String>>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinQuantResearchPayload {
    symbol: String,
    benchmark: String,
    start: String,
    end: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OptionalSourcesPayload {
    symbol: String,
    start: String,
    end: String,
    #[serde(default)]
    sec_cik: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LimitPayload {
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceVersionPayload {
    source_version_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UrlPayload {
    url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceComparePayload {
    source_version_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupPayload {
    group: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchlistPayload {
    symbol: String,
    group: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchlistPinPayload {
    symbol: String,
    group: String,
    pinned: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MinimaxCredentialPayload {
    key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinQuantCredentialPayload {
    username: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheCleanupPayload {
    target_mb: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DataRootMigrationPayload {
    destination: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobalTransmissionPayload {
    root_entity_id: String,
    #[serde(default)]
    as_of: Option<i64>,
    #[serde(default)]
    max_depth: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MarketPoolPayload {
    pool: String,
    #[serde(default)]
    date: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BoardConstituentsPayload {
    board_code: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisclosureDetailPayload {
    disclosure_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewsProviderTogglePayload {
    provider_id: String,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewsDocumentPayload {
    document_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewsObservationPayload {
    #[serde(default)]
    provider_id: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewsUserStatePayload {
    document_id: String,
    action: String,
    value: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewsClusterPayload {
    cluster_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewsClusterMergePayload {
    from_cluster_id: String,
    to_cluster_id: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewsClusterSplitPayload {
    revision_id: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewsReviewResolvePayload {
    task_id: String,
    conclusion_key: String,
    triggering_revision: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntityLinksPayload {
    revision_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntityReviewResolvePayload {
    link_id: String,
    #[serde(default)]
    entity_id: Option<String>,
    accept: bool,
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventJobPayload {
    job_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RelationJobPayload {
    job_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RelationReviewPagePayload {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    document_kind: Option<astock_relation_extraction::DocumentKind>,
    #[serde(default)]
    min_confidence_bps: Option<u16>,
    page: usize,
    page_size: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RelationRetractPayload {
    candidate_id: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphSubgraphPayload {
    symbol_or_node: String,
    #[serde(default)]
    hops: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphAsOfPayload {
    business_time: i64,
    knowledge_time: i64,
    #[serde(default)]
    symbol_or_node: Option<String>,
    #[serde(default)]
    hops: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphIdentityPayload {
    identity_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphSnapshotPayload {
    snapshot_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphSnapshotDiffPayload {
    left_business_time: i64,
    left_knowledge_time: i64,
    right_business_time: i64,
    right_knowledge_time: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphShockPayload {
    subject: String,
    direction: String,
    #[serde(default)]
    magnitude_pct: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MarketRelationshipPayload {
    symbols: Vec<String>,
    #[serde(default)]
    window_days: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuantJobStatusPayload {
    #[serde(default)]
    job_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuantJobCancelPayload {
    job_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuantSnapshotPayload {
    snapshot_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuantSnapshotListPayload {
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskIdPayload {
    task_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskListPayload {
    #[serde(default)]
    limit: Option<usize>,
}

fn decode_payload<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, ServiceError> {
    serde_json::from_value(value.clone()).map_err(|error| {
        ServiceError::new(
            "invalid_payload",
            format!("invalid request payload: {error}"),
            false,
        )
    })
}

fn joinquant_username_store() -> astock_minimax::KeyStore {
    astock_minimax::KeyStore::with_service("astock-terminal", "joinquant-username")
}

fn joinquant_password_store() -> astock_minimax::KeyStore {
    astock_minimax::KeyStore::with_service("astock-terminal", "joinquant-password")
}

fn credential_store(error: astock_minimax::MinimaxError) -> ServiceError {
    ServiceError::new("credential_store", error.to_string(), false)
}

fn cache_stats_json(stats: astock_storage::CacheStats, disk_free_bytes: Option<u64>) -> Value {
    json!({
        "kline_parquet_bytes": stats.kline_parquet_bytes,
        "kline_parquet_files": stats.kline_parquet_files,
        "sqlite_bytes": stats.sqlite_bytes,
        "tool_cache_rows": stats.tool_cache_rows,
        "tool_cache_bytes": stats.tool_cache_bytes,
        "chat_bytes": stats.chat_bytes,
        "total_bytes": stats.total_bytes(),
        "disk_free_bytes": disk_free_bytes
    })
}

fn parse_research_date(value: &str, field: &str) -> Result<NaiveDate, ServiceError> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").map_err(|_| {
        ServiceError::new(
            "invalid_research_date",
            format!("{field} 必须是 YYYY-MM-DD 日期"),
            false,
        )
    })
}

fn parse_period(value: &str) -> Result<KlinePeriod, ServiceError> {
    match value {
        "day" => Ok(KlinePeriod::Day),
        "week" => Ok(KlinePeriod::Week),
        "month" => Ok(KlinePeriod::Month),
        _ => Err(ServiceError::new(
            "invalid_period",
            format!("unsupported period: {value}"),
            false,
        )),
    }
}

fn parse_adjust(value: &str) -> Result<Adjust, ServiceError> {
    match value {
        "qfq" => Ok(Adjust::Qfq),
        "hfq" => Ok(Adjust::Hfq),
        "none" => Ok(Adjust::None),
        _ => Err(ServiceError::new(
            "invalid_adjust",
            format!("unsupported adjust: {value}"),
            false,
        )),
    }
}

fn parse_live_symbol(value: &str) -> Result<Symbol, ServiceError> {
    let symbol = Symbol::new(value).map_err(invalid)?;
    if symbol.is_supported_market_instrument() {
        Ok(symbol)
    } else {
        Err(ServiceError::new(
            "unsupported_live_symbol",
            format!(
                "证券代码 {} 不是当前可查询的沪深京上市股票或场内基金；北交所请使用 920xxx 新代码，历史/场外代码只保留在研究档案中",
                symbol.code()
            ),
            false,
        ))
    }
}

fn invalid(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new("invalid_argument", error.to_string(), false)
}

fn upstream(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new("upstream", error.to_string(), true)
}

fn storage(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new("storage", error.to_string(), true)
}

fn disclosure_error(error: astock_disclosure::Error) -> ServiceError {
    let retryable = !matches!(error, astock_disclosure::Error::NotFound(_));
    ServiceError::new("disclosure", error.to_string(), retryable)
}

fn serialize_error(error: serde_json::Error) -> ServiceError {
    ServiceError::new("serialization", error.to_string(), false)
}

fn news_intelligence_error(error: astock_news_intelligence::Error) -> ServiceError {
    ServiceError::new("news_clustering", error.to_string(), false)
}

fn entity_linking_error(error: astock_entity_linking::Error) -> ServiceError {
    ServiceError::new("entity_linking", error.to_string(), false)
}

fn relation_extraction_error(error: astock_relation_extraction::Error) -> ServiceError {
    ServiceError::new("relation_extraction", error.to_string(), false)
}

fn graph_error(error: astock_graph::Error) -> ServiceError {
    ServiceError::new("graph", error.to_string(), false)
}

fn graph_service_error(error: String) -> ServiceError {
    ServiceError::new("graph", error, false)
}

fn quant_research_error(error: String) -> ServiceError {
    ServiceError::new("quant_research", error, false)
}

fn backtest_error(error: String) -> ServiceError {
    ServiceError::new("backtest", error, false)
}

fn market_regime_error(error: String) -> ServiceError {
    ServiceError::new("market_regime", error, true)
}

async fn persist_driver_tree(
    storage_handle: &Storage,
    tree: &EarningsDriverTree,
) -> Result<(), ServiceError> {
    let tree = tree.clone();
    storage_handle
        .run(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO earnings_driver_snapshots
                 (snapshot_id,parameter_snapshot_id,symbol,model_version,report_period,
                  knowledge_time,tree_json,created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    tree.snapshot_id,
                    tree.parameter_snapshot_id,
                    tree.symbol,
                    tree.model_version,
                    tree.report_period,
                    tree.knowledge_time,
                    serde_json::to_string(&tree)?,
                    now_secs(),
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(storage)
}

async fn persist_driver_shock_bridge(
    storage_handle: &Storage,
    bridge: &ShockBridge,
) -> Result<(), ServiceError> {
    let bridge = bridge.clone();
    storage_handle
        .run(move |conn| {
            let evidence_ids = bridge
                .shocks
                .iter()
                .filter_map(|shock| shock.evidence_version_id.as_deref())
                .collect::<Vec<_>>();
            conn.execute(
                "INSERT OR IGNORE INTO earnings_driver_shock_bridges
                 (bridge_id,base_snapshot_id,evidence_version_ids_json,shocks_json,bridge_json,created_at)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    bridge.shocked_snapshot_id,
                    bridge.base_snapshot_id,
                    serde_json::to_string(&evidence_ids)?,
                    serde_json::to_string(&bridge.shocks)?,
                    serde_json::to_string(&bridge)?,
                    now_secs(),
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(storage)
}

async fn load_driver_tree(
    storage_handle: &Storage,
    snapshot_id: String,
) -> Result<EarningsDriverTree, ServiceError> {
    let requested_id = snapshot_id.clone();
    let json = storage_handle
        .run(move |conn| {
            conn.query_row(
                "SELECT tree_json FROM earnings_driver_snapshots WHERE snapshot_id=?1",
                [snapshot_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(astock_storage::Error::from)
        })
        .await
        .map_err(storage)?
        .ok_or_else(|| {
            ServiceError::new(
                "not_found",
                format!("未找到盈利驱动快照 {requested_id}"),
                false,
            )
        })?;
    serde_json::from_str(&json).map_err(|error| {
        ServiceError::new(
            "storage_corrupt",
            format!("盈利驱动快照损坏：{error}"),
            false,
        )
    })
}

fn event_store_error(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new("agent_event_store", error.to_string(), false)
}

fn source_verification(error: impl std::fmt::Display) -> ServiceError {
    ServiceError::new("source_verification", error.to_string(), false)
}

fn flow_rows(points: &[astock_core::FundFlowPoint]) -> Vec<Value> {
    points
        .iter()
        .map(|point| {
            json!({
                "date": point.time.date().to_string(),
                "main_net": point.main_net,
                "super_large_net": point.super_large_net,
                "large_net": point.large_net,
                "medium_net": point.medium_net,
                "small_net": point.small_net,
                "main_pct": point.main_pct
            })
        })
        .collect()
}

fn latest_trading_day_on_or_before(rules: &RuleSet, mut date: NaiveDate) -> NaiveDate {
    while !rules.is_trading_day(date) {
        date -= chrono::Duration::days(1);
    }
    date
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarketPoolKind {
    LimitUp,
    PreviousLimitUp,
    Strong,
    SubNew,
    BrokenLimit,
    LimitDown,
}

fn parse_market_pool_kind(raw: &str) -> Result<MarketPoolKind, ServiceError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "zt" => Ok(MarketPoolKind::LimitUp),
        "prev" => Ok(MarketPoolKind::PreviousLimitUp),
        "strong" => Ok(MarketPoolKind::Strong),
        "sub_new" => Ok(MarketPoolKind::SubNew),
        "broken" => Ok(MarketPoolKind::BrokenLimit),
        "dt" => Ok(MarketPoolKind::LimitDown),
        other => Err(ServiceError::new(
            "invalid_payload",
            format!("pool 只能是 zt/prev/strong/sub_new/broken/dt，收到 `{other}`"),
            false,
        )),
    }
}

fn parse_board_code(raw: &str) -> Result<String, ServiceError> {
    let code = raw.trim().to_ascii_uppercase();
    let valid = code.len() == 6
        && code.starts_with("BK")
        && code[2..].bytes().all(|byte| byte.is_ascii_digit());
    if valid {
        Ok(code)
    } else {
        Err(ServiceError::new(
            "invalid_payload",
            format!("board_code 必须是 BK + 4 位数字（如 BK0447），收到 `{raw}`"),
            false,
        ))
    }
}

fn validate_news_identifier<'a>(raw: &'a str, field: &str) -> Result<&'a str, ServiceError> {
    let value = raw.trim();
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        Err(ServiceError::new(
            "invalid_payload",
            format!("{field} 必须是 1 至 256 个无控制字符的文本"),
            false,
        ))
    } else {
        Ok(value)
    }
}

fn validate_review_reason(raw: &str) -> Result<&str, ServiceError> {
    let value = raw.trim();
    if value.is_empty() || value.chars().count() > 2_000 || value.chars().any(char::is_control) {
        Err(ServiceError::new(
            "invalid_payload",
            "人工复核理由必须是 1 至 2000 个无控制字符的文本",
            false,
        ))
    } else {
        Ok(value)
    }
}

/// Preserve the legacy datacenter row contract while routing through the
/// coarse research service. Provenance belongs to every response so an empty
/// row set can never be confused with a provider failure.
fn research_rows_payload<T: serde::Serialize>(
    fetched: astock_core::Fetched<Vec<T>>,
) -> Result<Value, ServiceError> {
    let count = fetched.data.len();
    Ok(json!({
        "rows": fetched.data,
        "count": count,
        "source": fetched.source.to_string(),
        "fetched_at": fetched.fetched_at.to_rfc3339(),
    }))
}

fn tushare_pro_unavailable(
    tier: &Option<Result<astock_market_data::providers::TushareTier, astock_core::DataError>>,
) -> astock_core::DataError {
    use astock_market_data::providers::TushareTier;

    match tier {
        None => astock_core::DataError::NoProvider("tushare（未配置访问凭证）"),
        Some(Ok(TushareTier::Free120)) => {
            astock_core::DataError::NoProvider("tushare Pro（积分不足2000）")
        }
        Some(Ok(TushareTier::Unknown)) => {
            astock_core::DataError::NoProvider("tushare Pro（权限尚未确认）")
        }
        Some(Ok(TushareTier::Pro2000)) => {
            astock_core::DataError::NoProvider("tushare Pro（能力状态不一致）")
        }
        Some(Err(_)) => astock_core::DataError::NoProvider("tushare Pro（权限探测失败）"),
    }
}

fn bounded_research_dataset<T: serde::Serialize>(
    result: Result<astock_core::Fetched<Vec<T>>, astock_core::DataError>,
    limit: usize,
) -> Value {
    match result {
        Ok(fetched) => {
            let total_rows = fetched.data.len();
            let rows = fetched.data.into_iter().take(limit).collect::<Vec<_>>();
            json!({
                "ok": true,
                "rows": rows,
                "total_rows": total_rows,
                "truncated": total_rows > limit,
                "source": fetched.source,
                "fetched_at": fetched.fetched_at,
            })
        }
        Err(error) => json!({
            "ok": false,
            "rows": [],
            "total_rows": 0,
            "truncated": false,
            "error": error.to_string(),
        }),
    }
}

fn bounded_cninfo_dataset(
    result: Result<astock_market_data::providers::CninfoPage, astock_core::DataError>,
    limit: usize,
) -> Value {
    match result {
        Ok(page) => {
            let returned_rows = page.rows.len();
            let rows = page.rows.into_iter().take(limit).collect::<Vec<_>>();
            json!({
                "ok": true,
                "rows": rows,
                "total_rows": page.total,
                "returned_rows": returned_rows,
                "total_pages": page.total_pages,
                "page": page.page,
                "truncated": page.total > limit as u64,
                "source": "CNInfo",
                "fetched_at": astock_core::time::utc_now(),
                "trust_tier": "statutory_disclosure_index",
            })
        }
        Err(error) => json!({
            "ok": false,
            "rows": [],
            "total_rows": 0,
            "truncated": false,
            "source": "CNInfo",
            "error": error.to_string(),
        }),
    }
}

fn research_value_dataset<T: serde::Serialize>(
    result: Result<T, astock_core::DataError>,
    source: &str,
) -> Value {
    match result {
        Ok(data) => json!({
            "ok": true,
            "data": data,
            "source": source,
            "fetched_at": astock_core::time::utc_now(),
        }),
        Err(error) => json!({
            "ok": false,
            "data": null,
            "source": source,
            "error": error.to_string(),
        }),
    }
}

fn bounded_plain_dataset<T: serde::Serialize>(
    result: Result<Vec<T>, astock_core::DataError>,
    limit: usize,
    source: &str,
) -> Value {
    match result {
        Ok(data) => {
            let total_rows = data.len();
            let rows = data.into_iter().take(limit).collect::<Vec<_>>();
            json!({
                "ok": true,
                "rows": rows,
                "total_rows": total_rows,
                "truncated": total_rows > limit,
                "source": source,
                "fetched_at": astock_core::time::utc_now(),
            })
        }
        Err(error) => json!({
            "ok": false,
            "rows": [],
            "total_rows": 0,
            "truncated": false,
            "source": source,
            "error": error.to_string(),
        }),
    }
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let text = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{text}…")
    } else {
        text
    }
}

fn normalized_news_item(item: astock_market_data::FinanceNewsItem) -> Value {
    json!({
        "document_id": item.id,
        "revision_id": item.document_revision_id,
        "source_id": item.source_id,
        "source_name": item.source_name,
        "provider_id": item.provider_id,
        "title": item.title,
        "summary": truncate_text(&item.summary, 360),
        "url": item.url,
        "published_at": item.published_at,
        "important": item.important,
        "trust_tier": item.trust_tier,
        "trust_tier_name": item.trust_tier_name,
        "independent_source_count": item.independent_source_count,
        "old_republication": item.old_republication,
        "entity_links": item.entity_links,
    })
}

fn tail_value<T: serde::Serialize>(rows: &[T], limit: usize) -> Value {
    let start = rows.len().saturating_sub(limit);
    serde_json::to_value(&rows[start..]).unwrap_or(Value::Array(Vec::new()))
}

/// Bounded point-in-time financial evidence for Agent prompts. The original
/// database remains complete; only the IPC view is capped so one security can
/// never consume the 8 MiB frame budget. Section failures are explicit and
/// missing numeric fields stay JSON null rather than becoming zero.
fn fundamental_research_payload(
    symbol: &Symbol,
    outcome: astock_fundamental::BundleOutcome,
) -> Value {
    let missing = outcome
        .failures
        .iter()
        .filter_map(|failure| failure.split(':').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let bundle = outcome.bundle;
    json!({
        "symbol": symbol.code(),
        "profile": bundle.profile,
        "income": tail_value(&bundle.income, 12),
        "balance": tail_value(&bundle.balance, 12),
        "cashflow": tail_value(&bundle.cashflow, 12),
        "indicators": tail_value(&bundle.indicators, 12),
        "dividends": tail_value(&bundle.dividends, 12),
        "valuation_snapshot": bundle.snapshot,
        "valuation_history": tail_value(&bundle.valuation_history, 750),
        "missing_sections": missing,
        "section_errors": outcome.failures,
        "source": "eastmoney_f10",
        "point_in_time_note": "财报仅可在 announced 公告日期之后用于历史判断；缺失值保持 null"
    })
}

fn numeric_check(field: &str, left: f64, right: f64, absolute: f64, relative: f64) -> Value {
    let difference = (left - right).abs();
    let tolerance = absolute.max(left.abs().max(right.abs()) * relative);
    json!({
        "field": field,
        "tdx": left,
        "eastmoney": right,
        "absolute_difference": difference,
        "tolerance": tolerance,
        "consistent": difference <= tolerance,
    })
}

fn reconcile_market_sources(
    symbol: &str,
    tdx_quote: Result<astock_core::Fetched<astock_core::Quote>, astock_core::DataError>,
    eastmoney_quote: Result<astock_core::Fetched<astock_core::Quote>, astock_core::DataError>,
    tdx_bars: Result<astock_core::Fetched<Vec<astock_core::Bar>>, astock_core::DataError>,
    eastmoney_bars: Result<astock_core::Fetched<Vec<astock_core::Bar>>, astock_core::DataError>,
    tencent_bars: Result<astock_core::Fetched<Vec<astock_core::Bar>>, astock_core::DataError>,
    sina_bars: Result<astock_core::Fetched<Vec<astock_core::Bar>>, astock_core::DataError>,
) -> Value {
    let mut quote_checks = Vec::new();
    if let (Ok(left), Ok(right)) = (&tdx_quote, &eastmoney_quote) {
        for (field, lv, rv, absolute, relative) in [
            ("price", left.data.price, right.data.price, 0.01, 0.002),
            ("open", left.data.open, right.data.open, 0.01, 0.002),
            ("high", left.data.high, right.data.high, 0.01, 0.002),
            ("low", left.data.low, right.data.low, 0.01, 0.002),
            (
                "pre_close",
                left.data.pre_close,
                right.data.pre_close,
                0.01,
                0.002,
            ),
            ("volume", left.data.volume, right.data.volume, 1.0, 0.02),
            ("amount", left.data.amount, right.data.amount, 1.0, 0.02),
            // One A-share price tick can move the displayed percentage by
            // ~0.17 percentage points on a 6 CNY stock. Providers are sampled
            // a few seconds apart, so a 0.25pp absolute tolerance represents
            // one live tick instead of a data conflict; price/pre-close still
            // have their own strict checks above.
            ("pct", left.data.pct, right.data.pct, 0.25, 0.02),
        ] {
            quote_checks.push(numeric_check(field, lv, rv, absolute, relative));
        }
    }
    let providers = [
        ("tdx", &tdx_bars),
        ("eastmoney", &eastmoney_bars),
        ("tencent", &tencent_bars),
        ("sina", &sina_bars),
    ];
    let reference = providers
        .iter()
        .find_map(|(provider, result)| result.as_ref().ok().map(|value| (*provider, value)));
    let mut kline_checks = Vec::new();
    if let Some((reference_provider, reference_rows)) = reference {
        for (provider, result) in providers
            .iter()
            .filter(|(provider, _)| *provider != reference_provider)
        {
            let Ok(candidate) = result else { continue };
            let candidate_by_date = candidate
                .data
                .iter()
                .map(|bar| (bar.date, bar))
                .collect::<BTreeMap<_, _>>();
            let mut provider_checks = Vec::new();
            for bar in reference_rows.data.iter().rev().take(60) {
                if let Some(other) = candidate_by_date.get(&bar.date) {
                    let difference = (bar.close - other.close).abs();
                    let tolerance = 0.01_f64.max(bar.close.abs().max(other.close.abs()) * 0.002);
                    provider_checks.push(json!({
                        "date": bar.date,
                        "reference_provider": reference_provider,
                        "provider": provider,
                        "reference_close": bar.close,
                        "provider_close": other.close,
                        "absolute_difference": difference,
                        "tolerance": tolerance,
                        "consistent": difference <= tolerance,
                    }));
                }
            }
            provider_checks.reverse();
            kline_checks.extend(provider_checks);
        }
    }
    let quote_conflicts = quote_checks
        .iter()
        .filter(|row| row.get("consistent") == Some(&Value::Bool(false)))
        .count();
    let kline_conflicts = kline_checks
        .iter()
        .filter(|row| row.get("consistent") == Some(&Value::Bool(false)))
        .count();
    let quote_sources = [
        provider_result("tdx", &tdx_quote),
        provider_result("eastmoney", &eastmoney_quote),
    ];
    let kline_sources = [
        bar_provider_result("tdx", &tdx_bars),
        bar_provider_result("eastmoney", &eastmoney_bars),
        bar_provider_result("tencent", &tencent_bars),
        bar_provider_result("sina", &sina_bars),
    ];
    let quote_successes = quote_sources
        .iter()
        .filter(|row| row.get("ok") == Some(&Value::Bool(true)))
        .count();
    let kline_successes = kline_sources
        .iter()
        .filter(|row| row.get("ok") == Some(&Value::Bool(true)))
        .count();
    json!({
        "symbol": symbol,
        "quote_sources": quote_sources,
        "quote_checks": quote_checks,
        "quote_conflicts": quote_conflicts,
        "kline_sources": kline_sources,
        "kline_successful_sources": kline_successes,
        "kline_overlap_days": kline_checks.len(),
        "kline_close_checks": kline_checks,
        "kline_conflicts": kline_conflicts,
        "blocking": quote_successes < 2 || kline_successes < 2 || quote_conflicts > 0 || kline_conflicts > 0,
        "policy": "报价要求TDX与东方财富双源；K线从TDX/东方财富/腾讯/新浪至少取得两个独立源；价格容差max(0.01元,0.2%)，量额容差2%；冲突不得用于高置信度结论",
    })
}

fn provider_result(
    provider: &str,
    value: &Result<astock_core::Fetched<astock_core::Quote>, astock_core::DataError>,
) -> Value {
    match value {
        Ok(fetched) => json!({
            "provider": provider,
            "ok": true,
            "fetched_at": fetched.fetched_at,
            "source": fetched.source,
            "quote": fetched.data,
        }),
        Err(error) => json!({ "provider": provider, "ok": false, "error": error.to_string() }),
    }
}

fn bar_provider_result(
    provider: &str,
    value: &Result<astock_core::Fetched<Vec<astock_core::Bar>>, astock_core::DataError>,
) -> Value {
    match value {
        Ok(fetched) => json!({
            "provider": provider,
            "ok": true,
            "fetched_at": fetched.fetched_at,
            "source": fetched.source,
            "rows": fetched.data.len(),
            "first_date": fetched.data.first().map(|bar| bar.date),
            "last_date": fetched.data.last().map(|bar| bar.date),
        }),
        Err(error) => json!({ "provider": provider, "ok": false, "error": error.to_string() }),
    }
}

fn parse_session_time(value: &str) -> NaiveTime {
    NaiveTime::parse_from_str(value, "%H:%M")
        .expect("RuleSet validates every configured A-share session time")
}

fn local_at(offset: &FixedOffset, date: NaiveDate, time: NaiveTime) -> DateTime<FixedOffset> {
    offset
        .from_local_datetime(&date.and_time(time))
        .single()
        .expect("China fixed offset has no ambiguous local time")
}

/// Exact A-share session status from the versioned trading calendar and
/// configured auction windows. This deliberately does not infer holidays from
/// weekdays alone.
fn market_session_payload(rules: &RuleSet, now: DateTime<FixedOffset>) -> Value {
    let date = now.date_naive();
    let time = now.time();
    let windows = &rules.data.auction;
    let open_start = parse_session_time(&windows.open_call_auction.start);
    let open_no_cancel = parse_session_time(&windows.open_call_auction.no_cancel_from);
    let open_end = parse_session_time(&windows.open_call_auction.end);
    let morning_start = parse_session_time(&windows.continuous_morning.start);
    let morning_end = parse_session_time(&windows.continuous_morning.end);
    let afternoon_start = parse_session_time(&windows.continuous_afternoon.start);
    let afternoon_end = parse_session_time(&windows.continuous_afternoon.end);
    let close_end = parse_session_time(&windows.close_call_auction.end);
    let trading_day = rules.is_trading_day(date);

    let (state, state_label, is_trading, next, next_label) = if !trading_day {
        let next_date = rules.next_trading_day(date);
        (
            "closed",
            "休市日",
            false,
            local_at(now.offset(), next_date, open_start),
            "下一交易日集合竞价",
        )
    } else if time < open_start {
        (
            "pre_open",
            "等待开盘",
            false,
            local_at(now.offset(), date, open_start),
            "开盘集合竞价",
        )
    } else if time < open_no_cancel {
        (
            "opening_auction",
            "开盘集合竞价",
            true,
            local_at(now.offset(), date, open_no_cancel),
            "集合竞价不可撤单",
        )
    } else if time < open_end {
        (
            "opening_auction_no_cancel",
            "集合竞价（不可撤单）",
            true,
            local_at(now.offset(), date, open_end),
            "集合竞价结束",
        )
    } else if time < morning_start {
        (
            "pre_continuous",
            "等待连续交易",
            false,
            local_at(now.offset(), date, morning_start),
            "上午开盘",
        )
    } else if time < morning_end {
        (
            "trading",
            "交易中",
            true,
            local_at(now.offset(), date, morning_end),
            "午间休市",
        )
    } else if time < afternoon_start {
        (
            "lunch_break",
            "午间休市",
            false,
            local_at(now.offset(), date, afternoon_start),
            "下午开盘",
        )
    } else if time < afternoon_end {
        (
            "trading",
            "交易中",
            true,
            local_at(now.offset(), date, afternoon_end),
            "收盘集合竞价",
        )
    } else if time < close_end {
        (
            "closing_auction",
            "收盘集合竞价",
            true,
            local_at(now.offset(), date, close_end),
            "收盘",
        )
    } else {
        let next_date = rules.next_trading_day(date);
        (
            "closed",
            "已收盘",
            false,
            local_at(now.offset(), next_date, open_start),
            "下一交易日集合竞价",
        )
    };

    json!({
        "exchange_timezone": "Asia/Shanghai",
        "server_time": now.to_rfc3339(),
        "trading_date": date,
        "is_trading_day": trading_day,
        "is_trading": is_trading,
        "state": state,
        "state_label": state_label,
        "next_transition_at": next.to_rfc3339(),
        "next_transition_label": next_label,
        "seconds_to_transition": next.signed_duration_since(now).num_seconds().max(0),
        "calendar": {
            "rules_version": rules.data.version,
            "verified_at": rules.data.calendar.verified_at,
            "source_url": rules.data.calendar.source_url,
        }
    })
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn now_secs() -> i64 {
    (now_ms() / 1_000).min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn period_and_adjust_are_strict() {
        assert_eq!(parse_period("day").unwrap(), KlinePeriod::Day);
        assert!(parse_period("DAY").is_err());
        assert_eq!(parse_adjust("qfq").unwrap(), Adjust::Qfq);
        assert!(parse_adjust("forward").is_err());
    }

    #[test]
    fn live_symbol_gate_rejects_legacy_neeq_and_keeps_current_markets() {
        assert!(parse_live_symbol("603927").is_ok());
        assert!(parse_live_symbol("920001").is_ok());
        assert!(parse_live_symbol("510300").is_ok());
        let error = parse_live_symbol("430002").unwrap_err();
        assert_eq!(error.code, "unsupported_live_symbol");
        assert!(error.message.contains("920xxx"));
    }

    #[test]
    fn tushare_capability_errors_distinguish_configuration_and_tier() {
        use astock_market_data::providers::TushareTier;

        assert!(tushare_pro_unavailable(&None)
            .to_string()
            .contains("未配置"));
        assert!(tushare_pro_unavailable(&Some(Ok(TushareTier::Free120)))
            .to_string()
            .contains("积分不足2000"));
        assert!(
            tushare_pro_unavailable(&Some(Err(astock_core::DataError::Timeout(
                "tier probe".into()
            ))))
            .to_string()
            .contains("权限探测失败")
        );
    }

    #[test]
    fn market_pool_and_board_inputs_are_closed_enums() {
        assert_eq!(
            parse_market_pool_kind("zt").unwrap(),
            MarketPoolKind::LimitUp
        );
        assert_eq!(
            parse_market_pool_kind(" SUB_NEW ").unwrap(),
            MarketPoolKind::SubNew
        );
        assert_eq!(parse_board_code(" bk0447 ").unwrap(), "BK0447");

        let pool_error = parse_market_pool_kind("anything").unwrap_err();
        assert_eq!(pool_error.code, "invalid_payload");
        let board_error = parse_board_code("000001").unwrap_err();
        assert_eq!(board_error.code, "invalid_payload");
    }

    #[test]
    fn news_archive_identifiers_reject_empty_or_control_text() {
        assert_eq!(
            validate_news_identifier(" document:123 ", "document_id").unwrap(),
            "document:123"
        );
        assert!(validate_news_identifier("\n", "document_id").is_err());
        assert!(validate_news_identifier(&"x".repeat(257), "provider_id").is_err());
        assert_eq!(validate_review_reason(" 独立事件 ").unwrap(), "独立事件");
        assert!(validate_review_reason(" ").is_err());
    }

    #[tokio::test]
    async fn earnings_driver_snapshots_round_trip_and_missing_ids_stay_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(astock_storage::StorageConfig::with_base_dir(dir.path()))
            .expect("open isolated storage");
        let tree = build_earnings_driver_tree(
            "603927",
            &astock_fundamental::FundamentalBundle::default(),
            1_700_000_000,
        );

        persist_driver_tree(&storage, &tree)
            .await
            .expect("persist driver snapshot");
        let replayed = load_driver_tree(&storage, tree.snapshot_id.clone())
            .await
            .expect("replay driver snapshot");
        assert_eq!(replayed, tree);

        let missing = load_driver_tree(&storage, "missing-snapshot".into())
            .await
            .expect_err("missing snapshots must not be fabricated");
        assert_eq!(missing.code, "not_found");
    }

    #[test]
    fn session_clock_respects_lunch_break_and_holidays() {
        let rules = RuleSet::load(None).unwrap();
        let offset = astock_core::time::china_tz();
        let lunch = offset
            .with_ymd_and_hms(2026, 8, 24, 12, 0, 0)
            .single()
            .unwrap();
        let payload = market_session_payload(&rules, lunch);
        assert_eq!(payload["state"], "lunch_break");
        assert_eq!(payload["next_transition_label"], "下午开盘");

        let holiday = offset
            .with_ymd_and_hms(2026, 10, 2, 10, 0, 0)
            .single()
            .unwrap();
        let payload = market_session_payload(&rules, holiday);
        assert_eq!(payload["state"], "closed");
        assert_eq!(payload["state_label"], "休市日");
        assert_eq!(payload["is_trading_day"], false);
    }
}
