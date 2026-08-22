//! Live end-to-end test against the real EastMoney endpoints.
//! Run with: `cargo test -p astock-fundamental --test live_600519 -- --ignored --nocapture`

use astock_core::Symbol;
use astock_fundamental::model::ReportType;
use astock_fundamental::{metrics, scores, valuation, FundamentalClient};
use astock_market_data::{EastMoneyF10, HttpClient, TtlCache};
use std::sync::Arc;

#[tokio::test]
#[ignore = "hits live EastMoney endpoints"]
async fn full_pipeline_600519() {
    let http = Arc::new(HttpClient::new());
    let cache = Arc::new(TtlCache::default());
    let f10 = Arc::new(EastMoneyF10::new(http, cache));
    let client = FundamentalClient::new(f10);
    let symbol = Symbol::new("600519").unwrap();

    let outcome = client.bundle(&symbol).await;
    let b = &outcome.bundle;
    println!("=== failures: {:?} ===", outcome.failures);

    // --- coverage assertions ---
    assert!(b.income.len() >= 8, "income periods: {}", b.income.len());
    assert!(b.balance.len() >= 8, "balance periods: {}", b.balance.len());
    assert!(
        b.cashflow.len() >= 8,
        "cashflow periods: {}",
        b.cashflow.len()
    );
    let profile = b.profile.as_ref().expect("profile");
    let snapshot = b.snapshot.as_ref().expect("snapshot");
    assert!(!b.valuation_history.is_empty(), "valuation history");

    println!(
        "profile: {} ({}) industry={:?} listed={:?} shares={:?}",
        profile.name,
        profile.short_name,
        profile.industry,
        profile.listing_date,
        profile.total_shares
    );
    println!(
        "statements: income={} balance={} cashflow={} indicators={} dividends={} valuation_days={}",
        b.income.len(),
        b.balance.len(),
        b.cashflow.len(),
        b.indicators.len(),
        b.dividends.len(),
        b.valuation_history.len()
    );
    println!(
        "snapshot: price={} pe_ttm={:?} pe_static={:?} pb={:?} mcap={:?}",
        snapshot.price, snapshot.pe_ttm, snapshot.pe_static, snapshot.pb, snapshot.total_market_cap
    );

    // --- metrics on the last two annual periods ---
    let annual_inc: Vec<_> = b
        .income
        .iter()
        .filter(|s| s.meta.is_some_and(|m| m.report_type == ReportType::Annual))
        .collect();
    let annual_bs: Vec<_> = b
        .balance
        .iter()
        .filter(|s| s.meta.is_some_and(|m| m.report_type == ReportType::Annual))
        .collect();
    let annual_cf: Vec<_> = b
        .cashflow
        .iter()
        .filter(|s| s.meta.is_some_and(|m| m.report_type == ReportType::Annual))
        .collect();
    assert!(annual_inc.len() >= 2 && annual_bs.len() >= 2 && !annual_cf.is_empty());

    let inc = annual_inc[annual_inc.len() - 1];
    let inc_prev = annual_inc[annual_inc.len() - 2];
    let bs = annual_bs[annual_bs.len() - 1];
    let bs_prev = annual_bs[annual_bs.len() - 2];
    let cf = annual_cf[annual_cf.len() - 1];

    let gm = metrics::gross_margin(inc.operating_revenue, inc.operating_cost);
    let roe_v = metrics::roe(
        inc.net_profit_parent,
        bs_prev.total_parent_equity,
        bs.total_parent_equity,
    );
    let fcf_v = metrics::fcf(cf.net_cfo, cf.capex);
    let dup = metrics::dupont(inc, bs_prev, bs);
    println!(
        "FY{:?}: gross_margin={:?} roe={:?} fcf={:?} dupont={:?}",
        inc.meta.map(|m| m.period_end),
        gm,
        roe_v,
        fcf_v,
        dup.map(|d| d.roe)
    );
    assert!(gm.is_some() && roe_v.is_some() && fcf_v.is_some());

    // --- scores ---
    let bs_open_prev = if annual_bs.len() >= 3 {
        annual_bs[annual_bs.len() - 3]
    } else {
        bs_prev
    };
    let f_input = scores::piotroski_input_from(inc, inc_prev, cf, bs_open_prev, bs_prev, bs);
    let f = scores::piotroski(&f_input);
    println!("piotroski: {}/9 ({} available)", f.score, f.available);

    let altman_in = scores::AltmanInput {
        working_capital: metrics::working_capital(
            bs.total_current_assets,
            bs.total_current_liabilities,
        ),
        retained_earnings: bs.retained_earnings,
        ebit: scores::altman_ebit(inc),
        market_cap: snapshot.total_market_cap,
        book_equity: bs.total_equity,
        total_liabilities: bs.total_liabilities,
        total_assets: bs.total_assets,
        revenue: inc.total_operating_revenue,
    };
    let z = scores::altman(&altman_in);
    println!(
        "altman: classic={:?} ({:?}) z''={:?} ({:?})",
        z.classic, z.classic_zone, z.z_emerging, z.emerging_zone
    );

    // --- valuation ---
    let pe_hist: Vec<f64> = b
        .valuation_history
        .iter()
        .filter_map(|p| p.pe_ttm)
        .collect();
    let pe_now = snapshot.pe_ttm;
    let pct = pe_now.and_then(|c| valuation::percentile(&pe_hist, c));
    println!(
        "valuation: pe_ttm={:?} percentile={:?} ({} days)",
        pe_now,
        pct,
        pe_hist.len()
    );

    let net_debt = bs
        .interest_bearing_debt()
        .zip(bs.monetary_funds)
        .map(|(debt, cash)| debt - cash)
        .unwrap_or(0.0);
    if let (Some(fcf0), Some(shares)) = (fcf_v, snapshot.total_shares) {
        let inputs = valuation::DcfInputs {
            base_fcf: fcf0,
            stage1_years: 5,
            stage1_growth: 0.08,
            terminal_growth: 0.03,
            wacc: 0.10,
            net_debt,
            shares,
        };
        if let Some(s) = valuation::scenarios(&inputs, 0.02) {
            println!(
                "dcf range: bear={:.2} base={:.2} bull={:.2} (terminal_share={:.2})",
                s.bear.per_share, s.base.per_share, s.bull.per_share, s.base.terminal_share
            );
        }
    }

    // --- anomalies ---
    use astock_fundamental::anomaly::{self, PeriodObservation};
    let obs: Vec<PeriodObservation> = (0..annual_inc.len())
        .map(|i| {
            let bs_i = annual_bs.get(i).map(|b| (*b).clone()).unwrap_or_default();
            let cf_i = annual_cf.get(i).map(|c| (*c).clone()).unwrap_or_default();
            let inc_i = annual_inc[i];
            PeriodObservation {
                revenue: inc_i.total_operating_revenue,
                cfo: cf_i.net_cfo,
                receivables: bs_i
                    .notes_and_accounts_receivable
                    .or(bs_i.accounts_receivable),
                inventory: bs_i.inventory,
                operating_cost: inc_i.operating_cost,
                goodwill: bs_i.goodwill,
                equity: bs_i.total_parent_equity,
                monetary_funds: bs_i.monetary_funds,
                interest_bearing_debt: bs_i.interest_bearing_debt(),
                total_assets: bs_i.total_assets,
                gross_margin: metrics::gross_margin(inc_i.operating_revenue, inc_i.operating_cost),
                net_margin: metrics::net_margin(inc_i.net_profit, inc_i.total_operating_revenue),
            }
        })
        .collect();
    let flags = anomaly::detect(&obs);
    println!("anomaly flags: {}", flags.len());
    for f in &flags {
        println!("  [{:?}] {:?}: {}", f.severity, f.kind, f.explanation);
    }

    assert!(
        outcome.failures.is_empty(),
        "sections failed: {:?}",
        outcome.failures
    );
}
