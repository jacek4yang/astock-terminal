//! Thin Tauri v2 desktop adapter.
//!
//! This binary is presentation and OS integration only. It owns the native
//! window, the IPC surface and credential-store plumbing. It does **not** own
//! Agent orchestration: intent interpretation, planning, clarification, tool
//! loops, retries, cancellation, context management and report verification all
//! live in `astock-agent-runtime`, exactly as they do for the terminal adapter.
//!
//! ```text
//! React  ──invoke/emit──>  this adapter  ──>  astock-agent-runtime
//!                                       └──>  astock-engine (deterministic)
//! ```
//!
//! Two rules keep the architecture honest and are enforced by
//! `astock-agent-runtime/tests/architecture.rs`:
//!
//! * the adapter must not depend on any domain crate, so financial capability is
//!   only reachable through the Engine boundary;
//! * the renderer must not reach native capability outside this bridge, which is
//!   why the capability grant is deny-by-default and exposes no filesystem,
//!   shell or process permission.

// Release builds must not open a console window alongside the GUI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio_util::sync::CancellationToken;

use astock_agent_runtime::{
    AgentEvent, AgentRuntime, EngineGateway, MinimaxProvider, RuntimeConfig, RuntimeSession,
    RuntimeTask, SessionManager,
};
use astock_engine::Engine;
use astock_protocol::{ErrorBody, RequestEnvelope, ResponseEnvelope, PROTOCOL_VERSION};

/// Which side of the shared runtime a renderer request is addressed to.
///
/// `engine` and `agent` are forwarded. `host` is the only target the adapter
/// answers itself, because window state is genuinely an adapter concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum BridgeTarget {
    Engine,
    Agent,
    Host,
}

/// Long-lived adapter state.
///
/// The Engine and session manager are created at startup because neither needs
/// a model credential. The provider is attached lazily on first research, so the
/// window opens and the whole browsing surface works before the user has
/// supplied any secret.
struct Desktop {
    engine: Arc<Engine>,
    gateway: Arc<EngineGateway>,
    sessions: SessionManager,
    /// Attached on the first research request. Guarded so concurrent starts
    /// cannot build two providers.
    runtime: tokio::sync::Mutex<Option<Arc<AgentRuntime>>>,
    /// Cancellation handles for in-flight research, keyed by task id, so
    /// `/cancel` from the GUI reaches the same cooperative cancellation the
    /// terminal uses.
    running: tokio::sync::Mutex<std::collections::HashMap<String, CancellationToken>>,
}

