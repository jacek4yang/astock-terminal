//! Price input model for the backtester.
//!
//! The crate deliberately defines its own minimal [`Bar`] instead of reusing
//! `astock_core::Bar`: backtests need a `suspended` flag that the market-data
//! model does not carry, and keeping the input type local keeps the crate
//! free of adapter concerns. A lossless conversion from `astock_core::Bar`
//! is provided for the app layer.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// One daily OHLCV bar, backtester-local.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    /// Trading date.
    pub date: NaiveDate,
    /// Opening price.
    pub open: f64,
    /// Closing price.
    pub close: f64,
    /// Highest price.
    pub high: f64,
    /// Lowest price.
    pub low: f64,
    /// Volume in shares.
    pub volume: f64,
    /// Turnover amount in CNY, when known.
    pub amount: Option<f64>,
    /// Whether the stock was suspended (停牌) on this date.
    ///
    /// Suspended bars carry a stale indicative price; no order can fill on
    /// them and the engine marks positions to the last non-suspended close.
    pub suspended: bool,
}

impl Bar {
    /// Full constructor.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        date: NaiveDate,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: f64,
        amount: Option<f64>,
        suspended: bool,
    ) -> Self {
        Bar {
            date,
            open,
            close,
            high,
            low,
            volume,
            amount,
            suspended,
        }
    }

    /// A non-suspended bar with O=H=L=C=`price`; convenience for tests and
    /// synthetic series.
    pub fn flat(date: NaiveDate, price: f64) -> Self {
        Bar::new(date, price, price, price, price, 0.0, None, false)
    }
}

impl From<&astock_core::Bar> for Bar {
    /// Converts a market-data bar; `suspended` defaults to `false` and the
    /// volume unit is normalized to shares (1 lot = 100 shares).
    fn from(b: &astock_core::Bar) -> Self {
        let shares = match b.volume_unit {
            astock_core::VolumeUnit::Lots => b.volume * 100.0,
            astock_core::VolumeUnit::FundUnits => b.volume,
        };
        Bar {
            date: b.date,
            open: b.open,
            close: b.close,
            high: b.high,
            low: b.low,
            volume: shares,
            amount: b.amount,
            suspended: false,
        }
    }
}

/// A date-ordered price history for one symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceSeries {
    /// Bare or decorated symbol, e.g. "600519" or "600519.SH".
    pub symbol: String,
    /// Bars sorted by strictly increasing date.
    pub bars: Vec<Bar>,
}

impl PriceSeries {
    /// Build and validate a series (non-empty, strictly increasing dates).
    pub fn new(symbol: impl Into<String>, bars: Vec<Bar>) -> Result<Self> {
        let symbol = symbol.into();
        if bars.is_empty() {
            return Err(Error::EmptySeries { symbol });
        }
        for (i, w) in bars.windows(2).enumerate() {
            if w[1].date <= w[0].date {
                return Err(Error::UnsortedDates {
                    symbol,
                    index: i + 1,
                });
            }
        }
        Ok(PriceSeries { symbol, bars })
    }

    /// Number of bars.
    pub fn len(&self) -> usize {
        self.bars.len()
    }

    /// Whether the series is empty (always false after [`Self::new`]).
    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }

    /// A sub-series covering bar indices `[start, end)` — used by the
    /// walk-forward harness to cut train/test windows.
    pub fn slice(&self, start: usize, end: usize) -> Result<Self> {
        PriceSeries::new(self.symbol.clone(), self.bars[start..end].to_vec())
    }
}

/// Everything the backtester needs from the outside world for one study:
/// the instruments to trade plus an optional benchmark for relative metrics.
///
/// The crate performs no I/O — the app layer builds this from storage or
/// market-data.
#[derive(Debug, Clone, Default)]
pub struct BacktestUniverse {
    /// Tradable series, keyed by insertion order.
    pub series: Vec<PriceSeries>,
    /// Optional benchmark (e.g. an index) for alpha/beta/information ratio.
    pub benchmark: Option<PriceSeries>,
}

impl BacktestUniverse {
    /// Universe with a single instrument and no benchmark.
    pub fn single(series: PriceSeries) -> Self {
        BacktestUniverse {
            series: vec![series],
            benchmark: None,
        }
    }

    /// Attach a benchmark series.
    pub fn with_benchmark(mut self, benchmark: PriceSeries) -> Self {
        self.benchmark = Some(benchmark);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn rejects_empty_and_unsorted() {
        assert!(matches!(
            PriceSeries::new("600519", vec![]),
            Err(Error::EmptySeries { .. })
        ));
        let bars = vec![
            Bar::flat(d("2025-01-07"), 10.0),
            Bar::flat(d("2025-01-06"), 10.0),
        ];
        assert!(matches!(
            PriceSeries::new("600519", bars),
            Err(Error::UnsortedDates { index: 1, .. })
        ));
    }

    #[test]
    fn core_bar_conversion_normalizes_lots_to_shares() {
        let core = astock_core::Bar::new(
            d("2025-01-06"),
            10.0,
            10.5,
            9.5,
            10.2,
            1234.0,
            astock_core::VolumeUnit::Lots,
        );
        let bar = Bar::from(&core);
        assert_eq!(bar.volume, 123_400.0);
        assert!(!bar.suspended);
    }
}
