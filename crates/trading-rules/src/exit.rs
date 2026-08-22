//! Exit and risk-control rules, ported from the niuone project's framework
//! semantics (see `docs/niuone-analysis.md` §3.2). Only the deterministic
//! *structure* is borrowed — every threshold is an explicit configuration
//! parameter; no magic numbers are hardcoded.
//!
//! Five capabilities, all deterministic and side-effect free:
//!
//! 1. [`ScaledTakeProfit`] — R-multiple staged take profit. One R is defined
//!    by the entry price and the frozen initial stop; stages (e.g. 1R/2R/3R)
//!    each realize a configurable fraction of the original position.
//! 2. [`StructuralStop`] — the structural stop is frozen at entry and can
//!    only ever move *up* (e.g. to breakeven after a partial take profit).
//! 3. [`PeakTrailingStop`] — peak-close minus `k * ATR` trailing stop, with a
//!    [`WilderAtr`] implementation (no external TA dependency).
//! 4. [`risk_order_ceiling`] — dynamic position sizing: seven constraints
//!    (per-trade risk budget, total exposure cap, single-name cap,
//!    stop-distance inversion, available cash, minimum lot, liquidity cap),
//!    the minimum wins, rounded down to whole lots. Pure function.
//! 5. [`same_bar_conservative`] — conservative same-bar arbitration for
//!    backtests: when one daily bar touches both the stop and the target,
//!    the stop is assumed to fill first (daily bars cannot recover the true
//!    intraday order).
//!
//! Stateful trackers ([`WilderAtr`], [`StructuralStop`], [`ScaledTakeProfit`],
//! [`PeakTrailingStop`]) are deterministic state machines: same input
//! sequence, same outputs, no I/O, no clocks, no randomness.

/// OHLC triple used as ATR input. Kept minimal so any crate can feed it
/// without depending on a shared bar type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ohlc {
    /// Highest price of the bar.
    pub high: f64,
    /// Lowest price of the bar.
    pub low: f64,
    /// Closing price of the bar.
    pub close: f64,
}

impl Ohlc {
    /// Build a bar triple.
    pub fn new(high: f64, low: f64, close: f64) -> Self {
        Ohlc { high, low, close }
    }
}

/// True range of a bar against the previous close (None for the first bar,
/// where TR = high - low by definition).
fn true_range(high: f64, low: f64, prev_close: Option<f64>) -> f64 {
    match prev_close {
        Some(pc) => (high - low).max((high - pc).abs()).max((low - pc).abs()),
        None => high - low,
    }
}

/// Wilder's Average True Range, incremental.
///
/// Seeding: the first `period` true ranges are averaged with a simple mean;
/// afterwards the Wilder smoothing recurrence applies:
/// `ATR_t = (ATR_{t-1} * (period - 1) + TR_t) / period`.
///
/// Note: niuone itself uses a *simple* mean of TR for its ATR14; Wilder
/// smoothing is chosen here because it is the standard, incrementally
/// computable form and the difference is a parameter-free convention, not a
/// tuned threshold.
#[derive(Debug, Clone)]
pub struct WilderAtr {
    period: usize,
    seen: usize,
    tr_sum: f64,
    atr: Option<f64>,
    prev_close: Option<f64>,
}

impl WilderAtr {
    /// New tracker with the given period (>= 1).
    pub fn new(period: usize) -> Self {
        assert!(period >= 1, "ATR period must be >= 1");
        WilderAtr {
            period,
            seen: 0,
            tr_sum: 0.0,
            atr: None,
            prev_close: None,
        }
    }

    /// Feed one bar; returns the ATR once seeded (after `period` bars).
    pub fn update(&mut self, bar: Ohlc) -> Option<f64> {
        let tr = true_range(bar.high, bar.low, self.prev_close);
        self.prev_close = Some(bar.close);
        self.seen += 1;
        if self.seen < self.period {
            self.tr_sum += tr;
            return None;
        }
        self.atr = Some(if self.seen == self.period {
            (self.tr_sum + tr) / self.period as f64
        } else {
            let prev = self.atr.unwrap_or(0.0);
            (prev * (self.period - 1) as f64 + tr) / self.period as f64
        });
        self.atr
    }

    /// Current ATR value, if seeded.
    pub fn value(&self) -> Option<f64> {
        self.atr
    }
}

/// Batch convenience: Wilder ATR over a slice of bars, `None` if fewer than
/// `period` bars.
pub fn wilder_atr(bars: &[Ohlc], period: usize) -> Option<f64> {
    let mut atr = WilderAtr::new(period);
    let mut last = None;
    for &b in bars {
        last = atr.update(b);
    }
    last
}

/// One-R geometry defined at entry: `risk_per_share = entry - initial_stop`.
///
/// Construction fails (returns `None`) unless `0 < initial_stop < entry`,
/// mirroring niuone's `0 < stop_price < entry_price` guard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RMultiple {
    entry_price: f64,
    initial_stop: f64,
}

impl RMultiple {
    /// Build from entry price and frozen initial stop.
    pub fn new(entry_price: f64, initial_stop: f64) -> Option<Self> {
        if entry_price.is_finite()
            && initial_stop.is_finite()
            && 0.0 < initial_stop
            && initial_stop < entry_price
        {
            Some(RMultiple {
                entry_price,
                initial_stop,
            })
        } else {
            None
        }
    }

