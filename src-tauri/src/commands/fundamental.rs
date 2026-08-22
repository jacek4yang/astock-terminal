//! Fundamental-analysis commands (基本面分析): statement-derived metrics,
//! growth series, quality scores, red flags, dividends and valuation.
//!
//! Both commands fetch the full [`FundamentalBundle`] and project it onto
//! JSON-friendly wrapper structs (the engine types deliberately carry more
//! fields than the UI contract, and several use non-JSON shapes such as
//! `&'static str` criteria names). Projection is pure — see the tests at the
//! bottom of the file.
//!
//! Conventions: growth rates and ratios are decimals (0.15 = 15%) except
//! percentiles (0–100) and the score values themselves. Per-section
//! degradation: a section whose inputs are missing serializes as `null` (or
//! an empty array) and is listed in `missing`; the bundle as a whole never
//! fails because one section is unavailable.

use std::collections::HashMap;

use astock_core::Symbol;
use astock_fundamental::model::{
    BalanceSheet, CashFlowStatement, CompanyProfile, FundamentalBundle, IncomeStatement,
    PeriodMeta, ReportType, ValuationPoint,
};
use astock_fundamental::{anomaly, metrics, scores, valuation};
use chrono::{Datelike, NaiveDate};
use serde::Serialize;
use serde_json::{Map, Value};
use tauri::State;

use crate::error::CmdError;
use crate::state::AppState;

use super::market::parse_symbol;

// ---------------------------------------------------------------------------
// Default DCF assumptions (documented in the response's `assumptions` block).
// ---------------------------------------------------------------------------

/// Explicit forecast horizon (years) for the two-stage FCFF DCF.
const DCF_STAGE1_YEARS: u32 = 5;
/// Discount rate.
const DCF_WACC: f64 = 0.09;
/// Perpetuity growth after stage 1 (must stay below the WACC).
const DCF_TERMINAL_GROWTH: f64 = 0.025;
/// Bull/bear shift applied to stage-1 growth and WACC (opposite directions).
const DCF_SCENARIO_SPREAD: f64 = 0.02;
/// Stage-1 growth = mean of the last [`DCF_GROWTH_WINDOW`] annual revenue
/// YoY values, clamped into [`DCF_GROWTH_FLOOR`]..=[`DCF_GROWTH_CAP`]
/// (a shrinking company gets 0%, not a fabricated decline spiral).
const DCF_GROWTH_WINDOW: usize = 5;
/// Lower clamp for the default stage-1 growth.
const DCF_GROWTH_FLOOR: f64 = 0.0;
/// Upper clamp for the default stage-1 growth.
const DCF_GROWTH_CAP: f64 = 0.25;
/// Sensitivity grid axes (rows = WACC, columns = terminal growth).
const DCF_SENSITIVITY_WACCS: [f64; 5] = [0.07, 0.08, 0.09, 0.10, 0.11];
/// Sensitivity terminal-growth axis.
const DCF_SENSITIVITY_GROWTHS: [f64; 5] = [0.015, 0.02, 0.025, 0.03, 0.035];

/// Periods kept in `growth_series`.
const GROWTH_SERIES_PERIODS: usize = 12;
/// Dividend records returned (newest first).
const DIVIDEND_COUNT: usize = 10;
/// Trading days kept in the valuation-band `history_series` (~3 years).
const HISTORY_SERIES_DAYS: usize = 750;

/// Percentile method label ( surfaced in `percentile.method`).
const PERCENTILE_METHOD: &str =
    "历史分位 = 日频估值序列(RPT_VALUEANALYSIS_DET)中 ≤ 当前值的占比 × 100";
/// One-line Chinese explanations attached to each score block.
const PIOTROSKI_NOTE: &str =
    "Piotroski F 记分(0–9)：盈利能力、杠杆与流动性、运营效率共 9 项达标计数；数据缺失项不计入。";
const ALTMAN_NOTE: &str =
    "Altman Z''(新兴市场版)更适合 A 股非金融企业：>2.60 安全区，1.10–2.60 灰色区，<1.10 困境区；经典 Z 供对照。";
const DCF_CAVEAT: &str =
    "DCF 对折现率与永续增长率高度敏感，且基于自由现金流代理值，区间仅供参考，不构成投资建议。";

// ---------------------------------------------------------------------------
// JSON shapes — get_fundamentals
// ---------------------------------------------------------------------------

/// `get_fundamentals` response.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FundamentalsJson {
    /// 6-digit symbol code.
    pub symbol: String,
    /// Company profile (null when the survey endpoint failed).
    pub profile: Option<ProfileJson>,
    /// Latest reporting period (null when no statement is available).
    pub latest_period: Option<LatestPeriodJson>,
    /// Headline metrics of the latest period (null without statements).
    pub metrics: Option<MetricsJson>,
    /// Last ~12 reporting periods, oldest first, for charting.
    pub growth_series: Vec<GrowthPointJson>,
    /// Quality/distress scores (null when none is computable).
    pub scores: Option<ScoresJson>,
    /// Red flags over the company's own annual history.
    pub anomalies: Vec<AnomalyJson>,
    /// Last 10 dividend/bonus implementations, newest first.
    pub dividends: Vec<DividendJson>,
    /// Sections that are missing or degraded (fetch failures by section
    /// name plus logical gaps such as `"scores"`).
    pub missing: Vec<String>,
}

/// Company profile block.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfileJson {
    /// Display name (A股简称, falling back to the full name).
    pub name: String,
    /// 所属行业 (null when the upstream omits it).
    pub industry: Option<String>,
    /// 上市日期, `YYYY-MM-DD`.
    pub listing_date: Option<String>,
    /// 总股本 (shares).
    pub total_shares: Option<f64>,
    /// 流通A股 (shares).
    pub float_shares: Option<f64>,
}

impl From<&CompanyProfile> for ProfileJson {
    fn from(p: &CompanyProfile) -> Self {
        ProfileJson {
            name: if p.short_name.is_empty() {
                p.name.clone()
            } else {
                p.short_name.clone()
            },
            industry: p.industry.clone(),
            listing_date: p.listing_date.map(|d| d.to_string()),
            total_shares: p.total_shares,
            float_shares: p.float_shares,
        }
    }
}

/// Latest reporting period block.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LatestPeriodJson {
    /// Period end, `YYYY-MM-DD`.
    pub period_end: String,
    /// `q1` / `h1` / `q3` / `annual`.
    pub report_type: String,
    /// 公告日期 (when the market first saw these numbers).
    pub announced_date: Option<String>,
}

/// 3-factor DuPont decomposition (see `metrics::dupont`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DupontJson {
    /// 归母净利润 / 营业总收入.
    pub net_margin: f64,
    /// 营业总收入 / 平均总资产.
    pub asset_turnover: f64,
    /// 平均总资产 / 平均归母权益.
    pub equity_multiplier: f64,
}

