//! Data-quality diagnostics and evidence-preserving cross-source reconciliation.
//!
//! This module deliberately returns upstream failures as data. A missing or
//! conflicting provider can lower confidence, but is never converted to zero
//! or silently replaced by a different source.

use astock_core::{
    reconcile_numeric, AccountingScope, AdjustmentBasis, Currency, DataEnvelope,
    DataQualitySummary, DataUnit, DatasetKind, Fetched, NumericObservation, QualityFlag,
    QualityFlagCode, Quote, ReconciliationResult, ReconciliationStatus, ReconciliationTolerance,
    Symbol,
};
use astock_market_data::DataProvider;
use astock_storage::{FieldLineageRecord, QualityObservation, ReconciliationAudit, Storage};
use chrono::{Datelike, FixedOffset, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{invalid, storage, Engine, ServiceError};

const PUBLIC_RESEARCH_LICENSE: &str =
    "上游公开或用户授权研究接口；仅限应用内研究，使用时须遵守来源条款";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DataQualityQuery {
    pub action: String,
    #[serde(default)]
    pub dataset: Option<DatasetKind>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub entity_key: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default = "default_window")]
    pub window_secs: u64,
}

fn default_limit() -> usize {
    100
}

fn default_window() -> u64 {
    86_400
}

#[derive(Debug, Clone, Serialize)]
struct ProviderFailure {
    provider: String,
    error: String,
}

#[derive(Debug, Clone)]
struct ProviderValuation {
    provider: String,
    source_url: Option<String>,
    fetched_at: chrono::DateTime<Utc>,
    as_of_time: chrono::DateTime<Utc>,
    pe_ttm: Option<f64>,
    pb: Option<f64>,
    total_market_cap: Option<f64>,
    float_market_cap: Option<f64>,
}

pub(super) async fn query(engine: &Engine, query: DataQualityQuery) -> Result<Value, ServiceError> {
    match query.action.as_str() {
        "slo" => Ok(json!(engine
            .storage
            .quality_slo(query.window_secs)
            .await
            .map_err(storage)?)),
        "observations" => Ok(json!(engine
            .storage
            .quality_observations_recent(
                query.dataset,
                query.provider.as_deref(),
                query.limit.clamp(1, 500),
            )
            .await
            .map_err(storage)?)),
        "lineage" => Ok(json!(engine
            .storage
            .field_lineage_recent(
                query.dataset,
                query.entity_key.as_deref(),
                query.limit.clamp(1, 500),
            )
            .await
            .map_err(storage)?)),
        "reconciliations" => Ok(json!(engine
            .storage
            .reconciliations_recent(
                query.dataset,
                query.entity_key.as_deref(),
                query.limit.clamp(1, 500),
            )
            .await
            .map_err(storage)?)),
        "health" => health_report(&engine.storage, query.window_secs).await,
        action => Err(invalid(format!(
            "unsupported data-quality action {action:?}; expected slo, observations, lineage, reconciliations or health"
        ))),
    }
}

