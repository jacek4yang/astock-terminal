///|
pub struct Backend {
  engine : WorkerClient
  agent : WorkerClient
  mut window : @proton.WindowHandle?
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
  let backend = Backend::{ engine, agent, window: None }
  backend.handshake()
  backend
}

///|
/// Bind Host-local window commands after Proton has created its primary
/// native window. The icon path is supplied by the D-drive build entry point;
/// packaged executables fall back to their embedded RT_GROUP_ICON resource.
pub fn Backend::attach_window(
  self : Backend,
  window : @proton.WindowHandle,
) -> Unit {
  self.window = Some(window)
  let icon_path = @env.get_env_var("ASTOCK_ICON_PATH").unwrap_or("")
  ignore(apply_window_icon(icon_path))
}

///|
pub fn Backend::detach_window(self : Backend) -> Unit {
  self.window = None
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
    "agent" =>
      self.agent.request(
        request.request,
        timeout_ms=worker_timeout(request.request, 300000),
      )
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
            "heartbeat_interval_ms": 2000,
            "heartbeat_miss_limit": 3,
            "restart_budget": {
              "window_ms": 600000,
              "max": 3,
              "backoff_ms": [1000, 2000, 5000],
            },
          })
      }
    }
    other => fail("unsupported Host target: " + other)
  }
}

///|
pub fn Backend::close(self : Backend) -> Unit {
  self.engine.close()
  self.agent.close()
}
