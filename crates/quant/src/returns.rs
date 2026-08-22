//! Return calculations and volatility estimators.
//!
//! Conventions used throughout this module:
//! - Prices must be strictly positive (required for log returns; enforced for
//!   arithmetic returns too since A-share prices are never non-positive).
//! - Volatility figures are *per period* unless a function name says
//!   `annualized`; annualization multiplies per-period volatility by
//!   `sqrt(periods_per_year)` with the A-share convention of 252 trading days.

use crate::error::{validate_series, QuantError};

/// Trading days per year used for annualization (Chinese A-share convention).
pub const TRADING_DAYS_PER_YEAR: f64 = 252.0;

/// Default RiskMetrics (JP Morgan, 1996) decay factor for EWMA volatility
/// on daily data.
pub const RISKMETRICS_LAMBDA: f64 = 0.94;

/// Arithmetic (simple) returns `r_t = P_t / P_{t-1} - 1`.
///
/// Output length is `prices.len() - 1`. Requires at least 2 prices, all
/// finite and strictly positive.
pub fn arithmetic_returns(prices: &[f64]) -> Result<Vec<f64>, QuantError> {
    validate_series(prices, 2, "arithmetic_returns")?;
    if prices.iter().any(|p| *p <= 0.0) {
        return Err(QuantError::InvalidInput(
            "arithmetic_returns: prices must be strictly positive".into(),
        ));
    }
    Ok(prices.windows(2).map(|w| w[1] / w[0] - 1.0).collect())
}

/// Log returns `r_t = ln(P_t / P_{t-1})`.
///
/// Output length is `prices.len() - 1`. Log returns are time-additive:
/// `sum(r) = ln(P_n / P_0)`. Requires at least 2 strictly positive prices.
pub fn log_returns(prices: &[f64]) -> Result<Vec<f64>, QuantError> {
    validate_series(prices, 2, "log_returns")?;
    if prices.iter().any(|p| *p <= 0.0) {
        return Err(QuantError::InvalidInput(
            "log_returns: prices must be strictly positive".into(),
        ));
    }
    Ok(prices.windows(2).map(|w| (w[1] / w[0]).ln()).collect())
}

/// Realized (historical) volatility: the sample standard deviation of a
/// return series, `sqrt( Σ (r_i - r̄)² / (n - 1) )` (Bessel-corrected,
/// ddof = 1). Per period, not annualized.
pub fn realized_vol(returns: &[f64]) -> Result<f64, QuantError> {
    Ok(crate::correlation::variance(returns, "realized_vol")?.sqrt())
}

/// EWMA volatility (RiskMetrics).
///
/// Recursion: `σ²_t = λ σ²_{t-1} + (1 - λ) r²_{t-1}`, seeded with
/// `σ²_1 = r_1²` (the standard RiskMetrics initialization; the first
/// squared return is the best single-observation variance guess).
///
/// `lambda` defaults to [`RISKMETRICS_LAMBDA`] (0.94, daily data) when
/// `None` is passed. Must lie in the open interval (0, 1).
///
/// Returns the *last* conditional volatility σ_n (per period). With n
/// returns the recursion performs n - 1 updates after seeding.
pub fn ewma_vol(returns: &[f64], lambda: Option<f64>) -> Result<f64, QuantError> {
    validate_series(returns, 1, "ewma_vol")?;
    let lambda = lambda.unwrap_or(RISKMETRICS_LAMBDA);
    if !(0.0..1.0).contains(&lambda) {
        return Err(QuantError::InvalidInput(format!(
            "ewma_vol: lambda must be in (0, 1), got {lambda}"
        )));
    }
    let mut var = returns[0] * returns[0];
    for r in &returns[1..] {
        var = lambda * var + (1.0 - lambda) * r * r;
    }
    Ok(var.sqrt())
}

