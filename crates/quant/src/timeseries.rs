//! Time-series econometrics: ADF and KPSS stationarity tests, Granger
//! causality, AR(p) estimation, GARCH(1,1) MLE and a 1-D local-level
//! Kalman filter.
//!
//! ARIMA is **consciously omitted**: a conditional-least-squares ARIMA
//! adds little beyond the AR(p)-by-OLS estimator provided here, and a
//! proper state-space ML ARIMA is out of scope for this crate.

use nalgebra::DMatrix;

use crate::error::{validate_pair, validate_series, QuantError};
use crate::ols::ols;

/// Result of an Augmented Dickey–Fuller test.
#[derive(Debug, Clone, PartialEq)]
pub struct AdfResult {
    /// The ADF t-statistic on the lagged level coefficient β.
    pub statistic: f64,
    /// MacKinnon asymptotic critical values (1%, 5%, 10%) for the
    /// "constant, no trend" case: -3.43, -2.86, -2.57.
    pub critical_values: [f64; 3],
    /// Number of lagged difference terms used in the regression.
    pub lags: usize,
    /// Reject the unit-root null at the 5% level (statistic < -2.86).
    pub reject_unit_root_5pct: bool,
}

/// Augmented Dickey–Fuller test with a constant and **no time trend**:
///
/// `Δx_t = α + β x_{t-1} + Σ_{i=1..lags} γ_i Δx_{t-i} + ε_t`
///
/// H0: β = 0 (unit root / non-stationary). The test statistic is the OLS
/// t-ratio of β; more negative ⇒ more evidence of stationarity. Critical
/// values are the MacKinnon asymptotic ones for the constant-no-trend
/// case (1%: -3.43, 5%: -2.86, 10%: -2.57), hardcoded because they do not
/// follow a standard distribution; we report statistic-vs-critical-value
/// ordering rather than an interpolated p-value. Choose `lags` to remove
/// residual autocorrelation (0 = plain DF test). Requires
/// `n ≥ lags + 5` so the regression has degrees of freedom.
pub fn adf_test(x: &[f64], lags: usize) -> Result<AdfResult, QuantError> {
    validate_series(x, lags + 5, "adf_test")?;
    let n = x.len();
    // Regression sample: t runs from lags+1 .. n (1-based), i.e.
    // effective rows m = n - lags - 1.
    let m = n - lags - 1;
    let k = 2 + lags; // intercept, x_{t-1}, lagged diffs
    let mut y = Vec::with_capacity(m);
    let design = DMatrix::from_fn(m, k, |row, col| {
        let t = row + lags + 1; // 0-based index into x
        match col {
            0 => 1.0,
            1 => x[t - 1],
            c => x[t - c + 1] - x[t - c], // Δx_{t-c+1}
        }
    });
    for t in (lags + 1)..n {
        y.push(x[t] - x[t - 1]);
    }
    let fit = ols(&y, &design)?;
    let beta_var = fit.coeff_var[1];
    if beta_var <= 0.0 {
        return Err(QuantError::NumericalIssue(
            "adf_test: non-positive variance of the level coefficient".into(),
        ));
    }
    let stat = fit.coeffs[1] / beta_var.sqrt();
    let critical_values = [-3.43, -2.86, -2.57];
    Ok(AdfResult {
        statistic: stat,
        critical_values,
        lags,
        reject_unit_root_5pct: stat < critical_values[1],
    })
}

/// Result of a KPSS level-stationarity test.
#[derive(Debug, Clone, PartialEq)]
pub struct KpssResult {
    /// The KPSS η statistic.
    pub statistic: f64,
    /// Asymptotic critical values (1%, 5%, 10%): 0.739, 0.463, 0.347
    /// (Kwiatkowski et al. 1992, Table 1, level-stationarity row).
    pub critical_values: [f64; 3],
    /// Bartlett bandwidth used for the long-run variance.
    pub lags: usize,
    /// Reject the *stationarity* null at 5% (statistic > 0.463). Note the
    /// hypotheses are reversed relative to ADF: H0 is "stationary".
    pub reject_stationarity_5pct: bool,
}

