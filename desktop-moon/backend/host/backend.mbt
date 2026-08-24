///|
pub struct Backend {
  engine : WorkerClient
  agent : WorkerClient
  mut window : @proton.WindowHandle?
  mut window_events : @proton.WindowEventEmitter?
  mut closed : Bool
}

///|
/// Minimal envelope projection used only to correlate Host-local replies.
struct HostRequestEnvelope {
  request_id : String
  kind : String
} derive(FromJson)

///|
/// Renderer deadlines are part of the versioned request envelope. Keep a
/// Host-side ceiling so model calls may run longer than ordinary market
/// commands without allowing an unbounded bridge operation.
struct DeadlineEnvelope {
  deadline_ms : Int?
} derive(FromJson)

///|
/// Standard Worker response projection used by the generic Agent effect
/// runner. The Host never interprets financial payloads or chooses tools.
struct WorkerResponseEnvelope {
  ok : Bool
  payload : Json
  error : Json?
} derive(FromJson)

///|
struct WorkflowTaskState {
  task_id : String
  accepted_seq : Int
  phase : String
} derive(FromJson, ToJson)

///|
extend WorkflowTaskState with ToJson::{to_json}

///|
struct AgentHostEffect {
  task_id : String
  caused_by_seq : Int
  call_id : String
  target : String
  kind : String
  payload : Json
  deadline_ms : Int
  idempotency_key : String
} derive(FromJson)

///|
struct AgentContinuation {
  kind : String
  workflow : Json
} derive(FromJson)

///|
struct AgentWorkflowPayload {
  state : WorkflowTaskState?
  checkpoint : Json?
  host_effects : Array[AgentHostEffect]?
  continuation : AgentContinuation?
  activities : Json?
  verification_findings : Json?
} derive(FromJson)

///|
struct DurableEffectView {
  status : String
  result : Json?
  idempotency_key : String
} derive(FromJson)

///|
struct DurableEffectList {
  items : Array[DurableEffectView]
} derive(FromJson)

///|
struct DurableEventView {
  seq : Int
} derive(FromJson)

///|
struct DurableTaskView {
  events : Array[DurableEventView]
} derive(FromJson)

///|
struct HostToolProgress {
  call_id : String
  ok : Bool
  cache_hit : Bool
} derive(FromJson, ToJson)

///|
extend HostToolProgress with ToJson::{to_json}

///|
fn worker_timeout(request : Json, maximum : Int) -> Int {
  let projected : DeadlineEnvelope = @json.from_json(request) catch {
    _ => return 30000
  }
  let requested = projected.deadline_ms.unwrap_or(30000)
  if requested < 1000 {
    1000
  } else if requested > maximum {
    maximum
  } else {
    requested
  }
}

///|
fn worker_path(env_name : String, packaged_name : String) -> String {
  match @env.get_env_var(env_name) {
    Some(path) if !path.trim().is_empty() => path.trim().to_owned()
    _ => @proton.resource_dir() + "\\workers\\" + packaged_name
  }
}

///|
pub async fn Backend::start() -> Backend {
  guard create_worker_job() else {
    fail("failed to create the Windows Worker Job Object")
  }
  errdefer ignore(shutdown_worker_job(0))
  let engine = @async.with_timeout(15000, () => {
    WorkerClient::start(
      "engine",
      worker_path("ASTOCK_ENGINE_EXE", "astock-engine.exe"),
    )
  })
  errdefer engine.close()
  let agent = @async.with_timeout(15000, () => {
    WorkerClient::start(
      "agent",
      worker_path("ASTOCK_AGENT_EXE", "astock-agent.exe"),
    )
  })
  errdefer agent.close()
  let backend = Backend::{
    engine,
    agent,
    window: None,
    window_events: None,
    closed: false,
  }
  backend.handshake()
  backend
}

///|
/// Supervise both Worker protocol channels every two seconds. Three missed
/// health checks trigger a bounded 1/2/5 second restart; an active long-running
/// request is treated as busy while native process liveness remains checked.
pub async fn Backend::supervise(self : Backend) -> Unit {
  while !self.closed {
    @async.sleep(2000)
    if self.closed {
      return
    }
    self.engine.heartbeat() catch {
      _ => ()
    }
    self.agent.heartbeat() catch {
      _ => ()
    }
  }
}

///|
/// Bind Host-local window commands after Proton has created its primary
/// native window. The icon path is supplied by the D-drive build entry point;
/// packaged executables fall back to their embedded RT_GROUP_ICON resource.
pub fn Backend::attach_window(
  self : Backend,
  window : @proton.WindowHandle,
  events : @proton.WindowEventEmitter,
) -> Unit {
  self.window = Some(window)
  self.window_events = Some(events)
  let icon_path = @env.get_env_var("ASTOCK_ICON_PATH").unwrap_or("")
  ignore(apply_window_icon(icon_path))
}

