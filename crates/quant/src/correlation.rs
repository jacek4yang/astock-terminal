//! Dependence measures: covariance, Pearson/Spearman/Kendall correlations,
//! rolling and exponentially-weighted correlation, partial correlation,
//! Ledoit–Wolf shrinkage covariance, distance correlation and a
//! histogram-based mutual information estimator.

use nalgebra::DMatrix;

use crate::error::{validate_pair, validate_series, QuantError};
use crate::matrix::validate_multi;

/// Sample variance with Bessel correction (ddof = 1):
/// `s² = Σ (x_i - x̄)² / (n - 1)`. Requires n ≥ 2.
pub fn variance(x: &[f64], context: &'static str) -> Result<f64, QuantError> {
    validate_series(x, 2, context)?;
    let n = x.len() as f64;
    let mean = x.iter().sum::<f64>() / n;
    let ss: f64 = x.iter().map(|v| (v - mean) * (v - mean)).sum();
    Ok(ss / (n - 1.0))
}

/// Sample covariance with Bessel correction (ddof = 1):
/// `cov(x, y) = Σ (x_i - x̄)(y_i - ȳ) / (n - 1)`. Requires n ≥ 2.
pub fn covariance(x: &[f64], y: &[f64], context: &'static str) -> Result<f64, QuantError> {
    validate_pair(x, y, 2, context)?;
    let n = x.len() as f64;
    let mx = x.iter().sum::<f64>() / n;
    let my = y.iter().sum::<f64>() / n;
    let s: f64 = x.iter().zip(y).map(|(a, b)| (a - mx) * (b - my)).sum();
    Ok(s / (n - 1.0))
}

/// Pearson product-moment correlation `r = cov(x, y) / (s_x s_y)`.
///
/// Errors with [`QuantError::NumericalIssue`] when either series has zero
/// variance (correlation is then undefined) rather than returning NaN.
pub fn pearson(x: &[f64], y: &[f64]) -> Result<f64, QuantError> {
    let vx = variance(x, "pearson")?;
    let vy = variance(y, "pearson")?;
    let denom = (vx * vy).sqrt();
    if denom == 0.0 {
        return Err(QuantError::NumericalIssue(
            "pearson: zero variance series — correlation undefined".into(),
        ));
    }
    let c = covariance(x, y, "pearson")?;
    // Clamp against floating-point overshoot; |r| ≤ 1 mathematically.
    Ok((c / denom).clamp(-1.0, 1.0))
}

/// Average ("fractional") ranks, 1-based, with ties sharing the mean of
/// their positions — the convention used by Spearman's ρ with tie
/// correction.
fn average_ranks(x: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..x.len()).collect();
    idx.sort_by(|&a, &b| x[a].total_cmp(&x[b]));
    let mut ranks = vec![0.0; x.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && x[idx[j + 1]] == x[idx[i]] {
            j += 1;
        }
        // Positions i..=j in the sorted order share mean rank (i + j) / 2 + 1.
        let mean_rank = (i + j) as f64 / 2.0 + 1.0;
        for &k in &idx[i..=j] {
            ranks[k] = mean_rank;
        }
        i = j + 1;
    }
    ranks
}

/// Spearman rank correlation ρ: the Pearson correlation of average ranks.
/// Ties are handled via average ranks (equivalent to the tie-corrected
/// formula). No distributional assumptions; measures monotone association.
pub fn spearman(x: &[f64], y: &[f64]) -> Result<f64, QuantError> {
    validate_pair(x, y, 2, "spearman")?;
    let rx = average_ranks(x);
    let ry = average_ranks(y);
    pearson(&rx, &ry)
}

