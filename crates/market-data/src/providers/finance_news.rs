//! Provider-neutral finance-news facade.
//!
//! NewsNow is one optional discovery provider. Official announcement mirrors
//! and validated user-configured JSON providers participate through the same
//! [`NewsProvider`] contract and fail independently.

use crate::cache::TtlCache;
use crate::http::HttpClient;
use astock_core::DataError;
use astock_entity_linking::EntityLinkSummary;
use astock_news_intelligence::ClusterExplanation;
use astock_security::UrlSecurityPolicy;
use astock_storage::Storage;
use async_trait::async_trait;
use futures::{stream, StreamExt};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::sync::Arc;
use std::time::Duration;

use super::em_datacenter::{EmDataCenter, NoticeNode};
use super::news_ingest::{
    bounded_raw, classify_data_error, ConfiguredJsonNewsProvider, JsonNewsProviderConfig,
    NewsCapabilities, NewsDeliveryMode, NewsIngestOutcome, NewsIngestProgressReporter,
    NewsIngestRequest, NewsIngestor, NewsPage, NewsProvider, NewsProviderError, NewsProviderHealth,
    NewsTrustTier,
};

pub const NEWSNOW_ENDPOINT: &str = "https://newsnow.busiyi.world/api/s";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_CONCURRENT: usize = 3;

/// 财经来源白名单：(稳定标识、中文名称、上游建议刷新间隔秒)。
pub const FINANCE_NEWS_SOURCES: &[(&str, &str, u64)] = &[
    ("cls-telegraph", "财联社电报", 300),
    ("cls-depth", "财联社深度", 600),
    ("cls-hot", "财联社热门", 600),
    ("jin10", "金十数据", 600),
    ("wallstreetcn-quick", "华尔街见闻快讯", 300),
    ("wallstreetcn-hot", "华尔街见闻热门", 1_800),
    ("mktnews-flash", "MKTNews 快讯", 120),
    ("gelonghui", "格隆汇事件", 120),
    ("fastbull-express", "法布财经快讯", 120),
    ("fastbull-news", "法布财经头条", 1_800),
    ("xueqiu-hotstock", "雪球热门股票", 120),
    ("wallstreetcn-news", "华尔街见闻最新资讯", 1_800),
];

pub const DEFAULT_FINANCE_NEWS_SOURCES: &[&str] = &[
    "cls-telegraph",
    "jin10",
    "wallstreetcn-quick",
    "mktnews-flash",
    "gelonghui",
    "cls-depth",
    "fastbull-express",
];

/// Tolerant source selection used at every public finance-news boundary.
/// Models and ordinary users naturally submit Chinese display names or short
/// aliases; those must never abort a larger research run. Unknown values stay
/// observable, while an entirely unusable selection falls back to defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinanceNewsSourceSelection {
    pub sources: Vec<String>,
    pub ignored_sources: Vec<String>,
    pub used_default: bool,
}

pub fn normalize_finance_news_sources(requested: Option<&[String]>) -> FinanceNewsSourceSelection {
    let mut sources = Vec::new();
    let mut ignored_sources = Vec::new();
    if let Some(requested) = requested {
        for raw in requested.iter().take(32) {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(source) = finance_news_source_alias(trimmed) {
                if !sources.iter().any(|current| current == source) {
                    sources.push(source.to_string());
                }
            } else if !ignored_sources.iter().any(|current| current == trimmed) {
                ignored_sources.push(trimmed.to_string());
            }
        }
    }
    let used_default = sources.is_empty();
    if used_default {
        sources.extend(
            DEFAULT_FINANCE_NEWS_SOURCES
                .iter()
                .map(|value| value.to_string()),
        );
    }
    FinanceNewsSourceSelection {
        sources,
        ignored_sources,
        used_default,
    }
}

