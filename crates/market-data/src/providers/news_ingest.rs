//! Pluggable, provider-neutral news ingestion runtime.
//!
//! Providers declare capabilities and return one normalized model. The
//! runtime owns independent rate limits, retry/backoff, circuit state,
//! persistent cursors, last-good fallback, manual disable and health metrics.
//! It never treats a public aggregator as authoritative evidence.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use astock_security::{redact_text, UrlSecurityPolicy};
use astock_storage::{
    EvidenceTimestamp, NewsArchiveInput, NewsObservationInput, NewsProviderArchiveState, Storage,
};
use async_trait::async_trait;
use dashmap::DashMap;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Semaphore;

use crate::cache::TtlCache;
use crate::http::HttpClient;

use super::finance_news::FinanceNewsItem;

const MAX_CONCURRENT_PROVIDERS: usize = 4;
const MAX_RETRIES: u32 = 2;
const RETRY_BASE: Duration = Duration::from_millis(250);
const FAILURE_THRESHOLD: u32 = 3;
const BASE_COOLDOWN: Duration = Duration::from_secs(30);
const MAX_COOLDOWN: Duration = Duration::from_secs(30 * 60);
const MAX_RAW_PAYLOAD_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewsTrustTier {
    FirstPartyDisclosure,
    LicensedMedia,
    PublicAggregator,
    SearchLead,
}