/// Kendall's τ-b rank correlation with tie correction:
/// `τ_b = (C - D) / sqrt((n₀ - n₁)(n₀ - n₂))`
/// where C/D are concordant/discordant pair counts, `n₀ = n(n-1)/2`, and
/// `n₁`, `n₂` are the numbers of pairs tied only in x / only in y.
/// O(n²) — intended for moderate series (a few thousand points).
///
/// If every pair is tied in one variable (zero variance), τ is undefined
/// and a [`QuantError::NumericalIssue`] is returned.
pub fn kendall_tau_b(x: &[f64], y: &[f64]) -> Result<f64, QuantError> {
    validate_pair(x, y, 2, "kendall_tau_b")?;
    let n = x.len();
    let mut concordant = 0i64;
    let mut discordant = 0i64;
    let mut ties_x = 0i64; // tied in x only
    let mut ties_y = 0i64; // tied in y only
    for i in 0..n {
        for j in (i + 1)..n {
            // NB: f64::signum(0.0) == 1.0, so compare the raw differences.
            let dx = x[j] - x[i];
            let dy = y[j] - y[i];
            if dx == 0.0 && dy == 0.0 {
                continue; // tied in both: counted in neither n₁ nor n₂
            } else if dx == 0.0 {
                ties_x += 1;
            } else if dy == 0.0 {
                ties_y += 1;
            } else if (dx > 0.0) == (dy > 0.0) {
                concordant += 1;
            } else {
                discordant += 1;
            }
        }
    }
    let n0 = (n * (n - 1) / 2) as i64;
    let denom = ((n0 - ties_x) * (n0 - ties_y)) as f64;
    if denom <= 0.0 {
        return Err(QuantError::NumericalIssue(
            "kendall_tau_b: a variable is constant — tau undefined".into(),
        ));
    }
    Ok(((concordant - discordant) as f64) / denom.sqrt())
}

/// Rolling Pearson correlation with the given window.
///
/// Returns a vector of length `n - window + 1`; entry `i` is the
/// correlation over `x[i..i+window]` vs `y[i..i+window]`. No NaN padding:
/// the shorter output *is* the convention. Requires `window ≥ 2` and
/// `n ≥ window`. A zero-variance window yields an error for the whole call
/// (rolling correlations over price series with flat segments should use
/// return series instead).
pub fn rolling_correlation(x: &[f64], y: &[f64], window: usize) -> Result<Vec<f64>, QuantError> {
    if window < 2 {
        return Err(QuantError::InvalidInput(format!(
            "rolling_correlation: window must be >= 2, got {window}"
        )));
    }
    validate_pair(x, y, window, "rolling_correlation")?;
    (0..=(x.len() - window))
        .map(|i| pearson(&x[i..i + window], &y[i..i + window]))
        .collect()
}

/// Exponentially-weighted correlation (RiskMetrics-style), final value.
///
/// Recursions seeded with the first observation pair:
/// `w₁ = 1; w_t = 1 + λ w_{t-1}` (weight normalization),
/// `m^x_t = m^x_{t-1} + (x_t - m^x_{t-1}) / w_t`,
/// `c_t = c_{t-1} + (w_{t-1}/w_t) (x_t - m^x_{t-1})(y_t - m^y_{t-1})`,
/// analogously for variances; `ρ = c / sqrt(v_x v_y)`.
///
/// This mirrors Welford's algorithm with decaying weights, so it is
/// numerically stable. `lambda` must be in (0, 1); 0.94 is the RiskMetrics
/// daily default.
pub fn ewm_correlation(x: &[f64], y: &[f64], lambda: f64) -> Result<f64, QuantError> {
    validate_pair(x, y, 2, "ewm_correlation")?;
    if !(0.0..1.0).contains(&lambda) {
        return Err(QuantError::InvalidInput(format!(
            "ewm_correlation: lambda must be in (0, 1), got {lambda}"
        )));
    }
    let mut w = 1.0;
    let (mut mx, mut my) = (x[0], y[0]);
    let (mut vx, mut vy, mut cov) = (0.0, 0.0, 0.0);
    for i in 1..x.len() {
        let w_prev = w;
        w = 1.0 + lambda * w;
        let dx = x[i] - mx;
        let dy = y[i] - my;
        mx += dx / w;
        my += dy / w;
        let scale = w_prev / w;
        vx += scale * dx * (x[i] - mx);
        vy += scale * dy * (y[i] - my);
        cov += scale * dx * (y[i] - my);
    }
    let denom = (vx * vy).sqrt();
    if denom == 0.0 {
        return Err(QuantError::NumericalIssue(
            "ewm_correlation: zero weighted variance — correlation undefined".into(),
        ));
    }
    Ok((cov / denom).clamp(-1.0, 1.0))
}

