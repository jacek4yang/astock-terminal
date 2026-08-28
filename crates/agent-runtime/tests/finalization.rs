//! Structured finalization as the real publication path.
//!
//! These tests pin the property the product depends on: a report exists only
//! because every material number in it declared where it came from, and it is
//! published only after an independent verifier agreed. Every route that used to
//! bypass that — free-form prose, a hand-written citation, an invented identifier,
//! an estimate standing in for arithmetic — is asserted closed here.
//!
//! Deterministic throughout: a scripted provider and either a scripted Engine or a
//! real embedded Engine on a temporary directory. No provider quota is consumed.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use astock_agent_runtime::{
    AgentEvent, AgentRuntime, AgentStore, EffectIntent, EngineGateway, ModelChunk, ModelProvider,
    ModelRequest, ModelStream, ProviderError, RunOutcome, RuntimeConfig, RuntimeError, RuntimeTask,
    StoredCheckpoint, ToolExecutor,
};
use astock_engine::Engine;

type Turn = Vec<Result<ModelChunk, ProviderError>>;

#[derive(Clone)]
struct ScriptedProvider {
    turns: Arc<Mutex<VecDeque<Turn>>>,
    /// Tool names offered on each round, so a test can assert what the model could
    /// actually have called rather than what it was asked to do.
    offered_tool_names: Arc<Mutex<Vec<Vec<String>>>>,
}

impl ScriptedProvider {
    fn new(turns: Vec<Turn>) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns.into())),
            offered_tool_names: Arc::new(Mutex::new(Vec::new())),
        }
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
        self.offered_tool_names
            .lock()
            .unwrap()
            .push(request.tools.iter().map(|tool| tool.name.clone()).collect());
        let turn = self
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .expect("the script contains enough model turns");
        Ok(Box::pin(futures::stream::iter(turn)))
    }
}

/// An Engine whose verifier verdicts are scripted, so the runtime's response to a
/// refusal can be tested without depending on the verifier's own rules.
#[derive(Clone)]
struct ScriptedEngine {
    dispatched: Arc<Mutex<Vec<String>>>,
    verdicts: Arc<Mutex<VecDeque<Value>>>,
}

impl ScriptedEngine {
    fn new(verdicts: Vec<Value>) -> Self {
        Self {
            dispatched: Arc::new(Mutex::new(Vec::new())),
            verdicts: Arc::new(Mutex::new(verdicts.into())),
        }
    }

    fn dispatched(&self) -> Vec<String> {
        self.dispatched.lock().unwrap().clone()
    }
}

fn passing() -> Value {
    json!({"passed": true, "findings": [], "verification_version": "scripted-v1"})
}

fn refusing(findings: Vec<&str>) -> Value {
    json!({"passed": false, "findings": findings, "verification_version": "scripted-v1"})
}

#[async_trait]
impl ToolExecutor for ScriptedEngine {
    async fn execute(
        &self,
        engine_kind: &str,
        _payload: Value,
        _cancellation: CancellationToken,
    ) -> Result<Value, String> {
        self.dispatched.lock().unwrap().push(engine_kind.to_owned());
        match engine_kind {
            "market.quote" => Ok(quote_result()),
            "research.agent_report_verify" => Ok(self
                .verdicts
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(passing)),
            other => Err(format!("unexpected Engine operation: {other}")),
        }
    }
}

/// A quote result shaped like the Engine's, carrying a real evidence registry.
fn quote_result() -> Value {
    json!({
        "quote": {"symbol": "601899", "price": 21.5, "volume": 184_000_000},
        "source": "mock-current-quote",
        "fetched_at": "2026-08-25T12:00:00Z",
        "evidence_registry": {"facts": [
            {"evidence_id":"evf_price","path":"/quote/price","value":21.5,"source":"mock-current-quote","observed_at":"2026-08-25T12:00:00Z","source_version_id":"quote-v1","quality_blocking":false},
            {"evidence_id":"evf_volume","path":"/quote/volume","value":184000000,"source":"mock-current-quote","observed_at":"2026-08-25T12:00:00Z","source_version_id":"quote-v1","quality_blocking":false},
            {"evidence_id":"evf_symbol","path":"/quote/symbol","value":"601899","source":"security-master","observed_at":"2026-08-25T12:00:00Z","source_version_id":"master-v1","quality_blocking":false},
            {"evidence_id":"evf_time","path":"/fetched_at","value":"2026-08-25T12:00:00Z","source":"mock-current-quote","observed_at":"2026-08-25T12:00:00Z","source_version_id":"quote-v1","quality_blocking":false}
        ]}
    })
}

#[derive(Clone)]
struct NullStore;

#[async_trait]
impl AgentStore for NullStore {
    async fn create_task(&self, _task_id: &str, _task: &RuntimeTask) -> Result<(), String> {
        Ok(())
    }
    async fn append_event(
        &self,
        _task_id: &str,
        _seq: u64,
        _event: &AgentEvent,
    ) -> Result<(), String> {
        Ok(())
    }
    async fn put_checkpoint(&self, _checkpoint: &StoredCheckpoint) -> Result<(), String> {
        Ok(())
    }
    async fn begin_effect(&self, _intent: &EffectIntent) -> Result<(), String> {
        Ok(())
    }
    async fn complete_effect(
        &self,
        _effect_id: &str,
        _status: &str,
        _result: &Value,
    ) -> Result<(), String> {
        Ok(())
    }
}

fn quote_round() -> Turn {
    vec![
        Ok(ModelChunk::ToolCallDelta {
            index: 0,
            id: Some("call-quote".into()),
            name: Some("get_quote".into()),
            arguments: Some(json!({"symbol": "601899"}).to_string()),
        }),
        Ok(ModelChunk::Finished {
            reason: Some("tool_calls".into()),
        }),
    ]
}

