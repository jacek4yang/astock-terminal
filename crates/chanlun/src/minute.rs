//! Minute-level Chan theory pipeline (port of `analysis/chanlun_minute.py`).
//!
//! 1-minute bars are aggregated into 5-minute klines (groups reset at
//! session breaks such as the lunch gap), then the same
//! merge/fractal/stroke pipeline as the daily version runs, followed by
//! SMA-seeded MACD, divergence detection and type-1 buy/sell signals.

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::pyround::{py_round, py_round_int};

/// A 5-minute kline aggregated from 1-minute bars.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MinuteKline {
    /// Timestamp of the last minute in the group.
    pub time: String,
    /// Price of the first minute in the group.
    pub open: f64,
    /// Price of the last minute in the group.
    pub close: f64,
    /// Highest price in the group.
    pub high: f64,
    /// Lowest price in the group.
    pub low: f64,
    /// Summed volume of the group.
    pub volume: f64,
}

/// A 5-minute kline after containment merging.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MergedKline {
    /// Timestamp of the first raw kline in this segment.
    pub time_start: String,
    /// Timestamp of the last raw kline in this segment.
    pub time_end: String,
    /// Merged high.
    pub high: f64,
    /// Merged low.
    pub low: f64,
    /// Merge direction: 1 = up, -1 = down, 0 = undetermined.
    pub direction: i8,
    /// Number of raw klines absorbed into this segment.
    pub raw_count: usize,
}

/// A top or bottom fractal on the merged series.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fractal {
    /// Index into the merged kline series.
    pub index: usize,
    /// `"top"` or `"bottom"`.
    #[serde(rename = "type")]
    pub fractal_type: String,
    /// Fractal price.
    pub price: f64,
    /// Timestamp (end time of the merged kline).
    pub time: String,
}

/// A stroke (笔) connecting two fractals of opposite type.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stroke {
    /// `"up"` or `"down"`.
    pub direction: String,
    /// Price at the stroke start fractal.
    pub start_price: f64,
    /// Price at the stroke end fractal.
    pub end_price: f64,
    /// Timestamp at the stroke start fractal.
    pub start_time: String,
    /// Timestamp at the stroke end fractal.
    pub end_time: String,
    /// Position of the start fractal within the fractal list.
    pub start_idx: usize,
    /// Position of the end fractal within the fractal list.
    pub end_idx: usize,
    /// Sum of absolute MACD bars over the stroke's time range.
    pub macd_area: f64,
    /// Whether this stroke shows MACD divergence.
    pub has_divergence: bool,
}

/// A type-1 buy/sell signal.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChanlunSignal {
    /// `buy1` or `sell1`.
    #[serde(rename = "type")]
    pub signal_type: String,
    /// Signal price.
    pub price: f64,
    /// Signal timestamp.
    pub time: String,
    /// Human-readable description (Chinese, matches legacy wording).
    pub description: String,
    /// Confidence score.
    pub confidence: i64,
}

/// Full result of the minute-level Chan theory analysis.
#[derive(Debug, Clone, Serialize)]
pub struct ChanlunMinuteResult {
    /// Number of aggregated 5-minute klines.
    pub kline_count: usize,
    /// Number of fractals.
    pub fractal_count: usize,
    /// Detected fractals.
    pub fractals: Vec<Fractal>,
    /// Number of strokes.
    pub stroke_count: usize,
    /// Constructed strokes (with MACD area and divergence flags).
    pub strokes: Vec<Stroke>,
    /// Generated signals (type-1 only).
    pub signals: Vec<ChanlunSignal>,
    /// MACD DIF series on the 5-minute closes.
    pub macd_dif: Vec<f64>,
    /// MACD DEA series.
    pub macd_dea: Vec<f64>,
    /// MACD bar series (`2 * (DIF - DEA)`).
    pub macd_bar: Vec<f64>,
    /// One-line market state description.
    pub current_state: String,
    /// Latest-signal summary.
    pub summary: String,
    /// Aggregate counts plus state.
    pub description: String,
}

