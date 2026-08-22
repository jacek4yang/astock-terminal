//! Uniform command error type.
//!
//! Every Tauri command returns `Result<T, CmdError>`; `CmdError` serializes
//! to the contract shape `{ "error": string, "kind": string }`
//! (docs/command-contract.md).

use std::fmt;

use astock_core::DataError;
use astock_minimax::MinimaxError;
use serde::Serialize;

/// Error payload returned to the UI: human-readable message plus a stable
/// machine-readable `kind` for programmatic handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CmdError {
    /// Human-readable error description.
    pub error: String,
    /// Stable category, e.g. `"network"`, `"invalid_symbol"`, `"storage"`.
    pub kind: String,
}

impl CmdError {
    /// Build an error from a kind tag and a message.
    pub fn new(kind: impl Into<String>, error: impl Into<String>) -> Self {
        CmdError {
            error: error.into(),
            kind: kind.into(),
        }
    }
}

impl fmt::Display for CmdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.error)
    }
}

impl std::error::Error for CmdError {}

impl From<DataError> for CmdError {
    fn from(e: DataError) -> Self {
        let kind = match &e {
            DataError::Network { .. } => "network",
            DataError::Timeout(_) => "timeout",
            DataError::WafBlocked(_) => "waf_blocked",
            DataError::Empty(_) => "empty",
            DataError::Parse { .. } => "parse",
            DataError::RateLimited(_) => "rate_limited",
            DataError::NoProvider(_) => "no_provider",
            DataError::InvalidSymbol(_) => "invalid_symbol",
            DataError::AllFailed { .. } => "all_failed",
        };
        CmdError::new(kind, e.to_string())
    }
}

impl From<astock_storage::Error> for CmdError {
    fn from(e: astock_storage::Error) -> Self {
        CmdError::new("storage", e.to_string())
    }
}

impl From<astock_graph::Error> for CmdError {
    fn from(e: astock_graph::Error) -> Self {
        let kind = match &e {
            astock_graph::Error::NotFound(_) => "not_found",
            astock_graph::Error::Invalid(_) => "invalid_param",
            _ => "graph",
        };
        CmdError::new(kind, e.to_string())
    }
}

impl From<MinimaxError> for CmdError {
    fn from(e: MinimaxError) -> Self {
        let kind = match &e {
            MinimaxError::Auth(_) => "auth",
            MinimaxError::KeyStore(_) => "key_store",
            MinimaxError::Network(_) => "network",
            MinimaxError::QuotaExhausted { .. } => "quota_exhausted",
            _ => "minimax",
        };
        CmdError::new(kind, e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_to_contract_shape() {
        let err = CmdError::from(DataError::InvalidSymbol("abc".into()));
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["kind"], "invalid_symbol");
        assert!(json["error"].as_str().unwrap().contains("abc"));
        // Exactly the two contract keys.
        assert_eq!(json.as_object().unwrap().len(), 2);
    }

    #[test]
    fn all_failed_kind() {
        let err = CmdError::from(DataError::AllFailed {
            op: "kline",
            details: "x".into(),
        });
        assert_eq!(err.kind, "all_failed");
    }
}
