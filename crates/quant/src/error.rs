//! Error type for the quant laboratory.
//!
//! Every public function in this crate returns `Result<_, QuantError>`:
//! invalid input (non-finite values, wrong lengths, bad parameters) and
//! numerical failures (singular systems, non-convergence) are reported
//! explicitly instead of panicking or propagating `NaN`.

use thiserror::Error;

/// Unified error type for all quantitative routines.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum QuantError {
    /// The input slice is too short for the requested statistic.
    #[error("insufficient data for {context}: need at least {needed} observations, got {got}")]
    InsufficientData {
        /// What was being computed.
        context: &'static str,
        /// Minimum required length.
        needed: usize,
        /// Actual length supplied.
        got: usize,
    },

    /// Input violated a documented precondition (non-finite values,
    /// mismatched lengths, out-of-range parameters, ...).
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// A numerical procedure failed (singular matrix, non-convergent
    /// optimizer, degenerate variance, ...).
    #[error("numerical issue: {0}")]
    NumericalIssue(String),
}

/// Validate that a slice contains only finite values and has at least
/// `needed` elements. Shared by virtually every routine in the crate.
pub(crate) fn validate_series(x: &[f64], needed: usize, context: &'static str) -> Result<(), QuantError> {
    if x.len() < needed {
        return Err(QuantError::InsufficientData {
            context,
            needed,
            got: x.len(),
        });
    }
    if x.iter().any(|v| !v.is_finite()) {
        return Err(QuantError::InvalidInput(format!(
            "{context}: input contains NaN or infinite values"
        )));
    }
    Ok(())
}

/// Validate that two series have equal length and satisfy `validate_series`.
pub(crate) fn validate_pair(
    x: &[f64],
    y: &[f64],
    needed: usize,
    context: &'static str,
) -> Result<(), QuantError> {
    validate_series(x, needed, context)?;
    validate_series(y, needed, context)?;
    if x.len() != y.len() {
        return Err(QuantError::InvalidInput(format!(
            "{context}: series length mismatch ({} vs {})",
            x.len(),
            y.len()
        )));
    }
    Ok(())
}
