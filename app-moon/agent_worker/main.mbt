///|
/// Wire request accepted from the supervising Proton host.
struct RequestEnvelope {
  protocol_version : Int
  request_id : String
  kind : String
  payload : Json
  deadline_ms : Int?
  cancellation_id : String?
} derive(FromJson)

///|
struct StartPayload {
  task_id : String
  seq : Int
  spec : @protocol.TaskSpec
} derive(FromJson)

///|
/// Durable pure-kernel checkpoint loaded from the Engine event store after
/// a Worker or desktop restart. No sockets, secrets or provider handles are
/// present in this value.
struct RestorePayload {
  state : @agent.AgentState
} derive(FromJson)

///|
/// Renderer supplies only user-selected execution preferences. Tool order,
/// candidate planning and evidence policy remain inside the Agent Worker.
struct ResearchWorkflowPayload {
  task_id : String
  depth : String
  tool_policy : String
  preferred_symbols : Array[String]
  capital : Double?
} derive(FromJson)

///|
struct WorkflowToolResult {
  call_id : String
  ok : Bool
  payload : Json
  error : String?
  cache_hit : Bool
} derive(FromJson)

///|
struct ResearchWorkflowState {
  task_id : String
  stage : String
  depth : String
  tool_policy : String
  preferred_symbols : Array[String]
  capital : Double?
  prepare_context : Json?
  research_plan : ResearchPlan?
  report : String?
  activities : Array[Json]
} derive(FromJson, ToJson)

///|
extend ResearchWorkflowState with ToJson::{to_json}

///|
struct ResearchWorkflowContinuePayload {
  workflow : ResearchWorkflowState
  tool_results : Array[WorkflowToolResult]
} derive(FromJson)

///|
struct CandidatePage {
  items : Array[ResearchCandidate]
} derive(FromJson)

///|
struct AgentPrepareContext {
  market_overview : Json
  market_context : Json
  global_context : Json
  market_news : Json
  candidates : CandidatePage
  evidence_registry : Json
} derive(FromJson)

///|
struct AgentSecurityContext {
  securities : Array[Json]
  evidence_registry : Json
  analysis_modules : Array[String]?
  cross_security_analysis : Json?
  tool_activities : Array[Json]?
} derive(FromJson)

///|
struct AgentReportVerification {
  passed : Bool
  findings : Array[String]
  distinct_citations : Int
  numeric_claims_checked : Int
  registry_facts : Int
  verification_version : String
} derive(FromJson)

///|
struct ModelRoutingPayload {
  routing : ModelRouting
  validate : Bool?
} derive(FromJson)

///|
struct EventPayload {
  task_id : String
  seq : Int
  event_kind : String
  spec : @protocol.TaskSpec?
  call_id : String?
  tool : String?
  evidence_ids : Array[String]?
  errors : Array[String]?
  retryable : Bool?
  reason : String?
  clarification_response : ClarificationResponse?
} derive(FromJson)

///|
struct ClarificationResponse {
  title : String
  answers : Array[@protocol.ClarificationAnswer]
} derive(FromJson)

///|
fn error_payload(code : String, message : String, retryable : Bool) -> Json {
  { "code": code, "message": message, "retryable": retryable }
}

///|
fn response(
  request : RequestEnvelope,
  ok : Bool,
  payload : Json,
  error? : Json = null,
) -> Json {
  {
    "protocol_version": @protocol.protocol_version,
    "request_id": request.request_id,
    "kind": request.kind,
    "ok": ok,
    "payload": payload,
    "error": error,
  }
}

///|
fn state_json(state : @agent.AgentState) -> Json {
  {
    "task_id": state.task_id,
    "spec": match state.spec {
      Some(spec) => spec.to_json()
      None => null
    },
    "phase": state.phase_name(),
    "accepted_seq": state.accepted_seq,
    "model_rounds": state.model_rounds,
    "retry_count": state.retry_count,
    "missing_fields": state.missing_fields,
    "clarification": match state.clarification {
      Some(request) => request.to_json()
      None => null
    },
    "clarification_answers": state.clarification_answers.map(fn(answer) {
      answer.to_json()
    }),
    "pending_tool_count": state.pending_tools.length(),
    "completed_tool_count": state.completed_call_ids.length(),
    "reconciled_call_count": state.reconciled_call_ids.length(),
    "research_symbols": state.research_symbols,
    "evidence_ids": state.evidence_ids,
    "blocking_verification_errors": state.blocking_verification_errors,
    "terminal_reason": match state.terminal_reason {
      Some(reason) => reason.to_json()
      None => null
    },
  }
}

