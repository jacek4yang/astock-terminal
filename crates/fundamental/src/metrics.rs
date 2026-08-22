//! Fundamental metrics: pure functions over the normalized model.
//!
//! Conventions (all documented per formula; alternatives noted where the
//! industry uses more than one):
//! - **Averaging**: ROE/ROA/ROIC and turnover ratios use the *average* of the
//!   opening and closing balance ((begin + end) / 2), the standard textbook
//!   convention. EM's own `ROEJQ` uses the CSRC weighted-average convention
//!   instead — that vendor number is exposed separately in
//!   [`crate::model::KeyIndicators::roe_weighted`].
//! - **Cumulative vs single quarter**: EM income/cash-flow rows are
//!   cumulative (YTD). [`to_single_quarters`] differences them.
//! - All functions return `Option`: any missing or non-positive denominator
//!   yields `None` (Missing), never a fabricated number.

use crate::model::{BalanceSheet, CashFlowStatement, IncomeStatement, PeriodMeta, ReportType};
use chrono::{Datelike, NaiveDate};

/// A value tied to a period end, used by the growth helpers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeriodValue {
    /// Period end date.
    pub period_end: NaiveDate,
    /// The value for that period.
    pub value: f64,
}

/// Safe division: `None` when either side is missing or the denominator
/// is zero.
fn div(numerator: Option<f64>, denominator: Option<f64>) -> Option<f64> {
    let (n, d) = (numerator?, denominator?);
    if d == 0.0 {
        return None;
    }
    Some(n / d)
}

/// Public version of the internal safe division, for the other analytics
/// modules (`scores`, `anomaly`, `valuation`). Same semantics: missing
/// operands or a zero denominator yield `None`.
pub fn div_public(numerator: Option<f64>, denominator: Option<f64>) -> Option<f64> {
    div(numerator, denominator)
}

/// Mean of two period-end balances. If only one side is known, returns it
/// (documented approximation: first period of history has no opening
/// balance); `None` when both are missing.
pub fn average_balance(begin: Option<f64>, end: Option<f64>) -> Option<f64> {
    match (begin, end) {
        (Some(b), Some(e)) => Some((b + e) / 2.0),
        (Some(b), None) => Some(b),
        (None, Some(e)) => Some(e),
        (None, None) => None,
    }
}

// ---------------------------------------------------------------------------
// Growth
// ---------------------------------------------------------------------------

/// Growth rate `(curr - prev) / |prev|`. `None` when either value is missing
/// or `prev == 0`. Using `|prev|` keeps the sign intuitive when the base is
/// negative (e.g. loss narrowing reads as positive growth); note this differs
/// from EM's own `_YOY` fields, which are undefined for negative bases.
pub fn growth(curr: Option<f64>, prev: Option<f64>) -> Option<f64> {
    let (c, p) = (curr?, prev?);
    if p == 0.0 {
        return None;
    }
    Some((c - p) / p.abs())
}

/// Year-over-year growth of a period series (quarterly or annual):
/// matches each period to the same period one year earlier (same month/day).
/// Input must be sorted ascending; output is `(period_end, yoy)` pairs for
/// periods that have a same-period match one year back.
pub fn yoy_growth(series: &[PeriodValue]) -> Vec<(NaiveDate, f64)> {
    let mut out = Vec::new();
    for (i, pv) in series.iter().enumerate() {
        let target = prev_year_same_period(&pv.period_end);
        if let Some(prev) = series[..i].iter().rev().find(|p| p.period_end == target) {
            if let Some(g) = growth(Some(pv.value), Some(prev.value)) {
                out.push((pv.period_end, g));
            }
        }
    }
    out
}

/// Quarter-over-quarter growth of consecutive entries in the series.
pub fn qoq_growth(series: &[PeriodValue]) -> Vec<(NaiveDate, f64)> {
    let mut out = Vec::new();
    for w in series.windows(2) {
        if let Some(g) = growth(Some(w[1].value), Some(w[0].value)) {
            out.push((w[1].period_end, g));
        }
    }
    out
}

/// Same calendar period one year earlier (2025-12-31 → 2024-12-31).
/// Feb 29 maps to Feb 28 in non-leap years.
fn prev_year_same_period(d: &NaiveDate) -> NaiveDate {
    d.with_year(d.year() - 1)
        .unwrap_or_else(|| d.with_month(2).and_then(|x| x.with_day(28)).unwrap_or(*d))
}

