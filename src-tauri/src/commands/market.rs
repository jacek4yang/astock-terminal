//! Market-data commands (docs/command-contract.md §行情数据).

use astock_core::{Adjust, Bar, FundFlowPoint, KlinePeriod, MinuteData, Symbol};
use astock_market_data::DataProvider;
use serde::Serialize;
use tauri::State;

use crate::error::CmdError;
use crate::state::AppState;

/// Hard cap on requested kline length.
const MAX_KLINE_COUNT: u32 = 10_000;

/// Parse the contract period token.
pub(crate) fn parse_period(period: &str) -> Result<KlinePeriod, CmdError> {
    match period {
        "day" => Ok(KlinePeriod::Day),
        "week" => Ok(KlinePeriod::Week),
        "month" => Ok(KlinePeriod::Month),
        other => Err(CmdError::new(
            "invalid_param",
            format!("unknown period {other:?}; expected day|week|month"),
        )),
    }
}

/// Parse the contract adjust token.
pub(crate) fn parse_adjust(adjust: &str) -> Result<Adjust, CmdError> {
    match adjust {
        "qfq" => Ok(Adjust::Qfq),
        "hfq" => Ok(Adjust::Hfq),
        "none" => Ok(Adjust::None),
        other => Err(CmdError::new(
            "invalid_param",
            format!("unknown adjust {other:?}; expected qfq|hfq|none"),
        )),
    }
}

/// Clamp a requested kline count into `1..=10000`.
pub(crate) fn clamp_count(count: u32) -> u32 {
    count.clamp(1, MAX_KLINE_COUNT)
}

/// Parse and validate a 6-digit symbol.
pub(crate) fn parse_symbol(raw: &str) -> Result<Symbol, CmdError> {
    Symbol::new(raw).map_err(CmdError::from)
}

/// Contract `Bar` shape: {date,open,close,high,low,volume,amount,pct,turnover}.
/// (The core `Bar` carries an extra `volume_unit` field, so the app layer
/// projects onto this wrapper instead of serializing the core type.)
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BarJson {
    /// Trading date, `YYYY-MM-DD`.
    pub date: String,
    /// Opening price.
    pub open: f64,
    /// Closing price.
    pub close: f64,
    /// Highest price.
    pub high: f64,
    /// Lowest price.
    pub low: f64,
    /// Volume (lots for A-shares).
    pub volume: f64,
    /// Turnover amount in CNY (null when the upstream did not provide it).
    pub amount: Option<f64>,
    /// Percent change vs. previous close.
    pub pct: Option<f64>,
    /// Turnover rate in percent.
    pub turnover: Option<f64>,
}

impl From<&Bar> for BarJson {
    fn from(b: &Bar) -> Self {
        BarJson {
            date: b.date.to_string(),
            open: b.open,
            close: b.close,
            high: b.high,
            low: b.low,
            volume: b.volume,
            amount: b.amount,
            pct: b.pct,
            turnover: b.turnover,
        }
    }
}

/// `get_kline` response: `{ bars, source }`.
#[derive(Debug, Serialize)]
pub struct KlineResponse {
    /// Bars, oldest first.
    pub bars: Vec<BarJson>,
    /// Upstream that answered (`tencent`/`sina`/`eastmoney`).
    pub source: String,
}

/// `get_minute` response: `{ points, pre_close, name }`.
#[derive(Debug, Serialize)]
pub struct MinuteResponse {
    /// Intraday minute points in chronological order.
    pub points: Vec<MinutePointJson>,
    /// Previous close.
    pub pre_close: f64,
    /// Instrument name.
    pub name: String,
}

/// One minute point: `{time,price,avg_price,volume}` with `time` as `HH:MM`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MinutePointJson {
    /// Intraday time, `HH:MM`.
    pub time: String,
    /// Price at this minute.
    pub price: f64,
    /// Session VWAP.
    pub avg_price: f64,
    /// Volume traded during this minute (lots).
    pub volume: f64,
}

impl From<&MinuteData> for MinuteResponse {
    fn from(m: &MinuteData) -> Self {
        MinuteResponse {
            points: m
                .points
                .iter()
                .map(|p| MinutePointJson {
                    time: p.time.format("%H:%M").to_string(),
                    price: p.price,
                    avg_price: p.avg_price,
                    volume: p.volume,
                })
                .collect(),
            pre_close: m.pre_close,
            name: m.name.clone(),
        }
    }
}

