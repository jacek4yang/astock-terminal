//! Live MiniMax integration test (ignored by default).
//!
//! Run with: `MINIMAX_TEST_KEY=... cargo test -p astock-agent --test live -- --ignored`
//! The key is read from the environment only — never hardcoded or logged.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use astock_agent::testing::{EchoTool, NoopMarket};
use astock_agent::{
    AgentEngine, AgentEvent, AgentTool, EngineConfig, TaskSpec, ToolContext, ToolRegistry,
};
use astock_market_data::MarketData;
use astock_minimax::{MinimaxClient, SecretKey};
use astock_storage::{Storage, StorageConfig};
use futures::StreamExt;
use serde_json::json;

#[tokio::test]
#[ignore = "live market scan: hits configured public market-data endpoints"]
async fn live_full_width_scan_warms_and_reuses_cache() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
    let progress = Arc::new(Mutex::new(Vec::new()));
    let progress_sink = progress.clone();
    let ctx = ToolContext::new(Arc::new(MarketData::new()), storage).with_progress_reporter(
        Arc::new(move |detail| {
            progress_sink.lock().unwrap().push(detail);
        }),
    );
    let registry = astock_agent::default_registry();
    let args = json!({"top": 12, "candidates": 80, "mode": "interactive"});

    let started = Instant::now();
    let first = registry
        .dispatch("scan_market", args.clone(), &ctx)
        .await
        .expect("live full-width scan failed");
    let first_elapsed = started.elapsed();
    eprintln!(
        "first_scan elapsed_ms={} summary={}",
        first_elapsed.as_millis(),
        first.summary_json
    );
    assert_eq!(first.summary_json["effective_candidates"], json!(80));
    assert_eq!(first.summary_json["history_bars"], json!(250));
    assert_eq!(
        first.summary_json["coverage"]["completed"],
        first.summary_json["coverage"]["candidate_pool"]
    );

    let started = Instant::now();
    let second = registry
        .dispatch("scan_market", args, &ctx)
        .await
        .expect("cached live scan failed");
    let second_elapsed = started.elapsed();
    eprintln!(
        "cached_scan elapsed_ms={} cache_hits={} upstream_fetches={}",
        second_elapsed.as_millis(),
        second.summary_json["cache_hits"],
        second.summary_json["new_upstream_fetches"]
    );
    assert_eq!(second.summary_json["new_upstream_fetches"], json!(0));
    assert!(second.summary_json["cache_hits"].as_u64().unwrap_or(0) > 0);
    assert!(progress.lock().unwrap().last().is_some());
}

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
        joinquant: None,
        minimax_search: None,
        finance_news: None,
        global_assets: None,
        iwencai: None,
        progress: None,
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
        joinquant: None,
        minimax_search: None,
        finance_news: None,
        global_assets: None,
        iwencai: None,
        progress: None,
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

#[tokio::test]
#[ignore = "requires the desktop app's stored MiniMax key and public data endpoints"]
async fn live_gold_research_uses_real_sources_and_publishes_clean_chinese() {
    let Some(key) = astock_minimax::KeyStore::new()
        .load_key()
        .expect("load the desktop MiniMax credential")
    else {
        eprintln!("desktop MiniMax key not configured; skipping");
        return;
    };
    let client = Arc::new(MinimaxClient::new(key));
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
    let market = Arc::new(MarketData::with_storage(storage.clone()));
    let ctx = ToolContext {
        market: market.clone(),
        storage,
        graph: None,
        fundamental: None,
        joinquant: None,
        minimax_search: Some(client.clone()),
        finance_news: Some(market.finance_news.clone()),
        global_assets: Some(market.global_assets.clone()),
        iwencai: None,
        progress: None,
    };
    let registry = astock_agent::default_registry();
    let source_result = registry
        .dispatch(
            "research_gold_market",
            json!({
                "days": 90,
                "news_limit": 30,
                "include_official_search": false
            }),
            &ctx,
        )
        .await
        .expect("gold source aggregation should succeed");
    assert!(
        source_result.summary_json["gold_market"]["quotes"]
            .as_array()
            .is_some_and(|quotes| !quotes.is_empty()),
        "gold research must include a live exchange quote"
    );
    assert!(
        source_result.summary_json["gold_market"]["trend"]["observations"]
            .as_u64()
            .is_some_and(|observations| observations >= 20),
        "gold research must include a usable trend window"
    );
    assert!(
        source_result.summary_json["gold_news"]
            .as_array()
            .is_some_and(|headlines| !headlines.is_empty()),
        "gold research must include at least one matching headline"
    );

    let engine = AgentEngine::new(
        client,
        registry,
        ctx,
        EngineConfig {
            max_rounds: 8,
            ..Default::default()
        },
    );
    let spec = TaskSpec::new(
        "live-gold-research",
        "gold-research",
        "请分析当前黄金近20个交易日走势、近期黄金利好与利空，并说明对紫金矿业和山东黄金的可能影响。必须先使用黄金行情、财经新闻与官方机构信息，区分事实、推断和未知，用通俗中文回答。",
    )
    .with_run_options(
        "deep",
        "maximum",
        vec!["research_gold_market".into()],
        false,
    );

    let events: Vec<AgentEvent> = engine.run_task(spec).collect().await;
    let gold_finished = events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::ToolCallFinished {
                name,
                success: true,
                ..
            } if name == "research_gold_market"
        )
    });
    assert!(
        gold_finished,
        "the Agent must complete the dedicated gold research tool"
    );

    let answer = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::Completed { report } => Some(report.answer.as_str()),
            _ => None,
        })
        .expect("gold research task should complete");
    assert!(answer.contains("黄金"));
    for forbidden in [
        "证据:ev",
        "evf_",
        "calc_",
        "research_gold_market",
        "research_news",
        "research_global_transmission",
        "status=no_match",
        "total_documents=",
        "API_KEY",
        "_TOKEN",
        "_SECRET",
    ] {
        assert!(
            !answer.contains(forbidden),
            "public answer leaked internal marker: {forbidden}"
        );
    }
    eprintln!(
        "gold Agent smoke test completed; public_answer_chars={}",
        answer.chars().count()
    );
}
