//! End-to-end tests against a local mini HTTP server (see `tests/common`).

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use astock_minimax::{ChatMessage, ChatRequest, MinimaxError, ModelCatalog, Region};
use common::{quota_body, RawResponse};

/// Region routing by key: `cn-key` is valid on the China service only,
/// `intl-key` on the international one, anything else nowhere.
fn region_handler(req: &common::RecordedRequest) -> RawResponse {
    assert!(req.path.ends_with("/v1/token_plan/remains"));
    let on_cn = req.path.starts_with("/cn/");
    let key = req.authorization.as_deref().unwrap_or("");
    let valid = (on_cn && key == "Bearer cn-key") || (!on_cn && key == "Bearer intl-key");
    if valid {
        RawResponse::json(
            200,
            r#"{"model_remains":[],"base_resp":{"status_code":0,"status_msg":"success"}}"#,
        )
    } else {
        RawResponse::json(
            200,
            r#"{"base_resp":{"status_code":2049,"status_msg":"invalid api key"}}"#,
        )
    }
}

#[tokio::test]
async fn detects_china_region() {
    let server = common::spawn(region_handler);
    let client = common::test_client("cn-key", &server.url);
    let info = client.detect_service().await.unwrap();
    assert_eq!(info.region, Region::Cn);
    assert!(info.api_host.contains("/cn"));
    // Second call must be cached: still exactly two probe requests.
    let again = client.detect_service().await.unwrap();
    assert_eq!(again, info);
    assert_eq!(server.count_path("token_plan/remains"), 2);
}

#[tokio::test]
async fn detects_international_region() {
    let server = common::spawn(region_handler);
    let client = common::test_client("intl-key", &server.url);
    let info = client.detect_service().await.unwrap();
    assert_eq!(info.region, Region::Intl);
}

#[tokio::test]
async fn invalid_key_everywhere_is_auth_error() {
    let server = common::spawn(region_handler);
    let client = common::test_client("bogus-key", &server.url);
    let err = client.detect_service().await.unwrap_err();
    assert!(matches!(err, MinimaxError::Auth(_)), "got {err:?}");
}

#[tokio::test]
async fn quota_parses_over_the_wire() {
    let server = common::spawn(|req| {
        assert!(req.path.ends_with("/v1/token_plan/remains"));
        RawResponse::json(200, &quota_body(80))
    });
    let client = common::test_client("cn-key", &server.url);
    let quota = client.quota().await.unwrap();
    let m = quota.model("MiniMax-M2.5").unwrap();
    assert_eq!(m.current_interval_total_count, Some(100));
    assert_eq!(m.current_interval_usage_count, Some(20));
    assert_eq!(m.current_interval_remaining_percent, Some(80.0));
    assert!(!quota.throttled("MiniMax-M2.5"));
    assert!(!quota.exhausted("MiniMax-M2.5"));
    assert!(quota.window_reset_at("MiniMax-M2.5").is_some());
}

#[tokio::test]
async fn model_fallback_skips_failing_models() {
    let calls = Arc::new(AtomicUsize::new(0));
    let server = common::spawn({
        let calls = calls.clone();
        move |req| {
            if req.path.ends_with("/v1/token_plan/remains") {
                return RawResponse::json(200, &quota_body(80));
            }
            calls.fetch_add(1, Ordering::SeqCst);
            if req.body.contains("MiniMax-M3") {
                // Best model unavailable on this plan.
                RawResponse::json(
                    200,
                    r#"{"base_resp":{"status_code":1000,"status_msg":"model not available"}}"#,
                )
            } else {
                RawResponse::json(
                    200,
                    r#"{"id":"p","choices":[{"index":0,"message":{"role":"assistant","content":""}}],"base_resp":{"status_code":0,"status_msg":"success"}}"#,
                )
            }
        }
    });
    let client = common::test_client("cn-key", &server.url);
    let selected = client.selected_model().await.unwrap();
    assert_eq!(selected, "MiniMax-M2.7");
    assert_eq!(client.catalog().selected(), Some("MiniMax-M2.7"));
    // M3 tried once, M2.7 succeeded; second selected_model() call is cached.
    let again = client.selected_model().await.unwrap();
    assert_eq!(again, "MiniMax-M2.7");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn model_fallback_aborts_on_auth_error() {
    let server = common::spawn(|_req| {
        RawResponse::json(
            200,
            r#"{"base_resp":{"status_code":2049,"status_msg":"invalid api key"}}"#,
        )
    });
    let catalog = ModelCatalog::new();
    let http: Arc<dyn astock_minimax::Http> = Arc::new(astock_minimax::ReqwestHttp::new_direct());
    let err = catalog
        .probe_models(
            &*http,
            &server.url,
            &astock_minimax::SecretKey::new("bad-key"),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, MinimaxError::Auth(_)), "got {err:?}");
    // Exactly one probe: no point falling through the chain on a bad key.
    assert_eq!(server.count_path("chat/completions"), 1);
}

