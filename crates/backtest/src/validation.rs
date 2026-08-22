//! Anti-overfitting validation harness.
//!
//! Tools here are built on one principle: **report the whole distribution,
//! never just the best number.** The grid runner returns the full results
//! matrix; the walk-forward runner returns the full matrix per fold; the
//! overfit check compares the best parameter set against the *median* one.
//!
//! - [`walk_forward_folds`]: rolling-origin train/test windows over a series.
//! - [`run_grid`]: parameter grid over one series — full matrix.
//! - [`walk_forward_grid`]: grid × walk-forward — full matrix per fold, plus
//!   the parameter that won on each training window.
//! - [`WalkForwardReport::stability`]: dispersion of out-of-sample Sharpe
//!   across folds, fraction of parameter sets that were profitable, and any
//!   [`OverfitWarning`].
//! - [`bootstrap_sharpe`]: seeded bootstrap of a return series (e.g. daily
//!   returns or round-trip returns) yielding a confidence interval on the
//!   Sharpe ratio. Same seed ⇒ identical result.

use serde::{Deserialize, Serialize};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::data::PriceSeries;
use crate::engine::BacktestEngine;
use crate::metrics::{sharpe, std_pop, MetricsConfig};
use crate::strategy::Strategy;
use crate::Result;

/// One rolling-origin walk-forward fold over bar indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fold {
    /// Fold ordinal (0-based).
    pub index: usize,
    /// Train window: `[train_start, train_end)`.
    pub train_start: usize,
    /// Train window end (exclusive).
    pub train_end: usize,
    /// Test window: `[test_start, test_end)`; always starts at `train_end`.
    pub test_start: usize,
    /// Test window end (exclusive).
    pub test_end: usize,
}

/// Rolling-origin folds over `n_bars`: fold *k* trains on
/// `[k*step, k*step + train_bars)` and tests on the following `test_bars`.
/// Folds whose test window would exceed `n_bars` are dropped.
pub fn walk_forward_folds(
    n_bars: usize,
    train_bars: usize,
    test_bars: usize,
    step: usize,
) -> Vec<Fold> {
    assert!(train_bars > 0 && test_bars > 0 && step > 0);
    let mut folds = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while start + train_bars + test_bars <= n_bars {
        folds.push(Fold {
            index,
            train_start: start,
            train_end: start + train_bars,
            test_start: start + train_bars,
            test_end: start + train_bars + test_bars,
        });
        start += step;
        index += 1;
    }
    folds
}

/// One row of a grid-results matrix: full metrics for one parameter set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridRow<P> {
    /// The parameter set.
    pub param: P,
    /// Final equity of the run.
    pub final_equity: f64,
    /// Total return over the run.
    pub total_return: f64,
    /// Annualized Sharpe ratio (default metrics config).
    pub sharpe: f64,
    /// Maximum drawdown (positive fraction).
    pub max_drawdown: f64,
    /// Number of fills.
    pub trades: usize,
}

/// Run `make(param)` for every parameter set over one series and return the
/// **full** results matrix in parameter order. Panics only if the series is
/// empty (a programming error at the call site).
pub fn run_grid<P, S, F>(
    engine: &BacktestEngine,
    series: &PriceSeries,
    params: &[P],
    make: F,
) -> Vec<GridRow<P>>
where
    P: Clone,
    S: Strategy,
    F: Fn(&P) -> S,
{
    let cfg = MetricsConfig::default();
    params
        .iter()
        .map(|p| {
            let mut strategy = make(p);
            let res = engine
                .run(series, &mut strategy)
                .expect("grid run on a valid series");
            let report = res.performance_report(None, &cfg);
            GridRow {
                param: p.clone(),
                final_equity: res.final_equity(),
                total_return: report.as_ref().map(|r| r.total_return).unwrap_or(0.0),
                sharpe: report.as_ref().map(|r| r.sharpe).unwrap_or(0.0),
                max_drawdown: report.as_ref().map(|r| r.max_drawdown).unwrap_or(0.0),
                trades: res.trades.len(),
            }
        })
        .collect()
}

