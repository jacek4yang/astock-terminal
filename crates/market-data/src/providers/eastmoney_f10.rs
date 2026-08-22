//! EastMoney F10 / datacenter endpoints: financial statements, key indicators,
//! company survey, dividend history, daily valuation history, and the extended
//! quote snapshot used for valuation.
//!
//! Unlike the push2 quote endpoints, the datacenter APIs answer with a
//! `{"result": {"pages": N, "count": M, "data": [...]}}` envelope (no top-level
//! `data` key), so they cannot go through `get_json_pool` and are called
//! directly on their single host.
//!
//! # Endpoints verified live on 2026-08-21 with 600519 (贵州茅台)
//!
//! - **Statements / key indicators** — `GET https://datacenter.eastmoney.com/securities/api/data/v1/get`
//!   with `reportName=RPT_F10_FINANCE_{GINCOME|GBALANCE|GCASHFLOW|MAINFINADATA}`,
//!   `columns=ALL`, `filter=(SECUCODE="600519.SH")`, `sortColumns=REPORT_DATE`,
//!   `sortTypes=-1`, `pageSize=50`, `pageNumber=N`.
//!   600519 returns 103 income / 103 balance / 99 cash-flow / ~100 indicator
//!   periods (~25 years, quarterly + annual). Rows carry `NOTICE_DATE`
//!   (公告日期, needed for point-in-time correctness) and `REPORT_TYPE` ∈
//!   {一季报, 中报, 三季报, 年报}. Nulls and missing keys are common and must
//!   be tolerated. The older `emweb.securities.eastmoney.com/PC_HSF10/NewFinanceAnalysis/{Zcfzb,Lrb,Xjllb}`
//!   family is DEAD (redirects to a soft-block page) — do not use.
//! - **Company survey** — `GET https://emweb.securities.eastmoney.com/PC_HSF10/CompanySurvey/CompanySurveyAjax?code=SH600519`
//!   (code = market prefix + digits). Answers `{jbzl: {gsmc 公司全称, agjc 简称,
//!   sshy 所属行业, sszjhhy 证监会行业, ...}, fxxg: {ssrq 上市日期, clrq 成立日期, ...}}`.
//! - **Dividend history** — `GET https://datacenter-web.eastmoney.com/api/data/v1/get`
//!   with `reportName=RPT_SHAREBONUS_DET`, `filter=(SECURITY_CODE="600519")`,
//!   `sortColumns=EX_DIVIDEND_DATE`, `sortTypes=-1`. Fields include
//!   `IMPL_PLAN_PROFILE` (e.g. "10派280.2423元(含税)"), `PRETAX_BONUS_RMB`
//!   (pre-tax cash per 10 shares), `BONUS_RATIO` (送股), `IT_RATIO` (转增),
//!   `EQUITY_RECORD_DATE`, `EX_DIVIDEND_DATE`. Parsed per-share
//!   [`CorporateAction`]s for the adjustment engine are exposed via
//!   [`EastMoneyF10::corporate_actions`].
//! - **Daily valuation history** — same host, `reportName=RPT_VALUEANALYSIS_DET`,
//!   `filter=(SECURITY_CODE="600519")`, `sortColumns=TRADE_DATE`, `sortTypes=-1`,
//!   `pageSize=500` works (600519: 2096 rows ≈ 8.4 years). Fields: `CLOSE_PRICE`,
//!   `TOTAL_MARKET_CAP`, `TOTAL_SHARES`, `FREE_SHARES_A`, `PE_TTM`, `PE_LAR`
//!   (static, last annual report), `PB_MRQ`, `PCF_OCF_TTM`, `PS_TTM`.
//! - **Valuation snapshot** — push2 `GET /api/qt/stock/get` with
//!   `fields=f43,f57,f58,f84,f85,f116,f117,f162,f163,f164,f167`, verified
//!   against `RPT_VALUEANALYSIS_DET` on the same day: f84=总股本, f85=流通A股,
//!   f116=总市值, f117=流通市值, f162=PE(动), f163=PE(静/LAR), f164=PE(TTM),
//!   f167=PB(MRQ). PS/PCF are NOT available here — take them from the latest
//!   `RPT_VALUEANALYSIS_DET` row instead.

