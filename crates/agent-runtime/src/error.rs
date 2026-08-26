use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    Authentication,
    RateLimited,
    Quota,
    Network,
    MalformedResponse,
    Unavailable,
}

#[derive(Debug, Clone, Error)]
#[error("{message}")]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub message: String,
    pub retryable: bool,
    pub retry_after: Option<Duration>,
}

impl ProviderError {
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            kind,
            message: message.into(),
            retryable,
            retry_after: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("invalid configuration: {0}")]
    Configuration(String),
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),
    #[error("unknown tool `{0}`; the request was rejected")]
    UnknownTool(String),
    #[error("invalid arguments for tool `{tool}`: {message}")]
    InvalidToolArguments { tool: String, message: String },
    #[error("tool `{tool}` timed out after {timeout:?}")]
    ToolTimeout { tool: String, timeout: Duration },
    #[error("tool `{tool}` failed: {message}")]
    Tool { tool: String, message: String },
    #[error("tool `{tool}` returned {actual} bytes, above the {maximum}-byte limit")]
    ToolResultTooLarge {
        tool: String,
        actual: usize,
        maximum: usize,
    },
    #[error("durable Agent store error: {0}")]
    Store(String),
    #[error("Agent task was cancelled")]
    Cancelled,
    #[error("model round limit ({0}) was reached")]
    ModelRoundLimit(usize),
    #[error("model returned neither visible text nor a tool call")]
    EmptyModelTurn,
    #[error("report publication blocked by verification: {0}")]
    VerificationFailed(String),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("internal task failed: {0}")]
    Internal(String),
}
