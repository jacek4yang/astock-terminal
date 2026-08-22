//! OHLCV bar model with explicit volume unit tracking.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Unit the `volume` field of a [`Bar`] is expressed in.
///
/// A-share klines report volume in lots (手, 100 shares); ETF/fund klines
/// report it in fund units (份). Upstreams differ (Sina reports raw shares for
/// A-shares, which adapters convert to lots), so the unit travels with the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VolumeUnit {
    /// Lots (手) — 1 lot = 100 shares. Used for A-share stocks.
    Lots,
    /// Fund units (份). Used for ETFs / LOFs / closed-end funds.
    FundUnits,
}

/// One OHLCV kline bar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    /// Trading date of the bar (bar-start date for weekly/monthly aggregates).
    pub date: NaiveDate,
    /// Opening price.
    pub open: f64,
    /// Closing price.
    pub close: f64,
    /// Highest price.
    pub high: f64,
    /// Lowest price.
    pub low: f64,
    /// Volume in [`Self::volume_unit`].
    pub volume: f64,
    /// Unit of [`Self::volume`].
    pub volume_unit: VolumeUnit,
    /// Turnover amount in CNY, when the upstream provides it.
    pub amount: Option<f64>,
    /// Turnover rate in percent, when the upstream provides it.
    pub turnover: Option<f64>,
    /// Percent change vs. the previous bar's close, computed by the adapter.
    pub pct: Option<f64>,
}

impl Bar {
    /// Convenience constructor; optional fields default to `None`.
    pub fn new(
        date: NaiveDate,
        open: f64,
        close: f64,
        high: f64,
        low: f64,
        volume: f64,
        volume_unit: VolumeUnit,
    ) -> Self {
        Bar {
            date,
            open,
            close,
            high,
            low,
            volume,
            volume_unit,
            amount: None,
            turnover: None,
            pct: None,
        }
    }

    /// Structural sanity check, ported from the legacy validation filter:
    /// positive O/H/L/C, H ≥ max(O,C,L), L ≤ min(O,C), and close < 10000
    /// (prices at or above 10000 are treated as dirty data).
    pub fn is_valid(&self) -> bool {
        self.is_valid_with_ceiling(10000.0)
    }

    /// Sanity check with a custom dirty-data ceiling. Index levels routinely
    /// exceed 10000 (e.g. 深证成指 ≈ 14000), so index series must use a much
    /// higher ceiling than individual securities.
    pub fn is_valid_with_ceiling(&self, max_close: f64) -> bool {
        self.open > 0.0
            && self.high > 0.0
            && self.low > 0.0
            && self.close > 0.0
            && self.high >= self.low
            && self.high >= self.close
            && self.high >= self.open
            && self.low <= self.close
            && self.low <= self.open
            && self.close < max_close
    }

    /// Sanity check for index series (ceiling 1,000,000).
    pub fn is_valid_index(&self) -> bool {
        self.is_valid_with_ceiling(1_000_000.0)
    }
}
