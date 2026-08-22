//! Input data model mirroring the legacy Python dataclasses
//! (`data/kline_fetcher.py`: `Kline`, `Quote`, `FundFlow`) plus the market
//! breadth snapshot used by `app.py`'s post-processing.

use serde::{Deserialize, Serialize};

/// One OHLCV bar. Field names match the legacy Python `Kline` dataclass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kline {
    pub date: String,
    pub open: f64,
    pub close: f64,
    pub high: f64,
    pub low: f64,
    /// Volume in lots (手).
    pub volume: f64,
    /// Turnover amount in yuan.
    #[serde(default)]
    pub amount: f64,
    /// Daily percent change (%).
    #[serde(default)]
    pub pct: f64,
    /// Turnover rate (%).
    #[serde(default)]
    pub turnover: f64,
}

/// Realtime quote snapshot. Field names match the legacy `Quote` dataclass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quote {
    pub symbol: String,
    pub name: String,
    pub price: f64,
    pub pct: f64,
    pub change: f64,
    pub high: f64,
    pub low: f64,
    pub open: f64,
    pub pre_close: f64,
    /// Volume in lots (手).
    pub volume: f64,
    /// Turnover amount in yuan.
    pub amount: f64,
    /// Turnover rate (%).
    pub turnover: f64,
    #[serde(default)]
    pub timestamp: String,
}

/// One day of fund-flow data. Field names match the legacy `FundFlow`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundFlow {
    pub date: String,
    pub main_net: f64,
    pub super_large_net: f64,
    pub large_net: f64,
    pub medium_net: f64,
    pub small_net: f64,
    #[serde(default)]
    pub main_pct: f64,
}

/// Market breadth snapshot (advance/decline counts) from
/// `fetch_market_breadth`. Used for the CANSLIM M-score adjustment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Breadth {
    pub up: i64,
    pub down: i64,
    pub flat: i64,
    pub total: i64,
    pub breadth_ratio: f64,
}
