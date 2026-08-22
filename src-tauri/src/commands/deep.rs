//! Deep-analysis commands: supply-chain subgraph, event propagation,
//! cross-asset relationship networks, strategy backtests and the market
//! regime snapshot — the engine surfaces the upcoming UI pages build on.
//!
//! The heavy lifting lives in `astock_agent::deep` (shared with the agent
//! tools); this module is a thin argument-validation + error-mapping layer.

use astock_agent::deep::{
    impact_report_json, market_regime_json, relationship_graph_json, run_backtest_json,
};
use astock_agent::AgentError;
use astock_backtest::data::PriceSeries;
use astock_backtest::engine::{BacktestEngine, EngineConfig as BtConfig};
use astock_backtest::metrics::{self, MetricsConfig};
use astock_backtest::strategies::min_corr_rotation::{
    run_rotation, MinCorrRotation, RotationConfig, RotationError,
};
use astock_backtest::strategies::zscore_mean_reversion::ZscoreMeanReversion;
use astock_backtest::strategies::{strategy_meta, ParamError};
use astock_core::{Adjust, KlinePeriod, Symbol};
use astock_graph::{Edge, Engine as GraphEngine, Event, Node, NodeKind, Relation};
use astock_market_data::{DataProvider, MarketData};
use astock_trading_rules::{RuleSet, TradeSide};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tauri::State;
use tokio_util::sync::CancellationToken;

use crate::error::CmdError;
use crate::state::AppState;

/// Current unix time in seconds.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