/// Parse `"HH:MM"` into minutes since midnight (legacy `_to_minutes`).
fn to_minutes(t: &str) -> i64 {
    let mut parts = t.split(':');
    let hh: i64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let mm: i64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    hh * 60 + mm
}

/// Aggregate a group of minute indices into one 5-minute kline.
fn make_kline(times: &[String], prices: &[f64], volumes: &[f64], group: &[usize]) -> MinuteKline {
    let first = group[0];
    let last = group[group.len() - 1];
    let mut high = f64::NEG_INFINITY;
    let mut low = f64::INFINITY;
    let mut volume = 0.0;
    for &i in group {
        high = high.max(prices[i]);
        low = low.min(prices[i]);
        volume += volumes[i];
    }
    MinuteKline {
        time: times[last].clone(),
        open: prices[first],
        close: prices[last],
        high,
        low,
        volume,
    }
}

/// Aggregate 1-minute bars into 5-minute klines.
///
/// Groups of 5 consecutive minutes are formed within each trading session;
/// any non-1-minute gap between adjacent timestamps (e.g. the lunch break)
/// closes the current group and restarts grouping from the new minute. Each
/// kline is timestamped by the last minute of its group. A trailing partial
/// group is flushed at the end of the input.
pub fn construct_5min_klines(
    times: &[String],
    prices: &[f64],
    volumes: &[f64],
) -> Vec<MinuteKline> {
    let mut klines = Vec::new();
    let n = times.len();
    if n == 0 {
        return klines;
    }
    let mut group: Vec<usize> = Vec::new();
    let mut group_start_minute = to_minutes(&times[0]);
    for i in 0..n {
        let cur_minute = to_minutes(&times[i]);
        // Session break: a non-1-minute gap from the previous minute.
        if !group.is_empty() && cur_minute - to_minutes(&times[i - 1]) != 1 {
            klines.push(make_kline(times, prices, volumes, &group));
            group.clear();
            group_start_minute = cur_minute;
        }
        if group.is_empty() {
            group_start_minute = cur_minute;
        }
        group.push(i);
        // Close the group after 5 minutes, or flush a trailing partial group.
        if cur_minute - group_start_minute == 4 || (i == n - 1 && !group.is_empty()) {
            klines.push(make_kline(times, prices, volumes, &group));
            group.clear();
        }
    }
    klines
}

/// EMA with an SMA seed: the first `n` values are held constant at SMA(n),
/// then the recursion `out[i] = seq[i] * k + out[i-1] * (1 - k)` applies
/// (same operation order as the legacy minute implementation).
fn ema_sma_seed(seq: &[f64], n: usize) -> Vec<f64> {
    if seq.is_empty() {
        return Vec::new();
    }
    let k = 2.0 / (n as f64 + 1.0);
    let seed = seq.iter().take(n).sum::<f64>() / n as f64;
    let mut out: Vec<f64> = vec![seed; n.min(seq.len())];
    for i in n..seq.len() {
        out.push(seq[i] * k + out[i - 1] * (1.0 - k));
    }
    out
}

/// MACD(12, 26, 9) with SMA seeds on the 5-minute closes.
pub fn calc_macd(klines: &[MinuteKline]) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let closes: Vec<f64> = klines.iter().map(|k| k.close).collect();
    let ema12 = ema_sma_seed(&closes, 12);
    let ema26 = ema_sma_seed(&closes, 26);
    let dif: Vec<f64> = ema12.iter().zip(&ema26).map(|(a, b)| a - b).collect();
    let dea = ema_sma_seed(&dif, 9);
    let bar: Vec<f64> = dif.iter().zip(&dea).map(|(d, s)| (d - s) * 2.0).collect();
    (dif, dea, bar)
}

