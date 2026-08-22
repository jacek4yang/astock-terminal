//! Corporate-action-driven price adjustment (复权) engine.
//!
//! Pure functions implementing the math of `docs/data-foundation-v2.md`
//! §复权因子数学 verbatim:
//!
//! - Per corporate action (ex-date `E`, previous close `C`): with pre-tax
//!   cash dividend per share `D`, bonus+transfer shares per share `B`,
//!   rights-issue ratio `R` at rights price `P`:
//!   `X = (C − D + P×R) / (1 + B + R)` (theoretical ex price), `r = X / C`.
//! - Forward adjustment (前复权, anchor = latest by default):
//!   `factor_qfq(t) = ∏{r_i | E_i > t}`, the anchor day's factor is 1;
//!   `qfq price = raw × factor_qfq`.
//! - Backward adjustment (后复权): `hfq(t) = qfq(t) / qfq(t0) × raw(t0)`,
//!   i.e. cumulative from the earliest bar.
//! - **Only prices are adjusted.** Volume (手数口径) and turnover amount are
//!   left unchanged, matching mainstream software (spec §复权因子数学 last
//!   bullet); `pct` is recomputed from the adjusted closes so the ex-date
//!   does not show a fake crash.
//!
//! # Point-in-time convention (spec §原则)
//!
//! `compute_qfq` only applies actions with `ex_date ≤ anchor` — an analysis
//! anchored at T never sees later actions. When `notice_cutoff` is `Some(c)`,
//! the stricter PIT variant additionally requires `notice_date ≤ c` *for
//! actions whose notice date is known*; actions with `notice_date = None`
//! are kept (absence of evidence is not evidence of absence — the field is
//! often missing in upstream data). The default is the ex-date-based rule
//! (`notice_cutoff = None`).

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::bar::Bar;
use crate::period::Adjust;

/// One corporate action (分红/送转/配股), per-share magnitudes.
///
/// All ratio fields are **per share**: a 10送10 bonus is `bonus_share = 1.0`,
/// a 10派5元 dividend is `cash_div = 0.5`, a 10配3 rights issue is
/// `rights_ratio = 0.3`. Upstream adapters divide their per-10-shares
/// figures by 10 when building these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorporateAction {
    /// Ex-dividend / ex-rights date (除权除息日).
    pub ex_date: NaiveDate,
    /// Announcement date (公告日), when known; used by the strict PIT
    /// variant (see module docs).
    pub notice_date: Option<NaiveDate>,
    /// Pre-tax cash dividend per share (D).
    #[serde(default)]
    pub cash_div: f64,
    /// Bonus + capitalisation shares per share (B, 送股 + 转增).
    #[serde(default)]
    pub bonus_share: f64,
    /// Rights-issue shares per share (R, 配股).
    #[serde(default)]
    pub rights_ratio: f64,
    /// Rights-issue price per new share (P); `None` when unknown.
    #[serde(default)]
    pub rights_price: Option<f64>,
}

impl CorporateAction {
    /// Cash/bonus-only constructor; rights fields default to zero.
    pub fn new(ex_date: NaiveDate, cash_div: f64, bonus_share: f64) -> Self {
        CorporateAction {
            ex_date,
            notice_date: None,
            cash_div,
            bonus_share,
            rights_ratio: 0.0,
            rights_price: None,
        }
    }
}

/// A data problem encountered while computing adjustment factors. The
/// offending action is degraded or skipped, never fatal.
#[derive(Debug, Clone, PartialEq)]
pub enum AdjustWarning {
    /// Rights issue without a rights price: R treated as 0 (spec-mandated
    /// fallback; the cash side of the action still applies).
    RightsWithoutPrice {
        /// Ex-date of the offending action.
        ex_date: NaiveDate,
    },
    /// No bar before the ex-date, so the previous close `C` is unknown;
    /// the action is skipped entirely.
    MissingPrevClose {
        /// Ex-date of the offending action.
        ex_date: NaiveDate,
    },
    /// Previous close `C ≤ 0` (dirty data); the action is skipped.
    NonPositivePrevClose {
        /// Ex-date of the offending action.
        ex_date: NaiveDate,
        /// The offending previous close.
        prev_close: f64,
    },
    /// Theoretical ex price `X ≤ 0` (pathological inputs); skipped.
    NonPositiveExPrice {
        /// Ex-date of the offending action.
        ex_date: NaiveDate,
        /// The computed theoretical ex price.
        ex_price: f64,
    },
}

