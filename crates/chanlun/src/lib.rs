//! # astock-chanlun
//!
//! Chan theory (缠论) analysis engine, ported from the legacy Python
//! implementation (`analysis/chanlun_daily.py`, `analysis/chanlun_minute.py`).
//!
//! Two pipelines are provided:
//!
//! - [`daily`]: daily/weekly kline analysis — containment merge → fractals →
//!   strokes → zhongshus (中枢) → SMA-seeded MACD → divergence detection →
//!   type-1/2/3 buy/sell signals, plus the ECharts overlay payload.
//! - [`minute`]: 1-minute to 5-minute aggregation followed by the same
//!   merge/fractal/stroke pipeline, producing type-1 signals only.
//!
//! All public functions are pure (no I/O, no global state). Floating-point
//! arithmetic replicates the legacy operation order exactly so results match
//! the golden fixtures bit-for-bit (before the final display rounding).

pub mod daily;
pub mod minute;
mod pyround;