fn submit_round(id: &str, draft: Value) -> Turn {
    vec![
        Ok(ModelChunk::ToolCallDelta {
            index: 0,
            id: Some(id.to_owned()),
            name: Some("submit_report".into()),
            arguments: Some(draft.to_string()),
        }),
        Ok(ModelChunk::Finished {
            reason: Some("tool_calls".into()),
        }),
    ]
}

fn prose_round(text: &str) -> Turn {
    vec![
        Ok(ModelChunk::TextDelta(text.to_owned())),
        Ok(ModelChunk::Finished {
            reason: Some("stop".into()),
        }),
    ]
}

/// A provider turn that carries nothing at all: no text, no tool call.
fn empty_round() -> Turn {
    vec![Ok(ModelChunk::Finished {
        reason: Some("stop".into()),
    })]
}

/// A turn that ends without even a termination marker.
fn silent_round() -> Turn {
    Vec::new()
}

/// A draft citing the mock quote evidence, valid by construction.
fn valid_draft() -> Value {
    json!({
        "version": "astock-report-contract-v1",
        "title": "紫金矿业行情快照",
        "executive_summary": "仅覆盖当前行情事实。",
        "sections": [{"heading": "行情", "claim_ids": ["c1"]}],
        "claims": [{
            "id": "c1",
            "kind": "observed_fact",
            "statement": "最新成交价见下方数值",
            "evidence_ids": ["evf_price", "evf_time", "evf_symbol", "evf_volume"],
            "numeric_items": [{
                "label": "最新价",
                "value": 21.5,
                "unit": "元",
                "provenance": "observed",
                "evidence_id": "evf_price"
            }]
        }]
    })
}

fn with_claims(claims: Value, section_claim_ids: Value) -> Value {
    json!({
        "version": "astock-report-contract-v1",
        "title": "紫金矿业行情快照",
        "executive_summary": "仅覆盖当前行情事实。",
        "sections": [{"heading": "行情", "claim_ids": section_claim_ids}],
        "claims": claims
    })
}

async fn run(
    provider: ScriptedProvider,
    engine: ScriptedEngine,
) -> (Vec<AgentEvent>, Result<RunOutcome, RuntimeError>) {
    run_with_config(provider, engine, RuntimeConfig::default()).await
}

async fn run_with_config(
    provider: ScriptedProvider,
    engine: ScriptedEngine,
    config: RuntimeConfig,
) -> (Vec<AgentEvent>, Result<RunOutcome, RuntimeError>) {
    let runtime = AgentRuntime::new(Arc::new(provider), Arc::new(engine), Arc::new(NullStore))
        .with_config(config);
    let mut task = RuntimeTask::ask("紫金矿业最新价格是多少？");
    task.symbol = Some("601899".into());
    let mut stream = runtime.start(task);
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    (events, stream.finish().await)
}

fn tool_result_payloads(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolFailed { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

/// A fabricated identifier is refused, and repair names the claim to fix.
///
/// The live run invented `计算-BPS` and `财报口径-EPS-2024`. An identifier that is
/// well-shaped but absent from the catalog is the remaining hole, and it closes
/// here: the contract checks existence, not just shape.
#[tokio::test]
async fn an_identifier_absent_from_the_catalog_is_refused_and_repair_targets_the_claim() {
    let invented = with_claims(
        json!([{
            "id": "c1",
            "kind": "observed_fact",
            "statement": "每股净资产见下方数值",
            "evidence_ids": ["evf_fabricated_bps"],
            "numeric_items": [{
                "label": "每股净资产", "value": 8.4, "unit": "元",
                "provenance": "observed", "evidence_id": "evf_fabricated_bps"
            }]
        }]),
        json!(["c1"]),
    );
    let engine = ScriptedEngine::new(vec![passing()]);
    let (events, result) = run(
        ScriptedProvider::new(vec![
            quote_round(),
            submit_round("call-bad", invented),
            submit_round("call-good", valid_draft()),
        ]),
        engine.clone(),
    )
    .await;

    let outcome = result.expect("the corrected submission publishes");
    assert!(outcome.report.contains("21.5"));
    // The first submission never reached the verifier: the contract stopped it.
    assert_eq!(
        engine
            .dispatched()
            .iter()
            .filter(|kind| *kind == "research.agent_report_verify")
            .count(),
        1,
        "an invalid draft must not be sent to the verifier"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::VerificationFinding { finding } if finding.code == "unknown_evidence"
                && finding.blocking
        )),
        "the refusal must be recorded as a blocking finding"
    );
    assert!(tool_result_payloads(&events)
        .iter()
        .any(|message| message.contains("结构化修复指引")));
}

/// An estimate must not stand in for arithmetic the Engine can perform.
#[tokio::test]
async fn an_estimate_cannot_replace_a_computable_quantity() {
    let escape_hatch = with_claims(
        json!([{
            "id": "c1",
            "kind": "estimate",
            "statement": "市值大致如下",
            "evidence_ids": ["evf_price"],
            "numeric_items": [{
                "label": "市值", "value": 5_600.0, "unit": "亿元",
                "provenance": "estimated",
                "method": "按股本乘以市值估算",
                "basis_evidence_ids": ["evf_price"]
            }]
        }]),
        json!(["c1"]),
    );
    let engine = ScriptedEngine::new(vec![passing()]);
    let (events, result) = run(
        ScriptedProvider::new(vec![
            quote_round(),
            submit_round("call-bad", escape_hatch),
            submit_round("call-good", valid_draft()),
        ]),
        engine.clone(),
    )
    .await;

    result.expect("the corrected submission publishes");
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::VerificationFinding { finding } if finding.code == "invalid_estimate"
        )),
        "a computable quantity presented as an estimate must be refused"
    );
}