impl Desktop {
    async fn initialize() -> Result<Self, String> {
        let paths = data_root()?;
        let engine = Arc::new(
            Engine::initialize_at(&paths)
                .await
                .map_err(|error| format!("initialize Engine: {error}"))?,
        );
        let gateway = Arc::new(EngineGateway::new(engine.clone()));
        let sessions = SessionManager::new(gateway.clone());
        Ok(Self {
            engine,
            gateway,
            sessions,
            runtime: tokio::sync::Mutex::new(None),
            running: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// Attach the model provider on demand.
    ///
    /// Deferred so the desktop opens, browses market data and answers control
    /// requests without a credential, matching the terminal adapter.
    async fn runtime(&self) -> Result<Arc<AgentRuntime>, String> {
        let mut slot = self.runtime.lock().await;
        if let Some(existing) = slot.as_ref() {
            return Ok(existing.clone());
        }
        let key = astock_minimax::KeyStore::default()
            .load_key()
            .map_err(|error| format!("read the stored MiniMax credential: {error}"))?
            .ok_or_else(|| {
                "no MiniMax credential is installed; add one in Settings before starting research"
                    .to_string()
            })?;
        let client = astock_minimax::MinimaxClient::new(key);
        let provider = Arc::new(MinimaxProvider::new(client));
        let runtime = Arc::new(
            AgentRuntime::new(provider, self.gateway.clone(), self.gateway.clone())
                .with_config(RuntimeConfig::default()),
        );
        *slot = Some(runtime.clone());
        Ok(runtime)
    }
}

/// Resolve the durable data root, honouring the same override the CLI uses so
/// the two adapters can share one data directory or be pointed apart
/// deliberately.
fn data_root() -> Result<std::path::PathBuf, String> {
    if let Some(explicit) = std::env::var_os("ASTOCK_DATA_DIR") {
        if !explicit.is_empty() {
            return Ok(std::path::PathBuf::from(explicit).join("data"));
        }
    }
    directories::ProjectDirs::from("com", "AStock", "astock")
        .map(|dirs| dirs.data_dir().to_path_buf())
        .ok_or_else(|| "the operating system did not provide a user data directory".to_string())
}

/// The single renderer entry point.
///
/// The renderer sends the same protocol envelope the terminal adapter uses, so
/// there is one contract rather than a GUI-specific one. Engine requests are a
/// straight passthrough to `Engine::dispatch`, which already enforces the closed
/// request-kind allowlist, bounded pages and typed errors.
#[tauri::command]
async fn bridge_request(
    state: State<'_, Desktop>,
    target: BridgeTarget,
    envelope: RequestEnvelope,
) -> Result<ResponseEnvelope, String> {
    if envelope.protocol_version != PROTOCOL_VERSION {
        return Ok(reject(
            &envelope,
            "protocol_version_mismatch",
            &format!(
                "renderer spoke protocol {} but this host speaks {}",
                envelope.protocol_version, PROTOCOL_VERSION
            ),
        ));
    }

    match target {
        // Deterministic research and persistence. Forwarded unchanged.
        BridgeTarget::Engine => Ok(state.engine.dispatch(&envelope).await),
        // Agent surface. Session reads are served from the shared runtime's
        // session manager; anything that would require orchestration is
        // rejected rather than reimplemented here.
        BridgeTarget::Agent => agent_request(&state, &envelope).await,
        // Adapter-owned: window state only, never durable Agent truth.
        BridgeTarget::Host => Ok(host_request(&envelope)),
    }
}

async fn agent_request(
    state: &State<'_, Desktop>,
    envelope: &RequestEnvelope,
) -> Result<ResponseEnvelope, String> {
    let payload = match envelope.kind.as_str() {
        "agent.conversation.list" => {
            let limit = envelope
                .payload
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .min(astock_protocol::MAX_PAGE_SIZE as u64) as usize;
            let query = envelope.payload.get("query").and_then(Value::as_str);
            state
                .sessions
                .list(limit, query)
                .await
                .map_err(|error| error.to_string())
                .map(|items| json!({ "items": items }))
        }
        "agent.conversation.load" => {
            let id = envelope
                .payload
                .get("conversation_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            state
                .sessions
                .load(id)
                .await
                .map_err(|error| error.to_string())
                .and_then(|stored| serde_json::to_value(stored).map_err(|e| e.to_string()))
        }
        "diagnostics.status" => Ok(json!({
            "adapter": "tauri",
            "protocol_version": PROTOCOL_VERSION,
            "runtime": "astock-agent-runtime",
            "provider_attached": false,
        })),
        other => {
            return Ok(reject(
                envelope,
                "unsupported_agent_request",
                &format!(
                    "`{other}` is not part of the desktop agent surface; orchestration belongs to \
                     the shared runtime, not to this adapter"
                ),
            ))
        }
    };

    Ok(match payload {
        Ok(payload) => accept(envelope, payload),
        Err(message) => reject(envelope, "agent_request_failed", &message),
    })
}

fn host_request(envelope: &RequestEnvelope) -> ResponseEnvelope {
    match envelope.kind.as_str() {
        "diagnostics.status" => accept(
            envelope,
            json!({
                "adapter": "tauri",
                "version": env!("CARGO_PKG_VERSION"),
                "protocol_version": PROTOCOL_VERSION,
            }),
        ),
        other => reject(
            envelope,
            "unsupported_host_request",
            &format!("`{other}` is not a supported host request"),
        ),
    }
}

fn accept(envelope: &RequestEnvelope, payload: Value) -> ResponseEnvelope {
    ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: envelope.request_id.clone(),
        kind: envelope.kind.clone(),
        ok: true,
        payload,
        error: None,
    }
}

fn reject(envelope: &RequestEnvelope, code: &str, message: &str) -> ResponseEnvelope {
    ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: envelope.request_id.clone(),
        kind: envelope.kind.clone(),
        ok: false,
        payload: Value::Null,
        error: Some(ErrorBody {
            code: code.to_owned(),
            message: message.to_owned(),
            retryable: false,
            details: None,
        }),
    }
}

/// Forward a runtime event to the renderer.
///
/// The desktop and the terminal consume the *same* `AgentEvent` stream, so plan
/// revisions, clarification requests, tool progress and verification findings
/// cannot diverge between adapters.
fn forward_agent_event(app: &AppHandle, session_id: &str, task_id: &str, event: &AgentEvent) {
    let envelope = json!({
        "session_id": session_id,
        "task_id": task_id,
        "event": event,
    });
    if let Err(error) = app.emit("astock://agent-event", envelope) {
        tracing::warn!(%error, kind = event.kind(), "could not forward an Agent event");
    }
}

/// Start research from the renderer.
///
/// The adapter does not interpret the request: the prompt goes through the same
/// canonical intent path the terminal uses, and orchestration stays in the
/// runtime. The adapter's only jobs here are to attach the provider on demand,
/// register a cancellation handle, and relay events to the window.
#[tauri::command]
async fn agent_start(
    app: AppHandle,
    state: State<'_, Desktop>,
    prompt: String,
    session_id: Option<String>,
    depth: Option<String>,
) -> Result<Value, String> {
    let runtime = state.runtime().await?;
    let session = match session_id.as_deref() {
        Some(id) => {
            state
                .sessions
                .load(id)
                .await
                .map_err(|error| error.to_string())?
                .session
        }
        None => RuntimeSession::new(depth.as_deref().unwrap_or("balanced"), "full"),
    };

    let mut task = RuntimeTask::ask(&prompt);
    task.depth = depth.unwrap_or_else(|| session.depth.clone());
    task.tool_policy = session.tool_policy.clone();

    let mut stream = runtime.start_session_turn(session, task);
    let task_id = stream.task_id().to_owned();
    let stream_session_id = stream.session_id().to_owned();

    state
        .running
        .lock()
        .await
        .insert(task_id.clone(), stream.cancellation_token());

    let handle = app.clone();
    let relay_task_id = task_id.clone();
    let relay_session_id = stream_session_id.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(event) = stream.recv().await {
            forward_agent_event(&handle, &relay_session_id, &relay_task_id, &event);
        }
        match stream.finish().await {
            Ok(_) => tracing::debug!(task_id = %relay_task_id, "research finished"),
            Err(error) => {
                tracing::warn!(task_id = %relay_task_id, %error, "research ended with an error")
            }
        }
    });

