//! Python-compatible rounding helpers.
//!
//! The legacy implementation is Python, whose `round(x, n)` performs correct
//! rounding to nearest with ties-to-even on the exact binary value of `x`.
//! Rust's `{:.n}` float formatting uses the same correctly-rounded
//! nearest-even algorithm, so formatting and parsing back reproduces
//! `round(x, n)` bit-for-bit for finite values.

/// Equivalent of Python's `round(x, ndigits)` returning a float.
pub(crate) fn py_round(x: f64, ndigits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{:.*}", ndigits, x).parse().unwrap_or(x)
}

/// Equivalent of Python's `int(round(x))` (banker's rounding).
pub(crate) fn py_round_int(x: f64) -> i64 {
    x.round_ties_even() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ties_to_even() {
        // 0.125 is exactly representable: round(0.125, 2) == 0.12 in Python.
        assert_eq!(py_round(0.125, 2), 0.12);
        assert_eq!(py_round(0.375, 2), 0.38);
        // 2.675 is 2.67499... in binary: round(2.675, 2) == 2.67 in Python.
        assert_eq!(py_round(2.675, 2), 2.67);
        assert_eq!(py_round_int(94.5), 94);
        assert_eq!(py_round_int(93.5), 94);
        assert_eq!(py_round_int(-0.5), 0);
    }
}