fn finance_news_source_alias(raw: &str) -> Option<&'static str> {
    let normalized = raw
        .trim()
        .to_ascii_lowercase()
        .replace('_', "-")
        .split_whitespace()
        .collect::<String>();
    match normalized.as_str() {
        "cls" | "cls-telegraph" | "财联社" | "财联社电报" => Some("cls-telegraph"),
        "cls-depth" | "财联社深度" => Some("cls-depth"),
        "cls-hot" | "财联社热门" => Some("cls-hot"),
        "jin10" | "jin-10" | "金十" | "金十数据" => Some("jin10"),
        "wallstreetcn" | "wallstreetcn-quick" | "wscn" | "华尔街见闻" | "华尔街见闻快讯" => {
            Some("wallstreetcn-quick")
        }
        "wallstreetcn-news" | "华尔街见闻资讯" | "华尔街见闻新闻" | "华尔街见闻最新资讯" => {
            Some("wallstreetcn-news")
        }
        "wallstreetcn-hot" | "华尔街见闻热门" | "华尔街见闻最热" => {
            Some("wallstreetcn-hot")
        }
        "mktnews" | "mktnews-flash" | "mktnews快讯" => Some("mktnews-flash"),
        "gelonghui" | "格隆汇" | "格隆汇事件" => Some("gelonghui"),
        "fastbull" | "fastbull-express" | "法布财经" | "法布财经快讯" => {
            Some("fastbull-express")
        }
        "fastbull-news" | "法布财经头条" | "法布财经新闻" => Some("fastbull-news"),
        "xueqiu" | "xueqiu-hotstock" | "xueqiu-hot-stock" | "雪球" | "雪球热股" | "雪球热门"
        | "雪球热门股票" => Some("xueqiu-hotstock"),
        _ => None,
    }
}

fn newsnow_per_channel_limit(final_limit: usize, channels: usize) -> usize {
    final_limit
        .clamp(1, 100)
        .div_ceil(channels.max(1))
        .saturating_mul(3)
        .clamp(1, 100)
}

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
    /// Provider instance that fetched the item (separate from channel/source).
    pub provider_id: String,
    /// Evidence authority classification exposed to Agent and UI.
    pub trust_tier: NewsTrustTier,
    pub trust_tier_name: String,
    pub license: String,
    pub parser_version: String,
    /// Immutable archive revision backing this normalized item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_revision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_cluster_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_relationship: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_relationship_name: Option<String>,
    #[serde(default)]
    pub independent_source_count: usize,
    #[serde(default)]
    pub old_republication: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_explanation: Option<ClusterExplanation>,
    /// Only rule-validated, evidence-backed entity mappings are exposed to Agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_links: Vec<EntityLinkSummary>,
    #[serde(default)]
    pub entity_review_required: bool,
    /// Bounded original provider row for offline re-parsing/audit. Agent strips
    /// this field before model context to avoid needless prompt expansion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_payload: Option<Value>,
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
    #[serde(default)]
    http_status: Option<u16>,
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    last_modified: Option<String>,
}

/// Stable facade used by Agent/UI. Provider additions do not change callers.
pub struct FinanceNewsProvider {
    ingestor: NewsIngestor,
    research_ingestor: NewsIngestor,
}

impl FinanceNewsProvider {
    pub fn new(http: Arc<HttpClient>, cache: Arc<TtlCache>) -> Self {
        Self::build(http, cache, None, None)
    }

    /// Build the facade from explicit provider plugins. This is useful for
    /// commercial provider extensions and deterministic end-to-end tests;
    /// the same validation, rate limits, retries, circuit breakers and
    /// persistence rules still apply.
    pub fn from_providers(
        providers: Vec<Arc<dyn NewsProvider>>,
        storage: Option<Storage>,
    ) -> Result<Self, NewsProviderError> {
        let research_ingestor =
            NewsIngestor::without_document_persistence(providers.clone(), storage.clone())?;
        Ok(Self {
            ingestor: NewsIngestor::new(providers, storage)?,
            research_ingestor,
        })
    }

    pub fn with_storage(
        http: Arc<HttpClient>,
        cache: Arc<TtlCache>,
        storage: Storage,
        announcements: Arc<EmDataCenter>,
    ) -> Self {
        Self::build(http, cache, Some(storage), Some(announcements))
    }

