use astock_engine::Engine;
use astock_protocol::{read_frame, write_frame, RequestEnvelope, ResponseEnvelope};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::io::{stdin, stdout, BufReader, BufWriter};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;

fn main() {
    init_tracing();
    // The Engine dispatcher intentionally keeps all versioned services in one
    // process. Aggregate Agent snapshots reuse those async services one level
    // deep; Windows' default 2 MiB Tokio worker stack is insufficient for the
    // generated future. Four 8 MiB workers keep the total reservation bounded
    // while preserving concurrent provider I/O and deterministic CPU work.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(4)
        .thread_stack_size(8 * 1024 * 1024)
        .build()
        .expect("build Engine runtime");
    if let Err(error) = runtime.block_on(run()) {
        tracing::error!(error = %error, "engine terminated");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Arc::new(Engine::initialize().await.map_err(std::io::Error::other)?);
    let writer = Arc::new(Mutex::new(BufWriter::new(stdout())));
    let permits = Arc::new(Semaphore::new(256));
    let cancellations = Arc::new(Mutex::new(HashMap::<String, CancellationToken>::new()));
    let mut request_ids = ReplayWindow::new(4_096);
    let mut reader = BufReader::new(stdin());

    while let Some(request) = read_frame::<_, RequestEnvelope>(&mut reader).await? {
        if request.request_id.trim().is_empty() {
            let response = ResponseEnvelope::failure(
                &request,
                "invalid_request_id",
                "request_id must not be empty",
                false,
            );
            write_frame(&mut *writer.lock().await, &response).await?;
            continue;
        }
        if !request_ids.insert(request.request_id.clone()) {
            let response = ResponseEnvelope::failure(
                &request,
                "duplicate_request_id",
                "request_id was already observed on this Worker channel",
                false,
            );
            write_frame(&mut *writer.lock().await, &response).await?;
            continue;
        }
        if request.kind == "system.shutdown" {
            let response =
                ResponseEnvelope::success(&request, serde_json::json!({"accepted": true}));
            write_frame(&mut *writer.lock().await, &response).await?;
            break;
        }
        if request.kind == "system.cancel" {
            let cancellation_id = request
                .payload
                .get("cancellation_id")
                .and_then(serde_json::Value::as_str);
            let response = if let Some(cancellation_id) = cancellation_id {
                let cancelled = cancellations
                    .lock()
                    .await
                    .remove(cancellation_id)
                    .map(|token| {
                        token.cancel();
                        true
                    })
                    .unwrap_or(false);
                ResponseEnvelope::success(&request, serde_json::json!({"cancelled": cancelled}))
            } else {
                ResponseEnvelope::failure(
                    &request,
                    "invalid_request",
                    "cancellation_id is required",
                    false,
                )
            };
            write_frame(&mut *writer.lock().await, &response).await?;
            continue;
        }

        let permit = permits.clone().acquire_owned().await?;
        let engine = engine.clone();
        let writer = writer.clone();
        let cancellations = cancellations.clone();
        let token = if let Some(id) = request.cancellation_id.as_ref() {
            let mut active = cancellations.lock().await;
            if active.contains_key(id) {
                let response = ResponseEnvelope::failure(
                    &request,
                    "duplicate_cancellation_id",
                    "cancellation_id is already active",
                    false,
                );
                write_frame(&mut *writer.lock().await, &response).await?;
                drop(permit);
                continue;
            }
            let token = CancellationToken::new();
            active.insert(id.clone(), token.clone());
            Some(token)
        } else {
            None
        };
        tokio::spawn(async move {
            let dispatch = async {
                if let Some(deadline_ms) = request.deadline_ms {
                    match timeout(
                        Duration::from_millis(deadline_ms),
                        engine.dispatch(&request),
                    )
                    .await
                    {
                        Ok(response) => response,
                        Err(_) => ResponseEnvelope::failure(
                            &request,
                            "deadline_exceeded",
                            "Engine request exceeded its declared deadline",
                            true,
                        ),
                    }
                } else {
                    engine.dispatch(&request).await
                }
            };
            let response = if let Some(token) = token {
                tokio::select! {
                    _ = token.cancelled() => ResponseEnvelope::failure(
                        &request,
                        "cancelled",
                        "Engine request was cancelled",
                        true,
                    ),
                    response = dispatch => response,
                }
            } else {
                dispatch.await
            };
            if let Some(id) = request.cancellation_id.as_ref() {
                cancellations.lock().await.remove(id);
            }
            if let Err(error) = write_frame(&mut *writer.lock().await, &response).await {
                tracing::error!(error = %error, request_id = %request.request_id, "write response failed");
            }
            drop(permit);
        });
    }
    Ok(())
}

struct ReplayWindow {
    capacity: usize,
    order: VecDeque<String>,
    seen: HashSet<String>,
}

impl ReplayWindow {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            order: VecDeque::with_capacity(capacity.max(1)),
            seen: HashSet::with_capacity(capacity.max(1)),
        }
    }

    fn insert(&mut self, request_id: String) -> bool {
        if !self.seen.insert(request_id.clone()) {
            return false;
        }
        self.order.push_back(request_id);
        if self.order.len() > self.capacity {
            if let Some(expired) = self.order.pop_front() {
                self.seen.remove(&expired);
            }
        }
        true
    }
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .json()
        .init();
}

#[cfg(test)]
mod tests {
    use super::ReplayWindow;

    #[test]
    fn replay_window_rejects_duplicates_and_expires_only_the_oldest_id() {
        let mut window = ReplayWindow::new(2);
        assert!(window.insert("one".into()));
        assert!(!window.insert("one".into()));
        assert!(window.insert("two".into()));
        assert!(window.insert("three".into()));
        assert!(window.insert("one".into()), "expired IDs may be reused");
        assert!(!window.insert("three".into()));
    }
}
