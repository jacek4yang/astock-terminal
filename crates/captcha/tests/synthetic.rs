//! Synthetic slider-captcha tests.
//!
//! All images are generated programmatically (gradient + noise background,
//! jigsaw-shaped notch with a 1px bright border mimicking real providers).
//! These tests prove the algorithm on synthetic data only — real-world
//! success rates must be measured live by the integrating provider.

use astock_captcha::{
    detect_gap, detect_gap_with_config, detect_gap_with_template, generate_trajectory_seeded,
    solve_slider, CaptchaError, GapConfig,
};
use image::{DynamicImage, Rgba, RgbaImage};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

// ---------------------------------------------------------------------------
// Synthetic image generation
// ---------------------------------------------------------------------------

/// Gradient + per-pixel noise background, vaguely photo-like in statistics.
fn synth_background(rng: &mut StdRng, w: u32, h: u32, noise: i32) -> RgbaImage {
    let mut img = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let t = x as f32 / w as f32;
            let u = y as f32 / h as f32;
            let base = 70.0 + 100.0 * t + 30.0 * u;
            let j = |rng: &mut StdRng| rng.random_range(-noise..=noise) as f32;
            let clamp = |v: f32| v.clamp(0.0, 255.0) as u8;
            img.put_pixel(
                x,
                y,
                Rgba([
                    clamp(base + j(rng)),
                    clamp(base * 0.9 + 20.0 * u + j(rng)),
                    clamp(200.0 - base * 0.5 + j(rng)),
                    255,
                ]),
            );
        }
    }
    img
}

/// Jigsaw-notch mask: a `w`x`h` rectangle with a round knob on the top edge.
/// Returns `(mask, mask_width, mask_height)`; mask height = h + knob radius.
fn notch_mask(w: u32, h: u32, knob_r: u32) -> (Vec<bool>, u32, u32) {
    let mh = h + knob_r;
    let mut mask = vec![false; (w * mh) as usize];
    // Rectangle body.
    for y in 0..h {
        for x in 0..w {
            mask[((y + knob_r) * w + x) as usize] = true;
        }
    }
    // Knob: circle centered on the middle of the top edge.
    let cx = w as i64 / 2;
    let cy = knob_r as i64;
    let r = knob_r as i64;
    for y in 0..knob_r {
        for x in 0..w {
            let dx = x as i64 - cx;
            let dy = y as i64 - cy;
            if dx * dx + dy * dy <= r * r {
                mask[(y * w + x) as usize] = true;
            }
        }
    }
    (mask, w, mh)
}

/// Notch geometry: mask plus where it is stamped into the background.
struct Notch<'a> {
    mask: &'a [bool],
    mw: u32,
    mh: u32,
    x0: u32,
    y0: u32,
}

/// Stamp the notch into the background: flat fill (darker or lighter) with
/// light noise, then a 1px bright border like real providers.
fn apply_notch(bg: &mut RgbaImage, rng: &mut StdRng, n: &Notch<'_>, darken: Option<f32>) {
    let &Notch { mask, mw, mh, x0, y0 } = n;
    for my in 0..mh {
        for mx in 0..mw {
            if !mask[(my * mw + mx) as usize] {
                continue;
            }
            let p = bg.get_pixel_mut(x0 + mx, y0 + my);
            for c in &mut p.0[..3] {
                let v = match darken {
                    Some(f) => f32::from(*c) * f,
                    None => f32::from(*c) + 85.0,
                };
                *c = (v + rng.random_range(-3.0..3.0)).clamp(0.0, 255.0) as u8;
            }
        }
    }
    // 1px bright border on mask boundary pixels.
    let inside = |x: i64, y: i64| {
        x >= 0 && y >= 0 && x < mw as i64 && y < mh as i64 && mask[(y as u32 * mw + x as u32) as usize]
    };
    for my in 0..mh as i64 {
        for mx in 0..mw as i64 {
            if inside(mx, my)
                && !(inside(mx - 1, my) && inside(mx + 1, my) && inside(mx, my - 1) && inside(mx, my + 1))
            {
                let p = bg.get_pixel_mut(x0 + mx as u32, y0 + my as u32);
                *p = Rgba([232, 230, 226, 255]);
            }
        }
    }
}

