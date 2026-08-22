//! Live cross-validation of the self-computed adjustment factors against
//! provider-adjusted klines (data-foundation-v2 §复权因子数学 验证金标):
//!
//! 1. Our qfq (raw fqt=0 bars + `RPT_SHAREBONUS_DET` actions through
//!    `astock_core::compute_qfq`) vs EastMoney fqt=1.
//! 2. Our hfq/qfq vs the Sina factor endpoints (`hfq.js` multiplicative,
//!    `qfq.js` divisive, merge-asof).
//!
//! # Cross-source convention finding (verified 2026-08-21)
//!
//! Two 前复权 conventions coexist in the wild:
//!
//! - **Multiplicative (spec §复权因子数学, implemented here)**: per action,
//!   pre-ex prices scale by `r = X/C` with `X = (C − D + P×R)/(1 + B + R)`.
//!   **Sina**'s factor endpoints follow this: our series matches them to
//!   0.005% over 600 days.
//! - **Affine (等差)**: per action, pre-ex prices transform as
//!   `p → (p − D + P×R)/(1 + B + R)` (cash subtracted in then-current share
//!   units, splits divide). **EastMoney fqt=1 and Tencent qfq are identical
//!   to the cent** and follow this rule: reconstruction from our parsed
//!   actions matches them within 0.01% (600519) / 0.02% (002594).
//!
//! The two rules agree at the ex-date itself and diverge as prices move
//! away from it: up to ~0.55% over a year for 600519 (large absolute
//! dividends) and ~2.2% for 002594 across its 10送8转12 mega-split. The
//! spec's 0.5% golden tolerance vs EM/Tencent is therefore only attainable
//! in convention-insensitive windows; the EM comparisons below carry a
//! documented wider bound where the conventions diverge, and the affine
//! reconstruction tests prove the *action data* is exact.
//!
//! Gated behind `#[ignore]`; run with:
//! `cargo test -p astock-market-data --test adjust_live -- --ignored --nocapture`

use std::collections::BTreeMap;
use std::sync::Arc;

use astock_core::{compute_hfq, compute_qfq, Adjust, Bar, CorporateAction, KlinePeriod, Symbol};
use astock_market_data::{DataProvider, EastMoney, EastMoneyF10, HttpClient, TtlCache};
use chrono::NaiveDate;

fn providers() -> (EastMoney, EastMoneyF10) {
    let http = Arc::new(HttpClient::new());
    let cache = Arc::new(TtlCache::new(10_000));
    (
        EastMoney::new(http.clone(), cache.clone()),
        EastMoneyF10::new(http, cache),
    )
}

/// Parse a Sina factor JS body: `var sh600519hfq={"total":N,"data":[{"d":
/// "YYYY-MM-DD","f":"8.88..."}, ...]}` (dates descending, factors strings).
/// Parsed leniently: first `{` to last `}`, both string and numeric factors
/// accepted. Returned ascending by date.
fn parse_sina_factors(body: &str) -> Vec<(NaiveDate, f64)> {
    let start = body.find('{').expect("sina factor body has no JSON object");
    let end = body
        .rfind('}')
        .expect("sina factor body has no JSON object");
    let value: serde_json::Value =
        serde_json::from_str(&body[start..=end]).expect("sina factor JSON parse failed");
    let mut out: Vec<(NaiveDate, f64)> = value
        .get("data")
        .and_then(|d| d.as_array())
        .expect("sina factor body has no data array")
        .iter()
        .filter_map(|row| {
            let date = row
                .get("d")
                .and_then(|v| v.as_str())
                .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())?;
            let factor = match row.get("f") {
                Some(serde_json::Value::String(s)) => s.parse::<f64>().ok(),
                Some(serde_json::Value::Number(n)) => n.as_f64(),
                _ => None,
            }?;
            Some((date, factor))
        })
        .collect();
    out.sort_by_key(|(d, _)| *d);
    out
}