///|
fn effect_json(effect : @agent.AgentEffect) -> Json {
  match effect {
    PersistCheckpoint => { "kind": "persist_checkpoint" }
    AskForClarification(fields) =>
      { "kind": "ask_for_clarification", "fields": fields }
    PrepareResearch => { "kind": "prepare_research" }
    RequestModel => { "kind": "request_model" }
    ExecuteTool(call_id~, tool~) =>
      { "kind": "execute_tool", "call_id": call_id, "tool": tool }
    ScheduleRetry(call_id~, attempt~) =>
      { "kind": "schedule_retry", "call_id": call_id, "attempt": attempt }
    ReviewEvidence => { "kind": "review_evidence" }
    SynthesizeReport => { "kind": "synthesize_report" }
    VerifyReport => { "kind": "verify_report" }
    PublishReport => { "kind": "publish_report" }
    EmitTerminal => { "kind": "emit_terminal" }
  }
}

///|
fn transition_json(transition : @agent.Transition) -> Json {
  {
    "accepted": transition.accepted,
    "rejection": match transition.rejection {
      Some(reason) => reason.to_json()
      None => null
    },
    "state": state_json(transition.state),
    "checkpoint": transition.state.to_json(),
    "effects": transition.effects.map(effect_json),
  }
}

///|
fn required(value : String?, name : String) -> String raise {
  match value {
    Some(value) => value
    None => fail("missing event field: " + name)
  }
}

///|
fn parse_event(payload : EventPayload) -> @agent.AgentEvent raise {
  match payload.event_kind {
    "clarified" => Clarified(payload.spec.unwrap())
    "prepared" => Prepared
    "model_round_started" => ModelRoundStarted
    "tool_requested" =>
      ToolRequested(
        call_id=required(payload.call_id, "call_id"),
        tool=required(payload.tool, "tool"),
      )
    "tool_succeeded" =>
      ToolSucceeded(
        call_id=required(payload.call_id, "call_id"),
        evidence_ids=payload.evidence_ids.unwrap_or([]),
      )
    "tool_failed" =>
      ToolFailed(
        call_id=required(payload.call_id, "call_id"),
        retryable=payload.retryable.unwrap_or(false),
      )
    "reconcile_interrupted_workflow" => ReconcileInterruptedWorkflow
    "review_ready" => ReviewReady
    "synthesis_ready" => SynthesisReady
    "verification_passed" => VerificationPassed
    "verification_rejected" =>
      VerificationRejected(
        payload.errors.unwrap_or(["unspecified_verification_error"]),
      )
    "quota_suspended" => QuotaSuspended
    "provider_suspended" =>
      ProviderSuspended(payload.reason.unwrap_or("provider_unavailable"))
    "resume" => Resume
    "cancel" => Cancel
    "hard_failure" =>
      HardFailure(payload.reason.unwrap_or("unspecified_failure"))
    other => fail("unknown Agent event: " + other)
  }
}

///|
/// Worker imperative shell. Durable event/checkpoint writes are requested as
/// effects and must be committed by the Host/Engine before later effects run.
struct Worker {
  tasks : Map[String, @agent.AgentState]
  seen_request_ids : Map[String, Bool]
  request_order : Array[String]
  mut provider : ProviderConfig?
  mut model_routing : ModelRouting
}

///|
fn Worker::new() -> Worker {
  {
    tasks: Map([], capacity=32),
    seen_request_ids: Map([], capacity=4096),
    request_order: [],
    provider: None,
    model_routing: default_model_routing(),
  }
}

///|
fn transition_state(
  state : @agent.AgentState,
  event : @agent.AgentEvent,
) -> @agent.AgentState raise {
  let transition = @agent.reduce(state, state.accepted_seq + 1, event)
  if !transition.accepted {
    fail(
      "agent_transition_rejected:" + transition.rejection.unwrap_or("unknown"),
    )
  }
  transition.state
}

///|
fn valid_security_symbol(symbol : String) -> Bool {
  symbol.length() == 6 && symbol.all(fn(char) { char >= '0' && char <= '9' })
}

///|
fn bounded_research_symbols(
  state : @agent.AgentState,
  preferred : Array[String],
  maximum : Int,
) -> Array[String] {
  let symbols : Array[String] = []
  for
    symbol in [
      ..state.research_symbols,
      ..preferred,
      ..state.spec.unwrap().security_universe,
    ] {
    if valid_security_symbol(symbol) &&
      !symbols.contains(symbol) &&
      symbols.length() < maximum {
      symbols.push(symbol)
    }
  }
  symbols
}

///|
fn host_engine_effect(
  task_id : String,
  caused_by_seq : Int,
  call_id : String,
  kind : String,
  payload : Json,
  deadline_ms : Int,
) -> Json {
  {
    "task_id": task_id,
    "caused_by_seq": caused_by_seq,
    "call_id": call_id,
    "target": "engine",
    "kind": kind,
    "payload": payload,
    "deadline_ms": deadline_ms,
    // The reducer call id is unique for sequence safety while cache identity
    // is stable across crash reconciliation and includes every tool argument.
    "idempotency_key": task_id + ":tool:" + kind + ":" + payload.stringify(),
  }
}

