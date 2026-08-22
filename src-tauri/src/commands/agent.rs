//! Agent commands: MiniMax-driven Q&A tasks with suspend/resume
//! (docs/command-contract.md §Agent).
//!
//! `agent_ask` / `agent_resume` return immediately; every [`AgentEvent`] is
//! forwarded as a Tauri `agent-event` emission with payload
//! `{task_id, event}` until the stream ends (Completed / Failed /
//! Suspended), at which point the forwarder handle is dropped from
//! [`AppState::agent_handles`]. `agent_cancel` marks the task `cancelled`
//! in storage (the engine notices at the next round) and aborts the
//! forwarder.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use astock_agent::{
    default_registry, AgentEngine, AgentError, AgentEvent, EngineConfig, TaskSpec, TaskStream,
    ToolContext,
};
use astock_market_data::DataProvider;
use astock_minimax::MinimaxClient;
use astock_storage::{AgentTask, ChatMessage as StoredChatMessage};
use futures::StreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};

use crate::error::CmdError;
use crate::state::AppState;

/// Tauri event name every agent event is forwarded on.
pub(crate) const AGENT_EVENT: &str = "agent-event";

/// Task-id disambiguator within the same millisecond.
static TASK_COUNTER: AtomicU32 = AtomicU32::new(0);

/// `agent_ask` response: the task id doubles as the conversation id.
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

/// One row of `agent_tasks`: `{id, kind, status, created_at, updated_at}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentTaskSummary {
    /// Task id.
    pub id: String,
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
fn new_task_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = TASK_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("chat-{millis:x}-{seq:04x}")
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
    };
    AgentEngine::new(backend, default_registry(), ctx, EngineConfig::default())
}

/// `agent-event` payload: `{task_id, event}` where `event` keeps the agent
/// crate's own serde tags (`{type: "text_delta", ...}`).
fn event_payload(task_id: &str, event: &AgentEvent) -> Value {
    json!({ "task_id": task_id, "event": event })
}

/// Project a stored task row to its summary (drops `state_json`).
fn task_summary(task: AgentTask) -> AgentTaskSummary {
    AgentTaskSummary {
        id: task.id,
        kind: task.kind,
        status: task.status,
        created_at: task.created_at,
        updated_at: task.updated_at,
    }
}

/// One stored message as JSON. The agent crate stores the full provider
/// message (role/content/tool_calls/tool_call_id) serialized in `content`;
/// surface it as a parsed object, falling back to raw text for rows written
/// by other components.
fn message_json(message: &StoredChatMessage) -> Value {
    let content = serde_json::from_str::<Value>(&message.content)
        .unwrap_or_else(|_| Value::String(message.content.clone()));
    json!({
        "id": message.id,
        "role": message.role,
        "content": content,
        "created_at": message.created_at,
    })
}

/// Spawn the forwarder draining `stream` into `agent-event` emissions and
/// register its handle. The stream ends right after the terminal event
/// (the engine worker drops its sender), so the handle removes itself when
/// the loop exits — covering Completed, Failed and Suspended alike.
fn spawn_forwarder(
    handles: Arc<Mutex<std::collections::HashMap<String, tauri::async_runtime::JoinHandle<()>>>>,
    task_id: String,
    mut stream: TaskStream,
    app: AppHandle,
) {
    let id = task_id.clone();
    let forwarder = {
        let handles = Arc::clone(&handles);
        tauri::async_runtime::spawn(async move {
            while let Some(event) = stream.next().await {
                if let Err(e) = app.emit(AGENT_EVENT, event_payload(&id, &event)) {
                    tracing::warn!(task_id = %id, error = %e, "agent event emit failed");
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

/// Start a new agent task answering `question`. Requires a configured
/// MiniMax key (kind `no_key` otherwise). Returns immediately; progress
/// streams on `agent-event`.
#[tauri::command(rename_all = "snake_case")]
pub async fn agent_ask(
    state: State<'_, AppState>,
    app: AppHandle,
    question: String,
    conversation_id: Option<String>,
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

    let task_id = conversation_id
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(new_task_id);
    let engine = build_engine(&state, backend);
    let stream = engine.run_task(TaskSpec::new(task_id.clone(), "chat", question));
    spawn_forwarder(
        Arc::clone(&state.agent_handles),
        task_id.clone(),
        stream,
        app,
    );
    Ok(AgentAskResponse {
        conversation_id: task_id.clone(),
        task_id,
    })
}

/// Resume a suspended (or interrupted) task; events flow on the same
/// `agent-event` channel. Fails with kind `not_found` / `not_resumable`
/// when the task cannot be resumed.
#[tauri::command(rename_all = "snake_case")]
pub async fn agent_resume(
    state: State<'_, AppState>,
    app: AppHandle,
    task_id: String,
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

    let engine = build_engine(&state, backend);
    let stream = engine.resume_task(&task_id).await.map_err(|e| match e {
        AgentError::TaskNotFound(_) => CmdError::new("not_found", e.to_string()),
        AgentError::NotResumable(..) => CmdError::new("not_resumable", e.to_string()),
        other => CmdError::new("agent", other.to_string()),
    })?;
    spawn_forwarder(Arc::clone(&state.agent_handles), task_id, stream, app);
    Ok(AgentResumeResponse { resumed: true })
}

/// All persisted agent tasks, most recently updated first.
#[tauri::command(rename_all = "snake_case")]
pub async fn agent_tasks(state: State<'_, AppState>) -> Result<Vec<AgentTaskSummary>, CmdError> {
    let tasks = state.storage.agent_task_list().await?;
    Ok(tasks.into_iter().map(task_summary).collect())
}

/// Mark a task cancelled. A running engine loop notices at the next round;
/// the event forwarder is aborted immediately. `cancelled: false` when the
/// task does not exist.
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
) -> Result<Vec<Value>, CmdError> {
    let messages = state.storage.conversation_load(&conversation_id).await?;
    Ok(messages.iter().map(message_json).collect())
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
        let payload = event_payload("t1", &event);
        assert_eq!(payload["task_id"], "t1");
        assert_eq!(payload["event"]["type"], "text_delta");
        assert_eq!(payload["event"]["text"], "你好");
        // Exactly the two contract keys.
        assert_eq!(payload.as_object().unwrap().len(), 2);
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
        assert_eq!(json["status"], "suspended");
        assert_eq!(json["updated_at"], 1_700_000_100);
        assert!(json.get("state_json").is_none());
    }

    #[test]
    fn message_json_parses_provider_message_or_keeps_text() {
        let stored = StoredChatMessage {
            id: "t1-0002".into(),
            conversation_id: "t1".into(),
            role: "assistant".into(),
            content: "{\"role\":\"assistant\",\"content\":\"答案\"}".into(),
            tool_calls: None,
            created_at: 1_700_000_000,
        };
        let json = message_json(&stored);
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["content"]["content"], "答案");

        let plain = StoredChatMessage {
            content: "plain text".into(),
            ..stored
        };
        let json = message_json(&plain);
        assert_eq!(json["content"], "plain text");
    }
}
