//! v6 to v7 durable-data migration.
//!
//! v7 replaces the MoonBit Agent worker with `astock-agent-runtime`, so a v6
//! conversation may carry an executable task projection written by a reducer that
//! no longer exists. The rule is that readable history must survive, while
//! incompatible *executable* state must not be resumed as though it were ours: a
//! stale phase or effect cursor from another reducer could make the runtime skip
//! work it never performed, or repeat an effect it already completed.
//!
//! These tests pin that boundary against real v6-shaped payloads.

use astock_agent_runtime::{RuntimeSession, SESSION_VERSION};
use serde_json::json;

/// A v6 conversation with no Rust task projection must remain fully readable.
#[test]
fn a_v6_conversation_without_task_state_is_readable_and_valid() {
    let session: RuntimeSession = serde_json::from_value(json!({
        "sessionId": "v6-conversation",
        "title": "紫金矿业研究",
        "createdAt": 1_700_000_000_000_i64,
        "updatedAt": 1_700_000_100_000_i64,
        "input": "",
        "depth": "deep",
        "toolPolicy": "full",
        "messages": [
            { "id": "m1", "role": "user", "text": "分析紫金矿业", "timestamp": "2026-08-25T00:00:00Z" },
            { "id": "m2", "role": "agent", "text": "已完成报告", "timestamp": "2026-08-25T00:05:00Z" }
        ]
    }))
    .expect("a v6 conversation must stay readable");

    assert_eq!(session.messages.len(), 2, "history must be preserved");
    assert_eq!(
        session.version, SESSION_VERSION,
        "a versionless v6 record adopts the current session version on read"
    );
    assert!(
        session.task.is_none(),
        "no executable task state may be invented for a v6 conversation"
    );
    session
        .validate()
        .expect("a migrated v6 conversation must validate");
}

/// A v6 conversation whose task projection came from the retired reducer must not
/// silently become a v7 executable task.
///
/// The concrete risk: v6's projection used different field names and a phase
/// vocabulary the Rust runtime does not share. Adopting it would let the runtime
/// resume from a checkpoint it never wrote.
#[test]
fn an_incompatible_v6_task_projection_does_not_become_executable_v7_state() {
    let payload = json!({
        "sessionId": "v6-with-task",
        "title": "旧任务",
        "createdAt": 1_700_000_000_000_i64,
        "updatedAt": 1_700_000_100_000_i64,
        "input": "",
        "depth": "balanced",
        "toolPolicy": "full",
        "messages": [
            { "id": "m1", "role": "user", "text": "继续研究", "timestamp": "2026-08-25T00:00:00Z" }
        ],
        // Shape written by the MoonBit reducer: no `task_id`, a foreign phase
        // vocabulary and an effect cursor that means nothing to this runtime.
        "task": {
            "taskId": "moon-task-1",
            "state": "awaiting_effect",
            "effectCursor": 42,
            "reducerVersion": "moonbit-agent-v6"
        }
    });

    match serde_json::from_value::<RuntimeSession>(payload) {
        // Rejecting the record outright is acceptable: the caller then archives
        // it rather than resuming it.
        Err(_) => {}
        // Accepting it is only acceptable if no executable state was adopted.
        Ok(session) => {
            assert!(
                session.task.is_none(),
                "a foreign task projection must not be adopted as v7 executable state, got {:?}",
                session.task
            );
            assert_eq!(
                session.messages.len(),
                1,
                "history must survive even when task state is discarded"
            );
        }
    }
}

/// A record claiming an unknown session version must fail closed.
#[test]
fn an_unknown_session_version_is_refused_rather_than_guessed() {
    let session: RuntimeSession = serde_json::from_value(json!({
        "version": "some-future-session-v9",
        "sessionId": "future",
        "title": "未来版本",
        "createdAt": 1_700_000_000_000_i64,
        "updatedAt": 1_700_000_000_000_i64,
        "input": "",
        "depth": "balanced",
        "toolPolicy": "full",
        "messages": [
            { "id": "m1", "role": "user", "text": "hello", "timestamp": "2026-08-25T00:00:00Z" }
        ]
    }))
    .expect("the record still deserializes");

    let error = session
        .validate()
        .expect_err("an unknown session version must not validate");
    assert!(
        format!("{error}").contains("unsupported session version"),
        "the refusal must name the cause, got: {error}"
    );
}

/// The v7 plan is optional, so a session written before plans existed still
/// loads and simply has no plan to display.
#[test]
fn a_session_written_before_plans_existed_still_loads() {
    let session: RuntimeSession = serde_json::from_value(json!({
        "sessionId": "pre-plan",
        "title": "旧会话",
        "createdAt": 1_700_000_000_000_i64,
        "updatedAt": 1_700_000_000_000_i64,
        "input": "",
        "depth": "balanced",
        "toolPolicy": "full",
        "messages": [
            { "id": "m1", "role": "user", "text": "hello", "timestamp": "2026-08-25T00:00:00Z" }
        ],
        "task": {
            "task_id": "t1",
            "phase": "completed",
            "accepted_seq": 7
        }
    }))
    .expect("a pre-plan session must stay readable");

    let task = session
        .task
        .expect("the task projection is ours and is kept");
    assert!(
        task.plan.is_none(),
        "no plan may be invented for a session recorded before plans existed"
    );
    assert_eq!(task.accepted_seq, 7, "the effect cursor must be preserved");
}
