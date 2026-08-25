//! EastMoney provider: kline fallback + amount/turnover enrichment + quote,
//! fund flow, minute data, search, full-market list, and breadth.
//!
//! Ported from the legacy `kline_fetcher.py`, including host pools, the
//! `ut` token, CSV column layouts, and the clist pagination scheme.

use crate::cache::{ttl, TtlCache};
use crate::http::{HttpClient, EM_TOKEN};
use crate::provider::DataProvider;
use crate::providers::{fill_pct, json_f64, strip_jsonp};
use astock_core::time::{parse_date, parse_datetime_flexible};
use astock_core::{
    normalize_security_name, Adjust, Bar, DataError, Fetched, FundFlowPoint, KlinePeriod,
    MarketBreadth, MinuteData, MinutePoint, Quote, SearchResult, Source, StockListItem, Symbol,
    VolumeUnit,
};
use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use std::sync::Arc;

/// Quote/minute/clist host pool.
pub const QUOTE_HOSTS: [&str; 4] = [
    "https://push2delay.eastmoney.com",
    "https://push2.eastmoney.com",
    "https://82.push2.eastmoney.com",
    "https://90.push2.eastmoney.com",
];

/// History (fund-flow daily) host pool.
pub const HIS_HOSTS: [&str; 4] = [
    "https://push2his.eastmoney.com",
    "https://82.push2his.eastmoney.com",
    "https://90.push2his.eastmoney.com",
    "https://push2delay.eastmoney.com",
];

/// Kline host pool.
pub const EM_KLINE_HOSTS: [&str; 3] = [
    "https://push2his.eastmoney.com",
    "https://90.push2his.eastmoney.com",
    "https://82.push2his.eastmoney.com",
];

/// Realtime fund-flow host pool. push2test is deliberately excluded:
/// its `fflow/kline` endpoint returns 0 rows (legacy quirk).
pub const RT_FLOW_HOSTS: [&str; 4] = [
    "https://push2delay.eastmoney.com",
    "https://push2.eastmoney.com",
    "https://82.push2.eastmoney.com",
    "https://90.push2.eastmoney.com",
];

const SEARCH_HOST: &str = "https://searchapi.eastmoney.com";
const KLINE_FIELDS1: &str = "f1,f2,f3,f4,f5,f6";
const KLINE_FIELDS2: &str = "f51,f52,f53,f54,f55,f56,f57,f58,f59,f60,f61";
const FFLOW_FIELDS1: &str = "f1,f2,f3,f7";
const FFLOW_FIELDS2: &str = "f51,f52,f53,f54,f55,f56,f57";
const CLIST_FS: &str = "m:0+t:6,m:0+t:80,m:1+t:2,m:1+t:23,m:0+t:81+s:2048";
const CLIST_PAGE_SIZE: u32 = 100;
const CLIST_CONCURRENCY: usize = 10;
/// Production nodes may cap a whole-market request at 100 rows.  We probe the
/// production pool, validate `diff.len() == data.total`, then use the complete
/// paginated path.  A `push2test` response must never enter production: it
/// contains stale NEEQ/test instruments and non-production prices.
const MARKET_SNAPSHOT_PAGE_SIZE: u32 = 6_000;
const MARKET_SNAPSHOT_HOSTS: [&str; 2] = [
    "https://push2delay.eastmoney.com",
    "https://push2.eastmoney.com",
];
const MIN_COMPLETE_A_SHARE_ROWS: usize = 4_500;

/// One A-share with its EastMoney industry classification (clist `f100`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IndustryClassified {
    /// 6-digit ticker.
    pub code: String,
    /// Company short name.
    pub name: String,
    /// EastMoney industry tag, e.g. "酿酒行业".
    pub industry: String,
}

/// EastMoney adapter. All endpoints are reachable through this one provider.
pub struct EastMoney {
    http: Arc<HttpClient>,
    cache: Arc<TtlCache>,
    market_snapshot_gate: tokio::sync::Mutex<()>,
}

impl EastMoney {
    /// Wrap the shared HTTP client and cache.
    pub fn new(http: Arc<HttpClient>, cache: Arc<TtlCache>) -> Self {
        EastMoney {
            http,
            cache,
            market_snapshot_gate: tokio::sync::Mutex::new(()),
        }
    }

    /// EM GET against a host pool with the `ut` token attached.
    async fn em_json(
        &self,
        path: &str,
        mut params: Vec<(String, String)>,
        hosts: &[&str],
        op: &'static str,
    ) -> Result<serde_json::Value, DataError> {
        params.push(("ut".to_string(), EM_TOKEN.to_string()));
        self.http.get_json_pool(path, &params, hosts, op).await
    }

