//! Shared technical indicators, ported 1:1 from `analysis/_indicators.py`.
//!
//! All functions are pure and replicate the legacy floating-point operation
//! order so results are bit-identical to the Python originals.

use crate::util::py_round;

/// Simple moving average series. The first `period - 1` positions are `None`
/// placeholders, mirroring the legacy implementation (including its running
/// sum order).
pub fn sma_series(values: &[f64], period: usize) -> Vec<Option<f64>> {
    if period == 0 || values.is_empty() {
        return Vec::new();
    }
    let mut result: Vec<Option<f64>> = vec![None; values.len()];
    let mut running = 0.0_f64;
    for (i, &v) in values.iter().enumerate() {
        running += v;
        if i >= period {
            running -= values[i - period];
        }
        if i >= period - 1 {
            result[i] = Some(running / period as f64);
        }
    }
    result
}

/// Exponential moving average series, seeded with the SMA of the first full
/// window. Positions before the seed are `None`; if the series is shorter
/// than `period` the whole result is `None`s.
pub fn ema_series(values: &[f64], period: usize) -> Vec<Option<f64>> {
    if period == 0 || values.is_empty() {
        return Vec::new();
    }
    let mut result: Vec<Option<f64>> = vec![None; values.len()];
    let alpha = 2.0 / (period + 1) as f64;
    if values.len() < period {
        return result;
    }
    let seed: f64 = values[..period].iter().sum::<f64>() / period as f64;
    let mut prev = seed;
    result[period - 1] = Some(prev);
    for i in period..values.len() {
        prev = alpha * values[i] + (1.0 - alpha) * prev;
        result[i] = Some(prev);
    }
    result
}

/// Last SMA value of the series; 0.0 when data is insufficient.
pub fn last_sma(values: &[f64], period: usize) -> f64 {
    if values.len() < period || period == 0 {
        return 0.0;
    }
    values[values.len() - period..].iter().sum::<f64>() / period as f64
}

/// MA direction over the last `lookback + 1` valid values: slope vs a
/// threshold of 0.2% of the base value. Returns 向上 / 向下 / 走平 / 未知.
pub fn ma_direction(ma_values: &[Option<f64>], lookback: usize) -> &'static str {
    let valid: Vec<f64> = ma_values.iter().flatten().copied().collect();
    if valid.len() < lookback + 1 {
        return "未知";
    }
    let recent = &valid[valid.len() - (lookback + 1)..];
    let slope = recent[recent.len() - 1] - recent[0];
    let base = recent[0].abs();
    let threshold = if base != 0.0 { base * 0.002 } else { 1e-9 };
    if slope > threshold {
        "向上"
    } else if slope < -threshold {
        "向下"
    } else {
        "走平"
    }
}

/// Local-peak indices: a full `window` neighbourhood on both sides (edges
/// excluded), `>=` all neighbours, and not a completely flat plateau.
pub fn find_peaks(values: &[f64], window: usize) -> Vec<usize> {
    let mut peaks = Vec::new();
    let n = values.len();
    if n < 2 * window + 1 {
        return peaks;
    }
    for i in window..(n - window) {
        let lo = i - window;
        let hi = i + window;
        let is_peak = (lo..=hi)
            .filter(|&j| j != i)
            .all(|j| values[i] >= values[j]);
        if is_peak && !(lo..=hi).all(|j| values[j] == values[i]) {
            peaks.push(i);
        }
    }
    peaks
}

/// Local-trough indices; mirror image of [`find_peaks`].
pub fn find_troughs(values: &[f64], window: usize) -> Vec<usize> {
    let mut troughs = Vec::new();
    let n = values.len();
    if n < 2 * window + 1 {
        return troughs;
    }
    for i in window..(n - window) {
        let lo = i - window;
        let hi = i + window;
        let is_trough = (lo..=hi)
            .filter(|&j| j != i)
            .all(|j| values[i] <= values[j]);
        if is_trough && !(lo..=hi).all(|j| values[j] == values[i]) {
            troughs.push(i);
        }
    }
    troughs
}

