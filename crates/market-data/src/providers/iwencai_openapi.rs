//! iwencai (问财) **official OpenAPI** provider — `openapi.iwencai.com`,
//! Bearer-key authenticated pure-JSON POST with no anti-bot layer (the
//! SkillHub gateway; see `docs/niuone-analysis.md` §2.2). This is a
//! different route from the web iwencai (`crates/wencai`, hexin-v +
//! captcha), which the THS/xueqiu research marked not viable.
//!
//! Three query entries:
//! - 龙虎榜 (dragon-tiger list) via `hithink-market-query`;
//! - 消息面事件 (announcements / news / structured events) via the
//!   `announcement-search` / `news-search` skills and
//!   `hithink-event-query`;
//! - 板块归属 (sector membership) as the fallback for EastMoney's board
//!   snapshot.
//!
//! The API key is optional (`IWENCAI_KEY` env var; settings page later).
//! Without one the provider is unavailable: every call returns
//! [`DataError::NoProvider`] and the hub marks it on the health panel.
//!
//! Governance follows niuone: concurrency capped at 2, one retry on
//! 429/5xx with a short backoff, 龙虎榜 cached per trade day, 消息面 300s.
//! Row schemas are upstream-defined and unstable, so rows pass through as
//! JSON objects; callers project the fields they need.

use crate::cache::TtlCache;
use crate::http::HttpClient;
use astock_core::DataError;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::debug;

/// OpenAPI base URL.
pub const IWENCAI_API: &str = "https://openapi.iwencai.com";

/// Env var carrying the user's iwencai OpenAPI key.
pub const KEY_ENV: &str = "IWENCAI_KEY";

const QUERY2DATA: &str = "/v1/query2data";
const COMPREHENSIVE_SEARCH: &str = "/v1/comprehensive/search";

/// niuone governance: at most 2 in-flight requests, one 429/5xx retry.
const MAX_CONCURRENT: usize = 2;
const RETRY_PAUSE: Duration = Duration::from_millis(500);

/// 消息面证据缓存 TTL (niuone 用 300s).
const EVENTS_TTL: Duration = Duration::from_secs(300);
/// 龙虎榜按交易日归档,进程内缓存 6h.
const DRAGON_TIGER_TTL: Duration = Duration::from_secs(6 * 3600);

/// A page of iwencai rows plus the reported total hit count.
///
/// Rows are raw JSON objects keyed by (Chinese) upstream column names; the
/// schema is iwencai's and drifts, so we pass it through verbatim.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WencaiRows {
    /// Row objects (`datas` from the upstream payload).
    pub rows: Vec<serde_json::Value>,
    /// Total matches across pages (`code_count`), when reported.
    pub total: Option<i64>,
}

/// iwencai OpenAPI adapter (optional, key-gated).
pub struct IwencaiOpenApi {
    http: Arc<HttpClient>,
    cache: Arc<TtlCache>,
    key: Option<String>,
    sem: Semaphore,
}

impl IwencaiOpenApi {
    /// Wrap the shared HTTP layer with an optional API key.
    pub fn new(http: Arc<HttpClient>, cache: Arc<TtlCache>, key: Option<String>) -> Self {
        IwencaiOpenApi {
            http,
            cache,
            key: key.filter(|k| !k.trim().is_empty()),
            sem: Semaphore::new(MAX_CONCURRENT),
        }
    }

    /// Build from the `IWENCAI_KEY` env var (`None` when unset).
    pub fn from_env(http: Arc<HttpClient>, cache: Arc<TtlCache>) -> Self {
        Self::new(http, cache, std::env::var(KEY_ENV).ok())
    }

    /// Whether an API key is configured.
    pub fn available(&self) -> bool {
        self.key.is_some()
    }

    fn key(&self) -> Result<&str, DataError> {
        self.key
            .as_deref()
            .ok_or(DataError::NoProvider("iwencai-openapi (no key)"))
    }

