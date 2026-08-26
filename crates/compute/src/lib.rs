//! A bounded, deterministic financial calculation language.
//!
//! `astock-compute` deliberately evaluates a typed JSON AST rather than
//! arbitrary source code. Programs can compose scalar/series arithmetic,
//! rolling indicators and reductions, but cannot access the filesystem,
//! network, clock, randomness, processes or foreign-function interfaces.
//! Every program is validated, fuel-metered and content-addressed before its
//! result is returned.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const LANGUAGE_VERSION: u8 = 1;
pub const MAX_INPUTS: usize = 32;
pub const MAX_SERIES_LEN: usize = 5_000;
pub const MAX_TOTAL_INPUT_POINTS: usize = 100_000;
pub const MAX_BINDINGS: usize = 64;
pub const MAX_OUTPUTS: usize = 32;
pub const MAX_AST_NODES: usize = 1_024;
pub const MAX_AST_DEPTH: usize = 32;
pub const MAX_FUEL: usize = 5_000_000;

/// One calculation program. Bindings are evaluated in declaration order and
/// may reference inputs or earlier bindings. Outputs may reference all
/// bindings. Names are ASCII identifiers and cannot be shadowed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub version: u8,
    #[serde(default)]
    pub inputs: BTreeMap<String, Vec<Option<f64>>>,
    #[serde(default)]
    pub bindings: Vec<Binding>,
    pub outputs: BTreeMap<String, Expr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub name: String,
    pub expr: Expr,
}

