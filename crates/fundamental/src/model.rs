//! Normalized fundamental data model for A-share companies.
//!
//! Every numeric field is `Option<f64>`: EastMoney omits line items that do
//! not apply to a company (e.g. 茅台 has no goodwill) and sometimes serves
//! `"-"` or null. `None` is the typed representation of "Missing" — callers
//! must never treat it as zero. Monetary amounts are in CNY （元）, ratios in
//! percent where noted.

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

/// Reporting period kind, derived from the EastMoney `REPORT_TYPE` string
/// (一季报/中报/三季报/年报) with the period-end month as fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReportType {
    /// Q1 report (period end 03-31).
    Q1,
    /// Semi-annual report (period end 06-30).
    H1,
    /// Q3 report (period end 09-30).
    Q3,
    /// Annual report (period end 12-31).
    Annual,
}

impl ReportType {
    /// Classify from a period-end date (month 3/6/9/12).
    pub fn from_period_end(d: &NaiveDate) -> Option<Self> {
        match d.month() {
            3 => Some(ReportType::Q1),
            6 => Some(ReportType::H1),
            9 => Some(ReportType::Q3),
            12 => Some(ReportType::Annual),
            _ => None,
        }
    }
}

/// Period metadata shared by all statement kinds.
///
/// `announced` (公告日期, EM `NOTICE_DATE`) is the first day the market could
/// see these numbers — critical for point-in-time analysis. It is `None`
/// only when the upstream omits it; never guess it from `period_end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodMeta {
    /// Last day of the reporting period, e.g. 2025-12-31.
    pub period_end: NaiveDate,
    /// Q1/H1/Q3/Annual classification.
    pub report_type: ReportType,
    /// 公告日期 — when the report was published, if known.
    pub announced: Option<NaiveDate>,
}

/// 利润表 — income statement, cumulative for the period (YTD within the
/// fiscal year, matching the EM `RPT_F10_FINANCE_GINCOME` convention).
/// Single-quarter values must be derived by differencing (see `metrics`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IncomeStatement {
    /// Period metadata.
    pub meta: Option<PeriodMeta>,
    /// 营业总收入 TOTAL_OPERATE_INCOME.
    pub total_operating_revenue: Option<f64>,
    /// 营业收入 OPERATE_INCOME.
    pub operating_revenue: Option<f64>,
    /// 营业成本 OPERATE_COST.
    pub operating_cost: Option<f64>,
    /// 营业税金及附加 OPERATE_TAX_ADD.
    pub taxes_and_surcharges: Option<f64>,
    /// 销售费用 SALE_EXPENSE.
    pub selling_expense: Option<f64>,
    /// 管理费用 MANAGE_EXPENSE.
    pub admin_expense: Option<f64>,
    /// 研发费用 RESEARCH_EXPENSE.
    pub rd_expense: Option<f64>,
    /// 财务费用 FINANCE_EXPENSE (negative = net interest income).
    pub finance_expense: Option<f64>,
    /// 投资收益 INVEST_INCOME.
    pub invest_income: Option<f64>,
    /// 公允价值变动收益 FAIRVALUE_CHANGE_INCOME.
    pub fairvalue_change_income: Option<f64>,
    /// 营业利润 OPERATE_PROFIT.
    pub operating_profit: Option<f64>,
    /// 利润总额 TOTAL_PROFIT.
    pub total_profit: Option<f64>,
    /// 所得税 INCOME_TAX.
    pub income_tax: Option<f64>,
    /// 净利润 NETPROFIT (including minorities).
    pub net_profit: Option<f64>,
    /// 归母净利润 PARENT_NETPROFIT.
    pub net_profit_parent: Option<f64>,
    /// 扣非归母净利润 DEDUCT_PARENT_NETPROFIT.
    pub net_profit_parent_deducted: Option<f64>,
    /// 少数股东损益 MINORITY_INTEREST.
    pub minority_profit: Option<f64>,
    /// 基本每股收益 BASIC_EPS (CNY/share).
    pub basic_eps: Option<f64>,
}

