//! Jigsaw-notch ("slider gap") detection on captcha background images.
//!
//! Two strategies, both pure-Rust on top of the `image` crate:
//!
//! - [`detect_gap`] / [`detect_gap_with_config`]: column-profile analysis.
//!   The notch's left and right borders are strong vertical edges, while the
//!   notch interior is a flat fill (usually darkened or lightened) with far
//!   less texture than the background. After grayscale conversion, min-max
//!   brightness normalization and a horizontal Sobel pass, each candidate
//!   `(x, notch_width)` is scored by the ratio of border column energy to
//!   interior column energy. The leftmost [`GapConfig::ignore_left_fraction`]
//!   of the image is skipped because the slider piece always starts there.
//!
//! - [`detect_gap_with_template`]: when the puzzle-piece image is available,
//!   the boundary of its alpha mask is matched against the background edge
//!   map with normalized cross-correlation (NCC), accelerated with integral
//!   images. This variant is more robust on busy backgrounds.
//!
//! Both return [`GapDetection`] with a confidence in `[0, 1]`; below
//! [`GapConfig::confidence_threshold`] a typed [`CaptchaError::LowConfidence`]
//! is returned instead of a silent guess.

use image::DynamicImage;

use crate::CaptchaError;

/// Score ratio below which confidence is 0.
const RATIO_FLOOR: f64 = 1.6;
/// Score ratio at or above which confidence saturates at 1.
const RATIO_FULL: f64 = 4.5;

/// Result of a successful gap detection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GapDetection {
    /// X coordinate of the notch's left edge, in pixels.
    pub x: u32,
    /// Detection confidence in `[0, 1]` (already past the threshold).
    pub confidence: f32,
}

/// Tunables for gap detection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GapConfig {
    /// Fraction of the image width (from the left) that is ignored: the slider
    /// piece sits there initially, so edge structure in that zone is not the
    /// notch. Default `0.15`.
    pub ignore_left_fraction: f32,
    /// Minimum notch width in pixels. Default `24`.
    pub min_notch_width: u32,
    /// Maximum notch width in pixels (including any side knob). Default `110`.
    pub max_notch_width: u32,
    /// Minimum accepted confidence; below it [`CaptchaError::LowConfidence`]
    /// is returned. Default `0.35`.
    pub confidence_threshold: f32,
}

impl Default for GapConfig {
    fn default() -> Self {
        Self {
            ignore_left_fraction: 0.15,
            min_notch_width: 24,
            max_notch_width: 110,
            confidence_threshold: 0.35,
        }
    }
}

/// Detect the notch x-position via column-profile edge analysis.
pub fn detect_gap(background: &DynamicImage) -> Result<GapDetection, CaptchaError> {
    detect_gap_with_config(background, &GapConfig::default())
}

/// Detect the notch x-position via column-profile edge analysis, with config.
pub fn detect_gap_with_config(
    background: &DynamicImage,
    config: &GapConfig,
) -> Result<GapDetection, CaptchaError> {
    let (gray, w, h) = normalized_luma(background);
    check_size(w, h, config)?;

    let (w, h) = (w as usize, h as usize);
    let edges = sobel_x(&gray, w, h);

    // Per-column vertical-edge energy, plus prefix sums for O(1) window means.
    let mut col = vec![0.0f64; w];
    for (x, c) in col.iter_mut().enumerate() {
        let mut sum = 0.0;
        for y in 0..h {
            sum += f64::from(edges[y * w + x]);
        }
        *c = sum;
    }
    let mut prefix = vec![0.0f64; w + 1];
    for (x, &c) in col.iter().enumerate() {
        prefix[x + 1] = prefix[x] + c;
    }

    let x_start = (w as f32 * config.ignore_left_fraction) as usize;
    let w_min = config.min_notch_width as usize;
    let w_max = config.max_notch_width as usize;

    let mut best_x = x_start;
    let mut best_ratio = 0.0f64;
    for x in x_start..w.saturating_sub(w_min + 1) {
        let max_w = w_max.min(w - 1 - x);
        for nw in w_min..=max_w {
            let border = col[x] + col[x + nw];
            let interior = (prefix[x + nw] - prefix[x + 1]) / (nw - 1).max(1) as f64;
            // Border columns of a notch are far stronger than its flat fill;
            // on plain background texture this ratio stays near 1.
            let ratio = border / (2.0 * interior + 1.0);
            if ratio > best_ratio {
                best_ratio = ratio;
                best_x = x;
            }
        }
    }

    let confidence =
        ((best_ratio - RATIO_FLOOR) / (RATIO_FULL - RATIO_FLOOR)).clamp(0.0, 1.0) as f32;
    tracing::debug!(
        x = best_x,
        ratio = best_ratio,
        confidence,
        "notch column-profile scan"
    );
    if confidence < config.confidence_threshold {
        return Err(CaptchaError::LowConfidence {
            confidence,
            threshold: config.confidence_threshold,
        });
    }
    Ok(GapDetection {
        x: best_x as u32,
        confidence,
    })
}

