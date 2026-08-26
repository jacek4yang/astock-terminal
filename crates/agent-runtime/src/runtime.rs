use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use astock_protocol::TaskSpec;

use crate::error::{ProviderErrorKind, RuntimeError};
use crate::events::{AgentEvent, AgentPhase, VerificationFinding};
use crate::model::{Message, MessageRole, ModelChunk, ModelProvider, ModelRequest, ModelToolCall};
use crate::prompt;
use crate::session::{
    RuntimeSession, SessionMessageRole, SessionTaskState, MAX_MODEL_HISTORY_CHARS,
    MAX_MODEL_HISTORY_MESSAGES,
};
use crate::store::{AgentStore, EffectIntent, StoredCheckpoint};
use crate::tools::{default_registry, ToolDefinition, ToolExecutor, ToolRegistry};

const MAX_OBJECTIVE_CHARS: usize = 120_000;

/// Characters of a tool result that may enter the model conversation.
///
/// Deliberately far below `max_tool_result_bytes`, which bounds what may be
/// *stored*. A result large enough to be worth persisting is usually far too
/// large to put in a context window, and mixing the two limits caused a live
/// task to fail with `context window exceeds limit` after 13 successful tool
/// calls.
const DEFAULT_MAX_TOOL_RESULT_MODEL_CHARS: usize = 24_000;

/// Evidence identifiers listed to the model for a single tool result.
///
/// A market-wide preparation tool can register thousands of observations. The
/// model needs enough identifiers to cite specific facts, not the entire
/// registry; the full set stays in durable task state either way.
const DEFAULT_MAX_EVIDENCE_IDS_IN_CONTEXT: usize = 40;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeTask {
    pub objective: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub depth: String,
    pub tool_policy: String,
    pub language: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capital: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_summary: Option<String>,
}