/// A verifier refusal maps back to the claim that caused it.
///
/// The point of the structured contract is that a positional finding is no longer a
/// dead end: `line_3` becomes `c1`, with the offending figure attached.
#[tokio::test]
async fn a_verifier_refusal_is_returned_as_a_claim_level_repair_and_never_published() {
    let engine = ScriptedEngine::new(vec![
        refusing(vec!["numeric_claim_not_reproduced:line_3:21.5"]),
        passing(),
    ]);
    let (events, result) = run(
        ScriptedProvider::new(vec![
            quote_round(),
            submit_round("call-first", valid_draft()),
            submit_round("call-second", valid_draft_with_volume()),
        ]),
        engine.clone(),
    )
    .await;

    let outcome = result.expect("the second submission publishes");
    assert!(outcome.report.contains("成交量"));
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::VerificationFinding { finding }
                if finding.code == "numeric_claim_not_reproduced:line_3:21.5" && finding.blocking
        )),
        "the verifier finding must be recorded as blocking"
    );
    // Two verifications ran, so the refusal really was a refusal.
    assert_eq!(
        engine
            .dispatched()
            .iter()
            .filter(|kind| *kind == "research.agent_report_verify")
            .count(),
        2
    );
}

fn valid_draft_with_volume() -> Value {
    with_claims(
        json!([{
            "id": "c1",
            "kind": "observed_fact",
            "statement": "当日成交量已披露",
            "evidence_ids": ["evf_volume", "evf_time"],
            "numeric_items": [{
                "label": "成交量", "value": 184_000_000.0, "unit": "股",
                "provenance": "observed", "evidence_id": "evf_volume"
            }]
        }]),
        json!(["c1"]),
    )
}

/// Running out of finalization attempts fails closed. It never publishes.
#[tokio::test]
async fn an_exhausted_finalization_budget_refuses_to_publish() {
    let engine = ScriptedEngine::new(vec![]);
    let broken = with_claims(
        json!([{
            "id": "c1",
            "kind": "observed_fact",
            "statement": "无证据支撑的断言",
            "evidence_ids": []
        }]),
        json!(["c1"]),
    );
    let (_, result) = run_with_config(
        ScriptedProvider::new(vec![
            quote_round(),
            submit_round("s1", broken.clone()),
            submit_round("s2", broken.clone()),
        ]),
        engine.clone(),
        RuntimeConfig {
            max_finalization_attempts: 2,
            ..RuntimeConfig::default()
        },
    )
    .await;

    match result {
        Err(RuntimeError::VerificationFailed(reason)) => {
            assert!(
                reason.contains("validation failed after 2 attempts"),
                "the refusal must say why: {reason}"
            );
        }
        other => panic!("publication must be refused, got {other:?}"),
    }
    assert!(
        !engine
            .dispatched()
            .iter()
            .any(|kind| kind == "research.agent_report_verify"),
        "an invalid draft never reaches the verifier"
    );
}

/// Resubmitting an identical rejected draft is detected as no progress.
#[tokio::test]
async fn an_identical_resubmission_ends_finalization_rather_than_looping() {
    let engine = ScriptedEngine::new(vec![]);
    let broken = with_claims(
        json!([{
            "id": "c1",
            "kind": "observed_fact",
            "statement": "无证据支撑的断言",
            "evidence_ids": []
        }]),
        json!(["c1"]),
    );
    let (_, result) = run_with_config(
        ScriptedProvider::new(vec![
            quote_round(),
            submit_round("s1", broken.clone()),
            submit_round("s2", broken.clone()),
            submit_round("s3", broken.clone()),
        ]),
        engine,
        RuntimeConfig {
            // Generous budget: the loop must end on the repetition, not the budget.
            max_finalization_attempts: 20,
            ..RuntimeConfig::default()
        },
    )
    .await;
    assert!(matches!(result, Err(RuntimeError::VerificationFailed(_))));
}

/// Prose is not a publishable report.
///
/// This is the bypass the contract exists to close. Free-form Markdown used to go
/// straight to the verifier, which forced the model to hand-format `【E:evf_…】`
/// into investor-facing text and left provenance to its formatting discipline.
#[tokio::test]
async fn free_form_prose_cannot_publish_a_report() {
    let engine = ScriptedEngine::new(vec![]);
    let (events, result) = run_with_config(
        ScriptedProvider::new(vec![
            quote_round(),
            prose_round("紫金矿业现价为21.5元【E:evf_price】，建议关注。"),
            submit_round("call-good", valid_draft()),
        ]),
        engine.clone(),
        RuntimeConfig {
            max_finalization_attempts: 4,
            ..RuntimeConfig::default()
        },
    )
    .await;

    let outcome = result.expect("the structured submission publishes");
    // What was published is the rendered report, not the model's prose.
    assert!(!outcome.report.contains("建议关注"));
    assert!(!astock_agent_runtime::contains_internal_identifier(
        &outcome.report
    ));
    assert!(matches!(events.last(), Some(AgentEvent::Completed { .. })));
    // The prose turn was never verified: it was never a publication candidate.
    assert_eq!(
        engine
            .dispatched()
            .iter()
            .filter(|kind| *kind == "research.agent_report_verify")
            .count(),
        1
    );
}

/// Prose alone, repeated, ends the task without publishing anything.
#[tokio::test]
async fn a_task_that_only_ever_produces_prose_fails_closed() {
    let engine = ScriptedEngine::new(vec![]);
    let (_, result) = run_with_config(
        ScriptedProvider::new(vec![
            prose_round("第一次自然语言回答。"),
            prose_round("第二次自然语言回答。"),
        ]),
        engine,
        RuntimeConfig {
            max_finalization_attempts: 2,
            ..RuntimeConfig::default()
        },
    )
    .await;
    match result {
        Err(RuntimeError::VerificationFailed(reason)) => {
            assert!(reason.contains("never submitted through the structured contract"));
        }
        other => panic!("prose must not publish, got {other:?}"),
    }
}

