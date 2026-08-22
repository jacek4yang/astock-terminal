//! Shared JSON comparison helpers for golden-fixture tests.

use serde_json::Value;

/// Recursively compare `actual` against `expected`, appending a human-readable
/// entry to `errors` for every mismatch.
///
/// - integers compare exactly;
/// - floats compare within 1e-4 relative (floor of 1.0 absolute);
/// - strings/bools/null compare exactly;
/// - arrays compare element-wise and must have equal length;
/// - objects must have identical key sets and compare per key.
pub fn compare_values(path: &str, expected: &Value, actual: &Value, errors: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => {
            for key in e.keys() {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match a.get(key) {
                    Some(av) => compare_values(&child, &e[key], av, errors),
                    None => errors.push(format!("{child}: missing in actual")),
                }
            }
            for key in a.keys() {
                if !e.contains_key(key) {
                    errors.push(format!("{path}.{key}: unexpected extra key in actual"));
                }
            }
        }
        (Value::Array(e), Value::Array(a)) => {
            if e.len() != a.len() {
                errors.push(format!(
                    "{path}: array length expected {} got {}",
                    e.len(),
                    a.len()
                ));
                return;
            }
            for (i, (ev, av)) in e.iter().zip(a.iter()).enumerate() {
                compare_values(&format!("{path}[{i}]"), ev, av, errors);
            }
        }
        (Value::Number(e), Value::Number(a)) => {
            let integral = |n: &serde_json::Number| n.is_i64() || n.is_u64();
            if integral(e) && integral(a) {
                if e != a {
                    errors.push(format!("{path}: int expected {e} got {a}"));
                }
                return;
            }
            let (ev, av) = (e.as_f64().unwrap(), a.as_f64().unwrap());
            let tol = 1e-4 * ev.abs().max(1.0);
            if (ev - av).abs() > tol {
                errors.push(format!("{path}: float expected {ev} got {av} (tol {tol})"));
            }
        }
        (e, a) => {
            if e != a {
                errors.push(format!("{path}: expected {e} got {a}"));
            }
        }
    }
}
