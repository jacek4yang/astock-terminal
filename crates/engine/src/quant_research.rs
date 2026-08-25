//! Cancellable, persistent Quant Lab orchestration over the deterministic
//! `astock-quant` research core.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use astock_core::{Adjust, KlinePeriod, Symbol};
use astock_market_data::{DataProvider, MarketData};
use astock_quant::research::{ResearchConfig, ResearchProgress, ResearchSnapshot, SeriesInput};
use astock_storage::{QuantResearchSnapshotRow, Storage};
use futures::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

type ProgressReporter = Arc<dyn Fn(ResearchProgress) + Send + Sync>;
type CancellationCheck = Arc<dyn Fn() -> bool + Send + Sync>;

static JOB_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantResearchJobSnapshot {
    pub job_id: String,
    pub running: bool,
    pub status: String,
    pub phase: String,
    pub progress: u8,
    pub done_pairs: usize,
    pub total_pairs: usize,
    pub current_pair: Option<[String; 2]>,
    pub effective_observations: usize,
    pub fetched_series: usize,
    pub total_series: usize,
    pub estimated_remaining_seconds: Option<u64>,
    pub recent_logs: Vec<String>,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub started_at: i64,
    pub updated_at: i64,
}

#[derive(Default)]
struct QuantResearchState {
    jobs: Mutex<HashMap<String, QuantResearchJobSnapshot>>,
    cancels: Mutex<HashMap<String, CancellationToken>>,
}

#[derive(Clone, Default)]
pub struct QuantResearchService {
    inner: Arc<QuantResearchState>,
}

