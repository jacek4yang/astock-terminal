//! Runtime supervisor around the durable Agent orchestration loop.
//!
//! The core protocol implementation remains in `orchestrator_legacy.rs`.
//! This facade keeps its public API while supervising the event stream like a
//! mature coding Agent: transient provider failures and unexpected worker
//! exits are retried from the last durable round, partial streamed text is
//! explicitly reset, and hard/protocol/storage failures remain terminal.

#[path = "orchestrator_legacy.rs"]
mod legacy;

use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::channel::mpsc;
use futures::{Stream, StreamExt};
use serde_json::Value;

use astock_storage::{AgentTask, Storage};

use crate::backend::ChatBackend;
use crate::error::{AgentError, Result};
use crate::tools::{ToolContext, ToolRegistry};

pub use legacy::{
    compact_history, AgentEvent, EngineConfig, SpecialistRoute, SuspendReason, TaskSpec,
    SNAPSHOT_MARKER,
};

/// A boxed event stream for one supervised task.
pub type TaskStream = Pin<Box<dyn Stream<Item = AgentEvent> + Send>>;

/// Durable Agent engine with bounded automatic recovery around the original
/// tool-calling state machine.
#[derive(Clone)]
pub struct AgentEngine {
    inner: legacy::AgentEngine,
    storage: Storage,
    max_runtime_retries: u32,
}

impl AgentEngine {
    /// Build an engine over a chat backend, tool registry and storage.
    pub fn new(
        backend: Arc<dyn ChatBackend>,
        tools: ToolRegistry,
        ctx: ToolContext,
        config: EngineConfig,
    ) -> Self {
        let storage = ctx.storage.clone();
        Self {
            inner: legacy::AgentEngine::new(backend, tools, ctx, config),
            storage,
            max_runtime_retries: 3,
        }
    }

    /// Override the number of live transient-recovery attempts. This limit is
    /// per host process; every attempt still resumes from SQLite rather than
    /// reconstructing protocol state in memory.
    pub fn with_max_runtime_retries(mut self, max_runtime_retries: u32) -> Self {
        self.max_runtime_retries = max_runtime_retries;
        self
    }

    /// The tool registry this engine runs.
    pub fn tools(&self) -> &ToolRegistry {
        self.inner.tools()
    }

    /// Start a new durable task and supervise its event stream.
    pub fn run_task(&self, spec: TaskSpec) -> TaskStream {
        let task_id = spec.id.clone();
        let stream = self.inner.run_task(spec);
        self.supervise(task_id, stream)
    }

    /// Resume a suspended/interrupted task. A task that ended with a transient
    /// provider error is also made resumable; hard failures remain protected
    /// by the original status gate.
    pub async fn resume_task(&self, task_id: &str) -> Result<TaskStream> {
        if let Some(record) = self.storage.agent_task_get(task_id).await? {
            if record.status == "failed"
                && task_last_error(&record)
                    .as_deref()
                    .is_some_and(is_retryable_runtime_error)
            {
                self.prepare_runtime_retry(task_id).await?;
            }
        }
        let stream = self.inner.resume_task(task_id).await?;
        Ok(self.supervise(task_id.to_string(), stream))
    }

    /// List all persisted tasks, most recently updated first.
    pub async fn list_tasks(&self) -> Result<Vec<AgentTask>> {
        self.inner.list_tasks().await
    }

    /// Mark a task cancelled.
    pub async fn cancel_task(&self, task_id: &str) -> Result<bool> {
        self.inner.cancel_task(task_id).await
    }

    fn supervise(&self, task_id: String, initial: legacy::TaskStream) -> TaskStream {
        let (tx, rx) = mpsc::unbounded();
        let engine = self.clone();
        tokio::spawn(async move {
            let mut stream = initial;
            let mut attempt = 0_u32;

            loop {
                let mut retry_reason = None;
                while let Some(event) = stream.next().await {
                    match event {
                        AgentEvent::Failed { error } if is_retryable_runtime_error(&error) => {
                            retry_reason = Some(error);
                            break;
                        }
                        terminal @ (AgentEvent::Completed { .. }
                        | AgentEvent::Suspended { .. }
                        | AgentEvent::Failed { .. }) => {
                            let _ = tx.unbounded_send(terminal);
                            return;
                        }
                        event => {
                            if tx.unbounded_send(event).is_err() {
                                return;
                            }
                        }
                    }
                }

                let reason = retry_reason.unwrap_or_else(|| {
                    "Agent 执行流在没有终态事件的情况下意外结束".to_string()
                });
                if attempt >= engine.max_runtime_retries {
                    let error = format!(
                        "{reason}；已达到自动恢复上限 {} 次，任务已保留为可继续状态",
                        engine.max_runtime_retries
                    );
                    let _ = engine.mark_runtime_suspended(&task_id, &error).await;
                    let _ = tx.unbounded_send(AgentEvent::Failed { error });
                    return;
                }
                attempt += 1;

                let (round, max_rounds) = match engine.prepare_runtime_retry(&task_id).await {
                    Ok(meta) => meta,
                    Err(error) => {
                        let _ = tx.unbounded_send(AgentEvent::Failed {
                            error: format!("自动恢复检查点失败: {error}"),
                        });
                        return;
                    }
                };
                if tx
                    .unbounded_send(AgentEvent::TextReset {
                        message: format!(
                            "上游连接中断，已丢弃未完成草稿；正在从最近检查点自动恢复（{attempt}/{}）",
                            engine.max_runtime_retries
                        ),
                    })
                    .is_err()
                {
                    return;
                }
                let _ = tx.unbounded_send(AgentEvent::Progress {
                    phase: "recovering".to_string(),
                    message: format!(
                        "检测到可恢复的临时故障，{} 秒后继续：{}",
                        runtime_retry_delay(attempt).as_secs(),
                        compact_error(&reason)
                    ),
                    round,
                    max_rounds,
                    completed: Some(attempt as usize),
                    total: Some(engine.max_runtime_retries as usize),
                });
                tokio::time::sleep(runtime_retry_delay(attempt)).await;

                match engine.inner.resume_task(&task_id).await {
                    Ok(next) => stream = next,
                    Err(error) => {
                        let error = format!("自动恢复任务失败: {error}");
                        let _ = engine.mark_runtime_suspended(&task_id, &error).await;
                        let _ = tx.unbounded_send(AgentEvent::Failed { error });
                        return;
                    }
                }
            }
        });
        Box::pin(rx)
    }