static BACKTEST_JOB_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn next_backtest_job_id() -> String {
    format!(
        "bt-{}-{}",
        now_millis(),
        BACKTEST_JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

/// Map an agent-layer error onto the command error contract.
fn agent_err(e: AgentError) -> CmdError {
    match e {
        AgentError::InvalidArgs { msg, .. } => CmdError::new("invalid_param", msg),
        other => CmdError::new("engine", other.to_string()),
    }
}

/// Parse a shock direction: up/涨 → +1, down/跌 → −1.
fn parse_direction(raw: &str) -> Result<i8, CmdError> {
    match raw.to_ascii_lowercase().as_str() {
        "up" | "涨" | "上涨" => Ok(1),
        "down" | "跌" | "下跌" => Ok(-1),
        other => Err(CmdError::new(
            "invalid_param",
            format!("direction 只能是 up/down，收到 `{other}`"),
        )),
    }
}

/// Hop-limited neighborhood around a graph node (6-digit code, node id or
/// exact name). Nodes/edges carry kind/relation/weight/confidence/source.
#[tauri::command(rename_all = "snake_case")]
pub async fn graph_subgraph(
    state: State<'_, AppState>,
    symbol_or_node: String,
    hops: Option<u32>,
) -> Result<Value, CmdError> {
    let query = symbol_or_node.trim().to_string();
    if query.is_empty() {
        return Err(CmdError::new(
            "invalid_param",
            "symbol_or_node must not be empty",
        ));
    }
    let hops = hops.unwrap_or(2).clamp(1, 3);
    ensure_graph_company(&state, &query).await?;
    let sub = state.graph.subgraph(&query, hops).await?;
    Ok(json!({
        "center": query,
        "hops": hops,
        "coverage": if sub.edges.is_empty() { "identity_only" } else { "sourced_relations" },
        "coverage_note": if sub.edges.is_empty() {
            "已解析公司身份，但尚无经公开来源验证的产业链关系"
        } else {
            "仅展示带来源与置信度的关系"
        },
        "nodes": sub.nodes,
        "edges": sub.edges,
    }))
}

/// Propagate an event (e.g. 铜 +10%) through the supply-chain graph:
/// 一级受益/受损、二级与潜在映射，每条含逻辑链、滞后与置信度。
#[tauri::command(rename_all = "snake_case")]
pub async fn supply_chain_shock(
    state: State<'_, AppState>,
    subject: String,
    direction: String,
    magnitude_pct: Option<f64>,
) -> Result<Value, CmdError> {
    let subject = subject.trim().to_string();
    if subject.is_empty() {
        return Err(CmdError::new("invalid_param", "subject must not be empty"));
    }
    let direction = parse_direction(&direction)?;
    let word = if direction > 0 { "上涨" } else { "下跌" };
    let title = match magnitude_pct {
        Some(p) => format!("{subject}{word}{}%", p.abs()),
        None => format!("{subject}{word}"),
    };
    let event = Event::new(
        format!("cmd-{}", now_secs()),
        "manual",
        title,
        subject,
        magnitude_pct.map(|p| p.abs() / 100.0),
        direction,
        now_secs(),
    );
    let engine = GraphEngine::new(state.graph.clone());
    let report = engine.propagate(&event).await?;
    Ok(impact_report_json(&report))
}

/// Pairwise Pearson + lead-lag relationship network over daily returns
/// (2–12 symbols; window 60–500 trading days, default 250).
#[tauri::command(rename_all = "snake_case")]
pub async fn relationship_graph(
    state: State<'_, AppState>,
    symbols: Vec<String>,
    window_days: Option<u32>,
) -> Result<Value, CmdError> {
    if symbols.len() < 2 || symbols.len() > 12 {
        return Err(CmdError::new("invalid_param", "symbols 需包含 2-12 个代码"));
    }
    let window = window_days.unwrap_or(250).clamp(60, 500);
    relationship_graph_json(&*state.market, &symbols, window)
        .await
        .map_err(agent_err)
}

/// Registered strategy list with parameter metadata (`name/kind/description/
/// params[{name,ty,default,description}]`), sourced from the backtest
/// registry; the UI renders its strategy form from this.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_strategies() -> Result<Value, CmdError> {
    serde_json::to_value(strategy_meta())
        .map_err(|e| CmdError::new("engine", format!("serialize strategy meta: {e}")))
}

/// Initial cash for every command-driven backtest (same as the agent tool).
const BACKTEST_INITIAL_CASH: f64 = 1_000_000.0;
/// Minimum bars for a meaningful backtest.
const BACKTEST_MIN_BARS: usize = 60;
/// Trade rows kept in the payload (chronological order).
const BACKTEST_TRADES_TAIL: usize = 50;
/// Rotation pool size limits.
const BACKTEST_POOL_RANGE: std::ops::RangeInclusive<usize> = 2..=20;

fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn r4(x: f64) -> f64 {
    (x * 10000.0).round() / 10000.0
}

fn param_err(e: ParamError) -> CmdError {
    CmdError::new("invalid_param", e.to_string())
}

fn engine_err(e: impl std::fmt::Display) -> CmdError {
    CmdError::new("engine", e.to_string())
}

/// Resolve an arbitrary listed company into the graph. Identity nodes may be
/// partial; only source-backed F10 classification is allowed to create an edge.
async fn ensure_graph_company(state: &AppState, raw: &str) -> Result<(), CmdError> {
    if state.graph.find_node(raw).await?.is_some() {
        return Ok(());
    }
    let symbol = match Symbol::new(raw) {
        Ok(symbol) => symbol,
        Err(_) => return Ok(()),
    };
    if state.market.security_master.get(symbol.code()).is_none() {
        let _ = state.market.all_a_shares().await;
    }
    let mut identity = state.market.security_master.get(symbol.code());
    let profile = state
        .fundamental
        .profile(&symbol)
        .await
        .ok()
        .map(|f| f.data);
    let name = identity
        .as_ref()
        .map(|row| row.canonical_name.clone())
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            profile
                .as_ref()
                .map(|p| p.short_name.clone())
                .filter(|name| !name.trim().is_empty())
        })
        .ok_or_else(|| {
            CmdError::new(
                "not_found",
                format!("无法解析证券 {} 的身份", symbol.code()),
            )
        })?;
    let company_id = format!("company:{}", symbol.code());
    state
        .graph
        .upsert_node(&Node {
            id: company_id.clone(),
            kind: NodeKind::Company,
            name,
            code: Some(symbol.code().to_string()),
            meta: json!({
                "source": identity.as_ref().map(|row| row.source.as_str()).unwrap_or("eastmoney_f10"),
                "dynamic": true,
                "coverage": "identity",
            }),
        })
        .await?;

    if let Some(industry) = profile.as_ref().and_then(|p| p.industry.clone()) {
        let industry_id = format!("industry:f10:{industry}");
        state
            .graph
            .upsert_node(&Node {
                id: industry_id.clone(),
                kind: NodeKind::Industry,
                name: industry.clone(),
                code: None,
                meta: json!({"source": "eastmoney_f10", "dynamic": true}),
            })
            .await?;
        state
            .graph
            .upsert_edge(&Edge {
                id: None,
                src: company_id,
                dst: industry_id,
                relation: Relation::BelongsTo,
                weight: 1.0,
                source_name: "东方财富 F10 公司概况".to_string(),
                source_url: format!(
                    "https://emweb.securities.eastmoney.com/PC_HSF10/CompanySurvey/Index?type=web&code={}{}",
                    symbol.market(),
                    symbol.code()
                ),
                confidence: 0.95,
                valid_from: now_secs(),
                valid_to: None,
            })
            .await?;
        if let Some(mut record) = identity.take() {
            record.industry = Some(industry);
            record.refreshed_at = astock_core::time::utc_now();
            state.market.security_master.upsert(record.clone());
            state.storage.securities_upsert(vec![record]).await?;
        }
    }
    Ok(())
}

