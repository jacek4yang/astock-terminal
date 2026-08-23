//! The tool-calling conversation loop with resumable workflows.
//!
//! Every round is persisted: chat messages go to the conversation store and
//! the workflow state (spec, round counter, evidence) to `agent_tasks`. When
//! the MiniMax quota runs out, the task suspends with its reset time;
//! `resume_task` rebuilds the message history from storage and continues —
//! completed tool results come back as conversation messages and cached
//! payloads, never re-executed.

use std::collections::BTreeSet;
use std::pin::Pin;
use std::sync::Arc;

use futures::channel::mpsc;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::Instant;

use astock_minimax::{ChatMessage, ChatRequest, MinimaxError, ToolCall};
use astock_security::{authorize_tool, fingerprint_json, InvocationOrigin, ToolPermissionDomain};
use astock_storage::{AgentTask, AgentToolAudit, Report as StoredReport, Storage};

use crate::backend::ChatBackend;
use crate::error::{AgentError, Result};
use crate::prompt::initial_messages_with_context;
use crate::report::{
    assemble_report, index_tool_evidence, report_versions, AgentReport, Evidence,
    VerificationStatus,
};
use crate::tools::{now_secs, ToolContext, ToolProgressDetail, ToolRegistry};

/// A boxed event stream for one running task.
pub type TaskStream = Pin<Box<dyn Stream<Item = AgentEvent> + Send>>;

/// One isolated, tool-free specialist participating in a bounded review
/// panel. The host chooses the role prompt and model; the main analyst alone
/// owns tools and the final answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpecialistRoute {
    pub name: String,
    pub instruction: String,
    #[serde(default)]
    pub model: Option<String>,
}

/// What to work on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSpec {
    /// Unique id for this execution attempt.
    pub id: String,
    /// Stable conversation that owns the messages. Older persisted task
    /// states omit this field and therefore fall back to `id`.
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// Task kind, e.g. "analysis", "compare", "scan".
    pub kind: String,
    /// The user instruction, in Chinese.
    pub prompt: String,
    /// Round limit override (one round = one model call + its tool calls).
    pub max_rounds: Option<u32>,
    /// Model override; falls back to config, then to the backend's probe.
    pub model: Option<String>,
    /// Compact runtime context (e.g. "用户正在查看:600519 贵州茅台"), appended
    /// to the system message after the stable prompt prefix.
    #[serde(default)]
    pub context: Option<String>,
    /// Per-run tool allowlist. `None` keeps the backward-compatible behavior
    /// of enabling every registered tool; an empty list means text-only.
    #[serde(default)]
    pub enabled_tools: Option<Vec<String>>,
    /// User-selected workflow: quick / deep / plan.
    #[serde(default)]
    pub research_mode: Option<String>,
    /// User-selected analysis depth: standard / deep / maximum.
    #[serde(default)]
    pub reasoning_depth: Option<String>,
    /// Continue this durable task after the provider's rolling quota window
    /// resets. Scheduling is handled by the desktop host.
    #[serde(default)]
    pub auto_resume_on_quota: bool,
    /// Optional isolated specialists used once, after the main analyst has a
    /// first evidence-backed draft. Empty keeps the single-agent path.
    #[serde(default)]
    pub specialists: Vec<SpecialistRoute>,
}

impl TaskSpec {
    /// A task with just the mandatory fields.
    pub fn new(id: impl Into<String>, kind: impl Into<String>, prompt: impl Into<String>) -> Self {
        TaskSpec {
            id: id.into(),
            conversation_id: None,
            kind: kind.into(),
            prompt: prompt.into(),
            max_rounds: None,
            model: None,
            context: None,
            enabled_tools: None,
            research_mode: None,
            reasoning_depth: None,
            auto_resume_on_quota: false,
            specialists: Vec::new(),
        }
    }

    /// Attach a runtime-context block to the system prompt.
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// Attach this unique run to a stable conversation.
    pub fn in_conversation(mut self, conversation_id: impl Into<String>) -> Self {
        self.conversation_id = Some(conversation_id.into());
        self
    }

    /// Attach validated user-facing research controls to this durable task.
    pub fn with_run_options(
        mut self,
        research_mode: impl Into<String>,
        reasoning_depth: impl Into<String>,
        enabled_tools: Vec<String>,
        auto_resume_on_quota: bool,
    ) -> Self {
        self.research_mode = Some(research_mode.into());
        self.reasoning_depth = Some(reasoning_depth.into());
        self.enabled_tools = Some(enabled_tools);
        self.auto_resume_on_quota = auto_resume_on_quota;
        self
    }

    pub fn with_specialists(mut self, specialists: Vec<SpecialistRoute>) -> Self {
        self.specialists = specialists;
        self
    }

    fn runtime_directive(&self) -> String {
        let mode = match self.research_mode.as_deref().unwrap_or("deep") {
            "quick" => "快速模式：只调用回答当前问题必需的工具，优先在较少轮次内给出可核验结论。",
            "plan" => "计划模式：先判断目标、资金规模、期限、风险承受力和交易限制是否足以决定研究路线。若缺少会实质改变结论的信息，本轮只提出不超过3个具体问题并停止；必须用系统约定的astock-questions结构化选择框，禁止退化成普通Markdown问答列表。用户回答后检查仍未明确的关键项，可继续分批提问。信息充分后先列研究计划，再按计划取证、反证和综合。",
            _ => "深度模式：主动进行多源取证、交叉验证和反方检验，只在证据足以支持结论后完成回答。",
        };
        let depth = match self.reasoning_depth.as_deref().unwrap_or("deep") {
            "standard" => "思考深度为标准：聚焦最重要的证据、风险和可执行下一步。",
            "maximum" => "思考深度为极深：按大额资金决策标准，检查数据口径、反例、市场状态变化、参数敏感性、压力情景、容量与流动性；不得用更长篇幅代替更强证据。",
            _ => "思考深度为深入：至少核对关键数据口径、反方证据、失效条件和三种情景。",
        };
        let tools = match self.enabled_tools.as_deref() {
            Some([]) => "本轮用户关闭了全部工具：不得发起工具调用，只能说明现有上下文能支持的内容与证据缺口。".to_string(),
            Some(names) => format!(
                "本轮只允许调用这些工具：{}。任何未列出的工具都已被用户关闭，不得尝试调用。",
                names.join("、")
            ),
            None => "本轮可使用系统注册的全部工具。".to_string(),
        };
        format!("【本轮研究控制】\n{mode}\n{depth}\n{tools}")
    }

    fn conversation_id(&self) -> &str {
        self.conversation_id.as_deref().unwrap_or(&self.id)
    }
}

/// Why a task suspended.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SuspendReason {
    /// The MiniMax Token Plan window is exhausted.
    QuotaExhausted {
        /// When the window resets, unix seconds, if known.
        reset_at_unix: Option<u64>,
    },
}

/// Events emitted while a task runs.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Coarse lifecycle progress. `completed/total` is present only when the
    /// current phase has a knowable unit count (for example a tool batch).
    Progress {
        /// Stable phase id: preparing / reasoning / tools / synthesizing.
        phase: String,
        /// Human-readable, non-sensitive status text.
        message: String,
        /// Current model round, starting at 1.
        round: u32,
        /// Configured safety ceiling, not an ETA.
        max_rounds: u32,
        /// Finished units in this phase.
        completed: Option<usize>,
        /// Total units in this phase.
        total: Option<usize>,
    },
    /// The request history was deterministically compressed before a round.
    ContextCompacted {
        /// Approximate characters before compaction.
        before_chars: usize,
        /// Approximate characters sent after compaction.
        after_chars: usize,
        /// Number of recent raw messages retained verbatim.
        retained_messages: usize,
    },
    /// A fragment of the assistant's streamed text.
    TextDelta {
        /// The streamed text fragment.
        text: String,
    },
    /// Discard a streamed draft that failed the final-answer contract before
    /// an automatic bounded repair pass starts.
    TextReset {
        /// User-facing explanation of the transparent repair.
        message: String,
    },
    /// A tool call is about to execute.
    ToolCallStarted {
        /// Provider tool-call identity, used to match parallel completions.
        call_id: String,
        /// Tool name.
        name: String,
        /// Arguments as requested by the model.
        args: Value,
        /// One-based position in the current tool batch.
        position: usize,
        /// Total tool calls in the current batch.
        total: usize,
        /// Expected duration used only for UI guidance. It is never a
        /// cancellation deadline; the tool runs until completion or an
        /// explicit user cancellation.
        estimated_ms: u64,
    },
    /// Heartbeat while a tool is still running. This exposes truthful coarse
    /// stages without leaking provider/model internals or private reasoning.
    ToolCallProgress {
        call_id: String,
        name: String,
        elapsed_ms: u64,
        /// Expected duration used only for UI guidance.
        estimated_ms: u64,
        stage: String,
        /// Structured counters/current items for tools that can expose
        /// deeper deterministic progress.
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<ToolProgressDetail>,
    },
    /// A tool call finished.
    ToolCallFinished {
        /// Provider tool-call identity.
        call_id: String,
        /// Tool name.
        name: String,
        /// Cache key of the stored result (empty when not cacheable).
        cache_key: String,
        /// Wall-clock execution time.
        elapsed_ms: u64,
        /// Whether the deterministic tool completed successfully.
        success: bool,
        /// Provider/data source on success.
        source: Option<String>,
        /// Snapshot timestamp on success.
        fetched_at: Option<String>,
        /// Safe error detail on failure.
        error: Option<String>,
    },
    /// The task suspended; resume it later with `resume_task`.
    Suspended {
        /// Why it suspended.
        reason: SuspendReason,
    },
    /// The task finished with a final report.
    Completed {
        /// The assembled report.
        report: Box<AgentReport>,
    },
    /// The task failed unrecoverably.
    Failed {
        /// Error detail.
        error: String,
    },
}

/// Engine tuning knobs.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Maximum model rounds per task (default 32).
    pub max_rounds: u32,
    /// Maximum concurrent tool executions within one round (default 6).
    pub max_parallel_tools: usize,
    /// Character budget for the message history sent to the model; beyond it
    /// the older history is compacted into a deterministic working-state
    /// snapshot (see [`compact_history`]).
    pub history_char_budget: usize,
    /// Default model (overridden by `TaskSpec::model`).
    pub model: Option<String>,
    /// Sampling temperature, when set.
    pub temperature: Option<f64>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            max_rounds: 32,
            max_parallel_tools: 6,
            history_char_budget: 120_000,
            model: None,
            temperature: None,
        }
    }
}

/// The agent engine: chat backend + tool registry + persistence.
#[derive(Clone)]
pub struct AgentEngine {
    backend: Arc<dyn ChatBackend>,
    tools: ToolRegistry,
    ctx: ToolContext,
    config: EngineConfig,
}

/// Workflow state persisted to `agent_tasks.state_json` after every round.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskState {
    spec: TaskSpec,
    round: u32,
    evidence: Vec<Evidence>,
    #[serde(default)]
    context_compactions: u32,
    #[serde(default)]
    multi_agent_reviewed: bool,
    /// Last terminal error, persisted for user-visible diagnostics. Provider
    /// credentials and private reasoning are never written here.
    #[serde(default)]
    last_error: Option<String>,
}

/// How many trailing messages are never compacted.
const KEEP_RECENT_MESSAGES: usize = 6;

impl AgentEngine {
    /// Build an engine over a chat backend, tool registry and storage.
    pub fn new(
        backend: Arc<dyn ChatBackend>,
        tools: ToolRegistry,
        ctx: ToolContext,
        config: EngineConfig,
    ) -> Self {
        AgentEngine {
            backend,
            tools,
            ctx,
            config,
        }
    }

    /// The tool registry this engine runs.
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    /// Start a new task; events arrive on the returned stream.
    pub fn run_task(&self, spec: TaskSpec) -> TaskStream {
        let (tx, rx) = mpsc::unbounded();
        let engine = self.clone();
        tokio::spawn(async move {
            engine.start_worker(spec, tx).await;
        });
        Box::pin(rx)
    }

    /// Resume a suspended (or interrupted, still "running") task.
    ///
    /// The message history is rebuilt from the conversation store, so tool
    /// results completed before the suspension are replayed to the model
    /// without re-executing the tools.
    pub async fn resume_task(&self, task_id: &str) -> Result<TaskStream> {
        let record = self
            .ctx
            .storage
            .agent_task_get(task_id)
            .await?
            .ok_or_else(|| AgentError::TaskNotFound(task_id.to_string()))?;
        match record.status.as_str() {
            "suspended" | "running" => {}
            other => {
                return Err(AgentError::NotResumable(
                    task_id.to_string(),
                    other.to_string(),
                ))
            }
        }
        let state: TaskState = serde_json::from_str(&record.state_json)?;
        let mut messages = load_messages(&self.ctx.storage, state.spec.conversation_id()).await?;
        if messages.is_empty() {
            // Defensive: a task without history restarts from its prompt.
            messages =
                initial_messages_with_context(&state.spec.prompt, state.spec.context.as_deref());
        }
        let (tx, rx) = mpsc::unbounded();
        let engine = self.clone();
        tokio::spawn(async move {
            engine.run_loop(state, messages, tx).await;
        });
        Ok(Box::pin(rx))
    }

    /// List all persisted tasks, most recently updated first.
    pub async fn list_tasks(&self) -> Result<Vec<AgentTask>> {
        Ok(self.ctx.storage.agent_task_list().await?)
    }

    /// Mark a task cancelled; running tools notice on their next heartbeat and
    /// their futures are dropped together with the rest of the active batch.
    /// Returns `false` when the task does not exist.
    pub async fn cancel_task(&self, task_id: &str) -> Result<bool> {
        let Some(record) = self.ctx.storage.agent_task_get(task_id).await? else {
            return Ok(false);
        };
        self.ctx
            .storage
            .agent_task_save(AgentTask {
                status: "cancelled".to_string(),
                updated_at: now_secs(),
                ..record
            })
            .await?;
        Ok(true)
    }

    /// First-run worker: create the conversation, persist the opening
    /// messages, then enter the shared loop.
    async fn start_worker(&self, spec: TaskSpec, tx: mpsc::UnboundedSender<AgentEvent>) {
        let conversation_id = spec.conversation_id().to_string();
        if let Err(e) = self
            .ctx
            .storage
            .conversation_create(&conversation_id, Some(&spec.kind))
            .await
        {
            send(
                &tx,
                AgentEvent::Failed {
                    error: format!("storage: {e}"),
                },
            );
            return;
        }
        let (mut messages, is_new_conversation) =
            match load_messages(&self.ctx.storage, &conversation_id).await {
                Ok(existing) if !existing.is_empty() => (existing, false),
                Ok(_) => (
                    initial_messages_with_context(&spec.prompt, spec.context.as_deref()),
                    true,
                ),
                Err(e) => {
                    send(
                        &tx,
                        AgentEvent::Failed {
                            error: format!("storage: {e}"),
                        },
                    );
                    return;
                }
            };
        if is_new_conversation {
            for (i, m) in messages.iter().enumerate() {
                if let Err(e) =
                    store_message(&self.ctx.storage, &spec.id, &conversation_id, i, m).await
                {
                    send(
                        &tx,
                        AgentEvent::Failed {
                            error: format!("storage: {e}"),
                        },
                    );
                    return;
                }
            }
        } else {
            let user = ChatMessage::user(spec.prompt.clone());
            let seq = messages.len();
            if let Err(e) =
                store_message(&self.ctx.storage, &spec.id, &conversation_id, seq, &user).await
            {
                send(
                    &tx,
                    AgentEvent::Failed {
                        error: format!("storage: {e}"),
                    },
                );
                return;
            }
            messages.push(user);
        }
        let state = TaskState {
            spec,
            round: 0,
            evidence: Vec::new(),
            context_compactions: 0,
            multi_agent_reviewed: false,
            last_error: None,
        };
        self.run_loop(state, messages, tx).await;
    }