use crate::cache::{ttl, TtlCache};
use crate::http::{HttpClient, EM_TOKEN};
use crate::providers::eastmoney::QUOTE_HOSTS;
use crate::providers::json_f64;
use astock_core::{CorporateAction, DataError, Fetched, Source, Symbol};
use std::sync::Arc;
use std::time::Duration;

/// F10 statement host (datacenter, `securities` path).
const F10_HOST: &str = "https://datacenter.eastmoney.com";
/// Datacenter-web host (dividends, valuation history).
const DC_WEB_HOST: &str = "https://datacenter-web.eastmoney.com";
/// Old-style F10 host still serving CompanySurvey.
const EMWEB_HOST: &str = "https://emweb.securities.eastmoney.com";

/// Fundamental data changes at most a few times per quarter; an hour is a
/// conservative cache TTL. (Under memory pressure the shared cache may evict
/// entries older than `ttl::MAX` — caching here is best-effort only.)
const F10_TTL: Duration = Duration::from_secs(3600);

/// Page sizes verified live: 50 for statement rows (wide `columns=ALL` rows),
/// 500 for the narrow valuation-history rows.
const STATEMENT_PAGE_SIZE: u32 = 50;
const VALUATION_PAGE_SIZE: u32 = 500;

/// Extended quote fields for the valuation snapshot (see module docs).
const SNAPSHOT_FIELDS: &str = "f43,f57,f58,f84,f85,f116,f117,f162,f163,f164,f167";

/// Which F10 report to pull from the datacenter `securities` API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum F10Report {
    /// 利润表 (income statement), cumulative per report period.
    Income,
    /// 资产负债表 (balance sheet), point-in-time per report period.
    Balance,
    /// 现金流量表 (cash flow statement), cumulative per report period.
    CashFlow,
    /// 主要指标 (key indicators: EPS/BPS/ROE/毛利率/净利率/资产负债率/ROIC ...).
    MainIndicators,
}

impl F10Report {
    fn report_name(self) -> &'static str {
        match self {
            F10Report::Income => "RPT_F10_FINANCE_GINCOME",
            F10Report::Balance => "RPT_F10_FINANCE_GBALANCE",
            F10Report::CashFlow => "RPT_F10_FINANCE_GCASHFLOW",
            F10Report::MainIndicators => "RPT_F10_FINANCE_MAINFINADATA",
        }
    }

    fn op(self) -> &'static str {
        match self {
            F10Report::Income => "f10_income",
            F10Report::Balance => "f10_balance",
            F10Report::CashFlow => "f10_cashflow",
            F10Report::MainIndicators => "f10_mainfina",
        }
    }
}

/// EastMoney F10 adapter for fundamental data. Returns raw JSON rows; typed
/// parsing lives in the `astock-fundamental` crate.
pub struct EastMoneyF10 {
    http: Arc<HttpClient>,
    cache: Arc<TtlCache>,
}

/// Parameters of one datacenter-style paged query.
struct DatacenterQuery<'a> {
    host: &'a str,
    report_name: &'a str,
    filter: &'a str,
    sort_columns: &'a str,
    page_size: u32,
    max_pages: u32,
    op: &'static str,
}

impl EastMoneyF10 {
    /// Wrap the shared HTTP client and cache.
    pub fn new(http: Arc<HttpClient>, cache: Arc<TtlCache>) -> Self {
        EastMoneyF10 { http, cache }
    }

