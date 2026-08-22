//! Dynamic, versioned A-share trading rules.
//!
//! All trading policy (price limits, lot sizes, T+1, call auction windows,
//! fees, trading calendar) lives in `rules/a-share-rules.json` — embedded at
//! compile time and overridable by a file in the app config dir or an explicit
//! path — so policy changes are config edits, not code changes.
//!
//! # Quick start
//! ```
//! use astock_trading_rules::RuleSet;
//! let rules = RuleSet::load(None).unwrap();
//! let board = rules.for_symbol("600519").unwrap();
//! assert_eq!(board.price_limit_pct(false), 0.10);
//! ```

pub mod exit;

mod error;
mod model;

pub use error::{Error, Result};
pub use model::{
    AuctionPhase, AuctionWindows, BoardRule, BoardRules, CalendarRules, CallAuctionWindow, FeeKind,
    FeeRule, FeeSide, RuleSetData, TradeCost, TradeSide, Window,
};

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{Datelike, NaiveDate, NaiveTime};

/// The rules file embedded at compile time; the always-available fallback.
pub const EMBEDDED_RULES_JSON: &str = include_str!("../rules/a-share-rules.json");

/// Loaded and validated trading rules.
#[derive(Debug, Clone)]
pub struct RuleSet {
    /// Raw rule data exactly as loaded from JSON.
    pub data: RuleSetData,
    holidays: HashSet<NaiveDate>,
    auction: ParsedAuction,
}

/// Auction windows parsed into `NaiveTime` once at load.
#[derive(Debug, Clone)]
struct ParsedAuction {
    open_start: NaiveTime,
    open_no_cancel_from: NaiveTime,
    open_end: NaiveTime,
    morning_start: NaiveTime,
    morning_end: NaiveTime,
    afternoon_start: NaiveTime,
    afternoon_end: NaiveTime,
    close_start: NaiveTime,
    close_end: NaiveTime,
}

fn parse_hhmm(value: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(value, "%H:%M").map_err(|source| Error::InvalidTime {
        value: value.to_string(),
        source,
    })
}

impl ParsedAuction {
    fn from_data(w: &AuctionWindows) -> Result<Self> {
        Ok(ParsedAuction {
            open_start: parse_hhmm(&w.open_call_auction.start)?,
            open_no_cancel_from: parse_hhmm(&w.open_call_auction.no_cancel_from)?,
            open_end: parse_hhmm(&w.open_call_auction.end)?,
            morning_start: parse_hhmm(&w.continuous_morning.start)?,
            morning_end: parse_hhmm(&w.continuous_morning.end)?,
            afternoon_start: parse_hhmm(&w.continuous_afternoon.start)?,
            afternoon_end: parse_hhmm(&w.continuous_afternoon.end)?,
            close_start: parse_hhmm(&w.close_call_auction.start)?,
            close_end: parse_hhmm(&w.close_call_auction.end)?,
        })
    }
}

impl RuleSet {
    /// Load trading rules.
    ///
    /// Resolution order:
    /// 1. `path_override`, if given;
    /// 2. `a-share-rules.json` under the app config dir
    ///    (`%APPDATA%/astock-terminal/rules` on Windows,
    ///    `~/.config/astock-terminal/rules` elsewhere) — this is where a
    ///    remote-updated rules file is dropped;
    /// 3. the embedded copy compiled into the binary.
    pub fn load(path_override: Option<&Path>) -> Result<RuleSet> {
        if let Some(path) = path_override {
            return Self::from_file(path);
        }
        if let Some(path) = default_rules_path() {
            if path.is_file() {
                return Self::from_file(&path);
            }
        }
        Self::from_json(EMBEDDED_RULES_JSON)
    }

