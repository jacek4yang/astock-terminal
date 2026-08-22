//! Breakout module (Turtle trading rules), ported 1:1 from
//! `analysis/breakout_module.py`. System 1: 20-day Donchian; System 2: 55-day.

use crate::types::Kline;
use crate::util::py_round;
use serde::{Deserialize, Serialize};

/// Result of one Turtle system, mirroring the legacy `BreakoutResult`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreakoutResult {
    pub system: String,
    pub signal: String,
    pub breakout_price: f64,
    pub current_n: f64,
    pub stop_loss: f64,
    pub entry_price: Option<f64>,
    pub position_units: i64,
    pub exit_price: Option<f64>,
    pub channel_high: f64,
    pub channel_low: f64,
    pub next_add_price: Option<f64>,
    pub signals: Vec<String>,
    pub description: String,
}

/// True range: `max(H−L, |H−PDC|, |L−PDC|)`.
pub fn calc_true_range(high: f64, low: f64, pre_close: f64) -> f64 {
    (high - low)
        .max((high - pre_close).abs())
        .max((low - pre_close).abs())
}

/// N value: SMA of the last `period` true ranges, rounded to 4 decimals.
pub fn calc_n(klines: &[Kline], period: usize) -> f64 {
    if klines.len() < period + 1 {
        return 0.0;
    }
    let mut trs = Vec::with_capacity(period);
    for i in (klines.len() - period)..klines.len() {
        let k = &klines[i];
        let pre_close = klines[i - 1].close;
        trs.push(calc_true_range(k.high, k.low, pre_close));
    }
    py_round(trs.iter().sum::<f64>() / trs.len() as f64, 4)
}