/// Result of an adjustment run: the adjusted bars plus every data-quality
/// warning encountered (warnings are also log-worthy upstream).
#[derive(Debug, Clone, PartialEq)]
pub struct Adjusted {
    /// Adjusted bars, same order and count as the input.
    pub bars: Vec<Bar>,
    /// Degraded/skipped actions, in ex-date order.
    pub warnings: Vec<AdjustWarning>,
}

/// Per-action factor ratio `r = X / C` keyed by ex-date, restricted to
/// actions knowable at `as_of` (PIT: `ex_date ≤ as_of`, plus the optional
/// notice-date cutoff — see module docs).
///
/// `bars` supplies the previous close `C` (the close of the last bar
/// strictly before the ex-date). Returns the `(ex_date, r)` pairs sorted by
/// ascending ex-date, plus warnings for degraded/skipped actions.
pub fn action_factors(
    bars: &[Bar],
    actions: &[CorporateAction],
    as_of: NaiveDate,
    notice_cutoff: Option<NaiveDate>,
) -> (Vec<(NaiveDate, f64)>, Vec<AdjustWarning>) {
    let mut sorted: Vec<&CorporateAction> = actions
        .iter()
        .filter(|a| a.ex_date <= as_of)
        .filter(|a| match (notice_cutoff, a.notice_date) {
            (Some(cutoff), Some(notice)) => notice <= cutoff,
            _ => true,
        })
        .collect();
    sorted.sort_by_key(|a| a.ex_date);

    let mut out = Vec::with_capacity(sorted.len());
    let mut warnings = Vec::new();
    for action in sorted {
        // Previous close: last bar strictly before the ex-date. Bars are
        // expected date-sorted; partition_point keeps this O(log n).
        let idx = bars.partition_point(|b| b.date < action.ex_date);
        let Some(prev) = idx.checked_sub(1).map(|i| &bars[i]) else {
            warnings.push(AdjustWarning::MissingPrevClose {
                ex_date: action.ex_date,
            });
            continue;
        };
        let c = prev.close;
        if c <= 0.0 {
            warnings.push(AdjustWarning::NonPositivePrevClose {
                ex_date: action.ex_date,
                prev_close: c,
            });
            continue;
        }
        let (r_ratio, p) = match (action.rights_ratio, action.rights_price) {
            (r, None) if r > 0.0 => {
                warnings.push(AdjustWarning::RightsWithoutPrice {
                    ex_date: action.ex_date,
                });
                (0.0, 0.0)
            }
            (r, price) => (r, price.unwrap_or(0.0)),
        };
        // Spec §复权因子数学: X = (C − D + P×R) / (1 + B + R); r = X / C.
        let x = (c - action.cash_div + p * r_ratio) / (1.0 + action.bonus_share + r_ratio);
        if x <= 0.0 {
            warnings.push(AdjustWarning::NonPositiveExPrice {
                ex_date: action.ex_date,
                ex_price: x,
            });
            continue;
        }
        out.push((action.ex_date, x / c));
    }
    (out, warnings)
}

/// Scale one bar's prices by `factor`. `pct` is kept from the raw bar when
/// the factor matches the previous bar's (the close-to-close ratio is
/// unchanged) and recomputed from the previous adjusted close otherwise —
/// this removes the fake ex-date crash without touching steady-state values
/// (spec: only prices are adjusted; volume, amount and turnover pass through
/// unchanged).
fn scale_bar(bar: &Bar, factor: f64, prev_adjusted_close: Option<f64>, keep_pct: bool) -> Bar {
    let mut out = bar.clone();
    out.open *= factor;
    out.close *= factor;
    out.high *= factor;
    out.low *= factor;
    if !keep_pct {
        out.pct = prev_adjusted_close.and_then(|prev| {
            (prev > 0.0).then(|| {
                let pct = (out.close - prev) / prev * 100.0;
                (pct * 100.0).round() / 100.0
            })
        });
    }
    out
}

