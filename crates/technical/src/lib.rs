//! # astock-technical
//!
//! Deterministic technical-analysis engine ported from the legacy Python
//! A-share tool (`legacy-reference/analysis/*` plus the `app.py`
//! post-processing). Five analysis modules — trend, volume-price, pattern,
//! breakout (Turtle), CANSLIM — are aggregated by a composite signal engine,
//! followed by market-breadth M-score adjustment and signal optimization
//! (vetoes, re-grading, position sizing, risk-reward check).
//!
//! All functions are pure and deterministic: no I/O, no clocks, no network.
//! The floating-point operation order replicates CPython so results match the
//! legacy implementation bit-for-bit; string formatting helpers
//! ([`util::py_round`], [`util::py_f64`]) reproduce Python's `round()` and
//! `str(float)`.
//!
//! The top-level entry point is [`analyze`], which mirrors the fixture
//! generator pipeline: `run_analysis` → `signal_to_dict` → breadth M-score
//! adjust → `_apply_signal_optimization`.

pub mod breakout;
pub mod canslim;
pub mod engine;
pub mod indicators;
pub mod manual_plan;
pub mod optimize;
pub mod pattern;
pub mod trend;
pub mod types;
pub mod util;
pub mod volume_price;

pub use engine::{run_analysis, signal_to_json, SignalEngineResult, TradePlan};
pub use manual_plan::{
    build_manual_trading_plan, ManualCheckpoint, ManualEvidence, ManualScenario, ManualTradingPlan,
    SessionSchedule, TradingConstraints,
};
pub use optimize::{
    apply_breadth_m_adjustment, apply_signal_optimization, breadth_m_bonus, VetoInputs,
};
pub use types::{Breadth, FundFlow, Kline, Quote};

/// Run the full analysis pipeline and return the final signal dict as JSON,
/// exactly matching the legacy `handle_analyze` signal shape (including the
/// breadth M-score adjustment and the optimization post-processing).
///
/// * `klines` — bars for the analyzed period (ascending by date)
/// * `quote` — optional realtime quote
/// * `flows` — optional fund-flow history
/// * `index_klines` — optional index bars for the CANSLIM M score (when
///   `None`, the M score falls back to the stock's own MAs)
/// * `breadth` — optional market breadth snapshot for the M-score adjustment
pub fn analyze(
    klines: &[Kline],
    quote: Option<&Quote>,
    flows: Option<&[FundFlow]>,
    index_klines: Option<&[Kline]>,
    breadth: Option<&Breadth>,
) -> serde_json::Value {
    let result = run_analysis(klines, quote, flows, index_klines);
    let mut signal = signal_to_json(&result);
    if let Some(b) = breadth {
        apply_breadth_m_adjustment(&mut signal, b);
    }
    let veto = VetoInputs::from_result(&result);
    apply_signal_optimization(&mut signal, &veto);
    signal
}