/// Least-squares slope over the given sample indices. `None` when fewer than
/// two points or zero x-variance.
pub fn fit_trendline(points_idx: &[usize], values: &[f64]) -> Option<f64> {
    if points_idx.len() < 2 {
        return None;
    }
    let xs: Vec<f64> = points_idx.iter().map(|&i| i as f64).collect();
    let ys: Vec<f64> = points_idx.iter().map(|&i| values[i]).collect();
    let n = xs.len() as f64;
    let mean_x = xs.iter().sum::<f64>() / n;
    let mean_y = ys.iter().sum::<f64>() / n;
    let denom: f64 = xs.iter().map(|x| (x - mean_x).powi(2)).sum();
    if denom == 0.0 {
        return None;
    }
    let slope: f64 = xs
        .iter()
        .zip(ys.iter())
        .map(|(x, y)| (x - mean_x) * (y - mean_y))
        .sum::<f64>()
        / denom;
    Some(slope)
}

/// MACD (12, 26, 9) triple: (DIF, DEA, BAR) where BAR = 2 × (DIF − DEA).
/// Returns zero-filled series when there is less data than the slow period,
/// mirroring the legacy behaviour.
pub fn macd_series(
    closes: &[f64],
    fast: usize,
    slow: usize,
    signal: usize,
) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    if closes.len() < slow {
        let n = closes.len();
        return (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    }
    let ema_fast = ema_series(closes, fast);
    let ema_slow = ema_series(closes, slow);
    let dif: Vec<f64> = ema_fast
        .iter()
        .zip(ema_slow.iter())
        .map(|(ef, es)| ef.unwrap_or(0.0) - es.unwrap_or(0.0))
        .collect();
    let dea: Vec<f64> = ema_series(&dif, signal)
        .iter()
        .map(|d| d.unwrap_or(0.0))
        .collect();
    let bar: Vec<f64> = dif
        .iter()
        .zip(dea.iter())
        .map(|(d, e)| 2.0 * (d - e))
        .collect();
    (dif, dea, bar)
}

/// MACD bar area (sum of absolute values) over `[start, end)`.
pub fn macd_area(macd_bar: &[f64], start: usize, end: usize) -> f64 {
    let hi = end.min(macd_bar.len());
    let lo = start.min(hi);
    macd_bar[lo..hi].iter().map(|v| v.abs()).sum()
}

/// Round a price to `digits` decimals (Python `round` semantics).
pub fn round_price(value: f64, digits: u32) -> f64 {
    py_round(value, digits)
}