    /// Load rules from an explicit JSON file.
    pub fn from_file(path: &Path) -> Result<RuleSet> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_json(&text)
    }

    /// Parse and validate rules from a JSON string.
    pub fn from_json(text: &str) -> Result<RuleSet> {
        let data: RuleSetData = serde_json::from_str(text)?;
        let holidays: HashSet<NaiveDate> = data.calendar.holidays.iter().copied().collect();
        let auction = ParsedAuction::from_data(&data.auction)?;
        Ok(RuleSet {
            data,
            holidays,
            auction,
        })
    }

    /// Resolve the board rules for a symbol like "600519", "600519.SH" or
    /// "sz000001". Boards are matched by longest configured prefix.
    pub fn for_symbol(&self, symbol: &str) -> Result<BoardRules> {
        let code = normalize_symbol(symbol);
        let mut best: Option<(&BoardRule, usize)> = None;
        for board in &self.data.boards {
            for prefix in &board.prefixes {
                if code.starts_with(prefix.as_str()) {
                    let better = match best {
                        Some((_, best_len)) => prefix.len() > best_len,
                        None => true,
                    };
                    if better {
                        best = Some((board, prefix.len()));
                    }
                }
            }
        }
        best.map(|(board, _)| BoardRules::from_rule(board))
            .ok_or_else(|| Error::UnknownSymbol(symbol.to_string()))
    }

    /// Whether `date` is an exchange trading day (weekday and not a holiday).
    pub fn is_trading_day(&self, date: NaiveDate) -> bool {
        if self.data.calendar.weekend_closed
            && matches!(date.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun)
        {
            return false;
        }
        !self.holidays.contains(&date)
    }

    /// The first trading day strictly after `date`.
    pub fn next_trading_day(&self, date: NaiveDate) -> NaiveDate {
        let mut d = date;
        loop {
            d += chrono::Duration::days(1);
            if self.is_trading_day(d) {
                return d;
            }
        }
    }

    /// The intraday trading phase at `time`, from the configured windows.
    pub fn auction_phase(&self, time: NaiveTime) -> AuctionPhase {
        let a = &self.auction;
        if time >= a.open_start && time < a.open_no_cancel_from {
            AuctionPhase::OpenAuctionCancellable
        } else if time >= a.open_no_cancel_from && time < a.open_end {
            AuctionPhase::OpenAuctionNoCancel
        } else if time >= a.morning_start && time < a.morning_end {
            AuctionPhase::ContinuousMorning
        } else if time >= a.morning_end && time < a.afternoon_start {
            AuctionPhase::LunchBreak
        } else if time >= a.afternoon_start && time < a.afternoon_end {
            AuctionPhase::ContinuousAfternoon
        } else if time >= a.close_start && time < a.close_end {
            AuctionPhase::ClosingAuction
        } else {
            AuctionPhase::Closed
        }
    }

    /// Fee entries effective on `date` for `market` (e.g. "SH"). For each fee
    /// kind, the entry with the latest `effective_date` not after `date` wins.
    ///
    /// `market == None` selects only market-agnostic fees (entries with an
    /// empty `markets` list), so market-specific fees like the SH transfer fee
    /// are excluded when the market is unknown.
    pub fn fees_at(&self, date: NaiveDate, market: Option<&str>) -> Vec<&FeeRule> {
        let mut selected: Vec<&FeeRule> = Vec::new();
        for fee in &self.data.fees {
            if fee.effective_date > date {
                continue;
            }
            let market_ok = match market {
                Some(m) => fee.markets.is_empty() || fee.markets.iter().any(|x| x == m),
                None => fee.markets.is_empty(),
            };
            if !market_ok {
                continue;
            }
            match selected.iter_mut().find(|f| f.kind == fee.kind) {
                Some(existing) => {
                    if fee.effective_date > existing.effective_date {
                        *existing = fee;
                    }
                }
                None => selected.push(fee),
            }
        }
        selected
    }

    /// Cost breakdown for a trade of `amount` CNY on `date` in `market`.
    pub fn trade_cost_at(
        &self,
        side: TradeSide,
        amount: f64,
        market: Option<&str>,
        date: NaiveDate,
    ) -> TradeCost {
        let mut cost = TradeCost::default();
        for fee in self.fees_at(date, market) {
            if !fee.side.covers(side) {
                continue;
            }
            let mut value = amount * fee.rate;
            if let Some(min_fee) = fee.min_fee {
                value = value.max(min_fee);
            }
            match fee.kind {
                FeeKind::Commission => cost.commission += value,
                FeeKind::StampTax => cost.stamp_tax += value,
                FeeKind::TransferFee => cost.transfer_fee += value,
            }
        }
        cost.total = cost.commission + cost.stamp_tax + cost.transfer_fee;
        cost
    }

    /// Cost breakdown using the latest fee schedule in the file for `market`.
    pub fn trade_cost_with_market(&self, side: TradeSide, amount: f64, market: &str) -> TradeCost {
        self.trade_cost_at(side, amount, Some(market), self.latest_fee_date())
    }

    /// Latest `effective_date` among fee entries — the "current" policy date
    /// as far as this rules file knows.
    pub fn latest_fee_date(&self) -> NaiveDate {
        self.data
            .fees
            .iter()
            .map(|f| f.effective_date)
            .max()
            .unwrap_or(self.data.effective_date)
    }
}

