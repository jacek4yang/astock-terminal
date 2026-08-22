//! Golden tests: replay the full pipeline from each fixture's inputs and
//! compare against the legacy-produced `outputs.signal` field-by-field.
//!
//! Comparison rules: integers and strings match exactly; floats match within
//! 1e-4 relative tolerance (fixtures are rounded to 6 decimals).

use astock_technical::types::{Breadth, FundFlow, Kline, Quote};
use serde_json::Value;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/golden")
}

#[derive(serde::Deserialize)]
struct FixtureInputs {
    klines: Vec<Kline>,
    quote: Option<Quote>,
    flows: Option<Vec<FundFlow>>,
    index_klines: Option<Vec<Kline>>,
    breadth: Option<Breadth>,
}

#[derive(serde::Deserialize)]
struct Fixture {
    inputs: FixtureInputs,
    outputs: FixtureOutputs,
}

#[derive(serde::Deserialize)]
struct FixtureOutputs {
    signal: Value,
}

/// Recursively compare expected vs actual, collecting human-readable diffs.
fn diff_values(expected: &Value, actual: &Value, path: &str, diffs: &mut Vec<String>) {
    if diffs.len() > 50 {
        return;
    }
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => {
            for (k, ev) in e {
                match a.get(k) {
                    Some(av) => diff_values(ev, av, &format!("{}.{}", path, k), diffs),
                    None => diffs.push(format!("{}.{}: MISSING in actual", path, k)),
                }
            }
            for k in a.keys() {
                if !e.contains_key(k) {
                    diffs.push(format!("{}.{}: EXTRA in actual ({})", path, k, a[k]));
                }
            }
        }
        (Value::Array(e), Value::Array(a)) => {
            if e.len() != a.len() {
                diffs.push(format!(
                    "{}: array length expected {} got {}",
                    path,
                    e.len(),
                    a.len()
                ));
                return;
            }
            for (i, (ev, av)) in e.iter().zip(a.iter()).enumerate() {
                diff_values(ev, av, &format!("{}[{}]", path, i), diffs);
            }
        }
        (Value::Number(e), Value::Number(a)) => {
            if e.is_i64() || e.is_u64() {
                // Integers must match exactly (and stay integers).
                if !(a.is_i64() || a.is_u64()) || e.as_i64() != a.as_i64() {
                    diffs.push(format!("{}: int expected {} got {}", path, e, a));
                }
            } else {
                let (ef, af) = (e.as_f64().unwrap(), a.as_f64().unwrap());
                let tol = 1e-4 * ef.abs().max(af.abs()).max(1.0);
                if (ef - af).abs() > tol {
                    diffs.push(format!("{}: float expected {} got {}", path, ef, af));
                }
            }
        }
        _ => {
            if expected != actual {
                diffs.push(format!("{}: expected {} got {}", path, expected, actual));
            }
        }
    }
}

#[test]
fn golden_signal_matches_legacy() {
    let dir = fixtures_dir();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", dir.display(), e))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    assert_eq!(paths.len(), 16, "expected 16 golden fixtures");

    for path in &paths {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let raw = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {}", path.display(), e));
        let fixture: Fixture = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("cannot parse {}: {}", name, e));

        let actual = astock_technical::analyze(
            &fixture.inputs.klines,
            fixture.inputs.quote.as_ref(),
            fixture.inputs.flows.as_deref(),
            fixture.inputs.index_klines.as_deref(),
            fixture.inputs.breadth.as_ref(),
        );

        let mut diffs = Vec::new();
        diff_values(&fixture.outputs.signal, &actual, "signal", &mut diffs);
        assert!(
            diffs.is_empty(),
            "{}: {} diffs vs legacy:\n  {}",
            name,
            diffs.len(),
            diffs.join("\n  ")
        );

        // Surface the headline fields in the test log for the report.
        let score = actual.get("score").and_then(Value::as_i64).unwrap_or(-1);
        let action = actual
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("?");
        eprintln!("golden {:<20} score={:<3} action={} MATCHED", name, score, action);
    }
}