/// Full grid results for one walk-forward fold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FoldResult<P> {
    /// The fold windows.
    pub fold: Fold,
    /// Full in-sample results matrix (same order as the parameter list).
    pub train_rows: Vec<GridRow<P>>,
    /// Full out-of-sample results matrix (same order as the parameter list).
    pub test_rows: Vec<GridRow<P>>,
    /// Index of the parameter set with the best in-sample Sharpe.
    pub best_train_param: usize,
}

/// Walk-forward grid output: every fold, every parameter set, both windows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WalkForwardReport<P> {
    /// Per-fold results, in fold order.
    pub folds: Vec<FoldResult<P>>,
}

/// Run a parameter grid through rolling walk-forward validation.
///
/// For each fold the grid is evaluated on the training window (to pick the
/// best in-sample parameter set) **and** on the test window; both matrices
/// are returned in full so nothing is cherry-picked.
pub fn walk_forward_grid<P, S, F>(
    engine: &BacktestEngine,
    series: &PriceSeries,
    params: &[P],
    make: F,
    train_bars: usize,
    test_bars: usize,
    step: usize,
) -> Result<WalkForwardReport<P>>
where
    P: Clone,
    S: Strategy,
    F: Fn(&P) -> S,
{
    let folds = walk_forward_folds(series.len(), train_bars, test_bars, step);
    let mut results = Vec::with_capacity(folds.len());
    for fold in folds {
        let train = series.slice(fold.train_start, fold.train_end)?;
        let test = series.slice(fold.test_start, fold.test_end)?;
        let train_rows = run_grid(engine, &train, params, &make);
        let test_rows = run_grid(engine, &test, params, &make);
        let best_train_param = train_rows
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.sharpe.total_cmp(&b.1.sharpe))
            .map(|(i, _)| i)
            .unwrap_or(0);
        results.push(FoldResult {
            fold,
            train_rows,
            test_rows,
            best_train_param,
        });
    }
    Ok(WalkForwardReport { folds: results })
}

/// Median of a non-empty slice (average of the two middle values for even
/// lengths); 0 for empty input.
pub fn median(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut sorted = xs.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// Raised when the best in-sample parameter set looks suspiciously better
/// than the typical one — the classic overfitting signature.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OverfitWarning {
    /// Fold the warning applies to (`None` for a standalone grid check).
    pub fold: Option<usize>,
    /// Best in-sample Sharpe across the grid.
    pub best_sharpe: f64,
    /// Median in-sample Sharpe across the grid.
    pub median_sharpe: f64,
    /// `best / median` (exceeds 2.0 when this warning exists).
    pub ratio: f64,
}

/// Flag when the best in-sample Sharpe is more than 2× the **median**
/// in-sample Sharpe. Only meaningful when the median is positive: a
/// non-positive median means the whole grid is unprofitable, which is a
/// strategy problem, not an overfitting signal.
pub fn overfit_check<P>(rows: &[GridRow<P>], fold: Option<usize>) -> Option<OverfitWarning> {
    if rows.len() < 2 {
        return None;
    }
    let sharpes: Vec<f64> = rows.iter().map(|r| r.sharpe).collect();
    let med = median(&sharpes);
    let best = sharpes.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if med > 0.0 && best > 2.0 * med {
        Some(OverfitWarning {
            fold,
            best_sharpe: best,
            median_sharpe: med,
            ratio: best / med,
        })
    } else {
        None
    }
}

/// Cross-fold stability summary of a walk-forward grid run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StabilityReport {
    /// Out-of-sample Sharpe of each fold's best-in-sample parameter set.
    pub best_param_test_sharpes: Vec<f64>,
    /// Mean of `best_param_test_sharpes`.
    pub test_sharpe_mean: f64,
    /// Population std of `best_param_test_sharpes` — high dispersion means
    /// the "best" parameters do not travel across regimes.
    pub test_sharpe_std: f64,
    /// Fraction of all out-of-sample grid rows with positive total return.
    pub pct_params_profitable: f64,
    /// One warning per fold whose in-sample grid looks overfit.
    pub overfit_warnings: Vec<OverfitWarning>,
}

