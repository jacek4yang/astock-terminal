//! Kline bar validation filter, ported from the legacy `fetch_kline` checks.
//!
//! Drops bars with non-positive O/H/L/C, `high < max(open, close, low)`,
//! `low > min(open, close)`, or `close >= 10000` (dirty-data ceiling).

use astock_core::Bar;
use tracing::warn;

/// Filter structurally invalid bars, warning about every dropped row and
/// about suspiciously short survivors (< 10 bars).
pub fn filter_valid_bars(symbol: &str, bars: Vec<Bar>) -> Vec<Bar> {
    filter_bars_with(symbol, bars, Bar::is_valid)
}

/// Same filter for index series, whose levels routinely exceed the
/// individual-security dirty-data ceiling of 10000.
pub fn filter_valid_index_bars(symbol: &str, bars: Vec<Bar>) -> Vec<Bar> {
    filter_bars_with(symbol, bars, Bar::is_valid_index)
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
}