    /// Fetch one page from a datacenter-style API and return `(rows, pages)`.
    async fn datacenter_page(
        &self,
        query: &DatacenterQuery<'_>,
        page: u32,
    ) -> Result<(Vec<serde_json::Value>, u32), DataError> {
        let params = vec![
            ("reportName".to_string(), query.report_name.to_string()),
            ("columns".to_string(), "ALL".to_string()),
            ("filter".to_string(), query.filter.to_string()),
            ("sortColumns".to_string(), query.sort_columns.to_string()),
            ("sortTypes".to_string(), "-1".to_string()),
            ("pageSize".to_string(), query.page_size.to_string()),
            ("pageNumber".to_string(), page.to_string()),
        ];
        // The F10 statements live under /securities/api/... on the same host.
        let url = if query.host == F10_HOST {
            format!("{}/securities/api/data/v1/get", query.host)
        } else {
            format!("{}/api/data/v1/get", query.host)
        };
        let value = self.http.get_json(&url, &params).await?;
        let result = value
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if result.is_null() {
            return Err(DataError::Empty(format!("{}: null result", query.op)));
        }
        let pages = result.get("pages").and_then(|p| p.as_u64()).unwrap_or(0) as u32;
        let rows = result
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();
        Ok((rows, pages))
    }

    /// Page through a datacenter API until the data is exhausted or
    /// `query.max_pages` is reached.
    async fn datacenter_rows(
        &self,
        query: &DatacenterQuery<'_>,
    ) -> Result<Vec<serde_json::Value>, DataError> {
        let (first, pages) = self.datacenter_page(query, 1).await?;
        if first.is_empty() {
            return Err(DataError::Empty(format!("{}: no rows", query.op)));
        }
        let mut rows = first;
        let last = pages.min(query.max_pages);
        for page in 2..=last {
            let (mut more, _) = self.datacenter_page(query, page).await?;
            if more.is_empty() {
                break;
            }
            rows.append(&mut more);
        }
        Ok(rows)
    }

    /// Financial statement / key-indicator rows for `secucode`
    /// (e.g. `"600519.SH"`), newest period first.
    ///
    /// `max_pages` caps history at `max_pages * 50` periods; 2 pages already
    /// covers 25 years of quarterly reports for a 1999 lister like 600519.
    pub async fn f10_rows(
        &self,
        secucode: &str,
        report: F10Report,
        max_pages: u32,
    ) -> Result<Fetched<Vec<serde_json::Value>>, DataError> {
        let key = format!("f10_{:?}_{secucode}_{max_pages}", report);
        if let Some(hit) = self
            .cache
            .get::<Fetched<Vec<serde_json::Value>>>(&key, F10_TTL)
        {
            return Ok(hit);
        }
        let filter = format!("(SECUCODE=\"{secucode}\")");
        let rows = self
            .datacenter_rows(&DatacenterQuery {
                host: F10_HOST,
                report_name: report.report_name(),
                filter: &filter,
                sort_columns: "REPORT_DATE",
                page_size: STATEMENT_PAGE_SIZE,
                max_pages,
                op: report.op(),
            })
            .await?;
        let out = Fetched::now(rows, Source::EastMoney);
        self.cache.set(&key, &out);
        Ok(out)
    }

    /// Company survey (`公司概况` + 发行相关): full JSON with `jbzl`/`fxxg`.
    /// `code` is market-prefixed, e.g. `"SH600519"`.
    pub async fn company_survey(
        &self,
        code: &str,
    ) -> Result<Fetched<serde_json::Value>, DataError> {
        let key = format!("f10_survey_{code}");
        if let Some(hit) = self.cache.get::<Fetched<serde_json::Value>>(&key, F10_TTL) {
            return Ok(hit);
        }
        let params = vec![("code".to_string(), code.to_string())];
        let url = format!("{EMWEB_HOST}/PC_HSF10/CompanySurvey/CompanySurveyAjax");
        let value = self.http.get_json(&url, &params).await?;
        if value.get("jbzl").is_none() {
            return Err(DataError::Empty(format!("f10_survey {code}")));
        }
        let out = Fetched::now(value, Source::EastMoney);
        self.cache.set(&key, &out);
        Ok(out)
    }

