//! Parsing tests against real EastMoney responses, captured live on
//! 2026-08-21 for 600519 (贵州茅台) and trimmed to a few rows each.

use astock_fundamental::model::ReportType;
use astock_fundamental::parse;
use chrono::NaiveDate;
use serde_json::Value;

fn rows(fixture: &str) -> Vec<Value> {
    let v: Value = serde_json::from_str(fixture).unwrap();
    v.pointer("/result/data").unwrap().as_array().unwrap().clone()
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

#[test]
fn income_fixture_parses_with_point_in_time_date() {
    let parsed = parse::parse_income(rows(include_str!("fixtures/em_f10_income_600519.json")).as_slice());
    assert_eq!(parsed.len(), 2);
    // Rows are sorted ascending; the newest is 2026 H1.
    let latest = parsed.last().unwrap();
    let meta = latest.meta.unwrap();
    assert_eq!(meta.period_end, d(2026, 6, 30));
    assert_eq!(meta.report_type, ReportType::H1);
    // 公告日期 present — critical for point-in-time use.
    assert_eq!(meta.announced, Some(d(2026, 8, 15)));
    assert_eq!(latest.total_operating_revenue, Some(92278072083.21));
    assert_eq!(latest.net_profit_parent, Some(44516880421.86));
    assert_eq!(latest.basic_eps, Some(35.57));
}

#[test]
fn balance_fixture_tolerates_nulls() {
    let parsed =
        parse::parse_balance(rows(include_str!("fixtures/em_f10_balance_600519.json")).as_slice());
    assert_eq!(parsed.len(), 2);
    let latest = parsed.last().unwrap();
    assert_eq!(latest.total_assets, Some(309050784569.31));
    assert_eq!(latest.total_parent_equity, Some(251253594419.5));
    // Moutai reports no goodwill / short-term debt: Missing, not zero.
    assert_eq!(latest.goodwill, None);
    assert_eq!(latest.short_term_debt, None);
    assert_eq!(latest.share_capital, Some(1250081601.0));
    assert!(latest.retained_earnings.unwrap() > 0.0);
}

#[test]
fn cashflow_fixture_parses_cfo_and_capex() {
    let parsed = parse::parse_cashflow(
        rows(include_str!("fixtures/em_f10_cashflow_600519.json")).as_slice(),
    );
    let latest = parsed.last().unwrap();
    assert_eq!(latest.net_cfo, Some(70690750119.06));
    assert_eq!(latest.capex, Some(832142752.28));
    assert!(latest.depreciation.unwrap() > 0.0);
}

#[test]
fn mainfina_fixture_parses_key_indicators() {
    let parsed = parse::parse_indicators(
        rows(include_str!("fixtures/em_f10_mainfina_600519.json")).as_slice(),
    );
    let latest = parsed.last().unwrap();
    assert_eq!(latest.eps_basic, Some(35.57));
    assert_eq!(latest.roe_weighted, Some(16.75));
    assert!((latest.gross_margin.unwrap() - 89.5552128279).abs() < 1e-9);
    assert!(latest.roic.unwrap() > 0.0);
}

#[test]
fn survey_fixture_parses_profile() {
    let v: Value =
        serde_json::from_str(include_str!("fixtures/em_f10_survey_600519.json")).unwrap();
    let profile = parse::parse_survey(&v, "600519");
    assert_eq!(profile.name, "贵州茅台酒股份有限公司");
    assert_eq!(profile.short_name, "贵州茅台");
    assert_eq!(profile.industry.as_deref(), Some("酿酒行业"));
    assert_eq!(profile.listing_date, Some(d(2001, 8, 27)));
    // Share counts come from the quote snapshot, not the survey.
    assert_eq!(profile.total_shares, None);
}

#[test]
fn sharebonus_fixture_parses_dividends() {
    let parsed = parse::parse_dividends(
        rows(include_str!("fixtures/em_sharebonus_600519.json")).as_slice(),
    );
    assert!(!parsed.is_empty());
    let latest = parsed.last().unwrap();
    assert_eq!(latest.pretax_cash_per_10, Some(280.2423));
    assert_eq!(latest.ex_dividend_date, Some(d(2026, 6, 26)));
    assert!(latest.plan.as_deref().unwrap().contains("10派"));
}

#[test]
fn valueanalysis_fixture_parses_history() {
    let parsed = parse::parse_valuation_history(
        rows(include_str!("fixtures/em_valueanalysis_600519.json")).as_slice(),
    );
    assert_eq!(parsed.len(), 5);
    let latest = parsed.last().unwrap();
    assert_eq!(latest.date, d(2026, 8, 21));
    assert!((latest.pe_ttm.unwrap() - 19.53903349).abs() < 1e-6);
    assert!(latest.ps_ttm.unwrap() > 0.0);
    assert!(latest.pcf_ocf_ttm.unwrap() > 0.0);
    assert_eq!(latest.total_shares, Some(1250081601.0));
}

#[test]
fn quote_ext_fixture_parses_snapshot() {
    let v: Value =
        serde_json::from_str(include_str!("fixtures/em_quote_ext_600519.json")).unwrap();
    let snap = parse::parse_snapshot(v.get("data").unwrap());
    assert_eq!(snap.price, 1272.83);
    assert_eq!(snap.name, "贵州茅台");
    assert_eq!(snap.pe_ttm, Some(19.54)); // f164
    assert_eq!(snap.pe_static, Some(19.33)); // f163 == PE_LAR
    assert_eq!(snap.pe_dynamic, Some(17.87)); // f162
    assert_eq!(snap.pb, Some(6.33)); // f167
    assert_eq!(snap.total_shares, Some(1250081601.0)); // f84
}