/// `search_evidence` is served by the runtime and returns canonical identifiers.
#[tokio::test]
async fn evidence_search_is_served_by_the_runtime_and_returns_canonical_identifiers() {
    let engine = ScriptedEngine::new(vec![passing()]);
    let search = vec![
        Ok(ModelChunk::ToolCallDelta {
            index: 0,
            id: Some("call-search".into()),
            name: Some("search_evidence".into()),
            arguments: Some(json!({"symbol": "601899", "field": "price", "limit": 5}).to_string()),
        }),
        Ok(ModelChunk::Finished {
            reason: Some("tool_calls".into()),
        }),
    ];
    let (events, result) = run(
        ScriptedProvider::new(vec![
            quote_round(),
            search,
            submit_round("call-good", valid_draft()),
        ]),
        engine.clone(),
    )
    .await;
    result.expect("publication succeeds");

    // Only the two Engine operations were dispatched; the search was not one.
    let dispatched = engine.dispatched();
    assert_eq!(
        dispatched,
        vec!["market.quote", "research.agent_report_verify"],
        "a runtime-served tool must never reach the Engine"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCompleted { tool, .. } if tool == "search_evidence"
    )));
}

/// Evidence for another security cannot support a claim in this task.
#[tokio::test]
async fn evidence_outside_the_task_scope_is_refused() {
    let engine = ScriptedEngine::new(vec![passing()]);
    let foreign = json!({
        "securities": [{"symbol": "600036", "quote": {"last": 41.2}}],
        "evidence_registry": {"facts": [
            {"evidence_id":"evf_other","path":"/securities/0/quote/last","value":41.2,"source":"mock-current-quote","observed_at":"2026-08-25T12:00:00Z","source_version_id":"v1","quality_blocking":false}
        ]}
    });
    // A second engine that also serves a multi-security bundle.
    #[derive(Clone)]
    struct MixedEngine {
        inner: ScriptedEngine,
        foreign: Value,
    }
    #[async_trait]
    impl ToolExecutor for MixedEngine {
        async fn execute(
            &self,
            engine_kind: &str,
            payload: Value,
            cancellation: CancellationToken,
        ) -> Result<Value, String> {
            if engine_kind == "research.agent_security_context" {
                self.inner
                    .dispatched
                    .lock()
                    .unwrap()
                    .push(engine_kind.to_owned());
                return Ok(self.foreign.clone());
            }
            self.inner.execute(engine_kind, payload, cancellation).await
        }
    }

    let mixed = MixedEngine {
        inner: engine.clone(),
        foreign,
    };
    let securities_round = vec![
        Ok(ModelChunk::ToolCallDelta {
            index: 0,
            id: Some("call-sec".into()),
            name: Some("research_securities".into()),
            arguments: Some(
                json!({
                    "symbols": ["600036"], "depth": "fast", "tool_policy": "auto",
                    "analysis_modules": [], "benchmark": "000300",
                    "start": "2026-01-01", "end": "2026-08-25"
                })
                .to_string(),
            ),
        }),
        Ok(ModelChunk::Finished {
            reason: Some("tool_calls".into()),
        }),
    ];
    let out_of_scope = with_claims(
        json!([{
            "id": "c1",
            "kind": "observed_fact",
            "statement": "另一只证券的价格",
            "evidence_ids": ["evf_other"],
            "numeric_items": [{
                "label": "最新价", "value": 41.2, "unit": "元",
                "provenance": "observed", "evidence_id": "evf_other"
            }]
        }]),
        json!(["c1"]),
    );

    let runtime = AgentRuntime::new(
        Arc::new(ScriptedProvider::new(vec![
            quote_round(),
            securities_round,
            submit_round("call-bad", out_of_scope),
            submit_round("call-good", valid_draft()),
        ])),
        Arc::new(mixed),
        Arc::new(NullStore),
    );
    let mut task = RuntimeTask::ask("紫金矿业最新价格是多少？");
    task.symbol = Some("601899".into());
    let mut stream = runtime.start(task);
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    stream.finish().await.expect("the scoped draft publishes");

    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::VerificationFinding { finding }
                if finding.code == "evidence_outside_task_scope"
        )),
        "a claim leaning on another security's evidence must be refused"
    );
}