/// 资产负债表 — balance sheet at period end (EM `RPT_F10_FINANCE_GBALANCE`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BalanceSheet {
    /// Period metadata.
    pub meta: Option<PeriodMeta>,
    /// 货币资金 MONETARYFUNDS.
    pub monetary_funds: Option<f64>,
    /// 应收票据及应收账款 NOTE_ACCOUNTS_RECE.
    pub notes_and_accounts_receivable: Option<f64>,
    /// 应收账款 ACCOUNTS_RECE.
    pub accounts_receivable: Option<f64>,
    /// 预付款项 PREPAYMENT.
    pub prepayments: Option<f64>,
    /// 存货 INVENTORY.
    pub inventory: Option<f64>,
    /// 合同资产 CONTRACT_ASSET.
    pub contract_assets: Option<f64>,
    /// 流动资产合计 TOTAL_CURRENT_ASSETS.
    pub total_current_assets: Option<f64>,
    /// 固定资产 FIXED_ASSET.
    pub fixed_assets: Option<f64>,
    /// 在建工程 CIP.
    pub construction_in_progress: Option<f64>,
    /// 无形资产 INTANGIBLE_ASSET.
    pub intangible_assets: Option<f64>,
    /// 商誉 GOODWILL.
    pub goodwill: Option<f64>,
    /// 资产总计 TOTAL_ASSETS.
    pub total_assets: Option<f64>,
    /// 应付票据及应付账款 NOTE_ACCOUNTS_PAYABLE.
    pub notes_and_accounts_payable: Option<f64>,
    /// 应付账款 ACCOUNTS_PAYABLE.
    pub accounts_payable: Option<f64>,
    /// 预收款项 ADVANCE_RECEIVABLES.
    pub advance_from_customers: Option<f64>,
    /// 合同负债 CONTRACT_LIAB.
    pub contract_liabilities: Option<f64>,
    /// 短期借款 SHORT_LOAN.
    pub short_term_debt: Option<f64>,
    /// 一年内到期的非流动负债 NONCURRENT_LIAB_1YEAR.
    pub current_portion_of_noncurrent_debt: Option<f64>,
    /// 长期借款 LONG_LOAN.
    pub long_term_debt: Option<f64>,
    /// 应付债券 BOND_PAYABLE.
    pub bonds_payable: Option<f64>,
    /// 租赁负债 LEASE_LIAB.
    pub lease_liabilities: Option<f64>,
    /// 流动负债合计 TOTAL_CURRENT_LIAB.
    pub total_current_liabilities: Option<f64>,
    /// 负债合计 TOTAL_LIABILITIES.
    pub total_liabilities: Option<f64>,
    /// 实收资本(股本) SHARE_CAPITAL.
    pub share_capital: Option<f64>,
    /// 未分配利润 UNASSIGN_RPOFIT (retained earnings — needed for Altman X2).
    pub retained_earnings: Option<f64>,
    /// 归母股东权益 TOTAL_PARENT_EQUITY.
    pub total_parent_equity: Option<f64>,
    /// 少数股东权益 MINORITY_EQUITY.
    pub minority_equity: Option<f64>,
    /// 股东权益合计 TOTAL_EQUITY.
    pub total_equity: Option<f64>,
}

impl BalanceSheet {
    /// Interest-bearing debt proxy: short-term debt + current portion of
    /// non-current debt + long-term debt + bonds + lease liabilities.
    /// `None` when every component is missing; missing components otherwise
    /// count as zero (a company that reports long-term debt but no
    /// short-term line genuinely has none).
    pub fn interest_bearing_debt(&self) -> Option<f64> {
        let parts = [
            self.short_term_debt,
            self.current_portion_of_noncurrent_debt,
            self.long_term_debt,
            self.bonds_payable,
            self.lease_liabilities,
        ];
        if parts.iter().all(|p| p.is_none()) {
            return None;
        }
        Some(parts.iter().flatten().sum())
    }
}