///|
fn explicit_research_plan(
  symbols : Array[String],
  tool_policy : String,
) -> ResearchPlan raise {
  ResearchPlan::{
    symbols: symbols.copy(),
    analysis_modules: Some(analysis_modules_for_policy(tool_policy, [])),
    selection_summary: "优先研究用户明确指定或上次已持久化的证券；最终报告仍可拒绝全部候选并保留现金",
    verification_focus: [
      "逐项核对行情、财务、资金、公告、新闻与估值证据", "重大跨源冲突会阻止发布",
    ],
    rejected_risks: ["未完成详细取证前不形成买卖结论"],
  }
}

///|
fn provider_failure_reason(detail : String) -> String {
  if detail.contains("429") ||
    detail.contains("quota") ||
    detail.contains("rate_limit") {
    "provider_quota_or_rate_limit"
  } else if detail.contains("401") ||
    detail.contains("403") ||
    detail.contains("credential") {
    "provider_credential_rejected"
  } else {
    "provider_unavailable"
  }
}

///|
fn Worker::workflow_provider_suspended(
  self : Worker,
  request : RequestEnvelope,
  current : @agent.AgentState,
  detail : String,
) -> Json raise {
  let reason = provider_failure_reason(detail)
  let suspended = transition_state(current, ProviderSuspended(reason))
  self.tasks[suspended.task_id] = suspended
  response(request, true, {
    "accepted": true,
    "state": state_json(suspended),
    "checkpoint": suspended.to_json(),
    "effects": [@agent.PersistCheckpoint].map(effect_json),
    "report": null,
    "verification_findings": [
      if reason == "provider_quota_or_rate_limit" {
        "MiniMax Plus 额度或速率限制暂不可用；已保存工具结果，额度恢复后可继续"
      } else if reason == "provider_credential_rejected" {
        "MiniMax Plus 凭据缺失或被拒绝；请安全更新凭据后继续"
      } else {
        "MiniMax Plus 暂不可用；已保存工具结果，可在服务恢复后继续"
      },
    ],
    "activities": [
      {
        "kind": "provider_suspended",
        "detail": "Provider暂停不会发布不完整报告；恢复时会按参数命中已验证工具缓存",
      },
    ],
  })
}

///|
fn Worker::workflow_tool_failure(
  self : Worker,
  request : RequestEnvelope,
  current : @agent.AgentState,
  result : WorkflowToolResult,
) -> Json raise {
  let failed = transition_state(
    current,
    ToolFailed(call_id=result.call_id, retryable=false),
  )
  self.tasks[failed.task_id] = failed
  response(request, true, {
    "accepted": true,
    "state": state_json(failed),
    "checkpoint": failed.to_json(),
    "effects": [@agent.PersistCheckpoint, @agent.EmitTerminal].map(effect_json),
    "report": null,
    "verification_findings": [
      result.error.unwrap_or("Engine research effect failed"),
    ],
    "activities": [
      {
        "kind": "execute_tool",
        "tool": result.call_id,
        "call_id": result.call_id,
        "cache_hit": result.cache_hit,
        "detail": "研究工具失败，任务已安全停止且没有发布结论",
      },
    ],
  })
}

///|
async fn Worker::provider_config(self : Worker, key : String) -> ProviderConfig {
  match self.provider {
    Some(provider) => provider
    None => {
      let provider = discover_provider(key).with_routing(self.model_routing)
      self.provider = Some(provider)
      provider
    }
  }
}

