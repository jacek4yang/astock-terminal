//! Typed model for the Token Plan quota endpoint
//! (`GET {www-host}/v1/token_plan/remains`).
//!
//! The endpoint reports, per model, a rolling ~5-hour window and a weekly
//! window with usage counters, remaining-percent values and status codes.
//! Parsing is deliberately tolerant: every field is optional and numeric
//! values are accepted as JSON numbers or strings, so a schema drift on the
//! server side degrades to "unknown" instead of a hard parse error.

use std::time::{Duration, SystemTime};

use serde::Deserialize;

/// Below this remaining-percent threshold the plan is considered throttled.
pub const THROTTLE_PERCENT: f64 = 5.0;

/// Remaining quota for one model.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelQuota {
    /// Model name, e.g. `MiniMax-M2.5`.
    #[serde(default)]
    pub model_name: String,
    /// Rolling window start, epoch milliseconds.
    #[serde(default, deserialize_with = "de_opt_i64")]
    pub start_time: Option<i64>,
    /// Rolling window end (= reset time), epoch milliseconds.
    #[serde(default, deserialize_with = "de_opt_i64")]
    pub end_time: Option<i64>,
    /// Milliseconds remaining in the rolling window.
    #[serde(default, deserialize_with = "de_opt_i64")]
    pub remains_time: Option<i64>,
    /// Total requests allowed in the current rolling window.
    #[serde(default, deserialize_with = "de_opt_i64")]
    pub current_interval_total_count: Option<i64>,
    /// Requests already used in the current rolling window.
    #[serde(default, deserialize_with = "de_opt_i64")]
    pub current_interval_usage_count: Option<i64>,
    /// Total requests allowed in the current weekly window.
    #[serde(default, deserialize_with = "de_opt_i64")]
    pub current_weekly_total_count: Option<i64>,
    /// Requests already used in the current weekly window.
    #[serde(default, deserialize_with = "de_opt_i64")]
    pub current_weekly_usage_count: Option<i64>,
    /// Weekly window start, epoch milliseconds.
    #[serde(default, deserialize_with = "de_opt_i64")]
    pub weekly_start_time: Option<i64>,
    /// Weekly window end, epoch milliseconds.
    #[serde(default, deserialize_with = "de_opt_i64")]
    pub weekly_end_time: Option<i64>,
    /// Milliseconds remaining in the weekly window.
    #[serde(default, deserialize_with = "de_opt_i64")]
    pub weekly_remains_time: Option<i64>,
    /// Service-reported status code for the rolling window.
    #[serde(default, deserialize_with = "de_opt_i64")]
    pub current_interval_status: Option<i64>,
    /// Percent of the rolling window quota still available (0-100).
    #[serde(default, deserialize_with = "de_opt_f64")]
    pub current_interval_remaining_percent: Option<f64>,
    /// Service-reported status code for the weekly window.
    #[serde(default, deserialize_with = "de_opt_i64")]
    pub current_weekly_status: Option<i64>,
    /// Percent of the weekly quota still available (0-100).
    #[serde(default, deserialize_with = "de_opt_f64")]
    pub current_weekly_remaining_percent: Option<f64>,
    /// Any fields this crate does not model yet, kept for forward compat.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl ModelQuota {
    /// True when the rolling window has no requests left.
    pub fn interval_exhausted(&self) -> bool {
        match self.current_interval_remaining_percent {
            Some(p) => p <= 0.0,
            None => matches!(
                (self.current_interval_total_count, self.current_interval_usage_count),
                (Some(total), Some(used)) if total > 0 && used >= total
            ),
        }
    }

    /// When the rolling window resets.
    pub fn interval_reset_at(&self) -> Option<SystemTime> {
        self.end_time.and_then(epoch_ms_to_system_time)
    }

    /// When the weekly window resets.
    pub fn weekly_reset_at(&self) -> Option<SystemTime> {
        self.weekly_end_time.and_then(epoch_ms_to_system_time)
    }
}

/// Recommended request pacing derived from the remaining quota.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pacing {
    /// Minimum delay to keep between requests.
    pub min_interval: Duration,
    /// Why this pacing was chosen.
    pub reason: String,
}

