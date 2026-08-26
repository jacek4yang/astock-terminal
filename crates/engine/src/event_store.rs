use astock_storage::Storage;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
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
    pub checkpoint: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginEffect {
    pub effect_id: String,
    pub task_id: String,
    pub caused_by_seq: i64,
    pub effect_kind: String,
    pub effect: Value,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteEffect {
    pub effect_id: String,
    pub status: String,
    pub result: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct DurableTask {
    pub task_id: String,
    pub reducer_version: String,
    pub task_spec: Value,
    pub phase: String,
    pub accepted_seq: i64,
    pub checkpoint: Option<Value>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DurableEffect {
    pub effect_id: String,
    pub task_id: String,
    pub caused_by_seq: i64,
    pub effect_kind: String,
    pub effect: Value,
    pub status: String,
    pub result: Option<Value>,
    pub idempotency_key: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectPage {
    pub items: Vec<DurableEffect>,
    pub next_after_caused_by_seq: Option<i64>,
    pub next_after_effect_id: Option<String>,
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
    pub events_truncated: bool,
    pub next_before_seq: Option<i64>,
}

const MAX_CONVERSATION_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveConversation {
    pub conversation_id: String,
    pub title: String,
    pub session: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationId {
    pub conversation_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationList {
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenameConversation {
    pub conversation_id: String,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchConversation {
    pub source_conversation_id: String,
    pub new_conversation_id: String,
    pub message_id: String,
    pub title: String,
    #[serde(default)]
    pub checkpoint_task_id: Option<String>,
    #[serde(default)]
    pub checkpoint_accepted_seq: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoredConversation {
    pub conversation_id: String,
    pub title: String,
    pub session: Value,
    pub parent_conversation_id: Option<String>,
    pub branch_from_message_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationSummary {
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
                   checkpoint_json TEXT,
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
                 CREATE TABLE IF NOT EXISTS agent_conversations_v2 (
                   conversation_id TEXT PRIMARY KEY,
                   title TEXT NOT NULL,
                   session_json TEXT NOT NULL,
                   parent_conversation_id TEXT,
                   branch_from_message_id TEXT,
                   deleted_at INTEGER,
                   created_at INTEGER NOT NULL,
                   updated_at INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_agent_events_v2_task
                   ON agent_events_v2(task_id, seq);
                 CREATE INDEX IF NOT EXISTS idx_agent_effects_v2_task
                   ON agent_effects_v2(task_id, caused_by_seq);
                 CREATE INDEX IF NOT EXISTS idx_agent_conversations_v2_updated
                   ON agent_conversations_v2(deleted_at, updated_at DESC);
                 COMMIT;",
            )?;
            let has_checkpoint = {
                let mut statement = connection.prepare("PRAGMA table_info(agent_tasks_v2)")?;
                let columns = statement
                    .query_map([], |row| row.get::<_, String>(1))?
                    .collect::<Result<Vec<_>, _>>()?;
                columns.iter().any(|column| column == "checkpoint_json")
            };
            if !has_checkpoint {
                connection.execute(
                    "ALTER TABLE agent_tasks_v2 ADD COLUMN checkpoint_json TEXT",
                    [],
                )?;
            }
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
    let checkpoint_json = serde_json::to_string(&input.checkpoint)
        .map_err(|error| format!("checkpoint_encode_failed:{error}"))?;
    if checkpoint_json.len() > astock_protocol::MAX_FRAME_BYTES {
        return Err("checkpoint_too_large".into());
    }
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
                "UPDATE agent_tasks_v2
                 SET phase=?2,accepted_seq=?3,checkpoint_json=?4,updated_at=?5
                 WHERE task_id=?1",
                params![
                    input.task_id,
                    input.phase,
                    input.accepted_seq,
                    checkpoint_json,
                    now
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())
}

pub async fn begin_effect(storage: &Storage, input: BeginEffect) -> Result<bool, String> {
    validate_identity(&input.effect_id, "effect_id")?;
    validate_identity(&input.task_id, "task_id")?;
    validate_identity(&input.effect_kind, "effect_kind")?;
    let idempotency_key = normalize_idempotency_key(&input.idempotency_key)?;
    if input.caused_by_seq < 0 {
        return Err("caused_by_seq must not be negative".into());
    }
    let effect_json = serde_json::to_string(&input.effect).map_err(|error| error.to_string())?;
    if effect_json.len() > astock_protocol::MAX_FRAME_BYTES {
        return Err("effect_too_large".into());
    }
    let now = now_secs();
    storage
        .run(move |connection| {
            let transaction = connection.transaction()?;
            let existing: Option<(String, String, String, String)> = transaction
                .query_row(
                    "SELECT effect_id,task_id,effect_kind,effect_json
                     FROM agent_effects_v2
                     WHERE effect_id=?1 OR idempotency_key=?2",
                    params![input.effect_id, idempotency_key],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            if let Some((effect_id, task_id, effect_kind, stored_json)) = existing {
                if effect_id == input.effect_id
                    && task_id == input.task_id
                    && effect_kind == input.effect_kind
                    && stored_json == effect_json
                {
                    transaction.commit()?;
                    return Ok(false);
                }
                return Err(astock_storage::Error::Invalid(
                    "effect_identity_conflict".into(),
                ));
            }
            transaction.execute(
                "INSERT INTO agent_effects_v2
                 (effect_id,task_id,caused_by_seq,effect_kind,effect_json,status,
                  result_json,idempotency_key,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,'pending',NULL,?6,?7,?7)",
                params![
                    input.effect_id,
                    input.task_id,
                    input.caused_by_seq,
                    input.effect_kind,
                    effect_json,
                    idempotency_key,
                    now
                ],
            )?;
            transaction.commit()?;
            Ok(true)
        })
        .await
        .map_err(|error| error.to_string())
}

pub async fn complete_effect(storage: &Storage, input: CompleteEffect) -> Result<(), String> {
    validate_identity(&input.effect_id, "effect_id")?;
    if !matches!(input.status.as_str(), "succeeded" | "failed" | "cancelled") {
        return Err("invalid_effect_terminal_status".into());
    }
    let result_json = serde_json::to_string(&input.result).map_err(|error| error.to_string())?;
    if result_json.len() > astock_protocol::MAX_FRAME_BYTES {
        return Err("effect_result_too_large".into());
    }
    let now = now_secs();
    storage
        .run(move |connection| {
            let transaction = connection.transaction()?;
            let existing: Option<(String, Option<String>)> = transaction
                .query_row(
                    "SELECT status,result_json FROM agent_effects_v2 WHERE effect_id=?1",
                    params![input.effect_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((status, stored_result)) = existing else {
                return Err(astock_storage::Error::Invalid("effect_not_found".into()));
            };
            if status != "pending" {
                if status == input.status && stored_result.as_deref() == Some(&result_json) {
                    transaction.commit()?;
                    return Ok(());
                }
                return Err(astock_storage::Error::Invalid(
                    "effect_terminal_conflict".into(),
                ));
            }
            transaction.execute(
                "UPDATE agent_effects_v2 SET status=?2,result_json=?3,updated_at=?4
                 WHERE effect_id=?1",
                params![input.effect_id, input.status, result_json, now],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())
}

pub async fn list_effects(
    storage: &Storage,
    task_id: String,
    limit: usize,
    after: Option<(i64, String)>,
) -> Result<EffectPage, String> {
    validate_identity(&task_id, "task_id")?;
    let limit = limit.clamp(1, astock_protocol::MAX_PAGE_SIZE);
    let (after_seq, after_effect_id) = after.unwrap_or((-1, String::new()));
    if after_seq < -1 {
        return Err("after_caused_by_seq must be at least -1".into());
    }
    if !after_effect_id.is_empty() {
        validate_identity(&after_effect_id, "after_effect_id")?;
    }
    storage
        .run(move |connection| {
            let mut statement = connection.prepare(
                "SELECT effect_id,task_id,caused_by_seq,effect_kind,effect_json,status,
                        result_json,idempotency_key,created_at,updated_at
                 FROM agent_effects_v2
                 WHERE task_id=?1 AND
                       (caused_by_seq>?2 OR (caused_by_seq=?2 AND effect_id>?3))
                 ORDER BY caused_by_seq ASC,effect_id ASC LIMIT ?4",
            )?;
            let mut items = statement
                .query_map(
                    params![task_id, after_seq, after_effect_id, limit as i64 + 1],
                    |row| {
                        let effect_json: String = row.get(4)?;
                        let result_json: Option<String> = row.get(6)?;
                        Ok(DurableEffect {
                            effect_id: row.get(0)?,
                            task_id: row.get(1)?,
                            caused_by_seq: row.get(2)?,
                            effect_kind: row.get(3)?,
                            effect: serde_json::from_str(&effect_json).unwrap_or(Value::Null),
                            status: row.get(5)?,
                            result: result_json
                                .as_deref()
                                .map(serde_json::from_str)
                                .transpose()
                                .unwrap_or(None),
                            idempotency_key: row.get(7)?,
                            created_at: row.get(8)?,
                            updated_at: row.get(9)?,
                        })
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            let has_more = items.len() > limit;
            if has_more {
                items.truncate(limit);
            }
            let (next_after_caused_by_seq, next_after_effect_id) = if has_more {
                let last = items.last().expect("a non-empty bounded effect page");
                (Some(last.caused_by_seq), Some(last.effect_id.clone()))
            } else {
                (None, None)
            };
            Ok(EffectPage {
                items,
                next_after_caused_by_seq,
                next_after_effect_id,
            })
        })
        .await
        .map_err(|error| error.to_string())
}

pub async fn load_task(
    storage: &Storage,
    task_id: String,
    event_limit: usize,
    event_before_seq: Option<i64>,
) -> Result<LoadedTask, String> {
    validate_identity(&task_id, "task_id")?;
    let event_limit = event_limit.clamp(1, astock_protocol::MAX_PAGE_SIZE);
    let event_before_seq = event_before_seq.unwrap_or(i64::MAX);
    storage
        .run(move |connection| {
            let task = connection
                .query_row(
                    "SELECT task_id,reducer_version,task_spec_json,phase,accepted_seq,
                            checkpoint_json,created_at,updated_at
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
                            checkpoint: row
                                .get::<_, Option<String>>(5)?
                                .and_then(|value| serde_json::from_str(&value).ok()),
                            created_at: row.get(6)?,
                            updated_at: row.get(7)?,
                        })
                    },
                )
                .optional()?;
            let Some(task) = task else {
                return Err(astock_storage::Error::Invalid("task_not_found".into()));
            };
            let mut statement = connection.prepare(
                "SELECT seq,event_id,event_kind,event_json,created_at FROM agent_events_v2
                 WHERE task_id=?1 AND seq<?2 ORDER BY seq DESC LIMIT ?3",
            )?;
            let mut events = statement
                .query_map(
                    params![task.task_id, event_before_seq, event_limit as i64 + 1],
                    |row| {
                        let body: String = row.get(3)?;
                        Ok(DurableEvent {
                            seq: row.get(0)?,
                            event_id: row.get(1)?,
                            event_kind: row.get(2)?,
                            event: serde_json::from_str(&body).unwrap_or(Value::Null),
                            created_at: row.get(4)?,
                        })
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            let events_truncated = events.len() > event_limit;
            if events_truncated {
                events.truncate(event_limit);
            }
            let next_before_seq = events_truncated
                .then(|| events.last().map(|event| event.seq))
                .flatten();
            events.reverse();
            Ok(LoadedTask {
                task,
                events,
                events_truncated,
                next_before_seq,
            })
        })
        .await
        .map_err(|error| error.to_string())
}

pub async fn list_tasks(storage: &Storage, limit: usize) -> Result<Vec<DurableTask>, String> {
    let limit = limit.clamp(1, astock_protocol::MAX_PAGE_SIZE) as i64;
    storage
        .run(move |connection| {
            let mut statement = connection.prepare(
                "SELECT task_id,reducer_version,task_spec_json,phase,accepted_seq,
                        checkpoint_json,created_at,updated_at
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
                        checkpoint: row
                            .get::<_, Option<String>>(5)?
                            .and_then(|value| serde_json::from_str(&value).ok()),
                        created_at: row.get(6)?,
                        updated_at: row.get(7)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(tasks)
        })
        .await
        .map_err(|error| error.to_string())
}

pub async fn save_conversation(
    storage: &Storage,
    input: SaveConversation,
) -> Result<StoredConversation, String> {
    validate_identity(&input.conversation_id, "conversation_id")?;
    let title = validate_title(input.title)?;
    let session_json = encode_session(&input.session)?;
    let now = now_secs();
    let conversation_id = input.conversation_id;
    storage
        .run(move |connection| {
            connection.execute(
                "INSERT INTO agent_conversations_v2
                 (conversation_id,title,session_json,parent_conversation_id,branch_from_message_id,deleted_at,created_at,updated_at)
                 VALUES (?1,?2,?3,NULL,NULL,NULL,?4,?4)
                 ON CONFLICT(conversation_id) DO UPDATE SET
                   title=excluded.title,
                   session_json=excluded.session_json,
                   deleted_at=NULL,
                   updated_at=excluded.updated_at",
                params![conversation_id, title, session_json, now],
            )?;
            read_conversation(connection, &conversation_id)
        })
        .await
        .map_err(|error| error.to_string())
}

pub async fn load_conversation(
    storage: &Storage,
    conversation_id: String,
) -> Result<StoredConversation, String> {
    validate_identity(&conversation_id, "conversation_id")?;
    storage
        .run(move |connection| read_conversation(connection, &conversation_id))
        .await
        .map_err(|error| error.to_string())
}

pub async fn list_conversations(
    storage: &Storage,
    limit: usize,
    query: Option<String>,
) -> Result<Vec<ConversationSummary>, String> {
    let limit = limit.clamp(1, astock_protocol::MAX_PAGE_SIZE) as i64;
    let pattern = conversation_search_pattern(query)?;
    storage
        .run(move |connection| {
            let (sql, parameters): (&str, Vec<rusqlite::types::Value>) = match pattern {
                Some(pattern) => (
                    "SELECT conversation_id,title,session_json,parent_conversation_id,
                            branch_from_message_id,created_at,updated_at
                     FROM agent_conversations_v2
                     WHERE deleted_at IS NULL AND
                           (title LIKE ?1 ESCAPE '\\' COLLATE NOCASE OR
                            conversation_id LIKE ?1 ESCAPE '\\' COLLATE NOCASE)
                     ORDER BY updated_at DESC LIMIT ?2",
                    vec![pattern.into(), limit.into()],
                ),
                None => (
                    "SELECT conversation_id,title,session_json,parent_conversation_id,
                            branch_from_message_id,created_at,updated_at
                     FROM agent_conversations_v2
                     WHERE deleted_at IS NULL ORDER BY updated_at DESC LIMIT ?1",
                    vec![limit.into()],
                ),
            };
            let mut statement = connection.prepare(sql)?;
            let rows = statement
                .query_map(rusqlite::params_from_iter(parameters), |row| {
                    let session_json: String = row.get(2)?;
                    let session =
                        serde_json::from_str::<Value>(&session_json).unwrap_or(Value::Null);
                    Ok(conversation_summary(
                        row.get(0)?,
                        row.get(1)?,
                        session,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .map_err(|error| error.to_string())
}

pub async fn rename_conversation(
    storage: &Storage,
    input: RenameConversation,
) -> Result<StoredConversation, String> {
    validate_identity(&input.conversation_id, "conversation_id")?;
    let title = validate_title(input.title)?;
    let now = now_secs();
    storage
        .run(move |connection| {
            let changed = connection.execute(
                "UPDATE agent_conversations_v2 SET title=?2,updated_at=?3
                 WHERE conversation_id=?1 AND deleted_at IS NULL",
                params![input.conversation_id, title, now],
            )?;
            if changed != 1 {
                return Err(astock_storage::Error::Invalid(
                    "conversation_not_found".into(),
                ));
            }
            read_conversation(connection, &input.conversation_id)
        })
        .await
        .map_err(|error| error.to_string())
}

pub async fn delete_conversation(
    storage: &Storage,
    conversation_id: String,
) -> Result<bool, String> {
    validate_identity(&conversation_id, "conversation_id")?;
    let now = now_secs();
    storage
        .run(move |connection| {
            let changed = connection.execute(
                "UPDATE agent_conversations_v2 SET deleted_at=?2,updated_at=?2
                 WHERE conversation_id=?1 AND deleted_at IS NULL",
                params![conversation_id, now],
            )?;
            Ok(changed == 1)
        })
        .await
        .map_err(|error| error.to_string())
}

pub async fn branch_conversation(
    storage: &Storage,
    input: BranchConversation,
) -> Result<StoredConversation, String> {
    validate_identity(&input.source_conversation_id, "source_conversation_id")?;
    validate_identity(&input.new_conversation_id, "new_conversation_id")?;
    validate_identity(&input.message_id, "message_id")?;
    let title = validate_title(input.title)?;
    let now = now_secs();
    storage
        .run(move |connection| {
            let transaction = connection.transaction()?;
            let source_json: Option<String> = transaction
                .query_row(
                    "SELECT session_json FROM agent_conversations_v2
                     WHERE conversation_id=?1 AND deleted_at IS NULL",
                    params![input.source_conversation_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(source_json) = source_json else {
                return Err(astock_storage::Error::Invalid("conversation_not_found".into()));
            };
            let mut session = serde_json::from_str::<Value>(&source_json)
                .map_err(|error| astock_storage::Error::Invalid(error.to_string()))?;
            let checkpoint_origin = match (
                input.checkpoint_task_id.as_deref(),
                input.checkpoint_accepted_seq,
            ) {
                (None, None) => None,
                (Some(task_id), Some(accepted_seq)) => {
                    validate_identity(task_id, "checkpoint_task_id")
                        .map_err(astock_storage::Error::Invalid)?;
                    if accepted_seq < 1 {
                        return Err(astock_storage::Error::Invalid(
                            "checkpoint_accepted_seq_invalid".into(),
                        ));
                    }
                    if session["task"]["task_id"].as_str() != Some(task_id)
                        || session["task"]["accepted_seq"].as_i64() != Some(accepted_seq)
                    {
                        return Err(astock_storage::Error::Invalid(
                            "conversation_checkpoint_mismatch".into(),
                        ));
                    }
                    let durable: Option<(i64, Option<String>)> = transaction
                        .query_row(
                            "SELECT accepted_seq,checkpoint_json FROM agent_tasks_v2 WHERE task_id=?1",
                            params![task_id],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .optional()?;
                    let Some((durable_seq, Some(checkpoint_json))) = durable else {
                        return Err(astock_storage::Error::Invalid(
                            "durable_checkpoint_not_found".into(),
                        ));
                    };
                    let checkpoint: Value = serde_json::from_str(&checkpoint_json)
                        .map_err(|error| astock_storage::Error::Invalid(error.to_string()))?;
                    if durable_seq != accepted_seq
                        || checkpoint["task_id"].as_str() != Some(task_id)
                        || checkpoint["accepted_seq"].as_i64() != Some(accepted_seq)
                    {
                        return Err(astock_storage::Error::Invalid(
                            "durable_checkpoint_mismatch".into(),
                        ));
                    }
                    Some(serde_json::json!({
                        "kind": "durable_checkpoint",
                        "task_id": task_id,
                        "accepted_seq": accepted_seq,
                        "phase": checkpoint["phase"],
                    }))
                }
                _ => {
                    return Err(astock_storage::Error::Invalid(
                        "checkpoint_origin_incomplete".into(),
                    ));
                }
            };
            trim_session_for_branch(
                &mut session,
                &input.message_id,
                &input.new_conversation_id,
                &title,
                now,
            )?;
            if let Some(origin) = checkpoint_origin {
                session["branchOrigin"] = origin;
            }
            let session_json = encode_session(&session)
                .map_err(astock_storage::Error::Invalid)?;
            transaction.execute(
                "INSERT INTO agent_conversations_v2
                 (conversation_id,title,session_json,parent_conversation_id,branch_from_message_id,deleted_at,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,NULL,?6,?6)",
                params![
                    input.new_conversation_id,
                    title,
                    session_json,
                    input.source_conversation_id,
                    input.message_id,
                    now
                ],
            )?;
            transaction.commit()?;
            read_conversation(connection, &input.new_conversation_id)
        })
        .await
        .map_err(|error| error.to_string())
}

fn read_conversation(
    connection: &rusqlite::Connection,
    conversation_id: &str,
) -> Result<StoredConversation, astock_storage::Error> {
    let result = connection
        .query_row(
            "SELECT conversation_id,title,session_json,parent_conversation_id,
                    branch_from_message_id,created_at,updated_at
             FROM agent_conversations_v2 WHERE conversation_id=?1 AND deleted_at IS NULL",
            params![conversation_id],
            |row| {
                let session_json: String = row.get(2)?;
                Ok(StoredConversation {
                    conversation_id: row.get(0)?,
                    title: row.get(1)?,
                    session: serde_json::from_str(&session_json).unwrap_or(Value::Null),
                    parent_conversation_id: row.get(3)?,
                    branch_from_message_id: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            },
        )
        .optional()?;
    result.ok_or_else(|| astock_storage::Error::Invalid("conversation_not_found".into()))
}

fn conversation_summary(
    conversation_id: String,
    title: String,
    session: Value,
    parent_conversation_id: Option<String>,
    branch_from_message_id: Option<String>,
    created_at: i64,
    updated_at: i64,
) -> ConversationSummary {
    let message_count = session["messages"].as_array().map_or(0, Vec::len);
    let evidence_count = session["task"]["evidence_ids"]
        .as_array()
        .map_or(0, Vec::len);
    let phase = session["task"]["phase"]
        .as_str()
        .unwrap_or("idle")
        .to_string();
    let branch_from_checkpoint_task_id = session["branchOrigin"]["task_id"]
        .as_str()
        .map(ToOwned::to_owned);
    let branch_from_checkpoint_seq = session["branchOrigin"]["accepted_seq"].as_i64();
    ConversationSummary {
        conversation_id,
        title,
        phase,
        message_count,
        evidence_count,
        parent_conversation_id,
        branch_from_message_id,
        branch_from_checkpoint_task_id,
        branch_from_checkpoint_seq,
        created_at,
        updated_at,
    }
}

fn trim_session_for_branch(
    session: &mut Value,
    message_id: &str,
    new_conversation_id: &str,
    title: &str,
    now: i64,
) -> Result<(), astock_storage::Error> {
    let messages = session["messages"]
        .as_array_mut()
        .ok_or_else(|| astock_storage::Error::Invalid("conversation_messages_missing".into()))?;
    let index = messages
        .iter()
        .position(|message| message["id"].as_str() == Some(message_id))
        .ok_or_else(|| astock_storage::Error::Invalid("branch_message_not_found".into()))?;
    messages.truncate(index + 1);
    session["sessionId"] = Value::String(new_conversation_id.to_string());
    session["title"] = Value::String(title.to_string());
    session["createdAt"] = Value::from(now * 1000);
    session["updatedAt"] = Value::from(now * 1000);
    session["input"] = Value::String(String::new());
    session["task"] = Value::Null;
    session["effects"] = Value::Array(Vec::new());
    session["clarification"] = Value::Null;
    session["draft"] = serde_json::json!({"selections": {}, "other": {}});
    session["checkpoint"] = Value::Null;
    Ok(())
}

fn encode_session(session: &Value) -> Result<String, String> {
    let body = serde_json::to_string(session).map_err(|error| error.to_string())?;
    if body.len() > MAX_CONVERSATION_BYTES {
        Err(format!(
            "conversation_too_large: max={MAX_CONVERSATION_BYTES}"
        ))
    } else {
        Ok(body)
    }
}

fn validate_title(title: String) -> Result<String, String> {
    let title = title.trim().to_string();
    if title.is_empty() || title.len() > 240 {
        Err("title must contain 1..240 bytes".into())
    } else {
        Ok(title)
    }
}

fn conversation_search_pattern(query: Option<String>) -> Result<Option<String>, String> {
    let Some(query) = query else { return Ok(None) };
    if query.chars().any(char::is_control) {
        return Err("conversation search query must contain 1..120 visible characters".into());
    }
    let query = query.trim();
    if query.is_empty() {
        return Ok(None);
    }
    if query.chars().count() > 120 {
        return Err("conversation search query must contain 1..120 visible characters".into());
    }
    let escaped = query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    Ok(Some(format!("%{escaped}%")))
}

fn validate_identity(value: &str, name: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 128 {
        Err(format!("{name} must contain 1..128 bytes"))
    } else {
        Ok(())
    }
}

fn normalize_idempotency_key(value: &str) -> Result<String, String> {
    if value.is_empty() || value.len() > astock_protocol::MAX_FRAME_BYTES {
        return Err("idempotency_key must contain 1..8 MiB bytes".into());
    }
    Ok(format!("sha256:{:x}", Sha256::digest(value.as_bytes())))
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
        assert_eq!(tables.len(), 4);
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
                checkpoint: serde_json::json!({"task_id":"task-1","accepted_seq":1}),
            },
        )
        .await
        .unwrap();
        let loaded = load_task(&storage, "task-1".into(), 500, None)
            .await
            .unwrap();
        assert_eq!(loaded.task.accepted_seq, 1);
        assert_eq!(loaded.task.checkpoint.unwrap()["accepted_seq"], 1);
        assert_eq!(loaded.events.len(), 1);
        assert_eq!(loaded.events[0].event_id, "event-1");
        assert!(!loaded.events_truncated);
        assert!(loaded.next_before_seq.is_none());
    }

    #[tokio::test]
    async fn task_events_are_loaded_in_bounded_backward_pages() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        migrate(&storage).await.unwrap();
        create_task(
            &storage,
            CreateTask {
                task_id: "task-page".into(),
                reducer_version: "kernel-v1".into(),
                task_spec: serde_json::json!({"objective":"page"}),
                phase: "preparing".into(),
            },
        )
        .await
        .unwrap();
        for seq in 1..=3 {
            append_event(
                &storage,
                AppendEvent {
                    task_id: "task-page".into(),
                    seq,
                    event_id: format!("event-{seq}"),
                    event_kind: "step".into(),
                    event: serde_json::json!({"seq":seq}),
                },
            )
            .await
            .unwrap();
        }

        let latest = load_task(&storage, "task-page".into(), 2, None)
            .await
            .unwrap();
        assert_eq!(
            latest
                .events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(latest.events_truncated);
        assert_eq!(latest.next_before_seq, Some(2));

        let earlier = load_task(&storage, "task-page".into(), 2, latest.next_before_seq)
            .await
            .unwrap();
        assert_eq!(
            earlier
                .events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert!(!earlier.events_truncated);
        assert!(earlier.next_before_seq.is_none());
    }

    #[tokio::test]
    async fn effects_are_persisted_before_results_and_finish_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        migrate(&storage).await.unwrap();
        create_task(
            &storage,
            CreateTask {
                task_id: "task-effect".into(),
                reducer_version: "kernel-v1".into(),
                task_spec: serde_json::json!({"objective":"audit"}),
                phase: "preparing".into(),
            },
        )
        .await
        .unwrap();
        let intent = BeginEffect {
            effect_id: "effect-1".into(),
            task_id: "task-effect".into(),
            caused_by_seq: 1,
            effect_kind: "provider.request".into(),
            effect: serde_json::json!({"model":"minimax-plus","operation":"plan"}),
            idempotency_key: "task-effect:plan:1".into(),
        };
        assert!(begin_effect(&storage, intent.clone()).await.unwrap());
        assert!(!begin_effect(&storage, intent).await.unwrap());
        let pending = list_effects(&storage, "task-effect".into(), 500, None)
            .await
            .unwrap();
        assert_eq!(pending.items[0].status, "pending");
        assert!(pending.items[0].result.is_none());
        assert_eq!(pending.items[0].idempotency_key.len(), 71);
        assert!(pending.items[0].idempotency_key.starts_with("sha256:"));

        let completion = CompleteEffect {
            effect_id: "effect-1".into(),
            status: "succeeded".into(),
            result: serde_json::json!({"evidence_ids":["evidence-1"]}),
        };
        complete_effect(&storage, completion.clone()).await.unwrap();
        complete_effect(&storage, completion).await.unwrap();
        let completed = list_effects(&storage, "task-effect".into(), 500, None)
            .await
            .unwrap();
        assert_eq!(completed.items[0].status, "succeeded");
        assert_eq!(
            completed.items[0].result.as_ref().unwrap()["evidence_ids"][0],
            "evidence-1"
        );

        begin_effect(
            &storage,
            BeginEffect {
                effect_id: "effect-2".into(),
                task_id: "task-effect".into(),
                caused_by_seq: 2,
                effect_kind: "provider.request".into(),
                effect: serde_json::json!({"model":"minimax-plus","operation":"review"}),
                idempotency_key: "task-effect:review:2".into(),
            },
        )
        .await
        .unwrap();
        let first_page = list_effects(&storage, "task-effect".into(), 1, None)
            .await
            .unwrap();
        assert_eq!(first_page.items[0].effect_id, "effect-1");
        let cursor = (
            first_page.next_after_caused_by_seq.unwrap(),
            first_page.next_after_effect_id.unwrap(),
        );
        let second_page = list_effects(&storage, "task-effect".into(), 1, Some(cursor))
            .await
            .unwrap();
        assert_eq!(second_page.items[0].effect_id, "effect-2");
        assert!(second_page.next_after_effect_id.is_none());
    }

    #[tokio::test]
    async fn conversations_are_durable_branchable_and_soft_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        migrate(&storage).await.unwrap();
        let session = serde_json::json!({
            "sessionId": "conversation-1",
            "messages": [
                {"id":"message-1","role":"user","text":"研究目标"},
                {"id":"message-2","role":"agent","text":"历史结论"}
            ],
            "task": {"phase":"completed","evidence_ids":["evidence-1"]}
        });
        save_conversation(
            &storage,
            SaveConversation {
                conversation_id: "conversation-1".into(),
                title: "原始研究".into(),
                session,
            },
        )
        .await
        .unwrap();
        let listed = list_conversations(&storage, 20, None).await.unwrap();
        assert_eq!(listed[0].message_count, 2);
        assert_eq!(listed[0].evidence_count, 1);

        let branched = branch_conversation(
            &storage,
            BranchConversation {
                source_conversation_id: "conversation-1".into(),
                new_conversation_id: "conversation-2".into(),
                message_id: "message-1".into(),
                title: "分支研究".into(),
                checkpoint_task_id: None,
                checkpoint_accepted_seq: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(branched.session["messages"].as_array().unwrap().len(), 1);
        assert!(branched.session["task"].is_null());
        assert_eq!(
            branched.parent_conversation_id.as_deref(),
            Some("conversation-1")
        );

        assert!(delete_conversation(&storage, "conversation-1".into())
            .await
            .unwrap());
        assert!(load_conversation(&storage, "conversation-1".into())
            .await
            .is_err());
        assert_eq!(
            list_conversations(&storage, 20, None).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn checkpoint_branch_requires_matching_durable_engine_truth() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        migrate(&storage).await.unwrap();
        create_task(
            &storage,
            CreateTask {
                task_id: "task-checkpoint".into(),
                reducer_version: "moonbit-agent-kernel-v1".into(),
                task_spec: serde_json::json!({"objective":"核验检查点分支"}),
                phase: "preparing".into(),
            },
        )
        .await
        .unwrap();
        append_event(
            &storage,
            AppendEvent {
                task_id: "task-checkpoint".into(),
                seq: 1,
                event_id: "checkpoint-input-1".into(),
                event_kind: "start".into(),
                event: serde_json::json!({"objective":"核验检查点分支"}),
            },
        )
        .await
        .unwrap();
        put_checkpoint(
            &storage,
            PutCheckpoint {
                task_id: "task-checkpoint".into(),
                accepted_seq: 1,
                phase: "preparing".into(),
                checkpoint: serde_json::json!({
                    "task_id":"task-checkpoint",
                    "accepted_seq":1,
                    "phase":"preparing"
                }),
            },
        )
        .await
        .unwrap();
        save_conversation(
            &storage,
            SaveConversation {
                conversation_id: "checkpoint-conversation".into(),
                title: "检查点研究".into(),
                session: serde_json::json!({
                    "sessionId":"checkpoint-conversation",
                    "messages":[{"id":"checkpoint-message","role":"user","text":"原目标"}],
                    "task":{"task_id":"task-checkpoint","accepted_seq":1,"phase":"preparing"}
                }),
            },
        )
        .await
        .unwrap();

        let branched = branch_conversation(
            &storage,
            BranchConversation {
                source_conversation_id: "checkpoint-conversation".into(),
                new_conversation_id: "checkpoint-branch".into(),
                message_id: "checkpoint-message".into(),
                title: "从检查点重研".into(),
                checkpoint_task_id: Some("task-checkpoint".into()),
                checkpoint_accepted_seq: Some(1),
            },
        )
        .await
        .unwrap();
        assert!(branched.session["task"].is_null());
        assert!(branched.session["checkpoint"].is_null());
        assert_eq!(
            branched.session["branchOrigin"]["task_id"],
            "task-checkpoint"
        );
        assert_eq!(branched.session["branchOrigin"]["accepted_seq"], 1);
        let listed = list_conversations(&storage, 20, None).await.unwrap();
        let summary = listed
            .iter()
            .find(|item| item.conversation_id == "checkpoint-branch")
            .unwrap();
        assert_eq!(
            summary.branch_from_checkpoint_task_id.as_deref(),
            Some("task-checkpoint")
        );
        assert_eq!(summary.branch_from_checkpoint_seq, Some(1));

        let mismatch = branch_conversation(
            &storage,
            BranchConversation {
                source_conversation_id: "checkpoint-conversation".into(),
                new_conversation_id: "checkpoint-invalid".into(),
                message_id: "checkpoint-message".into(),
                title: "无效检查点".into(),
                checkpoint_task_id: Some("task-checkpoint".into()),
                checkpoint_accepted_seq: Some(2),
            },
        )
        .await
        .unwrap_err();
        assert!(mismatch.contains("conversation_checkpoint_mismatch"));
    }

    #[tokio::test]
    async fn conversation_search_is_bounded_and_treats_sql_wildcards_literally() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        migrate(&storage).await.unwrap();
        for (id, title) in [
            ("conversation-maotai", "贵州茅台估值研究"),
            ("conversation-percent", "百分比%与下划线_研究"),
            ("conversation-bank", "银行行业研究"),
        ] {
            save_conversation(
                &storage,
                SaveConversation {
                    conversation_id: id.into(),
                    title: title.into(),
                    session: serde_json::json!({"sessionId":id,"messages":[]}),
                },
            )
            .await
            .unwrap();
        }
        let chinese = list_conversations(&storage, 20, Some("茅台".into()))
            .await
            .unwrap();
        assert_eq!(chinese.len(), 1);
        assert_eq!(chinese[0].conversation_id, "conversation-maotai");
        let literal = list_conversations(&storage, 20, Some("%与下划线_".into()))
            .await
            .unwrap();
        assert_eq!(literal.len(), 1);
        assert_eq!(literal[0].conversation_id, "conversation-percent");
        assert!(list_conversations(&storage, 20, Some("\n".into()))
            .await
            .is_err());
    }
}