/// `get_market_breadth` response: `{up,down,flat,total,breadth_ratio}`.
#[derive(Debug, Serialize)]
pub struct BreadthJson {
    /// Stocks up on the day.
    pub up: u32,
    /// Stocks down on the day.
    pub down: u32,
    /// Stocks flat.
    pub flat: u32,
    /// Total stocks counted.
    pub total: u32,
    /// `up / (up + down)`, 0.5 when nothing moved (legacy rule).
    pub breadth_ratio: f64,
}

/// Professional market-table row with classification and explicit nulls.
#[derive(Debug, Serialize)]
pub struct AllShareJson {
    pub code: String,
    pub name: String,
    pub market: String,
    pub board: String,
    pub price: Option<f64>,
    pub pct: Option<f64>,
    pub amount: Option<f64>,
    pub source: String,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
pub struct OrderBookLevelJson {
    pub level: u8,
    pub price: f64,
    /// Quantity in lots (手).
    pub volume: f64,
}

#[derive(Debug, Serialize)]
pub struct OrderBookJson {
    pub symbol: String,
    pub server_time: String,
    pub current_volume: f64,
    pub inner_volume: f64,
    pub outer_volume: f64,
    pub bids: Vec<OrderBookLevelJson>,
    pub asks: Vec<OrderBookLevelJson>,
    pub source: String,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
    pub transaction_detail_available: bool,
    pub limitation: String,
}

/// Contract daily fund-flow row: `{date,main_net,super_large_net,large_net,
/// medium_net,small_net,main_pct}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FundFlowJson {
    /// Trading date, `YYYY-MM-DD`.
    pub date: String,
    /// Main-force net inflow (CNY).
    pub main_net: f64,
    /// Super-large-order net inflow.
    pub super_large_net: f64,
    /// Large-order net inflow.
    pub large_net: f64,
    /// Medium-order net inflow.
    pub medium_net: f64,
    /// Small-order net inflow.
    pub small_net: f64,
    /// Main net inflow as percent of turnover.
    pub main_pct: f64,
}

impl From<&FundFlowPoint> for FundFlowJson {
    fn from(f: &FundFlowPoint) -> Self {
        FundFlowJson {
            date: f.time.date().to_string(),
            main_net: f.main_net,
            super_large_net: f.super_large_net,
            large_net: f.large_net,
            medium_net: f.medium_net,
            small_net: f.small_net,
            main_pct: f.main_pct,
        }
    }
}

/// One realtime (intraday cumulative) fund-flow point.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RealtimeFlowPointJson {
    /// Intraday time, `HH:MM`.
    pub time: String,
    /// Cumulative main-force net inflow.
    pub main_net: f64,
    /// Cumulative small-order net inflow.
    pub small_net: f64,
    /// Cumulative medium-order net inflow.
    pub medium_net: f64,
    /// Cumulative large-order net inflow.
    pub large_net: f64,
    /// Cumulative super-large-order net inflow.
    pub super_large_net: f64,
}

/// `get_realtime_flow` response: points plus a summary holding the latest
/// cumulative totals (zeros when the market is closed / no data).
#[derive(Debug, Serialize)]
pub struct RealtimeFlowResponse {
    /// Intraday cumulative flow points in chronological order.
    pub points: Vec<RealtimeFlowPointJson>,
    /// Latest cumulative totals.
    pub summary: RealtimeFlowSummaryJson,
}

/// Latest cumulative fund-flow totals (the last point's values).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct RealtimeFlowSummaryJson {
    /// Cumulative main-force net inflow.
    pub main_net: f64,
    /// Cumulative small-order net inflow.
    pub small_net: f64,
    /// Cumulative medium-order net inflow.
    pub medium_net: f64,
    /// Cumulative large-order net inflow.
    pub large_net: f64,
    /// Cumulative super-large-order net inflow.
    pub super_large_net: f64,
}

/// Realtime quote snapshot.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_quote(
    state: State<'_, AppState>,
    symbol: String,
) -> Result<astock_core::Quote, CmdError> {
    let symbol = parse_symbol(&symbol)?;
    let fetched = state.market.quote(&symbol).await?;
    if let Some(record) = state.market.security_master.get(symbol.code()) {
        state.storage.securities_upsert(vec![record]).await?;
    }
    Ok(fetched.data)
}