    /// Raw kline request against EM (any period / adjust). Returns the
    /// `data.klines` CSV rows.
    async fn kline_rows(
        &self,
        secid: &str,
        period: KlinePeriod,
        adjust: Adjust,
        count: u32,
    ) -> Result<Vec<String>, DataError> {
        let params = vec![
            ("secid".to_string(), secid.to_string()),
            ("fields1".to_string(), KLINE_FIELDS1.to_string()),
            ("fields2".to_string(), KLINE_FIELDS2.to_string()),
            ("klt".to_string(), period.eastmoney_klt().to_string()),
            ("fqt".to_string(), adjust.eastmoney_fqt().to_string()),
            ("lmt".to_string(), count.to_string()),
            ("end".to_string(), "20500101".to_string()),
        ];
        let data = self
            .em_json("/api/qt/stock/kline/get", params, &EM_KLINE_HOSTS, "kline")
            .await?;
        let rows = data
            .get("data")
            .and_then(|d| d.get("klines"))
            .and_then(|k| k.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if rows.is_empty() {
            return Err(DataError::Empty(format!("eastmoney kline {secid}")));
        }
        Ok(rows)
    }

    /// Parse EM kline CSV rows: date,open,close,high,low,volume,amount,
    /// amplitude,pct,change,turnover (indices 0,1,2,3,4,5,6,10 used).
    pub(crate) fn parse_kline_rows(rows: &[String], volume_unit: VolumeUnit) -> Vec<Bar> {
        let mut bars = Vec::with_capacity(rows.len());
        for line in rows {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() < 7 {
                continue;
            }
            let (Some(date), Some(open), Some(close), Some(high), Some(low)) = (
                parse_date(parts[0]),
                parts[1].parse::<f64>().ok(),
                parts[2].parse::<f64>().ok(),
                parts[3].parse::<f64>().ok(),
                parts[4].parse::<f64>().ok(),
            ) else {
                continue;
            };
            let volume = parts[5].parse::<f64>().unwrap_or(0.0);
            let mut bar = Bar::new(date, open, close, high, low, volume, volume_unit);
            bar.amount = parts[6].parse::<f64>().ok();
            if parts.len() >= 11 {
                bar.turnover = parts[10].parse::<f64>().ok();
            }
            bars.push(bar);
        }
        fill_pct(&mut bars);
        bars
    }

    /// Merge EM amount/turnover into bars fetched from another source,
    /// matched by date.
    ///
    /// Fixes vs. the legacy `_enrich_from_eastmoney`:
    /// - `klt` follows the requested period (legacy always asked for daily,
    ///   so weekly/monthly bars never matched);
    /// - callers skip this entirely when EM was already the kline source
    ///   (legacy fetched twice).
    pub async fn enrich(
        &self,
        symbol: &Symbol,
        period: KlinePeriod,
        count: u32,
        bars: &mut [Bar],
    ) -> Result<usize, DataError> {
        if bars.is_empty() {
            return Ok(0);
        }
        // Request extra rows so the EM date range covers the other source's.
        let request_count = (count + 60).min(500);
        let rows = self
            .kline_rows(&symbol.secid(), period, Adjust::None, request_count)
            .await?;
        let mut map: std::collections::HashMap<String, (Option<f64>, Option<f64>)> =
            std::collections::HashMap::new();
        for line in &rows {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() >= 11 {
                map.insert(
                    parts[0].to_string(),
                    (parts[6].parse::<f64>().ok(), parts[10].parse::<f64>().ok()),
                );
            }
        }
        let mut matched = 0;
        for bar in bars.iter_mut() {
            let key = bar.date.to_string();
            if let Some((amount, turnover)) = map.get(&key) {
                bar.amount = *amount;
                bar.turnover = *turnover;
                matched += 1;
            }
        }
        tracing::debug!(%symbol, matched, total = bars.len(), "eastmoney enrichment merged");
        Ok(matched)
    }

    async fn fetch_clist_page(
        &self,
        fields: &str,
        page: u32,
    ) -> Result<(Vec<serde_json::Value>, u32), DataError> {
        let params = vec![
            ("po".to_string(), "1".to_string()),
            ("np".to_string(), "1".to_string()),
            ("fltt".to_string(), "2".to_string()),
            ("fields".to_string(), fields.to_string()),
            ("fs".to_string(), CLIST_FS.to_string()),
            ("pz".to_string(), CLIST_PAGE_SIZE.to_string()),
            ("pn".to_string(), page.to_string()),
        ];
        let data = self
            .em_json("/api/qt/clist/get", params, &QUOTE_HOSTS, "clist")
            .await?;
        let d = data.get("data").cloned().unwrap_or(serde_json::Value::Null);
        let total = d.get("total").and_then(|t| t.as_u64()).unwrap_or(0) as u32;
        // `diff` is normally an array; some responses use an object keyed by
        // index — accept both.
        let diff = match d.get("diff") {
            Some(serde_json::Value::Array(a)) => a.clone(),
            Some(serde_json::Value::Object(o)) => o.values().cloned().collect(),
            _ => Vec::new(),
        };
        if diff.is_empty() {
            return Err(DataError::Empty(format!("clist page {page}")));
        }
        Ok((diff, total))
    }

    /// Fetch page 1 for the row count, then pages 2..=N concurrently
    /// (bounded). Page 1 is fetched exactly once — the legacy code fetched it
    /// twice (once for rows, once for `total`).
    async fn fetch_clist_all(&self, fields: &str) -> Result<Vec<serde_json::Value>, DataError> {
        let (first, total) = self.fetch_clist_page(fields, 1).await?;
        let total_pages = if total > 0 {
            total.div_ceil(CLIST_PAGE_SIZE)
        } else {
            59 // legacy fallback estimate
        };
        let mut rows = first;
        if total_pages > 1 {
            let rest: Vec<_> = stream::iter(2..=total_pages)
                .map(|pn| async move { (pn, self.fetch_clist_page(fields, pn).await) })
                .buffer_unordered(CLIST_CONCURRENCY)
                .collect()
                .await;
            let mut missing_pages = Vec::new();
            for (page_number, page) in rest {
                match page {
                    Ok((diff, _)) => rows.extend(diff),
                    Err(error) => {
                        tracing::warn!(page = page_number, %error, "clist page failed; retrying");
                        missing_pages.push(page_number);
                    }
                }
            }
            let mut retry_failures = Vec::new();
            for page_number in missing_pages {
                tokio::time::sleep(crate::http::RETRY_PAUSE).await;
                match self.fetch_clist_page(fields, page_number).await {
                    Ok((diff, _)) => rows.extend(diff),
                    Err(error) => retry_failures.push(format!("page {page_number}: {error}")),
                }
            }
            if !retry_failures.is_empty() {
                return Err(DataError::AllFailed {
                    op: "clist complete pagination",
                    details: retry_failures.join("; "),
                });
            }
        }
        if total > 0 && rows.len() < total as usize {
            return Err(DataError::Empty(format!(
                "clist incomplete: expected {total} rows, received {}",
                rows.len()
            )));
        }
        Ok(rows)
    }

    /// Fetch one complete A-share snapshot. Each host is an independent retry
    /// with UA rotation and adaptive host throttling. A partial response is
    /// never cached as complete; if no host supports the large page we fall
    /// back to the validated paginated path above.
    async fn fetch_market_snapshot_rows(
        &self,
        fields: &str,
    ) -> Result<Vec<serde_json::Value>, DataError> {
        let params = vec![
            ("po".to_string(), "1".to_string()),
            ("np".to_string(), "1".to_string()),
            ("fltt".to_string(), "2".to_string()),
            ("fields".to_string(), fields.to_string()),
            ("fs".to_string(), CLIST_FS.to_string()),
            ("pz".to_string(), MARKET_SNAPSHOT_PAGE_SIZE.to_string()),
            ("pn".to_string(), "1".to_string()),
            ("ut".to_string(), EM_TOKEN.to_string()),
        ];
        let mut failures = Vec::new();
        for (attempt, host) in MARKET_SNAPSHOT_HOSTS.iter().enumerate() {
            if attempt > 0 {
                self.http.rotate_ua();
                tokio::time::sleep(crate::http::RETRY_PAUSE * attempt as u32).await;
            }
            let url = format!("{host}/api/qt/clist/get");
            match self.http.get_json(&url, &params).await {
                Ok(value) => {
                    let data = value.get("data").unwrap_or(&serde_json::Value::Null);
                    let total = data.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let rows = clist_diff_rows(data);
                    if total >= MIN_COMPLETE_A_SHARE_ROWS && rows.len() >= total {
                        tracing::debug!(
                            host,
                            total,
                            rows = rows.len(),
                            "complete market snapshot fetched"
                        );
                        return Ok(rows);
                    }
                    failures.push(format!(
                        "{host}: incomplete snapshot, expected {total}, received {}",
                        rows.len()
                    ));
                }
                Err(error) => failures.push(format!("{host}: {error}")),
            }
        }
        tracing::warn!(failures = %failures.join("; "), "large-page market snapshot failed; using complete pagination");
        self.fetch_clist_all(fields)
            .await
            .map_err(|error| DataError::AllFailed {
                op: "eastmoney complete market snapshot",
                details: format!(
                    "large-page retries: {}; pagination: {error}",
                    failures.join("; ")
                ),
            })
    }

    fn stock_items_from_rows(rows: Vec<serde_json::Value>) -> Vec<StockListItem> {
        rows.into_iter()
            .filter_map(|row| {
                let code = row.get("f12").and_then(|v| v.as_str())?.trim();
                if code.len() != 6 {
                    return None;
                }
                let symbol = Symbol::new(code).ok()?;
                if !symbol.is_current_a_share() {
                    return None;
                }
                let name =
                    normalize_security_name(row.get("f14").and_then(|v| v.as_str()).unwrap_or(""));
                if name.is_empty() {
                    return None;
                }
                Some(StockListItem {
                    code: code.to_string(),
                    name,
                    price: row
                        .get("f2")
                        .and_then(json_f64)
                        .filter(|value| value.is_finite() && *value > 0.0),
                    pct: row.get("f3").and_then(json_f64),
                    amount: row
                        .get("f6")
                        .and_then(json_f64)
                        .filter(|value| value.is_finite() && *value > 0.0),
                })
            })
            .collect()
    }

    fn validate_snapshot_trade_fields(
        items: &[StockListItem],
        minimum_rows: usize,
    ) -> Result<(), DataError> {
        if items.len() < minimum_rows {
            return Err(DataError::Empty(format!(
                "market snapshot incomplete: only {} A-share rows",
                items.len()
            )));
        }
        let price_present = items.iter().filter(|item| item.price.is_some()).count();
        let amount_present = items.iter().filter(|item| item.amount.is_some()).count();
        if price_present * 10 < items.len() * 9 || amount_present * 5 < items.len() * 4 {
            return Err(DataError::Empty(format!(
                "market snapshot contains placeholder trade fields: price {price_present}/{}, amount {amount_present}/{}",
                items.len(),
                items.len()
            )));
        }
        Ok(())
    }

    fn breadth_from_items(items: &[StockListItem]) -> Result<MarketBreadth, DataError> {
        if items.len() < MIN_COMPLETE_A_SHARE_ROWS {
            return Err(DataError::Empty(format!(
                "market breadth incomplete: only {} A-share rows",
                items.len()
            )));
        }
        let pct_present = items.iter().filter(|item| item.pct.is_some()).count();
        if pct_present * 10 < items.len() * 9 {
            return Err(DataError::Empty(format!(
                "market breadth incomplete: only {pct_present}/{} rows contain change percent",
                items.len()
            )));
        }
        Ok(count_breadth(items.iter().map(|item| item.pct)))
    }

    async fn complete_market_snapshot(&self) -> Result<Fetched<Vec<StockListItem>>, DataError> {
        if let Some(hit) = self
            .cache
            .get::<Fetched<Vec<StockListItem>>>("all_a_shares", ttl::ALL_A)
        {
            return Ok(hit);
        }
        let _guard = self.market_snapshot_gate.lock().await;
        // A concurrent all-A/breadth request may have warmed the cache while
        // this caller waited. Re-check before touching any upstream.
        if let Some(hit) = self
            .cache
            .get::<Fetched<Vec<StockListItem>>>("all_a_shares", ttl::ALL_A)
        {
            return Ok(hit);
        }
        let rows = self.fetch_market_snapshot_rows("f2,f3,f6,f12,f14").await?;
        let items = Self::stock_items_from_rows(rows);
        Self::validate_snapshot_trade_fields(&items, MIN_COMPLETE_A_SHARE_ROWS)?;
        let breadth = Self::breadth_from_items(&items)?;
        let fetched = Fetched::now(items, Source::EastMoney);
        let breadth_fetched = Fetched {
            data: breadth,
            source: fetched.source,
            fetched_at: fetched.fetched_at,
        };
        self.cache.set("all_a_shares", &fetched);
        self.cache.set("market_breadth", &breadth_fetched);
        Ok(fetched)
    }

    async fn quote_by_secid(
        &self,
        code: &str,
        secid: &str,
        cache_namespace: &'static str,
    ) -> Result<Fetched<Quote>, DataError> {
        let key = format!("{cache_namespace}_{secid}");
        if let Some(hit) = self.cache.get::<Fetched<Quote>>(&key, ttl::REALTIME) {
            return Ok(hit);
        }
        let params = vec![
            ("secid".to_string(), secid.to_string()),
            (
                "fields".to_string(),
                "f43,f44,f45,f46,f47,f48,f57,f58,f60,f169,f170,f168".to_string(),
            ),
            ("fltt".to_string(), "2".to_string()),
            ("invt".to_string(), "2".to_string()),
        ];
        let data = self
            .em_json("/api/qt/stock/get", params, &QUOTE_HOSTS, cache_namespace)
            .await?;
        let d = data.get("data").cloned().unwrap_or(serde_json::Value::Null);
        if d.is_null() {
            return Err(DataError::Empty(format!(
                "eastmoney {cache_namespace} {secid}"
            )));
        }
        let number = |field: &str| {
            d.get(field)
                .and_then(json_f64)
                .filter(|value| value.is_finite())
        };
        let positive_number = |field: &str| number(field).filter(|value| *value > 0.0);
        let (name, price, pre_close) = parse_quote_identity(&d, code, secid, cache_namespace)?;
        let timestamp = astock_core::time::utc_now();
        let mut field_provenance = std::collections::BTreeMap::new();
        field_provenance.insert(
            "name".to_string(),
            if name.is_empty() {
                astock_core::FieldProvenance::missing("eastmoney", "上游未返回标准证券名称")
            } else {
                astock_core::FieldProvenance::reported("eastmoney", timestamp)
            },
        );
        for field in ["price", "pre_close"] {
            field_provenance.insert(
                field.to_string(),
                astock_core::FieldProvenance::reported("eastmoney", timestamp),
            );
        }
        for (field, upstream) in [("high", "f44"), ("low", "f45"), ("open", "f46")] {
            field_provenance.insert(
                field.to_string(),
                positive_number(upstream).map_or_else(
                    || {
                        astock_core::FieldProvenance::missing(
                            "eastmoney",
                            format!("上游未返回{field}"),
                        )
                    },
                    |_| astock_core::FieldProvenance::reported("eastmoney", timestamp),
                ),
            );
        }
        for (field, upstream) in [("volume", "f47"), ("amount", "f48")] {
            field_provenance.insert(
                field.to_string(),
                number(upstream).map_or_else(
                    || {
                        astock_core::FieldProvenance::missing(
                            "eastmoney",
                            format!("上游未返回{field}"),
                        )
                    },
                    |_| astock_core::FieldProvenance::reported("eastmoney", timestamp),
                ),
            );
        }
        let reported_change = number("f169");
        let reported_pct = number("f170");
        for (field, reported) in [("change", reported_change), ("pct", reported_pct)] {
            let mut provenance = astock_core::FieldProvenance::reported("eastmoney", timestamp);
            if reported.is_none() {
                provenance.quality = astock_core::DataQuality::Derived;
                provenance.source = "eastmoney:price/pre_close".to_string();
            }
            field_provenance.insert(field.to_string(), provenance);
        }
        let turnover = number("f168");
        field_provenance.insert(
            "turnover".to_string(),
            turnover.map_or_else(
                || astock_core::FieldProvenance::missing("eastmoney", "上游未返回换手率"),
                |_| astock_core::FieldProvenance::reported("eastmoney", timestamp),
            ),
        );
        let quote = Quote {
            symbol: code.to_string(),
            name,
            price,
            high: positive_number("f44").unwrap_or(0.0),
            low: positive_number("f45").unwrap_or(0.0),
            open: positive_number("f46").unwrap_or(0.0),
            volume: number("f47").unwrap_or(0.0),
            amount: number("f48").unwrap_or(0.0),
            pre_close,
            change: reported_change.unwrap_or(price - pre_close),
            pct: reported_pct.unwrap_or((price - pre_close) / pre_close * 100.0),
            turnover,
            timestamp,
            field_provenance,
        };
        let out = Fetched::now(quote, Source::EastMoney);
        self.cache.set(&key, &out);
        Ok(out)
    }

    /// Quote for an index code using the index-specific EastMoney market id.
    pub async fn index_quote(&self, index_code: &str) -> Result<Fetched<Quote>, DataError> {
        self.quote_by_secid(index_code, &Symbol::index_secid(index_code), "index_quote")
            .await
    }

    /// Index bars never use stock split/dividend adjustment semantics.
    pub async fn index_kline_period(
        &self,
        index_secid: &str,
        period: KlinePeriod,
        count: u32,
    ) -> Result<Fetched<Vec<Bar>>, DataError> {
        let key = format!("index_{index_secid}_{period:?}_{count}");
        if let Some(hit) = self.cache.get::<Fetched<Vec<Bar>>>(&key, ttl::KLINE) {
            return Ok(hit);
        }
        let rows = self
            .kline_rows(index_secid, period, Adjust::None, count)
            .await?;
        let bars = Self::parse_kline_rows(&rows, VolumeUnit::Lots);
        let required = (count as usize).min(10);
        if bars.len() < required {
            return Err(DataError::Empty(format!(
                "eastmoney index kline {index_secid}: {} bars",
                bars.len()
            )));
        }
        let out = Fetched::now(bars, Source::EastMoney);
        self.cache.set(&key, &out);
        Ok(out)
    }

    /// Full A-share list with EastMoney industry tags (clist `f100` field).
    /// Rows with an empty industry tag are skipped. Used by the graph
    /// crate's industry enrichment.
    pub async fn industry_map(&self) -> Result<Fetched<Vec<IndustryClassified>>, DataError> {
        let key = "industry_map";
        if let Some(hit) = self
            .cache
            .get::<Fetched<Vec<IndustryClassified>>>(key, ttl::ALL_A)
        {
            return Ok(hit);
        }
        let rows = self.fetch_clist_all("f12,f14,f100").await?;
        let mut items = Vec::with_capacity(rows.len());
        for d in &rows {
            let code = d.get("f12").and_then(|v| v.as_str()).unwrap_or("").trim();
            let industry = d.get("f100").and_then(|v| v.as_str()).unwrap_or("").trim();
            if code.len() != 6 || industry.is_empty() {
                continue;
            }
            items.push(IndustryClassified {
                code: code.to_string(),
                name: d
                    .get("f14")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                industry: industry.to_string(),
            });
        }
        if items.is_empty() {
            return Err(DataError::Empty("eastmoney industry map".to_string()));
        }
        let out = Fetched::now(items, Source::EastMoney);
        self.cache.set(key, &out);
        Ok(out)
    }
}

#[async_trait]
impl DataProvider for EastMoney {
    fn name(&self) -> &'static str {
        "eastmoney"
    }

