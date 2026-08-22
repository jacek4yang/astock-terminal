//! Volume-price module, ported 1:1 from `analysis/volume_price_module.py`.

use crate::indicators::ma_direction;
use crate::types::{FundFlow, Kline, Quote};
use crate::util::py_round;
use serde::{Deserialize, Serialize};

/// Result of [`analyze_volume_price`], mirroring the legacy
/// `VolumePriceResult`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumePriceResult {
    pub pattern: String,
    pub direction: String,
    pub confidence: i64,
    pub volume_ratio: f64,
    pub turnover: f64,
    pub obv_trend: String,
    pub signals: Vec<String>,
    pub description: String,
}

/// OBV (on-balance volume) series.
fn calc_obv(klines: &[Kline]) -> Vec<f64> {
    let mut obv = Vec::with_capacity(klines.len());
    let mut running = 0.0;
    let mut prev_close: Option<f64> = None;
    for k in klines {
        if let Some(pc) = prev_close {
            if k.close != pc {
                running += if k.close > pc { k.volume } else { -k.volume };
            }
        }
        obv.push(running);
        prev_close = Some(k.close);
    }
    obv
}

/// Classify the price/volume pattern.
///
/// Price direction: 8-day gain > 2% → 涨, < −2% → 跌, else 平.
/// Volume direction: last-3d avg vs prior-5d avg (`vols[-8:-3]`), ±30%.
/// Returns `(pattern, direction, base_confidence)`.
fn classify_price_volume(klines: &[Kline]) -> (String, &'static str, i64) {
    if klines.len() < 10 {
        return ("数据不足".to_string(), "中性", 50);
    }
    let closes: Vec<f64> = klines.iter().map(|k| k.close).collect();
    let n = closes.len();
    let g8 = if closes[n - 8] != 0.0 {
        (closes[n - 1] - closes[n - 8]) / closes[n - 8] * 100.0
    } else {
        0.0
    };
    let vols: Vec<f64> = klines.iter().map(|k| k.volume).collect();

    let price_dir = if g8 > 2.0 {
        "涨"
    } else if g8 < -2.0 {
        "跌"
    } else {
        "平"
    };

    // Volume direction: last-3d average vs the 5 days before that
    let vol_change = if vols.len() >= 8 {
        let ma3_vol: f64 = vols[vols.len() - 3..].iter().sum::<f64>() / 3.0;
        let ma5_prev: f64 = vols[vols.len() - 8..vols.len() - 3].iter().sum::<f64>() / 5.0;
        if ma5_prev != 0.0 {
            (ma3_vol - ma5_prev) / ma5_prev * 100.0
        } else {
            0.0
        }
    } else {
        0.0
    };
    let vol_dir = if vol_change > 30.0 {
        "增"
    } else if vol_change < -30.0 {
        "缩"
    } else {
        "平"
    };

    let pattern = format!("价{}量{}", price_dir, vol_dir);
    let direction = match price_dir {
        "涨" => "看涨",
        "跌" => "看跌",
        _ => "中性",
    };
    let base_conf = match pattern.as_str() {
        "价涨量增" => 80,
        "价涨量平" => 55,
        "价涨量缩" => 60,
        "价平量增" => 50,
        "价平量平" => 20,
        "价平量缩" => 35,
        "价跌量增" => 75,
        "价跌量平" => 50,
        "价跌量缩" => 60,
        _ => 50,
    };
    (pattern, direction, base_conf)
}

/// Fund-flow signal text + confidence adjustment.
///
/// Rules: last 3 days all positive → +15; all negative → −15; otherwise
/// compare the last day against half the 2-day average (only when that
/// average is >= 0), ±10.
fn analyze_fund_flow(flows: Option<&[FundFlow]>) -> (&'static str, i64) {
    let Some(flows) = flows else { return ("", 0) };
    if flows.is_empty() {
        return ("", 0);
    }
    let start = flows.len().saturating_sub(3);
    let main_nets: Vec<f64> = flows[start..].iter().map(|f| f.main_net).collect();
    if main_nets.is_empty() {
        return ("", 0);
    }

    if main_nets.iter().all(|&v| v > 0.0) {
        return ("连续3日主力净流入", 15);
    }
    if main_nets.iter().all(|&v| v < 0.0) {
        return ("连续3日主力净流出", -15);
    }

    // Legacy would raise IndexError with a single non-uniform flow; that path
    // is unreachable with real data (>=2 flows), so guard to the neutral case.
    if main_nets.len() < 2 {
        return ("主力资金温和", 0);
    }
    let last_net = main_nets[main_nets.len() - 1];
    let prev_avg = (main_nets[0] + main_nets[1]) / 2.0;
    if prev_avg >= 0.0 {
        let threshold = 0.5 * prev_avg;
        if last_net < -threshold {
            return ("今日主力大幅流出", -10);
        }
        if last_net > threshold {
            return ("今日主力大幅流入", 10);
        }
    }
    ("主力资金温和", 0)
}

