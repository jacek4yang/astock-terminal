//! Resilient high-level MiniMax client.
//!
//! The provider implementation from the previous release remains verbatim in
//! `client_legacy.rs`. This facade preserves its public API while making SSE
//! rounds transactional before the first user-visible/tool-call delta:
//!
//! - reasoning-only chunks are buffered briefly, so a connection loss during
//!   private reasoning can be retried without duplicating visible output or
//!   corrupting the tool-call transcript;
//! - first-chunk and inter-chunk idle watchdogs turn a permanently silent
//!   connection into a typed transient error instead of an Agent that appears
//!   to run forever;
//! - a bounded restart loop is allowed only before protocol state is committed.
//!   Once visible text or a tool call has been emitted, failures propagate and
//!   the durable Agent task can be resumed from its last persisted round.

mod legacy {
    include!("client_legacy.rs");
}

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures::{Stream, StreamExt};
use rand::Rng;

use crate::chat::{ChatChunk, ChatRequest, ChatResponse};
use crate::error::MinimaxError;
use crate::http::{Http, ReqwestHttp};
use crate::key::SecretKey;
use crate::models::{AvailableModel, ModelCatalog};
use crate::quota::QuotaStatus;
use crate::rate_gate::RateGate;
use crate::region::{RegionDetector, ServiceInfo};

/// Safety policy for one streamed model round.
#[derive(Debug, Clone, Copy)]
pub struct StreamPolicy {
    /// Maximum silence before the first SSE chunk.
    pub first_chunk_timeout: Duration,
    /// Maximum silence between subsequent SSE chunks.
    pub idle_timeout: Duration,
    /// Number of complete request restarts allowed before visible text/tool
    /// protocol state has been emitted.
    pub max_precommit_restarts: u32,
    /// Maximum private-reasoning buffer before chunks are committed to the
    /// caller to keep memory bounded.
    pub max_buffered_bytes: usize,
}

impl Default for StreamPolicy {
    fn default() -> Self {
        Self {
            first_chunk_timeout: Duration::from_secs(90),
            idle_timeout: Duration::from_secs(120),
            max_precommit_restarts: 2,
            max_buffered_bytes: 2 * 1024 * 1024,
        }
    }
}

/// Entry point of the MiniMax provider with resilient streaming semantics.
pub struct MinimaxClient {
    inner: Arc<legacy::MinimaxClient>,
    stream_policy: StreamPolicy,
}

impl MinimaxClient {
    /// Client over production services with default catalog, rate gate and
    /// resilient stream policy.
    pub fn new(key: SecretKey) -> Self {
        Self::with_http(key, Arc::new(ReqwestHttp::new()))
    }

    /// Client over a custom transport (tests, explicit proxy transports).
    pub fn with_http(key: SecretKey, http: Arc<dyn Http>) -> Self {
        Self {
            inner: Arc::new(legacy::MinimaxClient::with_http(key, http)),
            stream_policy: StreamPolicy::default(),
        }
    }

    fn map_inner(
        self,
        update: impl FnOnce(legacy::MinimaxClient) -> legacy::MinimaxClient,
    ) -> Self {
        let Self {
            inner,
            stream_policy,
        } = self;
        let inner = match Arc::try_unwrap(inner) {
            Ok(inner) => inner,
            Err(_) => panic!("MiniMax client builders must run before the client is shared"),
        };
        Self {
            inner: Arc::new(update(inner)),
            stream_policy,
        }
    }

    /// Override region detection.
    pub fn with_detector(self, detector: RegionDetector) -> Self {
        self.map_inner(|inner| inner.with_detector(detector))
    }

    /// Override the model fallback chain.
    pub fn with_catalog(self, catalog: ModelCatalog) -> Self {
        self.map_inner(|inner| inner.with_catalog(catalog))
    }

    /// Override the retry/backoff policy used at request establishment.
    pub fn with_gate(self, gate: RateGate) -> Self {
        self.map_inner(|inner| inner.with_gate(gate))
    }

    /// Enable or disable the pre-flight Token Plan quota guard.
    pub fn with_quota_guard(self, enabled: bool) -> Self {
        self.map_inner(|inner| inner.with_quota_guard(enabled))
    }

