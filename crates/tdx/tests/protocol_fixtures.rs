//! 协议解析 golden 测试：基于录制自真实 tdx 服务器的响应字节
//! （`tests/fixtures/*.bin`，由 `live.rs::record_fixtures` 于 2026-08-22 录制，
//! 600519 数据与腾讯行情源交叉核对一致）。

use astock_tdx::protocol::parse::{
    parse_history_minute_time, parse_security_bars, parse_security_list, parse_security_quotes,
};

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn golden_bars_daily() {
    let bars = parse_security_bars(&fixture("bars_daily_600519.bin"), false).unwrap();
    assert_eq!(bars.len(), 100);

    let first = &bars[0];
    assert_eq!(first.datetime, "2026-03-30");
    assert!((first.open - 1407.0).abs() < 1e-9);
    assert!((first.close - 1420.0).abs() < 1e-9);
    assert!((first.high - 1431.0).abs() < 1e-9);
    assert!((first.low - 1402.52).abs() < 1e-9);
    assert!((first.vol - 2_868_459.0).abs() < 1.0);
    assert!((first.amount - 4_060_685_056.0).abs() < 1.0);

    let last = &bars[99];
    assert_eq!(last.datetime, "2026-08-21");
    assert!((last.open - 1291.5).abs() < 1e-9);
    assert!((last.close - 1272.83).abs() < 1e-9);
    assert!((last.high - 1291.5).abs() < 1e-9);
    assert!((last.low - 1272.01).abs() < 1e-9);
    assert!((last.vol - 3_347_231.0).abs() < 1.0);

    // 时间升序 + OHLC 关系
    for w in bars.windows(2) {
        assert!(w[0].datetime < w[1].datetime);
    }
    for b in &bars {
        assert!(b.high >= b.low && b.high >= b.open.max(b.close) && b.low <= b.open.min(b.close));
    }
}

#[test]
fn golden_bars_5min() {
    // 分钟级周期：位打包时间解码分支
    let bars = parse_security_bars(&fixture("bars_5min_600519.bin"), true).unwrap();
    assert_eq!(bars.len(), 100);
    assert_eq!(bars[0].datetime, "2026-08-19 14:45");
    assert!((bars[0].open - 1299.02).abs() < 1e-9);
    assert!((bars[0].close - 1298.57).abs() < 1e-9);
    assert_eq!(bars[99].datetime, "2026-08-21 15:00");
    assert!((bars[99].close - 1272.83).abs() < 1e-9);
    for w in bars.windows(2) {
        assert!(w[0].datetime < w[1].datetime);
    }
}

#[test]
fn golden_quotes() {
    let quotes = parse_security_quotes(&fixture("quotes_600519_000001.bin")).unwrap();
    assert_eq!(quotes.len(), 2);
    let q = &quotes[0];
    assert_eq!(q.code, "600519");
    assert_eq!(q.market, 1);
    assert!((q.price - 1272.83).abs() < 1e-9);
    assert!((q.last_close - 1291.5).abs() < 1e-9);
    assert!((q.open - 1291.5).abs() < 1e-9);
    // 五档：买一=收盘价（周末盘后），卖一略高
    assert!((q.bid[0].0 - 1272.83).abs() < 1e-9);
    assert!(q.ask[0].0 >= q.bid[0].0);
    assert_eq!(quotes[1].code, "000001");
    assert_eq!(quotes[1].market, 0);
}

#[test]
fn golden_minute() {
    let bars = parse_history_minute_time(&fixture("minute_600519.bin"), 1, "600519").unwrap();
    assert_eq!(bars.len(), 240);
    assert_eq!(bars[0].time, "09:31");
    assert_eq!(bars[119].time, "11:30");
    assert_eq!(bars[120].time, "13:01");
    assert_eq!(bars[239].time, "15:00");
    // 收盘价应与当日日线一致
    assert!((bars[239].price - 1272.83).abs() < 1e-9);
    // 均价应在当日价格区间内
    for b in &bars {
        assert!(b.price > 1200.0 && b.price < 1350.0, "bad price {:?}", b);
    }
}

#[test]
fn golden_security_list() {
    let list = parse_security_list(&fixture("list_sh_0.bin")).unwrap();
    assert_eq!(list.len(), 1000);
    assert_eq!(list[0].code, "999999");
    for s in &list {
        assert!(s.code.len() == 6 && s.code.bytes().all(|b| b.is_ascii_digit()));
        assert!(!s.name.is_empty());
    }
    // 第一页为指数段；GBK 名称解码 golden
    assert_eq!(list[0].name, "上证指数");
    assert_eq!(list[1].name, "Ａ股指数");
}
