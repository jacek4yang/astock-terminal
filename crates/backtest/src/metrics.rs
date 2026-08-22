//! Performance metrics computed from equity curves and trade logs.
//!
//! Conventions (all documented per function; the golden tests pin them):
//!
//! - Daily returns are simple returns of consecutive equity points.
//! - Annualization uses `periods_per_year` (default 252 trading days).
//! - Volatility / Sharpe / Sortino use **population** standard deviation
//!   (divide by N, not N-1) — deterministic and stable for short windows.
//! - Ratios with a zero denominator return 0.0 (documented per function)
//!   rather than NaN/inf, so grid reports stay comparable; the single
//!   exception is [`omega`], which returns `f64::INFINITY` when there are
//!   gains and zero losses.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::data::PriceSeries;
use crate::engine::{BacktestResult, RoundTrip};

/// Tunables shared by all metrics.
#[derive(Debug, Clone, Copy)]
pub struct MetricsConfig {
    /// Trading periods per year used for annualization (252 for daily bars).
    pub periods_per_year: f64,
    /// Annual risk-free rate used by Sharpe/Sortino/alpha (0.0 by default).
    pub risk_free_annual: f64,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        MetricsConfig {
            periods_per_year: 252.0,
            risk_free_annual: 0.0,
        }
    }
}

/// Simple returns between consecutive equity values. Points with a
/// non-positive predecessor are skipped (division guard).
pub fn daily_returns(equity: &[f64]) -> Vec<f64> {
    equity
        .windows(2)
        .filter(|w| w[0] > 0.0)
        .map(|w| w[1] / w[0] - 1.0)
        .collect()
}

/// `last / first - 1`, or 0 for a degenerate curve.
pub fn total_return(equity: &[f64]) -> f64 {
    match (equity.first(), equity.last()) {
        (Some(&first), Some(&last)) if first > 0.0 => last / first - 1.0,
        _ => 0.0,
    }
}

/// Compound annual growth rate over `years` (calendar-based:
/// `(end/start)^(1/years) - 1`). Returns 0 for degenerate input.
pub fn cagr(start: f64, end: f64, years: f64) -> f64 {
    if start <= 0.0 || end <= 0.0 || years <= 0.0 {
        return 0.0;
    }
    (end / start).powf(1.0 / years) - 1.0
}

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

/// Population standard deviation (divide by N). Crate-internal; the public
/// surface exposes annualized forms.
pub(crate) fn std_pop(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let m = mean(xs);
    (xs.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / xs.len() as f64).sqrt()
}

/// Geometric annualized return: `prod(1+r)^(ppy/n) - 1`.
pub fn annualized_return(returns: &[f64], cfg: &MetricsConfig) -> f64 {
    if returns.is_empty() {
        return 0.0;
    }
    let growth: f64 = returns.iter().map(|r| 1.0 + r).product();
    if growth <= 0.0 {
        return -1.0;
    }
    growth.powf(cfg.periods_per_year / returns.len() as f64) - 1.0
}

/// Annualized volatility: population std of returns × √ppy.
pub fn annualized_volatility(returns: &[f64], cfg: &MetricsConfig) -> f64 {
    std_pop(returns) * cfg.periods_per_year.sqrt()
}

/// Annualized Sharpe ratio vs `risk_free_annual` (per-period rf = rf/ppy).
/// Returns 0 when volatility is 0.
pub fn sharpe(returns: &[f64], cfg: &MetricsConfig) -> f64 {
    let rf = cfg.risk_free_annual / cfg.periods_per_year;
    let excess: Vec<f64> = returns.iter().map(|r| r - rf).collect();
    let sd = std_pop(&excess);
    if sd <= 0.0 {
        return 0.0;
    }
    mean(&excess) / sd * cfg.periods_per_year.sqrt()
}

