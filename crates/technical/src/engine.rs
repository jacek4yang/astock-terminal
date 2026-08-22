//! Signal engine: five-module aggregation and decision, ported 1:1 from
//! `analysis/signal_engine.py` (`run_analysis`) plus the `signal_to_dict`
//! serialization from `app.py`.

use crate::breakout::{analyze_breakout, BreakoutResult};
use crate::canslim::{analyze_canslim, CanslimResult};
use crate::pattern::{analyze_patterns, PatternResult};
use crate::trend::{analyze_trend, TrendResult};
use crate::types::{FundFlow, Kline, Quote};
use crate::util::{py_f64, py_int, py_round};
use crate::volume_price::{analyze_volume_price, VolumePriceResult};
use serde_json::{json, Map, Value};

// Risk-level / signal-strength tier thresholds (reverse-engineered constants;
// do not change).
const RISK_HEAVY: i64 = 5; // risk_points >= 5 → 高
const RISK_MEDIUM: i64 = 3; // risk_points >= 3 → 中
const STRONG_SCORE: i64 = 75; // score >= 75 → 强 / 正常仓位
const MEDIUM_SCORE: i64 = 60; // score >= 60 → 中

/// Full structured result of [`run_analysis`], mirroring the legacy
/// `SignalEngineResult` dataclass.
#[derive(Debug, Clone)]
pub struct SignalEngineResult {
    pub action: String,
    pub score: i64,
    pub confidence: i64,
    pub risk_level: String,
    pub signal_strength: String,
    pub trend: TrendResult,
    pub patterns: Vec<PatternResult>,
    pub volume_price: VolumePriceResult,
    pub breakouts: Vec<BreakoutResult>,
    pub canslim: CanslimResult,
    /// Module scores in legacy key order: 趋势/形态/量价/突破/CAN_SLIM.
    pub module_scores: Vec<(String, i64)>,
    pub buy_signals: Vec<String>,
    pub sell_signals: Vec<String>,
    pub risk_warnings: Vec<String>,
    pub key_levels: Vec<(String, f64)>,
    pub description: String,
    pub plain_summary: String,
    pub trade_plan: TradePlan,
}

/// Trade plan, mirroring the legacy dict.
#[derive(Debug, Clone)]
pub struct TradePlan {
    pub action: String,
    pub entry_price: f64,
    pub stop_loss: f64,
    pub target_price: f64,
    pub position_size: String,
    pub holding_period: String,
    pub risk_reward_ratio: f64,
    pub max_loss_pct: f64,
    pub notes: String,
}

/// Pattern module score: clamp(50 + Σ sign×conf×0.2, 20, 100).
fn pattern_to_score(patterns: &[PatternResult]) -> i64 {
    let mut total = 50.0;
    for p in patterns {
        let sign: f64 = match p.direction.as_str() {
            "看涨" => 1.0,
            "看跌" => -1.0,
            _ => 0.0,
        };
        total += sign * p.confidence as f64 * 0.2;
    }
    py_int(total).clamp(20, 100)
}

/// Volume-price module score: 看涨 → confidence; 看跌 → max(20, 100−conf);
/// 中性 → 50.
fn volume_price_to_score(vp: &VolumePriceResult) -> i64 {
    match vp.direction.as_str() {
        "看涨" => vp.confidence,
        "看跌" => (100 - vp.confidence).max(20),
        _ => 50,
    }
}

/// Breakout module score: base 50; 60 when any system is 持仓/空头平仓;
/// +3 extra when a short cover exists.
fn breakout_to_score(breakouts: &[BreakoutResult]) -> i64 {
    let mut score = 50;
    let mut has_signal = false;
    let mut has_short_cover = false;
    for b in breakouts {
        if matches!(b.signal.as_str(), "持仓" | "持仓空头" | "多头止损") {
            has_signal = true;
        }
        if b.signal == "空头平仓" {
            has_short_cover = true;
        }
    }
    if has_signal || has_short_cover {
        score = 60;
    }
    if has_short_cover {
        score += 3;
    }
    score.min(100)
}

