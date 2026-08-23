//! The v1 tool set: market data, indicators, the golden-tested signal
//! engine, Chan theory, fund flow, breadth, search, comparison and scanning.
//!
//! Every tool produces a compact `summary_json` (what the LLM sees) plus an
//! optional `full_json` (persisted to `tool_cache`, retrievable through
//! `get_cached_detail`). All numbers come from the deterministic engines or
//! upstream payloads — never from the model.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use astock_core::{Bar, Fetched, FundFlowPoint, MarketBreadth, Quote, Symbol};
use astock_security::ToolPermissionDomain;
use astock_storage::ToolCacheEntry;
use astock_technical as tech;

use crate::error::{AgentError, Result};
use crate::indicators::{bollinger_series, kdj_series, rsi_series};
use crate::tools::{
    now_secs, parse_adjust, parse_args, parse_period, schema_value, tool_cache_key, AgentTool,
    CacheEnvelope, ToolContext, ToolProgressDetail, ToolRegistry, ToolResult, ToolWorkItem,
};

/// Round to 2 decimals for display summaries.
pub(crate) fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Round to 4 decimals for display summaries.
pub(crate) fn r4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// Extract provenance from a fetched payload.
fn provenance<T>(f: &Fetched<T>) -> (String, String) {
    (f.source.to_string(), f.fetched_at.to_rfc3339())
}

pub(crate) fn parse_symbol(tool: &str, raw: &str) -> Result<Symbol> {
    Symbol::new(raw).map_err(|e| AgentError::InvalidArgs {
        tool: tool.to_string(),
        msg: e.to_string(),
    })
}

pub(crate) fn tool_err(tool: &str, msg: impl Into<String>) -> AgentError {
    AgentError::Tool {
        tool: tool.to_string(),
        msg: msg.into(),
    }
}

/// Compact `[date, open, close, high, low, volume]` encoding of one bar.
fn bar_row(b: &Bar) -> Value {
    json!([
        b.date.to_string(),
        r2(b.open),
        r2(b.close),
        r2(b.high),
        r2(b.low),
        b.volume as i64,
    ])
}

/// Summary statistics over a bar window.
pub(crate) fn bar_stats(bars: &[Bar]) -> Value {
    if bars.is_empty() {
        return json!({"bar_count": 0});
    }
    let first = bars.first().unwrap();
    let last = bars.last().unwrap();
    let max_high = bars
        .iter()
        .map(|b| b.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_low = bars.iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
    let avg_volume = bars.iter().map(|b| b.volume).sum::<f64>() / bars.len() as f64;
    let window_pct = if first.close > 0.0 {
        r2((last.close - first.close) / first.close * 100.0)
    } else {
        0.0
    };
    json!({
        "bar_count": bars.len(),
        "first_date": first.date.to_string(),
        "last_date": last.date.to_string(),
        "last_close": r2(last.close),
        "window_pct": window_pct,
        "max_high": r2(max_high),
        "min_low": r2(min_low),
        "avg_volume": avg_volume as i64,
    })
}

/// Convert core bars to the technical engine's kline input.
fn to_tech_klines(bars: &[Bar]) -> Vec<tech::Kline> {
    bars.iter()
        .map(|b| tech::Kline {
            date: b.date.to_string(),
            open: b.open,
            close: b.close,
            high: b.high,
            low: b.low,
            volume: b.volume,
            amount: b.amount.unwrap_or(0.0),
            pct: b.pct.unwrap_or(0.0),
            turnover: b.turnover.unwrap_or(0.0),
        })
        .collect()
}

fn to_tech_quote(q: &Quote) -> tech::Quote {
    tech::Quote {
        symbol: q.symbol.clone(),
        name: q.name.clone(),
        price: q.price,
        pct: q.pct,
        change: q.change,
        high: q.high,
        low: q.low,
        open: q.open,
        pre_close: q.pre_close,
        volume: q.volume,
        amount: q.amount,
        turnover: q.turnover.unwrap_or(0.0),
        timestamp: q.timestamp.to_rfc3339(),
    }
}

fn to_tech_flow(f: &FundFlowPoint) -> tech::FundFlow {
    tech::FundFlow {
        date: f.time.date().to_string(),
        main_net: f.main_net,
        super_large_net: f.super_large_net,
        large_net: f.large_net,
        medium_net: f.medium_net,
        small_net: f.small_net,
        main_pct: f.main_pct,
    }
}

fn to_tech_breadth(b: &MarketBreadth) -> tech::Breadth {
    tech::Breadth {
        up: i64::from(b.up),
        down: i64::from(b.down),
        flat: i64::from(b.flat),
        total: i64::from(b.total),
        breadth_ratio: b.ratio(),
    }
}

/// Keys copied from the full signal JSON into the LLM-facing summary. The
/// bulky per-module detail objects stay in `full_json` only.
const ANALYSIS_SUMMARY_KEYS: &[&str] = &[
    "action",
    "score",
    "confidence",
    "risk_level",
    "signal_strength",
    "description",
    "plain_summary",
    "trade_plan",
    "manual_plan",
    "module_scores",
    "buy_signals",
    "sell_signals",
    "risk_warnings",
    "key_levels",
];

/// Trim the golden-tested signal JSON to the compact summary the LLM sees.
pub(crate) fn analysis_summary(signal: &Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(obj) = signal.as_object() {
        for key in ANALYSIS_SUMMARY_KEYS {
            if let Some(v) = obj.get(*key) {
                out.insert((*key).to_string(), v.clone());
            }
        }
    }
    Value::Object(out)
}

/// Inputs shared by the analysis-based tools (full analysis, compare, scan).
struct AnalysisInputs {
    klines: Vec<tech::Kline>,
    quote: Option<tech::Quote>,
    flows: Option<Vec<tech::FundFlow>>,
    index: Option<Vec<tech::Kline>>,
    source: String,
    fetched_at: String,
}

fn attach_agent_manual_plan(signal: &mut Value, symbol: &Symbol, inputs: &AnalysisInputs) {
    let Ok(rules) = astock_trading_rules::RuleSet::load(None) else {
        return;
    };
    let auction = &rules.data.auction;
    let sessions = tech::SessionSchedule {
        open_auction_start: auction.open_call_auction.start.clone(),
        open_auction_end: auction.open_call_auction.end.clone(),
        morning_start: auction.continuous_morning.start.clone(),
        morning_end: auction.continuous_morning.end.clone(),
        afternoon_start: auction.continuous_afternoon.start.clone(),
        afternoon_end: auction.continuous_afternoon.end.clone(),
        close_auction_start: auction.close_call_auction.start.clone(),
        close_auction_end: auction.close_call_auction.end.clone(),
    };
    let board = rules.for_symbol(symbol.code()).ok();
    let constraints = tech::TradingConstraints {
        board_name: board
            .as_ref()
            .map(|value| value.board_name.clone())
            .unwrap_or_else(|| "未知板块".to_string()),
        price_limit_pct: board
            .as_ref()
            .map_or(0.10, |value| value.price_limit_pct(false)),
        min_lot: board.as_ref().map_or(100, |value| value.min_lot),
        lot_step: board.as_ref().map_or(100, |value| value.lot_step),
        t_plus_1: board.as_ref().is_none_or(|value| value.t_plus_1),
    };
    let name = inputs
        .quote
        .as_ref()
        .map(|quote| quote.name.as_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| symbol.code());
    if let Some(plan) = tech::build_manual_trading_plan(
        symbol.code(),
        name,
        &inputs.klines,
        signal,
        &sessions,
        &constraints,
        &inputs.fetched_at,
        &inputs.source,
    ) {
        if let (Some(object), Ok(plan_json)) = (signal.as_object_mut(), serde_json::to_value(plan))
        {
            object.insert("manual_plan".to_string(), plan_json);
        }
    }
}

/// Minimum history the signal engine needs to be meaningful (MA60/MACD).
const MIN_ANALYSIS_BARS: usize = 60;

/// Fetch kline (required) plus optional quote/flows/index context. Context
/// fetch failures degrade to `None` — the engine treats them as absent,
/// exactly like the legacy pipeline.
async fn fetch_analysis_inputs(
    ctx: &ToolContext,
    tool: &str,
    symbol: &Symbol,
    period: astock_core::KlinePeriod,
    count: u32,
    with_context: bool,
) -> Result<AnalysisInputs> {
    let fetched = ctx
        .market
        .kline(symbol, period, astock_core::Adjust::Qfq, count)
        .await?;
    if fetched.data.len() < MIN_ANALYSIS_BARS {
        return Err(tool_err(
            tool,
            format!(
                "k线数据不足：仅{}根，至少需要{}根",
                fetched.data.len(),
                MIN_ANALYSIS_BARS
            ),
        ));
    }
    let (source, fetched_at) = provenance(&fetched);
    let klines = to_tech_klines(&fetched.data);

    let (quote, flows, index) = if with_context {
        // These three sources are independent. Running them sequentially made
        // a comparison pay the sum of every upstream latency for every stock;
        // joining them bounds the context phase by the slowest one instead.
        let (quote, flows, index) = tokio::join!(
            ctx.market.quote(symbol),
            ctx.market.fund_flow_daily(symbol, 30),
            ctx.market.index_kline("1.000001", count),
        );
        (
            quote.ok().map(|q| to_tech_quote(&q.data)),
            flows
                .ok()
                .map(|f| f.data.iter().map(to_tech_flow).collect::<Vec<_>>()),
            index.ok().map(|f| to_tech_klines(&f.data)),
        )
    } else {
        (None, None, None)
    };
    Ok(AnalysisInputs {
        klines,
        quote,
        flows,
        index,
        source,
        fetched_at,
    })
}

/// Run the full signal pipeline over the gathered inputs.
fn run_engine(inputs: &AnalysisInputs, breadth: Option<&tech::Breadth>) -> Value {
    tech::analyze(
        &inputs.klines,
        inputs.quote.as_ref(),
        inputs.flows.as_deref(),
        inputs.index.as_deref(),
        breadth,
    )
}

// ---------------------------------------------------------------------
// get_quote
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct SymbolArgs {
    /// 6位证券代码，如 600519、000001
    symbol: String,
}

/// Realtime quote snapshot.
struct GetQuote;

#[async_trait]
impl AgentTool for GetQuote {
    fn name(&self) -> &'static str {
        "get_quote"
    }
    fn description(&self) -> &'static str {
        "获取个股实时行情快照（最新价、涨跌幅、量能）"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<SymbolArgs>()
    }
    fn cache_ttl_secs(&self) -> i64 {
        60
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args: SymbolArgs = parse_args(self.name(), args)?;
        let symbol = parse_symbol(self.name(), &args.symbol)?;
        let fetched = ctx.market.quote(&symbol).await?;
        let (source, fetched_at) = provenance(&fetched);
        let q = &fetched.data;
        let summary = json!({
            "symbol": q.symbol,
            "name": q.name,
            "price": q.price,
            "pct": q.pct,
            "change": q.change,
            "open": q.open,
            "high": q.high,
            "low": q.low,
            "pre_close": q.pre_close,
            "volume_lots": q.volume as i64,
            "amount": q.amount as i64,
            "turnover": q.turnover,
        });
        Ok(ToolResult {
            summary_json: summary,
            full_json: Some(serde_json::to_value(q)?),
            cache_key: String::new(),
            source,
            fetched_at,
        })
    }
}