/// Headline metrics of the latest reporting period. All rates/ratios are
/// decimals; `fcf` is CNY; `ccc` is days.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MetricsJson {
    /// 营业总收入 (YTD within the fiscal year).
    pub revenue: Option<f64>,
    /// 归母净利润 (falling back to 净利润).
    pub net_profit: Option<f64>,
    /// Revenue YoY vs the same period one year earlier.
    pub revenue_yoy: Option<f64>,
    /// Net profit YoY.
    pub profit_yoy: Option<f64>,
    /// Single-quarter revenue QoQ (cumulative rows differenced).
    pub revenue_qoq: Option<f64>,
    /// Single-quarter net profit QoQ.
    pub profit_qoq: Option<f64>,
    /// 销售毛利率.
    pub gross_margin: Option<f64>,
    /// 营业利润率.
    pub operating_margin: Option<f64>,
    /// 销售净利率.
    pub net_margin: Option<f64>,
    /// ROE (average-balance convention, YTD profit — not annualized).
    pub roe: Option<f64>,
    /// ROA (same convention).
    pub roa: Option<f64>,
    /// ROIC (NOPAT / average invested capital).
    pub roic: Option<f64>,
    /// DuPont decomposition (null when any leg is missing).
    pub dupont: Option<DupontJson>,
    /// Free cash flow = CFO − capex.
    pub fcf: Option<f64>,
    /// CFO / 净利润 — earnings cash-backing.
    pub cfo_to_net_income: Option<f64>,
    /// Cash conversion cycle, days (DSO + DIO − DPO over the YTD window).
    pub ccc: Option<f64>,
    /// 流动比率.
    pub current_ratio: Option<f64>,
    /// 资产负债率 (liabilities / assets).
    pub debt_ratio: Option<f64>,
}

/// One point of `growth_series`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GrowthPointJson {
    /// Period end, `YYYY-MM-DD`.
    pub period_end: String,
    /// 营业总收入 (cumulative YTD).
    pub revenue: Option<f64>,
    /// 归母净利润 (cumulative YTD).
    pub net_profit: Option<f64>,
    /// Revenue YoY.
    pub revenue_yoy: Option<f64>,
    /// 归母净利润 YoY.
    pub profit_yoy: Option<f64>,
    /// 销售毛利率.
    pub gross_margin: Option<f64>,
    /// 加权 ROE from the EM key-indicators feed (vendor convention).
    pub roe: Option<f64>,
}

/// One Piotroski criterion: `passed` is null when an input was Missing.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CriterionJson {
    /// Stable machine name, e.g. `positive_roa`.
    pub name: String,
    /// true/false when computable, null when Missing.
    pub passed: Option<bool>,
}

/// Piotroski F-score block.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PiotroskiJson {
    /// Passed criteria (Missing criteria do not count).
    pub score: u32,
    /// Criteria that were computable (≤ 9).
    pub available: u32,
    /// All 9 criteria in canonical order.
    pub criteria: Vec<CriterionJson>,
    /// 说明.
    pub note: String,
}

/// Altman Z-score block.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AltmanJson {
    /// Classic 1968 Z (null when any of X1..X5 is Missing).
    pub z_classic: Option<f64>,
    /// Emerging-market Z'' (null when any of X1..X4 is Missing).
    pub z_emerging: Option<f64>,
    /// Zone of the preferred variant (Z'' first): safe / grey / distress.
    pub zone: Option<String>,
    /// 说明.
    pub note: String,
}

/// Beneish M-score block.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BeneishJson {
    /// M-score (null unless all 8 indices are available).
    pub m_score: Option<f64>,
    /// 中文解读 (cut-off −1.78 from the original paper).
    pub interpretation: String,
}

/// Quality/distress score bundle; each sub-score degrades independently.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScoresJson {
    /// Piotroski F-score (needs two annual reports).
    pub piotroski: Option<PiotroskiJson>,
    /// Altman Z (needs the latest annual report + balance sheet).
    pub altman: Option<AltmanJson>,
    /// Beneish M-score (needs two annual reports).
    pub beneish: Option<BeneishJson>,
}

/// One red flag.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AnomalyJson {
    /// Category, snake_case (e.g. `revenue_up_cfo_down`).
    pub kind: String,
    /// `info` / `warn` / `high`.
    pub severity: String,
    /// Plain-language explanation.
    pub explanation: String,
    /// Evidence numbers as a `{label: value}` object.
    pub evidence: Value,
}

/// One dividend/bonus record.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DividendJson {
    /// Fiscal year of the distribution (null when upstream omits it).
    pub year: Option<i32>,
    /// 实施方案, e.g. `10派280.24元(含税)`.
    pub plan: Option<String>,
    /// 除权除息日, `YYYY-MM-DD`.
    pub ex_date: Option<String>,
}

// ---------------------------------------------------------------------------
// JSON shapes — get_valuation
// ---------------------------------------------------------------------------

/// `get_valuation` response.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValuationJson {
    /// 6-digit symbol code.
    pub symbol: String,
    /// Current multiples (null when the quote snapshot failed).
    pub current: Option<CurrentJson>,
    /// Historical percentiles (null without valuation history).
    pub percentile: Option<PercentileJson>,
    /// Two-stage FCFF DCF scenario set (null when inputs are missing or
    /// incoherent, e.g. negative FCF wiping out enterprise value).
    pub dcf: Option<DcfJson>,
    /// Last ~750 trading days for the valuation-band chart, oldest first.
    pub history_series: Vec<HistoryPointJson>,
    /// Missing/degraded sections.
    pub missing: Vec<String>,
}

/// Current multiples.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CurrentJson {
    /// 最新价.
    pub price: f64,
    /// PE(TTM).
    pub pe_ttm: Option<f64>,
    /// PE(静态, last annual report).
    pub pe_static: Option<f64>,
    /// PB(MRQ).
    pub pb: Option<f64>,
    /// PS(TTM) — from the latest valuation-history row.
    pub ps_ttm: Option<f64>,
    /// PCF(经营现金流TTM) — from the latest valuation-history row.
    pub pcf: Option<f64>,
    /// 总市值 (CNY).
    pub market_cap: Option<f64>,
}

/// Percentile block: share of history ≤ current × 100.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PercentileJson {
    /// PE(TTM) percentile, 0–100.
    pub pe_ttm_pct: Option<f64>,
    /// PB percentile, 0–100.
    pub pb_pct: Option<f64>,
    /// PS(TTM) percentile, 0–100.
    pub ps_pct: Option<f64>,
    /// Number of trading days in the percentile window.
    pub days: u32,
    /// Method label (Chinese).
    pub method: String,
}

/// One DCF scenario outcome.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DcfScenarioJson {
    /// Equity value per share (CNY).
    pub per_share: f64,
    /// Enterprise value (CNY).
    pub enterprise_value: f64,
    /// Equity value (CNY).
    pub equity_value: f64,
    /// PV(terminal) / EV — above ~0.8 the result is mostly terminal
    /// assumption and should be distrusted.
    pub terminal_share: f64,
}

impl From<&valuation::DcfResult> for DcfScenarioJson {
    fn from(r: &valuation::DcfResult) -> Self {
        DcfScenarioJson {
            per_share: r.per_share,
            enterprise_value: r.enterprise_value,
            equity_value: r.equity_value,
            terminal_share: r.terminal_share,
        }
    }
}