/// Risk level + signal strength from bearish-signal accumulation.
fn calc_risk_level(
    score: i64,
    trend: &TrendResult,
    vp: &VolumePriceResult,
    canslim: &CanslimResult,
    breakouts: &[BreakoutResult],
) -> (String, String) {
    let mut risk_points = 0;
    if trend.direction == "下降" {
        risk_points += 2;
    }
    if vp.direction == "看跌" {
        risk_points += 2;
    }
    if canslim.m_score < 30 {
        risk_points += 1;
    }
    // Legacy scans the generated signal texts for "止损"; both the stop-loss
    // and the holding texts contain it, so this fires whenever any system has
    // an active entry. Scanning our own generated texts is equivalent.
    if breakouts
        .iter()
        .flat_map(|b| b.signals.iter())
        .any(|s| s.contains("止损"))
    {
        risk_points += 1;
    }

    let risk_level = if risk_points >= RISK_HEAVY {
        "高"
    } else if risk_points >= RISK_MEDIUM {
        "中"
    } else {
        "低"
    };
    let strength = if score >= STRONG_SCORE {
        "强"
    } else if score >= MEDIUM_SCORE {
        "中"
    } else {
        "弱"
    };
    (risk_level.to_string(), strength.to_string())
}

/// Build the trade plan (stop = entry×0.95; target priority 头肩底 > 双底 >
/// 箱体, else box upper edge, else entry×1.10; position 正常仓位/半仓/空仓).
fn build_trade_plan(
    action: &str,
    score: i64,
    patterns: &[PatternResult],
    breakouts: &[BreakoutResult],
    klines: &[Kline],
) -> TradePlan {
    let entry = klines.last().map(|k| k.close).unwrap_or(0.0);
    let stop = py_round(entry * 0.95, 2); // fixed 5% stop, rounded BEFORE the RR

    // Target: first bullish-pattern target above entry, by priority
    let priority = |name: &str| match name {
        "头肩底" => 0,
        "双底" => 1,
        "箱体" => 2,
        _ => 9,
    };
    let mut ordered: Vec<&PatternResult> = patterns
        .iter()
        .filter(|p| p.direction == "看涨" && p.target_price.is_some())
        .collect();
    ordered.sort_by_key(|p| priority(&p.name));
    let mut target: Option<f64> = ordered
        .iter()
        .filter_map(|p| p.target_price)
        .find(|&t| t > entry);
    if target.is_none() {
        for p in patterns {
            if let Some((_, v)) = p.key_levels.iter().find(|(k, _)| k == "箱体上沿") {
                target = Some(*v);
                break;
            }
        }
    }
    let target = target.unwrap_or(entry * 1.10);

    let risk_amt = entry - stop;
    let reward_amt = target - entry;
    let risk_reward = if risk_amt > 0.0 {
        py_round(reward_amt / risk_amt, 1)
    } else {
        0.0
    };
    let max_loss_pct = 5.0; // fixed

    let position_size = if action == "观望" {
        "空仓等待"
    } else if score >= STRONG_SCORE {
        "正常仓位"
    } else {
        "半仓(1/2)"
    };

    let holding_period = "中线(1-3月)";

    // Notes: only the first 持仓 system; uses the raw (rounded) entry_price
    let mut notes: Vec<String> = Vec::new();
    for b in breakouts {
        // Legacy: `if b.signal == "持仓" and b.entry_price:` then break.
        if b.signal == "持仓" && b.entry_price.is_some_and(|e| e != 0.0) {
            let entry_price = b.entry_price.expect("checked above");
            let mut note = format!(
                "{}：持仓中(突破价{})，止损{:.2}",
                b.system,
                py_f64(entry_price),
                b.stop_loss
            );
            if let Some(next_add) = b.next_add_price {
                note.push_str(&format!("；加仓价{next_add:.2}"));
            }
            notes.push(note);
            break;
        }
    }

    TradePlan {
        action: action.to_string(),
        entry_price: py_round(entry, 2),
        stop_loss: py_round(stop, 2),
        target_price: py_round(target, 2),
        position_size: position_size.to_string(),
        holding_period: holding_period.to_string(),
        risk_reward_ratio: risk_reward,
        max_loss_pct,
        notes: notes.join("；"),
    }
}