// ---------------------------------------------------------------------
// get_kline
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct KlineArgs {
    /// 6位证券代码
    symbol: String,
    /// K线周期：day/week/month/1m/5m/15m/30m/60m，默认 day
    period: Option<String>,
    /// 复权方式：none/qfq/hfq，默认 qfq
    adjust: Option<String>,
    /// 拉取的K线根数，默认 120，上限 500
    count: Option<u32>,
}

/// How many recent bars the LLM sees inline; the rest stay in the cache.
const KLINE_TAIL: usize = 30;

/// Historical kline with window statistics.
struct GetKline;

#[async_trait]
impl AgentTool for GetKline {
    fn name(&self) -> &'static str {
        "get_kline"
    }
    fn description(&self) -> &'static str {
        "获取历史K线，返回窗口统计与最近若干根（完整数据入缓存）"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<KlineArgs>()
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args: KlineArgs = parse_args(self.name(), args)?;
        let symbol = parse_symbol(self.name(), &args.symbol)?;
        let period = parse_period(args.period.as_deref())?;
        let adjust = parse_adjust(args.adjust.as_deref())?;
        let count = args.count.unwrap_or(120).clamp(1, 500);
        let fetched = ctx.market.kline(&symbol, period, adjust, count).await?;
        if fetched.data.is_empty() {
            return Err(tool_err(self.name(), "k线数据为空"));
        }
        let (source, fetched_at) = provenance(&fetched);
        let bars = &fetched.data;
        let tail: Vec<Value> = bars
            .iter()
            .rev()
            .take(KLINE_TAIL)
            .rev()
            .map(bar_row)
            .collect();
        let summary = json!({
            "symbol": symbol.code(),
            "period": format!("{period:?}"),
            "adjust": format!("{adjust:?}"),
            "stats": bar_stats(bars),
            "columns": ["date", "open", "close", "high", "low", "volume_lots"],
            "tail": tail,
        });
        let full = json!({
            "columns": ["date", "open", "close", "high", "low", "volume_lots"],
            "bars": bars.iter().map(bar_row).collect::<Vec<_>>(),
        });
        Ok(ToolResult {
            summary_json: summary,
            full_json: Some(full),
            cache_key: String::new(),
            source,
            fetched_at,
        })
    }
}

// ---------------------------------------------------------------------
// compute_indicators
// ---------------------------------------------------------------------

/// Latest value of a series, or null when the window is not full yet.
fn last_of(series: &[Option<f64>]) -> Value {
    series.last().and_then(|v| v.map(r4)).into()
}

/// Last `n` non-null values of a series, rounded.
fn tail_of(series: &[Option<f64>], n: usize) -> Vec<Value> {
    series
        .iter()
        .rev()
        .filter_map(|v| v.map(r4))
        .take(n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|v| json!(v))
        .collect()
}

fn tail_f64(series: &[f64], n: usize) -> Vec<Value> {
    series
        .iter()
        .rev()
        .take(n)
        .map(|v| json!(r4(*v)))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// MA/MACD/RSI/KDJ/BOLL snapshot computed from kline closes.
struct ComputeIndicators;

#[async_trait]
impl AgentTool for ComputeIndicators {
    fn name(&self) -> &'static str {
        "compute_indicators"
    }
    fn description(&self) -> &'static str {
        "计算技术指标最新值（MA/MACD/RSI/KDJ/BOLL）及短序列尾部"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<KlineArgs>()
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args: KlineArgs = parse_args(self.name(), args)?;
        let symbol = parse_symbol(self.name(), &args.symbol)?;
        let period = parse_period(args.period.as_deref())?;
        let count = args.count.unwrap_or(120).clamp(30, 500);
        let fetched = ctx
            .market
            .kline(&symbol, period, astock_core::Adjust::Qfq, count)
            .await?;
        if fetched.data.is_empty() {
            return Err(tool_err(self.name(), "k线数据为空"));
        }
        let (source, fetched_at) = provenance(&fetched);
        let bars = &fetched.data;
        let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
        let highs: Vec<f64> = bars.iter().map(|b| b.high).collect();
        let lows: Vec<f64> = bars.iter().map(|b| b.low).collect();

        let ma = |p: usize| last_of(&tech::indicators::sma_series(&closes, p));
        let (dif, dea, bar) = tech::indicators::macd_series(&closes, 12, 26, 9);
        let rsi = rsi_series(&closes, 14);
        let (k, d, j) = kdj_series(&highs, &lows, &closes, 9);
        let (mid, up, lo) = bollinger_series(&closes, 20, 2.0);

        let summary = json!({
            "symbol": symbol.code(),
            "period": format!("{period:?}"),
            "bar_count": bars.len(),
            "ma": {"ma5": ma(5), "ma10": ma(10), "ma20": ma(20), "ma60": ma(60)},
            "macd": {
                "dif": dif.last().map(|v| r4(*v)),
                "dea": dea.last().map(|v| r4(*v)),
                "bar": bar.last().map(|v| r4(*v)),
            },
            "rsi14": last_of(&rsi),
            "kdj": {"k": last_of(&k), "d": last_of(&d), "j": last_of(&j)},
            "boll": {"mid": last_of(&mid), "upper": last_of(&up), "lower": last_of(&lo)},
        });
        let full = json!({
            "ma20_tail": tail_of(&tech::indicators::sma_series(&closes, 20), 60),
            "ma60_tail": tail_of(&tech::indicators::sma_series(&closes, 60), 60),
            "macd_dif_tail": tail_f64(&dif, 60),
            "macd_dea_tail": tail_f64(&dea, 60),
            "macd_bar_tail": tail_f64(&bar, 60),
            "rsi14_tail": tail_of(&rsi, 60),
            "kdj_k_tail": tail_of(&k, 60),
            "kdj_d_tail": tail_of(&d, 60),
            "kdj_j_tail": tail_of(&j, 60),
            "boll_mid_tail": tail_of(&mid, 60),
            "boll_upper_tail": tail_of(&up, 60),
            "boll_lower_tail": tail_of(&lo, 60),
        });
        Ok(ToolResult {
            summary_json: summary,
            full_json: Some(full),
            cache_key: String::new(),
            source,
            fetched_at,
        })
    }
}