/// Merge-asof: the factor of the latest entry with `date <=` the bar date
/// (Sina factors change on ex-dates and hold between them).
fn factor_asof(sorted: &[(NaiveDate, f64)], date: NaiveDate) -> Option<f64> {
    let idx = sorted.partition_point(|(d, _)| *d <= date);
    idx.checked_sub(1).map(|i| sorted[i].1)
}

fn by_date(bars: &[Bar]) -> BTreeMap<NaiveDate, &Bar> {
    bars.iter().map(|b| (b.date, b)).collect()
}

/// Max relative close deviation between two bar series over their last
/// `days` overlapping dates.
fn max_rel_deviation(a: &[Bar], b: &[Bar], days: usize) -> (f64, usize) {
    let b_map = by_date(b);
    let mut compared = 0_usize;
    let mut max_dev = 0.0_f64;
    for bar in a.iter().rev() {
        if compared >= days {
            break;
        }
        let Some(other) = b_map.get(&bar.date) else {
            continue;
        };
        compared += 1;
        let dev = (bar.close - other.close).abs() / other.close.abs().max(1e-12);
        max_dev = max_dev.max(dev);
    }
    (max_dev, compared)
}

/// Assert no adjustment warnings other than `MissingPrevClose` for actions
/// before the analysis window: those contribute nothing to in-window qfq
/// factors (only `E_i > t` enters `factor(t)`) and merely shift the
/// constant hfq base, which the cross-checks normalize away.
fn assert_only_pre_window_warnings(
    warnings: &[astock_core::AdjustWarning],
    window_start: NaiveDate,
) {
    let unexpected: Vec<_> = warnings
        .iter()
        .filter(|w| {
            !matches!(
                w,
                astock_core::AdjustWarning::MissingPrevClose { ex_date } if *ex_date <= window_start
            )
        })
        .collect();
    assert!(
        unexpected.is_empty(),
        "unexpected adjustment warnings: {unexpected:?}"
    );
}