/// 现金流量表 — cash flow statement, cumulative (EM `RPT_F10_FINANCE_GCASHFLOW`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CashFlowStatement {
    /// Period metadata.
    pub meta: Option<PeriodMeta>,
    /// 销售商品、提供劳务收到的现金 SALES_SERVICES.
    pub cash_from_sales: Option<f64>,
    /// 经营活动产生的现金流量净额 NETCASH_OPERATE.
    pub net_cfo: Option<f64>,
    /// 投资活动产生的现金流量净额 NETCASH_INVEST.
    pub net_cfi: Option<f64>,
    /// 筹资活动产生的现金流量净额 NETCASH_FINANCE.
    pub net_cff: Option<f64>,
    /// 购建固定资产、无形资产和其他长期资产支付的现金 CONSTRUCT_LONG_ASSET
    /// — the standard capex proxy.
    pub capex: Option<f64>,
    /// 期末现金及现金等价物余额 END_CCE.
    pub end_cash_and_equivalents: Option<f64>,
    /// 固定资产折旧 FA_IR_DEPR (supplementary section; needed for Beneish DEPI).
    pub depreciation: Option<f64>,
}

/// 主要指标 — key indicators (EM `RPT_F10_FINANCE_MAINFINADATA`).
/// Ratios are in percent, per-share values in CNY.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KeyIndicators {
    /// Period metadata.
    pub meta: Option<PeriodMeta>,
    /// 基本每股收益 EPSJB (CNY).
    pub eps_basic: Option<f64>,
    /// 扣非每股收益 EPSKCJB (CNY).
    pub eps_deducted: Option<f64>,
    /// 每股净资产 BPS (CNY).
    pub bps: Option<f64>,
    /// 每股经营现金流 MGJYXJJE (CNY).
    pub cfo_per_share: Option<f64>,
    /// 加权净资产收益率 ROEJQ (%). EM's own weighted-average convention.
    pub roe_weighted: Option<f64>,
    /// 扣非加权净资产收益率 ROEKCJQ (%).
    pub roe_deducted_weighted: Option<f64>,
    /// 销售毛利率 XSMLL (%).
    pub gross_margin: Option<f64>,
    /// 销售净利率 XSJLL (%).
    pub net_margin: Option<f64>,
    /// 资产负债率 ZCFZL (%).
    pub debt_ratio: Option<f64>,
    /// 投入资本回报率 ROIC (%). EM's convention; treat as vendor-computed.
    pub roic: Option<f64>,
    /// 营业总收入同比 TOTALOPERATEREVETZ (%).
    pub revenue_yoy: Option<f64>,
    /// 归母净利润同比 PARENTNETPROFITTZ (%).
    pub profit_yoy: Option<f64>,
}

/// 公司概况 — company profile from the CompanySurvey endpoint plus share
/// counts from the extended quote snapshot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompanyProfile {
    /// 6-digit code, e.g. "600519".
    pub code: String,
    /// 公司全称 (jbzl.gsmc).
    pub name: String,
    /// A股简称 (jbzl.agjc).
    pub short_name: String,
    /// 所属行业 (jbzl.sshy), e.g. "酿酒行业".
    pub industry: Option<String>,
    /// 证监会行业 (jbzl.sszjhhy), e.g. "制造业-酒、饮料和精制茶制造业".
    pub industry_csrc: Option<String>,
    /// 上市日期 (fxxg.ssrq).
    pub listing_date: Option<NaiveDate>,
    /// 总股本 (quote f84; cross-check: balance-sheet 股本 SHARE_CAPITAL).
    pub total_shares: Option<f64>,
    /// 流通A股 (quote f85).
    pub float_shares: Option<f64>,
}

