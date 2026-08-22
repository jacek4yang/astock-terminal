//! Hardening tests: single-flight coalescing, circuit-breaker integration
//! in the hub failover chain, and cache-TTL sanity for failures.
//!
//! All tests run against stub providers; no network is touched.

use astock_core::{Adjust, Bar, DataError, Fetched, KlinePeriod, Source, Symbol, VolumeUnit};
use astock_market_data::{BreakerConfig, CircuitState, DataProvider, MarketData};
use async_trait::async_trait;
use chrono::NaiveDate;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
enum Behavior {
    /// Answer with this many synthetic bars (< 10 → the hub's early-return
    /// path: no validation, no EM enrichment, no network at all).
    Bars(usize),
    /// Answer with an empty payload (the push2delay-style miss).
    Empty,
    /// Fail with the given error.
    Fail(DataError),
}

struct StubProvider {
    name: &'static str,
    calls: Arc<AtomicUsize>,
    delay: Duration,
    behavior: Mutex<Behavior>,
}

impl StubProvider {
    fn new(name: &'static str, behavior: Behavior) -> (Self, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            StubProvider {
                name,
                calls: calls.clone(),
                delay: Duration::ZERO,
                behavior: Mutex::new(behavior),
            },
            calls,
        )
    }

    fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    fn set_behavior(&self, behavior: Behavior) {
        *self.behavior.lock().unwrap() = behavior;
    }
}

fn make_bars(n: usize) -> Vec<Bar> {
    (1..=n as u32)
        .map(|day| {
            Bar::new(
                NaiveDate::from_ymd_opt(2025, 8, day).unwrap(),
                100.0 + f64::from(day),
                101.0 + f64::from(day),
                102.0 + f64::from(day),
                100.0 + f64::from(day),
                1000.0,
                VolumeUnit::Lots,
            )
        })
        .collect()
}

#[async_trait]
impl DataProvider for StubProvider {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn kline(
        &self,
        _symbol: &Symbol,
        _period: KlinePeriod,
        _adjust: Adjust,
        _count: u32,
    ) -> Result<Fetched<Vec<Bar>>, DataError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let behavior = self.behavior.lock().unwrap().clone();
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        match behavior {
            Behavior::Bars(n) => Ok(Fetched::now(make_bars(n), Source::EastMoney)),
            Behavior::Empty => Ok(Fetched::now(Vec::new(), Source::EastMoney)),
            Behavior::Fail(e) => Err(e),
        }
    }
}

fn waf() -> DataError {
    DataError::WafBlocked("tencent kline sh600519".to_string())
}

fn sym(code: &str) -> Symbol {
    Symbol::new(code).unwrap()
}

/// 10 concurrent identical kline calls → exactly 1 upstream invocation,
/// and every caller receives the same payload.
#[tokio::test]
async fn single_flight_coalesces_identical_kline_calls() {
    let (stub, calls) = StubProvider::new("eastmoney", Behavior::Bars(5));
    let stub = stub.with_delay(Duration::from_millis(100));
    let md = MarketData::with_kline_chain(
        vec![Arc::new(stub) as Arc<dyn DataProvider>],
        BreakerConfig::default(),
    );

    let sym = sym("600519");
    let results: Vec<_> = futures::future::join_all(
        (0..10).map(|_| md.kline(&sym, KlinePeriod::Day, Adjust::Qfq, 60)),
    )
    .await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "upstream called more than once"
    );
    let first = results[0].as_ref().expect("first call failed").clone();
    for r in &results {
        assert_eq!(r.as_ref().unwrap(), &first, "callers saw different results");
    }

    // A follow-up call is served from the TTL cache — still 1 upstream call.
    md.kline(&sym, KlinePeriod::Day, Adjust::Qfq, 60)
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Failure results are never cached: a failing provider is re-asked on the
/// next call, and every concurrent caller shares the same error.
#[tokio::test]
async fn failures_are_coalesced_but_never_cached() {
    let (stub, calls) = StubProvider::new("tencent", Behavior::Fail(waf()));
    let md = MarketData::with_kline_chain(
        vec![Arc::new(stub) as Arc<dyn DataProvider>],
        BreakerConfig::default(),
    );
    let s519 = sym("600519");

    let results: Vec<_> = futures::future::join_all(
        (0..5).map(|_| md.kline(&s519, KlinePeriod::Day, Adjust::Qfq, 60)),
    )
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    for r in &results {
        assert!(matches!(r, Err(DataError::AllFailed { .. })));
    }

    // Sequential retry hits the upstream again (the error was not cached) —
    // but the breaker is now Open, so the chain reports "circuit open"
    // without paying another upstream attempt.
    let err = md
        .kline(&sym("000858"), KlinePeriod::Day, Adjust::Qfq, 60)
        .await
        .unwrap_err();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "open breaker must skip the provider"
    );
    match err {
        DataError::AllFailed { details, .. } => assert!(details.contains("circuit open")),
        other => panic!("expected AllFailed, got {other}"),
    }

    // Never-cached, isolated from the breaker: a data-level error
    // (`Empty`, does not trip the circuit) is re-attempted on every call
    // with the same cache key.
    let (stub2, calls2) = StubProvider::new(
        "tencent",
        Behavior::Fail(DataError::Empty("no rows".to_string())),
    );
    let md2 = MarketData::with_kline_chain(
        vec![Arc::new(stub2) as Arc<dyn DataProvider>],
        BreakerConfig::default(),
    );
    for _ in 0..2 {
        md2.kline(&s519, KlinePeriod::Day, Adjust::Qfq, 60)
            .await
            .unwrap_err();
    }
    assert_eq!(
        calls2.load(Ordering::SeqCst),
        2,
        "failure must not be cached"
    );
}

