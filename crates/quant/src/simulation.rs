//! Monte Carlo simulation, bootstrap resampling, and risk measures
//! (VaR / Expected Shortfall, drawdown statistics).
//!
//! All randomness flows through `StdRng::seed_from_u64`, so every
//! function here is exactly reproducible for a fixed seed.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::error::{validate_series, QuantError};

/// Simulate geometric Brownian motion price paths using the **exact**
/// discretization
/// `S_{t+1} = S_t · exp((μ - σ²/2) Δt + σ √Δt · Z)`, `Z ~ N(0, 1)`.
///
/// - `mu`, `sigma` are per-unit-time (matching `dt`); e.g. annual values
///   with `dt = 1/252` for daily steps.
/// - Returns `n_paths` vectors, each of length `n_steps + 1` and starting
///   at `s0` (index 0 is the initial price).
/// - Requires `s0 > 0`, `sigma ≥ 0`, `n_steps ≥ 1`, `n_paths ≥ 1`.
///   Paths stay strictly positive by construction.
pub fn gbm_paths(
    s0: f64,
    mu: f64,
    sigma: f64,
    dt: f64,
    n_steps: usize,
    n_paths: usize,
    seed: u64,
) -> Result<Vec<Vec<f64>>, QuantError> {
    if !s0.is_finite() || s0 <= 0.0 {
        return Err(QuantError::InvalidInput(format!(
            "gbm_paths: s0 must be positive, got {s0}"
        )));
    }
    if !mu.is_finite() || !sigma.is_finite() || sigma < 0.0 {
        return Err(QuantError::InvalidInput(format!(
            "gbm_paths: invalid mu={mu} or sigma={sigma}"
        )));
    }
    if !dt.is_finite() || dt <= 0.0 {
        return Err(QuantError::InvalidInput(format!(
            "gbm_paths: dt must be positive, got {dt}"
        )));
    }
    if n_steps == 0 || n_paths == 0 {
        return Err(QuantError::InvalidInput(
            "gbm_paths: n_steps and n_paths must be >= 1".into(),
        ));
    }
    let mut rng = StdRng::seed_from_u64(seed);
    let drift = (mu - 0.5 * sigma * sigma) * dt;
    let diff = sigma * dt.sqrt();
    let mut paths = Vec::with_capacity(n_paths);
    for _ in 0..n_paths {
        let mut path = Vec::with_capacity(n_steps + 1);
        let mut s = s0;
        path.push(s);
        for _ in 0..n_steps {
            let z = standard_normal(&mut rng);
            s *= (drift + diff * z).exp();
            path.push(s);
        }
        paths.push(path);
    }
    Ok(paths)
}

/// Box–Muller standard normal draw (avoids an extra dependency; uniform
/// inputs come from the seeded `StdRng`, so determinism is preserved).
fn standard_normal(rng: &mut impl Rng) -> f64 {
    let u1: f64 = rng.random::<f64>().max(1e-300);
    let u2: f64 = rng.random();
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

/// Stationary bootstrap (Politis & Romano, 1994): resample `x` into a new
/// series of length `n_out` where each step either jumps to a uniform
/// random position (probability `p = 1 / avg_block_len`) or continues to
/// the next observation (circularly). Block lengths are geometric with
/// mean `avg_block_len`, preserving short-run dependence while keeping
/// the resampled series stationary. Requires `avg_block_len ≥ 1`.
pub fn stationary_bootstrap(
    x: &[f64],
    avg_block_len: f64,
    n_out: usize,
    seed: u64,
) -> Result<Vec<f64>, QuantError> {
    validate_series(x, 2, "stationary_bootstrap")?;
    if !avg_block_len.is_finite() || avg_block_len < 1.0 {
        return Err(QuantError::InvalidInput(format!(
            "stationary_bootstrap: avg_block_len must be >= 1, got {avg_block_len}"
        )));
    }
    if n_out == 0 {
        return Err(QuantError::InvalidInput(
            "stationary_bootstrap: n_out must be >= 1".into(),
        ));
    }
    let n = x.len();
    let p = 1.0 / avg_block_len;
    let mut rng = StdRng::seed_from_u64(seed);
    let mut out = Vec::with_capacity(n_out);
    let mut pos = rng.random_range(0..n);
    for _ in 0..n_out {
        out.push(x[pos]);
        if rng.random::<f64>() < p {
            pos = rng.random_range(0..n);
        } else {
            pos = (pos + 1) % n;
        }
    }
    Ok(out)
}

/// Circular block bootstrap: resample `x` by drawing blocks of fixed
/// length `block_size` from uniform random (wrap-around) starting
/// positions until `n_out` values are produced. Fixed block length — for
/// geometric block lengths use [`stationary_bootstrap`].
pub fn block_bootstrap(
    x: &[f64],
    block_size: usize,
    n_out: usize,
    seed: u64,
) -> Result<Vec<f64>, QuantError> {
    validate_series(x, 2, "block_bootstrap")?;
    if block_size == 0 || n_out == 0 {
        return Err(QuantError::InvalidInput(
            "block_bootstrap: block_size and n_out must be >= 1".into(),
        ));
    }
    let n = x.len();
    let mut rng = StdRng::seed_from_u64(seed);
    let mut out = Vec::with_capacity(n_out);
    while out.len() < n_out {
        let start = rng.random_range(0..n);
        for k in 0..block_size {
            if out.len() < n_out {
                out.push(x[(start + k) % n]);
            }
        }
    }
    Ok(out)
}

/// Value-at-Risk and Expected Shortfall pair (positive numbers = losses).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VarEs {
    /// VaR at the requested confidence level.
    pub var: f64,
    /// Expected Shortfall at the same level.
    pub es: f64,
}