impl QuantResearchService {
    pub async fn restore(storage: &Storage) -> Result<Self, String> {
        let rows = storage
            .run(|connection| {
                let mut statement = connection.prepare(
                    "SELECT job_id,progress_json,created_at,updated_at
                     FROM quant_research_jobs ORDER BY updated_at DESC LIMIT 100",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(|error| error.to_string())?;
        let service = Self::default();
        for (job_id, encoded, created_at, updated_at) in rows {
            let mut snapshot: QuantResearchJobSnapshot = match serde_json::from_str(&encoded) {
                Ok(value) => value,
                Err(error) => QuantResearchJobSnapshot {
                    job_id,
                    running: false,
                    status: "failed".into(),
                    phase: "历史任务状态损坏；原始数据库记录未被覆盖".into(),
                    progress: 0,
                    done_pairs: 0,
                    total_pairs: 0,
                    current_pair: None,
                    effective_observations: 0,
                    fetched_series: 0,
                    total_series: 0,
                    estimated_remaining_seconds: None,
                    recent_logs: vec![format!("progress_json 解析失败：{error}")],
                    result: None,
                    error: Some("storage_corrupt".into()),
                    started_at: created_at,
                    updated_at,
                },
            };
            if snapshot.running {
                snapshot.running = false;
                snapshot.status = "suspended".into();
                snapshot.phase = "应用重启中断了计算；已保存行情缓存，可按相同配置重新发起".into();
                snapshot.error = Some("worker_restart".into());
                snapshot.updated_at = now_secs();
                persist_job(storage, &snapshot, result_snapshot_id(&snapshot)).await?;
            }
            service
                .inner
                .jobs
                .lock()
                .expect("quant jobs poisoned")
                .insert(snapshot.job_id.clone(), snapshot);
        }
        Ok(service)
    }

    pub async fn start(
        &self,
        market: Arc<MarketData>,
        storage: Storage,
        mut config: ResearchConfig,
    ) -> Result<QuantResearchJobSnapshot, String> {
        normalize_config(&mut config)?;
        let running = self
            .inner
            .jobs
            .lock()
            .expect("quant jobs poisoned")
            .values()
            .filter(|job| job.running)
            .count();
        if running >= 2 {
            return Err("已有两个量化研究任务在后台运行，请等待或取消其中一个".into());
        }
        let job_id = format!(
            "quant-{}-{}",
            now_millis(),
            JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let now = now_secs();
        let snapshot = QuantResearchJobSnapshot {
            job_id: job_id.clone(),
            running: true,
            status: "running".into(),
            phase: "等待获取行情".into(),
            progress: 0,
            done_pairs: 0,
            total_pairs: 0,
            current_pair: None,
            effective_observations: 0,
            fetched_series: 0,
            total_series: config.symbols.len(),
            estimated_remaining_seconds: None,
            recent_logs: vec![format!("任务 {job_id} 已创建；无固定超时，可随时取消")],
            result: None,
            error: None,
            started_at: now,
            updated_at: now,
        };
        self.inner
            .jobs
            .lock()
            .expect("quant jobs poisoned")
            .insert(job_id.clone(), snapshot.clone());
        let token = CancellationToken::new();
        self.inner
            .cancels
            .lock()
            .expect("quant cancels poisoned")
            .insert(job_id.clone(), token.clone());
        persist_job(&storage, &snapshot, None).await?;

        let state = Arc::clone(&self.inner);
        tokio::spawn(async move {
            run_job(state, market, storage, token, job_id, config).await;
        });
        Ok(snapshot)
    }

    pub fn status(&self, job_id: Option<&str>) -> Result<Option<QuantResearchJobSnapshot>, String> {
        let guard = self.inner.jobs.lock().expect("quant jobs poisoned");
        if let Some(job_id) = job_id {
            let job_id = validate_id(job_id, "job_id")?;
            Ok(guard.get(job_id).cloned())
        } else {
            Ok(guard.values().max_by_key(|job| job.started_at).cloned())
        }
    }

    pub fn cancel(&self, job_id: &str) -> Result<bool, String> {
        let job_id = validate_id(job_id, "job_id")?;
        let token = self
            .inner
            .cancels
            .lock()
            .expect("quant cancels poisoned")
            .get(job_id)
            .cloned();
        let Some(token) = token else {
            return Ok(false);
        };
        token.cancel();
        if let Some(job) = self
            .inner
            .jobs
            .lock()
            .expect("quant jobs poisoned")
            .get_mut(job_id)
        {
            job.phase = "正在安全取消：等待当前统计单元结束".into();
            job.updated_at = now_secs();
            job.recent_logs
                .push("已收到取消请求，不会启动新的关系检验".into());
        }
        Ok(true)
    }
}

async fn run_job(
    state: Arc<QuantResearchState>,
    market: Arc<MarketData>,
    storage: Storage,
    token: CancellationToken,
    job_id: String,
    config: ResearchConfig,
) {
    let progress_state = Arc::clone(&state);
    let progress_job_id = job_id.clone();
    let progress: ProgressReporter = Arc::new(move |update| {
        let now = now_secs();
        let mut guard = progress_state.jobs.lock().expect("quant jobs poisoned");
        let Some(job) = guard.get_mut(&progress_job_id) else {
            return;
        };
        let fetching = update.phase == "获取版本化行情";
        if fetching {
            job.fetched_series = job.fetched_series.max(update.done_pairs);
            job.total_series = update.total_pairs;
            job.progress = update
                .done_pairs
                .saturating_mul(25)
                .checked_div(update.total_pairs)
                .unwrap_or(0)
                .min(25) as u8;
        } else {
            job.done_pairs = update.done_pairs;
            job.total_pairs = update.total_pairs;
            job.progress = (25
                + update
                    .done_pairs
                    .saturating_mul(70)
                    .checked_div(update.total_pairs)
                    .unwrap_or(0)
                    .min(70)) as u8;
        }
        job.phase = update.phase;
        job.current_pair = update.current_pair;
        job.effective_observations = update.effective_observations;
        job.updated_at = now;
        let (completed, total) = if fetching {
            (job.fetched_series, job.total_series)
        } else {
            (job.done_pairs, job.total_pairs)
        };
        job.estimated_remaining_seconds = if completed > 0 && total > completed {
            let elapsed = (now - job.started_at).max(1) as u64;
            Some(elapsed.saturating_mul((total - completed) as u64) / completed as u64)
        } else {
            None
        };
        if job.recent_logs.last() != Some(&update.message) {
            job.recent_logs.push(update.message);
            if job.recent_logs.len() > 80 {
                job.recent_logs.drain(0..job.recent_logs.len() - 80);
            }
        }
    });
    let cancel_token = token.clone();
    let cancellation: CancellationCheck = Arc::new(move || cancel_token.is_cancelled());
    let outcome = run_research(&market, &storage, config, progress, cancellation).await;
    let terminal = {
        let mut guard = state.jobs.lock().expect("quant jobs poisoned");
        let Some(job) = guard.get_mut(&job_id) else {
            return;
        };
        job.running = false;
        job.updated_at = now_secs();
        job.estimated_remaining_seconds = Some(0);
        match outcome {
            Ok(result) => match serde_json::to_value(&result) {
                Ok(value) => {
                    job.status = "completed".into();
                    job.phase = "研究完成，可展开查看全部检验与稳健性切片".into();
                    job.progress = 100;
                    job.done_pairs = result.results.len();
                    job.total_pairs = result.budget.executed_pairs;
                    job.recent_logs.push(format!(
                        "快照 {} 已保存，共 {} 个有效关系",
                        result.snapshot_id,
                        result.results.len()
                    ));
                    job.result = Some(value);
                }
                Err(error) => terminal_failure(job, format!("研究结果序列化失败：{error}")),
            },
            Err(_) if token.is_cancelled() => {
                job.status = "cancelled".into();
                job.phase = "已由用户取消，已完成的上游缓存仍保留".into();
                job.error = Some("用户取消".into());
                job.recent_logs.push("任务已安全取消".into());
            }
            Err(error) => terminal_failure(job, error),
        }
        job.clone()
    };
    state
        .cancels
        .lock()
        .expect("quant cancels poisoned")
        .remove(&job_id);
    if let Err(error) = persist_job(&storage, &terminal, result_snapshot_id(&terminal)).await {
        tracing::error!(%error, job_id, "persist terminal quant state failed");
    }
}

async fn run_research(
    market: &MarketData,
    storage: &Storage,
    config: ResearchConfig,
    progress: ProgressReporter,
    cancelled: CancellationCheck,
) -> Result<ResearchSnapshot, String> {
    let adjust = match config.adjust.as_str() {
        "qfq" => Adjust::Qfq,
        "hfq" => Adjust::Hfq,
        "none" => Adjust::None,
        _ => return Err("复权方式只能是 qfq/hfq/none".into()),
    };
    let adjust_token = config.adjust.clone();
    let lookback_bars = config.lookback_bars;
    let symbols = config
        .symbols
        .iter()
        .map(|raw| Symbol::new(raw).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let total = symbols.len();
    let completed = Arc::new(AtomicUsize::new(0));
    let fetched = stream::iter(symbols)
        .map(|symbol| {
            let progress = Arc::clone(&progress);
            let cancelled = Arc::clone(&cancelled);
            let completed = Arc::clone(&completed);
            let adjust_token = adjust_token.clone();
            async move {
                if cancelled() {
                    return Err(format!("{}：研究已由用户取消", symbol.code()));
                }
                progress(ResearchProgress {
                    phase: "获取版本化行情".into(),
                    done_pairs: completed.load(Ordering::Relaxed),
                    total_pairs: total,
                    current_pair: Some([symbol.code().into(), "行情数据".into()]),
                    effective_observations: 0,
                    message: format!("正在获取 {} 的日线与来源版本", symbol.code()),
                });
                let response = market
                    .kline(&symbol, KlinePeriod::Day, adjust, lookback_bars)
                    .await
                    .map_err(|error| format!("{}：{error}", symbol.code()))?;
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                progress(ResearchProgress {
                    phase: "获取版本化行情".into(),
                    done_pairs: done,
                    total_pairs: total,
                    current_pair: Some([symbol.code().into(), "行情数据".into()]),
                    effective_observations: response.data.len(),
                    message: format!(
                        "{} 行情获取完成：{} 根日线，来源 {}",
                        symbol.code(),
                        response.data.len(),
                        response.source
                    ),
                });
                let mut digest = Sha256::new();
                for bar in &response.data {
                    digest.update(bar.date.to_string().as_bytes());
                    digest.update(bar.close.to_bits().to_le_bytes());
                }
                let first = response.data.first().map(|bar| bar.date.to_string());
                let last = response.data.last().map(|bar| bar.date.to_string());
                let data_version = format!(
                    "{}:{}:{}:{}:{}:{:x}",
                    response.source,
                    adjust_token,
                    response.data.len(),
                    first.as_deref().unwrap_or("missing"),
                    last.as_deref().unwrap_or("missing"),
                    digest.finalize()
                );
                Ok(SeriesInput {
                    symbol: symbol.code().into(),
                    dates: response
                        .data
                        .iter()
                        .map(|bar| bar.date.to_string())
                        .collect(),
                    values: response.data.iter().map(|bar| bar.close).collect(),
                    data_version,
                })
            }
        })
        .buffer_unordered(4)
        .collect::<Vec<Result<SeriesInput, String>>>()
        .await;
    let mut inputs = Vec::with_capacity(total);
    let mut errors = Vec::new();
    for item in fetched {
        match item {
            Ok(input) => inputs.push(input),
            Err(error) => errors.push(error),
        }
    }
    if !errors.is_empty() {
        return Err(format!(
            "配置的证券序列未全部获取成功，已阻止不完整研究：{}",
            errors.join("；")
        ));
    }
    inputs.sort_by(|left, right| left.symbol.cmp(&right.symbol));
    let run_config = config.clone();
    let run_progress = Arc::clone(&progress);
    let run_cancelled = Arc::clone(&cancelled);
    let snapshot = tokio::task::spawn_blocking(move || {
        astock_quant::research::run_research_with_hooks(
            &inputs,
            &run_config,
            |update| run_progress(update),
            || run_cancelled(),
        )
    })
    .await
    .map_err(|error| format!("量化研究工作线程异常：{error}"))?
    .map_err(|error| error.to_string())?;
    storage
        .quant_research_snapshot_put(snapshot_row(&snapshot)?)
        .await
        .map_err(|error| error.to_string())?;
    Ok(snapshot)
}

pub async fn snapshot_get(
    storage: &Storage,
    snapshot_id: &str,
) -> Result<Option<ResearchSnapshot>, String> {
    let snapshot_id = validate_id(snapshot_id, "snapshot_id")?;
    storage
        .quant_research_snapshot_get(snapshot_id)
        .await
        .map_err(|error| error.to_string())?
        .map(|row| {
            serde_json::from_str(&row.snapshot_json)
                .map_err(|error| format!("量化快照内容损坏：{error}"))
        })
        .transpose()
}

pub async fn snapshot_list(storage: &Storage, limit: Option<usize>) -> Result<Value, String> {
    let rows = storage
        .quant_research_snapshot_list(limit.unwrap_or(20))
        .await
        .map_err(|error| error.to_string())?;
    let mut values = Vec::with_capacity(rows.len());
    for row in rows {
        let symbols: Value = serde_json::from_str(&row.symbols_json)
            .map_err(|error| format!("快照 {} 的证券列表损坏：{error}", row.snapshot_id))?;
        let data_versions: Value = serde_json::from_str(&row.data_versions_json)
            .map_err(|error| format!("快照 {} 的数据版本损坏：{error}", row.snapshot_id))?;
        let config: Value = serde_json::from_str(&row.config_json)
            .map_err(|error| format!("快照 {} 的配置损坏：{error}", row.snapshot_id))?;
        values.push(json!({
            "snapshot_id": row.snapshot_id,
            "function_version": row.function_version,
            "metric": row.metric,
            "symbols": symbols,
            "data_versions": data_versions,
            "config": config,
            "created_at": row.created_at,
        }));
    }
    Ok(Value::Array(values))
}

fn normalize_config(config: &mut ResearchConfig) -> Result<(), String> {
    if config.symbols.len() < 2 || config.symbols.len() > 50 {
        return Err("量化研究股票池需包含 2-50 个证券代码".into());
    }
    let mut normalized = Vec::with_capacity(config.symbols.len());
    let mut unique = HashSet::new();
    for raw in &config.symbols {
        let symbol = Symbol::new(raw.trim()).map_err(|error| error.to_string())?;
        if !unique.insert(symbol.code().to_string()) {
            return Err(format!("股票池中存在重复代码：{}", symbol.code()));
        }
        normalized.push(symbol.code().to_string());
    }
    config.symbols = normalized;
    for control in &mut config.controls {
        *control = Symbol::new(control.trim())
            .map_err(|error| error.to_string())?
            .code()
            .to_string();
        if !unique.contains(control) {
            return Err(format!(
                "控制变量 {control} 不在本次股票池中，无法获得同版本序列"
            ));
        }
    }
    if let Some(start) = config.start_date.as_deref() {
        chrono::NaiveDate::parse_from_str(start, "%Y-%m-%d")
            .map_err(|_| "start_date 必须是 YYYY-MM-DD".to_string())?;
    }
    if let Some(end) = config.end_date.as_deref() {
        chrono::NaiveDate::parse_from_str(end, "%Y-%m-%d")
            .map_err(|_| "end_date 必须是 YYYY-MM-DD".to_string())?;
    }
    if config.start_date > config.end_date && config.end_date.is_some() {
        return Err("start_date 不能晚于 end_date".into());
    }
    config.adjust = config.adjust.trim().to_ascii_lowercase();
    if !matches!(config.adjust.as_str(), "qfq" | "hfq" | "none") {
        return Err("adjust 只能是 qfq/hfq/none".into());
    }
    if config.lookback_bars < 60 || config.lookback_bars > 2_000 {
        return Err("lookback_bars 必须在 60-2000 之间".into());
    }
    if config.bootstrap_reps > 10_000
        || config.permutation_reps > 10_000
        || config.max_pairs > 10_000
        || config.max_observations_per_pair > 5_000
    {
        return Err("统计预算超过桌面研究安全上限".into());
    }
    Ok(())
}

fn terminal_failure(job: &mut QuantResearchJobSnapshot, error: String) {
    job.status = "failed".into();
    job.phase = "研究失败，可复制错误用于诊断".into();
    job.error = Some(error.clone());
    job.recent_logs.push(format!("错误：{error}"));
}

fn snapshot_row(snapshot: &ResearchSnapshot) -> Result<QuantResearchSnapshotRow, String> {
    let metric = serde_json::to_value(snapshot.config.metric)
        .map_err(|error| error.to_string())?
        .as_str()
        .ok_or_else(|| "量化指标序列化结果不是字符串".to_string())?
        .to_string();
    Ok(QuantResearchSnapshotRow {
        snapshot_id: snapshot.snapshot_id.clone(),
        function_version: snapshot.function_version.clone(),
        metric,
        symbols_json: serde_json::to_string(&snapshot.config.symbols)
            .map_err(|error| error.to_string())?,
        data_versions_json: serde_json::to_string(&snapshot.data_versions)
            .map_err(|error| error.to_string())?,
        config_json: serde_json::to_string(&snapshot.config).map_err(|error| error.to_string())?,
        snapshot_json: serde_json::to_string(snapshot).map_err(|error| error.to_string())?,
        created_at: snapshot.created_at,
    })
}

async fn persist_job(
    storage: &Storage,
    job: &QuantResearchJobSnapshot,
    snapshot_id: Option<String>,
) -> Result<(), String> {
    let job = job.clone();
    let progress_json = serde_json::to_string(&job).map_err(|error| error.to_string())?;
    storage
        .run(move |connection| {
            connection.execute(
                "INSERT INTO quant_research_jobs
                 (job_id,status,phase,progress_json,snapshot_id,error,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(job_id) DO UPDATE SET
                   status=excluded.status,phase=excluded.phase,progress_json=excluded.progress_json,
                   snapshot_id=excluded.snapshot_id,error=excluded.error,updated_at=excluded.updated_at",
                rusqlite::params![
                    job.job_id,
                    job.status,
                    job.phase,
                    progress_json,
                    snapshot_id,
                    job.error,
                    job.started_at,
                    job.updated_at,
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())
}

fn result_snapshot_id(job: &QuantResearchJobSnapshot) -> Option<String> {
    job.result
        .as_ref()
        .and_then(|value| value.get("snapshot_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn validate_id<'a>(raw: &'a str, field: &str) -> Result<&'a str, String> {
    let value = raw.trim();
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(format!("{field} 为空、过长或包含控制字符"));
    }
    Ok(value)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_rejects_duplicates_and_missing_control_series() {
        let mut duplicate = ResearchConfig {
            symbols: vec!["300308".into(), "300308".into()],
            ..Default::default()
        };
        assert!(normalize_config(&mut duplicate).is_err());

        let mut missing_control = ResearchConfig {
            symbols: vec!["300308".into(), "000300".into()],
            controls: vec!["000001".into()],
            ..Default::default()
        };
        assert!(normalize_config(&mut missing_control).is_err());
    }

    #[test]
    fn corrupt_identifiers_and_unbounded_budgets_are_rejected() {
        assert!(validate_id("snapshot\nforged", "snapshot_id").is_err());
        let mut config = ResearchConfig {
            symbols: vec!["300308".into(), "000300".into()],
            bootstrap_reps: 10_001,
            ..Default::default()
        };
        assert!(normalize_config(&mut config).is_err());
    }

    #[tokio::test]
    async fn restore_reconciles_running_jobs_and_surfaces_corrupt_rows() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(astock_storage::StorageConfig::with_base_dir(dir.path()))
            .expect("open isolated quant storage");
        let running = QuantResearchJobSnapshot {
            job_id: "quant-running".into(),
            running: true,
            status: "running".into(),
            phase: "计算中".into(),
            progress: 40,
            done_pairs: 2,
            total_pairs: 5,
            current_pair: None,
            effective_observations: 100,
            fetched_series: 2,
            total_series: 2,
            estimated_remaining_seconds: Some(5),
            recent_logs: Vec::new(),
            result: None,
            error: None,
            started_at: 10,
            updated_at: 11,
        };
        persist_job(&storage, &running, None).await.unwrap();
        storage
            .run(|connection| {
                connection.execute(
                    "INSERT INTO quant_research_jobs
                     (job_id,status,phase,progress_json,error,created_at,updated_at)
                     VALUES ('quant-corrupt','failed','损坏','<not-json>','storage_corrupt',12,13)",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let restored = QuantResearchService::restore(&storage).await.unwrap();
        let interrupted = restored.status(Some("quant-running")).unwrap().unwrap();
        assert_eq!(interrupted.status, "suspended");
        assert!(!interrupted.running);
        let corrupt = restored.status(Some("quant-corrupt")).unwrap().unwrap();
        assert_eq!(corrupt.error.as_deref(), Some("storage_corrupt"));
    }
}
