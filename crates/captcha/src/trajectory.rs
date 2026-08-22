//! Human-like drag trajectory generation for slider captchas.
//!
//! Profile: fast ease-out start that decelerates toward the target, a small
//! overshoot past it, then a smooth correction back — plus random ±2px
//! vertical jitter and irregular 8–22ms sample intervals. Total duration is
//! distance-scaled and clamped to roughly 0.6–1.4s, matching measured human
//! drag behavior. Use [`generate_trajectory_seeded`] for reproducible output
//! in tests.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// One relative mouse-move sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrajectoryPoint {
    /// Horizontal movement since the previous sample (pixels, signed).
    pub dx: f64,
    /// Vertical movement since the previous sample (pixels, signed).
    pub dy: f64,
    /// Delay before this move is emitted, in milliseconds.
    pub dt_ms: u64,
}

/// Generate a human-like drag trajectory covering `distance` pixels.
///
/// The cumulative `dx` ends exactly on `distance`; intermediate samples
/// overshoot it slightly and then correct back.
pub fn generate_trajectory(distance: i32, rng: &mut impl Rng) -> Vec<TrajectoryPoint> {
    let distance = f64::from(distance.max(1));

    // Distance-scaled duration with jitter, clamped to the human range.
    let total_ms = (550.0 + distance * 1.5 + rng.random_range(-60.0..120.0)).clamp(600.0, 1400.0);

    // Overshoot grows with distance but stays small, like a real hand.
    let overshoot = (2.0 + distance * 0.03 + rng.random_range(0.0..3.0)).min(9.0);
    let target = distance + overshoot;

    // Fraction of the time spent in the main (accelerating) phase; the rest
    // is the correction back from the overshoot.
    let split = 0.78 + rng.random_range(0.0..0.08);

    // Irregular sample intervals (mouse polling is not perfectly uniform).
    let mut times = vec![0.0f64];
    let mut t = 0.0;
    while t < total_ms {
        t = (t + rng.random_range(8.0..22.0)).min(total_ms);
        times.push(t);
    }

    let pos = |t: f64| -> f64 {
        let tf = t / total_ms;
        if tf <= split {
            // Ease-out cubic: fast start, decelerating into the target.
            let u = tf / split;
            target * (1.0 - (1.0 - u).powi(3))
        } else {
            // Smoothstep correction from the overshoot back to the target.
            let u = (tf - split) / (1.0 - split);
            target - overshoot * (u * u * (3.0 - 2.0 * u))
        }
    };

    let mut points = Vec::with_capacity(times.len());
    let mut prev_x = 0.0f64;
    let mut y = 0.0f64;
    for (i, &t) in times.iter().enumerate() {
        let px = pos(t);
        let dx = px - prev_x;
        prev_x = px;
        // Vertical jitter as a clamped random walk, never beyond ±2px
        // cumulative; emit the *applied* delta so the walk and the emitted
        // samples stay consistent.
        let dy = rng.random_range(-1.5..1.5);
        let new_y = (y + dy).clamp(-2.0, 2.0);
        let dy = new_y - y;
        y = new_y;
        let dt_ms = if i == 0 {
            0
        } else {
            (times[i] - times[i - 1]).round() as u64
        };
        points.push(TrajectoryPoint { dx, dy, dt_ms });
    }

    // Land exactly on the requested distance (rounding drift goes into the
    // final, tiny correction step).
    let sum: f64 = points.iter().map(|p| p.dx).sum();
    if let Some(last) = points.last_mut() {
        last.dx += distance - sum;
    }
    points
}

/// Generate a trajectory with a fixed seed — deterministic, for tests and
/// reproducible behavior replay.
pub fn generate_trajectory_seeded(distance: i32, seed: u64) -> Vec<TrajectoryPoint> {
    let mut rng = StdRng::seed_from_u64(seed);
    generate_trajectory(distance, &mut rng)
}
