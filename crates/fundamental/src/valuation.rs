//! Valuation: current multiples, historical percentiles, PEG, and a
//! two-stage FCFF DCF that always answers with ranges, never a single
//! target price.
//!
//! Method notes:
//! - **Percentile**: share of history ≤ current value × 100, over the daily
//!   `RPT_VALUEANALYSIS_DET` series (vendor-computed PE_TTM etc.). This is a
//!   true market-data percentile, NOT the price×latest-fundamentals
//!   approximation — when the history endpoint fails the caller must treat
//!   percentiles as Missing (we do not fall back silently).
//! - **PEG** = PE(TTM) / expected EPS growth in percent. The growth estimate
//!   is a caller-supplied parameter — we never invent one.
//! - **DCF**: two-stage FCFF. `base_fcf` is expected to be CFO − capex (the
//!   [`crate::metrics::fcf`] proxy for FCFF; strict FCFF adds back after-tax
//!   interest expense — documented approximation). Terminal value uses the
//!   Gordon growth model and requires wacc > terminal growth.

use serde::{Deserialize, Serialize};

/// Current valuation multiples, assembled from the quote snapshot (PE/PB)
/// and the latest valuation-history row (PS/PCF — not in the quote fields).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Multiples {
    /// PE(TTM).
    pub pe_ttm: Option<f64>,
    /// PB(MRQ).
    pub pb: Option<f64>,
    /// PS(TTM) — from the latest `RPT_VALUEANALYSIS_DET` row.
    pub ps_ttm: Option<f64>,
    /// PCF(经营现金流TTM) — from the latest `RPT_VALUEANALYSIS_DET` row.
    pub pcf_ttm: Option<f64>,
}

/// Percentile of `current` within `history`, in [0, 100]: the share of
/// historical values ≤ current × 100. Non-finite history entries are
/// skipped; `None` when history is empty or `current` is not finite.
pub fn percentile(history: &[f64], current: f64) -> Option<f64> {
    if !current.is_finite() {
        return None;
    }
    let clean: Vec<f64> = history.iter().copied().filter(|v| v.is_finite()).collect();
    if clean.is_empty() {
        return None;
    }
    let below = clean.iter().filter(|v| **v <= current).count();
    Some(below as f64 / clean.len() as f64 * 100.0)
}

/// PEG = PE / (growth in percent). `None` when growth ≤ 0 (PEG undefined
/// for shrinking or stagnant earnings) or PE ≤ 0.
pub fn peg(pe_ttm: Option<f64>, growth_pct: Option<f64>) -> Option<f64> {
    let (pe, g) = (pe_ttm?, growth_pct?);
    if pe <= 0.0 || g <= 0.0 {
        return None;
    }
    Some(pe / g)
}

/// Inputs for the two-stage FCFF DCF. All growth rates and the WACC are
/// decimals (0.10 = 10%).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DcfInputs {
    /// Base-year free cash flow (CFO − capex proxy; see module docs).
    pub base_fcf: f64,
    /// Explicit forecast horizon in years.
    pub stage1_years: u32,
    /// Annual FCF growth during stage 1.
    pub stage1_growth: f64,
    /// Perpetuity growth after stage 1 (must be < wacc).
    pub terminal_growth: f64,
    /// Discount rate.
    pub wacc: f64,
    /// Net debt (interest-bearing debt − cash) subtracted from EV.
    pub net_debt: f64,
    /// Total shares outstanding.
    pub shares: f64,
}

/// DCF output. `per_share` is ONE scenario's value — always consume it via
/// [`scenarios`] or [`sensitivity`], which produce ranges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DcfResult {
    /// PV of the explicit-stage flows.
    pub pv_stage1: f64,
    /// PV of the terminal value.
    pub pv_terminal: f64,
    /// Enterprise value = pv_stage1 + pv_terminal.
    pub enterprise_value: f64,
    /// Equity value = EV − net debt.
    pub equity_value: f64,
    /// Equity value per share.
    pub per_share: f64,
    /// pv_terminal / enterprise_value — above ~0.8 the result is mostly
    /// terminal-value assumption and should be distrusted.
    pub terminal_share: f64,
}