impl RuntimeTask {
    pub fn ask(objective: impl Into<String>) -> Self {
        Self {
            objective: objective.into(),
            symbol: None,
            depth: "balanced".into(),
            tool_policy: "full".into(),
            language: "zh-CN".into(),
            capital: None,
            history: Vec::new(),
            history_summary: None,
        }
    }

    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.objective.trim().is_empty() {
            return Err(RuntimeError::Configuration(
                "research objective must not be empty".into(),
            ));
        }
        if self.objective.chars().count() > MAX_OBJECTIVE_CHARS {
            return Err(RuntimeError::Configuration(format!(
                "research objective exceeds {MAX_OBJECTIVE_CHARS} characters"
            )));
        }
        if !matches!(
            self.depth.as_str(),
            "fast" | "balanced" | "deep" | "exhaustive"
        ) {
            return Err(RuntimeError::Configuration(format!(
                "unknown research depth `{}`",
                self.depth
            )));
        }
        if !matches!(
            self.tool_policy.as_str(),
            "auto" | "market" | "evidence" | "full"
        ) {
            return Err(RuntimeError::Configuration(format!(
                "unknown tool policy `{}`",
                self.tool_policy
            )));
        }
        if let Some(symbol) = &self.symbol {
            if symbol.len() != 6 || !symbol.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(RuntimeError::Configuration(format!(
                    "symbol `{symbol}` must contain exactly six digits"
                )));
            }
        }
        if self
            .capital
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            return Err(RuntimeError::Configuration(
                "capital must be a finite positive number".into(),
            ));
        }
        if self.history.len() > MAX_MODEL_HISTORY_MESSAGES {
            return Err(RuntimeError::Configuration(format!(
                "model history exceeds {MAX_MODEL_HISTORY_MESSAGES} messages"
            )));
        }
        let mut history_chars = 0usize;
        for message in &self.history {
            if !matches!(message.role, MessageRole::User | MessageRole::Assistant)
                || message.content.trim().is_empty()
                || !message.tool_calls.is_empty()
                || message.tool_call_id.is_some()
            {
                return Err(RuntimeError::Configuration(
                    "model history accepts only non-empty user/assistant text messages".into(),
                ));
            }
            history_chars = history_chars.saturating_add(message.content.chars().count());
        }
        if history_chars > MAX_MODEL_HISTORY_CHARS {
            return Err(RuntimeError::Configuration(format!(
                "model history exceeds {MAX_MODEL_HISTORY_CHARS} characters"
            )));
        }
        if self.history_summary.as_ref().is_some_and(|summary| {
            summary.trim().is_empty()
                || summary.chars().count() > crate::session::MAX_SESSION_SUMMARY_CHARS
        }) {
            return Err(RuntimeError::Configuration(format!(
                "history summary must contain 1..{} characters",
                crate::session::MAX_SESSION_SUMMARY_CHARS
            )));
        }
        Ok(())
    }

    fn verification_spec(&self) -> TaskSpec {
        let now = chrono::Utc::now();
        let start = now - chrono::Duration::days(365);
        TaskSpec {
            objective: self.objective.clone(),
            security_universe: vec![self.symbol.clone().unwrap_or_else(|| "A股市场".into())],
            as_of: now.to_rfc3339(),
            research_start: start.format("%Y-%m-%d").to_string(),
            research_end: now.format("%Y-%m-%d").to_string(),
            investment_horizon: "用户未指定；按中期研究框架并标为假设".into(),
            comparison_benchmark: "000300".into(),
            output_type: if self.objective.contains("计划") {
                "manual_plan".into()
            } else {
                "research_report".into()
            },
            evidence_requirement: "strict".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub max_model_rounds: usize,
    pub max_model_chunks_per_round: usize,
    pub max_visible_chars_per_round: usize,
    pub max_tool_calls_per_round: usize,
    pub max_tool_argument_chars: usize,
    pub max_parallel_tools: usize,
    pub max_tool_result_bytes: usize,
    pub max_tokens: u32,
    pub temperature: f64,
    pub provider_connect_timeout: Duration,
    pub provider_idle_timeout: Duration,
    pub verification_timeout: Duration,
    pub max_verification_revisions: usize,
    pub verify_reports: bool,
    pub event_channel_capacity: usize,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_model_rounds: 16,
            max_model_chunks_per_round: 10_000,
            max_visible_chars_per_round: 120_000,
            max_tool_calls_per_round: 32,
            max_tool_argument_chars: 256_000,
            max_parallel_tools: 4,
            max_tool_result_bytes: 2 * 1024 * 1024,
            max_tokens: 8_192,
            temperature: 0.2,
            provider_connect_timeout: Duration::from_secs(90),
            provider_idle_timeout: Duration::from_secs(120),
            verification_timeout: Duration::from_secs(30),
            max_verification_revisions: 2,
            verify_reports: true,
            event_channel_capacity: 256,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutcome {
    pub task_id: String,
    pub report: String,
    pub evidence_ids: Vec<String>,
}

#[derive(Clone)]
pub struct AgentRuntime {
    provider: Arc<dyn ModelProvider>,
    executor: Arc<dyn ToolExecutor>,
    store: Arc<dyn AgentStore>,
    tools: ToolRegistry,
    config: RuntimeConfig,
}

impl AgentRuntime {
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        executor: Arc<dyn ToolExecutor>,
        store: Arc<dyn AgentStore>,
    ) -> Self {
        Self {
            provider,
            executor,
            store,
            tools: default_registry(),
            config: RuntimeConfig::default(),
        }
    }

    pub fn with_registry(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_config(mut self, config: RuntimeConfig) -> Self {
        self.config = config;
        self
    }

    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    pub fn start(&self, task: RuntimeTask) -> TaskStream {
        let task_id = Uuid::new_v4().to_string();
        self.start_with_task_id(task_id, task)
    }

    fn start_with_task_id(&self, task_id: String, task: RuntimeTask) -> TaskStream {
        let cancellation = CancellationToken::new();
        let (sender, receiver) = mpsc::channel(self.config.event_channel_capacity.max(8));
        let runtime = self.clone();
        let spawned_task_id = task_id.clone();
        let spawned_cancellation = cancellation.clone();
        let join = tokio::spawn(async move {
            runtime
                .execute_task(spawned_task_id, task, sender, spawned_cancellation)
                .await
        });
        TaskStream {
            task_id,
            receiver,
            cancellation,
            join: Some(join),
        }
    }

    pub async fn run(&self, task: RuntimeTask) -> Result<RunOutcome, RuntimeError> {
        let mut stream = self.start(task);
        while stream.recv().await.is_some() {}
        stream.finish().await
    }

    /// Start one durable multi-turn session turn. The conversation is saved
    /// before the task starts, at every non-text transition and at terminal
    /// completion. Token deltas remain durable in the task event log without
    /// rewriting the conversation row for every fragment.
    pub fn start_session_turn(
        &self,
        mut session: RuntimeSession,
        mut task: RuntimeTask,
    ) -> SessionTaskStream {
        let task_id = Uuid::new_v4().to_string();
        let cancellation = CancellationToken::new();
        let (sender, receiver) = mpsc::channel(self.config.event_channel_capacity.max(8));
        let runtime = self.clone();
        let spawned_task_id = task_id.clone();
        let session_id = session.session_id.clone();
        let spawned_cancellation = cancellation.clone();
        let join = tokio::spawn(async move {
            session.validate()?;
            task.validate()?;
            session.ensure_title(&task.objective);
            if task.history.is_empty() {
                session.refresh_compacted_summary();
                task.history_summary = session.summary.clone();
                task.history = session.model_history();
            }
            task.validate()?;
            session.push_message(SessionMessageRole::User, task.objective.trim());
            session.input.clear();
            session.depth = task.depth.clone();
            session.tool_policy = task.tool_policy.clone();
            session.task = Some(SessionTaskState {
                task_id: spawned_task_id.clone(),
                phase: AgentPhase::Preparing,
                accepted_seq: 0,
                model_round: 0,
                completed_tool_ids: Vec::new(),
                evidence_ids: Vec::new(),
                plan: None,
            });
            session.validate()?;
            runtime
                .store
                .save_session(&session)
                .await
                .map_err(RuntimeError::Store)?;

            let mut task_stream = runtime.start_with_task_id(spawned_task_id, task);
            let mut cancellation_forwarded = false;
            loop {
                let event = tokio::select! {
                    _ = spawned_cancellation.cancelled(), if !cancellation_forwarded => {
                        cancellation_forwarded = true;
                        task_stream.cancel();
                        continue;
                    }
                    event = task_stream.recv() => event,
                };
                let Some(event) = event else { break };
                apply_session_event(&mut session, &event);
                if !matches!(event, AgentEvent::TextDelta { .. }) {
                    runtime
                        .store
                        .save_session(&session)
                        .await
                        .map_err(RuntimeError::Store)?;
                }
                if sender.send(event).await.is_err() {
                    task_stream.cancel();
                    return Err(RuntimeError::Cancelled);
                }
            }
            let run = task_stream.finish().await?;
            runtime
                .store
                .save_session(&session)
                .await
                .map_err(RuntimeError::Store)?;
            Ok(SessionRunOutcome { run, session })
        });
        SessionTaskStream {
            task_id,
            session_id,
            receiver,
            cancellation,
            join: Some(join),
        }
    }

    pub async fn run_session_turn(
        &self,
        session: RuntimeSession,
        task: RuntimeTask,
    ) -> Result<SessionRunOutcome, RuntimeError> {
        let mut stream = self.start_session_turn(session, task);
        while stream.recv().await.is_some() {}
        stream.finish().await
    }

    async fn execute_task(
        &self,
        task_id: String,
        task: RuntimeTask,
        sender: mpsc::Sender<AgentEvent>,
        cancellation: CancellationToken,
    ) -> Result<RunOutcome, RuntimeError> {
        task.validate()?;
        self.store
            .create_task(&task_id, &task)
            .await
            .map_err(RuntimeError::Store)?;
        let mut state = RunState::new(task_id, sender, cancellation);
        let result = self.execute_loop(&task, &mut state).await;
        if let Err(error) = &result {
            let terminal = match error {
                RuntimeError::Cancelled => {
                    state.phase = AgentPhase::Cancelled;
                    AgentEvent::Cancelled
                }
                RuntimeError::Provider(provider)
                    if provider.retryable
                        || matches!(
                            provider.kind,
                            ProviderErrorKind::Quota | ProviderErrorKind::RateLimited
                        ) =>
                {
                    state.phase = AgentPhase::Suspended;
                    AgentEvent::Suspended {
                        reason: provider.to_string(),
                    }
                }
                _ => {
                    state.phase = AgentPhase::Failed;
                    AgentEvent::Failed {
                        message: error.to_string(),
                    }
                }
            };
            if let Err(persist_error) = self.record(&mut state, terminal).await {
                tracing::error!(
                    task_id = %state.task_id,
                    error = %persist_error,
                    "failed to persist terminal Agent event"
                );
            }
        }
        result
    }

    async fn execute_loop(
        &self,
        task: &RuntimeTask,
        state: &mut RunState,
    ) -> Result<RunOutcome, RuntimeError> {
        self.record(
            state,
            AgentEvent::SessionStarted {
                task_id: state.task_id.clone(),
                objective: task.objective.clone(),
            },
        )
        .await?;
        self.record(state, AgentEvent::UserMessageAccepted).await?;
        state.phase = AgentPhase::Planning;
        self.record(state, AgentEvent::PlanningStarted).await?;
        self.record(
            state,
            AgentEvent::PlanUpdated {
                summary: "根据问题动态选择行情、基本面、资讯、量化和证据核验工具".into(),
            },
        )
        .await?;

        let model_effect = self
            .begin_effect(
                state,
                "provider.model.select",
                json!({"provider": self.provider.name()}),
            )
            .await?;
        let selection = tokio::select! {
            _ = state.cancellation.cancelled() => {
                self.complete_effect(
                    &model_effect,
                    "failed",
                    json!({"error": "cancelled during provider model discovery"}),
                )
                .await?;
                return Err(RuntimeError::Cancelled);
            }
            result = tokio::time::timeout(
                self.config.provider_connect_timeout,
                self.provider.selected_model(),
            ) => result,
        };
        let selected_model = match selection {
            Ok(Ok(model)) => {
                self.complete_effect(&model_effect, "succeeded", json!({"model": model}))
                    .await?;
                model
            }
            Ok(Err(error)) => {
                self.complete_effect(&model_effect, "failed", json!({"error": error.to_string()}))
                    .await?;
                return Err(error.into());
            }
            Err(_) => {
                self.complete_effect(
                    &model_effect,
                    "failed",
                    json!({"error": "provider model discovery timed out"}),
                )
                .await?;
                return Err(RuntimeError::Provider(crate::ProviderError::new(
                    ProviderErrorKind::Network,
                    "provider model discovery timed out",
                    true,
                )));
            }
        };

        let mut messages = Vec::with_capacity(task.history.len() + 2);
        messages.push(Message::text(
            MessageRole::System,
            prompt::system_prompt(task),
        ));
        if let Some(summary) = &task.history_summary {
            messages.push(Message::text(
                MessageRole::System,
                format!(
                    "以下是较早会话的确定性压缩索引，仅用于理解用户上下文；它不是当前事实、不是新证据，也不得替代重新取数与证据引用：\n{summary}"
                ),
            ));
        }
        messages.extend(task.history.clone());
        messages.push(Message::text(MessageRole::User, task.objective.trim()));
        let mut evidence_contexts = Vec::<Value>::new();
        let mut revision_count = 0usize;

        for round in 1..=self.config.max_model_rounds {
            self.ensure_active(state)?;
            state.model_round = round;
            state.phase = AgentPhase::Reasoning;
            self.record(
                state,
                AgentEvent::ModelStarted {
                    model: selected_model.clone(),
                    round,
                },
            )
            .await?;

            let request = ModelRequest {
                model: selected_model.clone(),
                messages: messages.clone(),
                tools: self.tools.definitions(),
                max_tokens: self.config.max_tokens,
                temperature: self.config.temperature,
            };
            let effect_id = self
                .begin_effect(
                    state,
                    "provider.stream",
                    json!({
                        "provider": self.provider.name(),
                        "model": selected_model,
                        "round": round,
                        "message_count": request.messages.len(),
                        "tool_count": request.tools.len(),
                    }),
                )
                .await?;
            let connection = tokio::select! {
                _ = state.cancellation.cancelled() => {
                    self.complete_effect(
                        &effect_id,
                        "failed",
                        json!({"error": "cancelled during provider connection"}),
                    )
                    .await?;
                    return Err(RuntimeError::Cancelled);
                }
                result = tokio::time::timeout(
                    self.config.provider_connect_timeout,
                    self.provider.stream(request),
                ) => result,
            };
            let mut stream = match connection {
                Ok(Ok(stream)) => stream,
                Ok(Err(error)) => {
                    self.complete_effect(&effect_id, "failed", json!({"error": error.to_string()}))
                        .await?;
                    return Err(error.into());
                }
                Err(_) => {
                    self.complete_effect(
                        &effect_id,
                        "failed",
                        json!({"error": "provider connection timed out"}),
                    )
                    .await?;
                    return Err(RuntimeError::Provider(crate::ProviderError::new(
                        ProviderErrorKind::Network,
                        "provider connection timed out",
                        true,
                    )));
                }
            };

            let mut text = String::new();
            let mut visible_chars = 0usize;
            let mut chunk_count = 0usize;
            let mut partial_calls = BTreeMap::<u32, PartialToolCall>::new();
            loop {
                self.ensure_active(state)?;
                let next = tokio::select! {
                    _ = state.cancellation.cancelled() => {
                        self.complete_effect(
                            &effect_id,
                            "failed",
                            json!({"error": "cancelled during provider stream"}),
                        )
                        .await?;
                        return Err(RuntimeError::Cancelled);
                    }
                    result = tokio::time::timeout(
                        self.config.provider_idle_timeout,
                        stream.next(),
                    ) => result,
                };
                match next {
                    Ok(Some(Ok(ModelChunk::TextDelta(delta)))) => {
                        chunk_count = chunk_count.saturating_add(1);
                        if chunk_count > self.config.max_model_chunks_per_round {
                            let error = self
                                .fail_malformed_stream(
                                    &effect_id,
                                    format!(
                                        "model round exceeded {} streamed chunks",
                                        self.config.max_model_chunks_per_round
                                    ),
                                )
                                .await?;
                            return Err(error);
                        }
                        if !delta.is_empty() {
                            visible_chars = visible_chars.saturating_add(delta.chars().count());
                            if visible_chars > self.config.max_visible_chars_per_round {
                                let error = self
                                    .fail_malformed_stream(
                                        &effect_id,
                                        format!(
                                            "model round exceeded {} visible characters",
                                            self.config.max_visible_chars_per_round
                                        ),
                                    )
                                    .await?;
                                return Err(error);
                            }
                            text.push_str(&delta);
                            self.record(state, AgentEvent::TextDelta { text: delta })
                                .await?;
                        }
                    }
                    Ok(Some(Ok(ModelChunk::ToolCallDelta {
                        index,
                        id,
                        name,
                        arguments,
                    }))) => {
                        chunk_count = chunk_count.saturating_add(1);
                        if chunk_count > self.config.max_model_chunks_per_round {
                            let error = self
                                .fail_malformed_stream(
                                    &effect_id,
                                    format!(
                                        "model round exceeded {} streamed chunks",
                                        self.config.max_model_chunks_per_round
                                    ),
                                )
                                .await?;
                            return Err(error);
                        }
                        if !partial_calls.contains_key(&index)
                            && partial_calls.len() >= self.config.max_tool_calls_per_round
                        {
                            let error = self
                                .fail_malformed_stream(
                                    &effect_id,
                                    format!(
                                        "model round exceeded {} tool calls",
                                        self.config.max_tool_calls_per_round
                                    ),
                                )
                                .await?;
                            return Err(error);
                        }
                        if let Err(message) = validate_tool_fragment(
                            id.as_deref(),
                            name.as_deref(),
                            arguments.as_deref(),
                            self.config.max_tool_argument_chars,
                        ) {
                            let error = self.fail_malformed_stream(&effect_id, message).await?;
                            return Err(error);
                        }
                        let partial = partial_calls.entry(index).or_default();
                        partial.merge(id, name, arguments);
                        if partial.argument_chars > self.config.max_tool_argument_chars {
                            let error = self
                                .fail_malformed_stream(
                                    &effect_id,
                                    format!(
                                        "tool call arguments exceeded {} characters",
                                        self.config.max_tool_argument_chars
                                    ),
                                )
                                .await?;
                            return Err(error);
                        }
                    }
                    Ok(Some(Ok(ModelChunk::Finished { .. }))) => {
                        chunk_count = chunk_count.saturating_add(1);
                        if chunk_count > self.config.max_model_chunks_per_round {
                            let error = self
                                .fail_malformed_stream(
                                    &effect_id,
                                    format!(
                                        "model round exceeded {} streamed chunks",
                                        self.config.max_model_chunks_per_round
                                    ),
                                )
                                .await?;
                            return Err(error);
                        }
                    }
                    Ok(Some(Err(error))) => {
                        self.complete_effect(
                            &effect_id,
                            "failed",
                            json!({"error": error.to_string()}),
                        )
                        .await?;
                        return Err(error.into());
                    }
                    Ok(None) => break,
                    Err(_) => {
                        self.complete_effect(
                            &effect_id,
                            "failed",
                            json!({"error": "provider stream idle timeout"}),
                        )
                        .await?;
                        return Err(RuntimeError::Provider(crate::ProviderError::new(
                            ProviderErrorKind::Network,
                            "provider stream idle timeout",
                            true,
                        )));
                    }
                }
            }
            let tool_call_count = partial_calls.len();
            let calls = match partial_calls
                .into_iter()
                .map(|(index, call)| call.finish(index))
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(calls) => calls,
                Err(error) => {
                    self.complete_effect(&effect_id, "failed", json!({"error": error.to_string()}))
                        .await?;
                    return Err(error);
                }
            };
            if calls.is_empty() && text.trim().is_empty() {
                self.complete_effect(
                    &effect_id,
                    "failed",
                    json!({"error": "model returned neither visible text nor a tool call"}),
                )
                .await?;
                return Err(RuntimeError::EmptyModelTurn);
            }
            self.complete_effect(
                &effect_id,
                "succeeded",
                json!({"visible_chars": visible_chars, "tool_calls": tool_call_count}),
            )
            .await?;
            if !calls.is_empty() {
                state.phase = AgentPhase::AwaitingTools;
                messages.push(Message {
                    role: MessageRole::Assistant,
                    content: text,
                    tool_calls: calls.clone(),
                    tool_call_id: None,
                });
                let tool_messages = self
                    .execute_tools(state, calls, &mut evidence_contexts)
                    .await?;
                messages.extend(tool_messages);
                continue;
            }

            if !self.config.verify_reports {
                return self.complete_run(state, text).await;
            }

            state.phase = AgentPhase::Verifying;
            self.record(state, AgentEvent::VerificationStarted).await?;
            let verification_spec = task.verification_spec();
            let verification_context = json!({
                "task_spec": verification_spec,
                "tool_results": evidence_contexts,
            });
            let verification_effect = self
                .begin_effect(
                    state,
                    "report.verify",
                    json!({"report_chars": text.chars().count()}),
                )
                .await?;
            let verification = tokio::time::timeout(
                self.config.verification_timeout,
                self.executor.execute(
                    "research.agent_report_verify",
                    json!({
                        "report": text,
                        "context": verification_context,
                        "task_spec": verification_spec,
                    }),
                    state.cancellation.child_token(),
                ),
            )
            .await;
            let verification = match verification {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    self.complete_effect(&verification_effect, "failed", json!({"error": error}))
                        .await?;
                    return Err(RuntimeError::VerificationFailed(error));
                }
                Err(_) => {
                    self.complete_effect(
                        &verification_effect,
                        "failed",
                        json!({"error": "verification timeout"}),
                    )
                    .await?;
                    return Err(RuntimeError::VerificationFailed(
                        "deterministic verification timed out".into(),
                    ));
                }
            };
            self.complete_effect(&verification_effect, "succeeded", verification.clone())
                .await?;
            if verification.get("passed").and_then(Value::as_bool) == Some(true) {
                return self.complete_run(state, text).await;
            }

            let findings = verification
                .get("findings")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect::<Vec<_>>();
            for code in &findings {
                self.record(
                    state,
                    AgentEvent::VerificationFinding {
                        finding: VerificationFinding {
                            code: code.clone(),
                            message: format!("确定性报告校验未通过：{code}"),
                            blocking: true,
                        },
                    },
                )
                .await?;
            }
            if revision_count >= self.config.max_verification_revisions {
                return Err(RuntimeError::VerificationFailed(findings.join(", ")));
            }
            revision_count += 1;
            state.phase = AgentPhase::Reviewing;
            messages.push(Message::text(MessageRole::Assistant, text));
            messages.push(Message::text(
                MessageRole::System,
                format!(
                    "独立确定性校验阻止发布。仅依据已有工具证据修订报告，不得发明数字。修复以下问题后重新提交完整报告：{}",
                    findings.join("；")
                ),
            ));
        }
        Err(RuntimeError::ModelRoundLimit(self.config.max_model_rounds))
    }

    async fn execute_tools(
        &self,
        state: &mut RunState,
        calls: Vec<ModelToolCall>,
        evidence_contexts: &mut Vec<Value>,
    ) -> Result<Vec<Message>, RuntimeError> {
        let mut prepared = Vec::with_capacity(calls.len());
        for call in calls {
            let definition = self
                .tools
                .get(&call.name)
                .cloned()
                .ok_or_else(|| RuntimeError::UnknownTool(call.name.clone()))?;
            let arguments: Value = serde_json::from_str(&call.arguments).map_err(|error| {
                RuntimeError::InvalidToolArguments {
                    tool: call.name.clone(),
                    message: error.to_string(),
                }
            })?;
            if !arguments.is_object() {
                return Err(RuntimeError::InvalidToolArguments {
                    tool: call.name,
                    message: "tool arguments must be a JSON object".into(),
                });
            }
            self.record(
                state,
                AgentEvent::ToolScheduled {
                    call_id: call.id.clone(),
                    tool: call.name.clone(),
                },
            )
            .await?;
            let effect_id = self
                .begin_effect(
                    state,
                    &format!("tool.{}", call.name),
                    json!({"call_id": call.id, "arguments": arguments}),
                )
                .await?;
            self.record(
                state,
                AgentEvent::ToolStarted {
                    call_id: call.id.clone(),
                    tool: call.name.clone(),
                },
            )
            .await?;
            prepared.push(PreparedTool {
                call,
                definition,
                arguments,
                effect_id,
            });
        }

        let executor = self.executor.clone();
        let cancellation = state.cancellation.clone();
        let max_parallel = self.config.max_parallel_tools.max(1);
        let mut executions = futures::stream::iter(prepared)
            .map(move |prepared| {
                let executor = executor.clone();
                let cancellation = cancellation.child_token();
                async move {
                    let result = tokio::time::timeout(
                        prepared.definition.timeout,
                        executor.execute(
                            &prepared.definition.engine_kind,
                            prepared.arguments.clone(),
                            cancellation,
                        ),
                    )
                    .await;
                    (prepared, result)
                }
            })
            .buffer_unordered(max_parallel);

        let mut completed = Vec::new();
        while let Some((prepared, result)) = executions.next().await {
            self.ensure_active(state)?;
            match result {
                Ok(Ok(value)) => {
                    let encoded = serde_json::to_vec(&value)?;
                    if encoded.len() > self.config.max_tool_result_bytes {
                        let message = RuntimeError::ToolResultTooLarge {
                            tool: prepared.call.name.clone(),
                            actual: encoded.len(),
                            maximum: self.config.max_tool_result_bytes,
                        }
                        .to_string();
                        self.complete_effect(
                            &prepared.effect_id,
                            "failed",
                            json!({"error": message}),
                        )
                        .await?;
                        self.record(
                            state,
                            AgentEvent::ToolFailed {
                                call_id: prepared.call.id.clone(),
                                tool: prepared.call.name.clone(),
                                message: message.clone(),
                                retryable: false,
                            },
                        )
                        .await?;
                        completed.push(CompletedTool::failure(prepared.call, message));
                        continue;
                    }
                    let evidence_ids = evidence_ids(&value);
                    self.complete_effect(&prepared.effect_id, "succeeded", value.clone())
                        .await?;
                    self.record(
                        state,
                        AgentEvent::ToolCompleted {
                            call_id: prepared.call.id.clone(),
                            tool: prepared.call.name.clone(),
                            evidence_ids: evidence_ids.clone(),
                        },
                    )
                    .await?;
                    if !evidence_ids.is_empty() {
                        self.record(
                            state,
                            AgentEvent::EvidenceAdded {
                                call_id: prepared.call.id.clone(),
                                evidence_ids: evidence_ids.clone(),
                            },
                        )
                        .await?;
                    }
                    state.completed_tool_ids.push(prepared.call.id.clone());
                    state.evidence_ids.extend(evidence_ids.iter().cloned());
                    state.evidence_ids.sort();
                    state.evidence_ids.dedup();
                    evidence_contexts.push(value.clone());
                    completed.push(CompletedTool::success(prepared.call, value, evidence_ids));
                }
                Ok(Err(message)) => {
                    self.complete_effect(&prepared.effect_id, "failed", json!({"error": message}))
                        .await?;
                    self.record(
                        state,
                        AgentEvent::ToolFailed {
                            call_id: prepared.call.id.clone(),
                            tool: prepared.call.name.clone(),
                            message: message.clone(),
                            retryable: false,
                        },
                    )
                    .await?;
                    completed.push(CompletedTool::failure(prepared.call, message));
                }
                Err(_) => {
                    let message = format!("tool timed out after {:?}", prepared.definition.timeout);
                    self.complete_effect(&prepared.effect_id, "failed", json!({"error": message}))
                        .await?;
                    self.record(
                        state,
                        AgentEvent::ToolFailed {
                            call_id: prepared.call.id.clone(),
                            tool: prepared.call.name.clone(),
                            message: message.clone(),
                            retryable: true,
                        },
                    )
                    .await?;
                    completed.push(CompletedTool::failure(prepared.call, message));
                }
            }
        }
        completed.sort_by_key(|item| item.index);
        completed
            .into_iter()
            .map(CompletedTool::into_message)
            .collect()
    }

    async fn complete_run(
        &self,
        state: &mut RunState,
        report: String,
    ) -> Result<RunOutcome, RuntimeError> {
        state.phase = AgentPhase::Completed;
        let evidence_ids = state.evidence_ids.clone();
        self.record(
            state,
            AgentEvent::Completed {
                report: report.clone(),
                evidence_ids: evidence_ids.clone(),
            },
        )
        .await?;
        Ok(RunOutcome {
            task_id: state.task_id.clone(),
            report,
            evidence_ids,
        })
    }

    async fn record(&self, state: &mut RunState, event: AgentEvent) -> Result<(), RuntimeError> {
        state.seq = state.seq.saturating_add(1);
        self.store
            .append_event(&state.task_id, state.seq, &event)
            .await
            .map_err(RuntimeError::Store)?;
        let checkpoint = StoredCheckpoint {
            task_id: state.task_id.clone(),
            phase: state.phase,
            accepted_seq: state.seq,
            model_round: state.model_round,
            completed_tool_ids: state.completed_tool_ids.clone(),
            evidence_ids: state.evidence_ids.clone(),
            state_version: "rust-agent-runtime-v1".into(),
        };
        self.store
            .put_checkpoint(&checkpoint)
            .await
            .map_err(RuntimeError::Store)?;
        state
            .sender
            .send(event)
            .await
            .map_err(|_| RuntimeError::Cancelled)
    }

    async fn begin_effect(
        &self,
        state: &RunState,
        kind: &str,
        payload: Value,
    ) -> Result<String, RuntimeError> {
        let effect_id = Uuid::new_v4().to_string();
        self.store
            .begin_effect(&EffectIntent {
                effect_id: effect_id.clone(),
                task_id: state.task_id.clone(),
                caused_by_seq: state.seq,
                effect_kind: kind.into(),
                idempotency_key: format!("{}:{}:{}", state.task_id, state.seq, kind),
                payload,
            })
            .await
            .map_err(RuntimeError::Store)?;
        Ok(effect_id)
    }

    async fn complete_effect(
        &self,
        effect_id: &str,
        status: &str,
        result: Value,
    ) -> Result<(), RuntimeError> {
        self.store
            .complete_effect(effect_id, status, &result)
            .await
            .map_err(RuntimeError::Store)
    }

    async fn fail_malformed_stream(
        &self,
        effect_id: &str,
        message: String,
    ) -> Result<RuntimeError, RuntimeError> {
        self.complete_effect(effect_id, "failed", json!({"error": &message}))
            .await?;
        Ok(malformed_provider(message))
    }

    fn ensure_active(&self, state: &RunState) -> Result<(), RuntimeError> {
        if state.cancellation.is_cancelled() {
            Err(RuntimeError::Cancelled)
        } else {
            Ok(())
        }
    }
}

