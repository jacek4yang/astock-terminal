//! Deterministic conditional playbook for human execution.
//!
//! The legacy signal engine is intentionally kept golden-test compatible.
//! This module consumes that signal plus OHLCV bars and turns it into a
//! volatility/structure-aware plan.  It never routes orders and does not use
//! an LLM to manufacture price levels.

use crate::types::Kline;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSchedule {
    pub open_auction_start: String,
    pub open_auction_end: String,
    pub morning_start: String,
    pub morning_end: String,
    pub afternoon_start: String,
    pub afternoon_end: String,
    pub close_auction_start: String,
    pub close_auction_end: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingConstraints {
    pub board_name: String,
    pub price_limit_pct: f64,
    pub min_lot: u32,
    pub lot_step: u32,
    pub t_plus_1: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualScenario {
    pub name: String,
    pub condition: String,
    pub response: String,
    pub invalidation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualCheckpoint {
    pub phase: String,
    pub time_window: String,
    pub observe: Vec<String>,
    pub required_conditions: Vec<String>,
    pub action_if_confirmed: String,
    pub action_if_failed: String,
    pub next_checkpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualEvidence {
    pub label: String,
    pub value: String,
    pub source: String,
    pub as_of: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualTradingPlan {
    pub plan_id: String,
    pub symbol: String,
    pub name: String,
    pub generated_at: String,
    pub data_as_of: String,
    pub market_regime: String,
    pub thesis: String,
    pub counter_thesis: String,
    pub confidence: i64,
    /// Maximum intended loss as a percentage of total account equity.
    pub risk_budget_pct: f64,
    pub entry_zone_low: f64,
    pub entry_zone_high: f64,
    pub stop_loss: f64,
    pub target_price: f64,
    pub risk_reward_ratio: f64,
    pub stop_basis: String,
    pub target_basis: String,
    pub expected_holding_period: String,
    pub position_guidance: String,
    pub scenarios: Vec<ManualScenario>,
    pub checkpoints: Vec<ManualCheckpoint>,
    pub invalidation_conditions: Vec<String>,
    pub review_triggers: Vec<String>,
    pub constraints: Vec<String>,
    pub evidence: Vec<ManualEvidence>,
    pub disclaimer: String,
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn atr(bars: &[Kline], period: usize) -> Option<f64> {
    if bars.len() < 2 {
        return None;
    }
    let start = bars.len().saturating_sub(period);
    let mut ranges = Vec::new();
    for index in start.max(1)..bars.len() {
        let current = &bars[index];
        let previous_close = bars[index - 1].close;
        let tr = (current.high - current.low)
            .max((current.high - previous_close).abs())
            .max((current.low - previous_close).abs());
        if tr.is_finite() && tr > 0.0 {
            ranges.push(tr);
        }
    }
    (!ranges.is_empty()).then(|| ranges.iter().sum::<f64>() / ranges.len() as f64)
}

fn signal_i64(signal: &Value, key: &str, fallback: i64) -> i64 {
    signal.get(key).and_then(Value::as_i64).unwrap_or(fallback)
}

fn signal_str<'a>(signal: &'a Value, key: &str, fallback: &'a str) -> &'a str {
    signal.get(key).and_then(Value::as_str).unwrap_or(fallback)
}

/// Build a manual, conditional route from deterministic inputs.
///
/// Price levels are derived exclusively from the supplied bars: ATR14,
/// 20-bar structural support/resistance, and pattern targets already emitted
/// by the deterministic signal engine.
#[allow(clippy::too_many_arguments)]
pub fn build_manual_trading_plan(
    symbol: &str,
    name: &str,
    bars: &[Kline],
    signal: &Value,
    sessions: &SessionSchedule,
    constraints: &TradingConstraints,
    generated_at: &str,
    source: &str,
) -> Option<ManualTradingPlan> {
    let last = bars.last()?;
    if !last.close.is_finite() || last.close <= 0.0 {
        return None;
    }

    let entry = last.close;
    let atr14 = atr(bars, 14).unwrap_or(entry * 0.025).max(entry * 0.002);
    let recent = &bars[bars.len().saturating_sub(20)..];
    let support = recent
        .iter()
        .map(|bar| bar.low)
        .filter(|value| value.is_finite() && *value > 0.0)
        .fold(entry, f64::min);
    let resistance = recent
        .iter()
        .map(|bar| bar.high)
        .filter(|value| value.is_finite() && *value > 0.0)
        .fold(entry, f64::max);

    // Use the tighter of 2×ATR and structural invalidation, while preventing
    // an unrealistically tiny stop caused by a single bar touching support.
    let atr_stop = entry - atr14 * 2.0;
    let structural_stop = support - atr14 * 0.15;
    let maximum_stop = entry - atr14 * 0.65;
    let stop = atr_stop.max(structural_stop).min(maximum_stop).max(0.01);
    let risk = (entry - stop).max(0.01);

    let pattern_target = signal
        .get("patterns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|pattern| pattern.get("direction").and_then(Value::as_str) == Some("看涨"))
        .filter_map(|pattern| pattern.get("target_price").and_then(Value::as_f64))
        .filter(|target| *target > entry + risk)
        .min_by(f64::total_cmp);
    let structural_target = (resistance > entry + risk).then_some(resistance);
    let target = pattern_target
        .or(structural_target)
        .unwrap_or(entry + risk * 2.0)
        .max(entry + risk * 1.5);

    let entry_zone_low = (entry - atr14 * 0.25).max(stop + risk * 0.2);
    let entry_zone_high = entry + atr14 * 0.20;
    let risk_reward = (target - entry) / risk;

    let score = signal_i64(signal, "score", 50);
    let confidence = signal_i64(signal, "confidence", 50);
    let action = signal_str(signal, "action", "观望");
    let m_score = signal
        .get("canslim")
        .and_then(|value| value.get("m_score"))
        .and_then(Value::as_i64)
        .unwrap_or(50);
    let trend = signal
        .get("trend")
        .and_then(|value| value.get("direction"))
        .and_then(Value::as_str)
        .unwrap_or("未知");
    let market_regime = match m_score {
        65.. => "风险偏好较强",
        40..=64 => "中性/分化",
        _ => "防御/风险偏好较弱",
    };
    let risk_budget_pct = if action.contains('买') && score >= 75 && m_score >= 50 {
        1.0
    } else if action.contains('买') && score >= 60 {
        0.65
    } else {
        0.35
    };
    let expected_holding_period = if atr14 / entry >= 0.045 {
        "日线高波动策略：3–10 个交易日，逐日复核"
    } else if trend == "上升" {
        "日线趋势策略：10–30 个交易日，按检查点复核"
    } else {
        "等待型策略：信号确认前不设固定持有期"
    };

    let stop_basis = format!(
        "ATR14={:.2}；20日结构支撑={:.2}；取波动止损与结构失效位的审慎组合",
        atr14, support
    );
    let target_basis = if pattern_target.is_some() {
        "确定性形态测量目标，且至少覆盖 1.5R".to_string()
    } else if structural_target.is_some() {
        format!("20日结构阻力 {:.2}，且至少覆盖 1.5R", resistance)
    } else {
        "未发现可靠上方结构位，采用 2R 风险单位作为复核目标而非收益承诺".to_string()
    };

    let mut hasher = DefaultHasher::new();
    symbol.hash(&mut hasher);
    last.date.hash(&mut hasher);
    entry.to_bits().hash(&mut hasher);
    stop.to_bits().hash(&mut hasher);
    target.to_bits().hash(&mut hasher);
    action.hash(&mut hasher);
    let plan_id = format!("mtp-{symbol}-{:016x}", hasher.finish());

    let checkpoints = vec![
        ManualCheckpoint {
            phase: "集合竞价".into(),
            time_window: format!(
                "{}–{}",
                sessions.open_auction_start, sessions.open_auction_end
            ),
            observe: vec![
                "竞价缺口与昨日收盘/入场区关系".into(),
                "竞价量能是否异常".into(),
            ],
            required_conditions: vec!["不以未确认竞价价格追单".into()],
            action_if_confirmed: "记录开盘情景，等待连续竞价确认".into(),
            action_if_failed: "出现无量高开或接近涨停不可成交时取消追价".into(),
            next_checkpoint: "开盘波动窗口".into(),
        },
        ManualCheckpoint {
            phase: "开盘波动窗口".into(),
            time_window: format!("{} 起首个30分钟", sessions.morning_start),
            observe: vec!["价格能否站稳入场区".into(), "量价与市场宽度是否同向".into()],
            required_conditions: vec![
                format!(
                    "价格位于 {:.2}–{:.2} 且未破失效位",
                    entry_zone_low, entry_zone_high
                ),
                "不是封死涨停或流动性显著不足".into(),
            ],
            action_if_confirmed: "仅按风险预算手工分批；不得由软件自动下单".into(),
            action_if_failed: "不追价，转入上午复核".into(),
            next_checkpoint: "上午盘复核".into(),
        },
        ManualCheckpoint {
            phase: "上午盘复核".into(),
            time_window: format!("{}–{}", sessions.morning_start, sessions.morning_end),
            observe: vec!["回踩是否缩量".into(), "板块相对强度和大盘环境".into()],
            required_conditions: vec![format!("收盘/实时结构保持在 {:.2} 上方", stop)],
            action_if_confirmed: "维持既定风险，不因短时上涨放大预算".into(),
            action_if_failed: "跌破失效位并得到成交量确认时撤销方案".into(),
            next_checkpoint: "午间复盘".into(),
        },
        ManualCheckpoint {
            phase: "午间复盘".into(),
            time_window: format!("{}–{}", sessions.morning_end, sessions.afternoon_start),
            observe: vec!["上午成交结构".into(), "公告/事件是否改变原假设".into()],
            required_conditions: vec!["数据无重大新增缺失或冲突".into()],
            action_if_confirmed: "保留方案，下午仅按原风险预算管理".into(),
            action_if_failed: "重大新信息出现时重新计算，不沿用旧计划".into(),
            next_checkpoint: "尾盘确认".into(),
        },
        ManualCheckpoint {
            phase: "尾盘确认".into(),
            time_window: format!(
                "{}–{}",
                sessions.close_auction_start, sessions.close_auction_end
            ),
            observe: vec!["收盘能否守住结构位".into(), "尾盘资金方向".into()],
            required_conditions: vec![format!("不以低于 {:.2} 的失效收盘延续原方案", stop)],
            action_if_confirmed: "记录收盘证据，下一交易日重新核验".into(),
            action_if_failed: "标记方案失效；受 T+1 约束的当日新仓次日优先风控".into(),
            next_checkpoint: "下一交易日盘前".into(),
        },
    ];

    let constraints_text = vec![
        format!(
            "{}涨跌幅规则参考 ±{:.0}%；涨停通常无法保证买入，跌停通常无法保证卖出",
            constraints.board_name,
            constraints.price_limit_pct * 100.0
        ),
        format!(
            "最小申报 {} 股，后续增量 {} 股",
            constraints.min_lot, constraints.lot_step
        ),
        if constraints.t_plus_1 {
            "A股 T+1：当日买入通常不能当日卖出，隔夜跳空风险必须计入预算".into()
        } else {
            "该品种规则快照未标记 T+1，执行前仍需人工核对交易规则".into()
        },
    ];

    Some(ManualTradingPlan {
        plan_id,
        symbol: symbol.into(),
        name: name.into(),
        generated_at: generated_at.into(),
        data_as_of: last.date.clone(),
        market_regime: market_regime.into(),
        thesis: format!(
            "确定性引擎结论为“{action}”，日线趋势为“{trend}”；只有价格、量能与市场环境共同确认时方案才成立"
        ),
        counter_thesis: format!(
            "若价格有效跌破 {:.2}，或市场/板块显著转弱，原假设失效而不是继续摊薄成本",
            round2(stop)
        ),
        confidence,
        risk_budget_pct,
        entry_zone_low: round2(entry_zone_low),
        entry_zone_high: round2(entry_zone_high),
        stop_loss: round2(stop),
        target_price: round2(target),
        risk_reward_ratio: (risk_reward * 10.0).round() / 10.0,
        stop_basis,
        target_basis,
        expected_holding_period: expected_holding_period.into(),
        position_guidance: format!(
            "按止损距离反推数量，使触发止损时账户损失不超过 {:.2}%；不把“仓位百分比”替代风险预算",
            risk_budget_pct
        ),
        scenarios: vec![
            ManualScenario {
                name: "基准确认".into(),
                condition: format!(
                    "价格回到 {:.2}–{:.2}，量价确认且市场环境不恶化",
                    round2(entry_zone_low),
                    round2(entry_zone_high)
                ),
                response: "按风险预算手工分批，并在每个检查点复核".into(),
                invalidation: format!("有效跌破 {:.2}", round2(stop)),
            },
            ManualScenario {
                name: "跳空高开".into(),
                condition: format!("开盘显著高于 {:.2}", round2(entry_zone_high)),
                response: "没有量能和回踩确认时不追；若接近涨停且不可成交则放弃该次机会".into(),
                invalidation: "高开回落并跌回突破区下方".into(),
            },
            ManualScenario {
                name: "跳空低开/防御".into(),
                condition: format!("开盘接近或低于 {:.2}", round2(stop)),
                response: "不新增风险；先评估是否可成交及是否触发价格限制".into(),
                invalidation: "放量跌破结构位或出现改变基本假设的事件".into(),
            },
        ],
        checkpoints,
        invalidation_conditions: vec![
            format!("价格有效跌破 {:.2} 且量能确认", round2(stop)),
            "大盘与所属板块同步转弱，原相对强度逻辑消失".into(),
            "公告、财报或监管事件改变原始投资假设".into(),
            "行情/财务数据出现过期、冲突或关键缺失".into(),
        ],
        review_triggers: vec![
            "突破入场区、触及止损或目标位".into(),
            "次一交易日盘前".into(),
            "财报/公告发布或除权除息".into(),
            "波动率、流动性或市场状态显著切换".into(),
        ],
        constraints: constraints_text,
        evidence: vec![
            ManualEvidence {
                label: "ATR14".into(),
                value: format!("{:.2}", atr14),
                source: source.into(),
                as_of: last.date.clone(),
            },
            ManualEvidence {
                label: "20日结构支撑/阻力".into(),
                value: format!("{:.2} / {:.2}", support, resistance),
                source: source.into(),
                as_of: last.date.clone(),
            },
            ManualEvidence {
                label: "综合评分/市场分".into(),
                value: format!("{score} / {m_score}"),
                source: "astock-deterministic-engine".into(),
                as_of: last.date.clone(),
            },
        ],
        disclaimer: "仅供人工研究与条件决策，不连接券商、不提交订单、不承诺收益。".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bars() -> Vec<Kline> {
        (0..40)
            .map(|index| {
                let close = 10.0 + index as f64 * 0.05;
                Kline {
                    date: format!("2026-07-{:02}", index % 28 + 1),
                    open: close - 0.05,
                    close,
                    high: close + 0.20,
                    low: close - 0.20,
                    volume: 10_000.0,
                    amount: 1_000_000.0,
                    pct: 0.5,
                    turnover: 1.0,
                }
            })
            .collect()
    }

    fn sessions() -> SessionSchedule {
        SessionSchedule {
            open_auction_start: "09:15".into(),
            open_auction_end: "09:25".into(),
            morning_start: "09:30".into(),
            morning_end: "11:30".into(),
            afternoon_start: "13:00".into(),
            afternoon_end: "14:57".into(),
            close_auction_start: "14:57".into(),
            close_auction_end: "15:00".into(),
        }
    }

    #[test]
    fn plan_levels_are_market_derived_and_conditional() {
        let signal = json!({
            "action": "买入",
            "score": 72,
            "confidence": 64,
            "trend": {"direction": "上升"},
            "canslim": {"m_score": 58},
            "patterns": []
        });
        let plan = build_manual_trading_plan(
            "300308",
            "中际旭创",
            &bars(),
            &signal,
            &sessions(),
            &TradingConstraints {
                board_name: "创业板".into(),
                price_limit_pct: 0.20,
                min_lot: 100,
                lot_step: 100,
                t_plus_1: true,
            },
            "2026-08-22T17:00:00+08:00",
            "fixture",
        )
        .unwrap();
        assert_eq!(plan.symbol, "300308");
        assert!(plan.stop_loss < plan.entry_zone_low);
        assert!(plan.target_price > plan.entry_zone_high);
        assert!(plan.risk_reward_ratio >= 1.5);
        assert_eq!(plan.checkpoints[0].time_window, "09:15–09:25");
        assert!(plan.constraints.iter().any(|item| item.contains("T+1")));
        assert!(plan.disclaimer.contains("不提交订单"));
    }
}
