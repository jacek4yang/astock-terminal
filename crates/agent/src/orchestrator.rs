//! The tool-calling conversation loop with resumable workflows.
//!
//! Every round is persisted: chat messages go to the conversation store and
//! the workflow state (spec, round counter, evidence) to `agent_tasks`. When
//! the MiniMax quota runs out, the task suspends with its reset time;
//! `resume_task` rebuilds the message history from storage and continues —
//! completed tool results come back as conversation messages and cached
//! payloads, never re-executed.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use futures::channel::mpsc;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use astock_minimax::{ChatMessage, ChatRequest, MinimaxError, ToolCall};
use astock_storage::{AgentTask, Storage};

use crate::backend::ChatBackend;
use crate::error::{AgentError, Result};
use crate::prompt::initial_messages_with_context;
use crate::report::{assemble_report, AgentReport, Evidence};
use crate::tools::{now_secs, ToolContext, ToolRegistry};

/// A boxed event stream for one running task.
pub type TaskStream = Pin<Box<dyn Stream<Item = AgentEvent> + Send>>;

/// What to work on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSpec {
    /// Caller-provided task id (also the conversation id).
    pub id: String,
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
}

impl TaskSpec {
    /// A task with just the mandatory fields.
    pub fn new(id: impl Into<String>, kind: impl Into<String>, prompt: impl Into<String>) -> Self {
        TaskSpec {
            id: id.into(),
            kind: kind.into(),
            prompt: prompt.into(),
            max_rounds: None,
            model: None,
            context: None,
        }
    }