/// Partial correlation of `x` and `y` given `controls`, computed via
/// *regression residuals*: regress `x` on `[1, controls]` and `y` on
/// `[1, controls]`, then take the Pearson correlation of the two residual
/// vectors. This equals the precision-matrix definition
/// `-P_xy / sqrt(P_xx P_yy)` with `P = Σ⁻¹` for jointly Gaussian data.
///
/// Requires more observations than `controls.len() + 1` and non-collinear
/// controls (enforced by the OLS helper).
pub fn partial_correlation(x: &[f64], y: &[f64], controls: &[&[f64]]) -> Result<f64, QuantError> {
    validate_pair(x, y, 3, "partial_correlation")?;
    let n = x.len();
    let k = controls.len() + 1; // + intercept
    for (i, c) in controls.iter().enumerate() {
        validate_series(c, 3, "partial_correlation")?;
        if c.len() != n {
            return Err(QuantError::InvalidInput(format!(
                "partial_correlation: control {i} length {} != {n}",
                c.len()
            )));
        }
    }
    let design = DMatrix::from_fn(n, k, |r, c| if c == 0 { 1.0 } else { controls[c - 1][r] });
    let rx = crate::ols::ols(x, &design)?.residuals;
    let ry = crate::ols::ols(y, &design)?.residuals;
    pearson(&rx, &ry)
}

/// Ledoit–Wolf (2004, "A well-conditioned estimator of large-dimensional
/// covariance matrices") shrinkage covariance with a scaled-identity
/// target `μI`.
///
/// Convention: with `X` the p×n matrix of demeaned observations and
/// `S = X Xᵀ / n` (the *biased* MLE covariance, as in the paper):
/// - `μ = tr(S)/p`, `δ² = ||S - μI||²_F / p` (distance to target),
/// - `β̄² = (1/(n² p)) Σ_k ||x_k x_kᵀ - S||²_F` (average estimation noise),
/// - `β² = min(β̄², δ²)`, intensity `α = β²/δ²` (0 when `δ² = 0`, i.e.
///   when S is already proportional to identity),
/// - estimator `Σ* = (1 - α) S + α μ I`.
///
/// The result is always well-conditioned and PSD. Input is a list of `p`
/// variable series of common length `n ≥ 2`. Returns the covariance
/// matrix (not correlation).
pub fn shrinkage_covariance(series: &[&[f64]]) -> Result<DMatrix<f64>, QuantError> {
    validate_multi(series, 2, "shrinkage_covariance")?;
    let p = series.len();
    let n = series[0].len();
    // Demean each series.
    let demeaned: Vec<Vec<f64>> = series
        .iter()
        .map(|s| {
            let m = s.iter().sum::<f64>() / n as f64;
            s.iter().map(|v| v - m).collect()
        })
        .collect();
    // S = X Xᵀ / n (biased, per the paper), computed as pairwise dot
    // products of the demeaned series.
    let mut s = DMatrix::zeros(p, p);
    for i in 0..p {
        for j in i..p {
            let dot: f64 = demeaned[i]
                .iter()
                .zip(&demeaned[j])
                .map(|(a, b)| a * b)
                .sum();
            s[(i, j)] = dot / n as f64;
            s[(j, i)] = s[(i, j)];
        }
    }
    let mu: f64 = s.diagonal().iter().sum::<f64>() / p as f64;
    // δ² = ||S - μI||²_F / p
    let delta2: f64 = s
        .iter()
        .enumerate()
        .map(|(flat, v)| {
            let (r, c) = (flat % p, flat / p); // nalgebra is column-major
            let d = if r == c { v - mu } else { *v };
            d * d
        })
        .sum::<f64>()
        / p as f64;
    if delta2 == 0.0 {
        // Already proportional to identity: no shrinkage needed.
        return Ok(s);
    }
    // β̄² = (1/(n² p)) Σ_k ||x_k x_kᵀ - S||²_F
    let mut beta_bar2 = 0.0;
    // (range loop is intentional: k is the observation index, i.e. a
    // *column* of the p×n data layout, so there is nothing to iterate)
    #[allow(clippy::needless_range_loop)]
    for k in 0..n {
        let mut acc = 0.0;
        for i in 0..p {
            for j in 0..p {
                let d = demeaned[i][k] * demeaned[j][k] - s[(i, j)];
                acc += d * d;
            }
        }
        beta_bar2 += acc;
    }
    beta_bar2 /= (n * n * p) as f64;
    let beta2 = beta_bar2.min(delta2);
    let alpha = beta2 / delta2;
    let mut out = s * (1.0 - alpha);
    for i in 0..p {
        out[(i, i)] += alpha * mu;
    }
    Ok(out)
}

