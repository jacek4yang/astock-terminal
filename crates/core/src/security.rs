//! Canonical security identity used independently from realtime quote feeds.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Market, Symbol};

/// Canonical display form for exchange security abbreviations.  Upstream
/// test/legacy feeds sometimes insert spaces between every Chinese character;
/// exchange abbreviations themselves do not use whitespace as identity.
pub fn normalize_security_name(raw: &str) -> String {
    raw.chars().filter(|ch| !ch.is_whitespace()).collect()
}

/// Broad instrument type in the security master.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetType {
    /// Mainland listed common equity.
    Stock,
    /// Exchange-traded or listed fund.
    Fund,
    /// Index.
    Index,
    /// Convertible or straight bond.
    Bond,
    /// Classification could not yet be established.
    Unknown,
}

/// A-share listing board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Board {
    Main,
    ChiNext,
    Star,
    Beijing,
    Fund,
    Other,
}

/// Stable security identity and slowly changing classification metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecurityMasterRecord {
    pub code: String,
    pub canonical_name: String,
    pub market: Market,
    pub board: Board,
    pub asset_type: AssetType,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub industry: Option<String>,
    #[serde(default)]
    pub concepts: Vec<String>,
    pub region: Option<String>,
    pub source: String,
    pub source_url: Option<String>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub refreshed_at: DateTime<Utc>,
}

impl SecurityMasterRecord {
    /// Build a minimally classified stock record from an exchange list.
    pub fn listed_stock(
        code: impl Into<String>,
        name: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        let code = code.into();
        let symbol = Symbol::new(&code).expect("security-master code must be six digits");
        let market = symbol.market();
        let board = board_for(&code, market);
        Self {
            code,
            canonical_name: normalize_security_name(&name.into()),
            market,
            board,
            asset_type: if symbol.is_etf() {
                AssetType::Fund
            } else {
                AssetType::Stock
            },
            aliases: Vec::new(),
            industry: None,
            concepts: Vec::new(),
            region: None,
            source: source.into(),
            source_url: None,
            valid_from: None,
            valid_to: None,
            refreshed_at: crate::time::utc_now(),
        }
    }
}

/// Deterministic board classification from the exchange and code prefix.
pub fn board_for(code: &str, market: Market) -> Board {
    if Symbol::new(code).is_ok_and(|symbol| symbol.is_etf()) {
        return Board::Fund;
    }
    if code.starts_with("300") || code.starts_with("301") {
        Board::ChiNext
    } else if code.starts_with("688") || code.starts_with("689") {
        Board::Star
    } else if market == Market::BJ {
        Board::Beijing
    } else if code.starts_with("00") || code.starts_with("60") {
        Board::Main
    } else {
        Board::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_representative_a_share_boards() {
        assert_eq!(board_for("000001", Market::SZ), Board::Main);
        assert_eq!(board_for("300308", Market::SZ), Board::ChiNext);
        assert_eq!(board_for("600519", Market::SH), Board::Main);
        assert_eq!(board_for("688981", Market::SH), Board::Star);
        assert_eq!(board_for("920001", Market::BJ), Board::Beijing);
    }

    #[test]
    fn canonical_name_removes_upstream_spacing_noise() {
        assert_eq!(normalize_security_name(" 中 科 软 "), "中科软");
        assert_eq!(normalize_security_name("*ST  测试"), "*ST测试");
    }
}
