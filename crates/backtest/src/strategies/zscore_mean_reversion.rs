//! S12 zscore 均值回归(来源:聚宽 2020/12,见 `docs/strategy-library-research.md` §S12/T4)。
//!
//! 口径(文档 T4 伪代码):
//!
//! ```text
//! sub = close − MA(close, ma_window)            // 文档默认 ma_window = 20
//! z   = (sub_last − mean(sub[-z_window:])) / std(sub[-z_window:])   // 默认 60
//! z ≤ entry_z → 满仓买入;z ≥ exit_z → 清仓       // 文档默认 −2 / +1
//! ```
//!
//! 与文档口径的偏差(显式声明):
//!
//! - **std 用总体标准差(除以 N)**,与本 crate `metrics` 的约定一致;原文
//!   pandas `std()` 默认 ddof=1(除以 N−1)。窗口 60 时两者差异约 0.8%,
//!   不改变信号性质,但逐点数值不可直接对齐。
//! - 任务书提到「±2σ 进出」,文档 S12 原文口径为「−2 买 / +1 卖」(非对称,
//!   均值回归做多逻辑:跌到极端便宜买,回到偏贵即卖)。两个阈值均为显式
//!   参数,默认取文档口径 −2.0 / +1.0;需要 ±2 时构造时传入即可。
//! - 原文标题「胜率100%」为单票(601238)参数拟合的标题党,文档 §4.2 已
//!   标注;本实现不继承任何绩效声明。

use crate::strategy::{Order, Qty, Strategy, StrategyContext};

use super::ParamError;

/// zscore 均值回归策略(单标的,满仓进出)。
#[derive(Debug, Clone)]
pub struct ZscoreMeanReversion {
    /// `sub = close − MA(close, ma_window)` 的均线窗口(文档默认 20)。
    pub ma_window: usize,
    /// z 分数的滚动统计窗口(文档默认 60)。
    pub z_window: usize,
    /// 入场阈值:z ≤ entry_z 时买入(文档默认 −2.0,必须为负)。
    pub entry_z: f64,
    /// 出场阈值:z ≥ exit_z 时清仓(文档默认 +1.0,必须大于 entry_z)。
    pub exit_z: f64,
}

impl ZscoreMeanReversion {
    /// 文档口径默认参数:20 / 60 / −2.0 / +1.0。
    pub fn default_params() -> Self {
        ZscoreMeanReversion {
            ma_window: 20,
            z_window: 60,
            entry_z: -2.0,
            exit_z: 1.0,
        }
    }

    /// 参数校验后构造。
    pub fn try_new(
        ma_window: usize,
        z_window: usize,
        entry_z: f64,
        exit_z: f64,
    ) -> Result<Self, ParamError> {
        let bad = |detail: &str| ParamError::invalid("zscore_mean_reversion", detail);
        if ma_window < 1 {
            return Err(bad("ma_window must be >= 1"));
        }
        if z_window < 2 {
            return Err(bad(
                "z_window must be >= 2 (std is undefined for one sample)",
            ));
        }
        if !entry_z.is_finite() || !exit_z.is_finite() {
            return Err(bad("entry_z / exit_z must be finite"));
        }
        if entry_z >= exit_z {
            return Err(bad("require entry_z < exit_z"));
        }
        Ok(ZscoreMeanReversion {
            ma_window,
            z_window,
            entry_z,
            exit_z,
        })
    }

    /// 当前 bar 的 z 分数;历史不足或标准差为 0(无波动)时返回 `None`。
    ///
    /// 只读 `ctx.bars()`(0..=当前 bar),结构上无前视。
    fn zscore(&self, ctx: &StrategyContext) -> Option<f64> {
        let n = ctx.len();
        // 需要 z_window 个 sub 值,最早的 sub 需要 ma_window 根 bar。
        if n < self.ma_window + self.z_window - 1 {
            return None;
        }
        let start = n - self.z_window;
        let mut subs = Vec::with_capacity(self.z_window);
        for i in start..n {
            let ma = ctx.bars()[i + 1 - self.ma_window..=i]
                .iter()
                .map(|b| b.close)
                .sum::<f64>()
                / self.ma_window as f64;
            subs.push(ctx.bars()[i].close - ma);
        }
        let mean = subs.iter().sum::<f64>() / self.z_window as f64;
        // 总体标准差(除以 N),与 metrics 模块约定一致 —— 见模块文档偏差说明。
        let var = subs.iter().map(|s| (s - mean) * (s - mean)).sum::<f64>() / self.z_window as f64;
        let std = var.sqrt();
        if std <= 0.0 {
            return None;
        }
        Some((subs[self.z_window - 1] - mean) / std)
    }
}

