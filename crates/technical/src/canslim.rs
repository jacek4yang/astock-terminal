//! CANSLIM module, ported 1:1 from `analysis/canslim_module.py`.
//!
//! Seven sub-scores: C recent momentum / A mid-term trend / N new-high
//! pattern / S supply-demand / L leadership / I institutional flow / M market
//! environment, plus cup-with-handle detection.

use crate::indicators::{pct_change, sma_series};
use crate::types::{FundFlow, Kline, Quote};
use crate::util::{py_int, py_round};
use serde::{Deserialize, Serialize};

/// Detected cup-with-handle shape (legacy dict, kept as a typed struct;
/// serialized with the legacy key order).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CupHandle {
    pub pattern: String,
    pub cup_high: f64,
    pub cup_low: f64,
    pub handle_high: f64,
    pub handle_low: f64,
    pub cup_depth: f64,
    pub handle_depth: f64,
    pub breakout: bool,
    pub buy_point: f64,
    pub target: f64,
}

/// Result of [`analyze_canslim`], mirroring the legacy `CanslimResult`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanslimResult {
    pub c_score: i64,
    pub a_score: i64,
    pub n_score: i64,
    pub s_score: i64,
    pub l_score: i64,
    pub i_score: i64,
    pub m_score: i64,
    pub total: i64,
    pub grade: String,
    pub signals: Vec<String>,
    pub cup_handle: Option<CupHandle>,
    pub description: String,
}

/// C — recent momentum: max of the 20-day and 5-day gain tiers.
fn calc_c_score(klines: &[Kline]) -> (i64, String) {
    if klines.len() < 10 {
        return (50, String::new());
    }
    let closes: Vec<f64> = klines.iter().map(|k| k.close).collect();
    let n = closes.len();
    let gain20 = if n >= 21 {
        pct_change(closes[n - 21], closes[n - 1])
    } else {
        0.0
    };
    let gain5 = pct_change(closes[n - 6], closes[n - 1]);
    let score20 = if gain20 > 20.0 {
        90
    } else if gain20 > 15.0 {
        80
    } else if gain20 > 10.0 {
        70
    } else if gain20 > 5.0 {
        60
    } else if gain20 > 0.0 {
        50
    } else if gain20 > -5.0 {
        35
    } else {
        20
    };
    let score5 = if gain5 > 5.0 {
        80
    } else if gain5 > 2.0 {
        70
    } else if gain5 > 0.0 {
        60
    } else if gain5 > -5.0 {
        50
    } else {
        35
    };
    let score = score20.max(score5);
    (score, format!("C(近期动量){score}分"))
}

/// A — mid-term trend: 120-day gain tiers.
fn calc_a_score(klines: &[Kline]) -> (i64, String) {
    if klines.len() < 125 {
        return (50, String::new());
    }
    let n = klines.len();
    let gain = pct_change(klines[n - 121].close, klines[n - 1].close);
    let score = if gain > 150.0 {
        90
    } else if gain > 30.0 {
        75
    } else if gain > 15.0 {
        70
    } else if gain > 12.0 {
        60
    } else if gain > 5.0 {
        50
    } else if gain > -15.0 {
        35
    } else {
        20
    };
    (score, format!("A(中期趋势){score}分"))
}

