//! Quality & distress scores: Piotroski F-score, Altman Z-score (classic and
//! emerging-market Z''), Beneish M-score.
//!
//! Convention notes:
//! - **Piotroski (2000)**: 9 binary criteria vs the *prior fiscal year*.
//!   Each criterion is `Some(true/false)` when computable and `None`
//!   (Missing, not counted) when an input is unavailable — we never guess.
//! - **Altman**: the classic 1968 Z (public manufacturers, X4 = *market*
//!   equity / total liabilities, zones 1.81 / 2.99) and the 1995/2000
//!   emerging-market Z'' = 3.25 + 6.56·X1 + 3.26·X2 + 6.72·X3 + 1.05·X4
//!   (X4 = *book* equity / total liabilities, no sales term, zones
//!   1.10 / 2.60) are BOTH exposed with explicit flags. For most A-share
//!   non-financials Z'' is the appropriate variant; classic Z is provided
//!   for comparability.
//! - **Beneish (1999)**: M = −4.84 + 0.92·DSRI + 0.528·GMI + 0.404·AQI +
//!   0.892·SGI + 0.115·DEPI − 0.172·SGAI + 4.679·TATA − 0.327·LVGI.
//!   Cut-off −1.78 (original paper; −2.22 is a common conservative variant).
//!   Any index whose line items are unavailable is `None` (Missing); the
//!   total is then `None` too — a partial M-score would be a fabrication.

use crate::metrics;
use crate::model::{BalanceSheet, CashFlowStatement, IncomeStatement};

/// One Piotroski criterion outcome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CriterionResult {
    /// Stable machine name, e.g. `"positive_roa"`.
    pub name: &'static str,
    /// Human-readable description of what passed means.
    pub description: &'static str,
    /// `Some(passed)` when computable, `None` when an input is Missing.
    pub passed: Option<bool>,
}

/// Piotroski F-score result: 0–9 over the criteria that were computable.
#[derive(Debug, Clone, PartialEq)]
pub struct FScore {
    /// All 9 criteria in canonical order.
    pub criteria: Vec<CriterionResult>,
    /// Number of passed criteria (Missing criteria do not count).
    pub score: u32,
    /// Number of criteria that were computable (≤ 9).
    pub available: u32,
}

/// Inputs for the Piotroski F-score: current vs prior fiscal year.
/// Build directly or via [`piotroski_input_from`].
#[derive(Debug, Clone, Copy, Default)]
pub struct PiotroskiInput {
    /// ROA current year (NI / avg assets).
    pub roa_curr: Option<f64>,
    /// ROA prior year.
    pub roa_prev: Option<f64>,
    /// Operating cash flow, current year.
    pub cfo_curr: Option<f64>,
    /// Net income, current year (for the accrual criterion CFO > NI).
    pub net_income_curr: Option<f64>,
    /// Long-term-debt / total assets, current year end.
    pub leverage_curr: Option<f64>,
    /// Long-term-debt / total assets, prior year end.
    pub leverage_prev: Option<f64>,
    /// Current ratio, current year end.
    pub current_ratio_curr: Option<f64>,
    /// Current ratio, prior year end.
    pub current_ratio_prev: Option<f64>,
    /// Shares outstanding, current year.
    pub shares_curr: Option<f64>,
    /// Shares outstanding, prior year.
    pub shares_prev: Option<f64>,
    /// Gross margin, current year.
    pub gross_margin_curr: Option<f64>,
    /// Gross margin, prior year.
    pub gross_margin_prev: Option<f64>,
    /// Asset turnover (revenue / avg assets), current year.
    pub asset_turnover_curr: Option<f64>,
    /// Asset turnover, prior year.
    pub asset_turnover_prev: Option<f64>,
}

