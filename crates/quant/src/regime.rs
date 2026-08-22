//! Regime detection: CUSUM change-point detection with binary
//! segmentation, and a 2-state Gaussian HMM estimated by Baum–Welch.

use crate::error::{validate_series, QuantError};

/// Default CUSUM detection threshold: the 5% critical value 1.358 of the
/// Kolmogorov distribution — the asymptotic distribution of the
/// σ-normalized CUSUM maximum under the no-change null (a Brownian
/// bridge supremum).
pub const CUSUM_THRESHOLD_5PCT: f64 = 1.358;

/// CUSUM statistic for a mean shift inside a segment.
///
/// With `x̄` the segment mean and `S_t = Σ_{i≤t} (x_i - x̄)`, returns
/// `(stat, location)` where `stat = max_t |S_t| / (σ̂ √m)` (σ̂ the segment
/// sample standard deviation, m the segment length) and `location` the
/// argmax index **relative to the segment start** — the change is placed
/// *after* that index.
fn cusum_stat(x: &[f64]) -> Result<(f64, usize), QuantError> {
    let m = x.len();
    let mean = x.iter().sum::<f64>() / m as f64;
    let sd = crate::correlation::variance(x, "cusum")?.sqrt();
    if sd == 0.0 {
        // Constant segment: no change detectable.
        return Ok((0.0, 0));
    }
    let mut s = 0.0;
    let mut best = 0.0;
    let mut best_t = 0;
    for (t, v) in x.iter().enumerate().take(m - 1) {
        s += v - mean;
        if s.abs() > best {
            best = s.abs();
            best_t = t;
        }
    }
    Ok((best / (sd * (m as f64).sqrt()), best_t))
}

