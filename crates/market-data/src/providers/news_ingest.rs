//! Pluggable, provider-neutral news ingestion runtime.
//!
//! Providers declare capabilities and return one normalized model. The
//! runtime owns independent rate limits, retry/backoff, circuit state,
//! persistent cursors, last-good fallback, manual disable and health metrics.
//! It never treats a public aggregator as authoritative evidence.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use astock_entity_linking::{EntityLinker, LinkStatus};
use astock_news_intelligence::{canonicalize_url, NewsEventClusterer};
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
const PROVIDER_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(30);
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

/// Split a natural-language research query into bounded OR terms. Requiring
/// the entire string `"短线 题材 龙头 板块轮动"` to occur verbatim caused a
/// healthy provider response to be misreported as a provider outage.
fn search_terms(query: &str) -> Vec<String> {
    let normalized = query.trim().to_lowercase();
    if normalized.is_empty() {
        return Vec::new();
    }
    let terms = normalized
        .split(|character: char| {
            character.is_whitespace()
                || matches!(
                    character,
                    ',' | '，' | ';' | '；' | '/' | '、' | '|' | '（' | '）' | '(' | ')'
                )
        })
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .take(12)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if terms.is_empty() {
        vec![normalized]
    } else {
        terms
    }
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
    /// Non-fatal provider/channel failures retained when a provider still
    /// returned usable rows. This keeps partial success observable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<NewsProviderError>,
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

/// Detailed, non-sensitive progress for one multi-provider news request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NewsIngestWorkItem {
    pub provider_id: String,
    pub display_name: String,
    pub status: String,
    pub attempt: u32,
    pub max_attempts: u32,
    pub elapsed_ms: u64,
    pub records_processed: usize,
    pub records_total: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NewsIngestProgress {
    pub completed: usize,
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub records: usize,
    pub current_provider: String,
    pub current_status: String,
    pub active: Vec<NewsIngestWorkItem>,
    pub latest_activity_at_ms: i64,
    pub recent_errors: Vec<String>,
}

pub type NewsIngestProgressReporter = Arc<dyn Fn(NewsIngestProgress) + Send + Sync>;

#[derive(Debug)]
struct ActiveNewsWork {
    display_name: String,
    status: String,
    attempt: u32,
    records_processed: usize,
    records_total: usize,
    started: Instant,
}

struct NewsProgressTracker {
    reporter: NewsIngestProgressReporter,
    state: Mutex<NewsProgressState>,
}

struct NewsProgressState {
    total: usize,
    completed: usize,
    succeeded: usize,
    failed: usize,
    records: usize,
    active: BTreeMap<String, ActiveNewsWork>,
    recent_errors: Vec<String>,
}

impl NewsProgressTracker {
    fn new(providers: &[Arc<dyn NewsProvider>], reporter: NewsIngestProgressReporter) -> Arc<Self> {
        let active = providers
            .iter()
            .map(|provider| {
                let capabilities = provider.capabilities();
                (
                    capabilities.provider_id.clone(),
                    ActiveNewsWork {
                        display_name: capabilities.display_name.clone(),
                        status: "等待调度".into(),
                        attempt: 0,
                        records_processed: 0,
                        records_total: 0,
                        started: Instant::now(),
                    },
                )
            })
            .collect();
        let tracker = Arc::new(Self {
            reporter,
            state: Mutex::new(NewsProgressState {
                total: providers.len(),
                completed: 0,
                succeeded: 0,
                failed: 0,
                records: 0,
                active,
                recent_errors: Vec::new(),
            }),
        });
        tracker.emit(
            "资讯采集调度器",
            format!("已创建 {} 个并行来源任务", providers.len()),
        );
        tracker
    }

    fn update(
        &self,
        provider_id: &str,
        status: impl Into<String>,
        attempt: u32,
        records_processed: usize,
        records_total: usize,
    ) {
        let status = status.into();
        {
            let mut state = self.state.lock();
            if let Some(work) = state.active.get_mut(provider_id) {
                work.status.clone_from(&status);
                work.attempt = attempt;
                work.records_processed = records_processed;
                work.records_total = records_total;
            }
        }
        self.emit(provider_id, status);
    }

    fn complete(
        &self,
        provider_id: &str,
        succeeded: bool,
        stale: bool,
        records: usize,
        errors: impl IntoIterator<Item = String>,
    ) {
        let status = if succeeded {
            if stale {
                "来源失败，已使用最后成功快照"
            } else {
                "来源采集、归档和关联处理完成"
            }
        } else {
            "来源失败，已释放并发槽并继续其他来源"
        };
        {
            let mut state = self.state.lock();
            state.active.remove(provider_id);
            state.completed += 1;
            if succeeded {
                state.succeeded += 1;
                state.records += records;
            } else {
                state.failed += 1;
            }
            for error in errors {
                if !error.trim().is_empty() {
                    state.recent_errors.push(error);
                }
            }
            if state.recent_errors.len() > 20 {
                let remove = state.recent_errors.len() - 20;
                state.recent_errors.drain(0..remove);
            }
        }
        self.emit(provider_id, status.into());
    }

    fn emit(&self, current_provider: impl Into<String>, current_status: String) {
        let current_provider = current_provider.into();
        let snapshot = {
            let state = self.state.lock();
            let active_records = state
                .active
                .values()
                .map(|work| work.records_processed)
                .sum::<usize>();
            NewsIngestProgress {
                completed: state.completed,
                total: state.total,
                succeeded: state.succeeded,
                failed: state.failed,
                records: state.records + active_records,
                current_provider,
                current_status,
                active: state
                    .active
                    .iter()
                    .map(|(provider_id, work)| NewsIngestWorkItem {
                        provider_id: provider_id.clone(),
                        display_name: work.display_name.clone(),
                        status: work.status.clone(),
                        attempt: work.attempt,
                        max_attempts: MAX_RETRIES + 1,
                        elapsed_ms: work.started.elapsed().as_millis().min(u128::from(u64::MAX))
                            as u64,
                        records_processed: work.records_processed,
                        records_total: work.records_total,
                    })
                    .collect(),
                latest_activity_at_ms: now_secs().saturating_mul(1_000),
                recent_errors: state.recent_errors.iter().rev().take(5).cloned().collect(),
            }
        };
        (self.reporter)(snapshot);
    }
}

pub struct NewsIngestor {
    providers: Vec<Arc<dyn NewsProvider>>,
    runtime: DashMap<String, Mutex<ProviderRuntime>>,
    permits: Semaphore,
    last_good: DashMap<String, NewsPage>,
    storage: Option<Storage>,
    entity_linker: Option<Arc<EntityLinker>>,
    provider_attempt_timeout: Duration,
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
        let entity_linker = storage
            .as_ref()
            .map(|storage| Arc::new(EntityLinker::new(storage.clone())));
        Ok(Self {
            providers,
            runtime,
            permits: Semaphore::new(MAX_CONCURRENT_PROVIDERS),
            last_good: DashMap::new(),
            storage,
            entity_linker,
            provider_attempt_timeout: PROVIDER_ATTEMPT_TIMEOUT,
        })
    }

    #[cfg(test)]
    fn with_attempt_timeout_for_tests(mut self, timeout: Duration) -> Self {
        self.provider_attempt_timeout = timeout;
        self
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
        self.ingest_with_progress(request, selected, None).await
    }

    pub async fn ingest_with_progress(
        &self,
        request: NewsIngestRequest,
        selected: Option<&[String]>,
        progress: Option<NewsIngestProgressReporter>,
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
        let total = providers.len();
        if total == 0 {
            return NewsIngestOutcome {
                errors: vec![NewsProviderError::configuration(
                    "provider-registry",
                    "没有可用于本次查询的已启用资讯来源；请检查来源配置和筛选条件",
                )],
                ..Default::default()
            };
        }
        let tracker = progress.map(|reporter| NewsProgressTracker::new(&providers, reporter));
        let mut pending = FuturesUnordered::new();
        for provider in providers {
            pending.push(self.fetch_named(provider, request.clone(), tracker.clone()));
        }
        let mut outcome = NewsIngestOutcome::default();
        while let Some((provider_id, result)) = pending.next().await {
            match result {
                Ok((page, stale)) => {
                    let records = page.items.len();
                    let diagnostics = page
                        .diagnostics
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>();
                    if stale {
                        outcome.stale_providers.push(provider_id.clone());
                    } else {
                        outcome.successful_providers.push(provider_id.clone());
                    }
                    outcome.errors.extend(page.diagnostics.iter().cloned());
                    outcome.items.extend(page.items);
                    if let Some(tracker) = &tracker {
                        tracker.complete(&provider_id, true, stale, records, diagnostics);
                    }
                }
                Err(error) => {
                    let message = error.to_string();
                    outcome.errors.push(error);
                    if let Some(tracker) = &tracker {
                        tracker.complete(&provider_id, false, false, 0, [message]);
                    }
                }
            }
        }
        if let Some(tracker) = &tracker {
            tracker.emit(
                "资讯合并与条件匹配",
                format!(
                    "正在对 {} 条上游记录去重、排序并匹配检索条件",
                    outcome.items.len()
                ),
            );
        }
        let mut seen = HashSet::new();
        let cluster_counts = outcome
            .items
            .iter()
            .filter_map(|item| {
                item.event_cluster_id
                    .as_ref()
                    .map(|cluster| (cluster.clone(), item.independent_source_count))
            })
            .fold(
                std::collections::HashMap::<String, usize>::new(),
                |mut counts, (cluster, count)| {
                    counts
                        .entry(cluster)
                        .and_modify(|current| *current = (*current).max(count))
                        .or_insert(count);
                    counts
                },
            );
        for item in &mut outcome.items {
            if let Some(cluster) = &item.event_cluster_id {
                item.independent_source_count = cluster_counts.get(cluster).copied().unwrap_or(1);
            }
        }
        outcome.items.sort_by(|left, right| {
            trust_rank(left.trust_tier)
                .cmp(&trust_rank(right.trust_tier))
                .then_with(|| {
                    right
                        .published_at_ms
                        .unwrap_or_default()
                        .cmp(&left.published_at_ms.unwrap_or_default())
                })
        });
        outcome.items.retain(|item| {
            seen.insert(
                item.event_cluster_id
                    .clone()
                    .unwrap_or_else(|| format!("document:{}", item.id)),
            )
        });
        outcome.items.sort_by(|left, right| {
            right
                .published_at_ms
                .unwrap_or_default()
                .cmp(&left.published_at_ms.unwrap_or_default())
                .then_with(|| left.rank.cmp(&right.rank))
        });
        let mut entity_queries = BTreeSet::new();
        if let Some(linker) = &self.entity_linker {
            for query in [request.symbol.as_deref(), request.keyword.as_deref()]
                .into_iter()
                .flatten()
                .map(str::trim)
                .filter(|query| !query.is_empty())
            {
                if let Ok(ids) = linker.resolve_query(query).await {
                    entity_queries.extend(ids);
                }
            }
        }
        if request.symbol.is_some() || request.keyword.is_some() {
            let raw_queries = [request.symbol.as_deref(), request.keyword.as_deref()]
                .into_iter()
                .flatten()
                .flat_map(search_terms)
                .collect::<Vec<_>>();
            outcome.items.retain(|item| {
                let text = format!("{} {}", item.title, item.summary).to_lowercase();
                raw_queries.iter().any(|query| text.contains(query))
                    || item.entity_links.iter().any(|link| {
                        entity_queries.contains(&link.entity_id)
                            || link.related_listed.iter().any(|related| {
                                entity_queries.contains(&related.entity_id)
                                    || raw_queries.iter().any(|query| query == &related.code)
                            })
                    })
            });
        }
        outcome.items.truncate(request.limit.clamp(1, 200));
        outcome
    }

    async fn fetch_named(
        &self,
        provider: Arc<dyn NewsProvider>,
        request: NewsIngestRequest,
        progress: Option<Arc<NewsProgressTracker>>,
    ) -> (String, Result<(NewsPage, bool), NewsProviderError>) {
        let id = provider.capabilities().provider_id.clone();
        (id, self.fetch_one(provider, request, progress).await)
    }

    async fn fetch_one(
        &self,
        provider: Arc<dyn NewsProvider>,
        mut request: NewsIngestRequest,
        progress: Option<Arc<NewsProgressTracker>>,
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
        self.wait_rate_limit(&capabilities, progress.as_ref()).await;
        if let Some(progress) = &progress {
            progress.update(&provider_id, "等待可用的采集并发槽", 0, 0, 0);
        }
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
            let attempt_number = attempt + 1;
            let attempt_started = Instant::now();
            self.mark_attempt(&provider_id);
            if let Some(progress) = &progress {
                progress.update(
                    &provider_id,
                    format!("正在访问上游，第 {attempt_number}/{} 次", MAX_RETRIES + 1),
                    attempt_number,
                    0,
                    0,
                );
            }
            let fetched = match tokio::time::timeout(
                self.provider_attempt_timeout,
                provider.fetch(request.clone()),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(NewsProviderError::new(
                    &provider_id,
                    NewsErrorKind::Timeout,
                    format!(
                        "单次来源访问超过 {} 秒，已中止本次尝试并释放资源",
                        self.provider_attempt_timeout.as_secs_f64()
                    ),
                    true,
                )),
            };
            match fetched {
                Ok(mut page) => {
                    let records = page.items.len();
                    if let Some(progress) = &progress {
                        progress.update(
                            &provider_id,
                            format!("已获取 {records} 条上游数据，开始保存可审计证据"),
                            attempt_number,
                            0,
                            records,
                        );
                    }
                    self.mark_success(&provider_id, started.elapsed());
                    self.persist_success(
                        &capabilities,
                        &mut page,
                        started.elapsed(),
                        progress.as_ref(),
                        attempt_number,
                    )
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
                    let delay = RETRY_BASE * 2_u32.pow(attempt);
                    if let (Some(progress), Some(error)) = (&progress, final_error.as_ref()) {
                        progress.update(
                            &provider_id,
                            format!(
                                "本次访问失败（{:?}），{:.1} 秒后自动重试",
                                error.kind,
                                delay.as_secs_f64()
                            ),
                            attempt_number,
                            0,
                            0,
                        );
                    }
                    tokio::time::sleep(delay).await;
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
        progress: Option<&Arc<NewsProgressTracker>>,
        attempt: u32,
    ) {
        let Some(storage) = &self.storage else {
            if let Some(progress) = progress {
                progress.update(
                    &capabilities.provider_id,
                    format!("已获取 {} 条数据；本次无需本地归档", page.items.len()),
                    attempt,
                    page.items.len(),
                    page.items.len(),
                );
            }
            return;
        };
        let observed_at = now_secs();
        let total = page.items.len();
        for (index, item) in page.items.iter_mut().enumerate() {
            let canonical_url = if item.url.trim().is_empty() {
                format!("urn:astock-news:{}:{}", item.provider_id, item.id)
            } else {
                canonicalize_url(&item.url)
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
                Ok(saved) => {
                    item.document_revision_id = Some(saved.revision_id.clone());
                    match NewsEventClusterer::new(storage.clone())
                        .assign_revision(&saved.revision_id)
                        .await
                    {
                        Ok(assignment) => {
                            item.event_cluster_id = Some(assignment.cluster_id);
                            item.event_relationship = Some(
                                serde_json::to_string(&assignment.relationship)
                                    .unwrap_or_else(|_| "\"follow_up\"".into())
                                    .trim_matches('"')
                                    .to_string(),
                            );
                            item.event_relationship_name =
                                Some(assignment.relationship.chinese_name().into());
                            item.independent_source_count = assignment.independent_sources as usize;
                            item.old_republication = assignment.old_republication;
                            item.cluster_explanation = Some(assignment.explanation);
                        }
                        Err(error) => tracing::warn!(
                            provider = capabilities.provider_id,
                            item = item.id,
                            %error,
                            "news event clustering failed; archived revision remains available"
                        ),
                    }
                    if let Some(linker) = &self.entity_linker {
                        match linker.link_revision(&saved.revision_id).await {
                            Ok(links) => {
                                item.entity_review_required = links
                                    .iter()
                                    .any(|link| link.status == LinkStatus::PendingReview);
                                item.entity_links = links
                                    .iter()
                                    .filter_map(|link| link.agent_summary())
                                    .collect();
                            }
                            Err(error) => tracing::warn!(
                                provider = capabilities.provider_id,
                                item = item.id,
                                %error,
                                "news entity linking failed; archived revision remains available"
                            ),
                        }
                    }
                }
                Err(error) => tracing::warn!(
                    provider = capabilities.provider_id,
                    item = item.id,
                    %error,
                    "news archive write failed; live result remains available"
                ),
            }
            let processed = index + 1;
            if let Some(progress) = progress
                .filter(|_| processed == 1 || processed == total || processed.is_multiple_of(5))
            {
                progress.update(
                    &capabilities.provider_id,
                    format!("正在保存证据并建立事件/实体关联：{processed}/{total} 条"),
                    attempt,
                    processed,
                    total,
                );
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

    async fn wait_rate_limit(
        &self,
        capabilities: &NewsCapabilities,
        progress: Option<&Arc<NewsProgressTracker>>,
    ) {
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
            if let Some(progress) = progress {
                progress.update(
                    &capabilities.provider_id,
                    format!(
                        "遵守来源访问频率，预计等待 {:.1} 秒后发起请求",
                        delay.as_secs_f64()
                    ),
                    0,
                    0,
                    0,
                );
            }
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
        self.persist_success(&capabilities, &mut page, Duration::ZERO, None, 0)
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

fn trust_rank(tier: NewsTrustTier) -> u8 {
    match tier {
        NewsTrustTier::FirstPartyDisclosure => 0,
        NewsTrustTier::LicensedMedia => 1,
        NewsTrustTier::PublicAggregator => 2,
        NewsTrustTier::SearchLead => 3,
    }
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
    let items = rows
        .iter()
        .take(request.limit.clamp(1, 200))
        .enumerate()
        .filter_map(|(index, raw)| normalize_generic_item(capabilities, raw, index + 1))
        .collect::<Vec<_>>();
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
        DataError::Network { ref message, .. } if message.starts_with("HTTP 4") => {
            // Authentication/WAF/client-policy failures do not recover by
            // immediately replaying the same request three times.
            (NewsErrorKind::Network, false)
        }
        DataError::Network { .. } => (NewsErrorKind::Network, true),
        DataError::WafBlocked(_) => (NewsErrorKind::Network, false),
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
                diagnostics: Vec::new(),
            })
        }
    }

    struct HangingProvider {
        capabilities: NewsCapabilities,
    }

    impl HangingProvider {
        fn new(id: &str) -> Self {
            Self {
                capabilities: FakeProvider::new(id, 0, NewsTrustTier::LicensedMedia).capabilities,
            }
        }
    }

    #[async_trait]
    impl NewsProvider for HangingProvider {
        fn capabilities(&self) -> &NewsCapabilities {
            &self.capabilities
        }

        async fn fetch(&self, _request: NewsIngestRequest) -> Result<NewsPage, NewsProviderError> {
            std::future::pending().await
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
    async fn same_event_from_two_sources_is_counted_once_with_source_diversity() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(temp.path())).unwrap();
        let mut official =
            FakeProvider::new("official-dedupe", 0, NewsTrustTier::FirstPartyDisclosure);
        official.title = "紫金矿业601899拟回购股份".into();
        let mut licensed = FakeProvider::new("licensed-dedupe", 0, NewsTrustTier::LicensedMedia);
        licensed.title = "紫金矿业601899拟回购股份".into();
        let ingestor =
            NewsIngestor::new(vec![Arc::new(official), Arc::new(licensed)], Some(storage)).unwrap();
        let outcome = ingestor
            .ingest(
                NewsIngestRequest {
                    limit: 10,
                    ..Default::default()
                },
                None,
            )
            .await;
        assert_eq!(
            outcome.items.len(),
            1,
            "same event must not be double-counted"
        );
        assert_eq!(outcome.items[0].independent_source_count, 2);
        assert!(outcome.items[0].event_cluster_id.is_some());
        assert_eq!(
            outcome.items[0].trust_tier,
            NewsTrustTier::FirstPartyDisclosure,
            "representative prefers first-party evidence"
        );
    }

    #[tokio::test]
    async fn entity_linking_finds_brand_news_for_listed_parent_query() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(temp.path())).unwrap();
        let mut provider = FakeProvider::new("brand-news", 0, NewsTrustTier::LicensedMedia);
        provider.title = "腾势发布新能源新车型".into();
        let ingestor = NewsIngestor::new(vec![Arc::new(provider)], Some(storage)).unwrap();
        let outcome = ingestor
            .ingest(
                NewsIngestRequest {
                    keyword: Some("比亚迪".into()),
                    limit: 10,
                    ..Default::default()
                },
                None,
            )
            .await;
        assert_eq!(outcome.items.len(), 1);
        assert!(outcome.items[0]
            .entity_links
            .iter()
            .any(|link| link.entity_id == "brand:denza"));
        assert!(outcome.items[0].entity_links.iter().any(|link| link
            .related_listed
            .iter()
            .any(|related| related.code == "002594" && related.eligible_for_agent)));
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
    async fn five_hanging_providers_time_out_independently_and_progress_finishes() {
        let providers = (0..5)
            .map(|index| {
                Arc::new(HangingProvider::new(&format!("hanging-{index}"))) as Arc<dyn NewsProvider>
            })
            .collect();
        let ingestor = NewsIngestor::new(providers, None)
            .unwrap()
            .with_attempt_timeout_for_tests(Duration::from_millis(20));
        let snapshots = Arc::new(Mutex::new(Vec::<NewsIngestProgress>::new()));
        let sink = Arc::clone(&snapshots);
        let started = Instant::now();
        let outcome = ingestor
            .ingest_with_progress(
                NewsIngestRequest {
                    limit: 15,
                    ..Default::default()
                },
                None,
                Some(Arc::new(move |progress| sink.lock().push(progress))),
            )
            .await;
        assert!(started.elapsed() < Duration::from_secs(4));
        assert!(outcome.items.is_empty());
        assert_eq!(outcome.errors.len(), 5);
        assert!(outcome
            .errors
            .iter()
            .all(|error| error.kind == NewsErrorKind::Timeout));
        let health = ingestor.health().await;
        assert_eq!(health.iter().map(|row| row.attempts).sum::<u64>(), 15);
        let snapshots = snapshots.lock();
        assert!(snapshots.iter().any(|snapshot| snapshot
            .active
            .iter()
            .any(|work| work.status.contains("重试"))));
        let final_progress = snapshots.last().unwrap();
        assert_eq!(final_progress.completed, 5);
        assert_eq!(final_progress.failed, 5);
        assert!(final_progress.active.is_empty());
        assert!(!final_progress.recent_errors.is_empty());
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

    #[tokio::test]
    async fn empty_provider_selection_has_actionable_error() {
        let ingestor = NewsIngestor::new(Vec::new(), None).unwrap();
        let outcome = ingestor.ingest(NewsIngestRequest::default(), None).await;
        assert!(outcome.items.is_empty());
        assert_eq!(outcome.errors.len(), 1);
        assert_eq!(outcome.errors[0].provider_id, "provider-registry");
        assert!(!outcome.errors[0].message.trim().is_empty());
    }

    #[tokio::test]
    async fn natural_language_terms_match_individually_and_report_progress() {
        let mut provider = FakeProvider::new("topics", 0, NewsTrustTier::LicensedMedia);
        provider.title = "新能源板块轮动与龙头跟踪".into();
        let ingestor = NewsIngestor::new(vec![Arc::new(provider)], None).unwrap();
        let snapshots = Arc::new(Mutex::new(Vec::<NewsIngestProgress>::new()));
        let sink = Arc::clone(&snapshots);
        let outcome = ingestor
            .ingest_with_progress(
                NewsIngestRequest {
                    keyword: Some("短线 题材 龙头 板块轮动".into()),
                    limit: 30,
                    ..Default::default()
                },
                None,
                Some(Arc::new(move |progress| sink.lock().push(progress))),
            )
            .await;
        assert_eq!(outcome.items.len(), 1);
        let snapshots = snapshots.lock();
        let final_progress = snapshots.last().unwrap();
        assert_eq!(final_progress.completed, 1);
        assert_eq!(final_progress.records, 1);
        assert_eq!(final_progress.failed, 0);
        assert_eq!(final_progress.current_provider, "资讯合并与条件匹配");
        assert!(final_progress.active.is_empty());
    }

    #[test]
    fn http_403_and_waf_are_not_immediately_retried() {
        let forbidden = classify_data_error(
            "news",
            astock_core::DataError::Network {
                host: "example.com".into(),
                message: "HTTP 403 Forbidden".into(),
            },
        );
        assert!(!forbidden.retryable);
        let waf = classify_data_error(
            "news",
            astock_core::DataError::WafBlocked("challenge".into()),
        );
        assert!(!waf.retryable);
    }

    #[test]
    fn generic_provider_contract_preserves_candidates_for_post_link_filtering() {
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
        assert_eq!(page.items.len(), 2);
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