// ---------------------------------------------------------------------
// run_full_analysis
// ---------------------------------------------------------------------

/// The golden-tested five-module signal pipeline.
struct RunFullAnalysis;

#[async_trait]
impl AgentTool for RunFullAnalysis {
    fn name(&self) -> &'static str {
        "run_full_analysis"
    }
    fn description(&self) -> &'static str {
        "运行完整信号引擎（趋势/形态/量价/突破/CANSLIM），返回评分、操作建议与交易计划"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<KlineArgs>()
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args: KlineArgs = parse_args(self.name(), args)?;
        let symbol = parse_symbol(self.name(), &args.symbol)?;
        let period = parse_period(args.period.as_deref())?;
        let count = args
            .count
            .unwrap_or(250)
            .clamp(MIN_ANALYSIS_BARS as u32, 500);

        let inputs = fetch_analysis_inputs(ctx, self.name(), &symbol, period, count, true).await?;
        let breadth = ctx
            .market
            .market_breadth()
            .await
            .ok()
            .map(|b| to_tech_breadth(&b.data));
        let mut signal = run_engine(&inputs, breadth.as_ref());
        attach_agent_manual_plan(&mut signal, &symbol, &inputs);

        let mut summary = analysis_summary(&signal);
        if let Some(obj) = summary.as_object_mut() {
            obj.insert("symbol".into(), json!(symbol.code()));
            obj.insert("period".into(), json!(format!("{period:?}")));
        }
        Ok(ToolResult {
            summary_json: summary,
            full_json: Some(signal),
            cache_key: String::new(),
            source: inputs.source,
            fetched_at: inputs.fetched_at,
        })
    }
}

// ---------------------------------------------------------------------
// run_chanlun
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct ChanlunArgs {
    /// 6位证券代码
    symbol: String,
    /// 分析级别：day（日线，默认）或 minute（当日分时，一类买卖点）
    period: Option<String>,
}

/// Chan theory (缠论) analysis, daily or intraday-minute.
struct RunChanlun;

#[async_trait]
impl AgentTool for RunChanlun {
    fn name(&self) -> &'static str {
        "run_chanlun"
    }
    fn description(&self) -> &'static str {
        "运行缠论分析（分型/笔/中枢/买卖点），返回结构计数、当前状态与信号"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<ChanlunArgs>()
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args: ChanlunArgs = parse_args(self.name(), args)?;
        let symbol = parse_symbol(self.name(), &args.symbol)?;
        let minute = args
            .period
            .as_deref()
            .map(|p| p.eq_ignore_ascii_case("minute") || p.eq_ignore_ascii_case("intraday"))
            .unwrap_or(false);

        if minute {
            let fetched = ctx.market.minute(&symbol).await?;
            if fetched.data.points.is_empty() {
                return Err(tool_err(self.name(), "当日无分时数据（可能非交易时段）"));
            }
            let (source, fetched_at) = provenance(&fetched);
            let times: Vec<String> = fetched
                .data
                .points
                .iter()
                .map(|p| p.time.format("%Y-%m-%d %H:%M").to_string())
                .collect();
            let prices: Vec<f64> = fetched.data.points.iter().map(|p| p.price).collect();
            let volumes: Vec<f64> = fetched.data.points.iter().map(|p| p.volume).collect();
            let result = astock_chanlun::minute::analyze_chanlun_minute(&times, &prices, &volumes);
            let dict = astock_chanlun::minute::signals_to_dict(&result);
            Ok(ToolResult {
                summary_json: chanlun_summary(&dict, false),
                full_json: Some(dict),
                cache_key: String::new(),
                source,
                fetched_at,
            })
        } else {
            let fetched = ctx
                .market
                .kline(
                    &symbol,
                    astock_core::KlinePeriod::Day,
                    astock_core::Adjust::Qfq,
                    250,
                )
                .await?;
            if fetched.data.len() < 10 {
                return Err(tool_err(self.name(), "k线数据不足，无法运行缠论分析"));
            }
            let (source, fetched_at) = provenance(&fetched);
            let bars = &fetched.data;
            let dates: Vec<String> = bars.iter().map(|b| b.date.to_string()).collect();
            let opens: Vec<f64> = bars.iter().map(|b| b.open).collect();
            let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
            let highs: Vec<f64> = bars.iter().map(|b| b.high).collect();
            let lows: Vec<f64> = bars.iter().map(|b| b.low).collect();
            let volumes: Vec<f64> = bars.iter().map(|b| b.volume).collect();
            let result = astock_chanlun::daily::analyze_chanlun_daily(
                &dates, &opens, &closes, &highs, &lows, &volumes,
            );
            let dict = astock_chanlun::daily::daily_result_to_dict(&result);
            Ok(ToolResult {
                summary_json: chanlun_summary(&dict, true),
                full_json: Some(dict),
                cache_key: String::new(),
                source,
                fetched_at,
            })
        }
    }
}

/// Compact Chan summary: structural counts, current state and signals; the
/// chart overlay payloads stay in `full_json`.
pub(crate) fn chanlun_summary(dict: &Value, daily: bool) -> Value {
    let get = |k: &str| dict.get(k).cloned().unwrap_or(Value::Null);
    let mut out = serde_json::Map::new();
    for key in [
        "kline_count",
        "fractal_count",
        "stroke_count",
        "current_state",
        "summary",
        "description",
    ] {
        out.insert(key.to_string(), get(key));
    }
    if daily {
        out.insert("merged_count".into(), get("merged_count"));
        out.insert("zhongshu_count".into(), get("zhongshu_count"));
    }
    let signals = dict
        .get("signals")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let kept = signals.len().min(10);
    out.insert("signal_count".into(), json!(signals.len()));
    out.insert(
        "signals".into(),
        json!(signals[signals.len() - kept..].to_vec()),
    );
    Value::Object(out)
}

// ---------------------------------------------------------------------
// get_fund_flow
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct FundFlowArgs {
    /// 6位证券代码
    symbol: String,
    /// 回看天数，默认 10，上限 60
    days: Option<u32>,
}

/// Daily main-force fund flow with a streak summary.
struct GetFundFlow;

#[async_trait]
impl AgentTool for GetFundFlow {
    fn name(&self) -> &'static str {
        "get_fund_flow"
    }
    fn description(&self) -> &'static str {
        "获取主力资金流向（近日明细 + 连续净流入/流出天数）"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<FundFlowArgs>()
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args: FundFlowArgs = parse_args(self.name(), args)?;
        let symbol = parse_symbol(self.name(), &args.symbol)?;
        let days = args.days.unwrap_or(10).clamp(1, 60);
        let fetched = ctx.market.fund_flow_daily(&symbol, days).await?;
        let (source, fetched_at) = provenance(&fetched);
        Ok(ToolResult {
            summary_json: fund_flow_summary(&fetched.data),
            full_json: Some(serde_json::to_value(&fetched.data)?),
            cache_key: String::new(),
            source,
            fetched_at,
        })
    }
}

/// Compact fund-flow summary: recent rows plus streak and totals.
pub(crate) fn fund_flow_summary(points: &[FundFlowPoint]) -> Value {
    let rows: Vec<Value> = points
        .iter()
        .map(|p| {
            json!([
                p.time.date().to_string(),
                (p.main_net / 1e4).round() as i64, // 万元
                r2(p.main_pct),
            ])
        })
        .collect();
    // Consecutive same-sign main_net days counting back from the latest row.
    let mut streak = 0i64;
    let mut direction = 0i64;
    for p in points.iter().rev() {
        let sign = if p.main_net > 0.0 {
            1
        } else if p.main_net < 0.0 {
            -1
        } else {
            0
        };
        if streak == 0 {
            direction = sign;
            streak = 1;
        } else if sign == direction {
            streak += 1;
        } else {
            break;
        }
    }
    let sum: f64 = points.iter().map(|p| p.main_net).sum();
    json!({
        "columns": ["date", "main_net_wan", "main_pct"],
        "rows": rows,
        "streak_days": streak,
        "streak_direction": if direction > 0 { "净流入" } else if direction < 0 { "净流出" } else { "持平" },
        "sum_main_net_wan": (sum / 1e4).round() as i64,
    })
}