    fn primary_host(&self) -> &'static str {
        "push2his.eastmoney.com"
    }

    async fn kline(
        &self,
        symbol: &Symbol,
        period: KlinePeriod,
        adjust: Adjust,
        count: u32,
    ) -> Result<Fetched<Vec<Bar>>, DataError> {
        let rows = self
            .kline_rows(&symbol.secid(), period, adjust, count)
            .await?;
        let unit = if symbol.is_etf() {
            VolumeUnit::FundUnits
        } else {
            VolumeUnit::Lots
        };
        let bars = Self::parse_kline_rows(&rows, unit);
        if bars.is_empty() {
            return Err(DataError::Empty(format!("eastmoney kline {symbol}")));
        }
        Ok(Fetched::now(bars, Source::EastMoney))
    }

    async fn quote(&self, symbol: &Symbol) -> Result<Fetched<Quote>, DataError> {
        self.quote_by_secid(symbol.code(), &symbol.secid(), "quote")
            .await
    }

    async fn search(&self, keyword: &str) -> Result<Fetched<Vec<SearchResult>>, DataError> {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return Ok(Fetched::now(Vec::new(), Source::EastMoney));
        }
        let key = format!("search_{keyword}");
        if let Some(hit) = self
            .cache
            .get::<Fetched<Vec<SearchResult>>>(&key, ttl::SEARCH)
        {
            return Ok(hit);
        }

        // Pure 6-digit numeric input short-circuits without a network call.
        if keyword.len() == 6 && keyword.bytes().all(|b| b.is_ascii_digit()) {
            let sym = Symbol::new(keyword)?;
            if !sym.is_supported_market_instrument() {
                return Ok(Fetched::now(Vec::new(), Source::EastMoney));
            }
            let out = Fetched::now(
                vec![SearchResult {
                    code: sym.code().to_string(),
                    name: String::new(),
                    classify: sym.market().to_string(),
                }],
                Source::EastMoney,
            );
            self.cache.set(&key, &out);
            return Ok(out);
        }

        let params = vec![
            ("input".to_string(), keyword.to_string()),
            ("type".to_string(), "14".to_string()),
            ("count".to_string(), "10".to_string()),
            ("ut".to_string(), EM_TOKEN.to_string()),
        ];
        let resp = self
            .http
            .get_text(&format!("{SEARCH_HOST}/api/suggest/get"), &params)
            .await?;
        // The suggest endpoint sometimes answers with JSONP.
        let body = strip_jsonp(resp.body.trim()).ok_or_else(|| DataError::Parse {
            upstream: "eastmoney search".to_string(),
            message: "no JSON object in response".to_string(),
        })?;
        let value: serde_json::Value =
            serde_json::from_str(body).map_err(|e| DataError::Parse {
                upstream: "eastmoney search".to_string(),
                message: e.to_string(),
            })?;
        let items = value
            .get("QuotationCodeTable")
            .and_then(|q| q.get("Data"))
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        let mut results = Vec::new();
        for item in items {
            let code = item.get("Code").and_then(|v| v.as_str()).unwrap_or("");
            let name =
                normalize_security_name(item.get("Name").and_then(|v| v.as_str()).unwrap_or(""));
            let classify = item.get("Classify").and_then(|v| v.as_str()).unwrap_or("");
            let supported =
                Symbol::new(code).is_ok_and(|symbol| symbol.is_supported_market_instrument());
            let plausible = classify == "AStock" || classify == "Fund" || supported;
            if plausible && supported && !name.is_empty() {
                results.push(SearchResult {
                    code: code.to_string(),
                    name,
                    classify: classify.to_string(),
                });
            }
        }
        results.truncate(10);
        let out = Fetched::now(results, Source::EastMoney);
        self.cache.set(&key, &out);
        Ok(out)
    }

    async fn fund_flow_daily(
        &self,
        symbol: &Symbol,
        days: u32,
    ) -> Result<Fetched<Vec<FundFlowPoint>>, DataError> {
        let key = format!("flow_{symbol}_{days}");
        if let Some(hit) = self
            .cache
            .get::<Fetched<Vec<FundFlowPoint>>>(&key, ttl::SEARCH)
        {
            return Ok(hit);
        }
        let params = vec![
            ("lmt".to_string(), days.to_string()),
            ("klt".to_string(), "101".to_string()),
            ("secid".to_string(), symbol.secid()),
            ("fields1".to_string(), FFLOW_FIELDS1.to_string()),
            ("fields2".to_string(), FFLOW_FIELDS2.to_string()),
        ];
        let data = self
            .em_json(
                "/api/qt/stock/fflow/daykline/get",
                params,
                &HIS_HOSTS,
                "fund_flow_daily",
            )
            .await?;
        let rows = data
            .get("data")
            .and_then(|d| d.get("klines"))
            .and_then(|k| k.as_array())
            .cloned()
            .unwrap_or_default();
        let mut points = Vec::with_capacity(rows.len());
        for row in rows.iter().filter_map(|r| r.as_str()) {
            // CSV order: date, main, SMALL, MEDIUM, large, super_large, main_pct.
            if let Some(p) = parse_flow_csv(row, true) {
                points.push(p);
            }
        }
        // Fewer than 3 rows means the history is unusable (legacy rule).
        if points.len() < 3 {
            return Err(DataError::Empty(format!(
                "eastmoney fund flow {symbol}: {} rows",
                points.len()
            )));
        }
        let out = Fetched::now(points, Source::EastMoney);
        self.cache.set(&key, &out);
        Ok(out)
    }

    async fn fund_flow_realtime(
        &self,
        symbol: &Symbol,
    ) -> Result<Fetched<Vec<FundFlowPoint>>, DataError> {
        let key = format!("rt_flow_{symbol}");
        if let Some(hit) = self
            .cache
            .get::<Fetched<Vec<FundFlowPoint>>>(&key, ttl::REALTIME)
        {
            return Ok(hit);
        }
        let params = vec![
            ("klt".to_string(), "1".to_string()),
            ("secid".to_string(), symbol.secid()),
            ("fields1".to_string(), FFLOW_FIELDS1.to_string()),
            ("fields2".to_string(), FFLOW_FIELDS2.to_string()),
            ("lmt".to_string(), "300".to_string()),
        ];
        let data = self
            .em_json(
                "/api/qt/stock/fflow/kline/get",
                params,
                &RT_FLOW_HOSTS,
                "fund_flow_realtime",
            )
            .await?;
        let rows = data
            .get("data")
            .and_then(|d| d.get("klines"))
            .and_then(|k| k.as_array())
            .cloned()
            .unwrap_or_default();
        // Empty off-market is a normal, cacheable answer.
        let points: Vec<FundFlowPoint> = rows
            .iter()
            .filter_map(|r| r.as_str())
            .filter_map(|row| parse_flow_csv(row, false))
            .collect();
        let out = Fetched::now(points, Source::EastMoney);
        self.cache.set(&key, &out);
        Ok(out)
    }

    async fn minute(&self, symbol: &Symbol) -> Result<Fetched<MinuteData>, DataError> {
        let key = format!("minute_{symbol}");
        if let Some(hit) = self.cache.get::<Fetched<MinuteData>>(&key, ttl::REALTIME) {
            return Ok(hit);
        }
        let params = vec![
            ("secid".to_string(), symbol.secid()),
            (
                "fields1".to_string(),
                "f1,f2,f3,f4,f5,f6,f7,f8,f9,f10,f11,f12,f13".to_string(),
            ),
            (
                "fields2".to_string(),
                "f51,f52,f53,f54,f55,f56,f57,f58".to_string(),
            ),
            ("isccr".to_string(), "1".to_string()),
            ("ndays".to_string(), "1".to_string()),
            ("iscca".to_string(), "0".to_string()),
            ("klt".to_string(), "5".to_string()),
            ("fqt".to_string(), "1".to_string()),
        ];
        let data = self
            .em_json("/api/qt/stock/trends2/get", params, &QUOTE_HOSTS, "minute")
            .await?;
        let d = data.get("data").cloned().unwrap_or(serde_json::Value::Null);
        let trends = d
            .get("trends")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        if trends.is_empty() {
            return Err(DataError::Empty(format!("eastmoney minute {symbol}")));
        }
        let mut points = Vec::with_capacity(trends.len());
        let mut high = 0.0_f64;
        let mut low = f64::MAX;
        for row in trends.iter().filter_map(|t| t.as_str()) {
            // CSV: time idx0, price idx2, volume idx5, avg idx7.
            let parts: Vec<&str> = row.split(',').collect();
            if parts.len() < 8 {
                continue;
            }
            let Some(time) = parse_datetime_flexible(parts[0]) else {
                continue;
            };
            let price = parts[2].parse::<f64>().unwrap_or(0.0);
            let volume = parts[5].parse::<f64>().unwrap_or(0.0);
            let avg_price = parts[7].parse::<f64>().unwrap_or(0.0);
            if price > 0.0 {
                high = high.max(price);
                low = low.min(price);
            }
            points.push(MinutePoint {
                time,
                price,
                avg_price,
                volume,
            });
        }
        let md = MinuteData {
            points,
            pre_close: d.get("preClose").and_then(json_f64).unwrap_or(0.0),
            name: d
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            high,
            low: if low == f64::MAX { 0.0 } else { low },
        };
        let out = Fetched::now(md, Source::EastMoney);
        self.cache.set(&key, &out);
        Ok(out)
    }

    async fn all_a_shares(&self) -> Result<Fetched<Vec<StockListItem>>, DataError> {
        self.complete_market_snapshot().await
    }

    async fn market_breadth(&self) -> Result<Fetched<MarketBreadth>, DataError> {
        let key = "market_breadth";
        if let Some(hit) = self.cache.get::<Fetched<MarketBreadth>>(key, ttl::BREADTH) {
            return Ok(hit);
        }
        // Reuse the full-market snapshot fetched by the scanner/list page.
        // On a cold start this fetch also warms that list, so the two tools
        // never download the same 5,000+ rows independently.
        let snapshot = self.complete_market_snapshot().await?;
        let out = Fetched {
            data: Self::breadth_from_items(&snapshot.data)?,
            source: snapshot.source,
            fetched_at: snapshot.fetched_at,
        };
        self.cache.set(key, &out);
        Ok(out)
    }

    async fn index_kline(
        &self,
        index_secid: &str,
        count: u32,
    ) -> Result<Fetched<Vec<Bar>>, DataError> {
        self.index_kline_period(index_secid, KlinePeriod::Day, count)
            .await
    }
}