/// Reject unknown keys in the `params` JSON object so typos fail loudly
/// instead of being silently ignored.
fn check_param_keys(params: &Option<Value>, allowed: &[&str]) -> Result<(), CmdError> {
    match params {
        None | Some(Value::Null) => Ok(()),
        Some(Value::Object(map)) => {
            for key in map.keys() {
                if !allowed.contains(&key.as_str()) {
                    return Err(CmdError::new(
                        "invalid_param",
                        format!("策略不支持参数 `{key}`(可选:{})", allowed.join("/")),
                    ));
                }
            }
            Ok(())
        }
        Some(_) => Err(CmdError::new(
            "invalid_param",
            "params 必须是 JSON 对象,如 {\"fast\":5,\"slow\":60}",
        )),
    }
}

/// Integer param from the `params` JSON object (absent → None).
fn param_u32(params: &Option<Value>, key: &str) -> Result<Option<u32>, CmdError> {
    let Some(v) = params.as_ref().and_then(|p| p.get(key)) else {
        return Ok(None);
    };
    match v.as_u64() {
        Some(n) if n <= u64::from(u32::MAX) => Ok(Some(n as u32)),
        _ => Err(CmdError::new(
            "invalid_param",
            format!("params.{key} 必须是 0..={} 的整数", u32::MAX),
        )),
    }
}

/// Float param from the `params` JSON object (absent → None).
fn param_f64(params: &Option<Value>, key: &str) -> Result<Option<f64>, CmdError> {
    let Some(v) = params.as_ref().and_then(|p| p.get(key)) else {
        return Ok(None);
    };
    v.as_f64().map(Some).ok_or_else(|| {
        CmdError::new(
            "invalid_param",
            format!("params.{key} 必须是数字(收到 {v})"),
        )
    })
}

/// Explicit scalar arg wins; otherwise fall back to the `params` JSON object.
fn opt_u32(
    explicit: Option<u32>,
    params: &Option<Value>,
    key: &str,
) -> Result<Option<u32>, CmdError> {
    match explicit {
        Some(v) => Ok(Some(v)),
        None => param_u32(params, key),
    }
}

/// `symbol` is required for single-symbol strategies.
fn require_symbol(symbol: &Option<String>) -> Result<String, CmdError> {
    symbol
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| CmdError::new("invalid_param", "单标的策略必须提供 symbol"))
}