// ---------------------------------------------------------------------
// get_market_breadth
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct NoArgs {}

/// Market-wide advance/decline counts.
struct GetMarketBreadth;

#[async_trait]
impl AgentTool for GetMarketBreadth {
    fn name(&self) -> &'static str {
        "get_market_breadth"
    }
    fn description(&self) -> &'static str {
        "获取市场宽度（上涨/下跌/平盘家数与涨跌比）"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<NoArgs>()
    }
    fn cache_ttl_secs(&self) -> i64 {
        60
    }
    async fn execute(&self, _args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let fetched = ctx.market.market_breadth().await?;
        let (source, fetched_at) = provenance(&fetched);
        let b = &fetched.data;
        let summary = json!({
            "up": b.up,
            "down": b.down,
            "flat": b.flat,
            "total": b.total,
            "ratio": r4(b.ratio()),
        });
        Ok(ToolResult {
            summary_json: summary.clone(),
            full_json: Some(summary),
            cache_key: String::new(),
            source,
            fetched_at,
        })
    }
}

// ---------------------------------------------------------------------
// search_stock
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchArgs {
    /// 股票名称关键字或代码片段
    keyword: String,
}

/// Symbol search by keyword or code fragment.
struct SearchStock;

#[async_trait]
impl AgentTool for SearchStock {
    fn name(&self) -> &'static str {
        "search_stock"
    }
    fn description(&self) -> &'static str {
        "按名称或代码搜索证券，返回候选代码列表"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<SearchArgs>()
    }
    fn cache_ttl_secs(&self) -> i64 {
        3600
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args: SearchArgs = parse_args(self.name(), args)?;
        let fetched = ctx.market.search(&args.keyword).await?;
        let (source, fetched_at) = provenance(&fetched);
        let hits: Vec<Value> = fetched
            .data
            .iter()
            .take(10)
            .map(|h| json!({"code": h.code, "name": h.name, "classify": h.classify}))
            .collect();
        Ok(ToolResult {
            summary_json: json!({"hits": hits, "hit_count": fetched.data.len()}),
            full_json: Some(serde_json::to_value(&fetched.data)?),
            cache_key: String::new(),
            source,
            fetched_at,
        })
    }
}

// ---------------------------------------------------------------------
// compare_stocks
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct CompareArgs {
    /// 要对比的6位证券代码列表（2-8只）
    symbols: Vec<String>,
}

/// One row of a comparison/scan table.
fn comparison_row(
    code: &str,
    name: Option<&str>,
    quote: Option<&tech::Quote>,
    signal: &Value,
) -> Value {
    let get = |k: &str| signal.get(k).cloned().unwrap_or(Value::Null);
    let mut row = serde_json::Map::new();
    row.insert("symbol".into(), json!(code));
    if let Some(name) = name {
        row.insert("name".into(), json!(name));
    }
    if let Some(q) = quote {
        row.insert("price".into(), json!(q.price));
        row.insert("pct".into(), json!(q.pct));
    }
    for key in ["score", "action", "confidence", "risk_level", "description"] {
        row.insert(key.into(), get(key));
    }
    Value::Object(row)
}

/// Side-by-side signal comparison across several stocks.
struct CompareStocks;

#[async_trait]
impl AgentTool for CompareStocks {
    fn name(&self) -> &'static str {
        "compare_stocks"
    }
    fn description(&self) -> &'static str {
        "多股对比：并行运行信号引擎，输出评分/建议/关键指标对比表"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<CompareArgs>()
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args: CompareArgs = parse_args(self.name(), args)?;
        if args.symbols.len() < 2 || args.symbols.len() > 8 {
            return Err(AgentError::InvalidArgs {
                tool: self.name().to_string(),
                msg: "symbols 需包含 2-8 个代码".to_string(),
            });
        }
        let breadth = ctx
            .market
            .market_breadth()
            .await
            .ok()
            .map(|b| to_tech_breadth(&b.data));
        let breadth = Arc::new(breadth);

        let rows: Vec<Result<(Value, String, String)>> =
            futures::stream::iter(args.symbols.iter().cloned())
                .map(|code| {
                    let ctx = ctx.clone();
                    let breadth = breadth.clone();
                    async move {
                        let symbol = parse_symbol(self.name(), &code)?;
                        let inputs = fetch_analysis_inputs(
                            &ctx,
                            self.name(),
                            &symbol,
                            astock_core::KlinePeriod::Day,
                            250,
                            true,
                        )
                        .await?;
                        let signal = run_engine(&inputs, breadth.as_ref().as_ref());
                        let name = inputs.quote.as_ref().map(|q| q.name.clone());
                        Ok::<_, AgentError>((
                            comparison_row(&code, name.as_deref(), inputs.quote.as_ref(), &signal),
                            inputs.source,
                            inputs.fetched_at,
                        ))
                    }
                })
                .buffer_unordered(4)
                .collect::<Vec<_>>()
                .await;

        let mut table = Vec::new();
        let mut errors = Vec::new();
        let mut source = "composite".to_string();
        let mut fetched_at = String::new();
        for row in rows {
            match row {
                Ok((value, src, at)) => {
                    source = src;
                    fetched_at = at;
                    table.push(value);
                }
                Err(e) => errors.push(e.to_string()),
            }
        }
        if table.is_empty() {
            return Err(tool_err(
                self.name(),
                format!("全部失败：{}", errors.join("; ")),
            ));
        }
        let summary = json!({"table": table, "errors": errors});
        Ok(ToolResult {
            summary_json: summary.clone(),
            full_json: Some(summary),
            cache_key: String::new(),
            source,
            fetched_at,
        })
    }
}

// ---------------------------------------------------------------------
// scan_market
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct ScanArgs {
    /// 返回评分最高的前N只，默认 10，上限 30
    top: Option<u32>,
    /// 参与评分的候选股数量（按成交额预筛），默认 50，上限 100
    candidates: Option<u32>,
}

/// Minimum turnover (CNY) for a stock to enter the scan candidate pool.
const SCAN_MIN_AMOUNT: f64 = 5e7;
/// TTL for per-stock scan rows.
const SCAN_ROW_TTL: i64 = 300;

/// Whole-market scan: rank candidates by the engine's composite score.
struct ScanMarket;

struct ScanOneOutcome {
    row: Value,
    cache_hit: bool,
    records: usize,
}

#[derive(Default)]
struct ScanProgressState {
    total: usize,
    completed: usize,
    succeeded: usize,
    failed: usize,
    cache_hits: usize,
    records: usize,
    active: BTreeMap<String, String>,
    recent_errors: Vec<String>,
}

impl ScanProgressState {
    fn snapshot(&self) -> ToolProgressDetail {
        ToolProgressDetail {
            completed: self.completed,
            total: self.total,
            succeeded: self.succeeded,
            failed: self.failed,
            cache_hits: self.cache_hits,
            records: self.records,
            active: self
                .active
                .iter()
                .map(|(label, stage)| ToolWorkItem {
                    label: label.clone(),
                    stage: stage.clone(),
                })
                .collect(),
            recent_errors: self.recent_errors.clone(),
        }
    }
}

