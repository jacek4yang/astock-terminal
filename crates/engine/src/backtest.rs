//! Persistent, cancellable historical backtests over versioned market data.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use astock_backtest::{
    data::PriceSeries,
    engine::{BacktestEngine, EngineConfig},
    metrics::{self, MetricsConfig},
    strategies::{
        formula::{FormulaStrategy, FormulaStrategySpec},
        min_corr_rotation::{run_rotation, MinCorrRotation, RotationConfig},
        strategy_meta,
        zscore_mean_reversion::ZscoreMeanReversion,
    },
    strategy::{BuyHold, MaCross, Strategy, TurtleBreakout},
};
use astock_core::{Adjust, KlinePeriod, Symbol};
use astock_market_data::{DataProvider, MarketData};
use astock_storage::Storage;
use astock_trading_rules::{RuleSet, TradeSide};
use futures::future::try_join_all;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

const INITIAL_CASH: f64 = 1_000_000.0;
const MIN_BARS: usize = 60;
const MAX_BARS: u32 = 2_000;
const TRADE_TAIL: usize = 50;
static JOB_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacktestRequest {
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub strategy: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub pool: Option<Vec<String>>,
    #[serde(default)]
    pub fast: Option<u32>,
    #[serde(default)]
    pub slow: Option<u32>,
    #[serde(default)]
    pub entry_n: Option<u32>,
    #[serde(default)]
    pub exit_n: Option<u32>,
    #[serde(default)]
    pub bars: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestJobSnapshot {
    pub job_id: Option<String>,
    pub status: String,
    pub phase: String,
    pub progress: Option<u8>,
    pub started_at: Option<u128>,
    pub updated_at: u128,
    pub result: Option<Value>,
    pub error: Option<String>,
}

impl Default for BacktestJobSnapshot {
    fn default() -> Self {
        Self {
            job_id: None,
            status: "idle".into(),
            phase: "尚未运行".into(),
            progress: None,
            started_at: None,
            updated_at: now_millis(),
            result: None,
            error: None,
        }
    }
}

#[derive(Default)]
struct BacktestState {
    snapshot: Mutex<BacktestJobSnapshot>,
    cancel: Mutex<Option<CancellationToken>>,
}

#[derive(Clone, Default)]
pub struct BacktestService {
    inner: Arc<BacktestState>,
}

impl BacktestService {
    pub async fn restore(storage: &Storage) -> Result<Self, String> {
        storage
            .run(|connection| {
                connection.execute_batch(
                    "CREATE TABLE IF NOT EXISTS backtest_jobs(
                       job_id TEXT PRIMARY KEY,
                       snapshot_json TEXT NOT NULL,
                       status TEXT NOT NULL,
                       created_at INTEGER NOT NULL,
                       updated_at INTEGER NOT NULL
                     );
                     CREATE INDEX IF NOT EXISTS idx_backtest_jobs_updated
                       ON backtest_jobs(updated_at DESC);",
                )?;
                Ok(())
            })
            .await
            .map_err(|error| error.to_string())?;
        let encoded = storage
            .run(|connection| {
                Ok(connection
                    .query_row(
                        "SELECT snapshot_json FROM backtest_jobs ORDER BY updated_at DESC LIMIT 1",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?)
            })
            .await
            .map_err(|error| error.to_string())?;
        let service = Self::default();
        if let Some(encoded) = encoded {
            let mut snapshot = serde_json::from_str::<BacktestJobSnapshot>(&encoded)
                .unwrap_or_else(|error| BacktestJobSnapshot {
                    status: "failed".into(),
                    phase: "历史回测状态损坏；原始数据库记录未被覆盖".into(),
                    error: Some(format!("storage_corrupt: {error}")),
                    ..Default::default()
                });
            if snapshot.status == "running" {
                snapshot.status = "suspended".into();
                snapshot.phase = "应用重启中断了回测；行情缓存保留，可重新发起".into();
                snapshot.progress = None;
                snapshot.error = Some("worker_restart".into());
                snapshot.updated_at = now_millis();
                persist(storage, &snapshot).await?;
            }
            *service
                .inner
                .snapshot
                .lock()
                .expect("backtest state poisoned") = snapshot;
        }
        Ok(service)
    }

    pub async fn start(
        &self,
        market: Arc<MarketData>,
        rules: RuleSet,
        storage: Storage,
        request: BacktestRequest,
    ) -> Result<Value, String> {
        validate_request(&request)?;
        let job_id = format!(
            "backtest-{}-{}",
            now_millis(),
            JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let snapshot = BacktestJobSnapshot {
            job_id: Some(job_id.clone()),
            status: "running".into(),
            phase: "校验参数并加载版本化历史行情".into(),
            progress: Some(0),
            started_at: Some(now_millis()),
            updated_at: now_millis(),
            result: None,
            error: None,
        };
        {
            let mut current = self.inner.snapshot.lock().expect("backtest state poisoned");
            if current.status == "running" {
                return Err("已有回测后台任务正在运行，请先等待或取消".into());
            }
            *current = snapshot.clone();
        }
        persist(&storage, &snapshot).await?;
        let token = CancellationToken::new();
        *self.inner.cancel.lock().expect("backtest cancel poisoned") = Some(token.clone());
        let state = Arc::clone(&self.inner);
        let spawned_id = job_id.clone();
        tokio::spawn(async move {
            let outcome = tokio::select! {
                _ = token.cancelled() => None,
                result = run(&market, &rules, request) => Some(result),
            };
            let terminal = {
                let mut current = state.snapshot.lock().expect("backtest state poisoned");
                if current.job_id.as_deref() != Some(spawned_id.as_str()) {
                    return;
                }
                current.updated_at = now_millis();
                match outcome {
                    None => {
                        current.status = "cancelled".into();
                        current.phase = "已安全取消".into();
                        current.progress = None;
                        current.error = None;
                    }
                    Some(Ok(result)) => {
                        current.status = "completed".into();
                        current.phase = "回测与绩效统计完成".into();
                        current.progress = Some(100);
                        current.result = Some(result);
                        current.error = None;
                    }
                    Some(Err(error)) => {
                        current.status = "failed".into();
                        current.phase = "回测失败；未发布不完整结果".into();
                        current.progress = None;
                        current.error = Some(error);
                    }
                }
                current.clone()
            };
            *state.cancel.lock().expect("backtest cancel poisoned") = None;
            if let Err(error) = persist(&storage, &terminal).await {
                tracing::error!(%error, job_id = spawned_id, "persist terminal backtest failed");
            }
        });
        Ok(json!({"job_id": job_id, "started": true}))
    }

    pub fn status(&self) -> BacktestJobSnapshot {
        self.inner
            .snapshot
            .lock()
            .expect("backtest state poisoned")
            .clone()
    }

    pub fn cancel(&self) -> bool {
        let token = self
            .inner
            .cancel
            .lock()
            .expect("backtest cancel poisoned")
            .clone();
        if let Some(token) = token {
            token.cancel();
            true
        } else {
            false
        }
    }
}

pub fn strategies() -> Value {
    let mut values = serde_json::to_value(strategy_meta())
        .unwrap_or_else(|_| Value::Array(Vec::new()))
        .as_array()
        .cloned()
        .unwrap_or_default();
    values.push(json!({
        "name": "turtle", "kind": "single", "multi_symbol": false,
        "description": "海龟突破：前期高点入场、前期低点或 2N 风险线退出",
        "params": [
            {"name":"entry_n","ty":"int","default":20,"description":"突破窗口，>=2"},
            {"name":"exit_n","ty":"int","default":10,"description":"离场窗口，>=1"}
        ]
    }));
    values.push(json!({
        "name": "buy_hold", "kind": "single", "multi_symbol": false,
        "description": "首个可执行交易日买入并持有，作为策略比较基准", "params": []
    }));
    for value in &mut values {
        if let Some(object) = value.as_object_mut() {
            let multi_symbol = object.get("kind").and_then(Value::as_str) == Some("rotation");
            object
                .entry("multi_symbol")
                .or_insert(Value::Bool(multi_symbol));
        }
    }
    Value::Array(values)
}

pub async fn run(
    market: &MarketData,
    rules: &RuleSet,
    request: BacktestRequest,
) -> Result<Value, String> {
    validate_request(&request)?;
    let name = request
        .strategy
        .as_deref()
        .unwrap_or("ma_cross")
        .trim()
        .to_ascii_lowercase();
    if name == "min_corr_etf_rotation" {
        return run_rotation_backtest(market, rules, request).await;
    }
    run_single(market, rules, request, &name).await
}

async fn run_single(
    market: &MarketData,
    rules: &RuleSet,
    request: BacktestRequest,
    name: &str,
) -> Result<Value, String> {
    let symbol = Symbol::new(
        request
            .symbol
            .as_deref()
            .ok_or_else(|| "单标的回测必须提供 symbol".to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let bars_requested = request.bars.unwrap_or(750).clamp(MIN_BARS as u32, MAX_BARS);
    let fetched = market
        .kline(&symbol, KlinePeriod::Day, Adjust::Qfq, bars_requested)
        .await
        .map_err(|error| error.to_string())?;
    if fetched.data.len() < MIN_BARS {
        return Err(format!(
            "{} K线数据不足：仅 {} 根，至少需要 {MIN_BARS} 根",
            symbol.code(),
            fetched.data.len()
        ));
    }
    let series = PriceSeries::new(
        symbol.code(),
        fetched
            .data
            .iter()
            .map(astock_backtest::data::Bar::from)
            .collect(),
    )
    .map_err(|error| error.to_string())?;
    let params = object_params(&request.params)?;
    let (mut strategy, audited_params): (Box<dyn Strategy>, Value) = match name {
        "ma_cross" | "ma" => {
            reject_unknown(&params, &["fast", "slow"])?;
            let fast = match request.fast {
                Some(value) => value,
                None => uint_param(&params, "fast")?.unwrap_or(5),
            } as usize;
            let slow = match request.slow {
                Some(value) => value,
                None => uint_param(&params, "slow")?.unwrap_or(60),
            } as usize;
            if fast == 0 || fast >= slow {
                return Err(format!("ma_cross 需要 1 <= fast({fast}) < slow({slow})"));
            }
            (
                Box::new(MaCross::new(fast, slow)),
                json!({"fast":fast,"slow":slow}),
            )
        }
        "turtle" | "turtle_breakout" => {
            reject_unknown(&params, &["entry_n", "exit_n"])?;
            let entry = match request.entry_n {
                Some(value) => value,
                None => uint_param(&params, "entry_n")?.unwrap_or(20),
            } as usize;
            let exit = match request.exit_n {
                Some(value) => value,
                None => uint_param(&params, "exit_n")?.unwrap_or(10),
            } as usize;
            if entry < 2 || exit < 1 {
                return Err(format!(
                    "turtle 需要 entry_n({entry}) >= 2 且 exit_n({exit}) >= 1"
                ));
            }
            (
                Box::new(TurtleBreakout::new(entry, exit)),
                json!({"entry_n":entry,"exit_n":exit}),
            )
        }
        "buy_hold" | "buyhold" => {
            reject_unknown(&params, &[])?;
            (Box::new(BuyHold), json!({}))
        }
        "zscore_mean_reversion" => {
            reject_unknown(&params, &["ma_window", "z_window", "entry_z", "exit_z"])?;
            let ma = uint_param(&params, "ma_window")?.unwrap_or(20) as usize;
            let z = uint_param(&params, "z_window")?.unwrap_or(60) as usize;
            let entry = number_param(&params, "entry_z")?.unwrap_or(-2.0);
            let exit = number_param(&params, "exit_z")?.unwrap_or(1.0);
            let strategy =
                ZscoreMeanReversion::try_new(ma, z, entry, exit).map_err(|e| e.to_string())?;
            (
                Box::new(strategy),
                json!({"ma_window":ma,"z_window":z,"entry_z":entry,"exit_z":exit}),
            )
        }
        "formula_dsl" | "formula" => {
            let raw = request
                .params
                .ok_or_else(|| "formula_dsl 必须提供完整策略定义".to_string())?;
            let spec: FormulaStrategySpec =
                serde_json::from_value(raw).map_err(|e| format!("公式策略定义无效：{e}"))?;
            let strategy = FormulaStrategy::try_new(spec).map_err(|e| e.to_string())?;
            let audited = serde_json::to_value(strategy.spec()).map_err(|e| e.to_string())?;
            (Box::new(strategy), audited)
        }
        other => return Err(format!("未知回测策略：{other}")),
    };
    let engine = BacktestEngine::new(
        rules.clone(),
        EngineConfig::new(symbol.code(), INITIAL_CASH),
    )
    .map_err(|error| error.to_string())?;
    let result = engine
        .run(&series, strategy.as_mut())
        .map_err(|error| error.to_string())?;
    let report = result
        .performance_report(None, &MetricsConfig::default())
        .ok_or_else(|| "回测区间过短，无法生成绩效报告".to_string())?;
    let version = version_id(&fetched.source.to_string(), &fetched.data);
    let trades = result.trades[result.trades.len().saturating_sub(TRADE_TAIL)..]
        .iter()
        .map(|trade| {
            json!({
                "date": trade.date.to_string(),
                "side": if trade.side == TradeSide::Buy {"buy"} else {"sell"},
                "shares": trade.shares, "price": r2(trade.price), "amount": r2(trade.amount),
                "fees": r2(trade.fees.total), "reason": trade.reason,
            })
        })
        .collect::<Vec<_>>();
    let equity_curve = result
        .equity
        .iter()
        .map(|point| json!([point.date.to_string(), r2(point.equity)]))
        .collect::<Vec<_>>();
    Ok(json!({
        "kind":"single", "symbol":symbol.code(), "strategy":strategy.name(), "params":audited_params,
        "data":{"start":report.start.to_string(),"end":report.end.to_string(),"bars":series.len(),"adjust":"qfq"},
        "source":fetched.source.to_string(), "fetched_at":fetched.fetched_at, "source_version_id":version,
        "verification_status":"source_versioned", "initial_cash":INITIAL_CASH,
        "final_equity":r2(result.final_equity()), "total_return":r4(report.total_return), "cagr":r4(report.cagr),
        "annualized_volatility":r4(report.annualized_volatility), "sharpe":r4(report.sharpe),
        "sortino":r4(report.sortino), "calmar":r4(report.calmar), "max_drawdown":r4(report.max_drawdown),
        "max_drawdown_duration_bars":report.max_drawdown_duration_bars, "round_trips":report.round_trips,
        "hit_rate":r4(report.hit_rate), "payoff_ratio":r4(report.payoff_ratio), "profit_factor":r4(report.profit_factor),
        "trades_count":result.trades.len(), "rejections":result.rejections.len(), "fees_total":r2(result.total_fees()),
        "equity_curve":equity_curve, "trades_tail":trades,
        "note":"单组参数的历史回测不代表未来收益；未做参数网格与过拟合检验",
    }))
}

async fn run_rotation_backtest(
    market: &MarketData,
    rules: &RuleSet,
    request: BacktestRequest,
) -> Result<Value, String> {
    let params = object_params(&request.params)?;
    reject_unknown(&params, &["lookback", "hold_n"])?;
    let lookback = uint_param(&params, "lookback")?.unwrap_or(60) as usize;
    let hold_n = uint_param(&params, "hold_n")?.unwrap_or(4) as usize;
    let strategy = MinCorrRotation::try_new(lookback, hold_n).map_err(|error| error.to_string())?;
    let mut symbols = Vec::new();
    for raw in request
        .pool
        .ok_or_else(|| "轮动策略必须提供 pool".to_string())?
    {
        let symbol = Symbol::new(raw.trim()).map_err(|error| error.to_string())?;
        if !symbols.contains(&symbol) {
            symbols.push(symbol);
        }
    }
    if !(2..=20).contains(&symbols.len()) {
        return Err(format!(
            "轮动池需包含 2-20 个不同证券，收到 {} 个",
            symbols.len()
        ));
    }
    let requested = request.bars.unwrap_or(750).clamp(MIN_BARS as u32, MAX_BARS);
    let fetched = try_join_all(symbols.iter().map(|symbol| async move {
        market
            .kline(symbol, KlinePeriod::Day, Adjust::Qfq, requested)
            .await
    }))
    .await
    .map_err(|error| format!("轮动池未全部获取成功，已阻止不完整回测：{error}"))?;
    let required = MIN_BARS.max(lookback + 2);
    let mut series = Vec::with_capacity(symbols.len());
    let mut versions = HashMap::new();
    for (symbol, item) in symbols.iter().zip(&fetched) {
        if item.data.len() < required {
            return Err(format!(
                "{} 仅有 {} 根K线，至少需要 {required} 根",
                symbol.code(),
                item.data.len()
            ));
        }
        versions.insert(
            symbol.code().to_string(),
            version_id(&item.source.to_string(), &item.data),
        );
        series.push(
            PriceSeries::new(
                symbol.code(),
                item.data
                    .iter()
                    .map(astock_backtest::data::Bar::from)
                    .collect(),
            )
            .map_err(|e| e.to_string())?,
        );
    }
    let result = run_rotation(
        &series,
        &strategy,
        rules,
        &RotationConfig::new(INITIAL_CASH),
    )
    .map_err(|e| e.to_string())?;
    let curve = result.equity_curve();
    if curve.len() < 2 {
        return Err("共同交易日不足，无法计算绩效".into());
    }
    let first = result.equity.first().expect("checked curve").date;
    let last = result.equity.last().expect("checked curve").date;
    let years = ((last - first).num_days() as f64 / 365.25).max(1.0 / 365.25);
    let returns = metrics::daily_returns(&curve);
    let config = MetricsConfig::default();
    let total_return = metrics::total_return(&curve);
    let cagr = metrics::cagr(curve[0], *curve.last().expect("checked curve"), years);
    let (max_drawdown, duration) = metrics::max_drawdown(&curve);
    let trades = result.trades[result.trades.len().saturating_sub(TRADE_TAIL)..].iter().map(|trade| json!({
        "date":trade.date.to_string(), "symbol":trade.symbol,
        "side":if trade.side == TradeSide::Buy {"buy"} else {"sell"}, "shares":trade.shares,
        "price":r2(trade.price), "amount":r2(trade.amount), "fees":r2(trade.fees.total), "reason":trade.reason,
    })).collect::<Vec<_>>();
    let equity_curve = result
        .equity
        .iter()
        .map(|point| json!([point.date.to_string(), r2(point.equity)]))
        .collect::<Vec<_>>();
    let fees: f64 = result.trades.iter().map(|trade| trade.fees.total).sum();
    Ok(json!({
        "kind":"rotation", "strategy":"min_corr_etf_rotation", "pool":symbols.iter().map(|s|s.code()).collect::<Vec<_>>(),
        "params":{"lookback":lookback,"hold_n":hold_n}, "data":{"start":first.to_string(),"end":last.to_string(),"bars":curve.len(),"adjust":"qfq"},
        "sources":fetched.iter().map(|item| item.source.to_string()).collect::<Vec<_>>(), "source_version_ids":versions,
        "verification_status":"all_pool_series_versioned", "initial_cash":INITIAL_CASH, "final_equity":r2(result.final_equity()),
        "total_return":r4(total_return), "cagr":r4(cagr), "annualized_volatility":r4(metrics::annualized_volatility(&returns,&config)),
        "sharpe":r4(metrics::sharpe(&returns,&config)), "sortino":r4(metrics::sortino(&returns,&config)),
        "calmar":r4(metrics::calmar(cagr,max_drawdown)), "max_drawdown":r4(max_drawdown), "max_drawdown_duration_bars":duration,
        "trades_count":result.trades.len(), "fees_total":r2(fees), "equity_curve":equity_curve, "trades_tail":trades,
        "note":"单组参数的历史回测不代表未来收益；池内序列按共同交易日对齐，未做过拟合检验",
    }))
}

fn validate_request(request: &BacktestRequest) -> Result<(), String> {
    if let Some(bars) = request.bars {
        if !(MIN_BARS as u32..=MAX_BARS).contains(&bars) {
            return Err(format!("bars 必须在 {MIN_BARS}-{MAX_BARS} 之间"));
        }
    }
    Ok(())
}

fn object_params(value: &Option<Value>) -> Result<Map<String, Value>, String> {
    match value {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(object)) => Ok(object.clone()),
        Some(_) => Err("params 必须是 JSON 对象".into()),
    }
}

fn reject_unknown(params: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    if let Some(key) = params.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!(
            "策略不支持参数 `{key}`；允许：{}",
            allowed.join("/")
        ));
    }
    Ok(())
}

fn uint_param(params: &Map<String, Value>, key: &str) -> Result<Option<u32>, String> {
    let Some(value) = params.get(key) else {
        return Ok(None);
    };
    let number = value
        .as_u64()
        .filter(|value| *value <= u32::MAX as u64)
        .ok_or_else(|| format!("params.{key} 必须是非负整数"))?;
    Ok(Some(number as u32))
}

fn number_param(params: &Map<String, Value>, key: &str) -> Result<Option<f64>, String> {
    let Some(value) = params.get(key) else {
        return Ok(None);
    };
    let number = value
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| format!("params.{key} 必须是有限数字"))?;
    Ok(Some(number))
}