impl Strategy for ZscoreMeanReversion {
    fn name(&self) -> &str {
        "zscore_mean_reversion"
    }

    fn on_bar(&mut self, ctx: &StrategyContext, _bar_index: usize) -> Vec<Order> {
        let Some(z) = self.zscore(ctx) else {
            return vec![];
        };
        let pos = ctx.position();
        if z <= self.entry_z && pos.shares == 0 {
            vec![Order::buy(Qty::Max).with_reason(format!("z {z:.3} <= entry {}", self.entry_z))]
        } else if z >= self.exit_z && pos.shares > 0 {
            vec![Order::sell(Qty::Max).with_reason(format!("z {z:.3} >= exit {}", self.exit_z))]
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{Bar, PriceSeries};
    use crate::engine::{BacktestEngine, EngineConfig};
    use crate::strategy::PositionSnapshot;
    use astock_trading_rules::{RuleSet, TradeSide};
    use chrono::NaiveDate;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn bars(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Bar::flat(d("2025-01-06") + chrono::Duration::days(i as i64), c))
            .collect()
    }

    #[test]
    fn rejects_invalid_params() {
        assert!(ZscoreMeanReversion::try_new(0, 60, -2.0, 1.0).is_err());
        assert!(ZscoreMeanReversion::try_new(20, 1, -2.0, 1.0).is_err());
        assert!(ZscoreMeanReversion::try_new(20, 60, 1.0, -2.0).is_err());
        assert!(ZscoreMeanReversion::try_new(20, 60, f64::NAN, 1.0).is_err());
        assert!(ZscoreMeanReversion::try_new(20, 60, -2.0, 1.0).is_ok());
    }

    /// 手算 golden:ma_window=2, z_window=3, closes = [10, 10, 9, 7, 10, 10]。
    ///
    /// sub 序列(sub_i = close_i − MA2_i,自 i=1 起):
    ///   sub_1 = 10 − (10+10)/2 = 0
    ///   sub_2 = 9  − (10+9)/2  = −0.5
    ///   sub_3 = 7  − (9+7)/2   = −1.0
    ///   sub_4 = 10 − (7+10)/2  = +1.5
    ///   sub_5 = 10 − (10+10)/2 = 0
    ///
    /// 首个可计算 z 的 bar 是 t=3(需要 ma_window+z_window−1 = 4 根 bar):
    ///   窗口 sub[1..=3] = [0, −0.5, −1.0],mean = −0.5,
    ///   总体 std = sqrt((0.25 + 0 + 0.25)/3) = sqrt(1/6) ≈ 0.40824829
    ///   z_3 = (−1.0 − (−0.5)) / 0.40824829 ≈ −1.22474487
    /// t=4:窗口 sub[2..=4] = [−0.5, −1.0, 1.5],mean = 0,
    ///   std = sqrt((0.25 + 1 + 2.25)/3) = sqrt(3.5/3) ≈ 1.08012345
    ///   z_4 = 1.5 / 1.08012345 ≈ +1.38873015
    ///
    /// 取 entry_z = −1.0、exit_z = +1.0:
    ///   bar 3 触发买入信号(z ≈ −1.225 ≤ −1),bar 4 触发卖出信号(z ≈ 1.389 ≥ 1)。
    #[test]
    fn golden_hand_computed_z_values_and_signals() {
        let b = bars(&[10.0, 10.0, 9.0, 7.0, 10.0, 10.0]);
        let strat = ZscoreMeanReversion::try_new(2, 3, -1.0, 1.0).unwrap();

        // 逐 bar 检查 z 分数数值。
        let expected_z: [(usize, Option<f64>); 6] = [
            (0, None),
            (1, None),
            (2, None),
            (3, Some(-1.224744871391589)),
            (4, Some(1.3887301496588271)),
            // t=5: sub[3..=5] = [−1.0, 1.5, 0], mean = 1/6,
            // std = sqrt(((−7/6)² + (4/3)² + (−1/6)²)/3) = sqrt((49/36+16/9+1/36)/3)
            //     = sqrt((114/36)/3) = sqrt(19/18) ≈ 1.02740233
            // z = (0 − 1/6) / 1.02740233 ≈ −0.16222142
            (5, Some(-0.16222142113076254)),
        ];
        for (t, want) in expected_z {
            let ctx = StrategyContext::new(&b[..=t], t, PositionSnapshot::default());
            match (strat.zscore(&ctx), want) {
                (None, None) => {}
                (Some(got), Some(w)) => assert!((got - w).abs() < 1e-12, "t={t}: {got} vs {w}"),
                (got, want) => panic!("t={t}: got {got:?}, want {want:?}"),
            }
        }

        // 信号层面:空仓时 bar 3 给出买单;持仓时 bar 4 给出卖单。
        let mut strat = strat;
        let ctx3 = StrategyContext::new(&b[..=3], 3, PositionSnapshot::default());
        let orders = strat.on_bar(&ctx3, 3);
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].side, crate::strategy::Side::Buy);