/// Distance correlation (Székely, Rizzo & Bakirov, 2007).
///
/// With `A`/`B` the double-centered distance matrices of x and y
/// (`a_ij = |x_i - x_j|`, `A_ij = a_ij - ā_i· - ā_·j + ā_··`):
/// `dCov² = mean(A ∘ B)`, `dCor = dCov / sqrt(dVar_x dVar_y)`.
/// dCor ∈ [0, 1] and equals 0 iff x and y are independent (population).
/// O(n²) memory-light implementation; intended for up to a few thousand
/// points. Returns 0.0 for a constant series (both dVar are 0 — dCor is
/// defined as 0 by convention in the original paper).
pub fn distance_correlation(x: &[f64], y: &[f64]) -> Result<f64, QuantError> {
    validate_pair(x, y, 2, "distance_correlation")?;
    let a = double_centered_distances(x);
    let b = double_centered_distances(y);
    let n2 = (x.len() * x.len()) as f64;
    let dcov2: f64 = a.iter().zip(b.iter()).map(|(u, v)| u * v).sum::<f64>() / n2;
    let dvar_x: f64 = a.iter().map(|u| u * u).sum::<f64>() / n2;
    let dvar_y: f64 = b.iter().map(|u| u * u).sum::<f64>() / n2;
    if dvar_x <= 0.0 || dvar_y <= 0.0 {
        return Ok(0.0);
    }
    Ok((dcov2.max(0.0) / (dvar_x * dvar_y).sqrt())
        .sqrt()
        .clamp(0.0, 1.0))
}

/// Double-centered pairwise distance matrix (row-major flattened).
fn double_centered_distances(x: &[f64]) -> Vec<f64> {
    let n = x.len();
    let mut a = vec![0.0; n * n];
    for i in 0..n {
        for j in (i + 1)..n {
            let d = (x[i] - x[j]).abs();
            a[i * n + j] = d;
            a[j * n + i] = d;
        }
    }
    let row_mean: Vec<f64> = (0..n)
        .map(|i| a[i * n..(i + 1) * n].iter().sum::<f64>() / n as f64)
        .collect();
    let grand: f64 = row_mean.iter().sum::<f64>() / n as f64;
    for i in 0..n {
        for j in 0..n {
            a[i * n + j] = a[i * n + j] - row_mean[i] - row_mean[j] + grand;
        }
    }
    a
}