    /// The conversation loop. Persists after every round; suspends on
    /// `QuotaExhausted`; completes when the model stops calling tools.
    async fn run_loop(
        &self,
        mut state: TaskState,
        mut messages: Vec<ChatMessage>,
        tx: mpsc::UnboundedSender<AgentEvent>,
    ) {
        let task_id = state.spec.id.clone();
        let conversation_id = state.spec.conversation_id().to_string();
        let max_rounds = state.spec.max_rounds.unwrap_or(self.config.max_rounds);

        let model = match self.resolve_model(&state.spec).await {
            Ok(m) => m,
            Err(e) => {
                self.finish_with_error(&state, &tx, format!("model selection: {e}"))
                    .await;
                return;
            }
        };

        if let Err(e) = self.save_state(&state, "running").await {
            send(
                &tx,
                AgentEvent::Failed {
                    error: format!("storage: {e}"),
                },
            );
            return;
        }
        send(
            &tx,
            AgentEvent::Progress {
                phase: "preparing".to_string(),
                message: format!("已选择 {model}，正在准备研究上下文"),
                round: state.round.saturating_add(1),
                max_rounds,
                completed: None,
                total: None,
            },
        );

        loop {
            if state.round >= max_rounds {
                self.finish_with_error(&state, &tx, format!("超过最大轮数 {max_rounds}"))
                    .await;
                return;
            }
            if let Err(e) = self.check_cancelled(&task_id).await {
                match e {
                    AgentError::Cancelled(_) => {
                        send(
                            &tx,
                            AgentEvent::Failed {
                                error: "任务已取消".to_string(),
                            },
                        );
                        return;
                    }
                    other => {
                        self.finish_with_error(&state, &tx, other.to_string()).await;
                        return;
                    }
                }
            }

            let last_round = state.round + 1 >= max_rounds;
            let before_chars: usize = messages.iter().map(message_len).sum();
            let compacted = compact_history(&messages, self.config.history_char_budget);
            let after_chars: usize = compacted.iter().map(message_len).sum();
            if after_chars < before_chars {
                state.context_compactions += 1;
                send(
                    &tx,
                    AgentEvent::ContextCompacted {
                        before_chars,
                        after_chars,
                        retained_messages: compacted.len().min(KEEP_RECENT_MESSAGES),
                    },
                );
            }
            // Treat the provider boundary as a final protocol firewall. Even
            // histories produced by older builds are repaired before every
            // request, and an invariant check prevents an invalid transcript
            // from ever reaching MiniMax.
            let mut request_messages = reconcile_tool_history(compacted);
            if let Err(problem) = validate_tool_history(&request_messages) {
                self.finish_with_error(&state, &tx, format!("工具调用历史无法安全恢复：{problem}"))
                    .await;
                return;
            }
            // A transient system control keeps the stored user message clean
            // while applying changed controls on every turn and after resume.
            let directive = state.spec.runtime_directive();
            if let Some(system) = request_messages
                .iter_mut()
                .find(|message| message.role == "system")
            {
                let mut content = system.content_text().unwrap_or_default();
                content.push('\n');
                content.push_str(&directive);
                system.content = Some(Value::String(content));
            } else {
                request_messages.insert(0, ChatMessage::system(directive));
            }
            let mut request = ChatRequest::new(model.clone(), request_messages);
            // Official MiniMax guidance recommends separated reasoning for
            // OpenAI-compatible calls. We preserve `reasoning_details` on the
            // assistant message for protocol-complete tool chains while only
            // regular content is streamed to the UI.
            request
                .extra
                .insert("reasoning_split".to_string(), Value::Bool(true));
            if !last_round {
                let specs = self.tools.specs_for(state.spec.enabled_tools.as_deref());
                if !specs.is_empty() {
                    request = request.with_tools(specs);
                }
            }
            if let Some(t) = self.config.temperature {
                request = request.with_temperature(t);
            }
            send(
                &tx,
                AgentEvent::Progress {
                    phase: "reasoning".to_string(),
                    message: "正在分析已有证据并规划下一步".to_string(),
                    round: state.round + 1,
                    max_rounds,
                    completed: None,
                    total: None,
                },
            );

            let mut stream = match self.backend.chat_stream(&request).await {
                Ok(s) => s,
                Err(MinimaxError::QuotaExhausted { window_reset_at }) => {
                    self.suspend(state, &tx, window_reset_at).await;
                    return;
                }
                Err(e) => {
                    self.finish_with_error(&state, &tx, e.to_string()).await;
                    return;
                }
            };

            // Consume one model round: accumulate text and tool-call fragments.
            let mut text = String::new();
            let mut calls: Vec<ToolCall> = Vec::new();
            let mut reasoning_content: Option<String> = None;
            let mut reasoning_details: Option<Value> = None;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(chunk) => {
                        if let Some(delta) = chunk.raw_delta() {
                            if !delta.is_empty() {
                                text.push_str(&delta);
                                send(&tx, AgentEvent::TextDelta { text: delta });
                            }
                        }
                        if let Some(delta) = chunk
                            .choices
                            .first()
                            .and_then(|choice| choice.delta.as_ref())
                        {
                            if let Some(reasoning) = delta.reasoning_content.as_ref() {
                                reasoning_content = Some(reasoning.clone());
                            }
                            if let Some(details) = delta.reasoning_details.as_ref() {
                                reasoning_details = Some(details.clone());
                            }
                        }
                        for call in chunk
                            .choices
                            .first()
                            .and_then(|c| c.delta.as_ref())
                            .and_then(|d| d.tool_calls.as_deref())
                            .unwrap_or(&[])
                        {
                            merge_tool_call(&mut calls, call);
                        }
                    }
                    Err(MinimaxError::QuotaExhausted { window_reset_at }) => {
                        self.suspend(state, &tx, window_reset_at).await;
                        return;
                    }
                    Err(e) => {
                        self.finish_with_error(&state, &tx, e.to_string()).await;
                        return;
                    }
                }
            }

            // MiniMax requires every tool-call id to be unique. Providers can
            // occasionally reuse an id in a later round (or emit duplicate ids
            // at different streaming indexes), so normalize before persisting,
            // executing, or echoing any result. Unique valid provider ids stay
            // untouched; only invalid or globally repeated ids are rewritten.
            sanitize_streamed_tool_calls(&mut calls);
            normalize_new_tool_calls(&mut calls, &messages, state.round + 1);

            let (_, mut clean_text) = astock_minimax::split_reasoning(&text);
            let mut assistant = ChatMessage {
                role: "assistant".to_string(),
                // With reasoning_split=true this is regular user-facing text;
                // split_reasoning remains as a defensive fallback for models
                // that ignore the switch.
                content: if text.is_empty() {
                    None
                } else {
                    Some(Value::String(text.clone()))
                },
                reasoning_content,
                reasoning_details,
                tool_calls: if calls.is_empty() {
                    None
                } else {
                    Some(calls.clone())
                },
                ..Default::default()
            };

            let chart_required = explicitly_requests_chart(&state.spec.prompt);
            let invalid_chart = chart_required
                && (!clean_text.contains("```astock-chart")
                    || clean_text.contains("<script")
                    || clean_text.contains("```html"));
            if calls.is_empty() && (clean_text.trim().is_empty() || invalid_chart) {
                send(
                    &tx,
                    AgentEvent::Progress {
                        phase: "synthesizing".to_string(),
                        message: "模型草稿格式不符合展示要求，正在自动整理简洁结论".to_string(),
                        round: state.round + 1,
                        max_rounds,
                        completed: Some(0),
                        total: Some(1),
                    },
                );
                send(
                    &tx,
                    AgentEvent::TextReset {
                        message: "已自动纠正模型输出格式".to_string(),
                    },
                );
                match self
                    .recover_final_answer(&model, &messages, chart_required, &tx)
                    .await
                {
                    Ok((recovered, answer)) => {
                        assistant = recovered;
                        clean_text = answer;
                    }
                    Err(MinimaxError::QuotaExhausted { window_reset_at }) => {
                        self.suspend(state, &tx, window_reset_at).await;
                        return;
                    }
                    Err(e) => {
                        self.finish_with_error(
                            &state,
                            &tx,
                            format!("模型最终回答自动整理失败: {e}"),
                        )
                        .await;
                        return;
                    }
                }
            }
            // A plan-mode clarification is a user-input boundary, not an
            // analyst draft. Sending it through the specialist panel used to
            // trigger TextReset and made the questions flash then disappear.
            let awaiting_user_input = state.spec.research_mode.as_deref() == Some("plan")
                && is_clarification_request(&clean_text);
            let awaiting_specialist_review = calls.is_empty()
                && !awaiting_user_input
                && !state.multi_agent_reviewed
                && !state.spec.specialists.is_empty();
            // Search snippets are URL-discovery hints, never evidence. Add the
            // disclosure only to a durable final answer: not to a structured
            // clarification and not to an internal draft awaiting review.
            if calls.is_empty()
                && !awaiting_user_input
                && !awaiting_specialist_review
                && contains_discovery_only(&messages)
                && !contains_primary_source_evidence(&messages)
                && !clean_text.contains("原文未核验")
            {
                let disclosure = "\n\n**核验状态：** 一级来源原文未核验。本轮搜索标题与摘要仅作为发现线索，不标记为【事实】，也不据此确认重大新闻结论。";
                clean_text.push_str(disclosure);
                assistant.content = Some(Value::String(clean_text.clone()));
                send(
                    &tx,
                    AgentEvent::TextDelta {
                        text: disclosure.to_string(),
                    },
                );
            }
            if calls.is_empty() && !awaiting_user_input && !awaiting_specialist_review {
                let (blocked, downgraded) = tool_quality_gate_counts(&messages);
                if (blocked > 0 || downgraded > 0) && !clean_text.contains("数据质量门禁") {
                    let disclosure = if blocked > 0 {
                        format!(
                            "\n\n**数据质量门禁：** 有 {blocked} 项工具结果因硬过期、口径不兼容或未解决冲突被阻止用于确定性计算；另有 {downgraded} 项仅可作为中低置信参考。以上项目不得用于明确买卖结论。"
                        )
                    } else {
                        format!(
                            "\n\n**数据质量门禁：** 有 {downgraded} 项工具结果存在陈旧、缺失或尚未跨源复核，结论置信度已自动下调，不应据此单独调度资金。"
                        )
                    };
                    clean_text.push_str(&disclosure);
                    assistant.content = Some(Value::String(clean_text.clone()));
                    send(&tx, AgentEvent::TextDelta { text: disclosure });
                }
            }
            // A first draft awaiting specialist review is internal working
            // material, not a durable user-facing answer. Persist tool-call
            // messages and final/single-agent answers normally; the reviewed
            // branch below persists a hidden system packet instead.
            if !awaiting_specialist_review && !calls.is_empty() {
                if let Err(e) = append_message(
                    &self.ctx.storage,
                    &task_id,
                    &conversation_id,
                    &mut messages,
                    &assistant,
                )
                .await
                {
                    self.finish_with_error(&state, &tx, format!("storage: {e}"))
                        .await;
                    return;
                }
            }

            if calls.is_empty() {
                if clean_text.is_empty() {
                    self.finish_with_error(&state, &tx, "模型未产出最终回答".to_string())
                        .await;
                    return;
                }
                if awaiting_specialist_review {
                    send(
                        &tx,
                        AgentEvent::TextReset {
                            message: "主分析师初稿已形成，正在进入多专家独立复核".to_string(),
                        },
                    );
                    send(
                        &tx,
                        AgentEvent::Progress {
                            phase: "reviewing".to_string(),
                            message: format!(
                                "{} 位独立专家正在并行检查证据、风险与策略稳健性",
                                state.spec.specialists.len()
                            ),
                            round: state.round + 1,
                            max_rounds,
                            completed: Some(0),
                            total: Some(state.spec.specialists.len()),
                        },
                    );
                    let packet =
                        specialist_review_packet(&state.spec.prompt, &clean_text, &messages);
                    match self
                        .run_specialist_panel(
                            &state.spec.specialists,
                            &packet,
                            &model,
                            &tx,
                            state.round + 1,
                            max_rounds,
                        )
                        .await
                    {
                        Ok(review) => {
                            let review_message = ChatMessage::system(format!(
                                "【主分析师待修订初稿】\n{clean_text}\n\n【多Agent独立复核结果】\n{review}\n主分析师必须核对这些意见与工具证据，修正初稿后再输出最终结论；不得把专家意见当作新事实，也不得展示初稿、审查材料或私有推理。"
                            ));
                            if let Err(error) = append_message(
                                &self.ctx.storage,
                                &task_id,
                                &conversation_id,
                                &mut messages,
                                &review_message,
                            )
                            .await
                            {
                                self.finish_with_error(&state, &tx, format!("storage: {error}"))
                                    .await;
                                return;
                            }
                            state.multi_agent_reviewed = true;
                            state.round += 1;
                            if let Err(error) = self.save_state(&state, "running").await {
                                self.finish_with_error(&state, &tx, format!("storage: {error}"))
                                    .await;
                                return;
                            }
                            continue;
                        }
                        Err(MinimaxError::QuotaExhausted { window_reset_at }) => {
                            self.suspend(state, &tx, window_reset_at).await;
                            return;
                        }
                        Err(error) => {
                            tracing::warn!(%error, "specialist panel failed; completing from main evidence");
                            state.multi_agent_reviewed = true;
                        }
                    }
                }
                let mut report =
                    assemble_report(&task_id, &clean_text, state.evidence.clone(), now_secs());
                if !awaiting_user_input {
                    for attempt in 1..=2 {
                        if report.research.verification.passed() {
                            break;
                        }
                        send(
                            &tx,
                            AgentEvent::Progress {
                                phase: "verifying".to_string(),
                                message: format!(
                                    "独立校验发现 {} 项发布阻断，正在进行第 {attempt}/2 次证据内修订",
                                    report.research.verification.findings.len()
                                ),
                                round: state.round + 1,
                                max_rounds,
                                completed: Some(attempt - 1),
                                total: Some(2),
                            },
                        );
                        send(
                            &tx,
                            AgentEvent::TextReset {
                                message: "草稿未通过证据校验，正在按具体错误自动修订".to_string(),
                            },
                        );
                        match self
                            .recover_verified_answer(
                                &model,
                                &messages,
                                &clean_text,
                                &report.research.verification.repair_instructions(),
                                &tx,
                            )
                            .await
                        {
                            Ok((recovered, answer)) => {
                                assistant = recovered;
                                clean_text = answer;
                                report = assemble_report(
                                    &task_id,
                                    &clean_text,
                                    state.evidence.clone(),
                                    now_secs(),
                                );
                            }
                            Err(MinimaxError::QuotaExhausted { window_reset_at }) => {
                                self.suspend(state, &tx, window_reset_at).await;
                                return;
                            }
                            Err(error) => {
                                tracing::warn!(%error, attempt, "verified-answer repair failed");
                                break;
                            }
                        }
                    }
                }
                let publication_blocked =
                    report.research.verification.status == VerificationStatus::Failed;
                if publication_blocked {
                    clean_text = format!(
                        "## 报告未通过证据校验\n\n本轮草稿已被阻止发布，共发现 {} 项需要修正的问题。你可以展开下方“结论与证据校验”查看具体字段、错误原因和证据缺口；补充或刷新数据后可继续本任务。",
                        report.research.verification.findings.len()
                    );
                    report.answer = clean_text.clone();
                    assistant = ChatMessage::assistant(clean_text.clone());
                }
                if let Err(error) = append_message(
                    &self.ctx.storage,
                    &task_id,
                    &conversation_id,
                    &mut messages,
                    &assistant,
                )
                .await
                {
                    self.finish_with_error(&state, &tx, format!("storage: {error}"))
                        .await;
                    return;
                }
                if !awaiting_user_input {
                    self.link_news_evidence(&task_id, &messages).await;
                }
                if let Err(error) = self.persist_report(&report).await {
                    self.finish_with_error(&state, &tx, format!("研究报告保存失败: {error}"))
                        .await;
                    return;
                }
                let terminal_status = if publication_blocked {
                    "verification_failed"
                } else {
                    "completed"
                };
                let _ = self.save_state(&state, terminal_status).await;
                send(
                    &tx,
                    AgentEvent::Progress {
                        phase: "synthesizing".to_string(),
                        message: if awaiting_user_input {
                            "需要你确认关键条件，已生成可选择的问题卡片".to_string()
                        } else if publication_blocked {
                            "报告未通过独立证据校验，草稿已阻止发布".to_string()
                        } else {
                            "证据核验完成，正在生成最终结论".to_string()
                        },
                        round: state.round + 1,
                        max_rounds,
                        completed: Some(1),
                        total: Some(1),
                    },
                );
                send(
                    &tx,
                    AgentEvent::Completed {
                        report: Box::new(report),
                    },
                );
                return;
            }

            if last_round {
                self.finish_with_error(
                    &state,
                    &tx,
                    format!("超过最大轮数 {max_rounds}：模型仍在请求工具"),
                )
                .await;
                return;
            }

            // Execute this round's tool calls with bounded concurrency.
            send(
                &tx,
                AgentEvent::Progress {
                    phase: "tools".to_string(),
                    message: format!("本轮计划执行 {} 项确定性分析", calls.len()),
                    round: state.round + 1,
                    max_rounds,
                    completed: Some(0),
                    total: Some(calls.len()),
                },
            );
            let executed = match self
                .execute_round(
                    &calls,
                    &tx,
                    state.round + 1,
                    max_rounds,
                    state.spec.enabled_tools.as_deref(),
                    &task_id,
                )
                .await
            {
                Ok(executed) => executed,
                Err(AgentError::Cancelled(_)) => {
                    // `agent_cancel` has already persisted the cancelled
                    // status. Dropping the round future cancels every other
                    // in-flight tool in the batch.
                    send(
                        &tx,
                        AgentEvent::Failed {
                            error: "任务已取消".to_string(),
                        },
                    );
                    return;
                }
                Err(error) => {
                    self.finish_with_error(&state, &tx, error.to_string()).await;
                    return;
                }
            };
            if let Err(error) = self.check_cancelled(&task_id).await {
                match error {
                    AgentError::Cancelled(_) => {
                        send(
                            &tx,
                            AgentEvent::Failed {
                                error: "任务已取消".to_string(),
                            },
                        );
                    }
                    other => {
                        self.finish_with_error(&state, &tx, other.to_string()).await;
                    }
                }
                return;
            }
            for exec in executed {
                if let Some(evidence) = exec.evidence.clone() {
                    state.evidence.push(evidence);
                }
                let message = ChatMessage::tool_result(exec.call_id, exec.message_content);
                if let Err(e) = append_message(
                    &self.ctx.storage,
                    &task_id,
                    &conversation_id,
                    &mut messages,
                    &message,
                )
                .await
                {
                    self.finish_with_error(&state, &tx, format!("storage: {e}"))
                        .await;
                    return;
                }
            }

