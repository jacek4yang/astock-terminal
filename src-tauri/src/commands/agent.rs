//! Agent commands: MiniMax-driven Q&A tasks with suspend/resume
//! (docs/command-contract.md §Agent).
//!
//! `agent_ask` / `agent_resume` return immediately; every [`AgentEvent`] is
//! forwarded as a Tauri `agent-event` emission with payload
//! `{task_id, event}` until the stream ends (Completed / Failed /
//! Suspended), at which point the forwarder handle is dropped from
//! [`AppState::agent_handles`]. `agent_cancel` marks the task `cancelled`
//! in storage (in-flight tools notice on their next heartbeat and drop their
//! futures) and aborts the UI forwarder.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use astock_agent::{
    default_registry, AgentEngine, AgentError, AgentEvent, EngineConfig, SpecialistRoute, TaskSpec,
    TaskStream, ToolContext,
};
use astock_market_data::DataProvider;
use astock_minimax::{ChatMessage as ProviderChatMessage, MinimaxClient, ToolCall};
use astock_storage::{AgentTask, ChatMessage as StoredChatMessage};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{ipc::Channel, State};

use crate::error::CmdError;
use crate::state::AppState;

/// Task-id disambiguator within the same millisecond.
static TASK_COUNTER: AtomicU32 = AtomicU32::new(0);

/// `agent_ask` response. The run and conversation identities are deliberately
/// separate: every user turn gets a new run while a conversation stays stable.
#[derive(Debug, Serialize)]
pub struct AgentAskResponse {
    /// The spawned task id.
    pub task_id: String,
    /// Conversation the task writes its messages into (same id).
    pub conversation_id: String,
}

/// `agent_resume` response.
#[derive(Debug, Serialize)]
pub struct AgentResumeResponse {
    /// Always true on success.
    pub resumed: bool,
}

/// `agent_cancel` response.
#[derive(Debug, Serialize)]
pub struct AgentCancelResponse {
    /// Whether the task existed and was marked cancelled.
    pub cancelled: bool,
}

/// Per-run controls selected in the chat UI. Values are validated at the
/// desktop boundary so persisted tasks can safely resume with the same
/// behavior even after the UI has navigated away.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AgentRunOptions {
    pub research_mode: String,
    pub reasoning_depth: String,
    pub enabled_tools: Option<Vec<String>>,
    pub auto_resume_on_quota: bool,
}

impl Default for AgentRunOptions {
    fn default() -> Self {
        Self {
            research_mode: "deep".to_string(),
            reasoning_depth: "deep".to_string(),
            enabled_tools: None,
            auto_resume_on_quota: true,
        }
    }
}

impl AgentRunOptions {
    fn normalize(self) -> Result<(String, String, Vec<String>, u32, bool), CmdError> {
        if !matches!(self.research_mode.as_str(), "quick" | "deep" | "plan") {
            return Err(CmdError::new("invalid_param", "不支持的研究模式"));
        }
        if !matches!(
            self.reasoning_depth.as_str(),
            "standard" | "deep" | "maximum"
        ) {
            return Err(CmdError::new("invalid_param", "不支持的思考深度"));
        }

        let known: Vec<String> = default_registry()
            .names()
            .into_iter()
            .map(str::to_string)
            .collect();
        let requested = self.enabled_tools.unwrap_or_else(|| known.clone());
        let unknown: Vec<&String> = requested
            .iter()
            .filter(|name| !known.contains(name))
            .collect();
        if !unknown.is_empty() {
            return Err(CmdError::new(
                "invalid_param",
                format!(
                    "包含未注册的工具：{}",
                    unknown.into_iter().cloned().collect::<Vec<_>>().join("、")
                ),
            ));
        }
        // Normalize to the registry's stable order and remove duplicates.
        let enabled = known
            .into_iter()
            .filter(|name| requested.contains(name))
            .collect();
        let mut max_rounds = match self.research_mode.as_str() {
            "quick" => 12,
            "plan" => 40,
            _ => 32,
        };
        if self.reasoning_depth == "maximum" {
            max_rounds = max_rounds.max(48);
        }
        Ok((
            self.research_mode,
            self.reasoning_depth,
            enabled,
            max_rounds,
            self.auto_resume_on_quota,
        ))
    }
}

/// One row of `agent_tasks`: `{id, kind, status, created_at, updated_at}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentTaskSummary {
    /// Task id.
    pub id: String,
    /// Stable conversation owning this run.
    pub conversation_id: String,
    /// Task kind, e.g. "chat".
    pub kind: String,
    /// Lifecycle status: running / suspended / completed / failed / cancelled.
    pub status: String,
    /// Creation time, unix seconds.
    pub created_at: i64,
    /// Last update time, unix seconds.
    pub updated_at: i64,
}