/// Merge klines by containment, extending extremes along the current
/// direction (identical rule to the daily version).
pub fn merge_klines(klines: &[MinuteKline]) -> Vec<MergedKline> {
    if klines.is_empty() {
        return Vec::new();
    }
    let mut merged = Vec::new();
    let mut cur = MergedKline {
        time_start: klines[0].time.clone(),
        time_end: klines[0].time.clone(),
        high: klines[0].high,
        low: klines[0].low,
        direction: 0,
        raw_count: 1,
    };
    for k in klines.iter().skip(1) {
        let (h, l) = (k.high, k.low);
        let contained = (h <= cur.high && l >= cur.low) || (h >= cur.high && l <= cur.low);
        if !contained {
            merged.push(cur.clone());
            let direction = if h > cur.high { 1 } else { -1 };
            cur = MergedKline {
                time_start: k.time.clone(),
                time_end: k.time.clone(),
                high: h,
                low: l,
                direction,
                raw_count: 1,
            };
        } else {
            let (new_high, new_low) = match cur.direction {
                1 => (h.max(cur.high), l.max(cur.low)),
                -1 => (h.min(cur.high), l.min(cur.low)),
                _ => (h.max(cur.high), l.min(cur.low)),
            };
            cur = MergedKline {
                time_start: cur.time_start,
                time_end: k.time.clone(),
                high: new_high,
                low: new_low,
                direction: cur.direction,
                raw_count: cur.raw_count + 1,
            };
        }
    }
    merged.push(cur);
    merged
}

/// Find top/bottom fractals on the merged series; boundary klines are never
/// fractals and a doubly-qualified kline is a top (legacy `if/elif`).
pub fn find_fractals(merged: &[MergedKline]) -> Vec<Fractal> {
    let mut fractals = Vec::new();
    let n = merged.len();
    for i in 1..n.saturating_sub(1) {
        let (left, cur, right) = (&merged[i - 1], &merged[i], &merged[i + 1]);
        if cur.high > left.high && cur.high > right.high {
            fractals.push(Fractal {
                index: i,
                fractal_type: "top".to_string(),
                price: cur.high,
                time: cur.time_end.clone(),
            });
        } else if cur.low < left.low && cur.low < right.low {
            fractals.push(Fractal {
                index: i,
                fractal_type: "bottom".to_string(),
                price: cur.low,
                time: cur.time_end.clone(),
            });
        }
    }
    fractals
}

/// Build strokes from the fractal sequence (same rules as the daily
/// version: same-type extremes move the start, opposite type with a
/// merged-index gap >= 4 becomes the endpoint).
pub fn find_strokes(fractals: &[Fractal]) -> Vec<Stroke> {
    let mut strokes = Vec::new();
    let n = fractals.len();
    if n == 0 {
        return strokes;
    }
    let mut start = fractals[0].clone();
    let mut start_pos = 0usize;
    let mut direction = if start.fractal_type == "top" {
        "down"
    } else {
        "up"
    };
    let mut end: Option<Fractal> = None;
    let mut end_pos = 0usize;
    let mut j = 1;
    while j < n {
        let f = &fractals[j];
        if f.fractal_type == start.fractal_type {
            if let Some(e) = end.take() {
                strokes.push(Stroke {
                    direction: direction.to_string(),
                    start_price: start.price,
                    end_price: e.price,
                    start_time: start.time.clone(),
                    end_time: e.time.clone(),
                    start_idx: start_pos,
                    end_idx: end_pos,
                    macd_area: 0.0,
                    has_divergence: false,
                });
                start = e;
                start_pos = end_pos;
                direction = if direction == "down" { "up" } else { "down" };
            } else {
                if (direction == "down" && f.price > start.price)
                    || (direction == "up" && f.price < start.price)
                {
                    start = f.clone();
                    start_pos = j;
                }
                j += 1;
            }
        } else {
            if f.index as i64 - start.index as i64 >= 4 {
                end = Some(f.clone());
                end_pos = j;
            }
            j += 1;
        }
    }
    if let Some(e) = end {
        strokes.push(Stroke {
            direction: direction.to_string(),
            start_price: start.price,
            end_price: e.price,
            start_time: start.time.clone(),
            end_time: e.time.clone(),
            start_idx: start_pos,
            end_idx: end_pos,
            macd_area: 0.0,
            has_divergence: false,
        });
    }
    strokes
}

