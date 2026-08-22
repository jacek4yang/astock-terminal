//! Signal post-processing, ported from `app.py`:
//! - `apply_breadth_m_adjustment`: market-breadth CANSLIM M-score ladder
//!   (as replicated in `fixtures/gen_golden.py`: only `m_score` changes, the
//!   CANSLIM signals/description keep the pre-adjustment M);
//! - `apply_signal_optimization`: hard/soft veto, re-grading, M-driven
//!   position sizing, risk-reward check.
//!
//! The legacy implementation discovers veto conditions by scanning the
//! *display texts* of signals for Chinese substrings. Here the same boolean
//! outcomes are computed from structured module data via [`VetoInputs`]:
//!
//! | legacy keyword   | structured source                                |
//! |------------------|--------------------------------------------------|
//! | 跌破MA20         | unreachable (no legacy signal text contains it)  |
//! | 价跌量增         | `volume_price.pattern == "价跌量增"`             |
//! | OBV下降/走低/下行 | `volume_price.obv_trend == "下降"` (only OBV下降 is reachable) |
//! | MA20向下/下行    | `"MA20向下" in trend.signals` (only MA20向下 is reachable)     |
//! | 受压60日         | unreachable (no legacy signal text contains it)  |
//!
//! Reachability was verified against the legacy signal-text generators:
//! buy/sell signal lists never embed these keywords (the 量价 buy entry only
//! fires for 看涨 patterns, trend MA20* signals are excluded from buy
//! signals, and 今日*/大盘* texts are filtered out of the scan upstream).
//! The golden tests prove the equivalence on all fixtures.

use crate::engine::SignalEngineResult;
use crate::types::Breadth;
use crate::util::py_f64;
use serde_json::{json, Value};

/// Structured veto inputs, derived from module results (see module docs).
#[derive(Debug, Clone, Default)]
pub struct VetoInputs {
    /// Legacy keyword 跌破MA20 — unreachable in legacy texts; always false.
    pub broke_ma20: bool,
    /// 价跌量增 pattern (checked separately by legacy against vp.pattern).
    pub price_down_volume_up: bool,
    /// OBV下降/走低/下行 — only OBV下降 is reachable, from vp.obv_trend.
    pub obv_falling: bool,
    /// MA20向下/下行 — only MA20向下 is reachable, from trend.signals.
    pub ma20_down: bool,
    /// Legacy keyword 受压60日 — unreachable in legacy texts; always false.
    pub pressed_ma60: bool,
}

impl VetoInputs {
    /// Derive the flags from the structured engine result.
    pub fn from_result(r: &SignalEngineResult) -> Self {
        VetoInputs {
            broke_ma20: false,
            price_down_volume_up: r.volume_price.pattern.contains("价跌量增"),
            obv_falling: r.volume_price.obv_trend == "下降",
            ma20_down: r.trend.signals.iter().any(|s| s == "MA20向下"),
            pressed_ma60: false,
        }
    }
}

/// Breadth-ratio → M-score bonus ladder: ±15/10/5/−5/−10/−15.
pub fn breadth_m_bonus(breadth_ratio: f64) -> i64 {
    if breadth_ratio >= 0.7 {
        15
    } else if breadth_ratio >= 0.6 {
        10
    } else if breadth_ratio >= 0.5 {
        5
    } else if breadth_ratio >= 0.4 {
        -5
    } else if breadth_ratio >= 0.3 {
        -10
    } else {
        -15
    }
}

/// Apply the market-breadth M-score adjustment, exactly as the golden-fixture
/// generator does: only `canslim.m_score` is modified (clamped to [0, 100]);
/// unlike `app.py`'s live handler, the CANSLIM signals/description are NOT
/// touched.
pub fn apply_breadth_m_adjustment(signal: &mut Value, breadth: &Breadth) {
    if breadth.total < 50 {
        return;
    }
    let Some(canslim) = signal.get_mut("canslim") else {
        return;
    };
    if canslim.is_null() {
        return;
    }
    let old_m = canslim.get("m_score").and_then(Value::as_i64).unwrap_or(0);
    let new_m = (old_m + breadth_m_bonus(breadth.breadth_ratio)).clamp(0, 100);
    canslim["m_score"] = json!(new_m);
}