/// The renderer's canonical form satisfies the **real** Engine verifier.
///
/// The scripted tests above prove the runtime's policy. This one proves the
/// interface: that what the renderer emits is what the deterministic verifier can
/// actually check, using the real verifier, real calculation evidence from the
/// fuel-metered Engine, and no network.
#[tokio::test]
async fn a_structured_report_passes_the_real_engine_verifier_and_publishes() {
    let temporary = tempfile::tempdir().unwrap();
    let engine = Arc::new(Engine::initialize_at(temporary.path()).await.unwrap());
    let gateway = Arc::new(EngineGateway::new(engine.clone()));

    // A deterministic calculation the Engine performs offline. Its outputs become
    // calculation evidence, which is what a `calculated` number must cite.
    let program = json!({
        "version": 1,
        "inputs": {"close": [100.0, 110.0, 99.0, 105.0]},
        "bindings": [{"name": "ret", "expr": {"op": "returns", "input": {"op": "var", "name": "close"}}}],
        "outputs": {
            "mean_return": {"op": "mean", "input": {"op": "var", "name": "ret"}},
            "max_drawdown": {"op": "max_drawdown", "input": {"op": "var", "name": "close"}},
            "last_close": {"op": "last", "input": {"op": "var", "name": "close"}}
        }
    });
    let compute = gateway
        .execute(
            "research.compute",
            json!({"program": program}),
            CancellationToken::new(),
        )
        .await
        .expect("the bounded calculation runs offline");

    // Discover the identifiers the Engine actually registered, rather than
    // hard-coding digests that would drift with any payload change.
    let facts = compute["evidence_registry"]["facts"]
        .as_array()
        .expect("the calculation registers evidence");
    let find = |path_suffix: &str| -> (String, f64) {
        let fact = facts
            .iter()
            .find(|fact| {
                fact["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with(path_suffix))
                    && fact["value"].is_number()
            })
            .unwrap_or_else(|| panic!("a numeric fact at {path_suffix}: {facts:#?}"));
        (
            fact["evidence_id"].as_str().unwrap().to_owned(),
            fact["value"].as_f64().unwrap(),
        )
    };
    let (last_close_id, last_close) = find("/last_close/value");
    let (drawdown_id, drawdown) = find("/max_drawdown/value");

    // The verifier requires a minimum number of distinct citations under a strict
    // evidence requirement, so cite the calculation's own provenance too. Every one
    // of these is real evidence the calculation produced.
    let extra: Vec<String> = facts
        .iter()
        .filter_map(|fact| fact["evidence_id"].as_str().map(str::to_owned))
        .filter(|id| id != &last_close_id && id != &drawdown_id)
        .take(8)
        .collect();
    assert!(
        extra.len() >= 6,
        "the calculation should register enough provenance to cite: {facts:#?}"
    );

    let draft = json!({
        "version": "astock-report-contract-v1",
        "title": "确定性计算结果核验",
        "executive_summary": "本报告只陈述确定性计算得到的数值。",
        "sections": [{"heading": "计算结果", "claim_ids": ["c1", "c2", "c3"]}],
        "claims": [
            {
                "id": "c1",
                "kind": "deterministic_calculation",
                "statement": "序列最后一个收盘值",
                "numeric_items": [{
                    "label": "last_close", "value": last_close,
                    "provenance": "calculated",
                    "calculation_evidence_id": last_close_id,
                    "operation": "last",
                    "input_evidence_ids": [drawdown_id]
                }]
            },
            {
                "id": "c2",
                "kind": "deterministic_calculation",
                "statement": "序列最大回撤",
                "numeric_items": [{
                    "label": "max_drawdown", "value": drawdown,
                    "provenance": "calculated",
                    "calculation_evidence_id": drawdown_id,
                    "operation": "max_drawdown",
                    "input_evidence_ids": [last_close_id]
                }]
            },
            {
                "id": "c3",
                "kind": "unknown",
                "statement": "本次计算不构成投资判断，未覆盖行情、基本面与公告证据。",
                "evidence_ids": extra
            }
        ],
        "limitations": ["仅为确定性计算核验，不含市场数据。"]
    });

    let compute_round = vec![
        Ok(ModelChunk::ToolCallDelta {
            index: 0,
            id: Some("call-compute".into()),
            name: Some("run_financial_calculation".into()),
            arguments: Some(json!({"program": program}).to_string()),
        }),
        Ok(ModelChunk::Finished {
            reason: Some("tool_calls".into()),
        }),
    ];
    let runtime = AgentRuntime::new(
        Arc::new(ScriptedProvider::new(vec![
            compute_round,
            submit_round("call-submit", draft),
        ])),
        gateway.clone(),
        gateway,
    );
    let mut stream = runtime.start(RuntimeTask::ask("核验确定性计算能否通过独立校验"));
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    let findings: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::VerificationFinding { finding } => Some(finding.code.clone()),
            _ => None,
        })
        .collect();
    let outcome = stream.finish().await.unwrap_or_else(|error| {
        panic!("the real verifier refused: {error}; findings {findings:?}")
    });

    assert!(
        findings.is_empty(),
        "a release-quality report must have zero blocking findings, got {findings:?}"
    );
    assert!(outcome.report.contains("确定性计算结果核验"));
    assert!(
        !astock_agent_runtime::contains_internal_identifier(&outcome.report),
        "the published report leaked an internal identifier:\n{}",
        outcome.report
    );
    // The reader sees numbered citations with human labels.
    assert!(outcome.report.contains("[1]"));
    assert!(outcome.report.contains("确定性计算"));
}

// ---------------------------------------------------------------------------
// Empty provider turns
//
// A live simple-price task ended on `model returned neither visible text nor a
// tool call` after ten successful tool calls, losing the whole task. An empty
// turn commits nothing — no text reached the user, no tool call was assembled —
// so re-issuing the identical request cannot repeat an effect. Recovery is
// bounded, and it must not silently swallow a real provider fault.
// ---------------------------------------------------------------------------

/// An empty turn is replayed, and the task still publishes.
#[tokio::test]
async fn an_empty_provider_turn_is_replayed_and_the_task_still_publishes() {
    let engine = ScriptedEngine::new(vec![passing()]);
    let (events, result) = run(
        ScriptedProvider::new(vec![
            quote_round(),
            empty_round(),
            submit_round("call-good", valid_draft()),
        ]),
        engine.clone(),
    )
    .await;

    let outcome = result.expect("the replayed round publishes");
    assert!(outcome.report.contains("21.5"));
    let replays: Vec<&AgentEvent> = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ModelTurnEmpty { .. }))
        .collect();
    assert_eq!(replays.len(), 1, "one empty turn, one recorded replay");
    assert!(matches!(
        replays[0],
        AgentEvent::ModelTurnEmpty { attempt: 1, action, .. } if action == "replay"
    ));
    // The recovery cost no reasoning round: the same round was re-issued.
    let rounds: Vec<usize> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ModelStarted { round, .. } => Some(*round),
            _ => None,
        })
        .collect();
    assert_eq!(rounds, vec![1, 2], "an empty turn must not consume a round");
}

/// A stream that ends without any chunk at all is the same case.
#[tokio::test]
async fn a_stream_that_ends_with_no_chunks_is_recovered_as_an_empty_turn() {
    let engine = ScriptedEngine::new(vec![passing()]);
    let (events, result) = run(
        ScriptedProvider::new(vec![
            quote_round(),
            silent_round(),
            submit_round("call-good", valid_draft()),
        ]),
        engine,
    )
    .await;
    result.expect("the replayed round publishes");
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::ModelTurnEmpty { .. })));
}

