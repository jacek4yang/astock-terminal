//! 有界的公共财经快讯聚合器。
//!
//! 数据结构与来源治理参考 Apache-2.0 项目 niuone 对 NewsNow 的接入：
//! 来源白名单、最多 3 路并发、一次瞬时错误重试、响应大小限制、逐来源缓存
//! 与失败时保留最后成功副本。快讯只用于发现线索，不属于权威公告源。

use crate::cache::TtlCache;
use crate::http::HttpClient;
use astock_core::DataError;
use astock_security::UrlSecurityPolicy;
use dashmap::DashMap;
use futures::{stream, StreamExt};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

pub const NEWSNOW_ENDPOINT: &str = "https://newsnow.busiyi.world/api/s";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_CONCURRENT: usize = 3;
const RETRY_PAUSE: Duration = Duration::from_millis(500);

/// 财经来源白名单：(稳定标识、中文名称、上游建议刷新间隔秒)。
pub const FINANCE_NEWS_SOURCES: &[(&str, &str, u64)] = &[
    ("cls-telegraph", "财联社电报", 300),
    ("jin10", "金十数据", 600),
    ("wallstreetcn-quick", "华尔街见闻快讯", 300),
    ("mktnews-flash", "MKTNews 快讯", 120),
    ("gelonghui", "格隆汇事件", 120),
    ("xueqiu-hotstock", "雪球热门股票", 120),
    ("wallstreetcn-news", "华尔街见闻最新资讯", 1_800),
];

static TAGS: Lazy<Regex> = Lazy::new(|| Regex::new(r"<[^>]{0,400}>").expect("tag regex"));

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FinanceNewsItem {
    pub id: String,
    pub source_id: String,
    pub source_name: String,
    pub title: String,
    pub summary: String,
    pub url: String,
    pub published_at: String,
    pub published_at_ms: Option<i64>,
    pub important: bool,
    pub rank: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FinanceNewsBatch {
    pub items: Vec<FinanceNewsItem>,
    pub successful_sources: Vec<String>,
    pub stale_sources: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourceSnapshot {
    items: Vec<FinanceNewsItem>,
}

/// Shared public-news client. It deliberately has no credentials.
pub struct FinanceNewsProvider {
    http: Arc<HttpClient>,
    cache: Arc<TtlCache>,
    permits: Semaphore,
    last_good: DashMap<String, SourceSnapshot>,
}

impl FinanceNewsProvider {
    pub fn new(http: Arc<HttpClient>, cache: Arc<TtlCache>) -> Self {
        Self {
            http,
            cache,
            permits: Semaphore::new(MAX_CONCURRENT),
            last_good: DashMap::new(),
        }
    }

    /// Load several allowlisted sources and merge them newest-first.
    pub async fn latest(
        &self,
        sources: &[String],
        per_source: usize,
    ) -> Result<FinanceNewsBatch, DataError> {
        if sources.is_empty() || sources.len() > FINANCE_NEWS_SOURCES.len() {
            return Err(DataError::Empty(
                "finance news sources must contain 1-7 entries".to_string(),
            ));
        }
        let mut unique = Vec::new();
        for source in sources {
            let source = source.trim().to_ascii_lowercase();
            if source_meta(&source).is_none() {
                return Err(DataError::Empty(format!(
                    "unsupported finance news source: {source}"
                )));
            }
            if !unique.contains(&source) {
                unique.push(source);
            }
        }
        let per_source = per_source.clamp(1, 100);
        let outcomes = stream::iter(unique.into_iter().map(|source| async move {
            let result = self.fetch_source(&source, per_source).await;
            (source, result)
        }))
        .buffered(MAX_CONCURRENT)
        .collect::<Vec<_>>()
        .await;

        let mut batch = FinanceNewsBatch::default();
        for (source, result) in outcomes {
            match result {
                Ok(snapshot) => {
                    batch.successful_sources.push(source);
                    batch.items.extend(snapshot.items);
                }
                Err(error) => {
                    batch.errors.push(format!("{source}: {error}"));
                    if let Some(snapshot) = self.last_good.get(&source) {
                        batch.stale_sources.push(source);
                        batch.items.extend(snapshot.items.clone());
                    }
                }
            }
        }
        let mut seen = HashSet::new();
        batch.items.retain(|row| seen.insert(row.id.clone()));
        batch.items.sort_by(|left, right| {
            right
                .published_at_ms
                .unwrap_or_default()
                .cmp(&left.published_at_ms.unwrap_or_default())
                .then_with(|| left.rank.cmp(&right.rank))
        });
        if batch.items.is_empty() {
            return Err(DataError::AllFailed {
                op: "finance news",
                details: batch.errors.join("; "),
            });
        }
        Ok(batch)
    }

    async fn fetch_source(&self, source: &str, limit: usize) -> Result<SourceSnapshot, DataError> {
        let (_, _, ttl_seconds) = source_meta(source).expect("source checked");
        let key = format!("finance_news_{source}_{limit}");
        if let Some(snapshot) = self
            .cache
            .get::<SourceSnapshot>(&key, Duration::from_secs(ttl_seconds))
        {
            self.last_good.insert(source.to_string(), snapshot.clone());
            return Ok(snapshot);
        }
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| DataError::NoProvider("finance news scheduler closed"))?;
        let params = vec![
            ("id".to_string(), source.to_string()),
            ("latest".to_string(), "true".to_string()),
        ];
        let mut response = self.http.get_text(NEWSNOW_ENDPOINT, &params).await;
        if matches!(
            response,
            Err(DataError::RateLimited(_)) | Err(DataError::Network { .. })
        ) {
            tokio::time::sleep(RETRY_PAUSE).await;
            response = self.http.get_text(NEWSNOW_ENDPOINT, &params).await;
        }
        let response = response?;
        if response.body.len() > MAX_RESPONSE_BYTES {
            return Err(DataError::Parse {
                upstream: "newsnow".to_string(),
                message: "response exceeds 2 MiB".to_string(),
            });
        }
        if response
            .content_type
            .as_deref()
            .is_some_and(|value| !value.to_ascii_lowercase().contains("json"))
        {
            return Err(DataError::Parse {
                upstream: "newsnow".to_string(),
                message: "response is not JSON".to_string(),
            });
        }
        let value: Value =
            serde_json::from_str(&response.body).map_err(|error| DataError::Parse {
                upstream: "newsnow".to_string(),
                message: error.to_string(),
            })?;
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let rows = value.get("items").and_then(Value::as_array);
        if !matches!(status.as_str(), "success" | "cache") || rows.is_none() {
            return Err(DataError::Parse {
                upstream: "newsnow".to_string(),
                message: "missing valid status or items".to_string(),
            });
        }
        let items = rows
            .into_iter()
            .flatten()
            .take(limit)
            .enumerate()
            .filter_map(|(index, row)| normalize_item(source, row, index + 1))
            .collect::<Vec<_>>();
        let snapshot = SourceSnapshot { items };
        self.cache.set(&key, &snapshot);
        self.last_good.insert(source.to_string(), snapshot.clone());
        Ok(snapshot)
    }
}

fn source_meta(source: &str) -> Option<(&'static str, &'static str, u64)> {
    FINANCE_NEWS_SOURCES
        .iter()
        .copied()
        .find(|(id, _, _)| *id == source)
}

fn normalize_item(source: &str, raw: &Value, rank: usize) -> Option<FinanceNewsItem> {
    let (_, source_name, _) = source_meta(source)?;
    let title = clean_text(raw.get("title"), 1_000);
    if title.is_empty() {
        return None;
    }
    let extra = raw.get("extra").filter(|value| value.is_object());
    let external_id = clean_text(raw.get("id"), 200);
    let published_at_ms = timestamp_ms(
        raw.get("pubDate")
            .or_else(|| extra.and_then(|value| value.get("date"))),
    );
    let url = safe_url(
        raw.get("url")
            .or_else(|| raw.get("mobileUrl"))
            .and_then(Value::as_str),
    );
    let important = extra
        .and_then(|value| value.get("info"))
        .is_some_and(important_marker);
    let fallback_id = format!(
        "{}-{}-{}",
        published_at_ms.unwrap_or_default(),
        rank,
        title.chars().take(24).collect::<String>()
    );
    Some(FinanceNewsItem {
        id: format!(
            "{source}:{}",
            if external_id.is_empty() {
                fallback_id
            } else {
                external_id
            }
        ),
        source_id: source.to_string(),
        source_name: source_name.to_string(),
        title,
        summary: clean_text(extra.and_then(|value| value.get("hover")), 3_000),
        url,
        published_at: published_at_ms
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|date| {
                date.with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
                    .to_rfc3339()
            })
            .unwrap_or_default(),
        published_at_ms,
        important,
        rank,
    })
}