#[tokio::test]
async fn chat_roundtrip_with_tool_calls() {
    let server = common::spawn(|req| {
        if req.path.ends_with("/v1/token_plan/remains") {
            return RawResponse::json(200, &quota_body(80));
        }
        assert!(req.path.ends_with("/v1/chat/completions"));
        assert!(req.body.contains("\"stream\":false"));
        assert!(req.body.contains("get_quote"));
        RawResponse::json(
            200,
            r#"{"id":"c1","choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_9","type":"function","function":{"name":"get_quote","arguments":"{\"symbol\":\"600519\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":12,"completion_tokens":7,"total_tokens":19},"base_resp":{"status_code":0,"status_msg":"success"}}"#,
        )
    });
    let client = common::test_client("cn-key", &server.url);
    let request = ChatRequest::new("MiniMax-M2.5", vec![ChatMessage::user("quote 600519")])
        .with_tools(vec![astock_minimax::ToolSpec::function(
            "get_quote",
            "Fetch a stock quote",
            serde_json::json!({"type": "object", "properties": {"symbol": {"type": "string"}}}),
        )]);
    let resp = client.chat(&request).await.unwrap();
    assert_eq!(resp.finish_reason(), Some("tool_calls"));
    let calls = resp.tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].function.as_ref().unwrap().arguments.as_deref(),
        Some("{\"symbol\":\"600519\"}")
    );
    assert_eq!(resp.usage.unwrap().total_tokens, Some(19));
}

#[tokio::test]
async fn chat_stream_parses_dripped_sse() {
    let sse_body = concat!(
        "data: {\"id\":\"s1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"<think>\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"thinking</think>answer\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"total_tokens\":3}}\n\n",
        "data: [DONE]\n\n",
    );
    let server = common::spawn(move |req| {
        if req.path.ends_with("/v1/token_plan/remains") {
            return RawResponse::json(200, &quota_body(80));
        }
        assert!(req.body.contains("\"stream\":true"));
        RawResponse::sse(sse_body)
    });
    let client = common::test_client("cn-key", &server.url);
    let request = ChatRequest::new("MiniMax-M2.5", vec![ChatMessage::user("hi")]);
    let stream = client.chat_stream(&request).await.unwrap();
    futures::pin_mut!(stream);
    let mut text = String::new();
    let mut finish = None;
    let mut chunks = 0;
    while let Some(item) = futures::StreamExt::next(&mut stream).await {
        let chunk = item.unwrap();
        chunks += 1;
        if let Some(t) = chunk.raw_delta() {
            text.push_str(&t);
        }
        if let Some(f) = chunk.finish_reason() {
            finish = Some(f.to_string());
        }
    }
    assert_eq!(chunks, 3);
    assert_eq!(text, "<think>thinking</think>answer");
    assert_eq!(finish.as_deref(), Some("stop"));
}

