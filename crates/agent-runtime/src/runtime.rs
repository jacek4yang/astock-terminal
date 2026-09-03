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

use crate::catalog::{EvidenceCatalog, EvidenceSearchRequest};
use crate::error::{ProviderErrorKind, RuntimeError};
use crate::events::{AgentEvent, AgentPhase, VerificationFinding};
use crate::finalize::{
    fingerprint, validation_repair, verification_repair, FinalizationLedger, RepairVerdict,
};
use crate::model::{Message, MessageRole, ModelChunk, ModelProvider, ModelRequest, ModelToolCall};
use crate::prompt;
use crate::render::{render, RenderedReport};
use crate::report::{validate_draft, VerifiedReportDraft};
use crate::session::{
    RuntimeSession, SessionMessageRole, SessionTaskState, MAX_MODEL_HISTORY_CHARS,
    MAX_MODEL_HISTORY_MESSAGES,
};
use crate::store::{AgentStore, EffectIntent, StoredCheckpoint};
use crate::tools::{default_registry, ToolDefinition, ToolExecutor, ToolHandler, ToolRegistry};

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
    /// Hard ceiling on model rounds for the whole task.
    ///
    /// Must accommodate both phases: `max_research_rounds` of retrieval plus roughly
    /// one round per finalization attempt, plus slack. A measured moderate run used
    /// 18 rounds of genuine tool work and then had only 6 rounds left, so the
    /// ceiling bound before the finalization budget did and the task died on the
    /// round limit while repair was still progressing. Raising this is not a way to
    /// paper over a failing Agent; it is what keeps the phase budgets meaningful
    /// rather than being silently pre-empted by a shared ceiling.
    pub max_model_rounds: usize,
    /// Rounds during which the model may still gather evidence.
    ///
    /// Separate from `max_model_rounds` because one budget cannot express "research
    /// widely, then finalize". Once these are spent the runtime tells the model to
    /// finalize with what it has, which is a bounded, honest report rather than a
    /// task that hits the round ceiling with nothing published.
    pub max_research_rounds: usize,
    /// Total `submit_report` attempts, counting both contract rejections and
    /// verifier refusals. Exhausting them fails closed; it never publishes.
    ///
    /// Eight, because the measured convergence needs it. Across live moderate runs the
    /// finding count per submission falls 40 → 16 → 2, so a report typically needs four
    /// or five submissions to become publishable and a sixth to finish. A budget set at
    /// the edge of the measured trajectory turns a converging repair loop into a
    /// failure and throws away the research that preceded it.
    ///
    /// This is not a blind increase: a loop that is *not* converging is caught by
    /// identical-resubmission detection, which ends finalization on the second
    /// unchanged draft regardless of how much budget remains.
    pub max_finalization_attempts: usize,
    pub max_model_chunks_per_round: usize,
    /// Calls to tools that do not exist, before the task fails closed.
    ///
    /// Such a call is never executed, so the capability boundary holds regardless.
    /// This bounds how many times the model may misremember a name before the task
    /// is judged to be making no progress.
    pub max_unknown_tool_rejections: usize,
    /// Safe replays of a provider turn that returned nothing at all.
    ///
    /// An empty turn commits nothing — no text reached the user and no tool call was
    /// assembled — so re-requesting the identical round cannot duplicate an effect.
    /// Counted separately from `max_model_rounds`, because a provider hiccup should
    /// not consume the task's reasoning budget.
    pub max_empty_turn_retries: usize,
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
    pub verify_reports: bool,
    pub event_channel_capacity: usize,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_model_rounds: 32,
            max_research_rounds: 20,
            // Finalization repair attempts. Measured live trajectories run
            // 49 → 22 → 14 → 2 → publish-class within six to eight repairs
            // when the first draft starts clean, but a draft that opens with a
            // large undeclared-figure count needs the tail rounds too — one
            // live run converged to one problem at attempt 7 and exhausted at
            // 8. Identical-resubmission detection still ends a non-converging
            // loop regardless of budget.
            max_finalization_attempts: 10,
            max_model_chunks_per_round: 10_000,
            max_unknown_tool_rejections: 3,
            max_empty_turn_retries: 2,
            max_visible_chars_per_round: 120_000,
            max_tool_calls_per_round: 32,
            max_tool_argument_chars: 256_000,
            max_parallel_tools: 4,
            max_tool_result_bytes: 2 * 1024 * 1024,
            // Output ceiling per model round. A structured report draft is
            // emitted as one tool-call payload, and the principal model also
            // spends private reasoning from the same budget: a live balanced
            // run wrote 9-claim drafts that cut off at ~14 KB of arguments
            // under the previous 8,192 cap, losing required fields, and burned
            // the whole finalization budget on the truncation. 32,768 leaves
            // room for reasoning plus a full deep draft; shorter generations
            // are billed the same as before, and the chunk/character bounds
            // below still cap a runaway round.
            max_tokens: 32_768,
            temperature: 0.2,
            provider_connect_timeout: Duration::from_secs(90),
            provider_idle_timeout: Duration::from_secs(120),
            verification_timeout: Duration::from_secs(30),
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
                resume_at: None,
                suspended_by: None,
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
        let mut state = RunState::new(
            task_id,
            sender,
            cancellation,
            self.config.max_finalization_attempts,
        );
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
                    // Record *when* the task may resume, not just that it stopped.
                    //
                    // A suspension used to carry prose only, so nothing knew when to try
                    // again and a user had to restart deep research by hand once the
                    // window reopened. A live run suspended with 123 minutes to go and
                    // no record of it.
                    let fault = crate::fault::ModelFault::classify(provider);
                    let action = crate::fault::plan(
                        &fault,
                        &crate::fault::AttemptBudget::new(1),
                        chrono::Utc::now(),
                    );
                    let resume_at = match &action {
                        crate::fault::RecoveryAction::SuspendUntil { resume_at, .. } => {
                            Some(resume_at.to_rfc3339())
                        }
                        _ => None,
                    };
                    state.phase = AgentPhase::Suspended;
                    AgentEvent::Suspended {
                        reason: provider.to_string(),
                        resume_at,
                        fault: Some(fault.as_str().to_owned()),
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

            // One round, with bounded safe replay of an empty provider turn.
            //
            // A live simple-price task died on `model returned neither visible text
            // nor a tool call` after ten successful tool calls. That turn committed
            // nothing: no text was streamed to the user and no tool call was
            // assembled, so re-issuing the identical request cannot repeat an
            // effect. Each attempt opens a fresh connection, so a replay is also a
            // reconnect. A provider *error* frame is not an empty turn — it is
            // returned as a typed provider error before reaching this point — so a
            // real fault is never silently retried as emptiness.
            // Once the research budget is spent, retrieval tools are withdrawn.
            //
            // The budget used to be a request in a system message, and a model that
            // kept researching simply ignored it: two live moderate runs reached the
            // 32-round ceiling having never submitted a report at all, one after 26
            // tool calls. A budget the model can decline is not a budget.
            //
            // Withdrawing the tools makes it a mechanism. `search_evidence` stays,
            // because finalization needs identifiers, and the deterministic
            // calculation tool stays, because a figure may still need to be computed
            // to be citable — neither reaches an upstream. The one-time prompt-cache
            // invalidation at the phase boundary is worth a task that finishes.
            let finalization_only = round
                > self
                    .config
                    .max_research_rounds
                    .min(self.config.max_model_rounds);
            let calc_withdrawn = state.calc_shape_failures >= MAX_CALC_SHAPE_FAILURES;
            let offered_tools = if finalization_only {
                self.tools
                    .definitions()
                    .into_iter()
                    .filter(|tool| {
                        tool.handler == ToolHandler::Runtime
                            || (!calc_withdrawn && tool.name == "run_financial_calculation")
                    })
                    .collect()
            } else if calc_withdrawn {
                self.tools
                    .definitions()
                    .into_iter()
                    .filter(|tool| !tool.name.contains("calculation"))
                    .collect()
            } else {
                self.tools.definitions()
            };
            let mut empty_turns = 0usize;
            let (text, calls) = loop {
                let request = ModelRequest {
                    model: selected_model.clone(),
                    messages: messages.clone(),
                    tools: offered_tools.clone(),
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
                        self.complete_effect(
                            &effect_id,
                            "failed",
                            json!({"error": error.to_string()}),
                        )
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
                        result = async {
                            if self.provider.manages_stream_liveness() {
                                // MiniMax resets its watchdog on every raw SSE
                                // chunk, including private reasoning, and owns its
                                // bounded pre-commit reconnects. The adapter hides
                                // those chunks correctly; timing only its visible
                                // output here caused false idles and multiplied the
                                // provider retry budget.
                                Ok(stream.next().await)
                            } else {
                                tokio::time::timeout(
                                    self.config.provider_idle_timeout,
                                    stream.next(),
                                )
                                .await
                            }
                        } => result,
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
                        self.complete_effect(
                            &effect_id,
                            "failed",
                            json!({"error": error.to_string()}),
                        )
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
                    empty_turns = empty_turns.saturating_add(1);
                    let exhausted = empty_turns > self.config.max_empty_turn_retries;
                    self.record(
                        state,
                        AgentEvent::ModelTurnEmpty {
                            round,
                            attempt: empty_turns,
                            action: if exhausted {
                                "exhausted".into()
                            } else if empty_turns == 1 {
                                "replay".into()
                            } else {
                                "replay_with_instruction".into()
                            },
                        },
                    )
                    .await?;
                    if exhausted {
                        return Err(RuntimeError::EmptyModelTurn);
                    }
                    if empty_turns > 1 {
                        // An identical replay already failed once, so the emptiness is
                        // unlikely to be a transient stream fault. Name the two things
                        // that constitute progress; anything vaguer invites another
                        // empty turn.
                        messages.push(Message::text(
                            MessageRole::System,
                            "Your previous turn returned nothing. Respond with exactly one of: a \
                         tool call that advances the research, or submit_report with the claims \
                         the evidence already supports. Do not reply with an empty turn.",
                        ));
                    }
                    continue;
                }
                self.complete_effect(
                    &effect_id,
                    "succeeded",
                    json!({"visible_chars": visible_chars, "tool_calls": tool_call_count}),
                )
                .await?;
                break (text, calls);
            };
            if !calls.is_empty() {
                state.phase = AgentPhase::AwaitingTools;
                messages.push(Message {
                    role: MessageRole::Assistant,
                    content: text,
                    tool_calls: calls.clone(),
                    tool_call_id: None,
                });
                let batch = self.execute_tools(task, state, calls).await?;
                messages.extend(batch.messages);
                // Publication is the only way out of the loop that produces a
                // report, and it happens only after the independent verifier has
                // passed. Everything else continues or fails closed.
                if let Some(outcome) = batch.published {
                    return Ok(outcome);
                }
                if let Some(reason) = batch.finalization_exhausted {
                    return Err(RuntimeError::VerificationFailed(reason));
                }
                // Research is bounded separately from the round ceiling, so a task
                // that keeps gathering is told to finalize rather than dying at the
                // limit with nothing published.
                if round
                    == self
                        .config
                        .max_research_rounds
                        .min(self.config.max_model_rounds)
                {
                    messages.push(Message::text(
                        MessageRole::System,
                        "The research budget for this task is spent, so the retrieval tools have \
                         been withdrawn. Only search_evidence, compute_from_evidence, run_financial_calculation and \
                         submit_report remain. Prefer compute_from_evidence for ratios. Finalize with the evidence already gathered and \
                         state remaining gaps as limitations or as claims of kind=unknown rather \
                         than asserting them.",
                    ));
                }
                continue;
            }

            if !self.config.verify_reports {
                return self.complete_run(state, text).await;
            }

            // A text-only turn is not a publication.
            //
            // Previously it was: the model's free-form Markdown went straight to
            // the verifier, which forced the model to hand-format `【E:evf_…】`
            // citations into investor-facing prose — leaking machine identity into
            // the product, and leaving provenance entirely to its formatting
            // discipline. Measured live, that path ended with 41 uncited figures
            // and 82 unreproducible ones and never published. Publication now has
            // exactly one entrance, `submit_report`, so a report cannot exist
            // without typed provenance for every number in it.
            let verdict = state.finalization.record_rejection(format!(
                "free-form-turn:{}",
                text.chars().take(512).collect::<String>()
            ));
            if verdict.is_exhausted() {
                return Err(RuntimeError::VerificationFailed(
                    "the report was never submitted through the structured contract".into(),
                ));
            }
            state.phase = AgentPhase::Reviewing;
            messages.push(Message::text(MessageRole::Assistant, text));
            messages.push(Message::text(
                MessageRole::System,
                "That answer was not published: prose is not a publishable report. Call \
                 submit_report with typed claims. Use search_evidence for canonical identifiers, \
                 give every material number a provenance, and state what the evidence does not \
                 support as a limitation or as kind=unknown.",
            ));
            continue;
        }
        Err(RuntimeError::ModelRoundLimit(self.config.max_model_rounds))
    }

    /// Validate, render, verify and publish one submitted draft.
    ///
    /// The order is the contract: nothing is rendered before it validates, and
    /// nothing is published before the independent verifier passes on the canonical
    /// form. A failure at either stage produces a targeted repair response, never a
    /// publication and never a relaxed check.
    async fn finalize_report(
        &self,
        task: &RuntimeTask,
        state: &mut RunState,
        arguments: Value,
    ) -> Result<Finalization, RuntimeError> {
        let draft: VerifiedReportDraft = match crate::report::decode_draft(&arguments) {
            Ok(draft) => draft,
            Err(error) => {
                // A shape error consumes the budget like any other rejection.
                //
                // It did not, and that was an unbounded loop: a live moderate run
                // submitted 11 drafts against a budget of 6 and died on the round
                // limit instead of failing closed, because a draft serde could not
                // decode never reached the ledger. Provenance shape is the usual
                // cause — `provenance: "calculated"` without
                // `calculation_evidence_id` fails to decode rather than validating —
                // so this path is reached by exactly the drafts most in need of
                // bounded repair.
                //
                // The fingerprint is of the raw arguments, so an identical malformed
                // resubmission is detected as no progress just like a decoded one.
                let verdict = state
                    .finalization
                    .record_rejection(format!("undecodable:{arguments}"));
                let response = json!({
                    "ok": false,
                    "stage": "decode",
                    "error": error,
                    "attempt": state.finalization.attempts(),
                    "instruction": if verdict.is_exhausted() {
                        "No finalization attempts remain. The report will not be published."
                    } else {
                        "The draft did not match the submit_report schema, so no claim was read. \
                         The error names the exact field: fix that field and resubmit the complete \
                         draft. Shape rules the live failures keep hitting: `claims` and \
                         `sections` are arrays of objects; a numeric item's `value` is a number, \
                         never text; every claim needs `id`, `kind` and `statement`; `kind` is \
                         one of observed_fact, deterministic_calculation, inference, estimate, \
                         scenario, unknown. A numeric item must carry every field its provenance \
                         requires: observed needs evidence_id; calculated needs \
                         calculation_evidence_id, operation and input_evidence_ids; estimated \
                         needs method and basis_evidence_ids."
                    },
                });
                return Ok(match verdict {
                    RepairVerdict::Exhausted { .. } => Finalization::Exhausted {
                        response,
                        reason: format!(
                            "the submitted report never matched the contract schema: {error}"
                        ),
                    },
                    RepairVerdict::Retry { .. } => Finalization::Repair(response),
                });
            }
        };

        let task_symbols: BTreeSet<String> = task.symbol.iter().cloned().collect();
        let problems = validate_draft(&draft, state.catalog.descriptors(), &task_symbols);
        if !problems.is_empty() {
            let verdict = state.finalization.record_rejection(fingerprint(&draft));
            for problem in problems.iter().take(crate::finalize::MAX_REPORTED_PROBLEMS) {
                self.record(
                    state,
                    AgentEvent::VerificationFinding {
                        finding: VerificationFinding {
                            code: problem.code().to_owned(),
                            message: format!(
                                "报告契约校验未通过：{}{}{}",
                                problem.code(),
                                problem
                                    .claim_id()
                                    .map(|id| format!("（结论 {id}）"))
                                    .unwrap_or_default(),
                                problem.event_detail(),
                            ),
                            blocking: true,
                        },
                    },
                )
                .await?;
            }
            let response = validation_repair(&problems, verdict);
            return Ok(match verdict {
                RepairVerdict::Exhausted { .. } => Finalization::Exhausted {
                    response,
                    reason: format!(
                        "structured report validation failed after {} attempts",
                        state.finalization.attempts()
                    ),
                },
                RepairVerdict::Retry { .. } => Finalization::Repair(response),
            });
        }

        let rendered = render(&draft, state.catalog.descriptors());
        state.phase = AgentPhase::Verifying;
        self.record(state, AgentEvent::VerificationStarted).await?;
        let verification = self
            .verify_canonical_report(task, state, &draft, &rendered)
            .await?;

        if verification.get("passed").and_then(Value::as_bool) == Some(true) {
            // The user-facing string is the rendered prose, never the canonical
            // form: a published report must not contain an `evf_` identifier.
            debug_assert!(
                !crate::render::contains_internal_identifier(&rendered.markdown),
                "the renderer must not leak a canonical identifier"
            );
            return Ok(Finalization::Published(Box::new(rendered)));
        }

        let findings: Vec<String> = verification
            .get("findings")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        for code in findings.iter().take(crate::finalize::MAX_REPORTED_PROBLEMS) {
            self.record(
                state,
                AgentEvent::VerificationFinding {
                    finding: VerificationFinding {
                        code: code.clone(),
                        message: format!("独立确定性校验阻止发布：{code}"),
                        blocking: true,
                    },
                },
            )
            .await?;
        }
        let verdict = state.finalization.record_rejection(fingerprint(&draft));
        let response = verification_repair(
            &findings,
            &crate::render::verifier_line_claims(&draft),
            verdict,
        );
        Ok(match verdict {
            RepairVerdict::Exhausted { .. } => Finalization::Exhausted {
                response,
                reason: findings.join(", "),
            },
            RepairVerdict::Retry { .. } => Finalization::Repair(response),
        })
    }

    /// Run the independent verifier over the canonical form of a rendered report.
    ///
    /// The context carries the registered facts the draft cites and nothing else.
    /// See [`EvidenceCatalog::verifier_facts`] for why that is both bounded and
    /// equivalent to handing over every tool result.
    async fn verify_canonical_report(
        &self,
        task: &RuntimeTask,
        state: &mut RunState,
        draft: &VerifiedReportDraft,
        rendered: &RenderedReport,
    ) -> Result<Value, RuntimeError> {
        let cited: BTreeSet<String> = draft
            .claims
            .iter()
            .flat_map(|claim| {
                claim
                    .evidence_ids
                    .iter()
                    .cloned()
                    .chain(claim.disclosed_conflicts.iter().cloned())
                    .chain(
                        claim
                            .numeric_items
                            .iter()
                            .flat_map(|item| item.provenance.referenced_evidence())
                            .map(str::to_owned),
                    )
            })
            .collect();
        let facts = state.catalog.verifier_facts(&cited);
        let verification_spec = task.verification_spec();
        let verification_context = json!({
            "task_spec": verification_spec,
            "evidence_registry": {"facts": facts},
        });
        let effect_id = self
            .begin_effect(
                state,
                "report.verify",
                json!({
                    "report_chars": rendered.verifier_markdown.chars().count(),
                    "claims": rendered.sections.iter().map(|s| s.claims.len()).sum::<usize>(),
                    "references": rendered.references.len(),
                    "cited_facts": cited.len(),
                    "contract": "structured",
                }),
            )
            .await?;
        let verification = tokio::time::timeout(
            self.config.verification_timeout,
            self.executor.execute(
                "research.agent_report_verify",
                json!({
                    "report": rendered.verifier_markdown,
                    "context": verification_context,
                    "task_spec": verification_spec,
                }),
                state.cancellation.child_token(),
            ),
        )
        .await;
        match verification {
            Ok(Ok(result)) => {
                self.complete_effect(&effect_id, "succeeded", result.clone())
                    .await?;
                Ok(result)
            }
            Ok(Err(error)) => {
                self.complete_effect(&effect_id, "failed", json!({"error": error}))
                    .await?;
                Err(RuntimeError::VerificationFailed(error))
            }
            Err(_) => {
                self.complete_effect(
                    &effect_id,
                    "failed",
                    json!({"error": "verification timeout"}),
                )
                .await?;
                Err(RuntimeError::VerificationFailed(
                    "deterministic verification timed out".into(),
                ))
            }
        }
    }

    /// Compute ratios/products from catalog evidence without asking the model for an AST.
    ///
    /// Live Case C burned research rounds on malformed `run_financial_calculation`
    /// programs after coverage was already complete. This path looks up operand
    /// values in the catalog (or accepts a scalar), builds a one-output Program, and
    /// dispatches it to the Engine so evidence registration stays the Engine's job.
    async fn compute_from_evidence(
        &self,
        task: &RuntimeTask,
        state: &RunState,
        arguments: Value,
    ) -> Result<Value, String> {
        let calculations = arguments
            .get("calculations")
            .and_then(Value::as_array)
            .ok_or_else(|| "missing calculations array".to_owned())?;
        if calculations.is_empty() || calculations.len() > MAX_BATCHED_PROGRAMS {
            return Err(format!(
                "calculations must contain 1 to {MAX_BATCHED_PROGRAMS} entries"
            ));
        }
        let mut results = Vec::with_capacity(calculations.len());
        for (index, calc) in calculations.iter().enumerate() {
            let label = calc
                .get("label")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("calculations[{index}]: missing label"))?;
            let op = calc
                .get("op")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("calculations[{index}]: missing op"))?;
            if !matches!(op, "div" | "mul" | "add" | "sub") {
                return Err(format!(
                    "calculations[{index}]: op must be div, mul, add or sub"
                ));
            }
            let (left_value, left_id) =
                resolve_compute_operand(state, calc.get("left"), index, "left")?;
            let (right_value, right_id) =
                resolve_compute_operand(state, calc.get("right"), index, "right")?;
            let program = json!({
                "version": 1,
                "inputs": {
                    "left": [left_value],
                    "right": [right_value]
                },
                "outputs": {
                    label: {
                        "op": op,
                        "left": {"op": "var", "name": "left"},
                        "right": {"op": "var", "name": "right"}
                    }
                }
            });
            let outcome = self
                .executor
                .execute(
                    "research.compute",
                    json!({"program": program}),
                    state.cancellation.child_token(),
                )
                .await
                .map_err(|error| format!("calculations[{index}] ({label}): {error}"))?;
            results.push(json!({
                "label": label,
                "op": op,
                "left_evidence_id": left_id,
                "right_evidence_id": right_id,
                "result": outcome
            }));
        }
        Ok(json!({
            "ok": true,
            "symbol": task.symbol,
            "calculations": results.len(),
            "results": results
        }))
    }

    async fn execute_tools(
        &self,
        task: &RuntimeTask,
        state: &mut RunState,
        calls: Vec<ModelToolCall>,
    ) -> Result<ToolBatch, RuntimeError> {
        let mut prepared = Vec::with_capacity(calls.len());
        let mut runtime_calls: Vec<PreparedTool> = Vec::new();
        let mut rejected: Vec<CompletedTool> = Vec::new();
        for call in calls {
            // An unknown tool name is refused, never executed — and no longer fatal
            // on the first occurrence.
            //
            // Fail-closed is about capability: the call must not reach the Engine and
            // must grant nothing. Ending the task is a separate policy, and it cost a
            // live run everything after eight successful tool calls because the model
            // asked for `search_news` instead of `research_news`. That is a slip, not
            // an attempted escape, and it is the same class as the malformed argument
            // that used to destroy a task.
            //
            // So: refuse, name the registered tools, and let the model correct itself
            // — bounded, because a model that keeps calling tools that do not exist is
            // not making progress and must not loop. Nothing is dispatched either way.
            let definition = match self.tools.get(&call.name).cloned() {
                Some(definition) => definition,
                None => {
                    state.unknown_tool_calls = state.unknown_tool_calls.saturating_add(1);
                    if state.unknown_tool_calls > self.config.max_unknown_tool_rejections {
                        return Err(RuntimeError::UnknownTool(call.name.clone()));
                    }
                    let available = self.tools.names().collect::<Vec<_>>().join(", ");
                    let message = format!(
                        "no such tool `{}`; it was not executed. Registered tools: {available}",
                        call.name
                    );
                    self.record(
                        state,
                        AgentEvent::ToolFailed {
                            call_id: call.id.clone(),
                            tool: call.name.clone(),
                            message: message.clone(),
                            retryable: true,
                        },
                    )
                    .await?;
                    rejected.push(CompletedTool::failure(call, message));
                    continue;
                }
            };

            // Calculation tools withdrawn after consecutive shape failures must not
            // run even if the provider still forwards a call for a tool that was
            // removed from the offer. Live Case C run 5 withdrew at round 7, then
            // the model called run_financial_calculation again at rounds 11 and 14
            // and each call was still executed. Omitting from the offer is not
            // enough when the provider does not enforce the list.
            if state.calc_shape_failures >= MAX_CALC_SHAPE_FAILURES
                && call.name.contains("calculation")
            {
                let message = format!(
                    "calculation tools are withdrawn after {MAX_CALC_SHAPE_FAILURES} consecutive \
                     shape failures; `{name}` was not executed. Use compute_from_evidence for PE/market_cap/YoY (no AST), or cite observed evidence, then submit_report.",
                    name = call.name
                );
                self.record(
                    state,
                    AgentEvent::ToolFailed {
                        call_id: call.id.clone(),
                        tool: call.name.clone(),
                        message: message.clone(),
                        retryable: false,
                    },
                )
                .await?;
                rejected.push(CompletedTool::failure(call, message));
                continue;
            }

            // Malformed arguments on a *registered* tool are different: the model
            // asked for something it is permitted to ask for and mis-encoded it.
            // That is one bad call, not a reason to destroy a task that has
            // already gathered evidence.
            //
            // A live deep-research run was lost to `invalid arguments for tool
            // run_financial_calculation: EOF while parsing an object at line 1
            // column 3559` — a truncated argument object, after eight tools had
            // already succeeded. Reporting it back as a tool failure lets the
            // model correct the call, which is how every other tool failure
            // already behaves.
            let arguments: Value = match serde_json::from_str::<Value>(&call.arguments) {
                Ok(value) if value.is_object() => value,
                Ok(_) => {
                    let message = "tool arguments must be a JSON object".to_string();
                    self.record(
                        state,
                        AgentEvent::ToolFailed {
                            call_id: call.id.clone(),
                            tool: call.name.clone(),
                            message: message.clone(),
                            retryable: true,
                        },
                    )
                    .await?;
                    rejected.push(CompletedTool::failure(call, message));
                    continue;
                }
                Err(error) => {
                    // Name the truncation explicitly: the model needs to know the
                    // payload was cut off rather than semantically wrong, so it
                    // can re-emit a smaller program instead of rewriting it.
                    let mut message = enrich_calculation_error(
                        &call.name,
                        format!(
                            "arguments were not valid JSON ({error}); if the payload was truncated, \
                             re-send a smaller argument object"
                        ),
                    );
                    if is_calculation_shape_error(&call.name, &message) {
                        state.calc_shape_failures = state.calc_shape_failures.saturating_add(1);
                        if state.calc_shape_failures >= MAX_CALC_SHAPE_FAILURES {
                            message = format!(
                                "{message} Calculation tools are now withdrawn after \
                                 {MAX_CALC_SHAPE_FAILURES} consecutive shape failures. \
                                 Use compute_from_evidence for PE/market_cap/YoY (no AST), or cite observed \
                                 evidence, then submit_report — do not keep rewriting the AST."
                            );
                        }
                    }
                    self.record(
                        state,
                        AgentEvent::ToolFailed {
                            call_id: call.id.clone(),
                            tool: call.name.clone(),
                            message: message.clone(),
                            retryable: true,
                        },
                    )
                    .await?;
                    rejected.push(CompletedTool::failure(call, message));
                    continue;
                }
            };
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

        // Runtime-served tools act on state this batch may still be changing, so
        // they run after the Engine calls rather than alongside them: a
        // `search_evidence` in the same round as a retrieval must see that
        // retrieval's evidence, and `submit_report` must validate against the
        // complete catalog.
        let mut index = 0;
        while index < prepared.len() {
            if prepared[index].definition.handler == ToolHandler::Runtime {
                runtime_calls.push(prepared.remove(index));
            } else {
                index += 1;
            }
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
                        execute_possibly_batched(
                            executor.as_ref(),
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

        // Rejected calls are already reported; carry them so the model sees a
        // result for every call it made, including the ones we refused.
        let mut completed = rejected;
        while let Some((prepared, result)) = executions.next().await {
            self.ensure_active(state)?;
            match result {
                Ok(Ok(value)) => {
                    if prepared.call.name.contains("calculation") {
                        state.calc_shape_failures = 0;
                    }
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
                    // The catalog is built from the durable result, not from the
                    // bounded projection the model sees, so a truncated context
                    // never means lost provenance.
                    state.catalog.ingest(
                        &value,
                        prepared
                            .arguments
                            .get("symbol")
                            .and_then(Value::as_str)
                            .or(task.symbol.as_deref()),
                    );
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
                    completed.push(CompletedTool::success(prepared.call, value, evidence_ids));
                }
                Ok(Err(message)) => {
                    let mut message = enrich_calculation_error(&prepared.call.name, message);
                    if is_calculation_shape_error(&prepared.call.name, &message) {
                        state.calc_shape_failures = state.calc_shape_failures.saturating_add(1);
                        if state.calc_shape_failures >= MAX_CALC_SHAPE_FAILURES {
                            message = format!(
                                "{message} Calculation tools are now withdrawn after \
                                 {MAX_CALC_SHAPE_FAILURES} consecutive shape failures. \
                                 Use compute_from_evidence for PE/market_cap/YoY (no AST), or cite observed \
                                 evidence, then submit_report — do not keep rewriting the AST."
                            );
                        }
                    }
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
        let mut messages = completed
            .into_iter()
            .map(CompletedTool::into_message)
            .collect::<Result<Vec<Message>, RuntimeError>>()?;

        // Runtime-served tools, in call order for determinism.
        runtime_calls.sort_by_key(|prepared| prepared.call.index);
        let mut published = None;
        let mut finalization_exhausted = None;
        for prepared in runtime_calls {
            self.ensure_active(state)?;
            let completed = match prepared.definition.name.as_str() {
                "search_evidence" => {
                    let response = match serde_json::from_value::<EvidenceSearchRequest>(
                        prepared.arguments.clone(),
                    ) {
                        Ok(request) => state.catalog.search_batch(&request.queries()),
                        Err(error) => json!({
                            "ok": false,
                            "error": error.to_string(),
                            "instruction": "Correct the search arguments and try again. Prefer \
                                            the batch form: {\"queries\": [{…}, {…}]}.",
                        }),
                    };
                    self.complete_effect(&prepared.effect_id, "succeeded", response.clone())
                        .await?;
                    self.record(
                        state,
                        AgentEvent::ToolCompleted {
                            call_id: prepared.call.id.clone(),
                            tool: prepared.call.name.clone(),
                            evidence_ids: Vec::new(),
                        },
                    )
                    .await?;
                    CompletedTool::runtime(prepared.call, response)
                }
                "compute_from_evidence" => {
                    match self
                        .compute_from_evidence(task, state, prepared.arguments.clone())
                        .await
                    {
                        Ok(value) => {
                            let evidence = evidence_ids(&value);
                            state.catalog.ingest(&value, task.symbol.as_deref());
                            state.evidence_ids.extend(evidence.iter().cloned());
                            state.evidence_ids.sort();
                            state.evidence_ids.dedup();
                            self.complete_effect(&prepared.effect_id, "succeeded", value.clone())
                                .await?;
                            self.record(
                                state,
                                AgentEvent::ToolCompleted {
                                    call_id: prepared.call.id.clone(),
                                    tool: prepared.call.name.clone(),
                                    evidence_ids: evidence.clone(),
                                },
                            )
                            .await?;
                            CompletedTool::runtime(prepared.call, value)
                        }
                        Err(error) => {
                            let response = json!({
                                "ok": false,
                                "error": error,
                                "instruction": "Each calculation needs label, op (div|mul|add|sub), \
                                                 and left/right as {\"evidence_id\":\"evf_…\"} or \
                                                 {\"value\":34.63}. Look up identifiers with \
                                                 search_evidence first."
                            });
                            self.complete_effect(&prepared.effect_id, "failed", response.clone())
                                .await?;
                            self.record(
                                state,
                                AgentEvent::ToolFailed {
                                    call_id: prepared.call.id.clone(),
                                    tool: prepared.call.name.clone(),
                                    message: error.clone(),
                                    retryable: true,
                                },
                            )
                            .await?;
                            CompletedTool::runtime(prepared.call, response)
                        }
                    }
                }
                "submit_report" => {
                    let outcome = self
                        .finalize_report(task, state, prepared.arguments.clone())
                        .await?;
                    match outcome {
                        Finalization::Published(rendered) => {
                            self.complete_effect(
                                &prepared.effect_id,
                                "succeeded",
                                json!({"published": true}),
                            )
                            .await?;
                            self.record(
                                state,
                                AgentEvent::ToolCompleted {
                                    call_id: prepared.call.id.clone(),
                                    tool: prepared.call.name.clone(),
                                    evidence_ids: rendered
                                        .references
                                        .iter()
                                        .map(|reference| reference.internal_id.clone())
                                        .collect(),
                                },
                            )
                            .await?;
                            published =
                                Some(self.complete_run(state, rendered.markdown.clone()).await?);
                            break;
                        }
                        Finalization::Repair(response) => {
                            let summary = repair_event_summary(&response);
                            self.complete_effect(
                                &prepared.effect_id,
                                "failed",
                                json!({"published": false}),
                            )
                            .await?;
                            self.record(
                                state,
                                AgentEvent::ToolFailed {
                                    call_id: prepared.call.id.clone(),
                                    tool: prepared.call.name.clone(),
                                    message: summary,
                                    retryable: true,
                                },
                            )
                            .await?;
                            CompletedTool::runtime(prepared.call, response)
                        }
                        Finalization::Exhausted { response, reason } => {
                            self.complete_effect(
                                &prepared.effect_id,
                                "failed",
                                json!({"published": false, "exhausted": true}),
                            )
                            .await?;
                            self.record(
                                state,
                                AgentEvent::ToolFailed {
                                    call_id: prepared.call.id.clone(),
                                    tool: prepared.call.name.clone(),
                                    message: "报告发布预算已用尽，拒绝发布".into(),
                                    retryable: false,
                                },
                            )
                            .await?;
                            finalization_exhausted = Some(reason);
                            CompletedTool::runtime(prepared.call, response)
                        }
                    }
                }
                other => {
                    // A Runtime handler is required for every Runtime-served tool.
                    // Reaching here means the registry advertises a capability the
                    // runtime cannot serve, which is a configuration defect, not a
                    // model error — and exactly the state this change repaired.
                    return Err(RuntimeError::UnknownTool(other.to_owned()));
                }
            };
            messages.push(completed.into_message()?);
        }

        Ok(ToolBatch {
            messages,
            published,
            finalization_exhausted,
        })
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
            // An empty turn is being replayed, so the task is still reasoning at
            // the same round. Advancing or rewinding the phase here would misreport
            // a recovered hiccup as progress or as a fault.
            AgentEvent::ModelTurnEmpty { .. } => {
                task.phase = AgentPhase::Reasoning;
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
            AgentEvent::Suspended {
                resume_at, fault, ..
            } => {
                task.phase = AgentPhase::Suspended;
                task.resume_at = resume_at.clone();
                task.suspended_by = fault.clone();
            }
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
    /// Calls to tools that do not exist, refused so far.
    unknown_tool_calls: usize,
    /// Consecutive calculation shape failures in this task.
    ///
    /// A live Case C run finished research coverage in four rounds, then burned
    /// sixteen more on malformed ASTs (strings in inputs, bare numbers as expr,
    /// truncated payloads). The worked example helps intermittently; a model that
    /// keeps thrashing simply declines it. After
    /// [`MAX_CALC_SHAPE_FAILURES`] consecutive shape failures the calculation
    /// tools are withdrawn, the same way retrieval tools are withdrawn when the
    /// research budget is spent — a budget the model can decline is not a budget.
    calc_shape_failures: usize,
    /// Canonical evidence the task has seen, with the metadata a citation needs.
    ///
    /// Durable task state, not model context: the model reaches it only through
    /// bounded `search_evidence` queries.
    catalog: EvidenceCatalog,
    finalization: FinalizationLedger,
    sender: mpsc::Sender<AgentEvent>,
    cancellation: CancellationToken,
}

impl RunState {
    fn new(
        task_id: String,
        sender: mpsc::Sender<AgentEvent>,
        cancellation: CancellationToken,
        max_finalization_attempts: usize,
    ) -> Self {
        Self {
            task_id,
            seq: 0,
            phase: AgentPhase::Preparing,
            model_round: 0,
            completed_tool_ids: Vec::new(),
            evidence_ids: Vec::new(),
            unknown_tool_calls: 0,
            calc_shape_failures: 0,
            catalog: EvidenceCatalog::default(),
            finalization: FinalizationLedger::new(max_finalization_attempts),
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

/// What one batch of tool calls produced.
struct ToolBatch {
    /// One tool result message per call the model made, in call order.
    messages: Vec<Message>,
    /// Set when `submit_report` validated, rendered and passed the independent
    /// verifier. The only path to a published report.
    published: Option<RunOutcome>,
    /// Set when finalization ran out of attempts. The task fails closed with this
    /// reason; it is never converted into a publication.
    finalization_exhausted: Option<String>,
}

/// Result of one `submit_report` call.
enum Finalization {
    /// Validated, rendered and verified. Boxed because a rendered report is large
    /// relative to the other variants.
    Published(Box<RenderedReport>),
    /// Refused, with a targeted repair response for the model.
    Repair(Value),
    /// Refused, and no attempts remain.
    Exhausted { response: Value, reason: String },
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

    /// A Runtime-served result, passed through as composed.
    ///
    /// Runtime responses are already bounded by construction — a search caps its
    /// rows, a repair response caps its problems — so they are not re-bounded here;
    /// doing so would truncate the very instructions the model must act on.
    fn runtime(call: ModelToolCall, content: Value) -> Self {
        Self {
            index: call.index,
            call_id: call.id,
            content,
        }
    }

    fn into_message(self) -> Result<Message, RuntimeError> {
        Ok(Message::tool_result(
            self.call_id,
            serde_json::to_string(&self.content)?,
        ))
    }
}

/// Run one Engine call, or a batch of calculation programs, as a single tool result.
///
/// The calculation tool has always accepted a program with many named outputs, and the
/// model did not use it: measured live it issued 12, 13 and 24 separate calculation
/// calls in three balanced tasks, one per figure, at one model round each. Asking it to
/// batch in the tool description changed nothing, so the batch is now something the
/// schema offers rather than something the prose requests.
///
/// Programs run sequentially and the first failure is returned, because a later program
/// often depends on an earlier one's result being correct; reporting the first real
/// error is more useful than a list of consequences. Each result is returned under its
/// index so the model can attribute outputs.
async fn execute_possibly_batched(
    executor: &dyn ToolExecutor,
    engine_kind: &str,
    arguments: Value,
    cancellation: CancellationToken,
) -> Result<Value, String> {
    let Some(programs) = arguments.get("programs") else {
        return executor.execute(engine_kind, arguments, cancellation).await;
    };
    let Some(programs) = programs.as_array() else {
        return Err("`programs` must be a JSON array of program objects; \
             for one program use `program` instead"
            .into());
    };
    if programs.len() > MAX_BATCHED_PROGRAMS {
        return Err(format!(
            "a calculation batch may contain at most {MAX_BATCHED_PROGRAMS} programs, received {}",
            programs.len()
        ));
    }
    // Everything except `programs` is shared by each call, so a batched JoinQuant
    // calculation keeps its symbol and window.
    let mut shared = arguments.clone();
    if let Some(object) = shared.as_object_mut() {
        object.remove("programs");
    }
    let mut results = Vec::with_capacity(programs.len());
    for (index, program) in programs.iter().enumerate() {
        let mut payload = shared.clone();
        if let Some(object) = payload.as_object_mut() {
            object.insert("program".into(), program.clone());
        }
        let outcome = executor
            .execute(engine_kind, payload, cancellation.child_token())
            .await
            .map_err(|error| format!("program {index}: {error}"))?;
        results.push(outcome);
    }
    Ok(json!({"programs": results.len(), "results": results}))
}

/// Calculation programs answerable in one call.
const MAX_BATCHED_PROGRAMS: usize = 12;

/// Consecutive calculation shape failures before the tools are withdrawn.
///
/// Measured: one Case C run spent 16 rounds on malformed ASTs after coverage was
/// already complete; another spent 2 and reached the verifier. Three is enough to
/// deliver the worked example twice and still leave finalization budget.
const MAX_CALC_SHAPE_FAILURES: usize = 3;

/// Append a worked scalar example when a calculation payload is the wrong shape.
///
/// A live Case C run burned sixteen research rounds on malformed ASTs after research
/// coverage was already complete: strings in `inputs`, bare numbers as `expr`, truncated
/// payloads. Naming the field path (Engine) is necessary but not sufficient — the model
/// also needs one correct shape to copy. The example is only attached to shape errors so
/// a real compute failure (`unknown variable`, fuel exhausted) stays uncluttered.
fn enrich_calculation_error(tool: &str, message: String) -> String {
    if !tool.contains("calculation") {
        return message;
    }
    let shape_error = message.contains("invalid type")
        || message.contains("missing field")
        || message.contains("unknown field")
        || message.contains("invalid request payload")
        || message.contains("not valid JSON")
        || message.contains("must be a JSON array");
    if !shape_error {
        return message;
    }
    format!(
        "{message}. Copy this scalar PE shape: \
         {{\"program\":{{\"version\":1,\"inputs\":{{\"price\":[34.63],\"eps\":[1.95]}},\
\"outputs\":{{\"pe\":{{\"op\":\"div\",\"left\":{{\"op\":\"var\",\"name\":\"price\"}},\
\"right\":{{\"op\":\"var\",\"name\":\"eps\"}}}}}}}}}}. \
         Inputs are arrays of numbers (never strings); every expr is a JSON object with \
         an `op` field (never a string or bare number)."
    )
}

/// One line of event-stream diagnosis for a refused submission.
///
/// The model receives the full structured repair guidance as its tool result;
/// a headless consumer watching the typed event stream saw only a fixed refusal
/// string, so a decode failure was invisible — a live moderate run lost two
/// finalization rounds to undecodable drafts and the event stream showed zero
/// findings for them, leaving the cause only in the model's ephemeral context.
/// The stage, the decode error, or the validation problem histogram is enough
/// to classify every refusal offline.
fn repair_event_summary(response: &Value) -> String {
    if response.get("stage").and_then(Value::as_str) == Some("decode") {
        let error = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let bounded: String = error.chars().take(400).collect();
        format!("报告未通过发布前校验（decode）：{bounded}")
    } else {
        let count = response
            .get("problem_count")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let mut codes: BTreeMap<String, usize> = BTreeMap::new();
        if let Some(problems) = response.get("problems").and_then(Value::as_array) {
            for problem in problems {
                if let Some(code) = problem.get("problem").and_then(Value::as_str) {
                    *codes.entry(code.to_owned()).or_default() += 1;
                }
            }
        }
        let histogram = codes
            .iter()
            .map(|(code, count)| format!("{code}×{count}"))
            .collect::<Vec<_>>()
            .join(", ");
        if histogram.is_empty() {
            format!("报告未通过发布前校验（validation）：{count} 个问题")
        } else {
            format!("报告未通过发布前校验（validation）：{count} 个问题：{histogram}")
        }
    }
}

fn is_calculation_shape_error(tool: &str, message: &str) -> bool {
    tool.contains("calculation")
        && (message.contains("invalid type")
            || message.contains("missing field")
            || message.contains("unknown field")
            || message.contains("invalid request payload")
            || message.contains("not valid JSON")
            || message.contains("must be a JSON array")
            || message.contains("Copy this scalar PE shape"))
}

/// Resolve one operand of `compute_from_evidence` to a numeric value.
fn resolve_compute_operand(
    state: &RunState,
    operand: Option<&Value>,
    index: usize,
    side: &str,
) -> Result<(f64, Option<String>), String> {
    let Some(operand) = operand.and_then(Value::as_object) else {
        return Err(format!("calculations[{index}]: missing {side} operand"));
    };
    if let Some(value) = operand.get("value").and_then(Value::as_f64) {
        return Ok((value, None));
    }
    let Some(evidence_id) = operand.get("evidence_id").and_then(Value::as_str) else {
        return Err(format!(
            "calculations[{index}].{side}: provide evidence_id or value"
        ));
    };
    let Some(descriptor) = state.catalog.descriptors().get(evidence_id) else {
        return Err(format!(
            "calculations[{index}].{side}: unknown evidence_id `{evidence_id}`; search_evidence first"
        ));
    };
    let Some(value) = descriptor.value.as_ref().and_then(|raw| match raw {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.replace(',', "").parse::<f64>().ok(),
        _ => None,
    }) else {
        return Err(format!(
            "calculations[{index}].{side}: evidence `{evidence_id}` has no numeric value"
        ));
    };
    Ok((value, Some(evidence_id.to_owned())))
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

    /// A malformed tool call must not destroy a task that has already worked.
    ///
    /// An unknown *tool name* remains fatal — that is an attempted capability
    /// escape from a closed registry, covered by the vertical suite. This is about
    /// a registered tool whose arguments were mis-encoded.
    ///
    /// A live deep-research run was lost to a truncated argument object —
    /// `EOF while parsing an object at line 1 column 3559` — after eight tools had
    /// already succeeded. The whole task was discarded because the parse error
    /// propagated with `?`. It is now reported as a failure of that one call, the
    /// same way every other tool failure behaves, so the model can re-send a
    /// smaller payload.
    #[test]
    fn a_rejected_tool_call_is_reported_as_a_tool_failure() {
        let completed = CompletedTool::failure(
            tool_call(),
            "arguments were not valid JSON (EOF while parsing an object)".into(),
        );
        assert_eq!(completed.content["ok"], json!(false));
        assert!(completed.content["error"]
            .as_str()
            .unwrap_or_default()
            .contains("not valid JSON"));
        assert!(
            completed.content.get("data").is_none(),
            "a rejected call must carry no payload"
        );
    }

    /// The truncation hint must reach the model, not just the parse error.
    ///
    /// Without it the model rewrites the program semantically instead of shrinking
    /// it, and hits the same output limit again.
    #[test]
    fn a_truncated_argument_message_tells_the_model_to_send_less() {
        let message = format!(
            "arguments were not valid JSON ({}); if the payload was truncated, \
             re-send a smaller argument object",
            "EOF while parsing an object at line 1 column 3559"
        );
        assert!(message.contains("truncated"));
        assert!(message.contains("smaller"));
    }

    /// A calculation shape error carries a worked scalar example the model can copy.
    ///
    /// Measured: sixteen research rounds on malformed ASTs after coverage was already
    /// complete. Naming the field path alone left the model without a correct shape.
    #[test]
    fn a_calculation_shape_error_includes_a_worked_scalar_example() {
        let enriched = enrich_calculation_error(
            "run_financial_calculation",
            "invalid_payload: invalid request payload: program.bindings[0].expr: invalid type: string \"2.0\", expected struct"
                .into(),
        );
        assert!(
            enriched.contains("Scalar PE shape")
                || enriched.contains("scalar PE shape")
                || enriched.contains("Copy this scalar PE shape"),
            "{enriched}"
        );
        assert!(enriched.contains("\"op\":\"div\""), "{enriched}");
        assert!(
            !enrich_calculation_error(
                "run_financial_calculation",
                "compute: unknown calculation variable `market_cap`".into(),
            )
            .contains("Copy this scalar PE shape"),
            "a real compute failure must not be cluttered with the shape example"
        );
        assert_eq!(
            enrich_calculation_error("get_quote", "upstream 429".into()),
            "upstream 429"
        );
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