fn apply_session_event(session: &mut RuntimeSession, event: &AgentEvent) {
    let mut completed_report = None;
    {
        let Some(task) = session.task.as_mut() else {
            return;
        };
        task.accepted_seq = task.accepted_seq.saturating_add(1);
        match event {
            AgentEvent::SessionStarted { .. } | AgentEvent::UserMessageAccepted => {
                task.phase = AgentPhase::Preparing;
            }
            AgentEvent::PlanningStarted | AgentEvent::PlanUpdated { .. } => {
                task.phase = AgentPhase::Planning;
            }
            // A structured plan revision may happen at any point during
            // research, so it records the plan without rewinding the phase.
            // Treating it as `Planning` would misreport an Agent that revised
            // its plan mid-execution as having gone back to planning.
            AgentEvent::PlanRevised { plan, .. } => {
                task.plan = Some(plan.clone());
            }
            // Waiting for a materially necessary user decision is a suspension
            // of autonomous progress, not a failure.
            AgentEvent::ClarificationRequested { .. } => {
                task.phase = AgentPhase::Suspended;
            }
            AgentEvent::ClarificationResolved { .. } => {
                task.phase = AgentPhase::Reasoning;
            }
            AgentEvent::ModelStarted { round, .. } => {
                task.phase = AgentPhase::Reasoning;
                task.model_round = *round;
            }
            AgentEvent::ToolScheduled { .. } | AgentEvent::ToolStarted { .. } => {
                task.phase = AgentPhase::AwaitingTools;
            }
            AgentEvent::ToolCompleted {
                call_id,
                evidence_ids,
                ..
            } => {
                task.phase = AgentPhase::AwaitingTools;
                if !task.completed_tool_ids.contains(call_id) {
                    task.completed_tool_ids.push(call_id.clone());
                }
                task.evidence_ids.extend(evidence_ids.iter().cloned());
                task.evidence_ids.sort();
                task.evidence_ids.dedup();
            }
            AgentEvent::ToolFailed { .. } | AgentEvent::EvidenceAdded { .. } => {
                task.phase = AgentPhase::AwaitingTools;
            }
            AgentEvent::VerificationStarted | AgentEvent::VerificationFinding { .. } => {
                task.phase = AgentPhase::Verifying;
            }
            AgentEvent::TextDelta { .. } => {}
            AgentEvent::Suspended { .. } => task.phase = AgentPhase::Suspended,
            AgentEvent::Cancelled => task.phase = AgentPhase::Cancelled,
            AgentEvent::Completed {
                report,
                evidence_ids,
            } => {
                task.phase = AgentPhase::Completed;
                task.evidence_ids = evidence_ids.clone();
                completed_report = Some(report.clone());
            }
            AgentEvent::Failed { .. } => task.phase = AgentPhase::Failed,
        }
    }
    if let Some(report) = completed_report {
        session.push_message(SessionMessageRole::Agent, report);
    }
    session.updated_at = chrono::Utc::now().timestamp_millis();
}

