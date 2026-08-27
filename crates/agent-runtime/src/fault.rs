//! Typed provider faults and the recovery each one warrants.
//!
//! Retry policy used to be spread across the HTTP client, the rate gate, the stream
//! reader and the runtime loop, each with its own notion of "retryable". That has two
//! failure modes: nested retries multiply into far more provider calls than any layer
//! intended, and a fault carrying real recovery information — a server `Retry-After`,
//! a quota reset time — is flattened into a boolean and the information is lost.
//!
//! This module is the single place that decides. A fault is classified once, and the
//! policy returns what to *do* rather than whether to try again.
//!
//! The distinction that matters most in practice is duration. A rate limit measured in
//! seconds is worth waiting for inside the task. One measured in minutes or hours is
//! not: the task should be persisted and resumed when the window reopens, because
//! holding a foreground task open for an hour wastes nothing but also achieves nothing,
//! and hammering the provider in the meantime is worse. A live run hit exactly this —
//! the rolling quota reset was 123 minutes away.

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::error::{ProviderError, ProviderErrorKind};

/// Longest delay worth holding a foreground task open for.
///
/// Above this the task is persisted and suspended instead. Chosen so an ordinary
/// burst-limit pause is absorbed invisibly while a quota window is not.
pub const MAX_FOREGROUND_WAIT: Duration = Duration::from_secs(45);

/// What went wrong, with the recovery information the provider gave us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelFault {
    /// Throughput limit. `retry_after` is the server's own guidance when it gave any.
    RateLimited { retry_after: Option<Duration> },
    /// The account's allowance for a window is spent.
    QuotaExhausted { reset_after: Option<Duration> },
    /// Transport failure: connection, TLS, timeout, reset.
    Network,
    /// The provider returned a turn with neither text nor a tool call.
    EmptyTurn,
    /// The stream was not decodable as the protocol requires.
    ProtocolCorruption,
    /// The model stopped because it ran out of output budget.
    TruncatedOutput,
    /// The request exceeded the model's context window.
    ContextLimit,
    /// The model or provider is temporarily unavailable.
    ModelUnavailable,
    /// The credential is missing, wrong or revoked.
    Authentication,
    /// The request itself is invalid and will fail identically on retry.
    InvalidRequest,
}

impl ModelFault {
    /// Classify a provider error.
    ///
    /// A quota error is distinguished from a rate limit because their recoveries differ
    /// in kind, not degree: a rate limit clears on its own within the task's lifetime,
    /// a quota window may not.
    pub fn classify(error: &ProviderError) -> Self {
        match error.kind {
            ProviderErrorKind::RateLimited => Self::RateLimited {
                retry_after: error.retry_after,
            },
            ProviderErrorKind::Quota => Self::QuotaExhausted {
                reset_after: error.retry_after,
            },
            ProviderErrorKind::Network => Self::Network,
            ProviderErrorKind::MalformedResponse => Self::ProtocolCorruption,
            ProviderErrorKind::Unavailable => Self::ModelUnavailable,
            ProviderErrorKind::Authentication => Self::Authentication,
        }
    }

    /// Would repeating the identical request plausibly succeed?
    ///
    /// Not the same question as "should we retry": an authentication failure is not
    /// transient, and a context-limit failure needs the request changed, not repeated.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. }
                | Self::QuotaExhausted { .. }
                | Self::Network
                | Self::EmptyTurn
                | Self::ProtocolCorruption
                | Self::ModelUnavailable
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RateLimited { .. } => "rate_limited",
            Self::QuotaExhausted { .. } => "quota_exhausted",
            Self::Network => "network",
            Self::EmptyTurn => "empty_turn",
            Self::ProtocolCorruption => "protocol_corruption",
            Self::TruncatedOutput => "truncated_output",
            Self::ContextLimit => "context_limit",
            Self::ModelUnavailable => "model_unavailable",
            Self::Authentication => "authentication",
            Self::InvalidRequest => "invalid_request",
        }
    }
}

