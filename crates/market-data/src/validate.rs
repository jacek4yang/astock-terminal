//! Kline bar validation filter, ported from the legacy `fetch_kline` checks.
//!
//! Drops bars with non-positive O/H/L/C, `high < max(open, close, low)`,
//! `low > min(open, close)`, or `close >= 10000` (dirty-data ceiling).

use astock_core::Bar;
use chrono::{Datelike, Weekday};
use tracing::warn;

/// Filter structurally invalid bars, warning about every dropped row and
/// about suspiciously short survivors (< 10 bars).
pub fn filter_valid_bars(symbol: &str, bars: Vec<Bar>) -> Vec<Bar> {
    filter_bars_with(symbol, bars, Bar::is_valid)
}

/// Same filter for index series, whose levels routinely exceed the
/// individual-security dirty-data ceiling of 10000.
pub fn filter_valid_index_bars(symbol: &str, bars: Vec<Bar>) -> Vec<Bar> {
    let mut valid = filter_bars_with(symbol, bars, Bar::is_valid_index);
    let before_calendar = valid.len();
    valid.retain(|bar| !matches!(bar.date.weekday(), Weekday::Sat | Weekday::Sun));
    if valid.len() < before_calendar {
        warn!(
            symbol,
            dropped = before_calendar - valid.len(),
            "filtered non-trading weekday index bars"
        );
    }

    // Public index feeds occasionally contain one structurally valid but
    // economically impossible point. Drop only isolated >25% jumps whose
    // neighbours remain close, plus an equivalent trailing spike. This is
    // intentionally index-only; individual stocks can legitimately hit much
    // larger board-specific moves over multiple sessions.
    let mut keep = vec![true; valid.len()];
    for index in 1..valid.len().saturating_sub(1) {
        let previous = valid[index - 1].close;
        let current = valid[index].close;
        let next = valid[index + 1].close;
        let jump_in = (current / previous - 1.0).abs();
        let jump_out = (current / next - 1.0).abs();
        let neighbours = (next / previous - 1.0).abs();
        if jump_in > 0.25 && jump_out > 0.25 && neighbours < 0.15 {
            keep[index] = false;
        }
    }
    if valid.len() >= 3 {
        let last = valid.len() - 1;
        let prior_move = (valid[last - 1].close / valid[last - 2].close - 1.0).abs();
        let trailing_move = (valid[last].close / valid[last - 1].close - 1.0).abs();
        if trailing_move > 0.25 && prior_move < 0.15 {
            keep[last] = false;
        }
    }
    let dropped = keep.iter().filter(|value| !**value).count();
    if dropped > 0 {
        warn!(symbol, dropped, "filtered isolated index price spikes");
    }
    valid = valid
        .into_iter()
        .zip(keep)
        .filter_map(|(bar, keep)| keep.then_some(bar))
        .collect();

    // Any removal invalidates the adapter's consecutive-close pct values.
    for bar in &mut valid {
        bar.pct = None;
    }
    for index in 1..valid.len() {
        let previous = valid[index - 1].close;
        if previous > 0.0 {
            valid[index].pct = Some(
                (((valid[index].close - previous) / previous * 100.0) * 100.0).round() / 100.0,
            );
        }
    }
    valid
}

fn filter_bars_with(symbol: &str, bars: Vec<Bar>, pred: fn(&Bar) -> bool) -> Vec<Bar> {
    let original = bars.len();
    let valid: Vec<Bar> = bars
        .into_iter()
        .filter(|b| {
            let ok = pred(b);
            if !ok {
                warn!(
                    symbol,
                    date = %b.date,
                    open = b.open,
                    high = b.high,
                    low = b.low,
                    close = b.close,
                    "abnormal kline bar filtered"
                );
            }
            ok
        })
        .collect();
    if valid.len() < original {
        warn!(
            symbol,
            dropped = original - valid.len(),
            "filtered abnormal kline bars"
        );
    }
    if valid.len() < 10 {
        warn!(
            symbol,
            survived = valid.len(),
            "fewer than 10 valid kline bars"
        );
    }
    valid
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_core::VolumeUnit;
    use chrono::NaiveDate;

    fn bar(open: f64, close: f64, high: f64, low: f64) -> Bar {
        Bar::new(
            NaiveDate::from_ymd_opt(2025, 8, 21).unwrap(),
            open,
            close,
            high,
            low,
            100.0,
            VolumeUnit::Lots,
        )
    }

    #[test]
    fn keeps_sane_bars() {
        let bars = vec![bar(10.0, 10.5, 10.8, 9.9), bar(10.5, 10.2, 10.6, 10.1)];
        assert_eq!(filter_valid_bars("600519", bars).len(), 2);
    }

    #[test]
    fn drops_dirty_bars() {
        let cases = vec![
            bar(0.0, 10.0, 10.0, 9.0),        // non-positive open
            bar(10.0, -1.0, 10.0, 9.0),       // non-positive close
            bar(10.0, 10.5, 10.2, 9.9),       // high < close
            bar(10.0, 10.5, 10.8, 10.6),      // low > open
            bar(10.0, 10000.0, 10001.0, 9.0), // close >= 10000
        ];
        assert!(filter_valid_bars("600519", cases).is_empty());
    }

    #[test]
    fn index_bars_allow_levels_above_10000() {
        // 深证成指 ~14000 points: must survive the index filter but still be
        // rejected by the individual-security filter.
        let idx = vec![bar(13935.64, 14094.17, 14132.04, 13866.39)];
        assert_eq!(filter_valid_index_bars("0.399001", idx.clone()).len(), 1);
        assert!(filter_valid_bars("0.399001", idx).is_empty());
    }

    #[test]
    fn index_filter_removes_weekend_and_isolated_spike_then_recomputes_pct() {
        let make = |date: &str, close: f64| {
            let mut row = bar(close, close, close + 10.0, close - 10.0);
            row.date = NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap();
            row
        };
        let rows = vec![
            make("2026-08-19", 3894.0),
            make("2026-08-20", 3903.72),
            make("2026-08-21", 6852.55),
            make("2026-08-22", 4089.16),
        ];
        let clean = filter_valid_index_bars("1.000001", rows);
        assert_eq!(clean.len(), 2);
        assert_eq!(clean.last().unwrap().date.to_string(), "2026-08-20");
        assert_eq!(clean.last().unwrap().pct, Some(0.25));
    }
}
