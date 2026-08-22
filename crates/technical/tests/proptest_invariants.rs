//! Property-based invariants over the indicators and the full pipeline.

use astock_technical::breakout::calc_true_range;
use astock_technical::indicators::{ema_series, find_peaks, find_troughs, macd_series, sma_series};
use astock_technical::types::Kline;
use proptest::prelude::*;

fn series_strategy(max_len: usize) -> impl Strategy<Value = Vec<f64>> {
    proptest::collection::vec(1.0f64..1000.0, 1..max_len)
}

/// Random klines preserving the H >= max(O, C, L) OHLC invariant.
fn klines_strategy(max_len: usize) -> impl Strategy<Value = Vec<Kline>> {
    proptest::collection::vec(
        (10.0f64..1000.0, 0.0f64..20.0, 0.0f64..20.0, 0.0f64..1000.0),
        1..max_len,
    )
    .prop_map(|rows| {
        rows.into_iter()
            .enumerate()
            .map(|(i, (base, up, down, volume))| {
                let mid = base;
                let open = mid - down / 2.0;
                let close = mid + up / 2.0;
                let low = mid - down; // <= open and <= close
                let high = mid + up; // >= open and >= close
                Kline {
                    date: format!("2024-01-{:02}", (i % 28) + 1),
                    open,
                    close,
                    high,
                    low,
                    volume,
                    amount: volume * close,
                    pct: 0.0,
                    turnover: 1.0,
                }
            })
            .collect()
    })
}

proptest! {
    #[test]
    fn sma_stays_within_data_range(values in series_strategy(120), period in 1usize..30) {
        let s = sma_series(&values, period);
        prop_assert_eq!(s.len(), values.len());
        let (min, max) = values.iter().copied().fold((f64::INFINITY, f64::NEG_INFINITY),
            |(lo, hi), v| (lo.min(v), hi.max(v)));
        for (i, v) in s.iter().enumerate() {
            if i < period - 1 {
                prop_assert!(v.is_none());
            } else {
                let v = v.unwrap();
                prop_assert!(v >= min - 1e-9 && v <= max + 1e-9);
            }
        }
    }

    #[test]
    fn ema_stays_within_data_range(values in series_strategy(120), period in 1usize..30) {
        let e = ema_series(&values, period);
        prop_assert_eq!(e.len(), values.len());
        let (min, max) = values.iter().copied().fold((f64::INFINITY, f64::NEG_INFINITY),
            |(lo, hi), v| (lo.min(v), hi.max(v)));
        for v in e.iter().flatten() {
            prop_assert!(*v >= min - 1e-9 && *v <= max + 1e-9);
        }
    }

    #[test]
    fn macd_bar_equals_twice_dif_minus_dea(values in series_strategy(120)) {
        let (dif, dea, bar) = macd_series(&values, 12, 26, 9);
        prop_assert_eq!(dif.len(), values.len());
        for i in 0..values.len() {
            prop_assert!((bar[i] - 2.0 * (dif[i] - dea[i])).abs() < 1e-9);
        }
    }

    #[test]
    fn peaks_and_troughs_are_extrema(values in series_strategy(80), window in 1usize..6) {
        for i in find_peaks(&values, window) {
            for j in (i - window)..=(i + window) {
                prop_assert!(values[i] >= values[j]);
            }
        }
        for i in find_troughs(&values, window) {
            for j in (i - window)..=(i + window) {
                prop_assert!(values[i] <= values[j]);
            }
        }
    }

    #[test]
    fn true_range_covers_high_low(
        high in 10.0f64..1000.0,
        span in 0.0f64..50.0,
        pre_close in 10.0f64..1000.0,
    ) {
        let low = high - span;
        let tr = calc_true_range(high, low, pre_close);
        prop_assert!(tr >= high - low);
        prop_assert!(tr >= (high - pre_close).abs());
        prop_assert!(tr >= (low - pre_close).abs());
    }

    #[test]
    fn pipeline_never_panics_and_scores_bounded(klines in klines_strategy(300)) {
        // Full engine on bare klines: no quote/flows/index/breadth.
        let signal = astock_technical::analyze(&klines, None, None, None, None);
        let score = signal.get("score").and_then(serde_json::Value::as_i64).unwrap();
        prop_assert!((0..=100).contains(&score));
        let confidence = signal.get("confidence").and_then(serde_json::Value::as_i64).unwrap();
        prop_assert!(confidence >= 10);
        let patterns = signal.get("patterns").and_then(serde_json::Value::as_array).unwrap();
        prop_assert!(patterns.len() <= 3);
        // OHLC invariant of the generated inputs still holds (sanity).
        for k in &klines {
            prop_assert!(k.high >= k.open.max(k.close).max(k.low));
        }
    }
}