/// Detect mean-shift change points by **binary segmentation over the
/// CUSUM statistic**.
///
/// Algorithm: for the current segment compute the σ-normalized CUSUM
/// maximum ([`cusum_stat`]); if it exceeds `threshold` (default
/// [`CUSUM_THRESHOLD_5PCT`] when `None`), record the argmax as a change
/// point and recurse into both sub-segments. Segments shorter than
/// `min_len` (default 10 when `None`) are not split further — CUSUM needs
/// a minimum of data to be meaningful. Returns sorted, deduplicated
/// change-point indices into the original series (change happens
/// *before* the returned index).
pub fn cusum_change_points(
    x: &[f64],
    threshold: Option<f64>,
    min_len: Option<usize>,
) -> Result<Vec<usize>, QuantError> {
    validate_series(x, 4, "cusum_change_points")?;
    let threshold = threshold.unwrap_or(CUSUM_THRESHOLD_5PCT);
    let min_len = min_len.unwrap_or(10);
    if !threshold.is_finite() || threshold <= 0.0 {
        return Err(QuantError::InvalidInput(format!(
            "cusum_change_points: threshold must be positive, got {threshold}"
        )));
    }
    if min_len < 4 {
        return Err(QuantError::InvalidInput(format!(
            "cusum_change_points: min_len must be >= 4, got {min_len}"
        )));
    }
    let mut out = Vec::new();
    bisect(x, 0, x.len(), threshold, min_len, &mut out)?;
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

fn bisect(
    x: &[f64],
    lo: usize,
    hi: usize,
    threshold: f64,
    min_len: usize,
    out: &mut Vec<usize>,
) -> Result<(), QuantError> {
    if hi - lo < min_len {
        return Ok(());
    }
    let (stat, loc) = cusum_stat(&x[lo..hi])?;
    if stat > threshold {
        // Change after relative index loc → absolute index lo + loc + 1.
        let cp = lo + loc + 1;
        out.push(cp);
        bisect(x, lo, cp, threshold, min_len, out)?;
        bisect(x, cp, hi, threshold, min_len, out)?;
    }
    Ok(())
}

/// Parameters of a 2-state Gaussian HMM.
#[derive(Debug, Clone, PartialEq)]
pub struct GaussianHmm2 {
    /// Initial state probabilities (sums to 1).
    pub initial: [f64; 2],
    /// Transition matrix `a[i][j] = P(s_{t+1} = j | s_t = i)`.
    pub transition: [[f64; 2]; 2],
    /// State means, **sorted ascending** (`means[0] <= means[1]`) to
    /// resolve the label-switching symmetry.
    pub means: [f64; 2],
    /// State standard deviations (aligned with `means`).
    pub stds: [f64; 2],
    /// Final scaled log-likelihood of the training data.
    pub log_likelihood: f64,
    /// EM iterations actually used by the winning restart.
    pub iterations: usize,
}

/// Fit a 2-state Gaussian HMM by **Baum–Welch (EM)** with scaling.
///
/// - Emission: `b_i(y) = N(y; μ_i, σ_i²)`; variances floored at 1e-8 to
///   avoid degenerate single-point states.
/// - Forward/backward use the standard rescaling (`c_t = 1/Σ α_t`), so
///   `loglik = Σ ln c_t` stays finite for long series.
/// - Convergence guard: stop when the log-likelihood improves by less
///   than `tol` (1e-6) or after `max_iter` (200) iterations.
/// - Multiple deterministic restarts: 4 initializations — quantile-split
///   means with sticky (0.95) and loose (0.8) self-transition
///   probabilities, both label orderings — best final log-likelihood
///   wins. No randomness: identical input always yields identical output.
///
/// **Identifiability caveats**: EM finds a *local* optimum; restarts
/// mitigate but do not eliminate this. The two states are only defined up
/// to relabeling (we canonicalize by sorting the means). If the true
/// process is not a 2-state Gaussian HMM, the "states" are a clustering,
/// not a structural decomposition. Requires `n ≥ 20`.
pub fn gaussian_hmm2(y: &[f64]) -> Result<GaussianHmm2, QuantError> {
    validate_series(y, 20, "gaussian_hmm2")?;
    let var = crate::correlation::variance(y, "gaussian_hmm2")?;
    if var <= 0.0 {
        return Err(QuantError::NumericalIssue(
            "gaussian_hmm2: zero-variance series".into(),
        ));
    }
    let sd = var.sqrt();
    let mean = y.iter().sum::<f64>() / y.len() as f64;
    // Deterministic restarts: (low-mean offset, high-mean offset, sticky).
    let starts: [(f64, f64, f64); 4] = [
        (-0.5, 0.5, 0.95),
        (-1.0, 1.0, 0.80),
        (0.5, -0.5, 0.95),
        (1.0, -1.0, 0.80),
    ];
    let mut best: Option<GaussianHmm2> = None;
    for (o1, o2, sticky) in starts {
        let init = GaussianHmm2 {
            initial: [0.5, 0.5],
            transition: [[sticky, 1.0 - sticky], [1.0 - sticky, sticky]],
            means: [mean + o1 * sd, mean + o2 * sd],
            stds: [sd, sd],
            log_likelihood: f64::NEG_INFINITY,
            iterations: 0,
        };
        let fit = baum_welch(y, init, 200, 1e-6);
        if best.as_ref().is_none_or(|b| fit.log_likelihood > b.log_likelihood) {
            best = Some(fit);
        }
    }
    let mut fit = best.expect("four restarts were evaluated");
    // Canonicalize label order: means ascending.
    if fit.means[0] > fit.means[1] {
        fit.means.swap(0, 1);
        fit.stds.swap(0, 1);
        fit.initial.swap(0, 1);
        fit.transition.swap(0, 1);
        for row in &mut fit.transition {
            row.swap(0, 1);
        }
    }
    Ok(fit)
}

/// One full Baum–Welch run from a given initialization.
fn baum_welch(y: &[f64], init: GaussianHmm2, max_iter: usize, tol: f64) -> GaussianHmm2 {
    let t_max = y.len();
    let mut pi = init.initial;
    let mut a = init.transition;
    let mut mu = init.means;
    let mut var = [init.stds[0].powi(2), init.stds[1].powi(2)];
    let mut prev_ll = f64::NEG_INFINITY;
    let mut iters = 0;
    let mut alpha = vec![[0.0; 2]; t_max];
    let mut c = vec![0.0; t_max];
    for it in 0..max_iter {
        iters = it + 1;
        // Emission densities.
        let emit = |i: usize, t: usize| -> f64 {
            let v = var[i].max(1e-8);
            let d = y[t] - mu[i];
            (-0.5 * (std::f64::consts::TAU.ln() + v.ln() + d * d / v)).exp()
        };
        // Scaled forward pass.
        for i in 0..2 {
            alpha[0][i] = pi[i] * emit(i, 0);
        }
        c[0] = alpha[0][0] + alpha[0][1];
        if c[0] <= 0.0 {
            break; // degenerate emissions under these parameters
        }
        alpha[0][0] /= c[0];
        alpha[0][1] /= c[0];
        for t in 1..t_max {
            for j in 0..2 {
                alpha[t][j] = (alpha[t - 1][0] * a[0][j] + alpha[t - 1][1] * a[1][j]) * emit(j, t);
            }
            c[t] = alpha[t][0] + alpha[t][1];
            if c[t] <= 0.0 {
                break;
            }
            alpha[t][0] /= c[t];
            alpha[t][1] /= c[t];
        }
        if c[t_max - 1] <= 0.0 {
            break;
        }
        let ll: f64 = c.iter().map(|v| v.ln()).sum();
        // Scaled backward pass.
        let mut beta = vec![[0.0; 2]; t_max];
        beta[t_max - 1] = [1.0, 1.0];
        for t in (0..t_max - 1).rev() {
            for i in 0..2 {
                beta[t][i] = (a[i][0] * emit(0, t + 1) * beta[t + 1][0]
                    + a[i][1] * emit(1, t + 1) * beta[t + 1][1])
                    / c[t + 1];
            }
        }
        // Posteriors and parameter updates.
        let mut gamma_sum = [0.0; 2];
        let mut mu_num = [0.0; 2];
        let mut xi_num = [[0.0; 2]; 2];
        for t in 0..t_max {
            let g0 = alpha[t][0] * beta[t][0];
            let g1 = alpha[t][1] * beta[t][1];
            let gsum = (g0 + g1).max(1e-300);
            let g = [g0 / gsum, g1 / gsum];
            for i in 0..2 {
                gamma_sum[i] += g[i];
                mu_num[i] += g[i] * y[t];
            }
            if t < t_max - 1 {
                let mut xisum = 0.0;
                let mut xi = [[0.0; 2]; 2];
                for i in 0..2 {
                    for j in 0..2 {
                        xi[i][j] = alpha[t][i] * a[i][j] * emit(j, t + 1) * beta[t + 1][j];
                        xisum += xi[i][j];
                    }
                }
                let xisum = xisum.max(1e-300);
                for i in 0..2 {
                    for j in 0..2 {
                        xi_num[i][j] += xi[i][j] / xisum;
                    }
                }
            }
        }
        for i in 0..2 {
            let gs = gamma_sum[i].max(1e-300);
            mu[i] = mu_num[i] / gs;
        }
        // Variance update needs the new means.
        let mut var_num = [0.0; 2];
        for t in 0..t_max {
            let g0 = alpha[t][0] * beta[t][0];
            let g1 = alpha[t][1] * beta[t][1];
            let gsum = (g0 + g1).max(1e-300);
            let g = [g0 / gsum, g1 / gsum];
            for i in 0..2 {
                let d = y[t] - mu[i];
                var_num[i] += g[i] * d * d;
            }
        }
        for i in 0..2 {
            let gs = gamma_sum[i].max(1e-300);
            var[i] = (var_num[i] / gs).max(1e-8);
            let xi_row: f64 = xi_num[i][0] + xi_num[i][1];
            if xi_row > 1e-300 {
                a[i][0] = xi_num[i][0] / xi_row;
                a[i][1] = xi_num[i][1] / xi_row;
            }
        }
        pi = [alpha[0][0] * beta[0][0], alpha[0][1] * beta[0][1]];
        let pisum = (pi[0] + pi[1]).max(1e-300);
        pi[0] /= pisum;
        pi[1] /= pisum;
        if ll - prev_ll < tol && ll > prev_ll {
            prev_ll = ll;
            break;
        }
        prev_ll = ll;
    }
    GaussianHmm2 {
        initial: pi,
        transition: a,
        means: mu,
        stds: [var[0].sqrt(), var[1].sqrt()],
        log_likelihood: prev_ll,
        iterations: iters,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    fn standard_normal(rng: &mut impl Rng) -> f64 {
        let u1: f64 = rng.random::<f64>().max(1e-300);
        let u2: f64 = rng.random();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    #[test]
    fn cusum_finds_planted_break() {
        // Level shifts from 0 to 5 at index 120 (noise σ = 0.5).
        let mut rng = StdRng::seed_from_u64(77);
        let mut x: Vec<f64> = (0..240).map(|_| 0.5 * standard_normal(&mut rng)).collect();
        for v in &mut x[120..] {
            *v += 5.0;
        }
        let cps = cusum_change_points(&x, None, Some(20)).unwrap();
        assert_eq!(cps.len(), 1, "cps = {cps:?}");
        assert!((cps[0] as isize - 120).abs() <= 5, "cp = {}", cps[0]);
    }

    #[test]
    fn cusum_quiet_on_stationary_series() {
        let mut rng = StdRng::seed_from_u64(13);
        let x: Vec<f64> = (0..200).map(|_| 0.5 * standard_normal(&mut rng)).collect();
        let cps = cusum_change_points(&x, None, Some(20)).unwrap();
        assert!(cps.len() <= 1, "false positives: {cps:?}");
    }

    #[test]
    fn hmm_recovers_two_regimes() {
        // Two sticky Gaussian regimes: N(0, 0.5²) and N(3, 0.8²).
        let mut rng = StdRng::seed_from_u64(31);
        let mut state = 0usize;
        let trans = [[0.97, 0.03], [0.04, 0.96]];
        let mut y = Vec::with_capacity(800);
        for _ in 0..800 {
            let u: f64 = rng.random();
            state = if u < trans[state][state] { state } else { 1 - state };
            let (m, s) = if state == 0 { (0.0, 0.5) } else { (3.0, 0.8) };
            y.push(m + s * standard_normal(&mut rng));
        }
        let fit = gaussian_hmm2(&y).unwrap();
        // Means recovered (wide tolerance: EM local optima).
        assert!((fit.means[0] - 0.0).abs() < 0.4, "mu0 = {}", fit.means[0]);
        assert!((fit.means[1] - 3.0).abs() < 0.4, "mu1 = {}", fit.means[1]);
        assert!(fit.stds[0] > 0.1 && fit.stds[0] < 1.5);
        assert!(fit.log_likelihood.is_finite());
        // Sticky transitions expected.
        assert!(fit.transition[0][0] > 0.8);
        assert!(fit.transition[1][1] > 0.8);
    }

    #[test]
    fn hmm_is_deterministic() {
        let mut rng = StdRng::seed_from_u64(8);
        let y: Vec<f64> = (0..120)
            .map(|i| if i % 40 < 20 { 0.0 } else { 2.0 } + 0.5 * standard_normal(&mut rng))
            .collect();
        let a = gaussian_hmm2(&y).unwrap();
        let b = gaussian_hmm2(&y).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(
            cusum_change_points(&[1.0, 2.0], None, None),
            Err(QuantError::InsufficientData { .. })
        ));
        assert!(matches!(
            gaussian_hmm2(&[1.0; 10]),
            Err(QuantError::InsufficientData { .. })
        ));
    }
}