fn get_i64(v: &Value, key: &str, default: i64) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(default)
}

fn get_f64(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// Apply the signal optimization (hard/soft veto, re-grading, position
/// sizing, risk-reward check), mutating the signal dict like the legacy
/// `_apply_signal_optimization`.
pub fn apply_signal_optimization(signal: &mut Value, veto: &VetoInputs) {
    let action = signal
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("观望")
        .to_string();
    let score = get_i64(signal, "score", 0);
    let confidence = get_i64(signal, "confidence", 0);
    let module_scores = signal.get("module_scores").cloned().unwrap_or(Value::Null);
    let mut risk_warnings: Vec<String> = signal
        .get("risk_warnings")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let m_score = signal
        .get("canslim")
        .filter(|c| !c.is_null())
        .map(|c| get_i64(c, "m_score", 50))
        .unwrap_or(50);
    let mut trade_plan = signal
        .get("trade_plan")
        .cloned()
        .filter(|v| !v.is_null())
        .unwrap_or_else(|| json!({}));

    let original_action = action.clone();
    let mut action = action;

    // ---- Hard veto (stock-signal text scan in legacy; structured here) ----
    let hard_veto_reason: Option<&str> = if veto.broke_ma20 {
        Some("价格跌破MA20，趋势已坏")
    } else if veto.obv_falling {
        Some("OBV下降，量能走弱")
    } else if veto.price_down_volume_up {
        // Legacy checks vp.pattern separately only when no text veto fired
        Some("价跌量增，恐慌抛售信号")
    } else {
        None
    };

    // ---- Soft veto ----
    let soft_veto_reason: Option<&str> = if veto.ma20_down {
        Some("MA20向下，短期趋势偏弱")
    } else if veto.pressed_ma60 {
        Some("受压60日决策线，上方压力大")
    } else {
        None
    };

    // ---- Re-grading ----
    let is_buy = matches!(action.as_str(), "买入" | "强烈买入");
    let is_sell = matches!(action.as_str(), "卖出" | "强烈卖出");
    let mut veto_reason: Option<String> = None;

    let scores_list = [
        get_i64(&module_scores, "趋势", 50),
        get_i64(&module_scores, "CAN_SLIM", 50),
        get_i64(&module_scores, "突破", 50),
        get_i64(&module_scores, "量价", 50),
        get_i64(&module_scores, "形态", 50),
    ];
    let modules_above_55 = scores_list.iter().filter(|&&s| s >= 55).count() as i64;

    if is_sell {
        // Sell signals are not intercepted
    } else if is_buy {
        if let Some(reason) = hard_veto_reason {
            action = "观望".to_string();
            veto_reason = Some(format!("硬否决：{}", reason));
        } else {
            let mut new_action = if score >= 75 && confidence >= 60 && modules_above_55 >= 4 {
                "强烈买入"
            } else if score >= 65 && confidence >= 45 && modules_above_55 >= 3 {
                "买入"
            } else if score >= 60 {
                "谨慎买入"
            } else {
                "观望"
            };

            // Soft veto demotes one tier
            if let Some(reason) = soft_veto_reason {
                if new_action == "强烈买入" {
                    new_action = "买入";
                    veto_reason = Some(format!("软否决：{}", reason));
                } else if new_action == "买入" {
                    new_action = "谨慎买入";
                    veto_reason = Some(format!("软否决：{}", reason));
                }
            }
            action = new_action.to_string();
        }
    }

    // ---- M-score-driven position sizing ----
    let original_position = trade_plan
        .get("position_size")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let position_advice: String;
    if matches!(action.as_str(), "买入" | "强烈买入" | "谨慎买入") {
        if m_score < 40 {
            position_advice = "轻仓(1/4) — 大盘偏空，严格控制仓位".to_string();
            if action == "强烈买入" {
                action = "买入".to_string();
                let extra = format!("大盘M分{}偏低，降级为买入", m_score);
                veto_reason = Some(match veto_reason {
                    Some(r) => format!("{}；{}", r, extra),
                    None => extra,
                });
            } else if action == "买入" {
                action = "谨慎买入".to_string();
                let extra = format!("大盘M分{}偏低，降级为谨慎买入", m_score);
                veto_reason = Some(match veto_reason {
                    Some(r) => format!("{}；{}", r, extra),
                    None => extra,
                });
            }
        } else if m_score < 55 {
            position_advice = "半仓(1/2) — 大盘中性偏弱".to_string();
        } else if m_score < 65 {
            position_advice = if original_position.is_empty() {
                "半仓(1/2)".to_string()
            } else {
                original_position.clone()
            };
        } else {
            position_advice = if original_position.is_empty() {
                "正常仓位".to_string()
            } else {
                original_position.clone()
            };
        }
    } else {
        position_advice = "空仓等待".to_string();
    }

    // ---- Risk-reward check ----
    let entry = get_f64(&trade_plan, "entry_price");
    let stop = get_f64(&trade_plan, "stop_loss");
    let target = get_f64(&trade_plan, "target_price");
    let mut risk_reward = get_f64(&trade_plan, "risk_reward_ratio");

    let mut risk_notes: Vec<String> = Vec::new();
    if entry != 0.0 && stop != 0.0 && target != 0.0 && entry > 0.0 {
        if risk_reward == 0.0 {
            let risk_amt = entry - stop;
            let reward_amt = target - entry;
            if risk_amt > 0.0 {
                risk_reward = crate::util::py_round(reward_amt / risk_amt, 1);
            }
        }

        if risk_reward != 0.0 {
            if risk_reward < 1.0 {
                risk_notes.push(format!("盈亏比{}倒挂，不建议入场", py_f64(risk_reward)));
                if matches!(action.as_str(), "买入" | "强烈买入" | "谨慎买入") {
                    action = "观望".to_string();
                    let extra = format!("盈亏比{}倒挂", py_f64(risk_reward));
                    veto_reason = Some(match veto_reason {
                        Some(r) => format!("{}；{}", r, extra),
                        None => extra,
                    });
                }
            } else if risk_reward < 1.5 {
                risk_notes.push(format!("盈亏比{}偏低，谨慎操作", py_f64(risk_reward)));
            } else if risk_reward < 2.0 {
                risk_notes.push(format!("盈亏比{}，勉强达标", py_f64(risk_reward)));
            } else {
                risk_notes.push(format!("盈亏比{}，风险收益比良好", py_f64(risk_reward)));
            }
        }
    }

    // ---- Write back ----
    signal["action"] = json!(action);
    signal["optimized_action"] = json!(action);
    signal["original_action"] = json!(original_action);
    if let Some(reason) = &veto_reason {
        signal["veto_reason"] = json!(reason);
        risk_warnings.insert(0, reason.clone());
    }
    signal["risk_warnings"] = json!(risk_warnings);
    signal["position_advice"] = json!(position_advice);
    signal["risk_notes"] = json!(risk_notes);
    signal["risk_reward"] = json!(risk_reward);

    // Legacy writes the plan back only when the dict is non-empty.
    if trade_plan.as_object().is_some_and(|m| !m.is_empty()) {
        trade_plan["position_size"] = json!(position_advice);
        signal["trade_plan"] = trade_plan;
    }

    // Update the plain summary when a veto changed the action
    if let Some(reason) = &veto_reason {
        if action != signal["original_action"].as_str().unwrap_or_default() {
            let prefix = format!(
                "[优化：{}→{}] {}。",
                signal["original_action"].as_str().unwrap_or_default(),
                action,
                reason
            );
            let old = signal
                .get("plain_summary")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            signal["plain_summary"] = json!(format!("{}{}", prefix, old));
        }
    }
}
