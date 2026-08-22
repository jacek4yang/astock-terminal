//! Serde data model for the versioned A-share trading rules file.
//!
//! Everything in this module is plain data deserialized from
//! `rules/a-share-rules.json` (or a remote-updated override file). No trading
//! policy is hardcoded; the structs here only describe the shape of the data.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Top-level rules document. Carries provenance (`version`, `effective_date`,
/// `source_url`) so callers can tell which policy snapshot they are using.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSetData {
    /// Rule file version, e.g. "2025.1".
    pub version: String,
    /// Date from which this snapshot of the rules applies.
    pub effective_date: NaiveDate,
    /// Where the rules were sourced from (official exchange pages).
    pub source_url: String,
    /// Free-form notes about the file.
    #[serde(default)]
    pub notes: Option<String>,
    /// Per-board trading rules; symbol classification matches by longest prefix.
    pub boards: Vec<BoardRule>,
    /// Call auction / continuous trading windows.
    pub auction: AuctionWindows,
    /// Fee schedule; multiple entries per kind with different `effective_date`
    /// encode policy history (the latest entry effective at the trade date wins).
    pub fees: Vec<FeeRule>,
    /// Trading calendar rules and holiday list.
    pub calendar: CalendarRules,
}

/// Trading rules for one board (主板/创业板/科创板/北交所).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardRule {
    /// Stable machine id, e.g. "sh_main".
    pub id: String,
    /// Human-readable name, e.g. "沪市主板".
    pub name: String,
    /// Exchange market code: "SH" | "SZ" | "BJ".
    pub market: String,
    /// Symbol prefixes belonging to this board (e.g. ["688", "689"]).
    /// Classification uses the longest matching prefix across all boards.
    pub prefixes: Vec<String>,
    /// Regular daily price limit as a fraction (0.10 = 10%).
    pub price_limit_pct: f64,
    /// Daily price limit for ST/*ST names as a fraction.
    pub st_price_limit_pct: f64,
    /// Number of days after IPO with no price limit (5 for SH/SZ boards under
    /// registration-based IPO, 1 for BSE).
    pub ipo_no_limit_days: u32,
    /// Minimum order size in shares (100; 200 for STAR).
    pub min_lot: u32,
    /// Increment above `min_lot` in shares (100 for most boards; 1 for STAR/BSE).
    pub lot_step: u32,
    /// Whether T+1 applies (true for all current A-share boards).
    pub t_plus_1: bool,
}

/// All intraday session windows as configured data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuctionWindows {
    /// Opening call auction, with the no-cancel sub-window start.
    pub open_call_auction: CallAuctionWindow,
    /// Morning continuous session.
    pub continuous_morning: Window,
    /// Afternoon continuous session (ends when the close auction starts).
    pub continuous_afternoon: Window,
    /// Closing call auction (unified for SH/SZ since 2018; orders not cancellable).
    pub close_call_auction: Window,
    /// Free-form notes.
    #[serde(default)]
    pub notes: Option<String>,
}

/// A simple intraday window, "HH:MM" inclusive-start / exclusive-end.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    /// Start time, "HH:MM".
    pub start: String,
    /// End time, "HH:MM".
    pub end: String,
}

/// Call auction window with an optional no-cancel sub-window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallAuctionWindow {
    /// Start time, "HH:MM".
    pub start: String,
    /// End time, "HH:MM".
    pub end: String,
    /// From this time orders can no longer be cancelled (e.g. 09:20).
    pub no_cancel_from: String,
}

/// One fee schedule entry. History is encoded by stacking entries of the same
/// `kind` with different `effective_date` values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeRule {
    /// Fee category.
    pub kind: FeeKind,
    /// Rate as a fraction of trade amount (0.0005 = 0.05%).
    pub rate: f64,
    /// Which trade side the fee applies to.
    pub side: FeeSide,
    /// Optional minimum fee in CNY (commissions typically have one).
    #[serde(default)]
    pub min_fee: Option<f64>,
    /// Markets the fee applies to ("SH"/"SZ"/"BJ"); empty means all markets.
    #[serde(default)]
    pub markets: Vec<String>,
    /// Date from which this rate is effective.
    pub effective_date: NaiveDate,
    /// Free-form note (provenance for policy changes).
    #[serde(default)]
    pub note: Option<String>,
}

/// Fee category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeeKind {
    /// Broker commission.
    Commission,
    /// Stamp tax (sell side only under current rules).
    StampTax,
    /// ChinaClear transfer fee.
    TransferFee,
}