/// Snapshot of the Token Plan for all models.
#[derive(Debug, Clone)]
pub struct QuotaStatus {
    /// Per-model quota rows.
    pub models: Vec<ModelQuota>,
    /// When this snapshot was fetched.
    pub fetched_at: SystemTime,
}

impl QuotaStatus {
    /// Quota row for a model, by name.
    pub fn model(&self, name: &str) -> Option<&ModelQuota> {
        self.models.iter().find(|m| m.model_name == name)
    }

    /// True when the model is at or below the throttle threshold
    /// ([`THROTTLE_PERCENT`]) in its rolling window. Unknown quota is not
    /// treated as throttled.
    pub fn throttled(&self, model: &str) -> bool {
        self.model(model).is_some_and(|m| {
            m.interval_exhausted()
                || m.current_interval_remaining_percent
                    .is_some_and(|p| p <= THROTTLE_PERCENT)
        })
    }

    /// True when the model's rolling window is exhausted; callers should stop
    /// sending requests until [`QuotaStatus::window_reset_at`].
    pub fn exhausted(&self, model: &str) -> bool {
        self.model(model).is_some_and(ModelQuota::interval_exhausted)
    }

    /// When the model's rolling window resets, if reported.
    pub fn window_reset_at(&self, model: &str) -> Option<SystemTime> {
        self.model(model).and_then(ModelQuota::interval_reset_at)
    }

    /// Recommended pacing for calls to `model`.
    pub fn pacing(&self, model: &str) -> Pacing {
        let Some(m) = self.model(model) else {
            return Pacing {
                min_interval: Duration::ZERO,
                reason: "no quota information for model; no pacing".to_string(),
            };
        };
        let Some(p) = m.current_interval_remaining_percent else {
            return Pacing {
                min_interval: Duration::ZERO,
                reason: "remaining percent unknown; no pacing".to_string(),
            };
        };
        if m.interval_exhausted() {
            Pacing {
                min_interval: Duration::from_secs(60),
                reason: "rolling window exhausted; wait for reset".to_string(),
            }
        } else if p <= THROTTLE_PERCENT {
            Pacing {
                min_interval: Duration::from_secs(10),
                reason: format!("only {p:.0}% of rolling window quota left; slow down"),
            }
        } else if p <= 20.0 {
            Pacing {
                min_interval: Duration::from_secs(2),
                reason: format!("{p:.0}% of rolling window quota left; gentle pacing"),
            }
        } else {
            Pacing {
                min_interval: Duration::ZERO,
                reason: format!("{p:.0}% of rolling window quota left; no pacing needed"),
            }
        }
    }
}

/// Interpret an epoch timestamp tolerant to seconds vs milliseconds.
fn epoch_ms_to_system_time(epoch: i64) -> Option<SystemTime> {
    let millis = if epoch.abs() < 100_000_000_000 {
        epoch.saturating_mul(1000)
    } else {
        epoch
    };
    if millis < 0 {
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + Duration::from_millis(millis as u64))
}

fn de_opt_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|v| match v {
        serde_json::Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }))
}

fn de_opt_f64<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|v| match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }))
}

/// Wire shape of the `token_plan/remains` response.
#[derive(Debug, Deserialize)]
pub(crate) struct RemainsResponse {
    #[serde(default)]
    pub model_remains: Vec<ModelQuota>,
    #[serde(default)]
    pub base_resp: Option<crate::chat::BaseResp>,
}

/// Parse a `token_plan/remains` response body into a [`QuotaStatus`],
/// failing on a non-zero `base_resp.status_code`.
pub fn parse_remains(body: &[u8]) -> Result<QuotaStatus, crate::error::MinimaxError> {
    let parsed: RemainsResponse = serde_json::from_slice(body)
        .map_err(|e| crate::error::MinimaxError::Parse(format!("token_plan/remains: {e}")))?;
    if let Some(base) = &parsed.base_resp {
        if base.status_code != 0 {
            return Err(crate::http::map_base_resp(
                base.status_code,
                &base.status_msg,
            ));
        }
    }
    Ok(QuotaStatus {
        models: parsed.model_remains,
        fetched_at: SystemTime::now(),
    })
}
