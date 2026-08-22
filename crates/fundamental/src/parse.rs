//! Tolerant parsing of EastMoney F10 / datacenter JSON rows into the
//! normalized [`crate::model`] types.
//!
//! Tolerance rules (learned from live responses):
//! - numbers may arrive as JSON numbers *or* numeric strings;
//! - `"- "`-style placeholders (`"-"`, `""`, `"--"`) and nulls mean Missing;
//! - unknown extra keys are ignored, missing keys yield `None`;
//! - dates arrive as `"2026-06-30 00:00:00"` (datetime) or `"2026-06-26"`.
//!
//! A row without a parseable `REPORT_DATE` is dropped entirely — period-less
//! statements are useless for time-series work.

use crate::model::{
    BalanceSheet, CashFlowStatement, CompanyProfile, DividendRecord, IncomeStatement,
    KeyIndicators, PeriodMeta, ReportType, ValuationPoint, ValuationSnapshot,
};
use chrono::NaiveDate;
use serde_json::Value;

/// Lenient float extraction: numbers pass through, numeric strings parse,
/// `"-"`/`""`/`"--"`/null become `None`.
pub fn json_f64(row: &Value, key: &str) -> Option<f64> {
    match row.get(key) {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() || s == "-" || s == "--" {
                None
            } else {
                s.parse::<f64>().ok()
            }
        }
        _ => None,
    }
}

/// Lenient string extraction.
pub fn json_str(row: &Value, key: &str) -> Option<String> {
    match row.get(key) {
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() || s == "--" {
                None
            } else {
                Some(s.to_string())
            }
        }
        _ => None,
    }
}

/// Parse an EM date/datetime string: takes the date part before any space.
pub fn json_date(row: &Value, key: &str) -> Option<NaiveDate> {
    let s = json_str(row, key)?;
    let date_part = s.split([' ', 'T']).next()?;
    NaiveDate::parse_from_str(date_part, "%Y-%m-%d").ok()
}

/// Build period metadata from a statement row. Returns `None` (drop the row)
/// when `REPORT_DATE` is unparseable.
fn meta(row: &Value) -> Option<PeriodMeta> {
    let period_end = json_date(row, "REPORT_DATE")?;
    let report_type = match json_str(row, "REPORT_TYPE").as_deref() {
        Some("一季报") => ReportType::Q1,
        Some("中报") => ReportType::H1,
        Some("三季报") => ReportType::Q3,
        Some("年报") => ReportType::Annual,
        // Fallback for unexpected labels: classify by period-end month.
        _ => ReportType::from_period_end(&period_end)?,
    };
    Some(PeriodMeta {
        period_end,
        report_type,
        announced: json_date(row, "NOTICE_DATE"),
    })
}

/// Parse `RPT_F10_FINANCE_GINCOME` rows into income statements,
/// sorted oldest first.
pub fn parse_income(rows: &[Value]) -> Vec<IncomeStatement> {
    let mut out: Vec<IncomeStatement> = rows
        .iter()
        .filter_map(|row| {
            Some(IncomeStatement {
                meta: Some(meta(row)?),
                total_operating_revenue: json_f64(row, "TOTAL_OPERATE_INCOME"),
                operating_revenue: json_f64(row, "OPERATE_INCOME"),
                operating_cost: json_f64(row, "OPERATE_COST"),
                taxes_and_surcharges: json_f64(row, "OPERATE_TAX_ADD"),
                selling_expense: json_f64(row, "SALE_EXPENSE"),
                admin_expense: json_f64(row, "MANAGE_EXPENSE"),
                rd_expense: json_f64(row, "RESEARCH_EXPENSE"),
                finance_expense: json_f64(row, "FINANCE_EXPENSE"),
                invest_income: json_f64(row, "INVEST_INCOME"),
                fairvalue_change_income: json_f64(row, "FAIRVALUE_CHANGE_INCOME"),
                operating_profit: json_f64(row, "OPERATE_PROFIT"),
                total_profit: json_f64(row, "TOTAL_PROFIT"),
                income_tax: json_f64(row, "INCOME_TAX"),
                net_profit: json_f64(row, "NETPROFIT"),
                net_profit_parent: json_f64(row, "PARENT_NETPROFIT"),
                net_profit_parent_deducted: json_f64(row, "DEDUCT_PARENT_NETPROFIT"),
                minority_profit: json_f64(row, "MINORITY_INTEREST"),
                basic_eps: json_f64(row, "BASIC_EPS"),
            })
        })
        .collect();
    out.sort_by_key(|s| s.meta.map(|m| m.period_end));
    out
}