/// One randomized synthetic case. Returns `(background, notch_x)`.
fn synth_case(rng: &mut StdRng) -> (DynamicImage, u32) {
    let w = rng.random_range(280..=340);
    let h = rng.random_range(140..=180);
    let noise = rng.random_range(8..=24);
    let mut bg = synth_background(rng, w, h, noise);

    let nw = rng.random_range(40..=64);
    let nh = rng.random_range(40..=60);
    let knob = rng.random_range(8..=14);
    let (mask, mw, mh) = notch_mask(nw, nh, knob);

    let x0 = rng.random_range((w as f32 * 0.4) as u32..=w - mw - 8);
    let y0 = rng.random_range(12..=h - mh - 8);
    let darken = if rng.random::<bool>() {
        Some(rng.random_range(0.35..0.6))
    } else {
        None
    };
    apply_notch(&mut bg, rng, &Notch { mask: &mask, mw, mh, x0, y0 }, darken);
    (DynamicImage::ImageRgba8(bg), x0)
}

/// Build a puzzle piece: pixels of the *clean* background at the notch
/// position, alpha = 255 inside the mask and 0 outside.
fn synth_piece(
    clean_bg: &RgbaImage,
    mask: &[bool],
    mw: u32,
    mh: u32,
    x0: u32,
    y0: u32,
) -> DynamicImage {
    let mut piece = RgbaImage::new(mw, mh);
    for my in 0..mh {
        for mx in 0..mw {
            if mask[(my * mw + mx) as usize] {
                piece.put_pixel(mx, my, *clean_bg.get_pixel(x0 + mx, y0 + my));
            }
        }
    }
    DynamicImage::ImageRgba8(piece)
}

// ---------------------------------------------------------------------------
// Notch detection (column profile)
// ---------------------------------------------------------------------------

#[test]
fn notch_detection_randomized_20_cases() {
    let mut rng = StdRng::seed_from_u64(0x0CA7_CA11);
    let mut worst_err = 0i64;
    for case in 0..20 {
        let (bg, x0) = synth_case(&mut rng);
        let det = detect_gap(&bg).unwrap_or_else(|e| panic!("case {case}: detect failed: {e}"));
        let err = i64::from(det.x) - i64::from(x0);
        worst_err = worst_err.max(err.abs());
        assert!(
            err.abs() <= 3,
            "case {case}: detected x={} true x={x0} (err {err}, conf {:.3})",
            det.x,
            det.confidence
        );
    }
    eprintln!("notch detection: worst |err| over 20 cases = {worst_err}px");
}