/// Build a [`PiotroskiInput`] from statements.
///
/// `bs_open_prev` is the balance sheet at the end of the year *before* the
/// prior year (needed to average prior-year assets); pass an empty/default
/// sheet when history runs out — the average falls back to the single known
/// balance (see [`metrics::average_balance`]).
pub fn piotroski_input_from(
    inc_curr: &IncomeStatement,
    inc_prev: &IncomeStatement,
    cf_curr: &CashFlowStatement,
    bs_open_prev: &BalanceSheet,
    bs_prev: &BalanceSheet,
    bs_curr: &BalanceSheet,
) -> PiotroskiInput {
    let roa = |inc: &IncomeStatement, b0: &BalanceSheet, b1: &BalanceSheet| {
        metrics::roa(inc.net_profit, b0.total_assets, b1.total_assets)
    };
    let lev = |bs: &BalanceSheet| {
        metrics::div_public(
            bs.long_term_debt
                .map(|d| d + bs.bonds_payable.unwrap_or(0.0) + bs.lease_liabilities.unwrap_or(0.0)),
            bs.total_assets,
        )
    };
    PiotroskiInput {
        roa_curr: roa(inc_curr, bs_prev, bs_curr),
        roa_prev: roa(inc_prev, bs_open_prev, bs_prev),
        cfo_curr: cf_curr.net_cfo,
        net_income_curr: inc_curr.net_profit,
        leverage_curr: lev(bs_curr),
        leverage_prev: lev(bs_prev),
        current_ratio_curr: metrics::current_ratio(
            bs_curr.total_current_assets,
            bs_curr.total_current_liabilities,
        ),
        current_ratio_prev: metrics::current_ratio(
            bs_prev.total_current_assets,
            bs_prev.total_current_liabilities,
        ),
        shares_curr: bs_curr.share_capital,
        shares_prev: bs_prev.share_capital,
        gross_margin_curr: metrics::gross_margin(
            inc_curr.operating_revenue,
            inc_curr.operating_cost,
        ),
        gross_margin_prev: metrics::gross_margin(
            inc_prev.operating_revenue,
            inc_prev.operating_cost,
        ),
        asset_turnover_curr: {
            let avg = metrics::average_balance(bs_prev.total_assets, bs_curr.total_assets);
            metrics::div_public(inc_curr.total_operating_revenue, avg)
        },
        asset_turnover_prev: {
            let avg = metrics::average_balance(bs_open_prev.total_assets, bs_prev.total_assets);
            metrics::div_public(inc_prev.total_operating_revenue, avg)
        },
    }
}

/// Piotroski F-score over the 9 canonical criteria.
pub fn piotroski(input: &PiotroskiInput) -> FScore {
    let gt = |a: Option<f64>, b: Option<f64>| a.zip(b).map(|(x, y)| x > y);
    let criteria = vec![
        CriterionResult {
            name: "positive_roa",
            description: "ROA > 0 (profitability)",
            passed: input.roa_curr.map(|r| r > 0.0),
        },
        CriterionResult {
            name: "positive_cfo",
            description: "Operating cash flow > 0",
            passed: input.cfo_curr.map(|c| c > 0.0),
        },
        CriterionResult {
            name: "roa_improving",
            description: "ROA higher than prior year",
            passed: gt(input.roa_curr, input.roa_prev),
        },
        CriterionResult {
            name: "accrual_quality",
            description: "CFO > net income (earnings backed by cash)",
            passed: gt(input.cfo_curr, input.net_income_curr),
        },
        CriterionResult {
            name: "leverage_falling",
            description: "Long-term leverage not higher than prior year",
            passed: input
                .leverage_curr
                .zip(input.leverage_prev)
                .map(|(c, p)| c <= p),
        },
        CriterionResult {
            name: "liquidity_improving",
            description: "Current ratio higher than prior year",
            passed: gt(input.current_ratio_curr, input.current_ratio_prev),
        },
        CriterionResult {
            name: "no_dilution",
            description: "No new shares issued (share count not increased)",
            passed: input
                .shares_curr
                .zip(input.shares_prev)
                .map(|(c, p)| c <= p),
        },
        CriterionResult {
            name: "gross_margin_improving",
            description: "Gross margin higher than prior year",
            passed: gt(input.gross_margin_curr, input.gross_margin_prev),
        },
        CriterionResult {
            name: "asset_turnover_improving",
            description: "Asset turnover higher than prior year",
            passed: gt(input.asset_turnover_curr, input.asset_turnover_prev),
        },
    ];
    let score = criteria.iter().filter(|c| c.passed == Some(true)).count() as u32;
    let available = criteria.iter().filter(|c| c.passed.is_some()).count() as u32;
    FScore {
        criteria,
        score,
        available,
    }
}