/// Mutual information (nats) via an equal-width histogram (binning)
/// estimator with `k` bins per variable:
/// `MI = Σ_ij p_ij ln( p_ij / (p_i p_j) )` over the k×k grid.
///
/// Bias caveat: this estimator is *positively biased* for finite samples
/// (expected bias ≈ (k-1)² / (2n) nats under independence) and sensitive
/// to `k`. Use it for relative comparisons at fixed `k` and `n`, not as an
/// absolute information measure. Requires `k ≥ 2` and `n ≥ 4`.
/// A constant series shares one bin with everything, giving MI = 0.
pub fn mutual_information(x: &[f64], y: &[f64], k: usize) -> Result<f64, QuantError> {
    validate_pair(x, y, 4, "mutual_information")?;
    if k < 2 {
        return Err(QuantError::InvalidInput(format!(
            "mutual_information: need at least 2 bins, got {k}"
        )));
    }
    let n = x.len();
    let bin = |v: &[f64]| -> Vec<usize> {
        let min = v.iter().copied().fold(f64::INFINITY, f64::min);
        let max = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let width = max - min;
        if width == 0.0 {
            return vec![0; v.len()];
        }
        v.iter()
            .map(|val| {
                let b = ((val - min) / width * k as f64) as usize;
                b.min(k - 1) // the max value lands in the last bin
            })
            .collect()
    };
    let bx = bin(x);
    let by = bin(y);
    let mut joint = vec![0usize; k * k];
    let mut cx = vec![0usize; k];
    let mut cy = vec![0usize; k];
    for i in 0..n {
        joint[bx[i] * k + by[i]] += 1;
        cx[bx[i]] += 1;
        cy[by[i]] += 1;
    }
    let nf = n as f64;
    let mut mi = 0.0;
    for i in 0..k {
        for j in 0..k {
            let c = joint[i * k + j];
            if c > 0 {
                let p_ij = c as f64 / nf;
                let p_i = cx[i] as f64 / nf;
                let p_j = cy[j] as f64 / nf;
                mi += p_ij * (p_ij / (p_i * p_j)).ln();
            }
        }
    }
    Ok(mi.max(0.0)) // numerical noise can produce tiny negatives
}

#[cfg(test)]
mod tests {
    use super::*;

    // Textbook dataset (x = study hours, y = exam score style), n = 5:
    // x = [1, 2, 3, 4, 5], y = [2, 4, 5, 4, 5]
    // Hand derivations:
    //   x̄ = 3, ȳ = 4.
    //   deviations: dx = [-2,-1,0,1,2], dy = [-2,0,1,0,1]
    //   Σdxdy = 4+0+0+0+2 = 6  → cov = 6/4 = 1.5
    //   Σdx² = 4+1+0+1+4 = 10 → var(x) = 2.5
    //   Σdy² = 4+0+1+0+1 = 6  → var(y) = 1.5
    //   Pearson r = 6/sqrt(10*6) = 6/sqrt(60) ≈ 0.7745966692
    //   Ranks: rx = [1,2,3,4,5]; y has a tie at value 4 (positions 2 and 4
    //   in sorted order? y sorted: 2,4,4,5,5 → ranks: 2→1, 4→2.5, 5→4.5,
    //   so ry = [1, 2.5, 4.5, 2.5, 4.5]).
    //   mean ranks: 3, 3. deviations: drx = [-2,-1,0,1,2],
    //   dry = [-2,-0.5,1.5,-0.5,1.5]
    //   Σdrxdry = 4 + 0.5 + 0 + (-0.5) + 3 = 7
    //   Σdrx² = 10, Σdry² = 4+0.25+2.25+0.25+2.25 = 9
    //   Spearman ρ = 7/sqrt(90) ≈ 0.7385489459
    //   Kendall: pairs (i<j), n0 = 10.
    //     x is strictly increasing → dx>0 always, no ties in x.
    //     y values: 2,4,5,4,5. Pair signs of dy:
    //       (1,2)+, (1,3)+, (1,4)+, (1,5)+,
    //       (2,3)+, (2,4)0, (2,5)+,
    //       (3,4)-, (3,5)0,
    //       (4,5)+
    //     C = 7, D = 1, ties_y = 2 (pairs (2,4) and (3,5)), ties_x = 0.
    //     τ_b = 6 / sqrt((10-0)(10-2)) = 6/sqrt(80) ≈ 0.6708203932
    const X: [f64; 5] = [1.0, 2.0, 3.0, 4.0, 5.0];
    const Y: [f64; 5] = [2.0, 4.0, 5.0, 4.0, 5.0];

