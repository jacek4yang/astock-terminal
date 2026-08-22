//! Strategy interface and three reference strategies.
//!
//! # No-lookahead contract
//!
//! [`Strategy::on_bar`] receives a [`StrategyContext`] whose `bars` field is a
//! slice covering indices `0..=bar_index` of the full series — future bars are
//! structurally absent, so a strategy *cannot* read them. The context exposes
//! absolute-index access via [`StrategyContext::bar`] for ergonomic indexing;
//! in debug/test builds that accessor carries a `debug_assert!` that traps any
//! index beyond the current bar, so a strategy that tries to peek ahead (e.g.
//! by tracking the series length through side channels and indexing
//! absolutely) panics in tests instead of silently returning `None`.
//!
//! Strategies are otherwise plain Rust: they may keep their own state (the
//! Turtle stop remembers the entry price), and they learn about fills only
//! through the position snapshot exposed in the next bar's context.

use crate::data::Bar;

/// Order size specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qty {
    /// Exact share count. Buys are rounded **down** to the largest valid
    /// board lot; sells may be any share count up to the sellable position
    /// (A-share sells are not lot-constrained).
    Shares(u32),
    /// Buy: as many valid lots as cash allows after fees.
    /// Sell: the entire T+1-sellable position.
    Max,
}

/// Order direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Buy.
    Buy,
    /// Sell.
    Sell,
}

/// An order emitted by a strategy at bar *t*; the engine attempts to fill it
/// according to the configured [`crate::engine::ExecutionPolicy`]
/// (default: at bar *t+1*'s open).
#[derive(Debug, Clone, PartialEq)]
pub struct Order {
    /// Direction.
    pub side: Side,
    /// Size.
    pub qty: Qty,
    /// Free-form reason copied onto the fill record (auditability).
    pub reason: String,
}

impl Order {
    /// Buy order.
    pub fn buy(qty: Qty) -> Self {
        Order {
            side: Side::Buy,
            qty,
            reason: String::new(),
        }
    }

    /// Sell order.
    pub fn sell(qty: Qty) -> Self {
        Order {
            side: Side::Sell,
            qty,
            reason: String::new(),
        }
    }

    /// Attach a reason that will appear on the resulting fill.
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = reason.into();
        self
    }
}

/// Read-only snapshot of the portfolio as of the current bar, after any fills
/// of that bar were processed.
#[derive(Debug, Clone, Copy, Default)]
pub struct PositionSnapshot {
    /// Total shares held (including today's buys).
    pub shares: u32,
    /// Shares that may be sold today under T+1 (bought before today).
    pub sellable: u32,
    /// Volume-weighted average cost of the current holdings (fees excluded),
    /// or 0 when flat.
    pub avg_cost: f64,
    /// Cash balance.
    pub cash: f64,
}

/// The strategy's window onto the world at one bar.
///
/// `bars` contains exactly the bars `0..=current` of the series — nothing
/// more exists as far as the strategy is concerned.
pub struct StrategyContext<'a> {
    bars: &'a [Bar],
    current: usize,
    position: PositionSnapshot,
}

impl<'a> StrategyContext<'a> {
    /// Build a context. `bars` must already be truncated to `0..=current`.
    pub(crate) fn new(bars: &'a [Bar], current: usize, position: PositionSnapshot) -> Self {
        debug_assert!(
            current < bars.len(),
            "context slice must end at the current bar"
        );
        StrategyContext {
            bars,
            current,
            position,
        }
    }

    /// All bars up to and including the current one. No future data exists
    /// in this slice.
    pub fn bars(&self) -> &'a [Bar] {
        self.bars
    }

    /// Number of visible bars (== `current + 1`).
    pub fn len(&self) -> usize {
        self.bars.len()
    }

    /// Whether the context is empty (never, for a valid engine run).
    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }

    /// Absolute-index access. Returns `None` for out-of-range indices —
    /// which includes every future bar, since they are not in the slice.
    ///
    /// In debug/test builds, requesting an index *beyond the current bar*
    /// triggers a `debug_assert!`: that is always a look-ahead bug, whereas
    /// other out-of-range indices (e.g. negative history before bar 0)
    /// legitimately yield `None`.
    pub fn bar(&self, index: usize) -> Option<&'a Bar> {
        debug_assert!(
            index <= self.current,
            "look-ahead: strategy requested future bar {index} (current is {})",
            self.current
        );
        self.bars.get(index)
    }

    /// The current bar.
    pub fn last(&self) -> &'a Bar {
        &self.bars[self.current]
    }

    /// Closing prices of all visible bars, oldest first.
    pub fn closes(&self) -> impl Iterator<Item = f64> + 'a {
        self.bars.iter().map(|b| b.close)
    }

    /// Portfolio snapshot as of the current bar (after today's fills).
    pub fn position(&self) -> PositionSnapshot {
        self.position
    }
}