/// A second empty turn is answered with an instruction, not just a replay.
#[tokio::test]
async fn a_repeated_empty_turn_is_answered_with_a_concrete_instruction() {
    let engine = ScriptedEngine::new(vec![passing()]);
    let (events, result) = run(
        ScriptedProvider::new(vec![
            quote_round(),
            empty_round(),
            empty_round(),
            submit_round("call-good", valid_draft()),
        ]),
        engine,
    )
    .await;
    result.expect("the second replay publishes");
    let actions: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ModelTurnEmpty { action, .. } => Some(action.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(actions, vec!["replay", "replay_with_instruction"]);
}

/// Replay is bounded: endless emptiness fails closed rather than looping.
#[tokio::test]
async fn endless_empty_turns_fail_closed() {
    let engine = ScriptedEngine::new(vec![]);
    let (events, result) = run_with_config(
        ScriptedProvider::new(vec![
            empty_round(),
            empty_round(),
            empty_round(),
            empty_round(),
        ]),
        engine,
        RuntimeConfig {
            max_empty_turn_retries: 2,
            ..RuntimeConfig::default()
        },
    )
    .await;
    assert!(
        matches!(result, Err(RuntimeError::EmptyModelTurn)),
        "an unrecoverable empty turn must fail closed, got {result:?}"
    );
    let actions: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ModelTurnEmpty { action, .. } => Some(action.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        actions,
        vec!["replay", "replay_with_instruction", "exhausted"],
        "the give-up point must be durable, not inferred from the error"
    );
}

/// A provider error frame is a fault, not an empty turn.
///
/// Retrying a real fault as if it were emptiness would hide it and waste the
/// replay budget on something a replay cannot fix.
#[tokio::test]
async fn a_provider_error_frame_is_not_treated_as_an_empty_turn() {
    let engine = ScriptedEngine::new(vec![]);
    let (events, result) = run(
        ScriptedProvider::new(vec![vec![Err(ProviderError::new(
            astock_agent_runtime::ProviderErrorKind::Authentication,
            "invalid credential",
            false,
        ))]]),
        engine,
    )
    .await;
    assert!(
        matches!(result, Err(RuntimeError::Provider(_))),
        "a provider fault must stay a provider fault, got {result:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ModelTurnEmpty { .. })),
        "a fault must not be recorded as an empty turn"
    );
}

// ---------------------------------------------------------------------------
// Malformed drafts
//
// A draft `serde` cannot decode used to bypass the finalization ledger, which
// made the repair loop unbounded: a live moderate run submitted 11 drafts
// against a budget of 6 and died on the model round limit instead of failing
// closed. Provenance shape is the usual cause — `provenance: "calculated"`
// without `calculation_evidence_id` fails to decode rather than validating — so
// this path is reached by exactly the drafts most in need of a bounded budget.
// ---------------------------------------------------------------------------

/// A draft with incomplete numeric provenance cannot be decoded.
fn undecodable_draft() -> Value {
    with_claims(
        json!([{
            "id": "c1",
            "kind": "deterministic_calculation",
            "statement": "计算结果",
            "numeric_items": [{
                "label": "市盈率", "value": 28.4,
                "provenance": "calculated"
            }]
        }]),
        json!(["c1"]),
    )
}

/// A malformed draft consumes the budget and eventually fails closed.
#[tokio::test]
async fn an_undecodable_draft_consumes_the_finalization_budget() {
    let engine = ScriptedEngine::new(vec![]);
    let (_, result) = run_with_config(
        ScriptedProvider::new(vec![
            quote_round(),
            submit_round("s1", undecodable_draft()),
            submit_round("s2", undecodable_draft()),
            submit_round("s3", undecodable_draft()),
            submit_round("s4", undecodable_draft()),
        ]),
        engine.clone(),
        RuntimeConfig {
            max_finalization_attempts: 2,
            ..RuntimeConfig::default()
        },
    )
    .await;
    match result {
        Err(RuntimeError::VerificationFailed(reason)) => {
            assert!(
                reason.contains("never matched the contract schema"),
                "the refusal must name the cause: {reason}"
            );
        }
        other => panic!("an unbounded decode loop must fail closed, got {other:?}"),
    }
    assert!(
        !engine
            .dispatched()
            .iter()
            .any(|kind| kind == "research.agent_report_verify"),
        "an undecodable draft never reaches the verifier"
    );
}

/// A malformed draft followed by a valid one still publishes.
#[tokio::test]
async fn a_repaired_draft_publishes_after_a_shape_error() {
    let engine = ScriptedEngine::new(vec![passing()]);
    let (events, result) = run(
        ScriptedProvider::new(vec![
            quote_round(),
            submit_round("s1", undecodable_draft()),
            submit_round("s2", valid_draft()),
        ]),
        engine,
    )
    .await;
    result.expect("the corrected draft publishes");
    assert!(matches!(events.last(), Some(AgentEvent::Completed { .. })));
}

// ---------------------------------------------------------------------------
// The research budget is a mechanism, not a request
//
// It used to be a system message asking the model to stop researching, and a
// model that kept going simply ignored it: two live moderate runs reached the
// 32-round ceiling having never submitted a report, one after 26 tool calls.
// Retrieval tools are now withdrawn once the budget is spent.
// ---------------------------------------------------------------------------

/// A retrieval tool disappears from the offered set once research is spent.
#[tokio::test]
async fn retrieval_tools_are_withdrawn_once_the_research_budget_is_spent() {
    let engine = ScriptedEngine::new(vec![passing()]);
    // Research budget of 1: round 1 may retrieve, round 2 onwards may not.
    let provider = ScriptedProvider::new(vec![
        quote_round(),
        submit_round("call-good", valid_draft()),
    ]);
    let offered = provider.offered_tool_names.clone();
    let runtime = AgentRuntime::new(Arc::new(provider), Arc::new(engine), Arc::new(NullStore))
        .with_config(RuntimeConfig {
            max_research_rounds: 1,
            ..RuntimeConfig::default()
        });
    let mut task = RuntimeTask::ask("紫金矿业最新价格是多少？");
    task.symbol = Some("601899".into());
    let mut stream = runtime.start(task);
    while stream.recv().await.is_some() {}
    stream.finish().await.expect("the task publishes");

    let rounds = offered.lock().unwrap().clone();
    assert_eq!(rounds.len(), 2, "two model rounds were requested");
    assert!(
        rounds[0].iter().any(|name| name == "get_quote"),
        "the research round must offer retrieval: {:?}",
        rounds[0]
    );
    assert!(
        !rounds[1].iter().any(|name| name == "get_quote"),
        "the finalization round must not offer retrieval: {:?}",
        rounds[1]
    );
    // Finalization still needs identifiers and deterministic arithmetic.
    for required in [
        "search_evidence",
        "submit_report",
        "run_financial_calculation",
    ] {
        assert!(
            rounds[1].iter().any(|name| name == required),
            "`{required}` must remain available during finalization: {:?}",
            rounds[1]
        );
    }
}

// ---------------------------------------------------------------------------
// A suspension records when it may resume
//
// It used to carry prose only, so nothing knew when to try again and a user had to
// restart deep research by hand once a quota window reopened. A live run suspended
// with 123 minutes to go and no record of it.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct FaultingProvider {
    error: ProviderError,
}

