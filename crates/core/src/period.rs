//! Kline period and price-adjustment enums with per-upstream encodings.

use serde::{Deserialize, Serialize};

/// Bar aggregation period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KlinePeriod {
    /// Daily bars.
    Day,
    /// Weekly bars.
    Week,
    /// Monthly bars.
    Month,
    /// 1-minute bars.
    Min1,
    /// 5-minute bars.
    Min5,
    /// 15-minute bars.
    Min15,
    /// 30-minute bars.
    Min30,
    /// 60-minute bars.
    Min60,
}

impl KlinePeriod {
    /// Tencent `fqkline` period token (`day` / `week` / `month`).
    ///
    /// Minute periods are not supported by the ported endpoints; returns `None`.
    pub fn tencent_token(self) -> Option<&'static str> {
        match self {
            KlinePeriod::Day => Some("day"),
            KlinePeriod::Week => Some("week"),
            KlinePeriod::Month => Some("month"),
            _ => None,
        }
    }

    /// Sina `scale` parameter: day=240, week=1200, month=7200.
    ///
    /// Minute periods are not ported; returns `None`.
    pub fn sina_scale(self) -> Option<u32> {
        match self {
            KlinePeriod::Day => Some(240),
            KlinePeriod::Week => Some(1200),
            KlinePeriod::Month => Some(7200),
            _ => None,
        }
    }

    /// EastMoney `klt` parameter: 101=day, 102=week, 103=month,
    /// 1/5/15/30/60 for the minute periods.
    pub fn eastmoney_klt(self) -> u32 {
        match self {
            KlinePeriod::Day => 101,
            KlinePeriod::Week => 102,
            KlinePeriod::Month => 103,
            KlinePeriod::Min1 => 1,
            KlinePeriod::Min5 => 5,
            KlinePeriod::Min15 => 15,
            KlinePeriod::Min30 => 30,
            KlinePeriod::Min60 => 60,
        }
    }
}

/// Price adjustment mode for kline data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Adjust {
    /// Unadjusted prices.
    #[default]
    None,
    /// Forward-adjusted (前复权).
    Qfq,
    /// Backward-adjusted (后复权).
    Hfq,
}

impl Adjust {
    /// Tencent `fq` token: empty string means unadjusted.
    pub fn tencent_token(self) -> &'static str {
        match self {
            Adjust::None => "",
            Adjust::Qfq => "qfq",
            Adjust::Hfq => "hfq",
        }
    }

    /// EastMoney `fqt` parameter: 0=none, 1=qfq, 2=hfq.
    pub fn eastmoney_fqt(self) -> u32 {
        match self {
            Adjust::None => 0,
            Adjust::Qfq => 1,
            Adjust::Hfq => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn period_encodings() {
        assert_eq!(KlinePeriod::Day.tencent_token(), Some("day"));
        assert_eq!(KlinePeriod::Week.tencent_token(), Some("week"));
        assert_eq!(KlinePeriod::Month.tencent_token(), Some("month"));
        assert_eq!(KlinePeriod::Day.sina_scale(), Some(240));
        assert_eq!(KlinePeriod::Week.sina_scale(), Some(1200));
        assert_eq!(KlinePeriod::Month.sina_scale(), Some(7200));
        assert_eq!(KlinePeriod::Day.eastmoney_klt(), 101);
        assert_eq!(KlinePeriod::Week.eastmoney_klt(), 102);
        assert_eq!(KlinePeriod::Month.eastmoney_klt(), 103);
    }

    #[test]
    fn adjust_encodings() {
        assert_eq!(Adjust::None.tencent_token(), "");
        assert_eq!(Adjust::Qfq.tencent_token(), "qfq");
        assert_eq!(Adjust::Hfq.tencent_token(), "hfq");
        assert_eq!(Adjust::None.eastmoney_fqt(), 0);
        assert_eq!(Adjust::Qfq.eastmoney_fqt(), 1);
        assert_eq!(Adjust::Hfq.eastmoney_fqt(), 2);
    }
}