    fn build(
        http: Arc<HttpClient>,
        cache: Arc<TtlCache>,
        storage: Option<Storage>,
        announcements: Option<Arc<EmDataCenter>>,
    ) -> Self {
        let endpoints = env::var("ASTOCK_NEWSNOW_ENDPOINTS")
            .ok()
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .filter(|values| !values.is_empty())
            .unwrap_or_else(|| vec![NEWSNOW_ENDPOINT.to_string()]);
        let mut providers: Vec<Arc<dyn NewsProvider>> = Vec::new();
        for (index, endpoint) in endpoints.into_iter().enumerate() {
            match NewsNowProvider::new(
                format!("newsnow-{}", index + 1),
                endpoint,
                http.clone(),
                cache.clone(),
            ) {
                Ok(provider) => providers.push(Arc::new(provider)),
                Err(error) => tracing::warn!(
                    provider = %error.provider_id,
                    kind = ?error.kind,
                    "ignored invalid NewsNow provider configuration"
                ),
            }
        }
        if let Some(announcements) = announcements {
            providers.push(Arc::new(OfficialAnnouncementProvider::new(announcements)));
        }
        if let Ok(raw) = env::var("ASTOCK_NEWS_PROVIDERS") {
            match serde_json::from_str::<Vec<JsonNewsProviderConfig>>(&raw) {
                Ok(configs) => {
                    for config in configs {
                        match ConfiguredJsonNewsProvider::new(config, http.clone(), cache.clone()) {
                            Ok(provider) => providers.push(Arc::new(provider)),
                            Err(error) => tracing::warn!(
                                provider = %error.provider_id,
                                kind = ?error.kind,
                                "ignored invalid configured news provider"
                            ),
                        }
                    }
                }
                Err(_) => tracing::warn!("ASTOCK_NEWS_PROVIDERS is not valid JSON; ignored"),
            }
        }
        let research_ingestor =
            NewsIngestor::without_document_persistence(providers.clone(), storage.clone())
                .expect("built-in research news provider configuration must be valid");
        let ingestor = NewsIngestor::new(providers, storage)
            .expect("built-in news provider configuration must be valid");
        Self {
            ingestor,
            research_ingestor,
        }
    }

    /// Load several allowlisted sources and merge them newest-first.
    pub async fn latest(
        &self,
        sources: &[String],
        per_source: usize,
    ) -> Result<FinanceNewsBatch, DataError> {
        let selection = normalize_finance_news_sources(Some(sources));
        let selected = self
            .ingestor
            .provider_ids()
            .into_iter()
            .filter(|id| id != "official-a-share-announcements")
            .collect::<Vec<_>>();
        let outcome = self
            .ingestor
            .ingest(
                NewsIngestRequest {
                    source_ids: selection.sources,
                    limit: per_source.clamp(1, 100),
                    ..Default::default()
                },
                Some(&selected),
            )
            .await;
        outcome_to_batch(outcome)
    }

    /// Unified research query. Official disclosures participate only when a
    /// symbol is supplied; public feeds remain discovery sources.
    pub async fn research(
        &self,
        sources: &[String],
        symbol: Option<&str>,
        keyword: Option<&str>,
        limit: usize,
    ) -> Result<FinanceNewsBatch, DataError> {
        self.research_with_progress(sources, symbol, keyword, limit, None)
            .await
    }

    pub async fn research_with_progress(
        &self,
        sources: &[String],
        symbol: Option<&str>,
        keyword: Option<&str>,
        limit: usize,
        progress: Option<NewsIngestProgressReporter>,
    ) -> Result<FinanceNewsBatch, DataError> {
        let selection = normalize_finance_news_sources(Some(sources));
        let selected = symbol.is_none().then(|| {
            self.research_ingestor
                .provider_ids()
                .into_iter()
                .filter(|id| id != "official-a-share-announcements")
                .collect::<Vec<_>>()
        });
        let outcome = self
            .research_ingestor
            .ingest_with_progress(
                NewsIngestRequest {
                    source_ids: selection.sources,
                    symbol: symbol.map(ToString::to_string),
                    keyword: keyword.map(ToString::to_string),
                    limit: limit.clamp(1, 200),
                    ..Default::default()
                },
                selected.as_deref(),
                progress,
            )
            .await;
        outcome_to_batch(outcome)
    }