///|
pub fn Backend::detach_window(self : Backend) -> Unit {
  self.window = None
  self.window_events = None
}

///|
/// Send only public workflow state and activity summaries after their durable
/// checkpoint has been committed. The event never contains prompts, model
/// reasoning, credentials, complete tool payloads or report drafts.
async fn Backend::emit_workflow_progress(
  self : Backend,
  request_id : String,
  round : Int,
  projection : AgentWorkflowPayload,
  stage : String,
  tool : AgentHostEffect?,
  tool_result : Json?,
) -> Unit {
  guard self.window_events is Some(events) else { return }
  let tool_summary : Json = match tool {
    Some(effect) => { "call_id": effect.call_id, "kind": effect.kind }
    None => null
  }
  events.emit(@shared.worker_event, {
    "request_id": request_id,
    "round": round,
    "stage": stage,
    "state": match projection.state {
      Some(state) => state.to_json()
      None => null
    },
    "activities": match projection.activities {
      Some(activities) => activities
      None => []
    },
    "verification_findings": match projection.verification_findings {
      Some(findings) => findings
      None => []
    },
    "tool": tool_summary,
    "tool_result": match tool_result {
      Some(result) => result
      None => null
    },
  }) catch {
    _ => ()
  }
}

///|
fn Backend::window_handle(self : Backend) -> @proton.WindowHandle raise {
  match self.window {
    Some(window) => window
    None => fail("primary window is not ready")
  }
}

///|
fn host_reply(envelope : HostRequestEnvelope, payload : Json) -> Json {
  {
    "protocol_version": 1,
    "request_id": envelope.request_id,
    "kind": envelope.kind,
    "ok": true,
    "payload": payload,
  }
}

///|
fn correlate_worker_reply(
  envelope : HostRequestEnvelope,
  reply : WorkerResponseEnvelope,
) -> Json {
  {
    "protocol_version": 1,
    "request_id": envelope.request_id,
    "kind": envelope.kind,
    "ok": reply.ok,
    "payload": reply.payload,
    "error": match reply.error {
      Some(error) => error
      None => null
    },
  }
}

///|
fn worker_request(
  request_id : String,
  kind : String,
  payload : Json,
  deadline_ms : Int,
) -> Json {
  {
    "protocol_version": 1,
    "request_id": request_id,
    "kind": kind,
    "payload": payload,
    "deadline_ms": deadline_ms,
  }
}

///|
async fn Backend::engine_response(
  self : Backend,
  request_id : String,
  kind : String,
  payload : Json,
  timeout_ms : Int,
) -> WorkerResponseEnvelope {
  let raw = self.engine.request(
    worker_request(request_id, kind, payload, timeout_ms),
    timeout_ms~,
  )
  @json.from_json(raw)
}

///|
fn response_error(reply : WorkerResponseEnvelope) -> String {
  match reply.error {
    Some(error) => error.stringify()
    None => "Worker request failed without an error payload"
  }
}

///|
async fn Backend::require_engine(
  self : Backend,
  request_id : String,
  kind : String,
  payload : Json,
  timeout_ms : Int,
) -> Json {
  let reply = self.engine_response(request_id, kind, payload, timeout_ms)
  if !reply.ok {
    fail("Engine durability operation failed: " + response_error(reply))
  }
  reply.payload
}

///|
/// Persist every intermediate Agent checkpoint before executing the effects
/// exposed by that response. The event bodies are intentionally generic: the
/// Agent owns semantic state while the Host only maintains sequence durability.
async fn Backend::persist_workflow_checkpoint(
  self : Backend,
  parent_id : String,
  projection : AgentWorkflowPayload,
) -> Unit {
  guard projection.state is Some(state) &&
    projection.checkpoint is Some(checkpoint) else {
    return
  }
  let loaded_json = self.require_engine(
    parent_id + ":task-load:" + state.accepted_seq.to_string(),
    "agent.task.load",
    { "task_id": state.task_id },
    30000,
  )
  let loaded : DurableTaskView = @json.from_json(loaded_json)
  let mut durable_max = 0
  for event in loaded.events {
    if event.seq > durable_max {
      durable_max = event.seq
    }
  }
  while durable_max < state.accepted_seq {
    durable_max += 1
    ignore(
      self.require_engine(
        parent_id + ":event:" + durable_max.to_string(),
        "agent.event.append",
        {
          "task_id": state.task_id,
          "seq": durable_max,
          "event_id": "workflow:" +
          state.task_id +
          ":" +
          durable_max.to_string(),
          "event_kind": "agent.workflow.transition",
          "event": { "worker_state": state.to_json() },
        },
        30000,
      ),
    )
  }
  ignore(
    self.require_engine(
      parent_id + ":checkpoint:" + state.accepted_seq.to_string(),
      "agent.checkpoint.put",
      {
        "task_id": state.task_id,
        "accepted_seq": state.accepted_seq,
        "phase": state.phase,
        "checkpoint": checkpoint,
      },
      30000,
    ),
  )
}