#[async_trait]
impl AgentTool for ScanMarket {
    fn name(&self) -> &'static str {
        "scan_market"
    }
    fn description(&self) -> &'static str {
        "全市场扫描：按成交额预筛后并行运行信号引擎，返回评分最高的股票"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<ScanArgs>()
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args: ScanArgs = parse_args(self.name(), args)?;
        let top = args.top.unwrap_or(10).clamp(1, 30) as usize;
        let candidates = args.candidates.unwrap_or(50).clamp(1, 100) as usize;

        let list = ctx.market.all_a_shares().await?;
        let (source, fetched_at) = provenance(&list);
        let mut pool: Vec<_> = list
            .data
            .iter()
            .filter(|s| {
                s.price.is_some_and(|price| price > 0.0)
                    && s.amount.is_some_and(|amount| amount >= SCAN_MIN_AMOUNT)
            })
            .filter(|s| {
                Symbol::new(&s.code)
                    .map(|sym| !sym.is_etf())
                    .unwrap_or(false)
            })
            .collect();
        pool.sort_by(|a, b| {
            b.amount
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(&a.amount.unwrap_or(f64::NEG_INFINITY))
        });
        pool.truncate(candidates);

        let progress_state = Arc::new(Mutex::new(ScanProgressState {
            total: pool.len(),
            ..Default::default()
        }));
        ctx.report_progress(
            progress_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .snapshot(),
        );

        let breadth = ctx
            .market
            .market_breadth()
            .await
            .ok()
            .map(|b| to_tech_breadth(&b.data));
        let breadth = Arc::new(breadth);

        let results: Vec<Result<Value>> = futures::stream::iter(pool.into_iter().cloned())
            .map(|item| {
                let ctx = ctx.clone();
                let breadth = breadth.clone();
                let progress_state = Arc::clone(&progress_state);
                async move {
                    let label = format!("{} {}", item.code, item.name);
                    let starting = {
                        let mut state = progress_state
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        state.active.insert(
                            label.clone(),
                            "检查5分钟缓存；未命中则获取250根日K并计算指标".to_string(),
                        );
                        state.snapshot()
                    };
                    ctx.report_progress(starting);
                    let outcome = self
                        .scan_one(&ctx, &item.code, breadth.as_ref().as_ref())
                        .await;
                    let result = match outcome {
                        Ok(mut outcome) => {
                            if let Some(obj) = outcome.row.as_object_mut() {
                                obj.insert("name".into(), json!(item.name));
                                obj.insert("price".into(), json!(item.price));
                                obj.insert("pct".into(), json!(item.pct));
                            }
                            let mut state = progress_state
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            state.completed += 1;
                            state.succeeded += 1;
                            state.cache_hits += usize::from(outcome.cache_hit);
                            state.records += outcome.records;
                            state.active.remove(&label);
                            Ok(outcome.row)
                        }
                        Err(error) => {
                            let error_text = format!("{label}：{error}");
                            let mut state = progress_state
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            state.completed += 1;
                            state.failed += 1;
                            state.active.remove(&label);
                            state.recent_errors.push(error_text);
                            if state.recent_errors.len() > 20 {
                                state.recent_errors.remove(0);
                            }
                            Err(error)
                        }
                    };
                    let updated = progress_state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .snapshot();
                    ctx.report_progress(updated);
                    result
                }
            })
            .buffer_unordered(10)
            .collect()
            .await;

        let mut scored = Vec::new();
        let mut failed = 0usize;
        for r in results {
            match r {
                Ok(row) => scored.push(row),
                Err(_) => failed += 1,
            }
        }
        scored.sort_by(|a, b| {
            let sa = a.get("score").and_then(Value::as_i64).unwrap_or(0);
            let sb = b.get("score").and_then(Value::as_i64).unwrap_or(0);
            sb.cmp(&sa)
        });
        let scanned = scored.len();
        scored.truncate(top);

        let final_progress = progress_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .snapshot();

        let summary = json!({
            "criteria": format!("成交额≥{}万元取前{}只候选，按综合评分排序", (SCAN_MIN_AMOUNT / 1e4) as i64, candidates),
            "scanned": scanned,
            "failed": failed,
            "cache_hits": final_progress.cache_hits,
            "records": final_progress.records,
            "errors": final_progress.recent_errors,
            "top": scored,
        });
        Ok(ToolResult {
            summary_json: summary.clone(),
            full_json: Some(summary),
            cache_key: String::new(),
            source,
            fetched_at,
        })
    }
}

impl ScanMarket {
    /// Score one stock, read-through cached per symbol so repeated scans
    /// within the TTL do not recompute.
    async fn scan_one(
        &self,
        ctx: &ToolContext,
        code: &str,
        breadth: Option<&tech::Breadth>,
    ) -> Result<ScanOneOutcome> {
        let key = tool_cache_key("scan_stock", &json!({"symbol": code}));
        if let Some(entry) = ctx.storage.tool_cache_get(&key).await? {
            if let Ok(env) = serde_json::from_str::<CacheEnvelope>(&entry.result_json) {
                return Ok(ScanOneOutcome {
                    row: env.summary,
                    cache_hit: true,
                    records: 0,
                });
            }
        }
        let symbol = parse_symbol(self.name(), code)?;
        let inputs = fetch_analysis_inputs(
            ctx,
            self.name(),
            &symbol,
            astock_core::KlinePeriod::Day,
            250,
            false,
        )
        .await?;
        let signal = run_engine(&inputs, breadth);
        let records = inputs.klines.len();
        let row = comparison_row(code, None, None, &signal);
        let env = CacheEnvelope {
            summary: row.clone(),
            full: None,
            source: inputs.source,
            fetched_at: inputs.fetched_at,
        };
        let now = now_secs();
        ctx.storage
            .tool_cache_put(ToolCacheEntry {
                cache_key: key,
                tool: "scan_stock".to_string(),
                params_json: json!({"symbol": code}).to_string(),
                result_json: serde_json::to_string(&env)?,
                data_version: None,
                created_at: now,
                ttl_seconds: SCAN_ROW_TTL,
                accessed_at: now,
            })
            .await?;
        Ok(ScanOneOutcome {
            row,
            cache_hit: false,
            records,
        })
    }
}

// ---------------------------------------------------------------------
// get_cached_detail
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct CachedDetailArgs {
    /// 之前工具返回中的 cache_key
    cache_key: String,
}

/// Drill into a cached full payload by cache key.
struct GetCachedDetail;

#[async_trait]
impl AgentTool for GetCachedDetail {
    fn name(&self) -> &'static str {
        "get_cached_detail"
    }
    fn description(&self) -> &'static str {
        "按 cache_key 取回之前工具结果的完整数据（摘要不够用时下钻）"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<CachedDetailArgs>()
    }
    fn permission_domain(&self) -> ToolPermissionDomain {
        ToolPermissionDomain::ReadOnlyLocal
    }
    fn cacheable(&self) -> bool {
        false
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args: CachedDetailArgs = parse_args(self.name(), args)?;
        let entry = ctx.storage.tool_cache_get(&args.cache_key).await?;
        let summary = match entry {
            Some(entry) => match serde_json::from_str::<CacheEnvelope>(&entry.result_json) {
                Ok(env) => json!({
                    "found": true,
                    "cache_key": args.cache_key,
                    "tool": entry.tool,
                    "source": env.source,
                    "fetched_at": env.fetched_at,
                    "detail": env.full.unwrap_or(env.summary),
                }),
                // Non-envelope rows (e.g. written by other components): serve raw.
                Err(_) => json!({
                    "found": true,
                    "cache_key": args.cache_key,
                    "tool": entry.tool,
                    "detail": serde_json::from_str::<Value>(&entry.result_json)
                        .unwrap_or(Value::String(entry.result_json)),
                }),
            },
            None => json!({"found": false, "cache_key": args.cache_key}),
        };
        Ok(ToolResult {
            summary_json: summary,
            full_json: None,
            cache_key: String::new(),
            source: "tool_cache".to_string(),
            fetched_at: String::new(),
        })
    }
}

// ---------------------------------------------------------------------
// get_watchlist
// ---------------------------------------------------------------------

/// The user's watchlist across all groups, enriched with realtime quotes.
struct GetWatchlist;

