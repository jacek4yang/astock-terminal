use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use astock_core::{Adjust, Bar, KlinePeriod, Symbol};
use astock_event_intelligence::{
    analyze_price_in, build_event_study_sample, extract_structured_event, CalibrationSummary,
    EventEntityRef, EventExtractionInput, EventResearchBundle, EventStore, EventStudyInput,
    PriceInInput, PriceSeriesPoint,
};
use astock_fundamental::FundamentalClient;
use astock_market_data::{DataProvider, MarketData};
use astock_storage::{BarRow, Storage};
use astock_trading_rules::{
    classify_news_session, publication_precision_from_source, NewsSessionInput, RuleSet,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EventAnalysisRequest {
    pub revision_id: String,
    pub security_code: Option<String>,
    pub structured_impact_bps: Option<i64>,
    pub consensus_impact_bps: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventAnalysisStart {
    pub job_id: String,
    pub started: bool,
    pub reused: bool,
    pub estimated_seconds: u32,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventAnalysisSnapshot {
    pub job_id: String,
    pub revision_id: String,
    pub security_code: Option<String>,
    pub running: bool,
    pub status: String,
    pub phase: String,
    pub progress: u8,
    pub current_item: String,
    pub estimated_remaining_seconds: Option<u32>,
    pub recent_logs: Vec<String>,
    pub result: Option<EventResearchBundle>,
    pub error: Option<String>,
    pub started_at: i64,
    pub updated_at: i64,
}

#[derive(Default)]
struct EventAnalysisState {
    jobs: Mutex<HashMap<String, EventAnalysisSnapshot>>,
    cancels: Mutex<HashMap<String, CancellationToken>>,
}

#[derive(Clone, Default)]
pub struct EventAnalysisService {
    inner: Arc<EventAnalysisState>,
}

impl EventAnalysisService {
    pub async fn start(
        &self,
        storage: Storage,
        market: Arc<MarketData>,
        fundamental: Arc<FundamentalClient>,
        rules: RuleSet,
        request: EventAnalysisRequest,
    ) -> Result<EventAnalysisStart, String> {
        let revision_id = validate_id(&request.revision_id, "revision_id")?.to_string();
        let security_code = request
            .security_code
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| Symbol::new(value).map(|symbol| symbol.code().to_string()))
            .transpose()
            .map_err(|error| error.to_string())?;
        let key = job_key(
            &revision_id,
            security_code.as_deref(),
            request.structured_impact_bps,
            request.consensus_impact_bps,
        );
        if let Some(existing) = self
            .inner
            .jobs
            .lock()
            .expect("event jobs poisoned")
            .get(&key)
            .cloned()
        {
            if !matches!(existing.status.as_str(), "failed" | "cancelled") {
                return Ok(EventAnalysisStart {
                    job_id: existing.job_id,
                    started: existing.running,
                    reused: true,
                    estimated_seconds: existing.estimated_remaining_seconds.unwrap_or(0),
                    note: "已恢复同一事件的后台分析；切换页面不会丢失进度和结果。".into(),
                });
            }
        }
        self.inner
            .jobs
            .lock()
            .expect("event jobs poisoned")
            .remove(&key);
        self.inner
            .cancels
            .lock()
            .expect("event cancels poisoned")
            .remove(&key);

        let estimated_seconds = if security_code.is_some() { 35 } else { 5 };
        let now = now_secs();
        self.inner.jobs.lock().expect("event jobs poisoned").insert(
            key.clone(),
            EventAnalysisSnapshot {
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
            },
        );
        let token = CancellationToken::new();
        self.inner
            .cancels
            .lock()
            .expect("event cancels poisoned")
            .insert(key.clone(), token.clone());
        let state = Arc::clone(&self.inner);
        let job_id = key.clone();
        tokio::spawn(async move {
            run_job(
                state,
                storage,
                market,
                fundamental,
                rules,
                token,
                job_id,
                revision_id,
                security_code,
                request.structured_impact_bps,
                request.consensus_impact_bps,
                estimated_seconds,
            )
            .await;
        });
        Ok(EventAnalysisStart {
            job_id: key,
            started: true,
            reused: false,
            estimated_seconds,
            note: "分析已进入 Engine 后台；预计时间只用于展示，不设置强制截止。".into(),
        })
    }

    pub fn status(&self, job_id: &str) -> Result<EventAnalysisSnapshot, String> {
        let job_id = validate_id(job_id, "job_id")?;
        self.inner
            .jobs
            .lock()
            .expect("event jobs poisoned")
            .get(job_id)
            .cloned()
            .ok_or_else(|| "事件分析任务不存在或已清理".into())
    }

    pub fn cancel(&self, job_id: &str) -> Result<bool, String> {
        let job_id = validate_id(job_id, "job_id")?;
        let running = self
            .inner
            .jobs
            .lock()
            .expect("event jobs poisoned")
            .get(job_id)
            .is_some_and(|snapshot| snapshot.running);
        if !running {
            return Ok(false);
        }
        let token = self
            .inner
            .cancels
            .lock()
            .expect("event cancels poisoned")
            .get(job_id)
            .cloned();
        if let Some(token) = token {
            token.cancel();
            update(
                &self.inner,
                job_id,
                99,
                "正在安全停止事件分析",
                "已保存的事件证据与校准样本不会删除",
                None,
            );
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_job(
    state: Arc<EventAnalysisState>,
    storage: Storage,
    market: Arc<MarketData>,
    fundamental: Arc<FundamentalClient>,
    rules: RuleSet,
    token: CancellationToken,
    job_id: String,
    revision_id: String,
    security_code: Option<String>,
    structured_impact_bps: Option<i64>,
    consensus_impact_bps: Option<i64>,
    estimated_seconds: u32,
) {
    let store = EventStore::new(storage.clone());
    let revision = match storage.news_archive_revision(&revision_id).await {
        Ok(Some(value)) => value,
        Ok(None) => {
            fail(&state, &job_id, format!("来源修订不存在：{revision_id}"));
            return;
        }
        Err(error) => {
            fail(&state, &job_id, format!("读取来源修订失败：{error}"));
            return;
        }
    };
    if stop_if_cancelled(&state, &job_id, &token) {
        return;
    }
    update(
        &state,
        &job_id,
        12,
        "正在逐字段绑定原文证据",
        &format!("{} · {}", revision.source_name, revision.title),
        Some(estimated_seconds.saturating_sub(3)),
    );
    let subjects = match linked_subjects(&storage, &revision_id).await {
        Ok(values) => values,
        Err(error) => {
            log(
                &state,
                &job_id,
                format!("实体链接读取失败：{error}；事件主体保持未解析，不猜测证券身份"),
            );
            Vec::new()
        }
    };
    let source_is_primary = is_primary_revision(
        &revision.source_id,
        &revision.content_type,
        &revision.license,
    );
    let event = match store.event_by_revision(&revision_id).await {
        Ok(Some(event)) => {
            log(&state, &job_id, "复用已保存的结构化事件与字段证据".into());
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
                    fail(&state, &job_id, format!("结构化事件入库失败：{error}"));
                    return;
                }
                event
            }
            Err(error) => {
                fail(&state, &job_id, format!("结构化事件提取失败：{error}"));
                return;
            }
        },
    };
    log(
        &state,
        &job_id,
        format!(
            "事件本体：{}；状态：{}；事实缺口 {} 项；证据 {} 条",
            event.kind.chinese_name(),
            event.lifecycle.chinese_name(),
            event.missing_fields.len(),
            event.evidence.len()
        ),
    );
    if stop_if_cancelled(&state, &job_id, &token) {
        return;
    }

    let mut assessment = None;
    if let Some(code) = security_code.as_deref() {
        update(
            &state,
            &job_id,
            28,
            "正在读取事件前行情、成交量与市场基准",
            &format!("{code} 对比沪深300；日线最多 180 期"),
            Some(28),
        );
        let symbol = match Symbol::new(code) {
            Ok(value) => value,
            Err(error) => {
                fail(&state, &job_id, format!("证券代码无效：{error}"));
                return;
            }
        };
        let benchmark = Symbol::new("000300").expect("static benchmark symbol");
        let (stock_fetch, benchmark_fetch) = tokio::join!(
            market.kline(&symbol, KlinePeriod::Day, Adjust::Qfq, 180),
            market.kline(&benchmark, KlinePeriod::Day, Adjust::None, 180)
        );
        let (stock_bars, stock_source) = fetched_bars(
            &storage,
            code,
            "qfq",
            stock_fetch,
            &state,
            &job_id,
            "个股行情",
        )
        .await;
        let (benchmark_bars, benchmark_source) = fetched_bars(
            &storage,
            "000300",
            "none",
            benchmark_fetch,
            &state,
            &job_id,
            "市场基准",
        )
        .await;
        let event_date = match effective_event_date(&rules, &revision) {
            Ok(value) => value,
            Err(error) => {
                fail(&state, &job_id, error);
                return;
            }
        };
        update(
            &state,
            &job_id,
            52,
            "正在读取历史估值并构造板块相对序列",
            &format!("事件归属交易日 {event_date}"),
            Some(18),
        );
        let (valuation, valuation_source) = match fundamental.valuation_history(&symbol).await {
            Ok(values) => (
                values
                    .data
                    .into_iter()
                    .filter_map(|point| {
                        point.close.map(|close| PriceSeriesPoint {
                            date: point.date.to_string(),
                            close,
                            volume: 0.0,
                            pe_ttm: point.pe_ttm,
                        })
                    })
                    .collect(),
                values.source.to_string(),
            ),
            Err(error) => {
                log(
                    &state,
                    &job_id,
                    format!("历史估值缺口：{error}；不会用当前估值倒推历史"),
                );
                (Vec::new(), "missing".into())
            }
        };
        let sector = cached_sector_series(&storage, code).await;
        if sector.is_empty() {
            log(
                &state,
                &job_id,
                "板块历史序列缺口：行业分类或同业缓存不足；结果会降低可量化程度".into(),
            );
        }
        let stock = bars_to_points(&stock_bars);
        let benchmark_points = bars_to_points(&benchmark_bars);
        let analogs = match store.historical_analogs(event.kind, 200).await {
            Ok(values) => values,
            Err(error) => {
                log(
                    &state,
                    &job_id,
                    format!("历史同类事件读取失败：{error}；本轮不输出同类概率结论"),
                );
                Vec::new()
            }
        };
        update(
            &state,
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
            .unwrap_or_else(|| event_date.clone());
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
            benchmark: benchmark_points.clone(),
            sector,
            valuation: valuation.clone(),
            structured_impact_bps,
            consensus_impact_bps,
            historical_analogs: analogs,
            data_versions,
        }) {
            Ok(value) => {
                if let Err(error) = store.save_assessment(value.clone()).await {
                    log(&state, &job_id, format!("市场定价评估保存失败：{error}"));
                }
                let study_version = format!("{stock_source}+{benchmark_source}");
                if let Some(sample) = build_event_study_sample(EventStudyInput {
                    event: &event,
                    security_code: code,
                    event_date: &event_date,
                    stock: &stock,
                    benchmark: &benchmark_points,
                    valuation: &valuation,
                    post_window_days: 20,
                    data_version: &study_version,
                }) {
                    if let Err(error) = store.save_study_sample(sample).await {
                        log(
                            &state,
                            &job_id,
                            format!("事件研究样本保存失败：{error}；本轮结果不计入未来校准"),
                        );
                    }
                }
                assessment = Some(value);
            }
            Err(error) => log(&state, &job_id, format!("price-in 无法完成量化：{error}")),
        }
    }
    if stop_if_cancelled(&state, &job_id, &token) {
        return;
    }
    update(
        &state,
        &job_id,
        92,
        "正在汇总事件时间线、催化路径与失效条件",
        "不把正负情绪转换为买卖指令",
        Some(3),
    );
    let timeline = match store.timeline(&event.event_id).await {
        Ok(value) => value,
        Err(error) => {
            log(
                &state,
                &job_id,
                format!("事件时间线读取失败：{error}；不会生成推测时间线"),
            );
            Vec::new()
        }
    };
    let calibration = match store.calibration_summary(event.kind).await {
        Ok(value) => value,
        Err(error) => {
            log(
                &state,
                &job_id,
                format!("历史校准读取失败：{error}；概率与胜率结论保持不可用"),
            );
            CalibrationSummary {
                ontology_kind: event.kind,
                sample_count: 0,
                median_post_abnormal_return_bps: None,
                positive_sample_ratio_bps: None,
                data_versions: Vec::new(),
            }
        }
    };
    finish(
        &state,
        &job_id,
        EventResearchBundle {
            event,
            timeline,
            assessment,
            calibration,
        },
    );
}

async fn fetched_bars(
    storage: &Storage,
    symbol: &str,
    adjust: &str,
    fetched: Result<astock_core::Fetched<Vec<Bar>>, astock_core::DataError>,
    state: &Arc<EventAnalysisState>,
    job_id: &str,
    label: &str,
) -> (Vec<Bar>, String) {
    match fetched {
        Ok(value) => {
            let source = value.source.to_string();
            let rows = value
                .data
                .iter()
                .map(|bar| BarRow {
                    date: bar.date,
                    open: bar.open,
                    high: bar.high,
                    low: bar.low,
                    close: bar.close,
                    volume: bar.volume,
                    amount: bar.amount,
                    turnover: bar.turnover,
                    source: source.clone(),
                    fetched_at: value.fetched_at.timestamp(),
                })
                .collect::<Vec<_>>();
            if let Err(error) = storage
                .merge_and_write_bars(symbol, "day", adjust, rows)
                .await
            {
                log(
                    state,
                    job_id,
                    format!("{label}缓存写入失败：{error}；本轮仍使用已获取数据"),
                );
            }
            (value.data, source)
        }
        Err(error) => {
            log(
                state,
                job_id,
                format!("{label}缺口：{error}；不会用其他指标伪造替代"),
            );
            (Vec::new(), "missing".into())
        }
    }
}

async fn linked_subjects(
    storage: &Storage,
    revision_id: &str,
) -> astock_storage::Result<Vec<EventEntityRef>> {
    let revision_id = revision_id.to_string();
    storage
        .run(move |connection| {
            let mut statement = connection.prepare(
                "SELECT l.final_entity_id,COALESCE(e.canonical_name,l.span_text),e.listed_code
                 FROM document_entity_links l LEFT JOIN research_entities e
                   ON e.entity_id=l.final_entity_id
                 WHERE l.revision_id=?1 AND l.status='accepted' AND l.final_entity_id IS NOT NULL
                 ORDER BY l.confidence DESC,l.span_start LIMIT 20",
            )?;
            let rows = statement.query_map([revision_id], |row| {
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
    rules: &RuleSet,
    revision: &astock_storage::ArchivedNewsRevision,
) -> Result<String, String> {
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
    .or_else(|classification_error| {
        chrono::DateTime::from_timestamp(
            revision
                .publish_time
                .utc
                .or(revision.event_time.utc)
                .unwrap_or(revision.first_seen_time_utc),
            0,
        )
        .map(|value| value.date_naive().to_string())
        .ok_or_else(|| {
            format!("事件时间不可解析：{classification_error}；不会用当前日期替代来源时间")
        })
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

async fn cached_sector_series(storage: &Storage, code: &str) -> Vec<PriceSeriesPoint> {
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
            if window[0].close.is_finite()
                && window[0].close > 0.0
                && window[1].close.is_finite()
                && window[1].close > 0.0
            {
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
            let change = values.iter().map(|value| value.0).sum::<f64>() / values.len() as f64;
            close *= 1.0 + change;
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

fn validate_id<'a>(raw: &'a str, field: &str) -> Result<&'a str, String> {
    let value = raw.trim();
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        Err(format!("{field} 必须是 1 至 256 个无控制字符的文本"))
    } else {
        Ok(value)
    }
}

fn update(
    state: &Arc<EventAnalysisState>,
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
        snapshot.progress = snapshot.progress.max(progress.min(100));
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

fn log(state: &Arc<EventAnalysisState>, job_id: &str, message: String) {
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

fn finish(state: &Arc<EventAnalysisState>, job_id: &str, result: EventResearchBundle) {
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

fn fail(state: &Arc<EventAnalysisState>, job_id: &str, error: String) {
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

fn stop_if_cancelled(
    state: &Arc<EventAnalysisState>,
    job_id: &str,
    token: &CancellationToken,
) -> bool {
    if !token.is_cancelled() {
        return false;
    }
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
    true
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn cancel_only_changes_running_jobs() {
        let service = EventAnalysisService::default();
        assert!(!service.cancel("event-missing").unwrap());
        assert!(service.status("event-missing").is_err());
    }

    #[tokio::test]
    async fn missing_revision_finishes_as_visible_failure_without_network_access() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(astock_storage::StorageConfig::with_base_dir(dir.path()))
            .expect("open isolated event storage");
        let market = Arc::new(MarketData::with_storage(storage.clone()));
        let fundamental = Arc::new(FundamentalClient::new(Arc::new(
            astock_market_data::EastMoneyF10::new(market.http.clone(), market.cache.clone()),
        )));
        let service = EventAnalysisService::default();
        let started = service
            .start(
                storage,
                market,
                fundamental,
                RuleSet::load(None).unwrap(),
                EventAnalysisRequest {
                    revision_id: "revision:missing".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let snapshot = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let snapshot = service.status(&started.job_id).unwrap();
                if !snapshot.running {
                    break snapshot;
                }
                // A finite number of `yield_now` calls is scheduler dependent on
                // current-thread runtimes. Poll at a bounded real interval so this
                // test verifies the observable job contract instead of executor luck.
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("missing revision job did not reach a terminal state");
        assert_eq!(snapshot.status, "failed");
        assert!(snapshot.error.as_deref().unwrap().contains("不存在"));
    }
}