impl<P: Clone> WalkForwardReport<P> {
    /// Summarize out-of-sample stability across folds.
    pub fn stability(&self) -> StabilityReport {
        let sharpes: Vec<f64> = self
            .folds
            .iter()
            .map(|f| {
                f.test_rows
                    .get(f.best_train_param)
                    .map(|r| r.sharpe)
                    .unwrap_or(0.0)
            })
            .collect();
        let all_test: Vec<&GridRow<P>> =
            self.folds.iter().flat_map(|f| f.test_rows.iter()).collect();
        let profitable = all_test.iter().filter(|r| r.total_return > 0.0).count();
        let pct = if all_test.is_empty() {
            0.0
        } else {
            profitable as f64 / all_test.len() as f64
        };
        let warnings = self
            .folds
            .iter()
            .filter_map(|f| overfit_check(&f.train_rows, Some(f.fold.index)))
            .collect();
        let mean = if sharpes.is_empty() {
            0.0
        } else {
            sharpes.iter().sum::<f64>() / sharpes.len() as f64
        };
        StabilityReport {
            best_param_test_sharpes: sharpes.clone(),
            test_sharpe_mean: mean,
            test_sharpe_std: std_pop(&sharpes),
            pct_params_profitable: pct,
            overfit_warnings: warnings,
        }
    }
}

/// Seeded bootstrap result for a Sharpe ratio distribution.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BootstrapResult {
    /// Number of replicates drawn.
    pub replicates: usize,
    /// Mean bootstrapped Sharpe.
    pub mean_sharpe: f64,
    /// 5th percentile of bootstrapped Sharpes.
    pub ci_low: f64,
    /// 95th percentile of bootstrapped Sharpes.
    pub ci_high: f64,
}