/// Empty kline payloads count as a miss: failover treats them as failure
/// and nothing is cached, so the next call asks the upstream again.
#[tokio::test]
async fn empty_kline_is_a_miss_and_not_cached() {
    let (stub, calls) = StubProvider::new("eastmoney", Behavior::Empty);
    let md = MarketData::with_kline_chain(
        vec![Arc::new(stub) as Arc<dyn DataProvider>],
        BreakerConfig::default(),
    );

    for code in ["600519", "000858"] {
        let err = md
            .kline(&sym(code), KlinePeriod::Day, Adjust::Qfq, 60)
            .await
            .unwrap_err();
        assert!(matches!(err, DataError::AllFailed { .. }));
    }
    // Different cache keys → both calls reached the upstream.
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // Same key again: still no cache hit (empty was never stored).
    md.kline(&sym("600519"), KlinePeriod::Day, Adjust::Qfq, 60)
        .await
        .unwrap_err();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

/// WAF-blocked primary is skipped while its circuit is Open; after the
/// cooldown a HalfOpen probe re-tries it, and a recovered provider closes
/// the circuit and serves again.
#[tokio::test(start_paused = true)]
async fn breaker_skips_waf_provider_and_probes_after_cooldown() {
    let (tencent, tc_calls) = StubProvider::new("tencent", Behavior::Fail(waf()));
    let (eastmoney, em_calls) = StubProvider::new("eastmoney", Behavior::Bars(5));
    let tencent = Arc::new(tencent);
    let md = MarketData::with_kline_chain(
        vec![
            tencent.clone() as Arc<dyn DataProvider>,
            Arc::new(eastmoney) as Arc<dyn DataProvider>,
        ],
        BreakerConfig::default(), // 10 min cooldown
    );

    // 1st call: tencent attempted, WAF-opens the circuit, EM answers.
    md.kline(&sym("600519"), KlinePeriod::Day, Adjust::Qfq, 60)
        .await
        .unwrap();
    assert_eq!(tc_calls.load(Ordering::SeqCst), 1);
    assert_eq!(em_calls.load(Ordering::SeqCst), 1);
    let health = md.provider_health();
    let tc = health.iter().find(|h| h.name == "tencent").unwrap();
    assert_eq!(tc.state, CircuitState::Open);
    assert!(tc.cooldown_remaining_secs.unwrap() > 0);
    assert_eq!(
        health.iter().find(|h| h.name == "eastmoney").unwrap().state,
        CircuitState::Closed
    );

    // 2nd call (new cache key): tencent skipped entirely.
    md.kline(&sym("000858"), KlinePeriod::Day, Adjust::Qfq, 60)
        .await
        .unwrap();
    assert_eq!(
        tc_calls.load(Ordering::SeqCst),
        1,
        "open circuit not skipped"
    );
    assert_eq!(em_calls.load(Ordering::SeqCst), 2);

    // Cooldown elapsed → next call is the HalfOpen probe against tencent.
    tokio::time::advance(Duration::from_secs(600)).await;
    md.kline(&sym("600036"), KlinePeriod::Day, Adjust::Qfq, 60)
        .await
        .unwrap();
    assert_eq!(
        tc_calls.load(Ordering::SeqCst),
        2,
        "no probe after cooldown"
    );
    assert_eq!(
        md.provider_health()
            .iter()
            .find(|h| h.name == "tencent")
            .unwrap()
            .state,
        CircuitState::Open,
        "failed probe must re-open (now with 20 min backoff)"
    );

    // 10 min later: still re-open (backoff doubled).
    tokio::time::advance(Duration::from_secs(600)).await;
    md.kline(&sym("601318"), KlinePeriod::Day, Adjust::Qfq, 60)
        .await
        .unwrap();
    assert_eq!(tc_calls.load(Ordering::SeqCst), 2);

    // Full 20 min elapsed and tencent has recovered → probe succeeds,
    // circuit closes, tencent serves the request itself.
    tokio::time::advance(Duration::from_secs(600)).await;
    tencent.set_behavior(Behavior::Bars(5));
    let out = md
        .kline(&sym("600900"), KlinePeriod::Day, Adjust::Qfq, 60)
        .await
        .unwrap();
    assert_eq!(tc_calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        md.provider_health()
            .iter()
            .find(|h| h.name == "tencent")
            .unwrap()
            .state,
        CircuitState::Closed
    );
    // The recovered primary answered, so EM was not needed this time.
    assert_eq!(out.data.len(), 5);
    assert_eq!(em_calls.load(Ordering::SeqCst), 4);
}
