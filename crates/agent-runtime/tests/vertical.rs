use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use astock_agent_runtime::{
    AgentEvent, AgentRuntime, AgentStore, EffectIntent, EngineGateway, MessageRole, ModelChunk,
    ModelProvider, ModelRequest, ModelStream, ProviderError, ProviderErrorKind, RunOutcome,
    RuntimeConfig, RuntimeError, RuntimeSession, RuntimeTask, SessionManager, SessionMessageRole,
    StoredCheckpoint, ToolExecutor,
};
use astock_engine::Engine;
use astock_protocol::{RequestEnvelope, PROTOCOL_VERSION};

#[derive(Clone)]
struct ScriptedProvider {
    turns: Arc<Mutex<VecDeque<ScriptedTurn>>>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

type ScriptedTurn = Vec<Result<ModelChunk, ProviderError>>;

impl ScriptedProvider {
    fn new(turns: Vec<ScriptedTurn>) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns.into())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    fn name(&self) -> &'static str {
        "scripted"
    }

    async fn selected_model(&self) -> Result<String, ProviderError> {
        Ok("scripted-financial-model".into())
    }

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
        self.requests.lock().unwrap().push(request);
        let turn = self
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .expect("script contains enough model turns");
        Ok(Box::pin(futures::stream::iter(turn)))
    }
}

#[derive(Clone)]
struct PendingProvider;

#[async_trait]
impl ModelProvider for PendingProvider {
    fn name(&self) -> &'static str {
        "pending"
    }

    async fn selected_model(&self) -> Result<String, ProviderError> {
        Ok("pending-financial-model".into())
    }

    async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ProviderError> {
        Ok(Box::pin(futures::stream::pending()))
    }
}

#[derive(Clone)]
struct RecordingEngine {
    log: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone)]
