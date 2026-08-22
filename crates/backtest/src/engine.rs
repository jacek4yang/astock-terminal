//! Event-driven daily-bar backtesting engine with A-share trading constraints.
//!
//! # Execution model
//!
//! A strategy emits orders at bar *t* after seeing bars `0..=t`. When those
//! orders fill is governed by [`ExecutionPolicy`]:
//!
//! - `NextOpen` (**default**): fill at bar *t+1*'s open. This mirrors how a
//!   daily-signal trader actually behaves — decide after the close, send the
//!   order for the next session — and every input to the fill (open price,
//!   limit prices from *t*'s close) was knowable when the order was placed.
//! - `NextClose`: fill at bar *t+1*'s close. Still causal (the *t+1* close is
//!   unknown at decision time), useful to compare against close-based marks.
//! - `SameClose`: fill at bar *t*'s close — the same close the strategy just
//!   observed. **Look-ahead risky**: in live trading you cannot both use the
//!   closing print as a signal input and guarantee execution at that print.
//!   Provided only for research comparisons against engines that silently do
//!   this; never use it to validate a strategy for production.
//!
//! Orders live for exactly one execution attempt: an order that cannot fill
//! (limit-locked, suspended, T+1, insufficient cash, below min lot) is
//! recorded in [`BacktestResult::rejections`] and dropped — it does not linger.
//!
//! # A-share realism (all enforced at fill time)
//!
//! - **T+1**: only shares bought *before* the current bar are sellable.
//! - **Lots**: buys round down to the board's valid lot (100-share lots on
//!   main/ChiNext boards; 200-share minimum with 1-share increments on STAR)
//!   via [`astock_trading_rules::BoardRules::is_valid_lot`]. Sells are not
//!   lot-constrained, matching exchange rules.
//! - **Price limits**: a buy cannot fill at or above the limit-up price, a
//!   sell cannot fill at or below the limit-down price; limits are computed
//!   from the previous close with the board's (ST-aware) limit percentage.
//! - **Suspensions**: no fills on suspended bars; positions are marked at the
//!   last non-suspended close.
//! - **Fees**: commission, sell-side stamp tax, and transfer fee from the
//!   versioned `astock-trading-rules` schedule, evaluated at the trade date
//!   (so historical policy changes like the 2023 stamp-tax cut apply).
//! - **Slippage**: a fixed `slippage_bps` applied adversely to the raw
//!   execution price (buys pay more, sells receive less).
//!
//! # Price adjustment (data-foundation-v2 §回测对接)
//!
//! `EngineConfig::adjustment` selects what the *strategy* sees:
//!
//! - [`AdjustmentPolicy::Raw`] (default): the strategy sees the same raw
//!   (不复权) prices the engine trades on.
//! - [`AdjustmentPolicy::QfqAsOf`]: at each bar *t* the strategy-visible
//!   history `0..=t` is forward-adjusted (前复权) **anchored at t**, using
//!   only corporate actions with `ex_date ≤ t` (point-in-time, per
//!   `docs/data-foundation-v2.md` §原则). Because the anchor is the current
//!   bar, the visible price of bar *t* itself is always the raw price, and
//!   an ex-date no longer appears as a fake crash (a 10送10 shows as a
//!   continuous series instead of −50%). Fills, fees, lot math, price-limit
//!   gates and equity marks always use **raw** prices regardless of policy.
//!
//! Caveat: corporate actions that occur *while a position is held* do not
//! change the portfolio's share count (dividends are not reinvested, splits
//! do not double holdings). The position's `avg_cost` therefore stays in
//! raw units, which match the current-bar scale under `QfqAsOf`; strategies
//! comparing cost against *past* visible prices across an ex-date should be
//! aware of the scale break.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use astock_core::CorporateAction;
use astock_trading_rules::{BoardRules, RuleSet, TradeCost, TradeSide};

use crate::data::PriceSeries;
use crate::strategy::{Order, PositionSnapshot, Qty, Side, Strategy, StrategyContext};
use crate::{Error, Result};

/// When orders placed at bar *t* are executed. See module docs for the
/// look-ahead discussion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExecutionPolicy {
    /// Fill at bar *t+1*'s open (default; recommended).
    #[default]
    NextOpen,
    /// Fill at bar *t+1*'s close.
    NextClose,
    /// Fill at bar *t*'s close. Look-ahead risky — see module docs.
    SameClose,
}

/// What the strategy sees, price-adjustment-wise. Execution always uses raw
/// prices; see the module docs for the full discussion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AdjustmentPolicy {
    /// Strategy sees raw (不复权) prices — identical to execution prices.
    #[default]
    Raw,
    /// Strategy sees 前复权 prices re-anchored at every bar (point-in-time:
    /// only actions with `ex_date ≤` the current bar are applied). The
    /// current bar's visible price always equals the raw price.
    QfqAsOf,
}

/// Engine configuration.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Symbol being traded; determines board rules (limits, lots, market).
    pub symbol: String,
    /// Starting cash in CNY.
    pub initial_cash: f64,
    /// Execution policy.
    pub execution: ExecutionPolicy,
    /// Adjustment policy for strategy-visible prices.
    pub adjustment: AdjustmentPolicy,
    /// Corporate actions backing [`AdjustmentPolicy::QfqAsOf`]
    /// (per-share ratios, see [`astock_core::CorporateAction`]).
    pub corporate_actions: Vec<CorporateAction>,
    /// Adverse slippage in basis points applied to every fill.
    pub slippage_bps: f64,
    /// Whether the stock is ST/*ST (affects the daily price limit).
    pub is_st: bool,
}

impl EngineConfig {
    /// Defaults: `NextOpen`, no slippage, not ST.
    pub fn new(symbol: impl Into<String>, initial_cash: f64) -> Self {
        EngineConfig {
            symbol: symbol.into(),
            initial_cash,
            execution: ExecutionPolicy::NextOpen,
            adjustment: AdjustmentPolicy::Raw,
            corporate_actions: Vec::new(),
            slippage_bps: 0.0,
            is_st: false,
        }
    }

    /// Set the execution policy.
    pub fn with_execution(mut self, execution: ExecutionPolicy) -> Self {
        self.execution = execution;
        self
    }

