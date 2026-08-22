//! Error type for the graph crate.

/// Errors from the graph layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Storage failure.
    #[error("storage error: {0}")]
    Storage(#[from] astock_storage::Error),

    /// Market-data failure (industry enrichment).
    #[error("market data error: {0}")]
    Data(#[from] astock_core::DataError),

    /// JSON (de)serialization failure.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// A value failed an invariant check (bad confidence, unknown kind...).
    #[error("invalid graph data: {0}")]
    Invalid(String),

    /// A referenced node does not exist.
    #[error("node not found: {0}")]
    NotFound(String),
}

/// Convenience result alias.
pub type Result<T> = std::result::Result<T, Error>;