impl NewsTrustTier {
    pub fn chinese_name(self) -> &'static str {
        match self {
            Self::FirstPartyDisclosure => "一手披露",
            Self::LicensedMedia => "授权媒体",
            Self::PublicAggregator => "公共聚合快讯",
            Self::SearchLead => "搜索线索",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewsDeliveryMode {
    PushStream,
    ScheduledIndex,
    PublishedIncremental,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsCapabilities {
    pub provider_id: String,
    pub display_name: String,
    pub endpoint: String,
    pub modes: BTreeSet<NewsDeliveryMode>,
    pub min_refresh_secs: u64,
    pub rate_limit_per_minute: u32,
    pub license: String,
    pub trust_tier: NewsTrustTier,
    pub parser_version: String,
    pub supports_symbol_filter: bool,
}

impl NewsCapabilities {
    pub fn validate(&self) -> Result<(), NewsProviderError> {
        if !valid_id(&self.provider_id) {
            return Err(NewsProviderError::configuration(
                &self.provider_id,
                "provider_id 只能包含字母、数字、点、下划线和短横线",
            ));
        }
        if self.display_name.trim().is_empty()
            || self.license.trim().is_empty()
            || self.parser_version.trim().is_empty()
            || self.modes.is_empty()
        {
            return Err(NewsProviderError::configuration(
                &self.provider_id,
                "名称、许可、解析器版本和采集模式不能为空",
            ));
        }
        if !(15..=86_400).contains(&self.min_refresh_secs) {
            return Err(NewsProviderError::configuration(
                &self.provider_id,
                "刷新间隔必须在 15 秒到 24 小时之间",
            ));
        }
        if !(1..=600).contains(&self.rate_limit_per_minute) {
            return Err(NewsProviderError::configuration(
                &self.provider_id,
                "每分钟请求上限必须在 1 到 600 之间",
            ));
        }
        UrlSecurityPolicy::default()
            .validate_static(&self.endpoint)
            .map_err(|error| {
                NewsProviderError::configuration(&self.provider_id, error.to_string())
            })?;
        Ok(())
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsIngestRequest {
    pub source_ids: Vec<String>,
    pub symbol: Option<String>,
    pub keyword: Option<String>,
    pub cursor: Option<String>,
    pub published_after_ms: Option<i64>,
    pub limit: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NewsPage {
    pub items: Vec<FinanceNewsItem>,
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub http_status: Option<u16>,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewsErrorKind {
    Configuration,
    Authentication,
    RateLimited,
    Timeout,
    Network,
    Parse,
    Empty,
    CircuitOpen,
    Disabled,
    Storage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{provider_id} [{kind:?}]: {message}")]
pub struct NewsProviderError {
    pub provider_id: String,
    pub kind: NewsErrorKind,
    pub message: String,
    pub retryable: bool,
    /// Bounded original bytes retained only for archive diagnostics. It is
    /// never serialized into Agent/UI error payloads.
    #[serde(skip)]
    pub raw_evidence: Option<Vec<u8>>,
}

impl NewsProviderError {
    pub fn new(
        provider_id: impl Into<String>,
        kind: NewsErrorKind,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            kind,
            message: message.into(),
            retryable,
            raw_evidence: None,
        }
    }

    pub fn with_raw_evidence(mut self, raw: &[u8]) -> Self {
        self.raw_evidence = Some(raw[..raw.len().min(MAX_RAW_PAYLOAD_BYTES)].to_vec());
        self
    }

    fn configuration(provider_id: &str, message: impl Into<String>) -> Self {
        Self::new(provider_id, NewsErrorKind::Configuration, message, false)
    }
}

#[async_trait]
pub trait NewsProvider: Send + Sync {
    fn capabilities(&self) -> &NewsCapabilities;
    async fn fetch(&self, request: NewsIngestRequest) -> Result<NewsPage, NewsProviderError>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewsProviderHealth {
    pub provider_id: String,
    pub display_name: String,
    pub enabled: bool,
    pub circuit_state: String,
    pub trust_tier: NewsTrustTier,
    pub trust_tier_name: String,
    pub modes: BTreeSet<NewsDeliveryMode>,
    pub license: String,
    pub endpoint: String,
    pub min_refresh_secs: u64,
    pub rate_limit_per_minute: u32,
    pub last_success_at: Option<i64>,
    pub last_latency_ms: Option<u64>,
    pub attempts: u64,
    pub failures: u64,
    pub failure_rate: f64,
    pub stale: bool,
    pub cursor_present: bool,
    pub cooldown_remaining_secs: Option<u64>,
    pub last_error_kind: Option<NewsErrorKind>,
    pub archived_documents: u64,
    pub archived_revisions: u64,
    pub archive_last_observed_at: Option<i64>,
    pub stale_age_secs: Option<u64>,
}

#[derive(Debug)]
struct ProviderRuntime {
    enabled: bool,
    consecutive_failures: u32,
    cooldown: Duration,
    open_until: Option<Instant>,
    next_allowed: Option<Instant>,
    attempts: u64,
    failures: u64,
    last_success_at: Option<i64>,
    last_latency_ms: Option<u64>,
    last_error_kind: Option<NewsErrorKind>,
    cursor_present: bool,
}

impl Default for ProviderRuntime {
    fn default() -> Self {
        Self {
            enabled: true,
            consecutive_failures: 0,
            cooldown: BASE_COOLDOWN,
            open_until: None,
            next_allowed: None,
            attempts: 0,
            failures: 0,
            last_success_at: None,
            last_latency_ms: None,
            last_error_kind: None,
            cursor_present: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NewsIngestOutcome {
    pub items: Vec<FinanceNewsItem>,
    pub successful_providers: Vec<String>,
    pub stale_providers: Vec<String>,
    pub errors: Vec<NewsProviderError>,
}

pub struct NewsIngestor {
    providers: Vec<Arc<dyn NewsProvider>>,
    runtime: DashMap<String, Mutex<ProviderRuntime>>,
    permits: Semaphore,
    last_good: DashMap<String, NewsPage>,
    storage: Option<Storage>,
}

impl NewsIngestor {
    pub fn new(
        providers: Vec<Arc<dyn NewsProvider>>,
        storage: Option<Storage>,
    ) -> Result<Self, NewsProviderError> {
        let mut ids = HashSet::new();
        let runtime = DashMap::new();
        for provider in &providers {
            let capabilities = provider.capabilities();
            capabilities.validate()?;
            if !ids.insert(capabilities.provider_id.clone()) {
                return Err(NewsProviderError::configuration(
                    &capabilities.provider_id,
                    "provider_id 重复",
                ));
            }
            runtime.insert(
                capabilities.provider_id.clone(),
                Mutex::new(ProviderRuntime::default()),
            );
        }
        Ok(Self {
            providers,
            runtime,
            permits: Semaphore::new(MAX_CONCURRENT_PROVIDERS),
            last_good: DashMap::new(),
            storage,
        })
    }

    pub fn provider_ids(&self) -> Vec<String> {
        self.providers
            .iter()
            .map(|provider| provider.capabilities().provider_id.clone())
            .collect()
    }

    pub async fn ingest(
        &self,
        request: NewsIngestRequest,
        selected: Option<&[String]>,
    ) -> NewsIngestOutcome {
        let selected = selected.map(|ids| ids.iter().cloned().collect::<HashSet<_>>());
        let providers = self
            .providers
            .iter()
            .filter(|provider| {
                selected
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&provider.capabilities().provider_id))
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut pending = FuturesUnordered::new();
        for provider in providers {
            pending.push(self.fetch_named(provider, request.clone()));
        }
        let mut rows = Vec::new();
        while let Some(row) = pending.next().await {
            rows.push(row);
        }
        let mut outcome = NewsIngestOutcome::default();
        for (provider_id, result) in rows {
            match result {
                Ok((page, stale)) => {
                    if stale {
                        outcome.stale_providers.push(provider_id);
                    } else {
                        outcome.successful_providers.push(provider_id);
                    }
                    outcome.items.extend(page.items);
                }
                Err(error) => outcome.errors.push(error),
            }
        }
        let mut seen = HashSet::new();
        outcome.items.retain(|item| seen.insert(item.id.clone()));
        outcome.items.sort_by(|left, right| {
            right
                .published_at_ms
                .unwrap_or_default()
                .cmp(&left.published_at_ms.unwrap_or_default())
                .then_with(|| left.rank.cmp(&right.rank))
        });
        outcome
    }

    async fn fetch_named(
        &self,
        provider: Arc<dyn NewsProvider>,
        request: NewsIngestRequest,
    ) -> (String, Result<(NewsPage, bool), NewsProviderError>) {
        let id = provider.capabilities().provider_id.clone();
        (id, self.fetch_one(provider, request).await)
    }

    async fn fetch_one(
        &self,
        provider: Arc<dyn NewsProvider>,
        mut request: NewsIngestRequest,
    ) -> Result<(NewsPage, bool), NewsProviderError> {
        let capabilities = provider.capabilities().clone();
        let provider_id = capabilities.provider_id.clone();
        self.sync_disabled(&provider_id).await;
        self.sync_persisted_runtime(&provider_id).await;
        let blocked = {
            let runtime = self.runtime.get(&provider_id).expect("registered provider");
            let state = runtime.lock();
            if !state.enabled {
                Some(NewsProviderError::new(
                    &provider_id,
                    NewsErrorKind::Disabled,
                    "来源已被用户手动停用",
                    false,
                ))
            } else if state.open_until.is_some_and(|until| Instant::now() < until) {
                Some(NewsProviderError::new(
                    &provider_id,
                    NewsErrorKind::CircuitOpen,
                    "来源熔断冷却中",
                    false,
                ))
            } else {
                None
            }
        };
        if let Some(error) = blocked {
            return self.fallback_or_error(error).await;
        }
        if request.cursor.is_none() {
            request.cursor = self.load_cursor(&provider_id).await;
        }
        self.wait_rate_limit(&capabilities).await;
        let _permit = self.permits.acquire().await.map_err(|_| {
            NewsProviderError::new(
                &provider_id,
                NewsErrorKind::Network,
                "采集调度器已关闭",
                true,
            )
        })?;
        let started = Instant::now();
        let mut final_error = None;
        for attempt in 0..=MAX_RETRIES {
            let attempt_started = Instant::now();
            self.mark_attempt(&provider_id);
            match provider.fetch(request.clone()).await {
                Ok(mut page) => {
                    self.mark_success(&provider_id, started.elapsed());
                    self.persist_success(&capabilities, &mut page, started.elapsed())
                        .await;
                    self.persist_runtime_state(&provider_id).await;
                    self.last_good.insert(provider_id.clone(), page.clone());
                    return Ok((page, false));
                }
                Err(error) => {
                    let retryable = error.retryable;
                    self.mark_failed_attempt(&provider_id, error.kind);
                    self.persist_failure_observation(
                        &capabilities,
                        &error,
                        attempt_started.elapsed(),
                    )
                    .await;
                    self.persist_runtime_state(&provider_id).await;
                    final_error = Some(error);
                    if !retryable || attempt == MAX_RETRIES {
                        break;
                    }
                    tokio::time::sleep(RETRY_BASE * 2_u32.pow(attempt)).await;
                }
            }
        }
        let error = final_error.unwrap_or_else(|| {
            NewsProviderError::new(
                &provider_id,
                NewsErrorKind::Empty,
                "来源没有返回结果",
                false,
            )
        });
        self.mark_failure(&provider_id, error.kind);
        self.persist_runtime_state(&provider_id).await;
        self.fallback_or_error(error).await
    }

    async fn fallback_or_error(
        &self,
        error: NewsProviderError,
    ) -> Result<(NewsPage, bool), NewsProviderError> {
        // A manual disable is an explicit user decision, not an outage. Do not
        // quietly re-introduce that source through a stale snapshot.
        if error.kind == NewsErrorKind::Disabled {
            return Err(error);
        }
        if let Some(page) = self.last_good.get(&error.provider_id) {
            return Ok((page.clone(), true));
        }
        if let Some(storage) = &self.storage {
            if let Ok(Some(raw)) = storage
                .settings_get(&snapshot_key(&error.provider_id))
                .await
            {
                if let Ok(page) = serde_json::from_str::<NewsPage>(&raw) {
                    self.last_good
                        .insert(error.provider_id.clone(), page.clone());
                    return Ok((page, true));
                }
            }
        }
        Err(error)
    }

    async fn sync_disabled(&self, provider_id: &str) {
        let Some(storage) = &self.storage else {
            return;
        };
        if let Ok(Some(value)) = storage.settings_get(&disabled_key(provider_id)).await {
            if let Some(runtime) = self.runtime.get(provider_id) {
                runtime.lock().enabled = value != "true";
            }
        }
    }

    async fn load_cursor(&self, provider_id: &str) -> Option<String> {
        let storage = self.storage.as_ref()?;
        let cursor = storage
            .settings_get(&cursor_key(provider_id))
            .await
            .ok()
            .flatten();
        if cursor.is_some() {
            if let Some(runtime) = self.runtime.get(provider_id) {
                runtime.lock().cursor_present = true;
            }
        }
        cursor
    }

    async fn persist_success(
        &self,
        capabilities: &NewsCapabilities,
        page: &mut NewsPage,
        elapsed: Duration,
    ) {
        let Some(storage) = &self.storage else {
            return;
        };
        let observed_at = now_secs();
        for item in &mut page.items {
            let canonical_url = if item.url.trim().is_empty() {
                format!("urn:astock-news:{}:{}", item.provider_id, item.id)
            } else {
                item.url.clone()
            };
            let raw_snapshot = (item.trust_tier == NewsTrustTier::FirstPartyDisclosure)
                .then(|| {
                    item.raw_payload
                        .as_ref()
                        .and_then(|raw| serde_json::to_vec(raw).ok())
                })
                .flatten();
            let archived = storage
                .news_archive_upsert(NewsArchiveInput {
                    canonical_url,
                    source_id: item.provider_id.clone(),
                    source_name: item.source_name.clone(),
                    license: item.license.clone(),
                    content_type: if item.trust_tier == NewsTrustTier::FirstPartyDisclosure {
                        "announcement"
                    } else {
                        "news"
                    }
                    .into(),
                    language: "zh-CN".into(),
                    parser_version: item.parser_version.clone(),
                    title: item.title.clone(),
                    factual_summary: item.summary.clone(),
                    raw_snapshot,
                    raw_snapshot_permitted: item.trust_tier == NewsTrustTier::FirstPartyDisclosure,
                    event_time: EvidenceTimestamp::default(),
                    publish_time: EvidenceTimestamp {
                        utc: item.published_at_ms.map(|value| value / 1_000),
                        original: (!item.published_at.is_empty())
                            .then(|| item.published_at.clone()),
                    },
                    first_seen_time_utc: observed_at,
                    revision_time: EvidenceTimestamp {
                        utc: Some(observed_at),
                        original: None,
                    },
                    retention_class: "research_evidence".into(),
                    observation: NewsObservationInput {
                        document_id: None,
                        revision_id: None,
                        provider_id: capabilities.provider_id.clone(),
                        endpoint: capabilities.endpoint.clone(),
                        fetched_at: observed_at,
                        http_status: page.http_status.or(Some(200)),
                        etag: page.etag.clone(),
                        last_modified: page.last_modified.clone(),
                        latency_ms: Some(elapsed.as_millis().min(u128::from(u64::MAX)) as u64),
                        parse_status: "ok".into(),
                        parse_error: None,
                        raw_evidence: None,
                    },
                })
                .await;
            match archived {
                Ok(saved) => item.document_revision_id = Some(saved.revision_id),
                Err(error) => tracing::warn!(
                    provider = capabilities.provider_id,
                    item = item.id,
                    %error,
                    "news archive write failed; live result remains available"
                ),
            }
        }
        let provider_id = &capabilities.provider_id;
        if let Some(cursor) = &page.next_cursor {
            let _ = storage.settings_set(&cursor_key(provider_id), cursor).await;
            if let Some(runtime) = self.runtime.get(provider_id) {
                runtime.lock().cursor_present = true;
            }
        }
        if let Ok(raw) = serde_json::to_string(page) {
            let _ = storage.settings_set(&snapshot_key(provider_id), &raw).await;
        }
    }

    async fn persist_failure_observation(
        &self,
        capabilities: &NewsCapabilities,
        error: &NewsProviderError,
        elapsed: Duration,
    ) {
        let Some(storage) = &self.storage else {
            return;
        };
        let result = storage
            .news_observation_record(NewsObservationInput {
                document_id: None,
                revision_id: None,
                provider_id: capabilities.provider_id.clone(),
                endpoint: capabilities.endpoint.clone(),
                fetched_at: now_secs(),
                http_status: None,
                etag: None,
                last_modified: None,
                latency_ms: Some(elapsed.as_millis().min(u128::from(u64::MAX)) as u64),
                parse_status: if error.kind == NewsErrorKind::Parse {
                    "parse_error"
                } else {
                    "request_error"
                }
                .into(),
                parse_error: Some(redact_text(&error.message).chars().take(2_000).collect()),
                raw_evidence: error.raw_evidence.clone(),
            })
            .await;
        if let Err(storage_error) = result {
            tracing::warn!(
                provider = capabilities.provider_id,
                %storage_error,
                "failed news observation could not be archived"
            );
        }
    }

    async fn sync_persisted_runtime(&self, provider_id: &str) {
        let Some(storage) = &self.storage else {
            return;
        };
        let should_load = self.runtime.get(provider_id).is_some_and(|runtime| {
            let state = runtime.lock();
            state.attempts == 0 && state.last_success_at.is_none()
        });
        if !should_load {
            return;
        }
        let Ok(Some(saved)) = storage.news_provider_state_get(provider_id).await else {
            return;
        };
        if let Some(runtime) = self.runtime.get(provider_id) {
            let mut state = runtime.lock();
            if state.attempts == 0 && state.last_success_at.is_none() {
                state.last_success_at = saved.last_success_at;
                state.last_latency_ms = saved.last_latency_ms;
                state.attempts = saved.attempts;
                state.failures = saved.failures;
                state.last_error_kind = saved.last_error_kind.as_deref().and_then(parse_error_kind);
            }
        }
    }

    async fn persist_runtime_state(&self, provider_id: &str) {
        let Some(storage) = &self.storage else {
            return;
        };
        let Some(runtime) = self.runtime.get(provider_id) else {
            return;
        };
        let saved = {
            let state = runtime.lock();
            NewsProviderArchiveState {
                provider_id: provider_id.to_string(),
                last_success_at: state.last_success_at,
                last_observation_at: Some(now_secs()),
                last_latency_ms: state.last_latency_ms,
                attempts: state.attempts,
                failures: state.failures,
                last_error_kind: state.last_error_kind.map(error_kind_token),
                updated_at: now_secs(),
            }
        };
        if let Err(error) = storage.news_provider_state_put(saved).await {
            tracing::warn!(provider = provider_id, %error, "news provider state persistence failed");
        }
    }

    fn mark_attempt(&self, provider_id: &str) {
        if let Some(runtime) = self.runtime.get(provider_id) {
            runtime.lock().attempts += 1;
        }
    }

    fn mark_success(&self, provider_id: &str, elapsed: Duration) {
        if let Some(runtime) = self.runtime.get(provider_id) {
            let mut state = runtime.lock();
            state.consecutive_failures = 0;
            state.cooldown = BASE_COOLDOWN;
            state.open_until = None;
            state.last_success_at = Some(now_secs());
            state.last_latency_ms = Some(elapsed.as_millis().min(u128::from(u64::MAX)) as u64);
            state.last_error_kind = None;
        }
    }

    fn mark_failed_attempt(&self, provider_id: &str, kind: NewsErrorKind) {
        if let Some(runtime) = self.runtime.get(provider_id) {
            let mut state = runtime.lock();
            state.failures += 1;
            state.last_error_kind = Some(kind);
        }
    }

    fn mark_failure(&self, provider_id: &str, kind: NewsErrorKind) {
        if let Some(runtime) = self.runtime.get(provider_id) {
            let mut state = runtime.lock();
            let failed_half_open_probe = state
                .open_until
                .is_some_and(|until| Instant::now() >= until);
            state.consecutive_failures += 1;
            state.last_error_kind = Some(kind);
            if failed_half_open_probe || state.consecutive_failures >= FAILURE_THRESHOLD {
                state.open_until = Some(Instant::now() + state.cooldown);
                state.cooldown = (state.cooldown * 2).min(MAX_COOLDOWN);
                state.consecutive_failures = 0;
            }
        }
    }

    async fn wait_rate_limit(&self, capabilities: &NewsCapabilities) {
        let delay = self
            .runtime
            .get(&capabilities.provider_id)
            .and_then(|runtime| {
                let mut state = runtime.lock();
                let now = Instant::now();
                let delay = state
                    .next_allowed
                    .and_then(|next| (next > now).then_some(next - now));
                let gap =
                    Duration::from_secs_f64(60.0 / f64::from(capabilities.rate_limit_per_minute));
                state.next_allowed = Some(now + gap);
                delay
            });
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
    }

    pub async fn set_enabled(
        &self,
        provider_id: &str,
        enabled: bool,
    ) -> Result<(), NewsProviderError> {
        let runtime = self
            .runtime
            .get(provider_id)
            .ok_or_else(|| NewsProviderError::configuration(provider_id, "未知资讯来源"))?;
        runtime.lock().enabled = enabled;
        drop(runtime);
        if let Some(storage) = &self.storage {
            storage
                .settings_set(
                    &disabled_key(provider_id),
                    if enabled { "false" } else { "true" },
                )
                .await
                .map_err(|_| {
                    NewsProviderError::new(
                        provider_id,
                        NewsErrorKind::Storage,
                        "无法保存来源启用状态",
                        false,
                    )
                })?;
        }
        Ok(())
    }

    pub async fn accept_push(
        &self,
        provider_id: &str,
        page: NewsPage,
    ) -> Result<(), NewsProviderError> {
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.capabilities().provider_id == provider_id)
            .ok_or_else(|| NewsProviderError::configuration(provider_id, "未知资讯来源"))?;
        if !provider
            .capabilities()
            .modes
            .contains(&NewsDeliveryMode::PushStream)
        {
            return Err(NewsProviderError::configuration(
                provider_id,
                "该来源未声明推送/流式能力",
            ));
        }
        let capabilities = provider.capabilities().clone();
        self.mark_success(provider_id, Duration::ZERO);
        let mut page = page;
        self.persist_success(&capabilities, &mut page, Duration::ZERO)
            .await;
        self.persist_runtime_state(provider_id).await;
        self.last_good.insert(provider_id.to_string(), page);
        Ok(())
    }

    pub async fn health(&self) -> Vec<NewsProviderHealth> {
        let now = now_secs();
        let mut rows = Vec::new();
        for provider in &self.providers {
            let capabilities = provider.capabilities();
            self.sync_disabled(&capabilities.provider_id).await;
            self.sync_persisted_runtime(&capabilities.provider_id).await;
            let archive = match &self.storage {
                Some(storage) => storage
                    .news_archive_source_stats(&capabilities.provider_id)
                    .await
                    .unwrap_or_default(),
                None => Default::default(),
            };
            let runtime = self
                .runtime
                .get(&capabilities.provider_id)
                .expect("registered provider");
            let state = runtime.lock();
            let remaining = state.open_until.and_then(|until| {
                (until > Instant::now()).then_some((until - Instant::now()).as_secs())
            });
            let stale = state.last_success_at.is_none_or(|last| {
                now.saturating_sub(last) > (capabilities.min_refresh_secs * 3) as i64
            });
            rows.push(NewsProviderHealth {
                provider_id: capabilities.provider_id.clone(),
                display_name: capabilities.display_name.clone(),
                enabled: state.enabled,
                circuit_state: if remaining.is_some() {
                    "open"
                } else if state.open_until.is_some() {
                    "half_open"
                } else {
                    "closed"
                }
                .to_string(),
                trust_tier: capabilities.trust_tier,
                trust_tier_name: capabilities.trust_tier.chinese_name().to_string(),
                modes: capabilities.modes.clone(),
                license: capabilities.license.clone(),
                endpoint: public_endpoint(&capabilities.endpoint),
                min_refresh_secs: capabilities.min_refresh_secs,
                rate_limit_per_minute: capabilities.rate_limit_per_minute,
                last_success_at: state.last_success_at,
                last_latency_ms: state.last_latency_ms,
                attempts: state.attempts,
                failures: state.failures,
                failure_rate: if state.attempts == 0 {
                    0.0
                } else {
                    state.failures as f64 / state.attempts as f64
                },
                stale,
                cursor_present: state.cursor_present,
                cooldown_remaining_secs: remaining,
                last_error_kind: state.last_error_kind,
                archived_documents: archive.documents,
                archived_revisions: archive.revisions,
                archive_last_observed_at: archive.last_observed_at,
                stale_age_secs: state
                    .last_success_at
                    .map(|last| now.saturating_sub(last).max(0) as u64),
            });
        }
        rows.sort_by(|left, right| left.provider_id.cmp(&right.provider_id));
        rows
    }
}

fn cursor_key(provider_id: &str) -> String {
    format!("news.cursor.{provider_id}")
}

fn snapshot_key(provider_id: &str) -> String {
    format!("news.snapshot.{provider_id}")
}

fn disabled_key(provider_id: &str) -> String {
    format!("news.disabled.{provider_id}")
}

fn public_endpoint(endpoint: &str) -> String {
    endpoint
        .split(['?', '#'])
        .next()
        .unwrap_or(endpoint)
        .to_string()
}

fn error_kind_token(kind: NewsErrorKind) -> String {
    serde_json::to_string(&kind)
        .unwrap_or_else(|_| "\"storage\"".into())
        .trim_matches('"')
        .to_string()
}

fn parse_error_kind(value: &str) -> Option<NewsErrorKind> {
    serde_json::from_str(&format!("\"{value}\"")).ok()
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonNewsProviderConfig {
    pub provider_id: String,
    pub display_name: String,
    pub endpoint: String,
    pub min_refresh_secs: u64,
    pub rate_limit_per_minute: u32,
    pub license: String,
    pub trust_tier: NewsTrustTier,
    pub modes: BTreeSet<NewsDeliveryMode>,
    #[serde(default = "default_parser_version")]
    pub parser_version: String,
}

fn default_parser_version() -> String {
    "generic-json-v1".to_string()
}

/// Configurable independent JSON feed. The endpoint must return either an
/// array or `{ "items": [...] }`; common title/url/date/id fields are read.
pub struct ConfiguredJsonNewsProvider {
    capabilities: NewsCapabilities,
    http: Arc<HttpClient>,
    cache: Arc<TtlCache>,
}

impl ConfiguredJsonNewsProvider {
    pub fn new(
        config: JsonNewsProviderConfig,
        http: Arc<HttpClient>,
        cache: Arc<TtlCache>,
    ) -> Result<Self, NewsProviderError> {
        let capabilities = NewsCapabilities {
            provider_id: config.provider_id,
            display_name: config.display_name,
            endpoint: config.endpoint,
            modes: config.modes,
            min_refresh_secs: config.min_refresh_secs,
            rate_limit_per_minute: config.rate_limit_per_minute,
            license: config.license,
            trust_tier: config.trust_tier,
            parser_version: config.parser_version,
            supports_symbol_filter: true,
        };
        capabilities.validate()?;
        Ok(Self {
            capabilities,
            http,
            cache,
        })
    }
}

#[async_trait]
impl NewsProvider for ConfiguredJsonNewsProvider {
    fn capabilities(&self) -> &NewsCapabilities {
        &self.capabilities
    }

    async fn fetch(&self, request: NewsIngestRequest) -> Result<NewsPage, NewsProviderError> {
        let key = format!(
            "news_json_{}_{}_{}",
            self.capabilities.provider_id,
            request.symbol.as_deref().unwrap_or("*"),
            request.cursor.as_deref().unwrap_or("*")
        );
        if let Some(page) = self.cache.get::<NewsPage>(
            &key,
            Duration::from_secs(self.capabilities.min_refresh_secs),
        ) {
            return Ok(page);
        }
        let mut params = Vec::new();
        if let Some(symbol) = &request.symbol {
            params.push(("symbol".to_string(), symbol.clone()));
        }
        if let Some(cursor) = &request.cursor {
            params.push(("cursor".to_string(), cursor.clone()));
        }
        if let Some(after) = request.published_after_ms {
            params.push(("published_after".to_string(), after.to_string()));
        }
        params.push(("limit".to_string(), request.limit.clamp(1, 200).to_string()));
        let response = self
            .http
            .get_text(&self.capabilities.endpoint, &params)
            .await
            .map_err(|error| classify_data_error(&self.capabilities.provider_id, error))?;
        if response.body.len() > 2 * 1024 * 1024 {
            return Err(NewsProviderError::new(
                &self.capabilities.provider_id,
                NewsErrorKind::Parse,
                "响应超过 2 MiB",
                false,
            ));
        }
        let value: Value = serde_json::from_str(&response.body).map_err(|_| {
            NewsProviderError::new(
                &self.capabilities.provider_id,
                NewsErrorKind::Parse,
                "响应不是有效 JSON",
                false,
            )
            .with_raw_evidence(response.body.as_bytes())
        })?;
        let mut page = parse_generic_page(&self.capabilities, &request, &value)
            .map_err(|error| error.with_raw_evidence(response.body.as_bytes()))?;
        page.http_status = Some(response.status);
        page.etag = response.etag;
        page.last_modified = response.last_modified;
        self.cache.set(&key, &page);
        Ok(page)
    }
}

fn parse_generic_page(
    capabilities: &NewsCapabilities,
    request: &NewsIngestRequest,
    value: &Value,
) -> Result<NewsPage, NewsProviderError> {
    let rows = value
        .as_array()
        .or_else(|| value.get("items").and_then(Value::as_array))
        .ok_or_else(|| {
            NewsProviderError::new(
                &capabilities.provider_id,
                NewsErrorKind::Parse,
                "缺少 items 数组",
                false,
            )
        })?;
    let mut items = rows
        .iter()
        .take(request.limit.clamp(1, 200))
        .enumerate()
        .filter_map(|(index, raw)| normalize_generic_item(capabilities, raw, index + 1))
        .collect::<Vec<_>>();
    if let Some(keyword) = request.keyword.as_deref().filter(|value| !value.is_empty()) {
        let keyword = keyword.to_lowercase();
        items.retain(|item| {
            item.title.to_lowercase().contains(&keyword)
                || item.summary.to_lowercase().contains(&keyword)
        });
    }
    let next_cursor = value
        .get("next_cursor")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| items.last().map(|item| item.id.clone()));
    Ok(NewsPage {
        items,
        next_cursor,
        ..Default::default()
    })
}

fn normalize_generic_item(
    capabilities: &NewsCapabilities,
    raw: &Value,
    rank: usize,
) -> Option<FinanceNewsItem> {
    let title = text_field(raw, &["title", "headline", "name"], 1_000);
    if title.is_empty() {
        return None;
    }
    let external_id = text_field(raw, &["id", "uuid", "guid"], 200);
    let summary = text_field(raw, &["summary", "description", "content"], 3_000);
    let url = raw
        .get("url")
        .or_else(|| raw.get("link"))
        .and_then(Value::as_str)
        .and_then(|value| UrlSecurityPolicy::default().validate_static(value).ok())
        .map(|value| value.as_str().chars().take(2_048).collect())
        .unwrap_or_default();
    let published_at_ms = raw
        .get("published_at_ms")
        .or_else(|| raw.get("timestamp"))
        .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
        .map(|value| {
            if value < 10_000_000_000 {
                value * 1_000
            } else {
                value
            }
        });
    let id = if external_id.is_empty() {
        format!("{}-{rank}", published_at_ms.unwrap_or_default())
    } else {
        external_id
    };
    Some(FinanceNewsItem {
        id: format!("{}:{id}", capabilities.provider_id),
        source_id: capabilities.provider_id.clone(),
        source_name: capabilities.display_name.clone(),
        title,
        summary,
        url,
        published_at: published_at_ms
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|date| date.to_rfc3339())
            .unwrap_or_default(),
        published_at_ms,
        important: raw
            .get("important")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        rank,
        provider_id: capabilities.provider_id.clone(),
        trust_tier: capabilities.trust_tier,
        trust_tier_name: capabilities.trust_tier.chinese_name().to_string(),
        license: capabilities.license.clone(),
        parser_version: capabilities.parser_version.clone(),
        document_revision_id: None,
        raw_payload: bounded_raw(raw),
    })
}

fn text_field(raw: &Value, keys: &[&str], max: usize) -> String {
    keys.iter()
        .find_map(|key| raw.get(*key).and_then(Value::as_str))
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

pub(crate) fn bounded_raw(raw: &Value) -> Option<Value> {
    let encoded = serde_json::to_vec(raw).ok()?;
    (encoded.len() <= MAX_RAW_PAYLOAD_BYTES).then(|| raw.clone())
}

pub(crate) fn classify_data_error(
    provider_id: &str,
    error: astock_core::DataError,
) -> NewsProviderError {
    use astock_core::DataError;
    let (kind, retryable) = match error {
        DataError::RateLimited(_) => (NewsErrorKind::RateLimited, true),
        DataError::Timeout(_) => (NewsErrorKind::Timeout, true),
        DataError::Network { .. } | DataError::WafBlocked(_) => (NewsErrorKind::Network, true),
        DataError::Parse { .. } => (NewsErrorKind::Parse, false),
        DataError::Empty(_) | DataError::AllFailed { .. } => (NewsErrorKind::Empty, false),
        DataError::NoProvider(_) => (NewsErrorKind::Configuration, false),
        _ => (NewsErrorKind::Network, false),
    };
    NewsProviderError::new(provider_id, kind, error.to_string(), retryable)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use astock_storage::StorageConfig;

    struct FakeProvider {
        capabilities: NewsCapabilities,
        failures_left: AtomicUsize,
        title: String,
    }

    impl FakeProvider {
        fn new(id: &str, failures: usize, tier: NewsTrustTier) -> Self {
            Self {
                capabilities: NewsCapabilities {
                    provider_id: id.into(),
                    display_name: id.into(),
                    endpoint: format!("https://{id}.example.com/news"),
                    modes: [NewsDeliveryMode::PublishedIncremental].into(),
                    min_refresh_secs: 15,
                    rate_limit_per_minute: 600,
                    license: "fixture-license".into(),
                    trust_tier: tier,
                    parser_version: "fixture-v1".into(),
                    supports_symbol_filter: true,
                },
                failures_left: AtomicUsize::new(failures),
                title: format!("{id} result"),
            }
        }
    }

    #[async_trait]
    impl NewsProvider for FakeProvider {
        fn capabilities(&self) -> &NewsCapabilities {
            &self.capabilities
        }

        async fn fetch(&self, _request: NewsIngestRequest) -> Result<NewsPage, NewsProviderError> {
            if self
                .failures_left
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                    value.checked_sub(1)
                })
                .is_ok()
            {
                return Err(NewsProviderError::new(
                    &self.capabilities.provider_id,
                    NewsErrorKind::Network,
                    "injected outage",
                    true,
                ));
            }
            Ok(NewsPage {
                items: vec![FinanceNewsItem::fixture(&self.capabilities, &self.title)],
                next_cursor: Some("cursor-2".into()),
                http_status: Some(200),
                etag: Some("fixture-etag".into()),
                last_modified: None,
            })
        }
    }

    #[tokio::test]
    async fn aggregator_outage_does_not_block_official_and_independent_sources() {
        let ingestor = NewsIngestor::new(
            vec![
                Arc::new(FakeProvider::new(
                    "newsnow",
                    99,
                    NewsTrustTier::PublicAggregator,
                )),
                Arc::new(FakeProvider::new(
                    "official",
                    0,
                    NewsTrustTier::FirstPartyDisclosure,
                )),
                Arc::new(FakeProvider::new(
                    "licensed",
                    0,
                    NewsTrustTier::LicensedMedia,
                )),
            ],
            None,
        )
        .unwrap();
        let outcome = ingestor
            .ingest(
                NewsIngestRequest {
                    limit: 20,
                    ..Default::default()
                },
                None,
            )
            .await;
        assert_eq!(outcome.items.len(), 2);
        assert!(outcome
            .items
            .iter()
            .any(|item| item.provider_id == "official"));
        assert!(outcome
            .items
            .iter()
            .any(|item| item.provider_id == "licensed"));
        assert!(outcome
            .errors
            .iter()
            .any(|error| error.provider_id == "newsnow"));
    }

    #[tokio::test]
    async fn transient_failure_retries_and_health_exposes_metrics() {
        let ingestor = NewsIngestor::new(
            vec![Arc::new(FakeProvider::new(
                "retry",
                1,
                NewsTrustTier::LicensedMedia,
            ))],
            None,
        )
        .unwrap();
        let outcome = ingestor
            .ingest(
                NewsIngestRequest {
                    limit: 10,
                    ..Default::default()
                },
                None,
            )
            .await;
        assert_eq!(outcome.items.len(), 1);
        let health = ingestor.health().await;
        assert_eq!(health[0].attempts, 2);
        assert_eq!(health[0].failures, 1);
        assert!((health[0].failure_rate - 0.5).abs() < f64::EPSILON);
        assert!(health[0].last_latency_ms.is_some());
    }

    #[tokio::test]
    async fn cursor_and_last_good_snapshot_survive_runtime_restart() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(temp.path())).unwrap();
        let provider = Arc::new(FakeProvider::new(
            "persist",
            0,
            NewsTrustTier::FirstPartyDisclosure,
        ));
        let ingestor = NewsIngestor::new(vec![provider], Some(storage.clone())).unwrap();
        let first = ingestor
            .ingest(
                NewsIngestRequest {
                    limit: 10,
                    ..Default::default()
                },
                None,
            )
            .await;
        assert_eq!(first.items.len(), 1);
        assert_eq!(
            storage.settings_get("news.cursor.persist").await.unwrap(),
            Some("cursor-2".into())
        );

        let restarted = NewsIngestor::new(
            vec![Arc::new(FakeProvider::new(
                "persist",
                99,
                NewsTrustTier::FirstPartyDisclosure,
            ))],
            Some(storage),
        )
        .unwrap();
        let fallback = restarted
            .ingest(
                NewsIngestRequest {
                    limit: 10,
                    ..Default::default()
                },
                None,
            )
            .await;
        assert_eq!(fallback.items.len(), 1);
        assert_eq!(fallback.stale_providers, vec!["persist"]);
        assert!(restarted.health().await[0].cursor_present);
    }

    #[tokio::test]
    async fn manually_disabled_provider_is_never_restored_from_stale_snapshot() {
        let ingestor = NewsIngestor::new(
            vec![Arc::new(FakeProvider::new(
                "manual",
                0,
                NewsTrustTier::LicensedMedia,
            ))],
            None,
        )
        .unwrap();
        let first = ingestor
            .ingest(
                NewsIngestRequest {
                    limit: 10,
                    ..Default::default()
                },
                None,
            )
            .await;
        assert_eq!(first.items.len(), 1);
        ingestor.set_enabled("manual", false).await.unwrap();
        let disabled = ingestor
            .ingest(
                NewsIngestRequest {
                    limit: 10,
                    ..Default::default()
                },
                None,
            )
            .await;
        assert!(disabled.items.is_empty());
        assert_eq!(disabled.errors[0].kind, NewsErrorKind::Disabled);
    }

    #[test]
    fn generic_provider_contract_parses_offline_fixture_with_provenance() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/news/generic_feed.json"))
                .unwrap();
        let capabilities =
            FakeProvider::new("fixture", 0, NewsTrustTier::LicensedMedia).capabilities;
        let page = parse_generic_page(
            &capabilities,
            &NewsIngestRequest {
                keyword: Some("回购".into()),
                limit: 20,
                ..Default::default()
            },
            &fixture,
        )
        .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.next_cursor.as_deref(), Some("fixture-cursor-2"));
        assert_eq!(page.items[0].license, "fixture-license");
        assert_eq!(page.items[0].parser_version, "fixture-v1");
        assert!(page.items[0].raw_payload.is_some());
    }

    #[test]
    fn provider_contract_rejects_unsafe_endpoint_and_incomplete_license() {
        let mut capabilities = FakeProvider::new("bad", 0, NewsTrustTier::SearchLead).capabilities;
        capabilities.endpoint = "http://127.0.0.1/internal".into();
        assert!(capabilities.validate().is_err());
        capabilities.endpoint = "https://example.com/feed".into();
        capabilities.license.clear();
        assert!(capabilities.validate().is_err());
    }
}