    Ok(json!({ "task_id": task_id, "session_id": stream_session_id }))
}

/// Cancel in-flight research cooperatively.
///
/// This reaches the same cancellation token the terminal's `/cancel` and
/// `先停一下` reach, so there is no GUI-specific cancellation semantics.
#[tauri::command]
async fn agent_cancel(state: State<'_, Desktop>, task_id: String) -> Result<Value, String> {
    let running = state.running.lock().await;
    match running.get(&task_id) {
        Some(token) => {
            token.cancel();
            Ok(json!({ "cancelled": true, "task_id": task_id }))
        }
        None => Ok(json!({ "cancelled": false, "reason": "no such running task" })),
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("ASTOCK_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            // Engine initialization touches storage, so it runs on the async
            // runtime rather than blocking window creation.
            tauri::async_runtime::block_on(async move {
                match Desktop::initialize().await {
                    Ok(desktop) => {
                        handle.manage(desktop);
                        Ok(())
                    }
                    Err(error) => {
                        tracing::error!(%error, "could not initialize the desktop adapter");
                        Err(error)
                    }
                }
            })?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            bridge_request,
            agent_start,
            agent_cancel
        ])
        .run(tauri::generate_context!())
        .expect("run the AStock Terminal desktop adapter");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(kind: &str, payload: Value) -> RequestEnvelope {
        RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: "test-1".into(),
            kind: kind.into(),
            payload,
            deadline_ms: None,
            cancellation_id: None,
        }
    }

    #[test]
    fn host_answers_only_its_own_diagnostics() {
        let ok = host_request(&envelope("diagnostics.status", json!({})));
        assert!(ok.ok);
        assert_eq!(ok.payload["adapter"], "tauri");
    }

    #[test]
    fn the_host_surface_refuses_unknown_requests_rather_than_guessing() {
        let denied = host_request(&envelope("window.evaluate_script", json!({})));
        assert!(!denied.ok);
        assert_eq!(
            denied.error.expect("a typed error").code,
            "unsupported_host_request"
        );
    }

    #[test]
    fn rejection_preserves_request_correlation() {
        // The renderer correlates responses by request id; losing it would make
        // a failure indistinguishable from a dropped request.
        let request = envelope("whatever", json!({}));
        let denied = reject(&request, "code", "message");
        assert_eq!(denied.request_id, "test-1");
        assert_eq!(denied.kind, "whatever");
        assert!(!denied.ok);
        assert!(denied.payload.is_null());
    }

    #[test]
    fn bridge_targets_use_the_wire_names_the_renderer_sends() {
        assert_eq!(
            serde_json::from_value::<BridgeTarget>(json!("engine")).unwrap(),
            BridgeTarget::Engine
        );
        assert_eq!(
            serde_json::from_value::<BridgeTarget>(json!("agent")).unwrap(),
            BridgeTarget::Agent
        );
        assert_eq!(
            serde_json::from_value::<BridgeTarget>(json!("host")).unwrap(),
            BridgeTarget::Host
        );
        assert!(serde_json::from_value::<BridgeTarget>(json!("domain")).is_err());
    }

    #[test]
    fn an_explicit_data_root_is_honoured_like_the_cli() {
        // Sharing the override keeps the two adapters pointing at one data
        // directory unless the operator deliberately separates them.
        std::env::set_var("ASTOCK_DATA_DIR", "/tmp/astock-desktop-probe");
        let resolved = data_root().expect("a data root");
        std::env::remove_var("ASTOCK_DATA_DIR");
        assert!(resolved.ends_with("data"));
        assert!(resolved.starts_with("/tmp/astock-desktop-probe"));
    }
}
