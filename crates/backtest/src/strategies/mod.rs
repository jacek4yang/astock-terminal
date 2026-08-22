//! 首批移植策略库(来源:`docs/strategy-library-research.md` §五 第一批)。
//!
//! 首批三个,口径与偏差均在各模块文档中显式声明:
//!
//! - [`zscore_mean_reversion::ZscoreMeanReversion`] —— S12,单标的,经
//!   [`crate::engine::BacktestEngine`] 运行。
//! - 双均线(聚宽 2022/72)—— 复用既有 [`crate::strategy::MaCross`],
//!   已核对与文档口径一致:金叉全仓买、死叉清仓,fast/slow 窗口显式参数
//!   (文档默认 5/60)。既有实现仅提供会 panic 的 `MaCross::new`,此处补
//!   参数校验版构造器 [`ma_cross`]。
//! - [`min_corr_rotation::MinCorrRotation`] —— 多标的轮动,引擎当前只
//!   支持单标的,按「策略内自管多标的 + 模块内置轮动 runner」方案实现,
//!   方案取舍见该模块文档。
//!
//! # 注册表
//!
//! [`build_strategy`] / [`list_strategies`] 提供「名字 → 构造器」的稳定
//! 映射,供 UI / Agent 层按名调用。多标的轮动不是单标的
//! [`crate::strategy::Strategy`],注册表以 [`StrategyHandle`] 枚举区分。

pub mod formula;
pub mod min_corr_rotation;
pub mod zscore_mean_reversion;

use thiserror::Error;

use crate::strategy::{MaCross, Strategy};

pub use formula::{FormulaStrategy, FormulaStrategySpec, PriceField, RuleExpr, ValueExpr};
use min_corr_rotation::MinCorrRotation;
use zscore_mean_reversion::ZscoreMeanReversion;

/// 策略参数校验错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParamError {
    /// 某个策略的参数不合法。
    #[error("{strategy}: invalid parameters: {detail}")]
    Invalid {
        /// 策略名。
        strategy: &'static str,
        /// 具体原因。
        detail: String,
    },
    /// 注册表里没有这个名字。
    #[error("unknown strategy name: {0}")]
    UnknownStrategy(String),
}

impl ParamError {
    pub(crate) fn invalid(strategy: &'static str, detail: impl Into<String>) -> Self {
        ParamError::Invalid {
            strategy,
            detail: detail.into(),
        }
    }
}

/// 双均线策略的参数校验构造器(文档 2022/72 口径:金叉全仓、死叉清仓,
/// 默认 5/60)。校验规则与 `MaCross::new` 的断言一致:`1 <= fast < slow`,
/// 但不 panic。
pub fn ma_cross(fast: usize, slow: usize) -> Result<MaCross, ParamError> {
    if fast < 1 || fast >= slow {
        return Err(ParamError::invalid(
            "ma_cross",
            format!("require 1 <= fast < slow, got fast={fast}, slow={slow}"),
        ));
    }
    Ok(MaCross::new(fast, slow))
}

/// 注册表返回的策略句柄:单标的(走引擎)或多标的轮动(走轮动 runner)。
pub enum StrategyHandle {
    /// 单标的策略,直接传给 [`crate::engine::BacktestEngine::run`]。
    Single(Box<dyn Strategy>),
    /// 多标的轮动,传给 [`min_corr_rotation::run_rotation`]。
    Rotation(MinCorrRotation),
}

impl StrategyHandle {
    /// 稳定策略名。
    pub fn name(&self) -> &str {
        match self {
            StrategyHandle::Single(s) => s.name(),
            StrategyHandle::Rotation(r) => r.name(),
        }
    }
}

/// 注册表条目元信息(供 UI 展示 / Agent 选择)。
#[derive(Debug, Clone)]
pub struct StrategyInfo {
    /// 稳定名(`build_strategy` 的入参)。
    pub name: &'static str,
    /// 一句话说明(含文档出处)。
    pub description: &'static str,
    /// 是否多标的(多标的走 `run_rotation`,单标的走 `BacktestEngine`)。
    pub multi_symbol: bool,
}

/// 首批策略清单。
pub fn list_strategies() -> Vec<StrategyInfo> {
    vec![
        StrategyInfo {
            name: "zscore_mean_reversion",
            description: "S12 zscore 均值回归:sub=close−MA20 的 60 日 z 分数,≤−2 满仓买、≥+1 清仓(单标的)",
            multi_symbol: false,
        },
        StrategyInfo {
            name: "ma_cross",
            description: "双均线(2022/72):5/60 日 SMA 金叉全仓买、死叉清仓(单标的)",
            multi_symbol: false,
        },
        StrategyInfo {
            name: "formula_dsl",
            description: "AI 公式策略:受限条件树只读取当前及历史行情，支持价格、SMA、区间高低点与 RSI，禁止文件/网络/任意代码(单标的)",
            multi_symbol: false,
        },
        StrategyInfo {
            name: "min_corr_etf_rotation",
            description: "最小相关 ETF 轮动:候选池两两相关矩阵,持有平均相关最低的 4 只等权,月度再平衡(多标的)",
            multi_symbol: true,
        },
    ]
}

