//! Live MiniMax integration test (ignored by default).
//!
//! Run with: `MINIMAX_TEST_KEY=... cargo test -p astock-agent --test live -- --ignored`
//! The key is read from the environment only — never hardcoded or logged.

use std::sync::Arc;

use astock_agent::testing::{EchoTool, NoopMarket};
use astock_agent::{
    AgentEngine, AgentEvent, AgentTool, EngineConfig, TaskSpec, ToolContext, ToolRegistry,
};
use astock_market_data::MarketData;
use astock_minimax::{MinimaxClient, SecretKey};
use astock_storage::{Storage, StorageConfig};
use futures::StreamExt;

#[tokio::test]
#[ignore = "requires MINIMAX_TEST_KEY and network access"]
async fn live_one_tool_conversation() {
    let Ok(key) = std::env::var("MINIMAX_TEST_KEY") else {
        eprintln!("MINIMAX_TEST_KEY not set; skipping");
        return;
    };
    let client = MinimaxClient::new(SecretKey::new(key));
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
    let ctx = ToolContext {
        market: Arc::new(NoopMarket),
        storage,
        graph: None,
        fundamental: None,
    };
    let registry = ToolRegistry::new(vec![Arc::new(EchoTool::new()) as Arc<dyn AgentTool>]);
    let engine = AgentEngine::new(
        Arc::new(client),
        registry,
        ctx,
        EngineConfig {
            max_rounds: 5,
            ..Default::default()
        },
    );

    let spec = TaskSpec::new(
        "live-1",
        "live-test",
        "调用 echo 工具（text 参数填 \"ping\"），然后用一句话告诉我回显结果。",
    );
    let events: Vec<AgentEvent> = engine.run_task(spec).collect().await;
    for event in &events {
        // Never print the key; events carry no secrets.
        eprintln!("event: {}", serde_json::to_string(event).unwrap());
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCallFinished { name, .. } if name == "echo")),
        "model should call the echo tool"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Completed { .. })),
        "task should complete"
    );
}

#[tokio::test]
#[ignore = "requires MINIMAX_TEST_KEY and network access"]
async fn live_market_data_tool_conversation() {
    let Ok(key) = std::env::var("MINIMAX_TEST_KEY") else {
        eprintln!("MINIMAX_TEST_KEY not set; skipping");
        return;
    };
    let client = MinimaxClient::new(SecretKey::new(key));
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
    let ctx = ToolContext {
        market: Arc::new(MarketData::new()),
        storage,
        graph: None,
        fundamental: None,
    };
    let engine = AgentEngine::new(
        Arc::new(client),
        astock_agent::default_registry(),
        ctx,
        EngineConfig {
            max_rounds: 5,
            ..Default::default()
        },
    );
    let spec = TaskSpec::new(
        "live-2",
        "live-test",
        "用 get_quote 查询 600519 的实时行情，并一句话总结最新价和涨跌幅。",
    );
    let events: Vec<AgentEvent> = engine.run_task(spec).collect().await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCallFinished { name, .. } if name == "get_quote")),
        "model should call get_quote"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Completed { .. })),
        "task should complete"
    );
}