    /// Attach a runtime-context block to the system prompt.
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
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
    /// A fragment of the assistant's streamed text.
    TextDelta {
        /// The streamed text fragment.
        text: String,
    },
    /// A tool call is about to execute.
    ToolCallStarted {
        /// Tool name.
        name: String,
        /// Arguments as requested by the model.
        args: Value,
    },
    /// A tool call finished.
    ToolCallFinished {
        /// Tool name.
        name: String,
        /// Cache key of the stored result (empty when not cacheable).
        cache_key: String,
        /// Wall-clock execution time.
        elapsed_ms: u64,
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
    /// Maximum model rounds per task (default 20).
    pub max_rounds: u32,
    /// Maximum concurrent tool executions within one round (default 4).
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
            max_rounds: 20,
            max_parallel_tools: 4,
            history_char_budget: 24_000,
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
        let mut messages = load_messages(&self.ctx.storage, task_id).await?;
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

    /// Mark a task cancelled; a running loop notices at the next round.
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
        if let Err(e) = self
            .ctx
            .storage
            .conversation_create(&spec.id, Some(&spec.kind))
            .await
        {
            send(&tx, AgentEvent::Failed {
                error: format!("storage: {e}"),
            });
            return;
        }
        let messages = initial_messages_with_context(&spec.prompt, spec.context.as_deref());
        for (i, m) in messages.iter().enumerate() {
            if let Err(e) = store_message(&self.ctx.storage, &spec.id, i, m).await {
                send(&tx, AgentEvent::Failed {
                    error: format!("storage: {e}"),
                });
                return;
            }
        }
        let state = TaskState {
            spec,
            round: 0,
            evidence: Vec::new(),
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
            send(&tx, AgentEvent::Failed {
                error: format!("storage: {e}"),
            });
            return;
        }

        loop {
            if state.round >= max_rounds {
                self.finish_with_error(&state, &tx, format!("超过最大轮数 {max_rounds}"))
                    .await;
                return;
            }
            if let Err(e) = self.check_cancelled(&task_id).await {
                match e {
                    AgentError::Cancelled(_) => {
                        send(&tx, AgentEvent::Failed {
                            error: "任务已取消".to_string(),
                        });
                        return;
                    }
                    other => {
                        self.finish_with_error(&state, &tx, other.to_string()).await;
                        return;
                    }
                }
            }

            let last_round = state.round + 1 >= max_rounds;
            let mut request = ChatRequest::new(
                model.clone(),
                compact_history(&messages, self.config.history_char_budget),
            );
            if !last_round {
                request = request.with_tools(self.tools.specs());
            }
            if let Some(t) = self.config.temperature {
                request = request.with_temperature(t);
            }

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
            while let Some(item) = stream.next().await {
                match item {
                    Ok(chunk) => {
                        if let Some(delta) = chunk.raw_delta() {
                            if !delta.is_empty() {
                                text.push_str(&delta);
                                send(&tx, AgentEvent::TextDelta { text: delta });
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

            let (_, clean_text) = astock_minimax::split_reasoning(&text);
            let assistant = ChatMessage {
                role: "assistant".to_string(),
                content: if clean_text.is_empty() {
                    None
                } else {
                    Some(Value::String(clean_text.clone()))
                },
                tool_calls: if calls.is_empty() {
                    None
                } else {
                    Some(calls.clone())
                },
                ..Default::default()
            };
            if let Err(e) = append_message(&self.ctx.storage, &task_id, &mut messages, &assistant).await {
                self.finish_with_error(&state, &tx, format!("storage: {e}")).await;
                return;
            }

            if calls.is_empty() {
                if clean_text.is_empty() {
                    self.finish_with_error(&state, &tx, "模型未产出最终回答".to_string())
                        .await;
                    return;
                }
                let report = assemble_report(&task_id, &clean_text, state.evidence.clone(), now_secs());
                let _ = self.save_state(&state, "completed").await;
                send(&tx, AgentEvent::Completed {
                    report: Box::new(report),
                });
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
            let executed = self.execute_round(&calls, &tx).await;
            for exec in executed {
                if exec.ok {
                    state.evidence.push(Evidence {
                        tool: exec.name.clone(),
                        cache_key: exec.cache_key.clone(),
                        source: exec.source.clone(),
                        fetched_at: exec.fetched_at.clone(),
                    });
                }
                let message = ChatMessage::tool_result(exec.call_id, exec.message_content);
                if let Err(e) =
                    append_message(&self.ctx.storage, &task_id, &mut messages, &message).await
                {
                    self.finish_with_error(&state, &tx, format!("storage: {e}")).await;
                    return;
                }
            }

            state.round += 1;
            if let Err(e) = self.save_state(&state, "running").await {
                send(&tx, AgentEvent::Failed {
                    error: format!("storage: {e}"),
                });
                return;
            }
        }
    }

    /// Execute one round of tool calls (bounded concurrency), in call order.
    async fn execute_round(
        &self,
        calls: &[ToolCall],
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Vec<ToolExec> {
        let mut indexed: Vec<(usize, ToolExec)> =
            futures::stream::iter(calls.iter().cloned().enumerate())
                .map(|(idx, call)| async move { (idx, self.execute_one(call, tx).await) })
                .buffer_unordered(self.config.max_parallel_tools.max(1))
                .collect()
                .await;
        indexed.sort_by_key(|(idx, _)| *idx);
        indexed.into_iter().map(|(_, r)| r).collect()
    }

    /// Execute a single tool call; tool failures become error payloads fed
    /// back to the model (the loop survives bad calls).
    async fn execute_one(&self, call: ToolCall, tx: &mpsc::UnboundedSender<AgentEvent>) -> ToolExec {
        let call_id = call
            .id
            .clone()
            .unwrap_or_else(|| "call_0".to_string());
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
                    return ToolExec {
                        ok: false,
                        call_id,
                        name: name.clone(),
                        cache_key: String::new(),
                        source: String::new(),
                        fetched_at: String::new(),
                        message_content: json!({
                            "tool": name,
                            "error": format!("参数不是合法JSON: {e}"),
                        })
                        .to_string(),
                    }
                }
            }
        };

        send(tx, AgentEvent::ToolCallStarted {
            name: name.clone(),
            args: args.clone(),
        });
        let started = Instant::now();
        let outcome = self.tools.dispatch(&name, args, &self.ctx).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;

        match outcome {
            Ok(result) => {
                send(tx, AgentEvent::ToolCallFinished {
                    name: name.clone(),
                    cache_key: result.cache_key.clone(),
                    elapsed_ms,
                });
                ToolExec {
                    ok: true,
                    call_id,
                    name: name.clone(),
                    cache_key: result.cache_key.clone(),
                    source: result.source.clone(),
                    fetched_at: result.fetched_at.clone(),
                    message_content: json!({
                        "tool": name,
                        "cache_key": result.cache_key,
                        "source": result.source,
                        "fetched_at": result.fetched_at,
                        "summary": result.summary_json,
                    })
                    .to_string(),
                }
            }
            Err(e) => {
                send(tx, AgentEvent::ToolCallFinished {
                    name: name.clone(),
                    cache_key: String::new(),
                    elapsed_ms,
                });
                ToolExec {
                    ok: false,
                    call_id,
                    name: name.clone(),
                    cache_key: String::new(),
                    source: String::new(),
                    fetched_at: String::new(),
                    message_content: json!({
                        "tool": name,
                        "error": e.to_string(),
                    })
                    .to_string(),
                }
            }
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
        send(tx, AgentEvent::Suspended {
            reason: SuspendReason::QuotaExhausted { reset_at_unix },
        });
    }

    /// Persist the failure status and emit `Failed`.
    async fn finish_with_error(
        &self,
        state: &TaskState,
        tx: &mpsc::UnboundedSender<AgentEvent>,
        error: String,
    ) {
        let _ = self.save_state(state, "failed").await;
        send(tx, AgentEvent::Failed { error });
    }
}

/// One executed tool call, ready to become a `tool` message.
struct ToolExec {
    /// Whether the tool succeeded (its evidence joins the report).
    ok: bool,
    call_id: String,
    name: String,
    cache_key: String,
    source: String,
    fetched_at: String,
    message_content: String,
}

fn send(tx: &mpsc::UnboundedSender<AgentEvent>, event: AgentEvent) {
    // A send error means the consumer dropped the stream; stop quietly.
    let _ = tx.unbounded_send(event);
}

/// Merge a streamed tool-call fragment into the accumulator, by index.
fn merge_tool_call(acc: &mut Vec<ToolCall>, delta: &ToolCall) {
    let idx = delta
        .index
        .map(|i| i as usize)
        .unwrap_or_else(|| acc.len().saturating_sub(1));
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

/// Persist one message under sequential id `{task}-{seq:04}`.
async fn store_message(
    storage: &Storage,
    task_id: &str,
    seq: usize,
    message: &ChatMessage,
) -> Result<()> {
    storage
        .conversation_append(astock_storage::ChatMessage {
            id: format!("{task_id}-{seq:04}"),
            conversation_id: task_id.to_string(),
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
    messages: &mut Vec<ChatMessage>,
    message: &ChatMessage,
) -> Result<()> {
    store_message(storage, task_id, messages.len(), message).await?;
    messages.push(message.clone());
    Ok(())
}

/// Rebuild the provider message history from the conversation store.
async fn load_messages(storage: &Storage, task_id: &str) -> Result<Vec<ChatMessage>> {
    let stored = storage.conversation_load(task_id).await?;
    let mut out = Vec::with_capacity(stored.len());
    for row in stored {
        match serde_json::from_str::<ChatMessage>(&row.content) {
            Ok(m) => out.push(m),
            // Rows written by other components: degrade to plain text.
            Err(_) => out.push(ChatMessage::text(row.role, row.content)),
        }
    }
    Ok(out)
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
            None => (name.clone(), String::new(), String::new(), String::new(), "（无内容）".to_string()),
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
    let get = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
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
    use std::sync::atomic::Ordering;

    use serde_json::json;

    use astock_storage::StorageConfig;

    use crate::testing::{EchoTool, NoopMarket, ScriptedChat};
    use crate::tools::AgentTool;

    fn build_engine(
        storage: Storage,
        chat: Arc<ScriptedChat>,
        echo: Arc<EchoTool>,
    ) -> AgentEngine {
        let ctx = ToolContext {
            market: Arc::new(NoopMarket),
            storage,
            graph: None,
            fundamental: None,
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

    #[tokio::test]
    async fn completes_simple_conversation() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("test-model"));
        chat.push_text("【计算】答案是42");
        let echo = Arc::new(EchoTool::new());
        let engine = build_engine(storage.clone(), chat.clone(), echo);

        let events = collect(engine.run_task(spec("t1"))).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { text } if text.contains("答案是42"))));
        let completed = events.iter().find_map(|e| match e {
            AgentEvent::Completed { report } => Some(report),
            _ => None,
        });
        let report = completed.expect("task should complete");
        assert!(report.answer.contains("答案是42"));
        assert!(!report.answer.contains("免责声明"));
        assert_eq!(report.conclusions.len(), 1);
        assert_eq!(report.conclusions[0].grade, "计算");

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
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Completed { .. })));

        {
            let requests = chat.requests.lock().unwrap();
            let sys = requests[0].messages[0].content_text().unwrap();
            assert_eq!(requests[0].messages[0].role, "system");
            assert!(sys.starts_with(&crate::prompt::system_prompt()));
            assert_eq!(
                sys.matches("当前上下文:用户正在查看:600519 贵州茅台").count(),
                1,
                "context block exactly once: {sys}"
            );
            // The user message is untouched.
            assert_eq!(
                requests[0].messages[1].content_text().as_deref(),
                Some("测试任务")
            );
        }

        // Without context the system message is the bare stable prompt.
        let chat2 = Arc::new(ScriptedChat::new("test-model"));
        chat2.push_text("完成");
        let echo2 = Arc::new(EchoTool::new());
        let (_dir2, storage2) = test_storage();
        let engine2 = build_engine(storage2, chat2.clone(), echo2);
        let events2 = collect(engine2.run_task(spec("t-plain"))).await;
        assert!(events2.iter().any(|e| matches!(e, AgentEvent::Completed { .. })));
        let requests2 = chat2.requests.lock().unwrap();
        let sys2 = requests2[0].messages[0].content_text().unwrap();
        assert_eq!(sys2, crate::prompt::system_prompt());
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
        assert!(events.iter().any(
            |e| matches!(e, AgentEvent::ToolCallStarted { name, .. } if name == "echo")
        ));
        assert!(events.iter().any(
            |e| matches!(e, AgentEvent::ToolCallFinished { name, cache_key, .. }
                if name == "echo" && cache_key.starts_with("echo:"))
        ));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Completed { .. })));
        assert_eq!(echo.calls.load(Ordering::SeqCst), 1);