/// Parse `RPT_F10_FINANCE_GBALANCE` rows into balance sheets, oldest first.
pub fn parse_balance(rows: &[Value]) -> Vec<BalanceSheet> {
    let mut out: Vec<BalanceSheet> = rows
        .iter()
        .filter_map(|row| {
            Some(BalanceSheet {
                meta: Some(meta(row)?),
                monetary_funds: json_f64(row, "MONETARYFUNDS"),
                notes_and_accounts_receivable: json_f64(row, "NOTE_ACCOUNTS_RECE"),
                accounts_receivable: json_f64(row, "ACCOUNTS_RECE"),
                prepayments: json_f64(row, "PREPAYMENT"),
                inventory: json_f64(row, "INVENTORY"),
                contract_assets: json_f64(row, "CONTRACT_ASSET"),
                total_current_assets: json_f64(row, "TOTAL_CURRENT_ASSETS"),
                fixed_assets: json_f64(row, "FIXED_ASSET"),
                construction_in_progress: json_f64(row, "CIP"),
                intangible_assets: json_f64(row, "INTANGIBLE_ASSET"),
                goodwill: json_f64(row, "GOODWILL"),
                total_assets: json_f64(row, "TOTAL_ASSETS"),
                notes_and_accounts_payable: json_f64(row, "NOTE_ACCOUNTS_PAYABLE"),
                accounts_payable: json_f64(row, "ACCOUNTS_PAYABLE"),
                advance_from_customers: json_f64(row, "ADVANCE_RECEIVABLES"),
                contract_liabilities: json_f64(row, "CONTRACT_LIAB"),
                short_term_debt: json_f64(row, "SHORT_LOAN"),
                current_portion_of_noncurrent_debt: json_f64(row, "NONCURRENT_LIAB_1YEAR"),
                long_term_debt: json_f64(row, "LONG_LOAN"),
                bonds_payable: json_f64(row, "BOND_PAYABLE"),
                lease_liabilities: json_f64(row, "LEASE_LIAB"),
                total_current_liabilities: json_f64(row, "TOTAL_CURRENT_LIAB"),
                total_liabilities: json_f64(row, "TOTAL_LIABILITIES"),
                share_capital: json_f64(row, "SHARE_CAPITAL"),
                retained_earnings: json_f64(row, "UNASSIGN_RPOFIT"),
                total_parent_equity: json_f64(row, "TOTAL_PARENT_EQUITY"),
                minority_equity: json_f64(row, "MINORITY_EQUITY"),
                total_equity: json_f64(row, "TOTAL_EQUITY"),
            })
        })
        .collect();
    out.sort_by_key(|s| s.meta.map(|m| m.period_end));
    out
}