/// Compute each stroke's MACD area (`sum |bar|` over the inclusive time
/// range on 5-minute klines) and flag divergence vs the previous
/// same-direction stroke.
pub fn detect_divergence(strokes: &mut [Stroke], macd_bar: &[f64], klines: &[MinuteKline]) {
    // Last occurrence wins, matching the legacy dict comprehension.
    let time_to_idx: HashMap<&str, usize> = klines
        .iter()
        .enumerate()
        .map(|(i, k)| (k.time.as_str(), i))
        .collect();
    let mut areas = Vec::with_capacity(strokes.len());
    for st in strokes.iter() {
        match (
            time_to_idx.get(st.start_time.as_str()),
            time_to_idx.get(st.end_time.as_str()),
        ) {
            (Some(&si), Some(&ei)) => {
                areas.push(macd_bar[si..=ei].iter().map(|x| x.abs()).sum());
            }
            _ => areas.push(0.0),
        }
    }
    let mut flags = vec![false; strokes.len()];
    for i in 0..strokes.len() {
        let prev = match (0..i)
            .rev()
            .find(|&j| strokes[j].direction == strokes[i].direction)
        {
            Some(p) => p,
            None => continue,
        };
        let area_less = areas[i] < areas[prev];
        let new_extreme = if strokes[i].direction == "down" {
            strokes[i].end_price < strokes[prev].end_price
        } else {
            strokes[i].end_price > strokes[prev].end_price
        };
        flags[i] = area_less && new_extreme;
    }
    for (i, st) in strokes.iter_mut().enumerate() {
        st.macd_area = areas[i];
        st.has_divergence = flags[i];
    }
}

/// Divergence confidence: `clamp(round(100 - 40 * area_ratio), 55, 90)`.
fn signal_confidence(area: f64, prev_area: f64) -> i64 {
    if prev_area <= 0.0 {
        return 55;
    }
    let ratio = area / prev_area;
    let raw = 100.0 - 40.0 * ratio;
    py_round_int(raw).clamp(55, 90)
}

/// Chinese display name of a signal type (unknown types pass through).
pub fn get_signal_type_name(sig_type: &str) -> &str {
    match sig_type {
        "buy1" => "一类买点",
        "sell1" => "一类卖点",
        other => other,
    }
}

/// Generate type-1 signals from divergent strokes (buy1 for down strokes,
/// sell1 for up strokes), in stroke order.
pub fn generate_signals(strokes: &[Stroke]) -> Vec<ChanlunSignal> {
    let mut signals = Vec::new();
    for (i, st) in strokes.iter().enumerate() {
        if !st.has_divergence {
            continue;
        }
        let prev = match (0..i).rev().find(|&j| strokes[j].direction == st.direction) {
            Some(p) => p,
            None => continue,
        };
        let conf = signal_confidence(st.macd_area, strokes[prev].macd_area);
        let (sig_type, desc) = if st.direction == "up" {
            (
                "sell1",
                format!(
                    "一类卖点：顶背驰，MACD面积{:.2}较前笔衰减，多头力度衰竭",
                    st.macd_area
                ),
            )
        } else {
            (
                "buy1",
                format!(
                    "一类买点：底背驰，MACD面积{:.2}较前笔衰减，空头力度衰竭",
                    st.macd_area
                ),
            )
        };
        signals.push(ChanlunSignal {
            signal_type: sig_type.to_string(),
            price: st.end_price,
            time: st.end_time.clone(),
            description: desc,
            confidence: conf,
        });
    }
    signals
}