/// Closed expression set for the v1 language.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Expr {
    Scalar {
        value: f64,
    },
    Var {
        name: String,
    },
    Add {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Sub {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Mul {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Div {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Neg {
        input: Box<Expr>,
    },
    Abs {
        input: Box<Expr>,
    },
    Clip {
        input: Box<Expr>,
        min: f64,
        max: f64,
    },
    Lag {
        input: Box<Expr>,
        periods: usize,
    },
    Diff {
        input: Box<Expr>,
    },
    Returns {
        input: Box<Expr>,
    },
    LogReturns {
        input: Box<Expr>,
    },
    CumulativeReturn {
        input: Box<Expr>,
    },
    Sma {
        input: Box<Expr>,
        window: usize,
    },
    Ema {
        input: Box<Expr>,
        window: usize,
    },
    RollingStd {
        input: Box<Expr>,
        window: usize,
        #[serde(default)]
        annualization: Option<f64>,
    },
    Zscore {
        input: Box<Expr>,
        window: usize,
    },
    Rsi {
        input: Box<Expr>,
        window: usize,
    },
    Tail {
        input: Box<Expr>,
        count: usize,
    },
    Mean {
        input: Box<Expr>,
    },
    Std {
        input: Box<Expr>,
    },
    Sum {
        input: Box<Expr>,
    },
    Min {
        input: Box<Expr>,
    },
    Max {
        input: Box<Expr>,
    },
    Last {
        input: Box<Expr>,
    },
    Count {
        input: Box<Expr>,
    },
    Correlation {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    MaxDrawdown {
        input: Box<Expr>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComputedValue {
    Scalar { value: Option<f64> },
    Series { values: Vec<Option<f64>> },
}

impl ComputedValue {
    pub fn scalar(&self) -> Option<f64> {
        match self {
            Self::Scalar { value } => *value,
            Self::Series { .. } => None,
        }
    }

    pub fn series(&self) -> Option<&[Option<f64>]> {
        match self {
            Self::Scalar { .. } => None,
            Self::Series { values } => Some(values),
        }
    }

    fn cost(&self) -> usize {
        match self {
            Self::Scalar { .. } => 1,
            Self::Series { values } => values.len().max(1),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    pub language: String,
    pub language_version: u8,
    pub program_sha256: String,
    pub fuel_used: usize,
    pub outputs: BTreeMap<String, ComputedValue>,
    pub semantics: Vec<String>,
}

#[derive(Debug, Error, PartialEq)]
pub enum ComputeError {
    #[error("invalid calculation program: {0}")]
    Invalid(String),
    #[error("unknown calculation variable `{0}`")]
    UnknownVariable(String),
    #[error("calculation type error: {0}")]
    Type(String),
    #[error("calculation resource limit exceeded: {0}")]
    Limit(String),
    #[error("cannot fingerprint calculation program: {0}")]
    Fingerprint(String),
}

/// Validate and execute one program with the fixed v1 resource limits.
pub fn execute(program: &Program) -> Result<Execution, ComputeError> {
    validate(program)?;
    let fingerprint = fingerprint(program)?;
    let mut evaluator = Evaluator::new();
    for (name, values) in &program.inputs {
        evaluator.env.insert(
            name.clone(),
            ComputedValue::Series {
                values: values.clone(),
            },
        );
    }
    for binding in &program.bindings {
        let value = evaluator.eval(&binding.expr)?;
        evaluator.env.insert(binding.name.clone(), value);
    }
    let mut outputs = BTreeMap::new();
    for (name, expression) in &program.outputs {
        outputs.insert(name.clone(), evaluator.eval(expression)?);
    }
    Ok(Execution {
        language: "astock-finance-calc".into(),
        language_version: LANGUAGE_VERSION,
        program_sha256: fingerprint,
        fuel_used: evaluator.fuel.used,
        outputs,
        semantics: vec![
            "all series operations are oldest-to-newest".into(),
            "missing observations propagate through point operations".into(),
            "rolling windows require a complete finite window".into(),
            "std and correlation use population moments".into(),
            "division by zero and undefined numeric results become null".into(),
            "no filesystem, network, clock, random, process or arbitrary code access".into(),
        ],
    })
}

pub fn fingerprint(program: &Program) -> Result<String, ComputeError> {
    let bytes = serde_json::to_vec(program)
        .map_err(|error| ComputeError::Fingerprint(error.to_string()))?;
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(encoded)
}

pub fn validate(program: &Program) -> Result<(), ComputeError> {
    if program.version != LANGUAGE_VERSION {
        return Err(ComputeError::Invalid(format!(
            "version must be {LANGUAGE_VERSION}"
        )));
    }
    if program.inputs.len() > MAX_INPUTS {
        return Err(ComputeError::Limit(format!(
            "at most {MAX_INPUTS} input series are allowed"
        )));
    }
    if program.bindings.len() > MAX_BINDINGS {
        return Err(ComputeError::Limit(format!(
            "at most {MAX_BINDINGS} bindings are allowed"
        )));
    }
    if program.outputs.is_empty() || program.outputs.len() > MAX_OUTPUTS {
        return Err(ComputeError::Limit(format!(
            "outputs must contain 1-{MAX_OUTPUTS} values"
        )));
    }

    let mut names = BTreeSet::new();
    let mut total_points = 0usize;
    for (name, values) in &program.inputs {
        validate_name(name)?;
        if values.is_empty() || values.len() > MAX_SERIES_LEN {
            return Err(ComputeError::Limit(format!(
                "input `{name}` must contain 1-{MAX_SERIES_LEN} observations"
            )));
        }
        total_points = total_points.saturating_add(values.len());
        if total_points > MAX_TOTAL_INPUT_POINTS {
            return Err(ComputeError::Limit(format!(
                "inputs exceed {MAX_TOTAL_INPUT_POINTS} total observations"
            )));
        }
        if values
            .iter()
            .flatten()
            .any(|value| !value.is_finite() || value.abs() > 1e100)
        {
            return Err(ComputeError::Invalid(format!(
                "input `{name}` contains a non-finite or unreasonable value"
            )));
        }
        names.insert(name.clone());
    }
    for binding in &program.bindings {
        validate_name(&binding.name)?;
        if !names.insert(binding.name.clone()) {
            return Err(ComputeError::Invalid(format!(
                "variable `{}` is declared more than once",
                binding.name
            )));
        }
    }
    for name in program.outputs.keys() {
        validate_name(name)?;
    }

    let mut nodes = 0usize;
    for binding in &program.bindings {
        validate_expr(&binding.expr, 1, &mut nodes)?;
    }
    for expression in program.outputs.values() {
        validate_expr(expression, 1, &mut nodes)?;
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), ComputeError> {
    let mut bytes = name.bytes();
    let first = bytes
        .next()
        .ok_or_else(|| ComputeError::Invalid("calculation identifiers must not be empty".into()))?;
    if name.len() > 64
        || !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(ComputeError::Invalid(format!(
            "`{name}` is not a valid calculation identifier"
        )));
    }
    Ok(())
}

fn validate_expr(expr: &Expr, depth: usize, nodes: &mut usize) -> Result<(), ComputeError> {
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_AST_NODES {
        return Err(ComputeError::Limit(format!(
            "AST exceeds {MAX_AST_NODES} nodes"
        )));
    }
    if depth > MAX_AST_DEPTH {
        return Err(ComputeError::Limit(format!(
            "AST exceeds depth {MAX_AST_DEPTH}"
        )));
    }
    match expr {
        Expr::Scalar { value } => {
            if !value.is_finite() || value.abs() > 1e100 {
                return Err(ComputeError::Invalid(
                    "scalar must be finite and reasonably bounded".into(),
                ));
            }
        }
        Expr::Var { name } => validate_name(name)?,
        Expr::Add { left, right }
        | Expr::Sub { left, right }
        | Expr::Mul { left, right }
        | Expr::Div { left, right }
        | Expr::Correlation { left, right } => {
            validate_expr(left, depth + 1, nodes)?;
            validate_expr(right, depth + 1, nodes)?;
        }
        Expr::Neg { input }
        | Expr::Abs { input }
        | Expr::Diff { input }
        | Expr::Returns { input }
        | Expr::LogReturns { input }
        | Expr::CumulativeReturn { input }
        | Expr::Mean { input }
        | Expr::Std { input }
        | Expr::Sum { input }
        | Expr::Min { input }
        | Expr::Max { input }
        | Expr::Last { input }
        | Expr::Count { input }
        | Expr::MaxDrawdown { input } => validate_expr(input, depth + 1, nodes)?,
        Expr::Clip { input, min, max } => {
            if !min.is_finite() || !max.is_finite() || min > max {
                return Err(ComputeError::Invalid(
                    "clip bounds must be finite and ordered".into(),
                ));
            }
            validate_expr(input, depth + 1, nodes)?;
        }
        Expr::Lag { input, periods } => {
            if *periods > MAX_SERIES_LEN {
                return Err(ComputeError::Limit(format!(
                    "lag cannot exceed {MAX_SERIES_LEN} periods"
                )));
            }
            validate_expr(input, depth + 1, nodes)?;
        }
        Expr::Sma { input, window }
        | Expr::Ema { input, window }
        | Expr::Zscore { input, window }
        | Expr::Rsi { input, window } => {
            validate_window(*window)?;
            validate_expr(input, depth + 1, nodes)?;
        }
        Expr::RollingStd {
            input,
            window,
            annualization,
        } => {
            validate_window(*window)?;
            if annualization
                .is_some_and(|value| !value.is_finite() || value <= 0.0 || value > 100_000.0)
            {
                return Err(ComputeError::Invalid(
                    "annualization must be finite and between 0 and 100000".into(),
                ));
            }
            validate_expr(input, depth + 1, nodes)?;
        }
        Expr::Tail { input, count } => {
            if *count == 0 || *count > MAX_SERIES_LEN {
                return Err(ComputeError::Limit(format!(
                    "tail count must be between 1 and {MAX_SERIES_LEN}"
                )));
            }
            validate_expr(input, depth + 1, nodes)?;
        }
    }
    Ok(())
}

fn validate_window(window: usize) -> Result<(), ComputeError> {
    if !(1..=MAX_SERIES_LEN).contains(&window) {
        return Err(ComputeError::Limit(format!(
            "rolling window must be between 1 and {MAX_SERIES_LEN}"
        )));
    }
    Ok(())
}

struct Fuel {
    used: usize,
}

impl Fuel {
    fn consume(&mut self, amount: usize) -> Result<(), ComputeError> {
        self.used = self.used.saturating_add(amount.max(1));
        if self.used > MAX_FUEL {
            return Err(ComputeError::Limit(format!(
                "program consumed more than {MAX_FUEL} fuel units"
            )));
        }
        Ok(())
    }
}

struct Evaluator {
    env: BTreeMap<String, ComputedValue>,
    fuel: Fuel,
}

impl Evaluator {
    fn new() -> Self {
        Self {
            env: BTreeMap::new(),
            fuel: Fuel { used: 0 },
        }
    }

    fn eval(&mut self, expression: &Expr) -> Result<ComputedValue, ComputeError> {
        match expression {
            Expr::Scalar { value } => {
                self.fuel.consume(1)?;
                Ok(ComputedValue::Scalar {
                    value: Some(*value),
                })
            }
            Expr::Var { name } => {
                let value = self
                    .env
                    .get(name)
                    .cloned()
                    .ok_or_else(|| ComputeError::UnknownVariable(name.clone()))?;
                self.fuel.consume(value.cost())?;
                Ok(value)
            }
            Expr::Add { left, right } => self.binary(left, right, |a, b| finite(a + b)),
            Expr::Sub { left, right } => self.binary(left, right, |a, b| finite(a - b)),
            Expr::Mul { left, right } => self.binary(left, right, |a, b| finite(a * b)),
            Expr::Div { left, right } => {
                self.binary(
                    left,
                    right,
                    |a, b| {
                        if b == 0.0 {
                            None
                        } else {
                            finite(a / b)
                        }
                    },
                )
            }
            Expr::Neg { input } => self.unary(input, |value| finite(-value)),
            Expr::Abs { input } => self.unary(input, |value| finite(value.abs())),
            Expr::Clip { input, min, max } => {
                self.unary(input, |value| Some(value.clamp(*min, *max)))
            }
            Expr::Lag { input, periods } => {
                let values = self.eval_series(input, "lag")?;
                self.fuel.consume(values.len())?;
                let mut output = vec![None; values.len()];
                if *periods < values.len() {
                    output[*periods..].clone_from_slice(&values[..values.len() - periods]);
                }
                Ok(series(output))
            }
            Expr::Diff { input } => {
                let values = self.eval_series(input, "diff")?;
                self.fuel.consume(values.len())?;
                Ok(series(pair_change(&values, |current, previous| {
                    finite(current - previous)
                })))
            }
            Expr::Returns { input } => {
                let values = self.eval_series(input, "returns")?;
                self.fuel.consume(values.len())?;
                Ok(series(pair_change(&values, |current, previous| {
                    if previous == 0.0 {
                        None
                    } else {
                        finite(current / previous - 1.0)
                    }
                })))
            }
            Expr::LogReturns { input } => {
                let values = self.eval_series(input, "log_returns")?;
                self.fuel.consume(values.len())?;
                Ok(series(pair_change(&values, |current, previous| {
                    if current <= 0.0 || previous <= 0.0 {
                        None
                    } else {
                        finite((current / previous).ln())
                    }
                })))
            }
            Expr::CumulativeReturn { input } => {
                let values = self.eval_series(input, "cumulative_return")?;
                self.fuel.consume(values.len())?;
                let mut wealth = 1.0;
                let mut output = Vec::with_capacity(values.len());
                for value in values {
                    match value {
                        Some(value) if value > -1.0 => {
                            wealth *= 1.0 + value;
                            output.push(finite(wealth - 1.0));
                        }
                        _ => output.push(None),
                    }
                }
                Ok(series(output))
            }
            Expr::Sma { input, window } => {
                let values = self.eval_series(input, "sma")?;
                self.fuel.consume(values.len())?;
                Ok(series(rolling(&values, *window, |sum, _, window| {
                    finite(sum / window as f64)
                })))
            }
            Expr::Ema { input, window } => {
                let values = self.eval_series(input, "ema")?;
                self.fuel.consume(values.len())?;
                let alpha = 2.0 / (*window as f64 + 1.0);
                let mut current = None;
                let output = values
                    .into_iter()
                    .map(|value| match (current, value) {
                        (_, None) => None,
                        (None, Some(value)) => {
                            current = Some(value);
                            current
                        }
                        (Some(previous), Some(value)) => {
                            current = finite(alpha * value + (1.0 - alpha) * previous);
                            current
                        }
                    })
                    .collect();
                Ok(series(output))
            }
            Expr::RollingStd {
                input,
                window,
                annualization,
            } => {
                let values = self.eval_series(input, "rolling_std")?;
                self.fuel.consume(values.len())?;
                let multiplier = annualization.unwrap_or(1.0).sqrt();
                Ok(series(rolling(&values, *window, |sum, sum_sq, window| {
                    finite(variance(sum, sum_sq, window).sqrt() * multiplier)
                })))
            }
            Expr::Zscore { input, window } => {
                let values = self.eval_series(input, "zscore")?;
                self.fuel.consume(values.len())?;
                Ok(series(rolling_with_current(
                    &values,
                    *window,
                    |current, sum, sum_sq, window| {
                        let std = variance(sum, sum_sq, window).sqrt();
                        if std == 0.0 {
                            None
                        } else {
                            finite((current - sum / window as f64) / std)
                        }
                    },
                )))
            }
            Expr::Rsi { input, window } => {
                let values = self.eval_series(input, "rsi")?;
                self.fuel.consume(values.len())?;
                Ok(series(rsi(&values, *window)))
            }
            Expr::Tail { input, count } => {
                let values = self.eval_series(input, "tail")?;
                self.fuel.consume((*count).min(values.len()))?;
                let start = values.len().saturating_sub(*count);
                Ok(series(values[start..].to_vec()))
            }
            Expr::Mean { input } => self.reduce(input, "mean", |values| {
                if values.is_empty() {
                    None
                } else {
                    finite(values.iter().sum::<f64>() / values.len() as f64)
                }
            }),
            Expr::Std { input } => self.reduce(input, "std", |values| {
                if values.is_empty() {
                    None
                } else {
                    let sum = values.iter().sum::<f64>();
                    let sum_sq = values.iter().map(|value| value * value).sum::<f64>();
                    finite(variance(sum, sum_sq, values.len()).sqrt())
                }
            }),
            Expr::Sum { input } => self.reduce(input, "sum", |values| {
                (!values.is_empty()).then(|| values.iter().sum::<f64>())
            }),
            Expr::Min { input } => self.reduce(input, "min", |values| {
                values.iter().copied().reduce(f64::min)
            }),
            Expr::Max { input } => self.reduce(input, "max", |values| {
                values.iter().copied().reduce(f64::max)
            }),
            Expr::Last { input } => self.reduce(input, "last", |values| values.last().copied()),
            Expr::Count { input } => {
                self.reduce(input, "count", |values| Some(values.len() as f64))
            }
            Expr::Correlation { left, right } => {
                let left = self.eval_series(left, "correlation")?;
                let right = self.eval_series(right, "correlation")?;
                if left.len() != right.len() {
                    return Err(ComputeError::Type(
                        "correlation series must have equal lengths".into(),
                    ));
                }
                self.fuel.consume(left.len())?;
                let pairs = left
                    .iter()
                    .zip(&right)
                    .filter_map(|(left, right)| Some(((*left)?, (*right)?)))
                    .collect::<Vec<_>>();
                let value = correlation(&pairs);
                Ok(scalar(value))
            }
            Expr::MaxDrawdown { input } => {
                let values = self.eval_series(input, "max_drawdown")?;
                self.fuel.consume(values.len())?;
                let mut peak: Option<f64> = None;
                let mut worst = 0.0_f64;
                let mut observed = false;
                for value in values.into_iter().flatten() {
                    peak = Some(peak.map_or(value, |current| current.max(value)));
                    if let Some(peak) = peak.filter(|peak| *peak != 0.0) {
                        worst = worst.min(value / peak - 1.0);
                        observed = true;
                    }
                }
                Ok(scalar(observed.then_some(worst)))
            }
        }
    }

    fn eval_series(
        &mut self,
        expression: &Expr,
        operation: &str,
    ) -> Result<Vec<Option<f64>>, ComputeError> {
        match self.eval(expression)? {
            ComputedValue::Series { values } => Ok(values),
            ComputedValue::Scalar { .. } => {
                Err(ComputeError::Type(format!("{operation} requires a series")))
            }
        }
    }

    fn unary(
        &mut self,
        expression: &Expr,
        operation: impl Fn(f64) -> Option<f64>,
    ) -> Result<ComputedValue, ComputeError> {
        let value = self.eval(expression)?;
        self.fuel.consume(value.cost())?;
        Ok(match value {
            ComputedValue::Scalar { value } => scalar(value.and_then(operation)),
            ComputedValue::Series { values } => series(
                values
                    .into_iter()
                    .map(|value| value.and_then(&operation))
                    .collect(),
            ),
        })
    }

    fn binary(
        &mut self,
        left: &Expr,
        right: &Expr,
        operation: impl Fn(f64, f64) -> Option<f64>,
    ) -> Result<ComputedValue, ComputeError> {
        let left = self.eval(left)?;
        let right = self.eval(right)?;
        let cost = left.cost().max(right.cost());
        self.fuel.consume(cost)?;
        match (left, right) {
            (ComputedValue::Scalar { value: left }, ComputedValue::Scalar { value: right }) => {
                Ok(scalar(zip_number(left, right, &operation)))
            }
            (ComputedValue::Series { values }, ComputedValue::Scalar { value: right }) => {
                Ok(series(
                    values
                        .into_iter()
                        .map(|left| zip_number(left, right, &operation))
                        .collect(),
                ))
            }
            (ComputedValue::Scalar { value: left }, ComputedValue::Series { values }) => {
                Ok(series(
                    values
                        .into_iter()
                        .map(|right| zip_number(left, right, &operation))
                        .collect(),
                ))
            }
            (ComputedValue::Series { values: left }, ComputedValue::Series { values: right }) => {
                if left.len() != right.len() {
                    return Err(ComputeError::Type(
                        "binary series operands must have equal lengths".into(),
                    ));
                }
                Ok(series(
                    left.into_iter()
                        .zip(right)
                        .map(|(left, right)| zip_number(left, right, &operation))
                        .collect(),
                ))
            }
        }
    }

    fn reduce(
        &mut self,
        expression: &Expr,
        operation: &str,
        reducer: impl FnOnce(&[f64]) -> Option<f64>,
    ) -> Result<ComputedValue, ComputeError> {
        let values = self.eval_series(expression, operation)?;
        self.fuel.consume(values.len())?;
        let present = values.into_iter().flatten().collect::<Vec<_>>();
        Ok(scalar(reducer(&present).and_then(finite)))
    }
}

fn scalar(value: Option<f64>) -> ComputedValue {
    ComputedValue::Scalar { value }
}

fn series(values: Vec<Option<f64>>) -> ComputedValue {
    ComputedValue::Series { values }
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn zip_number(
    left: Option<f64>,
    right: Option<f64>,
    operation: &impl Fn(f64, f64) -> Option<f64>,
) -> Option<f64> {
    operation(left?, right?)
}

fn pair_change(
    values: &[Option<f64>],
    operation: impl Fn(f64, f64) -> Option<f64>,
) -> Vec<Option<f64>> {
    let mut output = Vec::with_capacity(values.len());
    if values.is_empty() {
        return output;
    }
    output.push(None);
    output.extend(values.windows(2).map(|pair| {
        let previous = pair[0]?;
        let current = pair[1]?;
        operation(current, previous)
    }));
    output
}

fn rolling(
    values: &[Option<f64>],
    window: usize,
    operation: impl Fn(f64, f64, usize) -> Option<f64>,
) -> Vec<Option<f64>> {
    rolling_with_current(values, window, |_, sum, sum_sq, window| {
        operation(sum, sum_sq, window)
    })
}

fn rolling_with_current(
    values: &[Option<f64>],
    window: usize,
    operation: impl Fn(f64, f64, f64, usize) -> Option<f64>,
) -> Vec<Option<f64>> {
    let mut output = Vec::with_capacity(values.len());
    let (mut sum, mut sum_sq, mut missing) = (0.0, 0.0, 0usize);
    for (index, value) in values.iter().enumerate() {
        match value {
            Some(value) => {
                sum += value;
                sum_sq += value * value;
            }
            None => missing += 1,
        }
        if index >= window {
            match values[index - window] {
                Some(value) => {
                    sum -= value;
                    sum_sq -= value * value;
                }
                None => missing -= 1,
            }
        }
        if index + 1 >= window && missing == 0 {
            output.push(value.and_then(|current| operation(current, sum, sum_sq, window)));
        } else {
            output.push(None);
        }
    }
    output
}

fn variance(sum: f64, sum_sq: f64, count: usize) -> f64 {
    let count = count as f64;
    (sum_sq / count - (sum / count).powi(2)).max(0.0)
}

fn rsi(values: &[Option<f64>], window: usize) -> Vec<Option<f64>> {
    let mut output = vec![None; values.len()];
    if values.len() <= window {
        return output;
    }
    let mut gains = vec![None; values.len()];
    let mut losses = vec![None; values.len()];
    for index in 1..values.len() {
        if let (Some(previous), Some(current)) = (values[index - 1], values[index]) {
            let change = current - previous;
            gains[index] = Some(change.max(0.0));
            losses[index] = Some((-change).max(0.0));
        }
    }
    let rolling_gains = rolling(&gains[1..], window, |sum, _, window| {
        Some(sum / window as f64)
    });
    let rolling_losses = rolling(&losses[1..], window, |sum, _, window| {
        Some(sum / window as f64)
    });
    for index in window..values.len() {
        output[index] = match (rolling_gains[index - 1], rolling_losses[index - 1]) {
            (Some(gain), Some(0.0)) => Some(if gain == 0.0 { 50.0 } else { 100.0 }),
            (Some(gain), Some(loss)) => Some(100.0 - 100.0 / (1.0 + gain / loss)),
            _ => None,
        };
    }
    output
}

fn correlation(pairs: &[(f64, f64)]) -> Option<f64> {
    if pairs.len() < 2 {
        return None;
    }
    let count = pairs.len() as f64;
    let mean_left = pairs.iter().map(|pair| pair.0).sum::<f64>() / count;
    let mean_right = pairs.iter().map(|pair| pair.1).sum::<f64>() / count;
    let covariance = pairs
        .iter()
        .map(|pair| (pair.0 - mean_left) * (pair.1 - mean_right))
        .sum::<f64>()
        / count;
    let variance_left = pairs
        .iter()
        .map(|pair| (pair.0 - mean_left).powi(2))
        .sum::<f64>()
        / count;
    let variance_right = pairs
        .iter()
        .map(|pair| (pair.1 - mean_right).powi(2))
        .sum::<f64>()
        / count;
    let denominator = (variance_left * variance_right).sqrt();
    (denominator != 0.0)
        .then(|| covariance / denominator)
        .and_then(finite)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(name: &str) -> Box<Expr> {
        Box::new(Expr::Var { name: name.into() })
    }

    fn last(input: Expr) -> Expr {
        Expr::Last {
            input: Box::new(input),
        }
    }

    #[test]
    fn composed_returns_rolling_and_reduction_are_deterministic() {
        let program = Program {
            version: 1,
            inputs: BTreeMap::from([("close".into(), vec![Some(100.0), Some(110.0), Some(99.0)])]),
            bindings: vec![
                Binding {
                    name: "ret".into(),
                    expr: Expr::Returns {
                        input: var("close"),
                    },
                },
                Binding {
                    name: "cum".into(),
                    expr: Expr::CumulativeReturn { input: var("ret") },
                },
            ],
            outputs: BTreeMap::from([
                (
                    "total_return".into(),
                    last(Expr::Var { name: "cum".into() }),
                ),
                (
                    "sma2".into(),
                    Expr::Sma {
                        input: var("close"),
                        window: 2,
                    },
                ),
            ]),
        };
        let first = execute(&program).unwrap();
        let second = execute(&program).unwrap();
        assert_eq!(first, second);
        let total = first.outputs["total_return"].scalar().unwrap();
        assert!((total + 0.01).abs() < 1e-12);
        assert_eq!(
            first.outputs["sma2"].series().unwrap(),
            &[None, Some(105.0), Some(104.5)]
        );
        assert_eq!(first.program_sha256.len(), 64);
    }

    #[test]
    fn correlation_drawdown_and_missing_values_have_explicit_semantics() {
        let program = Program {
            version: 1,
            inputs: BTreeMap::from([
                (
                    "equity".into(),
                    vec![Some(100.0), Some(120.0), None, Some(90.0), Some(108.0)],
                ),
                (
                    "factor".into(),
                    vec![Some(1.0), Some(2.0), None, Some(3.0), Some(4.0)],
                ),
            ]),
            bindings: vec![],
            outputs: BTreeMap::from([
                (
                    "drawdown".into(),
                    Expr::MaxDrawdown {
                        input: var("equity"),
                    },
                ),
                (
                    "corr".into(),
                    Expr::Correlation {
                        left: var("equity"),
                        right: var("factor"),
                    },
                ),
            ]),
        };
        let result = execute(&program).unwrap();
        assert!((result.outputs["drawdown"].scalar().unwrap() + 0.25).abs() < 1e-12);
        assert!(result.outputs["corr"].scalar().unwrap().is_finite());
    }

    #[test]
    fn unknown_variables_and_unbounded_programs_fail_closed() {
        let unknown = Program {
            version: 1,
            inputs: BTreeMap::new(),
            bindings: vec![],
            outputs: BTreeMap::from([(
                "answer".into(),
                Expr::Var {
                    name: "missing".into(),
                },
            )]),
        };
        assert_eq!(
            execute(&unknown),
            Err(ComputeError::UnknownVariable("missing".into()))
        );

        let oversized = Program {
            version: 1,
            inputs: BTreeMap::from([("x".into(), vec![Some(1.0); MAX_SERIES_LEN + 1])]),
            bindings: vec![],
            outputs: BTreeMap::from([("x".into(), Expr::Var { name: "x".into() })]),
        };
        assert!(matches!(validate(&oversized), Err(ComputeError::Limit(_))));
    }

    #[test]
    fn division_by_zero_is_null_and_unknown_json_fields_are_rejected() {
        let program: Program = serde_json::from_value(serde_json::json!({
            "version": 1,
            "outputs": {
                "answer": {
                    "op": "div",
                    "left": {"op": "scalar", "value": 1.0},
                    "right": {"op": "scalar", "value": 0.0}
                }
            }
        }))
        .unwrap();
        assert_eq!(execute(&program).unwrap().outputs["answer"].scalar(), None);

        assert!(serde_json::from_value::<Program>(serde_json::json!({
            "version": 1,
            "outputs": {"answer": {"op": "scalar", "value": 1.0, "code": "rm"}}
        }))
        .is_err());
    }
}
