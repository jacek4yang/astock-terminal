//! # astock-captcha
//!
//! Pure-Rust slider-captcha solving toolkit (no OpenCV):
//!
//! - [`gap`]: locate the jigsaw notch in a slider-captcha background image —
//!   column-profile edge analysis, or alpha-mask template matching when the
//!   puzzle piece image is available.
//! - [`trajectory`]: generate a human-like drag path (ease-out, overshoot,
//!   correction, jitter, realistic timing).
//! - [`solve_slider`]: one-call combination of both.
//! - [`mod@ocr`] (optional feature `ocr`, default OFF): a thin [`ocr::OcrSolver`]
//!   trait so integrating crates can plug in an OCR engine (e.g. ddddocr-rs)
//!   without this crate depending on ONNX.
//!
//! ## Scope and honest limitations
//!
//! This crate solves the **image side only**. Real slider services vary
//! widely: rotated pieces, interference lines drawn over the notch, hollow
//! notches, and — most importantly — **server-side behavioral analysis** of
//! the drag (timing distributions, velocity curves, preceding page behavior).
//! No trajectory generator guarantees acceptance. Synthetic tests in this
//! crate prove the algorithm works on programmatically generated notches;
//! they say nothing about real-world success rates. Integrating providers
//! must measure success live and use **bounded retries with captcha refresh**
//! (treat [`CaptchaError::LowConfidence`] as a refresh signal, never guess).
//! There is no claim of guaranteed bypass.

mod error;
pub mod gap;
#[cfg(feature = "ocr")]
pub mod ocr;
pub mod trajectory;

use image::DynamicImage;

pub use error::CaptchaError;
pub use gap::{
    detect_gap, detect_gap_with_config, detect_gap_with_template, GapConfig, GapDetection,
};
pub use trajectory::{generate_trajectory, generate_trajectory_seeded, TrajectoryPoint};

/// Full solution for one slider captcha challenge.
#[derive(Debug, Clone)]
pub struct SliderSolution {
    /// Drag distance in pixels: the x-coordinate of the notch's left edge.
    ///
    /// If the slider piece does not start at x = 0 in the page, subtract its
    /// initial x offset before applying this distance.
    pub distance: u32,
    /// Relative mouse moves (`dx`, `dy`, `dt_ms`) covering `distance`.
    pub trajectory: Vec<TrajectoryPoint>,
    /// Detection confidence in `[0, 1]`.
    pub confidence: f32,
}

/// Solve a slider captcha: detect the notch, then build the drag trajectory.
///
/// Pass the puzzle-piece image as `piece` when the provider serves it — the
/// alpha-mask template match is more robust than plain notch detection.
pub fn solve_slider(
    background: &DynamicImage,
    piece: Option<&DynamicImage>,
) -> Result<SliderSolution, CaptchaError> {
    let config = GapConfig::default();
    let detection = match piece {
        Some(p) => gap::detect_gap_with_template(background, p, &config)?,
        None => gap::detect_gap_with_config(background, &config)?,
    };
    let mut rng = rand::rng();
    let trajectory = trajectory::generate_trajectory(detection.x as i32, &mut rng);
    Ok(SliderSolution {
        distance: detection.x,
        trajectory,
        confidence: detection.confidence,
    })
}
