//! Tencent kline provider (primary source).
//!
//! Ported from the legacy `_fetch_kline_tencent` / `_fetch_kline_tencent_index`.
//! Quirks preserved:
//! - Response Content-Type is `text/html` even for valid JSON — sniff the body.
//! - A body starting `<!DOCTYPE` / `<html` means the WAF served a challenge page.
//! - Row order is **date, open, close, high, low, volume** (close before high).
//! - Volume unit: lots (手) for A-shares, fund units (份) for ETFs.

use crate::http::HttpClient;
use crate::providers::{fill_pct, json_f64};
use astock_core::time::parse_date;
use astock_core::{Adjust, Bar, DataError, Fetched, KlinePeriod, Source, Symbol, VolumeUnit};
use async_trait::async_trait;
use std::sync::Arc;
use tracing::debug;

use crate::provider::DataProvider;

/// Tencent fqkline endpoint.
pub const TENCENT_KLINE_URL: &str = "https://web.ifzq.gtimg.cn/appstock/app/fqkline/get";
const HOST_KEY: &str = "web.ifzq.gtimg.cn";

/// Tencent kline adapter.
pub struct TencentKline {
    http: Arc<HttpClient>,
}

impl TencentKline {
    /// Wrap the shared HTTP client.
    pub fn new(http: Arc<HttpClient>) -> Self {
        TencentKline { http }
    }

    /// Fetch and parse bars for an already-converted Tencent symbol
    /// (`sh600519` / `sz399001`). `volume_unit` decides how volume is tagged.
    pub async fn fetch(
        &self,
        tc_symbol: &str,
        period: KlinePeriod,
        adjust: Adjust,
        count: u32,
        volume_unit: VolumeUnit,
    ) -> Result<Vec<Bar>, DataError> {
        let period_token = period
            .tencent_token()
            .ok_or(DataError::NoProvider("tencent minute kline"))?;
        let fq = adjust.tencent_token();
        let param = format!("{tc_symbol},{period_token},,,{count},{fq}");
        let resp = self
            .http
            .get_text(TENCENT_KLINE_URL, &[("param".to_string(), param)])
            .await?;

        let text = resp.body.trim();
        // 1. An HTML page up front is always the WAF challenge.
        if text.starts_with("<!DOCTYPE") || text.starts_with("<html") {
            self.http.on_failure(HOST_KEY);
            return Err(DataError::WafBlocked(format!("tencent kline {tc_symbol}")));
        }
        // 2. Parse JSON even when Content-Type claims text/html.
        let data: serde_json::Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                let looks_html = resp
                    .content_type
                    .as_deref()
                    .is_some_and(|ct| ct.contains("text/html"))
                    || text[..text.len().min(50)].contains('<');
                if looks_html {
                    self.http.on_failure(HOST_KEY);
                    return Err(DataError::WafBlocked(format!(
                        "tencent kline {tc_symbol} (JSON parse failed on HTML)"
                    )));
                }
                return Err(DataError::Parse {
                    upstream: format!("tencent {tc_symbol}"),
                    message: e.to_string(),
                });
            }
        };

        // 3. API-level error.
        let code = data.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 0 {
            return Err(DataError::Empty(format!(
                "tencent kline {tc_symbol} returned code={code}"
            )));
        }

        let stock_data = data
            .get("data")
            .and_then(|d| d.get(tc_symbol))
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        // Response key is `{fq}{period}` (e.g. `qfqday`); legacy fallback
        // order is the plain `day` then `week` key regardless of period.
        let key = format!("{fq}{period_token}");
        let rows = stock_data
            .get(&key)
            .or_else(|| stock_data.get("day"))
            .or_else(|| stock_data.get("week"))
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        if rows.is_empty() {
            return Err(DataError::Empty(format!(
                "tencent kline {tc_symbol} key={key}"
            )));
        }

        let mut bars = Vec::with_capacity(rows.len());
        for row in &rows {
            let Some(cols) = row.as_array() else { continue };
            if cols.len() < 6 {
                continue;
            }
            let date_str = cols[0].as_str().unwrap_or_default();
            let (Some(date), Some(open), Some(close), Some(high), Some(low)) = (
                parse_date(date_str),
                json_f64(&cols[1]),
                json_f64(&cols[2]),
                json_f64(&cols[3]),
                json_f64(&cols[4]),
            ) else {
                debug!(tc_symbol, %date_str, "skipping unparseable tencent row");
                continue;
            };
            let volume = json_f64(&cols[5]).unwrap_or(0.0);
            bars.push(Bar::new(date, open, close, high, low, volume, volume_unit));
        }
        if bars.is_empty() {
            return Err(DataError::Empty(format!(
                "tencent kline {tc_symbol}: no parseable rows"
            )));
        }
        fill_pct(&mut bars);
        Ok(bars)
    }

    /// Index kline fallback: unadjusted daily bars via the plain `day` key.
    pub async fn index_kline(&self, index_code: &str, count: u32) -> Result<Vec<Bar>, DataError> {
        let tc = Symbol::index_tencent(index_code);
        self.fetch(&tc, KlinePeriod::Day, Adjust::None, count, VolumeUnit::Lots)
            .await
    }
}

