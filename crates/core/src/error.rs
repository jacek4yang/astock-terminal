//! Structured error type shared by all data providers.
//!
//! Every variant identifies which upstream or operation failed so callers can
//! log actionable diagnostics instead of opaque "request failed" strings.

use thiserror::Error;

/// Errors produced by market-data providers and the HTTP layer.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum DataError {
    /// Transport-level failure (DNS, connect, TLS, reset) against a host.
    #[error("network error on {host}: {message}")]
    Network {
        /// Host or URL that failed.
        host: String,
        /// Underlying error message.
        message: String,
    },

    /// Request timed out.
    #[error("timeout on {0}")]
    Timeout(String),

    /// The upstream served an HTML verification page instead of data
    /// (typical for the Tencent kline endpoint behind its WAF).
    #[error("blocked by WAF: {0}")]
    WafBlocked(String),

    /// Upstream answered but returned no usable data (e.g. empty `klines`).
    #[error("empty data from {0}")]
    Empty(String),

    /// Response body could not be parsed into the expected shape.
    #[error("parse error from {upstream}: {message}")]
    Parse {
        /// Upstream that produced the unparseable body.
        upstream: String,
        /// What went wrong while parsing.
        message: String,
    },

    /// Upstream throttled us (HTTP 429 or explicit rate-limit payload).
    #[error("rate limited by {0}")]
    RateLimited(String),

    /// No configured provider implements the requested operation.
    #[error("no provider available for operation {0}")]
    NoProvider(&'static str),

    /// Symbol failed validation (not a 6-digit numeric code, etc.).
    #[error("invalid symbol: {0}")]
    InvalidSymbol(String),

    /// Every provider in a failover chain failed; `details` lists each attempt.
    #[error("all providers failed for {op}: {details}")]
    AllFailed {
        /// Operation that was attempted, e.g. `"kline"`.
        op: &'static str,
        /// Per-provider failure summary joined with `; `.
        details: String,
    },
}
