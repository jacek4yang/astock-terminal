//! 最小相关 ETF 轮动(任务书首批第 3 个;参考 `docs/strategy-library-research.md` §S2 的
//! 「最小相关 ETF 轮动」思路)。
//!
//! # 口径(按任务书)
//!
//! ```text
//! 候选池内,用最近 lookback 个日收益算两两 Pearson 相关矩阵;
//! 对每只候选算「与其他候选的平均相关系数」,升序取最低的前 N 只持有;
//! 月度再平衡:每月首个共同交易日收盘后产生信号,次一交易日开盘调仓。
//! ```
//!
//! # 与文档 S2 口径的偏差(显式声明)
//!
//! - 文档 S2(2025/26)是「729 日波动率过滤 ∈ [5%,33%] → 取平均相关最小的
//!   4 只候选 → 再按 25 日回归年化收益 × R² 买第 1 名,单标的满仓」。
//!   任务书指定的首批口径更简单(不做波动率过滤、不动量打分、持有 N 只
//!   等权),本实现按任务书;S2 完整版留待第二批,动量评分可复用 T3 公式。
//! - 默认 lookback = 60(文档 S2 用 729;729 在多数本地数据上冷启动过长,
//!   60 是日频散户级的常用折中,参数显式可调)。
//! - 调仓只在再平衡日发生;持仓权重在月内随价格漂移,不做月内再平衡。
//!
//! # 多标的工程方案(任务书要求文档化)
//!
//! 现有 [`crate::engine::BacktestEngine`] 只支持单标的(组合状态是单个
//! `Portfolio`,上下文只暴露一条序列)。两个候选方案:
//!
//! - **A. 扩展引擎为多标的**:改动 `engine.rs` / `strategy.rs` 的组合状态、
//!   上下文与成交管线, blast radius 大,且这些文件正由其他代理并行维护。
//! - **B. 策略内自管多标的 + 本模块内置轻量轮动 runner**(本实现):
//!   [`run_rotation`] 复用 `astock-trading-rules` 的板块规则(整手、涨跌停)
//!   与费率表(`RuleSet::trade_cost_at`),语义对齐引擎默认的 `NextOpen`
//!   执行策略(信号日收盘决策、次日开盘成交),同样强制 T+1、停牌/涨跌停
//!   门控。所有代码自包含于本文件,不触碰共享引擎文件。
//!
//! 选 B,工程量显著小且隔离。后续若引擎原生支持多标的,本模块可整体退役,
//! 选股逻辑 [`select_min_corr`] 是纯函数,可直接搬走。
//!
//! # Runner 与引擎的语义差异(已知,可接受)
//!
//! - 卖出与买入在同一根 bar 的开盘价依次成交(先卖后买,卖出所得现金立
//!   即可用于买入);单标的引擎不存在此场景。
//! - 涨跌停/停牌导致某条腿无法成交时,该腿被跳过(持仓保持 / 现金留存),
//!   不像引擎那样记录 rejection 日志 —— 轮动场景下「买不到就不买」是更
//!   合理的默认,差异在此声明。
//! - 权益按当日收盘价逐日盯市(停牌 bar 用其携带的指示性收盘价,与
//!   `data::Bar` 的约定一致)。

use std::collections::{BTreeSet, HashMap};

use chrono::{Datelike, NaiveDate};
use thiserror::Error;

use astock_trading_rules::{BoardRules, RuleSet, TradeCost, TradeSide};

use crate::data::PriceSeries;

use super::ParamError;

/// 最小相关轮动参数(多标的,配合 [`run_rotation`] 使用)。
#[derive(Debug, Clone)]
pub struct MinCorrRotation {
    /// 相关系数所用的日收益窗口长度(默认 60;文档 S2 原文用 729)。
    pub lookback: usize,
    /// 持有的标的只数(默认 4,与文档 S2 的候选数一致)。
    pub hold_n: usize,
}

impl MinCorrRotation {
    /// 默认参数:60 日相关窗口,持有 4 只。
    pub fn default_params() -> Self {
        MinCorrRotation {
            lookback: 60,
            hold_n: 4,
        }
    }

    /// 参数校验后构造。
    pub fn try_new(lookback: usize, hold_n: usize) -> Result<Self, ParamError> {
        let bad = |detail: &str| ParamError::invalid("min_corr_etf_rotation", detail);
        if lookback < 2 {
            return Err(bad("lookback must be >= 2 (need at least 2 returns)"));
        }
        if hold_n < 1 {
            return Err(bad("hold_n must be >= 1"));
        }
        Ok(MinCorrRotation { lookback, hold_n })
    }

