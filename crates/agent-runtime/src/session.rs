use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::RuntimeError;
use crate::events::AgentPhase;
use crate::model::{Message, MessageRole};
use crate::store::AgentStore;

pub const SESSION_VERSION: &str = "rust-agent-session-v1";
pub const MAX_SESSION_MESSAGES: usize = 1_000;
pub const MAX_SESSION_TEXT_CHARS: usize = 2_000_000;
pub const MAX_MODEL_HISTORY_MESSAGES: usize = 40;
pub const MAX_MODEL_HISTORY_CHARS: usize = 120_000;
pub const MAX_SESSION_SUMMARY_CHARS: usize = 30_000;
const MAX_SUMMARY_MESSAGE_CHARS: usize = 480;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMessageRole {
    User,
    Agent,
    System,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMessage {
    pub id: String,
    pub role: SessionMessageRole,
    pub text: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTaskState {
    pub task_id: String,
    pub phase: AgentPhase,
    pub accepted_seq: u64,
    #[serde(default)]
    pub model_round: usize,
    #[serde(default)]
    pub completed_tool_ids: Vec<String>,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    /// User-visible research plan. Persisted so an interrupted task can be
    /// resumed with its plan intact instead of silently losing the work
    /// breakdown the user was watching. `default` keeps sessions written by
    /// earlier versions readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<crate::plan::Plan>,
}

/// Persisted multi-turn session shared by terminal and future desktop
/// adapters. Camel-case top-level names retain compatibility with the current
/// v6 conversation projection while the task checkpoint stays snake-case.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSession {
    #[serde(default = "default_session_version")]
    pub version: String,
    pub session_id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub input: String,
    pub depth: String,
    pub tool_policy: String,
    #[serde(default)]
    pub messages: Vec<SessionMessage>,
    #[serde(default)]
    pub task: Option<SessionTaskState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl RuntimeSession {
    pub fn new(depth: impl Into<String>, tool_policy: impl Into<String>) -> Self {
        let now = now_millis();
        Self {
            version: SESSION_VERSION.into(),
            session_id: Uuid::new_v4().to_string(),
            title: "新研究".into(),
            created_at: now,
            updated_at: now,
            input: String::new(),
            depth: depth.into(),
            tool_policy: tool_policy.into(),
            messages: Vec::new(),
            task: None,
            summary: None,
        }
    }

    pub fn validate(&self) -> Result<(), RuntimeError> {
        validate_id(&self.session_id, "session_id")?;
        if self.version != SESSION_VERSION {
            return Err(RuntimeError::Configuration(format!(
                "unsupported session version `{}`",
                self.version
            )));
        }
        if self.title.trim().is_empty() || self.title.len() > 240 {
            return Err(RuntimeError::Configuration(
                "session title must contain 1..240 bytes".into(),
            ));
        }
        if self.messages.len() > MAX_SESSION_MESSAGES {
            return Err(RuntimeError::Configuration(format!(
                "session contains more than {MAX_SESSION_MESSAGES} messages"
            )));
        }
        let mut chars = 0usize;
        for message in &self.messages {
            validate_id(&message.id, "message_id")?;
            if message.text.trim().is_empty() {
                return Err(RuntimeError::Configuration(
                    "session messages must not be empty".into(),
                ));
            }
            chars = chars.saturating_add(message.text.chars().count());
        }
        if chars > MAX_SESSION_TEXT_CHARS {
            return Err(RuntimeError::Configuration(format!(
                "session text exceeds {MAX_SESSION_TEXT_CHARS} characters"
            )));
        }
        if self.summary.as_ref().is_some_and(|summary| {
            summary.trim().is_empty() || summary.chars().count() > MAX_SESSION_SUMMARY_CHARS
        }) {
            return Err(RuntimeError::Configuration(format!(
                "session summary must contain 1..{MAX_SESSION_SUMMARY_CHARS} characters"
            )));
        }
        Ok(())
    }

    pub fn push_message(&mut self, role: SessionMessageRole, text: impl Into<String>) {
        self.messages.push(SessionMessage {
            id: Uuid::new_v4().to_string(),
            role,
            text: text.into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        });
        self.updated_at = now_millis();
    }

    pub fn model_history(&self) -> Vec<Message> {
        let relevant = self.relevant_messages();
        let retained = retained_history_count(&relevant);
        relevant[relevant.len().saturating_sub(retained)..]
            .iter()
            .map(|message| {
                let role = match message.role {
                    SessionMessageRole::User => MessageRole::User,
                    SessionMessageRole::Agent => MessageRole::Assistant,
                    SessionMessageRole::System | SessionMessageRole::Tool => {
                        unreachable!("relevant_messages filters non-model roles")
                    }
                };
                Message::text(role, message.text.clone())
            })
            .collect()
    }

    /// Refresh an extractive, deterministic index of messages omitted from
    /// the bounded model history. Full messages remain untouched in durable
    /// storage; this is context compaction, not conversation deletion.
    pub fn refresh_compacted_summary(&mut self) -> bool {
        let relevant = self.relevant_messages();
        let retained = retained_history_count(&relevant);
        let omitted_count = relevant.len().saturating_sub(retained);
        if omitted_count == 0 {
            return false;
        }
        let mut lines = Vec::new();
        let mut used = 0usize;
        for message in relevant[..omitted_count].iter().rev() {
            let role = match message.role {
                SessionMessageRole::User => "用户",
                SessionMessageRole::Agent => "Agent",
                SessionMessageRole::System | SessionMessageRole::Tool => continue,
            };
            let normalized = message
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let excerpt = truncate_chars(&normalized, MAX_SUMMARY_MESSAGE_CHARS);
            let line = format!("- {role}: {excerpt}");
            let line_chars = line.chars().count() + 1;
            if used.saturating_add(line_chars) > MAX_SESSION_SUMMARY_CHARS - 160 {
                break;
            }
            used += line_chars;
            lines.push(line);
        }
        lines.reverse();
        let included = lines.len();
        let mut summary = format!(
            "较早消息压缩索引：共省略 {omitted_count} 条，保留其中最近 {included} 条摘录。原文仍在持久会话中；涉及当前价格、日期、额度或结论时必须重新取证。"
        );
        if !lines.is_empty() {
            summary.push('\n');
            summary.push_str(&lines.join("\n"));
        }
        self.summary = Some(summary);
        self.updated_at = now_millis();
        true
    }

    pub fn ensure_title(&mut self, objective: &str) {
        if self.messages.is_empty() && self.title == "新研究" {
            let title = objective.trim().chars().take(60).collect::<String>();
            if !title.is_empty() {
                self.title = title;
            }
        }
    }

    fn relevant_messages(&self) -> Vec<&SessionMessage> {
        self.messages
            .iter()
            .filter(|message| {
                matches!(
                    message.role,
                    SessionMessageRole::User | SessionMessageRole::Agent
                )
            })
            .collect()
    }
}

fn retained_history_count(messages: &[&SessionMessage]) -> usize {
    let mut retained = 0usize;
    let mut chars = 0usize;
    for message in messages.iter().rev() {
        let count = message.text.chars().count();
        if retained >= MAX_MODEL_HISTORY_MESSAGES
            || chars.saturating_add(count) > MAX_MODEL_HISTORY_CHARS
        {
            break;
        }
        retained += 1;
        chars += count;
    }
    retained
}

fn truncate_chars(text: &str, maximum: usize) -> String {
    let mut chars = text.chars();
    let truncated = chars.by_ref().take(maximum).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub conversation_id: String,
    pub title: String,
    pub session: RuntimeSession,
    pub parent_conversation_id: Option<String>,
    pub branch_from_message_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub conversation_id: String,
    pub title: String,
    pub phase: String,
    pub message_count: usize,
    pub evidence_count: usize,
    pub parent_conversation_id: Option<String>,
    pub branch_from_message_id: Option<String>,
    pub branch_from_checkpoint_task_id: Option<String>,
    pub branch_from_checkpoint_seq: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBranchRequest {
    pub source_session_id: String,
    pub new_session_id: String,
    pub message_id: String,
    pub title: String,
    pub checkpoint_task_id: Option<String>,
    pub checkpoint_accepted_seq: Option<i64>,
}

#[derive(Clone)]
pub struct SessionManager {
    store: Arc<dyn AgentStore>,
}

impl SessionManager {
    pub fn new(store: Arc<dyn AgentStore>) -> Self {
        Self { store }
    }

    pub async fn save(&self, session: &RuntimeSession) -> Result<StoredSession, RuntimeError> {
        session.validate()?;
        self.store
            .save_session(session)
            .await
            .map_err(RuntimeError::Store)
    }

    pub async fn load(&self, session_id: &str) -> Result<StoredSession, RuntimeError> {
        validate_id(session_id, "session_id")?;
        let stored = self
            .store
            .load_session(session_id)
            .await
            .map_err(RuntimeError::Store)?;
        stored.session.validate()?;
        Ok(stored)
    }

    pub async fn list(
        &self,
        limit: usize,
        query: Option<&str>,
    ) -> Result<Vec<SessionSummary>, RuntimeError> {
        if !(1..=500).contains(&limit) {
            return Err(RuntimeError::Configuration(
                "session list limit must be between 1 and 500".into(),
            ));
        }
        self.store
            .list_sessions(limit, query)
            .await
            .map_err(RuntimeError::Store)
    }

    pub async fn latest(&self) -> Result<Option<StoredSession>, RuntimeError> {
        let Some(summary) = self.list(1, None).await?.into_iter().next() else {
            return Ok(None);
        };
        self.load(&summary.conversation_id).await.map(Some)
    }

    /// Create an immutable-history branch at a selected message. When the
    /// selected point is the latest message, the current durable checkpoint is
    /// supplied to the Engine for an additional task/sequence consistency
    /// check. The new conversation never copies executable task state.
    pub async fn branch(
        &self,
        source_session_id: &str,
        message_id: Option<&str>,
        title: Option<&str>,
    ) -> Result<StoredSession, RuntimeError> {
        let source = self.load(source_session_id).await?;
        let selected = match message_id {
            Some(message_id) => source
                .session
                .messages
                .iter()
                .find(|message| message.id == message_id)
                .ok_or_else(|| {
                    RuntimeError::Configuration(format!(
                        "message `{message_id}` does not belong to session `{source_session_id}`"
                    ))
                })?,
            None => source.session.messages.last().ok_or_else(|| {
                RuntimeError::Configuration("cannot branch a session without messages".into())
            })?,
        };
        let selected_is_latest = source
            .session
            .messages
            .last()
            .is_some_and(|message| message.id == selected.id);
        let checkpoint = selected_is_latest
            .then_some(source.session.task.as_ref())
            .flatten()
            .filter(|task| task.accepted_seq > 0);
        let title = title
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{} · 分支", source.title));
        if title.len() > 240 {
            return Err(RuntimeError::Configuration(
                "branch title must contain at most 240 bytes".into(),
            ));
        }
        let checkpoint_accepted_seq = checkpoint
            .map(|task| {
                i64::try_from(task.accepted_seq).map_err(|_| {
                    RuntimeError::Configuration(
                        "session checkpoint sequence exceeds Engine range".into(),
                    )
                })
            })
            .transpose()?;
        let request = SessionBranchRequest {
            source_session_id: source.conversation_id,
            new_session_id: Uuid::new_v4().to_string(),
            message_id: selected.id.clone(),
            title,
            checkpoint_task_id: checkpoint.map(|task| task.task_id.clone()),
            checkpoint_accepted_seq,
        };
        let branched = self
            .store
            .branch_session(&request)
            .await
            .map_err(RuntimeError::Store)?;
        branched.session.validate()?;
        if branched.session.task.is_some() {
            return Err(RuntimeError::Store(
                "Engine returned a branch with executable task state".into(),
            ));
        }
        Ok(branched)
    }
}

fn validate_id(value: &str, name: &str) -> Result<(), RuntimeError> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        Err(RuntimeError::Configuration(format!(
            "{name} must contain 1..128 visible bytes"
        )))
    } else {
        Ok(())
    }
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn default_session_version() -> String {
    SESSION_VERSION.into()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn completed_v6_conversation_without_rust_version_remains_readable() {
        let session: RuntimeSession = serde_json::from_value(json!({
            "sessionId": "legacy-session",
            "title": "旧研究",
            "createdAt": 1_700_000_000_000_i64,
            "updatedAt": 1_700_000_001_000_i64,
            "input": "",
            "depth": "deep",
            "toolPolicy": "full",
            "messages": [{
                "id": "legacy-message",
                "role": "agent",
                "text": "已完成报告",
                "timestamp": "2026-08-25T00:00:00Z"
            }],
            "task": {
                "task_id": "legacy-task",
                "phase": "completed",
                "accepted_seq": 9,
                "legacy_projection_field": true
            },
            "effects": [],
            "verification": null
        }))
        .unwrap();

        session.validate().unwrap();
        assert_eq!(session.version, SESSION_VERSION);
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.task.unwrap().model_round, 0);
    }

    #[test]
    fn compaction_keeps_full_history_and_bounds_model_context() {
        let mut session = RuntimeSession::new("deep", "full");
        for index in 0..45 {
            let role = if index % 2 == 0 {
                SessionMessageRole::User
            } else {
                SessionMessageRole::Agent
            };
            session.push_message(role, format!("历史消息 {index}"));
        }

        assert_eq!(session.messages.len(), 45);
        assert_eq!(session.model_history().len(), MAX_MODEL_HISTORY_MESSAGES);
        assert!(session.refresh_compacted_summary());
        assert_eq!(session.messages.len(), 45);
        assert!(session.summary.as_deref().unwrap().contains("共省略 5 条"));
        session.validate().unwrap();
    }
}