/// N — new-high pattern: distance to the 52-week high + cup-handle breakout.
fn calc_n_score(klines: &[Kline]) -> (i64, String, Option<CupHandle>) {
    if klines.len() < 60 {
        return (50, String::new(), None);
    }
    let high_250 = if klines.len() >= 100 {
        klines[klines.len().saturating_sub(250)..]
            .iter()
            .map(|k| k.high)
            .fold(f64::NEG_INFINITY, f64::max)
    } else {
        klines
            .iter()
            .map(|k| k.high)
            .fold(f64::NEG_INFINITY, f64::max)
    };
    let price = klines[klines.len() - 1].close;
    let dist = if high_250 != 0.0 {
        (high_250 - price) / high_250 * 100.0
    } else {
        100.0
    };
    let cup_handle = detect_cup_handle(klines);
    // New recent high: price broke the prior 120-day high (excluding today)
    let high_120_prev = if klines.len() >= 121 {
        klines[klines.len() - 121..klines.len() - 1]
            .iter()
            .map(|k| k.high)
            .fold(f64::NEG_INFINITY, f64::max)
    } else {
        0.0
    };
    let score = if price >= high_120_prev {
        100
    } else if dist < 3.0 {
        70
    } else if cup_handle.as_ref().is_some_and(|ch| ch.breakout) {
        if dist < 8.0 {
            90
        } else if dist < 12.0 {
            85
        } else {
            75
        }
    } else {
        40
    };
    (score, format!("N(新高/形态){score}分"), cup_handle)
}

/// S — supply/demand: base 40, shrink −2, big volume +15, volume +5.
fn calc_s_score(klines: &[Kline], quote: Option<&Quote>) -> (i64, String) {
    // Legacy computes a turnover here but never uses it; omitted.
    let _ = quote;
    let vol_ratio = if klines.len() >= 6 {
        let prev_avg: f64 = klines[klines.len() - 6..klines.len() - 1]
            .iter()
            .map(|k| k.volume)
            .sum::<f64>()
            / 5.0;
        klines[klines.len() - 1].volume / prev_avg
    } else {
        1.0
    };
    let mut score: i64 = 40;
    if vol_ratio < 0.5 {
        score -= 2; // shrink
    } else if vol_ratio >= 2.0 {
        score += 15; // significant volume
    } else if vol_ratio >= 1.5 {
        score += 5; // volume
    }
    (score, format!("S(供需关系){score}分"))
}

/// L — leadership strength: 60-day gain base tier + 250-day gain adjustment.
fn calc_l_score(klines: &[Kline]) -> (i64, String) {
    if klines.len() < 95 {
        return (50, String::new());
    }
    let closes: Vec<f64> = klines.iter().map(|k| k.close).collect();
    let n = closes.len();
    let gain60 = if n >= 61 {
        pct_change(closes[n - 61], closes[n - 1])
    } else {
        0.0
    };
    let gain250 = if n >= 251 {
        pct_change(closes[n - 251], closes[n - 1])
    } else {
        pct_change(closes[0], closes[n - 1])
    };
    let base: i64 = if gain60 >= 30.0 {
        70
    } else if gain60 >= 15.0 {
        60
    } else if gain60 >= 5.0 {
        50
    } else if gain60 >= 1.0 {
        43
    } else if gain60 >= -9.0 {
        30
    } else {
        20
    };
    let adj: i64 = if gain250 > 2.5 {
        18
    } else if gain250 > 0.0 {
        13
    } else if gain250 < -30.0 {
        -5
    } else {
        0
    };
    let score = (base + adj).clamp(0, 100);
    (score, format!("L(相对强度){score}分"))
}

/// I — institutional flow (fund flow as a proxy for institutional holdings).
fn calc_i_score(flows: Option<&[FundFlow]>) -> (i64, String) {
    let Some(flows) = flows else {
        return (50, String::new());
    };
    let main_nets: Vec<f64> = flows.iter().map(|f| f.main_net).collect();
    if main_nets.is_empty() {
        return (50, String::new());
    }
    let mut streak = 0;
    for &v in main_nets[main_nets.len().saturating_sub(3)..].iter().rev() {
        if v > 0.0 {
            streak += 1;
        } else {
            break;
        }
    }
    let sum5: f64 = main_nets[main_nets.len().saturating_sub(5)..].iter().sum();
    let last = main_nets[main_nets.len() - 1];
    let score: i64 = if streak >= 3 {
        85
    } else if sum5 < 0.0 {
        10
    } else if last < -5e8 {
        45
    } else if sum5 > 0.0 {
        75
    } else {
        55
    };
    (score, format!("I(机构资金){score}分"))
}