            state.round += 1;
            if let Err(e) = self.save_state(&state, "running").await {
                send(
                    &tx,
                    AgentEvent::Failed {
                        error: format!("storage: {e}"),
                    },
                );
                return;
            }
        }
    }

    /// Link every immutable news revision present in successful tool results
    /// to the final report. Archive diagnostics are best-effort and cannot
    /// discard an otherwise valid analysis.
    async fn link_news_evidence(&self, task_id: &str, messages: &[ChatMessage]) {
        for revision_id in news_revision_ids(messages) {
            if let Err(error) = self
                .ctx
                .storage
                .news_agent_evidence_link(task_id, "final_answer", &revision_id)
                .await
            {
                tracing::warn!(task_id, revision_id, %error, "agent news evidence link failed");
            }
        }
        let verifier = astock_source_verification::SourceVerifier::new(self.ctx.storage.clone());
        for (source_version_id, fact_id) in source_evidence_pairs(messages) {
            if let Err(error) = verifier
                .link_agent_evidence(
                    task_id,
                    "final_answer",
                    &source_version_id,
                    (!fact_id.is_empty()).then_some(fact_id.as_str()),
                )
                .await
            {
                tracing::warn!(task_id, source_version_id, fact_id, %error, "agent source evidence link failed");
            }
        }
    }

    async fn run_specialist_panel(
        &self,
        specialists: &[SpecialistRoute],
        packet: &str,
        fallback_model: &str,
        tx: &mpsc::UnboundedSender<AgentEvent>,
        round: u32,
        max_rounds: u32,
    ) -> std::result::Result<String, MinimaxError> {
        let total = specialists.len();
        let mut pending = futures::stream::iter(specialists.iter().cloned().enumerate())
            .map(|(index, specialist)| async move {
                let model = specialist
                    .model
                    .clone()
                    .unwrap_or_else(|| fallback_model.to_string());
                let messages = vec![
                    ChatMessage::system(format!(
                        "你是隔离运行的{}。{} 你不能调用工具，不得补造数字，只能审查所给材料；输出不超过500字的结构化中文审查意见。",
                        specialist.name, specialist.instruction
                    )),
                    ChatMessage::user(packet.to_string()),
                ];
                let mut request = ChatRequest::new(model, messages).with_temperature(0.1);
                request
                    .extra
                    .insert("reasoning_split".to_string(), Value::Bool(true));
                request
                    .extra
                    .insert("max_completion_tokens".to_string(), json!(1600));
                let mut stream = self.backend.chat_stream(&request).await?;
                let mut text = String::new();
                while let Some(chunk) = stream.next().await {
                    if let Some(delta) = chunk?.raw_delta() {
                        text.push_str(&delta);
                    }
                }
                let (_, visible) = astock_minimax::split_reasoning(&text);
                if visible.trim().is_empty() {
                    return Err(MinimaxError::Parse(format!(
                        "{}未返回可见审查意见",
                        specialist.name
                    )));
                }
                Ok::<_, MinimaxError>((index, specialist.name, visible))
            })
            .buffer_unordered(total.clamp(1, 4));

        let mut reviews = Vec::with_capacity(total);
        while let Some(result) = pending.next().await {
            let item = result?;
            reviews.push(item);
            send(
                tx,
                AgentEvent::Progress {
                    phase: "reviewing".to_string(),
                    message: format!("多专家复核已完成 {} / {total}", reviews.len()),
                    round,
                    max_rounds,
                    completed: Some(reviews.len()),
                    total: Some(total),
                },
            );
        }
        reviews.sort_by_key(|(index, _, _)| *index);
        Ok(reviews
            .into_iter()
            .map(|(_, name, review)| format!("### {name}\n{review}"))
            .collect::<Vec<_>>()
            .join("\n\n"))
    }

    /// One transparent, tool-free repair pass for a truncated answer or an
    /// unsafe chart format. MiniMax-M3 can disable thinking for this pass, so
    /// the small output budget is spent on the final answer rather than a new
    /// analysis. M2.x accepts the field but may keep thinking enabled.
    async fn recover_final_answer(
        &self,
        model: &str,
        messages: &[ChatMessage],
        chart_required: bool,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> std::result::Result<(ChatMessage, String), MinimaxError> {
        let chart_rule = if chart_required {
            "必须包含一张```astock-chart围栏图；围栏内只能是title、unit、x、series字段的JSON，禁止HTML、JavaScript和ECharts配置。"
        } else {
            "只有证据适合时间序列或横向比较时才输出astock-chart。"
        };
        let repair_instruction = format!(
            "【系统自动整理】已有证据足够，不再调用工具。请直接输出给普通股民看的最终中文回答，控制在1200字内。先一句话结论，再给关键证据、反方证据、风险和下一步；不得展示思考过程。{chart_rule}"
        );
        let mut repair_messages = messages.to_vec();
        repair_messages.push(ChatMessage::user(repair_instruction));
        let mut request = ChatRequest::new(model.to_string(), repair_messages);
        request
            .extra
            .insert("reasoning_split".to_string(), Value::Bool(true));
        request
            .extra
            .insert("thinking".to_string(), json!({ "type": "disabled" }));
        request
            .extra
            .insert("max_completion_tokens".to_string(), json!(4096));
        request = request.with_temperature(0.2);

        let mut stream = self.backend.chat_stream(&request).await?;
        let mut text = String::new();
        let mut reasoning_content: Option<String> = None;
        let mut reasoning_details: Option<Value> = None;
        while let Some(item) = stream.next().await {
            let chunk = item?;
            if let Some(delta) = chunk.raw_delta() {
                if !delta.is_empty() {
                    text.push_str(&delta);
                    send(tx, AgentEvent::TextDelta { text: delta });
                }
            }
            if let Some(delta) = chunk
                .choices
                .first()
                .and_then(|choice| choice.delta.as_ref())
            {
                if let Some(reasoning) = delta.reasoning_content.as_ref() {
                    reasoning_content = Some(reasoning.clone());
                }
                if let Some(details) = delta.reasoning_details.as_ref() {
                    reasoning_details = Some(details.clone());
                }
            }
        }
        let (_, answer) = astock_minimax::split_reasoning(&text);
        if answer.trim().is_empty() {
            return Err(MinimaxError::Parse(
                "自动整理后仍未产出可见回答".to_string(),
            ));
        }
        if chart_required
            && (!answer.contains("```astock-chart")
                || answer.contains("<script")
                || answer.contains("```html"))
        {
            return Err(MinimaxError::Parse(
                "自动整理后图表仍不符合安全协议".to_string(),
            ));
        }
        Ok((
            ChatMessage {
                role: "assistant".to_string(),
                content: Some(Value::String(text)),
                reasoning_content,
                reasoning_details,
                ..Default::default()
            },
            answer,
        ))
    }

    /// Repair only the publication-contract violations reported by the
    /// deterministic verifier. No tools are exposed during this pass: the
    /// model must use the already indexed evidence or explicitly downgrade a
    /// statement to an assumption/unknown and remove unsupported numbers.
    async fn recover_verified_answer(
        &self,
        model: &str,
        messages: &[ChatMessage],
        draft: &str,
        verification_errors: &str,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> std::result::Result<(ChatMessage, String), MinimaxError> {
        let mut repair_messages = messages.to_vec();
        repair_messages.push(ChatMessage::user(format!(
            "【独立发布校验失败】\n以下是未发布草稿：\n---\n{draft}\n---\n校验器错误：\n{verification_errors}\n\n只基于已有工具消息中的evidence字段修订全文，不得调用工具、猜测或换一个数字规避错误。每条关键结论使用【事实】【计算】【外部】【推断】【假设】【未知】之一；数字后精确写〔证据:evf_xxx〕，确定性计算同时写〔计算引用:calc_xxx〕。无法支持的数字必须删除，无法确认的结论明确写【未知】。保留反方证据、冲突和失效条件。直接输出修订后的完整中文报告，不解释修订过程。"
        )));
        let mut request =
            ChatRequest::new(model.to_string(), repair_messages).with_temperature(0.1);
        request
            .extra
            .insert("reasoning_split".to_string(), Value::Bool(true));
        request
            .extra
            .insert("thinking".to_string(), json!({ "type": "disabled" }));
        request
            .extra
            .insert("max_completion_tokens".to_string(), json!(4096));

        let mut stream = self.backend.chat_stream(&request).await?;
        let mut text = String::new();
        let mut reasoning_content = None;
        let mut reasoning_details = None;
        while let Some(item) = stream.next().await {
            let chunk = item?;
            if let Some(delta) = chunk.raw_delta() {
                if !delta.is_empty() {
                    text.push_str(&delta);
                    send(tx, AgentEvent::TextDelta { text: delta });
                }
            }
            if let Some(delta) = chunk
                .choices
                .first()
                .and_then(|choice| choice.delta.as_ref())
            {
                if let Some(reasoning) = delta.reasoning_content.as_ref() {
                    reasoning_content = Some(reasoning.clone());
                }
                if let Some(details) = delta.reasoning_details.as_ref() {
                    reasoning_details = Some(details.clone());
                }
            }
        }
        let (_, answer) = astock_minimax::split_reasoning(&text);
        if answer.trim().is_empty() {
            return Err(MinimaxError::Parse(
                "证据校验修订没有产出可见回答".to_string(),
            ));
        }
        Ok((
            ChatMessage {
                role: "assistant".to_string(),
                content: Some(Value::String(text)),
                reasoning_content,
                reasoning_details,
                ..Default::default()
            },
            answer,
        ))
    }

    /// Execute one round of tool calls (bounded concurrency), in call order.
    async fn execute_round(
        &self,
        calls: &[ToolCall],
        tx: &mpsc::UnboundedSender<AgentEvent>,
        round: u32,
        max_rounds: u32,
        enabled_tools: Option<&[String]>,
        task_id: &str,
    ) -> Result<Vec<ToolExec>> {
        let total = calls.len();
        let mut pending = futures::stream::iter(calls.iter().cloned().enumerate())
            .map(|(idx, call)| async move {
                Ok::<_, AgentError>((
                    idx,
                    self.execute_one(call, idx + 1, total, tx, enabled_tools, task_id)
                        .await?,
                ))
            })
            .buffer_unordered(self.config.max_parallel_tools.max(1));
        let mut indexed: Vec<(usize, ToolExec)> = Vec::with_capacity(total);
        while let Some(item) = pending.next().await {
            indexed.push(item?);
            send(
                tx,
                AgentEvent::Progress {
                    phase: "tools".to_string(),
                    message: format!("已完成 {} / {} 项分析", indexed.len(), total),
                    round,
                    max_rounds,
                    completed: Some(indexed.len()),
                    total: Some(total),
                },
            );
        }
        indexed.sort_by_key(|(idx, _)| *idx);
        Ok(indexed.into_iter().map(|(_, r)| r).collect())
    }

    /// Execute a single tool call; tool failures become error payloads fed
    /// back to the model (the loop survives bad calls).
    async fn execute_one(
        &self,
        call: ToolCall,
        position: usize,
        total: usize,
        tx: &mpsc::UnboundedSender<AgentEvent>,
        enabled_tools: Option<&[String]>,
        task_id: &str,
    ) -> Result<ToolExec> {
        let call_id = call.id.clone().unwrap_or_else(|| "call_0".to_string());
        let name = call
            .function
            .as_ref()
            .and_then(|f| f.name.clone())
            .unwrap_or_default();
        let raw_args = call
            .function
            .as_ref()
            .and_then(|f| f.arguments.clone())
            .unwrap_or_default();
        let args: Value = if raw_args.trim().is_empty() {
            json!({})
        } else {
            match serde_json::from_str(&raw_args) {
                Ok(v) => v,
                Err(e) => {
                    let error = format!("参数不是合法JSON: {e}");
                    let permission_domain = self
                        .tools
                        .permission_domain(&name)
                        .unwrap_or(ToolPermissionDomain::ReadOnlyNetwork);
                    let args_fingerprint = fingerprint_json(&json!({ "raw": &raw_args }));
                    let audit = ToolAuditMeta {
                        task_id,
                        call_id: &call_id,
                        tool: &name,
                        permission_domain,
                        args_fingerprint: &args_fingerprint,
                    };
                    self.append_tool_audit(&audit, "invalid_arguments", Some(0))
                        .await;
                    send(
                        tx,
                        AgentEvent::ToolCallStarted {
                            call_id: call_id.clone(),
                            name: name.clone(),
                            args: json!({ "raw": &raw_args }),
                            position,
                            total,
                            estimated_ms: tool_estimated_secs(&name) * 1000,
                        },
                    );
                    send(
                        tx,
                        AgentEvent::ToolCallFinished {
                            call_id: call_id.clone(),
                            name: name.clone(),
                            cache_key: String::new(),
                            elapsed_ms: 0,
                            success: false,
                            source: None,
                            fetched_at: None,
                            error: Some(error.clone()),
                        },
                    );
                    return Ok(ToolExec {
                        call_id,
                        evidence: None,
                        message_content: json!({
                            "tool": name,
                            "error": error,
                        })
                        .to_string(),
                    });
                }
            }
        };

        let enabled_by_user =
            enabled_tools.is_none_or(|allowed| allowed.iter().any(|item| item == &name));
        let permission_domain = self
            .tools
            .permission_domain(&name)
            .unwrap_or(ToolPermissionDomain::ReadOnlyNetwork);
        let args_fingerprint = fingerprint_json(&args);
        let audit = ToolAuditMeta {
            task_id,
            call_id: &call_id,
            tool: &name,
            permission_domain,
            args_fingerprint: &args_fingerprint,
        };
        tracing::info!(
            target: "astock::agent_tool_audit",
            task_id,
            call_id,
            tool = name,
            permission_domain = %permission_domain,
            origin = "model_plan",
            args_fingerprint,
            event = "requested",
            "Agent tool audit"
        );
        self.append_tool_audit(&audit, "requested", None).await;
        send(
            tx,
            AgentEvent::ToolCallStarted {
                call_id: call_id.clone(),
                name: name.clone(),
                args: args.clone(),
                position,
                total,
                estimated_ms: tool_estimated_secs(&name) * 1000,
            },
        );
        if let Err(reason) = authorize_tool(
            permission_domain,
            InvocationOrigin::ModelPlan,
            enabled_by_user,
        ) {
            let error = if enabled_by_user {
                format!("工具 {name} 的权限请求被安全策略拒绝：{reason}")
            } else {
                format!("工具 {name} 已被用户在本轮研究设置中关闭")
            };
            tracing::warn!(
                target: "astock::agent_tool_audit",
                task_id,
                call_id,
                tool = name,
                permission_domain = %permission_domain,
                origin = "model_plan",
                args_fingerprint,
                event = "denied",
                reason = %reason,
                "Agent tool audit"
            );
            self.append_tool_audit(&audit, "denied", Some(0)).await;
            send(
                tx,
                AgentEvent::ToolCallFinished {
                    call_id: call_id.clone(),
                    name: name.clone(),
                    cache_key: String::new(),
                    elapsed_ms: 0,
                    success: false,
                    source: None,
                    fetched_at: None,
                    error: Some(error.clone()),
                },
            );
            return Ok(ToolExec {
                call_id,
                evidence: None,
                message_content: json!({ "tool": name, "error": error }).to_string(),
            });
        }
        let started = Instant::now();
        // Tools have no orchestration deadline. They run asynchronously until
        // they finish, fail at their own provider boundary, or the user
        // explicitly cancels the durable task. The estimate is UI guidance
        // only and never participates in control flow.
        let estimated_ms = tool_estimated_secs(&name) * 1000;
        let progress_tx = tx.clone();
        let progress_call_id = call_id.clone();
        let progress_name = name.clone();
        let progress_started = started;
        let dispatch_context = self
            .ctx
            .clone()
            .with_progress_reporter(Arc::new(move |detail| {
                let stage = format!(
                    "已处理 {}/{}，当前并行 {} 项，成功 {} 项，失败 {} 项",
                    detail.completed,
                    detail.total,
                    detail.active.len(),
                    detail.succeeded,
                    detail.failed
                );
                send(
                    &progress_tx,
                    AgentEvent::ToolCallProgress {
                        call_id: progress_call_id.clone(),
                        name: progress_name.clone(),
                        elapsed_ms: progress_started.elapsed().as_millis() as u64,
                        estimated_ms,
                        stage,
                        detail: Some(detail),
                    },
                );
            }));
        let dispatch = self.tools.dispatch(&name, args, &dispatch_context);
        tokio::pin!(dispatch);
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(2));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Consume the immediate first tick: ToolCallStarted already describes it.
        heartbeat.tick().await;
        let outcome = loop {
            tokio::select! {
                result = &mut dispatch => break result,
                _ = heartbeat.tick() => {
                    let elapsed_ms = started.elapsed().as_millis() as u64;
                    self.check_cancelled(task_id).await?;
                    send(tx, AgentEvent::ToolCallProgress {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        elapsed_ms,
                        estimated_ms,
                        stage: tool_progress_stage(&name, elapsed_ms, estimated_ms).to_string(),
                        detail: None,
                    });
                }
            }
        };
        let elapsed_ms = started.elapsed().as_millis() as u64;

        match outcome {
            Ok(result) => {
                let evidence = index_tool_evidence(
                    &name,
                    &result.cache_key,
                    &result.source,
                    &result.fetched_at,
                    &result.summary_json,
                );
                tracing::info!(
                    target: "astock::agent_tool_audit",
                    task_id,
                    call_id,
                    tool = name,
                    permission_domain = %permission_domain,
                    origin = "model_plan",
                    args_fingerprint,
                    event = "succeeded",
                    elapsed_ms,
                    "Agent tool audit"
                );
                self.append_tool_audit(&audit, "succeeded", Some(elapsed_ms))
                    .await;
                send(
                    tx,
                    AgentEvent::ToolCallFinished {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        cache_key: result.cache_key.clone(),
                        elapsed_ms,
                        success: true,
                        source: Some(result.source.clone()),
                        fetched_at: Some(result.fetched_at.clone()),
                        error: None,
                    },
                );
                Ok(ToolExec {
                    call_id,
                    evidence: Some(evidence.clone()),
                    message_content: json!({
                        "tool": name,
                        "cache_key": result.cache_key,
                        "source": result.source,
                        "fetched_at": result.fetched_at,
                        "summary": result.summary_json,
                        "evidence": evidence,
                    })
                    .to_string(),
                })
            }
            Err(e) => {
                let error = e.to_string();
                tracing::warn!(
                    target: "astock::agent_tool_audit",
                    task_id,
                    call_id,
                    tool = name,
                    permission_domain = %permission_domain,
                    origin = "model_plan",
                    args_fingerprint,
                    event = "failed",
                    elapsed_ms,
                    error_kind = %std::any::type_name_of_val(&e),
                    "Agent tool audit"
                );
                self.append_tool_audit(&audit, "failed", Some(elapsed_ms))
                    .await;
                send(
                    tx,
                    AgentEvent::ToolCallFinished {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        cache_key: String::new(),
                        elapsed_ms,
                        success: false,
                        source: None,
                        fetched_at: None,
                        error: Some(error.clone()),
                    },
                );
                Ok(ToolExec {
                    call_id,
                    evidence: None,
                    message_content: json!({
                        "tool": name,
                        "error": error,
                    })
                    .to_string(),
                })
            }
        }
    }

    /// Persist only bounded metadata. Audit failure never blocks the user's
    /// analysis, and the fallback log deliberately omits the storage error
    /// text because it can contain machine-local paths.
    async fn append_tool_audit(
        &self,
        meta: &ToolAuditMeta<'_>,
        event: &str,
        elapsed_ms: Option<u64>,
    ) {
        let audit = AgentToolAudit {
            id: None,
            task_id: meta.task_id.to_string(),
            call_id: meta.call_id.to_string(),
            tool: meta.tool.to_string(),
            permission_domain: meta.permission_domain.to_string(),
            origin: "model_plan".to_string(),
            args_fingerprint: meta.args_fingerprint.to_string(),
            event: event.to_string(),
            elapsed_ms: elapsed_ms.map(|value| value.min(i64::MAX as u64) as i64),
            created_at: now_secs(),
        };
        if self
            .ctx
            .storage
            .agent_tool_audit_append(audit)
            .await
            .is_err()
        {
            tracing::warn!(
                target: "astock::agent_tool_audit",
                task_id = meta.task_id,
                call_id = meta.call_id,
                tool = meta.tool,
                event,
                "Agent tool audit persistence failed"
            );
        }
    }

    async fn resolve_model(&self, spec: &TaskSpec) -> std::result::Result<String, MinimaxError> {
        if let Some(m) = spec.model.clone().or_else(|| self.config.model.clone()) {
            return Ok(m);
        }
        self.backend.selected_model().await
    }

    async fn check_cancelled(&self, task_id: &str) -> Result<()> {
        let record = self.ctx.storage.agent_task_get(task_id).await?;
        if record.map(|r| r.status == "cancelled").unwrap_or(false) {
            return Err(AgentError::Cancelled(task_id.to_string()));
        }
        Ok(())
    }

    async fn save_state(&self, state: &TaskState, status: &str) -> Result<()> {
        // Never resurrect a task after an external cancellation races with a
        // round completion. The storage status is the durable source of truth.
        if status != "cancelled"
            && self
                .ctx
                .storage
                .agent_task_get(&state.spec.id)
                .await?
                .is_some_and(|task| task.status == "cancelled")
        {
            return Err(AgentError::Cancelled(state.spec.id.clone()));
        }
        let now = now_secs();
        self.ctx
            .storage
            .agent_task_save(AgentTask {
                id: state.spec.id.clone(),
                kind: state.spec.kind.clone(),
                status: status.to_string(),
                state_json: serde_json::to_string(state)?,
                created_at: now,
                updated_at: now,
            })
            .await?;
        Ok(())
    }

    /// Persist the complete report, including verification findings and exact
    /// tool/data versions, before the completion event reaches the UI.
    async fn persist_report(&self, report: &AgentReport) -> Result<()> {
        let (tool_versions, data_versions) = report_versions(report);
        let content = json!({
            "report": report,
            "tool_versions": tool_versions,
            "data_versions": data_versions,
            "verification": &report.research.verification,
        });
        self.ctx
            .storage
            .reports_insert(StoredReport {
                id: report.task_id.clone(),
                kind: if report.research.verification.passed() {
                    "verified-research".to_string()
                } else {
                    "verification-blocked".to_string()
                },
                title: format!("AI研究报告 · {}", report.task_id),
                content_json: serde_json::to_string(&content)?,
                created_at: report.generated_at,
            })
            .await?;
        Ok(())
    }

    /// Suspend the task: persist state and emit `Suspended`.
    async fn suspend(
        &self,
        state: TaskState,
        tx: &mpsc::UnboundedSender<AgentEvent>,
        reset_at: Option<std::time::SystemTime>,
    ) {
        let _ = self.save_state(&state, "suspended").await;
        let reset_at_unix = reset_at.and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs())
        });
        send(
            tx,
            AgentEvent::Suspended {
                reason: SuspendReason::QuotaExhausted { reset_at_unix },
            },
        );
    }

    /// Persist the failure status and emit `Failed`.
    async fn finish_with_error(
        &self,
        state: &TaskState,
        tx: &mpsc::UnboundedSender<AgentEvent>,
        error: String,
    ) {
        let mut failed_state = state.clone();
        failed_state.last_error = Some(error.clone());
        let _ = self.save_state(&failed_state, "failed").await;
        send(tx, AgentEvent::Failed { error });
    }
}

