use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use astock_evaluation::{
    check_thresholds, compare, evaluate, read_json, render_html, write_json, Dataset, EvalError,
    EvalReport, GateResult, Thresholds,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> astock_evaluation::Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str) {
        Some("evaluate") => run_evaluate(&args[1..]),
        Some("compare") => run_compare(&args[1..]),
        _ => Err(EvalError::Invalid(usage().into())),
    }
}

fn run_evaluate(args: &[String]) -> astock_evaluation::Result<()> {
    let dataset_path = required(args, "--dataset")?;
    let thresholds_path = required(args, "--thresholds")?;
    let json_path = required(args, "--json")?;
    let html_path = required(args, "--html")?;
    let split = option(args, "--split").unwrap_or_else(|| "test".into());
    let baseline_path = option(args, "--baseline");
    let dataset: Dataset = read_json(Path::new(&dataset_path))?;
    let mut thresholds: Thresholds = read_json(Path::new(&thresholds_path))?;
    let report = evaluate(&dataset, &split)?;
    let baseline = baseline_path
        .as_deref()
        .map(|path| read_json::<EvalReport>(Path::new(path)))
        .transpose()?;
    if args.iter().any(|arg| arg == "--establish-baseline") {
        if baseline.is_some() {
            return Err(EvalError::Invalid(
                "建立新基线时不能同时传入旧基线".to_string(),
            ));
        }
        thresholds.max_regression.clear();
    }
    let gate = check_thresholds(&report, &thresholds, baseline.as_ref())?;
    write_json(Path::new(&json_path), &report)?;
    write_text(Path::new(&html_path), &render_html(&report, Some(&gate)))?;
    print_summary(&report, &gate);
    if args.iter().any(|arg| arg == "--check") && !gate.passed {
        return Err(EvalError::Gate(gate.violations.join("；")));
    }
    Ok(())
}

fn run_compare(args: &[String]) -> astock_evaluation::Result<()> {
    let from: EvalReport = read_json(Path::new(&required(args, "--from")?))?;
    let to: EvalReport = read_json(Path::new(&required(args, "--to")?))?;
    let output = PathBuf::from(required(args, "--json")?);
    write_json(&output, &compare(&from, &to)?)
}

fn option(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn required(args: &[String], name: &str) -> astock_evaluation::Result<String> {
    option(args, name).ok_or_else(|| EvalError::Invalid(format!("缺少参数 {name}\n{}", usage())))
}

fn write_text(path: &Path, value: &str) -> astock_evaluation::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| EvalError::Read {
            path: parent.display().to_string(),
            message: error.to_string(),
        })?;
    }
    fs::write(path, value).map_err(|error| EvalError::Read {
        path: path.display().to_string(),
        message: error.to_string(),
    })
}

fn print_summary(report: &EvalReport, gate: &GateResult) {
    println!(
        "评测完成：{} {} / {}，{} 个样例，门禁{}",
        report.dataset_id,
        report.dataset_version,
        report.split,
        report.case_count,
        if gate.passed { "通过" } else { "失败" }
    );
    for violation in &gate.violations {
        println!("- {violation}");
    }
    for claim in &gate.supported_release_claims {
        println!("- 已获评测支持：{claim}");
    }
}

fn usage() -> &'static str {
    "用法：\n  astock-eval evaluate --dataset <cases.json> --thresholds <thresholds.json> --baseline <report.json> --json <report.json> --html <report.html> [--split test] [--check]\n  astock-eval evaluate ... --establish-baseline（仅在评审新数据集版本时使用，且不能传旧基线）\n  astock-eval compare --from <old.json> --to <new.json> --json <diff.json>"
}