/// The default DCF assumptions actually used (decimals).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DcfAssumptionsJson {
    /// Base-year FCF (latest annual CFO − capex).
    pub base_fcf: f64,
    /// Stage-1 horizon, years.
    pub stage1_years: u32,
    /// Stage-1 FCF growth (5y avg annual revenue growth, clamped 0–25%).
    pub stage1_growth: f64,
    /// Discount rate.
    pub wacc: f64,
    /// Perpetuity growth.
    pub terminal_growth: f64,
    /// Net debt subtracted from EV (0 when no balance sheet is available).
    pub net_debt: f64,
    /// Shares outstanding.
    pub shares: f64,
}

/// WACC × terminal-growth sensitivity grid of per-share values.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SensitivityJson {
    /// Row axis.
    pub wacc: Vec<f64>,
    /// Column axis.
    pub growth: Vec<f64>,
    /// `values[i][j]` = per-share value at `wacc[i]` × `growth[j]`; null for
    /// incoherent cells (wacc ≤ g).
    pub values: Vec<Vec<Option<f64>>>,
}

/// DCF block: always a range (bear..bull), never a single target price.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DcfJson {
    /// Assumptions used.
    pub assumptions: DcfAssumptionsJson,
    /// Pessimistic scenario (growth − spread, wacc + spread).
    pub bear: DcfScenarioJson,
    /// Central scenario.
    pub base: DcfScenarioJson,
    /// Optimistic scenario (growth + spread, wacc − spread).
    pub bull: DcfScenarioJson,
    /// WACC × terminal-growth sensitivity table.
    pub sensitivity: SensitivityJson,
    /// 中文提示.
    pub caveat: String,
}

/// One day of the valuation-band series.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryPointJson {
    /// Trading date, `YYYY-MM-DD`.
    pub date: String,
    /// PE(TTM).
    pub pe_ttm: Option<f64>,
    /// PB(MRQ).
    pub pb: Option<f64>,
}

// ---------------------------------------------------------------------------
// Pure projection helpers
// ---------------------------------------------------------------------------

/// `q1` / `h1` / `q3` / `annual`.
fn report_type_str(rt: ReportType) -> &'static str {
    match rt {
        ReportType::Q1 => "q1",
        ReportType::H1 => "h1",
        ReportType::Q3 => "q3",
        ReportType::Annual => "annual",
    }
}

/// Annual-report rows of one statement vec, oldest first.
fn annual_rows<T>(rows: &[T], meta: impl Fn(&T) -> Option<PeriodMeta>) -> Vec<&T> {
    rows.iter()
        .filter(|r| meta(r).is_some_and(|m| m.report_type == ReportType::Annual))
        .collect()
}

/// YoY map (`period_end → growth`) over the full statement history, so the
/// year-ago period is found even when it falls outside the display window.
fn yoy_map(
    rows: &[IncomeStatement],
    field: impl Fn(&IncomeStatement) -> Option<f64>,
) -> HashMap<NaiveDate, f64> {
    let series = metrics::series(rows, |s| s.meta, field);
    metrics::yoy_growth(&series).into_iter().collect()
}

/// Append `name` to a missing list, preserving order without duplicates.
fn push_missing(missing: &mut Vec<String>, name: &str) {
    if !missing.iter().any(|m| m == name) {
        missing.push(name.to_string());
    }
}

/// Section prefixes from `BundleOutcome::failures` (`"income: ..."` → `"income"`).
fn failure_sections(failures: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for f in failures {
        if let Some(prefix) = f.split(':').next() {
            push_missing(&mut out, prefix);
        }
    }
    out
}

/// Latest period metadata + headline metrics off the latest income
/// statement (any report type; averages use the two most recent balance
/// sheets — documented approximation for the opening balance).
fn latest_period_and_metrics(
    bundle: &FundamentalBundle,
) -> (Option<LatestPeriodJson>, Option<MetricsJson>) {
    let Some((inc, meta)) = bundle
        .income
        .iter()
        .rev()
        .find_map(|s| s.meta.map(|m| (s, m)))
    else {
        return (None, None);
    };
    let empty_bs = BalanceSheet::default();
    let empty_cf = CashFlowStatement::default();
    let bs_end = bundle.balance.last().unwrap_or(&empty_bs);
    let bs_begin = match bundle.balance.len().checked_sub(2) {
        Some(i) => &bundle.balance[i],
        None => &empty_bs,
    };
    let cf = bundle.cashflow.last().unwrap_or(&empty_cf);
    // Statements are cumulative YTD, so the window length is the day-of-year
    // of the period end (365/366 for annual reports).
    let days = meta.period_end.ordinal() as f64;

    let rev_yoy = yoy_map(&bundle.income, |s| s.total_operating_revenue);
    let prof_yoy = yoy_map(&bundle.income, |s| s.net_profit_parent.or(s.net_profit));
    let sq = metrics::to_single_quarters(&bundle.income);
    let rev_qoq: HashMap<NaiveDate, f64> = metrics::qoq_growth(&metrics::series(
        &sq,
        |s| s.meta,
        |s| s.total_operating_revenue,
    ))
    .into_iter()
    .collect();
    let prof_qoq: HashMap<NaiveDate, f64> = metrics::qoq_growth(&metrics::series(
        &sq,
        |s| s.meta,
        |s| s.net_profit_parent.or(s.net_profit),
    ))
    .into_iter()
    .collect();

    let recv = |bs: &BalanceSheet| bs.notes_and_accounts_receivable.or(bs.accounts_receivable);
    let pay = |bs: &BalanceSheet| bs.notes_and_accounts_payable.or(bs.accounts_payable);
    let dso = metrics::dso(
        recv(bs_begin),
        recv(bs_end),
        inc.total_operating_revenue,
        days,
    );
    let dio = metrics::dio(
        bs_begin.inventory,
        bs_end.inventory,
        inc.operating_cost,
        days,
    );
    let dpo = metrics::dpo(pay(bs_begin), pay(bs_end), inc.operating_cost, days);

    let latest = LatestPeriodJson {
        period_end: meta.period_end.to_string(),
        report_type: report_type_str(meta.report_type).to_string(),
        announced_date: meta.announced.map(|d| d.to_string()),
    };
    let dupont = metrics::dupont(inc, bs_begin, bs_end).map(|d| DupontJson {
        net_margin: d.net_margin,
        asset_turnover: d.asset_turnover,
        equity_multiplier: d.equity_multiplier,
    });
    let metrics_json = MetricsJson {
        revenue: inc.total_operating_revenue,
        net_profit: inc.net_profit_parent.or(inc.net_profit),
        revenue_yoy: rev_yoy.get(&meta.period_end).copied(),
        profit_yoy: prof_yoy.get(&meta.period_end).copied(),
        revenue_qoq: rev_qoq.get(&meta.period_end).copied(),
        profit_qoq: prof_qoq.get(&meta.period_end).copied(),
        gross_margin: metrics::gross_margin(inc.operating_revenue, inc.operating_cost),
        operating_margin: metrics::operating_margin(
            inc.operating_profit,
            inc.total_operating_revenue,
        ),
        net_margin: metrics::net_margin(inc.net_profit, inc.total_operating_revenue),
        roe: metrics::roe(
            inc.net_profit_parent,
            bs_begin.total_parent_equity,
            bs_end.total_parent_equity,
        ),
        roa: metrics::roa(inc.net_profit, bs_begin.total_assets, bs_end.total_assets),
        roic: metrics::roic(
            metrics::nopat(inc),
            metrics::invested_capital(bs_begin),
            metrics::invested_capital(bs_end),
        ),
        dupont,
        fcf: metrics::fcf(cf.net_cfo, cf.capex),
        cfo_to_net_income: metrics::cfo_to_net_income(cf.net_cfo, inc.net_profit),
        ccc: metrics::cash_conversion_cycle(dso, dio, dpo),
        current_ratio: metrics::current_ratio(
            bs_end.total_current_assets,
            bs_end.total_current_liabilities,
        ),
        debt_ratio: metrics::debt_to_assets(bs_end.total_liabilities, bs_end.total_assets),
    };
    (Some(latest), Some(metrics_json))
}