struct CooperativeBlockingEngine {
    log: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl ToolExecutor for CooperativeBlockingEngine {
    async fn execute(
        &self,
        engine_kind: &str,
        _payload: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, String> {
        self.log
            .lock()
            .unwrap()
            .push(format!("execute:{engine_kind}"));
        cancellation.cancelled().await;
        self.log
            .lock()
            .unwrap()
            .push(format!("cancelled:{engine_kind}"));
        Err("cancelled".into())
    }
}

#[async_trait]
impl ToolExecutor for RecordingEngine {
    async fn execute(
        &self,
        engine_kind: &str,
        _payload: Value,
        _cancellation: CancellationToken,
    ) -> Result<Value, String> {
        self.log
            .lock()
            .unwrap()
            .push(format!("execute:{engine_kind}"));
        match engine_kind {
            "market.quote" => Ok(json!({
                "quote": {"symbol": "601899", "price": 21.5},
                "source": "mock-current-quote",
                "fetched_at": "2026-08-25T12:00:00Z",
                "evidence_registry": {"facts": [
                    {"evidence_id":"evf_price","path":"/quote/price","value":21.5,"source":"mock-current-quote","observed_at":"2026-08-25T12:00:00Z","source_version_id":"quote-v1","quality_blocking":false},
                    {"evidence_id":"evf_symbol","path":"/quote/symbol","value":"601899","source":"security-master","observed_at":"2026-08-25T12:00:00Z","source_version_id":"master-v1","quality_blocking":false},
                    {"evidence_id":"evf_time","path":"/fetched_at","value":"2026-08-25T12:00:00Z","source":"mock-current-quote","observed_at":"2026-08-25T12:00:00Z","source_version_id":"quote-v1","quality_blocking":false},
                    {"evidence_id":"evf_source","path":"/source","value":"mock-current-quote","source":"mock-current-quote","observed_at":"2026-08-25T12:00:00Z","source_version_id":"quote-v1","quality_blocking":false}
                ]}
            })),
            "research.agent_report_verify" => Ok(json!({
                "passed": true,
                "findings": [],
                "verification_version": "mock-engine-verifier-v1"
            })),
            other => Err(format!("unexpected Engine operation: {other}")),
        }
    }
}

#[derive(Clone)]
struct RecordingStore {
    log: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl AgentStore for RecordingStore {
    async fn create_task(&self, _task_id: &str, _task: &RuntimeTask) -> Result<(), String> {
        self.log.lock().unwrap().push("task:create".into());
        Ok(())
    }

    async fn append_event(
        &self,
        _task_id: &str,
        seq: u64,
        event: &AgentEvent,
    ) -> Result<(), String> {
        self.log
            .lock()
            .unwrap()
            .push(format!("event:{seq}:{}", event.kind()));
        Ok(())
    }

    async fn put_checkpoint(&self, checkpoint: &StoredCheckpoint) -> Result<(), String> {
        self.log
            .lock()
            .unwrap()
            .push(format!("checkpoint:{}", checkpoint.accepted_seq));
        Ok(())
    }

    async fn begin_effect(&self, intent: &EffectIntent) -> Result<(), String> {
        self.log
            .lock()
            .unwrap()
            .push(format!("begin:{}", intent.effect_kind));
        Ok(())
    }

    async fn complete_effect(
        &self,
        _effect_id: &str,
        status: &str,
        _result: &Value,
    ) -> Result<(), String> {
        self.log
            .lock()
            .unwrap()
            .push(format!("effect-complete:{status}"));
        Ok(())
    }
}

fn tool_round() -> Vec<Result<ModelChunk, ProviderError>> {
    vec![
        Ok(ModelChunk::TextDelta("我会先核验最新行情。".into())),
        Ok(ModelChunk::ToolCallDelta {
            index: 0,
            id: Some("call-quote".into()),
            name: Some("get_quote".into()),
            arguments: Some("{\"symbol\":\"601899\"}".into()),
        }),
        Ok(ModelChunk::Finished {
            reason: Some("tool_calls".into()),
        }),
    ]
}

/// The model asks for canonical identifiers before citing anything.
fn search_round() -> Vec<Result<ModelChunk, ProviderError>> {
    vec![
        Ok(ModelChunk::ToolCallDelta {
            index: 0,
            id: Some("call-search".into()),
            name: Some("search_evidence".into()),
            arguments: Some("{\"symbol\":\"601899\",\"field\":\"price\",\"limit\":10}".into()),
        }),
        Ok(ModelChunk::Finished {
            reason: Some("tool_calls".into()),
        }),
    ]
}

/// The structured draft. Every number carries provenance; no citation markup is
/// written by the model, because the renderer owns that.
fn submit_round() -> Vec<Result<ModelChunk, ProviderError>> {
    let draft = json!({
        "version": "astock-report-contract-v1",
        "title": "紫金矿业当前风险收益比",
        "executive_summary": "单一行情快照不足以支持完整投资结论。",
        "sections": [{"heading": "当前市场状态", "claim_ids": ["c1", "c2"]}],
        "claims": [
            {
                "id": "c1",
                "kind": "observed_fact",
                "statement": "最新成交价为每股 21.5 元",
                "evidence_ids": ["evf_price", "evf_time", "evf_symbol", "evf_source"],
                "numeric_items": [{
                    "label": "最新价",
                    "value": 21.5,
                    "unit": "元",
                    "provenance": "observed",
                    "evidence_id": "evf_price",
                    "field": "/quote/price"
                }]
            },
            {
                "id": "c2",
                "kind": "unknown",
                "statement": "仅凭一次行情快照无法判断风险收益比，仍需基本面、公告与反方证据。",
                "evidence_ids": []
            }
        ],
        "overall_uncertainty": "证据覆盖不足，结论仅限当前行情事实。",
        "limitations": ["未取得基本面与公告证据。"]
    });
    vec![
        Ok(ModelChunk::ToolCallDelta {
            index: 0,
            id: Some("call-submit".into()),
            name: Some("submit_report".into()),
            arguments: Some(draft.to_string()),
        }),
        Ok(ModelChunk::Finished {
            reason: Some("tool_calls".into()),
        }),
    ]
}

async fn collect(runtime: AgentRuntime, task: RuntimeTask) -> (Vec<AgentEvent>, RunOutcome) {
    let mut stream = runtime.start(task);
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    let outcome = stream.finish().await.unwrap();
    (events, outcome)
}

/// The whole publication path, end to end.
///
/// prompt → tool → evidence catalog → `search_evidence` → `submit_report` →
/// contract validation → deterministic rendering → independent verifier → publish.
/// Nothing in this flow lets the model write a citation or publish prose: the
/// report exists only because every number in it declared where it came from.
#[tokio::test]
async fn prompt_to_tool_to_structured_submission_to_verified_publication_is_one_durable_flow() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_round(),
        search_round(),
        submit_round(),
    ]));
    let engine = Arc::new(RecordingEngine { log: log.clone() });
    let store = Arc::new(RecordingStore { log: log.clone() });
    let runtime = AgentRuntime::new(provider, engine, store);
    let mut task = RuntimeTask::ask("分析紫金矿业当前风险收益比");
    task.symbol = Some("601899".into());

    let (events, outcome) = collect(runtime, task).await;
    // The published report is the rendered form: numbered citations, and never a
    // canonical identifier.
    assert!(outcome.report.contains("紫金矿业当前风险收益比"));
    assert!(outcome.report.contains("[1]"));
    assert!(
        !astock_agent_runtime::contains_internal_identifier(&outcome.report),
        "a published report must not leak an internal identifier:\n{}",
        outcome.report
    );
    assert!(outcome.evidence_ids.contains(&"evf_price".to_string()));
    assert!(events.iter().any(
        |event| matches!(event, AgentEvent::ToolCompleted { tool, .. } if tool == "get_quote")
    ));
    // Evidence discovery really ran and really was served by the runtime.
    assert!(events.iter().any(
        |event| matches!(event, AgentEvent::ToolCompleted { tool, .. } if tool == "search_evidence")
    ));
    assert!(events.iter().any(
        |event| matches!(event, AgentEvent::ToolCompleted { tool, .. } if tool == "submit_report")
    ));
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::VerificationStarted)));
    assert!(matches!(events.last(), Some(AgentEvent::Completed { .. })));

    let log = log.lock().unwrap();
    // A Runtime tool must never reach the Engine.
    assert!(
        !log.iter().any(|line| line == "execute:"),
        "a runtime-served tool was dispatched to the Engine: {log:?}"
    );
    let intent = log
        .iter()
        .position(|line| line == "begin:tool.get_quote")
        .unwrap();
    let effect = log
        .iter()
        .position(|line| line == "execute:market.quote")
        .unwrap();
    assert!(
        intent < effect,
        "tool intent must be durable before execution"
    );
    for (index, line) in log.iter().enumerate() {
        if let Some(seq) = line.strip_prefix("event:").and_then(|tail| {
            tail.split(':')
                .next()
                .and_then(|value| value.parse::<u64>().ok())
        }) {
            assert_eq!(log.get(index + 1), Some(&format!("checkpoint:{seq}")));
        }
    }
}