#[async_trait]
impl DataProvider for TencentKline {
    fn name(&self) -> &'static str {
        "tencent"
    }

    fn primary_host(&self) -> &'static str {
        HOST_KEY
    }

    async fn kline(
        &self,
        symbol: &Symbol,
        period: KlinePeriod,
        adjust: Adjust,
        count: u32,
    ) -> Result<Fetched<Vec<Bar>>, DataError> {
        let unit = if symbol.is_etf() {
            VolumeUnit::FundUnits
        } else {
            VolumeUnit::Lots
        };
        let bars = self
            .fetch(&symbol.tencent(), period, adjust, count, unit)
            .await?;
        Ok(Fetched::now(bars, Source::Tencent))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a captured Tencent payload body (fixture) into bars.
    fn parse_fixture(body: &str, tc_symbol: &str, key: &str) -> Vec<Bar> {
        let data: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(data["code"].as_i64().unwrap(), 0);
        let rows = data["data"][tc_symbol][key].as_array().unwrap().clone();
        let mut bars = Vec::new();
        for row in &rows {
            let cols = row.as_array().unwrap();
            bars.push(Bar::new(
                parse_date(cols[0].as_str().unwrap()).unwrap(),
                json_f64(&cols[1]).unwrap(),
                json_f64(&cols[2]).unwrap(),
                json_f64(&cols[3]).unwrap(),
                json_f64(&cols[4]).unwrap(),
                json_f64(&cols[5]).unwrap_or(0.0),
                VolumeUnit::Lots,
            ));
        }
        fill_pct(&mut bars);
        bars
    }

    #[test]
    fn parses_date_open_close_high_low_volume_order() {
        // Row order is date, open, CLOSE, high, low, volume — not OHLC.
        let body = r#"{"code":0,"data":{"sh600519":{"qfqday":[
            ["2025-08-20","1400.00","1410.00","1415.00","1390.00","65181"],
            ["2025-08-21","1412.00","1405.50","1418.00","1400.00",70234.5]
        ]}}}"#;
        let bars = parse_fixture(body, "sh600519", "qfqday");
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].open, 1400.0);
        assert_eq!(bars[0].close, 1410.0);
        assert_eq!(bars[0].high, 1415.0);
        assert_eq!(bars[0].low, 1390.0);
        assert_eq!(bars[0].volume, 65181.0);
        // pct computed from consecutive closes, rounded to 2dp.
        let expect = ((1405.5_f64 - 1410.0) / 1410.0 * 100.0 * 100.0).round() / 100.0;
        assert_eq!(bars[1].pct, Some(expect));
    }

    /// Contract guard: the same trading day, delivered in three upstream
    /// formats, must normalize to the SAME `Bar.volume` in lots (手).
    ///
    /// - Tencent `fqkline` rows carry volume in lots already → pass through;
    /// - Sina `getKLineData` reports raw shares for A-shares → ÷100;
    /// - EastMoney kline CSV column 5 carries lots → pass through.
    ///
    /// Ground truth is the golden fixture `fixtures/golden/600519_day.json`
    /// (legacy pipeline output, volumes in lots). Each golden bar is
    /// re-encoded into the three raw upstream layouts and re-parsed.
    #[test]
    fn volume_unit_normalization_agrees_across_upstreams() {
        use crate::providers::eastmoney::EastMoney;
        use crate::providers::sina::SinaKline;
        use std::path::PathBuf;

        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/golden/600519_day.json");
        let golden: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("golden fixture readable"))
                .unwrap();
        let klines = golden["inputs"]["klines"].as_array().unwrap();
        assert!(klines.len() >= 30, "golden fixture too small");

        let sym = Symbol::new("600519").unwrap();
        for k in klines.iter().take(30) {
            let date = k["date"].as_str().unwrap();
            let open = k["open"].as_f64().unwrap();
            let close = k["close"].as_f64().unwrap();
            let high = k["high"].as_f64().unwrap();
            let low = k["low"].as_f64().unwrap();
            let lots = k["volume"].as_f64().unwrap();
            let amount = k["amount"].as_f64().unwrap();

            // Tencent raw row: date, open, close, high, low, volume(手).
            let tc_body = format!(
                r#"{{"code":0,"data":{{"sh600519":{{"qfqday":[["{date}","{open}","{close}","{high}","{low}","{lots}"]]}}}}}}"#
            );
            let tc = parse_fixture(&tc_body, "sh600519", "qfqday");

            // Sina raw item: volume in shares for A-shares.
            let sina_body = format!(
                r#"[{{"day":"{date}","open":"{open}","high":"{high}","low":"{low}","close":"{close}","volume":"{}"}}]"#,
                lots * 100.0
            );
            let sina = SinaKline::parse_body(&sina_body, &sym).unwrap();

            // EastMoney raw CSV row: date,open,close,high,low,volume(手),amount,...
            let em_row = format!("{date},{open},{close},{high},{low},{lots},{amount},0,0,0,0");
            let em = EastMoney::parse_kline_rows(&[em_row], VolumeUnit::Lots);

            for (label, bar) in [
                ("tencent", &tc[0]),
                ("sina", &sina[0]),
                ("eastmoney", &em[0]),
            ] {
                assert_eq!(bar.volume_unit, VolumeUnit::Lots, "{label} unit on {date}");
                assert!(
                    (bar.volume - lots).abs() < 0.01,
                    "{label} volume {} != golden lots {lots} on {date}",
                    bar.volume
                );
            }
        }
    }
}