#[test]
fn notch_free_image_yields_low_confidence_error() {
    let mut rng = StdRng::seed_from_u64(42);
    // Textured background with no notch at all.
    let bg = DynamicImage::ImageRgba8(synth_background(&mut rng, 300, 150, 20));
    match detect_gap(&bg) {
        Err(CaptchaError::LowConfidence { confidence, threshold }) => {
            assert!(confidence < threshold);
        }
        other => panic!("expected LowConfidence, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Template matching (alpha-mask NCC)
// ---------------------------------------------------------------------------

#[test]
fn template_match_recovers_x() {
    let mut rng = StdRng::seed_from_u64(0x7E29_1A5E);
    for case in 0..6 {
        let w = rng.random_range(280..=320);
        let h = rng.random_range(140..=170);
        let noise = rng.random_range(10..=20);
        let clean = synth_background(&mut rng, w, h, noise);

        let nw = rng.random_range(42..=58);
        let nh = rng.random_range(42..=56);
        let knob = rng.random_range(8..=12);
        let (mask, mw, mh) = notch_mask(nw, nh, knob);
        let x0 = rng.random_range((w as f32 * 0.4) as u32..=w - mw - 8);
        let y0 = rng.random_range(12..=h - mh - 8);

        let piece = synth_piece(&clean, &mask, mw, mh, x0, y0);
        let mut bg = clean.clone();
        apply_notch(&mut bg, &mut rng, &Notch { mask: &mask, mw, mh, x0, y0 }, Some(0.45));
        let bg = DynamicImage::ImageRgba8(bg);

        let det = detect_gap_with_template(&bg, &piece, &GapConfig::default())
            .unwrap_or_else(|e| panic!("case {case}: template match failed: {e}"));
        let err = i64::from(det.x) - i64::from(x0);
        assert!(
            err.abs() <= 2,
            "case {case}: template x={} true x={x0} (err {err}, conf {:.3})",
            det.x,
            det.confidence
        );
    }
}

// ---------------------------------------------------------------------------
// Trajectory
// ---------------------------------------------------------------------------

#[test]
fn trajectory_reaches_distance_with_overshoot() {
    for distance in [40, 120, 230] {
        let traj = generate_trajectory_seeded(distance, 7);
        let cum: Vec<f64> = traj
            .iter()
            .scan(0.0, |acc, p| {
                *acc += p.dx;
                Some(*acc)
            })
            .collect();
        let total = *cum.last().unwrap();
        assert!(
            (total - f64::from(distance)).abs() <= 1.0,
            "distance {distance}: cumulative dx {total}"
        );
        // Overshoot: at some point we went past the target, then came back.
        let peak = cum.iter().cloned().fold(f64::MIN, f64::max);
        assert!(peak > f64::from(distance) + 1.0, "distance {distance}: peak {peak}");
        // Monotonic-ish: nondecreasing up to the peak (the correction phase
        // after the overshoot is intentionally backward).
        let peak_idx = cum
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        for w in cum[..=peak_idx].windows(2) {
            assert!(w[1] >= w[0] - 1e-9, "distance {distance}: regression in main phase");
        }
        // Vertical jitter stays within ±2px cumulative.
        let y: f64 = traj.iter().map(|p| p.dy).sum();
        assert!(y.abs() <= 2.0 + 1e-9, "distance {distance}: cumulative dy {y}");
    }
}

#[test]
fn trajectory_duration_within_human_bounds() {
    for distance in [30, 100, 200, 300] {
        for seed in 0..5 {
            let traj = generate_trajectory_seeded(distance, seed);
            let total_ms: u64 = traj.iter().map(|p| p.dt_ms).sum();
            assert!(
                (550..=1450).contains(&total_ms),
                "distance {distance} seed {seed}: {total_ms}ms"
            );
        }
    }
}

#[test]
fn trajectory_deterministic_under_seed() {
    let a = generate_trajectory_seeded(150, 99);
    let b = generate_trajectory_seeded(150, 99);
    assert_eq!(a, b);
    let c = generate_trajectory_seeded(150, 100);
    assert_ne!(a, c);
}

// ---------------------------------------------------------------------------
// End-to-end
// ---------------------------------------------------------------------------

#[test]
fn solve_slider_end_to_end() {
    let mut rng = StdRng::seed_from_u64(2024);
    let (bg, x0) = synth_case(&mut rng);
    let sol = solve_slider(&bg, None).expect("solve without piece");
    assert!(sol.distance.abs_diff(x0) <= 3, "{} vs {x0}", sol.distance);
    let total: f64 = sol.trajectory.iter().map(|p| p.dx).sum();
    assert!((total - f64::from(sol.distance)).abs() <= 1.0);
    assert!(sol.confidence > 0.0);
}

#[test]
fn solve_slider_with_piece() {
    let mut rng = StdRng::seed_from_u64(777);
    let w = 300u32;
    let h = 150u32;
    let clean = synth_background(&mut rng, w, h, 15);
    let (mask, mw, mh) = notch_mask(50, 50, 10);
    let x0 = 180u32;
    let y0 = 30u32;
    let piece = synth_piece(&clean, &mask, mw, mh, x0, y0);
    let mut bg = clean.clone();
    apply_notch(&mut bg, &mut rng, &Notch { mask: &mask, mw, mh, x0, y0 }, Some(0.5));
    let bg = DynamicImage::ImageRgba8(bg);

    let sol = solve_slider(&bg, Some(&piece)).expect("solve with piece");
    assert!(sol.distance.abs_diff(x0) <= 2, "{} vs {x0}", sol.distance);
}

// ---------------------------------------------------------------------------
// OCR glue (feature-gated, compile + behavior smoke test)
// ---------------------------------------------------------------------------

#[cfg(feature = "ocr")]
#[test]
fn ocr_solver_trait_accepts_structs_and_closures() {
    use astock_captcha::ocr::OcrSolver;

    struct Dummy;
    impl OcrSolver for Dummy {
        fn solve(&self, _image: &DynamicImage) -> Result<String, CaptchaError> {
            Ok("ab12".to_string())
        }
    }
    let img = DynamicImage::ImageRgba8(RgbaImage::new(60, 20));
    assert_eq!(Dummy.solve(&img).unwrap(), "ab12");

    let closure = |_img: &DynamicImage| Ok("zz9".to_string());
    assert_eq!(OcrSolver::solve(&closure, &img).unwrap(), "zz9");

    // Object safety matters for plugin-style integration.
    let boxed: Box<dyn OcrSolver> = Box::new(Dummy);
    assert_eq!(boxed.solve(&img).unwrap(), "ab12");
}

// keep `detect_gap_with_config` import used even without extra tests
#[allow(unused)]
fn _use_config(bg: &DynamicImage) {
    let _ = detect_gap_with_config(bg, &GapConfig::default());
}
