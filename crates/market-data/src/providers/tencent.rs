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
use astock_core::time::{china_tz, parse_date, utc_now};
use astock_core::{
    normalize_security_name, Adjust, Bar, DataError, Fetched, KlinePeriod, Quote, Source,
    StockListItem, Symbol, VolumeUnit,
};
use async_trait::async_trait;
use chrono::{NaiveDateTime, TimeZone, Utc};
use encoding_rs::GBK;
use futures::stream::{self, StreamExt};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::debug;

use crate::provider::DataProvider;

/// Tencent fqkline endpoint.
pub const TENCENT_KLINE_URL: &str = "https://web.ifzq.gtimg.cn/appstock/app/fqkline/get";
/// Tencent GBK quote endpoint; the symbol is appended to the URL path as
/// `q=sh600519` / `q=sz300308`.
pub const TENCENT_QUOTE_URL: &str = "https://qt.gtimg.cn/q=";
const HOST_KEY: &str = "web.ifzq.gtimg.cn";
const QUOTE_HOST_KEY: &str = "qt.gtimg.cn";
const TENCENT_QUOTE_BATCH: usize = 60;
const TENCENT_SNAPSHOT_CONCURRENCY: usize = 6;
const TENCENT_MARKET_SNAPSHOT_TTL: Duration = Duration::from_secs(2);

/// Tencent kline adapter.
pub struct TencentKline {
    http: Arc<HttpClient>,
    market_snapshot: Mutex<Option<(Instant, Fetched<Vec<StockListItem>>)>>,
}

impl TencentKline {
    /// Wrap the shared HTTP client.
    pub fn new(http: Arc<HttpClient>) -> Self {
        TencentKline {
            http,
            market_snapshot: Mutex::new(None),
        }
    }