/// Altman zone classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AltmanZone {
    /// Above the safe cut-off.
    Safe,
    /// Between distress and safe cut-offs.
    Grey,
    /// Below the distress cut-off.
    Distress,
}

/// Inputs for the Altman Z-score variants. All are raw CNY amounts from the
/// balance sheet / income statement; ratios are derived internally.
#[derive(Debug, Clone, Copy, Default)]
pub struct AltmanInput {
    /// Working capital (current assets − current liabilities).
    pub working_capital: Option<f64>,
    /// Retained earnings (未分配利润).
    pub retained_earnings: Option<f64>,
    /// EBIT (use [`metrics::nopat`]'s pre-tax sibling: total_profit +
    /// finance_expense; see [`altman_ebit`]).
    pub ebit: Option<f64>,
    /// Market value of equity (total market cap) — classic Z only.
    pub market_cap: Option<f64>,
    /// Book value of equity (股东权益合计) — Z'' X4.
    pub book_equity: Option<f64>,
    /// Total liabilities.
    pub total_liabilities: Option<f64>,
    /// Total assets.
    pub total_assets: Option<f64>,
    /// Revenue (营业总收入) — classic Z X5 only.
    pub revenue: Option<f64>,
}

/// EBIT under the Altman convention = pre-tax profit + interest expense.
/// We approximate interest expense with the whole 财务费用 line (the EM
/// statement does not split it); see [`metrics::nopat`] for the same note.
pub fn altman_ebit(stmt: &IncomeStatement) -> Option<f64> {
    Some(stmt.total_profit? + stmt.finance_expense.unwrap_or(0.0))
}

/// Both Altman variants with explicit convention flags.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AltmanZ {
    /// Classic 1968 Z (public manufacturers): X4 = market equity / liabilities.
    /// `None` when any of X1..X5 is Missing.
    pub classic: Option<f64>,
    /// Zone under the classic cut-offs (distress < 1.81, safe > 2.99).
    pub classic_zone: Option<AltmanZone>,
    /// Emerging-market Z'' = 3.25 + 6.56·X1 + 3.26·X2 + 6.72·X3 + 1.05·X4
    /// (book equity, no sales term). `None` when any of X1..X4 is Missing.
    pub z_emerging: Option<f64>,
    /// Zone under the Z'' cut-offs (distress < 1.10, safe > 2.60).
    pub emerging_zone: Option<AltmanZone>,
}

/// Compute both Altman variants.
pub fn altman(input: &AltmanInput) -> AltmanZ {
    let ta = input.total_assets.filter(|a| *a > 0.0);
    let tl = input.total_liabilities.filter(|l| *l > 0.0);
    let x1 = metrics::div_public(input.working_capital, ta);
    let x2 = metrics::div_public(input.retained_earnings, ta);
    let x3 = metrics::div_public(input.ebit, ta);
    let x4_mkt = metrics::div_public(input.market_cap, tl);
    let x4_book = metrics::div_public(input.book_equity, tl);
    let x5 = metrics::div_public(input.revenue, ta);

    let classic = match (x1, x2, x3, x4_mkt, x5) {
        (Some(a), Some(b), Some(c), Some(d), Some(e)) => {
            Some(1.2 * a + 1.4 * b + 3.3 * c + 0.6 * d + 1.0 * e)
        }
        _ => None,
    };
    let classic_zone = classic.map(|z| {
        if z > 2.99 {
            AltmanZone::Safe
        } else if z < 1.81 {
            AltmanZone::Distress
        } else {
            AltmanZone::Grey
        }
    });

    let z_emerging = match (x1, x2, x3, x4_book) {
        (Some(a), Some(b), Some(c), Some(d)) => {
            Some(3.25 + 6.56 * a + 3.26 * b + 6.72 * c + 1.05 * d)
        }
        _ => None,
    };
    let emerging_zone = z_emerging.map(|z| {
        if z > 2.60 {
            AltmanZone::Safe
        } else if z < 1.10 {
            AltmanZone::Distress
        } else {
            AltmanZone::Grey
        }
    });

    AltmanZ {
        classic,
        classic_zone,
        z_emerging,
        emerging_zone,
    }
}

