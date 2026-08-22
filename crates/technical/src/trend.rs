//! Trend module, ported 1:1 from `analysis/trend_module.py`.

use crate::indicators::{ma_direction, sma_series};
use crate::types::Kline;
use crate::util::py_round;
use serde::{Deserialize, Serialize};

/// Per-item MA scores, serialized in the legacy key order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MaScores {
    pub ma20_dir: i64,
    pub ma60_dir: i64,
    pub price_vs_ma20: i64,
    pub price_vs_ma60: i64,
    pub resonance: i64,
}

/// A detected uptrend line (only uptrends are considered, as in legacy).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trendline {
    #[serde(rename = "type")]
    pub kind: String,
    pub slope: f64,
    pub current_price: f64,
    pub points: [i64; 2],
}

/// Result of [`analyze_trend`], mirroring the legacy `TrendResult`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendResult {
    pub direction: String,
    pub strength: i64,
    pub stage: String,
    pub ma_arrangement: String,
    pub ma_scores: MaScores,
    pub trendline: Option<Trendline>,
    pub signals: Vec<String>,
}

/// Find the most recent valid uptrend line.
///
/// Legacy rules: window = last 20 bars; troughs use a ±5 neighbourhood
/// truncated to the window; `t0` = first in-window trough (or window start);
/// `t1` = lowest low after `t0`; valid only if `low[t1] > low[t0]` and
/// `low[t1]` undercuts the pre-`t0` window minimum; slope must be positive.
fn find_trendline(klines: &[Kline], direction: &str) -> Option<Trendline> {
    if direction != "上升" || klines.len() < 21 {
        return None;
    }
    let lows: Vec<f64> = klines.iter().map(|k| k.low).collect();
    let n = lows.len();
    let window_start = n - 20;
    let window_end = n - 1;

    // In-window local troughs (neighbourhood truncated to window bounds)
    let mut troughs = Vec::new();
    for i in window_start..=window_end {
        let lo = window_start.max(i.saturating_sub(5));
        let hi = window_end.min(i + 5);
        if lo == i && hi == i {
            continue;
        }
        let min_neighbor = lows[lo..i]
            .iter()
            .chain(lows[i + 1..=hi].iter())
            .copied()
            .fold(f64::INFINITY, f64::min);
        if lows[i] <= min_neighbor {
            troughs.push(i);
        }
    }

    let t0 = troughs.first().copied().unwrap_or(window_start);
    // t1 = point with the smallest low after t0 within the window
    let after = &lows[t0 + 1..=window_end];
    if after.is_empty() {
        return None;
    }
    let min_after = after.iter().copied().fold(f64::INFINITY, f64::min);
    let t1 = t0 + 1 + after.iter().position(|&v| v == min_after)?;

    // Validity: rising, and t1 below the lowest low before t0 in the window
    if lows[t1] <= lows[t0] {
        return None;
    }
    if t0 > window_start {
        let left_min = lows[window_start..t0]
            .iter()
            .copied()
            .fold(f64::INFINITY, f64::min);
        if lows[t1] >= left_min {
            return None;
        }
    }
    let slope = (lows[t1] - lows[t0]) / (t1 - t0) as f64;
    if slope <= 0.0 {
        return None;
    }
    Some(Trendline {
        kind: "上升趋势线".to_string(),
        slope: py_round(slope, 4),
        current_price: py_round(lows[t0] + slope * (n - 1 - t0) as f64, 2),
        points: [(t0 - window_start) as i64, (t1 - window_start) as i64],
    })
}

