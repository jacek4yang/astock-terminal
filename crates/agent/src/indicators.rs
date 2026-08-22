//! Indicator series missing from `astock-technical` (which ships MA/MACD
//! only): RSI (Wilder), KDJ (Chinese 9,3,3 SMA recursion) and BOLL (20,2).
//!
//! All functions are pure and deterministic; the LLM never computes numbers.

/// An indicator series aligned with the input bars.
pub type Series = Vec<Option<f64>>;

/// Three aligned indicator series (e.g. K/D/J or mid/upper/lower).
pub type TripleSeries = (Series, Series, Series);

/// Wilder RSI series. First `period` entries are `None`.
pub fn rsi_series(closes: &[f64], period: usize) -> Vec<Option<f64>> {
    let n = closes.len();
    let mut out = vec![None; n];
    if n <= period || period == 0 {
        return out;
    }
    let mut gain = 0.0;
    let mut loss = 0.0;
    for i in 1..=period {
        let diff = closes[i] - closes[i - 1];
        if diff > 0.0 {
            gain += diff;
        } else {
            loss -= diff;
        }
    }
    let mut avg_gain = gain / period as f64;
    let mut avg_loss = loss / period as f64;
    out[period] = Some(rsi(avg_gain, avg_loss));
    for i in (period + 1)..n {
        let diff = closes[i] - closes[i - 1];
        let (g, l) = if diff > 0.0 { (diff, 0.0) } else { (0.0, -diff) };
        avg_gain = (avg_gain * (period as f64 - 1.0) + g) / period as f64;
        avg_loss = (avg_loss * (period as f64 - 1.0) + l) / period as f64;
        out[i] = Some(rsi(avg_gain, avg_loss));
    }
    out
}

fn rsi(avg_gain: f64, avg_loss: f64) -> f64 {
    if avg_loss == 0.0 {
        return 100.0;
    }
    100.0 - 100.0 / (1.0 + avg_gain / avg_loss)
}

/// Chinese KDJ (9,3,3): RSV over a 9-bar window, K/D seeded at 50 with the
/// 1/3 SMA recursion, J = 3K − 2D. Returns `(K, D, J)` series aligned with
/// the input; entries before the first full window are `None`.
pub fn kdj_series(
    highs: &[f64],
    lows: &[f64],
    closes: &[f64],
    period: usize,
) -> TripleSeries {
    let n = closes.len();
    let (mut ks, mut ds, mut js): TripleSeries = (vec![None; n], vec![None; n], vec![None; n]);
    if n < period || period == 0 {
        return (ks, ds, js);
    }
    let mut k = 50.0;
    let mut d = 50.0;
    for i in (period - 1)..n {
        let lo = lows[i + 1 - period..=i]
            .iter()
            .fold(f64::INFINITY, |a, &b| a.min(b));
        let hi = highs[i + 1 - period..=i]
            .iter()
            .fold(f64::NEG_INFINITY, |a, &b| a.max(b));
        let rsv = if hi > lo {
            (closes[i] - lo) / (hi - lo) * 100.0
        } else {
            50.0
        };
        k = 2.0 / 3.0 * k + 1.0 / 3.0 * rsv;
        d = 2.0 / 3.0 * d + 1.0 / 3.0 * k;
        let j = 3.0 * k - 2.0 * d;
        ks[i] = Some(k);
        ds[i] = Some(d);
        js[i] = Some(j);
    }
    (ks, ds, js)
}

/// Bollinger bands `(mid, upper, lower)`: SMA(period) ± mult × population
/// standard deviation. Entries before the first full window are `None`.
pub fn bollinger_series(
    closes: &[f64],
    period: usize,
    mult: f64,
) -> TripleSeries {
    let n = closes.len();
    let (mut mid, mut up, mut lo): TripleSeries = (vec![None; n], vec![None; n], vec![None; n]);
    if n < period || period == 0 {
        return (mid, up, lo);
    }
    for i in (period - 1)..n {
        let window = &closes[i + 1 - period..=i];
        let mean = window.iter().sum::<f64>() / period as f64;
        let var = window.iter().map(|c| (c - mean) * (c - mean)).sum::<f64>() / period as f64;
        let sd = var.sqrt();
        mid[i] = Some(mean);
        up[i] = Some(mean + mult * sd);
        lo[i] = Some(mean - mult * sd);
    }
    (mid, up, lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsi_all_gains_is_100() {
        let closes: Vec<f64> = (0..20).map(|i| 10.0 + i as f64).collect();
        let rsi = rsi_series(&closes, 14);
        assert!(rsi[..14].iter().all(Option::is_none));
        assert_eq!(rsi[14], Some(100.0));
        assert_eq!(rsi[19], Some(100.0));
    }

    #[test]
    fn rsi_known_value() {
        // Classic Wilder example shape: symmetric up/down of equal size
        // over the seed window gives RSI 50.
        let mut closes = vec![100.0];
        for i in 1..=14 {
            closes.push(if i % 2 == 0 { 100.0 } else { 101.0 });
        }
        let rsi = rsi_series(&closes, 14);
        assert!((rsi[14].unwrap() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn kdj_bounds_and_seed() {
        let highs = vec![10.0; 20];
        let lows = vec![9.0; 20];
        let closes = vec![9.5; 20];
        let (k, d, j) = kdj_series(&highs, &lows, &closes, 9);
        assert!(k[..8].iter().all(Option::is_none));
        // RSV is constantly 50 and K/D seed at 50 → all stay at 50.
        assert!((k[19].unwrap() - 50.0).abs() < 1e-9);
        assert!((d[19].unwrap() - 50.0).abs() < 1e-9);
        assert!((j[19].unwrap() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn bollinger_flat_series_collapses() {
        let closes = vec![7.5; 30];
        let (mid, up, lo) = bollinger_series(&closes, 20, 2.0);
        assert!(mid[..19].iter().all(Option::is_none));
        assert_eq!(mid[29], Some(7.5));
        assert_eq!(up[29], Some(7.5));
        assert_eq!(lo[29], Some(7.5));
    }
}
