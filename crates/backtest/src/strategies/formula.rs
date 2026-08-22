//! 受限、可审计的公式策略语言。
//!
//! 这不是通用脚本运行时：表达式只能读取当前及历史日线，不能访问文件、
//! 网络、系统进程或任意代码。反序列化后还会校验表达式深度、节点数、窗口
//! 和历史偏移，适合由 Agent 生成后直接进入确定性的回测引擎。

use serde::{Deserialize, Serialize};

use crate::data::Bar;
use crate::strategy::{Order, Qty, Strategy, StrategyContext};

use super::ParamError;

const STRATEGY: &str = "formula_dsl";
const MAX_DEPTH: usize = 8;
const MAX_NODES: usize = 64;
const MAX_WINDOW: usize = 250;
const MAX_OFFSET: usize = 250;

/// 一份完整的公式策略。`version` 当前必须为 1。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormulaStrategySpec {
    pub version: u8,
    /// 仅用于审计日志和界面展示，不参与代码执行。
    #[serde(default = "default_display_name")]
    pub name: String,
    /// 空仓时满足该条件，在下一根日线开盘尝试满仓买入。
    pub entry: RuleExpr,
    /// 持仓且存在可卖数量时满足该条件，在下一根日线开盘尝试清仓。
    pub exit: RuleExpr,
}

fn default_display_name() -> String {
    "AI 公式策略".to_string()
}

/// 布尔规则。所有比较遇到历史数据不足时均返回 false。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleExpr {
    And { rules: Vec<RuleExpr> },
    Or { rules: Vec<RuleExpr> },
    Not { rule: Box<RuleExpr> },
    GreaterThan { left: ValueExpr, right: ValueExpr },
    GreaterOrEqual { left: ValueExpr, right: ValueExpr },
    LessThan { left: ValueExpr, right: ValueExpr },
    LessOrEqual { left: ValueExpr, right: ValueExpr },
    CrossAbove { left: ValueExpr, right: ValueExpr },
    CrossBelow { left: ValueExpr, right: ValueExpr },
}

