//! Lead–lag analysis: lagged cross-correlation scan and a block-bootstrap
//! significance test for the detected lag.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::error::{validate_pair, QuantError};

/// Result of a lagged cross-correlation scan.
#[derive(Debug, Clone, PartialEq)]
pub struct CrossCorrelation {
    /// Correlation for each tested lag, in the order `-max_lag … +max_lag`.
    /// Entry `i` corresponds to `lag = i - max_lag`.
    pub values: Vec<f64>,
    /// Lag with the largest absolute correlation. Sign convention:
    /// **positive lag means `x` leads `y`** — the correlation is computed
    /// between `x[t]` and `y[t + lag]`. A negative lag means `y` leads.
    pub best_lag: isize,
    /// The correlation at `best_lag` (signed).
    pub best_value: f64,
}

/// Lagged cross-correlation scan over lags `-max_lag..=max_lag`.
///
/// For lag `ℓ > 0`, correlates `x[0..n-ℓ]` with `y[ℓ..n]` — i.e. x shifted
/// *earlier*, so a positive best lag means x moves first ("x leads y by ℓ
/// periods"). For `ℓ < 0` the roles are reversed. Overlaps shorter than
/// 3 points are impossible by construction (`max_lag ≤ n - 3` enforced,
/// since Pearson needs at least 2 points and we keep a margin).
///
/// Ties in |ρ| are broken toward the lag closest to 0, then negative
/// (deterministic).
pub fn cross_correlation_scan(
    x: &[f64],
    y: &[f64],
    max_lag: usize,
) -> Result<CrossCorrelation, QuantError> {
    validate_pair(x, y, 4, "cross_correlation_scan")?;
    let n = x.len();
    if max_lag == 0 || max_lag > n - 3 {
        return Err(QuantError::InvalidInput(format!(
            "cross_correlation_scan: max_lag must be in 1..={}, got {max_lag}",
            n - 3
        )));
    }
    let mut values = Vec::with_capacity(2 * max_lag + 1);
    for lag in -(max_lag as isize)..=(max_lag as isize) {
        let (a, b) = if lag >= 0 {
            let l = lag as usize;
            (&x[..n - l], &y[l..])
        } else {
            let l = (-lag) as usize;
            (&x[l..], &y[..n - l])
        };
        values.push(crate::correlation::pearson(a, b)?);
    }
    let mut best_idx = 0usize;
    for (i, v) in values.iter().enumerate() {
        let better = v.abs() > values[best_idx].abs()
            // deterministic tie-break: closer to lag 0, then the earlier entry
            || (v.abs() == values[best_idx].abs()
                && (i as isize - max_lag as isize).abs()
                    < (best_idx as isize - max_lag as isize).abs());
        if better {
            best_idx = i;
        }
    }
    Ok(CrossCorrelation {
        best_lag: best_idx as isize - max_lag as isize,
        best_value: values[best_idx],
        values,
    })
}

/// Block-bootstrap p-value for the cross-correlation at `lag`
/// (same sign convention as [`cross_correlation_scan`]).
///
/// Null hypothesis: no lead–lag dependence at this lag. Resampling scheme:
/// **circular block bootstrap applied independently to x and y** — blocks
/// of length `block_size` are drawn with replacement (wrap-around) to
/// preserve short-run autocorrelation within each series while destroying
/// any genuine cross-series alignment. For each of `n_boot` replicates the
/// lagged correlation is recomputed; the p-value is the two-sided share
/// `#{|ρ*| ≥ |ρ_obs|} / n_boot`.
///
/// Parameters: `block_size ≥ 1`, `n_boot ≥ 99`, `n ≥ 2 * block_size + |lag| + 2`.
/// Fully deterministic for a fixed `seed`.
pub fn leadlag_bootstrap_pvalue(
    x: &[f64],
    y: &[f64],
    lag: isize,
    block_size: usize,
    n_boot: usize,
    seed: u64,
) -> Result<f64, QuantError> {
    validate_pair(x, y, 4, "leadlag_bootstrap_pvalue")?;
    let n = x.len();
    if block_size == 0 || n < 2 * block_size + lag.unsigned_abs() + 2 {
        return Err(QuantError::InvalidInput(format!(
            "leadlag_bootstrap_pvalue: block_size {block_size} too large for n = {n} and lag {lag}"
        )));
    }
    if n_boot < 99 {
        return Err(QuantError::InvalidInput(format!(
            "leadlag_bootstrap_pvalue: n_boot must be >= 99, got {n_boot}"
        )));
    }
    let obs = lagged_corr(x, y, lag)?;
    let mut rng = StdRng::seed_from_u64(seed);
    let mut exceed = 0usize;
    for _ in 0..n_boot {
        let bx = circular_block_resample(x, block_size, &mut rng);
        let by = circular_block_resample(y, block_size, &mut rng);
        let boot = lagged_corr(&bx, &by, lag)?;
        if boot.abs() >= obs.abs() {
            exceed += 1;
        }
    }
    Ok(exceed as f64 / n_boot as f64)
}

