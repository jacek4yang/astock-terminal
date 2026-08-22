//! Provenance metadata attached to every fetch result.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