/// Our qfq vs EastMoney fqt=1.
///
/// `tolerance`/`days` encode the convention finding (see module docs): in
/// convention-sensitive windows (600519 full year, 002594 across the split)
/// EM's affine rule and our multiplicative rule diverge beyond the spec's
/// 0.5%; in convention-insensitive windows the spec's 0.5% golden holds.
async fn check_qfq_vs_eastmoney(code: &str, tolerance: f64, days: usize) {
    let (em, f10) = providers();
    let symbol = Symbol::new(code).unwrap();
    let raw = em
        .kline(&symbol, KlinePeriod::Day, Adjust::None, 600)
        .await
        .expect("raw kline fetch failed");
    let em_qfq = em
        .kline(&symbol, KlinePeriod::Day, Adjust::Qfq, 600)
        .await
        .expect("qfq kline fetch failed");
    let actions = f10
        .corporate_actions(code, 5)
        .await
        .expect("corporate actions fetch failed");
    println!(
        "{code}: raw={} bars, em_qfq={} bars, actions={}",
        raw.data.len(),
        em_qfq.data.len(),
        actions.data.len()
    );
    for a in actions.data.iter().take(3) {
        println!(
            "  latest action: ex={} cash_div={:.4} bonus={:.4}",
            a.ex_date, a.cash_div, a.bonus_share
        );
    }

    let anchor = raw.data.last().unwrap().date;
    let ours = compute_qfq(&raw.data, &actions.data, anchor, None);
    assert_only_pre_window_warnings(&ours.warnings, raw.data[0].date);

    let (max_dev, compared) = max_rel_deviation(&ours.bars, &em_qfq.data, days);
    println!(
        "{code}: qfq vs EM fqt=1: max_dev={:.6}% over {compared} days",
        max_dev * 100.0
    );
    assert!(
        compared >= 200,
        "{code}: too little overlap: {compared} days"
    );
    assert!(
        max_dev < tolerance,
        "{code}: qfq deviation {:.4}% exceeds the {:.1}% tolerance",
        max_dev * 100.0,
        tolerance * 100.0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live network test"]
async fn live_qfq_matches_eastmoney_600519() {
    // 1% bound: EM/Tencent use additive cash adjustment, we implement the
    // spec's multiplicative form — see `check_qfq_vs_eastmoney` docs.
    check_qfq_vs_eastmoney("600519", 0.01, 250).await;
}

/// Vendor-affine qfq close for one bar: apply, for every action with
/// `ex_date > date` in ascending ex-date order, the transform
/// `p → (p − D + P×R)/(1 + B + R)` — the EM/Tencent convention (module
/// docs). Needs no previous close, so pre-window actions simply drop out.
fn vendor_affine_close(raw_close: f64, date: NaiveDate, actions: &[CorporateAction]) -> f64 {
    let mut p = raw_close;
    for a in actions.iter().filter(|a| a.ex_date > date) {
        p = (p - a.cash_div + a.rights_price.unwrap_or(0.0) * a.rights_ratio)
            / (1.0 + a.bonus_share + a.rights_ratio);
    }
    p
}

/// Proof that our parsed `RPT_SHAREBONUS_DET` actions are exact: rebuilding
/// the vendor series with the affine rule from those actions must match EM
/// fqt=1 within 0.2% on both a cash-only stock (600519) and a combo
/// split stock (002594, 2025-07-29 10送8转12派39.74).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live network test"]
async fn live_affine_reconstruction_matches_vendor() {
    for code in ["600519", "002594"] {
        let (em, f10) = providers();
        let symbol = Symbol::new(code).unwrap();
        let raw = em
            .kline(&symbol, KlinePeriod::Day, Adjust::None, 600)
            .await
            .expect("raw kline fetch failed");
        let em_qfq = em
            .kline(&symbol, KlinePeriod::Day, Adjust::Qfq, 600)
            .await
            .expect("qfq kline fetch failed");
        let fetched = f10
            .corporate_actions(code, 5)
            .await
            .expect("corporate actions fetch failed");
        let mut actions = fetched.data;
        actions.sort_by_key(|a| a.ex_date);
        let em_map = by_date(&em_qfq.data);
        let mut max_dev = 0.0_f64;
        let mut compared = 0_usize;
        for bar in &raw.data {
            let Some(em_bar) = em_map.get(&bar.date) else {
                continue;
            };
            compared += 1;
            let vendor_like = vendor_affine_close(bar.close, bar.date, &actions);
            let dev = (vendor_like - em_bar.close).abs() / em_bar.close;
            max_dev = max_dev.max(dev);
        }
        println!(
            "{code}: affine reconstruction vs EM fqt=1: max_dev={:.6}% over {compared} days",
            max_dev * 100.0
        );
        assert!(compared >= 500, "{code}: too little overlap: {compared}");
        assert!(
            max_dev < 0.002,
            "{code}: affine reconstruction deviates {:.4}% — action data problem?",
            max_dev * 100.0
        );
    }
}

/// Same golden check on a stock with a recent 送转 (bonus + transfer), to
/// validate the `BONUS_RATIO`/`IT_RATIO` per-10-shares parsing. 002594
/// (比亚迪) implemented 10送8转12派39.74元 on 2025-07-29. The 250-day window
/// is post-split: no action with a share multiplier sits inside it, so the
/// affine/multiplicative conventions coincide and the spec's 0.5% golden
/// tolerance applies. (Across the split the conventions diverge ~2.2% —
/// see module docs and `live_affine_reconstruction_matches_vendor`.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live network test"]
async fn live_qfq_matches_eastmoney_002594_bonus_transfer() {
    check_qfq_vs_eastmoney("002594", 0.005, 250).await;
}

/// Our hfq vs the Sina multiplicative factor, and our qfq vs the Sina
/// divisive factor (merge-asof), 600519, tolerance 1%.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live network test"]
async fn live_factors_cross_check_sina_600519() {
    let (em, f10) = providers();
    let http = Arc::new(HttpClient::new());
    let symbol = Symbol::new("600519").unwrap();
    let raw = em
        .kline(&symbol, KlinePeriod::Day, Adjust::None, 600)
        .await
        .expect("raw kline fetch failed");
    let actions: Vec<CorporateAction> = f10
        .corporate_actions("600519", 5)
        .await
        .expect("corporate actions fetch failed")
        .data;

    let hfq_body = http
        .get_text(
            "https://finance.sina.com.cn/realstock/company/sh600519/hfq.js",
            &[],
        )
        .await
        .expect("sina hfq.js fetch failed")
        .body;
    let qfq_body = http
        .get_text(
            "https://finance.sina.com.cn/realstock/company/sh600519/qfq.js",
            &[],
        )
        .await
        .expect("sina qfq.js fetch failed")
        .body;
    let sina_hfq = parse_sina_factors(&hfq_body);
    let sina_qfq = parse_sina_factors(&qfq_body);
    assert!(sina_hfq.len() >= 20, "suspiciously few sina hfq factors");
    assert_eq!(sina_hfq.len(), sina_qfq.len(), "hfq/qfq factor sets differ");

    // --- hfq: absolute levels differ from sina by the pre-window
    // cumulative factor (our window starts 600 bars back, sina factors are
    // since-IPO), so compare growth ratios against the first bar instead:
    // our_hfq(t)/raw(t0) must equal raw(t)*f(t)/(raw(t0)*f(t0)). ---
    let ours_hfq = compute_hfq(&raw.data, &actions, None);
    assert_only_pre_window_warnings(&ours_hfq.warnings, raw.data[0].date);
    let t0 = raw.data[0].date;
    let f_t0 = factor_asof(&sina_hfq, t0).expect("no sina hfq factor at window start");
    let mut max_dev = 0.0_f64;
    let mut compared = 0_usize;
    for (raw_bar, our_bar) in raw.data.iter().zip(&ours_hfq.bars) {
        let Some(f) = factor_asof(&sina_hfq, raw_bar.date) else {
            continue;
        };
        compared += 1;
        let expected = raw_bar.close * f / f_t0; // == raw(t0)-anchored hfq
        let dev = (our_bar.close - expected).abs() / expected.abs().max(1e-12);
        max_dev = max_dev.max(dev);
    }
    println!(
        "sina hfq cross-check: max_dev={:.6}% over {compared} days",
        max_dev * 100.0
    );
    assert!(compared >= 500, "too little sina hfq overlap: {compared}");
    assert!(
        max_dev < 0.01,
        "hfq vs sina deviation {:.4}% exceeds 1%",
        max_dev * 100.0
    );

    // --- qfq: sina's divisive factor g(t) means qfq = raw/g; anchored at
    // the latest ex-date exactly like our anchor=latest computation. ---
    let anchor = raw.data.last().unwrap().date;
    let ours_qfq = compute_qfq(&raw.data, &actions, anchor, None);
    let mut max_dev = 0.0_f64;
    let mut compared = 0_usize;
    for (raw_bar, our_bar) in raw.data.iter().zip(&ours_qfq.bars) {
        let Some(g) = factor_asof(&sina_qfq, raw_bar.date) else {
            continue;
        };
        compared += 1;
        let expected = raw_bar.close / g;
        let dev = (our_bar.close - expected).abs() / expected.abs().max(1e-12);
        max_dev = max_dev.max(dev);
    }
    println!(
        "sina qfq cross-check: max_dev={:.6}% over {compared} days",
        max_dev * 100.0
    );
    assert!(compared >= 500, "too little sina qfq overlap: {compared}");
    assert!(
        max_dev < 0.01,
        "qfq vs sina deviation {:.4}% exceeds 1%",
        max_dev * 100.0
    );
}