/// Build `current_state` / `summary` / `description` (legacy wording).
fn describe_state(
    strokes: &[Stroke],
    signals: &[ChanlunSignal],
    fractal_count: usize,
) -> (String, String, String) {
    if strokes.is_empty() {
        return (
            "笔形成中".to_string(),
            "暂无买卖信号".to_string(),
            format!("共{fractal_count}个分型、0笔。笔形成中"),
        );
    }

    let last_stroke = &strokes[strokes.len() - 1];
    let is_up = last_stroke.direction == "up";
    let direction_cn = if is_up { "向上" } else { "向下" };
    let bull_cn = if is_up { "多头" } else { "空头" };
    let latest = signals.last();

    let mut state;
    let summary;
    // Divergence on the last stroke -> type-1 risk warning.
    if last_stroke.has_divergence {
        if last_stroke.direction == "up" {
            state = "向上笔顶背驰，注意一类卖点风险".to_string();
        } else {
            state = "向下笔底背驰，注意一类买点风险".to_string();
        }
        if let Some(latest) = latest {
            state += &format!("，最近{}信号在{}", latest.signal_type, latest.time);
        }
        summary = match latest {
            Some(l) => format!(
                "最新信号：{}@{:.2}",
                get_signal_type_name(&l.signal_type),
                l.price
            ),
            None => "暂无买卖信号".to_string(),
        };
    } else {
        state = format!("处于{direction_cn}笔中，{bull_cn}延续");
        if let Some(latest) = latest {
            state += &format!("，最近{}信号在{}", latest.signal_type, latest.time);
            summary = format!(
                "最新信号：{}@{:.2}",
                get_signal_type_name(&latest.signal_type),
                latest.price
            );
        } else {
            summary = "暂无买卖信号".to_string();
        }
    }

    let description = if signals.is_empty() {
        format!("共{}个分型、{}笔。{}", fractal_count, strokes.len(), state)
    } else {
        format!(
            "共{}个分型、{}笔、{}个信号。{}",
            fractal_count,
            strokes.len(),
            signals.len(),
            state
        )
    };
    (state, summary, description)
}

/// Run the full minute-level Chan theory analysis pipeline.
pub fn analyze_chanlun_minute(
    times: &[String],
    prices: &[f64],
    volumes: &[f64],
) -> ChanlunMinuteResult {
    let klines = construct_5min_klines(times, prices, volumes);
    let merged = merge_klines(&klines);
    let fractals = find_fractals(&merged);
    let mut strokes = find_strokes(&fractals);
    let (dif, dea, bar) = calc_macd(&klines);
    detect_divergence(&mut strokes, &bar, &klines);
    let signals = generate_signals(&strokes);
    let (state, summary, description) = describe_state(&strokes, &signals, fractals.len());
    ChanlunMinuteResult {
        kline_count: klines.len(),
        fractal_count: fractals.len(),
        fractals,
        stroke_count: strokes.len(),
        strokes,
        signals,
        macd_dif: dif,
        macd_dea: dea,
        macd_bar: bar,
        current_state: state,
        summary,
        description,
    }
}

/// Serialize the result to the exact JSON shape of the legacy
/// `signals_to_dict` (`macd_bar` rounded to 6 decimals).
pub fn signals_to_dict(result: &ChanlunMinuteResult) -> Value {
    json!({
        "kline_count": result.kline_count,
        "fractal_count": result.fractal_count,
        "stroke_count": result.stroke_count,
        "current_state": result.current_state,
        "summary": result.summary,
        "description": result.description,
        "signals": result.signals.iter().map(|s| json!({
            "type": s.signal_type,
            "type_name": get_signal_type_name(&s.signal_type),
            "price": py_round(s.price, 2),
            "time": s.time,
            "description": s.description,
            "confidence": s.confidence,
        })).collect::<Vec<_>>(),
        "fractals": result.fractals.iter().map(|f| json!({
            "type": f.fractal_type,
            "type_name": if f.fractal_type == "top" { "顶分型" } else { "底分型" },
            "price": py_round(f.price, 2),
            "time": f.time,
        })).collect::<Vec<_>>(),
        "strokes": result.strokes.iter().map(|s| json!({
            "direction": s.direction,
            "start_price": py_round(s.start_price, 2),
            "end_price": py_round(s.end_price, 2),
            "start_time": s.start_time,
            "end_time": s.end_time,
            "macd_area": py_round(s.macd_area, 4),
            "has_divergence": s.has_divergence,
        })).collect::<Vec<_>>(),
        "macd_bar": result.macd_bar.iter().map(|x| py_round(*x, 6)).collect::<Vec<_>>(),
    })
}
