//! Typed error hierarchy for the MiniMax provider.

use std::time::{Duration, SystemTime};

use thiserror::Error;

/// Errors produced by the MiniMax provider.
///
/// The variants are designed so the caller can drive scheduling decisions:
/// [`MinimaxError::RateLimited`] and [`MinimaxError::Network`] are transient
/// and retried by [`crate::RateGate`]; [`MinimaxError::QuotaExhausted`] carries
/// the window reset time so the app can pause and resume later.
#[derive(Debug, Error)]
pub enum MinimaxError {
    /// The API key was rejected (HTTP 401/403 or `base_resp.status_code == 2049`).
    #[error("authentication failed: {0}")]
    Auth(String),

    /// The service asked us to slow down (HTTP 429 or a rate-limit `base_resp` code).
    #[error("rate limited{}", retry_after.map(|d| format!("; retry after {d:?}")).unwrap_or_default())]
    RateLimited {
        /// Server-provided retry hint (`Retry-After` header), if any.
        retry_after: Option<Duration>,
    },

    /// The Token Plan reports zero remaining quota in the current window.
    ///
    /// Raised *before* burning a request when the quota guard is enabled.
    #[error("token plan quota exhausted{}", window_reset_at.map(|t| format!("; window resets at {t:?}")).unwrap_or_default())]
    QuotaExhausted {
        /// When the current quota window resets, if known.
        window_reset_at: Option<SystemTime>,
    },

    /// Transport-level failure (connect, TLS, timeout, HTTP 5xx).
    #[error("network error: {0}")]
    Network(String),

    /// The service returned a non-success `base_resp` or HTTP status.
    #[error("api error {code}: {msg}")]
    Api {
        /// `base_resp.status_code`, or the HTTP status when no body code exists.
        code: i64,
        /// Human-readable message from the service.
        msg: String,
    },

    /// A response body could not be parsed into the expected shape.
    #[error("parse error: {0}")]
    Parse(String),

    /// The OS keyring operation failed.
    #[error("key store error: {0}")]
    KeyStore(String),
}

impl MinimaxError {
    /// Whether [`crate::RateGate`] should retry the operation after a backoff.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            MinimaxError::RateLimited { .. } | MinimaxError::Network(_)
        )
    }
}