/// Forward-adjusted (前复权) bars anchored at `anchor_date`.
///
/// `factor_qfq(t) = ∏{r_i | E_i > t}` over PIT-eligible actions, so the
/// anchor day's factor is exactly 1 and bars after the anchor (if any) pass
/// through unscaled. `anchor_date` need not be a bar date.
pub fn compute_qfq(
    bars: &[Bar],
    actions: &[CorporateAction],
    anchor_date: NaiveDate,
    notice_cutoff: Option<NaiveDate>,
) -> Adjusted {
    let (factors, warnings) = action_factors(bars, actions, anchor_date, notice_cutoff);
    // Suffix products: suffix[k] = ∏{r_i | i ≥ k}; factor(t) picks the first
    // action with E_i > t.
    let mut suffix = vec![1.0_f64; factors.len() + 1];
    for k in (0..factors.len()).rev() {
        suffix[k] = suffix[k + 1] * factors[k].1;
    }
    // Per-bar factors; an all-1.0 series is an exact passthrough (identity,
    // including untouched `pct` fields).
    let bar_factors: Vec<f64> = bars
        .iter()
        .map(|bar| {
            let k = factors.partition_point(|(e, _)| *e <= bar.date);
            suffix[k]
        })
        .collect();
    if bar_factors.iter().all(|&f| f == 1.0) {
        return Adjusted {
            bars: bars.to_vec(),
            warnings,
        };
    }
    let mut out: Vec<Bar> = Vec::with_capacity(bars.len());
    let mut prev_factor = 1.0;
    for (i, bar) in bars.iter().enumerate() {
        let factor = bar_factors[i];
        let prev_close = i.checked_sub(1).map(|j| out[j].close);
        // Keep the adapter-supplied `pct` when the factor is unchanged from
        // the previous bar (ratio preserved); recompute at factor breaks and
        // when the raw bar carried no `pct` at all.
        let keep_pct = factor == prev_factor && bar.pct.is_some();
        out.push(scale_bar(bar, factor, prev_close, keep_pct));
        prev_factor = factor;
    }
    Adjusted {
        bars: out,
        warnings,
    }
}

/// Backward-adjusted (后复权) bars: cumulative from the earliest bar.
///
/// Per the spec, implemented literally as `hfq(t) = qfq(t) / qfq(t0) ×
/// raw(t0)` with the qfq anchor at the last bar — so `hfq(t0) = raw(t0)`
/// and later bars carry the compounded factors.
pub fn compute_hfq(
    bars: &[Bar],
    actions: &[CorporateAction],
    notice_cutoff: Option<NaiveDate>,
) -> Adjusted {
    if bars.is_empty() {
        return Adjusted {
            bars: Vec::new(),
            warnings: Vec::new(),
        };
    }
    let anchor = bars.last().expect("non-empty").date;
    let Adjusted {
        bars: qfq,
        warnings,
    } = compute_qfq(bars, actions, anchor, notice_cutoff);
    let t0_qfq_close = qfq[0].close;
    let t0_raw_close = bars[0].close;
    if t0_qfq_close <= 0.0 || t0_raw_close <= 0.0 {
        // Dirty first bar: return the qfq-shaped series unscaled rather than
        // fabricating factors from a zero base.
        return Adjusted {
            bars: qfq,
            warnings,
        };
    }
    let scale = t0_raw_close / t0_qfq_close;
    let mut out: Vec<Bar> = Vec::with_capacity(qfq.len());
    for (i, bar) in qfq.iter().enumerate() {
        let prev_close = i.checked_sub(1).map(|j| out[j].close);
        // The qfq pass already fixed boundary `pct`s; scaling by a constant
        // never changes close-to-close ratios, so `pct` is always kept.
        out.push(scale_bar(bar, scale, prev_close, true));
    }
    Adjusted {
        bars: out,
        warnings,
    }
}