    /// Set the adjustment policy for strategy-visible prices.
    pub fn with_adjustment(mut self, adjustment: AdjustmentPolicy) -> Self {
        self.adjustment = adjustment;
        self
    }

    /// Set the corporate actions used by [`AdjustmentPolicy::QfqAsOf`].
    pub fn with_corporate_actions(mut self, actions: Vec<CorporateAction>) -> Self {
        self.corporate_actions = actions;
        self
    }

    /// Set adverse slippage in basis points.
    pub fn with_slippage_bps(mut self, bps: f64) -> Self {
        self.slippage_bps = bps;
        self
    }

    /// Mark the stock as ST/*ST.
    pub fn with_st(mut self, is_st: bool) -> Self {
        self.is_st = is_st;
        self
    }
}

/// A single executed trade.
#[derive(Debug, Clone, PartialEq)]
pub struct Fill {
    /// Execution date.
    pub date: NaiveDate,
    /// Side.
    pub side: TradeSide,
    /// Shares filled.
    pub shares: u32,
    /// Fill price per share, slippage-adjusted.
    pub price: f64,
    /// Gross trade amount (`price * shares`).
    pub amount: f64,
    /// Fee breakdown charged on this fill.
    pub fees: TradeCost,
    /// Strategy-supplied reason (from [`Order::reason`]).
    pub reason: String,
    /// Cash balance immediately after this fill.
    pub cash_after: f64,
}

/// Why an order could not fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    /// Buy at/above the limit-up price.
    LimitUp,
    /// Sell at/below the limit-down price.
    LimitDown,
    /// Bar was suspended.
    Suspended,
    /// T+1: shares were bought today and cannot be sold yet.
    T1Restriction,
    /// Position is flat; nothing to sell.
    NoPosition,
    /// Cash does not cover even the minimum lot plus fees.
    InsufficientCash,
    /// Requested buy size is below the board minimum lot.
    BelowMinLot,
}

/// A dropped order, kept for auditability (one-bar TTL — see module docs).
#[derive(Debug, Clone, PartialEq)]
pub struct Rejection {
    /// Date on which execution was attempted.
    pub date: NaiveDate,
    /// Order side.
    pub side: Side,
    /// Requested quantity.
    pub qty: Qty,
    /// Why it was rejected.
    pub reason: RejectReason,
}

/// End-of-day portfolio snapshot; the equity curve is a `Vec` of these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EquityPoint {
    /// Bar date.
    pub date: NaiveDate,
    /// Cash balance.
    pub cash: f64,
    /// Shares held.
    pub shares: u32,
    /// Mark price (last non-suspended close).
    pub close: f64,
    /// `shares * close`.
    pub market_value: f64,
    /// Cumulative fees paid up to and including this bar.
    pub fees_cum: f64,
    /// `cash + market_value`.
    pub equity: f64,
}

/// Output of one engine run.
#[derive(Debug, Clone, Default)]
pub struct BacktestResult {
    /// Daily equity curve, one point per bar.
    pub equity: Vec<EquityPoint>,
    /// Every fill, in execution order.
    pub trades: Vec<Fill>,
    /// Every rejected order, in execution order.
    pub rejections: Vec<Rejection>,
    /// Corporate-action data problems surfaced by the adjustment engine
    /// under [`AdjustmentPolicy::QfqAsOf`]; empty under `Raw`.
    pub adjustment_warnings: Vec<astock_core::AdjustWarning>,
}

impl BacktestResult {
    /// Final equity (0 for an empty run).
    pub fn final_equity(&self) -> f64 {
        self.equity.last().map(|p| p.equity).unwrap_or(0.0)
    }

    /// Total fees paid over the run.
    pub fn total_fees(&self) -> f64 {
        self.trades.iter().map(|f| f.fees.total).sum()
    }

    /// Total traded amount (buys + sells), for turnover metrics.
    pub fn traded_amount(&self) -> f64 {
        self.trades.iter().map(|f| f.amount).sum()
    }

    /// FIFO round trips derived from the fill log.
    pub fn round_trips(&self) -> Vec<RoundTrip> {
        round_trips(&self.trades)
    }
}

/// A closed FIFO lot pairing: shares bought then sold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoundTrip {
    /// Date of the (first) matched buy.
    pub entry_date: NaiveDate,
    /// Date of the sell that closed the shares.
    pub exit_date: NaiveDate,
    /// Shares in this round trip.
    pub shares: u32,
    /// Net P&L in CNY after all fees on both legs.
    pub pnl: f64,
    /// `pnl / entry cost (including buy fees)`.
    pub return_pct: f64,
}

/// Match buys and sells FIFO into round trips. Partial closes split lots;
/// an open residual position at the end is ignored (no exit, no P&L).
pub fn round_trips(fills: &[Fill]) -> Vec<RoundTrip> {
    use std::collections::VecDeque;

    // Open lots: (date, per-share cost incl. buy fees, remaining shares).
    let mut open: VecDeque<(NaiveDate, f64, u32)> = VecDeque::new();
    let mut trips = Vec::new();

    for fill in fills {
        match fill.side {
            TradeSide::Buy => {
                let per_share = fill.price + fill.fees.total / fill.shares as f64;
                open.push_back((fill.date, per_share, fill.shares));
            }
            TradeSide::Sell => {
                let per_share_net = fill.price - fill.fees.total / fill.shares as f64;
                let mut remaining = fill.shares;
                while remaining > 0 {
                    let Some((entry_date, cost, lot_shares)) = open.front_mut() else {
                        break; // sell without an open lot (shouldn't happen)
                    };
                    let take = remaining.min(*lot_shares);
                    let pnl = take as f64 * (per_share_net - *cost);
                    trips.push(RoundTrip {
                        entry_date: *entry_date,
                        exit_date: fill.date,
                        shares: take,
                        pnl,
                        return_pct: pnl / (take as f64 * *cost),
                    });
                    *lot_shares -= take;
                    remaining -= take;
                    if *lot_shares == 0 {
                        open.pop_front();
                    }
                }
            }
        }
    }
    trips
}

