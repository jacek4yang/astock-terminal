//! Analysis-engine commands (docs/command-contract.md §分析引擎).

use astock_core::{Adjust, Bar, FundFlowPoint, KlinePeriod, Quote as CoreQuote, Symbol};
use astock_market_data::{DataProvider, MarketData};
use astock_trading_rules::RuleSet;
use serde_json::Value;
use tauri::State;

use crate::cache_path;
use crate::convert;
use crate::error::CmdError;
use crate::state::AppState;

use super::market::{clamp_count, parse_period, parse_symbol};

/// Kline depth used by the signal engine (legacy value).
pub(crate) const ANALYZE_KLINE_COUNT: u32 = 250;
/// Fund-flow history depth used by the signal engine (legacy value).
pub(crate) const ANALYZE_FLOW_DAYS: u32 = 30;
/// EastMoney secid of the Shanghai Composite (上证指数) for CANSLIM M.
pub(crate) const INDEX_SECID: &str = "1.000001";
/// Index kline depth for the CANSLIM M score (legacy value).
pub(crate) const INDEX_KLINE_COUNT: u32 = 60;

/// Shared market context for the signal pipeline: index klines for the
/// CANSLIM M score plus the market-breadth snapshot. Both are best-effort —
/// a failure degrades to `None` exactly like the legacy implementation.
pub(crate) async fn fetch_shared_context(
    market: &MarketData,
) -> (
    Option<Vec<astock_technical::Kline>>,
    Option<astock_technical::Breadth>,
) {
    let (index, breadth) = tokio::join!(
        market.index_kline(INDEX_SECID, INDEX_KLINE_COUNT),
        market.market_breadth()
    );
    let index = match index {
        Ok(f) => Some(convert::bars_to_klines(&f.data)),
        Err(e) => {
            tracing::warn!(error = %e, "index kline unavailable; CANSLIM M uses stock MAs");
            None
        }
    };
    let breadth = match breadth {
        Ok(f) => Some(convert::breadth_to_technical(&f.data)),
        Err(e) => {
            tracing::warn!(error = %e, "market breadth unavailable; skipping M adjustment");
            None
        }
    };
    (index, breadth)
}

/// Fetch kline + quote + fund flow for one symbol and run the full signal
/// pipeline. Returns the signal JSON plus the quote (for the caller's
/// display name); kline/quote failures are hard errors, fund-flow failure
/// degrades to `None` as in the legacy code. `min_bars` rejects symbols
/// with too little history (the scan uses the legacy threshold of 30; the
/// `analyze` command passes 0 to stay permissive).
pub(crate) async fn analyze_symbol(
    market: &MarketData,
    symbol: &Symbol,
    period: KlinePeriod,
    index_klines: Option<&[astock_technical::Kline]>,
    breadth: Option<&astock_technical::Breadth>,
    min_bars: usize,
    rules: Option<&RuleSet>,
) -> Result<(Value, Option<CoreQuote>), CmdError> {
    let klines_fetched = market
        .kline(symbol, period, Adjust::Qfq, ANALYZE_KLINE_COUNT)
        .await?;
    if klines_fetched.data.len() < min_bars.max(1) {
        return Err(CmdError::new(
            "insufficient_data",
            format!(
                "only {} kline bars for {symbol} (need {min_bars})",
                klines_fetched.data.len()
            ),
        ));
    }
    let (quote, flows) = tokio::join!(
        market.quote(symbol),
        market.fund_flow_daily(symbol, ANALYZE_FLOW_DAYS)
    );
    let quote = match quote {
        Ok(f) => Some(f.data),
        Err(e) => {
            tracing::debug!(%symbol, error = %e, "quote unavailable for analysis");
            None
        }
    };
    let flows = match flows {
        Ok(f) => Some(f.data),
        Err(e) => {
            tracing::debug!(%symbol, error = %e, "fund flow unavailable for analysis");
            None
        }
    };

    let mut signal = run_signal_pipeline(
        &klines_fetched.data,
        quote.as_ref(),
        flows.as_deref(),
        index_klines,
        breadth,
    );
    if let (Some(rules), Some(quote)) = (rules, quote.as_ref()) {
        attach_manual_plan(
            &mut signal,
            symbol,
            quote,
            &klines_fetched.data,
            rules,
            &klines_fetched.source.to_string(),
        );
    }
    Ok((signal, quote))
}