#[derive(Debug, Clone)]
pub struct SessionRunOutcome {
    pub run: RunOutcome,
    pub session: RuntimeSession,
}

pub struct SessionTaskStream {
    task_id: String,
    session_id: String,
    receiver: mpsc::Receiver<AgentEvent>,
    cancellation: CancellationToken,
    join: Option<JoinHandle<Result<SessionRunOutcome, RuntimeError>>>,
}

impl SessionTaskStream {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub async fn recv(&mut self) -> Option<AgentEvent> {
        self.receiver.recv().await
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Clone the cooperative cancellation handle.
    ///
    /// An adapter that hands the stream to a relay task still needs to cancel
    /// it from a separate request, which is how the desktop's cancel command and
    /// the terminal's `/cancel` reach the *same* cancellation path rather than
    /// growing two different ones.
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub async fn finish(mut self) -> Result<SessionRunOutcome, RuntimeError> {
        while self.receiver.recv().await.is_some() {}
        let join = self
            .join
            .take()
            .ok_or_else(|| RuntimeError::Internal("session task was already joined".into()))?;
        join.await
            .map_err(|error| RuntimeError::Internal(format!("session task join failed: {error}")))?
    }
}

impl Drop for SessionTaskStream {
    fn drop(&mut self) {
        if !self.receiver.is_closed() {
            self.cancellation.cancel();
        }
    }
}

pub struct TaskStream {
    task_id: String,
    receiver: mpsc::Receiver<AgentEvent>,
    cancellation: CancellationToken,
    join: Option<JoinHandle<Result<RunOutcome, RuntimeError>>>,
}

impl TaskStream {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub async fn recv(&mut self) -> Option<AgentEvent> {
        self.receiver.recv().await
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub async fn finish(mut self) -> Result<RunOutcome, RuntimeError> {
        while self.receiver.recv().await.is_some() {}
        let join = self
            .join
            .take()
            .ok_or_else(|| RuntimeError::Internal("task was already joined".into()))?;
        join.await
            .map_err(|error| RuntimeError::Internal(format!("task join failed: {error}")))?
    }
}

impl Drop for TaskStream {
    fn drop(&mut self) {
        if !self.receiver.is_closed() {
            self.cancellation.cancel();
        }
    }
}

struct RunState {
    task_id: String,
    seq: u64,
    phase: AgentPhase,
    model_round: usize,
    completed_tool_ids: Vec<String>,
    evidence_ids: Vec<String>,
    sender: mpsc::Sender<AgentEvent>,
    cancellation: CancellationToken,
}

impl RunState {
    fn new(
        task_id: String,
        sender: mpsc::Sender<AgentEvent>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            task_id,
            seq: 0,
            phase: AgentPhase::Preparing,
            model_round: 0,
            completed_tool_ids: Vec::new(),
            evidence_ids: Vec::new(),
            sender,
            cancellation,
        }
    }
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
    argument_chars: usize,
}

impl PartialToolCall {
    fn merge(&mut self, id: Option<String>, name: Option<String>, arguments: Option<String>) {
        merge_fragment(&mut self.id, id);
        merge_fragment(&mut self.name, name);
        merge_counted_fragment(&mut self.arguments, &mut self.argument_chars, arguments);
    }