        // system + user + assistant(tool_calls) + tool + assistant
        let messages = storage.conversation_load("t2").await.unwrap();
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[3].role, "tool");

        // The second request contains the tool result with merged arguments.
        let requests = chat.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let second = &requests[1].messages;
        let tool_msg = second.iter().find(|m| m.role == "tool").unwrap();
        let content = tool_msg.content_text().unwrap();
        assert!(content.contains("\"echo\""), "tool result replayed: {content}");
        assert!(content.contains("cache_key"));
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
        assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolCallFinished { .. })));
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
        assert!(report.expect("resumed task completes").answer.contains("最终答案"));
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
    async fn cancel_and_resume_guards() {
        let (_dir, storage) = test_storage();
        let chat = Arc::new(ScriptedChat::new("test-model"));
        chat.push_quota_exhausted();
        let echo = Arc::new(EchoTool::new());
        let engine = build_engine(storage.clone(), chat, echo);
        let events = collect(engine.run_task(spec("t4"))).await;
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Suspended { .. })));

        assert!(!engine.cancel_task("missing").await.unwrap());
        assert!(engine.cancel_task("t4").await.unwrap());
        let record = storage.agent_task_get("t4").await.unwrap().unwrap();
        assert_eq!(record.status, "cancelled");

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
        for m in messages {
            match m.role.as_str() {
                "assistant" => {
                    for c in m.tool_calls.as_deref().unwrap_or(&[]) {
                        pending.push(c.id.clone().unwrap_or_default());
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
        assert!(pending.is_empty(), "tool calls without results: {pending:?}");
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
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Suspended { .. })));
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
        assert!(report.expect("resumed task completes").answer.contains("最终答案"));
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