/// M — market environment: index MA20/MA60 + up-day count; falls back to the
/// stock's own MAs when no index data is available.
fn calc_m_score(index_klines: Option<&[Kline]>, stock_klines: &[Kline]) -> (i64, String) {
    let use_index = index_klines.is_some_and(|ik| ik.len() >= 30);
    let src: &[Kline] = if use_index {
        index_klines.expect("checked")
    } else {
        stock_klines
    };
    let src_name = if use_index {
        "大盘指数"
    } else {
        "个股均线(近似)"
    };
    if src.len() < 30 {
        return (50, String::new());
    }
    let closes: Vec<f64> = src.iter().map(|k| k.close).collect();
    let ma20 = sma_series(&closes, 20);
    let ma60 = sma_series(&closes, 60);
    let (Some(ma20_val), Some(ma60_val)) = (ma20[ma20.len() - 1], ma60[ma60.len() - 1]) else {
        return (50, String::new());
    };

    let n = closes.len();
    let up_days20 = (n - 20..n).filter(|&i| closes[i] > closes[i - 1]).count();
    // MA20 direction is computed on the MA series tail (legacy quirk: the
    // `ma20_rising`/`ma20_txt` values are computed but unused downstream).
    let _ma20_rising = ma20.len() >= 2
        && matches!((ma20[ma20.len() - 1], ma20[ma20.len() - 6]), (Some(a), Some(b)) if a >= b);

    let score: i64 = if ma20_val > ma60_val && up_days20 >= 13 {
        80
    } else if ma20_val > ma60_val && up_days20 >= 7 {
        70
    } else if ma20_val > ma60_val {
        60
    } else if src_name == "大盘指数" {
        15
    } else {
        35
    };
    (score, format!("M(市场环境){score}分"))
}

/// Grade tiers: A+/A/B+/B/C+/C/D at 85/70/60/50/40/30.
fn calc_grade(score: i64) -> &'static str {
    if score >= 85 {
        "A+"
    } else if score >= 70 {
        "A"
    } else if score >= 60 {
        "B+"
    } else if score >= 50 {
        "B"
    } else if score >= 40 {
        "C+"
    } else if score >= 30 {
        "C"
    } else {
        "D"
    }
}

/// Cup-with-handle detection over a 120-bar window.
///
/// Cup low must not be in the first/last 20 bars of the window; cup_high =
/// max high in the 20 bars before the cup bottom; depth 5–35%; handle ≤ 30%;
/// buy_point = handle_high; target = buy_point + (cup_high − cup_low);
/// breakout = any high in the last 30 bars reached buy_point.
fn detect_cup_handle(klines: &[Kline]) -> Option<CupHandle> {
    if klines.len() < 80 {
        return None;
    }
    let window = &klines[klines.len().saturating_sub(120)..];
    let lows_v: Vec<f64> = window.iter().map(|k| k.low).collect();
    let cup_low = lows_v.iter().copied().fold(f64::INFINITY, f64::min);
    let cup_low_idx = lows_v.iter().position(|&v| v == cup_low)?;
    if cup_low_idx < 20 || cup_low_idx > window.len() - 20 {
        return None;
    }

    let left = &window[..cup_low_idx];
    if left.len() < 10 {
        return None;
    }
    // Cup rim high = highest high within the 20 bars before the cup bottom
    let left_recent = &left[left.len().saturating_sub(20)..];
    let cup_high = left_recent
        .iter()
        .map(|k| k.high)
        .fold(f64::NEG_INFINITY, f64::max);
    if cup_high <= cup_low {
        return None;
    }

    let right = &window[cup_low_idx..];
    // Python max() keeps the FIRST maximum on ties; track it manually.
    let mut handle_high_idx = 0usize;
    for (i, k) in right.iter().enumerate() {
        if k.high > right[handle_high_idx].high {
            handle_high_idx = i;
        }
    }
    let handle_high = right[handle_high_idx].high;
    if handle_high <= cup_low {
        return None;
    }

    // Handle low = lowest low over the full series from `len-20+2` onward
    // (legacy reverse-engineered quirk: uses the FULL kline array, not the
    // 120-bar window, and starts two bars after `ws = len - 20`).
    let ws = klines.len() - 20;
    let all_lows: Vec<f64> = klines.iter().map(|k| k.low).collect();
    if ws + 2 >= all_lows.len() {
        return None;
    }
    let handle_low = all_lows[ws + 2..]
        .iter()
        .copied()
        .fold(f64::INFINITY, f64::min);
    if handle_low <= 0.0 {
        return None;
    }

    let cup_depth = (cup_high - cup_low) / cup_high * 100.0;
    let handle_depth = (handle_high - handle_low) / handle_high * 100.0;
    if !(5.0..=35.0).contains(&cup_depth) || handle_depth > 30.0 {
        return None;
    }

    let buy_point = handle_high;
    let target = buy_point + (cup_high - cup_low);
    // Breakout: any high in the last 30 bars reached the handle high
    let recent_start = klines.len().saturating_sub(30);
    let breakout = klines[recent_start..]
        .iter()
        .map(|k| k.high)
        .fold(f64::NEG_INFINITY, f64::max)
        >= buy_point;

    Some(CupHandle {
        pattern: "杯柄形态".to_string(),
        cup_high: py_round(cup_high, 2),
        cup_low: py_round(cup_low, 2),
        handle_high: py_round(handle_high, 2),
        handle_low: py_round(handle_low, 2),
        cup_depth: py_round(cup_depth, 1),
        handle_depth: py_round(handle_depth, 1),
        breakout,
        buy_point: py_round(buy_point, 2),
        target: py_round(target, 2),
    })
}