    /// Entry price.
    pub fn entry_price(&self) -> f64 {
        self.entry_price
    }

    /// Frozen initial stop.
    pub fn initial_stop(&self) -> f64 {
        self.initial_stop
    }

    /// Per-share risk: `entry - initial_stop` (> 0 by construction).
    pub fn risk_per_share(&self) -> f64 {
        self.entry_price - self.initial_stop
    }

    /// Price at `r` multiples of the initial risk above the entry.
    pub fn target_price(&self, r: f64) -> f64 {
        self.entry_price + r * self.risk_per_share()
    }
}

/// One staged take-profit leg: realize `ratio` of the *original* position
/// when the price reaches `r` R.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaleOutStage {
    /// R multiple at which the leg triggers (e.g. 1.0, 2.0, 3.0).
    pub r: f64,
    /// Fraction of the original position to sell (0, 1].
    pub ratio: f64,
}

/// Why a [`ScaledTakeProfit`] configuration is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleOutConfigError {
    /// At least one stage is required.
    Empty,
    /// Every stage needs a positive R.
    NonPositiveR,
    /// Stages must be strictly ascending in R.
    NotAscending,
    /// Every ratio must be in (0, 1].
    BadRatio,
    /// Ratios must sum to at most 1 (you cannot sell more than you own).
    RatiosExceedOne,
}

/// R-multiple staged take profit (分段止盈).
///
/// niuone semantics: `target = entry + R * (entry - frozen_initial_stop)`;
/// when the observed price (intraday high, or close — caller's choice)
/// reaches the next stage's target, that leg fires exactly once. Stages fire
/// in ascending R order; a price jump can fire at most one stage per call —
/// call `on_price` again with the same price to drain further stages.
#[derive(Debug, Clone)]
pub struct ScaledTakeProfit {
    stages: Vec<ScaleOutStage>,
    next: usize,
}

impl ScaledTakeProfit {
    /// Validate and build. Stages must be strictly ascending in R and the
    /// ratios must sum to at most 1.
    pub fn new(stages: Vec<ScaleOutStage>) -> std::result::Result<Self, ScaleOutConfigError> {
        if stages.is_empty() {
            return Err(ScaleOutConfigError::Empty);
        }
        let mut sum = 0.0;
        for (i, s) in stages.iter().enumerate() {
            if !s.r.is_finite() || s.r <= 0.0 {
                return Err(ScaleOutConfigError::NonPositiveR);
            }
            if i > 0 && s.r <= stages[i - 1].r {
                return Err(ScaleOutConfigError::NotAscending);
            }
            if !s.ratio.is_finite() || s.ratio <= 0.0 || s.ratio > 1.0 {
                return Err(ScaleOutConfigError::BadRatio);
            }
            sum += s.ratio;
        }
        if sum > 1.0 + 1e-9 {
            return Err(ScaleOutConfigError::RatiosExceedOne);
        }
        Ok(ScaledTakeProfit { stages, next: 0 })
    }

    /// The pending stage, if any.
    pub fn next_stage(&self) -> Option<ScaleOutStage> {
        self.stages.get(self.next).copied()
    }

    /// Whether all stages have fired.
    pub fn exhausted(&self) -> bool {
        self.next >= self.stages.len()
    }

    /// Observe a price (intraday high or close); returns the stage that
    /// fired, if its target was reached.
    pub fn on_price(&mut self, rm: &RMultiple, observed: f64) -> Option<ScaleOutStage> {
        let stage = self.next_stage()?;
        if observed >= rm.target_price(stage.r) {
            self.next += 1;
            Some(stage)
        } else {
            None
        }
    }
}

/// Structural stop (结构止损): frozen at entry, only ever raised, never
/// lowered. niuone freezes the breakout pivot / structure low at entry and
/// later raises it to breakeven after the first partial take profit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StructuralStop {
    stop: f64,
}

impl StructuralStop {
    /// Freeze the stop at `stop`.
    pub fn new(stop: f64) -> Self {
        assert!(stop.is_finite() && stop > 0.0, "stop must be positive");
        StructuralStop { stop }
    }

    /// Current stop price.
    pub fn price(&self) -> f64 {
        self.stop
    }

    /// Raise the stop to `candidate` if it is higher; a lower candidate is
    /// silently ignored (只上移不下移).
    pub fn raise(&mut self, candidate: f64) {
        if candidate.is_finite() && candidate > self.stop {
            self.stop = candidate;
        }
    }

    /// Intraday check: the stop triggers when the bar's low trades through
    /// it (`low < stop`). On a trigger, returns the conservative fill
    /// reference `min(open, stop)` — if the market opens below the stop you
    /// get the open, otherwise the stop price.
    pub fn check(&self, open: f64, low: f64) -> Option<f64> {
        if low < self.stop {
            Some(open.min(self.stop))
        } else {
            None
        }
    }
}

