///|
#cfg(platform="windows")
async test "dead worker restarts after three misses and re-handshakes" {
  assert_true(create_worker_job())
  let child_pid = @process.spawn_orphan(
    "powershell.exe",
    ["-NoLogo", "-NoProfile", "-Command", "Start-Sleep -Seconds 30"],
    no_console_window=true,
  )
  assert_true(assign_worker_to_job(child_pid))
  assert_true(worker_process_running(child_pid))
  assert_true(worker_job_active_processes() >= 1)
  assert_true(shutdown_worker_job(0))
  @async.sleep(100)
  assert_false(worker_process_running(child_pid))
  guard @env.get_env_var("ASTOCK_SUPERVISION_TEST_WORKER") is Some(executable) else {
    return
  }
  assert_true(create_worker_job())
  let worker = WorkerClient::start("agent-test", executable)
  let handshake : Json = {
    "protocol_version": 1,
    "request_id": "supervision-test-handshake",
    "kind": "system.handshake",
    "payload": {
      "app_version": "6.0.0",
      "host_version": "moonbit-proton-0.2.1",
      "protocol_version": 1,
    },
    "deadline_ms": 15000,
  }
  let reply = worker.request(handshake, timeout_ms=15000)
  assert_true(reply is { "ok": true, .. })
  worker.heartbeat() catch {
    error => fail("initial heartbeat failed: " + error.to_string())
  }
  assert_eq(worker.heartbeat_misses, 0)
  let original_pid = worker.pid
  assert_true(terminate_worker_process(original_pid))
  @async.sleep(100)
  assert_false(worker_process_running(original_pid))
  worker.heartbeat() catch {
    error => fail("first missed heartbeat failed: " + error.to_string())
  }
  assert_eq(worker.heartbeat_misses, 1)
  worker.heartbeat() catch {
    error => fail("second missed heartbeat failed: " + error.to_string())
  }
  assert_eq(worker.heartbeat_misses, 2)
  worker.heartbeat() catch {
    error => fail("restart heartbeat failed: " + error.to_string())
  }
  assert_eq(worker.heartbeat_misses, 0)
  assert_eq(worker.restart_count, 1)
  assert_true(worker.pid != original_pid)
  assert_true(worker_process_running(worker.pid))
  assert_eq(worker.status, "ready")
  worker.close()
  assert_true(shutdown_worker_job(0))
  @async.sleep(100)
}

///|
test "Agent Effect permissions allow only bounded research aggregates" {
  assert_true(permitted_agent_effect_kind("research.agent_prepare_context"))
  assert_true(permitted_agent_effect_kind("research.agent_security_context"))
  assert_true(permitted_agent_effect_kind("research.agent_report_verify"))
  assert_false(permitted_agent_effect_kind("credentials.provider.delete"))
  assert_false(permitted_agent_effect_kind("storage.data_root.migrate"))
  assert_false(permitted_agent_effect_kind("research.backtest.start"))
}

///|
test "Renderer request contracts are target-specific and deny internal continuation" {
  assert_true(renderer_engine_request_kind("market.quote"))
  assert_false(renderer_engine_request_kind("market.unknown"))
  assert_false(renderer_engine_request_kind("agent.task.create"))
  assert_false(renderer_engine_request_kind("agent.event.append"))
  assert_false(renderer_engine_request_kind("agent.checkpoint.put"))
  assert_false(renderer_engine_request_kind("agent.effect.begin"))
  assert_false(renderer_engine_request_kind("agent.effect.complete"))
  assert_false(renderer_engine_request_kind("agent.effect.list"))
  assert_true(renderer_agent_request_kind("agent.research.workflow"))
  assert_false(renderer_agent_request_kind("agent.research.workflow.continue"))
  assert_false(renderer_agent_request_kind("agent.restore"))
  assert_false(renderer_agent_request_kind("agent.task.snapshot"))
  assert_true(durable_agent_request_kind("agent.start"))
  assert_true(durable_agent_request_kind("agent.event"))
  assert_true(durable_agent_request_kind("agent.research.workflow"))
  assert_false(durable_agent_request_kind("agent.provider.configure"))
  assert_false(durable_agent_request_kind("agent.restore"))
  assert_true(renderer_host_request_kind("window.toggle_maximize"))
  assert_false(renderer_host_request_kind("window.unknown"))
}