        let held = PositionSnapshot {
            shares: 100,
            sellable: 100,
            avg_cost: 7.0,
            cash: 0.0,
        };
        let ctx4 = StrategyContext::new(&b[..=4], 4, held);
        let orders = strat.on_bar(&ctx4, 4);
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].side, crate::strategy::Side::Sell);
    }

    /// 引擎级 golden:完整回测,逐笔核对成交。
    ///
    /// 序列(ma_window=2, z_window=3, entry −1.0, exit +1.0;ma_window=2 时
    /// sub_i = (close_i − close_{i−1})/2),closes = [10, 10, 10.3, 9.4, 9.8,
    /// 10.3, 11.3, 12.0](日间最大涨幅 9.71%,均在 10% 涨跌停内):
    ///
    /// | t | close | sub_t | 窗口 z(总体 std)| 信号 |
    /// |---|-------|-------|----------------|------|
    /// | 3 | 9.4   | −0.45 | [0,.15,−.45]: mean=−0.1, std=√0.065≈0.254951, z=−0.35/0.254951≈−1.372818 | 买 |
    /// | 4 | 9.8   | +0.20 | [.15,−.45,.2]: mean=−1/30, std≈0.295334, z≈+0.790069 | — |
    /// | 5 | 10.3  | +0.25 | [−.45,.2,.25]: mean=0, std≈0.318852, z≈+0.784060 | — |
    /// | 6 | 11.3  | +0.50 | [.2,.25,.5]: mean=0.95/3, std≈0.131232, z=(0.5−0.316667)/0.131232≈+1.397021 | 卖 |
    /// | 7 | 12.0  | +0.35 | [.25,.5,.35]: mean=1.1/3, std≈0.102742, z≈−0.162221 | — |
    ///
    /// 执行(默认 NextOpen):
    /// - t=3 买单 → t=4 开盘成交。open_4 = 9.8(pre_close 9.4,涨停 10.34 不挡)。
    ///   可负担 floor(100_000/9.8) = 10_204 → 整手 10_200 股;金额 99_960;
    ///   佣金 99_960×0.00025 = 24.99 + 过户 0.9996 = 25.9896;合计
    ///   99_985.9896 ≤ 100_000,不缩手;余现金 14.0104。
    /// - t=6 卖单 → t=7 开盘成交。open_7 = 12.0(pre_close 11.3,跌停 10.17 不挡)。
    ///   金额 10_200×12 = 122_400;佣金 30.60 + 印花税 61.20 + 过户 1.224
    ///   = 93.024;现金 = 14.0104 + 122_400 − 93.024 = 122_320.9864。
    #[test]
    fn golden_engine_run_fills() {
        let mk = |t: usize, open: f64, close: f64| {
            Bar::new(
                d("2025-01-06") + chrono::Duration::days(t as i64),
                open,
                close.max(open),
                close.min(open),
                close,
                1e6,
                None,
                false,
            )
        };
        let closes = [10.0, 10.0, 10.3, 9.4, 9.8, 10.3, 11.3, 12.0];
        let opens = [10.0, 10.0, 10.3, 9.4, 9.8, 10.3, 11.3, 12.0];
        let bars: Vec<Bar> = (0..8).map(|t| mk(t, opens[t], closes[t])).collect();
        let series = PriceSeries::new("600519", bars).unwrap();
        let engine = BacktestEngine::new(
            RuleSet::load(None).unwrap(),
            EngineConfig::new("600519", 100_000.0),
        )
        .unwrap();
        let mut strat = ZscoreMeanReversion::try_new(2, 3, -1.0, 1.0).unwrap();
        let res = engine.run(&series, &mut strat).unwrap();

        let approx = |a: f64, b: f64| (a - b).abs() < 1e-6;
        assert_eq!(res.trades.len(), 2, "trades: {:?}", res.trades);

        let buy = &res.trades[0];
        assert_eq!(buy.side, TradeSide::Buy);
        assert_eq!(buy.date, d("2025-01-10")); // t = 4
        assert_eq!(buy.shares, 10_200);
        assert!(approx(buy.price, 9.8));
        assert!(approx(buy.amount, 99_960.0));
        assert!(approx(buy.fees.total, 25.9896));
        assert!(approx(buy.cash_after, 14.0104));

        let sell = &res.trades[1];
        assert_eq!(sell.side, TradeSide::Sell);
        assert_eq!(sell.date, d("2025-01-13")); // t = 7
        assert_eq!(sell.shares, 10_200);
        assert!(approx(sell.price, 12.0));
        assert!(approx(sell.amount, 122_400.0));
        assert!(approx(sell.fees.total, 93.024));
        assert!(approx(sell.cash_after, 122_320.986_4));

        assert!(res.rejections.is_empty());
        assert!(approx(res.final_equity(), 122_320.986_4));
    }

    /// 冒烟(文档默认参数 20/60/−2/+1):合成「平台 + 周期性 V 型超跌
    /// 反弹」序列 —— 价格长期在 10 走平(sub ≈ 0),每 35 根出现一次
    /// 急跌 9.2 → 9.5 → 9.8 → 10.3(超调) → 10.1 → 10.0 的均值回归
    /// 剧本。急跌日 z ≈ −7.7(触发买入),超调日 z ≈ +3(触发卖出)。
    /// 低买高卖应当盈利:总收益/Sharpe 为正、回撤在 [0,1) 内。
    #[test]
    fn smoke_mean_reverting_series_is_profitable() {
        let mut closes = vec![10.0; 300];
        for k in 0..6 {
            let b = 90 + 35 * k;
            closes[b] = 9.2; // 急跌(−8%,未触跌停)
            closes[b + 1] = 9.5;
            closes[b + 2] = 9.8;
            closes[b + 3] = 10.3; // 超调反弹(+5.1%,未触涨停)
            closes[b + 4] = 10.1;
            // b+5 回到 10.0
        }
        let series = PriceSeries::new("600519", bars(&closes)).unwrap();
        let engine = BacktestEngine::new(
            RuleSet::load(None).unwrap(),
            EngineConfig::new("600519", 100_000.0),
        )
        .unwrap();
        let mut strat = ZscoreMeanReversion::default_params();
        let res = engine.run(&series, &mut strat).unwrap();
        let report = res
            .performance_report(None, &crate::metrics::MetricsConfig::default())
            .unwrap();

        assert!(
            res.round_trips().len() >= 5,
            "expect ~6 buy-low/sell-high cycles, got trades: {:?}",
            res.trades
        );
        assert!(
            report.total_return > 0.0,
            "total_return {} should be positive",
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
