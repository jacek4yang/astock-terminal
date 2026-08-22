//! Red-flag detection over a company's own history.
//!
//! Every flag carries the evidence numbers it was raised from and a plain
//! explanation. Detectors are conservative: a rule only fires when all its
//! inputs are present (Missing inputs = no flag, never a guess), and the
//! margin-outlier rule needs enough history to be statistically meaningful.

use crate::metrics;
use serde::{Deserialize, Serialize};

/// Flag category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FlagKind {
    /// Revenue grew while operating cash flow shrank (YoY divergence).
    RevenueUpCfoDown,
    /// Receivables grew faster than revenue for 2 consecutive periods.
    ReceivablesOutpaceRevenue,
    /// Inventory grew much faster than COGS.
    InventorySpike,
    /// Goodwill exceeds 30% of equity.
    GoodwillHeavy,
    /// A margin is a statistical outlier vs the company's own history.
    MarginOutlier,
    /// 存贷双高: large cash pile alongside large interest-bearing debt.
    CashAndDebtBothHigh,
}

/// How alarming the flag is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    /// Worth knowing.
    Info,
    /// Deserves attention.
    Warn,
    /// Strong warning.
    High,
}

/// One red flag with its evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Flag {
    /// Category.
    pub kind: FlagKind,
    /// Severity.
    pub severity: Severity,
    /// Evidence numbers as `(label, value)` pairs (e.g. `("revenue_yoy", 0.15)`).
    pub evidence: Vec<(String, f64)>,
    /// Plain-language explanation (English).
    pub explanation: String,
}

/// One period of detector inputs (usually annual, or TTM).
#[derive(Debug, Clone, Copy, Default)]
pub struct PeriodObservation {
    /// 营业总收入.
    pub revenue: Option<f64>,
    /// 经营现金流净额.
    pub cfo: Option<f64>,
    /// 应收票据及应收账款 (or 应收账款 fallback — caller's choice).
    pub receivables: Option<f64>,
    /// 存货.
    pub inventory: Option<f64>,
    /// 营业成本.
    pub operating_cost: Option<f64>,
    /// 商誉.
    pub goodwill: Option<f64>,
    /// 归母权益.
    pub equity: Option<f64>,
    /// 货币资金.
    pub monetary_funds: Option<f64>,
    /// 有息负债.
    pub interest_bearing_debt: Option<f64>,
    /// 总资产.
    pub total_assets: Option<f64>,
    /// Gross margin for the period (ratio).
    pub gross_margin: Option<f64>,
    /// Net margin for the period (ratio).
    pub net_margin: Option<f64>,
}

/// Goodwill/equity threshold for [`FlagKind::GoodwillHeavy`].
pub const GOODWILL_EQUITY_THRESHOLD: f64 = 0.30;
/// 存贷双高 heuristic: cash AND debt each exceed this share of total assets.
pub const CASH_DEBT_ASSET_THRESHOLD: f64 = 0.25;
/// Inventory-spike: inventory growth minus COGS growth must exceed this.
pub const INVENTORY_COGS_GAP: f64 = 0.20;
/// Margin-outlier: |z-score| vs own history must exceed this.
pub const MARGIN_Z_THRESHOLD: f64 = 2.0;
/// Minimum history length (periods before the current one) for z-scores.
pub const MARGIN_MIN_HISTORY: usize = 6;

fn growth_opt(curr: Option<f64>, prev: Option<f64>) -> Option<f64> {
    metrics::growth(curr, prev)
}