    fn finish(self, index: u32) -> Result<ModelToolCall, RuntimeError> {
        if self.id.is_empty() || self.name.is_empty() || self.arguments.is_empty() {
            return Err(RuntimeError::Provider(crate::ProviderError::new(
                ProviderErrorKind::MalformedResponse,
                format!("incomplete tool call at streaming index {index}"),
                false,
            )));
        }
        Ok(ModelToolCall {
            id: self.id,
            name: self.name,
            arguments: self.arguments,
            index,
        })
    }
}

fn merge_fragment(target: &mut String, fragment: Option<String>) {
    let Some(fragment) = fragment.filter(|value| !value.is_empty()) else {
        return;
    };
    if target.is_empty() || fragment.starts_with(target.as_str()) {
        *target = fragment;
    } else if !target.ends_with(&fragment) {
        target.push_str(&fragment);
    }
}

fn merge_counted_fragment(
    target: &mut String,
    character_count: &mut usize,
    fragment: Option<String>,
) {
    let Some(fragment) = fragment.filter(|value| !value.is_empty()) else {
        return;
    };
    if target.is_empty() || fragment.starts_with(target.as_str()) {
        *character_count = fragment.chars().count();
        *target = fragment;
    } else if !target.ends_with(&fragment) {
        *character_count = character_count.saturating_add(fragment.chars().count());
        target.push_str(&fragment);
    }
}

fn validate_tool_fragment(
    id: Option<&str>,
    name: Option<&str>,
    arguments: Option<&str>,
    max_argument_chars: usize,
) -> Result<(), String> {
    if id.is_some_and(|value| value.len() > 128 || value.chars().any(char::is_control)) {
        return Err("tool call ID exceeds 128 visible bytes".into());
    }
    if name.is_some_and(|value| value.len() > 128 || value.chars().any(char::is_control)) {
        return Err("tool call name exceeds 128 visible bytes".into());
    }
    if arguments.is_some_and(|value| value.chars().count() > max_argument_chars) {
        return Err(format!(
            "tool call argument fragment exceeded {max_argument_chars} characters"
        ));
    }
    Ok(())
}

fn malformed_provider(message: String) -> RuntimeError {
    RuntimeError::Provider(crate::ProviderError::new(
        ProviderErrorKind::MalformedResponse,
        message,
        false,
    ))
}

struct PreparedTool {
    call: ModelToolCall,
    definition: ToolDefinition,
    arguments: Value,
    effect_id: String,
}

struct CompletedTool {
    index: u32,
    call_id: String,
    content: Value,
}

impl CompletedTool {
    fn success(call: ModelToolCall, value: Value, evidence_ids: Vec<String>) -> Self {
        Self::success_bounded(
            call,
            value,
            evidence_ids,
            DEFAULT_MAX_TOOL_RESULT_MODEL_CHARS,
            DEFAULT_MAX_EVIDENCE_IDS_IN_CONTEXT,
        )
    }

