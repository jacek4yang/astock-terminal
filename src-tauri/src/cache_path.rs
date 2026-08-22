//! Persistent read-through kline cache and short-lived analysis-result cache.
//!
//! Two layers on top of the pre-existing storage APIs:
//!
//! - **K线读穿缓存** (`kline_read_through`): kline bars are persisted as
//!   parquet via `Storage::{load_bars, merge_and_write_bars, last_bar_date}`.
//!   Freshness is decided against the trading calendar ([`RuleSet`]) and the
//!   Asia/Shanghai clock: a cache whose last bar covers the latest expected
//!   trading day is served entirely from parquet; otherwise the market layer
//!   is queried and the result merged into the cache (incremental, keyed by
//!   date) before serving. During a trading session today's bar is treated
//!   as possibly-incomplete and refreshed at most every
//!   [`INTRADAY_REFRESH_INTERVAL`] per `(symbol, period, adjust)` key.
//! - **分析结果短缓存** (`tool_cache_*_json`): analysis results are cached in
//!   the storage `tool_cache` table with a 60s TTL during trading sessions
//!   and 4h after close; the cache key carries the kline last-bar date so a
//!   fresh bar invalidates stale results.
//!
//! `BarRow` ↔ `Bar` conversion is field-by-field (see the layout docs in
//! `crates/storage/src/timeseries.rs`): `volume` is carried through unchanged
//! and rows read back are stamped [`VolumeUnit::Lots`], the A-share display
//! convention used by the command contract. `pct` is not persisted; it is
//! recomputed from consecutive closes exactly like the upstream adapters
//! (`fill_pct` in `astock-market-data`).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use astock_core::{Adjust, Bar, KlinePeriod, Symbol, VolumeUnit};
use astock_market_data::{DataProvider, MarketData};
use astock_storage::{BarRow, Storage, ToolCacheEntry};
use astock_trading_rules::{AuctionPhase, RuleSet};
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, Utc};
use serde_json::Value;

use crate::error::CmdError;

/// Min interval between intraday refreshes of one kline key while the cached
/// last bar is today's possibly-incomplete session bar.
pub const INTRADAY_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
/// Analysis-result TTL while a trading session is live.
pub const ANALYSIS_TTL_TRADING_SECS: i64 = 60;
/// Analysis-result TTL outside trading sessions (post-close / weekend).
pub const ANALYSIS_TTL_CLOSED_SECS: i64 = 4 * 3600;
/// `source` label used when bars are served from the parquet cache.
pub const CACHE_SOURCE: &str = "cache";

/// Asia/Shanghai is a fixed UTC+8 (China has observed no DST since 1991).
fn shanghai_offset() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("UTC+8 is a valid offset")
}

/// Current time in Asia/Shanghai — the timezone the A-share calendar runs on.
pub fn shanghai_now() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&shanghai_offset())
}

/// Storage path token for a kline period (parquet layout `{symbol}/{period}/{adjust}.parquet`).
pub fn period_token(period: KlinePeriod) -> &'static str {
    match period {
        KlinePeriod::Day => "day",
        KlinePeriod::Week => "week",
        KlinePeriod::Month => "month",
        KlinePeriod::Min1 => "min1",
        KlinePeriod::Min5 => "min5",
        KlinePeriod::Min15 => "min15",
        KlinePeriod::Min30 => "min30",
        KlinePeriod::Min60 => "min60",
    }
}

/// Storage path token for an adjust mode.
pub fn adjust_token(adjust: Adjust) -> &'static str {
    match adjust {
        Adjust::Qfq => "qfq",
        Adjust::Hfq => "hfq",
        Adjust::None => "none",
    }
}

/// Latest trading day on or before `date` (walks back over weekends/holidays;
/// bounded at 40 days to stay total even with a corrupt calendar).
pub fn latest_trading_day_on_or_before(rules: &RuleSet, date: NaiveDate) -> NaiveDate {
    let mut d = date;
    for _ in 0..40 {
        if rules.is_trading_day(d) {
            return d;
        }
        d -= chrono::Duration::days(1);
    }
    d
}