/// TDX five-level order book. The bundled upstream currently exposes the
/// snapshot and minute series but not tick-by-tick transaction details.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_order_book(
    state: State<'_, AppState>,
    symbol: String,
) -> Result<OrderBookJson, CmdError> {
    let symbol = parse_symbol(&symbol)?;
    let raw = state.market.tdx.order_book(&symbol).await?;
    let levels = |rows: &[(f64, f64); 5]| {
        rows.iter()
            .enumerate()
            .map(|(index, (price, volume))| OrderBookLevelJson {
                level: (index + 1) as u8,
                price: *price,
                volume: *volume,
            })
            .collect()
    };
    Ok(OrderBookJson {
        symbol: raw.code,
        server_time: raw.servertime,
        current_volume: raw.cur_vol,
        inner_volume: raw.s_vol,
        outer_volume: raw.b_vol,
        bids: levels(&raw.bid),
        asks: levels(&raw.ask),
        source: "tdx".to_string(),
        fetched_at: astock_core::time::utc_now(),
        transaction_detail_available: false,
        limitation: "当前内置 TDX 协议层支持五档快照与分时，不支持逐笔成交；未使用虚构逐笔数据"
            .to_string(),
    })
}

/// Historical kline bars (`count` clamped to ≤ 10000).
///
/// Read-through over the persistent parquet cache: fresh caches (last bar ≥
/// latest expected trading day per the trading calendar) are served from
/// parquet with `source: "cache"` and no network traffic; stale caches are
/// refreshed from the market layer and merged incrementally. During a live
/// session today's bar is refreshed at most every 60s per key.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_kline(
    state: State<'_, AppState>,
    symbol: String,
    period: String,
    adjust: String,
    count: u32,
) -> Result<KlineResponse, CmdError> {
    let symbol = parse_symbol(&symbol)?;
    let period = parse_period(&period)?;
    let adjust = parse_adjust(&adjust)?;
    let (bars, source) = crate::cache_path::kline_read_through(
        &state.storage,
        &state.market,
        &state.rules,
        &symbol,
        period,
        adjust,
        clamp_count(count),
    )
    .await?;
    Ok(KlineResponse {
        bars: bars.iter().map(BarJson::from).collect(),
        source,
    })
}

/// Per-provider circuit-breaker health for the settings page
/// (`[{name, state, cooldown_remaining_secs}]`).
#[tauri::command(rename_all = "snake_case")]
pub async fn get_provider_health(
    state: State<'_, AppState>,
) -> Result<Vec<astock_market_data::ProviderHealth>, CmdError> {
    Ok(state.market.provider_health())
}

/// Intraday minute (分时) series for the current session.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_minute(
    state: State<'_, AppState>,
    symbol: String,
) -> Result<MinuteResponse, CmdError> {
    let symbol = parse_symbol(&symbol)?;
    let fetched = state.market.minute(&symbol).await?;
    Ok(MinuteResponse::from(&fetched.data))
}

/// Symbol search by keyword or code.
#[tauri::command(rename_all = "snake_case")]
pub async fn search_stocks(
    state: State<'_, AppState>,
    keyword: String,
) -> Result<Vec<astock_core::SearchResult>, CmdError> {
    let fetched = state.market.search(&keyword).await?;
    let records = fetched
        .data
        .iter()
        .filter_map(|hit| state.market.security_master.get(&hit.code))
        .collect();
    state.storage.securities_upsert(records).await?;
    Ok(fetched.data)
}

/// Market-wide advance/decline counts.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_market_breadth(state: State<'_, AppState>) -> Result<BreadthJson, CmdError> {
    let fetched = state.market.market_breadth().await?;
    let b = fetched.data;
    Ok(BreadthJson {
        up: b.up,
        down: b.down,
        flat: b.flat,
        total: b.total,
        breadth_ratio: b.ratio(),
    })
}