    pub async fn provider_health(&self) -> Vec<NewsProviderHealth> {
        self.ingestor.health().await
    }

    pub async fn set_provider_enabled(
        &self,
        provider_id: &str,
        enabled: bool,
    ) -> Result<(), NewsProviderError> {
        self.ingestor.set_enabled(provider_id, enabled).await?;
        self.research_ingestor
            .set_enabled(provider_id, enabled)
            .await
    }
}

fn outcome_to_batch(outcome: NewsIngestOutcome) -> Result<FinanceNewsBatch, DataError> {
    let batch = FinanceNewsBatch {
        items: outcome.items,
        successful_sources: outcome.successful_providers,
        stale_sources: outcome.stale_providers,
        errors: outcome.errors.iter().map(ToString::to_string).collect(),
    };
    if batch.items.is_empty()
        && batch.successful_sources.is_empty()
        && batch.stale_sources.is_empty()
    {
        Err(DataError::AllFailed {
            op: "finance news",
            details: if batch.errors.is_empty() {
                "资讯来源注册表为空或全部被筛选/停用".into()
            } else {
                batch.errors.join("; ")
            },
        })
    } else {
        Ok(batch)
    }
}

struct NewsNowProvider {
    capabilities: NewsCapabilities,
    http: Arc<HttpClient>,
    cache: Arc<TtlCache>,
}

impl NewsNowProvider {
    fn new(
        provider_id: String,
        endpoint: String,
        http: Arc<HttpClient>,
        cache: Arc<TtlCache>,
    ) -> Result<Self, NewsProviderError> {
        let capabilities = NewsCapabilities {
            provider_id,
            display_name: "NewsNow 公共快讯".to_string(),
            endpoint,
            modes: [NewsDeliveryMode::ScheduledIndex].into(),
            min_refresh_secs: 120,
            rate_limit_per_minute: 30,
            license: "公共聚合发现层；必须回链并核对原始来源许可".to_string(),
            trust_tier: NewsTrustTier::PublicAggregator,
            parser_version: "newsnow-v2".to_string(),
            supports_symbol_filter: false,
        };
        capabilities.validate()?;
        Ok(Self {
            capabilities,
            http,
            cache,
        })
    }

    async fn fetch_source(
        &self,
        source: &str,
        limit: usize,
    ) -> Result<SourceSnapshot, NewsProviderError> {
        let (_, _, ttl_seconds) = source_meta(source).ok_or_else(|| {
            NewsProviderError::new(
                &self.capabilities.provider_id,
                super::news_ingest::NewsErrorKind::Configuration,
                format!("不支持的 NewsNow 频道 {source}"),
                false,
            )
        })?;
        let key = format!(
            "finance_news_{}_{}_{}",
            self.capabilities.provider_id, source, limit
        );
        if let Some(snapshot) = self
            .cache
            .get::<SourceSnapshot>(&key, Duration::from_secs(ttl_seconds))
        {
            return Ok(snapshot);
        }
        let params = vec![
            ("id".to_string(), source.to_string()),
            ("latest".to_string(), "true".to_string()),
        ];
        let response = self
            .http
            .get_text(&self.capabilities.endpoint, &params)
            .await
            .map_err(|error| classify_data_error(&self.capabilities.provider_id, error))?;
        if response.body.len() > MAX_RESPONSE_BYTES {
            return Err(NewsProviderError::new(
                &self.capabilities.provider_id,
                super::news_ingest::NewsErrorKind::Parse,
                "响应超过 2 MiB",
                false,
            )
            .with_raw_evidence(response.body.as_bytes()));
        }
        if response
            .content_type
            .as_deref()
            .is_some_and(|value| !value.to_ascii_lowercase().contains("json"))
        {
            return Err(NewsProviderError::new(
                &self.capabilities.provider_id,
                super::news_ingest::NewsErrorKind::Parse,
                "响应不是 JSON",
                false,
            )
            .with_raw_evidence(response.body.as_bytes()));
        }
        let value: Value = serde_json::from_str(&response.body).map_err(|_| {
            NewsProviderError::new(
                &self.capabilities.provider_id,
                super::news_ingest::NewsErrorKind::Parse,
                "响应 JSON 无法解析",
                false,
            )
            .with_raw_evidence(response.body.as_bytes())
        })?;
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let rows = value.get("items").and_then(Value::as_array);
        if !matches!(status.as_str(), "success" | "cache") || rows.is_none() {
            return Err(NewsProviderError::new(
                &self.capabilities.provider_id,
                super::news_ingest::NewsErrorKind::Parse,
                "缺少有效 status/items",
                false,
            )
            .with_raw_evidence(response.body.as_bytes()));
        }
        let items = rows
            .into_iter()
            .flatten()
            .take(limit)
            .enumerate()
            .filter_map(|(index, row)| normalize_item(&self.capabilities, source, row, index + 1))
            .collect::<Vec<_>>();
        let snapshot = SourceSnapshot {
            items,
            http_status: Some(response.status),
            etag: response.etag,
            last_modified: response.last_modified,
        };
        self.cache.set(&key, &snapshot);
        Ok(snapshot)
    }
}

