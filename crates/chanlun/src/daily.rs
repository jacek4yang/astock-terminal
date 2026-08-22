//! Daily/weekly Chan theory pipeline (port of `analysis/chanlun_daily.py`).
//!
//! Pipeline: kline containment merge → fractals → strokes → zhongshus →
//! SMA-seeded MACD → divergence → buy/sell signals → ECharts overlay.
//!
//! Every function reproduces the legacy Python semantics exactly, including
//! floating-point operation order and rounding conventions, so that outputs
//! match the golden fixtures.

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::pyround::{py_round, py_round_int};

/// A kline after containment merging.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MergedDailyKline {
    /// Date of the first raw kline in this segment.
    pub date_start: String,
    /// Date of the last raw kline in this segment.
    pub date_end: String,
    /// Merged high.
    pub high: f64,
    /// Merged low.
    pub low: f64,
    /// Merge direction: 1 = up, -1 = down, 0 = undetermined (first segment).
    pub direction: i8,
    /// Number of raw klines absorbed into this segment.
    pub raw_count: usize,
}

/// A top or bottom fractal on the merged kline series.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DailyFractal {
    /// Index into the merged kline series.
    pub index: usize,
    /// `"top"` or `"bottom"`.
    #[serde(rename = "type")]
    pub fractal_type: String,
    /// Fractal price (merged high for tops, merged low for bottoms).
    pub price: f64,
    /// Date (end date of the merged kline).
    pub date: String,
}

/// A stroke (笔) connecting two fractals of opposite type.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DailyStroke {
    /// `"up"` or `"down"`.
    pub direction: String,
    /// Price at the stroke start fractal.
    pub start_price: f64,
    /// Price at the stroke end fractal.
    pub end_price: f64,
    /// Date at the stroke start fractal.
    pub start_date: String,
    /// Date at the stroke end fractal.
    pub end_date: String,
    /// Position of the start fractal within the fractal list.
    pub start_idx: usize,
    /// Position of the end fractal within the fractal list.
    pub end_idx: usize,
    /// Sum of absolute MACD bars over the stroke's date range.
    pub macd_area: f64,
    /// Whether this stroke shows MACD divergence vs the previous
    /// same-direction stroke.
    pub has_divergence: bool,
}

/// A zhongshu (中枢, overlapping consolidation zone of adjacent strokes).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Zhongshu {
    /// Start date of the first stroke forming the zhongshu.
    pub start_date: String,
    /// End date: the second stroke's end, or the breaking stroke's start
    /// once broken.
    pub end_date: String,
    /// Upper bound.
    pub zg: f64,
    /// Lower bound.
    pub zd: f64,
    /// Midpoint `(zg + zd) / 2`.
    pub zz: f64,
    /// Index of the first stroke forming the zhongshu.
    pub stroke_start_idx: usize,
    /// Index of the second stroke forming the zhongshu.
    pub stroke_end_idx: usize,
    /// Whether a later stroke broke out of the zone.
    pub is_broken: bool,
    /// `"up"` / `"down"` when broken, empty string otherwise.
    pub break_direction: String,
}

/// A buy/sell signal (买卖点).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChanlunDailySignal {
    /// `buy1`/`buy2`/`buy3`/`sell1`/`sell2`/`sell3`.
    #[serde(rename = "type")]
    pub signal_type: String,
    /// Signal price.
    pub price: f64,
    /// Signal date.
    pub date: String,
    /// Human-readable description (Chinese, matches legacy wording).
    pub description: String,
    /// Confidence score.
    pub confidence: i64,
}

/// Full result of the daily Chan theory analysis.
#[derive(Debug, Clone, Serialize)]
pub struct ChanlunDailyResult {
    /// Number of input klines.
    pub kline_count: usize,
    /// Number of merged klines.
    pub merged_count: usize,
    /// Number of fractals.
    pub fractal_count: usize,
    /// Number of strokes.
    pub stroke_count: usize,
    /// Number of zhongshus.
    pub zhongshu_count: usize,
    /// Detected fractals.
    pub fractals: Vec<DailyFractal>,
    /// Constructed strokes (with MACD area and divergence flags).
    pub strokes: Vec<DailyStroke>,
    /// Constructed zhongshus.
    pub zhongshus: Vec<Zhongshu>,
    /// Generated signals, ordered `[type1, buy2, sell2, sell3, buy3]`.
    pub signals: Vec<ChanlunDailySignal>,
    /// MACD DIF series.
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
    /// ECharts signal markers payload.
    pub chart_signals: Vec<Value>,
    /// ECharts fractal markers payload.
    pub chart_fractals: Vec<Value>,
    /// ECharts zhongshu rectangles payload.
    pub chart_zhongshus: Vec<Value>,
    /// ECharts stroke segments payload.
    pub chart_strokes: Vec<Value>,
}

