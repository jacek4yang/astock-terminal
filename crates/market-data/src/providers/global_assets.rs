//! Global-asset snapshots used by cross-market Agent research.
//!
//! This provider deliberately keeps global commodities outside the A-share
//! [`DataProvider`] symbol contract. It reuses the shared HTTP throttle/cache
//! and returns a small, typed snapshot that can be cross-checked and audited.

use crate::cache::TtlCache;
use crate::http::{HttpClient, EM_TOKEN};
use astock_core::DataError;
use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

const EASTMONEY_HOSTS: &[&str] = &[
    "https://push2.eastmoney.com",
    "https://push2delay.eastmoney.com",
];
const GOLD_QUOTE_PATH: &str = "/api/qt/stock/get";
const YAHOO_GOLD_CHART: &str = "https://query1.finance.yahoo.com/v8/finance/chart/GC=F";
const WORLD_GOLD_COUNCIL_HOME: &str = "https://www.gold.org/";
const SGE_HOME: &str = "https://www.sge.com.cn/cn";

static HTML_TAGS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)<[^>]{0,1000}>").expect("valid HTML tag regex"));
static WGC_RESEARCH_CARD: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?s)<wgc-card>\s*<a href="(?P<url>/goldhub/(?:research|gold-focus)/[^"]+)".*?<h2[^>]*class="m-card__title"[^>]*>(?P<title>.*?)</h2>.*?<div class="m-card__copy">(?P<summary>.*?)</div>.*?<time datetime="(?P<date>[^"]+)""#,
    )
    .expect("valid WGC research-card regex")
});
static WGC_INSIGHT_CARD: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?s)<div class="m-insights-card">.*?<a href="(?P<url>/goldhub/gold-focus/[^"]+)">\s*<h3[^>]*>(?P<title>.*?)</h3>\s*<time class="plain">(?P<date>.*?)</time>"#,
    )
    .expect("valid WGC insight-card regex")
});
static SGE_NOTICE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?s)<a href="(?P<url>/jjsnotice/\d+)">\s*<p class="title clearfix">\s*<span[^>]*>(?P<title>.*?)</span>.*?</p>\s*<p class="time">(?P<date>\d{2}-\d{2}-\d{2})</p>"#,
    )
    .expect("valid SGE notice regex")
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlobalAssetQuote {
    pub symbol: String,
    pub name: String,
    pub venue: String,
    pub price: f64,
    pub previous_close: f64,
    pub change: f64,
    pub change_pct: f64,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub unit: String,
    pub observed_at: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlobalAssetPoint {
    pub date: String,
    pub close: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoldTrendSummary {
    pub observations: usize,
    pub latest_date: String,
    pub latest_close: f64,
    pub return_5_sessions_pct: Option<f64>,
    pub return_20_sessions_pct: Option<f64>,
    pub return_60_sessions_pct: Option<f64>,
    pub high_60_sessions: Option<f64>,
    pub low_60_sessions: Option<f64>,
    pub direction: String,
    pub series: Vec<GlobalAssetPoint>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoldMarketSnapshot {
    pub quotes: Vec<GlobalAssetQuote>,
    pub trend: Option<GoldTrendSummary>,
    pub successful_sources: Vec<String>,
    pub source_errors: Vec<String>,
    pub fetched_at: String,
}

/// A first-party gold-market publication or exchange notice. These items are
/// fetched directly from the publishing organization rather than through a
/// public news aggregator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoldPrimaryNewsItem {
    pub title: String,
    pub summary: String,
    pub source_name: String,
    pub published_at: String,
    pub url: String,
    pub evidence_level: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoldPrimaryNewsBatch {
    pub items: Vec<GoldPrimaryNewsItem>,
    pub successful_sources: Vec<String>,
    pub source_errors: Vec<String>,
    pub fetched_at: String,
}

#[derive(Clone)]
pub struct GlobalAssetProvider {
    http: Arc<HttpClient>,
    cache: Arc<TtlCache>,
}

impl GlobalAssetProvider {
    pub fn new(http: Arc<HttpClient>, cache: Arc<TtlCache>) -> Self {
        Self { http, cache }
    }

    /// Fetch COMEX continuous gold, Shanghai Gold Exchange Au99.99 and a
    /// bounded COMEX daily series. Each source fails independently.
    pub async fn gold_snapshot(&self, days: usize) -> Result<GoldMarketSnapshot, DataError> {
        let days = days.clamp(20, 180);
        let cache_key = format!("global_assets_gold_{days}");
        if let Some(snapshot) = self
            .cache
            .get::<GoldMarketSnapshot>(&cache_key, Duration::from_secs(30))
        {
            return Ok(snapshot);
        }

        let (comex, sge, trend) = tokio::join!(
            self.eastmoney_quote("101.GC00Y", "COMEX", "美元/盎司", "东方财富全球期货行情"),
            self.eastmoney_quote(
                "118.AU9999",
                "上海黄金交易所",
                "元/克",
                "东方财富上海黄金现货行情"
            ),
            self.yahoo_gold_trend(days),
        );
        let mut quotes = Vec::new();
        let mut successful_sources = Vec::new();
        let mut source_errors = Vec::new();
        match comex {
            Ok(value) => {
                successful_sources.push(value.source.clone());
                quotes.push(value);
            }
            Err(error) => source_errors.push(format!("COMEX 黄金行情：{error}")),
        }
        match sge {
            Ok(value) => {
                successful_sources.push(value.source.clone());
                quotes.push(value);
            }
            Err(error) => source_errors.push(format!("上海黄金现货行情：{error}")),
        }
        let trend = match trend {
            Ok(value) => {
                successful_sources.push(value.source.clone());
                Some(value)
            }
            Err(error) => {
                source_errors.push(format!("COMEX 黄金历史趋势：{error}"));
                None
            }
        };
        if quotes.is_empty() && trend.is_none() {
            return Err(DataError::AllFailed {
                op: "gold market snapshot",
                details: source_errors.join("; "),
            });
        }
        successful_sources.sort();
        successful_sources.dedup();
        let snapshot = GoldMarketSnapshot {
            quotes,
            trend,
            successful_sources,
            source_errors,
            fetched_at: Utc::now().to_rfc3339(),
        };
        self.cache.set(&cache_key, &snapshot);
        Ok(snapshot)
    }

    /// Read gold research and exchange notices directly from first-party
    /// publishers. Each publisher fails independently and successful results
    /// are cached briefly to avoid repeatedly downloading the same pages.
    pub async fn primary_gold_news(&self, limit: usize) -> Result<GoldPrimaryNewsBatch, DataError> {
        let limit = limit.clamp(1, 50);
        let cache_key = format!("global_assets_primary_gold_news_{limit}");
        if let Some(batch) = self
            .cache
            .get::<GoldPrimaryNewsBatch>(&cache_key, Duration::from_secs(600))
        {
            return Ok(batch);
        }
        let (wgc, sge) = tokio::join!(self.fetch_wgc_news(limit), self.fetch_sge_notices(limit));
        let mut items = Vec::new();
        let mut successful_sources = Vec::new();
        let mut source_errors = Vec::new();
        match wgc {
            Ok(mut rows) if !rows.is_empty() => {
                successful_sources.push("世界黄金协会".to_string());
                items.append(&mut rows);
            }
            Ok(_) => source_errors.push("世界黄金协会：页面没有可识别的最新研究".to_string()),
            Err(error) => source_errors.push(format!("世界黄金协会：{error}")),
        }
        match sge {
            Ok(mut rows) if !rows.is_empty() => {
                successful_sources.push("上海黄金交易所".to_string());
                items.append(&mut rows);
            }
            Ok(_) => source_errors.push("上海黄金交易所：页面没有可识别的最新公告".to_string()),
            Err(error) => source_errors.push(format!("上海黄金交易所：{error}")),
        }
        items.sort_by(|left, right| right.published_at.cmp(&left.published_at));
        items.dedup_by(|left, right| left.url == right.url);
        items.truncate(limit);
        if items.is_empty() {
            return Err(DataError::AllFailed {
                op: "primary gold news",
                details: source_errors.join("; "),
            });
        }
        let batch = GoldPrimaryNewsBatch {
            items,
            successful_sources,
            source_errors,
            fetched_at: Utc::now().to_rfc3339(),
        };
        self.cache.set(&cache_key, &batch);
        Ok(batch)
    }

    async fn fetch_wgc_news(&self, limit: usize) -> Result<Vec<GoldPrimaryNewsItem>, DataError> {
        let response = self.http.get_text(WORLD_GOLD_COUNCIL_HOME, &[]).await?;
        let mut items = parse_wgc_news(&response.body);
        items.truncate(limit);
        Ok(items)
    }

    async fn fetch_sge_notices(&self, limit: usize) -> Result<Vec<GoldPrimaryNewsItem>, DataError> {
        let response = self.http.get_text(SGE_HOME, &[]).await?;
        let mut items = parse_sge_notices(&response.body);
        items.truncate(limit);
        Ok(items)
    }

    async fn eastmoney_quote(
        &self,
        secid: &str,
        venue: &str,
        unit: &str,
        source: &str,
    ) -> Result<GlobalAssetQuote, DataError> {
        let params = vec![
            ("secid".to_string(), secid.to_string()),
            ("ut".to_string(), EM_TOKEN.to_string()),
            (
                "fields".to_string(),
                "f57,f58,f43,f44,f45,f46,f59,f60,f86,f169,f170".to_string(),
            ),
        ];
        let payload = self
            .http
            .get_json_pool(GOLD_QUOTE_PATH, &params, EASTMONEY_HOSTS, "gold quote")
            .await?;
        let data = payload
            .get("data")
            .filter(|value| value.is_object())
            .ok_or_else(|| DataError::Empty(format!("{venue} 黄金行情没有有效数据")))?;
        let decimal = data
            .get("f59")
            .and_then(Value::as_i64)
            .unwrap_or(2)
            .clamp(0, 6);
        let divisor = 10_f64.powi(decimal as i32);
        let scaled = |field: &str| data.get(field).and_then(Value::as_f64).map(|v| v / divisor);
        let price = scaled("f43")
            .filter(|value| *value > 0.0)
            .ok_or_else(|| DataError::Empty(format!("{venue} 黄金最新价为空")))?;
        let previous_close = scaled("f60").unwrap_or_default();
        let change = scaled("f169").unwrap_or(price - previous_close);
        let change_pct = data
            .get("f170")
            .and_then(Value::as_f64)
            .map(|value| value / 100.0)
            .or_else(|| {
                (previous_close > 0.0).then_some((price - previous_close) / previous_close * 100.0)
            })
            .unwrap_or_default();
        let observed_at = data
            .get("f86")
            .and_then(Value::as_i64)
            .and_then(|value| DateTime::from_timestamp(value, 0))
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| Utc::now().to_rfc3339());
        Ok(GlobalAssetQuote {
            symbol: data
                .get("f57")
                .and_then(Value::as_str)
                .unwrap_or(secid)
                .to_string(),
            name: data
                .get("f58")
                .and_then(Value::as_str)
                .unwrap_or("黄金")
                .to_string(),
            venue: venue.to_string(),
            price: round(price, 4),
            previous_close: round(previous_close, 4),
            change: round(change, 4),
            change_pct: round(change_pct, 2),
            open: scaled("f46").map(|value| round(value, 4)),
            high: scaled("f44").map(|value| round(value, 4)),
            low: scaled("f45").map(|value| round(value, 4)),
            unit: unit.to_string(),
            observed_at,
            source: source.to_string(),
        })
    }

    async fn yahoo_gold_trend(&self, days: usize) -> Result<GoldTrendSummary, DataError> {
        let params = vec![
            ("range".to_string(), "6mo".to_string()),
            ("interval".to_string(), "1d".to_string()),
            ("events".to_string(), "history".to_string()),
        ];
        let payload = self.http.get_json(YAHOO_GOLD_CHART, &params).await?;
        let result = payload
            .pointer("/chart/result/0")
            .filter(|value| value.is_object())
            .ok_or_else(|| DataError::Empty("COMEX 黄金历史趋势为空".to_string()))?;
        let timestamps = result
            .get("timestamp")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let closes = result
            .pointer("/indicators/quote/0/close")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut series = timestamps
            .iter()
            .zip(closes.iter())
            .filter_map(|(timestamp, close)| {
                let timestamp = timestamp.as_i64()?;
                let close = close
                    .as_f64()
                    .filter(|value| value.is_finite() && *value > 0.0)?;
                Some(GlobalAssetPoint {
                    date: DateTime::from_timestamp(timestamp, 0)?
                        .date_naive()
                        .to_string(),
                    close: round(close, 2),
                })
            })
            .collect::<Vec<_>>();
        if series.len() < 5 {
            return Err(DataError::Empty(format!(
                "COMEX 黄金历史趋势仅有 {} 个有效点",
                series.len()
            )));
        }
        if series.len() > days {
            series.drain(0..series.len() - days);
        }
        let latest = series.last().expect("series is non-empty");
        let return_for = |sessions: usize| {
            (series.len() > sessions).then(|| {
                let previous = series[series.len() - 1 - sessions].close;
                round((latest.close - previous) / previous * 100.0, 2)
            })
        };
        let window = series.iter().rev().take(60).collect::<Vec<_>>();
        let high_60 = window.iter().map(|point| point.close).reduce(f64::max);
        let low_60 = window.iter().map(|point| point.close).reduce(f64::min);
        let return_20 = return_for(20);
        let direction = match return_20.or_else(|| return_for(5)) {
            Some(value) if value >= 3.0 => "明显上行",
            Some(value) if value > 0.5 => "震荡上行",
            Some(value) if value <= -3.0 => "明显下行",
            Some(value) if value < -0.5 => "震荡下行",
            _ => "区间震荡",
        };
        Ok(GoldTrendSummary {
            observations: series.len(),
            latest_date: latest.date.clone(),
            latest_close: latest.close,
            return_5_sessions_pct: return_for(5),
            return_20_sessions_pct: return_20,
            return_60_sessions_pct: return_for(60),
            high_60_sessions: high_60,
            low_60_sessions: low_60,
            direction: direction.to_string(),
            series,
            source: "Yahoo Finance COMEX 日线（与东方财富实时行情交叉核对）".to_string(),
        })
    }
}

fn parse_wgc_news(html: &str) -> Vec<GoldPrimaryNewsItem> {
    let mut items = WGC_RESEARCH_CARD
        .captures_iter(html)
        .filter_map(|capture| {
            let path = capture.name("url")?.as_str();
            let title = clean_html(capture.name("title")?.as_str(), 400);
            let summary = clean_html(capture.name("summary")?.as_str(), 1_200);
            let published_at = capture.name("date")?.as_str().trim().to_string();
            (!title.is_empty()).then(|| GoldPrimaryNewsItem {
                title,
                summary,
                source_name: "世界黄金协会".to_string(),
                published_at,
                url: format!("https://www.gold.org{path}"),
                evidence_level: "行业一手研究".to_string(),
            })
        })
        .collect::<Vec<_>>();
    items.extend(WGC_INSIGHT_CARD.captures_iter(html).filter_map(|capture| {
        let path = capture.name("url")?.as_str();
        let title = clean_html(capture.name("title")?.as_str(), 400);
        let date = clean_html(capture.name("date")?.as_str(), 80);
        (!title.is_empty()).then(|| GoldPrimaryNewsItem {
            title,
            summary: String::new(),
            source_name: "世界黄金协会".to_string(),
            published_at: parse_english_date(&date).unwrap_or(date),
            url: format!("https://www.gold.org{path}"),
            evidence_level: "行业一手研究".to_string(),
        })
    }));
    items.sort_by(|left, right| right.published_at.cmp(&left.published_at));
    items.dedup_by(|left, right| left.url == right.url);
    items
}

fn parse_sge_notices(html: &str) -> Vec<GoldPrimaryNewsItem> {
    SGE_NOTICE
        .captures_iter(html)
        .filter_map(|capture| {
            let path = capture.name("url")?.as_str();
            let title = clean_html(capture.name("title")?.as_str(), 400);
            let short_date = capture.name("date")?.as_str();
            let published_at = chrono::NaiveDate::parse_from_str(short_date, "%y-%m-%d")
                .ok()
                .map(|date| format!("{date}T00:00:00+08:00"))
                .unwrap_or_else(|| short_date.to_string());
            (!title.is_empty()).then(|| GoldPrimaryNewsItem {
                title,
                summary: "上海黄金交易所最新公开公告".to_string(),
                source_name: "上海黄金交易所".to_string(),
                published_at,
                url: format!("https://www.sge.com.cn{path}"),
                evidence_level: "交易所原始公告".to_string(),
            })
        })
        .collect()
}

fn parse_english_date(value: &str) -> Option<String> {
    chrono::NaiveDate::parse_from_str(value, "%d %B, %Y")
        .ok()
        .map(|date| format!("{date}T12:00:00Z"))
}

fn clean_html(value: &str, max: usize) -> String {
    HTML_TAGS
        .replace_all(value, " ")
        .replace("&#039;", "'")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

fn round(value: f64, decimals: i32) -> f64 {
    let scale = 10_f64.powi(decimals);
    (value * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires public market-data endpoints"]
    async fn live_gold_snapshot_has_price_and_trend() {
        let provider =
            GlobalAssetProvider::new(Arc::new(HttpClient::new()), Arc::new(TtlCache::default()));
        let snapshot = provider.gold_snapshot(90).await.unwrap();
        assert!(!snapshot.quotes.is_empty());
        assert!(snapshot.quotes.iter().all(|quote| quote.price > 0.0));
        assert!(snapshot
            .trend
            .as_ref()
            .is_some_and(|trend| trend.observations >= 20));
    }

    #[tokio::test]
    #[ignore = "requires World Gold Council and Shanghai Gold Exchange pages"]
    async fn live_primary_gold_news_has_first_party_items() {
        let provider =
            GlobalAssetProvider::new(Arc::new(HttpClient::new()), Arc::new(TtlCache::default()));
        let batch = provider.primary_gold_news(20).await.unwrap();
        assert!(!batch.items.is_empty());
        assert!(batch
            .items
            .iter()
            .all(|item| item.url.starts_with("https://")));
        assert!(batch.successful_sources.iter().any(|source| {
            matches!(source.as_str(), "世界黄金协会" | "上海黄金交易所")
        }));
    }

    #[test]
    fn parses_primary_gold_news_fixtures() {
        let wgc = r#"<wgc-card><a href="/goldhub/research/gold-market"><h2 class="m-card__title">Gold &amp; rates</h2><div class="m-card__copy"><p>Real yields &lt; expectations.</p></div><time datetime="2026-08-21T12:00:00Z"></time></a></wgc-card>"#;
        let parsed = parse_wgc_news(wgc);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].title, "Gold & rates");
        assert!(parsed[0].summary.contains("Real yields"));

        let sge = r#"<a href="/jjsnotice/10001"><p class="title clearfix"><span>关于黄金交易的通知</span></p><p class="time">26-08-21</p></a>"#;
        let parsed = parse_sge_notices(sge);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].published_at, "2026-08-21T00:00:00+08:00");
    }
}