/// What the runtime should do about a fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Re-issue the identical request now. Only for faults that commit nothing.
    RetryNow,
    /// Wait, then re-issue. Bounded by [`MAX_FOREGROUND_WAIT`].
    RetryAfter(Duration),
    /// Persist the task and resume when the window reopens.
    SuspendUntil {
        resume_at: DateTime<Utc>,
        reason: String,
    },
    /// Persist the task; the reopen time is unknown, so resumption needs a check.
    SuspendIndefinitely { reason: String },
    /// Shrink the request and re-issue.
    CompactAndRetry,
    /// Only the operator can clear this.
    RequireUserAction { reason: String },
    /// Stop. Publishing or continuing would be wrong.
    FailClosed { reason: String },
}

impl RecoveryAction {
    /// Does this action end the task rather than continue it?
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::FailClosed { .. } | Self::RequireUserAction { .. }
        )
    }

    /// Does this action persist the task for later?
    pub fn suspends(&self) -> bool {
        matches!(
            self,
            Self::SuspendUntil { .. } | Self::SuspendIndefinitely { .. }
        )
    }
}

/// One task's share of provider attempts, across every layer.
///
/// A single budget rather than one per layer, because per-layer budgets multiply: five
/// HTTP attempts inside three stream restarts inside three runtime retries is
/// forty-five provider calls where each layer believed it was allowing three.
#[derive(Debug, Clone)]
pub struct AttemptBudget {
    max_attempts: usize,
    attempts: usize,
}

impl AttemptBudget {
    pub fn new(max_attempts: usize) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            attempts: 0,
        }
    }

    pub fn attempts(&self) -> usize {
        self.attempts
    }

    pub fn remaining(&self) -> usize {
        self.max_attempts.saturating_sub(self.attempts)
    }

    /// Record one provider attempt. Returns false once the budget is spent.
    pub fn consume(&mut self) -> bool {
        self.attempts = self.attempts.saturating_add(1);
        self.attempts <= self.max_attempts
    }
}

/// Decide what to do about a fault.
///
/// `now` is injected so the policy is a pure function and its suspension timestamps are
/// testable rather than wall-clock dependent.
pub fn plan(fault: &ModelFault, budget: &AttemptBudget, now: DateTime<Utc>) -> RecoveryAction {
    // A spent budget ends the task regardless of how recoverable the fault looks. This
    // is what prevents a transient fault from becoming an unbounded loop.
    if budget.remaining() == 0 && fault.is_transient() {
        return RecoveryAction::FailClosed {
            reason: format!(
                "provider attempt budget of {} exhausted while recovering from {}",
                budget.max_attempts,
                fault.as_str()
            ),
        };
    }
    match fault {
        // Short waits are absorbed; long ones are persisted. Capping a server's long
        // Retry-After to something short and trying again is the behaviour this
        // distinction exists to prevent.
        ModelFault::RateLimited { retry_after } => match retry_after {
            Some(delay) if *delay <= MAX_FOREGROUND_WAIT => RecoveryAction::RetryAfter(*delay),
            Some(delay) => RecoveryAction::SuspendUntil {
                resume_at: now + chrono::Duration::from_std(*delay).unwrap_or_default(),
                reason: format!("provider rate limit; retry after {}s", delay.as_secs()),
            },
            // No guidance: a short bounded wait is the safe reading of a rate limit.
            None => RecoveryAction::RetryAfter(Duration::from_secs(5)),
        },
        // A quota window is an account-level allowance, not a burst limit. Waiting in
        // the foreground achieves nothing, so the task is always persisted.
        ModelFault::QuotaExhausted { reset_after } => match reset_after {
            Some(delay) => RecoveryAction::SuspendUntil {
                resume_at: now + chrono::Duration::from_std(*delay).unwrap_or_default(),
                reason: format!("provider quota exhausted; resets in {}s", delay.as_secs()),
            },
            None => RecoveryAction::SuspendIndefinitely {
                reason: "provider quota exhausted; reset time not reported".into(),
            },
        },
        ModelFault::Network | ModelFault::ModelUnavailable => {
            RecoveryAction::RetryAfter(backoff(budget.attempts()))
        }
        // These commit nothing, so replaying the identical request is safe.
        ModelFault::EmptyTurn | ModelFault::ProtocolCorruption => RecoveryAction::RetryNow,
        // The request has to change, not be repeated.
        ModelFault::ContextLimit | ModelFault::TruncatedOutput => RecoveryAction::CompactAndRetry,
        ModelFault::Authentication => RecoveryAction::RequireUserAction {
            reason: "provider rejected the credential; install a valid one".into(),
        },
        ModelFault::InvalidRequest => RecoveryAction::FailClosed {
            reason: "provider rejected the request as invalid".into(),
        },
    }
}