/// 放量涨停: pct >= 9.5 and volume > 1.5× the prior 5-day average.
fn detect_limit_up_volume(klines: &[Kline]) -> Option<String> {
    if klines.len() < 6 {
        return None;
    }
    let latest = &klines[klines.len() - 1];
    let vols = &klines[klines.len() - 6..klines.len() - 1];
    let avg_vol = if vols.is_empty() {
        1.0
    } else {
        vols.iter().map(|k| k.volume).sum::<f64>() / vols.len() as f64
    };
    if latest.pct >= 9.5 && avg_vol != 0.0 && latest.volume > avg_vol * 1.5 {
        return Some(format!("放量涨停(pct={:.1}%)", latest.pct));
    }
    None
}

/// 量能突破: today's volume is the 20-day max and > 1.5× its average.
fn detect_volume_breakout(klines: &[Kline]) -> Option<&'static str> {
    if klines.len() < 20 {
        return None;
    }
    let latest = &klines[klines.len() - 1];
    // Python `klines[-21:-1]` clamps the start index to 0 for len == 20.
    let start = klines.len().saturating_sub(21);
    let vols = &klines[start..klines.len() - 1];
    let avg_vol = if vols.is_empty() {
        1.0
    } else {
        vols.iter().map(|k| k.volume).sum::<f64>() / vols.len() as f64
    };
    let max_vol = vols
        .iter()
        .map(|k| k.volume)
        .fold(f64::NEG_INFINITY, f64::max);
    if latest.volume > avg_vol * 1.5 && latest.volume > max_vol {
        return Some("量能突破，资金活跃");
    }
    None
}

/// Combined volume-price analysis.
pub fn analyze_volume_price(
    klines: &[Kline],
    quote: Option<&Quote>,
    flows: Option<&[FundFlow]>,
) -> VolumePriceResult {
    let (pattern, direction, base) = classify_price_volume(klines);

    // Volume ratio = realtime quote volume / prior 5-day average.
    let volume_ratio = if let Some(q) = quote {
        if klines.len() >= 6 {
            let prev = &klines[klines.len() - 6..klines.len() - 1];
            let avg5 = prev.iter().map(|k| k.volume).sum::<f64>() / prev.len().max(1) as f64;
            if avg5 != 0.0 {
                py_round(q.volume / avg5, 2)
            } else {
                1.0
            }
        } else {
            1.0
        }
    } else {
        1.0
    };

    let turnover = match quote {
        Some(q) if q.turnover != 0.0 => q.turnover,
        _ => klines.last().map(|k| k.turnover).unwrap_or(0.0),
    };
    let turnover = if turnover != 0.0 { turnover } else { 0.0 };

    let obv = calc_obv(klines);
    let obv_opts: Vec<Option<f64>> = obv.iter().map(|&v| Some(v)).collect();
    // Legacy reverse-engineered: OBV direction uses lookback = 8.
    let obv_dir = ma_direction(&obv_opts, 8);
    let obv_trend = match obv_dir {
        "向上" => "上升",
        "向下" => "下降",
        _ => "走平",
    };

    let (fund_text, fund_delta) = analyze_fund_flow(flows);
    // Volume-ratio adjustment: <0.5 → −3; 0.5~1.5 → +2; 1.5~2.0 → +7; ≥2.0 → +12
    let vr_adj: i64 = if volume_ratio < 0.5 {
        -3
    } else if volume_ratio < 1.5 {
        2
    } else if volume_ratio < 2.0 {
        7
    } else {
        12
    };
    let confidence = (base + vr_adj + fund_delta).clamp(5, 95);

    let mut signals: Vec<String> = Vec::new();
    if !fund_text.is_empty() {
        signals.push(fund_text.to_string());
    }
    if obv_trend == "上升" {
        signals.push("OBV上升".to_string());
    } else if obv_trend == "下降" {
        signals.push("OBV下降".to_string());
    }

    if let Some(limit_up) = detect_limit_up_volume(klines) {
        signals.push(limit_up);
    }
    if let Some(vol_breakout) = detect_volume_breakout(klines) {
        if !signals.join(" ").contains("量能突破") {
            signals.push(vol_breakout.to_string());
        }
    }

    let mut desc = format!(
        "量价模式={}，量比={}，换手={:.1}%",
        pattern,
        crate::util::py_f64(volume_ratio),
        turnover
    );
    if !fund_text.is_empty() {
        desc.push_str(&format!("，{}", fund_text));
    }

    VolumePriceResult {
        pattern,
        direction: direction.to_string(),
        confidence,
        volume_ratio,
        turnover: py_round(turnover, 2),
        obv_trend: obv_trend.to_string(),
        signals,
        description: desc,
    }
}
