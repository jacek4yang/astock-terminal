//! Matrix helpers: covariance/correlation matrices from series, nearest
//! positive-semidefinite repair, and Cholesky factorization for simulation.

use nalgebra::{DMatrix, SymmetricEigen};

use crate::error::{validate_series, QuantError};

/// Validate a collection of variable series (each column of the data
/// matrix given as one slice). All series must be finite, equal length,
/// and have at least `needed` observations.
pub(crate) fn validate_multi(
    series: &[&[f64]],
    needed: usize,
    context: &'static str,
) -> Result<(), QuantError> {
    if series.is_empty() {
        return Err(QuantError::InvalidInput(format!(
            "{context}: at least one series is required"
        )));
    }
    validate_series(series[0], needed, context)?;
    let n = series[0].len();
    for s in &series[1..] {
        validate_series(s, needed, context)?;
        if s.len() != n {
            return Err(QuantError::InvalidInput(format!(
                "{context}: series length mismatch ({} vs {n})",
                s.len()
            )));
        }
    }
    Ok(())
}

/// Sample covariance matrix (ddof = 1) of `p` variable series, each of
/// length `n`. Entry (i, j) is `cov(series_i, series_j)`.
pub fn covariance_matrix(series: &[&[f64]]) -> Result<DMatrix<f64>, QuantError> {
    validate_multi(series, 2, "covariance_matrix")?;
    let p = series.len();
    let mut cov = DMatrix::zeros(p, p);
    for i in 0..p {
        for j in i..p {
            let c = crate::correlation::covariance(series[i], series[j], "covariance_matrix")?;
            cov[(i, j)] = c;
            cov[(j, i)] = c;
        }
    }
    Ok(cov)
}

/// Correlation matrix of `p` variable series. Entry (i, j) is the Pearson
/// correlation; the diagonal is exactly 1.
pub fn correlation_matrix(series: &[&[f64]]) -> Result<DMatrix<f64>, QuantError> {
    validate_multi(series, 2, "correlation_matrix")?;
    let p = series.len();
    let mut cor = DMatrix::identity(p, p);
    for i in 0..p {
        for j in (i + 1)..p {
            let r = crate::correlation::pearson(series[i], series[j])?;
            cor[(i, j)] = r;
            cor[(j, i)] = r;
        }
    }
    Ok(cor)
}

/// Nearest correlation/covariance matrix repair via *eigenvalue clipping*.
///
/// The matrix is symmetrized (`(M + Mᵀ)/2`), eigendecomposed, all
/// eigenvalues below `floor` are replaced by `floor`, and the matrix is
/// rebuilt as `Q Λ⁺ Qᵀ`. This is the simplest PSD projection — it is not
/// the Frobenius-norm nearest correlation matrix of Higham (2002), but it
/// guarantees symmetry and positive semidefiniteness and preserves the
/// eigenvectors. Documented edge case: an already-PSD input is returned
/// numerically unchanged (up to floating-point round-trip error).
pub fn nearest_psd(m: &DMatrix<f64>, floor: f64) -> Result<DMatrix<f64>, QuantError> {
    if m.nrows() != m.ncols() {
        return Err(QuantError::InvalidInput(
            "nearest_psd: matrix must be square".into(),
        ));
    }
    if !floor.is_finite() || floor < 0.0 {
        return Err(QuantError::InvalidInput(format!(
            "nearest_psd: eigenvalue floor must be finite and >= 0, got {floor}"
        )));
    }
    if m.iter().any(|v| !v.is_finite()) {
        return Err(QuantError::InvalidInput(
            "nearest_psd: matrix contains NaN or infinite entries".into(),
        ));
    }
    let n = m.nrows();
    let sym = (m + m.transpose()) * 0.5;
    let eig = SymmetricEigen::new(sym);
    let q = eig.eigenvectors;
    let mut lam = eig.eigenvalues;
    for v in lam.iter_mut() {
        if *v < floor {
            *v = floor;
        }
    }
    let d = DMatrix::from_diagonal(&lam);
    let rebuilt = &q * d * q.transpose();
    // Re-symmetrize to kill floating-point asymmetry from the round trip.
    let out = (&rebuilt + rebuilt.transpose()) * 0.5;
    debug_assert_eq!(out.nrows(), n);
    Ok(out)
}

/// Lower-triangular Cholesky factor `L` with `M = L Lᵀ`, intended for
/// correlated simulation (`Z ~ N(0, I)` -> `L Z ~ N(0, M)`).
///
/// The input is first repaired with [`nearest_psd`] (floor = 0), so
/// indefinite input from floating-point noise or inconsistent estimates is
/// tolerated. A genuinely zero-variance variable yields a zero row in `L`.
pub fn cholesky_psd(m: &DMatrix<f64>) -> Result<DMatrix<f64>, QuantError> {
    let cleaned = nearest_psd(m, 0.0)?;
    nalgebra::linalg::Cholesky::new(cleaned)
        .map(|c| c.l())
        .ok_or_else(|| QuantError::NumericalIssue("cholesky_psd: factorization failed".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covariance_matrix_golden() {
        // x = [1, 2, 3], y = [1, 3, 2]
        // x̄ = 2, ȳ = 2; cov(x,y) = ((-1)(-1) + 0*1 + 1*0)/2 = 0.5
        // var(x) = (1+0+1)/2 = 1; var(y) = (1+1+0)/2 = 1
        let m = covariance_matrix(&[&[1.0, 2.0, 3.0], &[1.0, 3.0, 2.0]]).unwrap();
        assert!((m[(0, 0)] - 1.0).abs() < 1e-12);
        assert!((m[(1, 1)] - 1.0).abs() < 1e-12);
        assert!((m[(0, 1)] - 0.5).abs() < 1e-12);
        assert!((m[(0, 1)] - m[(1, 0)]).abs() < 1e-15);
    }

    #[test]
    fn correlation_matrix_diagonal_is_one() {
        let m = correlation_matrix(&[&[1.0, 2.0, 3.0], &[1.0, 3.0, 2.0]]).unwrap();
        assert!((m[(0, 0)] - 1.0).abs() < 1e-15);
        assert!((m[(0, 1)] - 0.5).abs() < 1e-12);
    }

    #[test]
    fn nearest_psd_clips_negative_eigenvalues() {
        // Construct an indefinite symmetric matrix: swap-based with negative eigenvalue.
        let m = DMatrix::from_row_slice(2, 2, &[1.0, 2.0, 2.0, 1.0]); // eigenvalues 3, -1
        let fixed = nearest_psd(&m, 0.0).unwrap();
        let eig = SymmetricEigen::new(fixed.clone());
        for v in eig.eigenvalues.iter() {
            assert!(*v >= -1e-12);
        }
        // Eigenvector structure preserved: clipped matrix should be
        // Q diag(3, 0) Qᵀ = [[1.5, 1.5], [1.5, 1.5]].
        assert!((fixed[(0, 0)] - 1.5).abs() < 1e-10);
        assert!((fixed[(0, 1)] - 1.5).abs() < 1e-10);
    }

    #[test]
    fn cholesky_recovers_matrix() {
        let m = covariance_matrix(&[&[1.0, 2.0, 4.0, 3.0], &[2.0, 1.0, 3.0, 5.0]]).unwrap();
        let l = cholesky_psd(&m).unwrap();
        let rebuilt = &l * l.transpose();
        for i in 0..2 {
            for j in 0..2 {
                assert!((rebuilt[(i, j)] - m[(i, j)]).abs() < 1e-10);
            }
        }
    }
}