    fn parse_quote_body(
        body: &str,
        symbol: &Symbol,
        fetched_at: chrono::DateTime<Utc>,
    ) -> Result<Quote, DataError> {
        let payload = body
            .split_once('"')
            .and_then(|(_, tail)| tail.rsplit_once('"').map(|(value, _)| value))
            .ok_or_else(|| DataError::Parse {
                upstream: format!("tencent quote {symbol}"),
                message: "missing quoted quote payload".to_string(),
            })?;
        let fields = payload.split('~').collect::<Vec<_>>();
        if fields.len() <= 38 {
            return Err(DataError::Parse {
                upstream: format!("tencent quote {symbol}"),
                message: format!("expected at least 39 fields, received {}", fields.len()),
            });
        }
        if fields[2] != symbol.code() {
            return Err(DataError::Parse {
                upstream: format!("tencent quote {symbol}"),
                message: format!(
                    "security identity mismatch: expected {}, received {}",
                    symbol.code(),
                    fields[2]
                ),
            });
        }
        let number = |index: usize, label: &str| {
            fields[index]
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .ok_or_else(|| DataError::Parse {
                    upstream: format!("tencent quote {symbol}"),
                    message: format!("invalid {label} at field {index}"),
                })
        };
        let positive = |index: usize, label: &str| {
            number(index, label).and_then(|value| {
                if value > 0.0 {
                    Ok(value)
                } else {
                    Err(DataError::Empty(format!(
                        "tencent quote {symbol}: missing or non-positive {label}"
                    )))
                }
            })
        };
        let name = normalize_security_name(fields[1]);
        if name.is_empty() {
            return Err(DataError::Empty(format!(
                "tencent quote {symbol}: missing security name"
            )));
        }
        let price = positive(3, "price")?;
        let pre_close = positive(4, "pre-close")?;
        let timestamp = NaiveDateTime::parse_from_str(fields[30], "%Y%m%d%H%M%S")
            .ok()
            .and_then(|value| china_tz().from_local_datetime(&value).single())
            .map(|value| value.with_timezone(&Utc))
            .ok_or_else(|| DataError::Parse {
                upstream: format!("tencent quote {symbol}"),
                message: format!("invalid market timestamp {}", fields[30]),
            })?;
        let exact_amount = fields[35]
            .split('/')
            .nth(2)
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value >= 0.0)
            .or_else(|| {
                number(37, "amount in ten-thousand CNY")
                    .ok()
                    .map(|value| value * 10_000.0)
            })
            .ok_or_else(|| DataError::Parse {
                upstream: format!("tencent quote {symbol}"),
                message: "missing turnover amount".to_string(),
            })?;
        let mut field_provenance = std::collections::BTreeMap::new();
        for field in [
            "name",
            "price",
            "pre_close",
            "volume",
            "amount",
            "change",
            "pct",
            "turnover",
        ] {
            let mut provenance = astock_core::FieldProvenance::reported("tencent", timestamp);
            provenance.fetched_at = fetched_at;
            field_provenance.insert(field.to_string(), provenance);
        }
        for (field, index) in [("open", 5), ("high", 33), ("low", 34)] {
            let value = number(index, field)?;
            let provenance = if value > 0.0 {
                let mut provenance = astock_core::FieldProvenance::reported("tencent", timestamp);
                provenance.fetched_at = fetched_at;
                provenance
            } else {
                astock_core::FieldProvenance::missing(
                    "tencent",
                    format!("集合竞价/停牌阶段未返回{field}"),
                )
            };
            field_provenance.insert(field.to_string(), provenance);
        }
        Ok(Quote {
            symbol: symbol.code().to_string(),
            name,
            price,
            open: number(5, "open")?,
            high: number(33, "high")?,
            low: number(34, "low")?,
            pre_close,
            volume: number(6, "volume")?,
            amount: exact_amount,
            change: number(31, "change")?,
            pct: number(32, "pct")?,
            turnover: number(38, "turnover").ok(),
            timestamp,
            field_provenance,
        })
    }

    async fn fetch_quote_batch(
        &self,
        symbols: Vec<Symbol>,
    ) -> Result<Vec<StockListItem>, DataError> {
        let query = symbols
            .iter()
            .map(Symbol::tencent)
            .collect::<Vec<_>>()
            .join(",");
        let response = self
            .http
            .get_text(&format!("{TENCENT_QUOTE_URL}{query}"), &[])
            .await?;
        let (decoded, _, malformed) = GBK.decode(&response.body_bytes);
        if malformed {
            return Err(DataError::Parse {
                upstream: "tencent market snapshot".to_string(),
                message: "GBK response contains malformed bytes".to_string(),
            });
        }
        let lines = decoded
            .lines()
            .filter_map(|line| {
                let payload = line
                    .split_once('"')
                    .and_then(|(_, tail)| tail.rsplit_once('"').map(|(value, _)| value))?;
                let code = payload.split('~').nth(2)?;
                Some((code.to_string(), line))
            })
            .collect::<std::collections::HashMap<_, _>>();
        let fetched_at = utc_now();
        let rows = symbols
            .iter()
            .filter_map(|symbol| {
                let line = lines.get(symbol.code())?;
                let quote = Self::parse_quote_body(line, symbol, fetched_at).ok()?;
                Some(StockListItem {
                    code: quote.symbol,
                    name: quote.name,
                    price: Some(quote.price),
                    pct: Some(quote.pct),
                    amount: (quote.amount > 0.0).then_some(quote.amount),
                })
            })
            .collect::<Vec<_>>();
        if rows.len() * 10 < symbols.len() * 9 {
            return Err(DataError::Empty(format!(
                "tencent quote batch coverage {}/{}",
                rows.len(),
                symbols.len()
            )));
        }
        Ok(rows)
    }

    /// Complete current Shanghai/Shenzhen stock snapshot. Tencent's public
    /// quote endpoint supports comma-separated symbols, so this provides a
    /// real-price fallback when an EastMoney clist host returns placeholder
    /// zeros instead of silently degrading the Agent candidate universe.
    pub async fn market_snapshot(
        &self,
        securities: &[StockListItem],
    ) -> Result<Fetched<Vec<StockListItem>>, DataError> {
        let mut cache = self.market_snapshot.lock().await;
        if let Some((stored_at, fetched)) = cache.as_ref() {
            if stored_at.elapsed() <= TENCENT_MARKET_SNAPSHOT_TTL {
                return Ok(fetched.clone());
            }
        }
        let batches = securities
            .chunks(TENCENT_QUOTE_BATCH)
            .map(|chunk| {
                chunk
                    .iter()
                    .filter_map(|item| Symbol::new(&item.code).ok())
                    .collect::<Vec<_>>()
            })
            .filter(|batch| !batch.is_empty())
            .collect::<Vec<_>>();
        let requested = batches.iter().map(Vec::len).sum::<usize>();
        let results = stream::iter(batches)
            .map(|batch| async move { self.fetch_quote_batch(batch).await })
            .buffer_unordered(TENCENT_SNAPSHOT_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        let mut rows = Vec::with_capacity(requested);
        let mut failures = Vec::new();
        for result in results {
            match result {
                Ok(mut batch) => rows.append(&mut batch),
                Err(error) => failures.push(error.to_string()),
            }
        }
        let required = requested.saturating_mul(90).div_ceil(100);
        if rows.len() < required || rows.len() < 4_000 {
            return Err(DataError::AllFailed {
                op: "tencent market snapshot",
                details: format!(
                    "coverage {}/{requested}, required {required}; {}",
                    rows.len(),
                    failures.join("; ")
                ),
            });
        }
        let fetched = Fetched::now(rows, Source::Tencent);
        *cache = Some((Instant::now(), fetched.clone()));
        Ok(fetched)
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
        self.index_kline_period(index_code, KlinePeriod::Day, count)
            .await
    }

    /// Index kline for a supported day/week/month period. Adjustment is
    /// intentionally disabled because indices have no corporate actions.
    pub async fn index_kline_period(
        &self,
        index_code: &str,
        period: KlinePeriod,
        count: u32,
    ) -> Result<Vec<Bar>, DataError> {
        let tc = Symbol::index_tencent(index_code);
        self.fetch(&tc, period, Adjust::None, count, VolumeUnit::Lots)
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

    async fn quote(&self, symbol: &Symbol) -> Result<Fetched<Quote>, DataError> {
        let url = format!("{TENCENT_QUOTE_URL}{}", symbol.tencent());
        let response = self.http.get_text(&url, &[]).await?;
        let (decoded, _, malformed) = GBK.decode(&response.body_bytes);
        if malformed {
            self.http.on_failure(QUOTE_HOST_KEY);
            return Err(DataError::Parse {
                upstream: format!("tencent quote {symbol}"),
                message: "GBK response contains malformed bytes".to_string(),
            });
        }
        let fetched_at = utc_now();
        let quote = Self::parse_quote_body(&decoded, symbol, fetched_at)?;
        Ok(Fetched {
            data: quote,
            source: Source::Tencent,
            fetched_at,
        })
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

    #[test]
    fn parses_gbk_quote_fields_units_and_market_time() {
        let symbol = Symbol::new("300308").unwrap();
        let fetched_at = "2026-08-25T00:51:35Z".parse().unwrap();
        let body = r#"v_sz300308="51~中际旭创~300308~870.22~943.00~945.00~389093~166564~222529~870.22~8~870.11~1~870.08~1~870.07~1~870.05~1~870.60~1~870.63~1~870.70~2~870.71~16~870.72~10~~20260824161427~-72.78~-7.72~949.73~850.00~870.22/389093/34491670420~389093~3449167~3.51~49.77";"#;
        let quote = TencentKline::parse_quote_body(body, &symbol, fetched_at).unwrap();
        assert_eq!(quote.name, "中际旭创");
        assert_eq!(quote.price, 870.22);
        assert_eq!(quote.pre_close, 943.0);
        assert_eq!(quote.volume, 389_093.0);
        assert_eq!(quote.amount, 34_491_670_420.0);
        assert_eq!(quote.timestamp.to_rfc3339(), "2026-08-24T08:14:27+00:00");
        assert_eq!(quote.field_provenance["price"].fetched_at, fetched_at);
    }

    #[test]
    fn auction_zero_ohlc_is_marked_missing_instead_of_reported() {
        let symbol = Symbol::new("300308").unwrap();
        let fetched_at = "2026-08-25T00:51:35Z".parse().unwrap();
        let body = r#"v_sz300308="51~中际旭创~300308~851.00~870.22~0.00~0~0~0~851.00~1~0~0~0~0~0~0~0~0~851.00~1~0~0~0~0~0~0~0~0~~20260825091500~-19.22~-2.21~0.00~0.00~851.00/0/0~0~0~0.00";"#;
        let quote = TencentKline::parse_quote_body(body, &symbol, fetched_at).unwrap();
        assert_eq!(quote.price, 851.0);
        for field in ["open", "high", "low"] {
            assert_eq!(
                quote.field_provenance[field].quality,
                astock_core::DataQuality::Missing
            );
        }
    }

    #[test]
    fn quote_parser_rejects_zero_placeholder_and_wrong_identity() {
        let symbol = Symbol::new("300308").unwrap();
        let fetched_at = "2026-08-25T00:51:35Z".parse().unwrap();
        let zero = r#"v_sz300308="51~中际旭创~300308~0~943~945~1~~~~~~~~~~~~~~~~~~~~~~~~20260824161427~-943~-100~949~850~0/1/1~1~0~0~0";"#;
        assert!(TencentKline::parse_quote_body(zero, &symbol, fetched_at).is_err());
        let wrong = r#"v_sz000001="51~平安银行~000001~10~9~9~1~~~~~~~~~~~~~~~~~~~~~~~~20260824161427~1~1~10~9~10/1/1~1~0~0~0";"#;
        assert!(matches!(
            TencentKline::parse_quote_body(wrong, &symbol, fetched_at),
            Err(DataError::Parse { message, .. }) if message.contains("identity mismatch")
        ));
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
