//! Golden tests: run `analyze_chanlun_daily` on each fixture's input klines
//! and compare the full `daily_result_to_dict` output against the recorded
//! legacy output (counts and strings exact, floats within 1e-4 relative).

mod common;

use astock_chanlun::daily::{analyze_chanlun_daily, daily_result_to_dict};
use serde_json::Value;
use std::path::PathBuf;

const SYMBOLS: [&str; 8] = [
    "600519", "000001", "600036", "300750", "601318", "000858", "600900", "002594",
];
const PERIODS: [&str; 2] = ["day", "week"];

fn fixture_path(symbol: &str, period: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/golden")
        .join(format!("{symbol}_{period}.json"))
}

fn run_fixture(symbol: &str, period: &str) -> Vec<String> {
    let path = fixture_path(symbol, period);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let fixture: Value = serde_json::from_str(&raw).expect("fixture is valid JSON");

    let klines = fixture["inputs"]["klines"]
        .as_array()
        .expect("inputs.klines is an array");
    let mut dates = Vec::with_capacity(klines.len());
    let mut opens = Vec::with_capacity(klines.len());
    let mut closes = Vec::with_capacity(klines.len());
    let mut highs = Vec::with_capacity(klines.len());
    let mut lows = Vec::with_capacity(klines.len());
    let mut volumes = Vec::with_capacity(klines.len());
    for k in klines {
        dates.push(k["date"].as_str().unwrap().to_string());
        opens.push(k["open"].as_f64().unwrap());
        closes.push(k["close"].as_f64().unwrap());
        highs.push(k["high"].as_f64().unwrap());
        lows.push(k["low"].as_f64().unwrap());
        volumes.push(k["volume"].as_f64().unwrap());
    }

    let result = analyze_chanlun_daily(&dates, &opens, &closes, &highs, &lows, &volumes);
    let actual = daily_result_to_dict(&result);
    let expected = &fixture["outputs"]["chanlun_daily"];

    let mut errors = Vec::new();
    common::compare_values("chanlun_daily", expected, &actual, &mut errors);
    errors
}

#[test]
fn golden_daily_fixtures_match_legacy() {
    let mut failures = Vec::new();
    for symbol in SYMBOLS {
        for period in PERIODS {
            let errors = run_fixture(symbol, period);
            if errors.is_empty() {
                eprintln!("{symbol}_{period}: OK");
            } else {
                eprintln!("{symbol}_{period}: {} mismatch(es)", errors.len());
                for e in &errors {
                    eprintln!("  {e}");
                }
                failures.push(format!("{symbol}_{period}: {} mismatch(es)", errors.len()));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "golden mismatches:\n{}",
        failures.join("\n")
    );
}