/// 单个策略参数的元数据(供 UI 渲染表单 / Agent 生成参数说明)。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParamMeta {
    /// 参数名(`params` JSON 的键)。
    pub name: &'static str,
    /// 参数类型:`"int"`(整数窗口/只数)或 `"number"`(浮点阈值)。
    pub ty: &'static str,
    /// 文档口径默认值。
    pub default: serde_json::Value,
    /// 一句话说明(含取值约束)。
    pub description: &'static str,
}

/// 策略元数据:`kind` 区分单标的(`"single"`,走引擎)与多标的轮动
/// (`"rotation"`,走 [`min_corr_rotation::run_rotation`])。
#[derive(Debug, Clone, serde::Serialize)]
pub struct StrategyMeta {
    /// 稳定策略名(`build_strategy` / 命令层 `strategy` 入参)。
    pub name: &'static str,
    /// `"single"` 或 `"rotation"`。
    pub kind: &'static str,
    /// 一句话说明(含文档出处),与 [`list_strategies`] 一致。
    pub description: &'static str,
    /// 参数清单(顺序即 UI 展示顺序)。
    pub params: Vec<ParamMeta>,
}

/// 全部注册策略的参数元数据(与 [`list_strategies`] 一一对应)。
///
/// 默认值与 [`build_strategy`] 的构造口径保持一致;轮动策略的标的池
/// (`pool`)不是策略参数,由命令层另行接收。
pub fn strategy_meta() -> Vec<StrategyMeta> {
    vec![
        StrategyMeta {
            name: "zscore_mean_reversion",
            kind: "single",
            description: "S12 zscore 均值回归:sub=close−MA20 的 60 日 z 分数,≤−2 满仓买、≥+1 清仓(单标的)",
            params: vec![
                ParamMeta {
                    name: "ma_window",
                    ty: "int",
                    default: serde_json::json!(20),
                    description: "sub = close − MA(close, ma_window) 的均线窗口,>= 1",
                },
                ParamMeta {
                    name: "z_window",
                    ty: "int",
                    default: serde_json::json!(60),
                    description: "z 分数滚动统计窗口,>= 2(总体标准差)",
                },
                ParamMeta {
                    name: "entry_z",
                    ty: "number",
                    default: serde_json::json!(-2.0),
                    description: "入场阈值:z <= entry_z 满仓买入,必须为负且 < exit_z",
                },
                ParamMeta {
                    name: "exit_z",
                    ty: "number",
                    default: serde_json::json!(1.0),
                    description: "出场阈值:z >= exit_z 清仓,必须 > entry_z",
                },
            ],
        },
        StrategyMeta {
            name: "ma_cross",
            kind: "single",
            description: "双均线(2022/72):5/60 日 SMA 金叉全仓买、死叉清仓(单标的)",
            params: vec![
                ParamMeta {
                    name: "fast",
                    ty: "int",
                    default: serde_json::json!(5),
                    description: "快线窗口(交易日),>= 1 且 < slow",
                },
                ParamMeta {
                    name: "slow",
                    ty: "int",
                    default: serde_json::json!(60),
                    description: "慢线窗口(交易日),> fast",
                },
            ],
        },
        StrategyMeta {
            name: "formula_dsl",
            kind: "single",
            description: "AI 公式策略:受限条件树只读取当前及历史行情，支持价格、SMA、区间高低点与 RSI，禁止文件/网络/任意代码(单标的)",
            params: vec![],
        },
        StrategyMeta {
            name: "min_corr_etf_rotation",
            kind: "rotation",
            description: "最小相关 ETF 轮动:候选池两两相关矩阵,持有平均相关最低的 4 只等权,月度再平衡(多标的)",
            params: vec![
                ParamMeta {
                    name: "lookback",
                    ty: "int",
                    default: serde_json::json!(60),
                    description: "相关系数所用的日收益窗口长度,>= 2",
                },
                ParamMeta {
                    name: "hold_n",
                    ty: "int",
                    default: serde_json::json!(4),
                    description: "持有标的只数(等权),>= 1;超过池大小时持有全池",
                },
            ],
        },
    ]
}