/// Bar-start date of the aggregate bar (week/month) containing `date`.
/// Weekly/monthly upstream bars are dated at the start of their period, so
/// freshness compares the cached last-bar date against this floor: any date
/// on/after it belongs to the current (possibly still-forming) bar.
pub fn period_start(period: KlinePeriod, date: NaiveDate) -> NaiveDate {
    match period {
        KlinePeriod::Week => {
            date - chrono::Duration::days(i64::from(date.weekday().num_days_from_monday()))
        }
        KlinePeriod::Month => date.with_day(1).expect("day 1 always exists"),
        _ => date,
    }
}

/// Freshness verdict for the parquet kline cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Cache covers the latest expected bar; serve from parquet.
    Fresh,
    /// Cache covers the latest expected bar, but that bar is today's and the
    /// session is still live — refresh (throttled) to pick up new ticks.
    RefreshIntraday,
    /// Cache is missing or behind; a network fetch is required.
    Stale,
}

/// Decide cache freshness for `(period, last_cached)` at `now`
/// (Asia/Shanghai wall clock).
pub fn freshness(
    rules: &RuleSet,
    period: KlinePeriod,
    last_cached: Option<NaiveDate>,
    now: NaiveDateTime,
) -> Freshness {
    let Some(last) = last_cached else {
        return Freshness::Stale;
    };
    let latest = latest_trading_day_on_or_before(rules, now.date());
    if last < period_start(period, latest) {
        return Freshness::Stale;
    }
    if latest == now.date() && rules.auction_phase(now.time()) != AuctionPhase::Closed {
        Freshness::RefreshIntraday
    } else {
        Freshness::Fresh
    }
}

/// Whether a throttled action may run: allowed when never run or when
/// `min_interval` has elapsed since `last`.
pub fn throttle_allows(last: Option<Instant>, now: Instant, min_interval: Duration) -> bool {
    match last {
        None => true,
        Some(last) => now.duration_since(last) >= min_interval,
    }
}

