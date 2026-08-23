//! Background structured-event extraction and deterministic price-in research.

use std::collections::BTreeMap;
use std::sync::Arc;

use astock_core::{Adjust, Bar, KlinePeriod, Symbol};
use astock_event_intelligence::{
    analyze_price_in, build_event_study_sample, extract_structured_event, CalibrationSummary,
    EventEntityRef, EventExtractionInput, EventResearchBundle, EventStore, EventStudyInput,
    PriceInInput, PriceSeriesPoint,
};
use astock_storage::BarRow;
use astock_trading_rules::{
    classify_news_session, publication_precision_from_source, NewsSessionInput,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::State;
use tokio_util::sync::CancellationToken;

use crate::error::CmdError;
use crate::state::{AppState, EventAnalysisSnapshot, EventAnalysisState};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EventAnalysisRequest {
    pub revision_id: String,
    pub security_code: Option<String>,
    /// Optional sourced operating-impact estimate, in basis points. The UI
    /// does not manufacture this field; callers must provide a source.
    pub structured_impact_bps: Option<i64>,
    /// Optional sourced market-consensus estimate, in basis points.
    pub consensus_impact_bps: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventAnalysisStartResponse {
    pub job_id: String,
    pub started: bool,
    pub reused: bool,
    pub estimated_seconds: u32,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventAnalysisCancelResponse {
    pub cancelled: bool,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn event_analysis_start(
    state: State<'_, AppState>,
    request: EventAnalysisRequest,
) -> Result<EventAnalysisStartResponse, CmdError> {
    let revision_id = request.revision_id.trim().to_string();
    if revision_id.is_empty() {
        return Err(CmdError::new("invalid_revision", "事件来源修订不能为空"));
    }
    let security_code = request
        .security_code
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(code) = security_code.as_deref() {
        Symbol::new(code).map_err(CmdError::from)?;
    }
    let key = job_key(
        &revision_id,
        security_code.as_deref(),
        request.structured_impact_bps,
        request.consensus_impact_bps,
    );
    let existing = state
        .event_analysis
        .jobs
        .lock()
        .expect("event jobs poisoned")
        .get(&key)
        .cloned();
    if let Some(existing) = existing {
        if !matches!(existing.status.as_str(), "failed" | "cancelled") {
            return Ok(EventAnalysisStartResponse {
                job_id: existing.job_id,
                started: existing.running,
                reused: true,
                estimated_seconds: existing.estimated_remaining_seconds.unwrap_or(0),
                note: "已恢复同一事件的后台分析；切换页面不会丢失进度和结果。".into(),
            });
        }
        state
            .event_analysis
            .jobs
            .lock()
            .expect("event jobs poisoned")
            .remove(&key);
        state
            .event_analysis
            .cancels
            .lock()
            .expect("event cancels poisoned")
            .remove(&key);
    }

    let estimated_seconds = if security_code.is_some() { 35 } else { 5 };
    let now = now_secs();
    let snapshot = EventAnalysisSnapshot {
        job_id: key.clone(),
        revision_id: revision_id.clone(),
        security_code: security_code.clone(),
        running: true,
        status: "running".into(),
        phase: "正在建立事件事实与证据字段".into(),
        progress: 5,
        current_item: revision_id.clone(),
        estimated_remaining_seconds: Some(estimated_seconds),
        recent_logs: vec![format!(
            "任务参数：来源修订 {revision_id}；关联标的 {}；经营影响 {}；一致预期 {}",
            security_code.as_deref().unwrap_or("未指定"),
            request
                .structured_impact_bps
                .map(|value| format!("{value}bp"))
                .unwrap_or_else(|| "未提供".into()),
            request
                .consensus_impact_bps
                .map(|value| format!("{value}bp"))
                .unwrap_or_else(|| "未提供".into())
        )],
        result: None,
        error: None,
        started_at: now,
        updated_at: now,
    };
    state
        .event_analysis
        .jobs
        .lock()
        .expect("event jobs poisoned")
        .insert(key.clone(), snapshot);
    let token = CancellationToken::new();
    state
        .event_analysis
        .cancels
        .lock()
        .expect("event cancels poisoned")
        .insert(key.clone(), token.clone());

    let jobs = Arc::clone(&state.event_analysis);
    let storage = state.storage.clone();
    let market = Arc::clone(&state.market);
    let fundamental = Arc::clone(&state.fundamental);
    let rules = state.rules.clone();
    let job_id = key.clone();
    tauri::async_runtime::spawn(async move {
        let store = EventStore::new(storage.clone());
        let revision = match storage.news_archive_revision(&revision_id).await {
            Ok(Some(value)) => value,
            Ok(None) => {
                fail(&jobs, &job_id, format!("来源修订不存在：{revision_id}"));
                return;
            }
            Err(error) => {
                fail(&jobs, &job_id, format!("读取来源修订失败：{error}"));
                return;
            }
        };
        if token.is_cancelled() {
            cancel(&jobs, &job_id);
            return;
        }
        update(
            &jobs,
            &job_id,
            12,
            "正在逐字段绑定原文证据",
            &format!("{} · {}", revision.source_name, revision.title),
            Some(estimated_seconds.saturating_sub(3)),
        );
        let subjects = linked_subjects(&storage, &revision_id)
            .await
            .unwrap_or_default();
        let source_is_primary = is_primary_revision(
            &revision.source_id,
            &revision.content_type,
            &revision.license,
        );
        let event = match store.event_by_revision(&revision_id).await {
            Ok(Some(event)) => {
                log(&jobs, &job_id, "复用已保存的结构化事件与字段证据".into());
                event
            }
            _ => match extract_structured_event(EventExtractionInput {
                source_revision_id: revision.revision_id.clone(),
                source_version_id: None,
                title: revision.title.clone(),
                factual_summary: revision.factual_summary.clone(),
                original_language: revision.language.clone(),
                source_is_primary,
                event_time_utc: revision.event_time.utc,
                first_seen_at: revision.first_seen_time_utc,
                subjects,
            }) {
                Ok(event) => {
                    if let Err(error) = store.upsert_event(event.clone()).await {
                        fail(&jobs, &job_id, format!("结构化事件入库失败：{error}"));
                        return;
                    }
                    event
                }
                Err(error) => {
                    fail(&jobs, &job_id, format!("结构化事件提取失败：{error}"));
                    return;
                }
            },
        };
        log(
            &jobs,
            &job_id,
            format!(
                "事件本体：{}；状态：{}；事实缺口 {} 项；证据 {} 条",
                event.kind.chinese_name(),
                event.lifecycle.chinese_name(),
                event.missing_fields.len(),
                event.evidence.len()
            ),
        );
        if token.is_cancelled() {
            cancel(&jobs, &job_id);
            return;
        }

        let mut assessment = None;
        if let Some(code) = security_code.as_deref() {
            update(
                &jobs,
                &job_id,
                28,
                "正在读取事件前行情、成交量与市场基准",
                &format!("{code} 对比沪深300；日线最多 180 期"),
                Some(28),
            );
            let symbol = match Symbol::new(code) {
                Ok(value) => value,
                Err(error) => {
                    fail(&jobs, &job_id, format!("证券代码无效：{error}"));
                    return;
                }
            };
            let benchmark_symbol = Symbol::new("000300").expect("static benchmark symbol");
            let (stock_fetch, benchmark_fetch) = tokio::join!(
                crate::cache_path::kline_read_through(
                    &storage,
                    &market,
                    &rules,
                    &symbol,
                    KlinePeriod::Day,
                    Adjust::Qfq,
                    180,
                ),
                crate::cache_path::kline_read_through(
                    &storage,
                    &market,
                    &rules,
                    &benchmark_symbol,
                    KlinePeriod::Day,
                    Adjust::None,
                    180,
                )
            );
            let (stock_bars, stock_source) = match stock_fetch {
                Ok(value) => value,
                Err(error) => {
                    log(
                        &jobs,
                        &job_id,
                        format!("个股行情缺口：{error}；price-in 将保留不可量化状态"),
                    );
                    (Vec::new(), "missing".into())
                }
            };
            let (benchmark_bars, benchmark_source) = match benchmark_fetch {
                Ok(value) => value,
                Err(error) => {
                    log(
                        &jobs,
                        &job_id,
                        format!("市场基准缺口：{error}；不会用个股绝对涨跌替代异常收益"),
                    );
                    (Vec::new(), "missing".into())
                }
            };
            let event_date = effective_event_date(&rules, &revision);
            update(
                &jobs,
                &job_id,
                52,
                "正在读取历史估值并构造板块相对序列",
                &format!("事件归属交易日 {event_date}"),
                Some(18),
            );
            let valuation_fetch = fundamental.valuation_history(&symbol).await;
            let (valuation, valuation_source) = match valuation_fetch {
                Ok(values) => (
                    values
                        .data
                        .into_iter()
                        .map(|point| PriceSeriesPoint {
                            date: point.date.to_string(),
                            close: point.close.unwrap_or(0.0),
                            volume: 0.0,
                            pe_ttm: point.pe_ttm,
                        })
                        .collect(),
                    values.source.to_string(),
                ),
                Err(error) => {
                    log(
                        &jobs,
                        &job_id,
                        format!("历史估值缺口：{error}；不会用当前估值倒推历史"),
                    );
                    (Vec::new(), "missing".into())
                }
            };
            let sector = cached_sector_series(&storage, code).await;
            if sector.is_empty() {
                log(
                    &jobs,
                    &job_id,
                    "板块历史序列缺口：行业分类或同业缓存不足；结果会降低可量化程度".into(),
                );
            } else {
                log(
                    &jobs,
                    &job_id,
                    format!("板块代理序列：{} 个交易日，来自同业本地缓存", sector.len()),
                );
            }
            let stock = bars_to_points(&stock_bars);
            let benchmark = bars_to_points(&benchmark_bars);
            let analogs = store
                .historical_analogs(event.kind, 200)
                .await
                .unwrap_or_default();
            update(
                &jobs,
                &job_id,
                72,
                "正在分离基本面影响与市场定价",
                &format!(
                    "异常收益/成交量/板块/估值/预期/历史同类；校准样本 {} 个",
                    analogs.len()
                ),
                Some(10),
            );
            let as_of_date = stock
                .last()
                .map(|point| point.date.clone())
                .unwrap_or_else(|| Utc::now().date_naive().to_string());
            let data_versions = serde_json::json!({
                "price_in_model": astock_event_intelligence::PRICE_IN_MODEL_VERSION,
                "stock_bars": stock_source,
                "benchmark_bars": benchmark_source,
                "sector": if sector.is_empty() { "missing" } else { "cached_industry_peers" },
                "valuation": valuation_source,
                "event_extraction": event.extraction_version,
            });
            match analyze_price_in(PriceInInput {
                event: event.clone(),
                security_code: code.into(),
                event_date: event_date.clone(),
                as_of_date,
                stock: stock.clone(),
                benchmark: benchmark.clone(),
                sector,
                valuation: valuation.clone(),
                structured_impact_bps: request.structured_impact_bps,
                consensus_impact_bps: request.consensus_impact_bps,
                historical_analogs: analogs,
                data_versions: data_versions.clone(),
            }) {
                Ok(value) => {
                    if let Err(error) = store.save_assessment(value.clone()).await {
                        log(&jobs, &job_id, format!("市场定价评估保存失败：{error}"));
                    }
                    let study_version = format!("{stock_source}+{benchmark_source}");
                    if let Some(sample) = build_event_study_sample(EventStudyInput {
                        event: &event,
                        security_code: code,
                        event_date: &event_date,
                        stock: &stock,
                        benchmark: &benchmark,
                        valuation: &valuation,
                        post_window_days: 20,
                        data_version: &study_version,
                    }) {
                        if let Err(error) = store.save_study_sample(sample).await {
                            log(&jobs, &job_id, format!("历史事件研究样本保存失败：{error}"));
                        } else {
                            log(
                                &jobs,
                                &job_id,
                                "该历史事件已进入 20 日 price-in 校准集".into(),
                            );
                        }
                    } else {
                        log(
                            &jobs,
                            &job_id,
                            "事件后 20 个交易日尚不完整，本轮不写入校准集，避免未来数据泄漏".into(),
                        );
                    }
                    assessment = Some(value);
                }
                Err(error) => {
                    log(&jobs, &job_id, format!("price-in 无法完成量化：{error}"));
                }
            }
        }
        if token.is_cancelled() {
            cancel(&jobs, &job_id);
            return;
        }
        update(
            &jobs,
            &job_id,
            92,
            "正在汇总事件时间线、催化路径与失效条件",
            "不把正负情绪转换为买卖指令",
            Some(3),
        );
        let timeline = store.timeline(&event.event_id).await.unwrap_or_default();
        let calibration =
            store
                .calibration_summary(event.kind)
                .await
                .unwrap_or(CalibrationSummary {
                    ontology_kind: event.kind,
                    sample_count: 0,
                    median_post_abnormal_return_bps: None,
                    positive_sample_ratio_bps: None,
                    data_versions: Vec::new(),
                });
        finish(
            &jobs,
            &job_id,
            EventResearchBundle {
                event,
                timeline,
                assessment,
                calibration,
            },
        );
    });

    Ok(EventAnalysisStartResponse {
        job_id: key,
        started: true,
        reused: false,
        estimated_seconds,
        note: "分析已进入后台；预计时间只用于展示，不设置强制截止，可切换页面后继续。".into(),
    })
}

#[tauri::command(rename_all = "snake_case")]
pub async fn event_analysis_status(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<EventAnalysisSnapshot, CmdError> {
    state
        .event_analysis
        .jobs
        .lock()
        .expect("event jobs poisoned")
        .get(job_id.trim())
        .cloned()
        .ok_or_else(|| CmdError::new("event_job_not_found", "事件分析任务不存在或已清理"))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn event_analysis_cancel(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<EventAnalysisCancelResponse, CmdError> {
    let token = state
        .event_analysis
        .cancels
        .lock()
        .expect("event cancels poisoned")
        .get(job_id.trim())
        .cloned();
    if let Some(token) = token {
        token.cancel();
        update(
            &state.event_analysis,
            job_id.trim(),
            99,
            "正在安全停止事件分析",
            "已保存的事件证据与校准样本不会删除",
            None,
        );
        Ok(EventAnalysisCancelResponse { cancelled: true })
    } else {
        Ok(EventAnalysisCancelResponse { cancelled: false })
    }
}

async fn linked_subjects(
    storage: &astock_storage::Storage,
    revision_id: &str,
) -> astock_storage::Result<Vec<EventEntityRef>> {
    let revision_id = revision_id.to_string();
    storage
        .run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT l.final_entity_id,COALESCE(e.canonical_name,l.span_text),e.listed_code
                 FROM document_entity_links l LEFT JOIN research_entities e
                   ON e.entity_id=l.final_entity_id
                 WHERE l.revision_id=?1 AND l.status='accepted' AND l.final_entity_id IS NOT NULL
                 ORDER BY l.confidence DESC,l.span_start LIMIT 20",
            )?;
            let rows = stmt.query_map([revision_id], |row| {
                Ok(EventEntityRef {
                    entity_id: row.get(0)?,
                    name: row.get(1)?,
                    listed_code: row.get(2)?,
                    role: "subject".into(),
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
}

fn is_primary_revision(source_id: &str, content_type: &str, license: &str) -> bool {
    let source = source_id.to_ascii_lowercase();
    let content = content_type.to_ascii_lowercase();
    source.contains("official")
        || source.contains("cninfo")
        || source.contains("sse")
        || source.contains("szse")
        || source.contains("sec")
        || content.contains("announcement")
        || content.contains("disclosure")
        || license.contains("正式披露")
}

fn effective_event_date(
    rules: &astock_trading_rules::RuleSet,
    revision: &astock_storage::ArchivedNewsRevision,
) -> String {
    classify_news_session(
        rules,
        &NewsSessionInput {
            event_time_utc: revision.event_time.utc,
            publish_time_utc: revision.publish_time.utc,
            first_seen_time_utc: revision.first_seen_time_utc,
            revision_time_utc: revision.revision_time.utc,
            publication_precision: publication_precision_from_source(
                revision.publish_time.utc,
                Some(&revision.parser_version),
            ),
            stale: false,
            verified: is_primary_revision(
                &revision.source_id,
                &revision.content_type,
                &revision.license,
            ),
            discovery_only: revision.source_id.contains("newsnow"),
            old_republication: false,
        },
    )
    .map(|session| session.target_trading_date.to_string())
    .unwrap_or_else(|_| {
        chrono::DateTime::from_timestamp(
            revision
                .publish_time
                .utc
                .or(revision.event_time.utc)
                .unwrap_or(revision.first_seen_time_utc),
            0,
        )
        .map(|value| value.date_naive().to_string())
        .unwrap_or_else(|| Utc::now().date_naive().to_string())
    })
}

fn bars_to_points(bars: &[Bar]) -> Vec<PriceSeriesPoint> {
    bars.iter()
        .map(|bar| PriceSeriesPoint {
            date: bar.date.to_string(),
            close: bar.close,
            volume: bar.volume,
            pe_ttm: None,
        })
        .collect()
}

/// Cached peer basket only: price-in research must not fan out dozens of
/// live requests just to fabricate a sector benchmark. Missing cache is
/// explicitly reported instead.
async fn cached_sector_series(
    storage: &astock_storage::Storage,
    code: &str,
) -> Vec<PriceSeriesPoint> {
    let records = match storage.securities_list().await {
        Ok(records) => records,
        Err(_) => return Vec::new(),
    };
    let industry = records
        .iter()
        .find(|record| record.code == code)
        .and_then(|record| record.industry.clone());
    let Some(industry) = industry else {
        return Vec::new();
    };
    let peers = records
        .iter()
        .filter(|record| {
            record.code != code && record.industry.as_deref() == Some(industry.as_str())
        })
        .take(8)
        .map(|record| record.code.clone())
        .collect::<Vec<_>>();
    let mut cached = Vec::new();
    for peer in peers {
        if let Ok(rows) = storage.load_bars(&peer, "day", "qfq").await {
            if rows.len() >= 20 {
                cached.push(rows);
            }
        }
    }
    peer_composite(&cached)
}

fn peer_composite(series: &[Vec<BarRow>]) -> Vec<PriceSeriesPoint> {
    let mut daily: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
    for rows in series {
        for window in rows.windows(2) {
            if window[0].close > 0.0 && window[1].close.is_finite() {
                daily
                    .entry(window[1].date.to_string())
                    .or_default()
                    .push((window[1].close / window[0].close - 1.0, window[1].volume));
            }
        }
    }
    let mut close = 100.0;
    daily
        .into_iter()
        .filter_map(|(date, values)| {
            if values.len() < 2 {
                return None;
            }
            let ret = values.iter().map(|value| value.0).sum::<f64>() / values.len() as f64;
            close *= 1.0 + ret;
            Some(PriceSeriesPoint {
                date,
                close,
                volume: values.iter().map(|value| value.1).sum(),
                pe_ttm: None,
            })
        })
        .collect()
}

fn job_key(
    revision_id: &str,
    security_code: Option<&str>,
    structured_impact_bps: Option<i64>,
    consensus_impact_bps: Option<i64>,
) -> String {
    let input = format!(
        "{revision_id}|{}|{}|{}",
        security_code.unwrap_or("evidence-only"),
        structured_impact_bps
            .map(|number| number.to_string())
            .unwrap_or_else(|| "na".into()),
        consensus_impact_bps
            .map(|number| number.to_string())
            .unwrap_or_else(|| "na".into())
    );
    let digest = format!("{:x}", Sha256::digest(input.as_bytes()));
    format!("event-{}", &digest[..24])
}

fn update(
    state: &EventAnalysisState,
    job_id: &str,
    progress: u8,
    phase: &str,
    item: &str,
    estimate: Option<u32>,
) {
    if let Some(snapshot) = state
        .jobs
        .lock()
        .expect("event jobs poisoned")
        .get_mut(job_id)
    {
        snapshot.progress = progress;
        snapshot.phase = phase.into();
        snapshot.current_item = item.into();
        snapshot.estimated_remaining_seconds = estimate;
        snapshot.updated_at = now_secs();
        snapshot.recent_logs.push(format!("{phase} · {item}"));
        if snapshot.recent_logs.len() > 120 {
            let overflow = snapshot.recent_logs.len() - 120;
            snapshot.recent_logs.drain(..overflow);
        }
    }
}

fn log(state: &EventAnalysisState, job_id: &str, message: String) {
    if let Some(snapshot) = state
        .jobs
        .lock()
        .expect("event jobs poisoned")
        .get_mut(job_id)
    {
        snapshot.recent_logs.push(message);
        if snapshot.recent_logs.len() > 120 {
            snapshot.recent_logs.remove(0);
        }
        snapshot.updated_at = now_secs();
    }
}

fn finish(state: &EventAnalysisState, job_id: &str, result: EventResearchBundle) {
    if let Some(snapshot) = state
        .jobs
        .lock()
        .expect("event jobs poisoned")
        .get_mut(job_id)
    {
        snapshot.running = false;
        snapshot.status = "completed".into();
        snapshot.phase = "结构化事件与市场定价分析已完成".into();
        snapshot.progress = 100;
        snapshot.current_item = "基本面结论与股价机会已分开输出".into();
        snapshot.estimated_remaining_seconds = Some(0);
        snapshot.result = Some(result);
        snapshot.updated_at = now_secs();
        snapshot
            .recent_logs
            .push("完成：缺失字段与不可量化输入保持可见，未生成买卖指令".into());
    }
    state
        .cancels
        .lock()
        .expect("event cancels poisoned")
        .remove(job_id);
}

fn fail(state: &EventAnalysisState, job_id: &str, error: String) {
    if let Some(snapshot) = state
        .jobs
        .lock()
        .expect("event jobs poisoned")
        .get_mut(job_id)
    {
        snapshot.running = false;
        snapshot.status = "failed".into();
        snapshot.phase = "事件分析失败".into();
        snapshot.error = Some(error.clone());
        snapshot.recent_logs.push(error);
        snapshot.updated_at = now_secs();
    }
    state
        .cancels
        .lock()
        .expect("event cancels poisoned")
        .remove(job_id);
}

fn cancel(state: &EventAnalysisState, job_id: &str) {
    if let Some(snapshot) = state
        .jobs
        .lock()
        .expect("event jobs poisoned")
        .get_mut(job_id)
    {
        snapshot.running = false;
        snapshot.status = "cancelled".into();
        snapshot.phase = "事件分析已取消".into();
        snapshot.estimated_remaining_seconds = None;
        snapshot.updated_at = now_secs();
    }
    state
        .cancels
        .lock()
        .expect("event cancels poisoned")
        .remove(job_id);
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_core::time::utc_now;
    use astock_core::{AssetType, Board, Market, SecurityMasterRecord};

    #[test]
    fn primary_classification_never_promotes_generic_media() {
        assert!(is_primary_revision(
            "cninfo_official",
            "announcement",
            "正式披露"
        ));
        assert!(!is_primary_revision("media", "article", "公开转载"));
    }

    #[test]
    fn peer_composite_requires_more_than_one_peer_per_day() {
        let rows = |code_offset: f64| {
            (1..=25)
                .map(|day| BarRow {
                    date: chrono::NaiveDate::from_ymd_opt(2026, 7, day).unwrap(),
                    open: 10.0,
                    high: 11.0,
                    low: 9.0,
                    close: 10.0 + day as f64 * code_offset,
                    volume: 100.0,
                    amount: None,
                    turnover: None,
                    source: "fixture".into(),
                    fetched_at: 1,
                })
                .collect::<Vec<_>>()
        };
        assert!(peer_composite(&[rows(0.1)]).is_empty());
        assert_eq!(peer_composite(&[rows(0.1), rows(0.2)]).len(), 24);
    }

    #[test]
    fn fixture_security_record_shape_stays_compatible() {
        let record = SecurityMasterRecord {
            code: "600000".into(),
            canonical_name: "测试".into(),
            market: Market::SH,
            board: Board::Main,
            asset_type: AssetType::Stock,
            aliases: Vec::new(),
            industry: Some("银行".into()),
            concepts: Vec::new(),
            region: None,
            source: "fixture".into(),
            source_url: None,
            valid_from: None,
            valid_to: None,
            refreshed_at: utc_now(),
        };
        assert_eq!(record.industry.as_deref(), Some("银行"));
    }

    #[test]
    fn job_identity_uses_full_revision_and_sourced_expectations() {
        let shared_prefix = "revision:123456789012345678901234";
        let first = job_key(&format!("{shared_prefix}:a"), Some("600000"), None, None);
        let second = job_key(&format!("{shared_prefix}:b"), Some("600000"), None, None);
        let with_consensus = job_key(
            &format!("{shared_prefix}:a"),
            Some("600000"),
            Some(800),
            Some(700),
        );
        assert_ne!(first, second);
        assert_ne!(first, with_consensus);
        assert_eq!(
            first,
            job_key(&format!("{shared_prefix}:a"), Some("600000"), None, None)
        );
    }
}