fn specialist_review_packet(prompt: &str, draft: &str, messages: &[ChatMessage]) -> String {
    let mut evidence = String::new();
    let tool_messages: Vec<&ChatMessage> = messages
        .iter()
        .filter(|message| message.role == "tool")
        .collect();
    let start = tool_messages.len().saturating_sub(16);
    for message in &tool_messages[start..] {
        if let Some(content) = message.content_text() {
            evidence.push_str(&content);
            evidence.push('\n');
        }
    }
    let evidence: String = evidence.chars().take(18_000).collect();
    let draft: String = draft.chars().take(8_000).collect();
    format!(
        "【用户问题】\n{prompt}\n\n【主分析师初稿】\n{draft}\n\n【确定性工具结果摘要】\n{evidence}\n请只指出：证据是否支持、冲突或遗漏、反例、风险、需要主分析师修正之处。"
    )
}

fn explicitly_requests_chart(prompt: &str) -> bool {
    ["画图", "图表", "折线图", "柱状图", "走势图", "交互图"]
        .iter()
        .any(|needle| prompt.contains(needle))
}

/// Recognize both the structured selection protocol and the legacy numbered
/// Markdown format. The latter keeps older/provider-deviating answers from
/// entering specialist review and disappearing from the UI.
fn is_clarification_request(answer: &str) -> bool {
    if answer.contains("```astock-questions") {
        return true;
    }
    let mut question_count = 0_usize;
    let mut option_count = 0_usize;
    for line in answer.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("• ") {
            option_count += 1;
        }
        let plain = trimmed.trim_matches('*').trim_start_matches('#').trim();
        let numbered_question = plain.find(['.', '、', ')']).is_some_and(|separator| {
            let separator_len = plain[separator..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(0);
            let number = &plain[..separator];
            let text = &plain[separator + separator_len..];
            !number.is_empty()
                && number.chars().all(|ch| ch.is_ascii_digit())
                && (text.trim_end().ends_with('?') || text.trim_end().ends_with('？'))
        });
        if numbered_question {
            question_count += 1;
        }
    }
    (1..=3).contains(&question_count) && option_count >= question_count * 2
}

fn news_revision_ids(messages: &[ChatMessage]) -> BTreeSet<String> {
    fn collect(value: &Value, output: &mut BTreeSet<String>) {
        match value {
            Value::Object(fields) => {
                if let Some(id) = fields.get("document_revision_id").and_then(Value::as_str) {
                    if id.starts_with("rev:") {
                        output.insert(id.to_string());
                    }
                }
                for value in fields.values() {
                    collect(value, output);
                }
            }
            Value::Array(values) => {
                for value in values {
                    collect(value, output);
                }
            }
            Value::String(text) => {
                if let Ok(decoded) = serde_json::from_str::<Value>(text) {
                    collect(&decoded, output);
                }
            }
            _ => {}
        }
    }

    let mut output = BTreeSet::new();
    for message in messages {
        if let Some(content) = &message.content {
            collect(content, &mut output);
        }
    }
    output
}

fn source_evidence_pairs(messages: &[ChatMessage]) -> BTreeSet<(String, String)> {
    fn collect(value: &Value, output: &mut BTreeSet<(String, String)>) {
        match value {
            Value::Object(fields) => {
                if let Some(version) = fields.get("source_version_id").and_then(Value::as_str) {
                    if version.starts_with("srcver:") {
                        let fact = fields
                            .get("fact_id")
                            .and_then(Value::as_str)
                            .filter(|fact| fact.starts_with("fact:"))
                            .unwrap_or_default();
                        output.insert((version.to_string(), fact.to_string()));
                    }
                }
                for value in fields.values() {
                    collect(value, output);
                }
            }
            Value::Array(values) => {
                for value in values {
                    collect(value, output);
                }
            }
            Value::String(text) => {
                if let Ok(decoded) = serde_json::from_str::<Value>(text) {
                    collect(&decoded, output);
                }
            }
            _ => {}
        }
    }
    let mut output = BTreeSet::new();
    for message in messages {
        if let Some(content) = &message.content {
            collect(content, &mut output);
        }
    }
    output
}

fn contains_discovery_only(messages: &[ChatMessage]) -> bool {
    messages.iter().any(|message| {
        message
            .content
            .as_ref()
            .is_some_and(|content| content.to_string().contains("discovery_only"))
    })
}

fn contains_primary_source_evidence(messages: &[ChatMessage]) -> bool {
    fn inspect(value: &Value, has_primary: &mut bool, has_version: &mut bool) {
        match value {
            Value::Object(fields) => {
                *has_primary |= fields
                    .get("is_primary_source")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                *has_version |= fields
                    .get("source_version_id")
                    .and_then(Value::as_str)
                    .is_some_and(|version| version.starts_with("srcver:"));
                for value in fields.values() {
                    inspect(value, has_primary, has_version);
                }
            }
            Value::Array(values) => {
                for value in values {
                    inspect(value, has_primary, has_version);
                }
            }
            Value::String(text) => {
                if let Ok(decoded) = serde_json::from_str::<Value>(text) {
                    inspect(&decoded, has_primary, has_version);
                }
            }
            _ => {}
        }
    }

    messages.iter().any(|message| {
        let mut has_primary = false;
        let mut has_version = false;
        if let Some(content) = &message.content {
            inspect(content, &mut has_primary, &mut has_version);
        }
        has_primary && has_version
    })
}

fn tool_quality_gate_counts(messages: &[ChatMessage]) -> (usize, usize) {
    fn inspect(value: &Value, blocked: &mut usize, downgraded: &mut usize) {
        match value {
            Value::Object(fields) => {
                if let Some(quality) = fields.get("data_quality").and_then(Value::as_object) {
                    let deterministic = quality
                        .get("allow_deterministic_compute")
                        .and_then(Value::as_bool)
                        .unwrap_or(true);
                    let high_confidence = quality
                        .get("allow_high_confidence")
                        .and_then(Value::as_bool)
                        .unwrap_or(true);
                    if !deterministic {
                        *blocked += 1;
                    } else if !high_confidence {
                        *downgraded += 1;
                    }
                    return;
                }
                for child in fields.values() {
                    inspect(child, blocked, downgraded);
                }
            }
            Value::Array(values) => {
                for child in values {
                    inspect(child, blocked, downgraded);
                }
            }
            Value::String(text) => {
                if let Ok(decoded) = serde_json::from_str::<Value>(text) {
                    inspect(&decoded, blocked, downgraded);
                }
            }
            _ => {}
        }
    }

    let mut blocked = 0;
    let mut downgraded = 0;
    for message in messages.iter().filter(|message| message.role == "tool") {
        if let Some(content) = &message.content {
            inspect(content, &mut blocked, &mut downgraded);
        }
    }
    (blocked, downgraded)
}

/// Typical duration shown to the user. This value is deliberately not used as
/// a deadline: every tool keeps running until it completes or the user cancels
/// the durable task.
fn tool_estimated_secs(name: &str) -> u64 {
    match name {
        "scan_market" | "run_backtest" | "iterate_strategy" | "run_joinquant_research" => 180,
        "get_fundamentals"
        | "run_valuation"
        | "analyze_earnings_drivers"
        | "compare_stocks"
        | "research_news"
        | "research_disclosures"
        | "research_global_transmission"
        | "analyze_event_price_in"
        | "research_supply_chain_relations"
        | "query_graph_as_of"
        | "search_web"
        | "fetch_source_document" => 60,
        _ => 45,
    }
}

fn tool_progress_stage(name: &str, elapsed_ms: u64, estimated_ms: u64) -> &'static str {
    if elapsed_ms > estimated_ms {
        return "已超过预估时间，仍在后台继续，可随时取消";
    }
    if elapsed_ms < 2_500 {
        return "检查本地缓存并选择可用数据源";
    }
    match name {
        "compare_stocks" => "并行获取各标的数据，已完成结果会立即保留",
        "run_full_analysis" => "汇总行情、资金与市场环境并运行信号引擎",
        "get_fundamentals" | "run_valuation" => "读取财务报表并校验关键字段",
        "analyze_earnings_drivers" => "连接经营驱动、利润表、现金流与估值，并传播参数区间",
        "run_backtest" | "iterate_strategy" => "执行有上限的历史计算与稳健性检验",
        "run_joinquant_research" => "等待聚宽研究环境并执行受限数据模板",
        "research_news" => "并行读取多家财经快讯并核验可用的个股事件",
        "research_disclosures" => "查询正式披露、修订链、附件与原文核验状态",
        "research_global_transmission" => "核验海外一级来源、原时区/币种与逐边 A 股传导证据",
        "analyze_event_price_in" => "逐字段核验事件，并分离基本面影响与市场 price-in",
        "research_supply_chain_relations" => "抽取并核验供应链关系候选，只使用已审核发布关系",
        "query_graph_as_of" => "按业务时间与当时知悉时间重建历史图谱快照",
        "search_web" => "通过 MiniMax 联网检索权威来源并保留原始链接",
        "fetch_source_document" => "正在安全打开原始页面并提取页码、段落、原值与单位",
        "read_document" => "读取不可变文档版本与字段级证据",
        "compare_source_evidence" => "逐字段比较来源原值、时点与证据位置",
        "scan_market" => "并行分析候选股票并更新排名",
        _ => "等待数据源返回并执行确定性计算",
    }
}

/// One executed tool call, ready to become a `tool` message.
struct ToolAuditMeta<'a> {
    task_id: &'a str,
    call_id: &'a str,
    tool: &'a str,
    permission_domain: ToolPermissionDomain,
    args_fingerprint: &'a str,
}

struct ToolExec {
    call_id: String,
    evidence: Option<Evidence>,
    message_content: String,
}

fn send(tx: &mpsc::UnboundedSender<AgentEvent>, event: AgentEvent) {
    // A send error means the consumer dropped the stream; stop quietly.
    let _ = tx.unbounded_send(event);
}

/// Merge a streamed tool-call fragment into the accumulator, by index.
fn merge_tool_call(acc: &mut Vec<ToolCall>, delta: &ToolCall) {
    let idx = if let Some(index) = delta.index {
        index as usize
    } else if let Some(id) = delta.id.as_deref() {
        // Some compatible streaming providers omit `index`. A fragment with
        // an existing id continues that call; a new id starts a new slot.
        acc.iter()
            .position(|call| call.id.as_deref() == Some(id))
            .unwrap_or(acc.len())
    } else {
        // Argument-only continuation chunks belong to the most recent call.
        acc.len().saturating_sub(1)
    };
    if acc.len() <= idx {
        acc.resize(idx + 1, ToolCall::default());
    }
    let slot = &mut acc[idx];
    if let Some(id) = &delta.id {
        slot.id = Some(id.clone());
    }
    if let Some(kind) = &delta.kind {
        slot.kind = Some(kind.clone());
    }
    if let Some(f) = &delta.function {
        let target = slot.function.get_or_insert_with(Default::default);
        if let Some(name) = &f.name {
            target.name = Some(name.clone());
        }
        if let Some(args) = &f.arguments {
            target
                .arguments
                .get_or_insert_with(String::new)
                .push_str(args);
        }
    }
}

const MAX_TOOL_CALL_ID_BYTES: usize = 128;

/// Normalize a provider id without inventing additional restrictions beyond
/// what is required for a safe JSON/OpenAI-compatible transcript.
fn valid_tool_call_id(raw: Option<&str>) -> Option<String> {
    let id = raw?.trim();
    if id.is_empty() || id.len() > MAX_TOOL_CALL_ID_BYTES || id.chars().any(char::is_control) {
        return None;
    }
    Some(id.to_string())
}