///|
async fn Worker::handle(self : Worker, request : RequestEnvelope) -> Json {
  // Deadlines and cancellation are enforced by the supervisor before a
  // request reaches this synchronous pure-kernel dispatch.
  ignore(request.deadline_ms)
  ignore(request.cancellation_id)
  if request.request_id.trim().is_empty() {
    return response(
      request,
      false,
      null,
      error=error_payload(
        "invalid_request_id", "request_id must not be empty", false,
      ),
    )
  }
  if self.seen_request_ids.contains(request.request_id) {
    return response(
      request,
      false,
      null,
      error=error_payload(
        "duplicate_request_id", "request_id was already observed on this Worker channel",
        false,
      ),
    )
  }
  self.seen_request_ids[request.request_id] = true
  self.request_order.push(request.request_id)
  if self.request_order.length() > 4096 {
    let expired = self.request_order.remove(0)
    self.seen_request_ids.remove(expired)
  }
  if request.protocol_version != @protocol.protocol_version {
    return response(
      request,
      false,
      null,
      error=error_payload(
        "protocol_version_mismatch", "Agent worker requires protocol version 1",
        false,
      ),
    )
  }
  match request.kind {
    "system.handshake" =>
      response(request, true, {
        "protocol_version": @protocol.protocol_version,
        "agent_version": "6.0.0",
        "reducer_version": "moonbit-agent-kernel-v1",
        "capabilities": [
          "pure_reducer", "replay", "evidence_gate", "provider_discovery", "candidate_planning",
          "advanced_tool_planning", "closed_engine_effects", "multi_pass_review",
          "deterministic_report_verification", "sse_stream_recovery",
        ],
        "max_frame_bytes": @protocol.max_frame_bytes,
      })
    "diagnostics.status" =>
      response(request, true, {
        "status": "ready",
        "task_count": self.tasks.length(),
        "reducer_version": "moonbit-agent-kernel-v1",
      })
    "agent.provider.test" => {
      let key = read_minimax_key()
      if key.trim().is_empty() {
        response(
          request,
          false,
          null,
          error=error_payload(
            "provider_credential_missing", "未配置 MiniMax Plus API Key，请先在配置页安全保存",
            false,
          ),
        )
      } else {
        // Force catalog discovery so replacing a key or changing its Plus
        // entitlement takes effect without restarting the desktop process.
        let provider = discover_provider(key).with_routing(self.model_routing)
        self.provider = Some(provider)
        response(request, true, {
          "provider": "minimax_plus",
          "model": provider.model,
          "fast_model": provider.fast_model,
          "deep_model": provider.deep_model,
          "verifier_model": provider.verifier_model,
          "available_models": provider.available_models,
          "multi_agent_enabled": provider.multi_agent_enabled,
          "max_parallel_agents": provider.max_parallel_agents,
          "api_host": provider.api_host,
          "catalog_verified": true,
        })
      }
    }
    "agent.provider.configure" => {
      let payload : ModelRoutingPayload = @json.from_json(request.payload)
      let routing = payload.routing
      if routing.max_parallel_agents < 1 || routing.max_parallel_agents > 4 {
        response(
          request,
          false,
          null,
          error=error_payload(
            "invalid_model_routing", "并行复核数量必须在1到4之间", false,
          ),
        )
      } else {
        self.model_routing = routing
        self.provider = None
        let key = read_minimax_key()
        if !payload.validate.unwrap_or(false) || key.trim().is_empty() {
          response(request, true, {
            "stored": true,
            "catalog_verified": false,
            "routing": routing.to_json(),
          })
        } else {
          let provider = self.provider_config(key)
          response(request, true, {
            "stored": true,
            "catalog_verified": true,
            "model": provider.model,
            "fast_model": provider.fast_model,
            "deep_model": provider.deep_model,
            "verifier_model": provider.verifier_model,
            "available_models": provider.available_models,
            "multi_agent_enabled": provider.multi_agent_enabled,
            "max_parallel_agents": provider.max_parallel_agents,
          })
        }
      }
    }
    "agent.start" => {
      let payload : StartPayload = @json.from_json(request.payload)
      let current = self.tasks
        .get(payload.task_id)
        .unwrap_or(@agent.initial(payload.task_id))
      let transition = @agent.reduce(current, payload.seq, Start(payload.spec))
      if transition.accepted {
        self.tasks[payload.task_id] = transition.state
      }
      if transition.accepted && transition.state.phase == WaitingForUser {
        let key = read_minimax_key()
        if key.trim().is_empty() {
          return response(
            request,
            false,
            null,
            error=error_payload(
              "provider_credential_missing", "未配置 MiniMax Plus API Key，请先在配置页保存密钥",
              false,
            ),
          )
        }
        let provider = self.provider_config(key)
        let clarification = generate_clarification(
          provider,
          key,
          payload.spec,
          transition.state.missing_fields,
        )
        let generated = @agent.reduce(
          transition.state,
          transition.state.accepted_seq + 1,
          ClarificationGenerated(clarification),
        )
        if generated.accepted {
          self.tasks[payload.task_id] = generated.state
        }
        response(request, true, transition_json(generated))
      } else {
        response(request, true, transition_json(transition))
      }
    }
    "agent.restore" => {
      let payload : RestorePayload = @json.from_json(request.payload)
      self.tasks[payload.state.task_id] = payload.state
      response(request, true, {
        "restored": true,
        "state": state_json(payload.state),
        "checkpoint": payload.state.to_json(),
      })
    }
    "agent.event" => {
      let payload : EventPayload = @json.from_json(request.payload)
      match self.tasks.get(payload.task_id) {
        None =>
          response(
            request,
            false,
            null,
            error=error_payload(
              "task_not_loaded", "Agent task is not loaded", true,
            ),
          )
        Some(current) => {
          let event = if payload.event_kind == "clarification_answered" {
            let response_payload = payload.clarification_response.unwrap()
            ignore(response_payload.title)
            let clarification = current.clarification.unwrap()
            let spec = current.spec.unwrap()
            let key = read_minimax_key()
            if key.trim().is_empty() {
              return response(
                request,
                false,
                null,
                error=error_payload(
                  "provider_credential_missing", "MiniMax Plus API Key 不可用，任务仍保留在等待状态",
                  false,
                ),
              )
            }
            let provider = self.provider_config(key)
            let resolved = resolve_clarification(
              provider,
              key,
              spec,
              clarification,
              response_payload.answers,
            )
            @agent.ClarificationAnswered(resolved, response_payload.answers)
          } else {
            parse_event(payload)
          }
          let transition = @agent.reduce(current, payload.seq, event)
          if transition.accepted {
            self.tasks[payload.task_id] = transition.state
          }
          response(request, true, transition_json(transition))
        }
      }
    }
    "agent.research.workflow" => {
      let payload : ResearchWorkflowPayload = @json.from_json(request.payload)
      match self.tasks.get(payload.task_id) {
        None =>
          response(
            request,
            false,
            null,
            error=error_payload(
              "task_not_loaded", "Agent task is not loaded", true,
            ),
          )
        Some(current) =>
          if current.phase != Preparing &&
            current.phase != Suspended &&
            current.phase != AwaitingTools {
            response(
              request,
              false,
              null,
              error=error_payload(
                "task_not_ready", "研究任务不在可启动或可恢复状态",
                false,
              ),
            )
          } else {
            let mut state = match current.phase {
              Preparing => transition_state(current, Prepared)
              Suspended => transition_state(current, Resume)
              AwaitingTools =>
                transition_state(current, ReconcileInterruptedWorkflow)
              _ => current
            }
            let call_id = "prepare-context-" +
              payload.task_id +
              "-" +
              state.accepted_seq.to_string()
            state = transition_state(
              state,
              ToolRequested(call_id~, tool="research.agent_prepare_context"),
            )
            self.tasks[payload.task_id] = state
            let activities : Array[Json] = [
              {
                "kind": "prepare_research",
                "detail": "Agent已根据任务边界安排市场、宏观、资讯和候选池取证",
              },
            ]
            let workflow = ResearchWorkflowState::{
              task_id: payload.task_id,
              stage: "prepared",
              depth: payload.depth,
              tool_policy: payload.tool_policy,
              preferred_symbols: payload.preferred_symbols,
              capital: payload.capital,
              prepare_context: None,
              research_plan: None,
              report: None,
              activities,
            }
            response(request, true, {
              "accepted": true,
              "state": state_json(state),
              "checkpoint": state.to_json(),
              "effects": [
                @agent.PersistCheckpoint,
                @agent.ExecuteTool(
                  call_id~,
                  tool="research.agent_prepare_context",
                ),
              ].map(effect_json),
              "activities": activities,
              "host_effects": [
                host_engine_effect(
                  payload.task_id,
                  state.accepted_seq,
                  call_id,
                  "research.agent_prepare_context",
                  {
                    "depth": payload.depth,
                    "capital": match payload.capital {
                      Some(value) => value.to_json()
                      None => null
                    },
                  },
                  300000,
                ),
              ],
              "continuation": {
                "kind": "agent.research.workflow.continue",
                "workflow": workflow.to_json(),
              },
            })
          }
      }
    }
    "agent.research.workflow.continue" => {
      let payload : ResearchWorkflowContinuePayload = @json.from_json(
        request.payload,
      )
      match self.tasks.get(payload.workflow.task_id) {
        None =>
          response(
            request,
            false,
            null,
            error=error_payload(
              "task_not_loaded", "Agent task is not loaded during workflow continuation",
              true,
            ),
          )
        Some(current) => {
          guard payload.tool_results is [result] else {
            return response(
              request,
              false,
              null,
              error=error_payload(
                "invalid_tool_result_count", "Agent workflow expected exactly one bounded Engine result",
                false,
              ),
            )
          }
          if !result.ok {
            return self.workflow_tool_failure(request, current, result)
          }
          if payload.workflow.stage == "prepared" {
            let prepared : AgentPrepareContext = @json.from_json(result.payload)
            let maximum = match payload.workflow.capital {
              Some(capital) if capital <= 50000.0 => 3
              _ => 5
            }
            let mut state = transition_state(
              current,
              ToolSucceeded(call_id=result.call_id, evidence_ids=[
                "engine-market-context-" + current.task_id,
              ]),
            )
            let mut symbols = bounded_research_symbols(
              state,
              payload.workflow.preferred_symbols,
              maximum,
            )
            state = transition_state(state, MoreEvidenceRequired)
            let plan = if symbols.is_empty() ||
              payload.workflow.tool_policy == "auto" {
              let key = read_minimax_key()
              if key.trim().is_empty() {
                return self.workflow_provider_suspended(
                  request, state, "provider_credential_missing",
                )
              }
              state = transition_state(state, ModelRoundStarted)
              let provider = self.provider_config(key) catch {
                error =>
                  return self.workflow_provider_suspended(
                    request,
                    state,
                    error.to_string(),
                  )
              }
              let selected : ResearchPlan = if symbols.is_empty() {
                generate_research_plan(
                  provider,
                  key,
                  state.spec.unwrap(),
                  prepared.candidates.items,
                  prepared.market_overview,
                  payload.workflow.tool_policy,
                ) catch {
                  error =>
                    return self.workflow_provider_suspended(
                      request,
                      state,
                      error.to_string(),
                    )
                }
              } else {
                generate_explicit_research_plan(
                  provider,
                  key,
                  state.spec.unwrap(),
                  symbols,
                  prepared.market_overview,
                  payload.workflow.tool_policy,
                ) catch {
                  error =>
                    return self.workflow_provider_suspended(
                      request,
                      state,
                      error.to_string(),
                    )
                }
              }
              symbols = selected.symbols.copy()
              selected
            } else {
              explicit_research_plan(symbols, payload.workflow.tool_policy)
            }
            state = transition_state(state, ResearchPlanSelected(symbols))
            let call_id = "security-context-" +
              payload.workflow.task_id +
              "-" +
              state.accepted_seq.to_string()
            state = transition_state(
              state,
              ToolRequested(call_id~, tool="research.agent_security_context"),
            )
            self.tasks[payload.workflow.task_id] = state
            let spec = state.spec.unwrap()
            let activities : Array[Json] = [
              ..payload.workflow.activities,
              {
                "kind": "execute_tool",
                "tool": "市场、宏观与资讯研究包",
                "call_id": result.call_id,
                "cache_hit": result.cache_hit,
                "evidence_count": 16,
              },
              {
                "kind": "prepare_research",
                "detail": "Agent已完成候选复核并安排逐证券深度取证；此时仍未形成投资结论",
              },
            ]
            let workflow = ResearchWorkflowState::{
              task_id: payload.workflow.task_id,
              stage: "security",
              depth: payload.workflow.depth,
              tool_policy: payload.workflow.tool_policy,
              preferred_symbols: payload.workflow.preferred_symbols,
              capital: payload.workflow.capital,
              prepare_context: Some(result.payload),
              research_plan: Some(plan),
              report: None,
              activities,
            }
            response(request, true, {
              "accepted": true,
              "state": state_json(state),
              "checkpoint": state.to_json(),
              "effects": [
                @agent.PersistCheckpoint,
                @agent.ExecuteTool(
                  call_id~,
                  tool="research.agent_security_context",
                ),
              ].map(effect_json),
              "activities": activities,
              "host_effects": [
                host_engine_effect(
                  payload.workflow.task_id,
                  state.accepted_seq,
                  call_id,
                  "research.agent_security_context",
                  {
                    "symbols": symbols,
                    "depth": payload.workflow.depth,
                    "tool_policy": payload.workflow.tool_policy,
                    "analysis_modules": plan.analysis_modules.unwrap_or([]),
                    "benchmark": spec.comparison_benchmark,
                    "start": spec.research_start,
                    "end": spec.research_end,
                  },
                  600000,
                ),
              ],
              "continuation": {
                "kind": "agent.research.workflow.continue",
                "workflow": workflow.to_json(),
              },
            })
          } else if payload.workflow.stage == "security" {
            let prepared : AgentPrepareContext = @json.from_json(
              payload.workflow.prepare_context.unwrap(),
            )
            let security : AgentSecurityContext = @json.from_json(
              result.payload,
            )
            let plan = payload.workflow.research_plan.unwrap()
            let mut state = transition_state(
              current,
              ToolSucceeded(
                call_id=result.call_id,
                evidence_ids=plan.symbols.map(fn(symbol) {
                  "engine-security-context-" + symbol
                }),
              ),
            )
            let key = read_minimax_key()
            if key.trim().is_empty() {
              return self.workflow_provider_suspended(
                request, state, "provider_credential_missing",
              )
            }
            state = transition_state(state, MoreEvidenceRequired)
            state = transition_state(state, ModelRoundStarted)
            state = transition_state(state, ModelRoundStarted)
            state = transition_state(state, ModelRoundStarted)
            let provider = self.provider_config(key) catch {
              error =>
                return self.workflow_provider_suspended(
                  request,
                  state,
                  error.to_string(),
                )
            }
            let spec = state.spec.unwrap()
            let contract_facts : Array[Json] = [
              {
                "evidence_id": "evf_contract_a_share_lot",
                "path": "/contract/a_share_lot",
                "value": 100,
                "source": "AStock trading-rules v6",
                "observed_at": spec.as_of,
                "source_version_id": "trading-rules-v6",
                "quality_blocking": false,
              },
              {
                "evidence_id": "evf_task_as_of",
                "path": "/task_spec/as_of",
                "value": spec.as_of,
                "source": "user_task_spec",
                "observed_at": spec.as_of,
                "source_version_id": "task-spec-v1",
                "quality_blocking": false,
              },
              {
                "evidence_id": "evf_task_benchmark",
                "path": "/task_spec/comparison_benchmark",
                "value": spec.comparison_benchmark,
                "source": "user_task_spec",
                "observed_at": spec.as_of,
                "source_version_id": "task-spec-v1",
                "quality_blocking": false,
              },
              {
                "evidence_id": "evf_task_research_start",
                "path": "/task_spec/research_start",
                "value": spec.research_start,
                "source": "user_task_spec",
                "observed_at": spec.as_of,
                "source_version_id": "task-spec-v1",
                "quality_blocking": false,
              },
              {
                "evidence_id": "evf_task_research_end",
                "path": "/task_spec/research_end",
                "value": spec.research_end,
                "source": "user_task_spec",
                "observed_at": spec.as_of,
                "source_version_id": "task-spec-v1",
                "quality_blocking": false,
              },
              {
                "evidence_id": "evf_task_investment_horizon",
                "path": "/task_spec/investment_horizon",
                "value": spec.investment_horizon,
                "source": "user_task_spec",
                "observed_at": spec.as_of,
                "source_version_id": "task-spec-v1",
                "quality_blocking": false,
              },
            ]
            match payload.workflow.capital {
              Some(capital) =>
                contract_facts.push({
                  "evidence_id": "evf_user_capital",
                  "path": "/task_constraints/capital",
                  "value": capital,
                  "source": "user_task_spec",
                  "observed_at": spec.as_of,
                  "source_version_id": "task-spec-v1",
                  "quality_blocking": false,
                })
              None => ()
            }
            let context : Json = {
              "source": "moonbit_agent_orchestrated_engine",
              "retrieved_at": spec.as_of,
              "task_spec": spec.to_json(),
              "run_options": {
                "depth": payload.workflow.depth,
                "tool_policy": payload.workflow.tool_policy,
              },
              "research_plan": plan.to_json(),
              "market_overview": prepared.market_overview,
              "market_context": prepared.market_context,
              "global_context": prepared.global_context,
              "market_news": prepared.market_news,
              "securities": security.securities,
              "advanced_analysis": {
                "selected_modules": plan.analysis_modules.unwrap_or([]),
                "executed_modules": security.analysis_modules.unwrap_or([]),
                "cross_security": security.cross_security_analysis.unwrap_or({}),
                "tool_activities": security.tool_activities.unwrap_or([]),
              },
              "prepare_evidence": {
                "evidence_registry": prepared.evidence_registry,
              },
              "security_evidence": {
                "evidence_registry": security.evidence_registry,
              },
              "evidence_inventory": {
                "requested_symbols": plan.symbols,
                "advanced_modules": plan.analysis_modules.unwrap_or([]),
                "dimensions": [
                  "quote", "kline", "technical_analysis", "fund_flow", "market_pools",
                  "global_macro", "financial_statements", "valuation_history", "security_events",
                  "announcements", "multi_source_news", "cross_provider_reconciliation",
                  "optional_joinquant", "optional_tushare_iwencai_sec",
                ],
                "review_rounds": 3,
              },
              "contract_evidence": {
                "evidence_registry": {
                  "version": "astock-field-evidence/v1",
                  "scope": "task_contract",
                  "facts": contract_facts,
                  "truncated": false,
                },
              },
            }
            let report : String = generate_research_report(
              provider, key, spec, context,
            ) catch {
              error =>
                return self.workflow_provider_suspended(
                  request,
                  state,
                  error.to_string(),
                )
            }
            state = transition_state(state, EvidenceReviewCompleted)
            state = transition_state(state, ReviewReady)
            state = transition_state(state, SynthesisReady)
            let verification_errors = verify_research_report(
              report, spec, context,
            )
            let activities : Array[Json] = [
              ..payload.workflow.activities,
              {
                "kind": "execute_tool",
                "tool": "逐证券综合研究包",
                "call_id": result.call_id,
                "cache_hit": result.cache_hit,
                "evidence_count": plan.symbols.length() * 7,
                "analysis_modules": plan.analysis_modules.unwrap_or([]),
                "module_activities": security.tool_activities.unwrap_or([]),
              },
              {
                "kind": "review_evidence",
                "detail": "已复核缺失、时点、来源冲突、可成交性和组合约束",
              },
              {
                "kind": "synthesize_report",
                "detail": "MiniMax Plus 完成证据审查、反方挑战和最终综合三轮模型工作",
              },
            ]
            if !verification_errors.is_empty() {
              state = transition_state(
                state,
                VerificationRejected(verification_errors),
              )
              self.tasks[payload.workflow.task_id] = state
              return response(request, true, {
                "accepted": true,
                "state": state_json(state),
                "checkpoint": state.to_json(),
                "effects": [@agent.PersistCheckpoint].map(effect_json),
                "report": null,
                "verification_findings": verification_errors,
                "activities": [
                  ..activities,
                  {
                    "kind": "verify_report",
                    "detail": "MoonBit结构与安全边界校验失败，报告未发送到确定性Engine事实校验",
                  },
                ],
              })
            }
            let verify_call_id = "verify-report-" +
              payload.workflow.task_id +
              "-" +
              state.accepted_seq.to_string()
            state = transition_state(
              state,
              VerificationToolRequested(
                call_id=verify_call_id,
                tool="research.agent_report_verify",
              ),
            )
            self.tasks[payload.workflow.task_id] = state
            let workflow = ResearchWorkflowState::{
              task_id: payload.workflow.task_id,
              stage: "verify",
              depth: payload.workflow.depth,
              tool_policy: payload.workflow.tool_policy,
              preferred_symbols: payload.workflow.preferred_symbols,
              capital: payload.workflow.capital,
              prepare_context: None,
              research_plan: Some(plan),
              report: Some(report),
              activities: [
                ..activities,
                {
                  "kind": "verify_report",
                  "detail": "MoonBit结构门禁通过；正在由Engine复现数字、证据编号和质量状态",
                },
              ],
            }
            response(request, true, {
              "accepted": true,
              "state": state_json(state),
              "checkpoint": state.to_json(),
              "effects": [
                @agent.PersistCheckpoint,
                @agent.ExecuteTool(
                  call_id=verify_call_id,
                  tool="research.agent_report_verify",
                ),
              ].map(effect_json),
              "report": null,
              "verification_findings": [],
              "activities": workflow.activities,
              "host_effects": [
                host_engine_effect(
                  payload.workflow.task_id,
                  state.accepted_seq,
                  verify_call_id,
                  "research.agent_report_verify",
                  {
                    "report": report,
                    "context": context,
                    "task_spec": spec.to_json(),
                  },
                  120000,
                ),
              ],
              "continuation": {
                "kind": "agent.research.workflow.continue",
                "workflow": workflow.to_json(),
              },
            })
          } else if payload.workflow.stage == "verify" {
            let verification : AgentReportVerification = @json.from_json(
              result.payload,
            )
            let report = payload.workflow.report.unwrap()
            let mut state = transition_state(
              current,
              VerificationToolSucceeded(call_id=result.call_id, evidence_ids=[
                "engine-report-verifier-v1",
              ]),
            )
            state = if verification.passed {
              transition_state(state, VerificationPassed)
            } else {
              transition_state(
                state,
                VerificationRejected(verification.findings),
              )
            }
            self.tasks[payload.workflow.task_id] = state
            response(request, true, {
              "accepted": true,
              "state": state_json(state),
              "checkpoint": state.to_json(),
              "effects": if verification.passed {
                [
                  @agent.PersistCheckpoint,
                  @agent.PublishReport,
                  @agent.EmitTerminal,
                ].map(effect_json)
              } else {
                [@agent.PersistCheckpoint].map(effect_json)
              },
              "report": if verification.passed {
                report
              } else {
                null
              },
              "verification_findings": verification.findings,
              "verification": {
                "version": verification.verification_version,
                "distinct_citations": verification.distinct_citations,
                "numeric_claims_checked": verification.numeric_claims_checked,
                "registry_facts": verification.registry_facts,
              },
              "activities": [
                ..payload.workflow.activities,
                {
                  "kind": "verify_report",
                  "detail": if verification.passed {
                    "Engine已复现报告数字并验证证据编号、来源时点和质量状态"
                  } else {
                    "Engine事实校验失败，报告未发布"
                  },
                },
              ],
            })
          } else {
            response(
              request,
              false,
              null,
              error=error_payload(
                "invalid_workflow_stage", "Unknown Agent research workflow stage",
                false,
              ),
            )
          }
        }
      }
    }
    "agent.task.snapshot" => {
      let payload : EventPayload = @json.from_json(request.payload)
      match self.tasks.get(payload.task_id) {
        Some(state) => response(request, true, state_json(state))
        None =>
          response(
            request,
            false,
            null,
            error=error_payload(
              "task_not_loaded", "Agent task is not loaded", true,
            ),
          )
      }
    }
    _ =>
      response(
        request,
        false,
        null,
        error=error_payload(
          "unknown_request_kind",
          "Unsupported Agent worker request: " + request.kind,
          false,
        ),
      )
  }
}