/// Which trade side a fee applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeeSide {
    /// Buys only.
    Buy,
    /// Sells only.
    Sell,
    /// Both sides.
    Both,
}

impl FeeSide {
    /// Whether this fee side covers the given trade side.
    pub fn covers(self, side: TradeSide) -> bool {
        match self {
            FeeSide::Both => true,
            FeeSide::Buy => side == TradeSide::Buy,
            FeeSide::Sell => side == TradeSide::Sell,
        }
    }
}

/// Trading calendar rules: weekend handling plus an explicit holiday list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarRules {
    /// Whether Saturdays and Sundays are closed (always true for A-shares;
    /// kept as data in case of extraordinary exchange announcements).
    pub weekend_closed: bool,
    /// Weekday closures (public holidays). Only weekdays need listing;
    /// weekend dates in the list are harmless.
    #[serde(default)]
    pub holidays: Vec<NaiveDate>,
    /// Free-form note.
    #[serde(default)]
    pub note: Option<String>,
}

/// Trade direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeSide {
    /// Buy.
    Buy,
    /// Sell.
    Sell,
}

/// Intraday trading phase derived from the configured auction windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuctionPhase {
    /// Before the opening call auction (or any non-trading gap).
    Closed,
    /// Opening call auction, orders cancellable (09:15-09:20).
    OpenAuctionCancellable,
    /// Opening call auction, orders NOT cancellable (09:20-09:25).
    OpenAuctionNoCancel,
    /// Morning continuous trading (09:30-11:30).
    ContinuousMorning,
    /// Midday break (11:30-13:00).
    LunchBreak,
    /// Afternoon continuous trading (13:00-14:57).
    ContinuousAfternoon,
    /// Closing call auction, orders NOT cancellable (14:57-15:00).
    ClosingAuction,
}

/// Per-board rules resolved for a concrete symbol (owned snapshot).
#[derive(Debug, Clone)]
pub struct BoardRules {
    /// Stable board id, e.g. "sh_main".
    pub board_id: String,
    /// Human-readable board name.
    pub board_name: String,
    /// Exchange market code: "SH" | "SZ" | "BJ".
    pub market: String,
    /// Minimum order size in shares.
    pub min_lot: u32,
    /// Increment above `min_lot` in shares.
    pub lot_step: u32,
    /// Whether T+1 applies.
    pub t_plus_1: bool,
    /// Days after IPO with no price limit.
    pub ipo_no_limit_days: u32,
    price_limit_pct: f64,
    st_price_limit_pct: f64,
}

impl BoardRules {
    /// Daily price limit as a fraction, accounting for ST/*ST status.
    pub fn price_limit_pct(&self, is_st: bool) -> f64 {
        if is_st {
            self.st_price_limit_pct
        } else {
            self.price_limit_pct
        }
    }

    /// Limit-up price for the given previous close, rounded to 2 decimals.
    pub fn limit_up_price(&self, prev_close: f64, is_st: bool) -> f64 {
        round2(prev_close * (1.0 + self.price_limit_pct(is_st)))
    }

    /// Limit-down price for the given previous close, rounded to 2 decimals.
    pub fn limit_down_price(&self, prev_close: f64, is_st: bool) -> f64 {
        round2(prev_close * (1.0 - self.price_limit_pct(is_st)))
    }

    /// Whether `shares` is a valid buy order quantity on this board.
    pub fn is_valid_lot(&self, shares: u32) -> bool {
        shares >= self.min_lot && (shares - self.min_lot).is_multiple_of(self.lot_step)
    }

    pub(crate) fn from_rule(rule: &BoardRule) -> Self {
        BoardRules {
            board_id: rule.id.clone(),
            board_name: rule.name.clone(),
            market: rule.market.clone(),
            min_lot: rule.min_lot,
            lot_step: rule.lot_step,
            t_plus_1: rule.t_plus_1,
            ipo_no_limit_days: rule.ipo_no_limit_days,
            price_limit_pct: rule.price_limit_pct,
            st_price_limit_pct: rule.st_price_limit_pct,
        }
    }
}

/// Round to 2 decimal places (tick size 0.01 CNY for A-shares).
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Fee breakdown for a single trade, in CNY.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TradeCost {
    /// Broker commission (after min-fee floor).
    pub commission: f64,
    /// Stamp tax (sell side only).
    pub stamp_tax: f64,
    /// Transfer fee (market-dependent; zero if the market is unknown).
    pub transfer_fee: f64,
    /// Sum of all components.
    pub total: f64,
}