/// Run all detectors. `history` must be oldest-first; the last element is
/// the period under review.
pub fn detect(history: &[PeriodObservation]) -> Vec<Flag> {
    let mut flags = Vec::new();
    let Some(curr) = history.last().copied() else {
        return flags;
    };
    let prev = history.len().checked_sub(2).map(|i| history[i]);

    // 1. Revenue up but CFO down (YoY divergence).
    if let Some(prev) = prev {
        match (
            growth_opt(curr.revenue, prev.revenue),
            growth_opt(curr.cfo, prev.cfo),
        ) {
            (Some(rev_g), Some(cfo_g)) if rev_g > 0.0 && cfo_g < 0.0 => {
                flags.push(Flag {
                    kind: FlagKind::RevenueUpCfoDown,
                    severity: Severity::High,
                    evidence: vec![
                        ("revenue_yoy".into(), rev_g),
                        ("cfo_yoy".into(), cfo_g),
                    ],
                    explanation: format!(
                        "Revenue grew {:.1}% while operating cash flow fell {:.1}% — \
                         earnings may not be converting to cash.",
                        rev_g * 100.0,
                        -cfo_g * 100.0
                    ),
                });
            }
            _ => {}
        }
    }

    // 2. Receivables growing faster than revenue, 2 consecutive periods.
    if history.len() >= 3 {
        let gaps: Vec<f64> = history
            .windows(2)
            .filter_map(|w| {
                let rg = growth_opt(w[1].receivables, w[0].receivables)?;
                let rev_g = growth_opt(w[1].revenue, w[0].revenue)?;
                Some(rg - rev_g)
            })
            .collect();
        if gaps.len() >= 2 {
            let last_two = &gaps[gaps.len() - 2..];
            if last_two.iter().all(|g| *g > 0.0) {
                let worst = last_two.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                flags.push(Flag {
                    kind: FlagKind::ReceivablesOutpaceRevenue,
                    severity: if worst > 0.20 { Severity::High } else { Severity::Warn },
                    evidence: vec![
                        ("recv_growth_minus_rev_growth_t".into(), last_two[1]),
                        ("recv_growth_minus_rev_growth_t-1".into(), last_two[0]),
                    ],
                    explanation: format!(
                        "Receivables outgrew revenue by {:.0}pp and {:.0}pp in the last two \
                         periods — possible channel stuffing or loose credit terms.",
                        last_two[0] * 100.0,
                        last_two[1] * 100.0
                    ),
                });
            }
        }
    }

    // 3. Inventory spike vs COGS.
    if let Some(prev) = prev {
        match (
            growth_opt(curr.inventory, prev.inventory),
            growth_opt(curr.operating_cost, prev.operating_cost),
        ) {
            (Some(inv_g), Some(cogs_g)) if inv_g - cogs_g > INVENTORY_COGS_GAP => {
                flags.push(Flag {
                    kind: FlagKind::InventorySpike,
                    severity: Severity::Warn,
                    evidence: vec![
                        ("inventory_yoy".into(), inv_g),
                        ("cogs_yoy".into(), cogs_g),
                    ],
                    explanation: format!(
                        "Inventory grew {:.1}% vs COGS {:.1}% — stockpiling beyond \
                         what sales cost trends justify.",
                        inv_g * 100.0,
                        cogs_g * 100.0
                    ),
                });
            }
            _ => {}
        }
    }

    // 4. Goodwill / equity > 30%.
    if let (Some(gw), Some(eq)) = (curr.goodwill, curr.equity) {
        if eq > 0.0 {
            let ratio = gw / eq;
            if ratio > GOODWILL_EQUITY_THRESHOLD {
                flags.push(Flag {
                    kind: FlagKind::GoodwillHeavy,
                    severity: if ratio > 0.5 { Severity::High } else { Severity::Warn },
                    evidence: vec![
                        ("goodwill".into(), gw),
                        ("equity".into(), eq),
                        ("goodwill_to_equity".into(), ratio),
                    ],
                    explanation: format!(
                        "Goodwill is {:.0}% of equity (>{:.0}% threshold) — impairment risk \
                         dominates the balance sheet.",
                        ratio * 100.0,
                        GOODWILL_EQUITY_THRESHOLD * 100.0
                    ),
                });
            }
        }
    }

    // 5. Margin outliers vs own history (z-score, needs MARGIN_MIN_HISTORY).
    for (label, series_full, current) in [
        ("gross_margin", margin_series(history, |o| o.gross_margin), curr.gross_margin),
        ("net_margin", margin_series(history, |o| o.net_margin), curr.net_margin),
    ] {
        if series_full.len() > MARGIN_MIN_HISTORY {
            let hist = &series_full[..series_full.len() - 1];
            if let (Some(cur), Some(z)) = (current, zscore(hist, current.unwrap_or(f64::NAN))) {
                if z.abs() > MARGIN_Z_THRESHOLD {
                    flags.push(Flag {
                        kind: FlagKind::MarginOutlier,
                        severity: Severity::Warn,
                        evidence: vec![
                            (format!("{label}_current"), cur),
                            (format!("{label}_zscore"), z),
                        ],
                        explanation: format!(
                            "{label} {:.1}% is a {:.1}σ outlier vs the company's own \
                             {}-period history.",
                            cur * 100.0,
                            z,
                            hist.len()
                        ),
                    });
                }
            }
        }
    }

    // 6. 存贷双高: cash AND interest-bearing debt both large vs assets.
    if let (Some(cash), Some(debt), Some(assets)) =
        (curr.monetary_funds, curr.interest_bearing_debt, curr.total_assets)
    {
        if assets > 0.0 {
            let cash_ratio = cash / assets;
            let debt_ratio = debt / assets;
            if cash_ratio > CASH_DEBT_ASSET_THRESHOLD && debt_ratio > CASH_DEBT_ASSET_THRESHOLD {
                flags.push(Flag {
                    kind: FlagKind::CashAndDebtBothHigh,
                    severity: Severity::High,
                    evidence: vec![
                        ("cash_to_assets".into(), cash_ratio),
                        ("debt_to_assets".into(), debt_ratio),
                    ],
                    explanation: format!(
                        "存贷双高: cash is {:.0}% of assets while interest-bearing debt is \
                         {:.0}% — a real cash pile should not need that much borrowing.",
                        cash_ratio * 100.0,
                        debt_ratio * 100.0
                    ),
                });
            }
        }
    }

    flags
}