/// The engine: an immutable [`RuleSet`] + config, reusable across runs.
pub struct BacktestEngine {
    rules: RuleSet,
    board: BoardRules,
    config: EngineConfig,
}

/// Mutable per-run portfolio state.
struct Portfolio {
    cash: f64,
    shares: u32,
    sellable: u32,
    cost_basis: f64,
    fees_cum: f64,
}

impl Portfolio {
    fn snapshot(&self) -> PositionSnapshot {
        PositionSnapshot {
            shares: self.shares,
            sellable: self.sellable,
            avg_cost: if self.shares > 0 {
                self.cost_basis / self.shares as f64
            } else {
                0.0
            },
            cash: self.cash,
        }
    }
}

impl BacktestEngine {
    /// Build an engine, resolving board rules for `config.symbol`.
    pub fn new(rules: RuleSet, config: EngineConfig) -> Result<Self> {
        if config.initial_cash <= 0.0 {
            return Err(Error::NonPositiveCash(config.initial_cash));
        }
        let board = rules.for_symbol(&config.symbol)?;
        Ok(BacktestEngine {
            rules,
            board,
            config,
        })
    }

    /// The resolved board rules for the configured symbol.
    pub fn board(&self) -> &BoardRules {
        &self.board
    }

    /// The engine configuration.
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Run `strategy` over `series`.
    pub fn run(&self, series: &PriceSeries, strategy: &mut dyn Strategy) -> Result<BacktestResult> {
        if series.is_empty() {
            return Err(Error::EmptySeries {
                symbol: series.symbol.clone(),
            });
        }
        let bars = &series.bars;
        let n = bars.len();
        let mut pf = Portfolio {
            cash: self.config.initial_cash,
            shares: 0,
            sellable: 0,
            cost_basis: 0.0,
            fees_cum: 0.0,
        };
        let mut result = BacktestResult::default();
        let mut pending: Vec<Order> = Vec::new();
        // Mark price for suspended bars; starts at the first close.
        let mut last_close = bars[0].close;

        // --- AdjustmentPolicy::QfqAsOf setup (see module docs) ---
        // Per-ex-date factor multipliers, sorted ascending. r_i = X/C per the
        // spec formula; computing them once up front is still point-in-time
        // correct because a multiplier only enters the visible series when
        // the walk reaches its ex-date, and r_i depends only on the raw
        // close before that date.
        let qfq_asof = matches!(self.config.adjustment, AdjustmentPolicy::QfqAsOf);
        let multipliers: Vec<(NaiveDate, f64)> = if qfq_asof {
            let core_bars: Vec<astock_core::Bar> = bars
                .iter()
                .map(|b| {
                    astock_core::Bar::new(
                        b.date,
                        b.open,
                        b.close,
                        b.high,
                        b.low,
                        b.volume,
                        astock_core::VolumeUnit::Lots,
                    )
                })
                .collect();
            let (factors, warnings) = astock_core::action_factors(
                &core_bars,
                &self.config.corporate_actions,
                bars[n - 1].date,
                None,
            );
            result.adjustment_warnings = warnings;
            // Fold same-date actions into one multiplier.
            let mut folded: Vec<(NaiveDate, f64)> = Vec::with_capacity(factors.len());
            for (ex_date, r) in factors {
                match folded.last_mut() {
                    Some((d, acc)) if *d == ex_date => *acc *= r,
                    _ => folded.push((ex_date, r)),
                }
            }
            folded
        } else {
            Vec::new()
        };
        // Strategy-visible series under QfqAsOf: bars 0..=t re-anchored at t.
        // All multipliers are usually 1, so past bars are rescaled only on
        // bars that follow an ex-date (O(n) per action, not per bar).
        let mut visible: Vec<crate::data::Bar> = Vec::with_capacity(n);
        let mut next_mult = 0_usize;
        let mut pending_mult = 1.0_f64;

        // Index-based loop is intentional: each iteration needs the current
        // bar, the previous bar (pre_close), and a truncated slice for the
        // strategy context — an iterator would obscure that.
        #[allow(clippy::needless_range_loop)]
        for t in 0..n {
            // T+1: at the start of a new bar, everything held was bought
            // before today and becomes sellable.
            pf.sellable = pf.shares;

            if qfq_asof {
                // Fold in every action whose ex-date has arrived (ex-dates
                // with no bar, e.g. weekends, apply at the next bar).
                while next_mult < multipliers.len() && multipliers[next_mult].0 <= bars[t].date {
                    pending_mult *= multipliers[next_mult].1;
                    next_mult += 1;
                }
                if pending_mult != 1.0 {
                    for b in visible.iter_mut() {
                        b.open *= pending_mult;
                        b.close *= pending_mult;
                        b.high *= pending_mult;
                        b.low *= pending_mult;
                    }
                    pending_mult = 1.0;
                }
                visible.push(bars[t].clone());
            }

            match self.config.execution {
                ExecutionPolicy::NextOpen => {
                    // Orders from bar t-1 fill at today's open.
                    if t > 0 {
                        let price = bars[t].open;
                        self.execute_all(&mut pending, &mut pf, &mut result, series, t, price);
                    }
                    let orders = strategy
                        .on_bar(&self.ctx(self.bars_for_ctx(series, &visible, t), t, &pf), t);
                    pending = orders;
                }
                ExecutionPolicy::NextClose => {
                    // Decide first; orders from bar t-1 fill at today's close.
                    let prior = std::mem::take(&mut pending);
                    let orders = strategy
                        .on_bar(&self.ctx(self.bars_for_ctx(series, &visible, t), t, &pf), t);
                    pending = orders;
                    if t > 0 {
                        let mut prior = prior;
                        let price = bars[t].close;
                        self.execute_all(&mut prior, &mut pf, &mut result, series, t, price);
                    }
                }
                ExecutionPolicy::SameClose => {
                    // Look-ahead risky (see module docs): decide and fill on
                    // the same close the strategy just saw.
                    let mut orders = strategy
                        .on_bar(&self.ctx(self.bars_for_ctx(series, &visible, t), t, &pf), t);
                    let price = bars[t].close;
                    self.execute_all(&mut orders, &mut pf, &mut result, series, t, price);
                }
            }

            // End-of-day mark: suspended bars keep the last good close.
            if !bars[t].suspended {
                last_close = bars[t].close;
            }
            let market_value = pf.shares as f64 * last_close;
            result.equity.push(EquityPoint {
                date: bars[t].date,
                cash: pf.cash,
                shares: pf.shares,
                close: last_close,
                market_value,
                fees_cum: pf.fees_cum,
                equity: pf.cash + market_value,
            });
        }
        Ok(result)
    }

