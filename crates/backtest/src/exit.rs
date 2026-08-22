//! Optional exit-policy wiring point (默认关闭).
//!
//! Wraps any [`Strategy`] with the deterministic exit/risk rules from
//! [`astock_trading_rules::exit`] (ported from niuone's framework semantics):
//! a frozen structural stop that only moves up, R-multiple staged take
//! profit, and a peak-close `k*ATR` trailing stop (Wilder ATR).
//!
//! The wiring is a *decorator*, so the engine and every existing test are
//! untouched: strategies that are not wrapped see zero behavior change.
//! Opt in by wrapping:
//!
//! ```
//! use astock_backtest::exit::{ExitManaged, ExitPolicy, ExitPolicyConfig, InitialStop};
//! use astock_backtest::strategy::BuyHold;
//!
//! let policy = ExitPolicy::new(ExitPolicyConfig {
//!     initial_stop: InitialStop::AtrMultiple(2.0),
//!     atr_period: 14,
//!     scale_out: vec![(1.0, 0.45), (2.0, 0.35), (3.0, 0.20)],
//!     breakeven_after_partial: true,
//!     trailing_atr_mult: Some(2.0),
//!     trailing_entry_gate: true,
//! });
//! let strategy = ExitManaged::new(BuyHold, policy);
//! ```
//!
//! Semantics (all thresholds are explicit config, no magic numbers):
//!
//! - **Entry detection**: when the position snapshot flips from flat to
//!   non-flat, the policy freezes `entry = avg_cost` and the initial stop
//!   (per [`InitialStop`]). Position adds are not modelled: the frozen state
//!   belongs to the first entry.
//! - **Priority per bar** (niuone's order): structural stop (intraday low
//!   through the stop) → next R-multiple scale-out (intraday high through
//!   the target) → trailing stop (close at/below `peak_close - k*ATR`).
//!   At most one exit order per bar.
//! - **Exit orders replace the wrapped strategy's orders for that bar** —
//!   emitting both an exit sell and a strategy order on the same close would
//!   be mutually contradictory.
//! - Signals are close/high/low based, so under the default
//!   [`crate::engine::ExecutionPolicy::NextOpen`] they fill at the next
//!   bar's open: causal, no look-ahead.

use astock_trading_rules::exit::{
    Ohlc, PeakTrailingStop, RMultiple, ScaleOutStage, ScaledTakeProfit, StructuralStop, WilderAtr,
};

use crate::strategy::{Order, Qty, Strategy, StrategyContext};

/// How the frozen initial stop is derived at entry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InitialStop {
    /// `entry - k * ATR(period)`. If the ATR is not yet seeded when the
    /// position opens, falls back to the lowest low of the visible bars (the
    /// structural-low reading of the same idea).
    AtrMultiple(f64),
    /// Lowest low of the last `n` bars before the entry bar.
    RecentLow(usize),
    /// `entry * (1 - f)`, e.g. `0.05` for a fixed 5% stop.
    FixedFraction(f64),
}

/// Exit-policy configuration; every threshold is explicit.
#[derive(Debug, Clone, PartialEq)]
pub struct ExitPolicyConfig {
    /// Initial structural stop derivation.
    pub initial_stop: InitialStop,
    /// Wilder ATR period (e.g. 14).
    pub atr_period: usize,
    /// `(r_multiple, ratio_of_original_position)` stages, strictly ascending
    /// in R, ratios summing to at most 1 (e.g. `[(1.0, 0.45), (2.0, 0.35),
    /// (3.0, 0.20)]`). Empty disables staged take profit.
    pub scale_out: Vec<(f64, f64)>,
    /// Raise the structural stop to breakeven after the first scale-out leg
    /// fills (niuone's `break_even_after_partial`).
    pub breakeven_after_partial: bool,
    /// Trailing multiplier: exit when `close <= peak_close - k * ATR`.
    /// `None` disables the trailing stop.
    pub trailing_atr_mult: Option<f64>,
    /// Only let the trailing stop trigger while its level is above the entry
    /// price (below entry it would duplicate the structural stop).
    pub trailing_entry_gate: bool,
}