/// Extract a margin series (only present values) for the z-score rule.
fn margin_series(
    history: &[PeriodObservation],
    field: impl Fn(&PeriodObservation) -> Option<f64>,
) -> Vec<f64> {
    history.iter().filter_map(field).collect()
}

/// Sample z-score of `x` against `hist` (population std dev; with small
/// samples this is the documented simplification). `None` when `hist` has
/// fewer than 2 points or zero variance.
fn zscore(hist: &[f64], x: f64) -> Option<f64> {
    if hist.len() < 2 || !x.is_finite() {
        return None;
    }
    let n = hist.len() as f64;
    let mean = hist.iter().sum::<f64>() / n;
    let var = hist.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    let sd = var.sqrt();
    if sd == 0.0 {
        return None;
    }
    Some((x - mean) / sd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(revenue: f64, cfo: f64) -> PeriodObservation {
        PeriodObservation {
            revenue: Some(revenue),
            cfo: Some(cfo),
            ..Default::default()
        }
    }

    #[test]
    fn empty_history_yields_no_flags() {
        assert!(detect(&[]).is_empty());
    }

    #[test]
    fn revenue_up_cfo_down_fires() {
        let h = vec![obs(100.0, 50.0), obs(120.0, 40.0)];
        let flags = detect(&h);
        assert!(flags.iter().any(|f| f.kind == FlagKind::RevenueUpCfoDown));
        let f = flags.iter().find(|f| f.kind == FlagKind::RevenueUpCfoDown).unwrap();
        assert_eq!(f.severity, Severity::High);
    }

    #[test]
    fn revenue_up_cfo_down_silent_when_cfo_also_up() {
        let h = vec![obs(100.0, 50.0), obs(120.0, 60.0)];
        assert!(detect(&h).is_empty());
    }

    #[test]
    fn receivables_rule_needs_two_consecutive_periods() {
        let base = PeriodObservation {
            revenue: Some(100.0),
            receivables: Some(10.0),
            ..Default::default()
        };
        let p1 = PeriodObservation {
            revenue: Some(110.0), // +10%
            receivables: Some(12.0), // +20%
            ..Default::default()
        };
        let p2 = PeriodObservation {
            revenue: Some(121.0), // +10%
            receivables: Some(14.4), // +20%
            ..Default::default()
        };
        let flags = detect(&[base, p1, p2]);
        assert!(flags
            .iter()
            .any(|f| f.kind == FlagKind::ReceivablesOutpaceRevenue));
        // Only one period of divergence → no flag.
        assert!(detect(&[base, p1])
            .iter()
            .all(|f| f.kind != FlagKind::ReceivablesOutpaceRevenue));
    }

    #[test]
    fn inventory_spike_fires_on_gap() {
        let p0 = PeriodObservation {
            inventory: Some(100.0),
            operating_cost: Some(200.0),
            ..Default::default()
        };
        let p1 = PeriodObservation {
            inventory: Some(150.0), // +50%
            operating_cost: Some(220.0), // +10% → gap 40pp > 20pp
            ..Default::default()
        };
        let flags = detect(&[p0, p1]);
        assert!(flags.iter().any(|f| f.kind == FlagKind::InventorySpike));
    }

    #[test]
    fn goodwill_threshold() {
        let mut p = PeriodObservation {
            goodwill: Some(40.0),
            equity: Some(100.0), // 40% > 30%
            ..Default::default()
        };
        let flags = detect(&[p]);
        let f = flags.iter().find(|f| f.kind == FlagKind::GoodwillHeavy).unwrap();
        assert_eq!(f.severity, Severity::Warn);
        p.goodwill = Some(60.0); // 60% > 50% → High
        let flags = detect(&[p]);
        assert_eq!(
            flags
                .iter()
                .find(|f| f.kind == FlagKind::GoodwillHeavy)
                .unwrap()
                .severity,
            Severity::High
        );
        p.goodwill = Some(10.0); // 10% → silent
        assert!(detect(&[p])
            .iter()
            .all(|f| f.kind != FlagKind::GoodwillHeavy));
    }

    #[test]
    fn margin_outlier_with_history() {
        // 7 flat periods at 40% GM, then a drop to 10%: z far below −2.
        let mut h: Vec<PeriodObservation> = (0..7)
            .map(|i| PeriodObservation {
                gross_margin: Some(0.40 + i as f64 * 0.001), // tiny variance
                ..Default::default()
            })
            .collect();
        h.push(PeriodObservation {
            gross_margin: Some(0.10),
            ..Default::default()
        });
        let flags = detect(&h);
        assert!(flags.iter().any(|f| f.kind == FlagKind::MarginOutlier));
    }

    #[test]
    fn margin_outlier_silent_without_enough_history() {
        let h: Vec<PeriodObservation> = (0..4)
            .map(|_| PeriodObservation {
                gross_margin: Some(0.40),
                ..Default::default()
            })
            .collect();
        assert!(detect(&h).iter().all(|f| f.kind != FlagKind::MarginOutlier));
    }

    #[test]
    fn cash_and_debt_both_high() {
        let mut p = PeriodObservation {
            monetary_funds: Some(300.0),
            interest_bearing_debt: Some(280.0),
            total_assets: Some(1000.0), // 30% and 28% > 25%
            ..Default::default()
        };
        let flags = detect(&[p]);
        assert!(flags
            .iter()
            .any(|f| f.kind == FlagKind::CashAndDebtBothHigh));
        p.interest_bearing_debt = Some(100.0); // 10% → silent
        assert!(detect(&[p])
            .iter()
            .all(|f| f.kind != FlagKind::CashAndDebtBothHigh));
    }

    #[test]
    fn missing_inputs_never_fire() {
        // All-Missing current period must produce zero flags.
        assert!(detect(&[PeriodObservation::default()]).is_empty());
    }
}
