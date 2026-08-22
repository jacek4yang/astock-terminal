//! Symbol model and exchange-specific code conversions.
//!
//! Ports the classification logic of the legacy Python `_is_etf`,
//! `symbol_to_secid`, `symbol_to_tencent` and `_sina_symbol` helpers.

use crate::error::DataError;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Chinese exchange a symbol trades on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Market {
    /// Shanghai Stock Exchange.
    SH,
    /// Shenzhen Stock Exchange.
    SZ,
    /// Beijing Stock Exchange.
    BJ,
}

impl fmt::Display for Market {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Market::SH => write!(f, "SH"),
            Market::SZ => write!(f, "SZ"),
            Market::BJ => write!(f, "BJ"),
        }
    }
}

/// A bare 6-digit A-share / fund code, zero-padded on construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Symbol(String);

impl Symbol {
    /// Build a symbol from any string; trims and left-pads with zeros to 6 chars.
    ///
    /// Returns [`DataError::InvalidSymbol`] unless the result is exactly 6 ASCII digits.
    pub fn new(raw: impl AsRef<str>) -> Result<Self, DataError> {
        let trimmed = raw.as_ref().trim();
        if trimmed.is_empty() {
            return Err(DataError::InvalidSymbol(trimmed.to_string()));
        }
        let padded = format!("{:0>6}", trimmed);
        if padded.len() == 6 && padded.bytes().all(|b| b.is_ascii_digit()) {
            Ok(Symbol(padded))
        } else {
            Err(DataError::InvalidSymbol(trimmed.to_string()))
        }
    }

    /// The 6-digit code.
    pub fn code(&self) -> &str {
        &self.0
    }

    /// Exchange this symbol trades on, mirroring the legacy secid rules:
    /// `920xxx` → BJ, prefix 5/6/7/9 → SH, everything else → SZ.
    pub fn market(&self) -> Market {
        let s = &self.0;
        if s.starts_with("920") {
            Market::BJ
        } else if s.starts_with(['5', '6', '7', '9']) {
            Market::SH
        } else {
            Market::SZ
        }
    }

    /// EastMoney secid, e.g. `1.600519`. `920xxx` → `0.` (BJ trades on the
    /// SZ-side feed), prefix 5/6/7/9 → `1.`, otherwise `0.`.
    pub fn secid(&self) -> String {
        let s = &self.0;
        if s.starts_with("920") {
            format!("0.{s}")
        } else if s.starts_with(['5', '6', '7', '9']) {
            format!("1.{s}")
        } else {
            format!("0.{s}")
        }
    }

    /// Tencent symbol, e.g. `sh600519`. Prefix 6/5 → `sh`, else `sz` (no BJ feed).
    pub fn tencent(&self) -> String {
        let s = &self.0;
        if s.starts_with(['6', '5']) {
            format!("sh{s}")
        } else {
            format!("sz{s}")
        }
    }

    /// Sina symbol, e.g. `sh600519`. 6/5 → `sh`, 920 → `bj`, else `sz`.
    pub fn sina(&self) -> String {
        let s = &self.0;
        if s.starts_with(['6', '5']) {
            format!("sh{s}")
        } else if s.starts_with("920") {
            format!("bj{s}")
        } else {
            format!("sz{s}")
        }
    }

    /// ETF/LOF/closed-end fund heuristic, ported from the legacy `_is_etf`:
    /// SH `5xxxxx` = ETF/LOF; SZ `159xxx` = ETF, `18xxx` = closed-end fund;
    /// SZ `1xxxxx` outside the known A-share/B-share prefixes is also a fund.
    pub fn is_etf(&self) -> bool {
        let s = &self.0;
        if s.starts_with('5') {
            return true;
        }
        if s.starts_with("159") || s.starts_with("18") {
            return true;
        }
        // Exact port of the legacy exclusion list; `000/002/003` never start
        // with '1' but are kept for parity with the Python source.
        const NON_FUND_1X: [&str; 14] = [
            "000", "002", "003", "100", "110", "120", "130", "140", "150", "160", "170", "180",
            "200", "300",
        ];
        if s.starts_with('1') && !NON_FUND_1X.iter().any(|p| s.starts_with(p)) {
            return true;
        }
        false
    }

    /// EastMoney secid for an index code: `399xxx` → `0.`, else `1.`.
    pub fn index_secid(index_code: &str) -> String {
        if index_code.starts_with("399") {
            format!("0.{index_code}")
        } else {
            format!("1.{index_code}")
        }
    }

    /// Tencent symbol for an index code: `sh000001` / `sz399xxx`.
    pub fn index_tencent(index_code: &str) -> String {
        if index_code.starts_with("399") {
            format!("sz{index_code}")
        } else {
            format!("sh{index_code}")
        }
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for Symbol {
    type Error = DataError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Symbol::new(value)
    }
}

impl From<Symbol> for String {
    fn from(value: Symbol) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(s: &str) -> Symbol {
        Symbol::new(s).unwrap()
    }

    #[test]
    fn zfill_and_validate() {
        assert_eq!(sym("600519").code(), "600519");
        assert_eq!(sym("1").code(), "000001");
        assert!(Symbol::new("").is_err());
        assert!(Symbol::new("6005190").is_err());
        assert!(Symbol::new("abcdef").is_err());
    }

    #[test]
    fn secid_rules() {
        assert_eq!(sym("600519").secid(), "1.600519");
        assert_eq!(sym("510300").secid(), "1.510300");
        assert_eq!(sym("000001").secid(), "0.000001");
        assert_eq!(sym("300750").secid(), "0.300750");
        assert_eq!(sym("920001").secid(), "0.920001");
        assert_eq!(sym("900901").secid(), "1.900901");
        assert_eq!(sym("700001").secid(), "1.700001");
    }

    #[test]
    fn tencent_and_sina_rules() {
        assert_eq!(sym("600519").tencent(), "sh600519");
        assert_eq!(sym("510300").tencent(), "sh510300");
        assert_eq!(sym("000001").tencent(), "sz000001");
        // Tencent has no BJ feed: 920 falls back to sz, per legacy.
        assert_eq!(sym("920001").tencent(), "sz920001");

        assert_eq!(sym("600519").sina(), "sh600519");
        assert_eq!(sym("920001").sina(), "bj920001");
        assert_eq!(sym("300750").sina(), "sz300750");
    }

    #[test]
    fn market_classification() {
        assert_eq!(sym("600519").market(), Market::SH);
        assert_eq!(sym("510300").market(), Market::SH);
        assert_eq!(sym("000001").market(), Market::SZ);
        assert_eq!(sym("920001").market(), Market::BJ);
    }

    #[test]
    fn etf_heuristic() {
        assert!(sym("510300").is_etf()); // SH ETF
        assert!(sym("588000").is_etf()); // SH STAR ETF
        assert!(sym("159915").is_etf()); // SZ ETF
        assert!(sym("180801").is_etf()); // SZ closed-end fund
        assert!(sym("161725").is_etf()); // SZ LOF (1-prefix, not excluded)
        assert!(!sym("600519").is_etf());
        assert!(!sym("000001").is_etf());
        assert!(!sym("300750").is_etf());
    }

    #[test]
    fn index_codes() {
        assert_eq!(Symbol::index_secid("000001"), "1.000001");
        assert_eq!(Symbol::index_secid("399001"), "0.399001");
        assert_eq!(Symbol::index_tencent("000001"), "sh000001");
        assert_eq!(Symbol::index_tencent("399006"), "sz399006");
    }

    #[test]
    fn serde_roundtrip() {
        let s = sym("600519");
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"600519\"");
        let back: Symbol = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