/// Two-stage FCFF DCF. `None` when the inputs are incoherent
/// (wacc ≤ terminal growth, shares ≤ 0, empty stage 1, non-finite base FCF).
pub fn dcf_fcff(inputs: &DcfInputs) -> Option<DcfResult> {
    let DcfInputs {
        base_fcf,
        stage1_years,
        stage1_growth,
        terminal_growth,
        wacc,
        net_debt,
        shares,
    } = *inputs;
    if !base_fcf.is_finite()
        || stage1_years == 0
        || stage1_years > 30
        || wacc <= terminal_growth
        || wacc <= 0.0
        || shares <= 0.0
    {
        return None;
    }
    let mut pv_stage1 = 0.0;
    let mut fcf = base_fcf;
    for year in 1..=stage1_years {
        fcf *= 1.0 + stage1_growth;
        pv_stage1 += fcf / (1.0 + wacc).powi(year as i32);
    }
    let terminal_value = fcf * (1.0 + terminal_growth) / (wacc - terminal_growth);
    let pv_terminal = terminal_value / (1.0 + wacc).powi(stage1_years as i32);
    let enterprise_value = pv_stage1 + pv_terminal;
    let equity_value = enterprise_value - net_debt;
    if !enterprise_value.is_finite() || enterprise_value <= 0.0 {
        return None;
    }
    Some(DcfResult {
        pv_stage1,
        pv_terminal,
        enterprise_value,
        equity_value,
        per_share: equity_value / shares,
        terminal_share: pv_terminal / enterprise_value,
    })
}

/// Bull/Base/Bear scenario set. Bull = higher growth, lower WACC; Bear the
/// reverse. The answer to "what is it worth" is the RANGE
/// `bear.per_share ..= bull.per_share`, plus `base` as the midpoint case.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScenarioSet {
    /// Pessimistic: growth − spread, wacc + spread.
    pub bear: DcfResult,
    /// Central case (inputs as given).
    pub base: DcfResult,
    /// Optimistic: growth + spread, wacc − spread.
    pub bull: DcfResult,
}

/// Build the three scenarios by shifting stage-1 growth and WACC by
/// `spread` (e.g. 0.02). `None` when any scenario is incoherent.
pub fn scenarios(inputs: &DcfInputs, spread: f64) -> Option<ScenarioSet> {
    let mut bull = *inputs;
    bull.stage1_growth += spread;
    bull.wacc -= spread;
    let mut bear = *inputs;
    bear.stage1_growth -= spread;
    bear.wacc += spread;
    Some(ScenarioSet {
        bear: dcf_fcff(&bear)?,
        base: dcf_fcff(inputs)?,
        bull: dcf_fcff(&bull)?,
    })
}