/// Loss-quantile convention used by both the historical and the
/// Monte-Carlo estimators: with sorted losses `L₁ ≤ … ≤ L_n`,
/// `VaR_α = L_⌈αn⌉` and `ES_α = mean{ L_i : L_i ≥ VaR_α }` (the average
/// of the losses at or beyond VaR — the "tail mean" definition).
fn empirical_var_es(losses: &[f64], confidence: f64) -> VarEs {
    let n = losses.len();
    let k = ((confidence * n as f64).ceil() as usize).clamp(1, n);
    let var = losses[k - 1];
    let tail: Vec<f64> = losses.iter().copied().filter(|l| *l >= var).collect();
    let es = tail.iter().sum::<f64>() / tail.len() as f64;
    VarEs { var, es }
}

/// Historical (empirical) VaR and Expected Shortfall of a return series.
///
/// Losses are defined as `-r` for each return `r` (so long positions hurt
/// when returns are negative) and VaR/ES are reported as positive loss
/// magnitudes in return units. Confidence must be in (0.5, 1).
/// See [`empirical_var_es`] for the exact quantile convention.
pub fn historical_var_es(returns: &[f64], confidence: f64) -> Result<VarEs, QuantError> {
    validate_series(returns, 10, "historical_var_es")?;
    check_confidence(confidence, "historical_var_es")?;
    let mut losses: Vec<f64> = returns.iter().map(|r| -r).collect();
    losses.sort_by(f64::total_cmp);
    Ok(empirical_var_es(&losses, confidence))
}

/// Parametric (Gaussian) VaR and ES for a P&L distribution modeled as
/// Normal(mean, std) on **returns**; reported as positive loss numbers:
/// `VaR_α = -μ + σ z_α`,
/// `ES_α = -μ + σ φ(z_α) / (1 - α)`,
/// with `z_α` the standard normal α-quantile and φ its density.
pub fn parametric_var_es(mean: f64, std: f64, confidence: f64) -> Result<VarEs, QuantError> {
    if !mean.is_finite() || !std.is_finite() || std <= 0.0 {
        return Err(QuantError::InvalidInput(format!(
            "parametric_var_es: invalid mean={mean} or std={std}"
        )));
    }
    check_confidence(confidence, "parametric_var_es")?;
    use statrs::distribution::{Continuous, ContinuousCDF, Normal};
    let norm = Normal::new(0.0, 1.0)
        .map_err(|e| QuantError::NumericalIssue(format!("parametric_var_es: {e}")))?;
    let z = norm.inverse_cdf(confidence);
    let phi = norm.pdf(z);
    Ok(VarEs {
        var: -mean + std * z,
        es: -mean + std * phi / (1.0 - confidence),
    })
}