async fn health_report(storage_ref: &Storage, window_secs: u64) -> Result<Value, ServiceError> {
    let window_secs = window_secs.clamp(60, 31_536_000);
    let observations = storage_ref
        .quality_observations_recent(None, None, 5_000)
        .await
        .map_err(storage)?;
    let cutoff = Utc::now().timestamp().saturating_sub(window_secs as i64);
    let in_window = observations
        .iter()
        .filter(|item| item.recorded_at >= cutoff)
        .collect::<Vec<_>>();
    let coverage_start = in_window.iter().map(|item| item.recorded_at).min();
    let coverage_end = in_window.iter().map(|item| item.recorded_at).max();
    let coverage_secs = coverage_start
        .zip(coverage_end)
        .map_or(0, |(start, end)| end.saturating_sub(start) as u64);
    let rows = storage_ref
        .quality_slo(window_secs)
        .await
        .map_err(storage)?;
    let continuous_window_satisfied = !rows.is_empty() && coverage_secs >= window_secs * 9 / 10;
    let limitation = (!continuous_window_satisfied).then(|| {
        format!(
            "真实观测覆盖 {coverage_secs} 秒，尚未达到所选 {window_secs} 秒连续窗口；报告不补造历史数据"
        )
    });
    let generated_at = Utc::now();
    let mut markdown = format!(
        "# 数据源连续健康报告\n\n生成时间：{}\n\n真实观测：{} 条；覆盖：{} 秒。\n\n",
        generated_at.to_rfc3339(),
        in_window.len(),
        coverage_secs
    );
    for row in &rows {
        markdown.push_str(&format!(
            "- {} / {}：成功 {}/{}，错误率 {:.2}%，P95 {} 毫秒，陈旧连续次数 {}，缺失字段 {}，冲突 {}\n",
            row.dataset_name,
            row.provider,
            row.successes,
            row.observations,
            row.error_rate * 100.0,
            row.latency_p95_ms.map_or_else(|| "暂无".into(), |value| value.to_string()),
            row.consecutive_stale,
            row.missing_fields,
            row.conflicts
        ));
    }
    if let Some(message) = &limitation {
        markdown.push_str(&format!("\n限制：{message}\n"));
    }
    Ok(json!({
        "generated_at": generated_at.timestamp(),
        "window_secs": window_secs,
        "actual_observations": in_window.len(),
        "coverage_start": coverage_start,
        "coverage_end": coverage_end,
        "coverage_secs": coverage_secs,
        "continuous_window_satisfied": continuous_window_satisfied,
        "rows": rows,
        "markdown": markdown,
        "limitation": limitation,
    }))
}

fn china_trading_session() -> bool {
    let china = FixedOffset::east_opt(8 * 3_600).expect("valid China offset");
    let now = Utc::now().with_timezone(&china);
    let minutes = now.hour() * 60 + now.minute();
    now.weekday().number_from_monday() <= 5
        && ((570..=690).contains(&minutes) || (780..=900).contains(&minutes))
}

