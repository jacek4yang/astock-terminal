//! Explicit live regression for the EastMoney historical HTTPS cluster.
//! It is ignored in deterministic CI and contains no account credential.

use astock_core::{Adjust, KlinePeriod, Symbol};
use astock_market_data::{DataProvider, EastMoney, HttpClient, TtlCache};
use std::sync::Arc;

#[tokio::test]
#[ignore = "hits the live EastMoney public kline endpoint"]
async fn eastmoney_https_kline_returns_rows() {
    let provider = EastMoney::new(Arc::new(HttpClient::new()), Arc::new(TtlCache::default()));
    let symbol = Symbol::new("000725").unwrap();
    let fetched = provider
        .kline(&symbol, KlinePeriod::Day, Adjust::None, 5)
        .await
        .unwrap_or_else(|error| panic!("EastMoney host pool failed: {error:#?}"));
    assert_eq!(fetched.data.len(), 5);
    assert!(fetched
        .data
        .windows(2)
        .all(|rows| rows[0].date < rows[1].date));
}