    /// Build the model-facing projection of a tool result.
    ///
    /// What is persisted and what the model sees are deliberately different
    /// sizes. The full result is already written to the effect record and its
    /// observations are registered in the evidence store, so the model does not
    /// need the whole payload — it needs enough to reason plus the evidence IDs
    /// to cite.
    ///
    /// Sending the full value was a real production failure: three research
    /// tools returning close to the 2 MiB per-result ceiling produced a model
    /// request that MiniMax rejected with `context window exceeds limit`, which
    /// killed an otherwise successful multi-round task after 13 tool calls. A
    /// 2 MiB allowance is meaningful for durable storage and meaningless for a
    /// context window, so the two bounds are now separate.
    ///
    /// Truncation is explicit rather than silent: the model is told the payload
    /// was bounded and that the complete result is retrievable by evidence ID,
    /// so it cannot mistake a truncated page for the whole dataset.
    fn success_bounded(
        call: ModelToolCall,
        value: Value,
        evidence_ids: Vec<String>,
        max_chars: usize,
        max_evidence_ids: usize,
    ) -> Self {
        let total_evidence = evidence_ids.len();
        let (shown_ids, omitted_ids) = if total_evidence > max_evidence_ids {
            (
                &evidence_ids[..max_evidence_ids],
                total_evidence - max_evidence_ids,
            )
        } else {
            (&evidence_ids[..], 0)
        };

        let encoded = value.to_string();
        let content = if encoded.chars().count() <= max_chars {
            json!({
                "ok": true,
                "data": value,
                "evidence_ids": shown_ids,
            })
        } else {
            let kept: String = encoded.chars().take(max_chars).collect();
            json!({
                "ok": true,
                "truncated": true,
                "data_preview": kept,
                "original_chars": encoded.chars().count(),
                "preview_chars": max_chars,
                "evidence_ids": shown_ids,
                "note": "结果超过模型上下文预算，已按前缀截断。完整结果已持久化在证据库中，可通过 evidence_ids 引用；不要把此预览当作完整数据集。",
            })
        };

        let mut content = content;
        if omitted_ids > 0 {
            content["omitted_evidence_ids"] = json!(omitted_ids);
            content["evidence_note"] =
                json!("证据标识过多，仅列出前若干条；其余已记录在任务状态中，可按需引用。");
        }

        Self {
            index: call.index,
            call_id: call.id,
            content,
        }
    }