/// Dispatch on [`Adjust`]: `None` is a raw passthrough (actions ignored),
/// `Qfq` anchors at `anchor_date`, `Hfq` anchors at the last bar.
pub fn apply_adjustment(
    bars: &[Bar],
    actions: &[CorporateAction],
    kind: Adjust,
    anchor_date: NaiveDate,
    notice_cutoff: Option<NaiveDate>,
) -> Adjusted {
    match kind {
        Adjust::None => Adjusted {
            bars: bars.to_vec(),
            warnings: Vec::new(),
        },
        Adjust::Qfq => compute_qfq(bars, actions, anchor_date, notice_cutoff),
        Adjust::Hfq => compute_hfq(bars, actions, notice_cutoff),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar::VolumeUnit;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// Flat bar (O=H=L=C) for hand-computed goldens.
    fn bar(date: &str, price: f64) -> Bar {
        Bar::new(
            d(date),
            price,
            price,
            price,
            price,
            1000.0,
            VolumeUnit::Lots,
        )
    }

    fn closes(bars: &[Bar]) -> Vec<f64> {
        bars.iter().map(|b| b.close).collect()
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// The canonical example: close 20, then 10送10 (B = 1.0 per share),
    /// ex-date opens at 10.
    ///
    /// Hand computation (spec §复权因子数学):
    /// - X = (20 − 0) / (1 + 1) = 10; r = 10 / 20 = 0.5.
    /// - qfq anchored at 2025-01-08 (latest): factor(01-06) = 0.5,
    ///   factor(01-07) = factor(01-08) = 1 → closes [10, 10, 10.5].
    /// - hfq(t) = qfq(t)/qfq(t0)×raw(t0) = qfq(t) × 20/10 → [20, 20, 21].
    #[test]
    fn golden_bonus_10_to_10() {
        let bars = vec![
            bar("2025-01-06", 20.0),
            bar("2025-01-07", 10.0), // ex-date
            bar("2025-01-08", 10.5),
        ];
        let actions = vec![CorporateAction::new(d("2025-01-07"), 0.0, 1.0)];

        let qfq = compute_qfq(&bars, &actions, d("2025-01-08"), None);
        assert!(qfq.warnings.is_empty());
        assert_eq!(closes(&qfq.bars), vec![10.0, 10.0, 10.5]);
        // The fake −50% crash is gone from pct.
        assert_eq!(qfq.bars[1].pct, Some(0.0));
        assert_eq!(qfq.bars[2].pct, Some(5.0));

        let hfq = compute_hfq(&bars, &actions, None);
        assert_eq!(closes(&hfq.bars), vec![20.0, 20.0, 21.0]);
    }

    /// Cash dividend: prev close 10, 10派10元 (D = 1.0 per share).
    ///
    /// Hand computation: X = (10 − 1) / 1 = 9; r = 0.9.
    /// - qfq (anchor latest): closes [10×0.9, 9.2] = [9.0, 9.2].
    /// - hfq: scale = raw(t0)/qfq(t0) = 10/9 → [10.0, 9.2×10/9 ≈ 10.2222].
    #[test]
    fn golden_cash_dividend() {
        let bars = vec![bar("2025-03-10", 10.0), bar("2025-03-11", 9.2)];
        let actions = vec![CorporateAction::new(d("2025-03-11"), 1.0, 0.0)];

        let qfq = compute_qfq(&bars, &actions, d("2025-03-11"), None);
        assert!(qfq.warnings.is_empty());
        assert!(approx(qfq.bars[0].close, 9.0));
        assert!(approx(qfq.bars[1].close, 9.2));

        let hfq = compute_hfq(&bars, &actions, None);
        assert!(approx(hfq.bars[0].close, 10.0));
        assert!(approx(hfq.bars[1].close, 9.2 * 10.0 / 9.0));
    }

    /// Combined action: prev close 20, 10派5元 (D = 0.5), 10送10 (B = 1.0),
    /// 10配3 at 8元 (R = 0.3, P = 8).
    ///
    /// Hand computation:
    /// X = (20 − 0.5 + 8×0.3) / (1 + 1 + 0.3) = 21.9 / 2.3 = 9.521739…
    /// r = X / 20 = 0.4760869565…
    /// qfq pre-ex close = 20 × r = 9.5217391304…
    #[test]
    fn golden_combined_cash_bonus_rights() {
        let bars = vec![bar("2025-06-09", 20.0), bar("2025-06-10", 9.6)];
        let action = CorporateAction {
            ex_date: d("2025-06-10"),
            notice_date: Some(d("2025-05-30")),
            cash_div: 0.5,
            bonus_share: 1.0,
            rights_ratio: 0.3,
            rights_price: Some(8.0),
        };
        let qfq = compute_qfq(&bars, &[action], d("2025-06-10"), None);
        assert!(qfq.warnings.is_empty());
        let x = 21.9 / 2.3;
        assert!(approx(qfq.bars[0].close, 20.0 * x / 20.0));
        assert!(approx(qfq.bars[0].close, x)); // 20 cancels: pre-ex qfq == X
        assert!(approx(qfq.bars[1].close, 9.6));
    }

    /// Two successive 10送10 on the same stock: 40 → 20 → 10.
    /// factor(01-06) = 0.5 × 0.5 = 0.25 → qfq closes [10, 10, 10];
    /// hfq closes [40, 40, 40] (flat real value chain).
    #[test]
    fn golden_multiple_actions_compound() {
        let bars = vec![
            bar("2025-01-06", 40.0),
            bar("2025-01-07", 20.0), // first ex-date
            bar("2025-02-10", 10.0), // second ex-date
            bar("2025-02-11", 11.0),
        ];
        let actions = vec![
            CorporateAction::new(d("2025-01-07"), 0.0, 1.0),
            CorporateAction::new(d("2025-02-10"), 0.0, 1.0),
        ];
        let qfq = compute_qfq(&bars, &actions, d("2025-02-11"), None);
        assert_eq!(closes(&qfq.bars), vec![10.0, 10.0, 10.0, 11.0]);
        let hfq = compute_hfq(&bars, &actions, None);
        assert_eq!(closes(&hfq.bars), vec![40.0, 40.0, 40.0, 44.0]);
    }

    #[test]
    fn no_actions_is_identity() {
        let bars = vec![bar("2025-01-06", 20.0), bar("2025-01-07", 21.0)];
        let qfq = compute_qfq(&bars, &[], d("2025-01-07"), None);
        assert!(qfq.warnings.is_empty());
        assert_eq!(qfq.bars, bars);
        let hfq = compute_hfq(&bars, &[], None);
        assert_eq!(hfq.bars, bars);
    }

    /// PIT: an action whose ex-date is after the anchor must not leak into
    /// the anchored series (spec §原则).
    #[test]
    fn pit_anchor_before_ex_date_sees_no_action() {
        let bars = vec![
            bar("2025-01-06", 20.0),
            bar("2025-01-07", 10.0),
            bar("2025-01-08", 10.5),
        ];
        let actions = vec![CorporateAction::new(d("2025-01-07"), 0.0, 1.0)];
        // Anchor at 01-06: the 01-07 action is in the future.
        let qfq = compute_qfq(&bars, &actions, d("2025-01-06"), None);
        assert_eq!(closes(&qfq.bars), vec![20.0, 10.0, 10.5]);
    }

    /// Strict PIT: with a notice cutoff, actions announced after the cutoff
    /// are excluded; actions without a notice date are kept (documented
    /// lenient convention).
    #[test]
    fn notice_cutoff_gates_announced_actions() {
        let bars = vec![bar("2025-01-06", 20.0), bar("2025-01-07", 10.0)];
        let mut action = CorporateAction::new(d("2025-01-07"), 0.0, 1.0);
        action.notice_date = Some(d("2025-01-05"));

        // Cutoff before the notice date: action invisible → identity.
        let out = compute_qfq(
            &bars,
            &[action.clone()],
            d("2025-01-07"),
            Some(d("2025-01-04")),
        );
        assert_eq!(closes(&out.bars), vec![20.0, 10.0]);
        // Cutoff at/after the notice date: action applied.
        let out = compute_qfq(
            &bars,
            &[action.clone()],
            d("2025-01-07"),
            Some(d("2025-01-05")),
        );
        assert_eq!(closes(&out.bars), vec![10.0, 10.0]);
        // Unknown notice date: kept even under a cutoff.
        let undated = CorporateAction::new(d("2025-01-07"), 0.0, 1.0);
        let out = compute_qfq(&bars, &[undated], d("2025-01-07"), Some(d("2025-01-01")));
        assert_eq!(closes(&out.bars), vec![10.0, 10.0]);
    }

    #[test]
    fn rights_without_price_treated_as_zero_with_warning() {
        let bars = vec![bar("2025-01-06", 10.0), bar("2025-01-07", 10.1)];
        let action = CorporateAction {
            ex_date: d("2025-01-07"),
            notice_date: None,
            cash_div: 0.0,
            bonus_share: 0.0,
            rights_ratio: 0.3,
            rights_price: None,
        };
        let out = compute_qfq(&bars, &[action], d("2025-01-07"), None);
        // R → 0 makes X = 10/1 = 10, r = 1: identity, but the warning records
        // the degradation.
        assert_eq!(closes(&out.bars), vec![10.0, 10.1]);
        assert_eq!(
            out.warnings,
            vec![AdjustWarning::RightsWithoutPrice {
                ex_date: d("2025-01-07")
            }]
        );
    }

    #[test]
    fn non_positive_prev_close_skips_action_with_warning() {
        let mut bad_prev = bar("2025-01-06", 20.0);
        bad_prev.close = 0.0; // dirty data
        let bars = vec![bad_prev, bar("2025-01-07", 10.0)];
        let actions = vec![CorporateAction::new(d("2025-01-07"), 0.0, 1.0)];
        let out = compute_qfq(&bars, &actions, d("2025-01-07"), None);
        assert_eq!(closes(&out.bars), vec![0.0, 10.0]); // untouched
        assert_eq!(
            out.warnings,
            vec![AdjustWarning::NonPositivePrevClose {
                ex_date: d("2025-01-07"),
                prev_close: 0.0,
            }]
        );
    }

    #[test]
    fn action_before_first_bar_is_skipped_with_warning() {
        let bars = vec![bar("2025-01-07", 10.0), bar("2025-01-08", 10.5)];
        // Ex-date equals the first bar: no earlier close available.
        let actions = vec![CorporateAction::new(d("2025-01-07"), 0.0, 1.0)];
        let out = compute_qfq(&bars, &actions, d("2025-01-08"), None);
        assert_eq!(out.bars, bars);
        assert_eq!(
            out.warnings,
            vec![AdjustWarning::MissingPrevClose {
                ex_date: d("2025-01-07")
            }]
        );
    }

    #[test]
    fn apply_adjustment_dispatches() {
        let bars = vec![bar("2025-01-06", 20.0), bar("2025-01-07", 10.0)];
        let actions = vec![CorporateAction::new(d("2025-01-07"), 0.0, 1.0)];
        let raw = apply_adjustment(&bars, &actions, Adjust::None, d("2025-01-07"), None);
        assert_eq!(raw.bars, bars);
        let qfq = apply_adjustment(&bars, &actions, Adjust::Qfq, d("2025-01-07"), None);
        assert_eq!(closes(&qfq.bars), vec![10.0, 10.0]);
        let hfq = apply_adjustment(&bars, &actions, Adjust::Hfq, d("2025-01-07"), None);
        assert_eq!(closes(&hfq.bars), vec![20.0, 20.0]);
    }
}