// ---------------------------------------------------------------------------
// Single-quarter derivation
// ---------------------------------------------------------------------------

/// A single-quarter income statement (derived by differencing cumulative
/// rows). Fields that cannot be differenced meaningfully (EPS is still
/// differenced — cumulative EPS differences are exact) stay subtractive.
pub fn to_single_quarters(cumulative: &[IncomeStatement]) -> Vec<IncomeStatement> {
    let mut out = Vec::new();
    for (i, cur) in cumulative.iter().enumerate() {
        let Some(meta) = cur.meta else { continue };
        if meta.report_type == ReportType::Q1 {
            // Q1 cumulative IS the single quarter.
            out.push(cur.clone());
            continue;
        }
        // Find the previous cumulative row of the same fiscal year.
        let prev = cumulative[..i].iter().rev().find(|p| {
            p.meta
                .is_some_and(|m| m.period_end.year() == meta.period_end.year())
        });
        let sub = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(x), Some(y)) => Some(x - y),
            // No prior cumulative row (start of history): keep YTD as-is
            // rather than inventing a difference. Documented approximation.
            (Some(x), None) => Some(x),
            _ => None,
        };
        let p = prev.cloned().unwrap_or_default();
        out.push(IncomeStatement {
            meta: Some(meta),
            total_operating_revenue: sub(cur.total_operating_revenue, p.total_operating_revenue),
            operating_revenue: sub(cur.operating_revenue, p.operating_revenue),
            operating_cost: sub(cur.operating_cost, p.operating_cost),
            taxes_and_surcharges: sub(cur.taxes_and_surcharges, p.taxes_and_surcharges),
            selling_expense: sub(cur.selling_expense, p.selling_expense),
            admin_expense: sub(cur.admin_expense, p.admin_expense),
            rd_expense: sub(cur.rd_expense, p.rd_expense),
            finance_expense: sub(cur.finance_expense, p.finance_expense),
            invest_income: sub(cur.invest_income, p.invest_income),
            fairvalue_change_income: sub(cur.fairvalue_change_income, p.fairvalue_change_income),
            operating_profit: sub(cur.operating_profit, p.operating_profit),
            total_profit: sub(cur.total_profit, p.total_profit),
            income_tax: sub(cur.income_tax, p.income_tax),
            net_profit: sub(cur.net_profit, p.net_profit),
            net_profit_parent: sub(cur.net_profit_parent, p.net_profit_parent),
            net_profit_parent_deducted: sub(
                cur.net_profit_parent_deducted,
                p.net_profit_parent_deducted,
            ),
            minority_profit: sub(cur.minority_profit, p.minority_profit),
            basic_eps: sub(cur.basic_eps, p.basic_eps),
        });
    }
    out
}

