//! Search results, market breadth, and full-market list models.

use serde::{Deserialize, Serialize};

/// One hit from the EastMoney suggest endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    /// Bare 6-digit code.
    pub code: String,
    /// Display name (may be empty for the numeric short-circuit path).
    pub name: String,
    /// Upstream classification, e.g. `"AStock"` / `"Fund"` / `"SH"`-style
    /// market label for the short-circuit path.
    pub classify: String,
}

/// Market-wide advance/decline counts (市场宽度).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketBreadth {
    /// Number of stocks up on the day.
    pub up: u32,
    /// Number of stocks down on the day.
    pub down: u32,
    /// Number of stocks flat (or with unparseable pct).
    pub flat: u32,
    /// Total stocks counted (`up + down + flat`).
    pub total: u32,
}

impl MarketBreadth {
    /// `up / (up + down)`, or 0.5 when nothing moved, as in the legacy code.
    pub fn ratio(&self) -> f64 {
        let moved = self.up + self.down;
        if moved > 0 {
            self.up as f64 / moved as f64
        } else {
            0.5
        }
    }
}

/// One row of the full A-share list used by the scanner pre-filter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StockListItem {
    /// Bare 6-digit code.
    pub code: String,
    /// Display name.
    pub name: String,
    /// Latest price.
    pub price: Option<f64>,
    /// Percent change on the day.
    pub pct: Option<f64>,
    /// Turnover amount in CNY.
    pub amount: Option<f64>,
}