fn clean_text(value: Option<&Value>, max: usize) -> String {
    let raw = value.and_then(Value::as_str).unwrap_or_default();
    let plain = TAGS.replace_all(raw, "");
    plain
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

fn safe_url(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    UrlSecurityPolicy::default()
        .validate_static(value)
        .map(|url| url.as_str().chars().take(2_048).collect())
        .unwrap_or_default()
}

fn timestamp_ms(value: Option<&Value>) -> Option<i64> {
    let mut number =
        value.and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))?;
    if number <= 0 {
        return None;
    }
    if number < 10_000_000_000 {
        number = number.checked_mul(1_000)?;
    }
    (number < 100_000_000_000_000).then_some(number)
}

fn important_marker(value: &Value) -> bool {
    if value.as_bool() == Some(true) {
        return true;
    }
    matches!(
        clean_text(Some(value), 40).to_ascii_lowercase().as_str(),
        "1" | "important" | "on" | "true" | "yes" | "✰" | "★" | "⭐" | "重要"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_news_items() {
        let row = serde_json::json!({
            "id": "one",
            "title": "<b>政策</b> &amp; 市场",
            "pubDate": 1_786_420_800,
            "url": "javascript:alert(1)",
            "extra": {"hover": "补充", "info": "重要"}
        });
        let item = normalize_item("cls-telegraph", &row, 1).unwrap();
        assert_eq!(item.title, "政策 & 市场");
        assert!(item.url.is_empty());
        assert!(item.important);
        assert_eq!(item.published_at_ms, Some(1_786_420_800_000));
    }

    #[test]
    fn source_allowlist_is_unique() {
        let ids = FINANCE_NEWS_SOURCES
            .iter()
            .map(|row| row.0)
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), FINANCE_NEWS_SOURCES.len());
    }
}