/// 可用于比较的行情值或技术指标。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ValueExpr {
    Constant {
        value: f64,
    },
    Price {
        field: PriceField,
        #[serde(default)]
        offset: usize,
    },
    Sma {
        field: PriceField,
        window: usize,
        #[serde(default)]
        offset: usize,
    },
    Highest {
        field: PriceField,
        window: usize,
        #[serde(default)]
        offset: usize,
    },
    Lowest {
        field: PriceField,
        window: usize,
        #[serde(default)]
        offset: usize,
    },
    Rsi {
        window: usize,
        #[serde(default)]
        offset: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceField {
    Open,
    High,
    Low,
    Close,
    Volume,
}

impl PriceField {
    fn read(self, bar: &Bar) -> f64 {
        match self {
            Self::Open => bar.open,
            Self::High => bar.high,
            Self::Low => bar.low,
            Self::Close => bar.close,
            Self::Volume => bar.volume,
        }
    }
}

/// 已通过全部资源上限校验、可交给回测引擎的公式策略。
pub struct FormulaStrategy {
    spec: FormulaStrategySpec,
}

impl FormulaStrategy {
    pub fn try_new(spec: FormulaStrategySpec) -> Result<Self, ParamError> {
        if spec.version != 1 {
            return Err(ParamError::invalid(
                STRATEGY,
                format!("仅支持 version=1，收到 {}", spec.version),
            ));
        }
        let display_name = spec.name.trim();
        if display_name.is_empty() || display_name.chars().count() > 80 {
            return Err(ParamError::invalid(STRATEGY, "策略名称须为 1-80 个字符"));
        }
        if display_name.chars().any(char::is_control) {
            return Err(ParamError::invalid(STRATEGY, "策略名称不能包含控制字符"));
        }
        let mut nodes = 0;
        validate_rule(&spec.entry, 1, &mut nodes)?;
        validate_rule(&spec.exit, 1, &mut nodes)?;
        Ok(Self { spec })
    }

    pub fn spec(&self) -> &FormulaStrategySpec {
        &self.spec
    }
}

impl Strategy for FormulaStrategy {
    fn name(&self) -> &str {
        STRATEGY
    }

    fn on_bar(&mut self, ctx: &StrategyContext, _bar_index: usize) -> Vec<Order> {
        let position = ctx.position();
        if position.shares == 0 {
            if eval_rule(&self.spec.entry, ctx, 0) {
                vec![Order::buy(Qty::Max)
                    .with_reason(format!("公式策略「{}」满足入场条件", self.spec.name))]
            } else {
                vec![]
            }
        } else if position.sellable > 0 && eval_rule(&self.spec.exit, ctx, 0) {
            vec![Order::sell(Qty::Max)
                .with_reason(format!("公式策略「{}」满足离场条件", self.spec.name))]
        } else {
            vec![]
        }
    }
}

fn validate_rule(rule: &RuleExpr, depth: usize, nodes: &mut usize) -> Result<(), ParamError> {
    *nodes += 1;
    if depth > MAX_DEPTH {
        return Err(ParamError::invalid(
            STRATEGY,
            format!("表达式深度不能超过 {MAX_DEPTH}"),
        ));
    }
    if *nodes > MAX_NODES {
        return Err(ParamError::invalid(
            STRATEGY,
            format!("表达式节点不能超过 {MAX_NODES}"),
        ));
    }
    match rule {
        RuleExpr::And { rules } | RuleExpr::Or { rules } => {
            if rules.is_empty() || rules.len() > 16 {
                return Err(ParamError::invalid(
                    STRATEGY,
                    "and/or 每组须包含 1-16 个条件",
                ));
            }
            for child in rules {
                validate_rule(child, depth + 1, nodes)?;
            }
        }
        RuleExpr::Not { rule } => validate_rule(rule, depth + 1, nodes)?,
        RuleExpr::GreaterThan { left, right }
        | RuleExpr::GreaterOrEqual { left, right }
        | RuleExpr::LessThan { left, right }
        | RuleExpr::LessOrEqual { left, right }
        | RuleExpr::CrossAbove { left, right }
        | RuleExpr::CrossBelow { left, right } => {
            validate_value(left)?;
            validate_value(right)?;
        }
    }
    Ok(())
}

fn validate_value(value: &ValueExpr) -> Result<(), ParamError> {
    let (window, offset, constant) = match value {
        ValueExpr::Constant { value } => (None, 0, Some(*value)),
        ValueExpr::Price { offset, .. } => (None, *offset, None),
        ValueExpr::Sma { window, offset, .. }
        | ValueExpr::Highest { window, offset, .. }
        | ValueExpr::Lowest { window, offset, .. }
        | ValueExpr::Rsi { window, offset } => (Some(*window), *offset, None),
    };
    if let Some(value) = constant {
        if !value.is_finite() || value.abs() > 1e15 {
            return Err(ParamError::invalid(STRATEGY, "常数必须为有限且合理的数值"));
        }
    }
    if offset > MAX_OFFSET {
        return Err(ParamError::invalid(
            STRATEGY,
            format!("历史偏移不能超过 {MAX_OFFSET}"),
        ));
    }
    if let Some(window) = window {
        if !(1..=MAX_WINDOW).contains(&window) {
            return Err(ParamError::invalid(
                STRATEGY,
                format!("指标窗口须在 1-{MAX_WINDOW} 之间"),
            ));
        }
    }
    Ok(())
}

fn eval_rule(rule: &RuleExpr, ctx: &StrategyContext, shift: usize) -> bool {
    match rule {
        RuleExpr::And { rules } => rules.iter().all(|rule| eval_rule(rule, ctx, shift)),
        RuleExpr::Or { rules } => rules.iter().any(|rule| eval_rule(rule, ctx, shift)),
        RuleExpr::Not { rule } => !eval_rule(rule, ctx, shift),
        RuleExpr::GreaterThan { left, right } => compare(left, right, ctx, shift, |a, b| a > b),
        RuleExpr::GreaterOrEqual { left, right } => compare(left, right, ctx, shift, |a, b| a >= b),
        RuleExpr::LessThan { left, right } => compare(left, right, ctx, shift, |a, b| a < b),
        RuleExpr::LessOrEqual { left, right } => compare(left, right, ctx, shift, |a, b| a <= b),
        RuleExpr::CrossAbove { left, right } => {
            let now = values(left, right, ctx, shift);
            let previous = values(left, right, ctx, shift.saturating_add(1));
            matches!((now, previous), (Some((a, b)), Some((pa, pb))) if pa <= pb && a > b)
        }
        RuleExpr::CrossBelow { left, right } => {
            let now = values(left, right, ctx, shift);
            let previous = values(left, right, ctx, shift.saturating_add(1));
            matches!((now, previous), (Some((a, b)), Some((pa, pb))) if pa >= pb && a < b)
        }
    }
}

fn compare(
    left: &ValueExpr,
    right: &ValueExpr,
    ctx: &StrategyContext,
    shift: usize,
    predicate: impl FnOnce(f64, f64) -> bool,
) -> bool {
    values(left, right, ctx, shift).is_some_and(|(left, right)| predicate(left, right))
}

fn values(
    left: &ValueExpr,
    right: &ValueExpr,
    ctx: &StrategyContext,
    shift: usize,
) -> Option<(f64, f64)> {
    Some((
        eval_value(left, ctx, shift)?,
        eval_value(right, ctx, shift)?,
    ))
}

fn eval_value(value: &ValueExpr, ctx: &StrategyContext, shift: usize) -> Option<f64> {
    match value {
        ValueExpr::Constant { value } => Some(*value),
        ValueExpr::Price { field, offset } => {
            let index = history_index(ctx.len(), shift.checked_add(*offset)?)?;
            Some(field.read(&ctx.bars()[index]))
        }
        ValueExpr::Sma {
            field,
            window,
            offset,
        } => window_values(ctx, shift, *offset, *window, *field)
            .map(|rows| rows.iter().sum::<f64>() / *window as f64),
        ValueExpr::Highest {
            field,
            window,
            offset,
        } => window_values(ctx, shift, *offset, *window, *field)
            .map(|rows| rows.into_iter().fold(f64::NEG_INFINITY, f64::max)),
        ValueExpr::Lowest {
            field,
            window,
            offset,
        } => window_values(ctx, shift, *offset, *window, *field)
            .map(|rows| rows.into_iter().fold(f64::INFINITY, f64::min)),
        ValueExpr::Rsi { window, offset } => rsi(ctx, shift, *offset, *window),
    }
}

fn history_index(len: usize, shift: usize) -> Option<usize> {
    len.checked_sub(shift.checked_add(1)?)
}

fn window_values(
    ctx: &StrategyContext,
    shift: usize,
    offset: usize,
    window: usize,
    field: PriceField,
) -> Option<Vec<f64>> {
    let end = history_index(ctx.len(), shift.checked_add(offset)?)?.checked_add(1)?;
    let start = end.checked_sub(window)?;
    Some(
        ctx.bars()[start..end]
            .iter()
            .map(|bar| field.read(bar))
            .collect(),
    )
}

fn rsi(ctx: &StrategyContext, shift: usize, offset: usize, window: usize) -> Option<f64> {
    let end_index = history_index(ctx.len(), shift.checked_add(offset)?)?;
    let start = end_index.checked_sub(window)?;
    let rows = &ctx.bars()[start..=end_index];
    let (mut gains, mut losses) = (0.0, 0.0);
    for pair in rows.windows(2) {
        let change = pair[1].close - pair[0].close;
        if change >= 0.0 {
            gains += change;
        } else {
            losses -= change;
        }
    }
    if losses == 0.0 {
        Some(if gains == 0.0 { 50.0 } else { 100.0 })
    } else {
        let rs = gains / losses;
        Some(100.0 - 100.0 / (1.0 + rs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::PositionSnapshot;
    use chrono::NaiveDate;

    fn bars(values: &[f64]) -> Vec<Bar> {
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                Bar::flat(
                    NaiveDate::from_ymd_opt(2025, 1, 1).unwrap()
                        + chrono::Duration::days(index as i64),
                    *value,
                )
            })
            .collect()
    }

    fn cross_spec() -> FormulaStrategySpec {
        FormulaStrategySpec {
            version: 1,
            name: "三日均线上穿五日均线".into(),
            entry: RuleExpr::CrossAbove {
                left: ValueExpr::Sma {
                    field: PriceField::Close,
                    window: 3,
                    offset: 0,
                },
                right: ValueExpr::Sma {
                    field: PriceField::Close,
                    window: 5,
                    offset: 0,
                },
            },
            exit: RuleExpr::CrossBelow {
                left: ValueExpr::Sma {
                    field: PriceField::Close,
                    window: 3,
                    offset: 0,
                },
                right: ValueExpr::Sma {
                    field: PriceField::Close,
                    window: 5,
                    offset: 0,
                },
            },
        }
    }

    #[test]
    fn serde_roundtrip_and_cross_signal() {
        let json = serde_json::to_value(cross_spec()).unwrap();
        let mut strategy = FormulaStrategy::try_new(serde_json::from_value(json).unwrap()).unwrap();
        let rows = bars(&[5.0, 4.0, 3.0, 2.0, 1.0, 3.0, 6.0]);
        let ctx = StrategyContext::new(&rows, rows.len() - 1, PositionSnapshot::default());
        let orders = strategy.on_bar(&ctx, rows.len() - 1);
        assert_eq!(orders.len(), 1);
        assert!(orders[0].reason.contains("满足入场条件"));
    }

    #[test]
    fn rejects_resource_abuse() {
        let mut spec = cross_spec();
        spec.entry = RuleExpr::GreaterThan {
            left: ValueExpr::Sma {
                field: PriceField::Close,
                window: 251,
                offset: 0,
            },
            right: ValueExpr::Constant { value: 0.0 },
        };
        assert!(FormulaStrategy::try_new(spec).is_err());
    }

    #[test]
    fn insufficient_history_is_not_a_signal() {
        let mut strategy = FormulaStrategy::try_new(cross_spec()).unwrap();
        let rows = bars(&[1.0, 2.0]);
        let ctx = StrategyContext::new(&rows, 1, PositionSnapshot::default());
        assert!(strategy.on_bar(&ctx, 1).is_empty());
    }
}