    async fn prepare_runtime_retry(&self, task_id: &str) -> Result<(u32, u32)> {
        let mut record = self
            .storage
            .agent_task_get(task_id)
            .await?
            .ok_or_else(|| AgentError::TaskNotFound(task_id.to_string()))?;
        if record.status == "cancelled" {
            return Err(AgentError::Cancelled(task_id.to_string()));
        }

        let mut state: Value = serde_json::from_str(&record.state_json)?;
        let completed_rounds = state
            .pointer("/round")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let max_rounds = state
            .pointer("/spec/max_rounds")
            .and_then(Value::as_u64)
            .unwrap_or(32) as u32;
        if let Some(object) = state.as_object_mut() {
            object.insert("last_error".to_string(), Value::Null);
        }
        record.status = "running".to_string();
        record.state_json = serde_json::to_string(&state)?;
        record.updated_at = now_secs();
        self.storage.agent_task_save(record).await?;
        Ok((completed_rounds.saturating_add(1), max_rounds))
    }

    async fn mark_runtime_suspended(&self, task_id: &str, error: &str) -> Result<()> {
        let Some(mut record) = self.storage.agent_task_get(task_id).await? else {
            return Ok(());
        };
        let mut state: Value = serde_json::from_str(&record.state_json)?;
        if let Some(object) = state.as_object_mut() {
            object.insert("last_error".to_string(), Value::String(error.to_string()));
        }
        record.status = "suspended".to_string();
        record.state_json = serde_json::to_string(&state)?;
        record.updated_at = now_secs();
        self.storage.agent_task_save(record).await?;
        Ok(())
    }
}

fn task_last_error(task: &AgentTask) -> Option<String> {
    let state: Value = serde_json::from_str(&task.state_json).ok()?;
    state
        .get("last_error")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|error| !error.is_empty())
        .map(str::to_string)
}

/// Conservative terminal-error classification. Only transport/rate-limit and
/// unexpected-stream failures are retried; authentication, protocol, storage,
/// validation, cancellation and round-limit failures are never looped.
pub fn is_retryable_runtime_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    let hard = [
        "authentication",
        "invalid api key",
        "api error",
        "parse error",
        "storage",
        "工具调用历史",
        "超过最大轮数",
        "证据校验",
        "任务已取消",
        "task cancelled",
        "not resumable",
    ];
    if hard.iter().any(|needle| lower.contains(needle)) {
        return false;
    }

    [
        "network error",
        "rate limited",
        "timeout",
        "timed out",
        "connection",
        "unexpected eof",
        "stream",
        "sse",
        "temporarily unavailable",
        "service unavailable",
        "网络",
        "连接",
        "超时",
        "断流",
        "空闲看门狗",
        "终止标记前关闭",
        "执行流在没有终态",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn runtime_retry_delay(attempt: u32) -> Duration {
    Duration::from_secs(1_u64 << attempt.saturating_sub(1).min(4))
}

fn compact_error(error: &str) -> String {
    let mut compact: String = error.chars().take(160).collect();
    if error.chars().count() > 160 {
        compact.push('…');
    }
    compact
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::testing::{NoopMarket, ScriptedChat, ScriptedReply};
    use astock_minimax::MinimaxError;

    #[test]
    fn only_transient_runtime_failures_are_retried() {
        assert!(is_retryable_runtime_error(
            "network error: connection reset by peer"
        ));
        assert!(is_retryable_runtime_error("MiniMax 流连续 120 秒没有数据"));
        assert!(!is_retryable_runtime_error(
            "authentication failed: invalid api key"
        ));
        assert!(!is_retryable_runtime_error("storage: database is locked"));
        assert!(!is_retryable_runtime_error("超过最大轮数 48"));
    }

    #[test]
    fn retry_delay_is_bounded() {
        assert_eq!(runtime_retry_delay(1), Duration::from_secs(1));
        assert_eq!(runtime_retry_delay(3), Duration::from_secs(4));
        assert_eq!(runtime_retry_delay(100), Duration::from_secs(16));
    }

    #[tokio::test(start_paused = true)]
    async fn transient_model_failure_recovers_from_persisted_round() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(astock_storage::StorageConfig::with_base_dir(dir.path()))
            .unwrap();
        let backend = Arc::new(ScriptedChat::new("test-model"));
        backend
            .push(ScriptedReply::Error(MinimaxError::Network(
                "connection reset".to_string(),
            )))
            .push_quota_exhausted();
        let ctx = ToolContext::new(Arc::new(NoopMarket), storage);
        let engine = AgentEngine::new(
            backend.clone(),
            ToolRegistry::default(),
            ctx,
            EngineConfig::default(),
        );

        let events: Vec<_> = engine
            .run_task(TaskSpec::new("retry-task", "chat", "给出简短总结"))
            .collect()
            .await;
        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::TextReset { .. })));
        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::Suspended { .. })));
        assert_eq!(backend.requests.lock().unwrap().len(), 2);
    }
}