/// Exponential backoff, bounded by the foreground wait ceiling.
fn backoff(attempts: usize) -> Duration {
    let seconds = 1u64 << attempts.min(5);
    Duration::from_secs(seconds).min(MAX_FOREGROUND_WAIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-27T12:00:00Z")
            .expect("a fixed instant")
            .with_timezone(&Utc)
    }

    fn budget() -> AttemptBudget {
        AttemptBudget::new(4)
    }

    /// A short rate limit is waited out inside the task.
    #[test]
    fn a_short_rate_limit_is_absorbed_in_the_foreground() {
        let fault = ModelFault::RateLimited {
            retry_after: Some(Duration::from_secs(3)),
        };
        assert_eq!(
            plan(&fault, &budget(), now()),
            RecoveryAction::RetryAfter(Duration::from_secs(3))
        );
    }

    /// A long rate limit suspends rather than capping the wait and hammering.
    #[test]
    fn a_long_rate_limit_suspends_until_the_window_reopens() {
        let fault = ModelFault::RateLimited {
            retry_after: Some(Duration::from_secs(1_800)),
        };
        match plan(&fault, &budget(), now()) {
            RecoveryAction::SuspendUntil { resume_at, reason } => {
                assert_eq!(resume_at, now() + chrono::Duration::seconds(1_800));
                assert!(reason.contains("1800s"));
            }
            other => panic!("a 30-minute wait must suspend, got {other:?}"),
        }
    }

    /// A quota window always suspends, however short it claims to be.
    ///
    /// It is an account allowance rather than a burst limit, so waiting in the
    /// foreground consumes the task's lifetime without making progress.
    #[test]
    fn quota_exhaustion_always_suspends() {
        let fault = ModelFault::QuotaExhausted {
            reset_after: Some(Duration::from_secs(10)),
        };
        assert!(plan(&fault, &budget(), now()).suspends());
        let unknown = ModelFault::QuotaExhausted { reset_after: None };
        assert!(matches!(
            plan(&unknown, &budget(), now()),
            RecoveryAction::SuspendIndefinitely { .. }
        ));
    }

    /// A live quota exhaustion reported 123 minutes. It must not be waited out.
    #[test]
    fn the_observed_live_quota_window_suspends() {
        let fault = ModelFault::QuotaExhausted {
            reset_after: Some(Duration::from_secs(123 * 60)),
        };
        match plan(&fault, &budget(), now()) {
            RecoveryAction::SuspendUntil { resume_at, .. } => {
                assert_eq!(resume_at, now() + chrono::Duration::minutes(123));
            }
            other => panic!("expected suspension, got {other:?}"),
        }
    }

    /// Faults that commit nothing are replayed immediately.
    #[test]
    fn a_fault_that_commits_nothing_is_replayed() {
        for fault in [ModelFault::EmptyTurn, ModelFault::ProtocolCorruption] {
            assert_eq!(plan(&fault, &budget(), now()), RecoveryAction::RetryNow);
        }
    }

    /// A context or truncation failure changes the request rather than repeating it.
    #[test]
    fn a_size_failure_compacts_rather_than_retrying() {
        for fault in [ModelFault::ContextLimit, ModelFault::TruncatedOutput] {
            assert_eq!(
                plan(&fault, &budget(), now()),
                RecoveryAction::CompactAndRetry
            );
        }
    }

    /// A credential failure is the operator's, not the runtime's.
    #[test]
    fn an_authentication_failure_requires_the_operator() {
        assert!(matches!(
            plan(&ModelFault::Authentication, &budget(), now()),
            RecoveryAction::RequireUserAction { .. }
        ));
        assert!(plan(&ModelFault::Authentication, &budget(), now()).is_terminal());
    }

    /// An invalid request fails closed: repeating it cannot help.
    #[test]
    fn an_invalid_request_fails_closed() {
        let action = plan(&ModelFault::InvalidRequest, &budget(), now());
        assert!(action.is_terminal());
        assert!(!action.suspends());
    }

    /// Network backoff grows and stays inside the foreground ceiling.
    #[test]
    fn network_backoff_grows_and_stays_bounded() {
        let mut budget = AttemptBudget::new(12);
        let mut delays = Vec::new();
        for _ in 0..8 {
            budget.consume();
            if let RecoveryAction::RetryAfter(delay) = plan(&ModelFault::Network, &budget, now()) {
                delays.push(delay);
            }
        }
        assert!(
            delays.windows(2).all(|pair| pair[1] >= pair[0]),
            "backoff must not shrink: {delays:?}"
        );
        assert!(
            delays.iter().all(|delay| *delay <= MAX_FOREGROUND_WAIT),
            "backoff must stay inside the foreground ceiling: {delays:?}"
        );
    }

    /// One budget for every layer, so nested retries cannot multiply.
    #[test]
    fn a_spent_budget_ends_a_transient_fault() {
        let mut budget = AttemptBudget::new(2);
        assert!(budget.consume());
        assert!(budget.consume());
        assert!(!budget.consume(), "the third attempt is over budget");
        let action = plan(&ModelFault::Network, &budget, now());
        assert!(action.is_terminal(), "got {action:?}");
        match action {
            RecoveryAction::FailClosed { reason } => {
                assert!(reason.contains("attempt budget"), "{reason}");
            }
            other => panic!("expected a budget failure, got {other:?}"),
        }
    }

    /// A spent budget does not convert an operator problem into a retry problem.
    #[test]
    fn a_spent_budget_still_reports_a_credential_failure_as_such() {
        let mut budget = AttemptBudget::new(1);
        budget.consume();
        budget.consume();
        assert!(matches!(
            plan(&ModelFault::Authentication, &budget, now()),
            RecoveryAction::RequireUserAction { .. }
        ));
    }

    /// Classification preserves the recovery information the provider supplied.
    #[test]
    fn classification_preserves_provider_guidance() {
        let mut error = ProviderError::new(ProviderErrorKind::RateLimited, "slow down", true);
        error.retry_after = Some(Duration::from_secs(90));
        assert_eq!(
            ModelFault::classify(&error),
            ModelFault::RateLimited {
                retry_after: Some(Duration::from_secs(90))
            }
        );
        let quota = ProviderError::new(ProviderErrorKind::Quota, "no allowance", false);
        assert_eq!(
            ModelFault::classify(&quota),
            ModelFault::QuotaExhausted { reset_after: None }
        );
    }

    /// Every fault has a stable name, so durable events stay readable.
    #[test]
    fn every_fault_has_a_stable_name() {
        let faults = [
            ModelFault::RateLimited { retry_after: None },
            ModelFault::QuotaExhausted { reset_after: None },
            ModelFault::Network,
            ModelFault::EmptyTurn,
            ModelFault::ProtocolCorruption,
            ModelFault::TruncatedOutput,
            ModelFault::ContextLimit,
            ModelFault::ModelUnavailable,
            ModelFault::Authentication,
            ModelFault::InvalidRequest,
        ];
        let mut names: Vec<&str> = faults.iter().map(ModelFault::as_str).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "fault names must be distinct");
    }
}