/// KPSS test of **level stationarity** (Kwiatkowski, Phillips, Schmidt &
/// Shin, 1992).
///
/// With `e_t = x_t - x̄` the residuals from a regression on a constant,
/// `S_t = Σ_{i≤t} e_i` the partial sums, and `s²(l)` the Bartlett
/// long-run-variance estimator with weights `w_j = 1 - j/(l+1)`:
/// `η = Σ_t S_t² / (n² s²(l))`.
///
/// H0: the series is (level) stationary — the *opposite* of ADF. Reject
/// when η exceeds the critical value. Default bandwidth when `lags` is
/// `None`: `trunc(4 (n/100)^(1/4))`, a common rule of thumb. Critical
/// values are the asymptotic ones from the original paper (1%: 0.739,
/// 5%: 0.463, 10%: 0.347), hardcoded as no standard CDF applies.
pub fn kpss_test(x: &[f64], lags: Option<usize>) -> Result<KpssResult, QuantError> {
    validate_series(x, 8, "kpss_test")?;
    let n = x.len();
    let l = lags.unwrap_or_else(|| (4.0 * (n as f64 / 100.0).powf(0.25)) as usize);
    if l >= n / 2 {
        return Err(QuantError::InvalidInput(format!(
            "kpss_test: bandwidth {l} too large for n = {n}"
        )));
    }
    let mean = x.iter().sum::<f64>() / n as f64;
    let e: Vec<f64> = x.iter().map(|v| v - mean).collect();
    // Long-run variance with Bartlett weights.
    let gamma0: f64 = e.iter().map(|v| v * v).sum::<f64>() / n as f64;
    let mut s2 = gamma0;
    for j in 1..=l {
        let w = 1.0 - j as f64 / (l + 1) as f64;
        let cov: f64 = (j..n).map(|t| e[t] * e[t - j]).sum::<f64>() / n as f64;
        s2 += 2.0 * w * cov;
    }
    if s2 <= 0.0 {
        return Err(QuantError::NumericalIssue(
            "kpss_test: non-positive long-run variance (constant series?)".into(),
        ));
    }
    let mut s_partial = 0.0;
    let mut ssq = 0.0;
    for v in &e {
        s_partial += v;
        ssq += s_partial * s_partial;
    }
    let eta = ssq / ((n * n) as f64 * s2);
    let critical_values = [0.739, 0.463, 0.347];
    Ok(KpssResult {
        statistic: eta,
        critical_values,
        lags: l,
        reject_stationarity_5pct: eta > critical_values[1],
    })
}

/// Result of a bivariate Granger-causality F-test.
#[derive(Debug, Clone, PartialEq)]
pub struct GrangerResult {
    /// F statistic with (k, n - 2k - 1) degrees of freedom.
    pub f_stat: f64,
    /// p-value from the Fisher–Snedecor distribution.
    pub p_value: f64,
    /// Lag order used.
    pub lags: usize,
}

/// Bivariate Granger causality test: "does `x` Granger-cause `y`?"
///
/// Compares the restricted model `y_t = c + Σ_{i=1..k} a_i y_{t-i}` with
/// the unrestricted model that adds `Σ b_i x_{t-i}`:
/// `F = ((RSS_r - RSS_u)/k) / (RSS_u / (n_eff - 2k - 1))`
/// with `n_eff = n - k` usable observations. The p-value comes from the
/// F(k, n_eff - 2k - 1) distribution via `statrs`. H0: x does not
/// Granger-cause y (all b_i = 0). Requires `n ≥ 2k + 4`.
pub fn granger_causality(x: &[f64], y: &[f64], lags: usize) -> Result<GrangerResult, QuantError> {
    if lags == 0 {
        return Err(QuantError::InvalidInput(
            "granger_causality: lags must be >= 1".into(),
        ));
    }
    validate_pair(x, y, 2 * lags + 4, "granger_causality")?;
    let n = x.len();
    let m = n - lags; // usable rows
                      // Restricted: [1, y_{t-1..t-k}]
    let xr = DMatrix::from_fn(m, 1 + lags, |row, col| {
        if col == 0 {
            1.0
        } else {
            y[row + lags - col]
        }
    });
    // Unrestricted: [1, y lags, x lags]
    let xu = DMatrix::from_fn(m, 1 + 2 * lags, |row, col| {
        if col == 0 {
            1.0
        } else if col <= lags {
            y[row + lags - col]
        } else {
            x[row + 2 * lags - col]
        }
    });
    let dep: Vec<f64> = y[lags..].to_vec();
    let fit_r = ols(&dep, &xr)?;
    let fit_u = ols(&dep, &xu)?;
    let df2 = m as f64 - (2 * lags + 1) as f64;
    if fit_u.rss <= 0.0 || df2 <= 0.0 {
        return Err(QuantError::NumericalIssue(
            "granger_causality: degenerate unrestricted fit".into(),
        ));
    }
    let f = ((fit_r.rss - fit_u.rss) / lags as f64) / (fit_u.rss / df2);
    let dist = statrs::distribution::FisherSnedecor::new(lags as f64, df2)
        .map_err(|e| QuantError::NumericalIssue(format!("granger_causality: {e}")))?;
    use statrs::distribution::ContinuousCDF;
    let p = 1.0 - dist.cdf(f.max(0.0));
    Ok(GrangerResult {
        f_stat: f,
        p_value: p.clamp(0.0, 1.0),
        lags,
    })
}