#[async_trait]
impl NewsProvider for NewsNowProvider {
    fn capabilities(&self) -> &NewsCapabilities {
        &self.capabilities
    }

    async fn fetch(&self, request: NewsIngestRequest) -> Result<NewsPage, NewsProviderError> {
        let sources = if request.source_ids.is_empty() {
            FINANCE_NEWS_SOURCES
                .iter()
                .map(|row| row.0.to_string())
                .collect::<Vec<_>>()
        } else {
            request.source_ids
        };
        let total_channels = sources.len();
        // `request.limit` is the final provider budget, not a multiplier for
        // every channel. A small over-fetch keeps important/keyword filtering
        // useful without turning 15 requested rows into 75 archive jobs.
        let per_channel = newsnow_per_channel_limit(request.limit, total_channels);
        let outcomes = stream::iter(sources.into_iter().map(|source| async move {
            let result = self.fetch_source(&source, per_channel).await;
            (source, result)
        }))
        .buffer_unordered(MAX_CONCURRENT)
        .collect::<Vec<_>>()
        .await;
        let mut items = Vec::new();
        let mut errors = Vec::new();
        let mut http_status = None;
        let mut etag = None;
        let mut last_modified = None;
        for (source, outcome) in outcomes {
            match outcome {
                Ok(snapshot) => {
                    items.extend(snapshot.items);
                    http_status = http_status.or(snapshot.http_status);
                    etag = etag.or(snapshot.etag);
                    last_modified = last_modified.or(snapshot.last_modified);
                }
                Err(error) => errors.push((source, error)),
            }
        }
        if items.is_empty() {
            if errors.is_empty() {
                return Err(NewsProviderError::new(
                    &self.capabilities.provider_id,
                    super::news_ingest::NewsErrorKind::Empty,
                    "所有 NewsNow 频道均为空",
                    false,
                ));
            }
            let retryable = errors.iter().all(|(_, error)| error.retryable);
            let kind = errors
                .first()
                .map(|(_, error)| error.kind)
                .unwrap_or(super::news_ingest::NewsErrorKind::Empty);
            let details = errors
                .iter()
                .map(|(source, error)| format!("{source} [{:?}]: {}", error.kind, error.message))
                .collect::<Vec<_>>()
                .join("；");
            return Err(NewsProviderError::new(
                &self.capabilities.provider_id,
                kind,
                format!("{}/{} 个频道失败：{details}", errors.len(), total_channels),
                retryable,
            ));
        }
        items.sort_by(|left, right| {
            right
                .published_at_ms
                .unwrap_or_default()
                .cmp(&left.published_at_ms.unwrap_or_default())
                .then_with(|| left.rank.cmp(&right.rank))
        });
        items.truncate(request.limit.clamp(1, 100));
        let next_cursor = items.first().map(|item| item.id.clone());
        Ok(NewsPage {
            items,
            next_cursor,
            http_status,
            etag,
            last_modified,
            diagnostics: errors.into_iter().map(|(_, error)| error).collect(),
        })
    }
}