///|
async fn read_frame() -> Json {
  let header = @stdio.stdin.read_exactly(4)
  guard header is [u32le(length)] else { fail("invalid frame header") }
  let length = length.reinterpret_as_int()
  if length <= 0 || length > @protocol.max_frame_bytes {
    fail("invalid frame length")
  }
  let body = @stdio.stdin.read_exactly(length)
  @json.parse(@utf8.decode(body))
}

///|
async fn write_frame(value : Json) -> Unit {
  let body = @utf8.encode(value.stringify())
  if body.length() <= 0 || body.length() > @protocol.max_frame_bytes {
    fail("response frame length exceeds protocol limit")
  }
  let header = FixedArray::make(4, b'\x00')
  header.unsafe_write_uint32_le(0, body.length().reinterpret_as_uint())
  @stdio.stdout.write(header.unsafe_reinterpret_as_bytes())
  @stdio.stdout.write(body)
}

///|
async fn log_error(message : String) -> Unit {
  @stdio.stderr.write(
    (
      { "level": "error", "component": "agent_worker", "message": message } :
      Json).stringify() +
    "\n",
  )
}

///|
async fn main {
  let worker = Worker::new()
  while true {
    let request_json = read_frame() catch {
      @io.ReaderClosed => return
      error => {
        log_error(error.to_string())
        return
      }
    }
    let request : RequestEnvelope = @json.from_json(request_json) catch {
      error => {
        log_error(error.to_string())
        return
      }
    }
    let reply = worker.handle(request) catch {
      error =>
        response(
          request,
          false,
          null,
          error=error_payload("worker_error", error.to_string(), false),
        )
    }
    write_frame(reply) catch {
      error => {
        log_error(error.to_string())
        return
      }
    }
  }
}