/// Build the plain-language summary.
fn build_plain_summary(
    action: &str,
    trend: &TrendResult,
    patterns: &[PatternResult],
    vp: &VolumePriceResult,
    canslim: &CanslimResult,
    plan: &TradePlan,
) -> String {
    let has_head_shoulder = patterns.iter().any(|p| p.name == "头肩底");
    let has_flow_out = vp.signals.iter().any(|s| s.contains("流出"));
    // 量价配合良好: bullish and confidence >= 70
    let volume_price_ok = vp.direction == "看涨" && vp.confidence >= 70;

    if action == "观望" {
        let mut desc_parts = vec![format!("处于{}趋势", trend.direction)];
        if has_flow_out {
            desc_parts.push("主力资金流出".to_string());
        }
        if canslim.m_score < 30 {
            desc_parts.push("大盘环境偏空".to_string());
        }
        return format!(
            "建议观望，{}。建议耐心等待信号明确后再操作。",
            desc_parts.join("，")
        );
    }

    let trend_desc = if trend.strength >= 70 {
        "强势上升趋势"
    } else {
        "上升趋势"
    };
    let mut desc_parts = vec![format!("处于{}", trend_desc)];
    if canslim.m_score < 30 {
        desc_parts.push("⚠️大盘偏空".to_string());
    }
    if has_head_shoulder {
        desc_parts.push("头肩底形态确认".to_string());
    }
    if volume_price_ok {
        desc_parts.push("量价配合良好".to_string());
    }

    format!(
        "出现买入信号，{}。建议{}入场，买入价{:.2}，止损{:.2}，目标{:.2}（盈亏比{}）。",
        desc_parts.join("，"),
        plan.position_size,
        plan.entry_price,
        plan.stop_loss,
        plan.target_price,
        py_f64(plan.risk_reward_ratio)
    )
}