/// Cost breakdown for a trade of `amount` CNY using the latest fee schedule.
///
/// The market is unknown at this call site, so market-specific fees (e.g. the
/// SH transfer fee) are excluded; use [`RuleSet::trade_cost_with_market`] when
/// the market is known.
pub fn trade_cost(side: TradeSide, amount: f64, rules: &RuleSet) -> TradeCost {
    rules.trade_cost_at(side, amount, None, rules.latest_fee_date())
}

/// Default override location for a remote-updated rules file.
fn default_rules_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|p| {
            PathBuf::from(p)
                .join("astock-terminal")
                .join("rules")
                .join("a-share-rules.json")
        })
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(|p| {
            PathBuf::from(p)
                .join(".config")
                .join("astock-terminal")
                .join("rules")
                .join("a-share-rules.json")
        })
    }
}

/// Strip exchange decorations: "600519.SH" -> "600519", "sz000001" -> "000001".
fn normalize_symbol(symbol: &str) -> String {
    let head = symbol.trim().split('.').next().unwrap_or("");
    head.trim_start_matches(|c: char| c.is_ascii_alphabetic())
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> RuleSet {
        RuleSet::load(None).unwrap()
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn t(s: &str) -> NaiveTime {
        NaiveTime::parse_from_str(s, "%H:%M").unwrap()
    }

    #[test]
    fn embedded_file_carries_provenance() {
        let r = rules();
        assert!(!r.data.version.is_empty());
        assert!(!r.data.source_url.is_empty());
        assert_eq!(r.data.boards.len(), 5);
    }

    #[test]
    fn board_classification() {
        let r = rules();
        assert_eq!(r.for_symbol("600519").unwrap().board_id, "sh_main");
        assert_eq!(r.for_symbol("000001").unwrap().board_id, "sz_main");
        assert_eq!(r.for_symbol("002594").unwrap().board_id, "sz_main");
        assert_eq!(r.for_symbol("300750").unwrap().board_id, "chinext");
        assert_eq!(r.for_symbol("688981").unwrap().board_id, "star");
        assert_eq!(r.for_symbol("920001").unwrap().board_id, "bse");
        assert_eq!(r.for_symbol("430047").unwrap().board_id, "bse");
        assert_eq!(r.for_symbol("830799").unwrap().board_id, "bse");
        // Decorated forms.
        assert_eq!(r.for_symbol("600519.SH").unwrap().board_id, "sh_main");
        assert_eq!(r.for_symbol("sz000001").unwrap().board_id, "sz_main");
        assert!(r.for_symbol("123456").is_err());
    }

    #[test]
    fn board_limits_and_lots() {
        let r = rules();
        let sh = r.for_symbol("600519").unwrap();
        assert_eq!(sh.price_limit_pct(false), 0.10);
        assert_eq!(sh.price_limit_pct(true), 0.05); // ST on main board
        assert!(sh.t_plus_1);
        assert_eq!(sh.min_lot, 100);
        assert!(sh.is_valid_lot(300));
        assert!(!sh.is_valid_lot(150));

        let star = r.for_symbol("688981").unwrap();
        assert_eq!(star.price_limit_pct(false), 0.20);
        assert_eq!(star.price_limit_pct(true), 0.20); // STAR ST limit is 20%
        assert_eq!(star.min_lot, 200);
        assert!(star.is_valid_lot(201)); // 200 then +1
        assert!(!star.is_valid_lot(150));

        let bse = r.for_symbol("430047").unwrap();
        assert_eq!(bse.price_limit_pct(false), 0.30);
        assert_eq!(bse.ipo_no_limit_days, 1);
    }

    #[test]
    fn limit_price_rounding() {
        let r = rules();
        let sh = r.for_symbol("600519").unwrap();
        assert_eq!(sh.limit_up_price(10.0, false), 11.0);
        assert_eq!(sh.limit_down_price(10.0, false), 9.0);
        assert_eq!(sh.limit_up_price(10.0, true), 10.5);
    }

    #[test]
    fn trading_day_around_national_day_2025() {
        let r = rules();
        assert!(r.is_trading_day(d("2025-09-30"))); // Tuesday before the break
        for day in 1..=8 {
            assert!(
                !r.is_trading_day(d(&format!("2025-10-{day:02}"))),
                "2025-10-{day:02} should be closed"
            );
        }
        assert!(r.is_trading_day(d("2025-10-09"))); // Thursday, first day back
        assert_eq!(r.next_trading_day(d("2025-09-30")), d("2025-10-09"));
        assert_eq!(r.next_trading_day(d("2025-10-08")), d("2025-10-09"));
        // Plain weekend.
        assert!(!r.is_trading_day(d("2025-03-08"))); // Saturday
        assert_eq!(r.next_trading_day(d("2025-03-07")), d("2025-03-10"));
    }

    #[test]
    fn auction_phases() {
        let r = rules();
        assert_eq!(r.auction_phase(t("08:59")), AuctionPhase::Closed);
        assert_eq!(
            r.auction_phase(t("09:15")),
            AuctionPhase::OpenAuctionCancellable
        );
        assert_eq!(
            r.auction_phase(t("09:19")),
            AuctionPhase::OpenAuctionCancellable
        );
        assert_eq!(
            r.auction_phase(t("09:20")),
            AuctionPhase::OpenAuctionNoCancel
        );
        assert_eq!(
            r.auction_phase(t("09:24")),
            AuctionPhase::OpenAuctionNoCancel
        );
        assert_eq!(r.auction_phase(t("09:30")), AuctionPhase::ContinuousMorning);
        assert_eq!(r.auction_phase(t("11:29")), AuctionPhase::ContinuousMorning);
        assert_eq!(r.auction_phase(t("12:00")), AuctionPhase::LunchBreak);
        assert_eq!(
            r.auction_phase(t("13:30")),
            AuctionPhase::ContinuousAfternoon
        );
        assert_eq!(
            r.auction_phase(t("14:56")),
            AuctionPhase::ContinuousAfternoon
        );
        assert_eq!(r.auction_phase(t("14:57")), AuctionPhase::ClosingAuction);
        assert_eq!(r.auction_phase(t("15:00")), AuctionPhase::Closed);
    }

    #[test]
    fn fee_calc_example() {
        let r = rules();
        // Buy 1,000,000 CNY on SH: commission 250 + transfer 10, no stamp tax.
        let buy = r.trade_cost_with_market(TradeSide::Buy, 1_000_000.0, "SH");
        assert_eq!(buy.commission, 250.0);
        assert_eq!(buy.stamp_tax, 0.0);
        assert_eq!(buy.transfer_fee, 10.0);
        assert_eq!(buy.total, 260.0);
        // Sell: stamp tax 0.05% = 500 on top.
        let sell = r.trade_cost_with_market(TradeSide::Sell, 1_000_000.0, "SH");
        assert_eq!(sell.stamp_tax, 500.0);
        assert_eq!(sell.total, 760.0);
        // Min commission floor: 1,000 CNY trade -> 0.25 -> floor 5.0.
        let small = r.trade_cost_with_market(TradeSide::Buy, 1_000.0, "SH");
        assert_eq!(small.commission, 5.0);
        // Market-agnostic free function excludes the transfer fee.
        let generic = trade_cost(TradeSide::Sell, 1_000_000.0, &r);
        assert_eq!(generic.transfer_fee, 0.0);
        assert_eq!(generic.total, 750.0);
    }

    #[test]
    fn fee_history_respects_effective_dates() {
        let r = rules();
        // Before the 2023-08-28 stamp tax cut the sell-side rate was 0.1%.
        let old = r.trade_cost_at(TradeSide::Sell, 1_000_000.0, None, d("2023-08-27"));
        assert_eq!(old.stamp_tax, 1000.0);
        let new = r.trade_cost_at(TradeSide::Sell, 1_000_000.0, None, d("2023-08-28"));
        assert_eq!(new.stamp_tax, 500.0);
    }

    #[test]
    fn override_path_is_used() {
        let dir = std::env::temp_dir().join(format!("astock-rules-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rules.json");
        let mut json: serde_json::Value = serde_json::from_str(EMBEDDED_RULES_JSON).unwrap();
        json["version"] = serde_json::Value::String("override-test".into());
        std::fs::write(&path, serde_json::to_string(&json).unwrap()).unwrap();
        let r = RuleSet::load(Some(&path)).unwrap();
        assert_eq!(r.data.version, "override-test");
        std::fs::remove_dir_all(&dir).ok();
    }
}