/// Peak-drawdown trailing stop (峰值回撤跟踪止损):
/// `stop_level = highest_close_since_entry - atr_mult * ATR`.
///
/// niuone arms the trail only once it sits above the entry price (a trail
/// below entry would duplicate the structural stop); that gate is
/// configurable via [`PeakTrailingStop::with_entry_gate`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeakTrailingStop {
    atr_mult: f64,
    peak_close: f64,
    entry_gate: Option<f64>,
}

impl PeakTrailingStop {
    /// New tracker; the peak starts at the entry price.
    pub fn new(atr_mult: f64, entry_price: f64) -> Self {
        assert!(
            atr_mult.is_finite() && atr_mult > 0.0,
            "atr_mult must be positive"
        );
        assert!(
            entry_price.is_finite() && entry_price > 0.0,
            "entry_price must be positive"
        );
        PeakTrailingStop {
            atr_mult,
            peak_close: entry_price,
            entry_gate: None,
        }
    }

    /// Only trigger while the trail level is above the entry price
    /// (niuone's `trailing_stop > entry_price` guard).
    pub fn with_entry_gate(mut self, enabled: bool) -> Self {
        self.entry_gate = enabled.then_some(self.peak_close);
        self
    }

    /// Highest close observed since entry.
    pub fn peak(&self) -> f64 {
        self.peak_close
    }

    /// Current trail level for the given ATR.
    pub fn stop_level(&self, atr: f64) -> f64 {
        self.peak_close - self.atr_mult * atr
    }

    /// Update with today's close and ATR. Returns `Some(level)` when the
    /// close has fallen to/below the trail level (and the entry gate, if
    /// enabled, passes).
    pub fn update(&mut self, close: f64, atr: f64) -> Option<f64> {
        if close > self.peak_close {
            self.peak_close = close;
        }
        let level = self.stop_level(atr);
        let gate_ok = self.entry_gate.is_none_or(|entry| level > entry);
        if gate_ok && close <= level {
            Some(level)
        } else {
            None
        }
    }
}

/// Inputs for [`risk_order_ceiling`]. All money values in CNY.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SizingInputs {
    /// Intended buy price per share.
    pub price: f64,
    /// Total account equity (cash + market value).
    pub equity: f64,
    /// Available cash.
    pub cash: f64,
    /// Current market value of the existing position in this symbol (0 for a
    /// fresh entry).
    pub current_position_value: f64,
    /// Current total market value of all holdings.
    pub total_market_value: f64,
    /// Distance from the buy price to the protective stop, per share
    /// (`price - stop > 0`); the basis of the stop-distance inversion.
    pub stop_distance_per_share: f64,
    /// Average daily volume in shares, for the liquidity cap; `None`
    /// disables that constraint.
    pub avg_daily_volume_shares: Option<f64>,
}

/// Constraint parameters for [`risk_order_ceiling`]. Percentages are
/// fractions of equity (0.01 = 1%).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SizingConstraints {
    /// Per-trade risk budget as a fraction of equity (单笔风险预算). The
    /// budget in CNY is `per_trade_risk_pct * equity`.
    pub per_trade_risk_pct: f64,
    /// Total exposure cap as a fraction of equity (总仓位上限).
    pub total_position_cap_pct: f64,
    /// Single-name exposure cap as a fraction of equity (单标的仓位上限).
    pub single_name_cap_pct: f64,
    /// Max fraction of the average daily volume the order may represent
    /// (流动性上限); `None` disables it.
    pub liquidity_participation_pct: Option<f64>,
    /// Board lot size in shares (最小手数, 100 on the main boards).
    pub lot_size: u32,
    /// Estimated buy-side fee rate (for the cash-after-fees shrink loop).
    pub buy_fee_rate: f64,
    /// Estimated buy-side minimum fee in CNY.
    pub buy_min_fee: f64,
    /// Fraction of equity that must remain as cash after the trade
    /// (现金储备); `None` disables the reserve check.
    pub cash_reserve_pct: Option<f64>,
}

/// Which constraint bound the order size (audit trail).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BindingConstraint {
    /// Inputs were degenerate; no order is permitted.
    InvalidInputs,
    /// Per-trade risk budget, gross-value view: `risk_cny / (stop_dist / price)`.
    PerTradeRiskBudget,
    /// Stop-distance inversion, share view: `risk_cny / stop_distance`.
    /// Same budget as [`BindingConstraint::PerTradeRiskBudget`] seen in share
    /// units; both are reported so the audit shows the full derivation.
    StopDistance,
    /// Total exposure cap.
    TotalExposure,
    /// Single-name exposure cap.
    SingleName,
    /// Liquidity participation cap.
    Liquidity,
    /// Available cash (before the fee shrink loop).
    Cash,
    /// Cash after estimated fees (shrink loop engaged).
    CashAfterFees,
    /// Required cash reserve after the trade (shrink loop engaged).
    CashReserveAfterFees,
}

/// Result of [`risk_order_ceiling`].
#[derive(Debug, Clone, PartialEq)]
pub struct SizingResult {
    /// Largest permitted whole-lot share count (0 = no order allowed).
    pub shares: u32,
    /// `shares * price`.
    pub gross: f64,
    /// Constraints that bound the size, sorted (may be several ties).
    pub binding: Vec<BindingConstraint>,
}