#[tokio::test]
async fn unknown_model_tool_fails_closed_and_never_reaches_engine() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(ScriptedProvider::new(vec![vec![
        Ok(ModelChunk::ToolCallDelta {
            index: 0,
            id: Some("call-shell".into()),
            name: Some("run_shell".into()),
            arguments: Some("{}".into()),
        }),
        Ok(ModelChunk::Finished {
            reason: Some("tool_calls".into()),
        }),
    ]]));
    let engine = Arc::new(RecordingEngine { log: log.clone() });
    let store = Arc::new(RecordingStore { log: log.clone() });
    let runtime = AgentRuntime::new(provider, engine, store);
    let mut stream = runtime.start(RuntimeTask::ask("执行任意命令"));
    let mut saw_failure = false;
    while let Some(event) = stream.recv().await {
        saw_failure |= matches!(event, AgentEvent::Failed { .. });
    }
    let error = stream.finish().await.unwrap_err();
    assert!(error.to_string().contains("unknown tool"));
    assert!(saw_failure);
    assert!(!log
        .lock()
        .unwrap()
        .iter()
        .any(|line| line.starts_with("execute:")));
}

#[tokio::test]
async fn embedded_engine_persists_the_complete_headless_task_projection() {
    let temporary = tempfile::tempdir().unwrap();
    let engine = Arc::new(Engine::initialize_at(temporary.path()).await.unwrap());
    let gateway = Arc::new(EngineGateway::new(engine.clone()));
    let provider = Arc::new(ScriptedProvider::new(vec![vec![
        Ok(ModelChunk::TextDelta(
            "【未知】离线验证未请求实时数据，因此不发布投资判断。".into(),
        )),
        Ok(ModelChunk::Finished {
            reason: Some("stop".into()),
        }),
    ]]));
    let runtime =
        AgentRuntime::new(provider, gateway.clone(), gateway).with_config(RuntimeConfig {
            verify_reports: false,
            ..RuntimeConfig::default()
        });
    let (_, outcome) = collect(runtime, RuntimeTask::ask("离线持久化检查")).await;

    let response = engine
        .dispatch(&RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: "load-persisted-task".into(),
            kind: "agent.task.load".into(),
            payload: json!({"task_id": outcome.task_id}),
            deadline_ms: Some(5_000),
            cancellation_id: None,
        })
        .await;
    assert!(response.ok, "{:?}", response.error);
    assert_eq!(response.payload["task"]["phase"], "completed");
    assert_eq!(
        response.payload["events"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["event_kind"],
        "completed"
    );
    assert_eq!(
        response.payload["task"]["checkpoint"]["state_version"],
        "rust-agent-runtime-v1"
    );
}

