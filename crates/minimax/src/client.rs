//! High-level MiniMax client: detection, quota, model selection and chat.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::Stream;

use crate::chat::{ChatChunk, ChatRequest, ChatResponse, ChatStream};
use crate::error::MinimaxError;
use crate::http::{map_http_error, Http, ReqwestHttp};
use crate::key::SecretKey;
use crate::models::ModelCatalog;
use crate::quota::QuotaStatus;
use crate::rate_gate::RateGate;
use crate::region::{RegionDetector, ServiceInfo};

/// How long a quota snapshot is reused by the quota guard before refetching.
const QUOTA_CACHE_TTL: Duration = Duration::from_secs(30);

/// Entry point of the crate.
///
/// Wraps an API key with region detection, Token Plan quota introspection, a
/// model fallback chain and a backoff [`RateGate`]. Construct with
/// [`MinimaxClient::new`] and tune with the `with_*` builder methods.
pub struct MinimaxClient {
    http: Arc<dyn Http>,
    key: SecretKey,
    detector: RegionDetector,
    catalog: ModelCatalog,
    gate: RateGate,
    quota_guard: bool,
    quota_cache: tokio::sync::Mutex<Option<(SystemTime, QuotaStatus)>>,
}

impl MinimaxClient {
    /// Client over the production services with default catalog and gate.
    pub fn new(key: SecretKey) -> Self {
        Self::with_http(key, Arc::new(ReqwestHttp::new()))
    }

    /// Client over a custom transport (tests, proxies).
    pub fn with_http(key: SecretKey, http: Arc<dyn Http>) -> Self {
        Self {
            detector: RegionDetector::new(http.clone()),
            http,
            key,
            catalog: ModelCatalog::new(),
            gate: RateGate::default(),
            quota_guard: true,
            quota_cache: tokio::sync::Mutex::new(None),
        }
    }

    /// Override region detection (custom endpoints, pre-resolved service).
    pub fn with_detector(mut self, detector: RegionDetector) -> Self {
        self.detector = detector;
        self
    }

    /// Override the model fallback chain.
    pub fn with_catalog(mut self, catalog: ModelCatalog) -> Self {
        self.catalog = catalog;
        self
    }

    /// Override the retry/backoff policy.
    pub fn with_gate(mut self, gate: RateGate) -> Self {
        self.gate = gate;
        self
    }

    /// Enable/disable the pre-flight quota guard (enabled by default). When
    /// enabled, chat calls check the cached Token Plan first and fail with
    /// [`MinimaxError::QuotaExhausted`] instead of burning a request on an
    /// exhausted window.
    pub fn with_quota_guard(mut self, enabled: bool) -> Self {
        self.quota_guard = enabled;
        self
    }

    /// Detect (once, cached) which MiniMax service this key belongs to.
    pub async fn detect_service(&self) -> Result<ServiceInfo, MinimaxError> {
        self.detector.detect_service(&self.key).await
    }

    /// Fetch the Token Plan quota for all models.
    pub async fn quota(&self) -> Result<QuotaStatus, MinimaxError> {
        let service = self.detect_service().await?;
        let url = format!("{}/v1/token_plan/remains", service.www_host);
        let resp = self.http.get(&url, Some(&self.key)).await?;
        if resp.status != 200 {
            return Err(map_http_error(resp.status, &resp.headers, &resp.body));
        }
        crate::quota::parse_remains(&resp.body)
    }

    /// Probe the model fallback chain and return the selected model (cached).
    pub async fn selected_model(&self) -> Result<String, MinimaxError> {
        let service = self.detect_service().await?;
        self.catalog
            .probe_models(&*self.http, &service.api_host, &self.key)
            .await
    }

    /// The model fallback chain.
    pub fn catalog(&self) -> &ModelCatalog {
        &self.catalog
    }

    /// Non-streaming chat completion, wrapped in the [`RateGate`].
    pub async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, MinimaxError> {
        self.ensure_quota(&request.model).await?;
        let service = self.detect_service().await?;
        let url = format!("{}/v1/chat/completions", service.api_host);
        let mut body = serde_json::to_value(request)
            .map_err(|e| MinimaxError::Parse(format!("chat request: {e}")))?;
        if let Some(obj) = body.as_object_mut() {
            obj.insert("stream".to_string(), false.into());
        }
        tracing::debug!(model = %request.model, "chat completion request");
        let http = &self.http;
        let key = &self.key;
        self.gate
            .run(|| async {
                let resp = http.post(&url, Some(key), Some(&body)).await?;
                if resp.status != 200 {
                    return Err(map_http_error(resp.status, &resp.headers, &resp.body));
                }
                let parsed: ChatResponse = serde_json::from_slice(&resp.body)
                    .map_err(|e| MinimaxError::Parse(format!("chat response: {e}")))?;
                parsed.check_base_resp()?;
                Ok(parsed)
            })
            .await
    }

    /// Streaming chat completion as a stream of SSE chunks.
    ///
    /// Stream establishment goes through the quota guard; mid-stream failures
    /// surface as `Err` items and are not retried (retry by re-calling).
    pub async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> Result<impl Stream<Item = Result<ChatChunk, MinimaxError>> + Send + use<>, MinimaxError>
    {
        self.ensure_quota(&request.model).await?;
        let service = self.detect_service().await?;
        let url = format!("{}/v1/chat/completions", service.api_host);
        let mut body = serde_json::to_value(request)
            .map_err(|e| MinimaxError::Parse(format!("chat request: {e}")))?;
        if let Some(obj) = body.as_object_mut() {
            obj.insert("stream".to_string(), true.into());
        }
        tracing::debug!(model = %request.model, "streaming chat completion request");
        let bytes = self.http.post_stream(&url, &self.key, &body).await?;
        Ok(ChatStream::from_byte_stream(bytes))
    }

    /// Pre-flight quota check: fail fast when the rolling window is exhausted
    /// so pause-and-resume does not burn requests. A failed quota fetch is
    /// logged and treated as "unknown" — it never blocks a chat call.
    async fn ensure_quota(&self, model: &str) -> Result<(), MinimaxError> {
        if !self.quota_guard {
            return Ok(());
        }
        let quota = match self.cached_quota().await {
            Ok(quota) => quota,
            Err(e) => {
                tracing::warn!(error = %e, "quota check failed; proceeding without guard");
                return Ok(());
            }
        };
        if quota.exhausted(model) {
            tracing::info!(%model, "quota window exhausted; refusing to burn a request");
            return Err(MinimaxError::QuotaExhausted {
                window_reset_at: quota.window_reset_at(model),
            });
        }
        if quota.throttled(model) {
            let pacing = quota.pacing(model);
            tracing::info!(%model, ?pacing.min_interval, "quota nearly exhausted; consider pacing");
        }
        Ok(())
    }

    async fn cached_quota(&self) -> Result<QuotaStatus, MinimaxError> {
        {
            let cache = self.quota_cache.lock().await;
            if let Some((fetched, quota)) = &*cache {
                if fetched.elapsed().map(|age| age < QUOTA_CACHE_TTL).unwrap_or(false) {
                    return Ok(quota.clone());
                }
            }
        }
        let quota = self.quota().await?;
        let mut cache = self.quota_cache.lock().await;
        *cache = Some((SystemTime::now(), quota.clone()));
        Ok(quota)
    }
}
