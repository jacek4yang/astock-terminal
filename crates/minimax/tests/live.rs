//! Live smoke test against the real MiniMax services.
//!
//! Run explicitly with the key in the environment:
//!
//! ```sh
//! MINIMAX_TEST_KEY=sk-... cargo test -p astock-minimax --test live -- --ignored --nocapture
//! ```
//!
//! The key is read from `MINIMAX_TEST_KEY` and never printed or logged.

use astock_minimax::{ChatMessage, ChatRequest, MinimaxClient, MinimaxError, SecretKey};

#[tokio::test]
#[ignore = "requires MINIMAX_TEST_KEY and network access"]
async fn live_detect_and_chat() {
    let Ok(raw) = std::env::var("MINIMAX_TEST_KEY") else {
        eprintln!("MINIMAX_TEST_KEY not set; skipping live test");
        return;
    };
    let key = SecretKey::new(raw);
    let masked = format!("{key}");

    let client = MinimaxClient::new(key);

    let service = client.detect_service().await.expect("detect service");
    eprintln!(
        "[{masked}] region: {:?}, api: {}",
        service.region, service.api_host
    );

    let quota = client.quota().await.expect("quota");
    for m in &quota.models {
        eprintln!(
            "quota {}: interval {:?}% left, weekly {:?}% left",
            m.model_name, m.current_interval_remaining_percent, m.current_weekly_remaining_percent
        );
    }

    let model = client.selected_model().await.expect("select model");
    eprintln!("selected model: {model}");

    let request = ChatRequest::new(&model, vec![ChatMessage::user("Reply with exactly: pong")])
        .with_max_tokens(64);
    match client.chat(&request).await {
        Ok(resp) => {
            eprintln!("chat text: {:?}", resp.text());
            eprintln!("reasoning: {:?}", resp.reasoning());
            eprintln!("usage: {:?}", resp.usage);
        }
        Err(MinimaxError::QuotaExhausted { window_reset_at }) => {
            eprintln!("quota exhausted, resets at {window_reset_at:?}; chat skipped");
        }
        Err(e) => panic!("live chat failed: {e}"),
    }
}