fn version_id(source: &str, bars: &[astock_core::Bar]) -> String {
    let mut digest = Sha256::new();
    digest.update(source.as_bytes());
    for bar in bars {
        digest.update(bar.date.to_string().as_bytes());
        digest.update(bar.open.to_bits().to_le_bytes());
        digest.update(bar.high.to_bits().to_le_bytes());
        digest.update(bar.low.to_bits().to_le_bytes());
        digest.update(bar.close.to_bits().to_le_bytes());
        digest.update(bar.volume.to_bits().to_le_bytes());
    }
    format!("backtest:{:x}", digest.finalize())
}

async fn persist(storage: &Storage, snapshot: &BacktestJobSnapshot) -> Result<(), String> {
    let snapshot = snapshot.clone();
    let encoded = serde_json::to_string(&snapshot).map_err(|error| error.to_string())?;
    let job_id = snapshot.job_id.clone().unwrap_or_else(|| "unknown".into());
    storage
        .run(move |connection| {
            connection.execute(
                "INSERT INTO backtest_jobs(job_id,snapshot_json,status,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(job_id) DO UPDATE SET snapshot_json=excluded.snapshot_json,
               status=excluded.status,updated_at=excluded.updated_at",
                rusqlite::params![
                    job_id,
                    encoded,
                    snapshot.status,
                    snapshot.started_at.unwrap_or(snapshot.updated_at) as i64,
                    snapshot.updated_at as i64
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())
}

fn r2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
fn r4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}
fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or(0)
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_and_non_finite_parameters() {
        let params = serde_json::from_value::<BacktestRequest>(json!({
            "symbol":"300308", "strategy":"ma_cross", "params":{"fasst":5}
        }))
        .unwrap();
        let object = object_params(&params.params).unwrap();
        assert!(reject_unknown(&object, &["fast", "slow"]).is_err());
        assert!(number_param(&serde_json::from_value(json!({"x":null})).unwrap(), "x").is_err());
    }

    #[tokio::test]
    async fn restore_marks_interrupted_job_suspended() {
        let dir = tempfile::tempdir().unwrap();
        let storage =
            Storage::open(astock_storage::StorageConfig::with_base_dir(dir.path())).unwrap();
        let service = BacktestService::restore(&storage).await.unwrap();
        let snapshot = BacktestJobSnapshot {
            job_id: Some("backtest-test".into()),
            status: "running".into(),
            phase: "计算".into(),
            progress: Some(30),
            started_at: Some(1),
            updated_at: 2,
            result: None,
            error: None,
        };
        persist(&storage, &snapshot).await.unwrap();
        drop(service);
        let restored = BacktestService::restore(&storage).await.unwrap();
        assert_eq!(restored.status().status, "suspended");
        assert_eq!(restored.status().error.as_deref(), Some("worker_restart"));
    }
}