/// Detect the notch x-position by matching the puzzle piece's alpha-mask
/// boundary against the background edge map (normalized cross-correlation).
///
/// `piece` must be the puzzle-piece image with a transparent background
/// (alpha = 0 outside the piece shape), as served by most slider providers.
pub fn detect_gap_with_template(
    background: &DynamicImage,
    piece: &DynamicImage,
    config: &GapConfig,
) -> Result<GapDetection, CaptchaError> {
    let (gray, w, h) = normalized_luma(background);
    check_size(w, h, config)?;

    let (w, h) = (w as usize, h as usize);
    let edge = sobel_magnitude(&gray, w, h);

    let tpl = piece.to_rgba8();
    let (tw, th) = (tpl.width() as usize, tpl.height() as usize);
    if tw < 8 || th < 8 {
        return Err(CaptchaError::InvalidTemplate("piece smaller than 8x8"));
    }
    if tw > w || th > h {
        return Err(CaptchaError::InvalidTemplate("piece larger than background"));
    }

    // Alpha-mask boundary pixels: opaque pixels touching a transparent one.
    // Inset by 1px so they never land on the zeroed Sobel frame.
    let alpha_at = |x: usize, y: usize| tpl.get_pixel(x as u32, y as u32).0[3];
    let mut pts: Vec<(usize, usize)> = Vec::new();
    for y in 1..th - 1 {
        for x in 1..tw - 1 {
            if alpha_at(x, y) > 127
                && (alpha_at(x - 1, y) <= 127
                    || alpha_at(x + 1, y) <= 127
                    || alpha_at(x, y - 1) <= 127
                    || alpha_at(x, y + 1) <= 127)
            {
                pts.push((x, y));
            }
        }
    }
    if pts.len() < 8 {
        return Err(CaptchaError::InvalidTemplate(
            "alpha mask has no usable boundary",
        ));
    }

    // Integral images of the edge map and its square for O(1) window stats.
    let stride = w + 1;
    let mut ii = vec![0.0f64; stride * (h + 1)];
    let mut ii2 = vec![0.0f64; stride * (h + 1)];
    for y in 0..h {
        for x in 0..w {
            let e = f64::from(edge[y * w + x]);
            let dst = (y + 1) * stride + x + 1;
            ii[dst] = e + ii[y * stride + x + 1] + ii[(y + 1) * stride + x] - ii[y * stride + x];
            ii2[dst] =
                e * e + ii2[y * stride + x + 1] + ii2[(y + 1) * stride + x] - ii2[y * stride + x];
        }
    }
    let rect_sum = |table: &[f64], x: usize, y: usize| -> f64 {
        table[(y + th) * stride + x + tw] - table[y * stride + x + tw]
            - table[(y + th) * stride + x]
            + table[y * stride + x]
    };

    let n = (tw * th) as f64;
    let s_t = pts.len() as f64; // template sum (binary boundary mask)
    let s_tt = pts.len() as f64; // template sum of squares
    let x_start = (w as f32 * config.ignore_left_fraction) as usize;

    let mut best_ncc = f64::MIN;
    let mut best_x = x_start;
    for y in 0..=(h - th) {
        for x in x_start..=(w - tw) {
            let s_i = rect_sum(&ii, x, y);
            let s_ii = rect_sum(&ii2, x, y);
            let mut s_ti = 0.0f64;
            for &(px, py) in &pts {
                s_ti += f64::from(edge[(y + py) * w + (x + px)]);
            }
            let num = s_ti - s_t * s_i / n;
            let var_t = s_tt - s_t * s_t / n;
            let var_i = s_ii - s_i * s_i / n;
            let den = (var_t * var_i).sqrt();
            let ncc = if den > 1e-9 { num / den } else { 0.0 };
            if ncc > best_ncc {
                best_ncc = ncc;
                best_x = x;
            }
        }
    }

    let confidence = best_ncc.clamp(0.0, 1.0) as f32;
    tracing::debug!(x = best_x, ncc = best_ncc, confidence, "template match");
    if confidence < config.confidence_threshold {
        return Err(CaptchaError::LowConfidence {
            confidence,
            threshold: config.confidence_threshold,
        });
    }
    Ok(GapDetection {
        x: best_x as u32,
        confidence,
    })
}

fn check_size(w: u32, h: u32, config: &GapConfig) -> Result<(), CaptchaError> {
    let min_w = config.min_notch_width + 8 + (w as f32 * config.ignore_left_fraction) as u32;
    if w < min_w || h < 32 {
        return Err(CaptchaError::ImageTooSmall {
            width: w,
            height: h,
        });
    }
    Ok(())
}

/// Grayscale with min-max stretch, so dark/bright providers normalize to the
/// same contrast range before edge detection.
fn normalized_luma(image: &DynamicImage) -> (Vec<f32>, u32, u32) {
    let gray = image.to_luma8();
    let (w, h) = gray.dimensions();
    let mut min = u8::MAX;
    let mut max = u8::MIN;
    for &p in gray.as_raw() {
        min = min.min(p);
        max = max.max(p);
    }
    let span = f32::from(max.saturating_sub(min)).max(1.0);
    let data = gray
        .as_raw()
        .iter()
        .map(|&p| f32::from(p - min) * 255.0 / span)
        .collect();
    (data, w, h)
}

/// Horizontal Sobel gradient magnitude; vertical edges light up.
/// The outer 1px frame is left at zero.
fn sobel_x(gray: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let i = y * w + x;
            let gx = -gray[i - w - 1] - 2.0 * gray[i - 1] - gray[i + w - 1]
                + gray[i - w + 1]
                + 2.0 * gray[i + 1]
                + gray[i + w + 1];
            out[i] = gx.abs();
        }
    }
    out
}

/// Full Sobel gradient magnitude (`|gx| + |gy|`); used for template matching.
fn sobel_magnitude(gray: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let i = y * w + x;
            let gx = -gray[i - w - 1] - 2.0 * gray[i - 1] - gray[i + w - 1]
                + gray[i - w + 1]
                + 2.0 * gray[i + 1]
                + gray[i + w + 1];
            let gy = -gray[i - w - 1] - 2.0 * gray[i - w] - gray[i - w + 1]
                + gray[i + w - 1]
                + 2.0 * gray[i + w]
                + gray[i + w + 1];
            out[i] = gx.abs() + gy.abs();
        }
    }
    out
}
