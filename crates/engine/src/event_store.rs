use astock_storage::Storage;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTask {
    pub task_id: String,
    pub reducer_version: String,
    pub task_spec: Value,
    pub phase: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppendEvent {
    pub task_id: String,
    pub seq: i64,
    pub event_id: String,
    pub event_kind: String,
    pub event: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutCheckpoint {
    pub task_id: String,
    pub accepted_seq: i64,
    pub phase: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DurableTask {
    pub task_id: String,
    pub reducer_version: String,
    pub task_spec: Value,
    pub phase: String,
    pub accepted_seq: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DurableEvent {
    pub seq: i64,
    pub event_id: String,
    pub event_kind: String,
    pub event: Value,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadedTask {
    pub task: DurableTask,
    pub events: Vec<DurableEvent>,
}

pub async fn migrate(storage: &Storage) -> Result<(), astock_storage::Error> {
    storage
        .run(|connection| {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS agent_tasks_v2 (
                   task_id TEXT PRIMARY KEY,
                   reducer_version TEXT NOT NULL,
                   task_spec_json TEXT NOT NULL,
                   phase TEXT NOT NULL,
                   accepted_seq INTEGER NOT NULL DEFAULT 0,
                   created_at INTEGER NOT NULL,
                   updated_at INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS agent_events_v2 (
                   task_id TEXT NOT NULL,
                   seq INTEGER NOT NULL,
                   event_id TEXT NOT NULL,
                   event_kind TEXT NOT NULL,
                   event_json TEXT NOT NULL,
                   created_at INTEGER NOT NULL,
                   PRIMARY KEY(task_id, seq),
                   UNIQUE(event_id),
                   FOREIGN KEY(task_id) REFERENCES agent_tasks_v2(task_id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS agent_effects_v2 (
                   effect_id TEXT PRIMARY KEY,
                   task_id TEXT NOT NULL,
                   caused_by_seq INTEGER NOT NULL,
                   effect_kind TEXT NOT NULL,
                   effect_json TEXT NOT NULL,
                   status TEXT NOT NULL,
                   result_json TEXT,
                   idempotency_key TEXT NOT NULL UNIQUE,
                   created_at INTEGER NOT NULL,
                   updated_at INTEGER NOT NULL,
                   FOREIGN KEY(task_id) REFERENCES agent_tasks_v2(task_id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_agent_events_v2_task
                   ON agent_events_v2(task_id, seq);
                 CREATE INDEX IF NOT EXISTS idx_agent_effects_v2_task
                   ON agent_effects_v2(task_id, caused_by_seq);
                 COMMIT;",
            )?;
            Ok(())
        })
        .await
}

pub async fn create_task(storage: &Storage, input: CreateTask) -> Result<bool, String> {
    validate_identity(&input.task_id, "task_id")?;
    validate_identity(&input.reducer_version, "reducer_version")?;
    let spec = serde_json::to_string(&input.task_spec).map_err(|error| error.to_string())?;
    let now = now_secs();
    storage
        .run(move |connection| {
            let inserted = connection.execute(
                "INSERT OR IGNORE INTO agent_tasks_v2
                 (task_id,reducer_version,task_spec_json,phase,accepted_seq,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,0,?5,?5)",
                params![input.task_id, input.reducer_version, spec, input.phase, now],
            )?;
            Ok(inserted == 1)
        })
        .await
        .map_err(|error| error.to_string())
}

pub async fn append_event(storage: &Storage, input: AppendEvent) -> Result<bool, String> {
    validate_identity(&input.task_id, "task_id")?;
    validate_identity(&input.event_id, "event_id")?;
    if input.seq <= 0 {
        return Err("seq must be positive".into());
    }
    let event_json = serde_json::to_string(&input.event).map_err(|error| error.to_string())?;
    let now = now_secs();
    storage
        .run(move |connection| {
            let transaction = connection.transaction()?;
            let current: Option<i64> = transaction
                .query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM agent_events_v2 WHERE task_id=?1",
                    params![input.task_id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            let current = current.unwrap_or(0);
            if input.seq <= current {
                let existing: Option<(String, String)> = transaction
                    .query_row(
                        "SELECT event_id,event_json FROM agent_events_v2 WHERE task_id=?1 AND seq=?2",
                        params![input.task_id, input.seq],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                return match existing {
                    Some((event_id, body)) if event_id == input.event_id && body == event_json => {
                        transaction.commit()?;
                        Ok(false)
                    }
                    _ => Err(astock_storage::Error::Invalid(format!(
                        "sequence_conflict: current={current}, received={}",
                        input.seq
                    ))),
                };
            }
            if input.seq != current + 1 {
                return Err(astock_storage::Error::Invalid(format!(
                    "sequence_gap: expected={}, received={}",
                    current + 1,
                    input.seq
                )));
            }
            transaction.execute(
                "INSERT INTO agent_events_v2
                 (task_id,seq,event_id,event_kind,event_json,created_at)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    input.task_id,
                    input.seq,
                    input.event_id,
                    input.event_kind,
                    event_json,
                    now
                ],
            )?;
            transaction.commit()?;
            Ok(true)
        })
        .await
        .map_err(|error| error.to_string())
}

pub async fn put_checkpoint(storage: &Storage, input: PutCheckpoint) -> Result<(), String> {
    let now = now_secs();
    storage
        .run(move |connection| {
            let transaction = connection.transaction()?;
            let state: Option<(i64, i64)> = transaction
                .query_row(
                    "SELECT accepted_seq,
                       COALESCE((SELECT MAX(seq) FROM agent_events_v2 WHERE task_id=?1),0)
                     FROM agent_tasks_v2 WHERE task_id=?1",
                    params![input.task_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((current, durable_max)) = state else {
                return Err(astock_storage::Error::Invalid("task_not_found".into()));
            };
            if input.accepted_seq < current || input.accepted_seq > durable_max {
                return Err(astock_storage::Error::Invalid(format!(
                    "checkpoint_sequence_invalid: current={current}, durable_max={durable_max}, received={}",
                    input.accepted_seq
                )));
            }
            transaction.execute(
                "UPDATE agent_tasks_v2 SET phase=?2,accepted_seq=?3,updated_at=?4 WHERE task_id=?1",
                params![input.task_id, input.phase, input.accepted_seq, now],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())
}

pub async fn load_task(storage: &Storage, task_id: String) -> Result<LoadedTask, String> {
    storage
        .run(move |connection| {
            let task = connection
                .query_row(
                    "SELECT task_id,reducer_version,task_spec_json,phase,accepted_seq,created_at,updated_at
                     FROM agent_tasks_v2 WHERE task_id=?1",
                    params![task_id],
                    |row| {
                        let spec: String = row.get(2)?;
                        Ok(DurableTask {
                            task_id: row.get(0)?,
                            reducer_version: row.get(1)?,
                            task_spec: serde_json::from_str(&spec).unwrap_or(Value::Null),
                            phase: row.get(3)?,
                            accepted_seq: row.get(4)?,
                            created_at: row.get(5)?,
                            updated_at: row.get(6)?,
                        })
                    },
                )
                .optional()?;
            let Some(task) = task else {
                return Err(astock_storage::Error::Invalid("task_not_found".into()));
            };
            let mut statement = connection.prepare(
                "SELECT seq,event_id,event_kind,event_json,created_at FROM agent_events_v2
                 WHERE task_id=?1 ORDER BY seq ASC",
            )?;
            let events = statement
                .query_map(params![task.task_id], |row| {
                    let body: String = row.get(3)?;
                    Ok(DurableEvent {
                        seq: row.get(0)?,
                        event_id: row.get(1)?,
                        event_kind: row.get(2)?,
                        event: serde_json::from_str(&body).unwrap_or(Value::Null),
                        created_at: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(LoadedTask { task, events })
        })
        .await
        .map_err(|error| error.to_string())
}

pub async fn list_tasks(storage: &Storage, limit: usize) -> Result<Vec<DurableTask>, String> {
    let limit = limit.clamp(1, astock_protocol::MAX_PAGE_SIZE) as i64;
    storage
        .run(move |connection| {
            let mut statement = connection.prepare(
                "SELECT task_id,reducer_version,task_spec_json,phase,accepted_seq,created_at,updated_at
                 FROM agent_tasks_v2 ORDER BY updated_at DESC LIMIT ?1",
            )?;
            let tasks = statement
                .query_map(params![limit], |row| {
                    let spec: String = row.get(2)?;
                    Ok(DurableTask {
                        task_id: row.get(0)?,
                        reducer_version: row.get(1)?,
                        task_spec: serde_json::from_str(&spec).unwrap_or(Value::Null),
                        phase: row.get(3)?,
                        accepted_seq: row.get(4)?,
                        created_at: row.get(5)?,
                        updated_at: row.get(6)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(tasks)
        })
        .await
        .map_err(|error| error.to_string())
}

fn validate_identity(value: &str, name: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 128 {
        Err(format!("{name} must contain 1..128 bytes"))
    } else {
        Ok(())
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_storage::StorageConfig;

    #[tokio::test]
    async fn migration_is_idempotent_and_enforces_event_identity() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        migrate(&storage).await.unwrap();
        migrate(&storage).await.unwrap();
        let tables = storage
            .run(|connection| {
                let mut statement = connection.prepare(
                    "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'agent_%_v2' ORDER BY name",
                )?;
                let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap();
        assert_eq!(tables.len(), 3);
    }

    #[tokio::test]
    async fn event_log_rejects_gaps_and_replays_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        migrate(&storage).await.unwrap();
        assert!(create_task(
            &storage,
            CreateTask {
                task_id: "task-1".into(),
                reducer_version: "kernel-v1".into(),
                task_spec: serde_json::json!({"objective":"verify"}),
                phase: "preparing".into(),
            },
        )
        .await
        .unwrap());

        let event = AppendEvent {
            task_id: "task-1".into(),
            seq: 1,
            event_id: "event-1".into(),
            event_kind: "start".into(),
            event: serde_json::json!({"kind":"start"}),
        };
        assert!(append_event(&storage, event.clone()).await.unwrap());
        assert!(!append_event(&storage, event).await.unwrap());
        let gap = append_event(
            &storage,
            AppendEvent {
                task_id: "task-1".into(),
                seq: 3,
                event_id: "event-3".into(),
                event_kind: "prepared".into(),
                event: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err();
        assert!(gap.contains("sequence_gap"));

        put_checkpoint(
            &storage,
            PutCheckpoint {
                task_id: "task-1".into(),
                accepted_seq: 1,
                phase: "reasoning".into(),
            },
        )
        .await
        .unwrap();
        let loaded = load_task(&storage, "task-1".into()).await.unwrap();
        assert_eq!(loaded.task.accepted_seq, 1);
        assert_eq!(loaded.events.len(), 1);
        assert_eq!(loaded.events[0].event_id, "event-1");
    }
}
