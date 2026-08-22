//! Live tests for the optional token-gated providers (Tushare pro, iwencai
//! OpenAPI). All gated behind `#[ignore]` and the respective env vars; run
//! with:
//! `TUSHARE_TOKEN=... cargo test -p astock-market-data -- --ignored tushare`
//! `IWENCAI_KEY=...  cargo test -p astock-market-data -- --ignored iwencai`
//!
//! Without credentials every test returns early (passes vacuously).

use astock_core::{Bar, Symbol, VolumeUnit};
use astock_market_data::providers::tushare::{compare_qfq_golden, TushareTier};
use astock_market_data::{IwencaiOpenApi, MarketData, TushareProvider};
use chrono::NaiveDate;

fn tushare(md: &MarketData) -> Option<&TushareProvider> {
    md.tushare.available().then_some(&md.tushare)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live network test"]
async fn tushare_daily_raw_bars() {
    let md = MarketData::new();
    let Some(p) = tushare(&md) else {
        eprintln!("TUSHARE_TOKEN unset; skipping");
        return;
    };
    let sym = Symbol::new("600519").unwrap();
    let fetched = p
        .daily(
            &sym,
            NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 8, 21).unwrap(),
        )
        .await
        .expect("tushare daily failed");
    println!(
        "tushare daily(600519): bars={} first={} last={}",
        fetched.data.len(),
        fetched.data.first().unwrap().date,
        fetched.data.last().unwrap().date
    );
    assert!(fetched.data.len() > 10);
    assert!(fetched
        .data
        .iter()
        .all(|b| b.volume_unit == VolumeUnit::Lots && b.is_valid()));
    // Amount must be in yuan (thousand-CNY × 1000): 茅台日成交额以亿元计.
    let last = fetched.data.last().unwrap();
    assert!(last.amount.unwrap_or(0.0) > 1e8, "amount not in yuan");
    // Ascending dates.
    assert!(fetched.data.windows(2).all(|w| w[0].date < w[1].date));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live network test"]
async fn tushare_tier_probe_and_pro_apis() {
    let md = MarketData::new();
    let Some(p) = tushare(&md) else {
        eprintln!("TUSHARE_TOKEN unset; skipping");
        return;
    };
    let tier = p.detect_tier().await.expect("tier probe failed");
    println!("tushare tier: {tier:?}");
    // Health panel must list tushare as available.
    let h = md
        .provider_health()
        .into_iter()
        .find(|h| h.name == "tushare")
        .expect("tushare missing from health panel");
    assert!(h.available);

    if tier != TushareTier::Pro2000 {
        eprintln!("free 120 tier; skipping 2000-tier API checks");
        return;
    }

    let sym = Symbol::new("600519").unwrap();
    let start = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
    let end = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();

    let adj = p.adj_factor(&sym, start, end).await.expect("adj_factor");
    assert!(!adj.is_empty());
    assert!(adj.iter().all(|a| a.factor > 0.0));

    let days = p.trade_cal(start, end).await.expect("trade_cal");
    assert!(days.iter().any(|d| d.is_open));

    let basics = p.daily_basic(&sym, start, end).await.expect("daily_basic");
    assert!(basics.iter().any(|b| b.total_mv.unwrap_or(0.0) > 1e11));

    // Golden cross-check: tushare adj_factor vs the core adjust engine fed
    // with tushare's own dividend rows.
    let raw: Vec<Bar> = p.daily(&sym, start, end).await.unwrap().data;
    let actions = p.dividend(&sym).await.expect("dividend");
    let mismatches = compare_qfq_golden(&raw, &adj, &actions, 0.005);
    assert!(
        mismatches.len() <= raw.len() / 20,
        "too many golden mismatches: {mismatches:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live network test"]
async fn iwencai_dragon_tiger_and_events() {
    let md = MarketData::new();
    let p: &IwencaiOpenApi = &md.iwencai;
    if !p.available() {
        eprintln!("IWENCAI_KEY unset; skipping");
        return;
    }
    let lhb = p.dragon_tiger("20260821").await.expect("dragon_tiger");
    println!(
        "iwencai 龙虎榜: rows={} total={:?}",
        lhb.rows.len(),
        lhb.total
    );
    assert!(!lhb.rows.is_empty());

    let events = p.stock_events("贵州茅台").await.expect("stock_events");
    println!(
        "iwencai 消息面: announcements={} news={} events={}",
        events.announcements.rows.len(),
        events.news.rows.len(),
        events.events.rows.len()
    );

    let boards = p.sector_membership("贵州茅台").await.expect("sector");
    assert!(!boards.rows.is_empty());
}