/// Dynamic position sizing (动态定仓): the largest whole-lot order permitted
/// by every constraint.
///
/// The seven caps, each expressed as a gross CNY ceiling and minimized:
///
/// 1. per-trade risk budget — `risk_cny / loss_pct` where
///    `loss_pct = stop_distance / price`;
/// 2. total exposure — `total_cap * equity - total_market_value`;
/// 3. single-name exposure — `single_cap * equity - current_position_value`;
/// 4. stop-distance inversion — `floor(risk_cny / stop_distance) * price`
///    (the share-unit view of constraint 1; kept separate for audit);
/// 5. available cash;
/// 6. minimum lot — the result is rounded *down* to a whole lot, and a
///    result below one lot means no order;
/// 7. liquidity — `participation * avg_daily_volume * price`, if configured.
///
/// After the minimum is rounded down to lots, a shrink loop (niuone's
/// fail-closed pattern) drops one lot at a time while estimated fees push
/// the cost above cash, or the post-trade cash reserve would be breached.
pub fn risk_order_ceiling(inputs: &SizingInputs, cons: &SizingConstraints) -> SizingResult {
    let invalid = || SizingResult {
        shares: 0,
        gross: 0.0,
        binding: vec![BindingConstraint::InvalidInputs],
    };
    let SizingInputs {
        price,
        equity,
        cash,
        current_position_value,
        total_market_value,
        stop_distance_per_share,
        avg_daily_volume_shares,
    } = *inputs;
    if !price.is_finite()
        || !equity.is_finite()
        || !cash.is_finite()
        || !stop_distance_per_share.is_finite()
        || price <= 0.0
        || equity <= 0.0
        || cash < 0.0
        || stop_distance_per_share <= 0.0
        || cons.lot_size == 0
    {
        return invalid();
    }

    let risk_cny = (cons.per_trade_risk_pct * equity).max(0.0);
    let loss_pct = stop_distance_per_share / price;
    let mut caps: Vec<(BindingConstraint, f64)> = vec![
        (
            BindingConstraint::PerTradeRiskBudget,
            (risk_cny / loss_pct).max(0.0),
        ),
        (
            BindingConstraint::StopDistance,
            (risk_cny / stop_distance_per_share).max(0.0) * price,
        ),
        (
            BindingConstraint::TotalExposure,
            (cons.total_position_cap_pct * equity - total_market_value).max(0.0),
        ),
        (
            BindingConstraint::SingleName,
            (cons.single_name_cap_pct * equity - current_position_value).max(0.0),
        ),
        (BindingConstraint::Cash, cash.max(0.0)),
    ];
    if let (Some(part), Some(vol)) = (cons.liquidity_participation_pct, avg_daily_volume_shares) {
        caps.push((BindingConstraint::Liquidity, (part * vol * price).max(0.0)));
    }

    let gross_limit = caps.iter().map(|(_, v)| *v).fold(f64::INFINITY, f64::min);
    let mut binding: Vec<BindingConstraint> = caps
        .iter()
        .filter(|(_, v)| (*v - gross_limit).abs() <= 1e-7)
        .map(|(c, _)| *c)
        .collect();

    // Round down to whole lots (constraint 6).
    let lot = cons.lot_size;
    let mut shares = (gross_limit / price / lot as f64).floor().max(0.0) as u32 * lot;

    // Fail-closed shrink loop: estimated fees must fit in cash, and the
    // post-trade cash reserve must hold.
    let est_fee = |gross: f64| (gross * cons.buy_fee_rate).max(cons.buy_min_fee);
    let mut reduced_for_cash = false;
    let mut reduced_for_reserve = false;
    while shares > 0 {
        let gross = shares as f64 * price;
        let fee = est_fee(gross);
        if gross + fee > cash + 1e-9 {
            reduced_for_cash = true;
            shares -= lot;
            continue;
        }
        if let Some(reserve_pct) = cons.cash_reserve_pct {
            let equity_after = (equity - fee).max(0.0);
            let cash_after = cash - gross - fee;
            let reserve_needed = reserve_pct.clamp(0.0, 1.0) * equity_after;
            if cash_after + 1e-9 < reserve_needed {
                reduced_for_reserve = true;
                shares -= lot;
                continue;
            }
        }
        break;
    }
    if reduced_for_cash || reduced_for_reserve {
        binding.clear();
        if reduced_for_cash {
            binding.push(BindingConstraint::CashAfterFees);
        }
        if reduced_for_reserve {
            binding.push(BindingConstraint::CashReserveAfterFees);
        }
    }
    binding.sort();
    binding.dedup();

    SizingResult {
        shares,
        gross: shares as f64 * price,
        binding,
    }
}

/// Same-bar arbitration outcome for [`same_bar_conservative`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameBarTouch {
    /// Neither level was touched.
    Neither,
    /// Only the stop was touched.
    StopOnly,
    /// Only the target was touched.
    TargetOnly,
    /// Both were touched; daily bars cannot recover the intraday order, so
    /// the conservative assumption is that the stop filled first.
    BothStopFirst,
}