    /// Override streamed-round watchdogs. Primarily useful for deterministic
    /// integration tests and constrained private deployments.
    pub fn with_stream_policy(mut self, policy: StreamPolicy) -> Self {
        self.stream_policy = policy;
        self
    }

    /// Detect and cache the MiniMax service region for this key.
    pub async fn detect_service(&self) -> Result<ServiceInfo, MinimaxError> {
        self.inner.detect_service().await
    }

    /// Fetch Token Plan quota for all models.
    pub async fn quota(&self) -> Result<QuotaStatus, MinimaxError> {
        self.inner.quota().await
    }

    /// Discover models available to this key.
    pub async fn available_models(&self) -> Result<Vec<AvailableModel>, MinimaxError> {
        self.inner.available_models().await
    }

    /// Search the public web through MiniMax Coding Plan's official endpoint.
    pub async fn web_search(&self, query: &str) -> Result<serde_json::Value, MinimaxError> {
        self.inner.web_search(query).await
    }

    /// Probe the configured model fallback chain.
    pub async fn selected_model(&self) -> Result<String, MinimaxError> {
        self.inner.selected_model().await
    }

    /// The active model catalog.
    pub fn catalog(&self) -> &ModelCatalog {
        self.inner.catalog()
    }

    /// Non-streaming chat completion.
    pub async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, MinimaxError> {
        self.inner.chat(request).await
    }

    /// Streaming chat completion with pre-commit replay safety and idle
    /// watchdogs.
    pub async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> Result<impl Stream<Item = Result<ChatChunk, MinimaxError>> + Send + use<>, MinimaxError>
    {
        let initial = self.inner.chat_stream(request).await?;
        let state = ResilientStreamState {
            inner: self.inner.clone(),
            request: request.clone(),
            stream: Some(Box::pin(initial)),
            pending: VecDeque::new(),
            buffered: VecDeque::new(),
            buffered_bytes: 0,
            committed: false,
            terminal_seen: false,
            restart_count: 0,
            done: false,
            policy: self.stream_policy,
        };

        Ok(futures::stream::unfold(state, |mut state| async move {
            loop {
                if state.done {
                    return None;
                }
                if let Some(item) = state.pending.pop_front() {
                    return Some((item, state));
                }

                if state.stream.is_none() {
                    match state.inner.chat_stream(&state.request).await {
                        Ok(stream) => {
                            state.stream = Some(Box::pin(stream));
                        }
                        Err(error)
                            if error.is_transient()
                                && state.restart_count < state.policy.max_precommit_restarts =>
                        {
                            state.restart_count += 1;
                            tokio::time::sleep(restart_delay(state.restart_count)).await;
                            continue;
                        }
                        Err(error) => {
                            state.done = true;
                            return Some((Err(error), state));
                        }
                    }
                }

                let timeout = if state.buffered.is_empty() && !state.committed {
                    state.policy.first_chunk_timeout
                } else {
                    state.policy.idle_timeout
                };
                let next = {
                    let stream = state.stream.as_mut().expect("stream established");
                    tokio::time::timeout(timeout, stream.next()).await
                };

                match next {
                    Ok(Some(Ok(chunk))) => {
                        if chunk.finish_reason().is_some() {
                            state.terminal_seen = true;
                        }
                        if state.committed {
                            return Some((Ok(chunk), state));
                        }

                        state.buffered_bytes = state
                            .buffered_bytes
                            .saturating_add(serde_json::to_vec(&chunk).map_or(0, |value| value.len()));
                        let commits_protocol = chunk_commits_protocol(&chunk)
                            || state.buffered_bytes >= state.policy.max_buffered_bytes;
                        state.buffered.push_back(chunk);
                        if commits_protocol {
                            state.committed = true;
                            while let Some(buffered) = state.buffered.pop_front() {
                                state.pending.push_back(Ok(buffered));
                            }
                            state.buffered_bytes = 0;
                        }
                    }
                    Ok(Some(Err(error))) => {
                        if !state.committed
                            && error.is_transient()
                            && state.restart_count < state.policy.max_precommit_restarts
                        {
                            state.prepare_restart();
                            tokio::time::sleep(restart_delay(state.restart_count)).await;
                            continue;
                        }
                        state.done = true;
                        return Some((Err(error), state));
                    }
                    Ok(None) => {
                        if !state.committed
                            && state.restart_count < state.policy.max_precommit_restarts
                        {
                            state.prepare_restart();
                            tokio::time::sleep(restart_delay(state.restart_count)).await;
                            continue;
                        }
                        if state.committed && state.terminal_seen {
                            state.done = true;
                            return None;
                        }
                        state.done = true;
                        return Some((
                            Err(MinimaxError::Network(
                                "MiniMax 流在终止标记前关闭；本轮未被视为完整回答，可从持久化任务安全重试"
                                    .to_string(),
                            )),
                            state,
                        ));
                    }
                    Err(_) => {
                        if !state.committed
                            && state.restart_count < state.policy.max_precommit_restarts
                        {
                            state.prepare_restart();
                            tokio::time::sleep(restart_delay(state.restart_count)).await;
                            continue;
                        }
                        state.done = true;
                        return Some((
                            Err(MinimaxError::Network(format!(
                                "MiniMax 流连续 {} 秒没有数据，已触发空闲看门狗；本轮可从最后持久化检查点继续",
                                timeout.as_secs()
                            ))),
                            state,
                        ));
                    }
                }
            }
        }))
    }
}