#[async_trait]
impl AgentTool for GetWatchlist {
    fn name(&self) -> &'static str {
        "get_watchlist"
    }
    fn description(&self) -> &'static str {
        "获取用户自选股列表(含分组、最新价、涨跌幅)。当用户提到“我的自选股/持仓/我关注的股票”时必须先调用此工具确定具体股票。"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<NoArgs>()
    }
    fn permission_domain(&self) -> ToolPermissionDomain {
        ToolPermissionDomain::ReadOnlyLocal
    }
    fn cache_ttl_secs(&self) -> i64 {
        60
    }
    async fn execute(&self, _args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        // Aggregate every group exactly like the Tauri `watchlist_list`
        // command: group ascending, pinned first, then insertion order.
        let rows: Vec<(String, String, bool)> = ctx
            .storage
            .run(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT group_name, code, pinned FROM watchlist
                     ORDER BY group_name ASC, pinned DESC, added_at ASC",
                )?;
                let rows = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)? != 0,
                    ))
                })?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await?;

        // Quote enrichment, concurrency 4; failures degrade to null fields.
        // `buffered` (ordered) keeps the pinned-first group ordering.
        let items: Vec<Value> = futures::stream::iter(rows)
            .map(|(group, code, pinned)| {
                let ctx = ctx.clone();
                async move {
                    let quote = match Symbol::new(&code) {
                        Ok(symbol) => ctx.market.quote(&symbol).await.ok().map(|f| f.data),
                        Err(_) => None,
                    };
                    json!({
                        "group": group,
                        "code": code,
                        "pinned": pinned,
                        "name": quote.as_ref().map(|q| q.name.clone()),
                        "price": quote.as_ref().map(|q| r2(q.price)),
                        "pct": quote.as_ref().map(|q| r2(q.pct)),
                    })
                }
            })
            .buffered(4)
            .collect()
            .await;

        let summary = json!(items);
        Ok(ToolResult {
            summary_json: summary.clone(),
            full_json: Some(summary),
            cache_key: String::new(),
            source: "storage".to_string(),
            fetched_at: String::new(),
        })
    }
}