/// Sensitivity table of per-share values over WACC × terminal-growth grids.
/// Rows follow `waccs`, columns follow `terminal_growths`; incoherent cells
/// (wacc ≤ g) are `None`, never interpolated.
pub fn sensitivity(
    inputs: &DcfInputs,
    waccs: &[f64],
    terminal_growths: &[f64],
) -> Vec<Vec<Option<f64>>> {
    waccs
        .iter()
        .map(|w| {
            terminal_growths
                .iter()
                .map(|g| {
                    let mut variant = *inputs;
                    variant.wacc = *w;
                    variant.terminal_growth = *g;
                    dcf_fcff(&variant).map(|r| r.per_share)
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_golden() {
        // [10,20,30,40,50], current 30 → 3 of 5 values ≤ 30 → 60%.
        assert_eq!(
            percentile(&[10.0, 20.0, 30.0, 40.0, 50.0], 30.0),
            Some(60.0)
        );
        assert_eq!(percentile(&[10.0, 20.0], 5.0), Some(0.0));
        assert_eq!(percentile(&[10.0, 20.0], 25.0), Some(100.0));
        assert_eq!(percentile(&[], 10.0), None);
        // Non-finite entries are skipped, not counted.
        assert_eq!(percentile(&[10.0, f64::NAN, 20.0], 20.0), Some(100.0));
    }

    #[test]
    fn peg_conventions() {
        assert_eq!(peg(Some(30.0), Some(15.0)), Some(2.0));
        assert_eq!(peg(Some(30.0), Some(0.0)), None);
        assert_eq!(peg(Some(30.0), Some(-5.0)), None);
        assert_eq!(peg(Some(-10.0), Some(15.0)), None);
        assert_eq!(peg(Some(30.0), None), None);
    }

    #[test]
    fn dcf_golden_hand_computed() {
        // base_fcf 100, 5y @10%, terminal 3%, wacc 10%, no net debt, 1 share.
        // Stage-1 flows: 110, 121, 133.1, 146.41, 161.051.
        // At wacc = growth each PVs to exactly 100 → pv_stage1 = 500.
        // TV = 161.051·1.03/0.07 = 2369.7504; PV(TV) = 2369.7504/1.1^5
        //    = 1471.4286. EV = 1971.4286. terminal_share ≈ 0.74638.
        let inputs = DcfInputs {
            base_fcf: 100.0,
            stage1_years: 5,
            stage1_growth: 0.10,
            terminal_growth: 0.03,
            wacc: 0.10,
            net_debt: 0.0,
            shares: 1.0,
        };
        let r = dcf_fcff(&inputs).unwrap();
        assert!((r.pv_stage1 - 500.0).abs() < 1e-6);
        assert!((r.pv_terminal - 1471.428571).abs() < 1e-4);
        assert!((r.enterprise_value - 1971.428571).abs() < 1e-4);
        assert!((r.per_share - 1971.428571).abs() < 1e-4);
        assert!((r.terminal_share - 0.746377).abs() < 1e-5);
    }

    #[test]
    fn dcf_rejects_incoherent_inputs() {
        let mut inputs = DcfInputs {
            base_fcf: 100.0,
            stage1_years: 5,
            stage1_growth: 0.05,
            terminal_growth: 0.10,
            wacc: 0.10, // wacc == terminal growth → None
            net_debt: 0.0,
            shares: 1.0,
        };
        assert_eq!(dcf_fcff(&inputs), None);
        inputs.terminal_growth = 0.03;
        inputs.shares = 0.0;
        assert_eq!(dcf_fcff(&inputs), None);
        inputs.shares = 1.0;
        inputs.stage1_years = 0;
        assert_eq!(dcf_fcff(&inputs), None);
    }

    #[test]
    fn dcf_net_debt_reduces_equity_value() {
        let inputs = DcfInputs {
            base_fcf: 100.0,
            stage1_years: 5,
            stage1_growth: 0.05,
            terminal_growth: 0.03,
            wacc: 0.10,
            net_debt: 200.0,
            shares: 10.0,
        };
        let r = dcf_fcff(&inputs).unwrap();
        assert!((r.equity_value - (r.enterprise_value - 200.0)).abs() < 1e-9);
    }

    #[test]
    fn scenarios_produce_an_ordered_range() {
        let inputs = DcfInputs {
            base_fcf: 100.0,
            stage1_years: 5,
            stage1_growth: 0.08,
            terminal_growth: 0.03,
            wacc: 0.10,
            net_debt: 0.0,
            shares: 1.0,
        };
        let s = scenarios(&inputs, 0.02).unwrap();
        assert!(s.bear.per_share < s.base.per_share);
        assert!(s.base.per_share < s.bull.per_share);
    }

    #[test]
    fn sensitivity_grid_shape_and_holes() {
        let inputs = DcfInputs {
            base_fcf: 100.0,
            stage1_years: 5,
            stage1_growth: 0.08,
            terminal_growth: 0.03,
            wacc: 0.10,
            net_debt: 0.0,
            shares: 1.0,
        };
        let grid = sensitivity(&inputs, &[0.09, 0.10, 0.11], &[0.02, 0.03, 0.12]);
        assert_eq!(grid.len(), 3);
        assert!(grid.iter().all(|row| row.len() == 3));
        // wacc 0.11 with terminal growth 0.12 is incoherent → hole.
        assert_eq!(grid[2][2], None);
        // Value falls as wacc rises.
        assert!(grid[0][1].unwrap() > grid[1][1].unwrap());
        assert!(grid[1][1].unwrap() > grid[2][1].unwrap());
    }
}
