//! Helpers reproducing CPython numeric formatting/rounding semantics so the
//! ported pipeline produces byte-identical output strings.

/// Reproduce Python's `round(value, digits)`.
///
/// CPython rounds the exact decimal expansion of the binary double with
/// ties-to-even. Rust's `{:.N}` float formatting is also correctly rounded
/// with ties-to-even, so format-then-parse matches CPython bit-for-bit.
pub fn py_round(value: f64, digits: u32) -> f64 {
    format!("{:.*}", digits as usize, value)
        .parse()
        .unwrap_or(value)
}

/// Reproduce Python's `str(float)` / f-string `{x}` conversion.
///
/// Both CPython `repr` and Rust's `Debug` for `f64` emit the shortest string
/// that round-trips, and both keep a trailing `.0` for integral values
/// (`1362.0`, not `1362`). For the price/volume magnitudes used here (no
/// exponent notation on either side), the outputs are identical.
pub fn py_f64(value: f64) -> String {
    format!("{value:?}")
}

/// Reproduce Python's `int(x)` truncation toward zero for finite values.
pub fn py_int(value: f64) -> i64 {
    value.trunc() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_half_even_matches_python() {
        // Python: round(2.675, 2) == 2.67 (binary value is 2.67499999...)
        assert_eq!(py_round(2.675, 2), 2.67);
        // 0.125 is exact in binary -> ties-to-even -> 0.12, as Python does
        assert_eq!(py_round(0.125, 2), 0.12);
        assert_eq!(py_round(0.375, 2), 0.38);
    }

    #[test]
    fn float_str_matches_python_repr() {
        assert_eq!(py_f64(1362.0), "1362.0");
        assert_eq!(py_f64(1279.58), "1279.58");
        assert_eq!(py_f64(0.8), "0.8");
        assert_eq!(py_f64(1.4), "1.4");
    }
}