/// Donchian channel `(highest high, lowest low)` over `period` bars,
/// EXCLUDING the current bar (`klines[-period-1:-1]` in legacy slicing).
pub fn calc_donchian_channel(klines: &[Kline], period: usize) -> (f64, f64) {
    let window: &[Kline] = if klines.len() <= period {
        if klines.len() > 1 {
            &klines[..klines.len() - 1]
        } else {
            klines
        }
    } else {
        &klines[klines.len() - period - 1..klines.len() - 1]
    };
    let high = window
        .iter()
        .map(|k| k.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let low = window.iter().map(|k| k.low).fold(f64::INFINITY, f64::min);
    (high, low)
}

/// Find the most recent breakout entry: `(direction, entry_price, bar_index)`.
/// Scans backwards; at bar `i` the reference window is `klines[i-period..i]`.
fn find_last_entry(klines: &[Kline], period: usize) -> Option<(&'static str, f64, usize)> {
    let n = klines.len();
    if n <= period {
        return None;
    }
    for i in (period..n).rev() {
        let window = &klines[i - period..i];
        if window.len() < period {
            continue;
        }
        let window_high = window
            .iter()
            .map(|k| k.high)
            .fold(f64::NEG_INFINITY, f64::max);
        let window_low = window.iter().map(|k| k.low).fold(f64::INFINITY, f64::min);
        let k = &klines[i];
        if k.high > window_high {
            return Some(("多", window_high, i));
        }
        if k.low < window_low {
            return Some(("空", window_low, i));
        }
    }
    None
}

/// Analyze a single Turtle system.
fn analyze_system(klines: &[Kline], period: usize, system_name: &str) -> BreakoutResult {
    let n_val = calc_n(klines, 20);
    let (channel_high, channel_low) = calc_donchian_channel(klines, period);
    let last_entry = find_last_entry(klines, period);

    if n_val <= 0.0 || channel_high <= 0.0 || last_entry.is_none() {
        return BreakoutResult {
            system: system_name.to_string(),
            signal: "无信号".to_string(),
            breakout_price: py_round(channel_high, 2),
            current_n: n_val,
            stop_loss: 0.0,
            entry_price: None,
            position_units: 0,
            exit_price: None,
            channel_high: py_round(channel_high, 2),
            channel_low: py_round(channel_low, 2),
            next_add_price: None,
            signals: Vec::new(),
            description: format!("{}无突破信号", system_name),
        };
    }

    let (direction, entry, entry_idx) = last_entry.expect("checked above");
    let holding_days = klines.len() - 1 - entry_idx;

    let signal: &str;
    let exit_price: Option<f64>;
    let units: i64;
    let stop: f64;
    let next_add: Option<f64>;
    let sig_text: String;

    if direction == "多" {
        // Highest high since entry determines the number of add-on units
        let high_since = if klines.len() > entry_idx + 1 {
            klines[entry_idx + 1..]
                .iter()
                .map(|k| k.high)
                .fold(f64::NEG_INFINITY, f64::max)
        } else {
            entry
        };
        let mut extra: i64 = 0;
        if high_since > entry + 0.5 * n_val {
            // Python `int((high_since - entry) // (0.5 * n_val))`: float floor
            extra = ((high_since - entry) / (0.5 * n_val)).floor() as i64;
        }
        units = 1 + extra;
        // After adds the stop moves up to the last add price minus 2N
        let last_add_price = entry + (units - 1) as f64 * 0.5 * n_val;
        stop = last_add_price - 2.0 * n_val;
        next_add = if units <= 4 && system_name != "系统二(55日)" {
            Some(entry + units as f64 * 0.5 * n_val)
        } else {
            None
        };
        // Long exit: only the last bar's close vs the stop
        if klines[klines.len() - 1].close <= stop {
            signal = "卖出";
            exit_price = Some(stop);
            sig_text = format!("触及2N止损{:.2}，卖出", stop);
        } else {
            signal = "持仓";
            exit_price = None;
            sig_text = format!("持有多头{}日，止损{:.2}", holding_days, stop);
        }
    } else {
        stop = entry + 2.0 * n_val;
        units = 1;
        next_add = None;
        // Short cover: only the last bar. System 1 uses the 10-day high,
        // system 2 the 20-day high; channel breakout beats the 2N stop.
        let exit_window = if system_name.contains("系统一") {
            10
        } else {
            20
        };
        let high_exit_level = calc_donchian_channel(klines, exit_window).0;
        let last = &klines[klines.len() - 1];
        if last.high >= high_exit_level {
            signal = "空头平仓";
            exit_price = Some(high_exit_level);
            sig_text = format!("突破{}日高点{:.2}，空头平仓", exit_window, high_exit_level);
        } else if last.close >= stop {
            signal = "空头平仓";
            exit_price = Some(stop);
            sig_text = format!("触及2N止损{:.2}，空头平仓", stop);
        } else {
            signal = "持仓";
            exit_price = None;
            sig_text = format!("持有空头{}日，止损{:.2}", holding_days, stop);
        }
    }

    let short_system_name = system_name.replace("(20日)", "").replace("(55日)", "");
    BreakoutResult {
        system: system_name.to_string(),
        signal: signal.to_string(),
        breakout_price: py_round(channel_high, 2),
        current_n: n_val,
        stop_loss: py_round(stop, 2),
        entry_price: Some(py_round(entry, 2)),
        position_units: units,
        exit_price: exit_price.filter(|&e| e != 0.0).map(|e| py_round(e, 2)),
        channel_high: py_round(channel_high, 2),
        channel_low: py_round(channel_low, 2),
        next_add_price: next_add.filter(|&p| p != 0.0).map(|p| py_round(p, 2)),
        signals: vec![sig_text],
        description: format!(
            "{}入场={}@{:.2}，N={:.4}，持有{}日",
            short_system_name, direction, entry, n_val, holding_days
        ),
    }
}

/// System 1 (20-day channel).
pub fn analyze_breakout_system1(klines: &[Kline]) -> BreakoutResult {
    analyze_system(klines, 20, "系统一(20日)")
}

/// System 2 (55-day channel).
pub fn analyze_breakout_system2(klines: &[Kline]) -> BreakoutResult {
    analyze_system(klines, 55, "系统二(55日)")
}

/// Combined breakout analysis: both systems, in order.
pub fn analyze_breakout(klines: &[Kline]) -> Vec<BreakoutResult> {
    vec![
        analyze_breakout_system1(klines),
        analyze_breakout_system2(klines),
    ]
}