/// Result of fitting an AR(p) model.
#[derive(Debug, Clone, PartialEq)]
pub struct ArFit {
    /// Intercept c.
    pub intercept: f64,
    /// AR coefficients [φ₁ … φₚ].
    pub coeffs: Vec<f64>,
    /// Residual variance `RSS / (n - p - 1)`.
    pub sigma2: f64,
}

/// AR(p) by **ordinary least squares** (conditional on the first p
/// observations): `x_t = c + Σ φ_i x_{t-i} + ε_t`. OLS is used rather than
/// Yule–Walker because it handles the intercept directly and does not
/// force a stationary solution; the trade-off is that OLS estimates can
/// be non-stationary for short samples. Requires `n ≥ p + 3`.
pub fn ar_ols(x: &[f64], p: usize) -> Result<ArFit, QuantError> {
    if p == 0 {
        return Err(QuantError::InvalidInput("ar_ols: p must be >= 1".into()));
    }
    validate_series(x, p + 3, "ar_ols")?;
    let n = x.len();
    let m = n - p;
    let design = DMatrix::from_fn(
        m,
        p + 1,
        |row, col| {
            if col == 0 {
                1.0
            } else {
                x[row + p - col]
            }
        },
    );
    let dep: Vec<f64> = x[p..].to_vec();
    let fit = ols(&dep, &design)?;
    Ok(ArFit {
        intercept: fit.coeffs[0],
        coeffs: fit.coeffs[1..].to_vec(),
        sigma2: fit.sigma2,
    })
}

/// GARCH(1,1) parameter estimates by Gaussian maximum likelihood.
#[derive(Debug, Clone, PartialEq)]
pub struct GarchFit {
    /// ω > 0 (long-run variance scale).
    pub omega: f64,
    /// α ≥ 0 (reaction to last shock).
    pub alpha: f64,
    /// β ≥ 0 (volatility persistence).
    pub beta: f64,
    /// Maximized Gaussian log-likelihood.
    pub log_likelihood: f64,
    /// α + β — volatility persistence (< 1 by constraint).
    pub persistence: f64,
}

/// GARCH(1,1) by Gaussian quasi-MLE:
/// `σ²_t = ω + α ε²_{t-1} + β σ²_{t-1}`,
/// `-2 LL = Σ [ln(2π) + ln σ²_t + ε²_t/σ²_t]`.
///
/// Conventions and numerical guards:
/// - The recursion is seeded at the unconditional sample variance
///   `σ²_1 = var(ε)` (standard "backcast" simplification).
/// - Variances are floored at `1e-12` so `ln σ²` never blows up.
/// - Parameters are optimized over an unconstrained transform
///   `ω = e^a`, `α = 0.999 u/(1+u+v)`, `β = 0.999 v/(1+u+v)` with
///   `u, v = e^{b,c}`, enforcing ω > 0, α, β ≥ 0 and α + β ≤ 0.999
///   (covariance-stationarity guard).
/// - Optimizer: Nelder–Mead from four deterministic starting points
///   (covering low/high persistence corners); the best final likelihood
///   wins. No randomness → fully deterministic.
///
/// Requires `n ≥ 30`; meaningful estimates need a few hundred points.
pub fn garch11_mle(returns: &[f64]) -> Result<GarchFit, QuantError> {
    validate_series(returns, 30, "garch11_mle")?;
    let var0 = crate::correlation::variance(returns, "garch11_mle")?;
    if var0 <= 0.0 {
        return Err(QuantError::NumericalIssue(
            "garch11_mle: zero-variance series".into(),
        ));
    }
    let neg_ll = |p: &[f64]| -> f64 {
        let omega = p[0].exp();
        let u = p[1].exp();
        let v = p[2].exp();
        let alpha = 0.999 * u / (1.0 + u + v);
        let beta = 0.999 * v / (1.0 + u + v);
        let mut sigma2 = var0;
        let mut ll = 0.0;
        for &r in returns {
            // σ²_t is formed from ε²_{t-1} and σ²_{t-1}: evaluate the
            // likelihood of ε_t *first*, then update the recursion.
            let s = sigma2.max(1e-12);
            ll += (std::f64::consts::TAU).ln() + s.ln() + r * r / s;
            sigma2 = omega + alpha * r * r + beta * sigma2;
        }
        0.5 * ll
    };
    // Deterministic multi-start: (log ω vs sample var, low/high α, low/high β)
    let base = var0.ln();
    let starts: Vec<[f64; 3]> = vec![
        [base - 2.0, -2.0, 2.0], // ω small, α≈0.11, β≈0.86
        [base - 1.0, -1.0, 0.5], // moderate
        [base, 0.0, -1.0],       // α high, β low
        [base - 3.0, -3.0, 3.0], // very persistent
    ];
    let mut best: Option<(Vec<f64>, f64)> = None;
    for s in starts {
        let (p, f) = nelder_mead(&neg_ll, &s, 0.5, 2000, 1e-8);
        if best.as_ref().is_none_or(|(_, bf)| f < *bf) {
            best = Some((p, f));
        }
    }
    let (p, f) = best.expect("at least one start was evaluated");
    let omega = p[0].exp();
    let u = p[1].exp();
    let v = p[2].exp();
    let alpha = 0.999 * u / (1.0 + u + v);
    let beta = 0.999 * v / (1.0 + u + v);
    Ok(GarchFit {
        omega,
        alpha,
        beta,
        log_likelihood: -f,
        persistence: alpha + beta,
    })
}

