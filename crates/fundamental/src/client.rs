//! Fetch layer: assembles the normalized [`FundamentalBundle`] from the
//! EastMoney F10 provider ([`EastMoneyF10`]).
//!
//! Every sub-fetch keeps its own provenance (`Fetched`); the combined
//! [`bundle`] call is failure-tolerant per section — a failed section leaves
//! its part of the bundle empty/`None` and is reported in
//! [`BundleOutcome::failures`] instead of failing the whole pipeline.

use crate::model::{
    BalanceSheet, CashFlowStatement, CompanyProfile, DividendRecord, FundamentalBundle,
    IncomeStatement, KeyIndicators, ValuationPoint, ValuationSnapshot,
};
use crate::parse;
use astock_core::{DataError, Fetched, Symbol};
use astock_market_data::{EastMoneyF10, F10Report};
use std::sync::Arc;

/// Default page caps (statements: 2×50 = 100 periods ≈ 25y; valuation
/// history: 5×500 = 2500 trading days ≈ 10y).
pub const STATEMENT_PAGES: u32 = 2;
/// Bonus history pages (50 rows each; 600519 has ~30 records).
pub const BONUS_PAGES: u32 = 1;
/// Valuation-history pages.
pub const VALUATION_PAGES: u32 = 5;

/// Fundamental-data client for one shared F10 provider.
pub struct FundamentalClient {
    f10: Arc<EastMoneyF10>,
}

/// Result of a full bundle fetch: the bundle plus per-section failures.
#[derive(Debug, Clone)]
pub struct BundleOutcome {
    /// The assembled data; failed sections are empty/`None` inside.
    pub bundle: FundamentalBundle,
    /// `"section: error"` strings for sections that failed.
    pub failures: Vec<String>,
}

impl FundamentalClient {
    /// Wrap a shared F10 provider.
    pub fn new(f10: Arc<EastMoneyF10>) -> Self {
        FundamentalClient { f10 }
    }

    /// EastMoney `SECUCODE`, e.g. `"600519.SH"`. Note: the `.BJ` suffix for
    /// Beijing symbols follows the same convention but was NOT live-verified.
    pub fn secucode(symbol: &Symbol) -> String {
        format!("{}.{}", symbol.code(), symbol.market())
    }

    /// Market-prefixed code for the old F10 endpoints, e.g. `"SH600519"`.
    fn survey_code(symbol: &Symbol) -> String {
        format!("{}{}", symbol.market(), symbol.code())
    }

    /// Income statements (oldest first).
    pub async fn income_statements(
        &self,
        symbol: &Symbol,
    ) -> Result<Fetched<Vec<IncomeStatement>>, DataError> {
        let rows = self
            .f10
            .f10_rows(&Self::secucode(symbol), F10Report::Income, STATEMENT_PAGES)
            .await?;
        Ok(rows.map(|r| parse::parse_income(&r)))
    }

    /// Balance sheets (oldest first).
    pub async fn balance_sheets(
        &self,
        symbol: &Symbol,
    ) -> Result<Fetched<Vec<BalanceSheet>>, DataError> {
        let rows = self
            .f10
            .f10_rows(&Self::secucode(symbol), F10Report::Balance, STATEMENT_PAGES)
            .await?;
        Ok(rows.map(|r| parse::parse_balance(&r)))
    }

    /// Cash flow statements (oldest first).
    pub async fn cashflow_statements(
        &self,
        symbol: &Symbol,
    ) -> Result<Fetched<Vec<CashFlowStatement>>, DataError> {
        let rows = self
            .f10
            .f10_rows(
                &Self::secucode(symbol),
                F10Report::CashFlow,
                STATEMENT_PAGES,
            )
            .await?;
        Ok(rows.map(|r| parse::parse_cashflow(&r)))
    }