#[async_trait]
impl ModelProvider for FaultingProvider {
    fn name(&self) -> &'static str {
        "faulting"
    }

    async fn selected_model(&self) -> Result<String, ProviderError> {
        Ok("faulting-model".into())
    }

    async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ProviderError> {
        Err(self.error.clone())
    }
}

async fn run_faulting(error: ProviderError) -> Vec<AgentEvent> {
    let runtime = AgentRuntime::new(
        Arc::new(FaultingProvider { error }),
        Arc::new(ScriptedEngine::new(vec![])),
        Arc::new(NullStore),
    );
    let mut stream = runtime.start(RuntimeTask::ask("紫金矿业最新价格是多少？"));
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    let _ = stream.finish().await;
    events
}

/// A long rate limit suspends with the resume time recorded.
#[tokio::test]
async fn a_long_rate_limit_suspends_with_a_recorded_resume_time() {
    let mut error = ProviderError::new(
        astock_agent_runtime::ProviderErrorKind::RateLimited,
        "MiniMax rate limit reached",
        true,
    );
    error.retry_after = Some(std::time::Duration::from_secs(123 * 60));
    let events = run_faulting(error).await;
    let suspended = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::Suspended {
                resume_at, fault, ..
            } => Some((resume_at.clone(), fault.clone())),
            _ => None,
        })
        .expect("a rate limit suspends the task");
    let (resume_at, fault) = suspended;
    assert_eq!(fault.as_deref(), Some("rate_limited"));
    let resume_at = resume_at.expect("the resume time is recorded, not just the reason");
    let parsed = chrono::DateTime::parse_from_rfc3339(&resume_at).expect("an RFC3339 instant");
    let minutes = (parsed.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_minutes();
    assert!(
        (120..=124).contains(&minutes),
        "the resume time must reflect the provider's guidance, got {minutes} minutes"
    );
}

/// Quota exhaustion with no reported reset suspends without inventing a time.
#[tokio::test]
async fn quota_exhaustion_without_guidance_suspends_without_a_fabricated_time() {
    let error = ProviderError::new(
        astock_agent_runtime::ProviderErrorKind::Quota,
        "MiniMax quota exhausted",
        false,
    );
    let events = run_faulting(error).await;
    let (resume_at, fault) = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::Suspended {
                resume_at, fault, ..
            } => Some((resume_at.clone(), fault.clone())),
            _ => None,
        })
        .expect("quota exhaustion suspends the task");
    assert_eq!(fault.as_deref(), Some("quota_exhausted"));
    assert!(
        resume_at.is_none(),
        "a reset time the provider never reported must not be invented"
    );
}

/// A credential failure is not a suspension: only the operator can clear it.
#[tokio::test]
async fn an_authentication_failure_is_terminal_rather_than_suspended() {
    let error = ProviderError::new(
        astock_agent_runtime::ProviderErrorKind::Authentication,
        "invalid credential",
        false,
    );
    let events = run_faulting(error).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Suspended { .. })),
        "a credential failure must not look like a transient window"
    );
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::Failed { .. })));
}

// ---------------------------------------------------------------------------
// Batched calculation
//
// The calculation tool always accepted a program with many named outputs and the
// model did not use it: measured live it issued 12, 13 and 24 separate calculation
// calls in three balanced tasks, one per figure, at one model round each. Asking it
// to batch in prose changed nothing, so the batch is now in the schema.
// ---------------------------------------------------------------------------

/// A batch of programs is one Engine dispatch per program, one tool result.
#[tokio::test]
async fn a_batch_of_calculation_programs_runs_in_one_tool_call() {
    #[derive(Clone)]
    struct CountingEngine {
        calls: Arc<Mutex<Vec<Value>>>,
    }
    #[async_trait]
    impl ToolExecutor for CountingEngine {
        async fn execute(
            &self,
            engine_kind: &str,
            payload: Value,
            _cancellation: CancellationToken,
        ) -> Result<Value, String> {
            if engine_kind == "research.agent_report_verify" {
                return Ok(passing());
            }
            self.calls.lock().unwrap().push(payload.clone());
            Ok(json!({"execution": {"outputs": {"x": {"value": 1.0}}}}))
        }
    }

    let calls = Arc::new(Mutex::new(Vec::new()));
    let engine = CountingEngine {
        calls: calls.clone(),
    };
    let program = json!({
        "version": 1,
        "outputs": {"x": {"op": "scalar", "value": 1.0}}
    });
    let batch = vec![
        Ok(ModelChunk::ToolCallDelta {
            index: 0,
            id: Some("call-calc".into()),
            name: Some("run_financial_calculation".into()),
            arguments: Some(
                json!({"programs": [program.clone(), program.clone(), program]}).to_string(),
            ),
        }),
        Ok(ModelChunk::Finished {
            reason: Some("tool_calls".into()),
        }),
    ];
    let runtime = AgentRuntime::new(
        Arc::new(ScriptedProvider::new(vec![batch, prose_round("完成。")])),
        Arc::new(engine),
        Arc::new(NullStore),
    )
    .with_config(RuntimeConfig {
        max_finalization_attempts: 1,
        ..RuntimeConfig::default()
    });
    let mut stream = runtime.start(RuntimeTask::ask("批量计算"));
    let mut completions = 0usize;
    while let Some(event) = stream.recv().await {
        if let AgentEvent::ToolCompleted { tool, .. } = &event {
            if tool == "run_financial_calculation" {
                completions += 1;
            }
        }
    }
    let _ = stream.finish().await;

    // Three programs, three Engine dispatches, but exactly one tool call and one
    // tool result — which is the point: three figures cost one model round.
    assert_eq!(calls.lock().unwrap().len(), 3, "each program is dispatched");
    assert_eq!(completions, 1, "the batch is a single tool call");
    for payload in calls.lock().unwrap().iter() {
        assert!(
            payload.get("program").is_some() && payload.get("programs").is_none(),
            "each dispatch carries one program: {payload}"
        );
    }
}