/// Minimal Nelder–Mead simplex optimizer (minimization), standard
/// coefficients α=1, γ=2, ρ=0.5, σ=0.5. Deterministic given the start.
fn nelder_mead(
    f: &dyn Fn(&[f64]) -> f64,
    x0: &[f64; 3],
    step: f64,
    max_iter: usize,
    tol: f64,
) -> (Vec<f64>, f64) {
    let n = 3;
    let mut simplex: Vec<Vec<f64>> = vec![x0.to_vec()];
    for i in 0..n {
        let mut v = x0.to_vec();
        v[i] += step;
        simplex.push(v);
    }
    let mut vals: Vec<f64> = simplex.iter().map(|p| f(p)).collect();
    for _ in 0..max_iter {
        // Order by function value.
        let mut order: Vec<usize> = (0..=n).collect();
        order.sort_by(|&a, &b| vals[a].total_cmp(&vals[b]));
        let sorted: Vec<Vec<f64>> = order.iter().map(|&i| simplex[i].clone()).collect();
        let svals: Vec<f64> = order.iter().map(|&i| vals[i]).collect();
        // Convergence: spread of values below tol.
        if svals[n] - svals[0] < tol {
            return (sorted[0].clone(), svals[0]);
        }
        simplex = sorted;
        vals = svals;
        let centroid: Vec<f64> = (0..n)
            .map(|d| simplex[..n].iter().map(|p| p[d]).sum::<f64>() / n as f64)
            .collect();
        let worst = &simplex[n];
        // Reflect
        let xr: Vec<f64> = (0..n)
            .map(|d| centroid[d] + (centroid[d] - worst[d]))
            .collect();
        let fr = f(&xr);
        if fr < vals[0] {
            // Expand
            let xe: Vec<f64> = (0..n)
                .map(|d| centroid[d] + 2.0 * (xr[d] - centroid[d]))
                .collect();
            let fe = f(&xe);
            if fe < fr {
                simplex[n] = xe;
                vals[n] = fe;
            } else {
                simplex[n] = xr;
                vals[n] = fr;
            }
        } else if fr < vals[n - 1] {
            simplex[n] = xr;
            vals[n] = fr;
        } else {
            // Contract
            let xc: Vec<f64> = (0..n)
                .map(|d| centroid[d] + 0.5 * (worst[d] - centroid[d]))
                .collect();
            let fc = f(&xc);
            if fc < vals[n] {
                simplex[n] = xc;
                vals[n] = fc;
            } else {
                // Shrink toward best
                let best0 = simplex[0].clone();
                for i in 1..=n {
                    for d in 0..n {
                        simplex[i][d] = best0[d] + 0.5 * (simplex[i][d] - best0[d]);
                    }
                    vals[i] = f(&simplex[i]);
                }
            }
        }
    }
    // Return best after iteration budget.
    let (idx, _) = vals
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .expect("simplex is non-empty");
    (simplex[idx].clone(), vals[idx])
}