/// Last intraday-refresh times per kline key (`symbol|period|adjust`).
static INTRADAY_THROTTLE: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Claim an intraday refresh slot for `key`: returns true (and records the
/// attempt) at most once per [`INTRADAY_REFRESH_INTERVAL`].
fn claim_intraday_refresh(key: &str) -> bool {
    let mut map = INTRADAY_THROTTLE
        .lock()
        .expect("intraday throttle poisoned");
    let now = Instant::now();
    if throttle_allows(map.get(key).copied(), now, INTRADAY_REFRESH_INTERVAL) {
        map.insert(key.to_string(), now);
        true
    } else {
        false
    }
}

/// Core bar → storage row (field-by-field; `source`/`fetched_at` provenance).
pub fn bar_to_row(bar: &Bar, source: &str, fetched_at: i64) -> BarRow {
    BarRow {
        date: bar.date,
        open: bar.open,
        high: bar.high,
        low: bar.low,
        close: bar.close,
        volume: bar.volume,
        amount: bar.amount,
        turnover: bar.turnover,
        source: source.to_string(),
        fetched_at,
    }
}

/// Storage rows → core bars (ascending by date), stamping `VolumeUnit::Lots`
/// and recomputing `pct` from consecutive closes (2dp, first bar `None`),
/// mirroring the upstream adapters' `fill_pct`.
pub fn rows_to_bars(rows: &[BarRow]) -> Vec<Bar> {
    let mut bars: Vec<Bar> = rows
        .iter()
        .map(|r| Bar {
            date: r.date,
            open: r.open,
            high: r.high,
            low: r.low,
            close: r.close,
            volume: r.volume,
            volume_unit: VolumeUnit::Lots,
            amount: r.amount,
            turnover: r.turnover,
            pct: None,
        })
        .collect();
    for i in 1..bars.len() {
        let prev = bars[i - 1].close;
        if prev > 0.0 {
            let pct = (bars[i].close - prev) / prev * 100.0;
            bars[i].pct = Some((pct * 100.0).round() / 100.0);
        }
    }
    bars
}

/// Keep the last `count` bars of an ascending series.
fn tail(bars: Vec<Bar>, count: u32) -> Vec<Bar> {
    let count = count as usize;
    if bars.len() > count {
        bars[bars.len() - count..].to_vec()
    } else {
        bars
    }
}

/// Read-through kline fetch at the current Asia/Shanghai time; see
/// [`kline_read_through_at`].
pub async fn kline_read_through(
    storage: &Storage,
    market: &MarketData,
    rules: &RuleSet,
    symbol: &Symbol,
    period: KlinePeriod,
    adjust: Adjust,
    count: u32,
) -> Result<(Vec<Bar>, String), CmdError> {
    kline_read_through_at(
        storage,
        market,
        rules,
        symbol,
        period,
        adjust,
        count,
        shanghai_now().naive_local(),
    )
    .await
}

/// Read-through kline fetch: serve from the parquet cache when fresh, else
/// fetch from the market layer, merge into the cache and serve the merged
/// series (last `count` bars). Returns the bars plus a `source` label — the
/// upstream name on a fresh fetch, [`CACHE_SOURCE`] when served from parquet.
/// A failed refresh degrades to the stale cache when one exists.
#[allow(clippy::too_many_arguments)]
pub async fn kline_read_through_at(
    storage: &Storage,
    market: &MarketData,
    rules: &RuleSet,
    symbol: &Symbol,
    period: KlinePeriod,
    adjust: Adjust,
    count: u32,
    now: NaiveDateTime,
) -> Result<(Vec<Bar>, String), CmdError> {
    let code = symbol.code();
    let p = period_token(period);
    let a = adjust_token(adjust);

    let last = storage.last_bar_date(code, p, a).await?;
    let refresh = match freshness(rules, period, last, now) {
        Freshness::Fresh => false,
        Freshness::Stale => true,
        Freshness::RefreshIntraday => claim_intraday_refresh(&format!("{code}|{p}|{a}")),
    };

    let cached = storage.load_bars(code, p, a).await?;
    if !refresh && !cached.is_empty() {
        return Ok((tail(rows_to_bars(&cached), count), CACHE_SOURCE.to_string()));
    }

    match market.kline(symbol, period, adjust, count).await {
        Ok(fetched) => {
            let source = fetched.source.to_string();
            let fetched_at = now.and_utc().timestamp();
            let rows: Vec<BarRow> = fetched
                .data
                .iter()
                .map(|b| bar_to_row(b, &source, fetched_at))
                .collect();
            match storage.merge_and_write_bars(code, p, a, rows).await {
                Ok(_) => {
                    let merged = storage.load_bars(code, p, a).await?;
                    Ok((tail(rows_to_bars(&merged), count), source))
                }
                Err(e) => {
                    tracing::warn!(error = %e, %symbol, "kline cache write failed; serving upstream bars");
                    Ok((tail(fetched.data, count), source))
                }
            }
        }
        Err(e) => {
            if cached.is_empty() {
                return Err(e.into());
            }
            tracing::warn!(error = %e, %symbol, "kline refresh failed; serving stale cache");
            Ok((tail(rows_to_bars(&cached), count), CACHE_SOURCE.to_string()))
        }
    }
}

/// Analysis-result TTL at `now` (Asia/Shanghai): short while a trading
/// session is live, long after close / on non-trading days.
pub fn analysis_ttl_secs(rules: &RuleSet, now: NaiveDateTime) -> i64 {
    if rules.is_trading_day(now.date()) && rules.auction_phase(now.time()) != AuctionPhase::Closed {
        ANALYSIS_TTL_TRADING_SECS
    } else {
        ANALYSIS_TTL_CLOSED_SECS
    }
}

/// Deterministic tool-cache key: tool name, symbol, parameter detail and the
/// data version (kline last-bar date, `"none"` when unknown) so a fresh bar
/// invalidates stale analysis results.
pub fn analysis_cache_key(tool: &str, symbol: &Symbol, detail: &str, data_version: &str) -> String {
    format!("{tool}|{symbol}|{detail}|{data_version}")
}

/// Fetch a cached JSON tool result; cache errors degrade to a miss (logged).
pub async fn tool_cache_get_json(storage: &Storage, key: &str) -> Option<Value> {
    match storage.tool_cache_get(key).await {
        Ok(Some(entry)) => serde_json::from_str(&entry.result_json).ok(),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = %e, key, "tool cache read failed; treating as miss");
            None
        }
    }
}