/// Daily-bar strategy backtest with A-share trading rules (T+1, lots,
/// limits, fees); returns performance stats, the equity curve and the last
/// 50 trades.
///
/// Strategy dispatch:
/// - 内置策略 `ma_cross`(默认)/ `turtle` / `buy_hold`:委托给 agent 共享
///   引擎,标量参数 fast/slow/entry_n/exit_n 直接传或经 `params` JSON 传;
/// - 注册表单标的策略(如 `zscore_mean_reversion`):本层用 `params` JSON
///   构造后走 `BacktestEngine`;
/// - 注册表轮动策略(`min_corr_etf_rotation`):走 `run_rotation`,多标的
///   由 `pool`(2-20 个代码)提供。
#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)]
pub async fn run_backtest(
    state: State<'_, AppState>,
    symbol: Option<String>,
    strategy: Option<String>,
    params: Option<Value>,
    pool: Option<Vec<String>>,
    fast: Option<u32>,
    slow: Option<u32>,
    entry_n: Option<u32>,
    exit_n: Option<u32>,
    bars: Option<u32>,
) -> Result<Value, CmdError> {
    run_backtest_impl(
        &state.market,
        &state.rules,
        symbol,
        strategy,
        params,
        pool,
        fast,
        slow,
        entry_n,
        exit_n,
        bars,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_backtest_impl(
    market: &MarketData,
    rules: &RuleSet,
    symbol: Option<String>,
    strategy: Option<String>,
    params: Option<Value>,
    pool: Option<Vec<String>>,
    fast: Option<u32>,
    slow: Option<u32>,
    entry_n: Option<u32>,
    exit_n: Option<u32>,
    bars: Option<u32>,
) -> Result<Value, CmdError> {
    let bars = bars.unwrap_or(750).clamp(60, 2000);
    let name = strategy
        .as_deref()
        .unwrap_or("ma_cross")
        .to_ascii_lowercase();
    match name.as_str() {
        "ma_cross" | "ma" | "turtle" | "turtle_breakout" | "buy_hold" | "buyhold" => {
            let symbol = require_symbol(&symbol)?;
            check_param_keys(&params, &["fast", "slow", "entry_n", "exit_n"])?;
            let fast = opt_u32(fast, &params, "fast")?;
            let slow = opt_u32(slow, &params, "slow")?;
            let entry_n = opt_u32(entry_n, &params, "entry_n")?;
            let exit_n = opt_u32(exit_n, &params, "exit_n")?;
            run_backtest_json(
                market,
                &symbol,
                strategy.as_deref(),
                fast,
                slow,
                entry_n,
                exit_n,
                bars,
            )
            .await
            .map_err(agent_err)
        }
        "zscore_mean_reversion" => {
            let symbol = require_symbol(&symbol)?;
            run_registry_single(market, rules, &symbol, &params, bars).await
        }
        "min_corr_etf_rotation" => run_registry_rotation(market, rules, pool, &params, bars).await,
        other => Err(CmdError::new(
            "invalid_param",
            format!(
                "未知策略 `{other}`:可选 ma_cross / turtle / buy_hold / \
                 zscore_mean_reversion / min_corr_etf_rotation"
            ),
        )),
    }
}

/// Start a single background backtest. The job is independent of the page
/// component and is polled through `backtest_status`.
#[tauri::command(rename_all = "snake_case")]
#[allow(clippy::too_many_arguments)]
pub async fn backtest_start(
    state: State<'_, AppState>,
    symbol: Option<String>,
    strategy: Option<String>,
    params: Option<Value>,
    pool: Option<Vec<String>>,
    fast: Option<u32>,
    slow: Option<u32>,
    entry_n: Option<u32>,
    exit_n: Option<u32>,
    bars: Option<u32>,
) -> Result<Value, CmdError> {
    let job_id = next_backtest_job_id();
    let started_at = now_millis();
    {
        let mut snapshot = state
            .backtest
            .snapshot
            .lock()
            .expect("backtest snapshot poisoned");
        if snapshot.status == "running" {
            return Err(CmdError::new(
                "already_running",
                "已有回测后台任务正在运行，请先等待或取消",
            ));
        }
        *snapshot = crate::state::BacktestSnapshot {
            job_id: Some(job_id.clone()),
            status: "running".into(),
            phase: "校验参数并加载历史行情".into(),
            progress: None,
            started_at: Some(started_at),
            updated_at: started_at,
            result: None,
            error: None,
        };
    }

    let token = CancellationToken::new();
    *state
        .backtest
        .cancel
        .lock()
        .expect("backtest cancel poisoned") = Some(token.clone());
    let market = Arc::clone(&state.market);
    let rules = state.rules.clone();
    let backtest = Arc::clone(&state.backtest);
    let spawned_job_id = job_id.clone();
    tauri::async_runtime::spawn(async move {
        let outcome = tokio::select! {
            _ = token.cancelled() => None,
            result = run_backtest_impl(
                &market,
                &rules,
                symbol,
                strategy,
                params,
                pool,
                fast,
                slow,
                entry_n,
                exit_n,
                bars,
            ) => Some(result),
        };
        let now = now_millis();
        let mut snapshot = backtest
            .snapshot
            .lock()
            .expect("backtest snapshot poisoned");
        // A newer job cannot currently overlap, but keep the identity guard
        // so future multi-job support cannot overwrite another result.
        if snapshot.job_id.as_deref() != Some(spawned_job_id.as_str()) {
            return;
        }
        match outcome {
            None => {
                snapshot.status = "cancelled".into();
                snapshot.phase = "已取消".into();
                snapshot.progress = None;
            }
            Some(Ok(result)) => {
                snapshot.status = "completed".into();
                snapshot.phase = "回测与绩效统计完成".into();
                snapshot.progress = Some(100);
                snapshot.result = Some(result);
            }
            Some(Err(error)) => {
                snapshot.status = "failed".into();
                snapshot.phase = "回测失败".into();
                snapshot.error = Some(error.to_string());
                snapshot.progress = None;
            }
        }
        snapshot.updated_at = now;
        drop(snapshot);
        *backtest.cancel.lock().expect("backtest cancel poisoned") = None;
    });

    Ok(json!({ "job_id": job_id, "started": true }))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn backtest_status(
    state: State<'_, AppState>,
) -> Result<crate::state::BacktestSnapshot, CmdError> {
    Ok(state
        .backtest
        .snapshot
        .lock()
        .expect("backtest snapshot poisoned")
        .clone())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn backtest_cancel(state: State<'_, AppState>) -> Result<Value, CmdError> {
    let token = state
        .backtest
        .cancel
        .lock()
        .expect("backtest cancel poisoned")
        .clone();
    let cancelled = token.is_some();
    if let Some(token) = token {
        token.cancel();
    }
    Ok(json!({ "cancelled": cancelled }))
}

/// Registry single-symbol strategy backtest (currently
/// `zscore_mean_reversion`): params from the `params` JSON object, output
/// shape mirrors the agent `run_backtest_json` payload plus `kind`.
async fn run_registry_single(
    market: &MarketData,
    rules: &RuleSet,
    symbol_raw: &str,
    params: &Option<Value>,
    bars: u32,
) -> Result<Value, CmdError> {
    check_param_keys(params, &["ma_window", "z_window", "entry_z", "exit_z"])?;
    let ma_window = param_u32(params, "ma_window")?.unwrap_or(20) as usize;
    let z_window = param_u32(params, "z_window")?.unwrap_or(60) as usize;
    let entry_z = param_f64(params, "entry_z")?.unwrap_or(-2.0);
    let exit_z = param_f64(params, "exit_z")?.unwrap_or(1.0);
    let mut strat =
        ZscoreMeanReversion::try_new(ma_window, z_window, entry_z, exit_z).map_err(param_err)?;

    let symbol = Symbol::new(symbol_raw)?;
    let fetched = market
        .kline(&symbol, KlinePeriod::Day, Adjust::Qfq, bars)
        .await?;
    if fetched.data.len() < BACKTEST_MIN_BARS {
        return Err(CmdError::new(
            "engine",
            format!(
                "k线数据不足:仅{}根,至少需要{}根",
                fetched.data.len(),
                BACKTEST_MIN_BARS
            ),
        ));
    }
    let series = PriceSeries::new(
        symbol.code(),
        fetched
            .data
            .iter()
            .map(astock_backtest::data::Bar::from)
            .collect::<Vec<_>>(),
    )
    .map_err(engine_err)?;

    let engine = BacktestEngine::new(
        rules.clone(),
        BtConfig::new(symbol.code(), BACKTEST_INITIAL_CASH),
    )
    .map_err(engine_err)?;
    let result = engine.run(&series, &mut strat).map_err(engine_err)?;
    let report = result
        .performance_report(None, &MetricsConfig::default())
        .ok_or_else(|| engine_err("回测区间过短,无法生成绩效报告"))?;

    let tail_start = result.trades.len().saturating_sub(BACKTEST_TRADES_TAIL);
    let trades: Vec<Value> = result.trades[tail_start..]
        .iter()
        .map(|f| {
            json!({
                "date": f.date.to_string(),
                "side": match f.side { TradeSide::Buy => "buy", TradeSide::Sell => "sell" },
                "shares": f.shares,
                "price": r2(f.price),
                "amount": r2(f.amount),
                "fees": r2(f.fees.total),
                "reason": f.reason,
            })
        })
        .collect();
    let equity_curve: Vec<Value> = result
        .equity
        .iter()
        .map(|p| json!([p.date.to_string(), r2(p.equity)]))
        .collect();

    Ok(json!({
        "kind": "single",
        "symbol": symbol.code(),
        "strategy": "zscore_mean_reversion",
        "params": {
            "ma_window": ma_window,
            "z_window": z_window,
            "entry_z": entry_z,
            "exit_z": exit_z,
        },
        "data": {
            "start": report.start.to_string(),
            "end": report.end.to_string(),
            "bars": series.len(),
        },
        "initial_cash": BACKTEST_INITIAL_CASH,
        "final_equity": r2(result.final_equity()),
        "total_return": r4(report.total_return),
        "cagr": r4(report.cagr),
        "annualized_volatility": r4(report.annualized_volatility),
        "sharpe": r4(report.sharpe),
        "sortino": r4(report.sortino),
        "calmar": r4(report.calmar),
        "max_drawdown": r4(report.max_drawdown),
        "max_drawdown_duration_bars": report.max_drawdown_duration_bars,
        "round_trips": report.round_trips,
        "hit_rate": r4(report.hit_rate),
        "payoff_ratio": r4(report.payoff_ratio),
        "profit_factor": r4(report.profit_factor),
        "trades_count": result.trades.len(),
        "rejections": result.rejections.len(),
        "fees_total": r2(result.total_fees()),
        "equity_curve": equity_curve,
        "trades_tail": trades,
        "note": "单组参数的历史回测不代表未来收益;未做参数网格与过拟合检验",
    }))
}

/// Registry rotation strategy backtest (`min_corr_etf_rotation`): fetch the
/// pool's daily klines, align on common trading dates inside `run_rotation`
/// (monthly rebalance, equal weight) and compute metrics from the equity
/// curve via the backtest `metrics` module.
async fn run_registry_rotation(
    market: &MarketData,
    rules: &RuleSet,
    pool: Option<Vec<String>>,
    params: &Option<Value>,
    bars: u32,
) -> Result<Value, CmdError> {
    check_param_keys(params, &["lookback", "hold_n"])?;
    let lookback = param_u32(params, "lookback")?.unwrap_or(60) as usize;
    let hold_n = param_u32(params, "hold_n")?.unwrap_or(4) as usize;
    let strat = MinCorrRotation::try_new(lookback, hold_n).map_err(param_err)?;

    let raw_pool = pool.ok_or_else(|| {
        CmdError::new(
            "invalid_param",
            "min_corr_etf_rotation 需要 pool:代码列表(2-20 个)",
        )
    })?;
    let mut symbols: Vec<Symbol> = Vec::new();
    for raw in &raw_pool {
        let sym = Symbol::new(raw)?;
        if !symbols.contains(&sym) {
            symbols.push(sym);
        }
    }
    if !BACKTEST_POOL_RANGE.contains(&symbols.len()) {
        return Err(CmdError::new(
            "invalid_param",
            format!(
                "pool 需包含 {}-{} 个不同代码(去重后收到 {})",
                BACKTEST_POOL_RANGE.start(),
                BACKTEST_POOL_RANGE.end(),
                symbols.len()
            ),
        ));
    }

    // Fetch pool klines concurrently (order-preserving), all-or-nothing.
    let klines: Vec<Vec<astock_core::Bar>> =
        futures::future::try_join_all(symbols.iter().map(|sym| async move {
            market
                .kline(sym, KlinePeriod::Day, Adjust::Qfq, bars)
                .await
                .map(|f| f.data)
        }))
        .await?;
    let min_bars = BACKTEST_MIN_BARS.max(lookback + 2);
    let mut pool_series: Vec<PriceSeries> = Vec::with_capacity(symbols.len());
    for (sym, data) in symbols.iter().zip(klines) {
        if data.len() < min_bars {
            return Err(CmdError::new(
                "engine",
                format!(
                    "{} k线数据不足:仅{}根,至少需要{}根(lookback={lookback})",
                    sym.code(),
                    data.len(),
                    min_bars
                ),
            ));
        }
        pool_series.push(
            PriceSeries::new(
                sym.code(),
                data.iter()
                    .map(astock_backtest::data::Bar::from)
                    .collect::<Vec<_>>(),
            )
            .map_err(engine_err)?,
        );
    }

    let result = run_rotation(
        &pool_series,
        &strat,
        rules,
        &RotationConfig::new(BACKTEST_INITIAL_CASH),
    )
    .map_err(|e| match e {
        RotationError::PoolTooSmall(_) => CmdError::new("invalid_param", e.to_string()),
        other => engine_err(other),
    })?;

    let curve = result.equity_curve();
    if curve.len() < 2 {
        return Err(engine_err("共同交易日不足,无法计算绩效指标"));
    }
    let cfg = MetricsConfig::default();
    let returns = metrics::daily_returns(&curve);
    let first = result.equity[0].date;
    let last = result.equity[result.equity.len() - 1].date;
    let years = ((last - first).num_days() as f64 / 365.25).max(1.0 / 365.25);
    let total_return = metrics::total_return(&curve);
    let cagr = metrics::cagr(curve[0], curve[curve.len() - 1], years);
    let (max_dd, max_dd_dur) = metrics::max_drawdown(&curve);

    let tail_start = result.trades.len().saturating_sub(BACKTEST_TRADES_TAIL);
    let trades: Vec<Value> = result.trades[tail_start..]
        .iter()
        .map(|t| {
            json!({
                "date": t.date.to_string(),
                "symbol": t.symbol,
                "side": match t.side { TradeSide::Buy => "buy", TradeSide::Sell => "sell" },
                "shares": t.shares,
                "price": r2(t.price),
                "amount": r2(t.amount),
                "fees": r2(t.fees.total),
                "reason": t.reason,
            })
        })
        .collect();
    let equity_curve: Vec<Value> = result
        .equity
        .iter()
        .map(|p| json!([p.date.to_string(), r2(p.equity)]))
        .collect();
    let fees_total: f64 = result.trades.iter().map(|t| t.fees.total).sum();
    let codes: Vec<String> = symbols.iter().map(|s| s.code().to_string()).collect();

    Ok(json!({
        "kind": "rotation",
        "strategy": strat.name(),
        "pool": codes,
        "params": {"lookback": lookback, "hold_n": hold_n},
        "data": {
            "start": first.to_string(),
            "end": last.to_string(),
            "bars": curve.len(),
        },
        "initial_cash": BACKTEST_INITIAL_CASH,
        "final_equity": r2(result.final_equity()),
        "total_return": r4(total_return),
        "cagr": r4(cagr),
        "annualized_volatility": r4(metrics::annualized_volatility(&returns, &cfg)),
        "sharpe": r4(metrics::sharpe(&returns, &cfg)),
        "sortino": r4(metrics::sortino(&returns, &cfg)),
        "calmar": r4(metrics::calmar(cagr, max_dd)),
        "max_drawdown": r4(max_dd),
        "max_drawdown_duration_bars": max_dd_dur,
        "trades_count": result.trades.len(),
        "fees_total": r2(fees_total),
        "equity_curve": equity_curve,
        "trades_tail": trades,
        "note": "单组参数的历史回测不代表未来收益;池内序列按共同交易日对齐,月度再平衡;未做参数网格与过拟合检验",
    }))
}

/// Market regime snapshot: 进攻/中性/防守 plus the supporting breadth and
/// index-trend data.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_market_regime(state: State<'_, AppState>) -> Result<Value, CmdError> {
    let (mut payload, source, fetched_at) = market_regime_json(&*state.market)
        .await
        .map_err(agent_err)?;
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("source".into(), json!(source));
        obj.insert("fetched_at".into(), json!(fetched_at));
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_direction_accepts_aliases() {
        assert_eq!(parse_direction("up").unwrap(), 1);
        assert_eq!(parse_direction("UP").unwrap(), 1);
        assert_eq!(parse_direction("上涨").unwrap(), 1);
        assert_eq!(parse_direction("down").unwrap(), -1);
        assert_eq!(parse_direction("跌").unwrap(), -1);
        let err = parse_direction("sideways").unwrap_err();
        assert_eq!(err.kind, "invalid_param");
    }

    #[test]
    fn agent_err_maps_invalid_args() {
        let err = agent_err(AgentError::InvalidArgs {
            tool: "run_backtest".into(),
            msg: "bad combo".into(),
        });
        assert_eq!(err.kind, "invalid_param");
        assert_eq!(err.error, "bad combo");
        let other = agent_err(AgentError::UnknownTool("x".into()));
        assert_eq!(other.kind, "engine");
    }

    #[test]
    fn check_param_keys_rejects_unknown_and_non_object() {
        let params = Some(json!({"fast": 5, "slow": 60}));
        assert!(check_param_keys(&params, &["fast", "slow"]).is_ok());
        assert!(check_param_keys(&None, &["fast"]).is_ok());
        assert!(check_param_keys(&Some(Value::Null), &["fast"]).is_ok());

        let err = check_param_keys(&Some(json!({"fasst": 5})), &["fast"]).unwrap_err();
        assert_eq!(err.kind, "invalid_param");
        assert!(err.error.contains("fasst"));

        let err = check_param_keys(&Some(json!([1, 2])), &["fast"]).unwrap_err();
        assert_eq!(err.kind, "invalid_param");
    }

    #[test]
    fn param_readers_validate_types() {
        let params = Some(json!({"fast": 5, "entry_z": -2.0}));
        assert_eq!(param_u32(&params, "fast").unwrap(), Some(5));
        assert_eq!(param_u32(&params, "missing").unwrap(), None);
        assert_eq!(param_f64(&params, "entry_z").unwrap(), Some(-2.0));
        // 整数 5 也可读作 f64。
        assert_eq!(param_f64(&params, "fast").unwrap(), Some(5.0));

        let bad = Some(json!({"fast": "5", "z": -2.5}));
        assert!(param_u32(&bad, "fast").is_err(), "string is not a u32");
        assert!(param_u32(&bad, "z").is_err(), "float is not a u32");
    }

    #[test]
    fn opt_u32_explicit_wins_over_json() {
        let params = Some(json!({"fast": 5}));
        assert_eq!(opt_u32(Some(9), &params, "fast").unwrap(), Some(9));
        assert_eq!(opt_u32(None, &params, "fast").unwrap(), Some(5));
        assert_eq!(opt_u32(None, &None, "fast").unwrap(), None);
    }

    #[test]
    fn require_symbol_trims_and_rejects_empty() {
        assert_eq!(require_symbol(&Some(" 600519 ".into())).unwrap(), "600519");
        assert!(require_symbol(&None).is_err());
        assert!(require_symbol(&Some("  ".into())).is_err());
    }
}