/// Annualized Sortino ratio; downside deviation is the root-mean-square of
/// returns below the per-period risk-free threshold,
/// `sqrt(Σ min(r - t, 0)² / N)` (no mean subtraction). Returns 0 when there
/// is no downside deviation.
pub fn sortino(returns: &[f64], cfg: &MetricsConfig) -> f64 {
    if returns.is_empty() {
        return 0.0;
    }
    let rf = cfg.risk_free_annual / cfg.periods_per_year;
    let downside_var = returns
        .iter()
        .map(|r| (r - rf).min(0.0).powi(2))
        .sum::<f64>()
        / returns.len() as f64;
    let dd = downside_var.sqrt();
    if dd <= 0.0 {
        return 0.0;
    }
    (mean(returns) - rf) / dd * cfg.periods_per_year.sqrt()
}

/// Omega ratio at the per-period risk-free threshold:
/// `Σ max(r - t, 0) / Σ max(t - r, 0)`. Returns `f64::INFINITY` when there
/// are gains but no losses, 0.0 for an empty input or all-loss input.
pub fn omega(returns: &[f64], cfg: &MetricsConfig) -> f64 {
    let t = cfg.risk_free_annual / cfg.periods_per_year;
    let gains: f64 = returns.iter().map(|r| (r - t).max(0.0)).sum();
    let losses: f64 = returns.iter().map(|r| (t - r).max(0.0)).sum();
    if losses <= 0.0 {
        return if gains > 0.0 { f64::INFINITY } else { 0.0 };
    }
    gains / losses
}

/// Maximum drawdown as a positive fraction, plus the longest underwater
/// streak in bars (consecutive bars with equity strictly below the running
/// peak).
pub fn max_drawdown(equity: &[f64]) -> (f64, usize) {
    let mut peak = f64::NEG_INFINITY;
    let mut max_dd: f64 = 0.0;
    let mut streak = 0usize;
    let mut max_streak = 0usize;
    for &e in equity {
        if e >= peak {
            peak = e;
            streak = 0;
        } else {
            if peak > 0.0 {
                max_dd = max_dd.max(1.0 - e / peak);
            }
            streak += 1;
            max_streak = max_streak.max(streak);
        }
    }
    (max_dd, max_streak)
}

/// Calmar ratio: CAGR / max drawdown. Returns 0 when max drawdown is 0.
pub fn calmar(cagr_value: f64, max_dd: f64) -> f64 {
    if max_dd <= 0.0 {
        0.0
    } else {
        cagr_value / max_dd
    }
}

/// Share of round trips with positive P&L (0 when there are no trips).
pub fn hit_rate(trips: &[RoundTrip]) -> f64 {
    if trips.is_empty() {
        return 0.0;
    }
    let wins = trips.iter().filter(|t| t.pnl > 0.0).count();
    wins as f64 / trips.len() as f64
}

/// Average win / average loss (absolute). Returns 0 when either side is
/// missing.
pub fn payoff_ratio(trips: &[RoundTrip]) -> f64 {
    let wins: Vec<f64> = trips
        .iter()
        .filter(|t| t.pnl > 0.0)
        .map(|t| t.pnl)
        .collect();
    let losses: Vec<f64> = trips
        .iter()
        .filter(|t| t.pnl < 0.0)
        .map(|t| -t.pnl)
        .collect();
    if wins.is_empty() || losses.is_empty() {
        return 0.0;
    }
    mean(&wins) / mean(&losses)
}

/// Gross profits / gross losses. Returns `f64::INFINITY` when there are
/// profits but no losses, 0 when there are none.
pub fn profit_factor(trips: &[RoundTrip]) -> f64 {
    let wins: f64 = trips.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl).sum();
    let losses: f64 = trips.iter().filter(|t| t.pnl < 0.0).map(|t| -t.pnl).sum();
    if losses <= 0.0 {
        return if wins > 0.0 { f64::INFINITY } else { 0.0 };
    }
    wins / losses
}

/// Annualized turnover: total traded amount (buys + sells) divided by mean
/// equity, divided by years. 0 for degenerate input.
pub fn turnover(traded_amount: f64, mean_equity: f64, years: f64) -> f64 {
    if mean_equity <= 0.0 || years <= 0.0 {
        0.0
    } else {
        traded_amount / mean_equity / years
    }
}