/// MA sub-scores plus derived signals. Returns `(ma_scores, signals,
/// ma20_val, ma60_val)`; values are `None` when data is insufficient.
fn calc_ma_scores(klines: &[Kline]) -> (MaScores, Vec<String>, Option<f64>, Option<f64>) {
    let closes: Vec<f64> = klines.iter().map(|k| k.close).collect();
    let price = closes[closes.len() - 1];
    let ma20 = sma_series(&closes, 20);
    let ma60 = sma_series(&closes, 60);
    let (ma20_last, ma60_last) = (ma20[ma20.len() - 1], ma60[ma60.len() - 1]);
    let (Some(ma20_val), Some(ma60_val)) = (ma20_last, ma60_last) else {
        return (MaScores::default(), Vec::new(), ma20_last, ma60_last);
    };

    let ma20_dir = ma_direction(&ma20, 5);
    let ma60_dir = ma_direction(&ma60, 5);

    let mut ma_scores = MaScores::default();
    let mut signals = Vec::new();

    // MA20 direction (30 pts)
    ma_scores.ma20_dir = if ma20_dir == "向上" { 30 } else { 0 };
    if ma20_dir == "向上" {
        signals.push("MA20向上".to_string());
    } else if ma20_dir == "向下" {
        signals.push("MA20向下".to_string());
    }

    // MA60 direction (25 pts)
    ma_scores.ma60_dir = if ma60_dir == "向上" { 25 } else { 0 };

    // Price vs MA20 (15 pts)
    ma_scores.price_vs_ma20 = if price > ma20_val { 15 } else { 0 };

    // Price vs MA60 (10 pts) — the 60-day decision line
    ma_scores.price_vs_ma60 = if price > ma60_val { 10 } else { 0 };
    if price > ma60_val {
        signals.push("站稳60日决策线".to_string());
    }

    // MA resonance (20 pts): positive 20-day price gain
    let gain20 = if closes.len() >= 21 && closes[closes.len() - 21] != 0.0 {
        (price - closes[closes.len() - 21]) / closes[closes.len() - 21] * 100.0
    } else {
        0.0
    };
    ma_scores.resonance = if gain20 > 0.0 { 20 } else { 0 };

    (ma_scores, signals, Some(ma20_val), Some(ma60_val))
}

/// Stage from direction + strength (thresholds 70/45/30 as in legacy).
fn calc_stage(direction: &str, strength: i64) -> &'static str {
    if direction == "上升" {
        if strength >= 70 {
            return "强势上升趋势";
        }
        if strength >= 45 {
            return "上升趋势形成中";
        }
        return "弱势上升";
    }
    if direction == "下降" {
        if strength <= 30 {
            return "强势下降趋势";
        }
        return "下降趋势";
    }
    "震荡整理"
}

/// Analyze trend over the given bars. Legacy behaviour: `ma_arrangement` is
/// hardcoded to "纠缠" (verified against 8/8 encrypted-version samples); the
/// real arrangement computation exists only as dead code upstream.
pub fn analyze_trend(klines: &[Kline]) -> TrendResult {
    let (ma_scores, signals, ma20_val, _ma60_val) = calc_ma_scores(klines);
    let strength = ma_scores.ma20_dir
        + ma_scores.ma60_dir
        + ma_scores.price_vs_ma20
        + ma_scores.price_vs_ma60
        + ma_scores.resonance;

    let closes: Vec<f64> = klines.iter().map(|k| k.close).collect();
    let price = closes[closes.len() - 1];

    let direction = if let Some(ma20_val) = ma20_val {
        if ma20_val > 0.0 {
            if price > ma20_val && ma_scores.ma20_dir == 30 {
                "上升"
            } else if price < ma20_val && ma_scores.ma20_dir == 0 {
                "下降"
            } else {
                "震荡"
            }
        } else {
            "震荡"
        }
    } else {
        "震荡"
    };

    // Legacy: ma_arrangement is always "纠缠" (hardcoded; kept deliberately).
    let arrangement = "纠缠".to_string();
    let stage = calc_stage(direction, strength).to_string();
    let trendline = find_trendline(klines, direction);

    TrendResult {
        direction: direction.to_string(),
        strength,
        stage,
        ma_arrangement: arrangement,
        ma_scores,
        trendline,
        signals,
    }
}
