//! A-share backtesting engine: event-driven daily bars, trading-rules aware,
//! with an anti-overfitting validation harness.
//!
//! Design goals:
//!
//! - **Realistic trading constraints are first-class.** T+1 resale rules,
//!   board lot sizes (100-share lots; STAR 200+1), daily price limits,
//!   suspensions, and the versioned fee schedule (commission / stamp tax /
//!   transfer fee) all come from `astock-trading-rules` and are enforced at
//!   fill time, not approximated afterwards.
//! - **No look-ahead, structurally.** A [`strategy::Strategy`] only ever sees
//!   a slice of the price history ending at the current bar
//!   ([`strategy::StrategyContext`]). Future bars are not merely hidden by
//!   convention — they are not present in the context at all. A debug
//!   assertion in [`strategy::StrategyContext::bar`] additionally traps any
//!   absolute-index access beyond the current bar in debug/test builds.
//! - **Deterministic.** The engine itself uses no randomness; the validation
//!   harness uses only seeded RNGs. Same inputs produce byte-identical trade
//!   logs (covered by tests).
//!
//! The crate does no I/O: callers (the app layer) feed it [`data::PriceSeries`]
//! built from storage / market-data, plus a [`astock_trading_rules::RuleSet`].
//!
//! # Quick start
//! ```
//! use astock_backtest::{data::PriceSeries, engine::{BacktestEngine, EngineConfig}, strategy::BuyHold};
//! use astock_trading_rules::RuleSet;
//!
//! let rules = RuleSet::load(None).unwrap();
//! let series = PriceSeries::new("600519", vec![
//!     astock_backtest::data::Bar::flat("2025-01-06".parse().unwrap(), 10.0),
//!     astock_backtest::data::Bar::flat("2025-01-07".parse().unwrap(), 10.5),
//! ]).unwrap();
//! let engine = BacktestEngine::new(rules, EngineConfig::new("600519", 100_000.0)).unwrap();
//! let result = engine.run(&series, &mut BuyHold).unwrap();
//! assert_eq!(result.trades.len(), 1);
//! ```

pub mod data;
pub mod engine;
pub mod exit;
pub mod metrics;
pub mod news_event;
pub mod strategies;
pub mod strategy;
pub mod validation;

use thiserror::Error;

/// Crate-wide error type.
#[derive(Debug, Error)]
pub enum Error {
    /// A price series with no bars cannot be backtested.
    #[error("price series for {symbol} is empty")]
    EmptySeries {
        /// Symbol of the offending series.
        symbol: String,
    },
    /// Bars must be sorted by strictly increasing date.
    #[error("price series for {symbol} has unsorted or duplicate dates at index {index}")]
    UnsortedDates {
        /// Symbol of the offending series.
        symbol: String,
        /// Index of the first out-of-order bar.
        index: usize,
    },
    /// Initial cash must be positive.
    #[error("initial cash must be positive, got {0}")]
    NonPositiveCash(f64),
    /// The symbol could not be classified into a board by the rules.
    #[error(transparent)]
    Rules(#[from] astock_trading_rules::Error),
}

/// Convenience result alias.
pub type Result<T> = std::result::Result<T, Error>;
