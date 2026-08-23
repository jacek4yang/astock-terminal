//! # astock-quant — deterministic statistics / econometrics laboratory
//!
//! Pure functions over `&[f64]` slices and `nalgebra` matrices. Every
//! public function validates its input and returns
//! `Result<_, QuantError>` — nothing panics on bad data and no NaN
//! propagates silently. Conventions and edge cases are documented per
//! function; statistical tests ship with golden values derived by hand in
//! the test comments.
//!
//! ## Module map
//! - [`returns`] — arithmetic/log returns, realized vol, EWMA vol (λ = 0.94
//!   RiskMetrics default), 252-day annualization.
//! - [`correlation`] — covariance (ddof = 1), Pearson/Spearman/Kendall-τb,
//!   rolling & exponentially-weighted correlation, partial correlation,
//!   Ledoit–Wolf shrinkage covariance, distance correlation, histogram
//!   mutual information.
//! - [`leadlag`] — lagged cross-correlation scan, block-bootstrap
//!   significance.
//! - [`timeseries`] — ADF, KPSS, Granger causality, AR(p) by OLS,
//!   GARCH(1,1) MLE, 1-D local-level Kalman filter. (ARIMA is consciously
//!   omitted: conditional-least-squares ARIMA adds little beyond the AR
//!   estimator here and a proper state-space ML ARIMA is out of scope.)
//! - [`regime`] — CUSUM change-point detection with binary segmentation,
//!   2-state Gaussian HMM (Baum–Welch with restarts).
//! - [`dimred`] — PCA on correlation or covariance, seeded k-means with
//!   k-means++ initialization.
//! - [`simulation`] — seeded GBM Monte Carlo, stationary/block bootstrap,
//!   historical/parametric/Monte-Carlo VaR & Expected Shortfall, max
//!   drawdown and drawdown duration.
//! - [`matrix`] — covariance/correlation matrices, nearest-PSD repair by
//!   eigenvalue clipping, Cholesky for correlated simulation.

pub mod correlation;
pub mod dimred;
pub mod error;
pub mod leadlag;
pub mod matrix;
pub mod regime;
pub mod research;
pub mod returns;
pub mod simulation;
pub mod timeseries;

pub(crate) mod ols;

pub use error::QuantError;
