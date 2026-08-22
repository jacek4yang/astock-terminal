//! Provenance metadata attached to every fetch result.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Quality classification for one field in a composite payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataQuality {
    /// Directly reported by the named upstream.
    Reported,
    /// Filled from canonical security metadata.
    Reference,
    /// Derived deterministically from other reported fields.
    Derived,
    /// Not available; the value must stay `None` in the public contract.
    Missing,
}

/// Field-level lineage for composite market-data responses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldProvenance {
    /// Provider or reference dataset responsible for the field.
    pub source: String,
    /// Market time represented by the field, when known.
    pub as_of: Option<DateTime<Utc>>,
    /// Time at which the application fetched or resolved it.
    pub fetched_at: DateTime<Utc>,
    /// Whether the provider considers the value stale.
    pub stale: bool,
    /// Direct, reference, derived, or missing.
    pub quality: DataQuality,
    /// Human-readable reason when the field is unavailable.
    pub missing_reason: Option<String>,
}

impl FieldProvenance {
    /// Build provenance for a reported field.
    pub fn reported(source: impl Into<String>, at: DateTime<Utc>) -> Self {
        Self {
            source: source.into(),
            as_of: Some(at),
            fetched_at: at,
            stale: false,
            quality: DataQuality::Reported,
            missing_reason: None,
        }
    }

    /// Build provenance for canonical reference data.
    pub fn reference(source: impl Into<String>, fetched_at: DateTime<Utc>) -> Self {
        Self {
            source: source.into(),
            as_of: None,
            fetched_at,
            stale: false,
            quality: DataQuality::Reference,
            missing_reason: None,
        }
    }

    /// Mark a field as unavailable without inventing a numeric zero.
    pub fn missing(source: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            as_of: None,
            fetched_at: crate::time::utc_now(),
            stale: false,
            quality: DataQuality::Missing,
            missing_reason: Some(reason.into()),
        }
    }
}

/// Upstream that produced a piece of data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Source {
    /// Tencent `web.ifzq.gtimg.cn` kline API.
    Tencent,
    /// Sina `money.finance.sina.com.cn` kline API.
    Sina,
    /// EastMoney `push2*` APIs (the concrete host is logged by the HTTP layer).
    EastMoney,
    /// Tushare pro `api.tushare.pro` (optional token-gated provider).
    Tushare,
    /// TDX (通达信) quote TCP protocol servers (unadjusted data only).
    Tdx,
    /// JoinQuant (聚宽) research-environment channel (optional credential-gated).
    JoinQuant,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Tencent => write!(f, "tencent"),
            Source::Sina => write!(f, "sina"),
            Source::EastMoney => write!(f, "eastmoney"),
            Source::Tushare => write!(f, "tushare"),
            Source::Tdx => write!(f, "tdx"),
            Source::JoinQuant => write!(f, "joinquant"),
        }
    }
}

/// A fetched payload plus its provenance: which upstream answered and when.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fetched<T> {
    /// The payload itself.
    pub data: T,
    /// Upstream that produced it.
    pub source: Source,
    /// Fetch time (UTC). Cached entries keep the original fetch time.
    pub fetched_at: DateTime<Utc>,
}

impl<T> Fetched<T> {
    /// Stamp a payload as fetched from `source` right now.
    pub fn now(data: T, source: Source) -> Self {
        Fetched {
            data,
            source,
            fetched_at: crate::time::utc_now(),
        }
    }

    /// Map the payload while preserving provenance.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Fetched<U> {
        Fetched {
            data: f(self.data),
            source: self.source,
            fetched_at: self.fetched_at,
        }
    }
}