/// Last ~12 periods of revenue/profit with YoY, margin and vendor ROE.
fn growth_series_json(bundle: &FundamentalBundle) -> Vec<GrowthPointJson> {
    let rows: Vec<(&IncomeStatement, PeriodMeta)> = bundle
        .income
        .iter()
        .filter_map(|s| s.meta.map(|m| (s, m)))
        .collect();
    let rev_yoy = yoy_map(&bundle.income, |s| s.total_operating_revenue);
    let prof_yoy = yoy_map(&bundle.income, |s| s.net_profit_parent.or(s.net_profit));
    let roe_by_period: HashMap<NaiveDate, f64> = bundle
        .indicators
        .iter()
        .filter_map(|i| Some((i.meta?.period_end, i.roe_weighted? / 100.0)))
        .collect();
    let start = rows.len().saturating_sub(GROWTH_SERIES_PERIODS);
    rows[start..]
        .iter()
        .map(|(s, meta)| GrowthPointJson {
            period_end: meta.period_end.to_string(),
            revenue: s.total_operating_revenue,
            net_profit: s.net_profit_parent.or(s.net_profit),
            revenue_yoy: rev_yoy.get(&meta.period_end).copied(),
            profit_yoy: prof_yoy.get(&meta.period_end).copied(),
            gross_margin: metrics::gross_margin(s.operating_revenue, s.operating_cost),
            roe: roe_by_period.get(&meta.period_end).copied(),
        })
        .collect()
}

/// Piotroski F-score over the last two annual reports (null with fewer).
fn piotroski_json(
    inc_a: &[&IncomeStatement],
    cf_a: &[&CashFlowStatement],
    bs_a: &[&BalanceSheet],
) -> Option<PiotroskiJson> {
    let inc_curr = *inc_a.last()?;
    let inc_prev = *inc_a.get(inc_a.len().checked_sub(2)?)?;
    let empty_bs = BalanceSheet::default();
    let empty_cf = CashFlowStatement::default();
    let bs_curr = bs_a.last().copied().unwrap_or(&empty_bs);
    let bs_prev = bs_a
        .len()
        .checked_sub(2)
        .map(|i| bs_a[i])
        .unwrap_or(&empty_bs);
    let bs_open_prev = bs_a
        .len()
        .checked_sub(3)
        .map(|i| bs_a[i])
        .unwrap_or(&empty_bs);
    let cf_curr = cf_a.last().copied().unwrap_or(&empty_cf);
    let input =
        scores::piotroski_input_from(inc_curr, inc_prev, cf_curr, bs_open_prev, bs_prev, bs_curr);
    let f = scores::piotroski(&input);
    Some(PiotroskiJson {
        score: f.score,
        available: f.available,
        criteria: f
            .criteria
            .iter()
            .map(|c| CriterionJson {
                name: c.name.to_string(),
                passed: c.passed,
            })
            .collect(),
        note: PIOTROSKI_NOTE.to_string(),
    })
}

/// Altman Z (both variants) off the latest annual report. Market cap for the
/// classic X4 comes from the quote snapshot; when it is missing the classic
/// Z degrades to null while Z'' still computes.
fn altman_json(bundle: &FundamentalBundle) -> Option<AltmanJson> {
    let inc_a = annual_rows(&bundle.income, |s| s.meta);
    let bs_a = annual_rows(&bundle.balance, |s| s.meta);
    let inc = *inc_a.last()?;
    let bs = *bs_a.last()?;
    let z = scores::altman(&scores::AltmanInput {
        working_capital: metrics::working_capital(
            bs.total_current_assets,
            bs.total_current_liabilities,
        ),
        retained_earnings: bs.retained_earnings,
        ebit: scores::altman_ebit(inc),
        market_cap: bundle.snapshot.as_ref().and_then(|s| s.total_market_cap),
        book_equity: bs.total_equity,
        total_liabilities: bs.total_liabilities,
        total_assets: bs.total_assets,
        revenue: inc.total_operating_revenue,
    });
    let zone = z
        .emerging_zone
        .or(z.classic_zone)
        .map(|zone| match zone {
            scores::AltmanZone::Safe => "safe",
            scores::AltmanZone::Grey => "grey",
            scores::AltmanZone::Distress => "distress",
        })
        .map(str::to_string);
    Some(AltmanJson {
        z_classic: z.classic,
        z_emerging: z.z_emerging,
        zone,
        note: ALTMAN_NOTE.to_string(),
    })
}

/// Beneish M-score over the last two annual reports.
fn beneish_json(
    inc_a: &[&IncomeStatement],
    cf_a: &[&CashFlowStatement],
    bs_a: &[&BalanceSheet],
) -> Option<BeneishJson> {
    let inc_curr = *inc_a.last()?;
    let inc_prev = *inc_a.get(inc_a.len().checked_sub(2)?)?;
    let empty_bs = BalanceSheet::default();
    let empty_cf = CashFlowStatement::default();
    let bs_curr = bs_a.last().copied().unwrap_or(&empty_bs);
    let bs_prev = bs_a
        .len()
        .checked_sub(2)
        .map(|i| bs_a[i])
        .unwrap_or(&empty_bs);
    let cf_curr = cf_a.last().copied().unwrap_or(&empty_cf);
    let cf_prev = cf_a
        .len()
        .checked_sub(2)
        .map(|i| cf_a[i])
        .unwrap_or(&empty_cf);
    let m = scores::beneish(&scores::beneish_indices_from(
        inc_curr, inc_prev, cf_curr, cf_prev, bs_curr, bs_prev,
    ));
    let interpretation = match m.total {
        Some(total) if total > scores::BENEISH_CUTOFF => format!(
            "M={total:.2}，高于 {:.2} 阈值，存在利润操纵嫌疑，需结合红旗信号复核。",
            scores::BENEISH_CUTOFF
        ),
        Some(total) => format!(
            "M={total:.2}，低于 {:.2} 阈值，未见明显利润操纵迹象。",
            scores::BENEISH_CUTOFF
        ),
        None => "关键科目缺失，无法计算 M 值。".to_string(),
    };
    Some(BeneishJson {
        m_score: m.total,
        interpretation,
    })
}