struct OfficialAnnouncementProvider {
    capabilities: NewsCapabilities,
    announcements: Arc<EmDataCenter>,
}

impl OfficialAnnouncementProvider {
    fn new(announcements: Arc<EmDataCenter>) -> Self {
        Self {
            capabilities: NewsCapabilities {
                provider_id: "official-a-share-announcements".to_string(),
                display_name: "A 股公司公告".to_string(),
                endpoint: "https://np-anotice-stock.eastmoney.com/api/security/ann".to_string(),
                modes: [
                    NewsDeliveryMode::ScheduledIndex,
                    NewsDeliveryMode::PublishedIncremental,
                ]
                .into(),
                min_refresh_secs: 300,
                rate_limit_per_minute: 20,
                license: "上市公司公开披露；展示标题与原公告回链".to_string(),
                trust_tier: NewsTrustTier::FirstPartyDisclosure,
                parser_version: "em-announcement-v1".to_string(),
                supports_symbol_filter: true,
            },
            announcements,
        }
    }
}

#[async_trait]
impl NewsProvider for OfficialAnnouncementProvider {
    fn capabilities(&self) -> &NewsCapabilities {
        &self.capabilities
    }

    async fn fetch(&self, request: NewsIngestRequest) -> Result<NewsPage, NewsProviderError> {
        let Some(symbol) = request.symbol.as_deref().filter(|value| !value.is_empty()) else {
            return Ok(NewsPage::default());
        };
        let fetched = self
            .announcements
            .notices(Some(symbol), NoticeNode::All, None, None, 1)
            .await
            .map_err(|error| classify_data_error(&self.capabilities.provider_id, error))?;
        let items = fetched
            .data
            .into_iter()
            .take(request.limit.clamp(1, 200))
            .enumerate()
            .map(|(index, row)| FinanceNewsItem {
                id: format!("{}:{}", self.capabilities.provider_id, row.art_code),
                source_id: "official-announcement".to_string(),
                source_name: "上市公司公告".to_string(),
                title: row.title.clone(),
                summary: row.column_name.clone(),
                url: row.url.clone(),
                published_at: row
                    .notice_date
                    .map(|date| date.to_string())
                    .unwrap_or_default(),
                published_at_ms: row.notice_date.and_then(|date| {
                    date.and_hms_opt(0, 0, 0)
                        .map(|value| value.and_utc().timestamp_millis() - 8 * 3_600_000)
                }),
                important: matches!(row.column_name.as_str(), "重大事项" | "风险提示"),
                rank: index + 1,
                provider_id: self.capabilities.provider_id.clone(),
                trust_tier: self.capabilities.trust_tier,
                trust_tier_name: self.capabilities.trust_tier.chinese_name().to_string(),
                license: self.capabilities.license.clone(),
                parser_version: self.capabilities.parser_version.clone(),
                document_revision_id: None,
                event_cluster_id: None,
                event_relationship: None,
                event_relationship_name: None,
                independent_source_count: 1,
                old_republication: false,
                cluster_explanation: None,
                entity_links: Vec::new(),
                entity_review_required: false,
                raw_payload: serde_json::to_value(&row)
                    .ok()
                    .as_ref()
                    .and_then(bounded_raw),
            })
            .collect::<Vec<_>>();
        Ok(NewsPage {
            next_cursor: items.first().map(|item| item.id.clone()),
            items,
            http_status: Some(200),
            etag: None,
            last_modified: None,
            diagnostics: Vec::new(),
        })
    }
}

fn source_meta(source: &str) -> Option<(&'static str, &'static str, u64)> {
    FINANCE_NEWS_SOURCES
        .iter()
        .copied()
        .find(|(id, _, _)| *id == source)
}

fn normalize_item(
    capabilities: &NewsCapabilities,
    source: &str,
    raw: &Value,
    rank: usize,
) -> Option<FinanceNewsItem> {
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
        provider_id: capabilities.provider_id.clone(),
        trust_tier: capabilities.trust_tier,
        trust_tier_name: capabilities.trust_tier.chinese_name().to_string(),
        license: capabilities.license.clone(),
        parser_version: capabilities.parser_version.clone(),
        document_revision_id: None,
        event_cluster_id: None,
        event_relationship: None,
        event_relationship_name: None,
        independent_source_count: 1,
        old_republication: false,
        cluster_explanation: None,
        entity_links: Vec::new(),
        entity_review_required: false,
        raw_payload: bounded_raw(raw),
    })
}

