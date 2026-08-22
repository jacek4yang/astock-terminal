//! Error type for the storage crate.

/// Errors from the storage layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// SQLite failure.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Parquet/Arrow failure.
    #[error("parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),

    /// Arrow failure.
    #[error("arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    /// Filesystem failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization failure.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// The blocking database worker thread is gone.
    #[error("storage worker thread closed")]
    WorkerClosed,

    /// A value failed an invariant check.
    #[error("invalid data: {0}")]
    Invalid(String),
}

/// Convenience result alias.
pub type Result<T> = std::result::Result<T, Error>;
