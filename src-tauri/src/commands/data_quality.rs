//! Data lineage, freshness SLO and explicit dual-source reconciliation.

use astock_core::{
    reconcile_numeric, AccountingScope, AdjustmentBasis, Currency, DataEnvelope, DataError,
    DataQualitySummary, DataUnit, DatasetKind, Fetched, NumericObservation, QualityFlag,
    QualityFlagCode, Quote, ReconciliationResult, ReconciliationStatus, ReconciliationTolerance,
    Symbol,
};
use astock_market_data::DataProvider;
use astock_storage::{DatasetSlo, FieldLineageRecord, QualityObservation, ReconciliationAudit};
use chrono::{Datelike, FixedOffset, Timelike, Utc};
use serde::Serialize;
use tauri::State;

use crate::error::CmdError;
use crate::state::AppState;

const QUOTE_LICENSE: &str = "上游公开行情接口；仅限应用内研究，使用时须遵守来源条款";

#[derive(Debug, Clone, Serialize)]
pub struct ProviderFailure {
    pub provider: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuoteReconciliationReport {
    pub symbol: String,
    pub compared_at: i64,
    pub results: Vec<ReconciliationResult>,
    pub failures: Vec<ProviderFailure>,
    pub blocking: bool,
    pub comparable_sources: usize,
    pub limitation: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValuationReconciliationReport {
    pub symbol: String,
    pub compared_at: i64,
    pub results: Vec<ReconciliationResult>,
    pub failures: Vec<ProviderFailure>,
    pub blocking: bool,
    pub comparable_sources: usize,
    pub limitation: Option<String>,
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

#[derive(Debug, Clone, Serialize)]
pub struct DataHealthReport {
    pub generated_at: i64,
    pub window_secs: u64,
    pub actual_observations: usize,
    pub coverage_start: Option<i64>,
    pub coverage_end: Option<i64>,
    pub coverage_secs: u64,
    pub continuous_window_satisfied: bool,
    pub rows: Vec<DatasetSlo>,
    pub markdown: String,
    pub limitation: Option<String>,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_data_quality_slo(
    state: State<'_, AppState>,
    window_secs: u64,
) -> Result<Vec<DatasetSlo>, CmdError> {
    Ok(state.storage.quality_slo(window_secs).await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_data_quality_observations(
    state: State<'_, AppState>,
    dataset: Option<DatasetKind>,
    provider: Option<String>,
    limit: usize,
) -> Result<Vec<QualityObservation>, CmdError> {
    Ok(state
        .storage
        .quality_observations_recent(dataset, provider.as_deref(), limit)
        .await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_field_lineage(
    state: State<'_, AppState>,
    dataset: Option<DatasetKind>,
    entity_key: Option<String>,
    limit: usize,
) -> Result<Vec<FieldLineageRecord>, CmdError> {
    Ok(state
        .storage
        .field_lineage_recent(dataset, entity_key.as_deref(), limit)
        .await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_data_reconciliations(
    state: State<'_, AppState>,
    dataset: Option<DatasetKind>,
    entity_key: Option<String>,
    limit: usize,
) -> Result<Vec<ReconciliationAudit>, CmdError> {
    Ok(state
        .storage
        .reconciliations_recent(dataset, entity_key.as_deref(), limit)
        .await?)
}

pub(crate) async fn persist_quality_failure(
    state: &AppState,
    dataset: DatasetKind,
    provider: &str,
    entity_key: Option<String>,
    operation: &str,
    latency_ms: u64,
    error: String,
) -> Result<(), CmdError> {
    state
        .storage
        .quality_observation_add(QualityObservation {
            observation_id: None,
            dataset,
            provider: provider.into(),
            entity_key,
            operation: operation.into(),
            success: false,
            latency_ms: Some(latency_ms),
            summary: DataQualitySummary::evaluate(
                dataset,
                0,
                china_trading_session(),
                vec![QualityFlag::warning(
                    QualityFlagCode::Partial,
                    None,
                    "本次调用失败，没有产生可用于结论的数据",
                )],
            ),
            missing_fields: 0,
            conflicts: 0,
            error_kind: Some(error),
            recorded_at: Utc::now().timestamp(),
        })
        .await?;
    Ok(())
}

fn china_trading_session() -> bool {
    let china = FixedOffset::east_opt(8 * 3_600).expect("valid China offset");
    let now = Utc::now().with_timezone(&china);
    let minutes = now.hour() * 60 + now.minute();
    now.weekday().number_from_monday() <= 5
        && ((570..=690).contains(&minutes) || (780..=900).contains(&minutes))
}

fn quote_envelope(
    value: f64,
    fetched: &Fetched<Quote>,
    source: &str,
    source_url: Option<String>,
    unit: DataUnit,
    currency: Option<Currency>,
) -> DataEnvelope<f64> {
    DataEnvelope {
        data: value,
        dataset: DatasetKind::RealtimeQuote,
        source: source.into(),
        source_url,
        event_time: Some(fetched.data.timestamp),
        as_of_time: Some(fetched.data.timestamp),
        publish_time: None,
        fetched_at: fetched.fetched_at,
        parser_version: env!("CARGO_PKG_VERSION").into(),
        schema_version: "quote-v1".into(),
        license: QUOTE_LICENSE.into(),
        quality_flags: Vec::new(),
        unit: Some(unit),
        currency,
        adjustment: AdjustmentBasis::None,
        revision: None,
        accounting_scope: AccountingScope::NotApplicable,
    }
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

pub(crate) async fn persist_quote_source(
    state: &AppState,
    symbol: &str,
    provider: &str,
    source_url: Option<String>,
    operation: &str,
    latency_ms: Option<u64>,
    fetched: &Fetched<Quote>,
) -> Result<(), CmdError> {
    let missing = u32::from(fetched.data.turnover.is_none());
    let age = Utc::now()
        .timestamp()
        .saturating_sub(fetched.fetched_at.timestamp()) as u64;
    let mut flags = Vec::new();
    if missing > 0 {
        flags.push(QualityFlag::warning(
            QualityFlagCode::MissingField,
            Some("turnover"),
            "该行情源未返回换手率；保持为空，不以零代替",
        ));
    }
    let summary = DataQualitySummary::evaluate(
        DatasetKind::RealtimeQuote,
        age,
        china_trading_session(),
        flags,
    );
    state
        .storage
        .quality_observation_add(QualityObservation {
            observation_id: None,
            dataset: DatasetKind::RealtimeQuote,
            provider: provider.into(),
            entity_key: Some(symbol.into()),
            operation: operation.into(),
            success: true,
            latency_ms,
            summary,
            missing_fields: missing,
            conflicts: 0,
            error_kind: None,
            recorded_at: Utc::now().timestamp(),
        })
        .await?;
    for (field, value, unit, currency) in quote_fields(&fetched.data) {
        if field == "turnover" && fetched.data.turnover.is_none() {
            continue;
        }
        let envelope = quote_envelope(value, fetched, provider, source_url.clone(), unit, currency);
        state
            .storage
            .field_lineage_add(FieldLineageRecord::from((symbol, field, &envelope)))
            .await?;
    }
    Ok(())
}

fn numeric_observation(
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

fn valuation_observation(
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
        adjustment: AdjustmentBasis::NotApplicable,
        accounting_scope: if unit == DataUnit::Money {
            AccountingScope::Total
        } else {
            AccountingScope::NotApplicable
        },
        as_of_time: Some(as_of_time),
    }
}

fn reconcile_quote(left: &Quote, right: &Quote) -> Vec<ReconciliationResult> {
    let mut output = Vec::new();
    for (field, left_value, unit, currency) in quote_fields(left) {
        if field == "turnover" && (left.turnover.is_none() || right.turnover.is_none()) {
            continue;
        }
        let right_value = quote_fields(right)
            .into_iter()
            .find(|(candidate, _, _, _)| *candidate == field)
            .map(|(_, value, _, _)| value)
            .expect("same quote contract");
        let tolerance = match field {
            "amount" | "volume" => ReconciliationTolerance {
                absolute: 1.0,
                relative: 0.02,
            },
            "pct" | "turnover" => ReconciliationTolerance {
                absolute: 0.05,
                relative: 0.02,
            },
            _ => ReconciliationTolerance {
                absolute: 0.01,
                relative: 0.002,
            },
        };
        output.push(reconcile_numeric(
            numeric_observation("tdx", field, left_value, unit, currency, left.timestamp),
            numeric_observation(
                "eastmoney",
                field,
                right_value,
                unit,
                currency,
                right.timestamp,
            ),
            tolerance,
        ));
    }
    output
}

#[tauri::command(rename_all = "snake_case")]
pub async fn reconcile_quote_sources(
    state: State<'_, AppState>,
    symbol: String,
) -> Result<QuoteReconciliationReport, CmdError> {
    let parsed = Symbol::new(&symbol)?;
    let started = std::time::Instant::now();
    let (tdx, eastmoney) = tokio::join!(
        state.market.tdx.quote(&parsed),
        state.market.eastmoney.quote(&parsed)
    );
    let compared_at = Utc::now().timestamp();
    let mut failures = Vec::new();
    let mut successful = 0;

    if let Ok(fetched) = &tdx {
        successful += 1;
        persist_quote_source(
            &state,
            &symbol,
            "tdx",
            None,
            "quote_reconciliation",
            None,
            fetched,
        )
        .await?;
    } else if let Err(error) = &tdx {
        failures.push(ProviderFailure {
            provider: "tdx".into(),
            error: error.to_string(),
        });
    }
    if let Ok(fetched) = &eastmoney {
        successful += 1;
        persist_quote_source(
            &state,
            &symbol,
            "eastmoney",
            Some("https://push2.eastmoney.com".into()),
            "quote_reconciliation",
            None,
            fetched,
        )
        .await?;
    } else if let Err(error) = &eastmoney {
        failures.push(ProviderFailure {
            provider: "eastmoney".into(),
            error: error.to_string(),
        });
    }

    let results = match (&tdx, &eastmoney) {
        (Ok(left), Ok(right)) => reconcile_quote(&left.data, &right.data),
        _ => Vec::new(),
    };
    let blocking = successful < 2
        || results.iter().any(|result| {
            matches!(
                result.status,
                ReconciliationStatus::Conflict | ReconciliationStatus::IncompatibleContract
            )
        });
    for result in &results {
        state
            .storage
            .reconciliation_add(ReconciliationAudit {
                reconciliation_id: None,
                dataset: DatasetKind::RealtimeQuote,
                entity_key: symbol.clone(),
                blocking: matches!(
                    result.status,
                    ReconciliationStatus::Conflict | ReconciliationStatus::IncompatibleContract
                ),
                result: result.clone(),
                compared_at,
            })
            .await?;
    }
    for failure in &failures {
        let summary = DataQualitySummary::evaluate(
            DatasetKind::RealtimeQuote,
            0,
            china_trading_session(),
            vec![QualityFlag::warning(
                QualityFlagCode::Partial,
                None,
                "双源校验中该来源失败，不得将单源结果升级为高置信结论",
            )],
        );
        state
            .storage
            .quality_observation_add(QualityObservation {
                observation_id: None,
                dataset: DatasetKind::RealtimeQuote,
                provider: failure.provider.clone(),
                entity_key: Some(symbol.clone()),
                operation: "quote_reconciliation".into(),
                success: false,
                latency_ms: Some(started.elapsed().as_millis() as u64),
                summary,
                missing_fields: 0,
                conflicts: 0,
                error_kind: Some(failure.error.clone()),
                recorded_at: compared_at,
            })
            .await?;
    }
    Ok(QuoteReconciliationReport {
        symbol,
        compared_at,
        results,
        failures,
        blocking,
        comparable_sources: successful,
        limitation: (successful < 2)
            .then(|| "当前不足两个成功来源，已保留失败详情；不会用缓存或推算值伪装双源校验".into()),
    })
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
            let right_value = valuation_fields(right)
                .into_iter()
                .find(|(candidate, _, _, _)| *candidate == field)
                .and_then(|(_, value, _, _)| value)?;
            let left_value = left_value?;
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
            Some(reconcile_numeric(
                valuation_observation(
                    &left.provider,
                    field,
                    left_value,
                    unit,
                    currency,
                    left.as_of_time,
                ),
                valuation_observation(
                    &right.provider,
                    field,
                    right_value,
                    unit,
                    currency,
                    right.as_of_time,
                ),
                tolerance,
            ))
        })
        .collect()
}

async fn persist_valuation_source(
    state: &AppState,
    symbol: &str,
    value: &ProviderValuation,
) -> Result<(), CmdError> {
    let missing = valuation_fields(value)
        .iter()
        .filter(|(_, field, _, _)| field.is_none())
        .count() as u32;
    let mut flags = Vec::new();
    if missing > 0 {
        flags.push(QualityFlag::warning(
            QualityFlagCode::MissingField,
            None,
            format!("估值快照有 {missing} 个字段缺失，保持为空"),
        ));
    }
    let age = Utc::now()
        .timestamp()
        .saturating_sub(value.fetched_at.timestamp()) as u64;
    state
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
        .await?;
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
            license: QUOTE_LICENSE.into(),
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
        state
            .storage
            .field_lineage_add(FieldLineageRecord::from((symbol, field, &envelope)))
            .await?;
    }
    Ok(())
}

fn last_weekday(mut date: chrono::NaiveDate) -> chrono::NaiveDate {
    while date.weekday().number_from_monday() > 5 {
        date -= chrono::Duration::days(1);
    }
    date
}

#[tauri::command(rename_all = "snake_case")]
pub async fn reconcile_valuation_sources(
    state: State<'_, AppState>,
    symbol: String,
) -> Result<ValuationReconciliationReport, CmdError> {
    let parsed = Symbol::new(&symbol)?;
    let today = Utc::now()
        .with_timezone(&FixedOffset::east_opt(8 * 3_600).expect("valid China offset"))
        .date_naive();
    let trade_date = last_weekday(today);
    let jq_available = state.market.joinquant.available();
    let ts_available = state.market.tushare.available();
    let (eastmoney, joinquant, tushare) = tokio::join!(
        state.fundamental.snapshot(&parsed),
        async {
            if jq_available {
                state
                    .market
                    .joinquant
                    .valuation(std::slice::from_ref(&parsed), trade_date)
                    .await
            } else {
                Err(DataError::NoProvider("joinquant（未配置账号）"))
            }
        },
        async {
            if ts_available {
                state
                    .market
                    .tushare
                    .daily_basic(&parsed, trade_date - chrono::Duration::days(10), trade_date)
                    .await
            } else {
                Err(DataError::NoProvider("tushare（未配置访问凭证）"))
            }
        }
    );
    let market_close = |date: chrono::NaiveDate| {
        date.and_hms_opt(15, 0, 0)
            .expect("valid market close")
            .and_utc()
    };
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
        persist_valuation_source(&state, &symbol, source).await?;
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
    for result in &results {
        state
            .storage
            .reconciliation_add(ReconciliationAudit {
                reconciliation_id: None,
                dataset: DatasetKind::Valuation,
                entity_key: symbol.clone(),
                blocking: matches!(
                    result.status,
                    ReconciliationStatus::Conflict | ReconciliationStatus::IncompatibleContract
                ),
                result: result.clone(),
                compared_at,
            })
            .await?;
    }
    let blocking = sources.len() < 2
        || results.iter().any(|result| {
            matches!(
                result.status,
                ReconciliationStatus::Conflict | ReconciliationStatus::IncompatibleContract
            )
        });
    Ok(ValuationReconciliationReport {
        symbol,
        compared_at,
        results,
        failures,
        blocking,
        comparable_sources: sources.len(),
        limitation: (sources.len() < 2)
            .then(|| "已配置且成功返回的估值来源不足两个；估值仍可展示，但不得标为高置信".into()),
    })
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_data_health_report(
    state: State<'_, AppState>,
    window_secs: u64,
) -> Result<DataHealthReport, CmdError> {
    let window_secs = window_secs.clamp(60, 31_536_000);
    let observations = state
        .storage
        .quality_observations_recent(None, None, 5_000)
        .await?;
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
    let rows = state.storage.quality_slo(window_secs).await?;
    let continuous_window_satisfied = !rows.is_empty() && coverage_secs >= window_secs * 9 / 10;
    let limitation = (!continuous_window_satisfied).then(|| {
        format!(
            "真实观测覆盖 {coverage_secs} 秒，尚未达到所选 {window_secs} 秒连续窗口；报告不补造历史数据"
        )
    });
    let mut markdown = format!(
        "# 数据源连续健康报告\n\n生成时间：{}\n\n真实观测：{} 条；覆盖：{} 秒。\n\n",
        Utc::now().to_rfc3339(),
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
    Ok(DataHealthReport {
        generated_at: Utc::now().timestamp(),
        window_secs,
        actual_observations: in_window.len(),
        coverage_start,
        coverage_end,
        coverage_secs,
        continuous_window_satisfied,
        rows,
        markdown,
        limitation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote(provider_offset: f64) -> Quote {
        Quote {
            symbol: "600519".into(),
            name: "贵州茅台".into(),
            price: 1500.0 + provider_offset,
            open: 1490.0,
            high: 1510.0,
            low: 1480.0,
            pre_close: 1495.0,
            volume: 10_000.0,
            amount: 15_000_000.0,
            change: 5.0 + provider_offset,
            pct: 0.33,
            turnover: Some(0.2),
            timestamp: Utc::now(),
            field_provenance: Default::default(),
        }
    }

    #[test]
    fn quote_reconciliation_blocks_large_price_conflict() {
        let results = reconcile_quote(&quote(0.0), &quote(10.0));
        let price = results.iter().find(|item| item.field == "price").unwrap();
        assert_eq!(price.status, ReconciliationStatus::Conflict);
        assert!(price
            .quality_flags
            .iter()
            .any(|flag| flag.code == QualityFlagCode::SourceConflict));
    }

    #[test]
    fn quote_reconciliation_keeps_small_difference_within_tolerance() {
        let results = reconcile_quote(&quote(0.0), &quote(0.005));
        let price = results.iter().find(|item| item.field == "price").unwrap();
        assert_eq!(price.status, ReconciliationStatus::WithinTolerance);
    }
}