/// Percent change from `start` to `end`; 0.0 when `start` is zero.
pub fn pct_change(start: f64, end: f64) -> f64 {
    if start == 0.0 {
        return 0.0;
    }
    (end - start) / start * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sma_hand_computed() {
        let v = [1.0, 2.0, 3.0, 4.0, 5.0];
        let s = sma_series(&v, 3);
        assert_eq!(s[0], None);
        assert_eq!(s[1], None);
        assert_eq!(s[2], Some(2.0));
        assert_eq!(s[3], Some(3.0));
        assert_eq!(s[4], Some(4.0));
        assert!(sma_series(&v, 0).is_empty());
        assert!(sma_series(&[], 3).is_empty());
    }

    #[test]
    fn ema_seeded_with_first_window_sma() {
        let v = [1.0, 2.0, 3.0, 4.0, 5.0];
        let e = ema_series(&v, 3);
        // seed = (1+2+3)/3 = 2 at index 2; alpha = 0.5
        assert_eq!(e[2], Some(2.0));
        assert_eq!(e[3], Some(0.5 * 4.0 + 0.5 * 2.0));
        assert_eq!(e[4], Some(0.5 * 5.0 + 0.5 * 3.0));
        // shorter than period -> all None
        assert!(ema_series(&v[..2], 3).iter().all(|x| x.is_none()));
    }

    #[test]
    fn last_sma_hand_computed() {
        let v = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(last_sma(&v, 2), 4.5);
        assert_eq!(last_sma(&v, 6), 0.0);
        assert_eq!(last_sma(&v, 0), 0.0);
    }

    #[test]
    fn ma_direction_threshold() {
        // base 100, threshold 0.2 over 5 steps
        let up: Vec<Option<f64>> = [100.0, 100.1, 100.2, 100.3, 100.4, 100.5]
            .iter()
            .map(|&v| Some(v))
            .collect();
        assert_eq!(ma_direction(&up, 5), "向上");
        let flat: Vec<Option<f64>> = [100.0, 100.0, 100.1, 100.0, 100.05, 100.1]
            .iter()
            .map(|&v| Some(v))
            .collect();
        assert_eq!(ma_direction(&flat, 5), "走平");
        let down: Vec<Option<f64>> = [100.0, 99.9, 99.8, 99.7, 99.6, 99.5]
            .iter()
            .map(|&v| Some(v))
            .collect();
        assert_eq!(ma_direction(&down, 5), "向下");
        let short: Vec<Option<f64>> = [Some(1.0), None, Some(2.0)].to_vec();
        assert_eq!(ma_direction(&short, 5), "未知");
    }

    #[test]
    fn peaks_and_troughs_exclude_plateaus() {
        let v = [1.0, 2.0, 3.0, 2.0, 1.0, 2.0, 3.0, 2.0, 1.0];
        assert_eq!(find_peaks(&v, 3), Vec::<usize>::new());
        let w = [1.0, 2.0, 4.0, 3.0, 2.0, 3.0, 5.0, 4.0, 3.0, 2.0, 1.0];
        assert_eq!(find_peaks(&w, 3), vec![6]);
        assert_eq!(find_troughs(&w, 3), vec![4]);
        // partial plateau still counts (matches Python), full plateau excluded
        let partial = [1.0, 5.0, 5.0, 5.0, 5.0, 5.0, 1.0];
        assert_eq!(find_peaks(&partial, 3), vec![3]);
        let flat = [5.0, 5.0, 5.0, 5.0, 5.0, 5.0, 5.0];
        assert!(find_peaks(&flat, 3).is_empty());
    }

    #[test]
    fn macd_bar_is_twice_dif_minus_dea() {
        let closes: Vec<f64> = (0..60).map(|i| 10.0 + (i as f64 * 0.37).sin()).collect();
        let (dif, dea, bar) = macd_series(&closes, 12, 26, 9);
        for i in 0..closes.len() {
            assert!((bar[i] - 2.0 * (dif[i] - dea[i])).abs() < 1e-12);
        }
        // short series -> zeros
        let (d, e, b) = macd_series(&closes[..10], 12, 26, 9);
        assert!(d.iter().all(|&x| x == 0.0));
        assert!(e.iter().all(|&x| x == 0.0));
        assert!(b.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn macd_area_sums_abs() {
        let bar = [1.0, -2.0, 3.0, -4.0];
        assert_eq!(macd_area(&bar, 0, 4), 10.0);
        assert_eq!(macd_area(&bar, 1, 3), 5.0);
        assert_eq!(macd_area(&bar, 2, 99), 7.0);
    }

    #[test]
    fn pct_change_and_round_price() {
        assert_eq!(pct_change(100.0, 105.0), 5.0);
        assert_eq!(pct_change(0.0, 5.0), 0.0);
        assert_eq!(round_price(1.23456, 2), 1.23);
    }

    #[test]
    fn fit_trendline_slope() {
        let values = [1.0, 2.0, 3.0, 4.0];
        let slope = fit_trendline(&[0, 1, 2, 3], &values).unwrap();
        assert!((slope - 1.0).abs() < 1e-12);
        assert_eq!(fit_trendline(&[0], &values), None);
    }
}