fn clist_diff_rows(data: &serde_json::Value) -> Vec<serde_json::Value> {
    match data.get("diff") {
        Some(serde_json::Value::Array(rows)) => rows.clone(),
        Some(serde_json::Value::Object(rows)) => rows.values().cloned().collect(),
        _ => Vec::new(),
    }
}

fn parse_quote_identity(
    data: &serde_json::Value,
    expected_code: &str,
    secid: &str,
    cache_namespace: &str,
) -> Result<(String, f64, f64), DataError> {
    let upstream_code = data
        .get("f57")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if upstream_code != expected_code {
        return Err(DataError::Parse {
            upstream: format!("eastmoney {cache_namespace} {secid}"),
            message: format!(
                "security identity mismatch: expected {expected_code}, received {upstream_code}"
            ),
        });
    }
    let name = normalize_security_name(
        data.get("f58")
            .and_then(|value| value.as_str())
            .unwrap_or_default(),
    );
    let required_positive = |field: &str, label: &str| {
        data.get(field)
            .and_then(json_f64)
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or_else(|| {
                DataError::Empty(format!(
                    "eastmoney {cache_namespace} {secid}: missing or non-positive {label}"
                ))
            })
    };
    Ok((
        name,
        required_positive("f43", "price")?,
        required_positive("f60", "pre-close")?,
    ))
}

