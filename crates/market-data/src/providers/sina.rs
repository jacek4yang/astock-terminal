//! Sina kline provider (fallback).
//!
//! Ported from the legacy `_fetch_kline_sina`. Unadjusted but price-accurate.
//! scale: day=240, week=1200, month=7200. Volume quirk: A-shares report raw
//! shares (÷100 → lots); ETFs already report fund units (份) and pass through.

use crate::http::HttpClient;
use crate::provider::DataProvider;
use crate::providers::{fill_pct, json_f64};
use astock_core::time::parse_date;
use astock_core::{Bar, DataError, Fetched, Source, Symbol, VolumeUnit};
use async_trait::async_trait;
use std::sync::Arc;

/// Sina `CN_MarketData.getKLineData` endpoint.
pub const SINA_KLINE_URL: &str =
    "https://money.finance.sina.com.cn/quotes_service/api/json_v2.php/CN_MarketData.getKLineData";

/// Sina kline adapter.
pub struct SinaKline {
    http: Arc<HttpClient>,
}

impl SinaKline {
    /// Wrap the shared HTTP client.
    pub fn new(http: Arc<HttpClient>) -> Self {
        SinaKline { http }
    }

    /// Parse the Sina JSON array body into bars. `is_etf` selects the
    /// volume-unit conversion (shares ÷100 → lots for stocks, raw 份 for ETFs).
    pub(crate) fn parse_body(body: &str, symbol: &Symbol) -> Result<Vec<Bar>, DataError> {
        let items: serde_json::Value =
            serde_json::from_str(body.trim()).map_err(|e| DataError::Parse {
                upstream: format!("sina {symbol}"),
                message: e.to_string(),
            })?;
        let items = items.as_array().ok_or_else(|| DataError::Parse {
            upstream: format!("sina {symbol}"),
            message: "expected a JSON array".to_string(),
        })?;
        if items.is_empty() {
            return Err(DataError::Empty(format!("sina kline {symbol}")));
        }
        let is_etf = symbol.is_etf();
        let unit = if is_etf {
            VolumeUnit::FundUnits
        } else {
            VolumeUnit::Lots
        };
        let mut bars = Vec::with_capacity(items.len());
        for item in items {
            let day = item.get("day").and_then(|d| d.as_str()).unwrap_or("");
            let (Some(date), Some(open), Some(close), Some(high), Some(low)) = (
                parse_date(day),
                item.get("open").and_then(json_f64),
                item.get("close").and_then(json_f64),
                item.get("high").and_then(json_f64),
                item.get("low").and_then(json_f64),
            ) else {
                continue;
            };
            let raw_vol = item.get("volume").and_then(json_f64).unwrap_or(0.0);
            // A-share volume arrives in shares → ÷100 to lots; ETF 份 pass through.
            let volume = if is_etf { raw_vol } else { raw_vol / 100.0 };
            bars.push(Bar::new(date, open, close, high, low, volume, unit));
        }
        if bars.is_empty() {
            return Err(DataError::Empty(format!(
                "sina kline {symbol}: no parseable rows"
            )));
        }
        // Sina already returns oldest → newest.
        fill_pct(&mut bars);
        Ok(bars)
    }
}

#[async_trait]
impl DataProvider for SinaKline {
    fn name(&self) -> &'static str {
        "sina"
    }

    fn primary_host(&self) -> &'static str {
        "money.finance.sina.com.cn"
    }

    async fn kline(
        &self,
        symbol: &Symbol,
        period: astock_core::KlinePeriod,
        _adjust: astock_core::Adjust,
        count: u32,
    ) -> Result<Fetched<Vec<Bar>>, DataError> {
        let scale = period
            .sina_scale()
            .ok_or(DataError::NoProvider("sina minute kline"))?;
        let params = vec![
            ("symbol".to_string(), symbol.sina()),
            ("scale".to_string(), scale.to_string()),
            ("ma".to_string(), "no".to_string()),
            ("datalen".to_string(), count.to_string()),
        ];
        let resp = self.http.get_text(SINA_KLINE_URL, &params).await?;
        let bars = Self::parse_body(&resp.body, symbol)?;
        Ok(Fetched::now(bars, Source::Sina))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_converts_share_volume_to_lots() {
        let sym = Symbol::new("600519").unwrap();
        let body = r#"[
            {"day":"2025-08-20","open":"1400.00","high":"1415.00","low":"1390.00","close":"1410.00","volume":"7714770"},
            {"day":"2025-08-21","open":"1412.00","high":"1418.00","low":"1400.00","close":"1405.50","volume":"6500000"}
        ]"#;
        let bars = SinaKline::parse_body(body, &sym).unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].volume, 77147.7); // shares ÷ 100 → lots
        assert_eq!(bars[0].volume_unit, VolumeUnit::Lots);
        assert_eq!(bars[1].close, 1405.5);
    }

    #[test]
    fn etf_volume_stays_in_fund_units() {
        let sym = Symbol::new("510300").unwrap();
        let body = r#"[{"day":"2025-08-21","open":"4.0","high":"4.1","low":"3.9","close":"4.05","volume":"99655963"}]"#;
        let bars = SinaKline::parse_body(body, &sym).unwrap();
        assert_eq!(bars[0].volume, 99655963.0);
        assert_eq!(bars[0].volume_unit, VolumeUnit::FundUnits);
    }
}