/// Monte Carlo VaR/ES: draw `n_sims` returns from Normal(mean, std)
/// (seeded, reproducible), then apply the empirical estimator of
/// [`historical_var_es`]. Converges to [`parametric_var_es`] as
/// `n_sims → ∞`; use it when you want scenario sets rather than a
/// closed form.
pub fn mc_var_es(
    mean: f64,
    std: f64,
    confidence: f64,
    n_sims: usize,
    seed: u64,
) -> Result<VarEs, QuantError> {
    if !mean.is_finite() || !std.is_finite() || std <= 0.0 {
        return Err(QuantError::InvalidInput(format!(
            "mc_var_es: invalid mean={mean} or std={std}"
        )));
    }
    check_confidence(confidence, "mc_var_es")?;
    if n_sims < 100 {
        return Err(QuantError::InvalidInput(format!(
            "mc_var_es: need at least 100 simulations, got {n_sims}"
        )));
    }
    let mut rng = StdRng::seed_from_u64(seed);
    let mut losses: Vec<f64> = (0..n_sims)
        .map(|_| -(mean + std * standard_normal(&mut rng)))
        .collect();
    losses.sort_by(f64::total_cmp);
    Ok(empirical_var_es(&losses, confidence))
}

fn check_confidence(confidence: f64, context: &'static str) -> Result<(), QuantError> {
    if !(0.5..1.0).contains(&confidence) {
        return Err(QuantError::InvalidInput(format!(
            "{context}: confidence must be in (0.5, 1), got {confidence}"
        )));
    }
    Ok(())
}

/// Maximum drawdown of an equity/price curve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaxDrawdown {
    /// Depth as a positive fraction of the running peak:
    /// `(peak - trough) / peak`. 0 for a monotone non-decreasing curve.
    pub depth: f64,
    /// Index of the peak that the max drawdown is measured from.
    pub peak_idx: usize,
    /// Index of the trough.
    pub trough_idx: usize,
}

/// Maximum peak-to-trough drawdown of a curve.
///
/// The running peak is the historical maximum so far; the drawdown at t
/// is `(peak_t - x_t) / peak_t`. Returns the largest such value with its
/// peak/trough indices. Requires strictly positive values (equity
/// curves); a monotone rising curve yields depth 0 with both indices 0.
pub fn max_drawdown(equity: &[f64]) -> Result<MaxDrawdown, QuantError> {
    validate_series(equity, 1, "max_drawdown")?;
    if equity.iter().any(|v| *v <= 0.0) {
        return Err(QuantError::InvalidInput(
            "max_drawdown: equity values must be strictly positive".into(),
        ));
    }
    let mut peak = equity[0];
    let mut peak_idx = 0usize;
    let mut best = MaxDrawdown {
        depth: 0.0,
        peak_idx: 0,
        trough_idx: 0,
    };
    for (t, &v) in equity.iter().enumerate() {
        if v > peak {
            peak = v;
            peak_idx = t;
        }
        let dd = (peak - v) / peak;
        if dd > best.depth {
            best = MaxDrawdown {
                depth: dd,
                peak_idx,
                trough_idx: t,
            };
        }
    }
    Ok(best)
}

