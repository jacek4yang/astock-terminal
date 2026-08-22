//! 错误类型。

use thiserror::Error;

pub type Result<T> = std::result::Result<T, TdxError>;

#[derive(Debug, Error)]
pub enum TdxError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("request timed out")]
    Timeout,

    #[error("connection closed by server")]
    Disconnected,

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("no usable tdx server available")]
    NoServerAvailable,
}

impl From<tokio::time::error::Elapsed> for TdxError {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        TdxError::Timeout
    }
}
