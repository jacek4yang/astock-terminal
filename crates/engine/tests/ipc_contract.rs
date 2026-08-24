use astock_protocol::{
    read_frame, write_frame, RequestEnvelope, ResponseEnvelope, PROTOCOL_VERSION,
};
use serde_json::json;
use std::process::Stdio;
use tokio::process::Command;

fn request(id: &str, kind: &str, payload: serde_json::Value) -> RequestEnvelope {
    RequestEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: id.into(),
        kind: kind.into(),
        payload,
        deadline_ms: Some(5_000),
        cancellation_id: None,
    }
}

#[tokio::test]
async fn worker_stdout_is_protocol_only_and_handshake_is_versioned() {
    let data = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_astock-engine"))
        .env("ASTOCK_DATA_DIR", data.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();

    write_frame(
        &mut stdin,
        &request(
            "handshake-1",
            "system.handshake",
            json!({"app_version":"test","protocol_version":1}),
        ),
    )
    .await
    .unwrap();
    let response: ResponseEnvelope = read_frame(&mut stdout).await.unwrap().unwrap();
    assert!(response.ok);
    assert_eq!(response.protocol_version, PROTOCOL_VERSION);
    assert_eq!(response.request_id, "handshake-1");
    assert_eq!(response.payload["protocol_version"], 1);

    write_frame(
        &mut stdin,
        &request("shutdown-1", "system.shutdown", json!({})),
    )
    .await
    .unwrap();
    let shutdown: ResponseEnvelope = read_frame(&mut stdout).await.unwrap().unwrap();
    assert!(shutdown.ok);
    assert_eq!(child.wait().await.unwrap().code(), Some(0));
}