fn next_recovered_call_id(prefix: &str, seen: &mut std::collections::HashSet<String>) -> String {
    let mut attempt = 0_u32;
    loop {
        let candidate = if attempt == 0 {
            prefix.to_string()
        } else {
            format!("{prefix}_{attempt}")
        };
        if seen.insert(candidate.clone()) {
            return candidate;
        }
        attempt = attempt.saturating_add(1);
    }
}

fn used_tool_call_ids(messages: &[ChatMessage]) -> std::collections::HashSet<String> {
    messages
        .iter()
        .flat_map(|message| message.tool_calls.as_deref().unwrap_or(&[]))
        .filter_map(|call| valid_tool_call_id(call.id.as_deref()))
        .collect()
}

/// Remove empty slots created by sparse/malformed streaming indexes and fill
/// the only optional payload that can be safely defaulted. This prevents an
/// incomplete provider delta from becoming a malformed assistant tool call in
/// the next request.
fn sanitize_streamed_tool_calls(calls: &mut Vec<ToolCall>) {
    calls.retain_mut(|call| {
        let Some(function) = call.function.as_mut() else {
            return false;
        };
        let Some(name) = function.name.as_deref().map(str::trim) else {
            return false;
        };
        if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
            return false;
        }
        function.name = Some(name.to_string());
        if function
            .arguments
            .as_deref()
            .is_none_or(|arguments| arguments.trim().is_empty())
        {
            function.arguments = Some("{}".to_string());
        }
        true
    });
}

/// Make a newly streamed batch globally unique against the complete durable
/// history before the assistant message is persisted. Rewriting happens only
/// for missing, invalid, or repeated ids; all valid unique provider ids stay
/// byte-for-byte unchanged.
fn normalize_new_tool_calls(calls: &mut [ToolCall], messages: &[ChatMessage], round: u32) {
    let mut seen = used_tool_call_ids(messages);
    for (index, call) in calls.iter_mut().enumerate() {
        let provider_id = valid_tool_call_id(call.id.as_deref());
        let unique = provider_id
            .as_ref()
            .is_some_and(|id| seen.insert(id.clone()));
        if unique {
            call.id = provider_id;
        } else {
            call.id = Some(next_recovered_call_id(
                &format!("astock_call_r{round}_i{index}"),
                &mut seen,
            ));
        }
        if call.kind.as_deref().is_none_or(str::is_empty) {
            call.kind = Some("function".to_string());
        }
    }
}

/// Persist one message under sequential id `{task}-{seq:04}`.
async fn store_message(
    storage: &Storage,
    task_id: &str,
    conversation_id: &str,
    seq: usize,
    message: &ChatMessage,
) -> Result<()> {
    storage
        .conversation_append(astock_storage::ChatMessage {
            id: format!("{task_id}-{seq:04}"),
            conversation_id: conversation_id.to_string(),
            role: message.role.clone(),
            // The full provider message (incl. tool_calls / tool_call_id) is
            // serialized into `content` so resume rebuilds it losslessly.
            content: serde_json::to_string(message)?,
            tool_calls: None,
            created_at: now_secs(),
        })
        .await?;
    Ok(())
}

/// Append `message` to `messages` and persist it at the next sequence slot.
async fn append_message(
    storage: &Storage,
    task_id: &str,
    conversation_id: &str,
    messages: &mut Vec<ChatMessage>,
    message: &ChatMessage,
) -> Result<()> {
    store_message(storage, task_id, conversation_id, messages.len(), message).await?;
    messages.push(message.clone());
    Ok(())
}

/// Rebuild the provider message history from the conversation store.
async fn load_messages(storage: &Storage, conversation_id: &str) -> Result<Vec<ChatMessage>> {
    let stored = storage.conversation_load(conversation_id).await?;
    let mut out = Vec::with_capacity(stored.len());
    for row in stored {
        match serde_json::from_str::<ChatMessage>(&row.content) {
            Ok(m) => out.push(m),
            // Rows written by other components: degrade to plain text.
            Err(_) => out.push(ChatMessage::text(row.role, row.content)),
        }
    }
    Ok(reconcile_tool_history(out))
}

/// Repair an OpenAI tool-use transcript after a process interruption.
///
/// MiniMax rejects a request with code 2013 when tool-call ids are duplicated
/// anywhere in the submitted history, or when an assistant `tool_calls` entry
/// is not followed by exactly one `tool` result for every id. A desktop process
/// can also exit after the assistant message was persisted but before all
/// background tools returned.
///
/// This repair is global, deterministic, and order preserving. Valid unique
/// provider ids remain unchanged. Later duplicates (including duplicates in a
/// different round) are renamed together with their matching result. Missing
/// results receive an explicit interruption payload; orphan/excess results are
/// dropped.
fn reconcile_tool_history(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(messages.len());
    let mut globally_seen = std::collections::HashSet::<String>::new();
    let mut assistant_batch = 0usize;
    let mut index = 0usize;
    while index < messages.len() {
        let mut message = messages[index].clone();
        if message.role == "tool" {
            tracing::warn!(
                tool_call_id = ?message.tool_call_id,
                "dropping orphan Agent tool result while repairing history"
            );
            index += 1;
            continue;
        }

        let Some(calls) = message.tool_calls.as_mut() else {
            out.push(message);
            index += 1;
            continue;
        };
        if message.role != "assistant" {
            tracing::warn!(role = %message.role, "dropping tool_calls from non-assistant message");
            message.tool_calls = None;
            out.push(message);
            index += 1;
            continue;
        }
        if calls.is_empty() {
            message.tool_calls = None;
            out.push(message);
            index += 1;
            continue;
        }

        // Remember the provider ids so contiguous results can be paired by
        // occurrence even if two broken calls used the same id.
        let mut source_ids = Vec::with_capacity(calls.len());
        let mut expected = Vec::with_capacity(calls.len());
        for (call_index, call) in calls.iter_mut().enumerate() {
            let source_id = call
                .id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string);
            let provider_id = valid_tool_call_id(call.id.as_deref());
            let unique = provider_id
                .as_ref()
                .is_some_and(|id| globally_seen.insert(id.clone()));
            let assigned = if unique {
                provider_id.expect("checked Some above")
            } else {
                next_recovered_call_id(
                    &format!("astock_recovered_b{assistant_batch}_i{call_index}"),
                    &mut globally_seen,
                )
            };
            call.id = Some(assigned.clone());
            if call.kind.as_deref().is_none_or(str::is_empty) {
                call.kind = Some("function".to_string());
            }
            source_ids.push(source_id);
            expected.push(assigned);
        }
        assistant_batch += 1;
        out.push(message);

        let mut cursor = index + 1;
        let mut results = Vec::<ChatMessage>::new();
        while cursor < messages.len() && messages[cursor].role == "tool" {
            results.push(messages[cursor].clone());
            cursor += 1;
        }

        let mut consumed = vec![false; results.len()];
        for (call_index, call_id) in expected.into_iter().enumerate() {
            let result_index = source_ids[call_index].as_deref().and_then(|source_id| {
                results
                    .iter()
                    .enumerate()
                    .position(|(result_index, result)| {
                        !consumed[result_index]
                            && result
                                .tool_call_id
                                .as_deref()
                                .map(str::trim)
                                .is_some_and(|result_id| result_id == source_id)
                    })
            });
            if let Some(result_index) = result_index {
                consumed[result_index] = true;
                let mut result = results[result_index].clone();
                result.tool_call_id = Some(call_id);
                out.push(result);
            } else {
                tracing::warn!(%call_id, "repairing interrupted Agent tool call without a result");
                out.push(ChatMessage::tool_result(
                    call_id,
                    json!({
                        "error": "应用退出时工具尚未返回；该结果未完成，可按需重新调用",
                        "interrupted": true,
                    })
                    .to_string(),
                ));
            }
        }
        for (result_index, result) in results.iter().enumerate() {
            if !consumed[result_index] {
                tracing::warn!(
                    tool_call_id = ?result.tool_call_id,
                    "dropping unmatched or duplicate Agent tool result"
                );
            }
        }
        index = cursor;
    }
    out
}

/// Validate the exact provider transcript shape after reconciliation. This is
/// intentionally strict: a malformed history is stopped locally instead of
/// spending quota on a request MiniMax must reject with code 2013.
fn validate_tool_history(messages: &[ChatMessage]) -> std::result::Result<(), String> {
    let mut globally_seen = std::collections::HashSet::<String>::new();
    let mut index = 0usize;
    while index < messages.len() {
        let message = &messages[index];
        if message.role == "tool" {
            return Err(format!("第 {index} 条消息是孤立工具结果"));
        }
        let calls = message.tool_calls.as_deref().unwrap_or(&[]);
        if calls.is_empty() {
            index += 1;
            continue;
        }
        if message.role != "assistant" {
            return Err(format!("第 {index} 条非助手消息包含工具调用"));
        }
        for (offset, call) in calls.iter().enumerate() {
            let call_id = valid_tool_call_id(call.id.as_deref())
                .ok_or_else(|| format!("第 {index} 条消息的第 {offset} 个调用 ID 非法"))?;
            if !globally_seen.insert(call_id.clone()) {
                return Err(format!("工具调用 ID {call_id} 在历史中重复"));
            }
            let result_index = index + 1 + offset;
            let result = messages
                .get(result_index)
                .ok_or_else(|| format!("工具调用 {call_id} 缺少结果"))?;
            if result.role != "tool" || result.tool_call_id.as_deref() != Some(call_id.as_str()) {
                return Err(format!("工具调用 {call_id} 的结果顺序或 ID 不匹配"));
            }
        }
        index += 1 + calls.len();
    }
    Ok(())
}

/// Marker prefix of the synthetic snapshot message produced by
/// [`compact_history`]. Used to detect (and rebuild) an existing snapshot so
/// repeated compaction never nests.
pub const SNAPSHOT_MARKER: &str = "工作状态快照(自动压缩)";

/// Replace everything between the system message and the last
/// [`KEEP_RECENT_MESSAGES`] messages with ONE synthetic user message: a
/// deterministic working-state snapshot (goal / completed tool calls with
/// cache keys / evidence / round / continuation instruction). Zero LLM
/// calls; same input → same output.
///
/// Hard rules:
/// - the system message (with its context block) is kept verbatim;
/// - the last [`KEEP_RECENT_MESSAGES`] messages stay raw;
/// - a tool-call/result pair is never split: the boundary is moved forward
///   past any leading `tool` messages of the raw tail;
/// - idempotent: an existing snapshot is detected via [`SNAPSHOT_MARKER`],
///   its goal and round count recovered, then the snapshot is rebuilt from
///   scratch (never nested).
pub fn compact_history(messages: &[ChatMessage], char_budget: usize) -> Vec<ChatMessage> {
    let total: usize = messages.iter().map(message_len).sum();
    if total <= char_budget {
        return messages.to_vec();
    }

    // Strip a previous snapshot (idempotence), recovering goal and rounds.
    let mut recovered_goal: Option<String> = None;
    let mut recovered_rounds = 0u32;
    let mut flat: Vec<ChatMessage> = Vec::with_capacity(messages.len());
    for m in messages {
        if m.role == "user" {
            if let Some(text) = m.content_text() {
                if let Some(body) = text.strip_prefix(SNAPSHOT_MARKER) {
                    recovered_goal = extract_section(body, "目标");
                    recovered_rounds = extract_section(body, "当前轮次")
                        .map(|s| s.chars().filter(|c| c.is_ascii_digit()).collect::<String>())
                        .and_then(|digits| digits.parse().ok())
                        .unwrap_or(0);
                    continue;
                }
            }
        }
        flat.push(m.clone());
    }

    // Only the protected prefix/tail exists: nothing may be compacted.
    // Return the input untouched (an old snapshot, if any, stays valid).
    if flat.len() <= KEEP_RECENT_MESSAGES + 1 {
        return messages.to_vec();
    }

    // Boundary between the compacted region and the raw tail. Never split a
    // tool-call/result pair: pull leading `tool` messages of the tail (whose
    // requesting assistant message sits inside the region) into the region.
    let mut keep_from = flat.len() - KEEP_RECENT_MESSAGES;
    while keep_from < flat.len() && flat[keep_from].role == "tool" {
        keep_from += 1;
    }
    let region = &flat[1..keep_from];

    // 目标: the original task prompt, else the goal recovered from a
    // previous snapshot.
    let goal = region
        .iter()
        .find(|m| m.role == "user")
        .and_then(|m| m.content_text())
        .or(recovered_goal)
        .unwrap_or_else(|| "（未记录）".to_string());

    // Map call id → (tool name, compact args) from the region's assistant
    // messages; each assistant message carrying tool calls is one round.
    let mut call_args: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    let mut rounds = recovered_rounds;
    for m in region {
        if m.role != "assistant" {
            continue;
        }
        let calls = m.tool_calls.as_deref().unwrap_or(&[]);
        if !calls.is_empty() {
            rounds += 1;
        }
        for c in calls {
            let id = c.id.clone().unwrap_or_default();
            let (name, args) = c
                .function
                .as_ref()
                .map(|f| {
                    (
                        f.name.clone().unwrap_or_else(|| "unknown".to_string()),
                        compact_args(f.arguments.as_deref().unwrap_or("")),
                    )
                })
                .unwrap_or_else(|| ("unknown".to_string(), String::new()));
            call_args.insert(id, (name, args));
        }
    }

    // Summarize every tool result in the region, in execution order.
    let mut done_lines: Vec<String> = Vec::new();
    let mut evidence_lines: Vec<String> = Vec::new();
    for m in region.iter().filter(|m| m.role == "tool") {
        let idx = done_lines.len() + 1;
        let call_id = m.tool_call_id.clone().unwrap_or_default();
        let (name, args) = call_args
            .get(&call_id)
            .cloned()
            .unwrap_or_else(|| ("unknown".to_string(), String::new()));
        let fields = m.content_text().map(|s| parse_tool_result(&s));
        let (tool, cache_key, source, fetched_at, key_result) = match fields {
            Some(f) => (f.tool, f.cache_key, f.source, f.fetched_at, f.key_result),
            None => (
                name.clone(),
                String::new(),
                String::new(),
                String::new(),
                "（无内容）".to_string(),
            ),
        };
        let tool = if tool.is_empty() { name } else { tool };
        let mut line = format!("{idx}. {tool}({args})");
        if !cache_key.is_empty() {
            line.push_str(&format!(" cache_key={cache_key}"));
        }
        line.push_str(&format!(" | {key_result}"));
        done_lines.push(line);

        let mut ev = format!("{idx}. tool={tool}");
        if !source.is_empty() {
            ev.push_str(&format!(" source={source}"));
        }
        if !fetched_at.is_empty() {
            ev.push_str(&format!(" fetched_at={fetched_at}"));
        }
        evidence_lines.push(ev);
    }

    let mut snap = String::from(SNAPSHOT_MARKER);
    snap.push_str("\n【目标】");
    snap.push_str(goal.trim());
    snap.push_str("\n【已完成工具调用】");
    if done_lines.is_empty() {
        snap.push_str("\n（无）");
    }
    for line in &done_lines {
        snap.push('\n');
        snap.push_str(line);
    }
    snap.push_str("\n【证据】");
    if evidence_lines.is_empty() {
        snap.push_str("\n（无）");
    }
    for line in &evidence_lines {
        snap.push('\n');
        snap.push_str(line);
    }
    snap.push_str(&format!("\n【当前轮次】第{rounds}轮"));
    snap.push_str("\n【继续指令】基于以上状态继续，不要重复已完成的工具调用；需要明细用get_cached_detail按cache_key取回。");

    let mut out = Vec::with_capacity(flat.len() - region.len() + 2);
    out.push(flat[0].clone());
    out.push(ChatMessage::user(snap));
    out.extend_from_slice(&flat[keep_from..]);
    out
}

/// Rough per-message size in characters (content + tool-call payloads).
fn message_len(m: &ChatMessage) -> usize {
    let content = m.content_text().map(|t| t.chars().count()).unwrap_or(0);
    let calls = m
        .tool_calls
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|c| {
            c.function
                .as_ref()
                .map(|f| {
                    f.name.as_deref().unwrap_or("").len()
                        + f.arguments.as_deref().unwrap_or("").chars().count()
                })
                .unwrap_or(0)
        })
        .sum::<usize>();
    content + calls
}

/// The fields a snapshot needs from one tool-result message.
struct ToolResultFields {
    tool: String,
    cache_key: String,
    source: String,
    fetched_at: String,
    key_result: String,
}

/// Parse the JSON envelope of a tool-result message and derive a one-line
/// key result: the error, or the summary's first scalar fields (≤80 chars).
fn parse_tool_result(content: &str) -> ToolResultFields {
    let get = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match serde_json::from_str::<Value>(content) {
        Ok(v) => {
            let key_result = if let Some(err) = v.get("error").and_then(Value::as_str) {
                format!("错误：{}", cap_chars(err, 60))
            } else if let Some(summary) = v.get("summary") {
                summary_scalars(summary)
            } else {
                "（无摘要）".to_string()
            };
            ToolResultFields {
                tool: get(&v, "tool"),
                cache_key: get(&v, "cache_key"),
                source: get(&v, "source"),
                fetched_at: get(&v, "fetched_at"),
                key_result,
            }
        }
        // Non-envelope rows (plain text): keep a capped prefix.
        Err(_) => ToolResultFields {
            tool: String::new(),
            cache_key: String::new(),
            source: String::new(),
            fetched_at: String::new(),
            key_result: cap_chars(content, 80),
        },
    }
}