/// One conversation header: `{id, title, created_at}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConversationSummary {
    /// Conversation id (equals the task id).
    pub id: String,
    /// Title (the agent stores the task kind here), if any.
    pub title: Option<String>,
    /// Creation time, unix seconds.
    pub created_at: i64,
}

/// Current unix time in seconds.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The error returned when no MiniMax key is configured.
fn no_key_error() -> CmdError {
    CmdError::new(
        "no_key",
        "未配置 MiniMax API Key,请先到「设置」页填写密钥后再使用 Agent",
    )
}

/// Generate a fresh task/conversation id.
fn new_id(prefix: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = TASK_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{millis:x}-{seq:04x}")
}

fn new_task_id() -> String {
    new_id("run")
}

fn new_conversation_id() -> String {
    new_id("conv")
}

/// One engine per task: the builtin tools are stateless over the shared
/// market/storage context, so no engine pooling is needed.
fn build_engine(state: &AppState, backend: Arc<MinimaxClient>) -> AgentEngine {
    let market: Arc<dyn DataProvider> = state.market.clone();
    let ctx = ToolContext {
        market,
        storage: state.storage.clone(),
        graph: Some(state.graph.clone()),
        fundamental: Some(state.fundamental.clone()),
        joinquant: Some(state.market.joinquant.clone()),
        minimax_search: Some(backend.clone()),
        finance_news: Some(state.market.finance_news.clone()),
        iwencai: Some(state.market.iwencai.clone()),
    };
    AgentEngine::new(backend, default_registry(), ctx, EngineConfig::default())
}

/// `agent-event` payload: `{task_id, event}` where `event` keeps the agent
/// crate's own serde tags (`{type: "text_delta", ...}`).
#[derive(Debug, Clone, Serialize)]
pub struct AgentStreamEnvelope {
    /// Unique execution id.
    pub run_id: String,
    /// Stable conversation id.
    pub conversation_id: String,
    /// Monotonically increasing sequence number within the run.
    pub seq: u64,
    /// Typed engine event.
    pub event: AgentEvent,
}

fn event_payload(
    run_id: &str,
    conversation_id: &str,
    seq: u64,
    event: &AgentEvent,
) -> AgentStreamEnvelope {
    AgentStreamEnvelope {
        run_id: run_id.to_string(),
        conversation_id: conversation_id.to_string(),
        seq,
        event: event.clone(),
    }
}