/// Beta and annualized alpha of the asset's returns against benchmark
/// returns (OLS on per-period returns; alpha = mean excess asset return
/// minus beta × mean excess benchmark return, × ppy). Inputs must be
/// equal-length; empty input yields (0, 0).
pub fn alpha_beta(asset: &[f64], benchmark: &[f64], cfg: &MetricsConfig) -> (f64, f64) {
    let n = asset.len().min(benchmark.len());
    if n == 0 {
        return (0.0, 0.0);
    }
    let (a, b) = (&asset[..n], &benchmark[..n]);
    let (ma, mb) = (mean(a), mean(b));
    let var_b: f64 = b.iter().map(|r| (r - mb) * (r - mb)).sum::<f64>() / n as f64;
    if var_b <= 0.0 {
        return (0.0, 0.0);
    }
    let cov: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (x - ma) * (y - mb))
        .sum::<f64>()
        / n as f64;
    let beta = cov / var_b;
    let rf = cfg.risk_free_annual / cfg.periods_per_year;
    let alpha = ((ma - rf) - beta * (mb - rf)) * cfg.periods_per_year;
    (alpha, beta)
}

/// Information ratio: annualized mean active return divided by annualized
/// tracking error. Returns 0 when tracking error is 0.
pub fn information_ratio(asset: &[f64], benchmark: &[f64], cfg: &MetricsConfig) -> f64 {
    let n = asset.len().min(benchmark.len());
    if n == 0 {
        return 0.0;
    }
    let active: Vec<f64> = asset[..n]
        .iter()
        .zip(&benchmark[..n])
        .map(|(a, b)| a - b)
        .collect();
    let te = std_pop(&active);
    if te <= 0.0 {
        0.0
    } else {
        mean(&active) / te * cfg.periods_per_year.sqrt()
    }
}

/// Full performance summary of one backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceReport {
    /// First curve date.
    pub start: NaiveDate,
    /// Last curve date.
    pub end: NaiveDate,
    /// `end / start - 1`.
    pub total_return: f64,
    /// Calendar-based compound annual growth rate.
    pub cagr: f64,
    /// Geometric annualized return from bar returns.
    pub annualized_return: f64,
    /// Annualized volatility of bar returns.
    pub annualized_volatility: f64,
    /// Annualized Sharpe ratio.
    pub sharpe: f64,
    /// Annualized Sortino ratio.
    pub sortino: f64,
    /// CAGR / max drawdown.
    pub calmar: f64,
    /// Omega ratio at the risk-free threshold.
    pub omega: f64,
    /// Maximum drawdown (positive fraction).
    pub max_drawdown: f64,
    /// Longest underwater streak in bars.
    pub max_drawdown_duration_bars: usize,
    /// Number of closed FIFO round trips.
    pub round_trips: usize,
    /// Fraction of round trips with positive P&L.
    pub hit_rate: f64,
    /// Average win / average loss.
    pub payoff_ratio: f64,
    /// Gross profits / gross losses.
    pub profit_factor: f64,
    /// Annualized turnover.
    pub turnover: f64,
    /// Annualized alpha vs benchmark (when provided and non-degenerate).
    pub alpha: Option<f64>,
    /// Beta vs benchmark.
    pub beta: Option<f64>,
    /// Information ratio vs benchmark.
    pub information_ratio: Option<f64>,
}