    #[test]
    fn pearson_golden() {
        let r = pearson(&X, &Y).unwrap();
        assert!((r - 6.0 / 60.0_f64.sqrt()).abs() < 1e-12, "r = {r}");
    }

    #[test]
    fn spearman_golden_with_ties() {
        let rho = spearman(&X, &Y).unwrap();
        assert!((rho - 7.0 / 90.0_f64.sqrt()).abs() < 1e-12, "rho = {rho}");
    }

    #[test]
    fn kendall_tau_b_golden_with_ties() {
        let tau = kendall_tau_b(&X, &Y).unwrap();
        assert!((tau - 6.0 / 80.0_f64.sqrt()).abs() < 1e-12, "tau = {tau}");
    }

    #[test]
    fn kendall_no_ties_golden() {
        // x = [1,2,3], y = [1,3,2]: pairs (1,2)C, (1,3)C, (2,3)D → τ = 1/3.
        let tau = kendall_tau_b(&[1.0, 2.0, 3.0], &[1.0, 3.0, 2.0]).unwrap();
        assert!((tau - 1.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn perfect_monotone_relationships() {
        let x = [1.0, 2.0, 3.0, 4.0, 5.0];
        let y = [1.0, 4.0, 9.0, 16.0, 25.0]; // strictly monotone
        assert!((spearman(&x, &y).unwrap() - 1.0).abs() < 1e-12);
        assert!((kendall_tau_b(&x, &y).unwrap() - 1.0).abs() < 1e-12);
        // Pearson < 1 because the relation is nonlinear.
        assert!(pearson(&x, &y).unwrap() < 1.0);
        // Reversed: -1.
        assert!((spearman(&x, &[25.0, 16.0, 9.0, 4.0, 1.0]).unwrap() + 1.0).abs() < 1e-12);
    }

    #[test]
    fn rolling_correlation_golden() {
        // window 2 on x=[1,2,3], y=[2,1,2]: each 2-point window has
        // correlation ±1 (two points are perfectly collinear).
        let r = rolling_correlation(&[1.0, 2.0, 3.0], &[2.0, 1.0, 2.0], 2).unwrap();
        assert_eq!(r.len(), 2);
        assert!((r[0] + 1.0).abs() < 1e-12); // x up, y down
        assert!((r[1] - 1.0).abs() < 1e-12); // both up
    }

    #[test]
    fn ewm_correlation_bounds_and_sign() {
        let x: Vec<f64> = (0..50).map(|i| (i as f64 * 0.3).sin()).collect();
        let y_pos: Vec<f64> = x.iter().map(|v| v + 0.1).collect();
        let y_neg: Vec<f64> = x.iter().map(|v| -v).collect();
        let rp = ewm_correlation(&x, &y_pos, 0.94).unwrap();
        let rn = ewm_correlation(&x, &y_neg, 0.94).unwrap();
        assert!((rp - 1.0).abs() < 1e-9);
        assert!((rn + 1.0).abs() < 1e-9);
        assert!((-1.0..=1.0).contains(&ewm_correlation(&x, &y_pos, 0.5).unwrap()));
    }

    #[test]
    fn partial_correlation_recovers_zero() {
        // z drives both x and y: x = z + εx, y = z + εy with independent ε.
        // Controlling for z, x and y should be nearly uncorrelated.
        let z: Vec<f64> = (0..200).map(|i| (i as f64 * 0.7).sin() * 3.0).collect();
        let x: Vec<f64> = z
            .iter()
            .enumerate()
            .map(|(i, v)| v + (i as f64 * 1.3).cos() * 0.01)
            .collect();
        let y: Vec<f64> = z
            .iter()
            .enumerate()
            .map(|(i, v)| v + (i as f64 * 0.9).sin() * 0.01)
            .collect();
        let raw = pearson(&x, &y).unwrap();
        let partial = partial_correlation(&x, &y, &[&z]).unwrap();
        assert!(raw > 0.99);
        assert!(partial.abs() < 0.2, "partial = {partial}");
    }

    #[test]
    fn shrinkage_toward_diagonal() {
        // Two strongly correlated series: shrunk covariance should reduce
        // the off-diagonal relative to the sample estimator and stay PSD.
        let a: Vec<f64> = (0..100).map(|i| (i as f64 * 0.31).sin()).collect();
        let b: Vec<f64> = a.iter().map(|v| v * 1.1 + 0.05).collect();
        let shrunk = shrinkage_covariance(&[&a, &b]).unwrap();
        let sample_off = covariance(&a, &b, "test").unwrap() * 99.0 / 100.0; // biased MLE
        assert!(shrunk[(0, 1)].abs() <= sample_off.abs() + 1e-12);
        let eig = nalgebra::SymmetricEigen::new(shrunk);
        assert!(eig.eigenvalues.iter().all(|v| *v >= 0.0));
    }

    #[test]
    fn shrinkage_of_identity_is_identity() {
        // p = 1 series → S is 1×1, δ² = 0, returned unchanged.
        let out = shrinkage_covariance(&[&[1.0, 2.0, 3.0, 4.0]]).unwrap();
        assert_eq!(out.nrows(), 1);
        // Biased variance of [1,2,3,4]: mean 2.5, Σ(x-μ)² = 5 → 5/4 = 1.25
        assert!((out[(0, 0)] - 1.25).abs() < 1e-12);
    }

    #[test]
    fn distance_correlation_independence_vs_dependence() {
        // y = x² over a symmetric grid: Pearson ≈ 0 but dCor > 0.
        let x: Vec<f64> = (-20..=20).map(|i| i as f64 / 10.0).collect();
        let y: Vec<f64> = x.iter().map(|v| v * v).collect();
        assert!(pearson(&x, &y).unwrap().abs() < 1e-10);
        let dc = distance_correlation(&x, &y).unwrap();
        assert!(dc > 0.3, "dCor = {dc}");
        assert!((0.0..=1.0).contains(&dc));
    }

    #[test]
    fn mutual_information_golden_independent_grid() {
        // 4 points forming a product distribution with k=2 bins:
        // x = [0,0,1,1], y = [0,1,0,1] → joint is uniform over the 4
        // cells, marginals 0.5/0.5 → MI = Σ 0.25 ln(0.25/0.25) = 0.
        let mi = mutual_information(&[0.0, 0.0, 1.0, 1.0], &[0.0, 1.0, 0.0, 1.0], 2).unwrap();
        assert!(mi.abs() < 1e-12, "mi = {mi}");
        // Perfectly associated: x = y = [0,0,1,1] → p_ij in {0.5, 0.5},
        // MI = 2 * 0.5 * ln(0.5/0.25) = ln 2 ≈ 0.6931.
        let mi2 = mutual_information(&[0.0, 0.0, 1.0, 1.0], &[0.0, 0.0, 1.0, 1.0], 2).unwrap();
        assert!((mi2 - 2.0_f64.ln()).abs() < 1e-12, "mi2 = {mi2}");
    }

    #[test]
    fn rejects_degenerate_input() {
        assert!(matches!(
            pearson(&[1.0, 1.0], &[1.0, 2.0]),
            Err(QuantError::NumericalIssue(_))
        ));
        assert!(matches!(
            kendall_tau_b(&[1.0, 1.0], &[1.0, 2.0]),
            Err(QuantError::NumericalIssue(_))
        ));
        assert!(matches!(
            pearson(&[1.0], &[1.0]),
            Err(QuantError::InsufficientData { .. })
        ));
        assert!(matches!(
            rolling_correlation(&[1.0, 2.0], &[1.0, 2.0], 5),
            Err(QuantError::InsufficientData { .. })
        ));
    }
}