/// Full A-share list for scanner pre-filtering.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_all_a_shares(state: State<'_, AppState>) -> Result<Vec<AllShareJson>, CmdError> {
    let fetched = state.market.all_a_shares().await?;
    state
        .storage
        .securities_upsert(state.market.security_master.all())
        .await?;
    Ok(fetched
        .data
        .into_iter()
        .map(|item| {
            let identity = state.market.security_master.get(&item.code);
            AllShareJson {
                market: identity
                    .as_ref()
                    .map_or_else(|| "unknown".to_string(), |row| row.market.to_string()),
                board: identity.as_ref().map_or_else(
                    || "other".to_string(),
                    |row| format!("{:?}", row.board).to_lowercase(),
                ),
                code: item.code,
                name: identity.map_or(item.name, |row| row.canonical_name),
                price: item.price,
                pct: item.pct,
                amount: item.amount,
                source: fetched.source.to_string(),
                fetched_at: fetched.fetched_at,
            }
        })
        .collect())
}

/// Daily fund flow for the last `days` trading days.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_fund_flow(
    state: State<'_, AppState>,
    symbol: String,
    days: u32,
) -> Result<Vec<FundFlowJson>, CmdError> {
    let symbol = parse_symbol(&symbol)?;
    let fetched = state.market.fund_flow_daily(&symbol, days).await?;
    Ok(fetched.data.iter().map(FundFlowJson::from).collect())
}

/// Intraday cumulative fund flow, plus latest-total summary.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_realtime_flow(
    state: State<'_, AppState>,
    symbol: String,
) -> Result<RealtimeFlowResponse, CmdError> {
    let symbol = parse_symbol(&symbol)?;
    let fetched = state.market.fund_flow_realtime(&symbol).await?;
    let points: Vec<RealtimeFlowPointJson> = fetched
        .data
        .iter()
        .map(|p| RealtimeFlowPointJson {
            time: p.time.format("%H:%M").to_string(),
            main_net: p.main_net,
            small_net: p.small_net,
            medium_net: p.medium_net,
            large_net: p.large_net,
            super_large_net: p.super_large_net,
        })
        .collect();
    let summary = points
        .last()
        .map_or_else(RealtimeFlowSummaryJson::default, |p| {
            RealtimeFlowSummaryJson {
                main_net: p.main_net,
                small_net: p.small_net,
                medium_net: p.medium_net,
                large_net: p.large_net,
                super_large_net: p.super_large_net,
            }
        });
    Ok(RealtimeFlowResponse { points, summary })
}

/// Index kline given an EastMoney index secid such as `1.000001`.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_index_kline(
    state: State<'_, AppState>,
    secid: String,
    count: u32,
) -> Result<Vec<BarJson>, CmdError> {
    let fetched = state.market.index_kline(&secid, clamp_count(count)).await?;
    Ok(fetched.data.iter().map(BarJson::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_core::VolumeUnit;
    use chrono::NaiveDate;

    #[test]
    fn period_and_adjust_parsing() {
        assert_eq!(parse_period("day").unwrap(), KlinePeriod::Day);
        assert_eq!(parse_period("week").unwrap(), KlinePeriod::Week);
        assert_eq!(parse_period("month").unwrap(), KlinePeriod::Month);
        assert!(parse_period("min5").is_err());
        assert!(parse_period("").is_err());
        assert_eq!(parse_adjust("qfq").unwrap(), Adjust::Qfq);
        assert_eq!(parse_adjust("hfq").unwrap(), Adjust::Hfq);
        assert_eq!(parse_adjust("none").unwrap(), Adjust::None);
        assert!(parse_adjust("QFQ").is_err());
    }

    #[test]
    fn count_clamping() {
        assert_eq!(clamp_count(0), 1);
        assert_eq!(clamp_count(250), 250);
        assert_eq!(clamp_count(10000), 10000);
        assert_eq!(clamp_count(99999), 10000);
    }

    #[test]
    fn bar_json_matches_contract_fields() {
        let bar = Bar {
            date: NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(),
            open: 10.0,
            close: 10.5,
            high: 10.8,
            low: 9.9,
            volume: 100.0,
            volume_unit: VolumeUnit::Lots,
            amount: Some(1000.0),
            turnover: None,
            pct: Some(1.0),
        };
        let json = serde_json::to_value(BarJson::from(&bar)).unwrap();
        let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
        for want in [
            "date", "open", "close", "high", "low", "volume", "amount", "pct", "turnover",
        ] {
            assert!(keys.iter().any(|k| k.as_str() == want), "missing {want}");
        }
        assert!(!keys.iter().any(|k| k.as_str() == "volume_unit"));
        assert_eq!(json["date"], "2025-01-02");
        assert!(json["turnover"].is_null());
    }
}
