///|
const MAX_FRAME_BYTES : Int = 8 * 1024 * 1024

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
  self.request_count += 1
  errdefer {
    self.failure_count += 1
    self.status = "degraded"
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
async fn WorkerClient::restart(self : WorkerClient) -> Unit {
  self.lock.acquire()
  defer self.lock.release()
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
  let handshake : Json = {
    "protocol_version": 1,
    "request_id": "host-" +
    self.name +
    "-restart-handshake-" +
    self.restart_count.to_string(),
    "kind": "system.handshake",
    "payload": {
      "app_version": "6.0.0",
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
  guard reply is { "ok": true, .. } else {
    fail(self.name + " Worker restart handshake failed")
  }
  self.heartbeat_misses = 0
  self.status = "ready"
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