    /// Dividend/bonus history (`分红送配`), newest ex-dividend date first.
    /// `security_code` is the bare 6-digit code.
    pub async fn bonus_history(
        &self,
        security_code: &str,
        max_pages: u32,
    ) -> Result<Fetched<Vec<serde_json::Value>>, DataError> {
        let key = format!("f10_bonus_{security_code}_{max_pages}");
        if let Some(hit) = self
            .cache
            .get::<Fetched<Vec<serde_json::Value>>>(&key, F10_TTL)
        {
            return Ok(hit);
        }
        let filter = format!("(SECURITY_CODE=\"{security_code}\")");
        let rows = self
            .datacenter_rows(&DatacenterQuery {
                host: DC_WEB_HOST,
                report_name: "RPT_SHAREBONUS_DET",
                filter: &filter,
                sort_columns: "EX_DIVIDEND_DATE",
                page_size: STATEMENT_PAGE_SIZE,
                max_pages,
                op: "f10_bonus",
            })
            .await?;
        let out = Fetched::now(rows, Source::EastMoney);
        self.cache.set(&key, &out);
        Ok(out)
    }

    /// Dividend/bonus history parsed into per-share [`CorporateAction`]s
    /// for the adjustment engine (data-foundation-v2 §数据管线 step 2).
    ///
    /// Field conventions of `RPT_SHAREBONUS_DET` (verified live 2026-08-21
    /// on 600519): `PRETAX_BONUS_RMB` is the pre-tax cash per **10** shares,
    /// `BONUS_RATIO` (送股) and `IT_RATIO` (转增) are shares per **10**
    /// shares, dates are `"YYYY-MM-DD 00:00:00"`. Rows without an
    /// `EX_DIVIDEND_DATE` (announced but not yet implemented) are skipped.
    /// Rights issues (配股) are not covered by this report — `rights_*`
    /// stay zero (see the spec: 配股源待补充).
    pub async fn corporate_actions(
        &self,
        security_code: &str,
        max_pages: u32,
    ) -> Result<Fetched<Vec<CorporateAction>>, DataError> {
        let rows = self.bonus_history(security_code, max_pages).await?;
        Ok(rows.map(|r| r.iter().filter_map(parse_bonus_row).collect()))
    }

    /// Daily valuation history (PE_TTM/PE_LAR/PB_MRQ/PCF_OCF_TTM/PS_TTM,
    /// shares, market cap), newest trade date first. `pageSize=500`, so
    /// `max_pages=5` covers ~2500 trading days (~10 years).
    pub async fn value_analysis(
        &self,
        security_code: &str,
        max_pages: u32,
    ) -> Result<Fetched<Vec<serde_json::Value>>, DataError> {
        let key = format!("f10_value_{security_code}_{max_pages}");
        if let Some(hit) = self
            .cache
            .get::<Fetched<Vec<serde_json::Value>>>(&key, F10_TTL)
        {
            return Ok(hit);
        }
        let filter = format!("(SECURITY_CODE=\"{security_code}\")");
        let rows = self
            .datacenter_rows(&DatacenterQuery {
                host: DC_WEB_HOST,
                report_name: "RPT_VALUEANALYSIS_DET",
                filter: &filter,
                sort_columns: "TRADE_DATE",
                page_size: VALUATION_PAGE_SIZE,
                max_pages,
                op: "f10_value_analysis",
            })
            .await?;
        let out = Fetched::now(rows, Source::EastMoney);
        self.cache.set(&key, &out);
        Ok(out)
    }

    /// Valuation snapshot from the extended quote fields (see module docs for
    /// the f-code meanings). Uses the push2 host pool like the regular quote.
    pub async fn valuation_snapshot(
        &self,
        symbol: &Symbol,
    ) -> Result<Fetched<serde_json::Value>, DataError> {
        let key = format!("f10_snapshot_{symbol}");
        if let Some(hit) = self
            .cache
            .get::<Fetched<serde_json::Value>>(&key, ttl::REALTIME)
        {
            return Ok(hit);
        }
        let params = vec![
            ("secid".to_string(), symbol.secid()),
            ("fields".to_string(), SNAPSHOT_FIELDS.to_string()),
            ("fltt".to_string(), "2".to_string()),
            ("invt".to_string(), "2".to_string()),
            ("ut".to_string(), EM_TOKEN.to_string()),
        ];
        let data = self
            .http
            .get_json_pool("/api/qt/stock/get", &params, &QUOTE_HOSTS, "f10_snapshot")
            .await?;
        let d = data.get("data").cloned().unwrap_or(serde_json::Value::Null);
        if d.is_null() {
            return Err(DataError::Empty(format!("f10_snapshot {symbol}")));
        }
        let out = Fetched::now(d, Source::EastMoney);
        self.cache.set(&key, &out);
        Ok(out)
    }
}