/// 分红送配 — one dividend/bonus implementation record
/// (EM `RPT_SHAREBONUS_DET`; only rows that reached 实施分配 carry ex dates).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DividendRecord {
    /// 报告期 REPORT_DATE (the fiscal period the distribution belongs to).
    pub report_date: Option<NaiveDate>,
    /// 实施方案 IMPL_PLAN_PROFILE, e.g. "10派280.2423元(含税)".
    pub plan: Option<String>,
    /// 税前每10股派现 PRETAX_BONUS_RMB (CNY per 10 shares).
    pub pretax_cash_per_10: Option<f64>,
    /// 送股比例 BONUS_RATIO (per 10 shares).
    pub bonus_share_per_10: Option<f64>,
    /// 转增比例 IT_RATIO (per 10 shares).
    pub transfer_share_per_10: Option<f64>,
    /// 股权登记日 EQUITY_RECORD_DATE.
    pub record_date: Option<NaiveDate>,
    /// 除权除息日 EX_DIVIDEND_DATE.
    pub ex_dividend_date: Option<NaiveDate>,
}

/// Valuation snapshot from the extended push2 quote fields
/// (f-codes verified against `RPT_VALUEANALYSIS_DET`, 2026-08-21).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ValuationSnapshot {
    /// 最新价 f43.
    pub price: f64,
    /// 名称 f58.
    pub name: String,
    /// PE(TTM) — f164.
    pub pe_ttm: Option<f64>,
    /// PE(静, last-annual-report) — f163; matches EM's PE_LAR.
    pub pe_static: Option<f64>,
    /// PE(动, annualized current-year) — f162.
    pub pe_dynamic: Option<f64>,
    /// PB(MRQ) — f167.
    pub pb: Option<f64>,
    /// 总股本 f84.
    pub total_shares: Option<f64>,
    /// 流通A股 f85.
    pub float_shares: Option<f64>,
    /// 总市值 f116 (CNY).
    pub total_market_cap: Option<f64>,
    /// 流通市值 f117 (CNY).
    pub float_market_cap: Option<f64>,
}

/// One day of valuation history (`RPT_VALUEANALYSIS_DET`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ValuationPoint {
    /// Trading day TRADE_DATE.
    pub date: NaiveDate,
    /// 收盘价 CLOSE_PRICE.
    pub close: Option<f64>,
    /// PE(TTM).
    pub pe_ttm: Option<f64>,
    /// PE(静态, LAR).
    pub pe_lar: Option<f64>,
    /// PB(MRQ).
    pub pb_mrq: Option<f64>,
    /// PCF(经营现金流TTM).
    pub pcf_ocf_ttm: Option<f64>,
    /// PS(TTM).
    pub ps_ttm: Option<f64>,
    /// 总股本 TOTAL_SHARES.
    pub total_shares: Option<f64>,
    /// 总市值 TOTAL_MARKET_CAP.
    pub total_market_cap: Option<f64>,
}

/// Everything the fundamental pipeline fetched for one symbol. Vectors are
/// sorted ascending by period/date (oldest first).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FundamentalBundle {
    /// 公司概况 (None when the survey endpoint failed).
    pub profile: Option<CompanyProfile>,
    /// Income statements, oldest first.
    pub income: Vec<IncomeStatement>,
    /// Balance sheets, oldest first.
    pub balance: Vec<BalanceSheet>,
    /// Cash flow statements, oldest first.
    pub cashflow: Vec<CashFlowStatement>,
    /// Key indicators, oldest first.
    pub indicators: Vec<KeyIndicators>,
    /// Dividend records, oldest ex-date first.
    pub dividends: Vec<DividendRecord>,
    /// Current valuation snapshot (None when the quote pool failed).
    pub snapshot: Option<ValuationSnapshot>,
    /// Daily valuation history, oldest first.
    pub valuation_history: Vec<ValuationPoint>,
}