/// Parse `RPT_F10_FINANCE_GCASHFLOW` rows into cash flow statements,
/// oldest first.
pub fn parse_cashflow(rows: &[Value]) -> Vec<CashFlowStatement> {
    let mut out: Vec<CashFlowStatement> = rows
        .iter()
        .filter_map(|row| {
            Some(CashFlowStatement {
                meta: Some(meta(row)?),
                cash_from_sales: json_f64(row, "SALES_SERVICES"),
                net_cfo: json_f64(row, "NETCASH_OPERATE"),
                net_cfi: json_f64(row, "NETCASH_INVEST"),
                net_cff: json_f64(row, "NETCASH_FINANCE"),
                capex: json_f64(row, "CONSTRUCT_LONG_ASSET"),
                end_cash_and_equivalents: json_f64(row, "END_CCE"),
                depreciation: json_f64(row, "FA_IR_DEPR"),
            })
        })
        .collect();
    out.sort_by_key(|s| s.meta.map(|m| m.period_end));
    out
}

/// Parse `RPT_F10_FINANCE_MAINFINADATA` rows into key indicators,
/// oldest first.
pub fn parse_indicators(rows: &[Value]) -> Vec<KeyIndicators> {
    let mut out: Vec<KeyIndicators> = rows
        .iter()
        .filter_map(|row| {
            Some(KeyIndicators {
                meta: Some(meta(row)?),
                eps_basic: json_f64(row, "EPSJB"),
                eps_deducted: json_f64(row, "EPSKCJB"),
                bps: json_f64(row, "BPS"),
                cfo_per_share: json_f64(row, "MGJYXJJE"),
                roe_weighted: json_f64(row, "ROEJQ"),
                roe_deducted_weighted: json_f64(row, "ROEKCJQ"),
                gross_margin: json_f64(row, "XSMLL"),
                net_margin: json_f64(row, "XSJLL"),
                debt_ratio: json_f64(row, "ZCFZL"),
                roic: json_f64(row, "ROIC"),
                revenue_yoy: json_f64(row, "TOTALOPERATEREVETZ"),
                profit_yoy: json_f64(row, "PARENTNETPROFITTZ"),
            })
        })
        .collect();
    out.sort_by_key(|s| s.meta.map(|m| m.period_end));
    out
}

/// Parse the CompanySurvey response (`{jbzl, fxxg, ...}`). Share counts are
/// NOT in this payload — fill them from the valuation snapshot.
pub fn parse_survey(value: &Value, code: &str) -> CompanyProfile {
    let jbzl = value.get("jbzl").cloned().unwrap_or(Value::Null);
    let fxxg = value.get("fxxg").cloned().unwrap_or(Value::Null);
    CompanyProfile {
        code: code.to_string(),
        name: json_str(&jbzl, "gsmc").unwrap_or_default(),
        short_name: json_str(&jbzl, "agjc").unwrap_or_default(),
        industry: json_str(&jbzl, "sshy"),
        industry_csrc: json_str(&jbzl, "sszjhhy"),
        listing_date: json_date(&fxxg, "ssrq"),
        total_shares: None,
        float_shares: None,
    }
}

/// Parse `RPT_SHAREBONUS_DET` rows into dividend records, oldest first.
pub fn parse_dividends(rows: &[Value]) -> Vec<DividendRecord> {
    let mut out: Vec<DividendRecord> = rows
        .iter()
        .map(|row| DividendRecord {
            report_date: json_date(row, "REPORT_DATE"),
            plan: json_str(row, "IMPL_PLAN_PROFILE"),
            pretax_cash_per_10: json_f64(row, "PRETAX_BONUS_RMB"),
            bonus_share_per_10: json_f64(row, "BONUS_RATIO"),
            transfer_share_per_10: json_f64(row, "IT_RATIO"),
            record_date: json_date(row, "EQUITY_RECORD_DATE"),
            ex_dividend_date: json_date(row, "EX_DIVIDEND_DATE"),
        })
        .collect();
    out.sort_by_key(|d| d.ex_dividend_date);
    out
}