    fn failure(call: ModelToolCall, message: String) -> Self {
        Self {
            index: call.index,
            call_id: call.id,
            content: json!({"ok": false, "error": message}),
        }
    }

    fn into_message(self) -> Result<Message, RuntimeError> {
        Ok(Message::tool_result(
            self.call_id,
            serde_json::to_string(&self.content)?,
        ))
    }
}

fn evidence_ids(value: &Value) -> Vec<String> {
    fn collect(value: &Value, ids: &mut BTreeSet<String>) {
        match value {
            Value::Object(object) => {
                if let Some(facts) = object
                    .get("evidence_registry")
                    .and_then(|registry| registry.get("facts"))
                    .and_then(Value::as_array)
                {
                    for fact in facts {
                        if let Some(id) = fact.get("evidence_id").and_then(Value::as_str) {
                            ids.insert(id.to_owned());
                        }
                    }
                }
                for (key, child) in object {
                    if key != "evidence_registry" {
                        collect(child, ids);
                    }
                }
            }
            Value::Array(items) => {
                for child in items {
                    collect(child, ids);
                }
            }
            _ => {}
        }
    }
    let mut ids = BTreeSet::new();
    collect(value, &mut ids);
    ids.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_call() -> ModelToolCall {
        ModelToolCall {
            index: 0,
            id: "call-1".into(),
            name: "research_securities".into(),
            arguments: String::new(),
        }
    }

    /// A tool result that fits the context budget is passed through unchanged.
    #[test]
    fn a_small_tool_result_reaches_the_model_intact() {
        let value = json!({"symbol": "601899", "close": 18.42});
        let completed = CompletedTool::success_bounded(
            tool_call(),
            value.clone(),
            vec!["evf_1".into()],
            24_000,
            40,
        );
        assert_eq!(completed.content["ok"], json!(true));
        assert_eq!(completed.content["data"], value);
        assert!(completed.content.get("truncated").is_none());
    }

    /// An oversized result is bounded before it reaches the model, and says so.
    ///
    /// This is the failure live acceptance found: three research tools each
    /// returning close to the 2 MiB storage ceiling produced a request MiniMax
    /// rejected with `context window exceeds limit`, ending a task that had
    /// already completed 13 tool calls. The storage bound and the context bound
    /// must be different numbers.
    #[test]
    fn an_oversized_tool_result_is_bounded_for_the_model_and_marked_truncated() {
        let big = json!({"rows": vec!["0123456789"; 5_000]});
        let original_chars = big.to_string().chars().count();
        assert!(
            original_chars > 24_000,
            "the fixture must exceed the budget"
        );

        let completed =
            CompletedTool::success_bounded(tool_call(), big, vec!["evf_1".into()], 24_000, 40);

        assert_eq!(completed.content["truncated"], json!(true));
        assert!(
            completed.content.get("data").is_none(),
            "the unbounded payload must not be sent"
        );
        let preview = completed.content["data_preview"]
            .as_str()
            .expect("a bounded preview is provided");
        assert_eq!(preview.chars().count(), 24_000);
        assert_eq!(completed.content["original_chars"], json!(original_chars));

        // The model must be told this is a page, not the dataset, or it could
        // report a truncated view as complete coverage.
        let note = completed.content["note"].as_str().unwrap_or_default();
        assert!(
            note.contains("截断"),
            "the note must state it was truncated"
        );
        assert!(
            note.contains("证据库"),
            "the note must point at the durable evidence record"
        );

        // Evidence identifiers survive truncation, otherwise the model could not
        // cite the observations it just obtained.
        assert_eq!(completed.content["evidence_ids"], json!(["evf_1"]));
    }

    /// Thousands of evidence identifiers must not flood the context.
    #[test]
    fn evidence_identifiers_are_capped_and_the_omission_is_reported() {
        let ids: Vec<String> = (0..3_000).map(|n| format!("evf_{n}")).collect();
        let completed =
            CompletedTool::success_bounded(tool_call(), json!({"ok": true}), ids, 24_000, 40);

        let shown = completed.content["evidence_ids"]
            .as_array()
            .expect("identifiers are listed");
        assert_eq!(shown.len(), 40, "the listed identifiers must be capped");
        assert_eq!(
            completed.content["omitted_evidence_ids"],
            json!(2_960),
            "the omission must be counted rather than hidden"
        );
        assert!(completed.content["evidence_note"].is_string());
    }

    /// A failed tool keeps its typed error and gains no payload.
    #[test]
    fn a_failed_tool_reports_only_its_error() {
        let completed = CompletedTool::failure(tool_call(), "upstream 429".into());
        assert_eq!(completed.content["ok"], json!(false));
        assert_eq!(completed.content["error"], json!("upstream 429"));
        assert!(completed.content.get("data").is_none());
    }

    /// The context budget must stay far below the storage ceiling.
    #[test]
    fn the_context_budget_is_much_smaller_than_the_storage_ceiling() {
        let storage = RuntimeConfig::default().max_tool_result_bytes;
        assert!(
            DEFAULT_MAX_TOOL_RESULT_MODEL_CHARS * 8 < storage,
            "a context budget close to the storage ceiling reintroduces the overflow"
        );
    }

    #[test]
    fn task_optional_fields_are_omitted_not_encoded_as_null() {
        let value = serde_json::to_value(RuntimeTask::ask("分析市场")).unwrap();
        assert!(value.get("symbol").is_none());
        assert!(value.get("capital").is_none());
    }

    #[test]
    fn fragmented_tool_arguments_reconstruct_one_json_object() {
        let mut partial = PartialToolCall::default();
        partial.merge(
            Some("call-1".into()),
            Some("get_quote".into()),
            Some("{\"sym".into()),
        );
        partial.merge(None, None, Some("bol\":\"601899\"}".into()));
        let call = partial.finish(0).unwrap();
        assert_eq!(call.arguments, "{\"symbol\":\"601899\"}");
    }
}