#[cfg(test)]
impl FinanceNewsItem {
    pub(crate) fn fixture(capabilities: &NewsCapabilities, title: &str) -> Self {
        Self {
            id: format!("{}:fixture", capabilities.provider_id),
            source_id: capabilities.provider_id.clone(),
            source_name: capabilities.display_name.clone(),
            title: title.to_string(),
            summary: String::new(),
            url: capabilities.endpoint.clone(),
            published_at: String::new(),
            published_at_ms: Some(1),
            important: false,
            rank: 1,
            provider_id: capabilities.provider_id.clone(),
            trust_tier: capabilities.trust_tier,
            trust_tier_name: capabilities.trust_tier.chinese_name().to_string(),
            license: capabilities.license.clone(),
            parser_version: capabilities.parser_version.clone(),
            document_revision_id: None,
            event_cluster_id: None,
            event_relationship: None,
            event_relationship_name: None,
            independent_source_count: 1,
            old_republication: false,
            cluster_explanation: None,
            entity_links: Vec::new(),
            entity_review_required: false,
            raw_payload: None,
        }
    }
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
    use std::collections::HashSet;

    #[test]
    fn sanitizes_news_items() {
        let row = serde_json::json!({
            "id": "one",
            "title": "<b>政策</b> &amp; 市场",
            "pubDate": 1_786_420_800,
            "url": "javascript:alert(1)",
            "extra": {"hover": "补充", "info": "重要"}
        });
        let capabilities = NewsNowProvider::new(
            "newsnow-test".into(),
            NEWSNOW_ENDPOINT.into(),
            Arc::new(HttpClient::new()),
            Arc::new(TtlCache::default()),
        )
        .unwrap()
        .capabilities;
        let item = normalize_item(&capabilities, "cls-telegraph", &row, 1).unwrap();
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

    #[test]
    fn source_aliases_are_normalized_and_unknown_values_do_not_abort() {
        let requested = vec![
            "财联社".to_string(),
            "金十数据".to_string(),
            "华尔街见闻".to_string(),
            "CLS".to_string(),
            "不存在的来源".to_string(),
        ];
        let selection = normalize_finance_news_sources(Some(&requested));
        assert_eq!(
            selection.sources,
            vec!["cls-telegraph", "jin10", "wallstreetcn-quick"]
        );
        assert_eq!(selection.ignored_sources, vec!["不存在的来源"]);
        assert!(!selection.used_default);
    }

    #[test]
    fn entirely_unknown_source_selection_falls_back_to_defaults() {
        let requested = vec!["模型刚发明的来源".to_string()];
        let selection = normalize_finance_news_sources(Some(&requested));
        assert!(selection.used_default);
        assert_eq!(selection.sources.len(), DEFAULT_FINANCE_NEWS_SOURCES.len());
        assert_eq!(selection.ignored_sources, requested);
    }

    #[test]
    fn final_limit_is_not_multiplied_by_channel_count() {
        assert_eq!(newsnow_per_channel_limit(15, 5), 9);
        assert_eq!(newsnow_per_channel_limit(1, 7), 3);
        assert_eq!(newsnow_per_channel_limit(100, 1), 100);
    }

    #[test]
    fn healthy_no_match_is_not_misreported_as_provider_outage() {
        let batch = outcome_to_batch(NewsIngestOutcome {
            successful_providers: vec!["fixture".into()],
            ..Default::default()
        })
        .unwrap();
        assert!(batch.items.is_empty());
        assert_eq!(batch.successful_sources, vec!["fixture"]);
    }

    #[test]
    fn empty_registry_error_is_never_blank() {
        let error = outcome_to_batch(NewsIngestOutcome::default()).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("资讯来源注册表"));
        assert!(!message.ends_with(": "));
    }
}
