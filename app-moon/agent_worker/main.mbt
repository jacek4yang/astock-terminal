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
/// Engine-fetched, versioned research context supplied by the renderer. The
/// credential and database layout never cross this boundary.
struct ResearchPayload {
  task_id : String
  context : Json
} derive(FromJson)

///|
/// Candidate-planning input. All rows originate in the Rust Engine and the
/// returned symbols are validated against this exact bounded list.
struct PlanPayload {
  task_id : String
  candidates : Array[ResearchCandidate]
  market_context : Json
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
    "review_ready" => ReviewReady
    "synthesis_ready" => SynthesisReady
    "verification_passed" => VerificationPassed
    "verification_rejected" =>
      VerificationRejected(
        payload.errors.unwrap_or(["unspecified_verification_error"]),
      )
    "quota_suspended" => QuotaSuspended
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
  mut provider : ProviderConfig?
  mut model_routing : ModelRouting
}

///|
fn Worker::new() -> Worker {
  {
    tasks: Map([], capacity=32),
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
          "multi_pass_review", "deterministic_report_verification",
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
    "agent.research" => {
      let payload : ResearchPayload = @json.from_json(request.payload)
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
          if current.phase != Preparing {
            response(
              request,
              false,
              null,
              error=error_payload(
                "task_not_ready", "研究任务尚未完成必要的边界确认",
                false,
              ),
            )
          } else {
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
            let spec = current.spec.unwrap()
            let report = generate_research_report(
              provider,
              key,
              spec,
              payload.context,
            )
            let call_id = "desktop-context-" + payload.task_id
            let evidence_id = "engine-context-" + payload.task_id
            let mut state = transition_state(current, Prepared)
            state = transition_state(state, ModelRoundStarted)
            state = transition_state(state, ModelRoundStarted)
            state = transition_state(state, ModelRoundStarted)
            state = transition_state(
              state,
              ToolRequested(call_id~, tool="desktop_engine_context"),
            )
            state = transition_state(
              state,
              ToolSucceeded(call_id~, evidence_ids=[evidence_id]),
            )
            state = transition_state(state, ReviewReady)
            state = transition_state(state, SynthesisReady)
            let verification_errors = verify_research_report(
              report,
              spec,
              payload.context,
            )
            state = if verification_errors.is_empty() {
              transition_state(state, VerificationPassed)
            } else {
              transition_state(state, VerificationRejected(verification_errors))
            }
            self.tasks[payload.task_id] = state
            response(request, true, {
              "accepted": true,
              "rejection": null,
              "state": state_json(state),
              "checkpoint": state.to_json(),
              "effects": if verification_errors.is_empty() {
                [
                  @agent.PersistCheckpoint,
                  @agent.PublishReport,
                  @agent.EmitTerminal,
                ].map(effect_json)
              } else {
                [@agent.PersistCheckpoint].map(effect_json)
              },
              "report": if verification_errors.is_empty() {
                report
              } else {
                null
              },
              "verification_findings": verification_errors,
              "activities": [
                {
                  "kind": "prepare_research",
                  "detail": "研究边界已确认",
                },
                {
                  "kind": "execute_tool",
                  "tool": "桌面 Engine 研究快照",
                  "call_id": call_id,
                  "cache_hit": false,
                  "evidence_count": 1,
                },
                {
                  "kind": "review_evidence",
                  "detail": "第一轮已逐项检查行情、资金、财务、估值、新闻、公告与缺失值",
                },
                {
                  "kind": "review_evidence",
                  "detail": "第二轮独立反证：检查过时资讯、单一来源、不可成交与组合约束",
                },
                {
                  "kind": "synthesize_report",
                  "detail": "第三轮 MiniMax Plus 综合原始证据与两轮复核生成纯文本报告",
                },
                {
                  "kind": "verify_report",
                  "detail": if verification_errors.is_empty() {
                    "已通过结构、证据清单、人工计划要素、私有推理隔离与发布边界校验"
                  } else {
                    "校验未通过，报告未发布；请补齐缺失结构或证据后恢复任务"
                  },
                },
              ],
            })
          }
      }
    }
    "agent.plan" => {
      let payload : PlanPayload = @json.from_json(request.payload)
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
          if current.phase != Preparing {
            response(
              request,
              false,
              null,
              error=error_payload(
                "task_not_ready", "研究边界尚未确认", false,
              ),
            )
          } else {
            let key = read_minimax_key()
            if key.trim().is_empty() {
              return response(
                request,
                false,
                null,
                error=error_payload(
                  "provider_credential_missing", "未配置 MiniMax Plus API Key",
                  false,
                ),
              )
            }
            let provider = self.provider_config(key)
            let plan = generate_research_plan(
              provider,
              key,
              current.spec.unwrap(),
              payload.candidates,
              payload.market_context,
            )
            response(request, true, {
              "plan": plan.to_json(),
              "activities": [
                {
                  "kind": "prepare_research",
                  "detail": "Agent已从有真实行情且满足资金约束的候选池选择深度取证对象",
                },
              ],
            })
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
