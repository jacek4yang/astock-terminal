//! Typed errors for the iwencai (问财) provider.

/// Errors surfaced by the iwencai client.
#[derive(Debug, thiserror::Error)]
pub enum WencaiError {
    /// HTTP transport failure.
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    /// hexin-v signing inside the embedded QuickJS engine failed.
    #[error("hexin-v signing failed: {0}")]
    Js(String),

    /// iwencai answered with a captcha challenge and either the `captcha`
    /// feature is disabled or the challenge is not the known slider type.
    #[error("iwencai requires captcha verification: {captcha_url}")]
    NeedCaptcha {
        /// URL of the verification page returned by the server.
        captcha_url: String,
    },

    /// The slider captcha could not be solved within the attempt budget.
    #[error("slider captcha solving failed after {attempts} attempt(s): {last_reason}")]
    CaptchaFailed {
        /// How many solve attempts were made.
        attempts: u32,
        /// Why the last attempt failed.
        last_reason: String,
    },

    /// The server is rate-limiting us.
    #[error("rate limited by iwencai (HTTP {status})")]
    RateLimited {
        /// HTTP status code returned by the server.
        status: u16,
    },

    /// The response body did not match the expected schema.
    #[error("failed to parse iwencai response: {0}")]
    Parse(String),
}