#[tokio::test]
async fn bounded_financial_program_runs_through_the_agent_engine_gateway() {
    let temporary = tempfile::tempdir().unwrap();
    let engine = Arc::new(Engine::initialize_at(temporary.path()).await.unwrap());
    let gateway = Arc::new(EngineGateway::new(engine));
    let program = json!({
        "version": 1,
        "inputs": {"close": [100.0, 110.0, 99.0]},
        "bindings": [{
            "name": "ret",
            "expr": {"op": "returns", "input": {"op": "var", "name": "close"}}
        }],
        "outputs": {
            "mean_return": {"op": "mean", "input": {"op": "var", "name": "ret"}},
            "max_drawdown": {"op": "max_drawdown", "input": {"op": "var", "name": "close"}}
        }
    });
    let direct = gateway
        .execute(
            "research.compute",
            json!({"program": program.clone()}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let drawdown = direct["execution"]["outputs"]["max_drawdown"]["value"]
        .as_f64()
        .unwrap();
    assert!((drawdown + 0.1).abs() < 1e-12);
    assert_eq!(direct["safety"], "bounded_deterministic_json_ast");
    assert!(!direct["evidence_registry"]["facts"]
        .as_array()
        .unwrap()
        .is_empty());

    let provider = Arc::new(ScriptedProvider::new(vec![
        vec![
            Ok(ModelChunk::ToolCallDelta {
                index: 0,
                id: Some("call-compute".into()),
                name: Some("run_financial_calculation".into()),
                arguments: Some(json!({"program": program}).to_string()),
            }),
            Ok(ModelChunk::Finished {
                reason: Some("tool_calls".into()),
            }),
        ],
        vec![
            Ok(ModelChunk::TextDelta(
                "【计算】受限 Rust 金融计算语言已完成收益率与回撤计算。".into(),
            )),
            Ok(ModelChunk::Finished {
                reason: Some("stop".into()),
            }),
        ],
    ]));
    let runtime =
        AgentRuntime::new(provider, gateway.clone(), gateway).with_config(RuntimeConfig {
            verify_reports: false,
            ..RuntimeConfig::default()
        });
    let (events, outcome) = collect(runtime, RuntimeTask::ask("计算样本收益与最大回撤")).await;
    assert!(!outcome.evidence_ids.is_empty());
    assert!(events.iter().any(
        |event| matches!(event, AgentEvent::ToolCompleted { tool, .. } if tool == "run_financial_calculation")
    ));
}

#[tokio::test]
async fn multi_turn_session_is_durable_and_rehydrates_model_history() {
    let temporary = tempfile::tempdir().unwrap();
    let engine = Arc::new(Engine::initialize_at(temporary.path()).await.unwrap());
    let gateway = Arc::new(EngineGateway::new(engine));
    let provider = Arc::new(ScriptedProvider::new(vec![
        vec![
            Ok(ModelChunk::TextDelta("第一轮可审计回答".into())),
            Ok(ModelChunk::Finished {
                reason: Some("stop".into()),
            }),
        ],
        vec![
            Ok(ModelChunk::TextDelta("第二轮结合上一轮回答".into())),
            Ok(ModelChunk::Finished {
                reason: Some("stop".into()),
            }),
        ],
    ]));
    let runtime = AgentRuntime::new(provider.clone(), gateway.clone(), gateway.clone())
        .with_config(RuntimeConfig {
            verify_reports: false,
            ..RuntimeConfig::default()
        });
    let session = RuntimeSession::new("balanced", "full");
    let session_id = session.session_id.clone();
    let first = runtime
        .run_session_turn(session, RuntimeTask::ask("第一轮问题"))
        .await
        .unwrap();
    assert_eq!(first.session.messages.len(), 2);
    assert_eq!(
        first.session.task.as_ref().unwrap().phase.as_str(),
        "completed"
    );

    let second = runtime
        .run_session_turn(first.session, RuntimeTask::ask("第二轮问题"))
        .await
        .unwrap();
    assert_eq!(second.session.messages.len(), 4);
    assert_eq!(second.session.session_id, session_id);

    let manager = SessionManager::new(gateway);
    let listed = manager.list(10, None).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].conversation_id, session_id);
    assert_eq!(listed[0].message_count, 4);
    let loaded = manager.load(&session_id).await.unwrap();
    assert_eq!(loaded.session.messages, second.session.messages);
    let branch_message_id = loaded.session.messages.last().unwrap().id.clone();
    let branched = manager
        .branch(&session_id, None, Some("第二轮后的反方研究"))
        .await
        .unwrap();
    assert_ne!(branched.conversation_id, session_id);
    assert_eq!(
        branched.parent_conversation_id.as_deref(),
        Some(&*session_id)
    );
    assert_eq!(
        branched.branch_from_message_id.as_deref(),
        Some(&*branch_message_id)
    );
    assert_eq!(branched.session.messages, loaded.session.messages);
    assert!(branched.session.task.is_none());
    assert_eq!(manager.list(10, None).await.unwrap().len(), 2);
    assert!(manager
        .branch(&session_id, Some("not-a-message"), None)
        .await
        .unwrap_err()
        .to_string()
        .contains("does not belong"));

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    let second_messages = &requests[1].messages;
    assert_eq!(second_messages.len(), 4);
    assert_eq!(second_messages[1].content, "第一轮问题");
    assert_eq!(second_messages[2].content, "第一轮可审计回答");
    assert_eq!(second_messages[3].content, "第二轮问题");
}

