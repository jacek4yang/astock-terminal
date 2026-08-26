use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::events::{AgentEvent, AgentPhase};
use crate::runtime::RuntimeTask;
use crate::session::{RuntimeSession, SessionBranchRequest, SessionSummary, StoredSession};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCheckpoint {
    pub task_id: String,
    pub phase: AgentPhase,
    pub accepted_seq: u64,
    pub model_round: usize,
    pub completed_tool_ids: Vec<String>,
    pub evidence_ids: Vec<String>,
    pub state_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectIntent {
    pub effect_id: String,
    pub task_id: String,
    pub caused_by_seq: u64,
    pub effect_kind: String,
    pub payload: Value,
    pub idempotency_key: String,
}

#[async_trait]
pub trait AgentStore: Send + Sync {
    async fn create_task(&self, task_id: &str, task: &RuntimeTask) -> Result<(), String>;
    async fn append_event(&self, task_id: &str, seq: u64, event: &AgentEvent)
        -> Result<(), String>;
    async fn put_checkpoint(&self, checkpoint: &StoredCheckpoint) -> Result<(), String>;
    async fn begin_effect(&self, intent: &EffectIntent) -> Result<(), String>;
    async fn complete_effect(
        &self,
        effect_id: &str,
        status: &str,
        result: &Value,
    ) -> Result<(), String>;

    async fn save_session(&self, _session: &RuntimeSession) -> Result<StoredSession, String> {
        Err("session persistence is not supported by this AgentStore".into())
    }

    async fn load_session(&self, _session_id: &str) -> Result<StoredSession, String> {
        Err("session persistence is not supported by this AgentStore".into())
    }

    async fn list_sessions(
        &self,
        _limit: usize,
        _query: Option<&str>,
    ) -> Result<Vec<SessionSummary>, String> {
        Err("session persistence is not supported by this AgentStore".into())
    }

    async fn branch_session(
        &self,
        _request: &SessionBranchRequest,
    ) -> Result<StoredSession, String> {
        Err("session branching is not supported by this AgentStore".into())
    }
}