/// The 8 Beneish indices. `None` = Missing (line items unavailable).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BeneishIndices {
    /// Days sales in receivables index: DSO_t / DSO_{t-1}.
    pub dsri: Option<f64>,
    /// Gross margin index: GM_{t-1} / GM_t.
    pub gmi: Option<f64>,
    /// Asset quality index: AQI_t / AQI_{t-1}, where
    /// AQI = 1 − (current assets + net PPE) / total assets.
    pub aqi: Option<f64>,
    /// Sales growth index: revenue_t / revenue_{t-1}.
    pub sgi: Option<f64>,
    /// Depreciation index: DEPR_{t-1} / DEPR_t, where
    /// DEPR = depreciation / (depreciation + net PPE).
    pub depi: Option<f64>,
    /// SG&A index: (SGA_t / rev_t) / (SGA_{t-1} / rev_{t-1});
    /// SGA = selling + admin expense.
    pub sgai: Option<f64>,
    /// Leverage index: LVGI_t / LVGI_{t-1}, where
    /// LVGI = (current liabilities + long-term debt) / total assets.
    pub lvgi: Option<f64>,
    /// Total accruals to total assets: (NI − CFO) / total assets.
    pub tata: Option<f64>,
}

/// Beneish M-score result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MScore {
    /// The 8 indices (Missing where not computable).
    pub indices: BeneishIndices,
    /// M-score; `None` unless ALL 8 indices are available.
    pub total: Option<f64>,
    /// Likely manipulator flag under the original −1.78 cut-off.
    pub likely_manipulator: Option<bool>,
}

/// Beneish cut-off from the original 1999 paper.
pub const BENEISH_CUTOFF: f64 = -1.78;

/// Compute the Beneish M-score. The total is `None` when any index is
/// Missing — a partial sum is not an M-score.
pub fn beneish(indices: &BeneishIndices) -> MScore {
    let i = indices;
    let total = match (i.dsri, i.gmi, i.aqi, i.sgi, i.depi, i.sgai, i.tata, i.lvgi) {
        (
            Some(dsri),
            Some(gmi),
            Some(aqi),
            Some(sgi),
            Some(depi),
            Some(sgai),
            Some(tata),
            Some(lvgi),
        ) => Some(
            -4.84 + 0.92 * dsri + 0.528 * gmi + 0.404 * aqi + 0.892 * sgi + 0.115 * depi
                - 0.172 * sgai
                + 4.679 * tata
                - 0.327 * lvgi,
        ),
        _ => None,
    };
    MScore {
        indices: *indices,
        total,
        likely_manipulator: total.map(|m| m > BENEISH_CUTOFF),
    }
}

