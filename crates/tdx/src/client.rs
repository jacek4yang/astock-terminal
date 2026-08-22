//! 高层客户端 API：证券列表 / K 线（自动分页）/ 五档快照 / 分时。
//! 所有接口只返回未复权原始数据。

use crate::error::{Result, TdxError};
use crate::pool::ServerPool;
use crate::protocol::constants::MAX_KLINE_COUNT;
use crate::protocol::frame::{
    build_history_minute_packet, build_security_bars_packet, build_security_count_packet,
    build_security_list_packet, build_security_quotes_packet,
};
use crate::protocol::parse::{
    parse_history_minute_time, parse_security_bars, parse_security_count, parse_security_list,
    parse_security_quotes,
};
use crate::protocol::types::{
    auto_market, KlineCategory, MinuteBar, Quote, SecurityBar, SecurityInfo,
};

/// 通达信行情客户端（持有一个 [`ServerPool`]）。
pub struct TdxClient {
    pool: ServerPool,
}

impl TdxClient {
    pub fn new(pool: ServerPool) -> Self {
        Self { pool }
    }

    /// 用默认配置探测并建池。
    pub async fn start() -> Result<Self> {
        Ok(Self::new(ServerPool::start(Default::default()).await?))
    }

    pub fn pool(&self) -> &ServerPool {
        &self.pool
    }

    /// 证券数量（0x044E）。
    pub async fn security_count(&self, market: u8) -> Result<u16> {
        let body = self
            .pool
            .request(&build_security_count_packet(market))
            .await?;
        parse_security_count(&body)
    }

    /// 证券列表单页（0x0450，每页 1000 条）。
    pub async fn security_list_page(&self, market: u8, start: u16) -> Result<Vec<SecurityInfo>> {
        let body = self
            .pool
            .request(&build_security_list_packet(market, start))
            .await?;
        parse_security_list(&body)
    }

    /// 全量证券列表（自动翻页直到取满 count）。
    pub async fn security_list(&self, market: u8) -> Result<Vec<SecurityInfo>> {
        let total = self.security_count(market).await? as usize;
        let mut out = Vec::with_capacity(total);
        let mut start = 0u16;
        while out.len() < total {
            let page = self.security_list_page(market, start).await?;
            if page.is_empty() {
                break;
            }
            start += page.len() as u16;
            out.extend(page);
        }
        Ok(out)
    }

    /// K 线（0x052D，单次 ≤800 条，自动分页取满 `count`，按时间升序）。
    /// 只返回未复权数据（fq 保留位恒为 0）。
    ///
    /// 协议分页语义：`start` 为距最新一根的偏移，每页内部时间升序、
    /// `start` 越大数据越早，因此拼接时需页间倒序、页内保持升序。
    pub async fn kline(
        &self,
        market: u8,
        code: &str,
        category: KlineCategory,
        count: u16,
    ) -> Result<Vec<SecurityBar>> {
        let mut pages: Vec<Vec<SecurityBar>> = Vec::new();
        let mut fetched = 0usize;
        let mut start = 0u16;
        while fetched < count as usize {
            let want = (count as usize - fetched).min(MAX_KLINE_COUNT as usize) as u16;
            let pkt = build_security_bars_packet(category.as_u8(), market, code, start, want);
            let body = self.pool.request(&pkt).await?;
            let page = parse_security_bars(&body, category.is_minute_level())?;
            if page.is_empty() {
                break;
            }
            fetched += page.len();
            start += page.len() as u16;
            let short_page = page.len() < want as usize;
            pages.push(page);
            if short_page {
                break; // 服务端已没有更早数据
            }
        }
        let mut out: Vec<SecurityBar> = Vec::with_capacity(fetched);
        for page in pages.iter().rev() {
            out.extend(page.iter().cloned());
        }
        Ok(out)
    }

    /// 便捷：按 6 位代码自动判市取 K 线。北交所等无法判市的代码报错。
    pub async fn kline_auto(
        &self,
        code: &str,
        category: KlineCategory,
        count: u16,
    ) -> Result<Vec<SecurityBar>> {
        let market = auto_market(code)
            .ok_or_else(|| TdxError::Protocol(format!("cannot determine market for {code}")))?;
        self.kline(market, code, category, count).await
    }

    /// 五档快照（0x053E，单次 ≤60 只，超出截断）。
    pub async fn quotes(&self, stocks: &[(u8, &str)]) -> Result<Vec<Quote>> {
        let body = self
            .pool
            .request(&build_security_quotes_packet(stocks))
            .await?;
        parse_security_quotes(&body)
    }

    /// 历史分时（0x0FB4），`date` 为 YYYYMMDD。按时间升序。
    pub async fn history_minute(
        &self,
        market: u8,
        code: &str,
        date: u32,
    ) -> Result<Vec<MinuteBar>> {
        let body = self
            .pool
            .request(&build_history_minute_packet(market, code, date))
            .await?;
        parse_history_minute_time(&body, market, code)
    }

    /// 当日分时。绕开 0x051D 已知价格编码 bug，走 0x0FB4 传今日日期。
    pub async fn minute_today(&self, market: u8, code: &str) -> Result<Vec<MinuteBar>> {
        self.history_minute(market, code, today_yyyymmdd()).await
    }
}

/// 今日日期（YYYYMMDD，UTC+8，无 chrono 依赖）。
pub fn today_yyyymmdd() -> u32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + 8 * 3600;
    let days = now / 86400;
    // 由 epoch 天数推年月日（Howard Hinnant 算法）
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32) * 10000 + (m as u32) * 100 + d as u32
}

/// 常用市场常量重导出，方便调用方。
pub mod markets {
    pub use crate::protocol::constants::{MARKET_SH, MARKET_SZ};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn today_format() {
        let t = today_yyyymmdd();
        assert!(t > 20_260_101 && t < 21_000_101, "bad date {t}");
        // 月、日合法
        let m = (t % 10000) / 100;
        let d = t % 100;
        assert!((1..=12).contains(&m) && (1..=31).contains(&d));
    }

    #[test]
    fn epoch_algorithm_spot_check() {
        // 与 today_yyyymmdd 同算法手算验证: 2026-08-22 距 epoch 20687 天
        // (date -d @$((20687*86400)) +%Y-%m-%d → 2026-08-22 UTC)
        let days: u64 = 20687;
        let z = days as i64 + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        assert_eq!((y, m, d), (2026, 8, 22));
    }
}