/// A trading strategy.
///
/// Called once per bar, in date order, with a context that can only see the
/// past. Returned orders are attempted per the engine's execution policy.
pub trait Strategy {
    /// Stable name used in reports.
    fn name(&self) -> &str;

    /// Produce orders for the current bar.
    fn on_bar(&mut self, ctx: &StrategyContext, bar_index: usize) -> Vec<Order>;
}

/// Buy with all cash on the first bar, never sell. Baseline for comparisons.
pub struct BuyHold;

impl Strategy for BuyHold {
    fn name(&self) -> &str {
        "buy_hold"
    }

    fn on_bar(&mut self, ctx: &StrategyContext, bar_index: usize) -> Vec<Order> {
        if bar_index == 0 && ctx.position().shares == 0 {
            vec![Order::buy(Qty::Max).with_reason("initial entry")]
        } else {
            vec![]
        }
    }
}

/// Simple moving-average cross: long when `fast` SMA > `slow` SMA, flat
/// otherwise. Enters with all cash, exits the whole position.
pub struct MaCross {
    /// Fast SMA window in bars.
    pub fast: usize,
    /// Slow SMA window in bars.
    pub slow: usize,
}

impl MaCross {
    /// Validate and build (`fast` must be >= 1 and < `slow`).
    pub fn new(fast: usize, slow: usize) -> Self {
        assert!(fast >= 1 && fast < slow, "require 1 <= fast < slow");
        MaCross { fast, slow }
    }

    fn sma(ctx: &StrategyContext, window: usize, end_exclusive: usize) -> Option<f64> {
        if end_exclusive < window {
            return None;
        }
        let sum: f64 = ctx.bars()[end_exclusive - window..end_exclusive]
            .iter()
            .map(|b| b.close)
            .sum();
        Some(sum / window as f64)
    }
}

impl Strategy for MaCross {
    fn name(&self) -> &str {
        "ma_cross"
    }

    fn on_bar(&mut self, ctx: &StrategyContext, _bar_index: usize) -> Vec<Order> {
        let n = ctx.len();
        // Compare fast vs slow SMA at the last two bars to detect a cross.
        let (Some(f_now), Some(s_now), Some(f_prev), Some(s_prev)) = (
            Self::sma(ctx, self.fast, n),
            Self::sma(ctx, self.slow, n),
            Self::sma(ctx, self.fast, n.saturating_sub(1)),
            Self::sma(ctx, self.slow, n.saturating_sub(1)),
        ) else {
            return vec![];
        };
        let crossed_up = f_prev <= s_prev && f_now > s_now;
        let crossed_down = f_prev >= s_prev && f_now < s_now;
        let pos = ctx.position();
        if crossed_up && pos.shares == 0 {
            vec![Order::buy(Qty::Max).with_reason("fast SMA crossed above slow SMA")]
        } else if crossed_down && pos.shares > 0 {
            vec![Order::sell(Qty::Max).with_reason("fast SMA crossed below slow SMA")]
        } else {
            vec![]
        }
    }
}

/// Turtle-style Donchian breakout (System-1 semantics, reimplemented locally
/// because the technical crate is not a dependency):
///
/// - **Entry** (flat only): close above the highest high of the previous
///   `entry_n` bars, *excluding* the current bar.
/// - **Exit**: close below the lowest low of the previous `exit_n` bars
///   (excluding current), or a 2N protective stop where N is the 20-bar
///   average true range and the stop sits at `entry_price - 2N`.
///
/// Pyramiding and System-2 are deliberately out of scope.
pub struct TurtleBreakout {
    /// Entry channel length in bars (classic: 20).
    pub entry_n: usize,
    /// Exit channel length in bars (classic: 10).
    pub exit_n: usize,
    /// Fill price of the active entry, for the 2N stop.
    entry_price: Option<f64>,
}

impl TurtleBreakout {
    /// Classic 20/10 parameters.
    pub fn new(entry_n: usize, exit_n: usize) -> Self {
        assert!(entry_n >= 2 && exit_n >= 1, "channel windows too small");
        TurtleBreakout {
            entry_n,
            exit_n,
            entry_price: None,
        }
    }

