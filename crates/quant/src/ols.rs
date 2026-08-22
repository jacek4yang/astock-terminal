//! Internal ordinary-least-squares helper shared by partial correlation,
//! ADF, Granger causality and AR(p) estimation. Not part of the public API.

use nalgebra::DMatrix;

use crate::error::QuantError;

/// Result of an OLS fit `y = X β + ε`.
pub(crate) struct OlsFit {
    /// Estimated coefficients, one per column of the design matrix.
    pub coeffs: Vec<f64>,
    /// Residuals `y - X β̂`.
    pub residuals: Vec<f64>,
    /// Residual sum of squares.
    pub rss: f64,
    /// Residual variance estimate `RSS / (n - k)`.
    pub sigma2: f64,
    /// Diagonal of `σ² (XᵀX)⁻¹` — sampling variances of the coefficients.
    pub coeff_var: Vec<f64>,
}

/// Fit OLS given a *full* design matrix `x` (n × k, include an intercept
/// column yourself when needed). Errors on rank-deficient designs instead
/// of silently using a pseudoinverse, since all callers need valid
/// standard errors.
pub(crate) fn ols(y: &[f64], x: &DMatrix<f64>) -> Result<OlsFit, QuantError> {
    let n = y.len();
    let k = x.ncols();
    if x.nrows() != n {
        return Err(QuantError::InvalidInput(format!(
            "ols: design matrix has {} rows but y has {n} elements",
            x.nrows()
        )));
    }
    if n <= k {
        return Err(QuantError::InsufficientData {
            context: "ols",
            needed: k + 1,
            got: n,
        });
    }
    let xmat = x;
    let xt = xmat.transpose();
    let xtx = &xt * xmat;
    let xtx_inv = xtx
        .try_inverse()
        .ok_or_else(|| QuantError::NumericalIssue("ols: XᵀX is singular (collinear regressors)".into()))?;
    let xty = &xt * DMatrix::from_column_slice(n, 1, y);
    let beta = &xtx_inv * xty;
    let fitted = xmat * &beta;
    let ymat = DMatrix::from_column_slice(n, 1, y);
    let resid = &ymat - &fitted;
    let rss: f64 = resid.iter().map(|e| e * e).sum();
    let sigma2 = rss / (n - k) as f64;
    let coeffs: Vec<f64> = beta.iter().copied().collect();
    let residuals: Vec<f64> = resid.iter().copied().collect();
    let coeff_var: Vec<f64> = (0..k).map(|i| sigma2 * xtx_inv[(i, i)]).collect();
    Ok(OlsFit {
        coeffs,
        residuals,
        rss,
        sigma2,
        coeff_var,
    })
}