/// 按名构造策略(全部用文档口径默认参数;自定义参数请直接调用各策略的
/// `try_new` / [`ma_cross`])。未知名字返回 [`ParamError::UnknownStrategy`]。
pub fn build_strategy(name: &str) -> Result<StrategyHandle, ParamError> {
    match name {
        "zscore_mean_reversion" => Ok(StrategyHandle::Single(Box::new(
            ZscoreMeanReversion::default_params(),
        ))),
        // 文档 2022/72 口径:5/60 金叉全仓、死叉清仓。
        "ma_cross" => Ok(StrategyHandle::Single(Box::new(ma_cross(5, 60)?))),
        "formula_dsl" => Err(ParamError::invalid(
            "formula_dsl",
            "公式策略必须提供 spec 参数，不能使用空白默认策略",
        )),
        "min_corr_etf_rotation" => Ok(StrategyHandle::Rotation(MinCorrRotation::default_params())),
        other => Err(ParamError::UnknownStrategy(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{Bar, PriceSeries};
    use crate::engine::{BacktestEngine, EngineConfig};
    use chrono::NaiveDate;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn ma_cross_validates_params() {
        assert!(ma_cross(0, 60).is_err());
        assert!(ma_cross(60, 60).is_err());
        assert!(ma_cross(60, 5).is_err());
        assert!(ma_cross(5, 60).is_ok());
    }

    #[test]
    fn registry_roundtrip() {
        let infos = list_strategies();
        assert_eq!(infos.len(), 4);
        for info in &infos {
            if info.name == "formula_dsl" {
                continue;
            }
            let handle = build_strategy(info.name).unwrap();
            assert_eq!(handle.name(), info.name);
        }
        assert!(matches!(
            build_strategy("no_such_strategy"),
            Err(ParamError::UnknownStrategy(_))
        ));
    }

    #[test]
    fn strategy_meta_matches_registry() {
        let metas = strategy_meta();
        let infos = list_strategies();
        assert_eq!(metas.len(), infos.len());
        for (meta, info) in metas.iter().zip(&infos) {
            assert_eq!(meta.name, info.name);
            assert_eq!(meta.description, info.description);
            let expected_kind = if info.multi_symbol {
                "rotation"
            } else {
                "single"
            };
            assert_eq!(meta.kind, expected_kind, "{}", meta.name);
        }
        // 序列化为契约形状:{name, kind, description, params:[{name, ty, default, description}]}。
        let json = serde_json::to_value(&metas).unwrap();
        let ma = &json[1];
        assert_eq!(ma["name"], "ma_cross");
        assert_eq!(ma["kind"], "single");
        assert_eq!(ma["params"][0]["name"], "fast");
        assert_eq!(ma["params"][0]["ty"], "int");
        assert_eq!(ma["params"][0]["default"], 5);
        let formula = &json[2];
        assert_eq!(formula["name"], "formula_dsl");
        assert_eq!(formula["params"].as_array().unwrap().len(), 0);
        let rot = &json[3];
        assert_eq!(rot["kind"], "rotation");
        assert_eq!(rot["params"][1]["default"], 4);
    }

    /// 冒烟(双均线 5/60):先阴跌 100 根(快线压在慢线下)再单边上涨
    /// 150 根。金叉后全仓持有到末尾 → 总收益为正、Sharpe 为正、回撤
    /// 在 [0,1) 内。同时验证先跌段不会误开空仓单。
    #[test]
    fn smoke_ma_cross_rides_uptrend() {
        let mut closes: Vec<f64> = (0..100).map(|i| 10.0 - 0.05 * i as f64).collect();
        closes.extend((1..=150).map(|k| 5.0 + 0.1 * k as f64));
        let bars: Vec<Bar> = closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Bar::flat(d("2024-01-02") + chrono::Duration::days(i as i64), c))
            .collect();
        let series = PriceSeries::new("600519", bars).unwrap();
        let engine = BacktestEngine::new(
            astock_trading_rules::RuleSet::load(None).unwrap(),
            EngineConfig::new("600519", 100_000.0),
        )
        .unwrap();

        let mut strat = match build_strategy("ma_cross").unwrap() {
            StrategyHandle::Single(s) => s,
            _ => panic!("ma_cross must be single-symbol"),
        };
        let res = engine.run(&series, strat.as_mut()).unwrap();
        let report = res
            .performance_report(None, &crate::metrics::MetricsConfig::default())
            .unwrap();

        assert!(!res.trades.is_empty(), "golden cross must trigger a buy");
        assert!(
            res.trades
                .iter()
                .all(|t| t.side == astock_trading_rules::TradeSide::Buy),
            "no exit expected on a monotone post-cross rise: {:?}",
            res.trades
        );
        assert!(
            report.total_return > 0.0,
            "total_return {}",
            report.total_return
        );
        assert!(report.cagr > 0.0, "cagr {}", report.cagr);
        assert!(report.sharpe > 0.0, "sharpe {}", report.sharpe);
        assert!(
            (0.0..1.0).contains(&report.max_drawdown),
            "max_drawdown {}",
            report.max_drawdown
        );
    }
}
