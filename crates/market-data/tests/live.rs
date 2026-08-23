//! Live smoke tests against the real upstreams. Gated behind `#[ignore]`;
//! run with:
//! `cargo test -p astock-market-data -- --ignored live`
//!
//! Sequential and low-volume on purpose: be polite to the public endpoints.

use astock_core::{Adjust, KlinePeriod, Symbol};
use astock_market_data::{DataProvider, MarketData};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live network test"]
async fn live_kline_quote_search_breadth() {
    let md = MarketData::new();
    let sym = Symbol::new("600519").unwrap();

    // --- Kline: Tencent → Sina → EastMoney failover + enrichment ---
    let kline = md
        .kline(&sym, KlinePeriod::Day, Adjust::Qfq, 60)
        .await
        .expect("kline fetch failed");
    println!(
        "kline(600519, day, qfq, 60): source={} bars={} first={} last={} amount_on_last={:?} turnover_on_last={:?}",
        kline.source,
        kline.data.len(),
        kline.data.first().unwrap().date,
        kline.data.last().unwrap().date,
        kline.data.last().unwrap().amount,
        kline.data.last().unwrap().turnover,
    );
    assert!(kline.data.len() >= 30, "too few bars: {}", kline.data.len());
    let last = kline.data.last().unwrap();
    assert!(last.close > 0.0 && last.high >= last.low);
    // Non-EM sources must have been enriched with amount/turnover.
    if kline.source != astock_core::Source::EastMoney {
        assert!(
            kline.data.iter().filter(|b| b.amount.is_some()).count() > 20,
            "enrichment did not merge amounts"
        );
    }

    // --- Weekly kline (exercises the period-matched enrichment fix) ---
    let weekly = md
        .kline(&sym, KlinePeriod::Week, Adjust::Qfq, 30)
        .await
        .expect("weekly kline fetch failed");
    println!(
        "kline(600519, week, qfq, 30): source={} bars={}",
        weekly.source,
        weekly.data.len()
    );
    assert!(weekly.data.len() >= 10);

    // --- Quote ---
    let quote = md.quote(&sym).await.expect("quote fetch failed");
    println!(
        "quote(600519): source={} name={} price={} pct={} pre_close={}",
        quote.source, quote.data.name, quote.data.price, quote.data.pct, quote.data.pre_close
    );
    assert!(quote.data.price > 0.0, "quote price is zero");
    assert!(!quote.data.name.is_empty(), "quote name empty");

    // --- Search: keyword and numeric short-circuit ---
    let hits = md.search("茅台").await.expect("search failed");
    println!(
        "search(茅台): source={} hits={:?}",
        hits.source,
        hits.data
            .iter()
            .map(|h| format!("{} {}", h.code, h.name))
            .collect::<Vec<_>>()
    );
    assert!(
        hits.data.iter().any(|h| h.code == "600519"),
        "600519 not in search hits"
    );
    let numeric = md.search("600519").await.expect("numeric search failed");
    assert_eq!(numeric.data[0].code, "600519");
    assert_eq!(numeric.data[0].name, "贵州茅台");

    // --- Market breadth ---
    let breadth = md.market_breadth().await.expect("breadth fetch failed");
    println!(
        "market breadth: source={} up={} down={} flat={} total={} ratio={:.3}",
        breadth.source,
        breadth.data.up,
        breadth.data.down,
        breadth.data.flat,
        breadth.data.total,
        breadth.data.ratio()
    );
    assert!(
        breadth.data.total > 4000,
        "breadth total suspiciously small"
    );

    // --- Index kline ---
    let index = md
        .index_kline("1.000001", 30)
        .await
        .expect("index kline fetch failed");
    println!(
        "index kline(1.000001): source={} bars={} last_close={} last_pct={:?}",
        index.source,
        index.data.len(),
        index.data.last().unwrap().close,
        index.data.last().unwrap().pct,
    );
    println!(
        "index tail={:?}",
        index
            .data
            .iter()
            .rev()
            .take(3)
            .map(|bar| (bar.date, bar.close, bar.pct))
            .collect::<Vec<_>>()
    );
    assert!(index.data.len() >= 10);
    assert!(index.data.last().unwrap().close > 1000.0);

    // --- Fund flow daily (lenient: endpoint occasionally rate-limits) ---
    match md.fund_flow_daily(&sym, 10).await {
        Ok(flow) => {
            println!(
                "fund_flow_daily(600519, 10): source={} rows={} last_main_net={}",
                flow.source,
                flow.data.len(),
                flow.data.last().unwrap().main_net
            );
            assert!(flow.data.len() >= 3);
        }
        Err(e) => println!("fund_flow_daily unavailable (non-fatal): {e}"),
    }

    println!("live smoke test passed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live network test"]
async fn live_csi300_uses_index_market_identity() {
    let md = MarketData::new();
    let csi300 = Symbol::new("000300").unwrap();
    assert!(csi300.is_unambiguous_index());

    // Call the ordinary Agent-facing APIs. They must internally route to
    // SH index id 1.000300 and unadjusted index bars, not SZ stock 0.000300.
    let quote = md.quote(&csi300).await.expect("CSI 300 quote failed");
    assert_eq!(quote.data.name, "沪深300");
    assert!(quote.data.price > 1_000.0);

    for period in [KlinePeriod::Day, KlinePeriod::Week, KlinePeriod::Month] {
        let bars = md
            .kline(&csi300, period, Adjust::Qfq, 30)
            .await
            .unwrap_or_else(|error| panic!("CSI 300 {period:?} kline failed: {error}"));
        assert!(bars.data.len() >= 10, "too few {period:?} index bars");
        assert!(bars.data.last().is_some_and(|bar| bar.close > 1_000.0));
    }
}

/// 复权正确性守护: the two qfq sources (Tencent `qfqday` and EastMoney
/// `fqt=1`) must agree on 600519, and neither qfq series may contain
/// adjustment gaps that look like >11% single-day moves on a ±10% main-board
/// stock.
///
/// `DataProvider::kline(Adjust::Qfq)` maps to `fqt=1` on the EastMoney side.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live network test"]
async fn qfq_consistency_600519() {
    use astock_core::{Bar, DataError};
    use astock_market_data::TencentKline;
    use std::collections::HashMap;

    /// Days where the qfq close-to-close move exceeds the main-board limit
    /// (10% + 1% rounding slack). In a correct qfq series these only appear
    /// via adjustment bugs — real ex-dividend gaps are adjusted away.
    fn pct_violations(bars: &[Bar]) -> Vec<String> {
        let mut out = Vec::new();
        for w in bars.windows(2) {
            let (prev, cur) = (&w[0], &w[1]);
            if prev.close <= 0.0 {
                continue;
            }
            let pct = (cur.close - prev.close) / prev.close * 100.0;
            if pct.abs() > 11.0 {
                out.push(format!(
                    "{} -> {}: close {:.3} -> {:.3} ({pct:+.2}%)",
                    prev.date, cur.date, prev.close, cur.close
                ));
            }
        }
        out
    }

    let md = MarketData::new();
    let sym = Symbol::new("600519").unwrap();

    // --- EastMoney fqt=1 qfq (the reliable side) ---
    let em = md
        .eastmoney
        .kline(&sym, KlinePeriod::Day, Adjust::Qfq, 120)
        .await
        .expect("eastmoney qfq kline failed");
    println!(
        "EM qfq(600519, 120): bars={} last={} close={}",
        em.data.len(),
        em.data.last().unwrap().date,
        em.data.last().unwrap().close
    );
    assert!(em.data.len() >= 60, "EM returned too few bars");
    let em_violations = pct_violations(&em.data);
    for v in &em_violations {
        println!("EM qfq suspicious adjustment gap: {v}");
    }
    assert!(
        em_violations.is_empty(),
        "EM qfq series has |pct|>11% days (printed above)"
    );

    // --- Tencent qfq (skip the comparison when the WAF challenge is up) ---
    let tencent = TencentKline::new(md.http.clone());
    let tc = match tencent
        .kline(&sym, KlinePeriod::Day, Adjust::Qfq, 120)
        .await
    {
        Ok(f) => f.data,
        Err(DataError::WafBlocked(e)) => {
            println!("tencent WAF-blocked ({e}); cross-source comparison skipped");
            return;
        }
        Err(e) => panic!("tencent qfq kline failed unexpectedly: {e}"),
    };
    println!(
        "Tencent qfq(600519, 120): bars={} last={} close={}",
        tc.len(),
        tc.last().unwrap().date,
        tc.last().unwrap().close
    );
    let tc_violations = pct_violations(&tc);
    for v in &tc_violations {
        println!("Tencent qfq suspicious adjustment gap: {v}");
    }
    assert!(
        tc_violations.is_empty(),
        "Tencent qfq series has |pct|>11% days (printed above)"
    );

    // --- Cross-source agreement on the last 60 overlapping dates ---
    let em_close: HashMap<_, f64> = em.data.iter().map(|b| (b.date, b.close)).collect();
    let mut overlap: Vec<_> = tc
        .iter()
        .filter_map(|b| em_close.get(&b.date).map(|&e| (b.date, b.close, e)))
        .collect();
    overlap.sort_by_key(|(d, _, _)| *d);
    assert!(
        overlap.len() >= 30,
        "only {} overlapping dates — inconclusive",
        overlap.len()
    );
    let n = overlap.len().min(60);
    for (date, tc_close, em_close) in &overlap[overlap.len() - n..] {
        let diff = (tc_close - em_close).abs() / em_close;
        assert!(
            diff <= 0.005,
            "{date}: tencent close {tc_close} vs EM close {em_close} differ {:.3}%",
            diff * 100.0
        );
    }
    println!("qfq consistency OK: last {n} overlapping dates agree within 0.5%");
}
