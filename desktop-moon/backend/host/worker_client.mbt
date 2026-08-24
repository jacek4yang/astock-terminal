const MAX_FRAME_BYTES : Int = 8 * 1024 * 1024

///| A single full-duplex Worker channel. Calls are serialized so response
/// framing stays correlated; the renderer can still have requests queued at
/// the Host boundary where the hard limit is 256.
pub struct WorkerClient {
  name : String
  pid : Int
  input : @process.WriteToProcess
  output : @process.ReadFromProcess
  lock : @async.Mutex
  mut request_count : Int
  mut failure_count : Int
}

///|
pub async fn WorkerClient::start(name : String, executable : String) -> WorkerClient raise {
  let (child_stdin, host_input) = @process.write_to_process()
  let (host_output, child_stdout) = @process.read_from_process()
  let pid = @process.spawn_orphan(
    executable,
    [],
    stdin=child_stdin,
    stdout=child_stdout,
    no_console_window=true,
  )
  WorkerClient::{
    name,
    pid,
    input: host_input,
    output: host_output,
    lock: @async.Mutex::Mutex(),
    request_count: 0,
    failure_count: 0,
  }
}

///|
async fn WorkerClient::write_frame(self : WorkerClient, value : Json) -> Unit raise {
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
async fn WorkerClient::read_frame(self : WorkerClient) -> Json raise {
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
) -> Json raise {
  self.lock.acquire()
  defer self.lock.release()
  self.request_count += 1
  errdefer { self.failure_count += 1 }
  @async.with_timeout(timeout_ms, () => {
    self.write_frame(request)
    self.read_frame()
  })
}

///|
pub fn WorkerClient::diagnostics(self : WorkerClient) -> Json {
  {
    "name": self.name,
    "pid": self.pid,
    "request_count": self.request_count,
    "failure_count": self.failure_count,
    "status": if self.failure_count == 0 { "ready" } else { "degraded" },
  }
}

///|
pub fn WorkerClient::close(self : WorkerClient) -> Unit {
  self.input.close()
  self.output.close()
}