/// Combined CANSLIM analysis.
pub fn analyze_canslim(
    klines: &[Kline],
    quote: Option<&Quote>,
    flows: Option<&[FundFlow]>,
    index_klines: Option<&[Kline]>,
) -> CanslimResult {
    let (c_score, c_text) = calc_c_score(klines);
    let (a_score, a_text) = calc_a_score(klines);
    let (n_score, n_text, cup_handle) = calc_n_score(klines);
    let (s_score, s_text) = calc_s_score(klines, quote);
    let (l_score, l_text) = calc_l_score(klines);
    let (i_score, i_text) = calc_i_score(flows);
    let (m_score, m_text) = calc_m_score(index_klines, klines);

    // Weighted total (truncated): C15% A10% N25% S5% L20% I15% M10%
    let total = py_int(
        0.15 * c_score as f64
            + 0.10 * a_score as f64
            + 0.25 * n_score as f64
            + 0.05 * s_score as f64
            + 0.20 * l_score as f64
            + 0.15 * i_score as f64
            + 0.10 * m_score as f64,
    );
    let grade = calc_grade(total);

    let mut signals: Vec<String> = Vec::new();
    if c_score >= 65 {
        signals.push(c_text);
    }
    if a_score >= 65 {
        signals.push(a_text);
    }
    if n_score >= 65 {
        signals.push(n_text);
    }
    // L signal threshold 70 (reverse-engineered)
    if l_score >= 70 {
        signals.push(l_text);
    }
    if i_score >= 65 {
        signals.push(i_text);
    }
    if m_score >= 70 {
        signals.push(m_text);
    }
    if m_score < 40 {
        signals.push("⚠️ 市场环境偏空，谨慎操作".to_string());
    }
    if i_score < 30 {
        signals.push("⚠️ 机构资金流出，注意风险".to_string());
    }
    if l_score < 30 {
        signals.push("⚠️ 相对强度弱势，非领涨股".to_string());
    }
    let _ = s_text; // legacy computes the S text but never emits it as a signal

    let description = format!(
        "综合{total}分({grade}) | C={c_score} A={a_score} N={n_score} S={s_score} L={l_score} I={i_score} M={m_score}"
    );

    CanslimResult {
        c_score,
        a_score,
        n_score,
        s_score,
        l_score,
        i_score,
        m_score,
        total,
        grade: grade.to_string(),
        signals,
        cup_handle,
        description,
    }
}