#[tokio::test]
async fn provider_authentication_failure_is_terminal_failed() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(ScriptedProvider::new(vec![vec![Err(ProviderError::new(
        ProviderErrorKind::Authentication,
        "401 invalid credential",
        false,
    ))]]));
    let runtime = AgentRuntime::new(
        provider,
        Arc::new(RecordingEngine { log: log.clone() }),
        Arc::new(RecordingStore { log }),
    );
    let mut stream = runtime.start(RuntimeTask::ask("鉴权故障注入"));
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    let error = stream.finish().await.unwrap_err();
    assert!(matches!(
        error,
        RuntimeError::Provider(ProviderError {
            kind: ProviderErrorKind::Authentication,
            ..
        })
    ));
    assert!(matches!(events.last(), Some(AgentEvent::Failed { .. })));
    assert!(!events.iter().any(|event| matches!(
        event,
        AgentEvent::Suspended { .. } | AgentEvent::Completed { .. }
    )));
}

#[tokio::test]
async fn provider_rate_limit_after_partial_stream_is_suspended_not_completed() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(ScriptedProvider::new(vec![vec![
        Ok(ModelChunk::TextDelta("未完成草稿".into())),
        Err(ProviderError::new(
            ProviderErrorKind::RateLimited,
            "429 rate limited",
            false,
        )),
    ]]));
    let runtime = AgentRuntime::new(
        provider,
        Arc::new(RecordingEngine { log: log.clone() }),
        Arc::new(RecordingStore { log }),
    );
    let mut stream = runtime.start(RuntimeTask::ask("限流故障注入"));
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    let error = stream.finish().await.unwrap_err();
    assert!(matches!(
        error,
        RuntimeError::Provider(ProviderError {
            kind: ProviderErrorKind::RateLimited,
            ..
        })
    ));
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::TextDelta { .. })));
    assert!(matches!(events.last(), Some(AgentEvent::Suspended { .. })));
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::Completed { .. })));
}

