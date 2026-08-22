//! Fundamental analysis for A-shares: free EastMoney F10 data acquisition
//! (statements, key indicators, company profile, dividends, valuation
//! snapshots & history) plus pure-function analytics (growth, margins,
//! returns on capital, DuPont, Piotroski/Altman/Beneish scores, red flags,
//! multiples/percentile/PEG/DCF valuation).
//!
//! Data sources and exact endpoint parameters are documented in
//! `astock_market_data::providers::eastmoney_f10`. Missing upstream data is
//! represented as `Option::None` everywhere — never fabricated.

pub mod anomaly;
pub mod client;
pub mod metrics;
pub mod model;
pub mod parse;
pub mod scores;
pub mod valuation;

pub use client::{BundleOutcome, FundamentalClient};
pub use model::{
    BalanceSheet, CashFlowStatement, CompanyProfile, DividendRecord, FundamentalBundle,
    IncomeStatement, KeyIndicators, PeriodMeta, ReportType, ValuationPoint, ValuationSnapshot,
};
