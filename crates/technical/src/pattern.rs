//! Candlestick pattern module, ported 1:1 from `analysis/pattern_module.py`.
//!
//! Detectors run on fixed slices (60-bar: head-shoulders / double / flag /
//! gap / rounding; 20-bar: triangle / box) and fire in the legacy order:
//! 头肩 → 双顶/双底 → 三角形 → 箱体 → 旗形 → 跳空 → 双弧底, max 3 results.

use crate::indicators::{find_peaks, find_troughs};
use crate::types::Kline;
use crate::util::py_round;
use serde::{Deserialize, Serialize};

/// One detected pattern, mirroring the legacy `PatternResult`. `key_levels`
/// keeps insertion order (serde_json preserves it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternResult {
    pub name: String,
    pub direction: String,
    pub confidence: i64,
    pub status: String,
    pub target_price: Option<f64>,
    pub key_levels: Vec<(String, f64)>,
    pub description: String,
}

fn highs(klines: &[Kline]) -> Vec<f64> {
    klines.iter().map(|k| k.high).collect()
}

fn lows(klines: &[Kline]) -> Vec<f64> {
    klines.iter().map(|k| k.low).collect()
}

/// 箱体震荡 over the last 20 bars, amplitude 5–25%.
///
/// Legacy note: the original had a dead "已突破" branch (`price >= upper`),
/// fully covered by the reachable `price >= upper * 0.98` branch. The port
/// keeps only the two reachable branches — behaviour is identical.
fn detect_box(klines: &[Kline], price: f64) -> Option<PatternResult> {
    if klines.len() < 20 {
        return None;
    }
    let window = &klines[klines.len() - 20..];
    let upper = window
        .iter()
        .map(|k| k.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let lower = window.iter().map(|k| k.low).fold(f64::INFINITY, f64::min);
    if upper <= lower {
        return None;
    }
    let amplitude = (upper - lower) / lower * 100.0;
    if !(5.0..=25.0).contains(&amplitude) {
        return None;
    }

    let (status, direction, confidence, target) = if price >= upper * 0.98 {
        ("接近突破上沿", "看涨", 55, Some(upper + (upper - lower)))
    } else {
        ("形成中", "中性", 55, None)
    };

    let desc = format!(
        "箱体振幅{:.1}%，突破上沿目标{:.2}，跌破下沿目标{:.2}",
        amplitude,
        upper + (upper - lower),
        lower - (upper - lower)
    );
    Some(PatternResult {
        name: "箱体震荡".to_string(),
        direction: direction.to_string(),
        confidence,
        status: status.to_string(),
        target_price: target.map(|t| py_round(t, 2)),
        key_levels: vec![
            ("箱体上沿".to_string(), py_round(upper, 2)),
            ("箱体下沿".to_string(), py_round(lower, 2)),
        ],
        description: desc,
    })
}

/// 双底 / 双顶 over a 60-bar slice. Only the last trough/peak pair (window 5)
/// is examined, as in legacy. Double tops are only reported once price has
/// broken below the neck.
fn detect_double_top_bottom(klines: &[Kline], price: f64) -> Option<PatternResult> {
    if klines.len() < 60 {
        return None;
    }
    let lows_v = lows(klines);
    let highs_v = highs(klines);
    let troughs = find_troughs(&lows_v, 5);

    // 双底: last two troughs, < 3% apart, >= 5 bars apart, neck = mid high
    if troughs.len() >= 2 {
        let (t0, t1) = (troughs[troughs.len() - 2], troughs[troughs.len() - 1]);
        if t1 - t0 >= 5 {
            let (v0, v1) = (lows_v[t0], lows_v[t1]);
            if (v0 - v1).abs() / v0.min(v1) <= 0.03 {
                let neck = klines[t0..=t1]
                    .iter()
                    .map(|k| k.high)
                    .fold(f64::NEG_INFINITY, f64::max);
                let bottom = v0;
                if neck > bottom {
                    let target = neck + (neck - bottom);
                    let status = if price > neck {
                        "已突破"
                    } else {
                        "形成中"
                    };
                    return Some(PatternResult {
                        name: "双底".to_string(),
                        direction: "看涨".to_string(),
                        confidence: 55,
                        status: status.to_string(),
                        target_price: Some(py_round(target, 2)),
                        key_levels: vec![
                            ("颈线".to_string(), py_round(neck, 2)),
                            ("底部".to_string(), py_round(bottom, 2)),
                        ],
                        description: "双底可靠性约10%，需等待价格确认突破颈线".to_string(),
                    });
                }
            }
        }
    }

    // 双顶: last two peaks, neck = mid low; only when price < neck
    let peaks = find_peaks(&highs_v, 5);
    if peaks.len() >= 2 {
        let (p0, p1) = (peaks[peaks.len() - 2], peaks[peaks.len() - 1]);
        if p1 - p0 >= 5 {
            let (v0, v1) = (highs_v[p0], highs_v[p1]);
            if (v0 - v1).abs() / v0.min(v1) <= 0.03 {
                let neck = klines[p0..=p1]
                    .iter()
                    .map(|k| k.low)
                    .fold(f64::INFINITY, f64::min);
                let top = v0.max(v1);
                if neck < top && price < neck {
                    let target = neck - (top - neck);
                    return Some(PatternResult {
                        name: "双顶".to_string(),
                        direction: "看跌".to_string(),
                        confidence: 55,
                        status: "已突破".to_string(),
                        target_price: Some(py_round(target, 2)),
                        key_levels: vec![
                            ("颈线".to_string(), py_round(neck, 2)),
                            ("顶部".to_string(), py_round(top, 2)),
                        ],
                        description: format!("双顶颈线{:.2}，跌破后目标{:.2}", neck, target),
                    });
                }
            }
        }
    }
    None
}

/// 头肩底 / 头肩顶 over a 60-bar slice, using only the last three
/// troughs/peaks (window 3). Shoulders must be within 8%.
fn detect_head_shoulders(klines: &[Kline], price: f64) -> Option<PatternResult> {
    if klines.len() < 10 {
        return None;
    }
    let lows_v = lows(klines);
    let highs_v = highs(klines);
    let troughs = find_troughs(&lows_v, 3);

    // 头肩底: three troughs, middle one lowest
    if troughs.len() >= 3 {
        let (l, m, r) = (
            troughs[troughs.len() - 3],
            troughs[troughs.len() - 2],
            troughs[troughs.len() - 1],
        );
        if m - l >= 2 && r - m >= 2 {
            let (vl, vm, vr) = (lows_v[l], lows_v[m], lows_v[r]);
            if vm < vl && vm < vr && (vl - vr).abs() / vl.min(vr) <= 0.08 {
                // Neck = high of the higher-shoulder bar (legacy behaviour)
                let neck = highs_v[l].max(highs_v[r]);
                let depth = neck - vm;
                if neck > vm {
                    let target = neck + depth;
                    let status = if price > neck {
                        "已突破"
                    } else {
                        "形成中"
                    };
                    let confidence = if status == "已突破" { 80 } else { 60 };
                    return Some(PatternResult {
                        name: "头肩底".to_string(),
                        direction: "看涨".to_string(),
                        confidence,
                        status: status.to_string(),
                        target_price: Some(py_round(target, 2)),
                        key_levels: vec![
                            ("颈线".to_string(), py_round(neck, 2)),
                            ("头部".to_string(), py_round(vm, 2)),
                        ],
                        description: format!("底部深度{:.2}，突破颈线后目标{:.2}", depth, target),
                    });
                }
            }
        }
    }

    // 头肩顶: three peaks, middle one highest
    let peaks = find_peaks(&highs_v, 3);
    if peaks.len() >= 3 {
        let (l, m, r) = (
            peaks[peaks.len() - 3],
            peaks[peaks.len() - 2],
            peaks[peaks.len() - 1],
        );
        if m - l >= 2 && r - m >= 2 {
            let (vl, vm, vr) = (highs_v[l], highs_v[m], highs_v[r]);
            if vm > vl && vm > vr && (vl - vr).abs() / vl.min(vr) <= 0.08 {
                // Neck = low of the higher-peak shoulder bar (symmetric)
                let neck = lows_v[l].min(lows_v[r]);
                if neck < vm {
                    let height = vm - neck;
                    let target = neck - height;
                    let status = if price < neck {
                        "已突破"
                    } else {
                        "形成中"
                    };
                    return Some(PatternResult {
                        name: "头肩顶".to_string(),
                        direction: "看跌".to_string(),
                        confidence: if status == "形成中" { 45 } else { 60 },
                        status: status.to_string(),
                        target_price: Some(py_round(target, 2)),
                        key_levels: vec![
                            ("颈线".to_string(), py_round(neck, 2)),
                            ("头部".to_string(), py_round(vm, 2)),
                        ],
                        description: format!("头部高度{:.2}，跌破颈线后目标{:.2}", height, target),
                    });
                }
            }
        }
    }
    None
}

/// 双弧底 (rounding bottom): price lows form an arc over the last 60 bars
/// with the minimum in `[15, len-15]`, plus a positive volume floor.
fn detect_rounding_bottom(klines: &[Kline], _price: f64) -> Option<PatternResult> {
    if klines.len() < 60 {
        return None;
    }
    let window = &klines[klines.len() - 60..];
    let lows_v = lows(window);
    let min_low = lows_v.iter().copied().fold(f64::INFINITY, f64::min);
    let min_idx = lows_v.iter().position(|&v| v == min_low)?;
    if min_idx < 15 || min_idx > window.len() - 15 {
        return None;
    }
    let left = &lows_v[..min_idx];
    let right = &lows_v[min_idx + 1..];
    if left.len() < 8 || right.len() < 8 {
        return None;
    }
    // Arc shape: left side descends toward the low, right side ascends.
    // Legacy quirk: the left check iterates `range(len(left) - 2)` and skips
    // non-positive samples; the last left element is never compared.
    let left_desc = (0..left.len().saturating_sub(2))
        .filter(|&i| left[i] > 0.0)
        .all(|i| left[i] >= left[i + 1]);
    let right_asc = (0..right.len().saturating_sub(1)).all(|i| right[i] <= right[i + 1]);
    if !(left_desc && right_asc) {
        return None;
    }
    // Volume arc floor (legacy only checks the ±5 bar minimum is positive)
    let vols: Vec<f64> = window.iter().map(|k| k.volume).collect();
    let vol_start = min_idx.saturating_sub(5);
    let vol_end = (min_idx + 6).min(vols.len());
    let vol_min = vols[vol_start..vol_end]
        .iter()
        .copied()
        .fold(f64::INFINITY, f64::min);
    if vol_min <= 0.0 {
        return None;
    }
    let bottom = min_low;
    Some(PatternResult {
        name: "双弧底".to_string(),
        direction: "看涨".to_string(),
        confidence: 70,
        status: "形成中".to_string(),
        target_price: None,
        key_levels: vec![("弧底低点".to_string(), py_round(bottom, 2))],
        description: "K线与成交量同时呈圆弧底，连续放量2-3日可确认起涨".to_string(),
    })
}

/// Triangle over a 30-bar window (two 15-bar halves).
///
/// Legacy note: `analyze_patterns` calls this with the 20-bar slice, whose
/// length never reaches 30, so the detector is effectively dead code there.
/// It is ported faithfully anyway.
fn detect_triangle(klines: &[Kline], price: f64) -> Option<PatternResult> {
    if klines.len() < 30 {
        return None;
    }
    let window = &klines[klines.len().saturating_sub(30)..];
    let first_half = &window[..15];
    let second_half = &window[15..];
    if first_half.is_empty() || second_half.is_empty() {
        return None;
    }
    let h1 = first_half
        .iter()
        .map(|k| k.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let h2 = second_half
        .iter()
        .map(|k| k.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let l1 = first_half
        .iter()
        .map(|k| k.low)
        .fold(f64::INFINITY, f64::min);
    let l2 = second_half
        .iter()
        .map(|k| k.low)
        .fold(f64::INFINITY, f64::min);
    let upper_slope = h2 - h1;
    let lower_slope = l2 - l1;

    if upper_slope.abs() < 1e-6 || lower_slope.abs() < 1e-6 {
        return None;
    }

    // Symmetric: lower highs + higher lows
    if upper_slope < 0.0 && lower_slope > 0.0 {
        return Some(PatternResult {
            name: "对称三角形".to_string(),
            direction: "中性".to_string(),
            confidence: 50,
            status: "形成中".to_string(),
            target_price: None,
            key_levels: vec![],
            description: "对称三角形通常延续原有趋势，等待方向选择".to_string(),
        });
    }
    // Ascending: flat highs + higher lows
    if upper_slope.abs() / h1 < 0.02 && lower_slope > 0.0 {
        let status = if price > h2 {
            "已突破"
        } else {
            "接近突破"
        };
        return Some(PatternResult {
            name: "上升三角形".to_string(),
            direction: "看涨".to_string(),
            confidence: 60,
            status: status.to_string(),
            target_price: Some(py_round(h2 + (h2 - l1), 2)),
            key_levels: vec![("阻力位".to_string(), py_round(h2, 2))],
            description: format!("上升三角形偏多，突破{:.2}确认", py_round(h2, 2)),
        });
    }
    // Descending: flat lows + lower highs
    if lower_slope.abs() / l1 < 0.02 && upper_slope < 0.0 {
        let status = if price < l2 {
            "已跌破"
        } else {
            "接近跌破"
        };
        return Some(PatternResult {
            name: "下降三角形".to_string(),
            direction: "看跌".to_string(),
            confidence: 60,
            status: status.to_string(),
            target_price: Some(py_round(l2 - (h1 - l2), 2)),
            key_levels: vec![("支撑位".to_string(), py_round(l2, 2))],
            description: format!("下降三角形偏空，跌破{:.2}确认", py_round(l2, 2)),
        });
    }
    None
}

/// 上升旗形: 15-bar pole rise followed by a 15-bar flag with <= 8% range.
fn detect_flag(klines: &[Kline], _price: f64) -> Option<PatternResult> {
    if klines.len() < 30 {
        return None;
    }
    let pole = &klines[klines.len() - 30..klines.len() - 15];
    let flag = &klines[klines.len() - 15..];
    if pole.is_empty() || flag.is_empty() {
        return None;
    }
    let pole_rise = pole[pole.len() - 1].close - pole[0].close;
    if pole_rise <= 0.0 {
        return None;
    }
    let flag_high = flag
        .iter()
        .map(|k| k.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let flag_low = flag.iter().map(|k| k.low).fold(f64::INFINITY, f64::min);
    let flag_range = if flag_low != 0.0 {
        (flag_high - flag_low) / flag_low
    } else {
        0.0
    };
    if flag_range > 0.08 {
        return None;
    }
    Some(PatternResult {
        name: "上升旗形".to_string(),
        direction: "看涨".to_string(),
        confidence: 60,
        status: "整理中".to_string(),
        target_price: Some(py_round(flag_high + pole_rise, 2)),
        key_levels: vec![("旗形上沿".to_string(), py_round(flag_high, 2))],
        description: format!(
            "旗杆涨幅{:.2}，突破后目标{:.2}",
            py_round(pole_rise, 2),
            py_round(flag_high + pole_rise, 2)
        ),
    })
}

/// Gap between the last two bars of the slice.
fn detect_gap(klines: &[Kline], _price: f64) -> Option<PatternResult> {
    if klines.len() < 5 {
        return None;
    }
    let latest = &klines[klines.len() - 1];
    let prev = &klines[klines.len() - 2];
    if latest.low > prev.high {
        let gap = latest.low - prev.high;
        return Some(PatternResult {
            name: "向上突破缺口".to_string(),
            direction: "看涨".to_string(),
            confidence: 65,
            status: "已形成".to_string(),
            target_price: None,
            key_levels: vec![("缺口上沿".to_string(), py_round(latest.low, 2))],
            description: format!("向上跳空缺口{:.2}，回补前视为支撑", gap),
        });
    }
    if latest.high < prev.low {
        let gap = prev.low - latest.high;
        return Some(PatternResult {
            name: "向下突破缺口".to_string(),
            direction: "看跌".to_string(),
            confidence: 65,
            status: "已形成".to_string(),
            target_price: None,
            key_levels: vec![("缺口下沿".to_string(), py_round(latest.high, 2))],
            description: format!("向下跳空缺口{:.2}，回补前视为压力", gap),
        });
    }
    None
}

/// Combined pattern detection over fixed slices, in the legacy detector
/// order, capped at 3 results. (Python wrapped each detector in a bare
/// try/except; the port is panic-free so no equivalent is needed.)
pub fn analyze_patterns(klines: &[Kline]) -> Vec<PatternResult> {
    if klines.is_empty() {
        // Legacy would raise IndexError on klines[-1]; callers never do this.
        return Vec::new();
    }
    let price = klines[klines.len() - 1].close;
    let start60 = klines.len().saturating_sub(60);
    let start20 = klines.len().saturating_sub(20);
    let window60 = &klines[start60..];
    let window20 = &klines[start20..];

    // Slices per legacy: 头肩/双顶双底/旗形/跳空/双弧底 → 60-bar;
    // 三角形/箱体 → 20-bar. Detection order is significant and preserved.
    let candidates = [
        detect_head_shoulders(window60, price),
        detect_double_top_bottom(window60, price),
        detect_triangle(window20, price),
        detect_box(window20, price),
        detect_flag(window60, price),
        detect_gap(window60, price),
        detect_rounding_bottom(window60, price),
    ];
    let mut results: Vec<PatternResult> = candidates.into_iter().flatten().collect();
    results.truncate(3);
    results
}