/// The v1 registry: all tools in stable declaration order.
pub fn default_registry() -> ToolRegistry {
    ToolRegistry::new(vec![
        Arc::new(GetQuote),
        Arc::new(GetKline),
        Arc::new(ComputeIndicators),
        Arc::new(RunFullAnalysis),
        Arc::new(RunChanlun),
        Arc::new(GetFundFlow),
        Arc::new(GetMarketBreadth),
        Arc::new(SearchStock),
        Arc::new(CompareStocks),
        Arc::new(ScanMarket),
        Arc::new(GetWatchlist),
        Arc::new(GetCachedDetail),
        Arc::new(crate::deep::GetFundamentals),
        Arc::new(crate::deep::RunValuation),
        Arc::new(crate::deep::AnalyzeEarningsDrivers),
        Arc::new(crate::deep::GetIndustryChain),
        Arc::new(crate::deep::RunSupplyChainShock),
        Arc::new(crate::deep::BuildRelationshipGraph),
        Arc::new(crate::deep::RunQuantResearch),
        Arc::new(crate::deep::RunBacktest),
        Arc::new(crate::deep::IterateStrategy),
        Arc::new(crate::deep::RunJoinQuantResearch),
        Arc::new(crate::deep::SearchWeb),
        Arc::new(crate::deep::FetchSourceDocument),
        Arc::new(crate::deep::ReadDocument),
        Arc::new(crate::deep::CompareSourceEvidence),
        Arc::new(crate::deep::ResearchDisclosures),
        Arc::new(crate::deep::ResearchGlobalTransmission),
        Arc::new(crate::deep::AnalyzeEventPriceIn),
        Arc::new(crate::deep::ResearchSupplyChainRelations),
        Arc::new(crate::deep::QueryGraphAsOf),
        Arc::new(crate::deep::ResearchNews),
        Arc::new(crate::deep::GetMarketRegime),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use chrono::{NaiveDate, NaiveDateTime};

    use astock_core::{
        Adjust, DataError, KlinePeriod, MarketBreadth, MinuteData, MinutePoint, SearchResult,
        Source, StockListItem, VolumeUnit,
    };
    use astock_market_data::DataProvider;
    use astock_storage::{Storage, StorageConfig};

    /// Canned market data for tool tests: no network.
    struct MockMarket {
        bars: Vec<Bar>,
        quote: Quote,
        flows: Vec<FundFlowPoint>,
        breadth: MarketBreadth,
        hits: Vec<SearchResult>,
        stocks: Vec<StockListItem>,
        minute: MinuteData,
    }

    fn mock_bars(n: usize) -> Vec<Bar> {
        let start = NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
        (0..n)
            .map(|i| {
                let base = 10.0 + i as f64 * 0.05 + (i as f64 * 0.7).sin() * 0.3;
                Bar {
                    date: start + chrono::Duration::days(i as i64),
                    open: base - 0.1,
                    close: base,
                    high: base + 0.2,
                    low: base - 0.2,
                    volume: 10_000.0 + i as f64,
                    volume_unit: VolumeUnit::Lots,
                    amount: Some(base * 10_000.0),
                    turnover: Some(1.5),
                    pct: Some(0.5),
                }
            })
            .collect()
    }

    fn mock_minute(n: usize) -> MinuteData {
        let start = NaiveDateTime::parse_from_str("2026-08-21 09:30", "%Y-%m-%d %H:%M").unwrap();
        let points = (0..n)
            .map(|i| MinutePoint {
                time: start + chrono::Duration::minutes(i as i64),
                price: 10.0 + (i as f64 * 0.3).sin() * 0.2,
                avg_price: 10.0,
                volume: 100.0 + i as f64,
            })
            .collect();
        MinuteData {
            points,
            pre_close: 10.0,
            name: "测试股".to_string(),
            high: 10.5,
            low: 9.5,
        }
    }

    fn mock_market() -> MockMarket {
        MockMarket {
            bars: mock_bars(300),
            quote: Quote {
                symbol: "600519".to_string(),
                name: "贵州茅台".to_string(),
                price: 1800.5,
                open: 1790.0,
                high: 1810.0,
                low: 1785.0,
                pre_close: 1795.0,
                volume: 20_000.0,
                amount: 3.6e9,
                change: 5.5,
                pct: 0.31,
                turnover: Some(0.4),
                timestamp: chrono::DateTime::from_timestamp_millis(1_700_000_000_000).unwrap(),
                field_provenance: Default::default(),
            },
            flows: (0..10)
                .map(|i| FundFlowPoint {
                    time: NaiveDate::from_ymd_opt(2026, 8, 10)
                        .unwrap()
                        .and_hms_opt(0, 0, 0)
                        .unwrap()
                        + chrono::Duration::days(i),
                    main_net: if i < 7 { -1e7 } else { 2e7 },
                    small_net: 0.0,
                    medium_net: 0.0,
                    large_net: 1e7,
                    super_large_net: 1e7,
                    main_pct: 1.2,
                })
                .collect(),
            breadth: MarketBreadth {
                up: 3000,
                down: 2000,
                flat: 100,
                total: 5100,
            },
            hits: vec![SearchResult {
                code: "600519".to_string(),
                name: "贵州茅台".to_string(),
                classify: "AStock".to_string(),
            }],
            stocks: vec![
                StockListItem {
                    code: "600519".to_string(),
                    name: "贵州茅台".to_string(),
                    price: Some(1800.5),
                    pct: Some(0.31),
                    amount: Some(3.6e9),
                },
                StockListItem {
                    code: "000001".to_string(),
                    name: "平安银行".to_string(),
                    price: Some(12.0),
                    pct: Some(-0.5),
                    amount: Some(2e9),
                },
                StockListItem {
                    code: "510300".to_string(), // ETF: filtered out
                    name: "沪深300ETF".to_string(),
                    price: Some(4.0),
                    pct: Some(0.1),
                    amount: Some(5e9),
                },
            ],
            minute: mock_minute(240),
        }
    }

    #[async_trait]
    impl DataProvider for MockMarket {
        fn name(&self) -> &'static str {
            "mock"
        }
        async fn kline(
            &self,
            _symbol: &Symbol,
            _period: KlinePeriod,
            _adjust: Adjust,
            _count: u32,
        ) -> std::result::Result<Fetched<Vec<Bar>>, DataError> {
            Ok(Fetched::now(self.bars.clone(), Source::EastMoney))
        }
        async fn quote(&self, _symbol: &Symbol) -> std::result::Result<Fetched<Quote>, DataError> {
            Ok(Fetched::now(self.quote.clone(), Source::EastMoney))
        }
        async fn search(
            &self,
            _keyword: &str,
        ) -> std::result::Result<Fetched<Vec<SearchResult>>, DataError> {
            Ok(Fetched::now(self.hits.clone(), Source::EastMoney))
        }
        async fn fund_flow_daily(
            &self,
            _symbol: &Symbol,
            _days: u32,
        ) -> std::result::Result<Fetched<Vec<FundFlowPoint>>, DataError> {
            Ok(Fetched::now(self.flows.clone(), Source::EastMoney))
        }
        async fn minute(
            &self,
            _symbol: &Symbol,
        ) -> std::result::Result<Fetched<MinuteData>, DataError> {
            Ok(Fetched::now(self.minute.clone(), Source::EastMoney))
        }
        async fn all_a_shares(
            &self,
        ) -> std::result::Result<Fetched<Vec<StockListItem>>, DataError> {
            Ok(Fetched::now(self.stocks.clone(), Source::EastMoney))
        }
        async fn market_breadth(&self) -> std::result::Result<Fetched<MarketBreadth>, DataError> {
            Ok(Fetched::now(self.breadth, Source::EastMoney))
        }
        async fn index_kline(
            &self,
            _index_secid: &str,
            _count: u32,
        ) -> std::result::Result<Fetched<Vec<Bar>>, DataError> {
            Ok(Fetched::now(self.bars.clone(), Source::EastMoney))
        }
    }

    fn test_ctx() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        (
            dir,
            ToolContext {
                market: Arc::new(mock_market()),
                storage,
                graph: None,
                fundamental: None,
                joinquant: None,
                minimax_search: None,
                finance_news: None,
                iwencai: None,
                progress: None,
            },
        )
    }

    async fn dispatch(
        registry: &ToolRegistry,
        ctx: &ToolContext,
        name: &str,
        args: Value,
    ) -> ToolResult {
        registry
            .dispatch(name, args, ctx)
            .await
            .unwrap_or_else(|e| panic!("{name} failed: {e}"))
    }

    #[tokio::test]
    async fn tool_schemas_are_valid() {
        let registry = default_registry();
        assert_eq!(registry.len(), 33);
        let mut names = Vec::new();
        for spec in registry.specs() {
            assert_eq!(spec.kind, "function");
            let params = &spec.function.parameters;
            assert_eq!(params.get("type").and_then(Value::as_str), Some("object"));
            if params.get("required").is_some() {
                assert!(
                    params.get("properties").is_some(),
                    "{} properties",
                    spec.function.name
                );
            }
            assert!(!spec
                .function
                .description
                .as_deref()
                .unwrap_or("")
                .is_empty());
            names.push(spec.function.name.clone());
        }
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "tool names must be unique");
        for expected in [
            "get_quote",
            "get_kline",
            "compute_indicators",
            "run_full_analysis",
            "run_chanlun",
            "get_fund_flow",
            "get_market_breadth",
            "search_stock",
            "compare_stocks",
            "scan_market",
            "get_watchlist",
            "get_cached_detail",
            "get_fundamentals",
            "run_valuation",
            "analyze_earnings_drivers",
            "get_industry_chain",
            "run_supply_chain_shock",
            "build_relationship_graph",
            "run_quant_research",
            "run_backtest",
            "iterate_strategy",
            "run_joinquant_research",
            "search_web",
            "fetch_source_document",
            "read_document",
            "compare_source_evidence",
            "research_disclosures",
            "research_global_transmission",
            "analyze_event_price_in",
            "research_supply_chain_relations",
            "query_graph_as_of",
            "research_news",
            "get_market_regime",
        ] {
            assert!(names.contains(&expected.to_string()), "missing {expected}");
        }
        // Required fields are derived from non-Option struct fields.
        let registry = default_registry();
        let quote = registry.get("get_quote").unwrap();
        let schema = quote.parameters_schema();
        assert_eq!(
            schema.get("required").and_then(Value::as_array).unwrap(),
            &vec![json!("symbol")]
        );
    }

    #[tokio::test]
    async fn every_registered_tool_handles_malformed_arguments_without_panicking_or_leaking() {
        let (_dir, ctx) = test_ctx();
        let registry = default_registry();
        let malformed = json!("api_key=must-not-leak");

        for name in registry.names() {
            match registry.dispatch(name, malformed.clone(), &ctx).await {
                Ok(result) => {
                    // Argument-free tools intentionally ignore the payload.
                    let diagnostic = serde_json::to_string(&result.summary_json).unwrap();
                    assert!(
                        !result.summary_json.is_null(),
                        "{name} returned a null result"
                    );
                    assert!(!diagnostic.contains("must-not-leak"));
                }
                Err(error) => {
                    let diagnostic = error.to_string();
                    assert!(
                        !diagnostic.trim().is_empty(),
                        "{name} returned a blank error"
                    );
                    assert!(
                        !diagnostic.contains("must-not-leak"),
                        "{name} leaked argument contents in its error: {diagnostic}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn get_quote_summary_and_provenance() {
        let (_dir, ctx) = test_ctx();
        let registry = default_registry();
        let r = dispatch(&registry, &ctx, "get_quote", json!({"symbol": "600519"})).await;
        assert_eq!(r.summary_json["price"], json!(1800.5));
        assert_eq!(r.summary_json["name"], json!("贵州茅台"));
        assert_eq!(r.source, "eastmoney");
        assert!(!r.fetched_at.is_empty());
        assert!(r.cache_key.starts_with("get_quote:"));
        assert!(r.full_json.is_some());
    }

    #[tokio::test]
    async fn get_kline_compacts_to_stats_plus_tail() {
        let (_dir, ctx) = test_ctx();
        let registry = default_registry();
        let r = dispatch(
            &registry,
            &ctx,
            "get_kline",
            json!({"symbol": "600519", "period": "day", "count": 300}),
        )
        .await;
        assert_eq!(r.summary_json["stats"]["bar_count"], json!(300));
        let tail = r.summary_json["tail"].as_array().unwrap();
        assert_eq!(tail.len(), KLINE_TAIL);
        let full = r.full_json.unwrap();
        assert_eq!(full["bars"].as_array().unwrap().len(), 300);
        // Tail ends at the latest bar.
        let last = mock_bars(300).pop().unwrap();
        assert_eq!(r.summary_json["stats"]["last_close"], json!(r2(last.close)));
    }

    #[tokio::test]
    async fn compute_indicators_reports_all_families() {
        let (_dir, ctx) = test_ctx();
        let registry = default_registry();
        let r = dispatch(
            &registry,
            &ctx,
            "compute_indicators",
            json!({"symbol": "600519", "count": 300}),
        )
        .await;
        for key in ["ma", "macd", "rsi14", "kdj", "boll"] {
            assert!(r.summary_json.get(key).is_some(), "missing {key}");
        }
        assert!(r.summary_json["ma"]["ma5"].is_number());
        assert!(r.summary_json["macd"]["dif"].is_number());
        assert!(r.summary_json["rsi14"].is_number());
        assert!(r.summary_json["kdj"]["k"].is_number());
        assert!(r.summary_json["boll"]["mid"].is_number());
        let full = r.full_json.unwrap();
        assert_eq!(full["macd_bar_tail"].as_array().unwrap().len(), 60);
    }

    #[tokio::test]
    async fn run_full_analysis_trims_engine_json() {
        let (_dir, ctx) = test_ctx();
        let registry = default_registry();
        let r = dispatch(
            &registry,
            &ctx,
            "run_full_analysis",
            json!({"symbol": "600519"}),
        )
        .await;
        let s = &r.summary_json;
        assert!(s["action"].is_string());
        assert!(s["score"].is_number());
        assert!(s["trade_plan"].is_object());
        // Bulky per-module detail stays out of the summary...
        assert!(s.get("trend").is_none());
        assert!(s.get("canslim").is_none());
        // ...and lives in the full payload.
        let full = r.full_json.unwrap();
        assert!(full.get("trend").is_some());
        assert!(full.get("canslim").is_some());
    }

    #[tokio::test]
    async fn run_chanlun_daily_and_minute() {
        let (_dir, ctx) = test_ctx();
        let registry = default_registry();
        let daily = dispatch(&registry, &ctx, "run_chanlun", json!({"symbol": "600519"})).await;
        assert!(daily.summary_json["current_state"].is_string());
        assert!(daily.summary_json["fractal_count"].is_number());
        assert!(daily.summary_json.get("zhongshu_count").is_some());
        let full = daily.full_json.unwrap();
        assert!(
            full.get("chart_strokes").is_some(),
            "overlay kept in full payload"
        );

        let minute = dispatch(
            &registry,
            &ctx,
            "run_chanlun",
            json!({"symbol": "600519", "period": "minute"}),
        )
        .await;
        assert!(minute.summary_json["signal_count"].is_number());
        assert!(minute.summary_json.get("zhongshu_count").is_none());
    }

    #[test]
    fn fund_flow_summary_computes_streak() {
        let points: Vec<FundFlowPoint> = [-1.0, -2.0, 3.0, 4.0, 5.0]
            .iter()
            .enumerate()
            .map(|(i, net)| FundFlowPoint {
                time: NaiveDate::from_ymd_opt(2026, 8, 10)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    + chrono::Duration::days(i as i64),
                main_net: net * 1e4,
                small_net: 0.0,
                medium_net: 0.0,
                large_net: 0.0,
                super_large_net: 0.0,
                main_pct: 0.5,
            })
            .collect();
        let s = fund_flow_summary(&points);
        assert_eq!(s["streak_days"], json!(3));
        assert_eq!(s["streak_direction"], json!("净流入"));
        assert_eq!(s["sum_main_net_wan"], json!(9));
        assert_eq!(s["rows"].as_array().unwrap().len(), 5);
    }

    #[tokio::test]
    async fn fund_flow_tool_uses_summary() {
        let (_dir, ctx) = test_ctx();
        let registry = default_registry();
        let r = dispatch(
            &registry,
            &ctx,
            "get_fund_flow",
            json!({"symbol": "600519", "days": 10}),
        )
        .await;
        // Mock: last three days are inflows.
        assert_eq!(r.summary_json["streak_days"], json!(3));
        assert_eq!(r.summary_json["streak_direction"], json!("净流入"));
    }

    #[tokio::test]
    async fn breadth_and_search() {
        let (_dir, ctx) = test_ctx();
        let registry = default_registry();
        let b = dispatch(&registry, &ctx, "get_market_breadth", json!({})).await;
        assert_eq!(b.summary_json["up"], json!(3000));
        assert_eq!(b.summary_json["ratio"], json!(0.6));

        let s = dispatch(&registry, &ctx, "search_stock", json!({"keyword": "茅台"})).await;
        assert_eq!(s.summary_json["hits"][0]["code"], json!("600519"));
    }

    #[tokio::test]
    async fn compare_stocks_builds_table() {
        let (_dir, ctx) = test_ctx();
        let registry = default_registry();
        let r = dispatch(
            &registry,
            &ctx,
            "compare_stocks",
            json!({"symbols": ["600519", "000001"]}),
        )
        .await;
        let table = r.summary_json["table"].as_array().unwrap();
        assert_eq!(table.len(), 2);
        assert!(table[0]["score"].is_number());
        assert!(table[0]["action"].is_string());
        assert!(table[0]["price"].is_number());
    }

    #[tokio::test]
    async fn scan_market_ranks_by_score() {
        let (_dir, ctx) = test_ctx();
        let progress = Arc::new(Mutex::new(Vec::<ToolProgressDetail>::new()));
        let progress_sink = Arc::clone(&progress);
        let ctx = ctx.with_progress_reporter(Arc::new(move |detail| {
            progress_sink
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(detail);
        }));
        let registry = default_registry();
        let r = dispatch(
            &registry,
            &ctx,
            "scan_market",
            json!({"top": 10, "candidates": 50}),
        )
        .await;
        // The ETF (510300) is filtered out of the pool.
        assert_eq!(r.summary_json["scanned"], json!(2));
        let top = r.summary_json["top"].as_array().unwrap();
        assert_eq!(top.len(), 2);
        let first = top[0]["score"].as_i64().unwrap();
        let second = top[1]["score"].as_i64().unwrap();
        assert!(first >= second);
        let snapshot_count = {
            let snapshots = progress
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let final_snapshot = snapshots.last().unwrap();
            assert_eq!(final_snapshot.total, 2);
            assert_eq!(final_snapshot.completed, 2);
            assert_eq!(final_snapshot.succeeded, 2);
            assert_eq!(final_snapshot.failed, 0);
            assert!(final_snapshot.records >= MIN_ANALYSIS_BARS * 2);
            assert!(final_snapshot.active.is_empty());
            snapshots.len()
        };

        // Second dispatch is served from the read-through cache.
        let again = dispatch(
            &registry,
            &ctx,
            "scan_market",
            json!({"top": 10, "candidates": 50}),
        )
        .await;
        // Quality age is deliberately recomputed at read time, so crossing a
        // one-second wall-clock boundary must not make this cache test flaky.
        let mut first_summary = r.summary_json.clone();
        let mut cached_summary = again.summary_json.clone();
        for summary in [&mut first_summary, &mut cached_summary] {
            let age = summary["data_quality"]["age_secs"].as_u64().unwrap();
            assert!(age <= 2, "fresh cached scan unexpectedly aged {age}s");
            summary["data_quality"]
                .as_object_mut()
                .unwrap()
                .remove("age_secs");
        }
        assert_eq!(cached_summary, first_summary);
        assert_eq!(
            progress
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len(),
            snapshot_count,
            "outer tool cache should avoid replaying internal scan work"
        );
    }

    /// Quotes only for 600519; every other code fails, to exercise the
    /// failure-tolerant enrichment of `get_watchlist`.
    struct PartialQuoteMarket;

    #[async_trait]
    impl DataProvider for PartialQuoteMarket {
        fn name(&self) -> &'static str {
            "partial"
        }
        async fn quote(&self, symbol: &Symbol) -> std::result::Result<Fetched<Quote>, DataError> {
            if symbol.code() == "600519" {
                Ok(Fetched::now(mock_market().quote, Source::EastMoney))
            } else {
                Err(DataError::Empty(format!("no quote for {}", symbol.code())))
            }
        }
    }

    #[tokio::test]
    async fn get_watchlist_aggregates_groups_and_tolerates_quote_failures() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        storage.watchlist_add("core", "600519").await.unwrap();
        storage.watchlist_add("core", "000001").await.unwrap();
        storage
            .watchlist_set_pinned("core", "000001", true)
            .await
            .unwrap();
        storage.watchlist_add("watch", "300750").await.unwrap();
        let ctx = ToolContext {
            market: Arc::new(PartialQuoteMarket),
            storage,
            graph: None,
            fundamental: None,
            joinquant: None,
            minimax_search: None,
            finance_news: None,
            iwencai: None,
            progress: None,
        };
        let registry = default_registry();
        let r = dispatch(&registry, &ctx, "get_watchlist", json!({})).await;

        let items = r.summary_json["data"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        // Group ascending; pinned first within a group.
        assert_eq!(items[0]["group"], json!("core"));
        assert_eq!(items[0]["code"], json!("000001"));
        assert_eq!(items[0]["pinned"], json!(true));
        // Quote failure degrades to null enrichment, not a tool error.
        assert!(items[0]["name"].is_null());
        assert!(items[0]["price"].is_null());
        assert_eq!(items[1]["code"], json!("600519"));
        assert_eq!(items[1]["name"], json!("贵州茅台"));
        assert_eq!(items[1]["price"], json!(1800.5));
        assert_eq!(items[1]["pct"], json!(0.31));
        assert_eq!(items[2]["group"], json!("watch"));
        assert_eq!(items[2]["code"], json!("300750"));
        assert!(r.full_json.is_some());
        assert!(r.cache_key.starts_with("get_watchlist:"));

        // Empty watchlist → empty array, still not an error.
        let (_dir2, ctx2) = test_ctx();
        let empty = dispatch(&registry, &ctx2, "get_watchlist", json!({})).await;
        assert_eq!(empty.summary_json["data"].as_array().unwrap().len(), 0);
        assert!(empty.summary_json["data_quality"].is_object());
    }

    #[tokio::test]
    async fn cached_detail_drills_into_full_payload() {
        let (_dir, ctx) = test_ctx();
        let registry = default_registry();
        let r = dispatch(&registry, &ctx, "get_quote", json!({"symbol": "600519"})).await;
        let detail = dispatch(
            &registry,
            &ctx,
            "get_cached_detail",
            json!({"cache_key": r.cache_key}),
        )
        .await;
        assert_eq!(detail.summary_json["found"], json!(true));
        assert_eq!(detail.summary_json["detail"]["price"], json!(1800.5));

        let missing = dispatch(
            &registry,
            &ctx,
            "get_cached_detail",
            json!({"cache_key": "get_quote:0000000000000000"}),
        )
        .await;
        assert_eq!(missing.summary_json["found"], json!(false));
    }

    #[test]
    fn analysis_summary_keeps_only_compact_keys() {
        let signal = json!({
            "action": "买入",
            "score": 72,
            "confidence": 60,
            "trend": {"huge": "x".repeat(1000)},
            "patterns": [1, 2, 3],
        });
        let s = analysis_summary(&signal);
        assert_eq!(s["action"], json!("买入"));
        assert_eq!(s["score"], json!(72));
        assert!(s.get("trend").is_none());
        assert!(s.get("patterns").is_none());
    }
}