/// Same differencing for cumulative cash flow statements.
pub fn cashflow_to_single_quarters(cumulative: &[CashFlowStatement]) -> Vec<CashFlowStatement> {
    let mut out = Vec::new();
    for (i, cur) in cumulative.iter().enumerate() {
        let Some(meta) = cur.meta else { continue };
        if meta.report_type == ReportType::Q1 {
            out.push(cur.clone());
            continue;
        }
        let prev = cumulative[..i].iter().rev().find(|p| {
            p.meta
                .is_some_and(|m| m.period_end.year() == meta.period_end.year())
        });
        let sub = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(x), Some(y)) => Some(x - y),
            (Some(x), None) => Some(x),
            _ => None,
        };
        let p = prev.cloned().unwrap_or_default();
        out.push(CashFlowStatement {
            meta: Some(meta),
            cash_from_sales: sub(cur.cash_from_sales, p.cash_from_sales),
            net_cfo: sub(cur.net_cfo, p.net_cfo),
            net_cfi: sub(cur.net_cfi, p.net_cfi),
            net_cff: sub(cur.net_cff, p.net_cff),
            capex: sub(cur.capex, p.capex),
            // Stock variable: not differenced.
            end_cash_and_equivalents: cur.end_cash_and_equivalents,
            depreciation: sub(cur.depreciation, p.depreciation),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Margins
// ---------------------------------------------------------------------------

/// Gross margin = (operating_revenue − operating_cost) / operating_revenue.
/// Convention: uses 营业收入/营业成本 (not 营业总收入), the CSRC statement
/// definition; EM's XSMLL uses the same pair.
pub fn gross_margin(revenue: Option<f64>, cost: Option<f64>) -> Option<f64> {
    let (r, c) = (revenue?, cost?);
    if r <= 0.0 {
        return None;
    }
    Some((r - c) / r)
}

/// Operating margin = 营业利润 / 营业总收入.
pub fn operating_margin(op_profit: Option<f64>, total_revenue: Option<f64>) -> Option<f64> {
    let r = total_revenue?;
    if r <= 0.0 {
        return None;
    }
    div(op_profit, Some(r))
}

/// Net margin = 净利润(含少数) / 营业总收入, matching EM's XSJLL convention.
pub fn net_margin(net_profit: Option<f64>, total_revenue: Option<f64>) -> Option<f64> {
    let r = total_revenue?;
    if r <= 0.0 {
        return None;
    }
    div(net_profit, Some(r))
}

// ---------------------------------------------------------------------------
// Returns on capital
// ---------------------------------------------------------------------------

/// ROE = 归母净利润 / average 归母权益. Average-of-endpoints convention
/// (see module docs). Returns a ratio (0.10 = 10%).
pub fn roe(
    net_profit_parent: Option<f64>,
    equity_begin: Option<f64>,
    equity_end: Option<f64>,
) -> Option<f64> {
    let avg = average_balance(equity_begin, equity_end)?;
    if avg <= 0.0 {
        return None;
    }
    div(net_profit_parent, Some(avg))
}

/// ROA = 净利润(含少数) / average total assets. Ratio, not percent.
pub fn roa(
    net_profit: Option<f64>,
    assets_begin: Option<f64>,
    assets_end: Option<f64>,
) -> Option<f64> {
    let avg = average_balance(assets_begin, assets_end)?;
    if avg <= 0.0 {
        return None;
    }
    div(net_profit, Some(avg))
}

/// Effective tax rate = 所得税 / 利润总额, clamped to [0, 1]. `None` when
/// pre-tax profit ≤ 0 (rate meaningless for loss-makers).
pub fn effective_tax_rate(income_tax: Option<f64>, total_profit: Option<f64>) -> Option<f64> {
    let (t, p) = (income_tax?, total_profit?);
    if p <= 0.0 {
        return None;
    }
    Some((t / p).clamp(0.0, 1.0))
}

/// NOPAT = EBIT × (1 − effective tax rate), with EBIT ≈ 利润总额 + 财务费用.
/// Convention note: true EBIT adds back *interest* expense only; the EM
/// income statement does not split 财务费用 into interest vs other, so we add
/// back the whole line (a common approximation; for most A-share
/// non-financials 财务费用 is dominated by interest).
pub fn nopat(stmt: &IncomeStatement) -> Option<f64> {
    let total_profit = stmt.total_profit?;
    let ebit = total_profit + stmt.finance_expense.unwrap_or(0.0);
    let tax_rate = effective_tax_rate(stmt.income_tax, stmt.total_profit).unwrap_or(0.0);
    Some(ebit * (1.0 - tax_rate))
}

/// Invested capital = 股东权益合计 + interest-bearing debt − 货币资金.
/// Convention: "excess cash" is approximated by *all* monetary funds (the
/// operating-cash split is not disclosed); documented simplification.
pub fn invested_capital(bs: &BalanceSheet) -> Option<f64> {
    let equity = bs.total_equity?;
    let debt = bs.interest_bearing_debt().unwrap_or(0.0);
    let cash = bs.monetary_funds.unwrap_or(0.0);
    Some(equity + debt - cash)
}

/// ROIC = NOPAT / average invested capital. Ratio, not percent.
pub fn roic(nopat_value: Option<f64>, ic_begin: Option<f64>, ic_end: Option<f64>) -> Option<f64> {
    let avg = average_balance(ic_begin, ic_end)?;
    if avg <= 0.0 {
        return None;
    }
    div(nopat_value, Some(avg))
}

// ---------------------------------------------------------------------------
// DuPont
// ---------------------------------------------------------------------------

/// Classic 3-factor DuPont decomposition:
/// ROE = net margin × asset turnover × equity multiplier
///     = (NI/revenue) × (revenue/avg assets) × (avg assets/avg equity).
/// Uses 归母净利润 and 营业总收入; the product reproduces [`roe`] exactly
/// (up to float error), which the unit test asserts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dupont {
    /// 归母净利润 / 营业总收入.
    pub net_margin: f64,
    /// 营业总收入 / average total assets.
    pub asset_turnover: f64,
    /// Average total assets / average 归母权益.
    pub equity_multiplier: f64,
    /// Product of the three factors (= ROE under the average convention).
    pub roe: f64,
}

/// Compute the 3-factor DuPont decomposition. `None` when any leg is missing
/// or has a non-positive denominator.
pub fn dupont(
    stmt: &IncomeStatement,
    bs_begin: &BalanceSheet,
    bs_end: &BalanceSheet,
) -> Option<Dupont> {
    let revenue = stmt.total_operating_revenue?;
    let np = stmt.net_profit_parent?;
    let avg_assets = average_balance(bs_begin.total_assets, bs_end.total_assets)?;
    let avg_equity = average_balance(bs_begin.total_parent_equity, bs_end.total_parent_equity)?;
    if revenue <= 0.0 || avg_assets <= 0.0 || avg_equity <= 0.0 {
        return None;
    }
    let net_margin = np / revenue;
    let asset_turnover = revenue / avg_assets;
    let equity_multiplier = avg_assets / avg_equity;
    Some(Dupont {
        net_margin,
        asset_turnover,
        equity_multiplier,
        roe: net_margin * asset_turnover * equity_multiplier,
    })
}

// ---------------------------------------------------------------------------
// Cash, working capital, liquidity, leverage
// ---------------------------------------------------------------------------

/// Free cash flow = CFO − capex. Convention: this is the common
/// "owner-earnings lite" proxy, NOT strict FCFF (which adds back after-tax
/// interest); the DCF in [`crate::valuation`] documents the same proxy.
pub fn fcf(cfo: Option<f64>, capex: Option<f64>) -> Option<f64> {
    Some(cfo? - capex?)
}

/// Working capital = current assets − current liabilities.
pub fn working_capital(
    total_current_assets: Option<f64>,
    total_current_liabilities: Option<f64>,
) -> Option<f64> {
    Some(total_current_assets? - total_current_liabilities?)
}

/// Days sales outstanding = avg receivables / revenue × days.
/// Convention: uses 应收票据及应收账款 when available (broader), else 应收账款.
pub fn dso(
    receivables_begin: Option<f64>,
    receivables_end: Option<f64>,
    revenue: Option<f64>,
    days: f64,
) -> Option<f64> {
    let avg = average_balance(receivables_begin, receivables_end)?;
    let r = revenue?;
    if r <= 0.0 {
        return None;
    }
    Some(avg / r * days)
}

/// Days inventory outstanding = avg inventory / operating cost × days.
pub fn dio(
    inventory_begin: Option<f64>,
    inventory_end: Option<f64>,
    operating_cost: Option<f64>,
    days: f64,
) -> Option<f64> {
    let avg = average_balance(inventory_begin, inventory_end)?;
    let c = operating_cost?;
    if c <= 0.0 {
        return None;
    }
    Some(avg / c * days)
}

/// Days payables outstanding = avg payables / operating cost × days.
/// Convention: uses 应付票据及应付账款 when available, else 应付账款.
pub fn dpo(
    payables_begin: Option<f64>,
    payables_end: Option<f64>,
    operating_cost: Option<f64>,
    days: f64,
) -> Option<f64> {
    let avg = average_balance(payables_begin, payables_end)?;
    let c = operating_cost?;
    if c <= 0.0 {
        return None;
    }
    Some(avg / c * days)
}

/// Cash conversion cycle = DSO + DIO − DPO (days).
pub fn cash_conversion_cycle(dso: Option<f64>, dio: Option<f64>, dpo: Option<f64>) -> Option<f64> {
    Some(dso? + dio? - dpo?)
}

/// Current ratio = current assets / current liabilities.
pub fn current_ratio(
    total_current_assets: Option<f64>,
    total_current_liabilities: Option<f64>,
) -> Option<f64> {
    let l = total_current_liabilities?;
    if l <= 0.0 {
        return None;
    }
    div(total_current_assets, Some(l))
}

/// Quick ratio = (current assets − inventory) / current liabilities.
pub fn quick_ratio(
    total_current_assets: Option<f64>,
    inventory: Option<f64>,
    total_current_liabilities: Option<f64>,
) -> Option<f64> {
    let l = total_current_liabilities?;
    if l <= 0.0 {
        return None;
    }
    let quick = total_current_assets? - inventory.unwrap_or(0.0);
    Some(quick / l)
}

/// Debt-to-assets ratio = total liabilities / total assets.
pub fn debt_to_assets(
    total_liabilities: Option<f64>,
    total_assets: Option<f64>,
) -> Option<f64> {
    let a = total_assets?;
    if a <= 0.0 {
        return None;
    }
    div(total_liabilities, Some(a))
}

/// Cash-conversion quality = CFO / 净利润(含少数). Values ≫1 or <0 over
/// several periods are classic earnings-quality signals (see `anomaly`).
pub fn cfo_to_net_income(cfo: Option<f64>, net_profit: Option<f64>) -> Option<f64> {
    let np = net_profit?;
    if np == 0.0 {
        return None;
    }
    div(cfo, Some(np))
}

/// Extract a `(period_end, value)` series from cumulative statements,
/// applying a field selector. Missing values are skipped.
pub fn series<T>(
    statements: &[T],
    meta: impl Fn(&T) -> Option<PeriodMeta>,
    field: impl Fn(&T) -> Option<f64>,
) -> Vec<PeriodValue> {
    statements
        .iter()
        .filter_map(|s| {
            Some(PeriodValue {
                period_end: meta(s)?.period_end,
                value: field(s)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn growth_sign_conventions() {
        assert_eq!(growth(Some(110.0), Some(100.0)), Some(0.1));
        // Loss narrowing: −50 → −25 reads as +50% improvement.
        assert_eq!(growth(Some(-25.0), Some(-50.0)), Some(0.5));
        assert_eq!(growth(Some(1.0), Some(0.0)), None);
        assert_eq!(growth(None, Some(1.0)), None);
    }

    #[test]
    fn yoy_matches_same_period_last_year() {
        let series = vec![
            PeriodValue { period_end: d(2024, 12, 31), value: 100.0 },
            PeriodValue { period_end: d(2025, 6, 30), value: 50.0 },
            PeriodValue { period_end: d(2025, 12, 31), value: 120.0 },
        ];
        let yoy = yoy_growth(&series);
        assert_eq!(yoy, vec![(d(2025, 12, 31), 0.2)]);
    }

    #[test]
    fn qoq_uses_consecutive_periods() {
        let series = vec![
            PeriodValue { period_end: d(2025, 3, 31), value: 100.0 },
            PeriodValue { period_end: d(2025, 6, 30), value: 110.0 },
            PeriodValue { period_end: d(2025, 9, 30), value: 99.0 },
        ];
        let qoq = qoq_growth(&series);
        assert_eq!(qoq.len(), 2);
        assert!((qoq[0].1 - 0.1).abs() < 1e-12);
        assert!((qoq[1].1 - (-0.1)).abs() < 1e-12);
    }

    #[test]
    fn single_quarter_differencing() {
        let meta = |m: u32, rt: ReportType| {
            Some(PeriodMeta {
                period_end: d(2025, m, if m == 3 { 31 } else if m == 6 { 30 } else { 31 }),
                report_type: rt,
                announced: None,
            })
        };
        let cumulative = vec![
            IncomeStatement { meta: meta(3, ReportType::Q1), net_profit: Some(10.0), ..Default::default() },
            IncomeStatement { meta: meta(6, ReportType::H1), net_profit: Some(25.0), ..Default::default() },
            IncomeStatement { meta: meta(12, ReportType::Annual), net_profit: Some(60.0), ..Default::default() },
        ];
        let sq = to_single_quarters(&cumulative);
        assert_eq!(sq[0].net_profit, Some(10.0)); // Q1 as-is
        assert_eq!(sq[1].net_profit, Some(15.0)); // H1 − Q1
        // Q4 = Annual − Q3; no Q3 row → documented fallback keeps YTD.
        assert_eq!(sq[2].net_profit, Some(35.0)); // 60 − 25 (prev = H1)
    }

    #[test]
    fn roe_golden_average_convention() {
        // NI = 100, equity 900 → 1100: avg 1000 → ROE = 0.10.
        assert_eq!(roe(Some(100.0), Some(900.0), Some(1100.0)), Some(0.1));
        // First period of history: only end balance known → uses it as-is.
        assert_eq!(roe(Some(100.0), None, Some(1000.0)), Some(0.1));
        assert_eq!(roe(Some(100.0), Some(-100.0), Some(-100.0)), None);
    }

    #[test]
    fn dupont_golden_and_consistent_with_roe() {
        // Revenue 1000, NI(parent) 100, assets 400→600 (avg 500),
        // equity 200→300 (avg 250).
        // NM = 0.1, AT = 2.0, EM = 2.0, ROE = 0.4.
        let stmt = IncomeStatement {
            total_operating_revenue: Some(1000.0),
            net_profit_parent: Some(100.0),
            ..Default::default()
        };
        let begin = BalanceSheet {
            total_assets: Some(400.0),
            total_parent_equity: Some(200.0),
            ..Default::default()
        };
        let end = BalanceSheet {
            total_assets: Some(600.0),
            total_parent_equity: Some(300.0),
            ..Default::default()
        };
        let dp = dupont(&stmt, &begin, &end).unwrap();
        assert!((dp.net_margin - 0.1).abs() < 1e-12);
        assert!((dp.asset_turnover - 2.0).abs() < 1e-12);
        assert!((dp.equity_multiplier - 2.0).abs() < 1e-12);
        assert!((dp.roe - 0.4).abs() < 1e-12);
        let direct = roe(Some(100.0), Some(200.0), Some(300.0)).unwrap();
        assert!((dp.roe - direct).abs() < 1e-12);
    }

    #[test]
    fn margins_and_ratios() {
        assert_eq!(gross_margin(Some(100.0), Some(60.0)), Some(0.4));
        assert_eq!(gross_margin(Some(0.0), Some(0.0)), None);
        assert_eq!(operating_margin(Some(30.0), Some(100.0)), Some(0.3));
        assert_eq!(net_margin(Some(20.0), Some(100.0)), Some(0.2));
        assert_eq!(current_ratio(Some(200.0), Some(100.0)), Some(2.0));
        assert_eq!(quick_ratio(Some(200.0), Some(50.0), Some(100.0)), Some(1.5));
        assert_eq!(debt_to_assets(Some(40.0), Some(100.0)), Some(0.4));
        assert_eq!(fcf(Some(100.0), Some(30.0)), Some(70.0));
        assert_eq!(working_capital(Some(200.0), Some(120.0)), Some(80.0));
        assert_eq!(cfo_to_net_income(Some(120.0), Some(100.0)), Some(1.2));
    }

    #[test]
    fn cash_conversion_cycle_golden() {
        // avg receivables 50, revenue 365 → DSO 50; avg inv 36.5, cost 365 →
        // DIO 36.5; avg payables 73, cost 365 → DPO 73. CCC = 13.5 days.
        let dso_v = dso(Some(50.0), Some(50.0), Some(365.0), 365.0).unwrap();
        let dio_v = dio(Some(36.5), Some(36.5), Some(365.0), 365.0).unwrap();
        let dpo_v = dpo(Some(73.0), Some(73.0), Some(365.0), 365.0).unwrap();
        assert!((dso_v - 50.0).abs() < 1e-9);
        let ccc = cash_conversion_cycle(Some(dso_v), Some(dio_v), Some(dpo_v)).unwrap();
        assert!((ccc - 13.5).abs() < 1e-9);
    }

    #[test]
    fn nopat_and_roic_golden() {
        // total_profit 125, finance_expense 25 → EBIT 150; tax 25/125 = 20%
        // → NOPAT 120. IC 1000 → 1400 (avg 1200) → ROIC 0.10.
        let stmt = IncomeStatement {
            total_profit: Some(125.0),
            finance_expense: Some(25.0),
            income_tax: Some(25.0),
            ..Default::default()
        };
        let np = nopat(&stmt).unwrap();
        assert!((np - 120.0).abs() < 1e-9);
        assert!((roic(Some(np), Some(1000.0), Some(1400.0)).unwrap() - 0.1).abs() < 1e-12);
    }

    #[test]
    fn invested_capital_definition() {
        let bs = BalanceSheet {
            total_equity: Some(1000.0),
            long_term_debt: Some(200.0),
            monetary_funds: Some(300.0),
            ..Default::default()
        };
        // 1000 + 200 − 300 = 900.
        assert_eq!(invested_capital(&bs), Some(900.0));
    }
}
