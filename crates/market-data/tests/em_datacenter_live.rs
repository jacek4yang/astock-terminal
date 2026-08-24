use astock_market_data::providers::EmDataCenter;
use astock_market_data::{HttpClient, TtlCache};
use chrono::NaiveDate;
use std::sync::Arc;

fn provider() -> EmDataCenter {
    EmDataCenter::new(Arc::new(HttpClient::new()), Arc::new(TtlCache::default()))
}

/// Manual upstream contract check. It is ignored in normal CI because it
/// depends on EastMoney's public service, but the release data audit runs it.
#[tokio::test]
#[ignore = "live EastMoney datacenter contract"]
async fn symbol_filtered_unlocks_match_global_rows() {
    let provider = provider();
    let start = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
    let end = NaiveDate::from_ymd_opt(2027, 8, 31).unwrap();
    let global = provider
        .lift_stage(start, end, 2)
        .await
        .expect("global unlock report should be reachable");
    let sample = global
        .data
        .iter()
        .find(|row| !row.code.is_empty())
        .expect("unlock window should contain a sample row");
    let scoped = provider
        .lift_stage_for_symbol(&sample.code, start, end, 2)
        .await
        .expect("symbol-filtered unlock report should be reachable");
    assert!(!scoped.data.is_empty());
    assert!(scoped.data.iter().all(|row| row.code == sample.code));
}
