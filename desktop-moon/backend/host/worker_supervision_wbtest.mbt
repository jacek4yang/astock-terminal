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
  defer ignore(shutdown_worker_job(0))
  let worker = WorkerClient::start("agent-test", executable)
  defer worker.close()
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
  assert_true(renderer_agent_request_kind("agent.research.workflow"))
  assert_false(renderer_agent_request_kind("agent.research.workflow.continue"))
  assert_true(renderer_host_request_kind("window.toggle_maximize"))
  assert_false(renderer_host_request_kind("window.unknown"))
}
