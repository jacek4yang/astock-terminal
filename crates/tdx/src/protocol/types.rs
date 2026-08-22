//! 公开数据类型。

/// K 线周期（0x052D payload category 字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum KlineCategory {
    FiveMin = 0,
    FifteenMin = 1,
    ThirtyMin = 2,
    OneHour = 3,
    /// 日 K。注意：pytdx 名义日 K 是 9，但 2026-08 实测现役服务器上
    /// 9 只回最新一根，4 才返回完整分页数据，故日线用 4。
    Daily = 4,
    Weekly = 5,
    Monthly = 6,
    /// 1 分钟（扩展行情源）
    ExHqOneMin = 7,
    OneMin = 8,
    /// pytdx 名义日 K（"RI_K"），实测只回最新一根，保留勿用于日线。
    DailyRiK = 9,
    Quarterly = 10,
    Yearly = 11,
}

impl KlineCategory {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// 是否分钟级周期（影响时间解码分支：位打包相对时间 vs YYYYMMDD）。
    pub fn is_minute_level(self) -> bool {
        matches!(
            self,
            Self::FiveMin
                | Self::FifteenMin
                | Self::ThirtyMin
                | Self::OneHour
                | Self::ExHqOneMin
                | Self::OneMin
        )
    }
}

/// 单根 K 线（未复权，价格单位：元）。
#[derive(Debug, Clone, PartialEq)]
pub struct SecurityBar {
    /// "YYYY-MM-DD" 或分钟级 "YYYY-MM-DD HH:MM"
    pub datetime: String,
    pub open: f64,
    pub close: f64,
    pub high: f64,
    pub low: f64,
    /// 成交量（手；指数记录服务端已放大，保持原样）
    pub vol: f64,
    /// 成交额（元）
    pub amount: f64,
}

/// 五档快照（价格单位：元）。
#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    pub market: u8,
    pub code: String,
    pub price: f64,
    pub last_close: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    /// 服务器时间 "HH:MM:SS"
    pub servertime: String,
    /// 总手
    pub vol: f64,
    /// 现量
    pub cur_vol: f64,
    /// 成交额（元）
    pub amount: f64,
    /// 内盘
    pub s_vol: f64,
    /// 外盘
    pub b_vol: f64,
    pub bid: [(f64, f64); 5],
    pub ask: [(f64, f64); 5],
}

/// 证券列表条目。
#[derive(Debug, Clone, PartialEq)]
pub struct SecurityInfo {
    pub code: String,
    /// 名称（GBK 解码后）
    pub name: String,
    pub volunit: u16,
    pub decimal_point: u8,
    pub pre_close: f64,
}

/// 分时数据点。
#[derive(Debug, Clone, PartialEq)]
pub struct MinuteBar {
    /// "HH:MM"
    pub time: String,
    pub price: f64,
    pub avg_price: f64,
    pub vol: f64,
}

/// 证券类型 → 快照/分时价格系数（协议原始值为「分」级整数时乘系数得元）。
///
/// 内化自 tdxrs `src/protocol/types.rs`（MIT）。
pub fn price_coefficient(market: u8, code: &str) -> f64 {
    match security_type(market, code) {
        2 => 0.001,   // B 股
        3 => 0.001,   // 场内基金
        4 => 0.0001,  // 债券
        5 => 0.00001, // 场外基金
        _ => 0.01,    // A 股 / 指数 / 未知
    }
}

fn security_type(market: u8, code: &str) -> u8 {
    if market == 1 {
        // 上海
        if code.starts_with("60") || code.starts_with("68") {
            return 1; // A 股
        }
        if code.starts_with("90") {
            return 2; // B 股
        }
        if code.starts_with("519") {
            return 5; // 场外基金（须先于场内基金判断）
        }
        if code.starts_with("50") || code.starts_with("51") || code.starts_with("58") {
            return 3; // 场内基金
        }
        if code.starts_with("11") || code.starts_with("13") {
            return 4; // 债券
        }
        if code.starts_with("000") {
            return 0; // 指数
        }
    } else if market == 0 {
        // 深圳
        if code.starts_with("39") {
            return 0; // 指数
        }
        if code.starts_with("00") || code.starts_with("30") {
            return 1; // A 股
        }
        if code.starts_with("20") {
            return 2; // B 股
        }
        if code.starts_with("15") || code.starts_with("16") {
            return 3; // 基金
        }
        if code.starts_with("10") || code.starts_with("12") || code.starts_with("13") {
            return 4; // 债券
        }
    }
    1 // 默认按 A 股
}

/// 号段自动判市：`6/9/5`（含 68/50/51/58/11/13）→ 沪，`0/2/3/1` → 深。
/// 北交所（4/8 开头）hq 协议不支持，返回 None。
pub fn auto_market(code: &str) -> Option<u8> {
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match code.as_bytes()[0] {
        b'6' | b'9' | b'5' => Some(super::constants::MARKET_SH),
        b'0' | b'2' | b'3' | b'1' => Some(super::constants::MARKET_SZ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_minute_level() {
        assert!(KlineCategory::FiveMin.is_minute_level());
        assert!(KlineCategory::OneMin.is_minute_level());
        assert!(!KlineCategory::Daily.is_minute_level());
        assert!(!KlineCategory::DailyRiK.is_minute_level());
        assert!(!KlineCategory::Weekly.is_minute_level());
        assert!(!KlineCategory::Yearly.is_minute_level());
    }

    #[test]
    fn coefficient_by_type() {
        assert_eq!(price_coefficient(1, "600519"), 0.01);
        assert_eq!(price_coefficient(1, "510300"), 0.001);
        assert_eq!(price_coefficient(1, "519736"), 0.00001);
        assert_eq!(price_coefficient(1, "113044"), 0.0001);
        assert_eq!(price_coefficient(0, "399001"), 0.01);
        assert_eq!(price_coefficient(0, "159915"), 0.001);
    }

    #[test]
    fn auto_market_rules() {
        assert_eq!(auto_market("600519"), Some(1));
        assert_eq!(auto_market("688981"), Some(1));
        assert_eq!(auto_market("000858"), Some(0));
        assert_eq!(auto_market("300750"), Some(0));
        assert_eq!(auto_market("830799"), None); // 北交所不支持
        assert_eq!(auto_market("12345"), None);
        assert_eq!(auto_market("abcdef"), None);
    }
}