/// MACD(12, 26, 9) with SMA12/SMA26 as the EMA seeds (legacy convention).
///
/// EMA12 is held constant at SMA12 before index 12, EMA26 at SMA26 before
/// index 26, and DEA at SMA9 of the first 9 DIF values before index 9, so
/// the first 12 DIF values are exactly `SMA12 - SMA26`. The recursion uses
/// `ema[i] = ema[i-1] + (x[i] - ema[i-1]) * k` in the same operation order
/// as the legacy code. If the input is shorter than a window, the seed is
/// the sum of the available prefix divided by the full window length (the
/// exact legacy behaviour).
pub fn calc_daily_macd(closes: &[f64]) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let n = closes.len();
    if n == 0 {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let s12: f64 = closes.iter().take(12).sum::<f64>() / 12.0;
    let s26: f64 = closes.iter().take(26).sum::<f64>() / 26.0;
    let mut ema12 = vec![s12; n];
    let mut ema26 = vec![s26; n];
    // EMA12 recursion starts at index 12 (held at SMA12 before that).
    for i in 12..n {
        ema12[i] = ema12[i - 1] + (closes[i] - ema12[i - 1]) * (2.0 / 13.0);
    }
    // EMA26 recursion starts at index 26 (held at SMA26 before that).
    for i in 26..n {
        ema26[i] = ema26[i - 1] + (closes[i] - ema26[i - 1]) * (2.0 / 27.0);
    }
    let dif: Vec<f64> = (0..n).map(|i| ema12[i] - ema26[i]).collect();
    // DEA = EMA9 of DIF seeded with SMA9 of the first 9 DIF values.
    let s_dea: f64 = dif.iter().take(9).sum::<f64>() / 9.0;
    let mut dea = vec![s_dea; n];
    for i in 9..n {
        dea[i] = dea[i - 1] + (dif[i] - dea[i - 1]) * (2.0 / 10.0);
    }
    let bar: Vec<f64> = (0..n).map(|i| (dif[i] - dea[i]) * 2.0).collect();
    (dif, dea, bar)
}