///|
async fn Backend::execute_agent_effect(
  self : Backend,
  parent_id : String,
  effect : AgentHostEffect,
) -> Json {
  if effect.target != "engine" {
    return {
      "call_id": effect.call_id,
      "ok": false,
      "payload": null,
      "error": "Agent requested a target outside the Engine allowlist",
      "cache_hit": false,
    }
  }
  let listed = self.require_engine(
    parent_id + ":effect-list:" + effect.call_id,
    "agent.effect.list",
    { "task_id": effect.task_id },
    30000,
  )
  let history : DurableEffectList = @json.from_json(listed)
  let prior : Array[DurableEffectView] = []
  for item in history.items {
    if item.idempotency_key == effect.idempotency_key ||
      item.idempotency_key.has_prefix(effect.idempotency_key + ":retry:") {
      prior.push(item)
    }
  }
  for item in prior {
    if item.status == "succeeded" && item.result is Some(result) {
      return {
        "call_id": effect.call_id,
        "ok": true,
        "payload": result,
        "error": null,
        "cache_hit": true,
      }
    }
  }
  for item in prior {
    if item.status == "pending" {
      // These two aggregate services are read-only snapshots. If the process
      // died after intent persistence, a new attempt is safe and is recorded
      // under a retry key; every mutating/future target remains fail-closed.
      if effect.kind != "research.agent_prepare_context" &&
        effect.kind != "research.agent_security_context" &&
        effect.kind != "research.agent_report_verify" {
        return {
          "call_id": effect.call_id,
          "ok": false,
          "payload": null,
          "error": "相同工具 Effect 仍为 pending；为防止重复副作用已停止执行",
          "cache_hit": false,
        }
      }
    }
  }
  let retry = prior.length()
  let idempotency_key = if retry == 0 {
    effect.idempotency_key
  } else {
    effect.idempotency_key + ":retry:" + retry.to_string()
  }
  let effect_id = "host-tool:" +
    effect.task_id +
    ":" +
    effect.call_id +
    ":" +
    retry.to_string()
  ignore(
    self.require_engine(
      parent_id + ":effect-begin:" + effect.call_id,
      "agent.effect.begin",
      {
        "effect_id": effect_id,
        "task_id": effect.task_id,
        "caused_by_seq": effect.caused_by_seq,
        "effect_kind": "engine." + effect.kind,
        "effect": {
          "target": "engine",
          "kind": effect.kind,
          "payload": effect.payload,
        },
        "idempotency_key": idempotency_key,
      },
      30000,
    ),
  )
  let tool_reply = self.engine_response(
    parent_id + ":tool:" + effect.call_id,
    effect.kind,
    effect.payload,
    effect.deadline_ms,
  )
  let stored_result : Json = if tool_reply.ok {
    tool_reply.payload
  } else {
    { "error": response_error(tool_reply) }
  }
  ignore(
    self.require_engine(
      parent_id + ":effect-complete:" + effect.call_id,
      "agent.effect.complete",
      {
        "effect_id": effect_id,
        "status": if tool_reply.ok {
          "succeeded"
        } else {
          "failed"
        },
        "result": stored_result,
      },
      30000,
    ),
  )
  {
    "call_id": effect.call_id,
    "ok": tool_reply.ok,
    "payload": if tool_reply.ok {
      tool_reply.payload
    } else {
      null
    },
    "error": if tool_reply.ok {
      null
    } else {
      response_error(tool_reply)
    },
    "cache_hit": false,
  }
}