    /// One POST with the Bearer header, concurrency permit, and the
    /// 429/5xx single retry.
    async fn post(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, DataError> {
        let key = self.key()?.to_string();
        let _permit = self
            .sem
            .acquire()
            .await
            .map_err(|_| DataError::NoProvider("iwencai-openapi (closed)"))?;
        let url = format!("{IWENCAI_API}{path}");
        let headers = vec![("Authorization".to_string(), format!("Bearer {key}"))];

        let mut attempt = self.http.post_json(&url, &headers, body).await;
        if matches!(
            attempt,
            Err(DataError::RateLimited(_)) | Err(DataError::Network { .. })
        ) {
            debug!(path, "iwencai transient failure; retrying once");
            tokio::time::sleep(RETRY_PAUSE).await;
            attempt = self.http.post_json(&url, &headers, body).await;
        }
        attempt
    }

    /// Natural-language query (`hithink-market-query` by default).
    pub async fn query(&self, query: &str, page: u32, limit: u32) -> Result<WencaiRows, DataError> {
        self.query_skill("hithink-market-query", query, page, limit)
            .await
    }

    /// `query2data` with an explicit skill (e.g. `hithink-event-query`).
    pub async fn query_skill(
        &self,
        skill_id: &str,
        query: &str,
        page: u32,
        limit: u32,
    ) -> Result<WencaiRows, DataError> {
        let body = serde_json::json!({
            "query": query,
            "page": page,
            "limit": limit,
            "is_cache": true,
            "expand_index": true,
            "skill_id": skill_id,
        });
        let resp = self.post(QUERY2DATA, &body).await?;
        Ok(parse_rows(&resp))
    }

    /// `comprehensive/search` skills (`announcement-search`, `news-search`).
    pub async fn comprehensive_search(
        &self,
        skill_id: &str,
        query: &str,
        page: u32,
        limit: u32,
    ) -> Result<WencaiRows, DataError> {
        let body = serde_json::json!({
            "query": query,
            "page": page,
            "limit": limit,
            "skill_id": skill_id,
        });
        let resp = self.post(COMPREHENSIVE_SEARCH, &body).await?;
        Ok(parse_rows(&resp))
    }

    /// 龙虎榜 entries for one trade day (`YYYYMMDD`), cached per day.
    pub async fn dragon_tiger(&self, trade_date: &str) -> Result<WencaiRows, DataError> {
        self.key()?;
        let cache_key = format!("iwencai_lhb_{trade_date}");
        if let Some(hit) = self.cache.get::<WencaiRows>(&cache_key, DRAGON_TIGER_TTL) {
            return Ok(hit);
        }
        let out = self
            .query(
                &format!("{trade_date}龙虎榜 上榜原因 买卖额 净买入"),
                1,
                100,
            )
            .await?;
        self.cache.set(&cache_key, &out);
        Ok(out)
    }

    /// 消息面证据: announcements + news + structured events for one stock
    /// (name or code), 300s cache. Failures of individual skills are
    /// collected, not fatal — partial evidence beats none.
    pub async fn stock_events(&self, stock: &str) -> Result<StockEvents, DataError> {
        self.key()?;
        let cache_key = format!("iwencai_events_{stock}");
        if let Some(hit) = self.cache.get::<StockEvents>(&cache_key, EVENTS_TTL) {
            return Ok(hit);
        }

        let announcements = self
            .comprehensive_search("announcement-search", &format!("{stock}公告"), 1, 8)
            .await;
        let news = self
            .comprehensive_search("news-search", &format!("{stock}新闻"), 1, 8)
            .await;
        let events = self
            .query_skill("hithink-event-query", &format!("{stock}事件"), 1, 8)
            .await;

        // All three failing means the provider is effectively down.
        if let (Err(a), Err(n), Err(e)) = (&announcements, &news, &events) {
            return Err(DataError::AllFailed {
                op: "iwencai stock_events",
                details: format!("announcements: {a}; news: {n}; events: {e}"),
            });
        }
        let out = StockEvents {
            announcements: announcements.unwrap_or_else(|_| WencaiRows::default()),
            news: news.unwrap_or_else(|_| WencaiRows::default()),
            events: events.unwrap_or_else(|_| WencaiRows::default()),
        };
        self.cache.set(&cache_key, &out);
        Ok(out)
    }

    /// 板块归属 fallback for the EastMoney board snapshot: which
    /// industry/concept boards a stock belongs to.
    pub async fn sector_membership(&self, stock: &str) -> Result<WencaiRows, DataError> {
        self.query(&format!("{stock}所属行业板块 所属概念板块"), 1, 20)
            .await
    }
}

/// 消息面 evidence bundle for one stock.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StockEvents {
    /// `announcement-search` rows.
    pub announcements: WencaiRows,
    /// `news-search` rows.
    pub news: WencaiRows,
    /// `hithink-event-query` structured events.
    pub events: WencaiRows,
}

/// Lenient extraction of `datas` + `code_count`: the gateway has been
/// observed to nest the payload under `data` on some skills, so probe both
/// levels rather than pinning one envelope shape.
fn parse_rows(resp: &serde_json::Value) -> WencaiRows {
    let layer = |v: &serde_json::Value| -> Option<WencaiRows> {
        let rows = v.get("datas")?.as_array()?;
        let total = v
            .get("code_count")
            .and_then(|c| c.as_i64())
            .or_else(|| v.get("total").and_then(|c| c.as_i64()));
        Some(WencaiRows {
            rows: rows.clone(),
            total,
        })
    };
    layer(resp)
        .or_else(|| resp.get("data").and_then(layer))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_flat_envelope() {
        let resp = serde_json::json!({
            "datas": [{"股票代码": "600519", "净买入": 1.5e8}],
            "code_count": 42
        });
        let out = parse_rows(&resp);
        assert_eq!(out.rows.len(), 1);
        assert_eq!(out.total, Some(42));
    }

    #[test]
    fn parse_nested_envelope_and_total_fallback() {
        let resp = serde_json::json!({
            "data": {"datas": [{"a": 1}, {"a": 2}], "total": 2}
        });
        let out = parse_rows(&resp);
        assert_eq!(out.rows.len(), 2);
        assert_eq!(out.total, Some(2));
    }

    #[test]
    fn parse_garbage_is_empty_not_panic() {
        assert!(
            parse_rows(&serde_json::json!({"code": -1, "msg": "bad key"}))
                .rows
                .is_empty()
        );
        assert!(parse_rows(&serde_json::Value::Null).rows.is_empty());
    }

    #[test]
    fn unavailable_without_key() {
        let p = IwencaiOpenApi::new(
            Arc::new(HttpClient::new()),
            Arc::new(TtlCache::default()),
            None,
        );
        assert!(!p.available());
    }
}
