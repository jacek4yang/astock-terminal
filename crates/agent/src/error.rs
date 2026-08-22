//! Error type for the agent crate.

use thiserror::Error;

/// Errors produced by the agent layer.
#[derive(Debug, Error)]
pub enum AgentError {
    /// A tool failed during execution.
    #[error("tool `{tool}` failed: {msg}")]
    Tool {
        /// Tool name.
        tool: String,
        /// Failure detail.
        msg: String,
    },

    /// The model asked for a tool that is not registered.
    #[error("unknown tool: {0}")]
    UnknownTool(String),

    /// Tool arguments failed schema deserialization.
    #[error("invalid arguments for `{tool}`: {msg}")]
    InvalidArgs {
        /// Tool name.
        tool: String,
        /// Failure detail.
        msg: String,
    },

    /// Market-data layer failure.
    #[error("data error: {0}")]
    Data(#[from] astock_core::DataError),

    /// MiniMax provider failure.
    #[error("minimax error: {0}")]
    Minimax(#[from] astock_minimax::MinimaxError),

    /// Storage layer failure.
    #[error("storage error: {0}")]
    Storage(#[from] astock_storage::Error),

    /// JSON (de)serialization failure.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// `resume_task`/`cancel_task` referenced an unknown task id.
    #[error("task not found: {0}")]
    TaskNotFound(String),

    /// The task exists but its status does not allow resuming.
    #[error("task `{0}` is not resumable (status: {1})")]
    NotResumable(String, String),

    /// The task was cancelled while running.
    #[error("task cancelled: {0}")]
    Cancelled(String),
}

/// Convenience result alias.
pub type Result<T> = std::result::Result<T, AgentError>;