/// Parse `RPT_VALUEANALYSIS_DET` rows into daily valuation points,
/// oldest first. Rows without a trade date are dropped.
pub fn parse_valuation_history(rows: &[Value]) -> Vec<ValuationPoint> {
    let mut out: Vec<ValuationPoint> = rows
        .iter()
        .filter_map(|row| {
            Some(ValuationPoint {
                date: json_date(row, "TRADE_DATE")?,
                close: json_f64(row, "CLOSE_PRICE"),
                pe_ttm: json_f64(row, "PE_TTM"),
                pe_lar: json_f64(row, "PE_LAR"),
                pb_mrq: json_f64(row, "PB_MRQ"),
                pcf_ocf_ttm: json_f64(row, "PCF_OCF_TTM"),
                ps_ttm: json_f64(row, "PS_TTM"),
                total_shares: json_f64(row, "TOTAL_SHARES"),
                total_market_cap: json_f64(row, "TOTAL_MARKET_CAP"),
            })
        })
        .collect();
    out.sort_by_key(|p| p.date);
    out
}

/// Parse the `data` object of the extended push2 quote into a valuation
/// snapshot. Field-code mapping verified against `RPT_VALUEANALYSIS_DET`:
/// f164=PE_TTM, f163=PE_LAR, f167=PB_MRQ, f162=PE(dynamic).
pub fn parse_snapshot(data: &Value) -> ValuationSnapshot {
    ValuationSnapshot {
        price: json_f64(data, "f43").unwrap_or(0.0),
        name: json_str(data, "f58").unwrap_or_default(),
        pe_ttm: json_f64(data, "f164"),
        pe_static: json_f64(data, "f163"),
        pe_dynamic: json_f64(data, "f162"),
        pb: json_f64(data, "f167"),
        total_shares: json_f64(data, "f84"),
        float_shares: json_f64(data, "f85"),
        total_market_cap: json_f64(data, "f116"),
        float_market_cap: json_f64(data, "f117"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lenient_number_parsing() {
        let row = json!({"A": 1.5, "B": "2.5", "C": "-", "D": "", "E": null, "F": "--"});
        assert_eq!(json_f64(&row, "A"), Some(1.5));
        assert_eq!(json_f64(&row, "B"), Some(2.5));
        assert_eq!(json_f64(&row, "C"), None);
        assert_eq!(json_f64(&row, "D"), None);
        assert_eq!(json_f64(&row, "E"), None);
        assert_eq!(json_f64(&row, "F"), None);
        assert_eq!(json_f64(&row, "MISSING"), None);
    }

    #[test]
    fn date_parsing_accepts_datetime_and_date() {
        let row = json!({"A": "2026-06-30 00:00:00", "B": "2026-06-26", "C": "n/a"});
        assert_eq!(json_date(&row, "A"), NaiveDate::from_ymd_opt(2026, 6, 30));
        assert_eq!(json_date(&row, "B"), NaiveDate::from_ymd_opt(2026, 6, 26));
        assert_eq!(json_date(&row, "C"), None);
    }

    #[test]
    fn rows_without_report_date_are_dropped() {
        let rows = vec![
            json!({"REPORT_DATE": "2025-12-31 00:00:00", "REPORT_TYPE": "年报", "NETPROFIT": 1.0}),
            json!({"REPORT_TYPE": "年报", "NETPROFIT": 2.0}),
        ];
        let parsed = parse_income(&rows);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].net_profit, Some(1.0));
    }

    #[test]
    fn report_type_falls_back_to_period_end_month() {
        let rows = vec![json!({"REPORT_DATE": "2025-09-30 00:00:00", "REPORT_TYPE": "unexpected"})];
        let parsed = parse_income(&rows);
        assert_eq!(parsed[0].meta.map(|m| m.report_type), Some(ReportType::Q3));
    }

    #[test]
    fn interest_bearing_debt_treats_missing_components_as_absent() {
        // All components missing → None (typed Missing, not zero).
        assert_eq!(BalanceSheet::default().interest_bearing_debt(), None);
        let bs = BalanceSheet {
            long_term_debt: Some(100.0),
            ..Default::default()
        };
        assert_eq!(bs.interest_bearing_debt(), Some(100.0));
    }
}
