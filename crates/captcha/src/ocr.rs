//! Optional OCR glue (feature `ocr`, default OFF).
//!
//! This crate deliberately ships **no** ONNX/runtime dependency. Image-text
//! captchas (digits/letters) are recognized by an implementation plugged in
//! by the integrating crate — e.g. `astock-wencai` wrapping `ddddocr-rs`.

use image::DynamicImage;

use crate::CaptchaError;

/// Pluggable solver for image-text captchas.
///
/// Implementations are expected to be cheap to construct and safe to call
/// repeatedly; construction-time model loading belongs to the implementor.
pub trait OcrSolver {
    /// Recognize the text contained in `image`.
    fn solve(&self, image: &DynamicImage) -> Result<String, CaptchaError>;
}

/// Any closure with the right signature is an [`OcrSolver`], which keeps the
/// integrating crate free of boilerplate for trivial adapters.
impl<F> OcrSolver for F
where
    F: Fn(&DynamicImage) -> Result<String, CaptchaError>,
{
    fn solve(&self, image: &DynamicImage) -> Result<String, CaptchaError> {
        self(image)
    }
}