/// Score bundle; null when no sub-score is computable.
fn scores_json(bundle: &FundamentalBundle) -> Option<ScoresJson> {
    let inc_a = annual_rows(&bundle.income, |s| s.meta);
    let cf_a = annual_rows(&bundle.cashflow, |s| s.meta);
    let bs_a = annual_rows(&bundle.balance, |s| s.meta);
    let scores = ScoresJson {
        piotroski: piotroski_json(&inc_a, &cf_a, &bs_a),
        altman: altman_json(bundle),
        beneish: beneish_json(&inc_a, &cf_a, &bs_a),
    };
    if scores.piotroski.is_none() && scores.altman.is_none() && scores.beneish.is_none() {
        None
    } else {
        Some(scores)
    }
}

/// Snake-case flag category.
fn flag_kind_str(kind: anomaly::FlagKind) -> &'static str {
    match kind {
        anomaly::FlagKind::RevenueUpCfoDown => "revenue_up_cfo_down",
        anomaly::FlagKind::ReceivablesOutpaceRevenue => "receivables_outpace_revenue",
        anomaly::FlagKind::InventorySpike => "inventory_spike",
        anomaly::FlagKind::GoodwillHeavy => "goodwill_heavy",
        anomaly::FlagKind::MarginOutlier => "margin_outlier",
        anomaly::FlagKind::CashAndDebtBothHigh => "cash_and_debt_both_high",
    }
}

/// Red flags over the annual history (aligned across the three statements
/// by period end; missing statements degrade that period's inputs to None).
fn anomalies_json(bundle: &FundamentalBundle) -> Vec<AnomalyJson> {
    let inc_a = annual_rows(&bundle.income, |s| s.meta);
    let bs_by_period: HashMap<NaiveDate, &BalanceSheet> = annual_rows(&bundle.balance, |s| s.meta)
        .into_iter()
        .filter_map(|bs| Some((bs.meta?.period_end, bs)))
        .collect();
    let cf_by_period: HashMap<NaiveDate, &CashFlowStatement> =
        annual_rows(&bundle.cashflow, |s| s.meta)
            .into_iter()
            .filter_map(|cf| Some((cf.meta?.period_end, cf)))
            .collect();
    let empty_bs = BalanceSheet::default();
    let empty_cf = CashFlowStatement::default();
    let history: Vec<anomaly::PeriodObservation> = inc_a
        .iter()
        .map(|inc| {
            let period_end = inc.meta.map(|m| m.period_end);
            let bs = period_end
                .and_then(|pe| bs_by_period.get(&pe).copied())
                .unwrap_or(&empty_bs);
            let cf = period_end
                .and_then(|pe| cf_by_period.get(&pe).copied())
                .unwrap_or(&empty_cf);
            anomaly::PeriodObservation {
                revenue: inc.total_operating_revenue,
                cfo: cf.net_cfo,
                receivables: bs.notes_and_accounts_receivable.or(bs.accounts_receivable),
                inventory: bs.inventory,
                operating_cost: inc.operating_cost,
                goodwill: bs.goodwill,
                equity: bs.total_parent_equity,
                monetary_funds: bs.monetary_funds,
                interest_bearing_debt: bs.interest_bearing_debt(),
                total_assets: bs.total_assets,
                gross_margin: metrics::gross_margin(inc.operating_revenue, inc.operating_cost),
                net_margin: metrics::net_margin(inc.net_profit, inc.total_operating_revenue),
            }
        })
        .collect();
    anomaly::detect(&history)
        .iter()
        .map(|f| {
            let evidence: Map<String, Value> = f
                .evidence
                .iter()
                .map(|(k, v)| (k.clone(), Value::from(*v)))
                .collect();
            AnomalyJson {
                kind: flag_kind_str(f.kind).to_string(),
                severity: match f.severity {
                    anomaly::Severity::Info => "info",
                    anomaly::Severity::Warn => "warn",
                    anomaly::Severity::High => "high",
                }
                .to_string(),
                explanation: f.explanation.clone(),
                evidence: Value::Object(evidence),
            }
        })
        .collect()
}

/// Last 10 dividend implementations, newest first.
fn dividends_json(bundle: &FundamentalBundle) -> Vec<DividendJson> {
    bundle
        .dividends
        .iter()
        .rev()
        .take(DIVIDEND_COUNT)
        .map(|d| DividendJson {
            year: d.report_date.map(|d| d.year()),
            plan: d.plan.clone(),
            ex_date: d.ex_dividend_date.map(|d| d.to_string()),
        })
        .collect()
}

/// Pure projection: bundle → `get_fundamentals` response.
fn fundamentals_json(
    symbol: &Symbol,
    bundle: &FundamentalBundle,
    failures: &[String],
) -> FundamentalsJson {
    let profile = bundle.profile.as_ref().map(ProfileJson::from);
    let (latest_period, metrics_json) = latest_period_and_metrics(bundle);
    let growth_series = growth_series_json(bundle);
    let scores = scores_json(bundle);
    let anomalies = anomalies_json(bundle);
    let dividends = dividends_json(bundle);

    let mut missing = failure_sections(failures);
    if profile.is_none() {
        push_missing(&mut missing, "profile");
    }
    if metrics_json.is_none() {
        push_missing(&mut missing, "metrics");
    }
    if growth_series.is_empty() {
        push_missing(&mut missing, "growth_series");
    }
    if scores.is_none() {
        push_missing(&mut missing, "scores");
    }
    // An empty dividend list is legitimate (company never paid); only the
    // fetch failure (already in `missing` as "dividends") is a degradation.
    FundamentalsJson {
        symbol: symbol.code().to_string(),
        profile,
        latest_period,
        metrics: metrics_json,
        growth_series,
        scores,
        anomalies,
        dividends,
        missing,
    }
}

/// Current multiples: PE/PB from the quote snapshot, PS/PCF from the latest
/// valuation-history row (the quote fields do not carry them).
fn current_json(bundle: &FundamentalBundle) -> Option<CurrentJson> {
    let snap = bundle.snapshot.as_ref()?;
    let last_hist = bundle.valuation_history.last();
    Some(CurrentJson {
        price: snap.price,
        pe_ttm: snap.pe_ttm,
        pe_static: snap.pe_static,
        pb: snap.pb,
        ps_ttm: last_hist.and_then(|h| h.ps_ttm),
        pcf: last_hist.and_then(|h| h.pcf_ocf_ttm),
        market_cap: snap.total_market_cap,
    })
}