/// Five-module combined analysis entry point.
///
/// Unlike the legacy `run_analysis`, this is pure: when `index_klines` is
/// `None` the M score falls back to the stock's own MAs (the legacy network
/// fetch of the index is the caller's responsibility).
pub fn run_analysis(
    klines: &[Kline],
    quote: Option<&Quote>,
    flows: Option<&[FundFlow]>,
    index_klines: Option<&[Kline]>,
) -> SignalEngineResult {
    let trend = analyze_trend(klines);
    let patterns = analyze_patterns(klines);
    let vp = analyze_volume_price(klines, quote, flows);
    let breakouts = analyze_breakout(klines);
    let canslim = analyze_canslim(klines, quote, flows, index_klines);

    let trend_score = trend.strength;
    let pattern_score = pattern_to_score(&patterns);
    let vp_score = volume_price_to_score(&vp);
    let breakout_score = breakout_to_score(&breakouts);
    let canslim_score = canslim.total;

    let module_scores: Vec<(String, i64)> = vec![
        ("趋势".to_string(), trend_score),
        ("形态".to_string(), pattern_score),
        ("量价".to_string(), vp_score),
        ("突破".to_string(), breakout_score),
        ("CAN_SLIM".to_string(), canslim_score),
    ];
    // Composite = int(趋势25% + CAN20% + 突破20% + 量价20% + 形态15%)
    let score = py_int(
        trend_score as f64 * 0.25
            + canslim_score as f64 * 0.20
            + breakout_score as f64 * 0.20
            + vp_score as f64 * 0.20
            + pattern_score as f64 * 0.15,
    );

    // Actions: >= 75 强烈买入, >= 60 买入, else 观望 (legacy has no 谨慎买入
    // tier at this stage; the optimizer may introduce it).
    let action = if score >= 75 {
        "强烈买入"
    } else if score >= 60 {
        "买入"
    } else {
        "观望"
    };

    // confidence = max(10, int(score*0.8) + 12*n - 40), n = #modules >= 60
    let qualified_count = module_scores.iter().filter(|(_, s)| *s >= 60).count() as i64;
    let confidence = (py_int(score as f64 * 0.8) + 12 * qualified_count - 40).max(10);

    let (risk_level, signal_strength) = calc_risk_level(score, &trend, &vp, &canslim, &breakouts);

    // ---- Signal aggregation ----
    let mut buy_signals: Vec<String> = Vec::new();
    let mut sell_signals: Vec<String> = Vec::new();

    if trend.strength >= 65 {
        buy_signals.push(format!("趋势强势上升({}分)", trend.strength));
    } else if trend.strength >= 45 {
        buy_signals.push(format!("趋势上升({}分)", trend.strength));
    }
    for sig in &trend.signals {
        if !buy_signals.contains(sig) && !sig.starts_with("MA20") {
            buy_signals.push(sig.clone());
        }
    }

    // Only 头肩底 patterns are appended (legacy reverse-engineered behaviour)
    for p in &patterns {
        if p.name == "头肩底" && p.direction == "看涨" {
            buy_signals.push(format!("{}({})", p.name, p.status));
        }
    }

    // Volume-price signal enters only when bullish and confidence >= 60
    if vp.direction == "看涨" && vp.confidence >= 60 {
        buy_signals.push(format!("量价{}({}分)", vp.pattern, vp.confidence));
    }
    // vp.signals: only 净流入/流出-style entries are forwarded
    for sig in &vp.signals {
        if sig.contains("流出") {
            sell_signals.push(sig.clone());
        } else if sig.contains("净流入") {
            buy_signals.push(sig.clone());
        }
    }

    for b in &breakouts {
        if b.signal == "持仓" && b.entry_price.is_some_and(|e| e != 0.0) {
            buy_signals.push(format!(
                "{}持仓(N={:.2}，止损{:.2})",
                b.system, b.current_n, b.stop_loss
            ));
        } else if b.signal == "空头平仓" {
            // Uses breakout_price (not exit_price) — legacy quirk
            buy_signals.push(format!(
                "{}空头平仓@{}(偏多)",
                b.system,
                py_f64(b.breakout_price)
            ));
        } else if b.signal == "多头止损" {
            sell_signals.push(format!("{}多头止损@{:.2}", b.system, b.stop_loss));
        }
    }

    for sig in &canslim.signals {
        if sig.contains("⚠️") {
            sell_signals.push(sig.clone());
        } else if !sig.starts_with("M(") {
            buy_signals.push(sig.clone());
        }
    }

    // ---- Risk warnings ----
    let mut risk_warnings: Vec<String> = Vec::new();
    if canslim.m_score < 30 {
        risk_warnings.push("市场环境偏空".to_string());
    }
    if vp.direction == "看跌" {
        risk_warnings.push("量价配合不佳".to_string());
    }
    if trend.direction == "下降" {
        risk_warnings.push("处于下降趋势".to_string());
    }

    // ---- Key levels: only the highest-priority pattern's levels ----
    // Priority: 头肩(底/顶) > 双底 > 箱体
    let mut key_levels: Vec<(String, f64)> = Vec::new();
    let mut primary_pattern: Option<&PatternResult> = None;
    for name_prefix in ["头肩", "双底", "箱体"] {
        for p in &patterns {
            if p.name.starts_with(name_prefix) {
                primary_pattern = Some(p);
                break;
            }
        }
        if primary_pattern.is_some() {
            break;
        }
    }
    if let Some(p) = primary_pattern {
        for (label, val) in &p.key_levels {
            key_levels.push((format!("{}_{}", p.name, label), py_round(*val, 2)));
        }
    }
    for b in &breakouts {
        if b.stop_loss > 0.0 {
            key_levels.push((format!("{}_止损", b.system), b.stop_loss));
        }
    }
    if let Some(ch) = &canslim.cup_handle {
        key_levels.push(("杯柄买点".to_string(), ch.buy_point));
    }
    if let Some(tl) = &trend.trendline {
        key_levels.push(("趋势线".to_string(), tl.current_price));
    }

    let trade_plan = build_trade_plan(action, score, &patterns, &breakouts, klines);

    let mut desc_parts = vec![format!("综合{}分", score)];
    if !trend.direction.is_empty() {
        desc_parts.push(format!("趋势={}({})", trend.direction, trend_score));
    }
    desc_parts.push(format!("量价={}({})", vp.pattern, vp_score));
    desc_parts.push(format!("突破={breakout_score}"));
    desc_parts.push(format!("CS={}({})", canslim.grade, canslim_score));
    if !patterns.is_empty() {
        desc_parts.push(format!("形态={pattern_score}"));
    }
    let description = desc_parts.join(" | ");

    let plain_summary = build_plain_summary(action, &trend, &patterns, &vp, &canslim, &trade_plan);

    SignalEngineResult {
        action: action.to_string(),
        score,
        confidence,
        risk_level,
        signal_strength,
        trend,
        patterns,
        volume_price: vp,
        breakouts,
        canslim,
        module_scores,
        buy_signals,
        sell_signals,
        risk_warnings,
        key_levels,
        description,
        plain_summary,
        trade_plan,
    }
}

fn key_levels_to_json(key_levels: &[(String, f64)]) -> Value {
    let mut map = Map::new();
    for (k, v) in key_levels {
        map.insert(k.clone(), json!(v));
    }
    Value::Object(map)
}

fn module_scores_to_json(module_scores: &[(String, i64)]) -> Value {
    let mut map = Map::new();
    for (k, v) in module_scores {
        map.insert(k.clone(), json!(v));
    }
    Value::Object(map)
}