    /// Donchian channel `(highest high, lowest low)` over the `period` bars
    /// immediately *before* the current bar. Returns `None` when there is not
    /// enough history.
    fn donchian(ctx: &StrategyContext, period: usize) -> Option<(f64, f64)> {
        let n = ctx.len();
        if n < period + 1 {
            return None;
        }
        let window = &ctx.bars()[n - 1 - period..n - 1];
        let hi = window
            .iter()
            .map(|b| b.high)
            .fold(f64::NEG_INFINITY, f64::max);
        let lo = window.iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
        Some((hi, lo))
    }

    /// Average true range ("N") over `period` bars using the simple-mean
    /// convention of the legacy turtle module (`technical::calc_n`).
    fn n_value(ctx: &StrategyContext, period: usize) -> Option<f64> {
        let n = ctx.len();
        if n < period + 1 {
            return None;
        }
        let bars = &ctx.bars()[n - 1 - period..n];
        let mut sum = 0.0;
        for w in bars.windows(2) {
            let (prev, cur) = (w[0].close, &w[1]);
            let tr = (cur.high - cur.low)
                .max((cur.high - prev).abs())
                .max((cur.low - prev).abs());
            sum += tr;
        }
        Some(sum / period as f64)
    }
}

impl Strategy for TurtleBreakout {
    fn name(&self) -> &str {
        "turtle_breakout"
    }

    fn on_bar(&mut self, ctx: &StrategyContext, _bar_index: usize) -> Vec<Order> {
        let close = ctx.last().close;
        let pos = ctx.position();

        if pos.shares == 0 {
            self.entry_price = None;
            if let Some((hi, _)) = Self::donchian(ctx, self.entry_n) {
                if close > hi {
                    return vec![Order::buy(Qty::Max).with_reason(format!(
                        "close {close} above {n}-bar Donchian high {hi}",
                        n = self.entry_n
                    ))];
                }
            }
            return vec![];
        }

        // Position exists: track entry cost from the position snapshot so the
        // 2N stop survives across bars.
        if self.entry_price.is_none() {
            self.entry_price = Some(pos.avg_cost);
        }
        let entry = self.entry_price.unwrap();

        if let Some(n_val) = Self::n_value(ctx, 20) {
            let stop = entry - 2.0 * n_val;
            if close < stop {
                return vec![Order::sell(Qty::Max)
                    .with_reason(format!("close {close} below 2N stop {stop:.4}"))];
            }
        }
        if let Some((_, lo)) = Self::donchian(ctx, self.exit_n) {
            if close < lo {
                return vec![Order::sell(Qty::Max).with_reason(format!(
                    "close {close} below {n}-bar Donchian low {lo}",
                    n = self.exit_n
                ))];
            }
        }
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::PriceSeries;
    use chrono::NaiveDate;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn series(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Bar::flat(d("2025-01-06") + chrono::Duration::days(i as i64), c))
            .collect()
    }

    #[test]
    fn context_exposes_only_past() {
        let bars = series(&[1.0, 2.0, 3.0, 4.0]);
        let ctx = StrategyContext::new(&bars[..2], 1, PositionSnapshot::default());
        assert_eq!(ctx.len(), 2);
        assert_eq!(ctx.last().close, 2.0);
        // Index beyond the slice but not beyond `current` is impossible;
        // index below current but out of slice range yields None.
        assert!(ctx.bar(0).is_some());
    }