/// Conservative same-bar stop/target arbitration for backtests
/// (同 K 线"止损优先").
///
/// A daily bar's low confirms the stop traded (`low <= stop`); its high
/// confirms the target traded (`high >= target`). When both are true the
/// fill sequence is unknowable from daily data, so the stop is assumed to
/// have filled first — never the optimistic alternative.
pub fn same_bar_conservative(low: f64, high: f64, stop: f64, target: f64) -> SameBarTouch {
    let stop_hit = low <= stop;
    let target_hit = high >= target;
    match (stop_hit, target_hit) {
        (true, true) => SameBarTouch::BothStopFirst,
        (true, false) => SameBarTouch::StopOnly,
        (false, true) => SameBarTouch::TargetOnly,
        (false, false) => SameBarTouch::Neither,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    // ---------- Wilder ATR ----------

    /// Hand-computed Wilder ATR, period 3:
    /// bars:        TR:
    /// (12,10,11)   2                (no prev close -> high-low)
    /// (13,10,12)   max(3,2,1) = 3
    /// (14,12,13)   max(2,2,1) = 2   -> seed ATR = (2+3+2)/3 = 7/3
    /// (13,11,12)   max(2,0,2) = 2   -> ATR = (7/3*2 + 2)/3 = 20/9
    #[test]
    fn wilder_atr_golden() {
        let bars = [
            Ohlc::new(12.0, 10.0, 11.0),
            Ohlc::new(13.0, 10.0, 12.0),
            Ohlc::new(14.0, 12.0, 13.0),
            Ohlc::new(13.0, 11.0, 12.0),
        ];
        let mut atr = WilderAtr::new(3);
        assert_eq!(atr.update(bars[0]), None);
        assert_eq!(atr.update(bars[1]), None);
        let seeded = atr.update(bars[2]).unwrap();
        assert!(approx(seeded, 7.0 / 3.0), "{seeded}");
        let next = atr.update(bars[3]).unwrap();
        assert!(approx(next, 20.0 / 9.0), "{next}");
        assert!(approx(atr.value().unwrap(), 20.0 / 9.0));
        // Batch form agrees with the incremental form.
        assert!(approx(wilder_atr(&bars, 3).unwrap(), 20.0 / 9.0));
        assert_eq!(wilder_atr(&bars[..2], 3), None);
    }

    proptest! {
        /// ATR is deterministic, non-negative, and bounded by the min/max
        /// true range once seeded.
        #[test]
        fn wilder_atr_properties(
            period in 1usize..=10,
            seed in 1u64..=1_000_000,
        ) {
            // Deterministic pseudo-random walk from the seed (no rand dep).
            let mut x = seed;
            let mut next = move || {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((x >> 33) % 1000) as f64 / 100.0
            };
            let mut bars = Vec::new();
            let mut close = 10.0 + next();
            for _ in 0..30 {
                let h = close + next();
                let l = (close - next()).max(0.01);
                close = l + next() * (h - l);
                bars.push(Ohlc::new(h, l, close));
            }
            let run = |bars: &[Ohlc]| {
                let mut atr = WilderAtr::new(period);
                let mut trs = Vec::new();
                let mut prev: Option<f64> = None;
                let mut last = None;
                for &b in bars {
                    trs.push(true_range(b.high, b.low, prev));
                    prev = Some(b.close);
                    last = atr.update(b);
                }
                (last, trs)
            };
            let (a1, trs) = run(&bars);
            let (a2, _) = run(&bars);
            prop_assert_eq!(a1, a2, "deterministic");
            let atr = a1.unwrap();
            prop_assert!(atr >= 0.0);
            let window = &trs[trs.len() - period..];
            let lo = window.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = window.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            // ATR (a smoothed average) must sit within the historical TR range.
            let all_lo = trs.iter().copied().fold(f64::INFINITY, f64::min);
            let all_hi = trs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            prop_assert!(atr >= all_lo - 1e-9 && atr <= all_hi + 1e-9,
                "atr {atr} outside [{all_lo}, {all_hi}] (last window [{lo}, {hi}])");
        }
    }

    // ---------- R multiples & staged take profit ----------

    #[test]
    fn r_multiple_golden() {
        // Entry 10.0, initial stop 9.5 -> 1R = 0.5 CNY.
        let rm = RMultiple::new(10.0, 9.5).unwrap();
        assert!(approx(rm.risk_per_share(), 0.5));
        assert!(approx(rm.target_price(1.0), 10.5));
        assert!(approx(rm.target_price(2.0), 11.0));
        assert!(approx(rm.target_price(3.0), 11.5));
        // Invalid geometries rejected.
        assert_eq!(RMultiple::new(10.0, 10.0), None);
        assert_eq!(RMultiple::new(10.0, 10.5), None);
        assert_eq!(RMultiple::new(10.0, -1.0), None);
        assert_eq!(RMultiple::new(f64::NAN, 9.5), None);
    }

    #[test]
    fn scaled_take_profit_golden() {
        let rm = RMultiple::new(10.0, 9.5).unwrap();
        let mut tp = ScaledTakeProfit::new(vec![
            ScaleOutStage {
                r: 1.0,
                ratio: 0.45,
            },
            ScaleOutStage {
                r: 2.0,
                ratio: 0.35,
            },
            ScaleOutStage {
                r: 3.0,
                ratio: 0.20,
            },
        ])
        .unwrap();
        // Below 1R: nothing.
        assert_eq!(tp.on_price(&rm, 10.49), None);
        // 1R reached: first leg fires exactly once.
        assert_eq!(
            tp.on_price(&rm, 10.5),
            Some(ScaleOutStage {
                r: 1.0,
                ratio: 0.45
            })
        );
        assert_eq!(tp.on_price(&rm, 10.9), None);
        // A jump straight past 2R fires the 2R leg; the next call with the
        // same price drains the 3R leg too.
        assert_eq!(
            tp.on_price(&rm, 11.6),
            Some(ScaleOutStage {
                r: 2.0,
                ratio: 0.35
            })
        );
        assert_eq!(
            tp.on_price(&rm, 11.6),
            Some(ScaleOutStage {
                r: 3.0,
                ratio: 0.20
            })
        );
        assert!(tp.exhausted());
        assert_eq!(tp.on_price(&rm, 99.0), None);
    }

    #[test]
    fn scaled_take_profit_validation() {
        assert_eq!(
            ScaledTakeProfit::new(vec![]).unwrap_err(),
            ScaleOutConfigError::Empty
        );
        assert_eq!(
            ScaledTakeProfit::new(vec![ScaleOutStage { r: 0.0, ratio: 0.5 }]).unwrap_err(),
            ScaleOutConfigError::NonPositiveR
        );
        assert_eq!(
            ScaledTakeProfit::new(vec![
                ScaleOutStage { r: 2.0, ratio: 0.5 },
                ScaleOutStage { r: 1.0, ratio: 0.5 },
            ])
            .unwrap_err(),
            ScaleOutConfigError::NotAscending
        );
        assert_eq!(
            ScaledTakeProfit::new(vec![ScaleOutStage { r: 1.0, ratio: 0.0 }]).unwrap_err(),
            ScaleOutConfigError::BadRatio
        );
        assert_eq!(
            ScaledTakeProfit::new(vec![
                ScaleOutStage { r: 1.0, ratio: 0.6 },
                ScaleOutStage { r: 2.0, ratio: 0.6 },
            ])
            .unwrap_err(),
            ScaleOutConfigError::RatiosExceedOne
        );
    }

    // ---------- Structural stop ----------

    #[test]
    fn structural_stop_golden() {
        let mut stop = StructuralStop::new(9.5);
        // Only ever moves up.
        stop.raise(9.4);
        assert!(approx(stop.price(), 9.5));
        stop.raise(10.0); // breakeven after partial TP
        assert!(approx(stop.price(), 10.0));
        stop.raise(9.99);
        assert!(approx(stop.price(), 10.0));
        // Low above stop: no trigger. Low through stop: trigger with
        // conservative fill reference min(open, stop).
        assert_eq!(stop.check(10.5, 10.05), None);
        assert_eq!(stop.check(10.5, 9.99), Some(10.0)); // fills at the stop
        assert_eq!(stop.check(9.8, 9.7), Some(9.8)); // gapped below: the open
    }

    proptest! {
        /// The stop is monotone non-decreasing under any raise sequence.
        #[test]
        fn structural_stop_monotone(
            initial in 1.0f64..1000.0,
            raises in proptest::collection::vec(0.01f64..2000.0, 0..20),
        ) {
            let mut stop = StructuralStop::new(initial);
            let mut last = initial;
            for r in raises {
                stop.raise(r);
                prop_assert!(stop.price() >= last);
                last = stop.price();
            }
        }
    }

    // ---------- Peak trailing stop ----------

    /// Hand-computed: entry 10.0, ATR constant 1.0, mult 2.0.
    /// closes 10.5, 11.0, 10.6: peak 11.0 -> level 9.0 (below entry gate).
    /// With the gate on, no trigger while level <= entry (10.0).
    #[test]
    fn peak_trailing_golden() {
        let mut trail = PeakTrailingStop::new(2.0, 10.0).with_entry_gate(true);
        assert_eq!(trail.update(10.5, 1.0), None); // level 8.5, gated
        assert_eq!(trail.update(11.0, 1.0), None); // level 9.0, gated
        assert_eq!(trail.update(12.5, 1.0), None); // level 10.5 > entry, 12.5 > 10.5
        assert_eq!(trail.update(11.0, 1.0), None); // 11.0 > 10.5
                                                   // Close falls to the trail: 10.5 <= 10.5 triggers.
        assert_eq!(trail.update(10.5, 1.0), Some(10.5));
        assert!(approx(trail.peak(), 12.5));
    }

    #[test]
    fn peak_trailing_without_gate_triggers_below_entry() {
        let mut trail = PeakTrailingStop::new(2.0, 10.0);
        // level = 10.5 - 2 = 8.5; close 10.5 > 8.5 -> no trigger.
        assert_eq!(trail.update(10.5, 1.0), None);
        // level = 10.5 - 2*5 = 0.5; close 8.0 > 0.5 -> no trigger.
        assert_eq!(trail.update(8.0, 5.0), None);
        // level = 10.5 - 2*3 = 4.5; close 4.5 <= 4.5 -> trigger.
        assert_eq!(trail.update(4.5, 3.0), Some(4.5));
    }

    proptest! {
        /// Trail level never exceeds the peak; a trigger implies close <= level.
        #[test]
        fn peak_trailing_properties(
            entry in 1.0f64..1000.0,
            mult in 0.5f64..5.0,
            moves in proptest::collection::vec((0.5f64..1.5, 0.1f64..3.0), 1..30),
        ) {
            let mut trail = PeakTrailingStop::new(mult, entry).with_entry_gate(true);
            let mut close = entry;
            for (k, atr) in moves {
                close *= k;
                let level_before = trail.stop_level(atr);
                prop_assert!(level_before <= trail.peak());
                if let Some(level) = trail.update(close, atr) {
                    prop_assert!(close <= level + 1e-9);
                    prop_assert!(level > entry);
                }
            }
        }
    }

    // ---------- Dynamic sizing ----------

    fn base_inputs() -> SizingInputs {
        SizingInputs {
            price: 10.0,
            equity: 1_000_000.0,
            cash: 400_000.0,
            current_position_value: 0.0,
            total_market_value: 500_000.0,
            stop_distance_per_share: 0.5,
            avg_daily_volume_shares: None,
        }
    }

    fn base_cons() -> SizingConstraints {
        SizingConstraints {
            per_trade_risk_pct: 0.01,
            total_position_cap_pct: 0.70,
            single_name_cap_pct: 0.20,
            liquidity_participation_pct: None,
            lot_size: 100,
            buy_fee_rate: 0.00025,
            buy_min_fee: 5.0,
            cash_reserve_pct: None,
        }
    }

    /// Hand-computed: risk budget 1% of 1M = 10_000 CNY; loss distance 0.5 on
    /// a 10.0 price (5%) -> risk-view gross cap 10_000 / 0.05 = 200_000, and
    /// share view floor(10_000/0.5) = 20_000 shares * 10 = 200_000.
    /// Single-name: 20% * 1M = 200_000. Total: 70% * 1M - 500_000 = 200_000.
    /// Cash: 400_000. Min = 200_000 -> 20_000 shares (already whole lots).
    /// Fees: max(200_000 * 0.00025, 5) = 50; 200_050 <= 400_000 fits.
    #[test]
    fn sizing_golden_three_way_tie() {
        let res = risk_order_ceiling(&base_inputs(), &base_cons());
        assert_eq!(res.shares, 20_000);
        assert!(approx(res.gross, 200_000.0));
        assert_eq!(
            res.binding,
            vec![
                BindingConstraint::PerTradeRiskBudget,
                BindingConstraint::StopDistance,
                BindingConstraint::TotalExposure,
                BindingConstraint::SingleName,
            ]
        );
    }

    /// Liquidity cap binds: 10% of 150_000 shares daily volume = 15_000
    /// shares -> 150_000 gross < 200_000 risk cap.
    #[test]
    fn sizing_golden_liquidity_binds() {
        let mut inputs = base_inputs();
        inputs.avg_daily_volume_shares = Some(150_000.0);
        let mut cons = base_cons();
        cons.liquidity_participation_pct = Some(0.10);
        let res = risk_order_ceiling(&inputs, &cons);
        assert_eq!(res.shares, 15_000);
        assert_eq!(res.binding, vec![BindingConstraint::Liquidity]);
    }

    /// Cash shrink loop: risk cap allows 20_000 shares (200_050 with fees)
    /// but only 100_000 cash -> 9_900 shares: 99_000 + max(24.75, 5) fee
    /// = 99_024.75 <= 100_000; 10_000 shares would cost 100_025 > 100_000.
    #[test]
    fn sizing_golden_cash_after_fees_binds() {
        let mut inputs = base_inputs();
        inputs.cash = 100_000.0;
        let res = risk_order_ceiling(&inputs, &base_cons());
        assert_eq!(res.shares, 9_900);
        assert_eq!(res.binding, vec![BindingConstraint::CashAfterFees]);
    }

    /// Cash reserve: with 90% reserve of 1M equity required after the trade,
    /// at most ~100_000 - fee can be spent from 400_000 cash... the binding
    /// chain: cash allows 39_900 shares, but reserve needs
    /// 400_000 - gross - fee >= 0.90 * (1_000_000 - fee). Try 10_000 shares:
    /// 400_000 - 100_000 - 25 = 299_975 < 900_000 -> shrink. 1_100 shares:
    /// 400_000 - 11_000 - 5 = 388_995 < 900_000. Actually nothing fits except
    /// spending <= ~100_000: gross 100_000 -> cash_after 299_975 vs needed
    /// 899_977.5 — impossible since reserve (900k) exceeds cash (400k) minus
    /// any spend... The maximum spend satisfying the reserve is
    /// cash - reserve*equity_after; with reserve near equity the loop shrinks
    /// to zero. Use a milder reserve: 30% of equity = 300_000 must remain:
    /// spend <= 100_000 - fee. 9_900 shares: 99_000 + 24.75 -> cash_after
    /// 300_975.25 >= 300_007.5 (0.3 * (1M - 24.75)) OK. 10_000: cash_after
    /// 299_975 < 300_007.5 -> shrink. Result 9_900.
    #[test]
    fn sizing_golden_cash_reserve_binds() {
        let mut inputs = base_inputs();
        inputs.cash = 400_000.0;
        let mut cons = base_cons();
        cons.cash_reserve_pct = Some(0.30);
        // Risk cap 200_000 is above what the reserve allows (~100_000).
        let res = risk_order_ceiling(&inputs, &cons);
        assert_eq!(res.shares, 9_900);
        assert_eq!(res.binding, vec![BindingConstraint::CashReserveAfterFees]);
    }

    /// Degenerate inputs fail closed.
    #[test]
    fn sizing_invalid_inputs() {
        let bad_price = SizingInputs {
            price: 0.0,
            ..base_inputs()
        };
        let res = risk_order_ceiling(&bad_price, &base_cons());
        assert_eq!(res.shares, 0);
        assert_eq!(res.binding, vec![BindingConstraint::InvalidInputs]);

        let bad_stop = SizingInputs {
            stop_distance_per_share: 0.0,
            ..base_inputs()
        };
        assert_eq!(risk_order_ceiling(&bad_stop, &base_cons()).shares, 0);

        // Existing exposure already at the caps: no room left.
        let full = SizingInputs {
            current_position_value: 200_000.0,
            total_market_value: 700_000.0,
            ..base_inputs()
        };
        assert_eq!(risk_order_ceiling(&full, &base_cons()).shares, 0);
    }

    proptest! {
        /// Sizing invariants: whole lots, within every cap, cash-feasible,
        /// deterministic.
        #[test]
        fn sizing_properties(
            price in 0.5f64..500.0,
            equity in 10_000.0f64..10_000_000.0,
            cash_ratio in 0.0f64..1.0,
            risk_pct in 0.001f64..0.05,
            total_cap in 0.1f64..1.0,
            single_cap in 0.05f64..0.5,
            held_ratio in 0.0f64..1.0,
            stop_pct in 0.01f64..0.3,
        ) {
            let inputs = SizingInputs {
                price,
                equity,
                cash: equity * cash_ratio,
                current_position_value: equity * single_cap * held_ratio,
                total_market_value: equity * total_cap * held_ratio,
                stop_distance_per_share: price * stop_pct,
                avg_daily_volume_shares: None,
            };
            let cons = SizingConstraints {
                per_trade_risk_pct: risk_pct,
                total_position_cap_pct: total_cap,
                single_name_cap_pct: single_cap,
                liquidity_participation_pct: None,
                lot_size: 100,
                buy_fee_rate: 0.00025,
                buy_min_fee: 5.0,
                cash_reserve_pct: None,
            };
            let r1 = risk_order_ceiling(&inputs, &cons);
            let r2 = risk_order_ceiling(&inputs, &cons);
            prop_assert_eq!(&r1, &r2, "deterministic");
            prop_assert_eq!(r1.shares % 100, 0, "whole lots");
            if r1.shares > 0 {
                prop_assert!(r1.shares >= 100);
                let fee = (r1.gross * 0.00025).max(5.0);
                prop_assert!(r1.gross + fee <= inputs.cash + 1e-6, "cash feasible");
                let risk_cny = risk_pct * equity;
                prop_assert!(r1.gross <= risk_cny / stop_pct + 1e-6, "risk cap");
                prop_assert!(
                    inputs.total_market_value + r1.gross <= total_cap * equity + 1e-6,
                    "total cap"
                );
                prop_assert!(
                    inputs.current_position_value + r1.gross <= single_cap * equity + 1e-6,
                    "single-name cap"
                );
            } else {
                prop_assert_eq!(r1.gross, 0.0);
            }
        }
    }

    // ---------- Same-bar arbitration ----------

    #[test]
    fn same_bar_conservative_truth_table() {
        // stop 9.5, target 11.0.
        assert_eq!(
            same_bar_conservative(9.6, 10.9, 9.5, 11.0),
            SameBarTouch::Neither
        );
        assert_eq!(
            same_bar_conservative(9.5, 10.9, 9.5, 11.0),
            SameBarTouch::StopOnly
        );
        assert_eq!(
            same_bar_conservative(9.6, 11.0, 9.5, 11.0),
            SameBarTouch::TargetOnly
        );
        // Both touched: stop wins, always.
        assert_eq!(
            same_bar_conservative(9.4, 11.2, 9.5, 11.0),
            SameBarTouch::BothStopFirst
        );
    }

    proptest! {
        /// BothStopFirst iff both levels touched; outcomes are exhaustive.
        #[test]
        fn same_bar_properties(
            low in 1.0f64..100.0,
            span in 0.0f64..50.0,
            stop in 1.0f64..100.0,
            target in 1.0f64..100.0,
        ) {
            let high = low + span;
            let outcome = same_bar_conservative(low, high, stop, target);
            let stop_hit = low <= stop;
            let target_hit = high >= target;
            match outcome {
                SameBarTouch::BothStopFirst => {
                    prop_assert!(stop_hit && target_hit)
                }
                SameBarTouch::StopOnly => prop_assert!(stop_hit && !target_hit),
                SameBarTouch::TargetOnly => prop_assert!(!stop_hit && target_hit),
                SameBarTouch::Neither => prop_assert!(!stop_hit && !target_hit),
            }
        }
    }
}