/// Per-position frozen state.
#[derive(Debug)]
struct OpenState {
    rm: RMultiple,
    entry_shares: u32,
    stop: StructuralStop,
    take_profit: Option<ScaledTakeProfit>,
    trailing: Option<PeakTrailingStop>,
}

/// Deterministic exit state machine. Feed one [`StrategyContext`] per bar
/// (via [`ExitPolicy::on_bar`] or the [`ExitManaged`] decorator).
#[derive(Debug)]
pub struct ExitPolicy {
    config: ExitPolicyConfig,
    atr: WilderAtr,
    open: Option<OpenState>,
}

impl ExitPolicy {
    /// Build a policy. Panics on invalid configuration (bad ATR period,
    /// malformed scale-out stages) — these are programmer errors, not
    /// runtime data problems.
    pub fn new(config: ExitPolicyConfig) -> Self {
        let stages: Vec<ScaleOutStage> = config
            .scale_out
            .iter()
            .map(|&(r, ratio)| ScaleOutStage { r, ratio })
            .collect();
        if !stages.is_empty() {
            ScaledTakeProfit::new(stages).expect("invalid scale_out stages");
        }
        if let InitialStop::AtrMultiple(k) = config.initial_stop {
            assert!(k.is_finite() && k > 0.0, "initial stop ATR multiple");
        }
        if let Some(k) = config.trailing_atr_mult {
            assert!(k.is_finite() && k > 0.0, "trailing ATR multiple");
        }
        ExitPolicy {
            atr: WilderAtr::new(config.atr_period),
            config,
            open: None,
        }
    }

    /// Advance one bar and return exit orders (at most one).
    pub fn on_bar(&mut self, ctx: &StrategyContext) -> Vec<Order> {
        let bar = ctx.last();
        let atr_now = self.atr.update(Ohlc::new(bar.high, bar.low, bar.close));
        let pos = ctx.position();

        if pos.shares == 0 {
            self.open = None;
            return vec![];
        }

        if self.open.is_none() {
            self.open = self.open_position(ctx, pos.avg_cost, pos.shares, atr_now);
            // The entry bar itself is never evaluated: under T+1 those
            // shares are not sellable today, and the frozen state deserves
            // one full bar before it can fire.
            return vec![];
        }
        let Some(state) = self.open.as_mut() else {
            return vec![];
        };

        // Trailing peak must track every held bar, including bars where an
        // earlier rule fires — update first, trigger below in priority order.
        let trail_hit = match (state.trailing.as_mut(), atr_now) {
            (Some(trail), Some(atr)) => trail.update(bar.close, atr),
            _ => None,
        };

        // 1. Structural stop (intraday).
        if let Some(fill_ref) = state.stop.check(bar.open, bar.low) {
            return vec![Order::sell(Qty::Max).with_reason(format!(
                "structural stop {:.4} hit (low {:.4}), fill ref {:.4}",
                state.stop.price(),
                bar.low,
                fill_ref
            ))];
        }

        // 2. R-multiple staged take profit (intraday high).
        if let Some(tp) = state.take_profit.as_mut() {
            if let Some(stage) = tp.on_price(&state.rm, bar.high) {
                let shares = ((state.entry_shares as f64 * stage.ratio).floor() as u32).max(1);
                if self.config.breakeven_after_partial {
                    state.stop.raise(state.rm.entry_price());
                }
                return vec![Order::sell(Qty::Shares(shares)).with_reason(format!(
                    "{}R scale-out {:.0}% at target {:.4} (high {:.4})",
                    stage.r,
                    stage.ratio * 100.0,
                    state.rm.target_price(stage.r),
                    bar.high
                ))];
            }
        }

        // 3. Peak-close trailing stop (close-based).
        if let Some(level) = trail_hit {
            let trail = state.trailing.as_ref().expect("hit implies tracker");
            return vec![Order::sell(Qty::Max).with_reason(format!(
                "peak trailing stop: close {:.4} <= peak {:.4} - ATR trail {:.4}",
                bar.close,
                trail.peak(),
                level
            ))];
        }

        vec![]
    }