/// Annualize a per-period volatility: `σ_annual = σ_period * sqrt(periods_per_year)`.
///
/// Pass [`TRADING_DAYS_PER_YEAR`] for daily A-share data. The sqrt-of-time
/// rule assumes i.i.d. returns (no autocorrelation).
pub fn annualize_vol(vol_per_period: f64, periods_per_year: f64) -> Result<f64, QuantError> {
    if !vol_per_period.is_finite() || vol_per_period < 0.0 {
        return Err(QuantError::InvalidInput(format!(
            "annualize_vol: volatility must be finite and non-negative, got {vol_per_period}"
        )));
    }
    if !periods_per_year.is_finite() || periods_per_year <= 0.0 {
        return Err(QuantError::InvalidInput(format!(
            "annualize_vol: periods_per_year must be positive, got {periods_per_year}"
        )));
    }
    Ok(vol_per_period * periods_per_year.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_returns_golden() {
        // Hand-computed: 100 -> 110 -> 99.
        // r1 = 110/100 - 1 = 0.1 ; r2 = 99/110 - 1 = -0.1
        let r = arithmetic_returns(&[100.0, 110.0, 99.0]).unwrap();
        assert_eq!(r.len(), 2);
        assert!((r[0] - 0.1).abs() < 1e-15);
        assert!((r[1] - (-0.1)).abs() < 1e-12);
    }

    #[test]
    fn log_returns_golden() {
        // ln(110/100) = ln 1.1 ≈ 0.0953101798043; ln(99/110) = ln 0.9 ≈ -0.1053605156578
        let r = log_returns(&[100.0, 110.0, 99.0]).unwrap();
        assert!((r[0] - 1.1_f64.ln()).abs() < 1e-15);
        assert!((r[1] - 0.9_f64.ln()).abs() < 1e-15);
        // Time additivity: sum = ln(99/100).
        assert!((r.iter().sum::<f64>() - 0.99_f64.ln()).abs() < 1e-15);
    }

    #[test]
    fn realized_vol_golden() {
        // returns [0.1, -0.1]: mean 0, sample variance = (0.01 + 0.01)/(2-1) = 0.02
        // vol = sqrt(0.02) ≈ 0.141421356
        let v = realized_vol(&[0.1, -0.1]).unwrap();
        assert!((v - 0.02_f64.sqrt()).abs() < 1e-15);
    }

    #[test]
    fn ewma_vol_golden() {
        // returns [0.1, -0.2], lambda 0.94.
        // σ²_1 = 0.01; σ²_2 = 0.94*0.01 + 0.06*0.04 = 0.0094 + 0.0024 = 0.0118
        let v = ewma_vol(&[0.1, -0.2], None).unwrap();
        assert!((v - 0.0118_f64.sqrt()).abs() < 1e-12);
        // Single observation: vol = |r|.
        let v1 = ewma_vol(&[-0.05], None).unwrap();
        assert!((v1 - 0.05).abs() < 1e-15);
    }

    #[test]
    fn annualization_convention() {
        // 1% daily vol -> 1% * sqrt(252) ≈ 15.87% annual.
        let a = annualize_vol(0.01, TRADING_DAYS_PER_YEAR).unwrap();
        assert!((a - 0.01 * 252.0_f64.sqrt()).abs() < 1e-15);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(
            arithmetic_returns(&[1.0]),
            Err(QuantError::InsufficientData { .. })
        ));
        assert!(matches!(
            log_returns(&[1.0, 0.0]),
            Err(QuantError::InvalidInput(_))
        ));
        assert!(matches!(
            log_returns(&[1.0, f64::NAN]),
            Err(QuantError::InvalidInput(_))
        ));
        assert!(matches!(
            ewma_vol(&[0.01], Some(1.5)),
            Err(QuantError::InvalidInput(_))
        ));
        assert!(matches!(
            annualize_vol(-0.1, 252.0),
            Err(QuantError::InvalidInput(_))
        ));
    }
}