    /// The strategy-visible slice for bar `t`: the raw series under
    /// [`AdjustmentPolicy::Raw`], the re-anchored `visible` buffer under
    /// [`AdjustmentPolicy::QfqAsOf`]. Ends at the current bar either way
    /// (structural no-lookahead).
    fn bars_for_ctx<'a>(
        &self,
        series: &'a PriceSeries,
        visible: &'a [crate::data::Bar],
        t: usize,
    ) -> &'a [crate::data::Bar] {
        match self.config.adjustment {
            AdjustmentPolicy::Raw => &series.bars[..=t],
            AdjustmentPolicy::QfqAsOf => &visible[..=t],
        }
    }

    fn ctx<'a>(
        &self,
        bars: &'a [crate::data::Bar],
        t: usize,
        pf: &Portfolio,
    ) -> StrategyContext<'a> {
        // The slice ends at the current bar: structural no-lookahead.
        StrategyContext::new(bars, t, pf.snapshot())
    }

    /// Reference close used for price-limit computation at bar `t`.
    /// Bar 0 (only reachable under `SameClose`) has no previous close, so its
    /// own open is used as the reference — documented approximation.
    fn pre_close(&self, series: &PriceSeries, t: usize) -> f64 {
        if t == 0 {
            series.bars[0].open
        } else {
            series.bars[t - 1].close
        }
    }

    fn execute_all(
        &self,
        orders: &mut Vec<Order>,
        pf: &mut Portfolio,
        result: &mut BacktestResult,
        series: &PriceSeries,
        t: usize,
        raw_price: f64,
    ) {
        for order in orders.drain(..) {
            self.execute_one(&order, pf, result, series, t, raw_price);
        }
    }

    fn execute_one(
        &self,
        order: &Order,
        pf: &mut Portfolio,
        result: &mut BacktestResult,
        series: &PriceSeries,
        t: usize,
        raw_price: f64,
    ) {
        let bar = &series.bars[t];
        let reject = |result: &mut BacktestResult, reason: RejectReason| {
            result.rejections.push(Rejection {
                date: bar.date,
                side: order.side,
                qty: order.qty,
                reason,
            });
        };

        // Suspended: no trading at all, price is stale.
        if bar.suspended {
            reject(result, RejectReason::Suspended);
            return;
        }

        // Price-limit gate (checked against the raw execution price).
        let pre_close = self.pre_close(series, t);
        let limit_up = self.board.limit_up_price(pre_close, self.config.is_st);
        let limit_down = self.board.limit_down_price(pre_close, self.config.is_st);
        const EPS: f64 = 1e-9;
        match order.side {
            Side::Buy if raw_price >= limit_up - EPS => {
                reject(result, RejectReason::LimitUp);
                return;
            }
            Side::Sell if raw_price <= limit_down + EPS => {
                reject(result, RejectReason::LimitDown);
                return;
            }
            _ => {}
        }

        // Adverse slippage.
        let slip = self.config.slippage_bps / 10_000.0;
        let price = match order.side {
            Side::Buy => raw_price * (1.0 + slip),
            Side::Sell => raw_price * (1.0 - slip),
        };

        match order.side {
            Side::Buy => self.execute_buy(order, pf, result, bar, price),
            Side::Sell => self.execute_sell(order, pf, result, bar, price),
        }
    }

    /// Largest valid lot not exceeding `shares` (0 if below the minimum).
    fn round_lot(&self, shares: u32) -> u32 {
        let (min, step) = (self.board.min_lot, self.board.lot_step);
        if shares < min {
            0
        } else {
            min + (shares - min) / step * step
        }
    }

    fn execute_buy(
        &self,
        order: &Order,
        pf: &mut Portfolio,
        result: &mut BacktestResult,
        bar: &crate::data::Bar,
        price: f64,
    ) {
        let reject = |result: &mut BacktestResult, reason: RejectReason| {
            result.rejections.push(Rejection {
                date: bar.date,
                side: order.side,
                qty: order.qty,
                reason,
            });
        };

        let affordable = (pf.cash / price).floor().max(0.0) as u32;
        let mut shares = match order.qty {
            Qty::Shares(q) => {
                if q < self.board.min_lot {
                    reject(result, RejectReason::BelowMinLot);
                    return;
                }
                self.round_lot(q)
            }
            Qty::Max => self.round_lot(affordable),
        };
        if shares < self.board.min_lot {
            reject(result, RejectReason::InsufficientCash);
            return;
        }

        // Shrink by one lot step until cash covers amount + fees.
        let fitted = loop {
            let amount = shares as f64 * price;
            let fees = self.rules.trade_cost_at(
                TradeSide::Buy,
                amount,
                Some(&self.board.market),
                bar.date,
            );
            if amount + fees.total <= pf.cash + 1e-9 {
                break Some((shares, amount, fees));
            }
            if shares < self.board.min_lot + self.board.lot_step {
                break None;
            }
            shares -= self.board.lot_step;
        };
        let Some((shares, amount, fees)) = fitted else {
            reject(result, RejectReason::InsufficientCash);
            return;
        };

        pf.cash -= amount + fees.total;
        pf.shares += shares;
        pf.cost_basis += amount;
        pf.fees_cum += fees.total;
        // T+1: today's buy does NOT increase `sellable`.
        result.trades.push(Fill {
            date: bar.date,
            side: TradeSide::Buy,
            shares,
            price,
            amount,
            fees,
            reason: order.reason.clone(),
            cash_after: pf.cash,
        });
    }

    fn execute_sell(
        &self,
        order: &Order,
        pf: &mut Portfolio,
        result: &mut BacktestResult,
        bar: &crate::data::Bar,
        price: f64,
    ) {
        let reject = |result: &mut BacktestResult, reason: RejectReason| {
            result.rejections.push(Rejection {
                date: bar.date,
                side: order.side,
                qty: order.qty,
                reason,
            });
        };

        if pf.shares == 0 {
            reject(result, RejectReason::NoPosition);
            return;
        }
        if pf.sellable == 0 {
            reject(result, RejectReason::T1Restriction);
            return;
        }
        // Clamp to the sellable amount (partial fills of oversized requests).
        let shares = match order.qty {
            Qty::Shares(q) => q.min(pf.sellable),
            Qty::Max => pf.sellable,
        };
        if shares == 0 {
            reject(result, RejectReason::T1Restriction);
            return;
        }

        let amount = shares as f64 * price;
        let fees =
            self.rules
                .trade_cost_at(TradeSide::Sell, amount, Some(&self.board.market), bar.date);
        let avg = pf.cost_basis / pf.shares as f64;
        pf.cash += amount - fees.total;
        pf.shares -= shares;
        pf.sellable -= shares;
        pf.cost_basis -= avg * shares as f64;
        if pf.shares == 0 {
            pf.cost_basis = 0.0;
        }
        pf.fees_cum += fees.total;
        result.trades.push(Fill {
            date: bar.date,
            side: TradeSide::Sell,
            shares,
            price,
            amount,
            fees,
            reason: order.reason.clone(),
            cash_after: pf.cash,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Bar;
    use chrono::NaiveDate;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn rules() -> RuleSet {
        RuleSet::load(None).unwrap()
    }

    /// Strategy with a fixed per-bar order script, for golden tests.
    struct Scripted {
        script: std::collections::HashMap<usize, Vec<Order>>,
    }

    impl Scripted {
        fn new(script: Vec<(usize, Vec<Order>)>) -> Self {
            Scripted {
                script: script.into_iter().collect(),
            }
        }
    }

    impl Strategy for Scripted {
        fn name(&self) -> &str {
            "scripted"
        }
        fn on_bar(&mut self, _ctx: &StrategyContext, bar_index: usize) -> Vec<Order> {
            self.script.get(&bar_index).cloned().unwrap_or_default()
        }
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    /// Golden scenario: hand-computed fills on a scripted series.
    ///
    /// Series (2025-01-06..10, all weekdays), symbol 600519 (SH main board:
    /// 10% limit, 100-share lots, SH transfer fee 0.001%):
    ///
    /// | bar | date   | open  | close | note                          |
    /// |-----|--------|-------|-------|-------------------------------|
    /// | 0   | 01-06  | 10.00 | 10.00 | order: buy 1050 + sell 500    |
    /// | 1   | 01-07  | 10.00 | 11.00 | buy fills; sell blocked (T+1) |
    /// | 2   | 01-08  | 12.10 | 12.10 | open == limit-up: buy blocked |
    /// | 3   | 01-09  | 12.00 | 12.00 | sell-all fills                |
    /// | 4   | 01-10  | 12.00 | 13.00 | flat                          |
    ///
    /// Hand computation (fees: commission 0.025% min 5, stamp 0.05% sell,
    /// transfer 0.001% both sides):
    /// - Buy at 10.00 x1000 (1050 rounded down to lot): amount 10_000,
    ///   commission max(2.50, 5) = 5.00, transfer 0.10 -> fees 5.10;
    ///   cash = 100_000 - 10_000 - 5.10 = 89_994.90.
    /// - Sell at 12.00 x1000: amount 12_000, commission 5.00, stamp 6.00,
    ///   transfer 0.12 -> fees 11.12; cash = 89_994.90 + 12_000 - 11.12
    ///   = 101_983.78.
    #[test]
    fn golden_hand_computed_run() {
        let bars = vec![
            Bar::new(
                d("2025-01-06"),
                10.00,
                10.00,
                10.00,
                10.00,
                1e6,
                None,
                false,
            ),
            Bar::new(
                d("2025-01-07"),
                10.00,
                11.00,
                10.00,
                11.00,
                1e6,
                None,
                false,
            ),
            Bar::new(
                d("2025-01-08"),
                12.10,
                12.10,
                12.10,
                12.10,
                1e6,
                None,
                false,
            ),
            Bar::new(
                d("2025-01-09"),
                12.00,
                12.10,
                11.90,
                12.00,
                1e6,
                None,
                false,
            ),
            Bar::new(
                d("2025-01-10"),
                12.00,
                13.00,
                12.00,
                13.00,
                1e6,
                None,
                false,
            ),
        ];
        let series = PriceSeries::new("600519", bars).unwrap();
        let engine = BacktestEngine::new(rules(), EngineConfig::new("600519", 100_000.0)).unwrap();
        let mut strat = Scripted::new(vec![
            (
                0,
                vec![
                    Order::buy(Qty::Shares(1050)).with_reason("lot rounding check"),
                    Order::sell(Qty::Shares(500)).with_reason("must be T+1 blocked"),
                ],
            ),
            (
                1,
                vec![Order::buy(Qty::Shares(1000)).with_reason("must hit limit-up")],
            ),
            (2, vec![Order::sell(Qty::Max).with_reason("exit")]),
        ]);
        let res = engine.run(&series, &mut strat).unwrap();

        // Exactly two fills.
        assert_eq!(res.trades.len(), 2);

        let buy = &res.trades[0];
        assert_eq!(buy.date, d("2025-01-07"));
        assert_eq!(buy.side, TradeSide::Buy);
        assert_eq!(buy.shares, 1000); // 1050 rounded down to a valid lot
        assert!(approx(buy.price, 10.00));
        assert!(approx(buy.amount, 10_000.0));
        assert!(approx(buy.fees.commission, 5.0)); // min-fee floor
        assert!(approx(buy.fees.transfer_fee, 0.10));
        assert!(approx(buy.fees.stamp_tax, 0.0));
        assert!(approx(buy.fees.total, 5.10));
        assert!(approx(buy.cash_after, 89_994.90));

        let sell = &res.trades[1];
        assert_eq!(sell.date, d("2025-01-09"));
        assert_eq!(sell.side, TradeSide::Sell);
        assert_eq!(sell.shares, 1000);
        assert!(approx(sell.price, 12.00));
        assert!(approx(sell.fees.commission, 5.0));
        assert!(approx(sell.fees.stamp_tax, 6.0)); // 0.05% of 12_000
        assert!(approx(sell.fees.transfer_fee, 0.12));
        assert!(approx(sell.fees.total, 11.12));
        assert!(approx(sell.cash_after, 101_983.78));

        // Two rejections: T+1 on 01-07, limit-up on 01-08.
        assert_eq!(res.rejections.len(), 2);
        assert_eq!(res.rejections[0].date, d("2025-01-07"));
        assert_eq!(res.rejections[0].reason, RejectReason::T1Restriction);
        assert_eq!(res.rejections[1].date, d("2025-01-08"));
        assert_eq!(res.rejections[1].reason, RejectReason::LimitUp);

        // Exact equity curve.
        let expected = [
            100_000.00,                 // 01-06: all cash
            89_994.90 + 1_000.0 * 11.0, // 01-07: 100_994.90
            89_994.90 + 1_000.0 * 12.1, // 01-08: 102_094.90
            101_983.78,                 // 01-09: flat after exit
            101_983.78,                 // 01-10: flat
        ];
        assert_eq!(res.equity.len(), expected.len());
        for (point, want) in res.equity.iter().zip(expected) {
            assert!(approx(point.equity, want), "{} vs {want}", point.equity);
        }

        // Round trip P&L: 1000 * (12 - 10) - 5.10 - 11.12 = 1_983.78.
        let trips = res.round_trips();
        assert_eq!(trips.len(), 1);
        assert!(approx(trips[0].pnl, 1_983.78));
        assert_eq!(trips[0].entry_date, d("2025-01-07"));
        assert_eq!(trips[0].exit_date, d("2025-01-09"));
    }

    #[test]
    fn suspended_bar_blocks_fill_and_keeps_stale_mark() {
        let mut b1 = Bar::flat(d("2025-01-07"), 11.0);
        b1.suspended = true;
        let bars = vec![
            Bar::flat(d("2025-01-06"), 10.0),
            b1,
            Bar::flat(d("2025-01-08"), 12.0),
        ];
        let series = PriceSeries::new("600519", bars).unwrap();
        let engine = BacktestEngine::new(rules(), EngineConfig::new("600519", 100_000.0)).unwrap();
        let mut strat = Scripted::new(vec![(0, vec![Order::buy(Qty::Shares(100))])]);
        let res = engine.run(&series, &mut strat).unwrap();

        // One rejection (one-bar TTL: not retried on 01-08), no fills.
        assert!(res.trades.is_empty());
        assert_eq!(res.rejections.len(), 1);
        assert_eq!(res.rejections[0].reason, RejectReason::Suspended);
        // Equity constant at initial cash; marks stay stale-free here since
        // no position exists.
        assert!(res.equity.iter().all(|p| approx(p.equity, 100_000.0)));
    }

    #[test]
    fn limit_down_blocks_sell() {
        // Buy on day 1, then a sealed limit-down day blocks the exit.
        let bars = vec![
            Bar::flat(d("2025-01-06"), 10.0),
            Bar::flat(d("2025-01-07"), 10.0),
            Bar::flat(d("2025-01-08"), 9.0), // limit-down vs 10.0
            Bar::flat(d("2025-01-09"), 9.1),
        ];
        let series = PriceSeries::new("600519", bars).unwrap();
        let engine = BacktestEngine::new(rules(), EngineConfig::new("600519", 100_000.0)).unwrap();
        let mut strat = Scripted::new(vec![
            (0, vec![Order::buy(Qty::Shares(100))]),
            (1, vec![Order::sell(Qty::Max)]),
        ]);
        let res = engine.run(&series, &mut strat).unwrap();
        assert_eq!(res.trades.len(), 1); // only the buy
        assert_eq!(res.rejections.len(), 1);
        assert_eq!(res.rejections[0].reason, RejectReason::LimitDown);
        assert_eq!(res.rejections[0].date, d("2025-01-08"));
    }

    #[test]
    fn slippage_moves_fill_price_adversely() {
        let bars = vec![
            Bar::flat(d("2025-01-06"), 10.0),
            Bar::flat(d("2025-01-07"), 10.0),
            Bar::flat(d("2025-01-08"), 11.0),
        ];
        let series = PriceSeries::new("600519", bars).unwrap();
        let engine = BacktestEngine::new(
            rules(),
            EngineConfig::new("600519", 100_000.0).with_slippage_bps(10.0), // 0.10%
        )
        .unwrap();
        let mut strat = Scripted::new(vec![
            (0, vec![Order::buy(Qty::Shares(100))]),
            (1, vec![Order::sell(Qty::Max)]),
        ]);
        let res = engine.run(&series, &mut strat).unwrap();
        assert!(approx(res.trades[0].price, 10.0 * 1.001)); // buy pays more
                                                            // Sell order placed at bar 1 fills at bar 2's open (11.0).
        assert!(approx(res.trades[1].price, 11.0 * 0.999)); // sell receives less
    }

    #[test]
    fn star_board_lot_rules() {
        // STAR market: min 200 shares, then increments of 1.
        let bars = vec![
            Bar::flat(d("2025-01-06"), 10.0),
            Bar::flat(d("2025-01-07"), 10.0),
            Bar::flat(d("2025-01-08"), 10.0),
        ];
        let series = PriceSeries::new("688981", bars).unwrap();
        let engine = BacktestEngine::new(rules(), EngineConfig::new("688981", 100_000.0)).unwrap();
        let mut strat = Scripted::new(vec![
            (0, vec![Order::buy(Qty::Shares(250))]), // 200 + 50*1: valid
            (1, vec![Order::buy(Qty::Shares(150))]), // below min: rejected
        ]);
        let res = engine.run(&series, &mut strat).unwrap();
        assert_eq!(res.trades.len(), 1);
        assert_eq!(res.trades[0].shares, 250);
        assert_eq!(res.rejections.len(), 1);
        assert_eq!(res.rejections[0].reason, RejectReason::BelowMinLot);
    }

    #[test]
    fn max_buy_respects_cash_with_fees() {
        let bars = vec![
            Bar::flat(d("2025-01-06"), 10.0),
            Bar::flat(d("2025-01-07"), 10.0),
        ];
        let series = PriceSeries::new("600519", bars).unwrap();
        // 10_005 cash: 1000 shares at 10 = 10_000 + min commission 5 +
        // transfer 0.10 = 10_005.10 -> doesn't fit; the engine must shrink
        // one lot to 900 shares (9_000 + 5 + 0.09 = 9_005.09, fits).
        let engine = BacktestEngine::new(rules(), EngineConfig::new("600519", 10_005.0)).unwrap();
        let mut strat = Scripted::new(vec![(0, vec![Order::buy(Qty::Max)])]);
        let res = engine.run(&series, &mut strat).unwrap();
        assert_eq!(res.trades.len(), 1);
        assert_eq!(res.trades[0].shares, 900);

        // Cash below one minimum lot plus fees: rejected outright.
        let engine = BacktestEngine::new(rules(), EngineConfig::new("600519", 900.0)).unwrap();
        let mut strat = Scripted::new(vec![(0, vec![Order::buy(Qty::Max)])]);
        let res = engine.run(&series, &mut strat).unwrap();
        assert!(res.trades.is_empty());
        assert_eq!(res.rejections[0].reason, RejectReason::InsufficientCash);
    }

    #[test]
    fn determinism_same_inputs_same_trade_log() {
        // Non-trivial series + MA cross; two runs must be identical.
        let closes: Vec<f64> = (0..60)
            .map(|i| 10.0 + (i as f64 * 0.37).sin() * 2.0 + i as f64 * 0.05)
            .collect();
        let bars: Vec<Bar> = closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Bar::flat(d("2025-01-06") + chrono::Duration::days(i as i64), c))
            .collect();
        let series = PriceSeries::new("600519", bars).unwrap();
        let engine = BacktestEngine::new(rules(), EngineConfig::new("600519", 100_000.0)).unwrap();
        let r1 = engine
            .run(&series, &mut crate::strategy::MaCross::new(3, 10))
            .unwrap();
        let r2 = engine
            .run(&series, &mut crate::strategy::MaCross::new(3, 10))
            .unwrap();
        assert_eq!(r1.trades, r2.trades);
        assert_eq!(r1.equity, r2.equity);
        assert_eq!(r1.rejections, r2.rejections);
    }

    #[test]
    fn same_close_uses_current_close_for_fill() {
        // SameClose: order placed at bar 0 fills at bar 0's close.
        let bars = vec![
            Bar::new(d("2025-01-06"), 10.0, 10.5, 9.5, 10.2, 1e6, None, false),
            Bar::flat(d("2025-01-07"), 10.2),
        ];
        let series = PriceSeries::new("600519", bars).unwrap();
        let engine = BacktestEngine::new(
            rules(),
            EngineConfig::new("600519", 100_000.0).with_execution(ExecutionPolicy::SameClose),
        )
        .unwrap();
        let mut strat = Scripted::new(vec![(0, vec![Order::buy(Qty::Shares(100))])]);
        let res = engine.run(&series, &mut strat).unwrap();
        assert_eq!(res.trades.len(), 1);
        assert_eq!(res.trades[0].date, d("2025-01-06"));
        assert!(approx(res.trades[0].price, 10.2)); // close of bar 0
    }

    /// Strategy that panics out of the market on any visible single-day
    /// drop worse than −20% — the canonical "fake ex-date crash" detector.
    struct CrashGuard {
        worst_ratio: f64,
    }

    impl CrashGuard {
        fn new() -> Self {
            CrashGuard {
                worst_ratio: f64::INFINITY,
            }
        }
    }

    impl Strategy for CrashGuard {
        fn name(&self) -> &str {
            "crash_guard"
        }
        fn on_bar(&mut self, ctx: &StrategyContext, bar_index: usize) -> Vec<Order> {
            if bar_index == 0 {
                return vec![Order::buy(Qty::Max).with_reason("entry")];
            }
            let bars = ctx.bars();
            let ratio = bars[bar_index].close / bars[bar_index - 1].close;
            self.worst_ratio = self.worst_ratio.min(ratio);
            if ratio < 0.8 && ctx.position().shares > 0 {
                return vec![Order::sell(Qty::Max).with_reason("crash guard")];
            }
            vec![]
        }
    }

    /// Mid-series 10送10 split series (all bars flat O=H=L=C):
    ///
    /// | bar | date   | raw close | note                     |
    /// |-----|--------|-----------|--------------------------|
    /// | 0   | 01-06  | 10.0      |                          |
    /// | 1   | 01-07  | 10.4      | buy fills here (NextOpen)|
    /// | 2   | 01-08  | 10.8      |                          |
    /// | 3   | 01-09  | 5.5       | ex-date: X = 10.8/2 = 5.4|
    /// | 4   | 01-10  | 5.7       |                          |
    ///
    /// Under `QfqAsOf` the strategy sees, at bar 3, closes
    /// [5.0, 5.2, 5.4, 5.5] — no fake −49% crash, no exit. Under `Raw` it
    /// sees the 5.5/10.8 = 0.509 ratio and bails out; the resulting sell
    /// must still fill at the *raw* ex-date price (5.7 open of bar 4).
    fn split_series() -> (PriceSeries, Vec<astock_core::CorporateAction>) {
        let bars = vec![
            Bar::flat(d("2025-01-06"), 10.0),
            Bar::flat(d("2025-01-07"), 10.4),
            Bar::flat(d("2025-01-08"), 10.8),
            Bar::flat(d("2025-01-09"), 5.5),
            Bar::flat(d("2025-01-10"), 5.7),
        ];
        let actions = vec![astock_core::CorporateAction::new(d("2025-01-09"), 0.0, 1.0)];
        (PriceSeries::new("600519", bars).unwrap(), actions)
    }

    #[test]
    fn qfq_asof_hides_fake_split_crash_but_fills_raw() {
        let (series, actions) = split_series();

        // --- QfqAsOf: no fake crash, position rides through the ex-date ---
        let engine = BacktestEngine::new(
            rules(),
            EngineConfig::new("600519", 100_000.0)
                .with_adjustment(AdjustmentPolicy::QfqAsOf)
                .with_corporate_actions(actions.clone()),
        )
        .unwrap();
        let mut guard = CrashGuard::new();
        let res = engine.run(&series, &mut guard).unwrap();
        assert!(res.adjustment_warnings.is_empty());
        assert_eq!(res.trades.len(), 1, "no exit expected: {:?}", res.trades);
        assert_eq!(res.trades[0].side, TradeSide::Buy);
        // Fill uses the raw bar-1 open, not an adjusted price.
        assert!(approx(res.trades[0].price, 10.4));
        // The worst ratio the strategy ever saw is 5.5/5.4 ≈ +1.9%, not −49%.
        assert!(approx(guard.worst_ratio, 5.5 / 5.4));

        // --- Raw: the crash guard fires on the fake −49%, fill still raw ---
        let engine = BacktestEngine::new(rules(), EngineConfig::new("600519", 100_000.0)).unwrap();
        let mut guard = CrashGuard::new();
        let res = engine.run(&series, &mut guard).unwrap();
        assert_eq!(res.trades.len(), 2);
        assert_eq!(res.trades[1].side, TradeSide::Sell);
        assert_eq!(res.trades[1].date, d("2025-01-10"));
        assert!(approx(res.trades[1].price, 5.7)); // raw ex-date price
        assert!(approx(guard.worst_ratio, 5.5 / 10.8));
    }

    /// The reference strategies must run unchanged under both policies,
    /// including across a mid-series split.
    #[test]
    fn reference_strategies_run_under_both_policies() {
        let (series, actions) = split_series();
        for adjustment in [AdjustmentPolicy::Raw, AdjustmentPolicy::QfqAsOf] {
            let config = EngineConfig::new("600519", 100_000.0)
                .with_adjustment(adjustment)
                .with_corporate_actions(actions.clone());
            let engine = BacktestEngine::new(rules(), config).unwrap();
            let r1 = engine
                .run(&series, &mut crate::strategy::MaCross::new(1, 3))
                .unwrap();
            let r2 = engine
                .run(&series, &mut crate::strategy::TurtleBreakout::new(2, 1))
                .unwrap();
            assert!(r1.adjustment_warnings.is_empty());
            assert!(r2.adjustment_warnings.is_empty());
            assert_eq!(r1.equity.len(), series.len());
            assert_eq!(r2.equity.len(), series.len());
        }
    }

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        #[derive(Debug, Clone, Copy)]
        enum Action {
            None,
            BuyMax,
            SellAll,
        }

        struct RandomLongOnly {
            script: Vec<Action>,
        }

        impl crate::strategy::Strategy for RandomLongOnly {
            fn name(&self) -> &str {
                "random_long_only"
            }
            fn on_bar(&mut self, _ctx: &StrategyContext, bar_index: usize) -> Vec<Order> {
                match self.script.get(bar_index) {
                    Some(Action::BuyMax) => vec![Order::buy(Qty::Max)],
                    Some(Action::SellAll) => vec![Order::sell(Qty::Max)],
                    _ => vec![],
                }
            }
        }

        fn series_from_moves(moves: &[i32]) -> Vec<Bar> {
            // Random walk within +/-8% per bar, floor at 1.0, so prices stay
            // positive and mostly inside the 10% limit band.
            let mut price = 10.0;
            moves
                .iter()
                .enumerate()
                .map(|(i, &m)| {
                    price = (price * (1.0 + m as f64 / 100.0)).max(1.0);
                    Bar::flat(d("2025-01-06") + chrono::Duration::days(i as i64), price)
                })
                .collect()
        }

        fn action_strategy() -> impl proptest::strategy::Strategy<Value = Action> {
            proptest::prop_oneof![
                6 => Just(Action::None),
                2 => Just(Action::BuyMax),
                2 => Just(Action::SellAll),
            ]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            #[test]
            fn long_only_equity_never_negative(
                moves in proptest::collection::vec(-8i32..=8, 10..60),
                script in proptest::collection::vec(action_strategy(), 10..60),
            ) {
                let bars = series_from_moves(&moves);
                let series = PriceSeries::new("600519", bars).unwrap();
                let engine =
                    BacktestEngine::new(rules(), EngineConfig::new("600519", 100_000.0)).unwrap();
                let mut strat = RandomLongOnly { script };
                let res = engine.run(&series, &mut strat).unwrap();

                for p in &res.equity {
                    proptest::prop_assert!(p.cash >= -1e-6, "cash went negative: {}", p.cash);
                    proptest::prop_assert!(p.equity >= -1e-6, "equity went negative: {}", p.equity);
                    proptest::prop_assert!(p.market_value >= 0.0);
                }
            }

            #[test]
            fn cash_ledger_conserves(
                moves in proptest::collection::vec(-8i32..=8, 10..60),
                script in proptest::collection::vec(action_strategy(), 10..60),
            ) {
                let bars = series_from_moves(&moves);
                let series = PriceSeries::new("600519", bars).unwrap();
                let engine =
                    BacktestEngine::new(rules(), EngineConfig::new("600519", 100_000.0)).unwrap();
                let mut strat = RandomLongOnly { script };
                let res = engine.run(&series, &mut strat).unwrap();

                // Ledger identity: initial cash minus buy outflows plus sell
                // inflows minus fees equals final cash, exactly as booked.
                let buys: f64 = res
                    .trades
                    .iter()
                    .filter(|f| f.side == TradeSide::Buy)
                    .map(|f| f.amount)
                    .sum();
                let sells: f64 = res
                    .trades
                    .iter()
                    .filter(|f| f.side == TradeSide::Sell)
                    .map(|f| f.amount)
                    .sum();
                let fees = res.total_fees();
                let last = res.equity.last().unwrap();
                let expected_cash = 100_000.0 - buys + sells - fees;
                proptest::prop_assert!((last.cash - expected_cash).abs() < 1e-6);
                // Equity identity: cash + shares * mark.
                let expected_equity = last.cash + last.shares as f64 * last.close;
                proptest::prop_assert!((last.equity - expected_equity).abs() < 1e-6);
                // Cumulative fees on the curve equal the sum over fills.
                proptest::prop_assert!((last.fees_cum - fees).abs() < 1e-6);
            }
        }
    }
}