/// Longest drawdown duration, in **periods spent underwater**: the
/// longest run of consecutive observations strictly below the running
/// peak at the time (a new high resets the count). A curve that never
/// recovers from its last peak counts through the final observation.
/// A monotone rising curve has duration 0.
pub fn max_drawdown_duration(equity: &[f64]) -> Result<usize, QuantError> {
    validate_series(equity, 1, "max_drawdown_duration")?;
    let mut peak = equity[0];
    let mut current = 0usize;
    let mut longest = 0usize;
    for &v in equity {
        if v >= peak {
            peak = v;
            current = 0;
        } else {
            current += 1;
            longest = longest.max(current);
        }
    }
    Ok(longest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gbm_deterministic_and_positive() {
        let a = gbm_paths(100.0, 0.08, 0.2, 1.0 / 252.0, 50, 4, 123).unwrap();
        let b = gbm_paths(100.0, 0.08, 0.2, 1.0 / 252.0, 50, 4, 123).unwrap();
        assert_eq!(a, b, "same seed must reproduce identical paths");
        assert_eq!(a.len(), 4);
        assert_eq!(a[0].len(), 51);
        assert!(a.iter().flatten().all(|v| *v > 0.0));
        assert!(a.iter().all(|p| p[0] == 100.0));
        // Different seed → different paths.
        let c = gbm_paths(100.0, 0.08, 0.2, 1.0 / 252.0, 50, 4, 124).unwrap();
        assert_ne!(a, c);
    }

    #[test]
    fn gbm_zero_vol_is_deterministic_growth() {
        // σ = 0: S_t = 100 · exp(μ Δt · t) exactly.
        let paths = gbm_paths(100.0, 0.10, 0.0, 0.01, 10, 1, 0).unwrap();
        let expect = 100.0 * (0.10_f64 * 0.01 * 10.0).exp();
        assert!((paths[0][10] - expect).abs() < 1e-10);
    }

    #[test]
    fn bootstrap_deterministic_and_from_sample() {
        let x: Vec<f64> = (0..50).map(|i| i as f64 * 0.1).collect();
        let a = stationary_bootstrap(&x, 5.0, 100, 9).unwrap();
        let b = stationary_bootstrap(&x, 5.0, 100, 9).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 100);
        assert!(a.iter().all(|v| x.contains(v)));
        let c = block_bootstrap(&x, 4, 80, 9).unwrap();
        let d = block_bootstrap(&x, 4, 80, 9).unwrap();
        assert_eq!(c, d);
        assert!(c.iter().all(|v| x.contains(v)));
    }

    #[test]
    fn historical_var_es_golden_exact() {
        // Hand-computed on a known discrete distribution: build returns
        // whose losses are exactly 1, 2, …, 100 (returns -1 … -100,
        // arbitrary order). With the convention VaR_α = L_⌈αn⌉:
        //   α = 0.95, n = 100 → k = 95 → VaR = 95.
        //   ES = mean(95, 96, 97, 98, 99, 100) = 585/6 = 97.5.
        //   α = 0.90 → k = 90 → VaR = 90, ES = mean(90..=100) = 1045/11 = 95.
        let returns: Vec<f64> = (1..=100).map(|i| -(i as f64)).collect();
        let r95 = historical_var_es(&returns, 0.95).unwrap();
        assert_eq!(r95.var, 95.0);
        assert!((r95.es - 97.5).abs() < 1e-12);
        let r90 = historical_var_es(&returns, 0.90).unwrap();
        assert_eq!(r90.var, 90.0);
        assert!((r90.es - 95.0).abs() < 1e-12);
    }

    #[test]
    fn parametric_var_es_golden_standard_normal() {
        // μ = 0, σ = 1, α = 0.95: z = 1.6448536269515,
        // φ(z) = 0.10313564…, ES = φ(z)/0.05 = 2.0627128…
        // (standard textbook Normal VaR/ES values).
        let r = parametric_var_es(0.0, 1.0, 0.95).unwrap();
        assert!((r.var - 1.6448536269514729).abs() < 1e-9, "var = {}", r.var);
        assert!((r.es - 2.0627128070442834).abs() < 1e-6, "es = {}", r.es);
        // ES must exceed VaR at the same level.
        assert!(r.es > r.var);
    }

    #[test]
    fn mc_var_es_approaches_parametric() {
        let mc = mc_var_es(0.0, 1.0, 0.95, 200_000, 5).unwrap();
        let pa = parametric_var_es(0.0, 1.0, 0.95).unwrap();
        assert!((mc.var - pa.var).abs() < 0.05, "mc var = {}", mc.var);
        assert!((mc.es - pa.es).abs() < 0.05, "mc es = {}", mc.es);
    }

    #[test]
    fn max_drawdown_golden_hand_cases() {
        // [100, 120, 90, 110]: running peak 120 at idx 1, trough 90 at
        // idx 2 → depth (120-90)/120 = 0.25.
        let mdd = max_drawdown(&[100.0, 120.0, 90.0, 110.0]).unwrap();
        assert!((mdd.depth - 0.25).abs() < 1e-12);
        assert_eq!((mdd.peak_idx, mdd.trough_idx), (1, 2));
        // Monotone rising: depth 0.
        let flat = max_drawdown(&[1.0, 2.0, 3.0, 4.0]).unwrap();
        assert_eq!(flat.depth, 0.0);
        // Duration: [100, 120, 90, 110, 130, 80] → underwater at 2,3
        // (recovered at 4 with 130), then 5 (never recovers) → longest 2.
        let dur = max_drawdown_duration(&[100.0, 120.0, 90.0, 110.0, 130.0, 80.0]).unwrap();
        assert_eq!(dur, 2);
        assert_eq!(max_drawdown_duration(&[1.0, 2.0, 3.0]).unwrap(), 0);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(
            gbm_paths(-1.0, 0.1, 0.2, 0.01, 10, 1, 0),
            Err(QuantError::InvalidInput(_))
        ));
        assert!(matches!(
            historical_var_es(&[0.01; 20], 1.5),
            Err(QuantError::InvalidInput(_))
        ));
        assert!(matches!(
            max_drawdown(&[100.0, -5.0]),
            Err(QuantError::InvalidInput(_))
        ));
    }
}