fn task_conversation_id(task: &AgentTask) -> String {
    serde_json::from_str::<Value>(&task.state_json)
        .ok()
        .and_then(|v| {
            v.pointer("/spec/conversation_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| task.id.clone())
}

fn task_auto_resume_on_quota(task: &AgentTask) -> bool {
    serde_json::from_str::<Value>(&task.state_json)
        .ok()
        .and_then(|value| {
            value
                .pointer("/spec/auto_resume_on_quota")
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

/// Project a stored task row to its summary (drops `state_json`).
fn task_summary(task: AgentTask) -> AgentTaskSummary {
    let conversation_id = task_conversation_id(&task);
    AgentTaskSummary {
        id: task.id,
        conversation_id,
        kind: task.kind,
        status: task.status,
        created_at: task.created_at,
        updated_at: task.updated_at,
    }
}

/// Normalized function call exposed to the desktop UI.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryToolCall {
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

impl From<ToolCall> for HistoryToolCall {
    fn from(call: ToolCall) -> Self {
        let (name, arguments) = call
            .function
            .map(|function| (function.name, function.arguments))
            .unwrap_or_default();
        Self {
            id: call.id,
            name,
            arguments,
        }
    }
}

/// Explicit Rust -> TypeScript history contract. `content` is always text,
/// regardless of whether the provider stored a JSON string or multipart
/// content. Malformed legacy rows remain loadable and are marked.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentHistoryMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    pub tool_calls: Vec<HistoryToolCall>,
    pub tool_call_id: Option<String>,
    pub created_at: i64,
    pub malformed: bool,
}

fn normalize_message(message: &StoredChatMessage) -> AgentHistoryMessage {
    match serde_json::from_str::<ProviderChatMessage>(&message.content) {
        Ok(provider) => {
            let content = provider.content_text().unwrap_or_default();
            AgentHistoryMessage {
                id: message.id.clone(),
                role: if provider.role.is_empty() {
                    message.role.clone()
                } else {
                    provider.role
                },
                content,
                tool_calls: provider
                    .tool_calls
                    .unwrap_or_default()
                    .into_iter()
                    .map(HistoryToolCall::from)
                    .collect(),
                tool_call_id: provider.tool_call_id,
                created_at: message.created_at,
                malformed: false,
            }
        }
        Err(_) => AgentHistoryMessage {
            id: message.id.clone(),
            role: message.role.clone(),
            content: message.content.clone(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            created_at: message.created_at,
            malformed: true,
        },
    }
}

/// Spawn the forwarder draining `stream` into `agent-event` emissions and
/// register its handle. The stream ends right after the terminal event
/// (the engine worker drops its sender), so the handle removes itself when
/// the loop exits — covering Completed, Failed and Suspended alike.
fn spawn_forwarder(
    handles: Arc<Mutex<std::collections::HashMap<String, tauri::async_runtime::JoinHandle<()>>>>,
    task_id: String,
    conversation_id: String,
    engine: AgentEngine,
    auto_resume_on_quota: bool,
    mut stream: TaskStream,
    on_event: Channel<AgentStreamEnvelope>,
) {
    let id = task_id.clone();
    let forwarder = {
        let handles = Arc::clone(&handles);
        tauri::async_runtime::spawn(async move {
            let mut seq = 0_u64;
            let mut resume_attempt = 0_u32;
            loop {
                let mut reset_at_unix = None;
                while let Some(event) = stream.next().await {
                    if let AgentEvent::Suspended {
                        reason: astock_agent::SuspendReason::QuotaExhausted { reset_at_unix: at },
                    } = &event
                    {
                        reset_at_unix = *at;
                    }
                    seq += 1;
                    if let Err(e) = on_event.send(event_payload(&id, &conversation_id, seq, &event))
                    {
                        tracing::warn!(task_id = %id, error = %e, "agent channel send failed");
                    }
                }
                if !auto_resume_on_quota {
                    break;
                }
                let Some(delay) = quota_resume_delay(reset_at_unix, resume_attempt) else {
                    break;
                };
                tracing::info!(task_id = %id, ?delay, "waiting for quota window before automatic resume");
                tokio::time::sleep(delay).await;
                match engine.resume_task(&id).await {
                    Ok(next) => {
                        stream = next;
                        resume_attempt = resume_attempt.saturating_add(1);
                    }
                    Err(error) => {
                        tracing::warn!(task_id = %id, %error, "automatic agent resume failed");
                        break;
                    }
                }
            }
            handles.lock().expect("agent handles poisoned").remove(&id);
        })
    };
    handles
        .lock()
        .expect("agent handles poisoned")
        .insert(task_id, forwarder);
}

fn quota_resume_delay(reset_at_unix: Option<u64>, attempt: u32) -> Option<std::time::Duration> {
    let reset_at = reset_at_unix?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(reset_at);
    let until_reset = reset_at.saturating_sub(now).saturating_add(3);
    let retry_floor = 5_u64.saturating_mul(1_u64 << attempt.min(5));
    Some(std::time::Duration::from_secs(until_reset.max(retry_floor)))
}

/// Start a new agent task answering `question`. Requires a configured
/// MiniMax key (kind `no_key` otherwise). Returns immediately; progress
/// streams on `agent-event`.
#[tauri::command(rename_all = "snake_case")]
pub async fn agent_ask(
    state: State<'_, AppState>,
    question: String,
    conversation_id: Option<String>,
    options: Option<AgentRunOptions>,
    on_event: Channel<AgentStreamEnvelope>,
) -> Result<AgentAskResponse, CmdError> {
    let question = question.trim().to_string();
    if question.is_empty() {
        return Err(CmdError::new("invalid_param", "question must not be empty"));
    }
    if !state.ensure_minimax().await? {
        return Err(no_key_error());
    }
    let backend = state
        .minimax
        .read()
        .await
        .clone()
        .expect("ensure_minimax just built it");

    let (research_mode, reasoning_depth, enabled_tools, max_rounds, auto_resume_on_quota) =
        options.unwrap_or_default().normalize()?;
    let model_routing = crate::commands::settings::load_agent_model_routing(&state.storage).await;
    let routed_model = model_routing.route_for(&research_mode, &reasoning_depth);
    let specialist_count = if !model_routing.multi_agent_enabled
        || research_mode == "quick"
        || reasoning_depth == "standard"
    {
        0
    } else if reasoning_depth == "maximum" || research_mode == "plan" {
        usize::from(model_routing.max_parallel_agents)
    } else {
        usize::from(model_routing.max_parallel_agents.min(2))
    };
    let specialist_model = model_routing.verifier_model(routed_model.as_deref());
    let specialist_templates = [
        (
            "证据审计师",
            "逐项检查数字是否来自工具、时点和口径是否一致，指出证据冲突、陈旧数据与缺口。",
        ),
        (
            "风险与反方分析师",
            "站在反方检查行业、公司、流动性、仓位集中、尾部情景和结论失效条件。",
        ),
        (
            "量化策略审计师",
            "检查未来函数、过拟合、样本外不足、参数敏感性、交易成本、容量和A股执行约束。",
        ),
        (
            "政策与事件审计师",
            "检查政策和新闻的时间、来源权威性、传导链条以及市场是否已经提前定价。",
        ),
    ];
    let specialists = specialist_templates
        .into_iter()
        .take(specialist_count)
        .map(|(name, instruction)| SpecialistRoute {
            name: name.to_string(),
            instruction: instruction.to_string(),
            model: specialist_model.clone(),
        })
        .collect();
    let conversation_id = conversation_id
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(new_conversation_id);
    let task_id = new_task_id();
    let engine = build_engine(&state, backend);
    let mut spec = TaskSpec::new(task_id.clone(), "chat", question)
        .in_conversation(conversation_id.clone())
        .with_run_options(
            research_mode,
            reasoning_depth,
            enabled_tools,
            auto_resume_on_quota,
        )
        .with_specialists(specialists);
    spec.max_rounds = Some(max_rounds);
    spec.model = routed_model;
    let stream = engine.run_task(spec);
    spawn_forwarder(
        Arc::clone(&state.agent_handles),
        task_id.clone(),
        conversation_id.clone(),
        engine,
        auto_resume_on_quota,
        stream,
        on_event,
    );
    Ok(AgentAskResponse {
        conversation_id,
        task_id,
    })
}

/// Resume a suspended (or interrupted) task; events flow on the same
/// `agent-event` channel. Fails with kind `not_found` / `not_resumable`
/// when the task cannot be resumed.
#[tauri::command(rename_all = "snake_case")]
pub async fn agent_resume(
    state: State<'_, AppState>,
    task_id: String,
    on_event: Channel<AgentStreamEnvelope>,
) -> Result<AgentResumeResponse, CmdError> {
    if !state.ensure_minimax().await? {
        return Err(no_key_error());
    }
    let backend = state
        .minimax
        .read()
        .await
        .clone()
        .expect("ensure_minimax just built it");

    let record = state
        .storage
        .agent_task_get(&task_id)
        .await?
        .ok_or_else(|| CmdError::new("not_found", format!("task not found: {task_id}")))?;
    let conversation_id = task_conversation_id(&record);
    let auto_resume_on_quota = task_auto_resume_on_quota(&record);
    let engine = build_engine(&state, backend);
    let stream = engine.resume_task(&task_id).await.map_err(|e| match e {
        AgentError::TaskNotFound(_) => CmdError::new("not_found", e.to_string()),
        AgentError::NotResumable(..) => CmdError::new("not_resumable", e.to_string()),
        other => CmdError::new("agent", other.to_string()),
    })?;
    spawn_forwarder(
        Arc::clone(&state.agent_handles),
        task_id,
        conversation_id,
        engine,
        auto_resume_on_quota,
        stream,
        on_event,
    );
    Ok(AgentResumeResponse { resumed: true })
}

/// All persisted agent tasks, most recently updated first.
#[tauri::command(rename_all = "snake_case")]
pub async fn agent_tasks(state: State<'_, AppState>) -> Result<Vec<AgentTaskSummary>, CmdError> {
    let tasks = state.storage.agent_task_list().await?;
    let live_ids: std::collections::HashSet<String> = state
        .agent_handles
        .lock()
        .expect("agent handles poisoned")
        .keys()
        .cloned()
        .collect();
    Ok(tasks
        .into_iter()
        .map(|task| {
            let mut summary = task_summary(task);
            if summary.status == "running" && !live_ids.contains(&summary.id) {
                summary.status = "interrupted".to_string();
            }
            summary
        })
        .collect())
}

/// Mark a task cancelled. In-flight tool futures are dropped on the engine's
/// next heartbeat; the event forwarder is aborted immediately.
/// `cancelled: false` when the task does not exist.
#[tauri::command(rename_all = "snake_case")]
pub async fn agent_cancel(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<AgentCancelResponse, CmdError> {
    let Some(record) = state.storage.agent_task_get(&task_id).await? else {
        return Ok(AgentCancelResponse { cancelled: false });
    };
    state
        .storage
        .agent_task_save(AgentTask {
            status: "cancelled".to_string(),
            updated_at: now_secs(),
            ..record
        })
        .await?;
    if let Some(handle) = state
        .agent_handles
        .lock()
        .expect("agent handles poisoned")
        .remove(&task_id)
    {
        handle.abort();
    }
    Ok(AgentCancelResponse { cancelled: true })
}

/// All agent conversations, most recently created first.
#[tauri::command(rename_all = "snake_case")]
pub async fn agent_conversations(
    state: State<'_, AppState>,
) -> Result<Vec<ConversationSummary>, CmdError> {
    let conversations = state.storage.conversation_list().await?;
    Ok(conversations
        .into_iter()
        .map(|c| ConversationSummary {
            id: c.id,
            title: c.title,
            created_at: c.created_at,
        })
        .collect())
}

/// Load one conversation's messages in chronological order. Each message is
/// `{id, role, content, created_at}` with `content` parsed back to the full
/// provider message JSON when possible.
#[tauri::command(rename_all = "snake_case")]
pub async fn agent_conversation_load(
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<Vec<AgentHistoryMessage>, CmdError> {
    let messages = state.storage.conversation_load(&conversation_id).await?;
    Ok(messages.iter().map(normalize_message).collect())
}

/// Delete a conversation and all of its messages.
#[tauri::command(rename_all = "snake_case")]
pub async fn agent_conversation_delete(
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<AgentCancelResponse, CmdError> {
    let deleted = state.storage.conversation_delete(&conversation_id).await?;
    Ok(AgentCancelResponse { cancelled: deleted })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_payload_wraps_serde_tagged_event() {
        let event = AgentEvent::TextDelta {
            text: "你好".into(),
        };
        let payload = serde_json::to_value(event_payload("r1", "c1", 1, &event)).unwrap();
        assert_eq!(payload["run_id"], "r1");
        assert_eq!(payload["conversation_id"], "c1");
        assert_eq!(payload["seq"], 1);
        assert_eq!(payload["event"]["type"], "text_delta");
        assert_eq!(payload["event"]["text"], "你好");
    }

    #[test]
    fn no_key_error_points_to_settings() {
        let err = no_key_error();
        assert_eq!(err.kind, "no_key");
        assert!(err.error.contains("设置"));
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["kind"], "no_key");
    }

    #[test]
    fn task_summary_drops_state_json() {
        let summary = task_summary(AgentTask {
            id: "t1".into(),
            kind: "chat".into(),
            status: "suspended".into(),
            state_json: "{\"round\":1}".into(),
            created_at: 1_700_000_000,
            updated_at: 1_700_000_100,
        });
        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["id"], "t1");
        assert_eq!(json["conversation_id"], "t1");
        assert_eq!(json["status"], "suspended");
        assert_eq!(json["updated_at"], 1_700_000_100);
        assert!(json.get("state_json").is_none());
    }

    #[test]
    fn message_contract_normalizes_provider_message_or_keeps_text() {
        let stored = StoredChatMessage {
            id: "t1-0002".into(),
            conversation_id: "t1".into(),
            role: "assistant".into(),
            content: "{\"role\":\"assistant\",\"content\":\"答案\"}".into(),
            tool_calls: None,
            created_at: 1_700_000_000,
        };
        let message = normalize_message(&stored);
        assert_eq!(message.role, "assistant");
        assert_eq!(message.content, "答案");
        assert!(!message.malformed);

        let plain = StoredChatMessage {
            content: "plain text".into(),
            ..stored
        };
        let message = normalize_message(&plain);
        assert_eq!(message.content, "plain text");
        assert!(message.malformed);
    }

    #[test]
    fn run_options_are_future_safe_and_registry_ordered() {
        let (mode, depth, tools, rounds, auto_resume) = AgentRunOptions {
            research_mode: "plan".into(),
            reasoning_depth: "maximum".into(),
            enabled_tools: Some(vec!["run_backtest".into(), "get_quote".into()]),
            auto_resume_on_quota: true,
        }
        .normalize()
        .unwrap();
        assert_eq!(mode, "plan");
        assert_eq!(depth, "maximum");
        assert_eq!(tools, vec!["get_quote", "run_backtest"]);
        assert_eq!(rounds, 48);
        assert!(auto_resume);
    }

    #[test]
    fn automatic_quota_resume_has_a_retry_floor() {
        let now = now_secs().max(0) as u64;
        assert!(quota_resume_delay(Some(now), 0).unwrap().as_secs() >= 5);
        assert!(quota_resume_delay(Some(now), 3).unwrap().as_secs() >= 40);
        assert!(quota_resume_delay(None, 0).is_none());
    }
}
