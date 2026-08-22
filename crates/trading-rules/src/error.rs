//! Error type for the trading-rules crate.

/// Errors from loading or applying trading rules.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The rules file could not be read.
    #[error("failed to read rules file {path}: {source}")]
    Io {
        /// Path that failed to load.
        path: String,
        /// Underlying IO error.
        source: std::io::Error,
    },

    /// The rules file (embedded or override) is malformed.
    #[error("failed to parse rules: {0}")]
    Parse(#[from] serde_json::Error),

    /// A time window in the rules file is not "HH:MM".
    #[error("invalid time {value:?} in rules file: {source}")]
    InvalidTime {
        /// The offending value.
        value: String,
        /// Underlying chrono parse error.
        source: chrono::ParseError,
    },

    /// No board matches the given symbol.
    #[error("unknown symbol {0:?}: no board prefix matches")]
    UnknownSymbol(String),
}

/// Convenience result alias.
pub type Result<T> = std::result::Result<T, Error>;