    /// The malicious-look-ahead test. Mechanism: structural slicing (future
    /// bars are not in the context) plus a `debug_assert!` in
    /// `StrategyContext::bar` that fires on any absolute index beyond the
    /// current bar. Debug assertions are on in test builds, so the malicious
    /// strategy panics and we catch that panic here.
    #[test]
    fn malicious_strategy_cannot_read_future() {
        struct Malicious;
        impl Strategy for Malicious {
            fn name(&self) -> &str {
                "malicious"
            }
            fn on_bar(&mut self, ctx: &StrategyContext, bar_index: usize) -> Vec<Order> {
                // Try to peek one bar into the future via the absolute API.
                let _peek = ctx.bar(bar_index + 1);
                vec![]
            }
        }

        let bars = series(&[1.0, 2.0, 3.0]);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let ctx = StrategyContext::new(&bars[..1], 0, PositionSnapshot::default());
            let mut m = Malicious;
            let _ = m.on_bar(&ctx, 0);
        }));
        assert!(
            result.is_err(),
            "debug assertion must trap future-bar access in test builds"
        );
    }

    #[test]
    fn ma_cross_generates_entry_and_exit() {
        // Down-then-up: fast crosses below, then above.
        let closes = [10.0, 9.0, 8.0, 9.0, 10.0, 11.0];
        let bars = series(&closes);
        let mut strat = MaCross::new(1, 3);
        let mut saw_buy = false;
        let mut saw_sell = false;
        for i in 0..bars.len() {
            let ctx = StrategyContext::new(&bars[..=i], i, PositionSnapshot::default());
            for o in strat.on_bar(&ctx, i) {
                match o.side {
                    Side::Buy => saw_buy = true,
                    Side::Sell => saw_sell = true,
                }
            }
        }
        assert!(saw_buy, "expected a cross-up entry");
        assert!(!saw_sell, "no position held in this unit test, so no exit");
    }

    #[test]
    fn turtle_enters_on_donchian_breakout() {
        // 21 flat bars then a breakout bar.
        let mut closes = vec![10.0_f64; 21];
        closes.push(11.0);
        let bars = series(&closes);
        let mut strat = TurtleBreakout::new(20, 10);
        let mut orders_at_last = vec![];
        for i in 0..bars.len() {
            let ctx = StrategyContext::new(&bars[..=i], i, PositionSnapshot::default());
            orders_at_last = strat.on_bar(&ctx, i);
        }
        assert_eq!(orders_at_last.len(), 1);
        assert_eq!(orders_at_last[0].side, Side::Buy);
    }

    #[test]
    fn price_series_smoke_for_strategy_inputs() {
        // Guard the helper used across tests: strictly increasing dates.
        let s = PriceSeries::new("600519", series(&[1.0, 2.0, 3.0])).unwrap();
        assert_eq!(s.len(), 3);
    }

    /// Regression snapshot: Turtle(20, 10) on a fixed synthetic trend.
    ///
    /// Series (56 bars from 2025-01-06, O=H=L=C on every bar):
    /// - bars  0..=20: flat at 10.0
    /// - bars 21..=40: rising 0.5/bar to 20.0
    /// - bars 41..=55: falling 1.0/bar to 6.0
    ///
    /// Hand-derived expectations:
    /// - Entry signal at bar 21 (close 10.5 > prior-20-bar high 10.0),
    ///   fill at bar 22 open = 11.0. Affordable lots: floor(100_000/11) =
    ///   9090 -> 9_000 shares; amount 99_000; fees: commission 24.75 +
    ///   transfer 0.99 = 25.74; cash after = 974.26.
    /// - Exit signal at bar 44 (close 16.0 < prior-10-bar low 17.0; the 2N
    ///   stop at 11 - 2*0.6 = 9.8 is not hit), fill at bar 45 open = 15.0
    ///   (limit-down is 14.4, so the fill is legal); amount 135_000; fees:
    ///   commission 33.75 + stamp 67.50 + transfer 1.35 = 102.60; cash
    ///   after = 974.26 + 135_000 - 102.60 = 135_871.66.
    /// - No re-entry during the decline.
    #[test]
    fn turtle_trend_regression_snapshot() {
        use crate::engine::{BacktestEngine, EngineConfig};
        use astock_trading_rules::{RuleSet, TradeSide};

        let mut closes: Vec<f64> = Vec::new();
        closes.extend(std::iter::repeat_n(10.0, 21));
        closes.extend((1..=20).map(|k| 10.0 + 0.5 * k as f64));
        closes.extend((1..=15).map(|k| 20.0 - k as f64));
        assert_eq!(closes.len(), 56);
        let bars = series(&closes);
        let series = PriceSeries::new("600519", bars).unwrap();

        let engine = BacktestEngine::new(
            RuleSet::load(None).unwrap(),
            EngineConfig::new("600519", 100_000.0),
        )
        .unwrap();
        let res = engine
            .run(&series, &mut TurtleBreakout::new(20, 10))
            .unwrap();

        let approx = |a: f64, b: f64| (a - b).abs() < 1e-6;
        assert_eq!(res.trades.len(), 2, "unexpected trades: {:?}", res.trades);

        let buy = &res.trades[0];
        assert_eq!(buy.side, TradeSide::Buy);
        assert_eq!(buy.date, d("2025-01-28")); // bar 22
        assert_eq!(buy.shares, 9_000);
        assert!(approx(buy.price, 11.0));
        assert!(approx(buy.amount, 99_000.0));
        assert!(approx(buy.fees.total, 25.74));
        assert!(approx(buy.cash_after, 974.26));

        let sell = &res.trades[1];
        assert_eq!(sell.side, TradeSide::Sell);
        assert_eq!(sell.date, d("2025-02-20")); // bar 45
        assert_eq!(sell.shares, 9_000);
        assert!(approx(sell.price, 15.0));
        assert!(approx(sell.amount, 135_000.0));
        assert!(approx(sell.fees.total, 102.60));
        assert!(approx(sell.cash_after, 135_871.66));

        assert!(res.rejections.is_empty());
        assert!(approx(res.final_equity(), 135_871.66));
    }
}