///|
test "durable operation identity includes the full structured payload" {
  let stored = DurableEffectView::{
    status: "succeeded",
    result: Some({ "ok": true }),
    effect_kind: "agent.research.workflow",
    effect: {
      "worker_request_kind": "agent.research.workflow",
      "payload": { "task_id": "task-1", "depth": "deep", "tool_policy": "full" },
    },
  }
  assert_true(
    durable_operation_effect_matches(stored, "agent.research.workflow", {
      "tool_policy": "full",
      "depth": "deep",
      "task_id": "task-1",
    }),
  )
  assert_false(
    durable_operation_effect_matches(stored, "agent.research.workflow", {
      "tool_policy": "market",
      "depth": "deep",
      "task_id": "task-1",
    }),
  )
}

///|
#cfg(platform="windows")
async test "Host owns Agent durability and reuses a completed operation" {
  guard @env.get_env_var("ASTOCK_HOST_DURABILITY_TEST") is Some("1") &&
    @env.get_env_var("ASTOCK_ENGINE_EXE") is Some(_) &&
    @env.get_env_var("ASTOCK_AGENT_EXE") is Some(_) &&
    @env.get_env_var("ASTOCK_DATA_DIR") is Some(_) else {
    return
  }
  let backend = Backend::start()
  defer backend.close()
  let payload : Json = {
    "task_id": "host-durability-test",
    "seq": 1,
    "spec": {
      "objective": "验证 Host 持久化边界",
      "security_universe": ["600519"],
      "as_of": "2026-08-25T13:00:00+08:00",
      "research_start": "2025-08-25",
      "research_end": "2026-08-25",
      "investment_horizon": "3个月",
      "comparison_benchmark": "000300",
      "output_type": "manual_plan",
      "evidence_requirement": "strict",
    },
  }
  let first = backend.route(@shared.AppRequest::{
    target: "agent",
    request: worker_request(
      "host-durability-first", "agent.start", payload, 30000,
    ),
  })
  assert_true(
    first
    is {
      "request_id": "host-durability-first",
      "kind": "agent.start",
      "ok": true,
      ..
    },
  )
  let first_reply : WorkerResponseEnvelope = @json.from_json(first)
  let projection : AgentWorkflowPayload = @json.from_json(first_reply.payload)
  guard projection.state is Some(first_state) else {
    fail("Agent start response did not contain durable state")
  }
  assert_eq(first_state.accepted_seq, 1)
  assert_eq(first_state.phase, "preparing")
  let durable_json = backend.require_engine(
    "host-durability-load",
    "agent.task.load",
    { "task_id": "host-durability-test" },
    30000,
  )
  let durable : DurableTaskView = @json.from_json(durable_json)
  assert_eq(durable.task.accepted_seq, 1)
  assert_eq(durable.events.length(), 1)
  let effects_json = backend.require_engine(
    "host-durability-effects",
    "agent.effect.list",
    { "task_id": "host-durability-test" },
    30000,
  )
  let effects : DurableEffectList = @json.from_json(effects_json)
  assert_eq(effects.items.length(), 1)
  assert_eq(effects.items[0].status, "succeeded")
  let repeated = backend.route(@shared.AppRequest::{
    target: "agent",
    request: worker_request(
      "host-durability-repeated", "agent.start", payload, 30000,
    ),
  })
  assert_true(
    repeated
    is {
      "request_id": "host-durability-repeated",
      "kind": "agent.start",
      "ok": true,
      ..
    },
  )
  let repeated_effects_json = backend.require_engine(
    "host-durability-effects-repeated",
    "agent.effect.list",
    { "task_id": "host-durability-test" },
    30000,
  )
  let repeated_effects : DurableEffectList = @json.from_json(
    repeated_effects_json,
  )
  assert_eq(repeated_effects.items.length(), 1)
  let old_agent_pid = backend.agent.pid
  assert_true(terminate_worker_process(old_agent_pid))
  backend.agent.restart()
  assert_true(backend.agent.pid != old_agent_pid)
  let cancelled = backend.route(@shared.AppRequest::{
    target: "agent",
    request: worker_request(
      "host-durability-after-restart",
      "agent.event",
      { "task_id": "host-durability-test", "seq": 2, "event_kind": "cancel" },
      30000,
    ),
  })
  let cancelled_reply : WorkerResponseEnvelope = @json.from_json(cancelled)
  let cancelled_projection : AgentWorkflowPayload = @json.from_json(
    cancelled_reply.payload,
  )
  guard cancelled_projection.state is Some(cancelled_state) else {
    fail("Agent restart response did not contain restored state")
  }
  assert_eq(cancelled_state.accepted_seq, 2)
  assert_eq(cancelled_state.phase, "cancelled")
  let after_restart_json = backend.require_engine(
    "host-durability-after-restart-load",
    "agent.task.load",
    { "task_id": "host-durability-test" },
    30000,
  )
  let after_restart : DurableTaskView = @json.from_json(after_restart_json)
  assert_eq(after_restart.task.accepted_seq, 2)
  assert_eq(after_restart.events.length(), 2)
  let renderer_write : Json? = Some(
    backend.route(@shared.AppRequest::{
      target: "engine",
      request: worker_request(
        "host-durability-forbidden-write",
        "agent.effect.complete",
        { "effect_id": "forged-effect", "status": "succeeded", "result": null },
        30000,
      ),
    }),
  ) catch {
    _ => None
  }
  assert_true(renderer_write is None)
}