/// Build the Beneish indices from two consecutive years of statements.
/// `cf_*` supply CFO and depreciation; `bs_*` the balance sheets. Net PPE is
/// approximated by 固定资产 + 在建工程 (the EM sheet has no separate
/// accumulated-depreciation line).
pub fn beneish_indices_from(
    inc_curr: &IncomeStatement,
    inc_prev: &IncomeStatement,
    cf_curr: &CashFlowStatement,
    cf_prev: &CashFlowStatement,
    bs_curr: &BalanceSheet,
    bs_prev: &BalanceSheet,
) -> BeneishIndices {
    let recv = |bs: &BalanceSheet| bs.notes_and_accounts_receivable.or(bs.accounts_receivable);
    let rev = |inc: &IncomeStatement| inc.total_operating_revenue;
    let ppe = |bs: &BalanceSheet| {
        bs.fixed_assets
            .map(|f| f + bs.construction_in_progress.unwrap_or(0.0))
    };
    // Ratio-of-days DSO needs no day-count: DSO_t/DSO_{t-1} =
    // (recv_t/rev_t) / (recv_{t-1}/rev_{t-1}).
    let dsri = match (
        metrics::div_public(recv(bs_curr), rev(inc_curr)),
        metrics::div_public(recv(bs_prev), rev(inc_prev)),
    ) {
        (Some(c), Some(p)) if p > 0.0 => Some(c / p),
        _ => None,
    };
    let gmi = match (
        metrics::gross_margin(inc_prev.operating_revenue, inc_prev.operating_cost),
        metrics::gross_margin(inc_curr.operating_revenue, inc_curr.operating_cost),
    ) {
        (Some(p), Some(c)) if c != 0.0 => Some(p / c),
        _ => None,
    };
    let aqi_of = |bs: &BalanceSheet| {
        let ta = bs.total_assets.filter(|a| *a > 0.0)?;
        let ca = bs.total_current_assets?;
        let fixed = ppe(bs)?;
        Some(1.0 - (ca + fixed) / ta)
    };
    let aqi = match (aqi_of(bs_curr), aqi_of(bs_prev)) {
        (Some(c), Some(p)) if p != 0.0 => Some(c / p),
        _ => None,
    };
    let sgi = match (rev(inc_curr), rev(inc_prev)) {
        (Some(c), Some(p)) if p > 0.0 => Some(c / p),
        _ => None,
    };
    let depr_rate = |cf: &CashFlowStatement, bs: &BalanceSheet| {
        let d = cf.depreciation.filter(|d| *d > 0.0)?;
        let fixed = ppe(bs)?;
        Some(d / (d + fixed))
    };
    let depi = match (depr_rate(cf_curr, bs_curr), depr_rate(cf_prev, bs_prev)) {
        (Some(c), Some(p)) if c > 0.0 => Some(p / c),
        _ => None,
    };
    let sga_ratio = |inc: &IncomeStatement| {
        let r = rev(inc).filter(|r| *r > 0.0)?;
        let sga = inc.selling_expense.unwrap_or(0.0) + inc.admin_expense.unwrap_or(0.0);
        Some(sga / r)
    };
    let sgai = match (sga_ratio(inc_curr), sga_ratio(inc_prev)) {
        (Some(c), Some(p)) if p > 0.0 => Some(c / p),
        _ => None,
    };
    let lvg = |bs: &BalanceSheet| {
        let ta = bs.total_assets.filter(|a| *a > 0.0)?;
        let lev = bs.total_current_liabilities?
            + bs.long_term_debt.unwrap_or(0.0)
            + bs.bonds_payable.unwrap_or(0.0);
        Some(lev / ta)
    };
    let lvgi = match (lvg(bs_curr), lvg(bs_prev)) {
        (Some(c), Some(p)) if p > 0.0 => Some(c / p),
        _ => None,
    };
    let tata = match (inc_curr.net_profit, cf_curr.net_cfo, bs_curr.total_assets) {
        (Some(ni), Some(cfo), Some(ta)) if ta > 0.0 => Some((ni - cfo) / ta),
        _ => None,
    };
    BeneishIndices {
        dsri,
        gmi,
        aqi,
        sgi,
        depi,
        sgai,
        lvgi,
        tata,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fscore_all_pass_is_nine() {
        let input = PiotroskiInput {
            roa_curr: Some(0.1),
            roa_prev: Some(0.08),
            cfo_curr: Some(150.0),
            net_income_curr: Some(100.0),
            leverage_curr: Some(0.1),
            leverage_prev: Some(0.2),
            current_ratio_curr: Some(2.0),
            current_ratio_prev: Some(1.5),
            shares_curr: Some(100.0),
            shares_prev: Some(100.0),
            gross_margin_curr: Some(0.4),
            gross_margin_prev: Some(0.35),
            asset_turnover_curr: Some(1.1),
            asset_turnover_prev: Some(1.0),
        };
        let fs = piotroski(&input);
        assert_eq!(fs.score, 9);
        assert_eq!(fs.available, 9);
    }

    #[test]
    fn fscore_constructed_mixed_case() {
        // Pass: positive_roa, positive_cfo, no_dilution. Fail: roa_improving,
        // accrual (CFO < NI), leverage rising, liquidity falling, GM falling,
        // AT falling. Score = 3.
        let input = PiotroskiInput {
            roa_curr: Some(0.05),
            roa_prev: Some(0.08),
            cfo_curr: Some(80.0),
            net_income_curr: Some(100.0),
            leverage_curr: Some(0.3),
            leverage_prev: Some(0.2),
            current_ratio_curr: Some(1.0),
            current_ratio_prev: Some(1.5),
            shares_curr: Some(100.0),
            shares_prev: Some(100.0),
            gross_margin_curr: Some(0.3),
            gross_margin_prev: Some(0.35),
            asset_turnover_curr: Some(0.9),
            asset_turnover_prev: Some(1.0),
        };
        let fs = piotroski(&input);
        assert_eq!(fs.score, 3);
        let passed: Vec<&str> = fs
            .criteria
            .iter()
            .filter(|c| c.passed == Some(true))
            .map(|c| c.name)
            .collect();
        assert_eq!(passed, ["positive_roa", "positive_cfo", "no_dilution"]);
    }

    #[test]
    fn fscore_missing_inputs_not_counted() {
        let input = PiotroskiInput {
            roa_curr: Some(0.1),
            ..Default::default()
        };
        let fs = piotroski(&input);
        assert_eq!(fs.score, 1);
        assert_eq!(fs.available, 1);
    }

    #[test]
    fn altman_z_emerging_golden() {
        // Hand-computed: X1=X2=X3=0.1, X4=1.0 →
        // Z'' = 3.25 + 6.56·0.1 + 3.26·0.1 + 6.72·0.1 + 1.05·1
        //     = 3.25 + 0.656 + 0.326 + 0.672 + 1.05 = 5.954 → Safe (>2.60).
        let input = AltmanInput {
            working_capital: Some(10.0),
            retained_earnings: Some(10.0),
            ebit: Some(10.0),
            book_equity: Some(100.0),
            total_liabilities: Some(100.0),
            total_assets: Some(100.0),
            ..Default::default()
        };
        let z = altman(&input);
        let zz = z.z_emerging.unwrap();
        assert!((zz - 5.954).abs() < 1e-9);
        assert_eq!(z.emerging_zone, Some(AltmanZone::Safe));
        // Classic is Missing: market cap and revenue not provided.
        assert_eq!(z.classic, None);
        assert_eq!(z.classic_zone, None);
    }

    #[test]
    fn altman_classic_golden() {
        // X1..X5 all 0.5 → Z = (1.2+1.4+3.3+0.6+1.0)·0.5 = 7.5·0.5 = 3.75 → Safe.
        let input = AltmanInput {
            working_capital: Some(50.0),
            retained_earnings: Some(50.0),
            ebit: Some(50.0),
            market_cap: Some(50.0),
            book_equity: Some(50.0),
            total_liabilities: Some(100.0),
            total_assets: Some(100.0),
            revenue: Some(50.0),
        };
        let z = altman(&input);
        assert!((z.classic.unwrap() - 3.75).abs() < 1e-12);
        assert_eq!(z.classic_zone, Some(AltmanZone::Safe));
        // Z'': 3.25 + (6.56+3.26+6.72)·0.5 + 1.05·0.5 = 3.25 + 8.27 + 0.525 = 12.045.
        assert!((z.z_emerging.unwrap() - 12.045).abs() < 1e-9);
    }

    #[test]
    fn altman_distress_zone() {
        let input = AltmanInput {
            working_capital: Some(-50.0),
            retained_earnings: Some(-50.0),
            ebit: Some(-10.0),
            book_equity: Some(10.0),
            total_liabilities: Some(100.0),
            total_assets: Some(100.0),
            ..Default::default()
        };
        let z = altman(&input);
        assert_eq!(z.emerging_zone, Some(AltmanZone::Distress));
    }

    #[test]
    fn beneish_golden() {
        // All indices = 1.0 →
        // M = −4.84 + 0.92 + 0.528 + 0.404 + 0.892 + 0.115 − 0.172 + 4.679 − 0.327
        //   = 2.199 → above −1.78 → flagged.
        let idx = BeneishIndices {
            dsri: Some(1.0),
            gmi: Some(1.0),
            aqi: Some(1.0),
            sgi: Some(1.0),
            depi: Some(1.0),
            sgai: Some(1.0),
            lvgi: Some(1.0),
            tata: Some(1.0),
        };
        let m = beneish(&idx);
        assert!((m.total.unwrap() - 2.199).abs() < 1e-9);
        assert_eq!(m.likely_manipulator, Some(true));
    }

    #[test]
    fn beneish_missing_index_means_missing_total() {
        let idx = BeneishIndices {
            dsri: Some(1.0),
            ..Default::default()
        };
        let m = beneish(&idx);
        assert_eq!(m.total, None);
        assert_eq!(m.likely_manipulator, None);
        assert_eq!(m.indices.dsri, Some(1.0));
    }
}