/// One-line rendering of a tool summary: `k=v` pairs of its first scalar
/// fields, capped at 80 chars. Non-object summaries are capped directly.
fn summary_scalars(summary: &Value) -> String {
    match summary {
        Value::Object(map) => {
            let mut out = String::new();
            for (k, v) in map {
                let scalar = match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    _ => None,
                };
                if let Some(sv) = scalar {
                    if !out.is_empty() {
                        out.push_str(", ");
                    }
                    out.push_str(k);
                    out.push('=');
                    out.push_str(&sv);
                    if out.chars().count() >= 80 {
                        break;
                    }
                }
            }
            if out.is_empty() {
                "（结构化摘要，见缓存）".to_string()
            } else {
                cap_chars(&out, 80)
            }
        }
        Value::String(s) => cap_chars(s, 80),
        other => cap_chars(&other.to_string(), 80),
    }
}

/// Compact a tool-call arguments string (whitespace-free JSON), ≤60 chars.
fn compact_args(raw: &str) -> String {
    let compact = serde_json::from_str::<Value>(raw)
        .map(|v| v.to_string())
        .unwrap_or_else(|_| raw.to_string());
    cap_chars(&compact, 60)
}

/// Truncate to `max` chars, appending an ellipsis when truncated.
fn cap_chars(s: &str, max: usize) -> String {
    let out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        format!("{out}…")
    } else {
        out
    }
}