    /// 注册表/报告用稳定名。
    pub fn name(&self) -> &'static str {
        "min_corr_etf_rotation"
    }
}

/// 两条等长收益序列的 Pearson 相关系数(总体口径,与 crate 约定一致)。
/// 任一序列零方差(常数序列)时返回 `None`。
fn pearson(x: &[f64], y: &[f64]) -> Option<f64> {
    let n = x.len();
    if n < 2 || y.len() != n {
        return None;
    }
    let (mx, my) = (
        x.iter().sum::<f64>() / n as f64,
        y.iter().sum::<f64>() / n as f64,
    );
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (&a, &b) in x.iter().zip(y) {
        let (da, db) = (a - mx, b - my);
        sxy += da * db;
        sxx += da * da;
        syy += db * db;
    }
    if sxx <= 0.0 || syy <= 0.0 {
        return None;
    }
    Some(sxy / (sxx.sqrt() * syy.sqrt()))
}

/// 每个候选对其他候选的平均相关系数(只对可计算的配对取平均)。
/// 零方差候选的所有配对都无法计算,记为 `f64::INFINITY`,排序时自然
/// 垫底 —— 常数价格序列没有分散价值;但配对另一方不受牵连。
pub fn average_pairwise_correlation(returns: &[Vec<f64>]) -> Vec<f64> {
    let n = returns.len();
    let mut out = vec![f64::INFINITY; n];
    for i in 0..n {
        let mut sum = 0.0;
        let mut cnt = 0usize;
        for (j, other) in returns.iter().enumerate() {
            if i == j {
                continue;
            }
            if let Some(c) = pearson(&returns[i], other) {
                sum += c;
                cnt += 1;
            }
        }
        if cnt > 0 {
            out[i] = sum / cnt as f64;
        }
    }
    out
}