/// Parse one `RPT_SHAREBONUS_DET` row into a per-share [`CorporateAction`].
/// Returns `None` for rows without an ex-date (unimplemented plans).
/// See [`EastMoneyF10::corporate_actions`] for the field conventions.
fn parse_bonus_row(row: &serde_json::Value) -> Option<CorporateAction> {
    let date_field = |key: &str| {
        row.get(key)
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::NaiveDate::parse_from_str(&s[..10.min(s.len())], "%Y-%m-%d").ok())
    };
    let ex_date = date_field("EX_DIVIDEND_DATE")?;
    let num = |key: &str| row.get(key).and_then(json_f64).unwrap_or(0.0);
    Some(CorporateAction {
        ex_date,
        notice_date: date_field("NOTICE_DATE"),
        // Per-10-shares upstream -> per-share engine convention.
        cash_div: num("PRETAX_BONUS_RMB") / 10.0,
        bonus_share: (num("BONUS_RATIO") + num("IT_RATIO")) / 10.0,
        rights_ratio: 0.0,
        rights_price: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_names_are_the_verified_ones() {
        // Guard against typos in the live-verified report names.
        assert_eq!(F10Report::Income.report_name(), "RPT_F10_FINANCE_GINCOME");
        assert_eq!(F10Report::Balance.report_name(), "RPT_F10_FINANCE_GBALANCE");
        assert_eq!(
            F10Report::CashFlow.report_name(),
            "RPT_F10_FINANCE_GCASHFLOW"
        );
        assert_eq!(
            F10Report::MainIndicators.report_name(),
            "RPT_F10_FINANCE_MAINFINADATA"
        );
    }

    #[test]
    fn parses_bonus_row_per_share_conventions() {
        // Shape verified live on 2026-08-21 (600519, 10派280.2423元).
        let row = serde_json::json!({
            "EX_DIVIDEND_DATE": "2026-06-26 00:00:00",
            "NOTICE_DATE": "2026-06-22 00:00:00",
            "PRETAX_BONUS_RMB": 280.2423,
            "BONUS_RATIO": null,
            "IT_RATIO": null,
        });
        let action = parse_bonus_row(&row).unwrap();
        assert_eq!(action.ex_date.to_string(), "2026-06-26");
        assert_eq!(action.notice_date.unwrap().to_string(), "2026-06-22");
        assert!((action.cash_div - 28.02423).abs() < 1e-9);
        assert_eq!(action.bonus_share, 0.0);

        // 送转 row: 10送8转12 -> bonus_share = (8 + 12)/10 = 2.0.
        let row = serde_json::json!({
            "EX_DIVIDEND_DATE": "2025-07-29 00:00:00",
            "NOTICE_DATE": null,
            "PRETAX_BONUS_RMB": 39.74,
            "BONUS_RATIO": 8.0,
            "IT_RATIO": 12.0,
        });
        let action = parse_bonus_row(&row).unwrap();
        assert!((action.bonus_share - 2.0).abs() < 1e-9);
        assert!((action.cash_div - 3.974).abs() < 1e-9);
        assert!(action.notice_date.is_none());

        // Announced but not yet implemented: no ex-date -> skipped.
        let row = serde_json::json!({
            "EX_DIVIDEND_DATE": null,
            "PRETAX_BONUS_RMB": 100.0,
        });
        assert!(parse_bonus_row(&row).is_none());
    }
}