/// Extract the text of a `【name】` section from a snapshot body (the text
/// after [`SNAPSHOT_MARKER`]) up to the next section header or end of text.
fn extract_section(body: &str, name: &str) -> Option<String> {
    let tag = format!("【{name}】");
    let start = body.find(&tag)? + tag.len();
    let rest = &body[start..];
    let end = rest.find('【').unwrap_or(rest.len());
    let text = rest[..end].trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    use async_trait::async_trait;
    use serde_json::json;

    use astock_minimax::{ChatChoice, ChatChunk, ToolCallFunction};
    use astock_storage::StorageConfig;

    #[test]
    fn extracts_exact_news_revision_ids_from_tool_payloads() {
        let messages = vec![ChatMessage::tool_result(
            "call-news".to_string(),
            json!({
                "items": [
                    {"document_revision_id": "rev:abc123", "title": "公告"},
                    {"document_revision_id": null},
                    {"document_revision_id": "not-a-revision"}
                ]
            })
            .to_string(),
        )];
        assert_eq!(
            news_revision_ids(&messages),
            ["rev:abc123".to_string()].into_iter().collect()
        );
    }

    #[test]
    fn search_snippets_are_discovery_only_and_source_facts_keep_exact_ids() {
        let discovery = ChatMessage::tool_result(
            "search",
            json!({"verification_status":"discovery_only","fact_eligible":false}).to_string(),
        );
        assert!(contains_discovery_only(std::slice::from_ref(&discovery)));
        assert!(source_evidence_pairs(std::slice::from_ref(&discovery)).is_empty());

        let verified = ChatMessage::tool_result(
            "source",
            json!({
                "source_version_id":"srcver:abc123",
                "source":{"is_primary_source":true},
                "facts":[
                    {"source_version_id":"srcver:abc123","fact_id":"fact:amount","raw_value":"10亿元"},
                    {"source_version_id":"invalid","fact_id":"fact:ignored"}
                ]
            })
            .to_string(),
        );
        let pairs = source_evidence_pairs(std::slice::from_ref(&verified));
        assert!(pairs.contains(&("srcver:abc123".into(), "".into())));
        assert!(pairs.contains(&("srcver:abc123".into(), "fact:amount".into())));
        assert_eq!(pairs.len(), 2);
        assert!(contains_primary_source_evidence(&[verified]));
    }

    use crate::testing::{EchoTool, NoopMarket, ScriptedChat, ScriptedReply};
    use crate::tools::{AgentTool, ToolResult};

    struct SlowTool;

    #[async_trait]
    impl AgentTool for SlowTool {
        fn name(&self) -> &'static str {
            "slow_tool"
        }

        fn description(&self) -> &'static str {
            "用于验证无固定超时的异步测试工具"
        }

        fn parameters_schema(&self) -> Value {
            json!({"type": "object"})
        }

        fn cacheable(&self) -> bool {
            false
        }

        async fn execute(&self, _args: Value, _ctx: &ToolContext) -> Result<ToolResult> {
            tokio::time::sleep(std::time::Duration::from_secs(120)).await;
            Ok(ToolResult {
                summary_json: json!({"completed": true}),
                full_json: None,
                cache_key: String::new(),
                source: "test".to_string(),
                fetched_at: "2026-01-01T00:00:00Z".to_string(),
            })
        }
    }

    struct BarrierTool {
        barrier: Arc<tokio::sync::Barrier>,
        started: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl AgentTool for BarrierTool {
        fn name(&self) -> &'static str {
            "barrier_tool"
        }

        fn description(&self) -> &'static str {
            "用于验证工具批次并发启动的异步测试工具"
        }

        fn parameters_schema(&self) -> Value {
            json!({"type": "object"})
        }

        fn cacheable(&self) -> bool {
            false
        }

        async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolResult> {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.barrier.wait().await;
            Ok(ToolResult {
                summary_json: args,
                full_json: None,
                cache_key: String::new(),
                source: "test".to_string(),
                fetched_at: "2026-01-01T00:00:00Z".to_string(),
            })
        }
    }

    struct DropSignal(Arc<AtomicBool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    struct NeverTool {
        started: Arc<AtomicBool>,
        dropped: Arc<AtomicBool>,
    }

    #[async_trait]
    impl AgentTool for NeverTool {
        fn name(&self) -> &'static str {
            "never_tool"
        }

        fn description(&self) -> &'static str {
            "用于验证主动取消的异步测试工具"
        }

        fn parameters_schema(&self) -> Value {
            json!({"type": "object"})
        }

        fn cacheable(&self) -> bool {
            false
        }

        async fn execute(&self, _args: Value, _ctx: &ToolContext) -> Result<ToolResult> {
            self.started.store(true, Ordering::SeqCst);
            let _drop_signal = DropSignal(Arc::clone(&self.dropped));
            futures::future::pending::<()>().await;
            unreachable!("pending test tool only exits when its future is dropped")
        }
    }

    fn build_engine(storage: Storage, chat: Arc<ScriptedChat>, echo: Arc<EchoTool>) -> AgentEngine {
        let ctx = ToolContext {
            market: Arc::new(NoopMarket),
            storage,
            graph: None,
            fundamental: None,
            joinquant: None,
            minimax_search: None,
            finance_news: None,
            iwencai: None,
            progress: None,
        };
        let registry = ToolRegistry::new(vec![echo as Arc<dyn AgentTool>]);
        AgentEngine::new(chat, registry, ctx, EngineConfig::default())
    }

    fn test_storage() -> (tempfile::TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        (dir, storage)
    }

    fn spec(id: &str) -> TaskSpec {
        TaskSpec::new(id, "test", "测试任务")
    }

    async fn collect(stream: TaskStream) -> Vec<AgentEvent> {
        stream.collect().await
    }

    #[test]
    fn tool_estimates_are_guidance_instead_of_deadlines() {
        assert_eq!(tool_estimated_secs("run_full_analysis"), 45);
        assert_eq!(tool_estimated_secs("compare_stocks"), 60);
        assert_eq!(tool_estimated_secs("iterate_strategy"), 180);
        assert!(tool_progress_stage("compare_stocks", 5_000, 60_000).contains("并行获取"));
        assert!(tool_progress_stage("compare_stocks", 61_000, 60_000).contains("仍在后台继续"));
    }

    #[tokio::test]
    async fn terminal_error_is_persisted_for_user_diagnostics() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("test-model"));
        chat.push(ScriptedReply::Error(MinimaxError::Api {
            code: 500,
            msg: "diagnostic failure".to_string(),
        }));
        let engine = build_engine(storage.clone(), chat, Arc::new(EchoTool::new()));
        let events = collect(engine.run_task(spec("persisted-failure"))).await;
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Failed { error } if error.contains("diagnostic failure")
        )));
        let record = storage
            .agent_task_get("persisted-failure")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.status, "failed");
        let state: Value = serde_json::from_str(&record.state_json).unwrap();
        assert!(state["last_error"]
            .as_str()
            .unwrap()
            .contains("diagnostic failure"));
    }

    #[test]
    fn recognizes_structured_and_legacy_clarification_boundaries() {
        assert!(is_clarification_request(
            "```astock-questions\n{\"questions\":[]}\n```"
        ));
        assert!(is_clarification_request(
            "**1. 资金用途？**\n- A. 试探建仓\n- B. 长期定投\n\n**2. 风险偏好？**\n- 保守\n- 平衡"
        ));
        assert!(!is_clarification_request(
            "1. 关键依据\n- 营收增长\n- 现金流改善\n2. 风险因素\n- 估值偏高"
        ));
    }

    #[test]
    fn tool_event_contract_exposes_estimate_without_deadline() {
        let event = AgentEvent::ToolCallStarted {
            call_id: "c1".to_string(),
            name: "echo".to_string(),
            args: json!({}),
            position: 1,
            total: 1,
            estimated_ms: 45_000,
        };
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["estimated_ms"], 45_000);
        assert!(value.get("timeout_ms").is_none());
    }

    #[tokio::test]
    async fn every_tool_in_batch_is_polled_asynchronously() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("test-model"));
        let calls = [0_u32, 1_u32]
            .into_iter()
            .map(|index| ToolCall {
                id: Some(format!("parallel-{index}")),
                kind: Some("function".to_string()),
                index: Some(index),
                function: Some(ToolCallFunction {
                    name: Some("barrier_tool".to_string()),
                    arguments: Some(json!({"index": index}).to_string()),
                }),
            })
            .collect();
        chat.push(ScriptedReply::Chunks(vec![ChatChunk {
            choices: vec![ChatChoice {
                index: Some(0),
                delta: Some(ChatMessage {
                    role: "assistant".to_string(),
                    tool_calls: Some(calls),
                    ..Default::default()
                }),
                finish_reason: Some("tool_calls".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }]));
        chat.push_text("并行工具均已完成");
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ctx = ToolContext {
            market: Arc::new(NoopMarket),
            storage,
            graph: None,
            fundamental: None,
            joinquant: None,
            minimax_search: None,
            finance_news: None,
            iwencai: None,
            progress: None,
        };
        let engine = AgentEngine::new(
            chat,
            ToolRegistry::new(vec![Arc::new(BarrierTool {
                barrier: Arc::clone(&barrier),
                started: Arc::clone(&started),
            }) as Arc<dyn AgentTool>]),
            ctx,
            EngineConfig::default(),
        );
        let stream = engine.run_task(spec("parallel-tools"));
        // Full-workspace Windows runs can spend several seconds scheduling
        // freshly linked test processes. The barrier still proves both tool
        // futures are polled concurrently; the wider wall-clock guard only
        // removes host-load flakiness.
        tokio::time::timeout(std::time::Duration::from_secs(10), barrier.wait())
            .await
            .expect("both tools must reach the barrier concurrently");
        let events = collect(stream).await;
        assert_eq!(started.load(Ordering::SeqCst), 2);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::ToolCallFinished { success: true, .. }))
                .count(),
            2
        );
        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::Completed { .. })));
    }

    #[tokio::test(start_paused = true)]
    async fn tool_runs_past_estimate_until_success() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("test-model"));
        chat.push_tool_call("slow-id", "slow_tool", json!({}));
        chat.push_text("长任务已经完成");
        let ctx = ToolContext {
            market: Arc::new(NoopMarket),
            storage,
            graph: None,
            fundamental: None,
            joinquant: None,
            minimax_search: None,
            finance_news: None,
            iwencai: None,
            progress: None,
        };
        let engine = AgentEngine::new(
            chat,
            ToolRegistry::new(vec![Arc::new(SlowTool) as Arc<dyn AgentTool>]),
            ctx,
            EngineConfig::default(),
        );
        let mut stream = engine.run_task(spec("slow-past-estimate"));
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            let started = matches!(
                event,
                AgentEvent::ToolCallStarted {
                    estimated_ms: 45_000,
                    ..
                }
            );
            events.push(event);
            if started {
                break;
            }
        }
        tokio::time::advance(std::time::Duration::from_secs(130)).await;
        tokio::task::yield_now().await;
        events.extend(stream.collect::<Vec<_>>().await);
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallFinished {
                success: true,
                elapsed_ms,
                ..
            } if *elapsed_ms >= 120_000
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::Completed { .. })));
        assert!(!events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallFinished {
                error: Some(error),
                ..
            } if error.contains("超时") || error.contains("自动降级")
        )));
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_task_drops_in_flight_tool_future() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("test-model"));
        chat.push_tool_call("never-id", "never_tool", json!({}));
        let started = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let ctx = ToolContext {
            market: Arc::new(NoopMarket),
            storage,
            graph: None,
            fundamental: None,
            joinquant: None,
            minimax_search: None,
            finance_news: None,
            iwencai: None,
            progress: None,
        };
        let engine = AgentEngine::new(
            chat,
            ToolRegistry::new(vec![Arc::new(NeverTool {
                started: Arc::clone(&started),
                dropped: Arc::clone(&dropped),
            }) as Arc<dyn AgentTool>]),
            ctx,
            EngineConfig::default(),
        );
        let mut stream = engine.run_task(spec("cancel-in-flight"));
        while let Some(event) = stream.next().await {
            if matches!(event, AgentEvent::ToolCallStarted { .. }) {
                break;
            }
        }
        tokio::task::yield_now().await;
        assert!(started.load(Ordering::SeqCst));
        assert!(engine.cancel_task("cancel-in-flight").await.unwrap());
        tokio::time::advance(std::time::Duration::from_secs(3)).await;
        tokio::task::yield_now().await;
        let remaining = stream.collect::<Vec<_>>().await;
        assert!(dropped.load(Ordering::SeqCst));
        assert!(remaining
            .iter()
            .any(|event| matches!(event, AgentEvent::Failed { error } if error == "任务已取消")));
        let record = engine
            .ctx
            .storage
            .agent_task_get("cancel-in-flight")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.status, "cancelled");
    }

    #[tokio::test]
    async fn completes_simple_conversation() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("test-model"));
        chat.push_text("【未知】当前没有可验证证据");
        let echo = Arc::new(EchoTool::new());
        let engine = build_engine(storage.clone(), chat.clone(), echo);

        let events = collect(engine.run_task(spec("t1"))).await;
        assert!(events.iter().any(
            |e| matches!(e, AgentEvent::TextDelta { text } if text.contains("没有可验证证据"))
        ));
        let completed = events.iter().find_map(|e| match e {
            AgentEvent::Completed { report } => Some(report),
            _ => None,
        });
        let report = completed.expect("task should complete");
        assert!(report.answer.contains("没有可验证证据"));
        assert!(!report.answer.contains("免责声明"));
        assert_eq!(report.conclusions.len(), 1);
        assert_eq!(report.conclusions[0].grade, "未知");

        // The task row and the conversation were persisted.
        let record = storage.agent_task_get("t1").await.unwrap().unwrap();
        assert_eq!(record.status, "completed");
        let messages = storage.conversation_load("t1").await.unwrap();
        assert_eq!(messages.len(), 3); // system + user + assistant
                                       // The request carried the system prompt and no tools were needed.
        let requests = chat.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].model, "test-model");
        assert!(requests[0].tools.is_some());
        assert_eq!(
            requests[0].extra.get("reasoning_split"),
            Some(&Value::Bool(true))
        );
    }

    #[tokio::test]
    async fn verifier_repairs_an_unsupported_number_before_publication() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("test-model"));
        chat.push_text("【事实】目标价 99 元");
        chat.push_text("【未知】现有证据不足，无法确认具体目标价");
        let echo = Arc::new(EchoTool::new());
        let engine = build_engine(storage.clone(), chat.clone(), echo);

        let events = collect(engine.run_task(spec("t-verify-repair"))).await;
        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::TextReset { message } if message.contains("证据校验"))));
        let report = events.iter().find_map(|event| match event {
            AgentEvent::Completed { report } => Some(report),
            _ => None,
        });
        let report = report.expect("repaired report should be emitted");
        assert!(report.research.verification.passed());
        assert!(report.answer.contains("无法确认具体目标价"));
        assert!(!report.answer.contains("99"));
        assert_eq!(chat.requests.lock().unwrap().len(), 2);
        let persisted = storage
            .reports_get("t-verify-repair")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.kind, "verified-research");
        assert!(persisted.content_json.contains("tool_versions"));
        assert!(persisted.content_json.contains("data_versions"));
        assert!(persisted.content_json.contains("verification"));
    }

    #[tokio::test]
    async fn verifier_blocks_publication_after_two_failed_repairs() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("test-model"));
        chat.push_text("【事实】目标价 99 元");
        chat.push_text("【事实】目标价 98 元");
        chat.push_text("【事实】目标价 97 元");
        let echo = Arc::new(EchoTool::new());
        let engine = build_engine(storage.clone(), chat.clone(), echo);

        let events = collect(engine.run_task(spec("t-verify-block"))).await;
        let report = events.iter().find_map(|event| match event {
            AgentEvent::Completed { report } => Some(report),
            _ => None,
        });
        let report = report.expect("blocked report still carries diagnostics");
        assert_eq!(
            report.research.verification.status,
            VerificationStatus::Failed
        );
        assert!(report.answer.contains("报告未通过证据校验"));
        assert!(!report.answer.contains("97 元"));
        assert_eq!(chat.requests.lock().unwrap().len(), 3);
        assert_eq!(
            storage
                .agent_task_get("t-verify-block")
                .await
                .unwrap()
                .unwrap()
                .status,
            "verification_failed"
        );
        assert_eq!(
            storage
                .reports_get("t-verify-block")
                .await
                .unwrap()
                .unwrap()
                .kind,
            "verification-blocked"
        );
    }

    #[tokio::test]
    async fn specialist_panel_reviews_once_then_main_analyst_synthesizes() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("main-model"));
        chat.push_text("主分析师初稿");
        chat.push_text("证据口径需要核对");
        chat.push_text("需要补充尾部风险");
        chat.push_text("多专家复核后的最终答案");
        let echo = Arc::new(EchoTool::new());
        let engine = build_engine(storage.clone(), chat.clone(), echo);
        let task = spec("t-panel").with_specialists(vec![
            SpecialistRoute {
                name: "证据审计师".into(),
                instruction: "检查证据".into(),
                model: Some("review-model".into()),
            },
            SpecialistRoute {
                name: "风险审计师".into(),
                instruction: "检查风险".into(),
                model: Some("review-model".into()),
            },
        ]);

        let events = collect(engine.run_task(task)).await;
        assert!(events.iter().any(
            |event| matches!(event, AgentEvent::Progress { phase, .. } if phase == "reviewing")
        ));
        let completed = events.iter().find_map(|event| match event {
            AgentEvent::Completed { report } => Some(report),
            _ => None,
        });
        assert!(completed
            .expect("panel task completes")
            .answer
            .contains("最终答案"));

        {
            let requests = chat.requests.lock().unwrap();
            assert_eq!(requests.len(), 4);
            assert_eq!(requests[0].model, "main-model");
            assert!(requests[1..3]
                .iter()
                .all(|request| request.model == "review-model" && request.tools.is_none()));
            assert!(requests[3].messages.iter().any(|message| {
                message.role == "system"
                    && message
                        .content_text()
                        .is_some_and(|text| text.contains("多Agent独立复核结果"))
            }));
        }
        let stored = storage.conversation_load("t-panel").await.unwrap();
        let assistant_rows = stored
            .iter()
            .filter(|message| message.role == "assistant")
            .collect::<Vec<_>>();
        assert_eq!(assistant_rows.len(), 1, "内部初稿不得显示为历史答案");
        assert!(assistant_rows[0].content.contains("最终答案"));
        assert!(!assistant_rows[0].content.contains("主分析师初稿"));
    }

    #[tokio::test]
    async fn plan_clarification_waits_for_user_instead_of_flashing_into_specialist_review() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("main-model"));
        let clarification = "```astock-questions\n{\"title\":\"请确认\",\"questions\":[{\"id\":\"risk\",\"question\":\"风险偏好？\",\"kind\":\"single\",\"options\":[{\"id\":\"safe\",\"label\":\"保守\"},{\"id\":\"balanced\",\"label\":\"平衡\"}],\"allow_other\":true}]}\n```";
        chat.push_text(clarification);
        let echo = Arc::new(EchoTool::new());
        let engine = build_engine(storage.clone(), chat.clone(), echo);
        let task = spec("t-plan-input")
            .with_run_options("plan", "deep", Vec::new(), true)
            .with_specialists(vec![SpecialistRoute {
                name: "风险审计师".into(),
                instruction: "检查风险".into(),
                model: Some("review-model".into()),
            }]);

        let events = collect(engine.run_task(task)).await;
        assert!(!events.iter().any(
            |event| matches!(event, AgentEvent::Progress { phase, .. } if phase == "reviewing")
        ));
        assert!(!events
            .iter()
            .any(|event| matches!(event, AgentEvent::TextReset { .. })));
        let answer = events.iter().find_map(|event| match event {
            AgentEvent::Completed { report } => Some(report.answer.as_str()),
            _ => None,
        });
        assert_eq!(answer, Some(clarification));
        assert_eq!(chat.requests.lock().unwrap().len(), 1);
        let stored = storage.conversation_load("t-plan-input").await.unwrap();
        assert!(stored
            .iter()
            .any(|message| message.role == "assistant"
                && message.content.contains("astock-questions")));
    }

    #[tokio::test]
    async fn disabled_tools_are_not_offered_or_executed() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("test-model"));
        chat.push_tool_call("c1", "echo", json!({"text": "blocked"}));
        chat.push_text("已说明工具关闭");
        let echo = Arc::new(EchoTool::new());
        let engine = build_engine(storage.clone(), chat.clone(), echo.clone());
        let mut task = spec("t-disabled");
        task.enabled_tools = Some(Vec::new());

        let events = collect(engine.run_task(task)).await;
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallFinished { success: false, error: Some(error), .. }
                if error.contains("已被用户")
        )));
        assert_eq!(echo.calls.load(Ordering::SeqCst), 0);
        assert!(chat.requests.lock().unwrap()[0].tools.is_none());
        let audit = storage.agent_tool_audit_list("t-disabled").await.unwrap();
        assert_eq!(
            audit
                .iter()
                .map(|row| row.event.as_str())
                .collect::<Vec<_>>(),
            vec!["requested", "denied"]
        );
        assert!(audit
            .iter()
            .all(|row| row.args_fingerprint.starts_with("sha256:")
                && row.permission_domain == "read_only_network"));
    }

    #[tokio::test]
    async fn repairs_truncated_reasoning_and_enforces_safe_chart_output() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("MiniMax-M3"));
        chat.push_text("<think>生成了很长但未闭合的私有思考");
        chat.push_text(
            "一句话结论。\n```astock-chart\n{\"title\":\"走势\",\"unit\":\"元\",\"x\":[\"1\",\"2\"],\"series\":[{\"name\":\"收盘\",\"type\":\"line\",\"data\":[1,2]}]}\n```",
        );
        let echo = Arc::new(EchoTool::new());
        let engine = build_engine(storage.clone(), chat.clone(), echo);
        let chart_spec = TaskSpec::new("t-chart-repair", "test", "请画一张交互折线图");

        let events = collect(engine.run_task(chart_spec)).await;
        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::TextReset { .. })));
        let report = events.iter().find_map(|event| match event {
            AgentEvent::Completed { report } => Some(report),
            _ => None,
        });
        assert!(report
            .expect("repair pass should complete")
            .answer
            .contains("```astock-chart"));

        {
            let requests = chat.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert!(requests[0].tools.is_some());
            assert!(requests[1].tools.is_none());
            assert_eq!(
                requests[1].extra.get("thinking"),
                Some(&json!({ "type": "disabled" }))
            );
            assert_eq!(
                requests[1].extra.get("max_completion_tokens"),
                Some(&json!(4096))
            );
        }

        let stored = storage.conversation_load("t-chart-repair").await.unwrap();
        assert_eq!(stored.len(), 3, "malformed private draft is not persisted");
        assert!(stored[2].content.contains("astock-chart"));
        assert!(!stored[2].content.contains("未闭合"));
    }

    #[tokio::test]
    async fn context_is_injected_into_system_message_once() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("test-model"));
        chat.push_text("完成");
        let echo = Arc::new(EchoTool::new());
        let engine = build_engine(storage, chat.clone(), echo);

        let with_ctx = spec("t-ctx").with_context("用户正在查看:600519 贵州茅台");
        let events = collect(engine.run_task(with_ctx)).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Completed { .. })));

        {
            let requests = chat.requests.lock().unwrap();
            let sys = requests[0].messages[0].content_text().unwrap();
            assert_eq!(requests[0].messages[0].role, "system");
            assert!(sys.starts_with(&crate::prompt::system_prompt()));
            assert_eq!(
                sys.matches("当前上下文:用户正在查看:600519 贵州茅台")
                    .count(),
                1,
                "context block exactly once: {sys}"
            );
            // The user message is untouched.
            assert_eq!(
                requests[0].messages[1].content_text().as_deref(),
                Some("测试任务")
            );
        }

        // Without stock context the stable prefix remains intact; per-run
        // controls are appended transiently and never stored as user text.
        let chat2 = Arc::new(ScriptedChat::new("test-model"));
        chat2.push_text("完成");
        let echo2 = Arc::new(EchoTool::new());
        let (_dir2, storage2) = test_storage();
        let engine2 = build_engine(storage2, chat2.clone(), echo2);
        let events2 = collect(engine2.run_task(spec("t-plain"))).await;
        assert!(events2
            .iter()
            .any(|e| matches!(e, AgentEvent::Completed { .. })));
        let requests2 = chat2.requests.lock().unwrap();
        let sys2 = requests2[0].messages[0].content_text().unwrap();
        assert!(sys2.starts_with(&crate::prompt::system_prompt()));
        assert!(sys2.contains("【本轮研究控制】"));
        assert!(!sys2.contains("当前上下文:"));
    }

    #[tokio::test]
    async fn tool_round_executes_and_persists() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("test-model"));
        chat.push_tool_call("c1", "echo", json!({"text": "hi"}));
        chat.push_text("完成");
        let echo = Arc::new(EchoTool::new());
        let engine = build_engine(storage.clone(), chat.clone(), echo.clone());

        let events = collect(engine.run_task(spec("t2"))).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCallStarted { name, .. } if name == "echo")));
        assert!(events.iter().any(
            |e| matches!(e, AgentEvent::ToolCallFinished { name, cache_key, .. }
                if name == "echo" && cache_key.starts_with("echo:"))
        ));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Completed { .. })));
        assert_eq!(echo.calls.load(Ordering::SeqCst), 1);

        // system + user + assistant(tool_calls) + tool + assistant
        let messages = storage.conversation_load("t2").await.unwrap();
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[3].role, "tool");

        // The second request contains the tool result with merged arguments.
        {
            let requests = chat.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            let second = &requests[1].messages;
            let tool_msg = second.iter().find(|m| m.role == "tool").unwrap();
            let content = tool_msg.content_text().unwrap();
            assert!(
                content.contains("\"echo\""),
                "tool result replayed: {content}"
            );
            assert!(content.contains("cache_key"));
            assert!(content.contains("\"evidence_id\":\"ev_"));
            assert!(content.contains("\"field_path\""));
            let assistant_msg = second.iter().find(|m| m.role == "assistant").unwrap();
            let args = assistant_msg.tool_calls.as_ref().unwrap()[0]
                .function
                .as_ref()
                .unwrap()
                .arguments
                .clone()
                .unwrap();
            assert_eq!(args, "{\"text\":\"hi\"}", "fragments merged");
        }
        let persisted = storage.reports_get("t2").await.unwrap().unwrap();
        assert!(persisted.content_json.contains("agent-tool-contract-v2"));
        assert!(persisted.content_json.contains("data_versions"));
    }

    #[tokio::test]
    async fn suspend_then_resume_replays_without_reexecuting() {
        let (_dir, storage) = test_storage();
        let echo = Arc::new(EchoTool::new());

        // First run: one tool round, then the quota dies.
        let chat1 = Arc::new(ScriptedChat::new("test-model"));
        chat1.push_tool_call("c1", "echo", json!({"text": "hi"}));
        chat1.push_quota_exhausted();
        let engine1 = build_engine(storage.clone(), chat1, echo.clone());
        let events = collect(engine1.run_task(spec("t3"))).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCallFinished { .. })));
        let suspended = events.iter().find_map(|e| match e {
            AgentEvent::Suspended { reason } => Some(reason),
            _ => None,
        });
        match suspended.expect("task should suspend") {
            SuspendReason::QuotaExhausted { reset_at_unix } => {
                assert_eq!(*reset_at_unix, Some(1_800_000_000u64));
            }
        }
        let record = storage.agent_task_get("t3").await.unwrap().unwrap();
        assert_eq!(record.status, "suspended");
        assert!(record.state_json.contains("\"round\":1"));
        assert_eq!(echo.calls.load(Ordering::SeqCst), 1);

        // Resume with a fresh backend: history rebuilt from storage, the
        // completed echo call is replayed, never re-executed.
        let chat2 = Arc::new(ScriptedChat::new("test-model"));
        chat2.push_text("最终答案");
        let engine2 = build_engine(storage.clone(), chat2.clone(), echo.clone());
        let events2 = collect(engine2.resume_task("t3").await.unwrap()).await;
        let report = events2.iter().find_map(|e| match e {
            AgentEvent::Completed { report } => Some(report),
            _ => None,
        });
        assert!(report
            .expect("resumed task completes")
            .answer
            .contains("最终答案"));
        assert_eq!(
            echo.calls.load(Ordering::SeqCst),
            1,
            "completed tool results must be replayed, not re-executed"
        );
        let replayed_tool_message = {
            let requests = chat2.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            requests[0]
                .messages
                .iter()
                .any(|m| m.role == "tool" && m.content_text().unwrap().contains("\"echo\""))
        };
        assert!(replayed_tool_message);
        let record = storage.agent_task_get("t3").await.unwrap().unwrap();
        assert_eq!(record.status, "completed");
    }

    #[tokio::test]
    async fn resume_repairs_interrupted_parallel_batch_before_model_request() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("test-model"));
        chat.push_text("已基于修复后的历史继续完成");
        let echo = Arc::new(EchoTool::new());
        let engine = build_engine(storage.clone(), chat.clone(), echo.clone());
        let task_id = "resume-interrupted";
        storage
            .conversation_create(task_id, Some("test"))
            .await
            .unwrap();
        let state = TaskState {
            spec: spec(task_id),
            round: 0,
            evidence: Vec::new(),
            context_compactions: 0,
            multi_agent_reviewed: false,
            last_error: None,
        };
        engine.save_state(&state, "running").await.unwrap();

        let call1 = assistant_call("c1", "echo", json!({"text": "done"}))
            .tool_calls
            .unwrap()
            .remove(0);
        let call2 = assistant_call("c2", "echo", json!({"text": "interrupted"}))
            .tool_calls
            .unwrap()
            .remove(0);
        let persisted = [
            ChatMessage::system("system"),
            ChatMessage::user("continue"),
            ChatMessage {
                role: "assistant".to_string(),
                content: Some(Value::String(
                    "<think>provider state must be preserved</think>".to_string(),
                )),
                tool_calls: Some(vec![call1, call2]),
                ..Default::default()
            },
            ChatMessage::tool_result("c1", r#"{"tool":"echo","summary":{"echo":"done"}}"#),
        ];
        for (index, message) in persisted.iter().enumerate() {
            store_message(&storage, task_id, task_id, index, message)
                .await
                .unwrap();
        }

        let events = collect(engine.resume_task(task_id).await.unwrap()).await;
        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::Completed { .. })));
        assert_eq!(
            echo.calls.load(Ordering::SeqCst),
            0,
            "resume must replay/synthesize results instead of re-executing the interrupted batch"
        );
        let requests = chat.requests.lock().unwrap();
        let request = requests.first().expect("one resumed model request");
        let assistant = request
            .messages
            .iter()
            .find(|message| message.role == "assistant")
            .expect("persisted assistant call");
        assert_eq!(
            assistant.content_text().as_deref(),
            Some("<think>provider state must be preserved</think>")
        );
        assert_eq!(assistant.tool_calls.as_deref().unwrap().len(), 2);
        let results: Vec<_> = request
            .messages
            .iter()
            .filter(|message| message.role == "tool")
            .collect();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(results[1].tool_call_id.as_deref(), Some("c2"));
        assert!(results[1].content_text().unwrap().contains("interrupted"));
    }

    #[tokio::test]
    async fn cancel_and_resume_guards() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("test-model"));
        chat.push_quota_exhausted();
        let echo = Arc::new(EchoTool::new());
        let engine = build_engine(storage.clone(), chat, echo);
        let events = collect(engine.run_task(spec("t4"))).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Suspended { .. })));

        assert!(!engine.cancel_task("missing").await.unwrap());
        assert!(engine.cancel_task("t4").await.unwrap());
        let record = storage.agent_task_get("t4").await.unwrap().unwrap();
        assert_eq!(record.status, "cancelled");
        let cancelled_state: TaskState = serde_json::from_str(&record.state_json).unwrap();
        assert!(matches!(
            engine.save_state(&cancelled_state, "running").await,
            Err(AgentError::Cancelled(task_id)) if task_id == "t4"
        ));
        assert_eq!(
            storage.agent_task_get("t4").await.unwrap().unwrap().status,
            "cancelled",
            "a late tool completion must not resurrect a cancelled task"
        );

        let err = match engine.resume_task("t4").await {
            Err(e) => e,
            Ok(_) => panic!("cancelled task must not resume"),
        };
        assert!(matches!(err, AgentError::NotResumable(..)));
        assert!(matches!(
            match engine.resume_task("missing").await {
                Err(e) => e,
                Ok(_) => panic!("missing task must not resume"),
            },
            AgentError::TaskNotFound(_)
        ));
    }

    #[tokio::test]
    async fn max_rounds_bounds_the_loop() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("test-model"));
        // Round 1 asks for a tool; round 2 (the last, tool-less) still
        // returns a tool call in this script → bounded failure.
        chat.push_tool_call("c1", "echo", json!({"text": "a"}));
        chat.push_tool_call("c2", "echo", json!({"text": "b"}));
        let echo = Arc::new(EchoTool::new());
        let ctx = ToolContext {
            market: Arc::new(NoopMarket),
            storage,
            graph: None,
            fundamental: None,
            joinquant: None,
            minimax_search: None,
            finance_news: None,
            iwencai: None,
            progress: None,
        };
        let registry = ToolRegistry::new(vec![echo as Arc<dyn AgentTool>]);
        let engine = AgentEngine::new(
            chat,
            registry,
            ctx,
            EngineConfig {
                max_rounds: 2,
                ..Default::default()
            },
        );
        let events = collect(engine.run_task(spec("t5"))).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Failed { error } if error.contains("最大轮数"))));
    }

    fn dump(messages: &[ChatMessage]) -> String {
        serde_json::to_string(messages).unwrap()
    }

    fn big_tool_message(call_id: &str, cache_key: &str, size: usize) -> ChatMessage {
        ChatMessage::tool_result(
            call_id,
            json!({
                "tool": "get_kline",
                "cache_key": cache_key,
                "source": "eastmoney",
                "fetched_at": "2026-01-01T00:00:00Z",
                "summary": "x".repeat(size),
            })
            .to_string(),
        )
    }

    fn assistant_call(call_id: &str, name: &str, args: Value) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            tool_calls: Some(vec![ToolCall {
                id: Some(call_id.to_string()),
                kind: Some("function".to_string()),
                index: Some(0),
                function: Some(astock_minimax::ToolCallFunction {
                    name: Some(name.to_string()),
                    arguments: Some(args.to_string()),
                }),
            }]),
            ..Default::default()
        }
    }

    #[test]
    fn interrupted_tool_batch_is_repaired_for_minimax_resume() {
        let call1 = assistant_call("c1", "get_quote", json!({}))
            .tool_calls
            .unwrap()
            .remove(0);
        let call2 = assistant_call("c2", "get_kline", json!({}))
            .tool_calls
            .unwrap()
            .remove(0);
        let messages = vec![
            ChatMessage::system("system"),
            ChatMessage::user("分析"),
            ChatMessage {
                role: "assistant".to_string(),
                tool_calls: Some(vec![call1, call2]),
                ..Default::default()
            },
            ChatMessage::tool_result("c1", "quote complete"),
            // c2 was still running when the desktop process exited.
        ];

        let repaired = reconcile_tool_history(messages);
        assert_eq!(repaired.len(), 5);
        assert_eq!(repaired[3].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(repaired[4].tool_call_id.as_deref(), Some("c2"));
        assert!(repaired[4].content_text().unwrap().contains("interrupted"));
        assert_pair_integrity(&repaired);
    }

    #[test]
    fn orphan_and_duplicate_tool_results_are_dropped() {
        let messages = vec![
            ChatMessage::system("system"),
            ChatMessage::tool_result("orphan", "bad"),
            assistant_call("c1", "get_quote", json!({})),
            ChatMessage::tool_result("c1", "first"),
            ChatMessage::tool_result("c1", "duplicate"),
        ];
        let repaired = reconcile_tool_history(messages);
        assert_eq!(repaired.len(), 3);
        assert_eq!(repaired[2].content_text().as_deref(), Some("first"));
        assert_pair_integrity(&repaired);
    }

    #[test]
    fn duplicate_tool_call_ids_across_rounds_are_rewritten_with_results() {
        let messages = vec![
            ChatMessage::system("system"),
            ChatMessage::user("分析"),
            assistant_call("same-id", "get_quote", json!({"symbol": "600519"})),
            ChatMessage::tool_result("same-id", "first round"),
            assistant_call("same-id", "get_kline", json!({"symbol": "600519"})),
            ChatMessage::tool_result("same-id", "second round"),
        ];
        assert!(validate_tool_history(&messages)
            .unwrap_err()
            .contains("历史中重复"));

        let repaired = reconcile_tool_history(messages);
        let first_id = repaired[2].tool_calls.as_ref().unwrap()[0]
            .id
            .clone()
            .unwrap();
        let second_id = repaired[4].tool_calls.as_ref().unwrap()[0]
            .id
            .clone()
            .unwrap();
        assert_eq!(
            first_id, "same-id",
            "first valid provider id stays unchanged"
        );
        assert_ne!(first_id, second_id, "later round receives a fresh id");
        assert_eq!(repaired[3].tool_call_id.as_deref(), Some(first_id.as_str()));
        assert_eq!(
            repaired[5].tool_call_id.as_deref(),
            Some(second_id.as_str())
        );
        assert_eq!(repaired[3].content_text().as_deref(), Some("first round"));
        assert_eq!(repaired[5].content_text().as_deref(), Some("second round"));
        assert_pair_integrity(&repaired);
    }

    #[test]
    fn duplicate_ids_inside_parallel_batch_are_rewritten_by_occurrence() {
        let first = assistant_call("duplicate", "get_quote", json!({}))
            .tool_calls
            .unwrap()
            .remove(0);
        let second = assistant_call("duplicate", "get_kline", json!({}))
            .tool_calls
            .unwrap()
            .remove(0);
        let messages = vec![
            ChatMessage::system("system"),
            ChatMessage {
                role: "assistant".to_string(),
                tool_calls: Some(vec![first, second]),
                ..Default::default()
            },
            ChatMessage::tool_result("duplicate", "first result"),
            ChatMessage::tool_result("duplicate", "second result"),
        ];

        let repaired = reconcile_tool_history(messages);
        let calls = repaired[1].tool_calls.as_ref().unwrap();
        let first_id = calls[0].id.as_deref().unwrap();
        let second_id = calls[1].id.as_deref().unwrap();
        assert_eq!(first_id, "duplicate");
        assert_ne!(first_id, second_id);
        assert_eq!(repaired[2].tool_call_id.as_deref(), Some(first_id));
        assert_eq!(repaired[3].tool_call_id.as_deref(), Some(second_id));
        assert_eq!(repaired[2].content_text().as_deref(), Some("first result"));
        assert_eq!(repaired[3].content_text().as_deref(), Some("second result"));
        assert_pair_integrity(&repaired);
    }

    #[test]
    fn missing_invalid_and_colliding_ids_are_recovered_deterministically() {
        let mut missing = assistant_call("placeholder", "get_quote", json!({}));
        missing.tool_calls.as_mut().unwrap()[0].id = None;
        let invalid_id = "x".repeat(MAX_TOOL_CALL_ID_BYTES + 1);
        let messages = vec![
            ChatMessage::system("system"),
            missing,
            assistant_call(&invalid_id, "get_kline", json!({})),
            ChatMessage::tool_result(&invalid_id, "invalid but matched"),
            assistant_call("astock_recovered_b0_i0", "get_quote", json!({})),
            ChatMessage::tool_result("astock_recovered_b0_i0", "colliding provider id"),
        ];
        let first = reconcile_tool_history(messages.clone());
        let second = reconcile_tool_history(messages);
        assert_eq!(dump(&first), dump(&second));
        assert_pair_integrity(&first);
    }

    #[tokio::test]
    async fn provider_reused_id_is_normalized_before_next_request_and_persistence() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("test-model"));
        chat.push_tool_call("provider-reused", "echo", json!({"text": "one"}));
        chat.push_tool_call("provider-reused", "echo", json!({"text": "two"}));
        chat.push_text("两轮工具均已完成");
        let echo = Arc::new(EchoTool::new());
        let engine = build_engine(storage.clone(), chat.clone(), echo);

        let events = collect(engine.run_task(spec("provider-reused-id"))).await;
        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::Completed { .. })));
        {
            let requests = chat.requests.lock().unwrap();
            assert_eq!(requests.len(), 3);
            for request in requests.iter() {
                validate_tool_history(&request.messages).unwrap();
            }
            let ids: Vec<_> = requests[2]
                .messages
                .iter()
                .flat_map(|message| message.tool_calls.as_deref().unwrap_or(&[]))
                .filter_map(|call| call.id.as_deref())
                .collect();
            assert_eq!(ids.len(), 2);
            assert_eq!(ids[0], "provider-reused");
            assert_ne!(ids[0], ids[1]);
        }
        let persisted = load_messages(&storage, "provider-reused-id").await.unwrap();
        validate_tool_history(&persisted).unwrap();
        assert_pair_integrity(&persisted);
    }

    #[test]
    fn streaming_calls_without_indexes_keep_distinct_ids() {
        let mut calls = Vec::new();
        for (id, name) in [("one", "get_quote"), ("two", "get_kline")] {
            merge_tool_call(
                &mut calls,
                &ToolCall {
                    id: Some(id.to_string()),
                    kind: Some("function".to_string()),
                    index: None,
                    function: Some(astock_minimax::ToolCallFunction {
                        name: Some(name.to_string()),
                        arguments: Some("{}".to_string()),
                    }),
                },
            );
        }
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id.as_deref(), Some("one"));
        assert_eq!(calls[1].id.as_deref(), Some("two"));
    }

    #[test]
    fn sparse_stream_indexes_cannot_create_empty_tool_calls() {
        let mut calls = Vec::new();
        merge_tool_call(
            &mut calls,
            &ToolCall {
                id: None,
                kind: Some("function".to_string()),
                index: Some(2),
                function: Some(ToolCallFunction {
                    name: Some("get_quote".to_string()),
                    arguments: None,
                }),
            },
        );
        assert_eq!(
            calls.len(),
            3,
            "stream accumulator contains sparse placeholders"
        );
        sanitize_streamed_tool_calls(&mut calls);
        normalize_new_tool_calls(&mut calls, &[], 1);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id.as_deref(), Some("astock_call_r1_i0"));
        assert_eq!(
            calls[0]
                .function
                .as_ref()
                .and_then(|function| function.arguments.as_deref()),
            Some("{}")
        );
    }

    /// A fat multi-round history: system + user + `rounds` tool pairs + a
    /// trailing assistant note.
    fn fat_history(rounds: usize) -> Vec<ChatMessage> {
        let mut messages = vec![
            ChatMessage::system("系统提示词\n当前上下文:用户正在查看:600519 贵州茅台"),
            ChatMessage::user("全面分析600519"),
        ];
        for i in 0..rounds {
            messages.push(assistant_call(
                &format!("c{i}"),
                "get_kline",
                json!({"symbol": "600519", "count": 120}),
            ));
            messages.push(big_tool_message(
                &format!("c{i}"),
                &format!("get_kline:k{i}"),
                2_000,
            ));
        }
        messages.push(ChatMessage::assistant("阶段性结论"));
        messages
    }

    /// Every tool result answers an earlier tool call, and no tool call is
    /// left unanswered: the sequence is a valid provider message list.
    fn assert_pair_integrity(messages: &[ChatMessage]) {
        let mut pending: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for m in messages {
            match m.role.as_str() {
                "assistant" => {
                    for c in m.tool_calls.as_deref().unwrap_or(&[]) {
                        let id = c.id.clone().unwrap_or_default();
                        assert!(seen.insert(id.clone()), "duplicate tool call id: {id}");
                        pending.push(id);
                    }
                }
                "tool" => {
                    let id = m.tool_call_id.clone().unwrap_or_default();
                    let pos = pending
                        .iter()
                        .position(|p| *p == id)
                        .unwrap_or_else(|| panic!("orphan tool result: {id}"));
                    pending.remove(pos);
                }
                _ => {}
            }
        }
        assert!(
            pending.is_empty(),
            "tool calls without results: {pending:?}"
        );
        validate_tool_history(messages).unwrap();
    }

    fn snapshot_count(messages: &[ChatMessage]) -> usize {
        messages
            .iter()
            .filter(|m| {
                m.content_text()
                    .map(|t| t.starts_with(SNAPSHOT_MARKER))
                    .unwrap_or(false)
            })
            .count()
    }

    #[test]
    fn compact_history_is_noop_under_budget() {
        let messages = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("u"),
            big_tool_message("c1", "get_kline:aa", 100),
        ];
        let compacted = compact_history(&messages, 24_000);
        assert_eq!(dump(&compacted), dump(&messages));
    }

    #[test]
    fn compact_history_builds_working_state_snapshot() {
        let messages = fat_history(10);
        let compacted = compact_history(&messages, 8_000);

        // Deterministic.
        assert_eq!(dump(&compacted), dump(&compact_history(&messages, 8_000)));
        // The system message (with its context block) is verbatim.
        assert_eq!(
            compacted[0].content_text().as_deref(),
            messages[0].content_text().as_deref()
        );
        // One synthetic user snapshot follows.
        assert_eq!(compacted[1].role, "user");
        let snap = compacted[1].content_text().unwrap();
        for needle in [
            SNAPSHOT_MARKER,
            "【目标】全面分析600519",
            "【已完成工具调用】",
            "【证据】",
            "【当前轮次】第8轮",
            "【继续指令】",
            "get_cached_detail",
            "get_kline:k0",
            "cache_key=get_kline:k7",
            "source=eastmoney",
            "fetched_at=2026-01-01T00:00:00Z",
        ] {
            assert!(snap.contains(needle), "snapshot missing: {needle}\n{snap}");
        }
        // The raw tail starts at a non-tool message and is byte-identical.
        assert_ne!(compacted[2].role, "tool", "boundary must not split a pair");
        let tail = &compacted[compacted.len() - 5..];
        let original_tail = &messages[messages.len() - 5..];
        assert_eq!(dump(tail), dump(original_tail), "raw tail intact");
        // No orphan tool calls / results anywhere.
        assert_pair_integrity(&compacted);
        // Totals shrink by at least 40%.
        let before: usize = messages.iter().map(message_len).sum();
        let after: usize = compacted.iter().map(message_len).sum();
        assert!(
            after * 5 <= before * 3,
            "expected ≥40% reduction, before={before} after={after}"
        );
    }

    #[test]
    fn compact_history_is_idempotent() {
        let messages = fat_history(10);
        let compacted = compact_history(&messages, 8_000);
        // Under budget now: compacting again changes nothing.
        let again = compact_history(&compacted, 8_000);
        assert_eq!(dump(&again), dump(&compacted));
        assert_eq!(snapshot_count(&again), 1);

        // Grow past the budget again: the old snapshot is detected and the
        // snapshot is rebuilt from scratch — never nested.
        let mut grown = compacted.clone();
        for i in 10..16 {
            grown.push(assistant_call(
                &format!("c{i}"),
                "get_quote",
                json!({"symbol": "600519"}),
            ));
            grown.push(big_tool_message(
                &format!("c{i}"),
                &format!("get_quote:q{i}"),
                2_000,
            ));
        }
        let re = compact_history(&grown, 8_000);
        assert_eq!(snapshot_count(&re), 1, "snapshots must not nest");
        assert_pair_integrity(&re);
        let snap = re[1].content_text().unwrap();
        // Goal recovered from the old snapshot; rounds accumulate; the old
        // raw tail (k8/k9) is now summarized alongside the new calls.
        assert!(snap.contains("【目标】全面分析600519"), "{snap}");
        assert!(snap.contains("【当前轮次】第13轮"), "{snap}");
        assert!(snap.contains("get_kline:k8"), "{snap}");
        assert!(snap.contains("get_quote:q10"), "{snap}");
        // Still a strict shrink relative to the grown input.
        let before: usize = grown.iter().map(message_len).sum();
        let after: usize = re.iter().map(message_len).sum();
        assert!(after < before, "compacted history must shrink");
    }

    #[tokio::test]
    async fn resume_after_compaction_sends_valid_sequence() {
        let (_dir, storage) = test_storage();
        let echo = Arc::new(EchoTool::new());
        let config = EngineConfig {
            history_char_budget: 8_000,
            ..Default::default()
        };
        let build = |chat: Arc<ScriptedChat>| {
            let ctx = ToolContext {
                market: Arc::new(NoopMarket),
                storage: storage.clone(),
                graph: None,
                fundamental: None,
                joinquant: None,
                minimax_search: None,
                finance_news: None,
                iwencai: None,
                progress: None,
            };
            let registry = ToolRegistry::new(vec![echo.clone() as Arc<dyn AgentTool>]);
            AgentEngine::new(chat, registry, ctx, config.clone())
        };

        // First run: five fat tool rounds, then the quota dies.
        let chat1 = Arc::new(ScriptedChat::new("test-model"));
        let fat = "x".repeat(1_500);
        for i in 0..5 {
            // Distinct args per round: identical calls would be served from
            // the read-through tool cache without executing.
            chat1.push_tool_call(&format!("c{i}"), "echo", json!({"text": fat, "round": i}));
        }
        chat1.push_quota_exhausted();
        let events = collect(build(chat1).run_task(spec("t6"))).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Suspended { .. })));
        assert_eq!(echo.calls.load(Ordering::SeqCst), 5);

        // Resume: the rebuilt history exceeds the budget, so the request is
        // compacted identically to the live path — one snapshot, valid pairs.
        let chat2 = Arc::new(ScriptedChat::new("test-model"));
        chat2.push_text("最终答案");
        let events2 = collect(build(chat2.clone()).resume_task("t6").await.unwrap()).await;
        let report = events2.iter().find_map(|e| match e {
            AgentEvent::Completed { report } => Some(report),
            _ => None,
        });
        assert!(report
            .expect("resumed task completes")
            .answer
            .contains("最终答案"));
        assert_eq!(echo.calls.load(Ordering::SeqCst), 5, "no re-execution");

        let requests = chat2.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let sent = &requests[0].messages;
        assert_eq!(sent[0].role, "system");
        assert!(sent[0]
            .content_text()
            .unwrap()
            .starts_with(&crate::prompt::system_prompt()));
        assert_eq!(sent[1].role, "user");
        let snap = sent[1].content_text().unwrap();
        for needle in [
            SNAPSHOT_MARKER,
            "【目标】测试任务",
            "【已完成工具调用】",
            "【证据】",
            "【继续指令】",
            "cache_key=echo:",
            "source=test",
        ] {
            assert!(snap.contains(needle), "snapshot missing: {needle}\n{snap}");
        }
        assert_eq!(snapshot_count(sent), 1);
        assert_pair_integrity(sent);
    }
}
