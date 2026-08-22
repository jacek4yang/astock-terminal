//! Live smoke tests for the TDX adapter — `#[ignore]` by default; run with:
//!
//! ```sh
//! cargo test -p astock-market-data --test tdx_live -- --ignored --nocapture
//! ```
//!
//! Cross-checks the TDX feed against Tencent (kline close/volume) and
//! EastMoney (quote price) for 600519, and exercises the lazy pool init.

use astock_core::{Adjust, KlinePeriod, Source, Symbol, VolumeUnit};
use astock_market_data::{DataProvider, MarketData};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live network test (tdx TCP servers + tencent/eastmoney HTTP)"]
async fn tdx_daily_matches_tencent() {
    let md = MarketData::new();
    let sym = Symbol::new("600519").unwrap();

    // First call pays the 3–5s probe; the client is cached afterwards.
    let tdx = md
        .tdx
        .kline(&sym, KlinePeriod::Day, Adjust::None, 30)
        .await
        .expect("tdx kline failed");
    assert_eq!(tdx.source, Source::Tdx);
    assert!(tdx.data.len() >= 20, "too few bars: {}", tdx.data.len());
    assert!(tdx
        .data
        .iter()
        .all(|b| b.volume_unit == VolumeUnit::Lots && b.is_valid()));
    assert!(tdx.data.windows(2).all(|w| w[0].date < w[1].date));

    let tencent = md
        .tencent
        .kline(&sym, KlinePeriod::Day, Adjust::None, 30)
        .await
        .expect("tencent kline failed");

    // Align by date and cross-check close (exact to the cent) and volume
    // (手; allow 0.5% — feeds snapshot at slightly different times).
    let tdx_last = tdx.data.last().unwrap();
    let tc = tencent
        .data
        .iter()
        .find(|b| b.date == tdx_last.date)
        .expect("tencent missing tdx last date");
    assert!(
        (tdx_last.close - tc.close).abs() < 0.01,
        "close mismatch: tdx={} tencent={}",
        tdx_last.close,
        tc.close
    );
    assert!(
        (tdx_last.volume - tc.volume).abs() / tc.volume.max(1.0) < 0.005,
        "volume(手) mismatch: tdx={} tencent={}",
        tdx_last.volume,
        tc.volume
    );
    println!(
        "600519 {} tdx close={} vol={}手 | tencent close={} vol={}手",
        tdx_last.date, tdx_last.close, tdx_last.volume, tc.close, tc.volume
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live network test (tdx TCP servers + eastmoney HTTP)"]
async fn tdx_quote_matches_eastmoney() {
    let md = MarketData::new();
    let sym = Symbol::new("600519").unwrap();

    let tdx = md.tdx.quote(&sym).await.expect("tdx quote failed");
    assert_eq!(tdx.source, Source::Tdx);
    let em = md
        .eastmoney
        .quote(&sym)
        .await
        .expect("eastmoney quote failed");

    // Off-market both feeds report the last close; during the session prices
    // can tick apart, so only a loose band is asserted.
    assert!(
        (tdx.data.price - em.data.price).abs() / em.data.price.max(1.0) < 0.01,
        "price mismatch: tdx={} em={}",
        tdx.data.price,
        em.data.price
    );
    assert!(
        (tdx.data.pre_close - em.data.pre_close).abs() < 0.01,
        "pre_close mismatch: tdx={} em={}",
        tdx.data.pre_close,
        em.data.pre_close
    );
    println!(
        "600519 quote tdx price={} pre_close={} vol={} | em price={} pre_close={}",
        tdx.data.price, tdx.data.pre_close, tdx.data.volume, em.data.price, em.data.pre_close
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live network test (tdx TCP servers)"]
async fn tdx_all_a_shares_filtered() {
    let md = MarketData::new();
    let list = md
        .tdx
        .all_a_shares()
        .await
        .expect("tdx all_a_shares failed");
    assert_eq!(list.source, Source::Tdx);
    assert!(list.data.len() > 4000, "too few: {}", list.data.len());
    // 号段过滤生效：只剩沪深 A 股。
    assert!(list.data.iter().all(|s| {
        s.code.starts_with("60")
            || s.code.starts_with("68")
            || s.code.starts_with("00")
            || s.code.starts_with("30")
    }));
    assert!(list.data.iter().any(|s| s.code == "600519"));
    assert!(list.data.iter().all(|s| !s.name.is_empty()));
    println!("tdx 全A: {} 只", list.data.len());
}