fn quote_fields(quote: &Quote) -> [(&'static str, f64, DataUnit, Option<Currency>); 10] {
    [
        ("price", quote.price, DataUnit::Price, Some(Currency::Cny)),
        ("open", quote.open, DataUnit::Price, Some(Currency::Cny)),
        ("high", quote.high, DataUnit::Price, Some(Currency::Cny)),
        ("low", quote.low, DataUnit::Price, Some(Currency::Cny)),
        (
            "pre_close",
            quote.pre_close,
            DataUnit::Price,
            Some(Currency::Cny),
        ),
        ("volume", quote.volume, DataUnit::Lots, None),
        ("amount", quote.amount, DataUnit::Money, Some(Currency::Cny)),
        ("change", quote.change, DataUnit::Price, Some(Currency::Cny)),
        ("pct", quote.pct, DataUnit::Percentage, None),
        (
            "turnover",
            quote.turnover.unwrap_or_default(),
            DataUnit::Percentage,
            None,
        ),
    ]
}

fn quote_observation(
    provider: &str,
    field: &str,
    value: f64,
    unit: DataUnit,
    currency: Option<Currency>,
    as_of_time: chrono::DateTime<Utc>,
) -> NumericObservation {
    NumericObservation {
        provider: provider.into(),
        field: field.into(),
        value,
        unit,
        currency,
        adjustment: AdjustmentBasis::None,
        accounting_scope: AccountingScope::NotApplicable,
        as_of_time: Some(as_of_time),
    }
}

fn reconcile_quotes(left: &Quote, right: &Quote) -> Vec<ReconciliationResult> {
    quote_fields(left)
        .into_iter()
        .filter_map(|(field, left_value, unit, currency)| {
            if field == "turnover" && (left.turnover.is_none() || right.turnover.is_none()) {
                return None;
            }
            let right_value = quote_fields(right)
                .into_iter()
                .find(|(candidate, _, _, _)| *candidate == field)
                .map(|(_, value, _, _)| value)?;
            let tolerance = match field {
                "amount" | "volume" => ReconciliationTolerance {
                    absolute: 1.0,
                    relative: 0.02,
                },
                "pct" | "turnover" => ReconciliationTolerance {
                    absolute: 0.25,
                    relative: 0.02,
                },
                _ => ReconciliationTolerance {
                    absolute: 0.01,
                    relative: 0.002,
                },
            };
            Some(reconcile_numeric(
                quote_observation("tdx", field, left_value, unit, currency, left.timestamp),
                quote_observation(
                    "eastmoney",
                    field,
                    right_value,
                    unit,
                    currency,
                    right.timestamp,
                ),
                tolerance,
            ))
        })
        .collect()
}

async fn persist_quote_source(
    engine: &Engine,
    symbol: &str,
    provider: &str,
    source_url: Option<String>,
    fetched: &Fetched<Quote>,
) -> Result<(), ServiceError> {
    let missing = u32::from(fetched.data.turnover.is_none());
    let mut flags = Vec::new();
    if missing > 0 {
        flags.push(QualityFlag::warning(
            QualityFlagCode::MissingField,
            Some("turnover"),
            "该行情源未返回换手率；保持为空，不以零代替",
        ));
    }
    let age = Utc::now()
        .timestamp()
        .saturating_sub(fetched.fetched_at.timestamp()) as u64;
    engine
        .storage
        .quality_observation_add(QualityObservation {
            observation_id: None,
            dataset: DatasetKind::RealtimeQuote,
            provider: provider.into(),
            entity_key: Some(symbol.into()),
            operation: "quote_reconciliation".into(),
            success: true,
            latency_ms: None,
            summary: DataQualitySummary::evaluate(
                DatasetKind::RealtimeQuote,
                age,
                china_trading_session(),
                flags,
            ),
            missing_fields: missing,
            conflicts: 0,
            error_kind: None,
            recorded_at: Utc::now().timestamp(),
        })
        .await
        .map_err(storage)?;
    for (field, value, unit, currency) in quote_fields(&fetched.data) {
        if field == "turnover" && fetched.data.turnover.is_none() {
            continue;
        }
        let envelope = DataEnvelope {
            data: value,
            dataset: DatasetKind::RealtimeQuote,
            source: provider.into(),
            source_url: source_url.clone(),
            event_time: Some(fetched.data.timestamp),
            as_of_time: Some(fetched.data.timestamp),
            publish_time: None,
            fetched_at: fetched.fetched_at,
            parser_version: env!("CARGO_PKG_VERSION").into(),
            schema_version: "quote-v1".into(),
            license: PUBLIC_RESEARCH_LICENSE.into(),
            quality_flags: Vec::new(),
            unit: Some(unit),
            currency,
            adjustment: AdjustmentBasis::None,
            revision: None,
            accounting_scope: AccountingScope::NotApplicable,
        };
        engine
            .storage
            .field_lineage_add(FieldLineageRecord::from((symbol, field, &envelope)))
            .await
            .map_err(storage)?;
    }
    Ok(())
}

pub(super) async fn reconcile_quote(engine: &Engine, symbol: &str) -> Result<Value, ServiceError> {
    let parsed = Symbol::new(symbol).map_err(invalid)?;
    let started = std::time::Instant::now();
    let (tdx, eastmoney) = tokio::join!(
        engine.market.tdx.quote(&parsed),
        engine.market.eastmoney.quote(&parsed)
    );
    let compared_at = Utc::now().timestamp();
    let mut failures = Vec::new();
    let mut successful = 0usize;
    for (provider, source_url, result) in [
        ("tdx", None, &tdx),
        (
            "eastmoney",
            Some("https://push2.eastmoney.com".to_string()),
            &eastmoney,
        ),
    ] {
        match result {
            Ok(fetched) => {
                successful += 1;
                persist_quote_source(engine, symbol, provider, source_url, fetched).await?;
            }
            Err(error) => failures.push(ProviderFailure {
                provider: provider.into(),
                error: error.to_string(),
            }),
        }
    }
    let results = match (&tdx, &eastmoney) {
        (Ok(left), Ok(right)) => reconcile_quotes(&left.data, &right.data),
        _ => Vec::new(),
    };
    persist_reconciliations(
        engine,
        DatasetKind::RealtimeQuote,
        symbol,
        compared_at,
        &results,
    )
    .await?;
    for failure in &failures {
        engine
            .storage
            .quality_observation_add(QualityObservation {
                observation_id: None,
                dataset: DatasetKind::RealtimeQuote,
                provider: failure.provider.clone(),
                entity_key: Some(symbol.into()),
                operation: "quote_reconciliation".into(),
                success: false,
                latency_ms: Some(started.elapsed().as_millis() as u64),
                summary: DataQualitySummary::evaluate(
                    DatasetKind::RealtimeQuote,
                    0,
                    china_trading_session(),
                    vec![QualityFlag::warning(
                        QualityFlagCode::Partial,
                        None,
                        "双源校验中该来源失败，不得将单源结果升级为高置信结论",
                    )],
                ),
                missing_fields: 0,
                conflicts: 0,
                error_kind: Some(failure.error.clone()),
                recorded_at: compared_at,
            })
            .await
            .map_err(storage)?;
    }
    let blocking = successful < 2 || has_blocking_conflict(&results);
    Ok(json!({
        "symbol": symbol,
        "compared_at": compared_at,
        "results": results,
        "failures": failures,
        "blocking": blocking,
        "comparable_sources": successful,
        "limitation": (successful < 2).then(|| "当前不足两个成功来源，已保留失败详情；不会用缓存或推算值伪装双源校验"),
    }))
}

fn valuation_fields(
    value: &ProviderValuation,
) -> [(&'static str, Option<f64>, DataUnit, Option<Currency>); 4] {
    [
        ("pe_ttm", value.pe_ttm, DataUnit::Ratio, None),
        ("pb", value.pb, DataUnit::Ratio, None),
        (
            "total_market_cap",
            value.total_market_cap,
            DataUnit::Money,
            Some(Currency::Cny),
        ),
        (
            "float_market_cap",
            value.float_market_cap,
            DataUnit::Money,
            Some(Currency::Cny),
        ),
    ]
}

fn reconcile_valuations(
    left: &ProviderValuation,
    right: &ProviderValuation,
) -> Vec<ReconciliationResult> {
    valuation_fields(left)
        .into_iter()
        .filter_map(|(field, left_value, unit, currency)| {
            let left_value = left_value?;
            let right_value = valuation_fields(right)
                .into_iter()
                .find(|(candidate, _, _, _)| *candidate == field)
                .and_then(|(_, value, _, _)| value)?;
            let tolerance = if unit == DataUnit::Money {
                ReconciliationTolerance {
                    absolute: 10_000.0,
                    relative: 0.02,
                }
            } else {
                ReconciliationTolerance {
                    absolute: 0.05,
                    relative: 0.02,
                }
            };
            let observation = |provider: &str, value, as_of_time| NumericObservation {
                provider: provider.into(),
                field: field.into(),
                value,
                unit,
                currency,
                adjustment: AdjustmentBasis::NotApplicable,
                accounting_scope: if unit == DataUnit::Money {
                    AccountingScope::Total
                } else {
                    AccountingScope::NotApplicable
                },
                as_of_time: Some(as_of_time),
            };
            Some(reconcile_numeric(
                observation(&left.provider, left_value, left.as_of_time),
                observation(&right.provider, right_value, right.as_of_time),
                tolerance,
            ))
        })
        .collect()
}

async fn persist_valuation_source(
    engine: &Engine,
    symbol: &str,
    value: &ProviderValuation,
) -> Result<(), ServiceError> {
    let missing = valuation_fields(value)
        .iter()
        .filter(|(_, field, _, _)| field.is_none())
        .count() as u32;
    let flags = (missing > 0)
        .then(|| {
            QualityFlag::warning(
                QualityFlagCode::MissingField,
                None,
                format!("估值快照有 {missing} 个字段缺失，保持为空"),
            )
        })
        .into_iter()
        .collect();
    let age = Utc::now()
        .timestamp()
        .saturating_sub(value.fetched_at.timestamp()) as u64;
    engine
        .storage
        .quality_observation_add(QualityObservation {
            observation_id: None,
            dataset: DatasetKind::Valuation,
            provider: value.provider.clone(),
            entity_key: Some(symbol.into()),
            operation: "valuation_reconciliation".into(),
            success: true,
            latency_ms: None,
            summary: DataQualitySummary::evaluate(
                DatasetKind::Valuation,
                age,
                china_trading_session(),
                flags,
            ),
            missing_fields: missing,
            conflicts: 0,
            error_kind: None,
            recorded_at: Utc::now().timestamp(),
        })
        .await
        .map_err(storage)?;
    for (field, field_value, unit, currency) in valuation_fields(value) {
        let Some(field_value) = field_value else {
            continue;
        };
        let envelope = DataEnvelope {
            data: field_value,
            dataset: DatasetKind::Valuation,
            source: value.provider.clone(),
            source_url: value.source_url.clone(),
            event_time: Some(value.as_of_time),
            as_of_time: Some(value.as_of_time),
            publish_time: None,
            fetched_at: value.fetched_at,
            parser_version: env!("CARGO_PKG_VERSION").into(),
            schema_version: "valuation-v1".into(),
            license: PUBLIC_RESEARCH_LICENSE.into(),
            quality_flags: Vec::new(),
            unit: Some(unit),
            currency,
            adjustment: AdjustmentBasis::NotApplicable,
            revision: None,
            accounting_scope: if unit == DataUnit::Money {
                AccountingScope::Total
            } else {
                AccountingScope::NotApplicable
            },
        };
        engine
            .storage
            .field_lineage_add(FieldLineageRecord::from((symbol, field, &envelope)))
            .await
            .map_err(storage)?;
    }
    Ok(())
}

pub(super) async fn reconcile_valuation(
    engine: &Engine,
    symbol: &str,
) -> Result<Value, ServiceError> {
    let parsed = Symbol::new(symbol).map_err(invalid)?;
    let china = FixedOffset::east_opt(8 * 3_600).expect("valid China offset");
    let mut trade_date = Utc::now().with_timezone(&china).date_naive();
    while trade_date.weekday().number_from_monday() > 5 {
        trade_date -= chrono::Duration::days(1);
    }
    let market_close = |date: chrono::NaiveDate| {
        date.and_hms_opt(15, 0, 0)
            .expect("valid market close")
            .and_utc()
    };
    let jq_available = engine.market.joinquant.available();
    let ts_available = engine.market.tushare.available();
    let (eastmoney, joinquant, tushare) = tokio::join!(
        engine.fundamental.snapshot(&parsed),
        async {
            if jq_available {
                engine
                    .market
                    .joinquant
                    .valuation(std::slice::from_ref(&parsed), trade_date)
                    .await
            } else {
                Err(astock_core::DataError::NoProvider(
                    "joinquant（未配置账号）",
                ))
            }
        },
        async {
            if ts_available {
                engine
                    .market
                    .tushare
                    .daily_basic(&parsed, trade_date - chrono::Duration::days(10), trade_date)
                    .await
            } else {
                Err(astock_core::DataError::NoProvider(
                    "tushare（未配置访问凭证）",
                ))
            }
        }
    );
    let mut sources = Vec::new();
    let mut failures = Vec::new();
    match eastmoney {
        Ok(fetched) => sources.push(ProviderValuation {
            provider: "eastmoney".into(),
            source_url: Some("https://push2.eastmoney.com".into()),
            fetched_at: fetched.fetched_at,
            as_of_time: fetched.fetched_at,
            pe_ttm: fetched.data.pe_ttm,
            pb: fetched.data.pb,
            total_market_cap: fetched.data.total_market_cap,
            float_market_cap: fetched.data.float_market_cap,
        }),
        Err(error) => failures.push(ProviderFailure {
            provider: "eastmoney".into(),
            error: error.to_string(),
        }),
    }
    match joinquant {
        Ok(rows) => {
            if let Some(row) = rows.into_iter().next() {
                sources.push(ProviderValuation {
                    provider: "joinquant".into(),
                    source_url: Some("https://www.joinquant.com".into()),
                    fetched_at: Utc::now(),
                    as_of_time: market_close(trade_date),
                    pe_ttm: row.pe_ratio,
                    pb: row.pb_ratio,
                    total_market_cap: row.market_cap.map(|value| value * 100_000_000.0),
                    float_market_cap: row
                        .circulating_market_cap
                        .map(|value| value * 100_000_000.0),
                });
            }
        }
        Err(error) => failures.push(ProviderFailure {
            provider: "joinquant".into(),
            error: error.to_string(),
        }),
    }
    match tushare {
        Ok(rows) => {
            if let Some(row) = rows.into_iter().max_by_key(|row| row.date) {
                sources.push(ProviderValuation {
                    provider: "tushare".into(),
                    source_url: Some("https://api.tushare.pro".into()),
                    fetched_at: Utc::now(),
                    as_of_time: market_close(row.date),
                    pe_ttm: row.pe_ttm,
                    pb: row.pb,
                    total_market_cap: row.total_mv,
                    float_market_cap: row.circ_mv,
                });
            }
        }
        Err(error) => failures.push(ProviderFailure {
            provider: "tushare".into(),
            error: error.to_string(),
        }),
    }
    for source in &sources {
        persist_valuation_source(engine, symbol, source).await?;
    }
    let mut results = Vec::new();
    if let Some(primary) = sources.iter().find(|source| source.provider == "eastmoney") {
        for secondary in sources
            .iter()
            .filter(|source| source.provider != "eastmoney")
        {
            results.extend(reconcile_valuations(primary, secondary));
        }
    }
    let compared_at = Utc::now().timestamp();
    persist_reconciliations(
        engine,
        DatasetKind::Valuation,
        symbol,
        compared_at,
        &results,
    )
    .await?;
    let blocking = sources.len() < 2 || has_blocking_conflict(&results);
    Ok(json!({
        "symbol": symbol,
        "compared_at": compared_at,
        "results": results,
        "failures": failures,
        "blocking": blocking,
        "comparable_sources": sources.len(),
        "limitation": (sources.len() < 2).then(|| "已配置且成功返回的估值来源不足两个；估值仍可展示，但不得标为高置信"),
    }))
}

fn has_blocking_conflict(results: &[ReconciliationResult]) -> bool {
    results.iter().any(|result| {
        matches!(
            result.status,
            ReconciliationStatus::Conflict | ReconciliationStatus::IncompatibleContract
        )
    })
}

async fn persist_reconciliations(
    engine: &Engine,
    dataset: DatasetKind,
    symbol: &str,
    compared_at: i64,
    results: &[ReconciliationResult],
) -> Result<(), ServiceError> {
    for result in results {
        engine
            .storage
            .reconciliation_add(ReconciliationAudit {
                reconciliation_id: None,
                dataset,
                entity_key: symbol.into(),
                blocking: matches!(
                    result.status,
                    ReconciliationStatus::Conflict | ReconciliationStatus::IncompatibleContract
                ),
                result: result.clone(),
                compared_at,
            })
            .await
            .map_err(storage)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_storage::StorageConfig;

    #[tokio::test]
    async fn empty_health_report_is_explicit_and_never_fabricated() {
        let dir = tempfile::tempdir().unwrap();
        let storage_ref = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let report = health_report(&storage_ref, 86_400).await.unwrap();
        assert_eq!(report["actual_observations"], 0);
        assert_eq!(report["continuous_window_satisfied"], false);
        assert!(report["limitation"]
            .as_str()
            .unwrap()
            .contains("不补造历史数据"));
    }

    #[test]
    fn missing_turnover_is_not_reconciled_as_zero() {
        let now = Utc::now();
        let mut left = Quote {
            symbol: "600519".into(),
            name: "贵州茅台".into(),
            price: 1500.0,
            open: 1490.0,
            high: 1510.0,
            low: 1480.0,
            pre_close: 1495.0,
            volume: 10_000.0,
            amount: 15_000_000.0,
            change: 5.0,
            pct: 0.33,
            turnover: None,
            timestamp: now,
            field_provenance: Default::default(),
        };
        let mut right = left.clone();
        right.turnover = Some(0.2);
        assert!(!reconcile_quotes(&left, &right)
            .iter()
            .any(|row| row.field == "turnover"));
        left.turnover = Some(0.2);
        assert!(reconcile_quotes(&left, &right)
            .iter()
            .any(|row| row.field == "turnover"));
    }
}
