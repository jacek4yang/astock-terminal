//! Sina kline provider (fallback).
//!
//! Ported from the legacy `_fetch_kline_sina`. Unadjusted but price-accurate.
//! scale: day=240, week=1200, month=7200. Volume quirk: A-shares report raw
//! shares (÷100 → lots); ETFs already report fund units (份) and pass through.

use crate::http::HttpClient;
use crate::provider::DataProvider;
use crate::providers::{fill_pct, json_f64};
use astock_core::time::{china_tz, parse_date, utc_now};
use astock_core::{
    normalize_security_name, Bar, DataError, Fetched, Quote, Source, Symbol, VolumeUnit,
};
use async_trait::async_trait;
use chrono::{NaiveDateTime, TimeZone, Utc};
use encoding_rs::GBK;
use std::sync::Arc;

/// Sina `CN_MarketData.getKLineData` endpoint.
pub const SINA_KLINE_URL: &str =
    "https://money.finance.sina.com.cn/quotes_service/api/json_v2.php/CN_MarketData.getKLineData";
/// Sina GBK realtime quote endpoint. It rejects requests without a finance
/// page Referer, so the provider supplies that header explicitly.
pub const SINA_QUOTE_URL: &str = "https://hq.sinajs.cn/list=";

/// Sina kline adapter.
pub struct SinaKline {
    http: Arc<HttpClient>,
}

impl SinaKline {
    /// Wrap the shared HTTP client.
    pub fn new(http: Arc<HttpClient>) -> Self {
        SinaKline { http }
    }