    /// Freeze the entry state: R geometry from entry + initial stop, staged
    /// take profit, trailing tracker.
    fn open_position(
        &self,
        ctx: &StrategyContext,
        entry_price: f64,
        entry_shares: u32,
        atr_now: Option<f64>,
    ) -> Option<OpenState> {
        let stop_price = match self.config.initial_stop {
            InitialStop::AtrMultiple(k) => match atr_now {
                Some(atr) => entry_price - k * atr,
                None => recent_low(ctx, ctx.len()),
            },
            InitialStop::RecentLow(n) => recent_low(ctx, n),
            InitialStop::FixedFraction(f) => entry_price * (1.0 - f),
        };
        let rm = RMultiple::new(entry_price, stop_price)?;
        let take_profit = if self.config.scale_out.is_empty() {
            None
        } else {
            let stages = self
                .config
                .scale_out
                .iter()
                .map(|&(r, ratio)| ScaleOutStage { r, ratio })
                .collect();
            Some(ScaledTakeProfit::new(stages).expect("validated in new"))
        };
        let trailing = self.config.trailing_atr_mult.map(|k| {
            PeakTrailingStop::new(k, entry_price).with_entry_gate(self.config.trailing_entry_gate)
        });
        Some(OpenState {
            rm,
            entry_shares,
            stop: StructuralStop::new(stop_price),
            take_profit,
            trailing,
        })
    }
}

/// Lowest low of the last `n` visible bars (including the current one).
fn recent_low(ctx: &StrategyContext, n: usize) -> f64 {
    let len = ctx.len();
    let from = len.saturating_sub(n.max(1));
    ctx.bars()[from..]
        .iter()
        .map(|b| b.low)
        .fold(f64::INFINITY, f64::min)
}

/// Strategy decorator that runs an [`ExitPolicy`] before the wrapped
/// strategy. Bars where the policy emits an exit order suppress the wrapped
/// strategy's orders (exit takes precedence); all other bars delegate
/// unchanged.
pub struct ExitManaged<S> {
    inner: S,
    policy: ExitPolicy,
}

impl<S> ExitManaged<S> {
    /// Wrap `inner` with `policy`.
    pub fn new(inner: S, policy: ExitPolicy) -> Self {
        ExitManaged { inner, policy }
    }

    /// The wrapped strategy.
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// The exit policy (for inspecting state in tests).
    pub fn policy(&self) -> &ExitPolicy {
        &self.policy
    }
}