#[tokio::test]
async fn provider_idle_timeout_is_bounded_and_suspended() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let runtime = AgentRuntime::new(
        Arc::new(PendingProvider),
        Arc::new(RecordingEngine { log: log.clone() }),
        Arc::new(RecordingStore { log }),
    )
    .with_config(RuntimeConfig {
        provider_idle_timeout: Duration::from_millis(20),
        verify_reports: false,
        ..RuntimeConfig::default()
    });
    let mut stream = runtime.start(RuntimeTask::ask("流超时故障注入"));
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    let error = stream.finish().await.unwrap_err();
    assert!(error.to_string().contains("idle timeout"));
    assert!(matches!(events.last(), Some(AgentEvent::Suspended { .. })));
}

#[tokio::test]
async fn provider_independent_visible_text_limit_fails_and_closes_effect() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let runtime = AgentRuntime::new(
        Arc::new(ScriptedProvider::new(vec![vec![Ok(
            ModelChunk::TextDelta("12345".into()),
        )]])),
        Arc::new(RecordingEngine { log: log.clone() }),
        Arc::new(RecordingStore { log: log.clone() }),
    )
    .with_config(RuntimeConfig {
        max_visible_chars_per_round: 4,
        verify_reports: false,
        ..RuntimeConfig::default()
    });
    let mut stream = runtime.start(RuntimeTask::ask("流大小故障注入"));
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    let error = stream.finish().await.unwrap_err();
    assert!(matches!(
        error,
        RuntimeError::Provider(ProviderError {
            kind: ProviderErrorKind::MalformedResponse,
            ..
        })
    ));
    assert!(matches!(events.last(), Some(AgentEvent::Failed { .. })));
    assert!(log
        .lock()
        .unwrap()
        .iter()
        .any(|entry| entry == "effect-complete:failed"));
}

#[tokio::test]
async fn cancellation_interrupts_a_pending_provider_stream() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let runtime = AgentRuntime::new(
        Arc::new(PendingProvider),
        Arc::new(RecordingEngine { log: log.clone() }),
        Arc::new(RecordingStore { log }),
    )
    .with_config(RuntimeConfig {
        provider_idle_timeout: Duration::from_secs(30),
        verify_reports: false,
        ..RuntimeConfig::default()
    });
    let mut stream = runtime.start(RuntimeTask::ask("取消故障注入"));
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        let started = matches!(event, AgentEvent::ModelStarted { .. });
        events.push(event);
        if started {
            stream.cancel();
        }
    }
    let error = tokio::time::timeout(Duration::from_secs(1), stream.finish())
        .await
        .expect("cancellation must not wait for the provider idle timeout")
        .unwrap_err();
    assert!(matches!(error, RuntimeError::Cancelled));
    assert!(matches!(events.last(), Some(AgentEvent::Cancelled)));
}