/// Merge raw klines by containment: a kline contained in (or containing) the
/// current segment is absorbed, extending extremes along the current
/// direction; otherwise the segment is closed and a new one starts with
/// direction decided by the high comparison.
pub fn merge_daily_klines(dates: &[String], highs: &[f64], lows: &[f64]) -> Vec<MergedDailyKline> {
    if dates.is_empty() {
        return Vec::new();
    }
    let mut merged = Vec::new();
    let mut cur = MergedDailyKline {
        date_start: dates[0].clone(),
        date_end: dates[0].clone(),
        high: highs[0],
        low: lows[0],
        direction: 0,
        raw_count: 1,
    };
    for i in 1..dates.len() {
        let (h, l) = (highs[i], lows[i]);
        // Containment: new kline fully inside the segment or fully wrapping it.
        let contained = (h <= cur.high && l >= cur.low) || (h >= cur.high && l <= cur.low);
        if !contained {
            merged.push(cur.clone());
            let direction = if h > cur.high { 1 } else { -1 };
            cur = MergedDailyKline {
                date_start: dates[i].clone(),
                date_end: dates[i].clone(),
                high: h,
                low: l,
                direction,
                raw_count: 1,
            };
        } else {
            // Contained: extend extremes along the current direction.
            let (new_high, new_low) = match cur.direction {
                1 => (h.max(cur.high), l.max(cur.low)),
                -1 => (h.min(cur.high), l.min(cur.low)),
                _ => (h.max(cur.high), l.min(cur.low)),
            };
            cur = MergedDailyKline {
                date_start: cur.date_start,
                date_end: dates[i].clone(),
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

/// Find top/bottom fractals on the merged kline series. `index` refers to the
/// merged kline index; boundary klines are never fractals. A kline that is
/// both a top and a bottom is classified as a top (legacy `if/elif`).
pub fn find_daily_fractals(merged: &[MergedDailyKline]) -> Vec<DailyFractal> {
    let mut fractals = Vec::new();
    let n = merged.len();
    for i in 1..n.saturating_sub(1) {
        let (left, cur, right) = (&merged[i - 1], &merged[i], &merged[i + 1]);
        // Top fractal: middle high greater than both neighbours.
        if cur.high > left.high && cur.high > right.high {
            fractals.push(DailyFractal {
                index: i,
                fractal_type: "top".to_string(),
                price: cur.high,
                date: cur.date_end.clone(),
            });
        // Bottom fractal: middle low less than both neighbours.
        } else if cur.low < left.low && cur.low < right.low {
            fractals.push(DailyFractal {
                index: i,
                fractal_type: "bottom".to_string(),
                price: cur.low,
                date: cur.date_end.clone(),
            });
        }
    }
    fractals
}

/// Build strokes from the fractal sequence.
///
/// Rules (reverse-engineered from the legacy engine):
/// - The start is the first fractal; a top starts a down stroke, a bottom an
///   up stroke.
/// - A same-type fractal with no pending endpoint moves the start when more
///   extreme (higher top for down strokes / lower bottom for up strokes),
///   and is absorbed otherwise.
/// - An opposite-type fractal becomes the endpoint once its merged-kline
///   index gap from the start is >= 4.
/// - After an endpoint is set, the next same-type fractal closes the stroke;
///   the endpoint becomes the next stroke's start with reversed direction.
/// - `start_idx`/`end_idx` are positions in the fractal list.
pub fn find_daily_strokes(fractals: &[DailyFractal]) -> Vec<DailyStroke> {
    let mut strokes = Vec::new();
    let n = fractals.len();
    if n == 0 {
        return strokes;
    }
    let mut start = fractals[0].clone();
    let mut start_pos = 0usize;
    let mut direction = if start.fractal_type == "top" { "down" } else { "up" };
    let mut end: Option<DailyFractal> = None;
    let mut end_pos = 0usize;
    let mut j = 1;
    while j < n {
        let f = &fractals[j];
        if f.fractal_type == start.fractal_type {
            if let Some(e) = end.take() {
                strokes.push(DailyStroke {
                    direction: direction.to_string(),
                    start_price: start.price,
                    end_price: e.price,
                    start_date: start.date.clone(),
                    end_date: e.date.clone(),
                    start_idx: start_pos,
                    end_idx: end_pos,
                    macd_area: 0.0,
                    has_divergence: false,
                });
                start = e;
                start_pos = end_pos;
                direction = if direction == "down" { "up" } else { "down" };
            } else {
                // No pending endpoint: move the start if more extreme.
                if (direction == "down" && f.price > start.price)
                    || (direction == "up" && f.price < start.price)
                {
                    start = f.clone();
                    start_pos = j;
                }
                j += 1;
            }
        } else {
            // Opposite type: endpoint requires a merged-index gap >= 4.
            if f.index as i64 - start.index as i64 >= 4 {
                end = Some(f.clone());
                end_pos = j;
            }
            j += 1;
        }
    }
    if let Some(e) = end {
        strokes.push(DailyStroke {
            direction: direction.to_string(),
            start_price: start.price,
            end_price: e.price,
            start_date: start.date.clone(),
            end_date: e.date.clone(),
            start_idx: start_pos,
            end_idx: end_pos,
            macd_area: 0.0,
            has_divergence: false,
        });
    }
    strokes
}

/// Build zhongshus from pairwise stroke overlaps: for strokes `i` and `i+1`
/// the overlap is `[max(lows), min(highs)]`; on overlap advance by 2, else
/// by 1. Then detect breaks by later strokes; a broken zhongshu's `end_date`
/// becomes the breaking stroke's start date.
pub fn find_zhongshus(strokes: &[DailyStroke]) -> Vec<Zhongshu> {
    let mut zhongshus: Vec<Zhongshu> = Vec::new();
    let n = strokes.len();
    let mut i = 0;
    while i + 2 < n {
        let (a, b) = (&strokes[i], &strokes[i + 1]);
        // The four endpoints form two intervals; the overlap is the zhongshu.
        let zd = a.start_price.min(a.end_price).max(b.start_price.min(b.end_price));
        let zg = a.start_price.max(a.end_price).min(b.start_price.max(b.end_price));
        if zd < zg {
            zhongshus.push(Zhongshu {
                start_date: a.start_date.clone(),
                end_date: b.end_date.clone(),
                zg,
                zd,
                zz: (zg + zd) / 2.0,
                stroke_start_idx: i,
                stroke_end_idx: i + 1,
                is_broken: false,
                break_direction: String::new(),
            });
            i += 2;
        } else {
            i += 1;
        }
    }
    // Break detection: first later up stroke closing above zg (or down stroke
    // below zd) breaks the zhongshu; end_date moves to its start date.
    for z in &mut zhongshus {
        for s in &strokes[z.stroke_end_idx + 1..] {
            if s.direction == "up" && s.end_price > z.zg {
                z.is_broken = true;
                z.break_direction = "up".to_string();
                z.end_date = s.start_date.clone();
                break;
            }
            if s.direction == "down" && s.end_price < z.zd {
                z.is_broken = true;
                z.break_direction = "down".to_string();
                z.end_date = s.start_date.clone();
                break;
            }
        }
    }
    zhongshus
}

/// Compute each stroke's MACD area (`sum |bar|` over the stroke's inclusive
/// date range) and flag divergence: area smaller than the previous
/// same-direction stroke's area while making a new price extreme.
pub fn detect_daily_divergence(strokes: &mut [DailyStroke], macd_bar: &[f64], dates: &[String]) {
    // Last occurrence wins, matching the legacy dict comprehension.
    let date_to_idx: HashMap<&str, usize> = dates
        .iter()
        .enumerate()
        .map(|(i, d)| (d.as_str(), i))
        .collect();
    let mut areas = Vec::with_capacity(strokes.len());
    for st in strokes.iter() {
        match (date_to_idx.get(st.start_date.as_str()), date_to_idx.get(st.end_date.as_str())) {
            (Some(&si), Some(&ei)) => {
                areas.push(macd_bar[si..=ei].iter().map(|x| x.abs()).sum());
            }
            _ => areas.push(0.0),
        }
    }
    let mut flags = vec![false; strokes.len()];
    for i in 0..strokes.len() {
        // Previous stroke in the same direction (nearest one wins).
        let prev = match (0..i).rev().find(|&j| strokes[j].direction == strokes[i].direction) {
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

/// Divergence confidence: `clamp(round(94.5 - 35 * area_ratio), 55, 92)`.
fn divergence_confidence(strokes: &[DailyStroke], idx: usize, direction: &str) -> i64 {
    let st = &strokes[idx];
    let prev = (0..idx).rev().find(|&j| strokes[j].direction == direction);
    match prev {
        Some(p) if strokes[p].macd_area > 0.0 => {
            let ratio = st.macd_area / strokes[p].macd_area;
            let raw = 94.5 - 35.0 * ratio;
            py_round_int(raw).clamp(55, 92)
        }
        _ => 55,
    }
}

/// (symbol, rotate, position, color) per signal type for the chart overlay.
fn signal_style(sig_type: &str) -> (&'static str, i64, &'static str, &'static str) {
    match sig_type {
        "buy2" => ("triangle", 0, "bottom", "#D85A30"),
        "buy3" => ("triangle", 0, "bottom", "#BA7517"),
        "sell1" => ("pin", 180, "top", "#639922"),
        "sell2" => ("pin", 180, "top", "#3B6D11"),
        "sell3" => ("pin", 180, "top", "#27500A"),
        // "buy1" and any unknown type fall back to the buy1 style.
        _ => ("triangle", 0, "bottom", "#E24B4A"),
    }
}

/// Chinese display name of a signal type (unknown types pass through).
pub fn get_signal_type_name(sig_type: &str) -> &str {
    match sig_type {
        "buy1" => "一类买点",
        "buy2" => "二类买点",
        "buy3" => "三类买点",
        "sell1" => "一类卖点",
        "sell2" => "二类卖点",
        "sell3" => "三类卖点",
        other => other,
    }
}

/// Generate buy/sell signals.
///
/// Order: `[type-1 by date] + [buy2 by date] + [sell2 by date] + [sell3 by
/// date] + [buy3 by date]`. Type-1 signals fire at divergent stroke ends;
/// type-2 at the stroke two positions after a type-1 that does not break the
/// prior extreme (confidence 70); type-3 at the retracement stroke
/// (`zhongshu end + 2`) staying outside a broken zhongshu (confidence 75).
pub fn generate_daily_signals(
    strokes: &[DailyStroke],
    zhongshus: &[Zhongshu],
) -> Vec<ChanlunDailySignal> {
    let mut type1: Vec<ChanlunDailySignal> = Vec::new();
    let mut buy2_list: Vec<ChanlunDailySignal> = Vec::new();
    let mut sell2_list: Vec<ChanlunDailySignal> = Vec::new();
    let mut buy3_list: Vec<ChanlunDailySignal> = Vec::new();
    let mut sell3_list: Vec<ChanlunDailySignal> = Vec::new();

    // ---- Type-1: endpoints of divergent strokes ----
    for (i, st) in strokes.iter().enumerate() {
        if !st.has_divergence {
            continue;
        }
        if st.direction == "down" {
            type1.push(ChanlunDailySignal {
                signal_type: "buy1".to_string(),
                price: st.end_price,
                date: st.end_date.clone(),
                confidence: divergence_confidence(strokes, i, "down"),
                description: format!(
                    "一类买点：日线底背驰，MACD面积{:.1}较前笔衰减，空头力度衰竭",
                    st.macd_area
                ),
            });
        } else {
            type1.push(ChanlunDailySignal {
                signal_type: "sell1".to_string(),
                price: st.end_price,
                date: st.end_date.clone(),
                confidence: divergence_confidence(strokes, i, "up"),
                description: format!(
                    "一类卖点：日线顶背驰，MACD面积{:.1}较前笔衰减，多头力度衰竭",
                    st.macd_area
                ),
            });
        }
    }
    type1.sort_by(|a, b| a.date.cmp(&b.date));

    // ---- Type-2: pullback after a type-1 that holds the prior extreme ----
    for (i, st) in strokes.iter().enumerate() {
        if st.has_divergence {
            let idx2 = i + 2;
            if idx2 < strokes.len() {
                let nxt = &strokes[idx2];
                if st.direction == "down" && nxt.direction == "down" && nxt.end_price > st.end_price
                {
                    buy2_list.push(ChanlunDailySignal {
                        signal_type: "buy2".to_string(),
                        price: nxt.end_price,
                        date: nxt.end_date.clone(),
                        confidence: 70,
                        description: format!(
                            "二类买点：一类买点后反弹再回落，未破前低{:.2}",
                            st.end_price
                        ),
                    });
                } else if st.direction == "up"
                    && nxt.direction == "up"
                    && nxt.end_price < st.end_price
                {
                    sell2_list.push(ChanlunDailySignal {
                        signal_type: "sell2".to_string(),
                        price: nxt.end_price,
                        date: nxt.end_date.clone(),
                        confidence: 70,
                        description: format!(
                            "二类卖点：一类卖点后回落再反弹，未破前高{:.2}",
                            st.end_price
                        ),
                    });
                }
            }
        }
    }
    buy2_list.sort_by(|a, b| a.date.cmp(&b.date));
    sell2_list.sort_by(|a, b| a.date.cmp(&b.date));

    // ---- Type-3: retracement stroke (zhongshu end + 2) after a break ----
    for z in zhongshus {
        if !z.is_broken {
            continue;
        }
        let recess_idx = z.stroke_end_idx + 2;
        if recess_idx >= strokes.len() {
            continue;
        }
        let retro = &strokes[recess_idx];
        if z.break_direction == "up" {
            // Upward break -> buy3 on the down retracement staying above zg.
            if retro.direction == "down" && retro.end_price > z.zg {
                buy3_list.push(ChanlunDailySignal {
                    signal_type: "buy3".to_string(),
                    price: retro.end_price,
                    date: retro.end_date.clone(),
                    confidence: 75,
                    description: format!(
                        "三类买点：中枢[{:.2}-{:.2}]向上突破后回踩，未回到中枢内",
                        z.zd, z.zg
                    ),
                });
            }
        } else {
            // Downward break -> sell3 on the up rebound staying below zd.
            if retro.direction == "up" && retro.end_price < z.zd {
                sell3_list.push(ChanlunDailySignal {
                    signal_type: "sell3".to_string(),
                    price: retro.end_price,
                    date: retro.end_date.clone(),
                    confidence: 75,
                    description: format!(
                        "三类卖点：中枢[{:.2}-{:.2}]向下突破后反弹，未回到中枢内",
                        z.zd, z.zg
                    ),
                });
            }
        }
    }
    sell3_list.sort_by(|a, b| a.date.cmp(&b.date));
    buy3_list.sort_by(|a, b| a.date.cmp(&b.date));

    let mut out = type1;
    out.extend(buy2_list);
    out.extend(sell2_list);
    out.extend(sell3_list);
    out.extend(buy3_list);
    out
}

/// Build `current_state` / `summary` / `description` (legacy wording).
fn describe_state(
    strokes: &[DailyStroke],
    zhongshus: &[Zhongshu],
    signals: &[ChanlunDailySignal],
    fractal_count: usize,
) -> (String, String, String) {
    let last_stroke = strokes.last();
    let is_up = last_stroke.is_some_and(|s| s.direction == "up");
    let direction_cn = if is_up { "向上" } else { "向下" };
    let bull_cn = if is_up { "多头" } else { "空头" };

    let zs_text = match zhongshus.last() {
        Some(z) => {
            if z.is_broken {
                if z.break_direction == "up" {
                    "已向上突破".to_string()
                } else {
                    "已向下突破".to_string()
                }
            } else {
                format!("[{:.2}-{:.2}]震荡中", z.zd, z.zg)
            }
        }
        None => "无中枢".to_string(),
    };

    let latest = signals.last();
    let mut state = format!("处于{}笔中，{}延续，最近中枢{}", direction_cn, bull_cn, zs_text);
    let summary;
    if let Some(latest) = latest {
        let type_cn = get_signal_type_name(&latest.signal_type);
        state += &format!("，最新信号：{}@{:.2}", type_cn, latest.price);
        summary = format!("最新信号：{}@{:.2}({})", type_cn, latest.price, latest.date);
    } else {
        state += "，无信号";
        summary = "无信号".to_string();
    }

    let description = format!(
        "共{}个分型、{}笔、{}个中枢、{}个信号。{}",
        fractal_count,
        strokes.len(),
        zhongshus.len(),
        signals.len(),
        state
    );
    (state, summary, description)
}

/// Build the ECharts overlay payloads (signals/fractals/zhongshus/strokes).
/// Fractal coordinates carry the raw (unrounded) price.
fn build_chart_overlay(
    fractals: &[DailyFractal],
    strokes: &[DailyStroke],
    zhongshus: &[Zhongshu],
    signals: &[ChanlunDailySignal],
) -> (Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>) {
    let chart_fractals: Vec<Value> = fractals
        .iter()
        .map(|f| {
            json!({
                "coord": [f.date, f.price],
                "symbol": "circle",
                "symbolSize": 7,
                "itemStyle": {
                    "color": "transparent",
                    "borderColor": if f.fractal_type == "top" { "#A32D2D" } else { "#3B6D11" },
                    "borderWidth": 1.5,
                },
                "fractal_type": f.fractal_type,
            })
        })
        .collect();

    let chart_strokes: Vec<Value> = strokes
        .iter()
        .map(|s| {
            json!({
                "coords": [[s.start_date, s.start_price], [s.end_date, s.end_price]],
                "lineStyle": {
                    "color": if s.direction == "down" { "#639922" } else { "#E24B4A" },
                    "width": 1.5,
                    "type": if s.has_divergence { "dashed" } else { "solid" },
                },
                "has_divergence": s.has_divergence,
            })
        })
        .collect();

    let chart_zhongshus: Vec<Value> = zhongshus
        .iter()
        .map(|z| {
            json!({
                "xAxis": [z.start_date, z.end_date],
                "yAxis": [z.zd, z.zg],
                "itemStyle": {
                    "color": "rgba(83, 74, 183, 0.08)",
                    "borderColor": "rgba(83, 74, 183, 0.4)",
                },
                "broken": z.is_broken,
                "break_direction": z.break_direction,
                "zg": z.zg,
                "zd": z.zd,
            })
        })
        .collect();

    let chart_signals: Vec<Value> = signals
        .iter()
        .map(|s| {
            let (symbol, rotate, position, color) = signal_style(&s.signal_type);
            let type_cn = get_signal_type_name(&s.signal_type);
            json!({
                "coord": [s.date, s.price],
                "symbol": symbol,
                "symbolRotate": rotate,
                "symbolSize": 14,
                "itemStyle": {"color": color, "opacity": 0.9},
                "label": {
                    "show": true,
                    "position": position,
                    "formatter": type_cn,
                    "fontSize": 10,
                    "color": color,
                },
                "type_name": type_cn,
                "date": s.date,
                "price": py_round(s.price, 2),
                "confidence": s.confidence,
                "description": s.description,
            })
        })
        .collect();

    (chart_signals, chart_fractals, chart_zhongshus, chart_strokes)
}

/// Run the full daily Chan theory analysis pipeline.
///
/// `opens` and `volumes` are accepted for interface parity with the legacy
/// function; the algorithm itself uses only dates, highs, lows and closes.
pub fn analyze_chanlun_daily(
    dates: &[String],
    _opens: &[f64],
    closes: &[f64],
    highs: &[f64],
    lows: &[f64],
    _volumes: &[f64],
) -> ChanlunDailyResult {
    let merged = merge_daily_klines(dates, highs, lows);
    let fractals = find_daily_fractals(&merged);
    let mut strokes = find_daily_strokes(&fractals);
    let zhongshus = find_zhongshus(&strokes);
    let (dif, dea, bar) = calc_daily_macd(closes);
    detect_daily_divergence(&mut strokes, &bar, dates);
    let signals = generate_daily_signals(&strokes, &zhongshus);
    let (state, summary, description) =
        describe_state(&strokes, &zhongshus, &signals, fractals.len());
    let (cs, cf, cz, cst) = build_chart_overlay(&fractals, &strokes, &zhongshus, &signals);
    ChanlunDailyResult {
        kline_count: dates.len(),
        merged_count: merged.len(),
        fractal_count: fractals.len(),
        stroke_count: strokes.len(),
        zhongshu_count: zhongshus.len(),
        fractals,
        strokes,
        zhongshus,
        signals,
        macd_dif: dif,
        macd_dea: dea,
        macd_bar: bar,
        current_state: state,
        summary,
        description,
        chart_signals: cs,
        chart_fractals: cf,
        chart_zhongshus: cz,
        chart_strokes: cst,
    }
}

/// Serialize the result to the exact JSON shape of the legacy
/// `daily_result_to_dict` (this is what the golden fixtures store).
pub fn daily_result_to_dict(result: &ChanlunDailyResult) -> Value {
    json!({
        "kline_count": result.kline_count,
        "merged_count": result.merged_count,
        "fractal_count": result.fractal_count,
        "stroke_count": result.stroke_count,
        "zhongshu_count": result.zhongshu_count,
        "fractals": result.fractals.iter().map(|f| json!({
            "type": f.fractal_type,
            "type_name": if f.fractal_type == "top" { "顶分型" } else { "底分型" },
            "price": py_round(f.price, 2),
            "date": f.date,
        })).collect::<Vec<_>>(),
        "strokes": result.strokes.iter().map(|s| json!({
            "direction": s.direction,
            "start_price": py_round(s.start_price, 2),
            "end_price": py_round(s.end_price, 2),
            "start_date": s.start_date,
            "end_date": s.end_date,
            "macd_area": py_round(s.macd_area, 2),
            "has_divergence": s.has_divergence,
        })).collect::<Vec<_>>(),
        "zhongshus": result.zhongshus.iter().map(|z| json!({
            "start_date": z.start_date,
            "end_date": z.end_date,
            "zg": py_round(z.zg, 2),
            "zd": py_round(z.zd, 2),
            "zz": py_round(z.zz, 2),
            "is_broken": z.is_broken,
            "break_direction": z.break_direction,
        })).collect::<Vec<_>>(),
        "signals": result.signals.iter().map(|s| json!({
            "type": s.signal_type,
            "type_name": get_signal_type_name(&s.signal_type),
            "price": py_round(s.price, 2),
            "date": s.date,
            "confidence": s.confidence,
            "description": s.description,
        })).collect::<Vec<_>>(),
        "current_state": result.current_state,
        "summary": result.summary,
        "description": result.description,
        "chart_signals": result.chart_signals,
        "chart_fractals": result.chart_fractals,
        "chart_zhongshus": result.chart_zhongshus,
        "chart_strokes": result.chart_strokes,
    })
}
