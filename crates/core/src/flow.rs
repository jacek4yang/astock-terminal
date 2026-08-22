//! Fund-flow (资金流) models.
//!
//! EastMoney CSV column order for both daily and minute flows is:
//! `date, main, small, medium, large, super_large, main_pct` — note small
//! comes before medium, which is easy to get wrong.

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

/// Net fund inflow for one period (a day, or cumulative-to-minute intraday).
///
/// All values are in CNY; positive means net inflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FundFlowPoint {
    /// Trading date (midnight) for daily flows, or intraday timestamp for
    /// minute-level cumulative flows. Naive; China timezone by convention.
    pub time: NaiveDateTime,
    /// Main-force (主力) net inflow.
    pub main_net: f64,
    /// Small-order (小单) net inflow.
    pub small_net: f64,
    /// Medium-order (中单) net inflow.
    pub medium_net: f64,
    /// Large-order (大单) net inflow.
    pub large_net: f64,
    /// Super-large-order (超大单) net inflow.
    pub super_large_net: f64,
    /// Main-force net inflow as a percent of turnover (daily rows only;
    /// minute rows leave this at 0, mirroring the legacy behaviour).
    pub main_pct: f64,
}