///|
/// Interpret only the generic `host_effects + continuation` shell contract.
/// All selection, tool kinds, parameters and stopping decisions originate in
/// the MoonBit Agent Worker.
async fn Backend::route_agent(self : Backend, request : Json) -> Json {
  let envelope : HostRequestEnvelope = @json.from_json(request)
  let maximum = worker_timeout(request, 900000)
  let mut reply_json = self.agent.request(request, timeout_ms=maximum)
  let mut round = 0
  while round < 4 {
    let reply : WorkerResponseEnvelope = @json.from_json(reply_json)
    if !reply.ok {
      return correlate_worker_reply(envelope, reply)
    }
    let projection : AgentWorkflowPayload = @json.from_json(reply.payload) catch {
      _ => return correlate_worker_reply(envelope, reply)
    }
    self.persist_workflow_checkpoint(envelope.request_id, projection)
    self.emit_workflow_progress(
      envelope.request_id,
      round,
      projection,
      "checkpointed",
      None,
      None,
    )
    let effects = projection.host_effects.unwrap_or([])
    if effects.is_empty() {
      return correlate_worker_reply(envelope, reply)
    }
    guard projection.continuation is Some(continuation) else {
      fail("Agent returned host effects without a continuation")
    }
    let results : Array[Json] = []
    for effect in effects {
      let result = self.execute_agent_effect(envelope.request_id, effect)
      let progress : HostToolProgress = @json.from_json(result)
      self.emit_workflow_progress(
        envelope.request_id,
        round,
        projection,
        "tool_finished",
        Some(effect),
        Some(progress.to_json()),
      )
      results.push(result)
    }
    round += 1
    reply_json = self.agent.request(
      worker_request(
        envelope.request_id + ":continuation:" + round.to_string(),
        continuation.kind,
        { "workflow": continuation.workflow, "tool_results": results },
        maximum,
      ),
      timeout_ms=maximum,
    )
  }
  fail("Agent exceeded the bounded Host effect continuation limit")
}

///|
fn handshake_request(request_id : String) -> Json {
  {
    "protocol_version": 1,
    "request_id": request_id,
    "kind": "system.handshake",
    "payload": {
      "app_version": "6.0.0",
      "host_version": "moonbit-proton-0.2.1",
      "protocol_version": 1,
    },
    "deadline_ms": 15000,
  }
}

///|
async fn Backend::handshake(self : Backend) -> Unit {
  let engine_reply = self.engine.request(
    handshake_request("host-engine-startup"),
    timeout_ms=15000,
  )
  let agent_reply = self.agent.request(
    handshake_request("host-agent-startup"),
    timeout_ms=15000,
  )
  guard engine_reply is { "ok": true, .. } && agent_reply is { "ok": true, .. } else {
    fail("Worker protocol handshake failed")
  }
}

///|
pub async fn Backend::route(
  self : Backend,
  request : @shared.AppRequest,
) -> Json {
  match request.target {
    "engine" =>
      self.engine.request(
        request.request,
        timeout_ms=worker_timeout(request.request, 180000),
      )
    "agent" => self.route_agent(request.request)
    "host" => {
      let envelope : HostRequestEnvelope = @json.from_json(request.request)
      match envelope.kind {
        "window.state" => {
          let state = self.window_handle().state()
          host_reply(envelope, {
            "x": state.x,
            "y": state.y,
            "width": state.width,
            "height": state.height,
            "visible": state.visible,
            "focused": state.focused,
            "minimized": state.minimized,
            "maximized": state.maximized,
            "fullscreen": state.fullscreen,
            "zoom_percent": state.zoom_percent,
          })
        }
        "window.minimize" => {
          self.window_handle().minimize()
          host_reply(envelope, { "accepted": true })
        }
        "window.toggle_maximize" => {
          let window = self.window_handle()
          let was_maximized = window.state().maximized
          if was_maximized {
            window.restore()
          } else {
            window.maximize()
          }
          host_reply(envelope, { "maximized": !was_maximized })
        }
        "window.begin_drag" =>
          host_reply(envelope, { "accepted": begin_window_drag() })
        "window.system_menu" =>
          host_reply(envelope, { "accepted": show_window_system_menu() })
        _ =>
          host_reply(envelope, {
            "status": "ready",
            "host_version": "moonbit-proton-0.2.1",
            "engine": self.engine.diagnostics(),
            "agent": self.agent.diagnostics(),
            "queue_limit": 256,
            "worker_supervision": {
              "job_object_managed": true,
              "kill_on_host_close": true,
              "active_processes": worker_job_active_processes(),
              "graceful_shutdown_ms": 5000,
              "heartbeat_interval_ms": 2000,
              "heartbeat_miss_limit": 3,
              "busy_request_policy": "process_liveness",
              "restart_budget": {
                "window_ms": 600000,
                "max": 3,
                "backoff_ms": [1000, 2000, 5000],
              },
            },
          })
      }
    }
    other => fail("unsupported Host target: " + other)
  }
}

///|
pub fn Backend::close(self : Backend) -> Unit {
  if self.closed {
    return
  }
  self.closed = true
  self.engine.close()
  self.agent.close()
  ignore(shutdown_worker_job(5000))
}