type BoxChatStream = Pin<Box<dyn Stream<Item = Result<ChatChunk, MinimaxError>> + Send>>;

struct ResilientStreamState {
    inner: Arc<legacy::MinimaxClient>,
    request: ChatRequest,
    stream: Option<BoxChatStream>,
    pending: VecDeque<Result<ChatChunk, MinimaxError>>,
    buffered: VecDeque<ChatChunk>,
    buffered_bytes: usize,
    committed: bool,
    terminal_seen: bool,
    restart_count: u32,
    done: bool,
    policy: StreamPolicy,
}

impl ResilientStreamState {
    fn prepare_restart(&mut self) {
        self.stream = None;
        self.pending.clear();
        self.buffered.clear();
        self.buffered_bytes = 0;
        self.terminal_seen = false;
        self.restart_count = self.restart_count.saturating_add(1);
        tracing::warn!(
            attempt = self.restart_count,
            model = %self.request.model,
            "restarting MiniMax stream before protocol commit"
        );
    }
}

fn chunk_commits_protocol(chunk: &ChatChunk) -> bool {
    chunk
        .raw_delta()
        .is_some_and(|text| !text.is_empty())
        || !chunk.tool_calls().is_empty()
        || chunk.finish_reason().is_some()
}

fn restart_delay(attempt: u32) -> Duration {
    let ceiling_ms = 500_u64
        .saturating_mul(1_u64 << attempt.saturating_sub(1).min(3))
        .min(4_000);
    Duration::from_millis(rand::rng().random_range(0..=ceiling_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{ChatChoice, ChatMessage, ToolCall, ToolCallFunction};
    use serde_json::Value;

    #[test]
    fn only_visible_or_tool_protocol_chunks_commit_a_round() {
        let reasoning = ChatChunk {
            choices: vec![ChatChoice {
                delta: Some(ChatMessage {
                    reasoning_content: Some("private".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(!chunk_commits_protocol(&reasoning));

        let text = ChatChunk {
            choices: vec![ChatChoice {
                delta: Some(ChatMessage {
                    content: Some(Value::String("answer".to_string())),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(chunk_commits_protocol(&text));

        let tool = ChatChunk {
            choices: vec![ChatChoice {
                delta: Some(ChatMessage {
                    tool_calls: Some(vec![ToolCall {
                        function: Some(ToolCallFunction {
                            name: Some("get_quote".to_string()),
                            arguments: Some("{}".to_string()),
                        }),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(chunk_commits_protocol(&tool));
    }

    #[test]
    fn restart_backoff_is_bounded() {
        for attempt in 1..=20 {
            assert!(restart_delay(attempt) <= Duration::from_secs(4));
        }
    }
}