/// Historical percentiles over the full daily valuation series.
fn percentile_json(bundle: &FundamentalBundle) -> Option<PercentileJson> {
    if bundle.valuation_history.is_empty() {
        return None;
    }
    let hist = |field: fn(&ValuationPoint) -> Option<f64>| -> Vec<f64> {
        bundle.valuation_history.iter().filter_map(field).collect()
    };
    let pe_hist = hist(|p| p.pe_ttm);
    let pb_hist = hist(|p| p.pb_mrq);
    let ps_hist = hist(|p| p.ps_ttm);
    let last = bundle.valuation_history.last();
    let cur_pe = bundle
        .snapshot
        .as_ref()
        .and_then(|s| s.pe_ttm)
        .or_else(|| last.and_then(|h| h.pe_ttm));
    let cur_pb = bundle
        .snapshot
        .as_ref()
        .and_then(|s| s.pb)
        .or_else(|| last.and_then(|h| h.pb_mrq));
    let cur_ps = last.and_then(|h| h.ps_ttm);
    Some(PercentileJson {
        pe_ttm_pct: cur_pe.and_then(|c| valuation::percentile(&pe_hist, c)),
        pb_pct: cur_pb.and_then(|c| valuation::percentile(&pb_hist, c)),
        ps_pct: cur_ps.and_then(|c| valuation::percentile(&ps_hist, c)),
        days: bundle.valuation_history.len() as u32,
        method: PERCENTILE_METHOD.to_string(),
    })
}

/// Default stage-1 growth: mean of the last [`DCF_GROWTH_WINDOW`] annual
/// revenue YoY values, clamped into 0–25% (0% when no history exists).
fn capped_base_growth(bundle: &FundamentalBundle) -> f64 {
    let series: Vec<metrics::PeriodValue> = annual_rows(&bundle.income, |s| s.meta)
        .iter()
        .filter_map(|s| {
            Some(metrics::PeriodValue {
                period_end: s.meta?.period_end,
                value: s.total_operating_revenue?,
            })
        })
        .collect();
    let yoys = metrics::yoy_growth(&series);
    let start = yoys.len().saturating_sub(DCF_GROWTH_WINDOW);
    let window = &yoys[start..];
    let avg = if window.is_empty() {
        0.0
    } else {
        window.iter().map(|(_, g)| g).sum::<f64>() / window.len() as f64
    };
    avg.clamp(DCF_GROWTH_FLOOR, DCF_GROWTH_CAP)
}

/// Two-stage FCFF DCF scenario set under the default assumptions (constants
/// at the top of this file). Null when base FCF or share count is missing,
/// or when the scenarios are incoherent (e.g. negative FCF).
fn dcf_json(bundle: &FundamentalBundle) -> Option<DcfJson> {
    let cf_a = annual_rows(&bundle.cashflow, |s| s.meta);
    let cf = *cf_a.last()?;
    let base_fcf = metrics::fcf(cf.net_cfo, cf.capex)?;
    let shares = bundle
        .snapshot
        .as_ref()
        .and_then(|s| s.total_shares)
        .or_else(|| bundle.profile.as_ref().and_then(|p| p.total_shares))?;
    let net_debt = bundle
        .balance
        .last()
        .map(|bs| bs.interest_bearing_debt().unwrap_or(0.0) - bs.monetary_funds.unwrap_or(0.0))
        .unwrap_or(0.0);
    let inputs = valuation::DcfInputs {
        base_fcf,
        stage1_years: DCF_STAGE1_YEARS,
        stage1_growth: capped_base_growth(bundle),
        terminal_growth: DCF_TERMINAL_GROWTH,
        wacc: DCF_WACC,
        net_debt,
        shares,
    };
    let sc = valuation::scenarios(&inputs, DCF_SCENARIO_SPREAD)?;
    let grid = valuation::sensitivity(&inputs, &DCF_SENSITIVITY_WACCS, &DCF_SENSITIVITY_GROWTHS);
    Some(DcfJson {
        assumptions: DcfAssumptionsJson {
            base_fcf,
            stage1_years: inputs.stage1_years,
            stage1_growth: inputs.stage1_growth,
            wacc: inputs.wacc,
            terminal_growth: inputs.terminal_growth,
            net_debt,
            shares,
        },
        bear: DcfScenarioJson::from(&sc.bear),
        base: DcfScenarioJson::from(&sc.base),
        bull: DcfScenarioJson::from(&sc.bull),
        sensitivity: SensitivityJson {
            wacc: DCF_SENSITIVITY_WACCS.to_vec(),
            growth: DCF_SENSITIVITY_GROWTHS.to_vec(),
            values: grid,
        },
        caveat: DCF_CAVEAT.to_string(),
    })
}

/// Last ~750 trading days of PE/PB for the valuation-band chart.
fn history_series_json(bundle: &FundamentalBundle) -> Vec<HistoryPointJson> {
    let start = bundle
        .valuation_history
        .len()
        .saturating_sub(HISTORY_SERIES_DAYS);
    bundle.valuation_history[start..]
        .iter()
        .map(|p| HistoryPointJson {
            date: p.date.to_string(),
            pe_ttm: p.pe_ttm,
            pb: p.pb_mrq,
        })
        .collect()
}

