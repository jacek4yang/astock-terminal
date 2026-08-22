//! Quota-aware scheduling: exponential backoff with jitter around chat calls.
//!
//! [`RateGate`] retries transient failures ([`MinimaxError::RateLimited`],
//! [`MinimaxError::Network`]) with full-jitter exponential backoff and gives up
//! after a configurable number of attempts. Hard failures (auth, quota
//! exhaustion, API errors) propagate immediately.

use std::future::Future;
use std::time::Duration;

use rand::Rng;

use crate::error::MinimaxError;

/// Backoff policy for [`RateGate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateGateConfig {
    /// Total attempts (including the first) before giving up.
    pub max_attempts: u32,
    /// Base delay doubled after each failed attempt.
    pub base_delay: Duration,
    /// Upper bound for any single delay, including server hints.
    pub max_delay: Duration,
}

impl Default for RateGateConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
        }
    }
}

/// Wraps fallible async operations with retry/backoff for transient errors.
#[derive(Debug, Default)]
pub struct RateGate {
    config: RateGateConfig,
}

impl RateGate {
    /// A gate with the given policy.
    pub fn new(config: RateGateConfig) -> Self {
        Self { config }
    }

    /// The active policy.
    pub fn config(&self) -> RateGateConfig {
        self.config
    }

    /// Run `op`, retrying transient failures with backoff.
    ///
    /// Delay for attempt `n` is a uniform sample in `0..=base * 2^(n-1)`,
    /// capped at `max_delay` (full jitter). When the server provided a
    /// `Retry-After` hint it takes precedence, still capped at `max_delay`.
    pub async fn run<T, F, Fut>(&self, mut op: F) -> Result<T, MinimaxError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, MinimaxError>>,
    {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match op().await {
                Ok(value) => return Ok(value),
                Err(e) => {
                    let delay = self.retry_delay(&e, attempt);
                    match delay {
                        Some(delay) if attempt < self.config.max_attempts => {
                            tracing::warn!(
                                attempt,
                                ?delay,
                                error = %e,
                                "transient MiniMax error; backing off"
                            );
                            tokio::time::sleep(delay).await;
                        }
                        _ => return Err(e),
                    }
                }
            }
        }
    }

    fn retry_delay(&self, e: &MinimaxError, attempt: u32) -> Option<Duration> {
        match e {
            MinimaxError::RateLimited {
                retry_after: Some(hint),
            } => Some((*hint).min(self.config.max_delay)),
            MinimaxError::RateLimited { .. } | MinimaxError::Network(_) => {
                let shift = (attempt - 1).min(20);
                let backoff = self
                    .config
                    .base_delay
                    .saturating_mul(1u32 << shift)
                    .min(self.config.max_delay);
                let millis = backoff.as_millis().min(u64::MAX as u128) as u64;
                let sampled = if millis == 0 {
                    0
                } else {
                    rand::rng().random_range(0..=millis)
                };
                Some(Duration::from_millis(sampled))
            }
            _ => None,
        }
    }
}