/// 纯函数选股:按平均相关系数升序取前 `hold_n` 个(并列按下标升序,
/// 保证确定性),返回池内下标(升序)。零方差候选排在最后(见
/// [`average_pairwise_correlation`])。
pub fn select_min_corr(returns: &[Vec<f64>], hold_n: usize) -> Vec<usize> {
    let avg = average_pairwise_correlation(returns);
    let mut idx: Vec<usize> = (0..returns.len()).collect();
    idx.sort_by(|&a, &b| {
        avg[a]
            .partial_cmp(&avg[b])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    idx.truncate(hold_n.min(returns.len()));
    idx.sort_unstable();
    idx
}

/// 轮动 runner 配置。
#[derive(Debug, Clone)]
pub struct RotationConfig {
    /// 初始资金(CNY)。
    pub initial_cash: f64,
    /// 不利滑点(基点),买入加价、卖出减价,与引擎语义一致。
    pub slippage_bps: f64,
}

impl RotationConfig {
    /// 默认:无滑点。
    pub fn new(initial_cash: f64) -> Self {
        RotationConfig {
            initial_cash,
            slippage_bps: 0.0,
        }
    }
}

/// 一笔轮动成交。
#[derive(Debug, Clone, PartialEq)]
pub struct RotationTrade {
    /// 成交日期。
    pub date: NaiveDate,
    /// 标的代码。
    pub symbol: String,
    /// 方向。
    pub side: TradeSide,
    /// 成交股数。
    pub shares: u32,
    /// 成交价(滑点调整后)。
    pub price: f64,
    /// 成交金额(`price * shares`)。
    pub amount: f64,
    /// 费用明细(与引擎同一费率表)。
    pub fees: TradeCost,
    /// 调仓原因(可审计)。
    pub reason: String,
}

/// 每日收盘后的组合快照。
#[derive(Debug, Clone, PartialEq)]
pub struct RotationEquityPoint {
    /// 日期。
    pub date: NaiveDate,
    /// 现金。
    pub cash: f64,
    /// 持仓市值。
    pub market_value: f64,
    /// `cash + market_value`。
    pub equity: f64,
    /// 当日持仓的标的代码(有序,便于 diff)。
    pub holdings: Vec<String>,
}

/// 轮动回测输出。
#[derive(Debug, Clone, Default)]
pub struct RotationResult {
    /// 逐日权益曲线(共同交易日)。
    pub equity: Vec<RotationEquityPoint>,
    /// 全部成交。
    pub trades: Vec<RotationTrade>,
}

impl RotationResult {
    /// 期末权益(空曲线为 0)。
    pub fn final_equity(&self) -> f64 {
        self.equity.last().map(|p| p.equity).unwrap_or(0.0)
    }

    /// 纯权益数值序列,供 `metrics` 模块计算指标。
    pub fn equity_curve(&self) -> Vec<f64> {
        self.equity.iter().map(|p| p.equity).collect()
    }
}

/// 轮动 runner 的错误。
#[derive(Debug, Error)]
pub enum RotationError {
    /// 候选池少于 2 条序列,无从谈相关性。
    #[error("rotation pool must contain at least 2 series, got {0}")]
    PoolTooSmall(usize),
    /// 所有序列没有共同交易日。
    #[error("rotation pool has no common trading dates")]
    NoCommonDates,
    /// 初始资金非正。
    #[error("initial cash must be positive, got {0}")]
    NonPositiveCash(f64),
    /// 板块规则解析失败(注意:当前 trading-rules 只覆盖股票代码前缀,
    /// 真实 ETF 代码如 510500/159915 需先补基金板块规则)。
    #[error(transparent)]
    Rules(#[from] astock_trading_rules::Error),
}

/// 单个持仓的可变状态。
#[derive(Debug, Default, Clone, Copy)]
struct Holding {
    shares: u32,
    sellable: u32,
    cost: f64,
}

/// 多标的等权最小相关轮动回测。语义见模块文档「多标的工程方案」。
///
/// 信号在每月首个共同交易日收盘后产生(要求该 bar 已有 `lookback` 个
/// 日收益),于次一共同交易日开盘执行:先卖出掉出目标集的持仓,再把
/// 可用现金等权买入新进入目标集的标的(整手取整,含费)。
pub fn run_rotation(
    pool: &[PriceSeries],
    strategy: &MinCorrRotation,
    rules: &RuleSet,
    config: &RotationConfig,
) -> Result<RotationResult, RotationError> {
    if pool.len() < 2 {
        return Err(RotationError::PoolTooSmall(pool.len()));
    }
    if config.initial_cash <= 0.0 {
        return Err(RotationError::NonPositiveCash(config.initial_cash));
    }
    let boards: Vec<BoardRules> = pool
        .iter()
        .map(|s| rules.for_symbol(&s.symbol))
        .collect::<Result<_, _>>()?;

    // 共同交易日历(交集,升序)。
    let mut common: BTreeSet<NaiveDate> = pool[0].bars.iter().map(|b| b.date).collect();
    for s in &pool[1..] {
        let dates: BTreeSet<NaiveDate> = s.bars.iter().map(|b| b.date).collect();
        common = common.intersection(&dates).copied().collect();
    }
    if common.is_empty() {
        return Err(RotationError::NoCommonDates);
    }
    let dates: Vec<NaiveDate> = common.into_iter().collect();
    let bars: Vec<HashMap<NaiveDate, &crate::data::Bar>> = pool
        .iter()
        .map(|s| s.bars.iter().map(|b| (b.date, b)).collect())
        .collect();

    let mut cash = config.initial_cash;
    let mut holdings: HashMap<usize, Holding> = HashMap::new();
    let mut result = RotationResult::default();
    let mut pending: Option<Vec<usize>> = None;
    let slip = config.slippage_bps / 10_000.0;

    for t in 0..dates.len() {
        // T+1:新的一天,既有持仓全部变为可卖。
        for h in holdings.values_mut() {
            h.sellable = h.shares;
        }

        // 上一信号日的调仓在今日开盘执行。
        if let Some(target) = pending.take() {
            debug_assert!(t >= 1, "pending orders imply a prior signal bar");
            // 1) 先卖出掉出目标集的持仓(得现金)。
            let held: Vec<usize> = holdings.keys().copied().collect();
            for i in held {
                if target.contains(&i) {
                    continue;
                }
                let bar = bars[i][&dates[t]];
                let pre_close = bars[i][&dates[t - 1]].close;
                let limit_down = boards[i].limit_down_price(pre_close, false);
                // 停牌 / 跌停封死:这条腿跳过,持仓保留(模块文档已声明)。
                if bar.suspended || bar.open <= limit_down {
                    continue;
                }
                let h = holdings.get(&i).copied().unwrap_or_default();
                let shares = h.sellable.min(h.shares);
                if shares == 0 {
                    continue;
                }
                let price = bar.open * (1.0 - slip);
                let amount = shares as f64 * price;
                let fees =
                    rules.trade_cost_at(TradeSide::Sell, amount, Some(&boards[i].market), bar.date);
                cash += amount - fees.total;
                if shares == h.shares {
                    holdings.remove(&i);
                } else if let Some(h) = holdings.get_mut(&i) {
                    h.shares -= shares;
                    h.sellable -= shares;
                    h.cost *= h.shares as f64 / (h.shares + shares) as f64;
                }
                result.trades.push(RotationTrade {
                    date: bar.date,
                    symbol: pool[i].symbol.clone(),
                    side: TradeSide::Sell,
                    shares,
                    price,
                    amount,
                    fees,
                    reason: "dropped from min-correlation target set".to_string(),
                });
            }

            // 2) 现金等权买入新进入目标集的标的。
            let to_buy: Vec<usize> = target
                .iter()
                .copied()
                .filter(|i| !holdings.contains_key(i))
                .collect();
            if !to_buy.is_empty() {
                let per = cash / to_buy.len() as f64;
                for i in to_buy {
                    let bar = bars[i][&dates[t]];
                    let pre_close = bars[i][&dates[t - 1]].close;
                    let limit_up = boards[i].limit_up_price(pre_close, false);
                    if bar.suspended || bar.open >= limit_up {
                        continue; // 买不到就留现金(模块文档已声明)
                    }
                    let price = bar.open * (1.0 + slip);
                    let (min, step) = (boards[i].min_lot, boards[i].lot_step);
                    let round = |q: u32| {
                        if q < min {
                            0
                        } else {
                            min + (q - min) / step * step
                        }
                    };
                    let budget = per.min(cash);
                    let mut shares = round((budget / price).floor().max(0.0) as u32);
                    // 与引擎一致的「缩手直至现金覆盖金额+费」逻辑。
                    let fitted = loop {
                        if shares < min {
                            break None;
                        }
                        let amount = shares as f64 * price;
                        let fees = rules.trade_cost_at(
                            TradeSide::Buy,
                            amount,
                            Some(&boards[i].market),
                            bar.date,
                        );
                        if amount + fees.total <= budget + 1e-9 {
                            break Some((shares, amount, fees));
                        }
                        if shares < min + step {
                            break None;
                        }
                        shares -= step;
                    };
                    let Some((shares, amount, fees)) = fitted else {
                        continue;
                    };
                    cash -= amount + fees.total;
                    holdings.entry(i).or_default().shares += shares;
                    if let Some(h) = holdings.get_mut(&i) {
                        h.cost += amount;
                        // T+1:今日买入不增加 sellable(or_default 初值 0 已体现)。
                    }
                    result.trades.push(RotationTrade {
                        date: bar.date,
                        symbol: pool[i].symbol.clone(),
                        side: TradeSide::Buy,
                        shares,
                        price,
                        amount,
                        fees,
                        reason: "entered min-correlation target set".to_string(),
                    });
                }
            }
        }

        // 收盘后产生下一再平衡信号:每月首个共同交易日,且已有足够历史。
        if t + 1 < dates.len()
            && t >= strategy.lookback
            && (dates[t].year(), dates[t].month()) != (dates[t - 1].year(), dates[t - 1].month())
        {
            let returns: Vec<Vec<f64>> = bars
                .iter()
                .map(|m| {
                    (t + 1 - strategy.lookback..=t)
                        .map(|i| m[&dates[i]].close / m[&dates[i - 1]].close - 1.0)
                        .collect()
                })
                .collect();
            pending = Some(select_min_corr(&returns, strategy.hold_n));
        }

        // 逐日盯市。
        let mut market_value = 0.0;
        for (i, h) in &holdings {
            market_value += h.shares as f64 * bars[*i][&dates[t]].close;
        }
        let mut held_symbols: Vec<String> =
            holdings.keys().map(|&i| pool[i].symbol.clone()).collect();
        held_symbols.sort();
        result.equity.push(RotationEquityPoint {
            date: dates[t],
            cash,
            market_value,
            equity: cash + market_value,
            holdings: held_symbols,
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Bar;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn series(symbol: &str, start: NaiveDate, closes: &[f64]) -> PriceSeries {
        PriceSeries::new(
            symbol,
            closes
                .iter()
                .enumerate()
                .map(|(i, &c)| Bar::flat(start + chrono::Duration::days(i as i64), c))
                .collect(),
        )
        .unwrap()
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn rejects_invalid_params() {
        assert!(MinCorrRotation::try_new(1, 4).is_err());
        assert!(MinCorrRotation::try_new(60, 0).is_err());
        assert!(MinCorrRotation::try_new(60, 4).is_ok());
    }

    /// 手算 golden(选股):
    /// A = [1,-1,1,-1],B = A,C = −A(均值均为 0)。
    /// corr(A,B) = 1,corr(A,C) = corr(B,C) = −1。
    /// 平均相关:A = 0,B = 0,C = −1 → hold_n=1 选 C(下标 2);
    /// hold_n=2 时 A/B 并列,按下标升序取 A(0) → [0, 2]。
    #[test]
    fn golden_selection_by_average_correlation() {
        let a = vec![1.0, -1.0, 1.0, -1.0];
        let b = a.clone();
        let c = vec![-1.0, 1.0, -1.0, 1.0];
        let rets = vec![a, b, c];

        let avg = average_pairwise_correlation(&rets);
        assert!(approx(avg[0], 0.0));
        assert!(approx(avg[1], 0.0));
        assert!(approx(avg[2], -1.0));

        assert_eq!(select_min_corr(&rets, 1), vec![2]);
        assert_eq!(select_min_corr(&rets, 2), vec![0, 2]);
        // hold_n 超过池大小时取全池。
        assert_eq!(select_min_corr(&rets, 9), vec![0, 1, 2]);
    }

    /// 零方差(常数收益)序列无法计算相关系数,排名垫底。
    #[test]
    fn zero_variance_series_ranks_last() {
        let a = vec![1.0, -1.0, 1.0, -1.0];
        let flat = vec![0.01, 0.01, 0.01, 0.01];
        let c = vec![-1.0, 1.0, -1.0, 1.0];
        let rets = vec![a, flat, c];
        let avg = average_pairwise_correlation(&rets);
        assert!(avg[1].is_infinite());
        assert_eq!(select_min_corr(&rets, 2), vec![0, 2]);
    }

    /// 手算 golden(完整轮动):
    ///
    /// 池:A="600500"、B="600300"、C="600100"(均 SH 主板,10% 涨跌停,
    /// 100 股整手,佣金 0.025% 最低 5,印花 0.05% 卖方,过户 0.001%)。
    /// 参数:lookback=4,hold_n=1,initial_cash=100_000。
    ///
    /// 共同交易日(t):01-02, 01-03, 01-06, 01-07, 01-08, 02-03, 02-04, 02-05
    /// (t=0..7)。收益窗口在信号日 t=5 覆盖 i=2..=5:
    ///   A 收益 [+5%, −5%, +5%, −5%] ∝ [1,-1,1,-1]
    ///   B 同 A;C 收益相反 ∝ [-1,1,-1,1]
    ///   → 平均相关 A=0, B=0, C=−1 → 选中 C(下标 2)。
    ///
    /// 月份在 t=5(01-08 → 02-03)切换,首个信号于 t=5 收盘产生,
    /// t=6(02-04)开盘执行:C 开盘 10.00(pre_close 9.9500625,涨停
    /// 10.945 不挡)。
    ///
    /// 买入手算:预算 100_000,10_000×10 + 费 > 预算 → 缩至 9_900 股,
    /// 金额 99_000,佣金 24.75 + 过户 0.99 = 25.74,现金余 974.26。
    /// 权益:t=6 收盘 10.00 → 974.26 + 99_000 = 99_974.26;
    ///       t=7 收盘 11.00 → 974.26 + 108_900 = 109_874.26。
    #[test]
    fn golden_rotation_run_hand_computed() {
        let dates: Vec<NaiveDate> = [
            "2025-01-02",
            "2025-01-03",
            "2025-01-06",
            "2025-01-07",
            "2025-01-08",
            "2025-02-03",
            "2025-02-04",
            "2025-02-05",
        ]
        .iter()
        .map(|s| d(s))
        .collect();
        let mk = |symbol: &str, closes: &[f64]| {
            PriceSeries::new(
                symbol,
                closes
                    .iter()
                    .zip(&dates)
                    .map(|(&c, &dt)| Bar::flat(dt, c))
                    .collect(),
            )
            .unwrap()
        };

        // closes[t=0..7];t=6/7 的 A/B 取值不影响断言(C 被持有)。
        let a = mk(
            "600500",
            &[10.0, 10.0, 10.5, 9.975, 10.47375, 9.9500625, 10.0, 10.0],
        );
        let b = mk(
            "600300",
            &[10.0, 10.0, 10.5, 9.975, 10.47375, 9.9500625, 10.0, 10.0],
        );
        let c = mk(
            "600100",
            &[10.0, 10.0, 9.5, 9.975, 9.47625, 9.9500625, 10.0, 11.0],
        );
        let pool = vec![a, b, c];

        let rules = RuleSet::load(None).unwrap();
        let res = run_rotation(
            &pool,
            &MinCorrRotation::try_new(4, 1).unwrap(),
            &rules,
            &RotationConfig::new(100_000.0),
        )
        .unwrap();

        assert_eq!(res.trades.len(), 1, "trades: {:?}", res.trades);
        let buy = &res.trades[0];
        assert_eq!(buy.symbol, "600100");
        assert_eq!(buy.side, TradeSide::Buy);
        assert_eq!(buy.date, d("2025-02-04"));
        assert_eq!(buy.shares, 9_900);
        assert!(approx(buy.price, 10.0));
        assert!(approx(buy.amount, 99_000.0));
        assert!(approx(buy.fees.total, 25.74));

        assert_eq!(res.equity.len(), 8);
        assert!(approx(res.equity[6].equity, 99_974.26));
        assert!(approx(res.equity[7].equity, 109_874.26));
        assert_eq!(res.equity[7].holdings, vec!["600100".to_string()]);
        // 1 月内无交易(首个共同交易日之前无历史,且月内不重复调仓)。
        assert!(res.equity[..6].iter().all(|p| approx(p.equity, 100_000.0)));
    }

    #[test]
    fn rejects_degenerate_inputs() {
        let s = series("600500", d("2025-01-06"), &[10.0, 10.0]);
        let rules = RuleSet::load(None).unwrap();
        let strat = MinCorrRotation::default_params();
        assert!(matches!(
            run_rotation(
                std::slice::from_ref(&s),
                &strat,
                &rules,
                &RotationConfig::new(100_000.0)
            ),
            Err(RotationError::PoolTooSmall(1))
        ));
        let other = series("600300", d("2025-02-06"), &[10.0, 10.0]);
        assert!(matches!(
            run_rotation(&[s, other], &strat, &rules, &RotationConfig::new(100_000.0)),
            Err(RotationError::NoCommonDates)
        ));
    }

    /// 冒烟:三条整体上行、形态各异的合成序列,hold_n=2,月度再平衡。
    /// 任意持仓都赚钱 → 总收益为正、Sharpe 为正、回撤在 [0,1) 内。
    #[test]
    fn smoke_uptrending_pool_is_profitable() {
        let n = 400;
        let start = d("2024-01-02");
        let mk = |symbol: &str, f: &dyn Fn(usize) -> f64| {
            series(symbol, start, &(0..n).map(f).collect::<Vec<_>>())
        };
        let pool = vec![
            mk("600500", &|i| {
                10.0 * (1.0 + 0.001 * i as f64) + 0.3 * (i as f64 / 7.0).sin()
            }),
            mk("600300", &|i| {
                20.0 * (1.0 + 0.0008 * i as f64) + 0.5 * (i as f64 / 11.0).cos()
            }),
            mk("600100", &|i| {
                5.0 * (1.0 + 0.0012 * i as f64) + 0.2 * (i as f64 / 5.0).sin()
            }),
        ];
        let rules = RuleSet::load(None).unwrap();
        let res = run_rotation(
            &pool,
            &MinCorrRotation::try_new(20, 2).unwrap(),
            &rules,
            &RotationConfig::new(100_000.0),
        )
        .unwrap();

        let curve = res.equity_curve();
        let returns = crate::metrics::daily_returns(&curve);
        let cfg = crate::metrics::MetricsConfig::default();
        let total = crate::metrics::total_return(&curve);
        let sharpe = crate::metrics::sharpe(&returns, &cfg);
        let (max_dd, _) = crate::metrics::max_drawdown(&curve);

        assert!(!res.trades.is_empty(), "monthly rebalance must trade");
        assert!(total > 0.0, "total_return {total}");
        assert!(sharpe > 0.0, "sharpe {sharpe}");
        assert!((0.0..1.0).contains(&max_dd), "max_drawdown {max_dd}");
        assert!(res.final_equity() > 100_000.0);
    }
}
