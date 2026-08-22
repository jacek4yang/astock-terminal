//! Error type for the JoinQuant channel.

/// Errors returned by the JoinQuant research-environment channel.
#[derive(Debug, thiserror::Error)]
pub enum JoinQuantError {
    /// HTTP transport error.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    /// WebSocket transport/handshake error.
    #[error("websocket error: {0}")]
    Ws(Box<tokio_tungstenite::tungstenite::Error>),

    /// JSON (de)serialization error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// Captcha image decoding failed.
    #[error("image decode error: {0}")]
    Image(#[from] image::ImageError),

    /// Base64 decoding failed (captcha data-URI or kernel output payload).
    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    /// Slider-captcha solving failed (non-retryable variant).
    #[error("captcha solving error: {0}")]
    Captcha(#[from] astock_captcha::CaptchaError),

    /// Login rejected by the server.
    #[error("login failed (code {code}): {message}")]
    LoginFailed {
        /// Server error code (`"20000"`, `"10000"`, …).
        code: String,
        /// Server-provided message.
        message: String,
    },

    /// Slider captcha was not passed within the bounded retry budget.
    #[error("slider captcha not passed after {attempts} attempts")]
    CaptchaExhausted {
        /// Number of attempts made.
        attempts: usize,
    },

    /// The research bridge page did not contain the expected fields.
    #[error("bridge page parse failed: {0}")]
    BridgeParse(String),

    /// The single-user notebook server did not become ready in time.
    #[error("research server spawn timed out after {0}s")]
    SpawnTimeout(u64),

    /// The remote kernel reported an execution error.
    #[error("kernel error {ename}: {evalue}")]
    Kernel {
        /// Exception class name.
        ename: String,
        /// Exception message.
        evalue: String,
        /// Python traceback lines.
        traceback: Vec<String>,
    },

    /// Protocol violation / unexpected server response.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// The kernel stdout did not contain a `JQJSON:` payload line.
    #[error("JQJSON output line not found in kernel stdout")]
    OutputMissing,

    /// Invalid input (e.g. malformed security code or date).
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

impl From<tokio_tungstenite::tungstenite::Error> for JoinQuantError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        JoinQuantError::Ws(Box::new(e))
    }
}
