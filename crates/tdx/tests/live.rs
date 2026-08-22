//! 实盘集成测试（需要网络，默认 `#[ignore]`）。
//!
//! 运行：cargo test -p astock-tdx --test live -- --ignored --nocapture
//!
//! - `live_probe_and_pool`：探测选路 + 连接池启动 + 600519 日线/五档/分时/列表冒烟
//! - `record_fixtures`：录制真实响应字节到 `tests/fixtures/`（供离线 golden 测试）

use std::path::PathBuf;
use std::time::Duration;

use astock_tdx::pool::{probe_servers, PoolConfig};
use astock_tdx::protocol::constants::MARKET_SH;
use astock_tdx::protocol::frame::{
    build_history_minute_packet, build_security_bars_packet, build_security_list_packet,
    build_security_quotes_packet,
};
use astock_tdx::servers::{Server, ALL_SERVERS, PRIMARY_SERVERS};
use astock_tdx::{KlineCategory, ServerPool, TdxClient};

/// 最近一个交易日（2026-08-21 周五；本测试编写于 2026-08-22 周六）。
const LAST_TRADE_DATE: u32 = 20_260_821;

fn test_config() -> PoolConfig {
    PoolConfig {
        timeout: Duration::from_secs(2),
        deep_probe_limit: 10,
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires live network to tdx servers"]
async fn live_probe_and_pool() {
    let client = TdxClient::new(
        ServerPool::start(test_config())
            .await
            .expect("no tdx server reachable"),
    );
    let active = client.pool().active_servers().await;
    println!("active servers: {active:?}");
    assert!(!active.is_empty());

    // 600519 日线（未复权）
    let bars = client
        .kline(MARKET_SH, "600519", KlineCategory::Daily, 100)
        .await
        .expect("kline failed");
    assert!(bars.len() >= 90, "too few bars: {}", bars.len());
    let last = bars.last().unwrap();
    println!(
        "600519 daily last: {} O={} C={} H={} L={} V={}",
        last.datetime, last.open, last.close, last.high, last.low, last.vol
    );
    // 价格量级校验：茅台股价在百~千量级；OHLC 关系
    assert!(last.close > 100.0 && last.close < 10000.0);
    assert!(last.high >= last.low);
    assert!(last.high >= last.open.max(last.close));
    assert!(last.low <= last.open.min(last.close));

    // 周线/月线/5分钟各取一页
    for cat in [
        KlineCategory::Weekly,
        KlineCategory::Monthly,
        KlineCategory::FiveMin,
    ] {
        let bars = client
            .kline(MARKET_SH, "600519", cat, 10)
            .await
            .expect("kline category failed");
        assert!(!bars.is_empty(), "empty bars for {cat:?}");
        println!("{cat:?} last: {:?}", bars.last().map(|b| &b.datetime));
    }

    // 五档快照
    let quotes = client
        .quotes(&[(1, "600519"), (0, "000001")])
        .await
        .expect("quotes failed");
    assert_eq!(quotes.len(), 2);
    let q = &quotes[0];
    println!(
        "600519 quote: price={} last_close={} bid1={:?} ask1={:?}",
        q.price, q.last_close, q.bid[0], q.ask[0]
    );
    assert_eq!(q.code, "600519");
    assert!(q.last_close > 100.0 && q.last_close < 10000.0);

    // 分时（最近交易日，绕开 0x051D 走 0x0FB4）
    let minutes = client
        .history_minute(MARKET_SH, "600519", LAST_TRADE_DATE)
        .await
        .expect("minute failed");
    assert!(
        minutes.len() > 200,
        "too few minute bars: {}",
        minutes.len()
    );
    assert_eq!(minutes.first().unwrap().time, "09:31");
    assert_eq!(minutes.last().unwrap().time, "15:00");
    println!(
        "600519 minute last: {:?} price={}",
        minutes.last().map(|m| &m.time),
        minutes.last().map(|m| m.price).unwrap_or(0.0)
    );

    // 证券列表
    let count = client
        .security_count(MARKET_SH)
        .await
        .expect("count failed");
    let list = client
        .security_list_page(MARKET_SH, 0)
        .await
        .expect("list failed");
    assert!(count > 1000, "sh count {count}");
    assert_eq!(list.len(), 1000);
    println!("sh count={count} first={:?}", list.first().map(|s| &s.code));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires live network to tdx servers"]
async fn record_fixtures() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    std::fs::create_dir_all(&dir).unwrap();

    // 只探测 PRIMARY + 前 40 台即可，录夹具不需要全量
    let mut candidates: Vec<Server> = PRIMARY_SERVERS.to_vec();
    for &(name, ip, port) in ALL_SERVERS.iter().take(40) {
        let s = Server { name, ip, port };
        if !candidates.contains(&s) {
            candidates.push(s);
        }
    }
    let probed = probe_servers(&candidates, Duration::from_secs(2), 5).await;
    assert!(!probed.is_empty(), "no tdx server reachable");
    let server = probed[0].server;
    println!("recording via {} ({})", server.ip, server.name);

    let pool = ServerPool::start(PoolConfig {
        timeout: Duration::from_secs(3),
        pool_size: 1,
        deep_probe_limit: 5,
        extra_servers: vec![server],
        ..Default::default()
    })
    .await
    .expect("pool start failed");

    // 日线 600519 一页
    let body = pool
        .request(&build_security_bars_packet(
            KlineCategory::Daily.as_u8(),
            MARKET_SH,
            "600519",
            0,
            100,
        ))
        .await
        .unwrap();
    std::fs::write(dir.join("bars_daily_600519.bin"), &body).unwrap();

    // 5 分钟线一页
    let body = pool
        .request(&build_security_bars_packet(
            KlineCategory::FiveMin.as_u8(),
            MARKET_SH,
            "600519",
            0,
            100,
        ))
        .await
        .unwrap();
    std::fs::write(dir.join("bars_5min_600519.bin"), &body).unwrap();

    // 五档快照
    let body = pool
        .request(&build_security_quotes_packet(&[
            (1, "600519"),
            (0, "000001"),
        ]))
        .await
        .unwrap();
    std::fs::write(dir.join("quotes_600519_000001.bin"), &body).unwrap();

    // 证券列表第一页
    let body = pool
        .request(&build_security_list_packet(MARKET_SH, 0))
        .await
        .unwrap();
    std::fs::write(dir.join("list_sh_0.bin"), &body).unwrap();

    // 历史分时（最近交易日）
    let body = pool
        .request(&build_history_minute_packet(
            MARKET_SH,
            "600519",
            LAST_TRADE_DATE,
        ))
        .await
        .unwrap();
    std::fs::write(dir.join("minute_600519.bin"), &body).unwrap();

    println!("fixtures recorded to {}", dir.display());
}