#[tokio::test]
async fn chat_stream_retries_only_before_establishment() {
    let calls = Arc::new(AtomicUsize::new(0));
    let server = common::spawn({
        let calls = calls.clone();
        move |req| {
            if req.path.ends_with("/v1/token_plan/remains") {
                return RawResponse::json(200, &quota_body(80));
            }
            let attempt = calls.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                RawResponse {
                    status: 429,
                    headers: vec![("retry-after".to_string(), "0".to_string())],
                    body: br#"{"error":{"message":"slow down"}}"#.to_vec(),
                    drip: false,
                }
            } else {
                RawResponse::sse(
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                )
            }
        }
    });
    let client = common::test_client("cn-key", &server.url);
    let request = ChatRequest::new("MiniMax-M2.5", vec![ChatMessage::user("hi")]);
    let stream = client.chat_stream(&request).await.unwrap();
    futures::pin_mut!(stream);
    let chunk = futures::StreamExt::next(&mut stream)
        .await
        .expect("one SSE chunk")
        .unwrap();
    assert_eq!(chunk.raw_delta().as_deref(), Some("ok"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn rate_gate_retries_429_then_succeeds() {
    let calls = Arc::new(AtomicUsize::new(0));
    let server = common::spawn({
        let calls = calls.clone();
        move |req| {
            if req.path.ends_with("/v1/token_plan/remains") {
                return RawResponse::json(200, &quota_body(80));
            }
            let n = calls.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                RawResponse {
                    status: 429,
                    headers: vec![("retry-after".to_string(), "0".to_string())],
                    body: br#"{"error":{"message":"slow down"}}"#.to_vec(),
                    drip: false,
                }
            } else {
                RawResponse::json(
                    200,
                    r#"{"id":"ok","choices":[{"index":0,"message":{"role":"assistant","content":"done"}}],"base_resp":{"status_code":0,"status_msg":"success"}}"#,
                )
            }
        }
    });
    let client = common::test_client("cn-key", &server.url);
    let request = ChatRequest::new("MiniMax-M2.5", vec![ChatMessage::user("hi")]);
    let resp = client.chat(&request).await.unwrap();
    assert_eq!(resp.text().as_deref(), Some("done"));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn rate_gate_gives_up_after_max_attempts() {
    let server = common::spawn(|req| {
        if req.path.ends_with("/v1/token_plan/remains") {
            return RawResponse::json(200, &quota_body(80));
        }
        RawResponse::json(429, r#"{"error":{"message":"slow down"}}"#)
    });
    let client = common::test_client("cn-key", &server.url);
    let request = ChatRequest::new("MiniMax-M2.5", vec![ChatMessage::user("hi")]);
    let err = client.chat(&request).await.unwrap_err();
    assert!(
        matches!(err, MinimaxError::RateLimited { .. }),
        "got {err:?}"
    );
    assert_eq!(server.count_path("chat/completions"), 4); // gate max_attempts
}

#[tokio::test]
async fn quota_guard_blocks_without_burning_requests() {
    let server = common::spawn(|req| {
        if req.path.ends_with("/v1/token_plan/remains") {
            // Zero remaining in the rolling window.
            return RawResponse::json(200, &quota_body(0));
        }
        RawResponse::json(
            200,
            r#"{"id":"x","choices":[],"base_resp":{"status_code":0,"status_msg":"success"}}"#,
        )
    });
    let client = common::test_client("cn-key", &server.url);
    let request = ChatRequest::new("MiniMax-M2.5", vec![ChatMessage::user("hi")]);
    let err = client.chat(&request).await.unwrap_err();
    match err {
        MinimaxError::QuotaExhausted { window_reset_at } => {
            assert!(window_reset_at.is_some());
        }
        other => panic!("expected QuotaExhausted, got {other:?}"),
    }
    // The chat endpoint was never hit: pause-and-resume costs no quota.
    assert_eq!(server.count_path("chat/completions"), 0);
}

#[tokio::test]
async fn quota_guard_can_be_disabled() {
    let server = common::spawn(|req| {
        if req.path.ends_with("/v1/token_plan/remains") {
            return RawResponse::json(200, &quota_body(0));
        }
        RawResponse::json(
            200,
            r#"{"id":"x","choices":[{"index":0,"message":{"role":"assistant","content":"ok"}}],"base_resp":{"status_code":0,"status_msg":"success"}}"#,
        )
    });
    let client = common::test_client("cn-key", &server.url).with_quota_guard(false);
    let request = ChatRequest::new("MiniMax-M2.5", vec![ChatMessage::user("hi")]);
    let resp = client.chat(&request).await.unwrap();
    assert_eq!(resp.text().as_deref(), Some("ok"));
}