impl<S: Strategy> Strategy for ExitManaged<S> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn on_bar(&mut self, ctx: &StrategyContext, bar_index: usize) -> Vec<Order> {
        let exits = self.policy.on_bar(ctx);
        if !exits.is_empty() {
            return exits;
        }
        self.inner.on_bar(ctx, bar_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{Bar, PriceSeries};
    use crate::engine::{BacktestEngine, EngineConfig};
    use astock_trading_rules::RuleSet;
    use astock_trading_rules::TradeSide;
    use chrono::NaiveDate;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    /// Bar with symmetric range: open = close, high = close + 0.5,
    /// low = close - 0.5 (TR = 1.0 on every bar -> ATR is exactly 1.0 once
    /// seeded, which keeps the golden numbers hand-computable).
    fn bar(date: NaiveDate, close: f64) -> Bar {
        Bar::new(
            date,
            close,
            close + 0.5,
            close - 0.5,
            close,
            1e6,
            None,
            false,
        )
    }

    /// Golden scenario: entry via BuyHold at bar 0 (fills bar 1 open 10.0),
    /// managed by an exit policy with:
    /// - initial stop = entry - 2*ATR = 10 - 2*1 = 8.0 (1R = 2.0 CNY)
    /// - scale-out 1R@12.0 (40%), 2R@14.0 (30%), 3R@16.0 (30%)
    /// - breakeven raise after the first leg
    /// - trailing = peak_close - 2*ATR, gated above entry
    ///
    /// Path (closes step 0.5/bar so TR == high-low == 1.0 on every bar and
    /// Wilder ATR stays exactly 1.0; high = close + 0.5, low = close - 0.5):
    /// bar 0:  10.0  order placed
    /// bar 1:  10.0  buy fills @10.0 -> 9_900 shares (100 lots fit:
    ///               9_900*10 + 25.74 fees = 99_025.74 <= 100_000; 10_000
    ///               shares would cost 100_025 > 100_000)
    /// bar 2:  10.5
    /// bar 3:  11.0
    /// bar 4:  11.5  high 12.0 >= 1R target 12.0 -> sell floor(9900*0.4)
    ///               = 3_960; stop raised to 10.0
    /// bar 5:  12.0  leg 1 fills @12.0
    /// bar 6:  12.5
    /// bar 7:  13.0
    /// bar 8:  13.5  high 14.0 >= 2R target 14.0 -> sell floor(9900*0.3)
    ///               = 2_970
    /// bar 9:  14.0  leg 2 fills @14.0; new peak close 14.0
    /// bar 10: 13.5  trail level = 14.0 - 2*1 = 12.0 (> entry 10.0 gate ok)
    /// bar 11: 13.0
    /// bar 12: 12.5
    /// bar 13: 12.0  close 12.0 <= 12.0 -> trailing exit, sell all 2_970
    /// bar 14: 11.5  exit fills @11.5
    /// bar 15: 11.5  flat
    #[test]
    fn golden_managed_run_with_scale_out_and_trailing() {
        let dates = [
            "2025-01-06",
            "2025-01-07",
            "2025-01-08",
            "2025-01-09",
            "2025-01-10",
            "2025-01-13",
            "2025-01-14",
            "2025-01-15",
            "2025-01-16",
            "2025-01-17",
            "2025-01-20",
            "2025-01-21",
            "2025-01-22",
            "2025-01-23",
            "2025-01-24",
            "2025-01-27",
        ];
        let closes = [
            10.0, 10.0, 10.5, 11.0, 11.5, 12.0, 12.5, 13.0, 13.5, 14.0, 13.5, 13.0, 12.5, 12.0,
            11.5, 11.5,
        ];
        let bars: Vec<Bar> = dates
            .iter()
            .zip(closes)
            .map(|(ds, c)| bar(d(ds), c))
            .collect();
        let series = PriceSeries::new("600519", bars).unwrap();

        let policy = ExitPolicy::new(ExitPolicyConfig {
            initial_stop: InitialStop::AtrMultiple(2.0),
            atr_period: 2, // seeded at bar 1 (the entry bar); TR is 1.0 on every bar
            scale_out: vec![(1.0, 0.40), (2.0, 0.30), (3.0, 0.30)],
            breakeven_after_partial: true,
            trailing_atr_mult: Some(2.0),
            trailing_entry_gate: true,
        });
        let mut strat = ExitManaged::new(crate::strategy::BuyHold, policy);

        let engine = BacktestEngine::new(
            RuleSet::load(None).unwrap(),
            EngineConfig::new("600519", 100_000.0),
        )
        .unwrap();
        let res = engine.run(&series, &mut strat).unwrap();

        assert_eq!(res.trades.len(), 4, "trades: {:?}", res.trades);

        let buy = &res.trades[0];
        assert_eq!(buy.side, TradeSide::Buy);
        assert_eq!(buy.date, d("2025-01-07"));
        assert_eq!(buy.shares, 9_900);
        assert!(approx(buy.price, 10.0));

        let leg1 = &res.trades[1];
        assert_eq!(leg1.side, TradeSide::Sell);
        assert_eq!(leg1.date, d("2025-01-13"));
        assert_eq!(leg1.shares, 3_960);
        assert!(approx(leg1.price, 12.0));
        assert!(leg1.reason.contains("1R"), "{}", leg1.reason);

        let leg2 = &res.trades[2];
        assert_eq!(leg2.side, TradeSide::Sell);
        assert_eq!(leg2.date, d("2025-01-17"));
        assert_eq!(leg2.shares, 2_970);
        assert!(approx(leg2.price, 14.0));
        assert!(leg2.reason.contains("2R"), "{}", leg2.reason);

        let trail = &res.trades[3];
        assert_eq!(trail.side, TradeSide::Sell);
        assert_eq!(trail.date, d("2025-01-24"));
        assert_eq!(trail.shares, 2_970);
        assert!(approx(trail.price, 11.5));
        assert!(trail.reason.contains("trailing"), "{}", trail.reason);

        assert!(res.rejections.is_empty(), "{:?}", res.rejections);

        // Hand-derived cash (fees: commission 0.025% min 5, stamp 0.05% sell,
        // transfer 0.001% both sides):
        // buy:    100_000 - 99_000 - (24.75+0.99) = 974.26
        // leg1:   +47_520 - (11.88+23.76+0.4752)  = 48_458.1448
        // leg2:   +41_580 - (10.395+20.79+0.4158) = 90_006.5440
        // trail:  +34_155 - (8.53875+17.0775+0.34155) = 124_135.586_2
        assert!(
            approx(res.final_equity(), 124_135.586_2),
            "final equity {}",
            res.final_equity()
        );
    }

    /// Structural stop: entry 10.0, fixed 5% stop at 9.5; a bar whose low
    /// trades through 9.5 forces a full exit (before any strategy order).
    #[test]
    fn structural_stop_exits_through_the_low() {
        let dates = ["2025-01-06", "2025-01-07", "2025-01-08", "2025-01-09"];
        let bars = vec![
            bar(d(dates[0]), 10.0),
            bar(d(dates[1]), 10.0), // buy fills @10.0
            bar(d(dates[2]), 9.8),  // low 9.3 < 9.5 -> stop fires
            bar(d(dates[3]), 9.7),  // exit fills @9.7
        ];
        let series = PriceSeries::new("600519", bars).unwrap();
        let policy = ExitPolicy::new(ExitPolicyConfig {
            initial_stop: InitialStop::FixedFraction(0.05),
            atr_period: 3,
            scale_out: vec![(1.0, 0.5)],
            breakeven_after_partial: true,
            trailing_atr_mult: Some(2.0),
            trailing_entry_gate: true,
        });
        let mut strat = ExitManaged::new(crate::strategy::BuyHold, policy);
        let engine = BacktestEngine::new(
            RuleSet::load(None).unwrap(),
            EngineConfig::new("600519", 100_000.0),
        )
        .unwrap();
        let res = engine.run(&series, &mut strat).unwrap();
        assert_eq!(res.trades.len(), 2);
        assert_eq!(res.trades[1].side, TradeSide::Sell);
        assert_eq!(res.trades[1].date, d("2025-01-09"));
        assert_eq!(res.trades[1].shares, 9_900);
        assert!(approx(res.trades[1].price, 9.7));
        assert!(res.trades[1].reason.contains("structural stop"));
    }

    /// The wrapped strategy is untouched on bars without an exit signal,
    /// and the decorator is a no-op when no exit ever fires.
    #[test]
    fn passthrough_when_no_exit_fires() {
        let dates = ["2025-01-06", "2025-01-07", "2025-01-08"];
        let bars: Vec<Bar> = dates.iter().map(|ds| bar(d(ds), 10.0)).collect();
        let series = PriceSeries::new("600519", bars).unwrap();

        let mk = || {
            ExitPolicy::new(ExitPolicyConfig {
                initial_stop: InitialStop::FixedFraction(0.05),
                atr_period: 3,
                scale_out: vec![(5.0, 1.0)], // far away: never fires
                breakeven_after_partial: true,
                trailing_atr_mult: Some(20.0), // 20 ATR: never fires
                trailing_entry_gate: true,
            })
        };
        let engine = BacktestEngine::new(
            RuleSet::load(None).unwrap(),
            EngineConfig::new("600519", 100_000.0),
        )
        .unwrap();
        let mut bare = crate::strategy::BuyHold;
        let ref_res = engine.run(&series, &mut bare).unwrap();

        let engine2 = BacktestEngine::new(
            RuleSet::load(None).unwrap(),
            EngineConfig::new("600519", 100_000.0),
        )
        .unwrap();
        let mut wrapped = ExitManaged::new(crate::strategy::BuyHold, mk());
        let wrapped_res = engine2.run(&series, &mut wrapped).unwrap();

        assert_eq!(ref_res.trades, wrapped_res.trades);
        assert_eq!(ref_res.equity, wrapped_res.equity);
    }
}
