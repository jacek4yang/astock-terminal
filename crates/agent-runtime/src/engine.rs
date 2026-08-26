use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use astock_engine::Engine;
use astock_protocol::{RequestEnvelope, ResponseEnvelope, PROTOCOL_VERSION};

use crate::events::AgentEvent;
use crate::runtime::RuntimeTask;
use crate::session::{RuntimeSession, SessionBranchRequest, SessionSummary, StoredSession};
use crate::store::{AgentStore, EffectIntent, StoredCheckpoint};
use crate::tools::ToolExecutor;

pub struct EngineGateway {
    engine: Arc<Engine>,
    sequence: AtomicU64,
}

impl EngineGateway {
    pub fn new(engine: Arc<Engine>) -> Self {
        Self {
            engine,
            sequence: AtomicU64::new(1),
        }
    }

    async fn request(&self, kind: &str, payload: Value) -> Result<Value, String> {
        let request = RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: format!(
                "rust-agent-{}-{}",
                self.sequence.fetch_add(1, Ordering::Relaxed),
                Uuid::new_v4()
            ),
            kind: kind.to_owned(),
            payload,
            deadline_ms: None,
            cancellation_id: None,
        };
        response_payload(self.engine.dispatch(&request).await)
    }

    /// Read a bounded page of versioned source documents without involving a
    /// model provider.
    pub async fn recent_sources(&self, limit: usize) -> Result<Value, String> {
        if !(1..=astock_protocol::MAX_PAGE_SIZE).contains(&limit) {
            return Err(format!(
                "source limit must be between 1 and {}",
                astock_protocol::MAX_PAGE_SIZE
            ));
        }
        self.request("research.sources.list", json!({"limit": limit}))
            .await
    }

    /// Read deterministic Engine cache/storage counters without mutating the
    /// cache or requiring provider credentials.
    pub async fn cache_stats(&self) -> Result<Value, String> {
        self.request("storage.cache.stats", json!({})).await
    }

    /// Install the MiniMax credential in the OS credential store.
    ///
    /// The Engine validates the format, stores the value and verifies it by
    /// read-back, deleting it again if the read-back disagrees. The secret is
    /// passed by value here and never logged; callers must not print it.
    pub async fn set_minimax_credential(&self, key: &str) -> Result<Value, String> {
        self.request("credentials.minimax.set", json!({ "key": key }))
            .await
    }

    /// Install the optional JoinQuant account credential.
    pub async fn set_joinquant_credential(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Value, String> {
        self.request(
            "credentials.joinquant.set",
            json!({ "username": username, "password": password }),
        )
        .await
    }

    /// Report which credentials are installed. Presence only, never values.
    pub async fn credential_status(&self) -> Result<Value, String> {
        self.request("credentials.status", json!({})).await
    }

    /// Remove a stored credential.
    pub async fn delete_credential(&self, provider: &str) -> Result<Value, String> {
        let kind = match provider {
            "minimax" => "credentials.minimax.delete",
            "joinquant" => "credentials.joinquant.delete",
            other => return Err(format!("unknown credential provider `{other}`")),
        };
        self.request(kind, json!({})).await
    }
}

fn response_payload(response: ResponseEnvelope) -> Result<Value, String> {
    if response.ok {
        return Ok(response.payload);
    }
    let error = response.error.ok_or_else(|| {
        format!(
            "Engine request {} failed without a typed error",
            response.request_id
        )
    })?;
    Err(format!("{}: {}", error.code, error.message))
}

#[async_trait]
impl ToolExecutor for EngineGateway {
    async fn execute(
        &self,
        engine_kind: &str,
        payload: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, String> {
        tokio::select! {
            _ = cancellation.cancelled() => Err("cancelled".into()),
            result = self.request(engine_kind, payload) => result,
        }
    }
}

#[async_trait]
impl AgentStore for EngineGateway {
    async fn create_task(&self, task_id: &str, task: &RuntimeTask) -> Result<(), String> {
        self.request(
            "agent.task.create",
            json!({
                "task_id": task_id,
                "reducer_version": "rust-agent-runtime-v1",
                "task_spec": task,
                "phase": "preparing",
            }),
        )
        .await
        .map(|_| ())
    }

    async fn append_event(
        &self,
        task_id: &str,
        seq: u64,
        event: &AgentEvent,
    ) -> Result<(), String> {
        self.request(
            "agent.event.append",
            json!({
                "task_id": task_id,
                "seq": seq,
                "event_id": format!("{task_id}:{seq}"),
                "event_kind": event.kind(),
                "event": event,
            }),
        )
        .await
        .map(|_| ())
    }

    async fn put_checkpoint(&self, checkpoint: &StoredCheckpoint) -> Result<(), String> {
        self.request(
            "agent.checkpoint.put",
            json!({
                "task_id": checkpoint.task_id,
                "accepted_seq": checkpoint.accepted_seq,
                "phase": checkpoint.phase.as_str(),
                "checkpoint": checkpoint,
            }),
        )
        .await
        .map(|_| ())
    }

    async fn begin_effect(&self, intent: &EffectIntent) -> Result<(), String> {
        self.request(
            "agent.effect.begin",
            json!({
                "effect_id": intent.effect_id,
                "task_id": intent.task_id,
                "caused_by_seq": intent.caused_by_seq,
                "effect_kind": intent.effect_kind,
                "effect": intent.payload,
                "idempotency_key": intent.idempotency_key,
            }),
        )
        .await
        .map(|_| ())
    }

    async fn complete_effect(
        &self,
        effect_id: &str,
        status: &str,
        result: &Value,
    ) -> Result<(), String> {
        self.request(
            "agent.effect.complete",
            json!({
                "effect_id": effect_id,
                "status": status,
                "result": result,
            }),
        )
        .await
        .map(|_| ())
    }

    async fn save_session(&self, session: &RuntimeSession) -> Result<StoredSession, String> {
        let value = self
            .request(
                "agent.conversation.save",
                json!({
                    "conversation_id": session.session_id,
                    "title": session.title,
                    "session": session,
                }),
            )
            .await?;
        serde_json::from_value(value).map_err(|error| format!("decode stored session: {error}"))
    }

    async fn load_session(&self, session_id: &str) -> Result<StoredSession, String> {
        let value = self
            .request(
                "agent.conversation.load",
                json!({"conversation_id": session_id}),
            )
            .await?;
        serde_json::from_value(value).map_err(|error| format!("decode stored session: {error}"))
    }

    async fn list_sessions(
        &self,
        limit: usize,
        query: Option<&str>,
    ) -> Result<Vec<SessionSummary>, String> {
        let value = self
            .request(
                "agent.conversation.list",
                json!({"limit": limit, "query": query}),
            )
            .await?;
        serde_json::from_value(value.get("items").cloned().unwrap_or_default())
            .map_err(|error| format!("decode session list: {error}"))
    }

    async fn branch_session(&self, branch: &SessionBranchRequest) -> Result<StoredSession, String> {
        let value = self
            .request(
                "agent.conversation.branch",
                json!({
                    "source_conversation_id": branch.source_session_id,
                    "new_conversation_id": branch.new_session_id,
                    "message_id": branch.message_id,
                    "title": branch.title,
                    "checkpoint_task_id": branch.checkpoint_task_id,
                    "checkpoint_accepted_seq": branch.checkpoint_accepted_seq,
                }),
            )
            .await?;
        serde_json::from_value(value).map_err(|error| format!("decode branched session: {error}"))
    }
}