/// Pure projection: bundle → `get_valuation` response.
fn valuation_json(
    symbol: &Symbol,
    bundle: &FundamentalBundle,
    failures: &[String],
) -> ValuationJson {
    let current = current_json(bundle);
    let percentile = percentile_json(bundle);
    let dcf = dcf_json(bundle);
    let history_series = history_series_json(bundle);

    let mut missing = failure_sections(failures);
    if current.is_none() {
        push_missing(&mut missing, "current");
    }
    if percentile.is_none() {
        push_missing(&mut missing, "percentile");
    }
    if dcf.is_none() {
        push_missing(&mut missing, "dcf");
    }
    if history_series.is_empty() {
        push_missing(&mut missing, "history_series");
    }
    ValuationJson {
        symbol: symbol.code().to_string(),
        current,
        percentile,
        dcf,
        history_series,
        missing,
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Full fundamental snapshot for one symbol: profile, latest-period metrics,
/// growth series, quality scores, red flags and dividends.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_fundamentals(
    state: State<'_, AppState>,
    symbol: String,
) -> Result<FundamentalsJson, CmdError> {
    let symbol = parse_symbol(&symbol)?;
    let outcome = state.fundamental.bundle(&symbol).await;
    if !outcome.failures.is_empty() {
        tracing::warn!(%symbol, failures = ?outcome.failures, "fundamental bundle partially degraded");
    }
    Ok(fundamentals_json(
        &symbol,
        &outcome.bundle,
        &outcome.failures,
    ))
}

/// Valuation for one symbol: current multiples, historical percentiles,
/// two-stage FCFF DCF scenario range and the valuation-band series.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_valuation(
    state: State<'_, AppState>,
    symbol: String,
) -> Result<ValuationJson, CmdError> {
    let symbol = parse_symbol(&symbol)?;
    let outcome = state.fundamental.bundle(&symbol).await;
    if !outcome.failures.is_empty() {
        tracing::warn!(%symbol, failures = ?outcome.failures, "fundamental bundle partially degraded");
    }
    Ok(valuation_json(&symbol, &outcome.bundle, &outcome.failures))
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_fundamental::model::{DividendRecord, KeyIndicators, ValuationSnapshot};

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn meta(y: i32, m: u32, day: u32, rt: ReportType) -> Option<PeriodMeta> {
        Some(PeriodMeta {
            period_end: d(y, m, day),
            report_type: rt,
            announced: Some(d(y, m, day) + chrono::Duration::days(45)),
        })
    }

    fn income(
        y: i32,
        m: u32,
        day: u32,
        rt: ReportType,
        rev: f64,
        cost: f64,
        np: f64,
    ) -> IncomeStatement {
        IncomeStatement {
            meta: meta(y, m, day, rt),
            total_operating_revenue: Some(rev),
            operating_revenue: Some(rev),
            operating_cost: Some(cost),
            operating_profit: Some(np * 1.1),
            total_profit: Some(np * 1.2),
            income_tax: Some(np * 0.2),
            net_profit: Some(np),
            net_profit_parent: Some(np * 0.9),
            finance_expense: Some(10.0),
            selling_expense: Some(30.0),
            admin_expense: Some(40.0),
            ..Default::default()
        }
    }

    fn balance(y: i32, m: u32, day: u32, rt: ReportType, scale: f64) -> BalanceSheet {
        BalanceSheet {
            meta: meta(y, m, day, rt),
            monetary_funds: Some(300.0 * scale),
            notes_and_accounts_receivable: Some(100.0 * scale),
            inventory: Some(150.0 * scale),
            notes_and_accounts_payable: Some(120.0 * scale),
            total_current_assets: Some(800.0 * scale),
            total_current_liabilities: Some(400.0 * scale),
            fixed_assets: Some(500.0 * scale),
            total_assets: Some(2000.0 * scale),
            long_term_debt: Some(100.0 * scale),
            total_liabilities: Some(800.0 * scale),
            share_capital: Some(100.0),
            retained_earnings: Some(500.0 * scale),
            total_parent_equity: Some(1100.0 * scale),
            total_equity: Some(1200.0 * scale),
            goodwill: Some(10.0),
            ..Default::default()
        }
    }

    fn cashflow(
        y: i32,
        m: u32,
        day: u32,
        rt: ReportType,
        cfo: f64,
        capex: f64,
    ) -> CashFlowStatement {
        CashFlowStatement {
            meta: meta(y, m, day, rt),
            net_cfo: Some(cfo),
            capex: Some(capex),
            depreciation: Some(30.0),
            ..Default::default()
        }
    }

    /// A bundle with two annual years plus one H1 quarter — enough for every
    /// section to compute.
    fn sample_bundle() -> FundamentalBundle {
        FundamentalBundle {
            profile: Some(CompanyProfile {
                code: "600519".into(),
                name: "贵州茅台酒股份有限公司".into(),
                short_name: "贵州茅台".into(),
                industry: Some("酿酒行业".into()),
                listing_date: Some(d(2001, 8, 27)),
                total_shares: Some(100.0),
                float_shares: Some(100.0),
                ..Default::default()
            }),
            income: vec![
                income(2023, 12, 31, ReportType::Annual, 1000.0, 600.0, 100.0),
                income(2024, 6, 30, ReportType::H1, 550.0, 320.0, 55.0),
                income(2024, 12, 31, ReportType::Annual, 1200.0, 700.0, 130.0),
                income(2025, 6, 30, ReportType::H1, 700.0, 400.0, 80.0),
            ],
            balance: vec![
                balance(2023, 12, 31, ReportType::Annual, 1.0),
                balance(2024, 12, 31, ReportType::Annual, 1.1),
                balance(2025, 6, 30, ReportType::H1, 1.2),
            ],
            cashflow: vec![
                cashflow(2023, 12, 31, ReportType::Annual, 150.0, 50.0),
                cashflow(2024, 12, 31, ReportType::Annual, 180.0, 60.0),
                cashflow(2025, 6, 30, ReportType::H1, 90.0, 25.0),
            ],
            indicators: vec![
                KeyIndicators {
                    meta: meta(2024, 12, 31, ReportType::Annual),
                    roe_weighted: Some(9.5),
                    ..Default::default()
                },
                KeyIndicators {
                    meta: meta(2025, 6, 30, ReportType::H1),
                    roe_weighted: Some(5.0),
                    ..Default::default()
                },
            ],
            dividends: vec![
                DividendRecord {
                    report_date: Some(d(2023, 12, 31)),
                    plan: Some("10派100元(含税)".into()),
                    ex_dividend_date: Some(d(2024, 6, 18)),
                    ..Default::default()
                },
                DividendRecord {
                    report_date: Some(d(2024, 12, 31)),
                    plan: Some("10派120元(含税)".into()),
                    ex_dividend_date: Some(d(2025, 6, 17)),
                    ..Default::default()
                },
            ],
            snapshot: Some(ValuationSnapshot {
                price: 100.0,
                name: "贵州茅台".into(),
                pe_ttm: Some(20.0),
                pe_static: Some(22.0),
                pb: Some(3.0),
                total_shares: Some(100.0),
                total_market_cap: Some(10_000.0),
                ..Default::default()
            }),
            valuation_history: (0..10)
                .map(|i| ValuationPoint {
                    date: d(2025, 1, 2) + chrono::Duration::days(i),
                    pe_ttm: Some(18.0 + i as f64),
                    pb_mrq: Some(2.8),
                    ps_ttm: Some(5.0),
                    pcf_ocf_ttm: Some(15.0),
                    ..Default::default()
                })
                .collect(),
        }
    }

    fn sym() -> Symbol {
        Symbol::new("600519").unwrap()
    }

    #[test]
    fn fundamentals_full_bundle_populates_every_section() {
        let json = fundamentals_json(&sym(), &sample_bundle(), &[]);
        assert!(json.missing.is_empty(), "missing: {:?}", json.missing);

        let profile = json.profile.unwrap();
        assert_eq!(profile.name, "贵州茅台");
        assert_eq!(profile.industry.as_deref(), Some("酿酒行业"));
        assert_eq!(profile.listing_date.as_deref(), Some("2001-08-27"));

        let latest = json.latest_period.unwrap();
        assert_eq!(latest.period_end, "2025-06-30");
        assert_eq!(latest.report_type, "h1");
        assert_eq!(latest.announced_date.as_deref(), Some("2025-08-14"));

        let m = json.metrics.unwrap();
        assert_eq!(m.revenue, Some(700.0));
        assert_eq!(m.net_profit, Some(72.0)); // 归母 80*0.9
        assert!((m.revenue_yoy.unwrap() - (700.0 / 550.0 - 1.0)).abs() < 1e-12);
        assert!((m.gross_margin.unwrap() - 0.428571).abs() < 1e-4);
        assert!(m.roe.is_some() && m.roa.is_some() && m.roic.is_some());
        assert!(m.dupont.is_some());
        assert_eq!(m.fcf, Some(65.0)); // 90 − 25
        assert!(m.ccc.is_some());
        assert!((m.current_ratio.unwrap() - 2.0).abs() < 1e-12);
        assert!((m.debt_ratio.unwrap() - 800.0 * 1.2 / (2000.0 * 1.2)).abs() < 1e-12);

        // Growth series: 4 periods, oldest first, YoY where a year-ago row exists.
        assert_eq!(json.growth_series.len(), 4);
        assert_eq!(json.growth_series[0].period_end, "2023-12-31");
        assert_eq!(json.growth_series[0].revenue_yoy, None);
        assert_eq!(json.growth_series[1].period_end, "2024-06-30");
        assert_eq!(json.growth_series[1].revenue_yoy, None); // no 2023 H1 row
        assert!((json.growth_series[2].revenue_yoy.unwrap() - 0.2).abs() < 1e-12);
        assert_eq!(json.growth_series[2].roe, Some(0.095)); // vendor 9.5% → 0.095

        let scores = json.scores.unwrap();
        let f = scores.piotroski.unwrap();
        assert_eq!(f.criteria.len(), 9);
        assert!(f.score <= f.available && f.available <= 9);
        assert!(!f.note.is_empty());
        let altman = scores.altman.unwrap();
        assert!(altman.z_emerging.is_some());
        assert!(matches!(
            altman.zone.as_deref(),
            Some("safe" | "grey" | "distress")
        ));
        let beneish = scores.beneish.unwrap();
        assert!(!beneish.interpretation.is_empty());

        // Dividends newest first.
        assert_eq!(json.dividends.len(), 2);
        assert_eq!(json.dividends[0].year, Some(2024));
        assert_eq!(json.dividends[1].year, Some(2023));
        assert_eq!(json.dividends[0].ex_date.as_deref(), Some("2025-06-17"));
    }

    #[test]
    fn fundamentals_empty_bundle_degrades_per_section() {
        let json = fundamentals_json(&sym(), &FundamentalBundle::default(), &[]);
        assert!(json.profile.is_none());
        assert!(json.latest_period.is_none());
        assert!(json.metrics.is_none());
        assert!(json.growth_series.is_empty());
        assert!(json.scores.is_none());
        assert!(json.anomalies.is_empty());
        assert!(json.dividends.is_empty());
        for section in ["profile", "metrics", "growth_series", "scores"] {
            assert!(
                json.missing.iter().any(|m| m == section),
                "missing {section}"
            );
        }
        // Serializes with explicit nulls, not skipped keys.
        let v = serde_json::to_value(&json).unwrap();
        assert!(v["profile"].is_null());
        assert!(v["metrics"].is_null());
        assert_eq!(v["missing"].as_array().unwrap().len(), json.missing.len());
    }

    #[test]
    fn fundamentals_failures_feed_missing_list() {
        let failures = vec![
            "income: network timeout".to_string(),
            "dividends: empty".to_string(),
        ];
        let json = fundamentals_json(&sym(), &FundamentalBundle::default(), &failures);
        assert!(json.missing.iter().any(|m| m == "income"));
        assert!(json.missing.iter().any(|m| m == "dividends"));
        // No duplicate entries.
        let mut sorted = json.missing.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), json.missing.len());
    }

    #[test]
    fn valuation_full_bundle_populates_every_section() {
        let json = valuation_json(&sym(), &sample_bundle(), &[]);
        assert!(json.missing.is_empty(), "missing: {:?}", json.missing);

        let cur = json.current.unwrap();
        assert_eq!(cur.price, 100.0);
        assert_eq!(cur.pe_ttm, Some(20.0));
        assert_eq!(cur.pe_static, Some(22.0));
        assert_eq!(cur.pb, Some(3.0));
        assert_eq!(cur.ps_ttm, Some(5.0));
        assert_eq!(cur.pcf, Some(15.0));
        assert_eq!(cur.market_cap, Some(10_000.0));

        let pct = json.percentile.unwrap();
        assert_eq!(pct.days, 10);
        // PE history 18..=27, current 20 → 3 of 10 ≤ 20 → 30%.
        assert!((pct.pe_ttm_pct.unwrap() - 30.0).abs() < 1e-12);
        assert_eq!(pct.pb_pct, Some(100.0)); // 3.0 above all 2.8
        assert!(!pct.method.is_empty());

        let dcf = json.dcf.unwrap();
        assert_eq!(dcf.assumptions.base_fcf, 120.0); // latest annual 180 − 60
        assert_eq!(dcf.assumptions.shares, 100.0);
        assert_eq!(dcf.assumptions.wacc, DCF_WACC);
        // 5y window has a single YoY (20%) → stage-1 growth 0.20.
        assert!((dcf.assumptions.stage1_growth - 0.2).abs() < 1e-12);
        assert!(dcf.bear.per_share < dcf.base.per_share);
        assert!(dcf.base.per_share < dcf.bull.per_share);
        assert_eq!(dcf.sensitivity.wacc.len(), 5);
        assert_eq!(dcf.sensitivity.growth.len(), 5);
        assert_eq!(dcf.sensitivity.values.len(), 5);
        assert!(dcf.sensitivity.values.iter().all(|row| row.len() == 5));
        assert!(!dcf.caveat.is_empty());

        assert_eq!(json.history_series.len(), 10);
        assert_eq!(json.history_series[0].date, "2025-01-02");
        assert_eq!(json.history_series[0].pe_ttm, Some(18.0));
        assert_eq!(json.history_series[0].pb, Some(2.8));
    }

    #[test]
    fn valuation_empty_bundle_degrades_per_section() {
        let json = valuation_json(&sym(), &FundamentalBundle::default(), &[]);
        assert!(json.current.is_none());
        assert!(json.percentile.is_none());
        assert!(json.dcf.is_none());
        assert!(json.history_series.is_empty());
        for section in ["current", "percentile", "dcf", "history_series"] {
            assert!(
                json.missing.iter().any(|m| m == section),
                "missing {section}"
            );
        }
        let v = serde_json::to_value(&json).unwrap();
        assert!(v["current"].is_null());
        assert!(v["dcf"].is_null());
    }

    #[test]
    fn dcf_growth_is_clamped_and_defaults_to_zero() {
        // Shrinking revenue → floored at 0, not negative.
        let mut bundle = sample_bundle();
        bundle.income = vec![
            income(2023, 12, 31, ReportType::Annual, 1000.0, 600.0, 100.0),
            income(2024, 12, 31, ReportType::Annual, 800.0, 600.0, 80.0),
        ];
        assert_eq!(capped_base_growth(&bundle), 0.0);
        // No annual rows at all → 0, never a fabrication.
        bundle.income.clear();
        assert_eq!(capped_base_growth(&bundle), 0.0);
        // Explosive growth → capped at 25%.
        bundle.income = vec![
            income(2023, 12, 31, ReportType::Annual, 100.0, 50.0, 10.0),
            income(2024, 12, 31, ReportType::Annual, 200.0, 100.0, 20.0),
        ];
        assert_eq!(capped_base_growth(&bundle), DCF_GROWTH_CAP);
    }
}