/// Store a JSON tool result; cache errors are logged and never fail the
/// calling command.
pub async fn tool_cache_put_json(
    storage: &Storage,
    key: &str,
    tool: &str,
    params: Value,
    data_version: Option<String>,
    ttl_secs: i64,
    result: &Value,
) {
    let now = Utc::now().timestamp();
    let entry = ToolCacheEntry {
        cache_key: key.to_string(),
        tool: tool.to_string(),
        params_json: params.to_string(),
        result_json: result.to_string(),
        data_version,
        created_at: now,
        ttl_seconds: ttl_secs,
        accessed_at: now,
    };
    if let Err(e) = storage.tool_cache_put(entry).await {
        tracing::warn!(error = %e, key, "tool cache write failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveTime;

    fn rules() -> RuleSet {
        RuleSet::load(None).unwrap()
    }

    fn dt(date: &str, time: &str) -> NaiveDateTime {
        NaiveDateTime::new(
            NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
            NaiveTime::parse_from_str(time, "%H:%M").unwrap(),
        )
    }

    fn d(date: &str) -> NaiveDate {
        NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn freshness_empty_cache_is_stale() {
        let r = rules();
        assert_eq!(
            freshness(&r, KlinePeriod::Day, None, dt("2025-09-30", "10:00")),
            Freshness::Stale
        );
    }

    #[test]
    fn freshness_today_during_session_refreshes() {
        let r = rules();
        // 2025-09-30 is a Tuesday trading day; 10:00 is continuous trading.
        assert_eq!(
            freshness(
                &r,
                KlinePeriod::Day,
                Some(d("2025-09-30")),
                dt("2025-09-30", "10:00")
            ),
            Freshness::RefreshIntraday
        );
    }

    #[test]
    fn freshness_today_after_close_is_fresh() {
        let r = rules();
        for time in ["15:00", "16:30", "23:59"] {
            assert_eq!(
                freshness(
                    &r,
                    KlinePeriod::Day,
                    Some(d("2025-09-30")),
                    dt("2025-09-30", time)
                ),
                Freshness::Fresh,
                "at {time}"
            );
        }
    }

    #[test]
    fn freshness_yesterday_on_trading_day_is_stale() {
        let r = rules();
        assert_eq!(
            freshness(
                &r,
                KlinePeriod::Day,
                Some(d("2025-09-29")),
                dt("2025-09-30", "16:00")
            ),
            Freshness::Stale
        );
    }

    #[test]
    fn freshness_weekend_serves_friday() {
        let r = rules();
        // 2025-03-08 is a Saturday; the latest expected bar is Friday 03-07.
        for (now, last) in [
            (dt("2025-03-08", "12:00"), d("2025-03-07")),
            (dt("2025-03-09", "12:00"), d("2025-03-07")),
        ] {
            assert_eq!(
                freshness(&r, KlinePeriod::Day, Some(last), now),
                Freshness::Fresh
            );
        }
        // Monday 03-10 expects a new bar.
        assert_eq!(
            freshness(
                &r,
                KlinePeriod::Day,
                Some(d("2025-03-07")),
                dt("2025-03-10", "16:00")
            ),
            Freshness::Stale
        );
    }

    #[test]
    fn freshness_national_day_holiday() {
        let r = rules();
        // 2025-10-01..08 are holidays; the latest expected bar is 09-30.
        assert_eq!(
            freshness(
                &r,
                KlinePeriod::Day,
                Some(d("2025-09-30")),
                dt("2025-10-05", "12:00")
            ),
            Freshness::Fresh
        );
        // First day back (Thu 2025-10-09) expects a new bar.
        assert_eq!(
            freshness(
                &r,
                KlinePeriod::Day,
                Some(d("2025-09-30")),
                dt("2025-10-09", "16:00")
            ),
            Freshness::Stale
        );
        // Same day, mid-session: cached 09-30 is stale, cached 10-09 refreshes.
        assert_eq!(
            freshness(
                &r,
                KlinePeriod::Day,
                Some(d("2025-09-30")),
                dt("2025-10-09", "10:00")
            ),
            Freshness::Stale
        );
        assert_eq!(
            freshness(
                &r,
                KlinePeriod::Day,
                Some(d("2025-10-09")),
                dt("2025-10-09", "10:00")
            ),
            Freshness::RefreshIntraday
        );
    }

    #[test]
    fn freshness_weekly_keyed_on_week_start() {
        let r = rules();
        // Thursday 2025-10-09: current weekly bar starts Monday 2025-10-06
        // (a holiday — the bar may be dated 10-09, still >= the floor).
        assert_eq!(
            freshness(
                &r,
                KlinePeriod::Week,
                Some(d("2025-10-09")),
                dt("2025-10-09", "16:00")
            ),
            Freshness::Fresh
        );
        // Last week's bar (week starting Mon 2025-09-29) is stale.
        assert_eq!(
            freshness(
                &r,
                KlinePeriod::Week,
                Some(d("2025-09-29")),
                dt("2025-10-09", "16:00")
            ),
            Freshness::Stale
        );
        // Same week, mid-session: refresh the still-forming weekly bar.
        assert_eq!(
            freshness(
                &r,
                KlinePeriod::Week,
                Some(d("2025-10-09")),
                dt("2025-10-09", "10:00")
            ),
            Freshness::RefreshIntraday
        );
    }

    #[test]
    fn freshness_monthly_keyed_on_month_start() {
        let r = rules();
        // Mid-October: the October bar (floor 2025-10-01) covers 10-09.
        assert_eq!(
            freshness(
                &r,
                KlinePeriod::Month,
                Some(d("2025-10-09")),
                dt("2025-10-15", "16:00")
            ),
            Freshness::Fresh
        );
        // September's bar is stale once October has a trading day.
        assert_eq!(
            freshness(
                &r,
                KlinePeriod::Month,
                Some(d("2025-09-30")),
                dt("2025-10-15", "16:00")
            ),
            Freshness::Stale
        );
    }

    #[test]
    fn throttle_window() {
        let t0 = Instant::now();
        assert!(throttle_allows(None, t0, INTRADAY_REFRESH_INTERVAL));
        assert!(!throttle_allows(
            Some(t0),
            t0 + Duration::from_secs(30),
            INTRADAY_REFRESH_INTERVAL
        ));
        assert!(throttle_allows(
            Some(t0),
            t0 + Duration::from_secs(61),
            INTRADAY_REFRESH_INTERVAL
        ));
    }

    #[test]
    fn analysis_ttl_depends_on_session() {
        let r = rules();
        // Live session on a trading day.
        assert_eq!(
            analysis_ttl_secs(&r, dt("2025-09-30", "10:00")),
            ANALYSIS_TTL_TRADING_SECS
        );
        // Post-close on a trading day.
        assert_eq!(
            analysis_ttl_secs(&r, dt("2025-09-30", "16:00")),
            ANALYSIS_TTL_CLOSED_SECS
        );
        // Weekend.
        assert_eq!(
            analysis_ttl_secs(&r, dt("2025-03-08", "10:00")),
            ANALYSIS_TTL_CLOSED_SECS
        );
        // Holiday.
        assert_eq!(
            analysis_ttl_secs(&r, dt("2025-10-01", "10:00")),
            ANALYSIS_TTL_CLOSED_SECS
        );
    }

    #[test]
    fn cache_key_carries_all_params_and_version() {
        let sym = Symbol::new("600519").unwrap();
        let key = analysis_cache_key("analyze", &sym, "day", "2025-09-30");
        assert_eq!(key, "analyze|600519|day|2025-09-30");
        // Same params but a newer bar -> different key (stale results invalidate).
        let key2 = analysis_cache_key("analyze", &sym, "day", "2025-10-09");
        assert_ne!(key, key2);
        // Different detail -> different key.
        let key3 = analysis_cache_key("chanlun_daily", &sym, "day|250", "2025-09-30");
        assert!(key3.contains("day|250"));
    }

    #[test]
    fn rows_to_bars_recomputes_pct_and_preserves_fields() {
        let rows = vec![
            BarRow {
                date: d("2025-09-29"),
                open: 10.0,
                high: 10.5,
                low: 9.8,
                close: 10.0,
                volume: 100.0,
                amount: Some(1000.0),
                turnover: Some(0.5),
                source: "tencent".into(),
                fetched_at: 1,
            },
            BarRow {
                date: d("2025-09-30"),
                open: 10.2,
                high: 10.8,
                low: 10.1,
                close: 10.5,
                volume: 200.0,
                amount: None,
                turnover: None,
                source: "tencent".into(),
                fetched_at: 1,
            },
        ];
        let bars = rows_to_bars(&rows);
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].pct, None);
        assert_eq!(bars[1].pct, Some(5.0));
        assert_eq!(bars[1].volume, 200.0);
        assert_eq!(bars[1].volume_unit, VolumeUnit::Lots);
        assert_eq!(bars[0].amount, Some(1000.0));
        assert_eq!(bars[1].amount, None);
    }

    #[test]
    fn period_start_boundaries() {
        // Thursday 2025-10-09 -> Monday 2025-10-06.
        assert_eq!(
            period_start(KlinePeriod::Week, d("2025-10-09")),
            d("2025-10-06")
        );
        assert_eq!(
            period_start(KlinePeriod::Month, d("2025-10-09")),
            d("2025-10-01")
        );
        assert_eq!(
            period_start(KlinePeriod::Day, d("2025-10-09")),
            d("2025-10-09")
        );
    }

    /// Previous trading day strictly before `date` (test helper).
    fn prev_trading_day(rules: &RuleSet, date: NaiveDate) -> NaiveDate {
        let mut d = date - chrono::Duration::days(1);
        while !rules.is_trading_day(d) {
            d -= chrono::Duration::days(1);
        }
        d
    }

    fn canned_row(date: NaiveDate, close: f64) -> BarRow {
        BarRow {
            date,
            open: close - 0.2,
            high: close + 0.3,
            low: close - 0.4,
            close,
            volume: 1000.0,
            amount: Some(1.0e6),
            turnover: Some(0.5),
            source: "test".into(),
            fetched_at: 1,
        }
    }

    fn temp_storage(tag: &str) -> (Storage, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("astock-cache-path-{tag}-{}", std::process::id()));
        let storage = Storage::open(astock_storage::StorageConfig::with_base_dir(&dir)).unwrap();
        (storage, dir)
    }

    /// Merge canned bars into a tempfile Storage, then serve them through
    /// the read-through path with `now` after the close: no network is
    /// touched, `source` is `"cache"`, `count` tails the merged series and
    /// `pct` is recomputed across the full series (not just the tail).
    #[tokio::test]
    async fn merge_then_serve_from_parquet_cache() {
        let (storage, dir) = temp_storage("merge-serve");
        let r = rules();
        let symbol = Symbol::new("600519").unwrap();
        let today = shanghai_now().date_naive();
        let last_td = latest_trading_day_on_or_before(&r, today);
        let d1 = prev_trading_day(&r, prev_trading_day(&r, last_td));
        let d2 = prev_trading_day(&r, last_td);

        storage
            .merge_and_write_bars(
                "600519",
                "day",
                "qfq",
                vec![
                    canned_row(d1, 10.0),
                    canned_row(d2, 10.5),
                    canned_row(last_td, 10.5),
                ],
            )
            .await
            .unwrap();

        // Overlapping date overrides (incremental merge keyed by date).
        storage
            .merge_and_write_bars("600519", "day", "qfq", vec![canned_row(last_td, 11.0)])
            .await
            .unwrap();

        let market = MarketData::new();
        let now = NaiveDateTime::new(today, NaiveTime::from_hms_opt(16, 0, 0).unwrap());
        let (bars, source) = kline_read_through_at(
            &storage,
            &market,
            &r,
            &symbol,
            KlinePeriod::Day,
            Adjust::Qfq,
            2,
            now,
        )
        .await
        .unwrap();

        assert_eq!(source, CACHE_SOURCE);
        assert_eq!(bars.len(), 2, "count tails the merged series");
        assert_eq!(bars[1].date, last_td);
        assert_eq!(bars[1].close, 11.0, "overlap merge overrode the close");
        // pct of the tail's first bar is computed against the dropped bar
        // (10.0 -> 10.5 = +5.0%), not left null.
        assert_eq!(bars[0].pct, Some(5.0));
        assert_eq!(bars[1].pct, Some(4.76), "pct rounded to 2dp like upstream");

        storage.shutdown();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A stale cache with no network succeeds neither — but a fresh cache
    /// never errors even with a dead market layer. (The fresh path above is
    /// the no-network guarantee; here we assert the stale path would try the
    /// network, which we cannot reach in tests, so we only check the
    /// freshness gate that drives it.)
    #[test]
    fn stale_gate_forces_refresh() {
        let r = rules();
        assert_eq!(
            freshness(
                &r,
                KlinePeriod::Day,
                Some(d("2020-01-02")),
                dt("2025-09-30", "16:00")
            ),
            Freshness::Stale
        );
    }
}