    fn parse_quote_body(
        body: &str,
        symbol: &Symbol,
        fetched_at: chrono::DateTime<Utc>,
    ) -> Result<Quote, DataError> {
        let expected_prefix = format!("var hq_str_{}=", symbol.sina());
        let normalized_body = body.trim_start_matches('\u{feff}').trim_start();
        if !normalized_body.starts_with(&expected_prefix) {
            let response_prefix = normalized_body
                .chars()
                .take(80)
                .flat_map(char::escape_default)
                .collect::<String>();
            return Err(DataError::Parse {
                upstream: format!("sina quote {symbol}"),
                message: format!(
                    "security identity mismatch in response variable: {response_prefix}"
                ),
            });
        }
        let payload = normalized_body
            .split_once('"')
            .and_then(|(_, tail)| tail.rsplit_once('"').map(|(value, _)| value))
            .ok_or_else(|| DataError::Parse {
                upstream: format!("sina quote {symbol}"),
                message: "missing quoted quote payload".to_string(),
            })?;
        let fields = payload.split(',').collect::<Vec<_>>();
        if fields.len() <= 31 {
            return Err(DataError::Parse {
                upstream: format!("sina quote {symbol}"),
                message: format!("expected at least 32 fields, received {}", fields.len()),
            });
        }
        let number = |index: usize, label: &str| {
            fields[index]
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .ok_or_else(|| DataError::Parse {
                    upstream: format!("sina quote {symbol}"),
                    message: format!("invalid {label} at field {index}"),
                })
        };
        let positive = |index: usize, label: &str| {
            number(index, label).and_then(|value| {
                if value > 0.0 {
                    Ok(value)
                } else {
                    Err(DataError::Empty(format!(
                        "sina quote {symbol}: missing or non-positive {label}"
                    )))
                }
            })
        };
        let name = normalize_security_name(fields[0]);
        if name.is_empty() {
            return Err(DataError::Empty(format!(
                "sina quote {symbol}: missing security name"
            )));
        }
        let open = positive(1, "open")?;
        let pre_close = positive(2, "pre-close")?;
        let price = positive(3, "price")?;
        let timestamp = NaiveDateTime::parse_from_str(
            &format!("{} {}", fields[30], fields[31]),
            "%Y-%m-%d %H:%M:%S",
        )
        .ok()
        .and_then(|value| china_tz().from_local_datetime(&value).single())
        .map(|value| value.with_timezone(&Utc))
        .ok_or_else(|| DataError::Parse {
            upstream: format!("sina quote {symbol}"),
            message: format!("invalid market timestamp {} {}", fields[30], fields[31]),
        })?;
        let change = price - pre_close;
        let mut field_provenance = std::collections::BTreeMap::new();
        for field in [
            "name",
            "price",
            "open",
            "high",
            "low",
            "pre_close",
            "volume",
            "amount",
        ] {
            let mut provenance = astock_core::FieldProvenance::reported("sina", timestamp);
            provenance.fetched_at = fetched_at;
            field_provenance.insert(field.to_string(), provenance);
        }
        for field in ["change", "pct"] {
            let mut provenance = astock_core::FieldProvenance::reported("sina", timestamp);
            provenance.fetched_at = fetched_at;
            provenance.quality = astock_core::DataQuality::Derived;
            field_provenance.insert(field.to_string(), provenance);
        }
        field_provenance.insert(
            "turnover".to_string(),
            astock_core::FieldProvenance::missing("sina", "新浪快照不包含换手率"),
        );
        Ok(Quote {
            symbol: symbol.code().to_string(),
            name,
            price,
            open,
            high: positive(4, "high")?,
            low: positive(5, "low")?,
            pre_close,
            // Sina reports A-share volume in shares. Quote's contract is lots.
            volume: number(8, "volume")? / 100.0,
            amount: number(9, "amount")?,
            change,
            pct: change / pre_close * 100.0,
            turnover: None,
            timestamp,
            field_provenance,
        })
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

    async fn quote(&self, symbol: &Symbol) -> Result<Fetched<Quote>, DataError> {
        if !symbol.is_current_a_share() {
            return Err(DataError::NoProvider("sina quote (仅当前A股)"));
        }
        let url = format!("{SINA_QUOTE_URL}{}", symbol.sina());
        let response = self
            .http
            .get_text_with_headers(
                &url,
                &[(
                    "Referer".to_string(),
                    "https://finance.sina.com.cn/".to_string(),
                )],
                &[],
            )
            .await?;
        let (decoded, _, malformed) = GBK.decode(&response.body_bytes);
        if malformed {
            return Err(DataError::Parse {
                upstream: format!("sina quote {symbol}"),
                message: "GBK response contains malformed bytes".to_string(),
            });
        }
        let fetched_at = utc_now();
        let quote = Self::parse_quote_body(&decoded, symbol, fetched_at)?;
        Ok(Fetched {
            data: quote,
            source: Source::Sina,
            fetched_at,
        })
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

    #[test]
    fn parses_gbk_quote_and_converts_share_volume_to_lots() {
        let symbol = Symbol::new("300308").unwrap();
        let fetched_at = "2026-08-25T01:20:00Z".parse().unwrap();
        let body = r#"var hq_str_sz300308="中际旭创,945.000,943.000,870.220,949.730,850.000,870.220,870.210,38909300,34491670420.000,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,2026-08-24,16:14:27,00";"#;
        let quote = SinaKline::parse_quote_body(body, &symbol, fetched_at).unwrap();
        assert_eq!(quote.name, "中际旭创");
        assert_eq!(quote.price, 870.22);
        assert_eq!(quote.volume, 389_093.0);
        assert_eq!(quote.amount, 34_491_670_420.0);
        assert_eq!(quote.timestamp.to_rfc3339(), "2026-08-24T08:14:27+00:00");
    }

    #[test]
    fn quote_parser_rejects_wrong_identity_and_zero_price() {
        let symbol = Symbol::new("300308").unwrap();
        let fetched_at = "2026-08-25T01:20:00Z".parse().unwrap();
        let wrong = r#"var hq_str_sz000001="平安银行,10,9,10,10,9,0,0,1,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,2026-08-24,15:00:00,00";"#;
        assert!(matches!(
            SinaKline::parse_quote_body(wrong, &symbol, fetched_at),
            Err(DataError::Parse { message, .. }) if message.contains("identity mismatch")
        ));
        let zero = r#"var hq_str_sz300308="中际旭创,945,943,0,949,850,0,0,1,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,2026-08-24,15:00:00,00";"#;
        assert!(matches!(
            SinaKline::parse_quote_body(zero, &symbol, fetched_at),
            Err(DataError::Empty(message)) if message.contains("non-positive price")
        ));
    }
}
