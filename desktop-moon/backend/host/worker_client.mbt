///|
const MAX_FRAME_BYTES : Int = 8 * 1024 * 1024

///|
struct WorkerHandshakeEnvelope {
  protocol_version : Int
  request_id : String
  kind : String
  ok : Bool
  payload : Json
} derive(FromJson)

///|
struct EngineHandshakePayload {
  protocol_version : Int
  engine_version : String
  capabilities : Array[String]
  max_frame_bytes : Int
  max_page_size : Int
} derive(FromJson)

///|
struct AgentHandshakePayload {
  protocol_version : Int
  agent_version : String
  reducer_version : String
  capabilities : Array[String]
  max_frame_bytes : Int
} derive(FromJson)

///|
fn require_capabilities(
  worker_name : String,
  actual : Array[String],
  required : Array[String],
) -> Unit raise {
  for capability in required {
    guard actual.contains(capability) else {
      fail(
        worker_name +
        " Worker is missing required startup capability: " +
        capability,
      )
    }
  }
}

///|
fn validate_worker_handshake(
  worker_name : String,
  expected_request_id : String,
  reply : Json,
) -> Unit raise {
  let envelope : WorkerHandshakeEnvelope = @json.from_json(reply)
  guard envelope.ok &&
    envelope.protocol_version == 1 &&
    envelope.request_id == expected_request_id &&
    envelope.kind == "system.handshake" else {
    fail(worker_name + " Worker returned an incompatible handshake envelope")
  }
  if worker_name.has_prefix("engine") {
    let payload : EngineHandshakePayload = @json.from_json(envelope.payload)
    guard payload.protocol_version == 1 &&
      payload.engine_version == protocol_release_version &&
      payload.max_frame_bytes == MAX_FRAME_BYTES &&
      payload.max_page_size == 500 else {
      fail("Engine Worker returned incompatible protocol limits or version")
    }
    require_capabilities(
      "Engine",
      payload.capabilities,
      engine_startup_required_capabilities,
    )
  } else if worker_name.has_prefix("agent") {
    let payload : AgentHandshakePayload = @json.from_json(envelope.payload)
    guard payload.protocol_version == 1 &&
      payload.agent_version == protocol_release_version &&
      payload.reducer_version == "moonbit-agent-kernel-v1" &&
      payload.max_frame_bytes == MAX_FRAME_BYTES else {
      fail("Agent Worker returned incompatible protocol limits or version")
    }
    require_capabilities(
      "Agent",
      payload.capabilities,
      agent_startup_required_capabilities,
    )
  } else {
    fail("Host cannot validate an unknown Worker role: " + worker_name)
  }
}

///|
/// A single full-duplex Worker channel. Calls are serialized so response
/// framing stays correlated; the renderer can still have requests queued at
/// the Host boundary where the hard limit is 256.
pub struct WorkerClient {
  name : String
  executable : String
  mut pid : Int
  mut input : @process.WriteToProcess
  mut output : @process.ReadFromProcess
  lock : @async.Mutex
  mut request_count : Int
  mut failure_count : Int
  mut heartbeat_misses : Int
  mut restart_count : Int
  restart_times : Array[Int64]
  mut in_flight : Int
  mut status : String
  mut last_heartbeat_ms : Int64
  mut closed : Bool
}

///|
async fn spawn_worker(
  name : String,
  executable : String,
) -> (Int, @process.WriteToProcess, @process.ReadFromProcess) {
  let (child_stdin, host_input) = @process.write_to_process()
  let (host_output, child_stdout) = @process.read_from_process()
  let pid = @process.spawn_orphan(
    executable,
    [],
    stdin=child_stdin,
    stdout=child_stdout,
    no_console_window=true,
  )
  if !assign_worker_to_job(pid) {
    host_input.close()
    host_output.close()
    ignore(terminate_worker_process(pid))
    fail("failed to assign " + name + " Worker to the Host Job Object")
  }
  (pid, host_input, host_output)
}

///|
pub async fn WorkerClient::start(
  name : String,
  executable : String,
) -> WorkerClient {
  let (pid, input, output) = spawn_worker(name, executable)
  WorkerClient::{
    name,
    executable,
    pid,
    input,
    output,
    lock: @async.Mutex::Mutex(),
    request_count: 0,
    failure_count: 0,
    heartbeat_misses: 0,
    restart_count: 0,
    restart_times: [],
    in_flight: 0,
    status: "starting",
    last_heartbeat_ms: @async.now(),
    closed: false,
  }
}

///|
async fn WorkerClient::write_frame(self : WorkerClient, value : Json) -> Unit {
  let body = @utf8.encode(value.stringify())
  if body.length() <= 0 || body.length() > MAX_FRAME_BYTES {
    fail("request frame exceeds protocol limit")
  }
  let header = FixedArray::make(4, b'\x00')
  header.unsafe_write_uint32_le(0, body.length().reinterpret_as_uint())
  self.input.write(header.unsafe_reinterpret_as_bytes())
  self.input.write(body)
}

///|
async fn WorkerClient::read_frame(self : WorkerClient) -> Json {
  let header = self.output.read_exactly(4)
  guard header is [u32le(length)] else { fail("invalid response frame header") }
  let length = length.reinterpret_as_int()
  if length <= 0 || length > MAX_FRAME_BYTES {
    fail("invalid response frame length")
  }
  @json.parse(@utf8.decode(self.output.read_exactly(length)))
}