/// Filtered state path of a 1-D **local-level (random-walk-plus-noise)**
/// Kalman filter.
///
/// Model: state `μ_t = μ_{t-1} + w_t`, `w ~ N(0, q)`; observation
/// `y_t = μ_t + v_t`, `v ~ N(0, r)`.
/// - Predict: `μ⁻ = μ_{t-1}`, `P⁻ = P_{t-1} + q`
/// - Update: `K = P⁻/(P⁻ + r)`,
///   `μ_t = μ⁻ + K (y_t - μ⁻)`, `P_t = (1 - K) P⁻`
///
/// Initialization: `μ_0 = y_0`, `P_0 = r` (the first observation is the
/// best guess of the level; its uncertainty is the observation noise).
/// Output has the same length as the input and contains the *filtered*
/// (not smoothed) states. `q` and `r` must be positive; the ratio q/r
/// controls how fast the filter adapts.
pub fn kalman_local_level(y: &[f64], q: f64, r: f64) -> Result<Vec<f64>, QuantError> {
    validate_series(y, 1, "kalman_local_level")?;
    if !q.is_finite() || q <= 0.0 || !r.is_finite() || r <= 0.0 {
        return Err(QuantError::InvalidInput(format!(
            "kalman_local_level: q and r must be positive and finite, got q={q}, r={r}"
        )));
    }
    let mut states = Vec::with_capacity(y.len());
    let mut mu = y[0];
    let mut p = r;
    states.push(mu);
    for &obs in &y[1..] {
        // Predict
        let p_pred = p + q;
        // Update
        let k_gain = p_pred / (p_pred + r);
        mu += k_gain * (obs - mu);
        p = (1.0 - k_gain) * p_pred;
        states.push(mu);
    }
    Ok(states)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use rand_distr_helpers::standard_normal;

    // A tiny local helper module to draw standard normals without adding
    // rand_distr as a dependency: Box–Muller on two uniforms.
    mod rand_distr_helpers {
        use rand::Rng;
        pub fn standard_normal(rng: &mut impl Rng) -> f64 {
            let u1: f64 = rng.random::<f64>().max(1e-300);
            let u2: f64 = rng.random();
            (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
        }
    }

    /// Near-unit-root AR(1) with φ = 0.98: ADF should NOT reject the
    /// unit-root null (statistic above the 5% critical value -2.86),
    /// while a clearly stationary AR(1) with φ = 0.3 should reject.
    fn ar1_series(phi: f64, n: usize, seed: u64) -> Vec<f64> {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut x = vec![0.0; n];
        for i in 1..n {
            x[i] = phi * x[i - 1] + standard_normal(&mut rng);
        }
        x
    }

    #[test]
    fn adf_orders_statistic_against_critical_values() {
        let near_unit = ar1_series(0.98, 500, 7);
        let stationary = ar1_series(0.3, 500, 7);
        let r_unit = adf_test(&near_unit, 0).unwrap();
        let r_stat = adf_test(&stationary, 0).unwrap();
        // Ordering check (not exact p): stationary series yields a much
        // more negative statistic, below the 1% critical value.
        assert!(r_stat.statistic < r_unit.statistic);
        assert!(
            r_stat.statistic < r_stat.critical_values[0],
            "stat = {}",
            r_stat.statistic
        );
        assert!(r_stat.reject_unit_root_5pct);
        assert!(
            r_unit.statistic > r_unit.critical_values[1],
            "stat = {}",
            r_unit.statistic
        );
        assert!(!r_unit.reject_unit_root_5pct);
    }

    #[test]
    fn kpss_reversed_hypotheses() {
        let near_unit = ar1_series(1.0, 300, 11); // pure random walk
        let stationary = ar1_series(0.2, 300, 11);
        let r_walk = kpss_test(&near_unit, None).unwrap();
        let r_stat = kpss_test(&stationary, None).unwrap();
        // H0 is stationarity: random walk must reject, stationary must not.
        assert!(
            r_walk.reject_stationarity_5pct,
            "eta = {}",
            r_walk.statistic
        );
        assert!(
            !r_stat.reject_stationarity_5pct,
            "eta = {}",
            r_stat.statistic
        );
        assert!(r_walk.statistic > r_stat.statistic);
    }

    #[test]
    fn granger_detects_driver() {
        // x drives y with one-period lag: y[t] = 0.8 x[t-1] + small noise.
        let mut rng = StdRng::seed_from_u64(5);
        let n = 400;
        let x: Vec<f64> = (0..n).map(|_| standard_normal(&mut rng)).collect();
        let y: Vec<f64> = (0..n)
            .map(|t| {
                let driver = if t > 0 { 0.8 * x[t - 1] } else { 0.0 };
                driver + 0.2 * standard_normal(&mut rng)
            })
            .collect();
        let forward = granger_causality(&x, &y, 1).unwrap();
        let reverse = granger_causality(&y, &x, 1).unwrap();
        assert!(forward.p_value < 0.01, "p = {}", forward.p_value);
        assert!(reverse.p_value > 0.05, "p = {}", reverse.p_value);
    }

    #[test]
    fn ar_ols_recovers_coefficient() {
        let series = ar1_series(0.6, 2000, 3);
        let fit = ar_ols(&series, 1).unwrap();
        assert!(
            (fit.coeffs[0] - 0.6).abs() < 0.05,
            "phi = {}",
            fit.coeffs[0]
        );
        assert!(fit.intercept.abs() < 0.1);
        // Residual variance should be close to 1 (the innovation variance).
        assert!((fit.sigma2 - 1.0).abs() < 0.15, "sigma2 = {}", fit.sigma2);
    }

    #[test]
    fn garch_recovers_known_parameters() {
        // Simulate GARCH(1,1) with ω=1e-4·... use stable region:
        // ω = 0.05, α = 0.10, β = 0.85, unit unconditional variance.
        let (omega, alpha, beta) = (0.05, 0.10, 0.85);
        let mut rng = StdRng::seed_from_u64(99);
        let n = 4000;
        let mut rets = Vec::with_capacity(n);
        let mut sigma2: f64 = omega / (1.0 - alpha - beta);
        for _ in 0..n {
            let eps = sigma2.sqrt() * standard_normal(&mut rng);
            rets.push(eps);
            sigma2 = omega + alpha * eps * eps + beta * sigma2;
        }
        let fit = garch11_mle(&rets).unwrap();
        assert!(
            (fit.alpha - alpha).abs() < 0.10,
            "alpha = {} (true {alpha})",
            fit.alpha
        );
        assert!(
            (fit.beta - beta).abs() < 0.10,
            "beta = {} (true {beta})",
            fit.beta
        );
        assert!((fit.persistence - 0.95).abs() < 0.10);
        assert!(fit.omega > 0.0);
    }

    #[test]
    fn kalman_tracks_true_level() {
        // Local-level DGP: true level drifts slowly, observations noisy.
        let mut rng = StdRng::seed_from_u64(21);
        let n = 300;
        let mut level = vec![0.0; n];
        for i in 1..n {
            level[i] = level[i - 1] + 0.05 * standard_normal(&mut rng);
        }
        let obs: Vec<f64> = level
            .iter()
            .map(|l| l + 0.5 * standard_normal(&mut rng))
            .collect();
        let filtered = kalman_local_level(&obs, 0.05 * 0.05, 0.5 * 0.5).unwrap();
        assert_eq!(filtered.len(), n);
        // RMSE of the filtered state vs true level must beat raw obs.
        let rmse_f = (filtered
            .iter()
            .zip(&level)
            .map(|(f, l)| (f - l) * (f - l))
            .sum::<f64>()
            / n as f64)
            .sqrt();
        let rmse_o = (obs
            .iter()
            .zip(&level)
            .map(|(o, l)| (o - l) * (o - l))
            .sum::<f64>()
            / n as f64)
            .sqrt();
        assert!(rmse_f < rmse_o, "filtered {rmse_f} vs obs {rmse_o}");
        assert!(rmse_f < 0.35, "rmse_f = {rmse_f}");
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(
            adf_test(&[1.0, 2.0, 3.0], 0),
            Err(QuantError::InsufficientData { .. })
        ));
        assert!(matches!(
            granger_causality(&[1.0; 10], &[1.0; 10], 0),
            Err(QuantError::InvalidInput(_))
        ));
        assert!(matches!(
            kalman_local_level(&[1.0], -1.0, 1.0),
            Err(QuantError::InvalidInput(_))
        ));
        assert!(matches!(
            garch11_mle(&[0.01; 10]),
            Err(QuantError::InsufficientData { .. })
        ));
    }
}
