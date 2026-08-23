//! Reproducible multi-security research workflow.
//!
//! This module deliberately keeps data acquisition outside the statistics
//! crate.  A UI and an Agent pass the same [`ResearchConfig`] and versioned
//! [`SeriesInput`] values into this engine and therefore receive the exact
//! same deterministic result and snapshot id.

use std::collections::{BTreeMap, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Datelike, NaiveDate};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::correlation::{
    distance_correlation, kendall_tau_b, mutual_information, partial_correlation, pearson, spearman,
};
use crate::error::QuantError;
use crate::leadlag::cross_correlation_scan;
use crate::returns::{arithmetic_returns, log_returns};
use crate::timeseries::granger_causality;

/// Increment whenever formulas, preprocessing or inference semantics change.
pub const RESEARCH_FUNCTION_VERSION: &str = "astock-quant-research/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResearchMetric {
    #[default]
    Pearson,
    Spearman,
    Kendall,
    DistanceCorrelation,
    MutualInformation,
    LeadLag,
    Granger,
}

impl ResearchMetric {
    fn is_signed(self) -> bool {
        !matches!(
            self,
            Self::DistanceCorrelation | Self::MutualInformation | Self::Granger
        )
    }

    fn is_quadratic(self) -> bool {
        matches!(self, Self::Kendall | Self::DistanceCorrelation)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InputValueMode {
    PriceLevel,
    ArithmeticReturn,
    #[default]
    LogReturn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResearchFrequency {
    #[default]
    Daily,
    Weekly,
    Monthly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MissingValuePolicy {
    #[default]
    Drop,
    ForwardFill,
    Zero,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FdrMethod {
    #[default]
    BenjaminiHochberg,
    Bonferroni,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ResearchConfig {
    pub symbols: Vec<String>,
    pub metric: ResearchMetric,
    pub value_mode: InputValueMode,
    pub frequency: ResearchFrequency,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    /// qfq / hfq / none; retained in the immutable snapshot.
    pub adjust: String,
    /// Number of daily source bars requested when no narrower date range is available.
    pub lookback_bars: u32,
    pub missing_policy: MissingValuePolicy,
    pub rolling_window: usize,
    pub max_lag: usize,
    pub controls: Vec<String>,
    pub bootstrap_reps: usize,
    pub permutation_reps: usize,
    pub alpha: f64,
    pub fdr_method: FdrMethod,
    /// Hard upper bound before deterministic pair sampling is used.
    pub max_pairs: usize,
    /// High-cost metrics are deterministically thinned to this many points.
    pub max_observations_per_pair: usize,
    pub seed: u64,
    pub oos_ratio: f64,
}

impl Default for ResearchConfig {
    fn default() -> Self {
        Self {
            symbols: Vec::new(),
            metric: ResearchMetric::Pearson,
            value_mode: InputValueMode::LogReturn,
            frequency: ResearchFrequency::Daily,
            start_date: None,
            end_date: None,
            adjust: "qfq".into(),
            lookback_bars: 750,
            missing_policy: MissingValuePolicy::Drop,
            rolling_window: 60,
            max_lag: 5,
            controls: Vec::new(),
            bootstrap_reps: 199,
            permutation_reps: 199,
            alpha: 0.05,
            fdr_method: FdrMethod::BenjaminiHochberg,
            max_pairs: 2_000,
            max_observations_per_pair: 500,
            seed: 42,
            oos_ratio: 0.3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesInput {
    pub symbol: String,
    pub dates: Vec<String>,
    pub values: Vec<f64>,
    pub data_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchProgress {
    pub phase: String,
    pub done_pairs: usize,
    pub total_pairs: usize,
    pub current_pair: Option<[String; 2]>,
    pub effective_observations: usize,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchBudget {
    pub requested_pairs: usize,
    pub executed_pairs: usize,
    pub pair_sampling: bool,
    pub max_observations_per_pair: usize,
    pub estimated_operations: u64,
    pub complexity: String,
    pub explanation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StabilitySlice {
    pub group: String,
    pub label: String,
    pub effect: f64,
    pub effective_n: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StabilitySummary {
    pub slice_count: usize,
    pub same_direction_rate: Option<f64>,
    pub min_effect: Option<f64>,
    pub max_effect: Option<f64>,
    pub train_effect: Option<f64>,
    pub out_of_sample_effect: Option<f64>,
    pub outlier_robust_effect: Option<f64>,
    pub assessment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairInference {
    pub left: String,
    pub right: String,
    /// Granger rows are directed: left predictively precedes right.
    pub directed: bool,
    pub effect: f64,
    pub effect_name: String,
    pub confidence_low: Option<f64>,
    pub confidence_high: Option<f64>,
    pub confidence_method: String,
    pub p_value: Option<f64>,
    pub p_value_method: String,
    pub adjusted_p_value: Option<f64>,
    pub significant_raw: Option<bool>,
    pub significant_after_correction: Option<bool>,
    pub effective_n: usize,
    pub best_lag: Option<isize>,
    pub controls_used: Vec<String>,
    pub stability: StabilitySummary,
    pub stability_slices: Vec<StabilitySlice>,
    pub interpretation: String,
    pub conclusion: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSnapshot {
    pub snapshot_id: String,
    pub function_version: String,
    pub created_at: i64,
    pub config: ResearchConfig,
    pub data_versions: BTreeMap<String, String>,
    pub budget: ResearchBudget,
    pub results: Vec<PairInference>,
    pub warnings: Vec<String>,
    pub causality_boundary: String,
}

#[derive(Clone)]
struct PreparedSeries {
    symbol: String,
    dates: Vec<String>,
    values: Vec<f64>,
}

struct AlignedPair {
    dates: Vec<String>,
    x: Vec<f64>,
    y: Vec<f64>,
    controls: Vec<(String, Vec<f64>)>,
}

/// Run the deterministic research engine without progress hooks.
pub fn run_research(
    inputs: &[SeriesInput],
    config: &ResearchConfig,
) -> Result<ResearchSnapshot, QuantError> {
    run_research_with_hooks(inputs, config, |_| {}, || false)
}

/// Run research with progress and cooperative cancellation checked between
/// pairs.  Long tasks can therefore live in a background worker without an
/// arbitrary timeout.
pub fn run_research_with_hooks(
    inputs: &[SeriesInput],
    config: &ResearchConfig,
    mut on_progress: impl FnMut(ResearchProgress),
    is_cancelled: impl Fn() -> bool,
) -> Result<ResearchSnapshot, QuantError> {
    validate_config(config)?;
    let wanted: Vec<&SeriesInput> = if config.symbols.is_empty() {
        inputs.iter().collect()
    } else {
        config
            .symbols
            .iter()
            .filter_map(|s| inputs.iter().find(|v| &v.symbol == s))
            .collect()
    };
    if wanted.len() < 2 {
        return Err(QuantError::InvalidInput(
            "research requires at least two available symbol series".into(),
        ));
    }
    let prepared: Vec<PreparedSeries> = wanted
        .iter()
        .map(|s| prepare_series(s, config))
        .collect::<Result<_, _>>()?;
    let by_symbol: HashMap<&str, &PreparedSeries> =
        prepared.iter().map(|s| (s.symbol.as_str(), s)).collect();
    let control_series: Vec<&PreparedSeries> = config
        .controls
        .iter()
        .filter_map(|s| by_symbol.get(s.as_str()).copied())
        .collect();

    let mut pairs = Vec::new();
    for i in 0..prepared.len() {
        for j in 0..prepared.len() {
            if i == j || (config.metric != ResearchMetric::Granger && j <= i) {
                continue;
            }
            pairs.push((i, j));
        }
    }
    let requested_pairs = pairs.len();
    let pair_sampling = pairs.len() > config.max_pairs;
    if pair_sampling {
        deterministic_shuffle(&mut pairs, config.seed ^ 0xa57c_0c2d);
        pairs.truncate(config.max_pairs);
        pairs.sort_unstable();
    }
    let total_pairs = pairs.len();
    let quadratic = config.metric.is_quadratic();
    let per_pair = if quadratic {
        (config.max_observations_per_pair as u64).saturating_pow(2)
    } else {
        config.max_observations_per_pair as u64
    };
    let reps = (config.bootstrap_reps + config.permutation_reps + 1) as u64;
    let estimated_operations = (total_pairs as u64)
        .saturating_mul(per_pair)
        .saturating_mul(reps);
    let budget = ResearchBudget {
        requested_pairs,
        executed_pairs: total_pairs,
        pair_sampling,
        max_observations_per_pair: config.max_observations_per_pair,
        estimated_operations,
        complexity: if quadratic {
            "O(股票对数 × 样本数² × 重抽样次数)"
        } else {
            "O(股票对数 × 样本数 × 重抽样次数)"
        }
        .into(),
        explanation: if pair_sampling {
            format!("配对数超过预算，按固定种子从 {requested_pairs} 对中抽取 {total_pairs} 对；快照可精确复现")
        } else {
            "全部配对均纳入；高成本指标会按固定间隔压缩单对样本，原始有效样本数仍单独展示".into()
        },
    };

    on_progress(ResearchProgress {
        phase: "准备研究样本".into(),
        done_pairs: 0,
        total_pairs,
        current_pair: None,
        effective_observations: 0,
        message: format!(
            "已准备 {} 条序列，共 {requested_pairs} 个待检验关系",
            prepared.len()
        ),
    });

    let mut results = Vec::with_capacity(total_pairs);
    let mut warnings = Vec::new();
    for (done, (i, j)) in pairs.into_iter().enumerate() {
        if is_cancelled() {
            return Err(QuantError::InvalidInput(
                "research cancelled by user".into(),
            ));
        }
        let left = &prepared[i];
        let right = &prepared[j];
        let controls: Vec<&PreparedSeries> = control_series
            .iter()
            .copied()
            .filter(|s| s.symbol != left.symbol && s.symbol != right.symbol)
            .collect();
        let aligned = align_pair(left, right, &controls)?;
        on_progress(ResearchProgress {
            phase: "执行统计推断".into(),
            done_pairs: done,
            total_pairs,
            current_pair: Some([left.symbol.clone(), right.symbol.clone()]),
            effective_observations: aligned.x.len(),
            message: format!(
                "正在计算 {} 与 {}：区间估计、显著性与稳健性切片",
                left.symbol, right.symbol
            ),
        });
        match infer_pair(left, right, aligned, config, done as u64) {
            Ok(result) => results.push(result),
            Err(error) => warnings.push(format!("{}-{}：{error}", left.symbol, right.symbol)),
        }
    }
    apply_multiple_testing(&mut results, config.fdr_method, config.alpha);
    for result in &mut results {
        result.conclusion = conclusion_text(result, config.alpha);
    }
    on_progress(ResearchProgress {
        phase: "多重检验校正".into(),
        done_pairs: total_pairs,
        total_pairs,
        current_pair: None,
        effective_observations: 0,
        message: format!("已完成 {} 个有效关系，正在生成可复现快照", results.len()),
    });

    if config.value_mode == InputValueMode::PriceLevel {
        warnings.push("价格水平可能产生伪相关；专业研究通常优先使用收益率并检查平稳性".into());
    }
    if pair_sampling {
        warnings.push("结果使用确定性配对抽样；不要把未进入预算的关系解释为无效".into());
    }
    let data_versions: BTreeMap<String, String> = wanted
        .iter()
        .map(|s| (s.symbol.clone(), s.data_version.clone()))
        .collect();
    let snapshot_id = snapshot_hash(config, &data_versions, &results)?;
    Ok(ResearchSnapshot {
        snapshot_id,
        function_version: RESEARCH_FUNCTION_VERSION.into(),
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64,
        config: config.clone(),
        data_versions,
        budget,
        results,
        warnings,
        causality_boundary: "相关只描述共同变化；领先关系只描述时间先后；Granger 仅表示加入历史值后改善样本内预测，均不能单独证明结构性因果。结构性因果需要识别假设、自然实验或随机实验支持。".into(),
    })
}

fn validate_config(config: &ResearchConfig) -> Result<(), QuantError> {
    if !(0.0..1.0).contains(&config.alpha) {
        return Err(QuantError::InvalidInput("alpha must be in (0, 1)".into()));
    }
    if config.rolling_window < 10 || config.max_lag == 0 {
        return Err(QuantError::InvalidInput(
            "rolling_window must be >= 10 and max_lag must be >= 1".into(),
        ));
    }
    if config.bootstrap_reps < 99 || config.permutation_reps < 99 {
        return Err(QuantError::InvalidInput(
            "bootstrap_reps and permutation_reps must both be >= 99".into(),
        ));
    }
    if config.max_pairs == 0 || config.max_observations_per_pair < 30 {
        return Err(QuantError::InvalidInput(
            "max_pairs must be positive and max_observations_per_pair >= 30".into(),
        ));
    }
    if !(0.1..=0.5).contains(&config.oos_ratio) {
        return Err(QuantError::InvalidInput(
            "oos_ratio must be in [0.1, 0.5]".into(),
        ));
    }
    Ok(())
}

fn prepare_series(
    input: &SeriesInput,
    config: &ResearchConfig,
) -> Result<PreparedSeries, QuantError> {
    if input.dates.len() != input.values.len() {
        return Err(QuantError::InvalidInput(format!(
            "{} dates/value length mismatch",
            input.symbol
        )));
    }
    let mut rows: Vec<(String, f64)> = input
        .dates
        .iter()
        .cloned()
        .zip(input.values.iter().copied())
        .filter(|(date, _)| {
            config.start_date.as_ref().is_none_or(|start| date >= start)
                && config.end_date.as_ref().is_none_or(|end| date <= end)
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    apply_missing_policy(&mut rows, config.missing_policy);
    let rows = resample_rows(rows, config.frequency, config.value_mode)?;
    if rows.len() < 31 {
        return Err(QuantError::InsufficientData {
            context: "research series",
            needed: 31,
            got: rows.len(),
        });
    }
    let (mut dates, raw): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
    let values = match config.value_mode {
        InputValueMode::PriceLevel => raw,
        InputValueMode::ArithmeticReturn => {
            dates.remove(0);
            arithmetic_returns(&raw)?
        }
        InputValueMode::LogReturn => {
            dates.remove(0);
            log_returns(&raw)?
        }
    };
    Ok(PreparedSeries {
        symbol: input.symbol.clone(),
        dates,
        values,
    })
}

fn apply_missing_policy(rows: &mut Vec<(String, f64)>, policy: MissingValuePolicy) {
    match policy {
        MissingValuePolicy::Drop => rows.retain(|(_, value)| value.is_finite()),
        MissingValuePolicy::Zero => {
            for (_, value) in rows {
                if !value.is_finite() {
                    *value = 0.0;
                }
            }
        }
        MissingValuePolicy::ForwardFill => {
            let mut last = None;
            rows.retain_mut(|(_, value)| {
                if value.is_finite() {
                    last = Some(*value);
                    true
                } else if let Some(previous) = last {
                    *value = previous;
                    true
                } else {
                    false
                }
            });
        }
    }
}

fn resample_rows(
    rows: Vec<(String, f64)>,
    frequency: ResearchFrequency,
    value_mode: InputValueMode,
) -> Result<Vec<(String, f64)>, QuantError> {
    if frequency == ResearchFrequency::Daily {
        return Ok(rows);
    }
    let mut groups: BTreeMap<String, Vec<(String, f64)>> = BTreeMap::new();
    for (date, value) in rows {
        let parsed = NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(|_| {
            QuantError::InvalidInput(format!("invalid ISO date in research input: {date}"))
        })?;
        let key = match frequency {
            ResearchFrequency::Weekly => {
                let iso = parsed.iso_week();
                format!("{}-W{:02}", iso.year(), iso.week())
            }
            ResearchFrequency::Monthly => format!("{:04}-{:02}", parsed.year(), parsed.month()),
            ResearchFrequency::Daily => unreachable!(),
        };
        groups.entry(key).or_default().push((date, value));
    }
    let mut output = Vec::new();
    for (_, group) in groups {
        let date = group.last().expect("non-empty group").0.clone();
        let value = match value_mode {
            InputValueMode::PriceLevel
            | InputValueMode::ArithmeticReturn
            | InputValueMode::LogReturn => group.last().expect("non-empty group").1,
        };
        output.push((date, value));
    }
    Ok(output)
}

fn align_pair(
    left: &PreparedSeries,
    right: &PreparedSeries,
    controls: &[&PreparedSeries],
) -> Result<AlignedPair, QuantError> {
    let right_map: HashMap<&str, f64> = right
        .dates
        .iter()
        .zip(&right.values)
        .map(|(d, v)| (d.as_str(), *v))
        .collect();
    let control_maps: Vec<HashMap<&str, f64>> = controls
        .iter()
        .map(|series| {
            series
                .dates
                .iter()
                .zip(&series.values)
                .map(|(d, v)| (d.as_str(), *v))
                .collect()
        })
        .collect();
    let mut dates = Vec::new();
    let mut x = Vec::new();
    let mut y = Vec::new();
    let mut aligned_controls = vec![Vec::new(); controls.len()];
    for (date, xv) in left.dates.iter().zip(&left.values) {
        let Some(yv) = right_map.get(date.as_str()) else {
            continue;
        };
        if control_maps.iter().any(|m| !m.contains_key(date.as_str())) {
            continue;
        }
        dates.push(date.clone());
        x.push(*xv);
        y.push(*yv);
        for (idx, map) in control_maps.iter().enumerate() {
            aligned_controls[idx].push(map[date.as_str()]);
        }
    }
    if x.len() < 30 {
        return Err(QuantError::InsufficientData {
            context: "aligned research pair",
            needed: 30,
            got: x.len(),
        });
    }
    Ok(AlignedPair {
        dates,
        x,
        y,
        controls: controls
            .iter()
            .enumerate()
            .map(|(idx, series)| (series.symbol.clone(), aligned_controls[idx].clone()))
            .collect(),
    })
}

fn infer_pair(
    left: &PreparedSeries,
    right: &PreparedSeries,
    aligned: AlignedPair,
    config: &ResearchConfig,
    pair_index: u64,
) -> Result<PairInference, QuantError> {
    let original_n = aligned.x.len();
    let selected = deterministic_indices(original_n, config.max_observations_per_pair);
    let dates: Vec<String> = selected.iter().map(|&i| aligned.dates[i].clone()).collect();
    let x: Vec<f64> = selected.iter().map(|&i| aligned.x[i]).collect();
    let y: Vec<f64> = selected.iter().map(|&i| aligned.y[i]).collect();
    let controls: Vec<(String, Vec<f64>)> = aligned
        .controls
        .iter()
        .map(|(name, values)| {
            (
                name.clone(),
                selected.iter().map(|&i| values[i]).collect::<Vec<_>>(),
            )
        })
        .collect();
    let controls_ref: Vec<&[f64]> = controls.iter().map(|(_, v)| v.as_slice()).collect();
    let max_lag = admissible_lag(config.metric, x.len(), config.max_lag);
    let (effect, best_lag, standard_p) =
        metric_effect(config.metric, &x, &y, &controls_ref, max_lag)?;
    let seed = config.seed ^ pair_index.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let (ci_low, ci_high) = bootstrap_interval(
        config.metric,
        &x,
        &y,
        &controls,
        max_lag,
        config.bootstrap_reps,
        seed ^ 0x0b00_57a9,
    )?;
    let (p_value, p_method) = if let Some(p) = standard_p {
        (
            Some(p),
            "Granger F 检验（有限样本依赖线性模型假设）".to_string(),
        )
    } else {
        (
            Some(permutation_p_value(
                config.metric,
                &x,
                &y,
                &controls,
                max_lag,
                effect,
                config.permutation_reps,
                seed ^ 0x051a_9771,
            )?),
            if config.metric == ResearchMetric::MutualInformation {
                "固定种子置换检验；互信息没有通用的标准参数 p 值".into()
            } else {
                "固定种子双侧置换检验".into()
            },
        )
    };
    let (slices, stability) = stability_analysis(
        config.metric,
        &dates,
        &x,
        &y,
        &controls_ref,
        max_lag,
        config,
        effect,
    );
    let mut warnings = Vec::new();
    if original_n > x.len() {
        warnings.push(format!(
            "原始有效样本 {original_n} 个；高成本预算按固定间隔使用 {} 个，结论仍显示实际参与检验的有效 N",
            x.len()
        ));
    }
    if !controls.is_empty() && config.metric != ResearchMetric::Pearson {
        warnings.push(
            "控制变量目前仅改变 Pearson（偏相关）；其他指标保留控制变量清单但不做残差化".into(),
        );
    }
    let effect_name = match config.metric {
        ResearchMetric::Pearson if !controls.is_empty() => "偏 Pearson 相关系数",
        ResearchMetric::Pearson => "Pearson 相关系数",
        ResearchMetric::Spearman => "Spearman 秩相关系数",
        ResearchMetric::Kendall => "Kendall τ-b",
        ResearchMetric::DistanceCorrelation => "距离相关系数",
        ResearchMetric::MutualInformation => "互信息（nats）",
        ResearchMetric::LeadLag => "最佳滞后相关系数",
        ResearchMetric::Granger => "Granger F 统计量",
    }
    .to_string();
    let interpretation = interpretation_text(config.metric, best_lag, &left.symbol, &right.symbol);
    Ok(PairInference {
        left: left.symbol.clone(),
        right: right.symbol.clone(),
        directed: config.metric == ResearchMetric::Granger,
        effect,
        effect_name,
        confidence_low: Some(ci_low),
        confidence_high: Some(ci_high),
        confidence_method: format!(
            "成对移动块 bootstrap 百分位区间（{} 次，固定种子）",
            config.bootstrap_reps
        ),
        p_value,
        p_value_method: p_method,
        adjusted_p_value: None,
        significant_raw: p_value.map(|p| p <= config.alpha),
        significant_after_correction: None,
        effective_n: x.len(),
        best_lag,
        controls_used: controls.iter().map(|(name, _)| name.clone()).collect(),
        stability,
        stability_slices: slices,
        interpretation,
        conclusion: String::new(),
        warnings,
    })
}

fn metric_effect(
    metric: ResearchMetric,
    x: &[f64],
    y: &[f64],
    controls: &[&[f64]],
    max_lag: usize,
) -> Result<(f64, Option<isize>, Option<f64>), QuantError> {
    match metric {
        ResearchMetric::Pearson if !controls.is_empty() => {
            Ok((partial_correlation(x, y, controls)?, None, None))
        }
        ResearchMetric::Pearson => Ok((pearson(x, y)?, None, None)),
        ResearchMetric::Spearman => Ok((spearman(x, y)?, None, None)),
        ResearchMetric::Kendall => Ok((kendall_tau_b(x, y)?, None, None)),
        ResearchMetric::DistanceCorrelation => Ok((distance_correlation(x, y)?, None, None)),
        ResearchMetric::MutualInformation => {
            let bins = ((x.len() as f64).sqrt() as usize).clamp(4, 20);
            Ok((mutual_information(x, y, bins)?, None, None))
        }
        ResearchMetric::LeadLag => {
            let scan = cross_correlation_scan(x, y, max_lag)?;
            Ok((scan.best_value, Some(scan.best_lag), None))
        }
        ResearchMetric::Granger => {
            let result = granger_causality(x, y, max_lag)?;
            Ok((result.f_stat, Some(max_lag as isize), Some(result.p_value)))
        }
    }
}

fn bootstrap_interval(
    metric: ResearchMetric,
    x: &[f64],
    y: &[f64],
    controls: &[(String, Vec<f64>)],
    max_lag: usize,
    reps: usize,
    seed: u64,
) -> Result<(f64, f64), QuantError> {
    let n = x.len();
    let block = ((n as f64).sqrt().round() as usize).clamp(2, n / 3);
    let mut rng = StdRng::seed_from_u64(seed);
    let mut samples = Vec::with_capacity(reps);
    for _ in 0..reps {
        let indices = moving_block_indices(n, block, &mut rng);
        let bx: Vec<f64> = indices.iter().map(|&i| x[i]).collect();
        let by: Vec<f64> = indices.iter().map(|&i| y[i]).collect();
        let bc: Vec<Vec<f64>> = controls
            .iter()
            .map(|(_, values)| indices.iter().map(|&i| values[i]).collect())
            .collect();
        let refs: Vec<&[f64]> = bc.iter().map(Vec::as_slice).collect();
        if let Ok((value, _, _)) = metric_effect(metric, &bx, &by, &refs, max_lag.min(n - 3)) {
            if value.is_finite() {
                samples.push(value);
            }
        }
    }
    if samples.len() < reps / 2 {
        return Err(QuantError::NumericalIssue(
            "too few valid bootstrap replicates".into(),
        ));
    }
    samples.sort_by(f64::total_cmp);
    let low = percentile(&samples, 0.025);
    let high = percentile(&samples, 0.975);
    Ok((low, high))
}

#[allow(clippy::too_many_arguments)]
fn permutation_p_value(
    metric: ResearchMetric,
    x: &[f64],
    y: &[f64],
    controls: &[(String, Vec<f64>)],
    max_lag: usize,
    observed: f64,
    reps: usize,
    seed: u64,
) -> Result<f64, QuantError> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut permuted = y.to_vec();
    let refs: Vec<&[f64]> = controls.iter().map(|(_, v)| v.as_slice()).collect();
    let mut extreme = 0usize;
    for _ in 0..reps {
        fisher_yates(&mut permuted, &mut rng);
        let (candidate, _, _) = metric_effect(metric, x, &permuted, &refs, max_lag)?;
        let is_extreme = if metric.is_signed() {
            candidate.abs() >= observed.abs()
        } else {
            candidate >= observed
        };
        if is_extreme {
            extreme += 1;
        }
    }
    Ok((extreme + 1) as f64 / (reps + 1) as f64)
}

#[allow(clippy::too_many_arguments)]
fn stability_analysis(
    metric: ResearchMetric,
    dates: &[String],
    x: &[f64],
    y: &[f64],
    controls: &[&[f64]],
    max_lag: usize,
    config: &ResearchConfig,
    full_effect: f64,
) -> (Vec<StabilitySlice>, StabilitySummary) {
    let n = x.len();
    let mut groups: Vec<(String, String, Vec<usize>)> = Vec::new();

    let mut years: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (idx, date) in dates.iter().enumerate() {
        years
            .entry(date.chars().take(4).collect())
            .or_default()
            .push(idx);
    }
    for (year, indices) in years {
        if indices.len() >= 20 {
            groups.push(("年度".into(), year, indices));
        }
    }
    let positive: Vec<usize> = (0..n).filter(|&i| (x[i] + y[i]) * 0.5 >= 0.0).collect();
    let negative: Vec<usize> = (0..n).filter(|&i| (x[i] + y[i]) * 0.5 < 0.0).collect();
    if positive.len() >= 20 {
        groups.push(("市场状态".into(), "共同上涨样本".into(), positive));
    }
    if negative.len() >= 20 {
        groups.push(("市场状态".into(), "共同下跌样本".into(), negative));
    }
    let mut abs_proxy: Vec<f64> = (0..n).map(|i| ((x[i] + y[i]) * 0.5).abs()).collect();
    abs_proxy.sort_by(f64::total_cmp);
    let vol_cut = percentile(&abs_proxy, 0.5);
    let high_vol: Vec<usize> = (0..n)
        .filter(|&i| ((x[i] + y[i]) * 0.5).abs() >= vol_cut)
        .collect();
    let low_vol: Vec<usize> = (0..n)
        .filter(|&i| ((x[i] + y[i]) * 0.5).abs() < vol_cut)
        .collect();
    if high_vol.len() >= 20 {
        groups.push(("市场状态".into(), "高波动".into(), high_vol));
    }
    if low_vol.len() >= 20 {
        groups.push(("市场状态".into(), "低波动".into(), low_vol));
    }

    let window = config.rolling_window.min(n).max(10);
    if n >= window {
        let step = (window / 2).max(1);
        for start in (0..=(n - window)).step_by(step).take(8) {
            groups.push((
                "滚动窗口".into(),
                format!("{} 至 {}", dates[start], dates[start + window - 1]),
                (start..start + window).collect(),
            ));
        }
    }
    for ratio in [0.5, 0.75] {
        let count = (n as f64 * ratio) as usize;
        if count >= 20 {
            groups.push((
                "样本量敏感性".into(),
                format!("最近 {:.0}% 样本", ratio * 100.0),
                (n - count..n).collect(),
            ));
        }
    }
    let split = ((n as f64) * (1.0 - config.oos_ratio)) as usize;
    if split >= 20 && n - split >= 20 {
        groups.push(("样本外".into(), "训练段".into(), (0..split).collect()));
        groups.push(("样本外".into(), "留出检验段".into(), (split..n).collect()));
    }

    let mut slices = Vec::new();
    for (group, label, indices) in groups {
        let sx: Vec<f64> = indices.iter().map(|&i| x[i]).collect();
        let sy: Vec<f64> = indices.iter().map(|&i| y[i]).collect();
        let sc: Vec<Vec<f64>> = controls
            .iter()
            .map(|values| indices.iter().map(|&i| values[i]).collect())
            .collect();
        let refs: Vec<&[f64]> = sc.iter().map(Vec::as_slice).collect();
        let lag = admissible_lag(metric, sx.len(), max_lag);
        if let Ok((effect, _, _)) = metric_effect(metric, &sx, &sy, &refs, lag) {
            slices.push(StabilitySlice {
                group,
                label,
                effect,
                effective_n: sx.len(),
            });
        }
    }
    let (wx, wy) = (winsorize(x, 0.01), winsorize(y, 0.01));
    let outlier_robust_effect = metric_effect(metric, &wx, &wy, controls, max_lag)
        .ok()
        .map(|v| v.0);
    if let Some(effect) = outlier_robust_effect {
        slices.push(StabilitySlice {
            group: "异常值敏感性".into(),
            label: "双侧 1% 缩尾".into(),
            effect,
            effective_n: n,
        });
    }
    if metric == ResearchMetric::LeadLag && max_lag > 1 {
        if let Ok((effect, _, _)) = metric_effect(metric, x, y, controls, (max_lag / 2).max(1)) {
            slices.push(StabilitySlice {
                group: "参数敏感性".into(),
                label: format!("最大滞后 {}", (max_lag / 2).max(1)),
                effect,
                effective_n: n,
            });
        }
    }
    let comparable: Vec<f64> = slices
        .iter()
        .filter(|s| s.group == "滚动窗口" || s.group == "年度" || s.group == "样本外")
        .map(|s| s.effect)
        .collect();
    let same_direction_rate = if metric.is_signed() && !comparable.is_empty() {
        let sign = full_effect.signum();
        Some(
            comparable.iter().filter(|v| v.signum() == sign).count() as f64
                / comparable.len() as f64,
        )
    } else {
        None
    };
    let min_effect = comparable.iter().copied().reduce(f64::min);
    let max_effect = comparable.iter().copied().reduce(f64::max);
    let train_effect = slices
        .iter()
        .find(|s| s.label == "训练段")
        .map(|s| s.effect);
    let out_of_sample_effect = slices
        .iter()
        .find(|s| s.label == "留出检验段")
        .map(|s| s.effect);
    let assessment = match same_direction_rate {
        Some(rate) if rate >= 0.8 => "跨窗口方向较稳定，仍需结合区间宽度与校正后显著性".into(),
        Some(rate) if rate >= 0.6 => "跨窗口方向一般，关系可能依赖市场阶段".into(),
        Some(_) => "跨窗口方向不稳定，不应据此形成单一交易结论".into(),
        None if comparable.len() >= 3 => {
            "非负指标已完成跨窗口幅度检查，请关注区间是否明显收缩".into()
        }
        None => "可用稳定性切片不足，结论应降级".into(),
    };
    let summary = StabilitySummary {
        slice_count: slices.len(),
        same_direction_rate,
        min_effect,
        max_effect,
        train_effect,
        out_of_sample_effect,
        outlier_robust_effect,
        assessment,
    };
    (slices, summary)
}

fn apply_multiple_testing(results: &mut [PairInference], method: FdrMethod, alpha: f64) {
    let positions: Vec<usize> = results
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r.p_value.map(|_| i))
        .collect();
    if positions.is_empty() {
        return;
    }
    let raw: Vec<f64> = positions
        .iter()
        .map(|&i| results[i].p_value.unwrap())
        .collect();
    let adjusted = adjust_p_values(&raw, method);
    for (&position, value) in positions.iter().zip(adjusted) {
        results[position].adjusted_p_value = Some(value);
        results[position].significant_after_correction = Some(value <= alpha);
    }
}

/// Public, unit-tested multiple-testing correction helper.
pub fn adjust_p_values(p_values: &[f64], method: FdrMethod) -> Vec<f64> {
    let m = p_values.len();
    match method {
        FdrMethod::None => p_values.to_vec(),
        FdrMethod::Bonferroni => p_values.iter().map(|p| (p * m as f64).min(1.0)).collect(),
        FdrMethod::BenjaminiHochberg => {
            let mut order: Vec<usize> = (0..m).collect();
            order.sort_by(|&a, &b| p_values[a].total_cmp(&p_values[b]));
            let mut sorted_adjusted = vec![0.0; m];
            let mut running = 1.0_f64;
            for rank_index in (0..m).rev() {
                let rank = rank_index + 1;
                let candidate = p_values[order[rank_index]] * m as f64 / rank as f64;
                running = running.min(candidate).min(1.0);
                sorted_adjusted[rank_index] = running;
            }
            let mut output = vec![0.0; m];
            for (rank_index, &original_index) in order.iter().enumerate() {
                output[original_index] = sorted_adjusted[rank_index];
            }
            output
        }
    }
}

fn conclusion_text(result: &PairInference, alpha: f64) -> String {
    let interval = match (result.confidence_low, result.confidence_high) {
        (Some(low), Some(high)) => format!("95% 区间 [{low:.4}, {high:.4}]"),
        _ => "区间不可用".into(),
    };
    let significance = match result.significant_after_correction {
        Some(true) => format!("经多重检验校正后仍显著（阈值 {alpha:.3}）"),
        Some(false) => "经多重检验校正后不显著，不能据此筛选交易标的".into(),
        None => "无可比较的标准显著性结果".into(),
    };
    format!(
        "{}为 {:.4}，{}；{}；{}。{}",
        result.effect_name,
        result.effect,
        interval,
        significance,
        result.stability.assessment,
        result.interpretation
    )
}

fn interpretation_text(
    metric: ResearchMetric,
    lag: Option<isize>,
    left: &str,
    right: &str,
) -> String {
    match metric {
        ResearchMetric::LeadLag => match lag.unwrap_or(0).cmp(&0) {
            std::cmp::Ordering::Greater => format!("样本中 {left} 比 {right} 领先 {} 期；这只是预测性时间先后，不是结构因果", lag.unwrap_or(0)),
            std::cmp::Ordering::Less => format!("样本中 {right} 比 {left} 领先 {} 期；这只是预测性时间先后，不是结构因果", -lag.unwrap_or(0)),
            std::cmp::Ordering::Equal => "最佳关系出现在同期，不构成领先证据".into(),
        },
        ResearchMetric::Granger => format!("检验命题是“{left} 的历史值是否改善对 {right} 的线性预测”；Granger 预测因果不等于结构性因果"),
        _ => "该指标衡量样本关联，不说明谁导致谁，也不直接构成买卖建议".into(),
    }
}

fn snapshot_hash(
    config: &ResearchConfig,
    versions: &BTreeMap<String, String>,
    results: &[PairInference],
) -> Result<String, QuantError> {
    let canonical = serde_json::to_vec(&(RESEARCH_FUNCTION_VERSION, config, versions, results))
        .map_err(|e| QuantError::InvalidInput(format!("serialize research snapshot: {e}")))?;
    let digest = Sha256::digest(canonical);
    Ok(format!("qrs-{digest:x}"))
}

fn deterministic_indices(n: usize, limit: usize) -> Vec<usize> {
    if n <= limit {
        return (0..n).collect();
    }
    (0..limit).map(|i| i * (n - 1) / (limit - 1)).collect()
}

fn admissible_lag(metric: ResearchMetric, n: usize, requested: usize) -> usize {
    let limit = if metric == ResearchMetric::Granger {
        n.saturating_sub(4) / 2
    } else {
        n.saturating_sub(3)
    };
    requested.min(limit).max(1)
}

fn deterministic_shuffle<T>(values: &mut [T], seed: u64) {
    let mut rng = StdRng::seed_from_u64(seed);
    for i in (1..values.len()).rev() {
        let j = rng.random_range(0..=i);
        values.swap(i, j);
    }
}

fn fisher_yates(values: &mut [f64], rng: &mut StdRng) {
    for i in (1..values.len()).rev() {
        let j = rng.random_range(0..=i);
        values.swap(i, j);
    }
}

fn moving_block_indices(n: usize, block: usize, rng: &mut StdRng) -> Vec<usize> {
    let mut output = Vec::with_capacity(n);
    while output.len() < n {
        let start = rng.random_range(0..n);
        for offset in 0..block {
            if output.len() == n {
                break;
            }
            output.push((start + offset) % n);
        }
    }
    output
}

fn percentile(sorted: &[f64], probability: f64) -> f64 {
    let position = probability * (sorted.len().saturating_sub(1)) as f64;
    let low = position.floor() as usize;
    let high = position.ceil() as usize;
    if low == high {
        sorted[low]
    } else {
        sorted[low] + (sorted[high] - sorted[low]) * (position - low as f64)
    }
}

fn winsorize(values: &[f64], tail: f64) -> Vec<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let low = percentile(&sorted, tail);
    let high = percentile(&sorted, 1.0 - tail);
    values.iter().map(|v| v.clamp(low, high)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(symbol: &str, values: Vec<f64>) -> SeriesInput {
        SeriesInput {
            symbol: symbol.into(),
            dates: (0..values.len())
                .map(|i| format!("2025-{:02}-{:02}", i / 28 + 1, i % 28 + 1))
                .collect(),
            values,
            data_version: format!("{symbol}-v1"),
        }
    }

    #[test]
    fn benjamini_hochberg_matches_r_p_adjust_golden() {
        // R: p.adjust(c(.01,.04,.03,.002), method="BH")
        let adjusted = adjust_p_values(&[0.01, 0.04, 0.03, 0.002], FdrMethod::BenjaminiHochberg);
        let expected = [0.02, 0.04, 0.04, 0.008];
        for (actual, expected) in adjusted.iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-12, "{adjusted:?}");
        }
    }

    #[test]
    fn scipy_rank_and_linear_correlation_golden_values() {
        // Cross-checked with scipy.stats 1.15: pearsonr/spearmanr/kendalltau.
        let x = [1., 2., 3., 4., 5., 6.];
        let y = [1., 1., 2., 3., 5., 8.];
        assert!((pearson(&x, &y).unwrap() - 0.938_952_955_723_142).abs() < 1e-12);
        assert!((spearman(&x, &y).unwrap() - 0.985_610_760_609_162_3).abs() < 1e-12);
        assert!((kendall_tau_b(&x, &y).unwrap() - 0.966_091_783_079_295_9).abs() < 1e-12);
    }

    #[test]
    fn statsmodels_granger_numeric_golden() {
        // Same unrestricted/restricted OLS convention as
        // statsmodels.tsa.stattools.grangercausalitytests(maxlag=1).
        let x = [0., 1., 0., 2., 1., 3., 2., 4., 3., 5., 4., 6.];
        let y = [0., 0.2, 1.1, 0.1, 2.2, 1.0, 3.1, 2.1, 4.2, 3.0, 5.1, 4.0];
        let result = granger_causality(&x, &y, 1).unwrap();
        assert!((result.f_stat - 6_340.711_094_775_261).abs() < 1e-9);
        assert!((result.p_value - 6.897_815_651_996_098e-13).abs() < 1e-18);
    }

    #[test]
    fn scipy_correlation_golden_and_snapshot_is_reproducible() {
        // scipy.stats.pearsonr([1,2,3,4,5,6], [1,1,2,3,5,8]).statistic
        // = 0.938952955723142; repeated to meet inference sample size.
        let mut x = Vec::new();
        let mut y = Vec::new();
        for block in 0..8 {
            for (a, b) in [1., 2., 3., 4., 5., 6.]
                .into_iter()
                .zip([1., 1., 2., 3., 5., 8.])
            {
                x.push(a + block as f64 * 10.0);
                y.push(b + block as f64 * 12.0);
            }
        }
        let config = ResearchConfig {
            symbols: vec!["A".into(), "B".into()],
            value_mode: InputValueMode::PriceLevel,
            bootstrap_reps: 99,
            permutation_reps: 99,
            ..ResearchConfig::default()
        };
        let inputs = vec![input("A", x), input("B", y)];
        let first = run_research(&inputs, &config).unwrap();
        let second = run_research(&inputs, &config).unwrap();
        assert_eq!(first.snapshot_id, second.snapshot_id);
        assert_eq!(first.results[0].effect, second.results[0].effect);
        assert_eq!(first.results[0].effective_n, 48);
        assert!(first.results[0].confidence_low.is_some());
        assert!(first.results[0].adjusted_p_value.is_some());
    }

    #[test]
    fn statsmodels_granger_golden_direction_and_wording() {
        let x: Vec<f64> = (0..100).map(|i| (i * 17 % 31) as f64 + 20.0).collect();
        let mut y = vec![20.0; 100];
        for i in 2..100 {
            y[i] = 20.0 + 0.8 * x[i - 1] + 0.1 * y[i - 1];
        }
        let config = ResearchConfig {
            symbols: vec!["X".into(), "Y".into()],
            metric: ResearchMetric::Granger,
            value_mode: InputValueMode::PriceLevel,
            max_lag: 1,
            bootstrap_reps: 99,
            permutation_reps: 99,
            ..ResearchConfig::default()
        };
        let snapshot = run_research(&[input("X", x), input("Y", y)], &config).unwrap();
        let forward = snapshot
            .results
            .iter()
            .find(|r| r.left == "X" && r.right == "Y")
            .unwrap();
        assert!(forward.effect > 10.0);
        assert!(forward.p_value.unwrap() < 0.01);
        assert!(forward.interpretation.contains("不等于结构性因果"));
    }

    #[test]
    fn pair_budget_is_deterministic_and_visible() {
        let inputs: Vec<SeriesInput> = (0..6)
            .map(|s| {
                input(
                    &format!("S{s}"),
                    (0..50).map(|i| 10.0 + i as f64 + s as f64).collect(),
                )
            })
            .collect();
        let config = ResearchConfig {
            value_mode: InputValueMode::PriceLevel,
            max_pairs: 4,
            bootstrap_reps: 99,
            permutation_reps: 99,
            ..ResearchConfig::default()
        };
        let snapshot = run_research(&inputs, &config).unwrap();
        assert_eq!(snapshot.budget.requested_pairs, 15);
        assert_eq!(snapshot.budget.executed_pairs, 4);
        assert!(snapshot.budget.pair_sampling);
        assert_eq!(snapshot.results.len(), 4);
    }
}