/// Serialize a [`SignalEngineResult`] to the exact JSON shape of the legacy
/// `app.py::signal_to_dict`.
pub fn signal_to_json(r: &SignalEngineResult) -> Value {
    let trend = &r.trend;
    let trendline_json = match &trend.trendline {
        Some(tl) => json!({
            "type": tl.kind,
            "slope": tl.slope,
            "current_price": tl.current_price,
            "points": [tl.points[0], tl.points[1]],
        }),
        None => Value::Null,
    };
    let trend_json = json!({
        "direction": trend.direction,
        "strength": trend.strength,
        "stage": trend.stage,
        "ma_arrangement": trend.ma_arrangement,
        "ma_scores": {
            "ma20_dir": trend.ma_scores.ma20_dir,
            "ma60_dir": trend.ma_scores.ma60_dir,
            "price_vs_ma20": trend.ma_scores.price_vs_ma20,
            "price_vs_ma60": trend.ma_scores.price_vs_ma60,
            "resonance": trend.ma_scores.resonance,
        },
        "trendline": trendline_json,
        "signals": trend.signals,
    });

    let patterns_json: Vec<Value> = r
        .patterns
        .iter()
        .map(|p| {
            let mut kl = Map::new();
            for (k, v) in &p.key_levels {
                kl.insert(k.clone(), json!(v));
            }
            json!({
                "name": p.name,
                "direction": p.direction,
                "confidence": p.confidence,
                "status": p.status,
                "target_price": p.target_price,
                "key_levels": Value::Object(kl),
                "description": p.description,
            })
        })
        .collect();

    let vp = &r.volume_price;
    let vp_json = json!({
        "pattern": vp.pattern,
        "direction": vp.direction,
        "confidence": vp.confidence,
        "volume_ratio": vp.volume_ratio,
        "turnover": vp.turnover,
        "obv_trend": vp.obv_trend,
        "signals": vp.signals,
        "description": vp.description,
    });

    let breakouts_json: Vec<Value> = r
        .breakouts
        .iter()
        .map(|b| {
            json!({
                "system": b.system,
                "signal": b.signal,
                "breakout_price": b.breakout_price,
                "current_n": b.current_n,
                "stop_loss": b.stop_loss,
                "entry_price": b.entry_price,
                "position_units": b.position_units,
                "exit_price": b.exit_price,
                "channel_high": b.channel_high,
                "channel_low": b.channel_low,
                "next_add_price": b.next_add_price,
                "signals": b.signals,
                "description": b.description,
            })
        })
        .collect();

    let cs = &r.canslim;
    let cup_handle_json = match &cs.cup_handle {
        Some(ch) => json!({
            "pattern": ch.pattern,
            "cup_high": ch.cup_high,
            "cup_low": ch.cup_low,
            "handle_high": ch.handle_high,
            "handle_low": ch.handle_low,
            "cup_depth": ch.cup_depth,
            "handle_depth": ch.handle_depth,
            "breakout": ch.breakout,
            "buy_point": ch.buy_point,
            "target": ch.target,
        }),
        None => Value::Null,
    };
    let canslim_json = json!({
        "c_score": cs.c_score,
        "a_score": cs.a_score,
        "n_score": cs.n_score,
        "s_score": cs.s_score,
        "l_score": cs.l_score,
        "i_score": cs.i_score,
        "m_score": cs.m_score,
        "total": cs.total,
        "grade": cs.grade,
        "signals": cs.signals,
        "cup_handle": cup_handle_json,
        "description": cs.description,
    });

    let plan = &r.trade_plan;
    let trade_plan_json = json!({
        "action": plan.action,
        "entry_price": plan.entry_price,
        "stop_loss": plan.stop_loss,
        "target_price": plan.target_price,
        "position_size": plan.position_size,
        "holding_period": plan.holding_period,
        "risk_reward_ratio": plan.risk_reward_ratio,
        "max_loss_pct": plan.max_loss_pct,
        "notes": plan.notes,
    });

    json!({
        "action": r.action,
        "score": r.score,
        "confidence": r.confidence,
        "risk_level": r.risk_level,
        "signal_strength": r.signal_strength,
        "plain_summary": r.plain_summary,
        "trade_plan": trade_plan_json,
        "module_scores": module_scores_to_json(&r.module_scores),
        "buy_signals": r.buy_signals,
        "sell_signals": r.sell_signals,
        "risk_warnings": r.risk_warnings,
        "key_levels": key_levels_to_json(&r.key_levels),
        "description": r.description,
        "trend": trend_json,
        "patterns": patterns_json,
        "volume_price": vp_json,
        "breakouts": breakouts_json,
        "canslim": canslim_json,
    })
}
