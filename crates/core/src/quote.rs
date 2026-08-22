//! Realtime quote and intraday minute (分时) models.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

/// A realtime quote snapshot (EastMoney `stock/get` shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    /// Bare 6-digit code.
    pub symbol: String,
    /// Display name from the upstream.
    pub name: String,
    /// Latest price (fltt=2, no ÷100).
    pub price: f64,
    /// Today's open.
    pub open: f64,
    /// Today's high.
    pub high: f64,
    /// Today's low.
    pub low: f64,
    /// Previous close.
    pub pre_close: f64,
    /// Volume in lots (手).
    pub volume: f64,
    /// Turnover amount in CNY.
    pub amount: f64,
    /// Absolute change vs. pre_close.
    pub change: f64,
    /// Percent change vs. pre_close.
    pub pct: f64,
    /// Turnover rate in percent.
    pub turnover: f64,
    /// When this snapshot was fetched.
    pub timestamp: DateTime<Utc>,
}

/// One point of an intraday minute series (分时).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MinutePoint {
    /// Intraday timestamp (China time, naive).
    pub time: NaiveDateTime,
    /// Price at this minute.
    pub price: f64,
    /// Session volume-weighted average price.
    pub avg_price: f64,
    /// Volume traded during this minute (lots).
    pub volume: f64,
}

/// A full intraday minute series plus session metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MinuteData {
    /// Minute points in chronological order.
    pub points: Vec<MinutePoint>,
    /// Previous close, from the upstream payload.
    pub pre_close: f64,
    /// Instrument name, from the upstream payload.
    pub name: String,
    /// Session high derived from the points (0 when no positive prices).
    pub high: f64,
    /// Session low derived from the points (0 when no positive prices).
    pub low: f64,
}