///|
test "startup handshake requires exact protocol versions and capability subsets" {
  let engine_reply : Json = {
    "protocol_version": 1,
    "request_id": "engine-good",
    "kind": "system.handshake",
    "ok": true,
    "payload": {
      "protocol_version": 1,
      "engine_version": protocol_release_version,
      "capabilities": [
        "market", "research", "data_quality", "agent_advanced_analysis_v1", "storage",
        "credentials", "agent_event_store_v2", "future_capability",
      ],
      "max_frame_bytes": MAX_FRAME_BYTES,
      "max_page_size": 500,
    },
  }
  validate_worker_handshake("engine", "engine-good", engine_reply)
  let agent_reply : Json = {
    "protocol_version": 1,
    "request_id": "agent-good",
    "kind": "system.handshake",
    "ok": true,
    "payload": {
      "protocol_version": 1,
      "agent_version": protocol_release_version,
      "reducer_version": "moonbit-agent-kernel-v1",
      "capabilities": agent_startup_required_capabilities,
      "max_frame_bytes": MAX_FRAME_BYTES,
    },
  }
  validate_worker_handshake("agent", "agent-good", agent_reply)
  let bad_version : Unit? = Some(
    validate_worker_handshake("engine", "engine-good", {
      "protocol_version": 1,
      "request_id": "engine-good",
      "kind": "system.handshake",
      "ok": true,
      "payload": {
        "protocol_version": 1,
        "engine_version": "5.9.9",
        "capabilities": engine_startup_required_capabilities,
        "max_frame_bytes": MAX_FRAME_BYTES,
        "max_page_size": 500,
      },
    }),
  ) catch {
    _ => None
  }
  assert_true(bad_version is None)
  let missing_capability : Unit? = Some(
    validate_worker_handshake("agent", "agent-good", {
      "protocol_version": 1,
      "request_id": "agent-good",
      "kind": "system.handshake",
      "ok": true,
      "payload": {
        "protocol_version": 1,
        "agent_version": protocol_release_version,
        "reducer_version": "moonbit-agent-kernel-v1",
        "capabilities": ["pure_reducer", "replay"],
        "max_frame_bytes": MAX_FRAME_BYTES,
      },
    }),
  ) catch {
    _ => None
  }
  assert_true(missing_capability is None)
}