/// Circular block bootstrap: starting positions uniform over `0..n`,
/// blocks wrap around the end of the series, output truncated to n.
fn circular_block_resample(x: &[f64], block_size: usize, rng: &mut StdRng) -> Vec<f64> {
    let n = x.len();
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let start = rng.random_range(0..n);
        for k in 0..block_size {
            if out.len() < n {
                out.push(x[(start + k) % n]);
            }
        }
    }
    out
}

/// Pearson correlation between x and y at the given lag (x leads when
/// lag > 0), factored out of the bootstrap loop.
fn lagged_corr(x: &[f64], y: &[f64], lag: isize) -> Result<f64, QuantError> {
    let n = x.len();
    let (a, b) = if lag >= 0 {
        let l = lag as usize;
        (&x[..n - l], &y[l..])
    } else {
        let l = (-lag) as usize;
        (&x[l..], &y[..n - l])
    };
    crate::correlation::pearson(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;

    /// Box–Muller standard normal from the seeded test rng.
    fn standard_normal(rng: &mut impl Rng) -> f64 {
        let u1: f64 = rng.random::<f64>().max(1e-300);
        let u2: f64 = rng.random();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    #[test]
    fn scan_detects_known_lead() {
        // y is x delayed by 2 periods: y[t] = x[t - 2] → x leads y by 2,
        // so correlating x[t] with y[t + 2] must give ρ ≈ 1 at lag +2.
        let x: Vec<f64> = (0..60).map(|i| (i as f64 * 0.4).sin()).collect();
        let mut y = x.clone();
        y.rotate_right(2); // y[t] = x[t - 2]
        let scan = cross_correlation_scan(&x, &y, 5).unwrap();
        assert_eq!(scan.best_lag, 2, "scan = {scan:?}");
        assert!(scan.best_value > 0.99);
        assert_eq!(scan.values.len(), 11);
    }

    #[test]
    fn scan_zero_lag_for_synchronized() {
        let x: Vec<f64> = (0..50).map(|i| (i as f64 * 0.5).cos()).collect();
        let scan = cross_correlation_scan(&x, &x, 4).unwrap();
        assert_eq!(scan.best_lag, 0);
        assert!((scan.best_value - 1.0).abs() < 1e-12);
    }

    #[test]
    fn bootstrap_deterministic_and_sized() {
        // Noise-driven pair with a planted lead: y[t] = 0.8·x[t-2] + ε.
        // At lag +2 the link is strong; at lag 0 it is weak, so the
        // bootstrap null should reject the former and not the latter.
        let mut rng = StdRng::seed_from_u64(6);
        let n = 120;
        let x: Vec<f64> = (0..n).map(|_| standard_normal(&mut rng)).collect();
        let y: Vec<f64> = (0..n)
            .map(|t| {
                let driver = if t >= 2 { 0.8 * x[t - 2] } else { 0.0 };
                driver + standard_normal(&mut rng)
            })
            .collect();
        let p1 = leadlag_bootstrap_pvalue(&x, &y, 2, 5, 999, 42).unwrap();
        let p2 = leadlag_bootstrap_pvalue(&x, &y, 2, 5, 999, 42).unwrap();
        assert_eq!(p1, p2, "same seed must reproduce exactly");
        assert!((0.0..=1.0).contains(&p1));
        // A real lead of 2 should look significant against the null.
        assert!(p1 < 0.05, "p = {p1}");
        // At a wrong lag (0), the same data should not look significant.
        let p0 = leadlag_bootstrap_pvalue(&x, &y, 0, 5, 999, 42).unwrap();
        assert!(p0 > 0.3, "p0 = {p0}");
    }

    #[test]
    fn rejects_bad_params() {
        let x = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert!(matches!(
            cross_correlation_scan(&x, &x, 3),
            Err(QuantError::InvalidInput(_))
        ));
        assert!(matches!(
            leadlag_bootstrap_pvalue(&x, &x, 0, 0, 999, 1),
            Err(QuantError::InvalidInput(_))
        ));
    }
}