///|
pub async fn WorkerClient::request(
  self : WorkerClient,
  request : Json,
  timeout_ms? : Int = 30000,
) -> Json {
  if self.closed {
    fail(self.name + " Worker channel is closed")
  }
  self.in_flight += 1
  defer {
    self.in_flight -= 1
  }
  self.lock.acquire()
  defer self.lock.release()
  if !worker_process_running(self.pid) {
    self.restart_locked()
  }
  self.request_count += 1
  errdefer {
    self.failure_count += 1
    self.status = "degraded"
    // A timed-out single ordered channel can no longer prove which response
    // belongs to the next request. Kill it immediately; the next request or
    // heartbeat will restart and re-handshake instead of consuming a stale
    // frame under the wrong request_id.
    if worker_process_running(self.pid) {
      ignore(terminate_worker_process(self.pid))
    }
  }
  let reply = @async.with_timeout(timeout_ms, () => {
    self.write_frame(request)
    self.read_frame()
  })
  self.status = "ready"
  reply
}

///|
fn WorkerClient::restart_budget_available(self : WorkerClient) -> Bool {
  let now = @async.now()
  let mut recent = 0
  for timestamp in self.restart_times {
    if now - timestamp <= 600000L {
      recent += 1
    }
  }
  recent < 3
}

///|
async fn WorkerClient::restart_locked(self : WorkerClient) -> Unit {
  if self.closed {
    return
  }
  if !self.restart_budget_available() {
    self.status = "hard_failed"
    fail(self.name + " Worker exhausted its 10-minute restart budget")
  }
  let backoff = match self.restart_count % 3 {
    0 => 1000
    1 => 2000
    _ => 5000
  }
  self.status = "restarting"
  self.input.close()
  self.output.close()
  if worker_process_running(self.pid) {
    ignore(terminate_worker_process(self.pid))
  }
  @async.sleep(backoff)
  let (pid, input, output) = spawn_worker(self.name, self.executable)
  self.pid = pid
  self.input = input
  self.output = output
  self.restart_count += 1
  self.restart_times.push(@async.now())
  let handshake_request_id = "host-" +
    self.name +
    "-restart-handshake-" +
    self.restart_count.to_string()
  let handshake : Json = {
    "protocol_version": 1,
    "request_id": handshake_request_id,
    "kind": "system.handshake",
    "payload": {
      "app_version": protocol_release_version,
      "host_version": "moonbit-proton-0.2.1",
      "protocol_version": 1,
    },
    "deadline_ms": 15000,
  }
  errdefer {
    self.input.close()
    self.output.close()
    ignore(terminate_worker_process(self.pid))
    self.status = "degraded"
  }
  let reply = @async.with_timeout(15000, () => {
    self.write_frame(handshake)
    self.read_frame()
  })
  validate_worker_handshake(self.name, handshake_request_id, reply)
  self.heartbeat_misses = 0
  self.status = "ready"
}

///|
async fn WorkerClient::restart(self : WorkerClient) -> Unit {
  self.lock.acquire()
  defer self.lock.release()
  self.restart_locked()
}

///|
/// Preempt a long synchronous Worker operation. The process termination makes
/// the active framed read fail, after which this call acquires the channel lock
/// and performs a clean spawn plus version/capability handshake. This is used
/// only for an explicit durable Agent cancel event.
pub async fn WorkerClient::interrupt_and_restart(self : WorkerClient) -> Bool {
  if self.closed {
    return false
  }
  let previous_pid = self.pid
  self.status = "cancelling"
  if worker_process_running(previous_pid) {
    ignore(terminate_worker_process(previous_pid))
  }
  self.restart()
  self.pid != previous_pid && self.status == "ready"
}

///|
async fn WorkerClient::record_heartbeat_miss(
  self : WorkerClient,
  count_failure? : Bool = true,
) -> Unit {
  self.heartbeat_misses += 1
  if count_failure {
    self.failure_count += 1
  }
  self.status = "degraded"
  if self.heartbeat_misses >= 3 {
    self.restart()
  }
}

///|
pub async fn WorkerClient::heartbeat(self : WorkerClient) -> Unit {
  if self.closed {
    return
  }
  if !worker_process_running(self.pid) {
    self.record_heartbeat_miss()
    return
  }
  // A request already in flight proves the channel is actively supervised.
  // Do not inject a health frame behind a long provider request on the single
  // ordered Agent channel; process liveness is still checked above.
  if self.in_flight > 0 {
    self.status = "busy"
    self.heartbeat_misses = 0
    self.last_heartbeat_ms = @async.now()
    return
  }
  let heartbeat : Json = {
    "protocol_version": 1,
    "request_id": "host-heartbeat-" +
    self.name +
    "-" +
    self.request_count.to_string(),
    "kind": "diagnostics.status",
    "payload": {},
    "deadline_ms": 2000,
  }
  let reply = self.request(heartbeat, timeout_ms=2000) catch {
    _ => {
      self.record_heartbeat_miss(count_failure=false)
      return
    }
  }
  if reply is { "ok": true, .. } {
    self.heartbeat_misses = 0
    self.last_heartbeat_ms = @async.now()
    self.status = "ready"
  } else {
    self.record_heartbeat_miss()
  }
}

///|
pub fn WorkerClient::diagnostics(self : WorkerClient) -> Json {
  {
    "name": self.name,
    "pid": self.pid,
    "request_count": self.request_count,
    "failure_count": self.failure_count,
    "status": self.status,
    "process_running": worker_process_running(self.pid),
    "in_flight": self.in_flight,
    "heartbeat_misses": self.heartbeat_misses,
    "last_heartbeat_ms": self.last_heartbeat_ms,
    "restart_count": self.restart_count,
  }
}

///|
pub fn WorkerClient::close(self : WorkerClient) -> Unit {
  if self.closed {
    return
  }
  self.closed = true
  self.status = "stopping"
  self.input.close()
  self.output.close()
}