/// Attach the production manual-execution playbook without changing the
/// golden-tested legacy engine. Concrete levels come only from bars and the
/// deterministic signal, while sessions/limits/lots come from RuleSet data.
pub(crate) fn attach_manual_plan(
    signal: &mut Value,
    symbol: &Symbol,
    quote: &CoreQuote,
    bars: &[Bar],
    rules: &RuleSet,
    source: &str,
) {
    let auction = &rules.data.auction;
    let sessions = astock_technical::SessionSchedule {
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
    let constraints = astock_technical::TradingConstraints {
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
    let klines = convert::bars_to_klines(bars);
    let generated_at = cache_path::shanghai_now().to_rfc3339();
    let Some(plan) = astock_technical::build_manual_trading_plan(
        symbol.code(),
        &quote.name,
        &klines,
        signal,
        &sessions,
        &constraints,
        &generated_at,
        source,
    ) else {
        return;
    };
    let Ok(plan_json) = serde_json::to_value(&plan) else {
        return;
    };
    let Some(object) = signal.as_object_mut() else {
        return;
    };
    object.insert("manual_plan".to_string(), plan_json);
    object.insert(
        "plain_summary".to_string(),
        Value::String(format!(
            "{}；反方条件：{}。本方案只在检查点条件成立时供人工执行。",
            plan.thesis, plan.counter_thesis
        )),
    );
    if let Some(trade_plan) = object.get_mut("trade_plan").and_then(Value::as_object_mut) {
        trade_plan.insert(
            "entry_price".to_string(),
            serde_json::json!((plan.entry_zone_low + plan.entry_zone_high) / 2.0),
        );
        trade_plan.insert("stop_loss".to_string(), serde_json::json!(plan.stop_loss));
        trade_plan.insert(
            "target_price".to_string(),
            serde_json::json!(plan.target_price),
        );
        trade_plan.insert(
            "risk_reward_ratio".to_string(),
            serde_json::json!(plan.risk_reward_ratio),
        );
        trade_plan.insert(
            "max_loss_pct".to_string(),
            serde_json::json!(plan.risk_budget_pct),
        );
        trade_plan.insert(
            "position_size".to_string(),
            Value::String(plan.position_guidance.clone()),
        );
        trade_plan.insert(
            "holding_period".to_string(),
            Value::String(plan.expected_holding_period.clone()),
        );
        trade_plan.insert(
            "notes".to_string(),
            Value::String(format!("{}；{}", plan.stop_basis, plan.target_basis)),
        );
    }
}

/// Run the technical signal pipeline on bars the caller already has (the
/// bundle command derives its analysis from the same kline payload it
/// returns, avoiding a second fetch).
pub(crate) fn run_signal_pipeline(
    bars: &[Bar],
    quote: Option<&CoreQuote>,
    flows: Option<&[FundFlowPoint]>,
    index_klines: Option<&[astock_technical::Kline]>,
    breadth: Option<&astock_technical::Breadth>,
) -> Value {
    let klines = convert::bars_to_klines(bars);
    let tech_quote = quote.map(convert::quote_to_technical);
    let tech_flows = flows.map(convert::flows_to_technical);
    astock_technical::analyze(
        &klines,
        tech_quote.as_ref(),
        tech_flows.as_deref(),
        index_klines,
        breadth,
    )
}

/// Run daily/weekly/monthly Chan theory analysis on bars the caller already
/// has (shared by `chanlun_daily` and the bundle command).
pub(crate) fn chanlun_from_bars(symbol: &Symbol, bars: &[Bar]) -> Result<Value, CmdError> {
    if bars.is_empty() {
        return Err(CmdError::new(
            "empty",
            format!("no kline data for {symbol}"),
        ));
    }
    let dates: Vec<String> = bars.iter().map(|b| b.date.to_string()).collect();
    let opens: Vec<f64> = bars.iter().map(|b| b.open).collect();
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let highs: Vec<f64> = bars.iter().map(|b| b.high).collect();
    let lows: Vec<f64> = bars.iter().map(|b| b.low).collect();
    let volumes: Vec<f64> = bars.iter().map(|b| b.volume).collect();
    let result = astock_chanlun::daily::analyze_chanlun_daily(
        &dates, &opens, &closes, &highs, &lows, &volumes,
    );
    Ok(astock_chanlun::daily::daily_result_to_dict(&result))
}

/// Full signal pipeline for one symbol — same JSON shape as the legacy
/// `signal_to_dict` + breadth M-score adjustment + optimization.
///
/// Results are cached in the storage `tool_cache` (60s during a trading
/// session, 4h after close; the key carries the cached kline last-bar date
/// so a fresh bar invalidates stale results).
#[tauri::command(rename_all = "snake_case")]
pub async fn analyze(
    state: State<'_, AppState>,
    symbol: String,
    period: String,
) -> Result<Value, CmdError> {
    let symbol = parse_symbol(&symbol)?;
    let period = parse_period(&period)?;
    let now = cache_path::shanghai_now().naive_local();
    let data_version = state
        .storage
        .last_bar_date(
            symbol.code(),
            cache_path::period_token(period),
            cache_path::adjust_token(Adjust::Qfq),
        )
        .await
        .ok()
        .flatten()
        .map_or_else(|| "none".to_string(), |d| d.to_string());
    let key = cache_path::analysis_cache_key(
        "analyze",
        &symbol,
        cache_path::period_token(period),
        &data_version,
    );
    if let Some(hit) = cache_path::tool_cache_get_json(&state.storage, &key).await {
        return Ok(hit);
    }
    let (index_klines, breadth) = fetch_shared_context(&state.market).await;
    let (signal, _quote) = analyze_symbol(
        &state.market,
        &symbol,
        period,
        index_klines.as_deref(),
        breadth.as_ref(),
        0,
        Some(&state.rules),
    )
    .await?;
    cache_path::tool_cache_put_json(
        &state.storage,
        &key,
        "analyze",
        serde_json::json!({"symbol": symbol.code(), "period": cache_path::period_token(period)}),
        Some(data_version),
        cache_path::analysis_ttl_secs(&state.rules, now),
        &signal,
    )
    .await;
    Ok(signal)
}

/// Daily/weekly/monthly Chan theory (缠论) analysis. Short-cached like
/// [`analyze`] (60s trading / 4h post-close, keyed on the kline last-bar
/// date and all parameters).
#[tauri::command(rename_all = "snake_case")]
pub async fn chanlun_daily(
    state: State<'_, AppState>,
    symbol: String,
    period: String,
    count: u32,
) -> Result<Value, CmdError> {
    let symbol = parse_symbol(&symbol)?;
    let period = parse_period(&period)?;
    let count = clamp_count(count);
    let now = cache_path::shanghai_now().naive_local();
    let data_version = state
        .storage
        .last_bar_date(
            symbol.code(),
            cache_path::period_token(period),
            cache_path::adjust_token(Adjust::Qfq),
        )
        .await
        .ok()
        .flatten()
        .map_or_else(|| "none".to_string(), |d| d.to_string());
    let key = cache_path::analysis_cache_key(
        "chanlun_daily",
        &symbol,
        &format!("{}|{count}", cache_path::period_token(period)),
        &data_version,
    );
    if let Some(hit) = cache_path::tool_cache_get_json(&state.storage, &key).await {
        return Ok(hit);
    }
    let fetched = state
        .market
        .kline(&symbol, period, Adjust::Qfq, count)
        .await?;
    let result = chanlun_from_bars(&symbol, &fetched.data)?;
    cache_path::tool_cache_put_json(
        &state.storage,
        &key,
        "chanlun_daily",
        serde_json::json!({
            "symbol": symbol.code(),
            "period": cache_path::period_token(period),
            "count": count,
        }),
        Some(data_version),
        cache_path::analysis_ttl_secs(&state.rules, now),
        &result,
    )
    .await;
    Ok(result)
}

/// Minute-level Chan theory analysis on today's intraday (分时) series.
#[tauri::command(rename_all = "snake_case")]
pub async fn chanlun_minute(state: State<'_, AppState>, symbol: String) -> Result<Value, CmdError> {
    let symbol = parse_symbol(&symbol)?;
    let fetched = state.market.minute(&symbol).await?;
    if fetched.data.points.is_empty() {
        return Err(CmdError::new(
            "empty",
            format!("no minute data for {symbol} (market closed?)"),
        ));
    }
    let times: Vec<String> = fetched
        .data
        .points
        .iter()
        .map(|p| p.time.format("%H:%M").to_string())
        .collect();
    let prices: Vec<f64> = fetched.data.points.iter().map(|p| p.price).collect();
    let volumes: Vec<f64> = fetched.data.points.iter().map(|p| p.volume).collect();
    let result = astock_chanlun::minute::analyze_chanlun_minute(&times, &prices, &volumes);
    Ok(astock_chanlun::minute::signals_to_dict(&result))
}