/// An oversized batch is refused rather than dispatched.
#[tokio::test]
async fn an_oversized_calculation_batch_is_refused() {
    let program = json!({"version": 1, "outputs": {"x": {"op": "scalar", "value": 1.0}}});
    let programs: Vec<Value> = (0..40).map(|_| program.clone()).collect();
    let batch = vec![
        Ok(ModelChunk::ToolCallDelta {
            index: 0,
            id: Some("call-calc".into()),
            name: Some("run_financial_calculation".into()),
            arguments: Some(json!({"programs": programs}).to_string()),
        }),
        Ok(ModelChunk::Finished {
            reason: Some("tool_calls".into()),
        }),
    ];
    let runtime = AgentRuntime::new(
        Arc::new(ScriptedProvider::new(vec![batch, prose_round("完成。")])),
        Arc::new(ScriptedEngine::new(vec![])),
        Arc::new(NullStore),
    )
    .with_config(RuntimeConfig {
        max_finalization_attempts: 1,
        ..RuntimeConfig::default()
    });
    let mut stream = runtime.start(RuntimeTask::ask("批量计算"));
    let mut refusal = None;
    while let Some(event) = stream.recv().await {
        if let AgentEvent::ToolFailed { tool, message, .. } = &event {
            if tool == "run_financial_calculation" {
                refusal = Some(message.clone());
            }
        }
    }
    let _ = stream.finish().await;
    let refusal = refusal.expect("an oversized batch is refused");
    assert!(refusal.contains("at most"), "{refusal}");
}

/// Calculation tools are withdrawn after consecutive shape failures.
///
/// A budget the model can decline is not a budget: live Case C runs burned 16
/// rounds on malformed ASTs after coverage was already complete. Three shape
/// failures is enough to deliver the worked example twice; on the fourth round
/// the tools are gone and the model must finalize with what it has.
#[tokio::test]
async fn calculation_tools_are_withdrawn_after_consecutive_shape_failures() {
    fn calc_round(id: &str) -> Turn {
        vec![
            Ok(ModelChunk::ToolCallDelta {
                index: 0,
                id: Some(id.to_owned()),
                name: Some("run_financial_calculation".into()),
                arguments: Some(
                    json!({"program": {"version": 1, "outputs": {"x": {"op": "scalar", "value": 1}}}})
                        .to_string(),
                ),
            }),
            Ok(ModelChunk::Finished {
                reason: Some("tool_calls".into()),
            }),
        ]
    }

    #[derive(Clone)]
    struct CountingFailEngine {
        compute_calls: Arc<Mutex<usize>>,
    }
    #[async_trait]
    impl ToolExecutor for CountingFailEngine {
        async fn execute(
            &self,
            engine_kind: &str,
            _payload: Value,
            _cancellation: CancellationToken,
        ) -> Result<Value, String> {
            match engine_kind {
                "market.quote" => Ok(quote_result()),
                "research.compute" => {
                    *self.compute_calls.lock().unwrap() += 1;
                    Err(
                        "invalid_payload: invalid request payload: program.bindings[0].expr:                          invalid type: string \"2.0\", expected struct"
                            .into(),
                    )
                }
                "research.agent_report_verify" => Ok(passing()),
                other => Err(format!("unexpected Engine operation: {other}")),
            }
        }
    }

    let compute_calls = Arc::new(Mutex::new(0usize));
    let provider = ScriptedProvider::new(vec![
        calc_round("c1"),
        calc_round("c2"),
        calc_round("c3"),
        // Provider may still forward a calc call even after the offer omits it.
        calc_round("c4-should-be-refused"),
        submit_round("s1", valid_draft()),
    ]);
    let offered = provider.offered_tool_names.clone();
    let runtime = AgentRuntime::new(
        Arc::new(provider),
        Arc::new(CountingFailEngine {
            compute_calls: compute_calls.clone(),
        }),
        Arc::new(NullStore),
    );
    let mut task = RuntimeTask::ask("紫金矿业估值");
    task.symbol = Some("601899".into());
    let mut stream = runtime.start(task);
    while stream.recv().await.is_some() {}
    let _ = stream.finish().await;

    let rounds = offered.lock().unwrap().clone();
    assert!(
        rounds.len() >= 4,
        "expected at least four model rounds, got {}",
        rounds.len()
    );
    assert!(
        rounds[0]
            .iter()
            .any(|name| name == "run_financial_calculation"),
        "first round offers calculation: {:?}",
        rounds[0]
    );
    assert!(
        !rounds[3]
            .iter()
            .any(|name| name == "run_financial_calculation"),
        "after three shape failures calculation must be withdrawn: {:?}",
        rounds[3]
    );
    assert_eq!(
        *compute_calls.lock().unwrap(),
        3,
        "a calc call after withdrawal must not reach the Engine"
    );
}