#[tokio::test]
async fn cancellation_reaches_a_cooperative_engine_tool() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let runtime = AgentRuntime::new(
        Arc::new(ScriptedProvider::new(vec![tool_round()])),
        Arc::new(CooperativeBlockingEngine { log: log.clone() }),
        Arc::new(RecordingStore { log: log.clone() }),
    )
    .with_config(RuntimeConfig {
        verify_reports: false,
        ..RuntimeConfig::default()
    });
    let mut stream = runtime.start(RuntimeTask::ask("工具取消故障注入"));
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        let tool_started = matches!(event, AgentEvent::ToolStarted { .. });
        events.push(event);
        if tool_started {
            stream.cancel();
        }
    }
    let error = tokio::time::timeout(Duration::from_secs(1), stream.finish())
        .await
        .expect("cooperative tool cancellation must complete promptly")
        .unwrap_err();
    assert!(matches!(error, RuntimeError::Cancelled));
    assert!(matches!(events.last(), Some(AgentEvent::Cancelled)));
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolCompleted { .. })));
    let log = log.lock().unwrap();
    assert!(log.iter().any(|line| line == "execute:market.quote"));
    assert!(log.iter().any(|line| line == "cancelled:market.quote"));
}

#[tokio::test]
async fn oversized_tool_result_is_visible_to_model_as_a_bounded_failure() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(ScriptedProvider::new(vec![
        tool_round(),
        vec![
            Ok(ModelChunk::TextDelta(
                "工具结果超过上下文边界，未据此发布数字。".into(),
            )),
            Ok(ModelChunk::Finished {
                reason: Some("stop".into()),
            }),
        ],
    ]));
    let runtime = AgentRuntime::new(
        provider.clone(),
        Arc::new(RecordingEngine { log: log.clone() }),
        Arc::new(RecordingStore { log }),
    )
    .with_config(RuntimeConfig {
        max_tool_result_bytes: 64,
        verify_reports: false,
        ..RuntimeConfig::default()
    });
    let (events, outcome) = collect(runtime, RuntimeTask::ask("超大工具结果故障注入")).await;
    assert!(outcome.evidence_ids.is_empty());
    assert!(events.iter().any(|event| {
        matches!(event, AgentEvent::ToolFailed { message, .. } if message.contains("above the 64-byte limit"))
    }));
    assert!(matches!(events.last(), Some(AgentEvent::Completed { .. })));
    let requests = provider.requests();
    let tool_message = requests[1]
        .messages
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("call-quote"))
        .expect("the second model round receives the bounded tool failure");
    assert!(tool_message.content.contains("above the 64-byte limit"));
}

#[tokio::test]
async fn long_session_uses_summary_plus_bounded_recent_history() {
    let temporary = tempfile::tempdir().unwrap();
    let engine = Arc::new(Engine::initialize_at(temporary.path()).await.unwrap());
    let gateway = Arc::new(EngineGateway::new(engine));
    let provider = Arc::new(ScriptedProvider::new(vec![vec![
        Ok(ModelChunk::TextDelta("压缩后继续回答".into())),
        Ok(ModelChunk::Finished {
            reason: Some("stop".into()),
        }),
    ]]));
    let runtime =
        AgentRuntime::new(provider.clone(), gateway.clone(), gateway).with_config(RuntimeConfig {
            verify_reports: false,
            ..RuntimeConfig::default()
        });
    let mut session = RuntimeSession::new("balanced", "full");
    for index in 0..45 {
        let role = if index % 2 == 0 {
            SessionMessageRole::User
        } else {
            SessionMessageRole::Agent
        };
        session.push_message(role, format!("历史上下文 {index}"));
    }

    let outcome = runtime
        .run_session_turn(session, RuntimeTask::ask("继续长会话"))
        .await
        .unwrap();
    assert_eq!(outcome.session.messages.len(), 47);
    assert!(outcome.session.summary.is_some());
    let requests = provider.requests();
    assert_eq!(requests[0].messages.len(), 43);
    assert_eq!(requests[0].messages[0].role, MessageRole::System);
    assert_eq!(requests[0].messages[1].role, MessageRole::System);
    assert!(requests[0].messages[1]
        .content
        .contains("不是当前事实、不是新证据"));
    assert_eq!(requests[0].messages[42].content, "继续长会话");
}