impl BacktestResult {
    /// Assemble a [`PerformanceReport`] from this run's equity curve and
    /// round trips. `benchmark` (a price series aligned by date) enables
    /// alpha/beta/information ratio.
    pub fn performance_report(
        &self,
        benchmark: Option<&PriceSeries>,
        cfg: &MetricsConfig,
    ) -> Option<PerformanceReport> {
        if self.equity.len() < 2 {
            return None;
        }
        let dates: Vec<NaiveDate> = self.equity.iter().map(|p| p.date).collect();
        let equity: Vec<f64> = self.equity.iter().map(|p| p.equity).collect();
        let returns = daily_returns(&equity);
        let years = (*dates.last().unwrap() - dates[0]).num_days() as f64 / 365.25;
        let cagr_value = cagr(equity[0], *equity.last().unwrap(), years);
        let (max_dd, max_dd_dur) = max_drawdown(&equity);
        let trips = self.round_trips();
        let mean_equity = mean(&equity);

        // Align benchmark closes to equity dates, then take returns over
        // successive common dates.
        let rel = benchmark.and_then(|bench| {
            let map: std::collections::HashMap<NaiveDate, f64> =
                bench.bars.iter().map(|b| (b.date, b.close)).collect();
            let mut asset_r = Vec::new();
            let mut bench_r = Vec::new();
            let mut prev: Option<(f64, f64)> = None;
            for p in &self.equity {
                if let Some(&bc) = map.get(&p.date) {
                    if bc > 0.0 {
                        if let Some((pe, pb)) = prev {
                            asset_r.push(p.equity / pe - 1.0);
                            bench_r.push(bc / pb - 1.0);
                        }
                        prev = Some((p.equity, bc));
                    }
                }
            }
            if asset_r.is_empty() {
                return None;
            }
            let (alpha, beta) = alpha_beta(&asset_r, &bench_r, cfg);
            let ir = information_ratio(&asset_r, &bench_r, cfg);
            Some((alpha, beta, ir))
        });

        Some(PerformanceReport {
            start: dates[0],
            end: *dates.last().unwrap(),
            total_return: total_return(&equity),
            cagr: cagr_value,
            annualized_return: annualized_return(&returns, cfg),
            annualized_volatility: annualized_volatility(&returns, cfg),
            sharpe: sharpe(&returns, cfg),
            sortino: sortino(&returns, cfg),
            calmar: calmar(cagr_value, max_dd),
            omega: omega(&returns, cfg),
            max_drawdown: max_dd,
            max_drawdown_duration_bars: max_dd_dur,
            round_trips: trips.len(),
            hit_rate: hit_rate(&trips),
            payoff_ratio: payoff_ratio(&trips),
            profit_factor: profit_factor(&trips),
            turnover: turnover(self.traded_amount(), mean_equity, years),
            alpha: rel.map(|r| r.0),
            beta: rel.map(|r| r.1),
            information_ratio: rel.map(|r| r.2),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// Hand-computed golden values. Returns [0.10, -0.05, 0.02, -0.03]:
    /// mean = 0.01, population var = 0.00335
    /// ( (0.09² + 0.06² + 0.01² + 0.04²) / 4 = 0.0134 / 4 ).
    const RETS: [f64; 4] = [0.10, -0.05, 0.02, -0.03];

    #[test]
    fn golden_sharpe_and_volatility() {
        let cfg = MetricsConfig::default();
        let vol = annualized_volatility(&RETS, &cfg);
        assert!(approx(vol, 0.00335_f64.sqrt() * 252.0_f64.sqrt()));
        let s = sharpe(&RETS, &cfg);
        assert!(approx(s, 0.01 / 0.00335_f64.sqrt() * 252.0_f64.sqrt()));
        // Numeric literals, computed independently:
        assert!(approx(vol, 0.9188035698668132));
        assert!(approx(s, 2.7426972234830256));
    }

    #[test]
    fn golden_annualized_return() {
        let cfg = MetricsConfig::default();
        // growth = 1.10 * 0.95 * 1.02 * 0.97 = 1.033923
        let growth: f64 = 1.10 * 0.95 * 1.02 * 0.97;
        let want = growth.powf(252.0 / 4.0) - 1.0;
        assert!(approx(annualized_return(&RETS, &cfg), want));
        assert!(approx(annualized_return(&RETS, &cfg), 7.180057904501059));
    }

    #[test]
    fn golden_cagr() {
        // 100 -> 133.1 over exactly 3 years: (1.331)^(1/3) - 1 = 0.1.
        assert!(approx(cagr(100.0, 133.1, 3.0), 0.1));
    }

    #[test]
    fn golden_max_drawdown() {
        let equity = [100.0, 120.0, 90.0, 110.0, 85.0, 130.0];
        let (dd, dur) = max_drawdown(&equity);
        // Trough 85 vs peak 120: 1 - 85/120 = 7/24.
        assert!(approx(dd, 7.0 / 24.0));
        // Bars 90, 110, 85 are below the running peak; 130 recovers.
        assert_eq!(dur, 3);
    }

    #[test]
    fn golden_sortino_omega_calmar() {
        let cfg = MetricsConfig::default();
        // Downside semi-deviation (rf = 0): sqrt((0.05² + 0.03²) / 4)
        // = sqrt(0.00085).
        let dd = 0.00085_f64.sqrt();
        let want_sortino = 0.01 / dd * 252.0_f64.sqrt();
        assert!(approx(sortino(&RETS, &cfg), want_sortino));
        assert!(approx(sortino(&RETS, &cfg), 5.444911277838181));
        // Omega: gains 0.10 + 0.02 = 0.12, losses 0.05 + 0.03 = 0.08 -> 1.5.
        assert!(approx(omega(&RETS, &cfg), 1.5));
        // Calmar with CAGR 0.1 and dd 7/24.
        assert!(approx(calmar(0.1, 7.0 / 24.0), 0.1 / (7.0 / 24.0)));
    }

    #[test]
    fn golden_trade_based_metrics() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let trip = |pnl: f64| RoundTrip {
            entry_date: d("2025-01-06"),
            exit_date: d("2025-01-07"),
            shares: 100,
            pnl,
            return_pct: 0.0,
        };
        let trips = vec![trip(200.0), trip(-100.0), trip(100.0), trip(-50.0)];
        assert!(approx(hit_rate(&trips), 0.5));
        // avg win 150, avg loss 75 -> payoff 2.0
        assert!(approx(payoff_ratio(&trips), 2.0));
        // gross 300 / 150 -> profit factor 2.0
        assert!(approx(profit_factor(&trips), 2.0));
    }

    #[test]
    fn golden_alpha_beta_ir() {
        // Asset = benchmark + constant 0.001 per bar: beta = 1 exactly,
        // alpha = 0.001 * 252; active return constant -> tracking error 0
        // -> information ratio 0.
        let cfg = MetricsConfig::default();
        let bench: Vec<f64> = vec![0.01, -0.02, 0.03, -0.01, 0.02];
        let asset: Vec<f64> = bench.iter().map(|b| b + 0.001).collect();
        let (alpha, beta) = alpha_beta(&asset, &bench, &cfg);
        assert!(approx(beta, 1.0));
        assert!(approx(alpha, 0.252));
        // A leveraged clone has beta 2 and zero alpha.
        let lev: Vec<f64> = bench.iter().map(|b| 2.0 * b).collect();
        let (alpha2, beta2) = alpha_beta(&lev, &bench, &cfg);
        assert!(approx(beta2, 2.0));
        assert!(approx(alpha2, 0.0));
        // Information ratio: asset = 2*bench + 0.001 -> active = bench +
        // 0.001, so mean = 0.006 + 0.001 and std = std(bench) (shift
        // invariant). std(bench): var = 0.00172 / 5 = 0.000344.
        let want_ir = 0.007 / 0.000344_f64.sqrt() * 252.0_f64.sqrt();
        assert!(approx(
            information_ratio(&lev2(&bench), &bench, &cfg),
            want_ir
        ));
    }

    fn lev2(bench: &[f64]) -> Vec<f64> {
        bench.iter().map(|b| 2.0 * b + 0.001).collect()
    }

    #[test]
    fn degenerate_inputs_are_zero_not_nan() {
        let cfg = MetricsConfig::default();
        assert_eq!(sharpe(&[], &cfg), 0.0);
        assert_eq!(sharpe(&[0.01, 0.01], &cfg), 0.0); // zero variance
        assert_eq!(sortino(&[0.01, 0.02], &cfg), 0.0); // no downside
        assert_eq!(omega(&[0.01, 0.02], &cfg), f64::INFINITY);
        assert_eq!(max_drawdown(&[]), (0.0, 0));
        assert_eq!(profit_factor(&[]), 0.0);
        assert_eq!(annualized_return(&[], &cfg), 0.0);
    }
}