    /// Key indicators (oldest first).
    pub async fn key_indicators(
        &self,
        symbol: &Symbol,
    ) -> Result<Fetched<Vec<KeyIndicators>>, DataError> {
        let rows = self
            .f10
            .f10_rows(
                &Self::secucode(symbol),
                F10Report::MainIndicators,
                STATEMENT_PAGES,
            )
            .await?;
        Ok(rows.map(|r| parse::parse_indicators(&r)))
    }

    /// Company profile: survey fields plus share counts from the quote
    /// snapshot (share counts fall back to `None` when the quote pool fails).
    pub async fn profile(&self, symbol: &Symbol) -> Result<Fetched<CompanyProfile>, DataError> {
        let survey = self.f10.company_survey(&Self::survey_code(symbol)).await?;
        let mut profile = parse::parse_survey(&survey.data, symbol.code());
        if let Ok(snapshot) = self.f10.valuation_snapshot(symbol).await {
            let snap = parse::parse_snapshot(&snapshot.data);
            profile.total_shares = snap.total_shares;
            profile.float_shares = snap.float_shares;
        }
        Ok(survey.map(|_| profile))
    }

    /// Current valuation snapshot (PE_TTM/PE_static/PE_dynamic/PB, shares,
    /// market caps).
    pub async fn snapshot(&self, symbol: &Symbol) -> Result<Fetched<ValuationSnapshot>, DataError> {
        let data = self.f10.valuation_snapshot(symbol).await?;
        Ok(data.map(|d| parse::parse_snapshot(&d)))
    }

    /// Dividend/bonus history (oldest ex-date first).
    pub async fn dividends(
        &self,
        symbol: &Symbol,
    ) -> Result<Fetched<Vec<DividendRecord>>, DataError> {
        let rows = self.f10.bonus_history(symbol.code(), BONUS_PAGES).await?;
        Ok(rows.map(|r| parse::parse_dividends(&r)))
    }

    /// Daily valuation history (oldest first).
    pub async fn valuation_history(
        &self,
        symbol: &Symbol,
    ) -> Result<Fetched<Vec<ValuationPoint>>, DataError> {
        let rows = self
            .f10
            .value_analysis(symbol.code(), VALUATION_PAGES)
            .await?;
        Ok(rows.map(|r| parse::parse_valuation_history(&r)))
    }

    /// Full pipeline fetch. Sections are fetched concurrently; each failing
    /// section is recorded in `failures` and left empty — the bundle never
    /// contains fabricated data.
    pub async fn bundle(&self, symbol: &Symbol) -> BundleOutcome {
        let (income, balance, cashflow, indicators, profile, snapshot, dividends, history) = tokio::join!(
            self.income_statements(symbol),
            self.balance_sheets(symbol),
            self.cashflow_statements(symbol),
            self.key_indicators(symbol),
            self.profile(symbol),
            self.snapshot(symbol),
            self.dividends(symbol),
            self.valuation_history(symbol),
        );
        let mut failures = Vec::new();
        fn or_empty<T>(
            name: &str,
            r: Result<Fetched<Vec<T>>, DataError>,
            failures: &mut Vec<String>,
        ) -> Vec<T> {
            match r {
                Ok(f) => f.data,
                Err(e) => {
                    failures.push(format!("{name}: {e}"));
                    Vec::new()
                }
            }
        }
        let bundle = FundamentalBundle {
            income: or_empty("income", income, &mut failures),
            balance: or_empty("balance", balance, &mut failures),
            cashflow: or_empty("cashflow", cashflow, &mut failures),
            indicators: or_empty("indicators", indicators, &mut failures),
            profile: profile
                .map_err(|e| failures.push(format!("profile: {e}")))
                .ok()
                .map(|f| f.data),
            snapshot: snapshot
                .map_err(|e| failures.push(format!("snapshot: {e}")))
                .ok()
                .map(|f| f.data),
            dividends: or_empty("dividends", dividends, &mut failures),
            valuation_history: or_empty("valuation_history", history, &mut failures),
        };
        BundleOutcome { bundle, failures }
    }
}