/// Bootstrap the Sharpe ratio of `returns` (daily returns, or round-trip
/// returns for a trade-level view): resample with replacement `replicates`
/// times and report the mean and the 5%/95% percentile band.
///
/// Deterministic: the same `seed` and inputs always produce the same result.
/// Returns `None` for fewer than 2 returns or 0 replicates.
pub fn bootstrap_sharpe(
    returns: &[f64],
    replicates: usize,
    seed: u64,
    cfg: &MetricsConfig,
) -> Option<BootstrapResult> {
    if returns.len() < 2 || replicates == 0 {
        return None;
    }
    let mut rng = StdRng::seed_from_u64(seed);
    let mut stats = Vec::with_capacity(replicates);
    for _ in 0..replicates {
        let sample: Vec<f64> = (0..returns.len())
            .map(|_| returns[rng.random_range(0..returns.len())])
            .collect();
        stats.push(sharpe(&sample, cfg));
    }
    stats.sort_by(f64::total_cmp);
    let mean = stats.iter().sum::<f64>() / stats.len() as f64;
    let idx = |q: f64| -> usize { (((stats.len() - 1) as f64) * q).round() as usize };
    Some(BootstrapResult {
        replicates,
        mean_sharpe: mean,
        ci_low: stats[idx(0.05)],
        ci_high: stats[idx(0.95)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Bar;
    use crate::engine::EngineConfig;
    use crate::strategy::MaCross;
    use astock_trading_rules::RuleSet;
    use chrono::NaiveDate;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn trend_series(n: usize) -> PriceSeries {
        // Smooth upward drift with a mild oscillation: MA-cross-friendly.
        let bars: Vec<Bar> = (0..n)
            .map(|i| {
                let price = 10.0 + i as f64 * 0.05 + (i as f64 * 0.4).sin();
                Bar::flat(d("2025-01-06") + chrono::Duration::days(i as i64), price)
            })
            .collect();
        PriceSeries::new("600519", bars).unwrap()
    }

    fn engine() -> BacktestEngine {
        BacktestEngine::new(
            RuleSet::load(None).unwrap(),
            EngineConfig::new("600519", 100_000.0),
        )
        .unwrap()
    }

    #[test]
    fn folds_are_rolling_and_bounded() {
        let folds = walk_forward_folds(100, 40, 20, 20);
        assert_eq!(folds.len(), 3);
        assert_eq!(
            folds[0],
            Fold {
                index: 0,
                train_start: 0,
                train_end: 40,
                test_start: 40,
                test_end: 60
            }
        );
        assert_eq!(folds[2].train_start, 40);
        assert_eq!(folds[2].test_end, 100);
        // Step not dividing evenly: trailing partial fold is dropped.
        assert_eq!(walk_forward_folds(95, 40, 20, 20).len(), 2);
    }

    #[test]
    fn grid_returns_full_matrix() {
        let series = trend_series(120);
        let params: Vec<(usize, usize)> = vec![(2, 5), (3, 10), (5, 20)];
        let rows = run_grid(&engine(), &series, &params, |&(f, s)| MaCross::new(f, s));
        // Every parameter set is reported — not just the best.
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].param, (2, 5));
        assert_eq!(rows[2].param, (5, 20));
    }

    #[test]
    fn walk_forward_grid_covers_all_folds_and_params() {
        let series = trend_series(100);
        let params: Vec<(usize, usize)> = vec![(2, 5), (3, 10)];
        let report = walk_forward_grid(
            &engine(),
            &series,
            &params,
            |&(f, s)| MaCross::new(f, s),
            40,
            20,
            20,
        )
        .unwrap();
        assert_eq!(report.folds.len(), 3);
        for fold in &report.folds {
            assert_eq!(fold.train_rows.len(), 2);
            assert_eq!(fold.test_rows.len(), 2);
            assert!(fold.best_train_param < 2);
        }
        let stability = report.stability();
        assert_eq!(stability.best_param_test_sharpes.len(), 3);
        assert!((0.0..=1.0).contains(&stability.pct_params_profitable));
    }

    #[test]
    fn overfit_warning_triggers_above_2x_median() {
        // Median 1.25, best 3.0 -> ratio 2.4 > 2 -> warning.
        let rows: Vec<GridRow<u32>> = [0.5_f64, 1.0, 1.5, 3.0]
            .into_iter()
            .enumerate()
            .map(|(i, s)| GridRow {
                param: i as u32,
                final_equity: 0.0,
                total_return: 0.0,
                sharpe: s,
                max_drawdown: 0.0,
                trades: 0,
            })
            .collect();
        let w = overfit_check(&rows, None).expect("warning expected");
        assert!((w.median_sharpe - 1.25).abs() < 1e-12); // (1.0 + 1.5) / 2
        assert!((w.best_sharpe - 3.0).abs() < 1e-12);
        assert!(w.ratio > 2.0);

        // Best only 1.8x median -> no warning.
        let rows: Vec<GridRow<u32>> = [1.0_f64, 1.0, 1.8]
            .into_iter()
            .enumerate()
            .map(|(i, s)| GridRow {
                param: i as u32,
                final_equity: 0.0,
                total_return: 0.0,
                sharpe: s,
                max_drawdown: 0.0,
                trades: 0,
            })
            .collect();
        assert!(overfit_check(&rows, None).is_none());

        // Non-positive median: strategy problem, not overfitting.
        let rows: Vec<GridRow<u32>> = [-1.0_f64, -0.5, 3.0]
            .into_iter()
            .enumerate()
            .map(|(i, s)| GridRow {
                param: i as u32,
                final_equity: 0.0,
                total_return: 0.0,
                sharpe: s,
                max_drawdown: 0.0,
                trades: 0,
            })
            .collect();
        assert!(overfit_check(&rows, None).is_none());
    }

    #[test]
    fn bootstrap_is_seeded_and_ordered() {
        let returns: Vec<f64> = (0..100)
            .map(|i| 0.002 * (i as f64 * 0.7).sin() + 0.0005)
            .collect();
        let cfg = MetricsConfig::default();
        let a = bootstrap_sharpe(&returns, 500, 42, &cfg).unwrap();
        let b = bootstrap_sharpe(&returns, 500, 42, &cfg).unwrap();
        assert_eq!(a, b, "same seed must reproduce the same interval");
        assert!(a.ci_low <= a.ci_high);
        assert!(a.mean_sharpe.is_finite());
        // Degenerate inputs.
        assert!(bootstrap_sharpe(&[0.01], 100, 1, &cfg).is_none());
        assert!(bootstrap_sharpe(&returns, 0, 1, &cfg).is_none());
    }
}