fn count_breadth(pcts: impl IntoIterator<Item = Option<f64>>) -> MarketBreadth {
    let mut up = 0_u32;
    let mut down = 0_u32;
    let mut flat = 0_u32;
    for pct in pcts {
        match pct {
            Some(value) if value > 0.0 => up += 1,
            Some(value) if value < 0.0 => down += 1,
            _ => flat += 1,
        }
    }
    MarketBreadth {
        up,
        down,
        flat,
        total: up + down + flat,
    }
}

/// Parse one fflow CSV row: `time, main, small, medium, large, super_large,
/// main_pct`. Minute rows have no meaningful pct (left 0, per legacy).
fn parse_flow_csv(row: &str, daily: bool) -> Option<FundFlowPoint> {
    let parts: Vec<&str> = row.split(',').collect();
    if parts.len() < 6 {
        return None;
    }
    let time = parse_datetime_flexible(parts[0])?;
    let num = |i: usize| parts[i].parse::<f64>().unwrap_or(0.0);
    Some(FundFlowPoint {
        time,
        main_net: num(1),
        small_net: num(2),
        medium_net: num(3),
        large_net: num(4),
        super_large_net: num(5),
        main_pct: if daily && parts.len() >= 7 {
            num(6)
        } else {
            0.0
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kline_csv_with_amount_and_turnover() {
        let rows = vec![
            "2025-08-20,1400.00,1410.00,1415.00,1390.00,65181,9177000000,1.79,-0.32,-4.50,0.52"
                .to_string(),
            "2025-08-21,1412.00,1405.50,1418.00,1400.00,70234,9870000000,1.28,-0.32,-4.50,0.56"
                .to_string(),
        ];
        let bars = EastMoney::parse_kline_rows(&rows, VolumeUnit::Lots);
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].close, 1410.0);
        assert_eq!(bars[0].volume, 65181.0);
        assert_eq!(bars[0].amount, Some(9.177e9));
        assert_eq!(bars[0].turnover, Some(0.52));
        assert!(bars[1].pct.is_some());
    }

    #[test]
    fn quote_identity_rejects_zero_placeholder_prices() {
        let zero =
            serde_json::json!({"f57": "300308", "f58": "中 际 旭 创", "f43": 0, "f60": 870.22});
        assert!(matches!(
            parse_quote_identity(&zero, "300308", "0.300308", "quote"),
            Err(DataError::Empty(message)) if message.contains("non-positive price")
        ));
        let valid =
            serde_json::json!({"f57": "300308", "f58": "中 际 旭 创", "f43": 870.22, "f60": 943.0});
        let (name, price, pre_close) =
            parse_quote_identity(&valid, "300308", "0.300308", "quote").unwrap();
        assert_eq!(name, "中际旭创");
        assert_eq!(price, 870.22);
        assert_eq!(pre_close, 943.0);
    }

    #[test]
    fn flow_csv_column_order_small_before_medium() {
        // date, main, SMALL, MEDIUM, large, super_large, main_pct
        let p = parse_flow_csv("2025-08-21,-1000.0,800.0,200.0,-600.0,-400.0,-1.23", true).unwrap();
        assert_eq!(p.main_net, -1000.0);
        assert_eq!(p.small_net, 800.0);
        assert_eq!(p.medium_net, 200.0);
        assert_eq!(p.large_net, -600.0);
        assert_eq!(p.super_large_net, -400.0);
        assert_eq!(p.main_pct, -1.23);
    }

    #[test]
    fn flow_csv_minute_row_has_no_pct() {
        let p = parse_flow_csv("2025-08-21 09:31,100.0,-50.0,-30.0,60.0,40.0", false).unwrap();
        assert_eq!(p.main_pct, 0.0);
        assert_eq!(p.time.format("%H:%M").to_string(), "09:31");
    }

    #[test]
    fn breadth_counts_up_down_flat_and_missing() {
        let breadth = count_breadth([Some(1.2), Some(-0.3), Some(0.0), None]);
        assert_eq!(breadth.up, 1);
        assert_eq!(breadth.down, 1);
        assert_eq!(breadth.flat, 2);
        assert_eq!(breadth.total, 4);
    }

    #[test]
    fn complete_breadth_rejects_partial_market_snapshot() {
        let items = vec![
            StockListItem {
                code: "600000".to_string(),
                name: "测试".to_string(),
                price: Some(10.0),
                pct: Some(1.0),
                amount: Some(1_000.0),
            };
            MIN_COMPLETE_A_SHARE_ROWS - 1
        ];
        assert!(matches!(
            EastMoney::breadth_from_items(&items),
            Err(DataError::Empty(message)) if message.contains("incomplete")
        ));
    }

    #[test]
    fn clist_accepts_array_and_object_diff_shapes() {
        let array = serde_json::json!({"diff": [{"f12": "600000"}]});
        let object = serde_json::json!({"diff": {"0": {"f12": "600000"}}});
        assert_eq!(clist_diff_rows(&array).len(), 1);
        assert_eq!(clist_diff_rows(&object).len(), 1);
    }

    #[test]
    fn market_snapshot_filters_test_and_legacy_rows_and_normalizes_names() {
        let rows = vec![
            serde_json::json!({"f12":"430002","f14":"中 科 软","f2":988,"f3":22.28,"f6":26880400}),
            serde_json::json!({"f12":"810011","f14":"优机定转","f2":0,"f3":0,"f6":0}),
            serde_json::json!({"f12":"603927","f14":"中 科 软","f2":13.15,"f3":-0.68,"f6":63128588}),
            serde_json::json!({"f12":"920001","f14":"纬 达 光 电","f2":11.06,"f3":0.5,"f6":1000}),
        ];
        let items = EastMoney::stock_items_from_rows(rows);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].code, "603927");
        assert_eq!(items[0].name, "中科软");
        assert_eq!(items[1].code, "920001");
        assert_eq!(items[1].name, "纬达光电");
    }

    #[test]
    fn market_snapshot_rejects_placeholder_trade_fields() {
        let rows = (0..10)
            .map(|index| StockListItem {
                code: format!("600{index:03}"),
                name: format!("样本{index}"),
                price: (index == 0).then_some(10.0),
                pct: Some(0.0),
                amount: (index == 0).then_some(1_000_000.0),
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            EastMoney::validate_snapshot_trade_fields(&rows, 10),
            Err(DataError::Empty(message)) if message.contains("placeholder trade fields")
        ));
    }

    #[test]
    fn production_host_pools_never_include_test_endpoints() {
        for host in QUOTE_HOSTS
            .iter()
            .chain(HIS_HOSTS.iter())
            .chain(EM_KLINE_HOSTS.iter())
            .chain(MARKET_SNAPSHOT_HOSTS.iter())
        {
            assert!(!host.contains("test"), "test endpoint leaked: {host}");
        }
    }
}
