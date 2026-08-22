//! Error types for the captcha toolkit.

/// Errors returned by gap detection, trajectory generation and OCR glue.
#[derive(Debug, thiserror::Error)]
pub enum CaptchaError {
    /// The best candidate scored below the configured confidence threshold.
    ///
    /// Returned instead of a guess: a wrong drag distance is more expensive
    /// for the caller (a failed attempt) than an explicit error that can
    /// trigger a captcha refresh and a bounded retry.
    #[error("gap detection confidence {confidence:.3} below threshold {threshold:.3}")]
    LowConfidence {
        /// Confidence of the best candidate found.
        confidence: f32,
        /// Configured threshold that was not met.
        threshold: f32,
    },

    /// The image is too small for the configured notch size range.
    #[error("image too small for gap detection: {width}x{height}px")]
    ImageTooSmall {
        /// Image width in pixels.
        width: u32,
        /// Image height in pixels.
        height: u32,
    },

    /// The puzzle-piece template is unusable (wrong size, empty alpha mask).
    #[error("invalid template: {0}")]
    InvalidTemplate(&'static str),

    /// An [`crate::ocr::OcrSolver`] implementation failed.
    #[error("ocr failed: {0}")]
    Ocr(String),
}
