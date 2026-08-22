//! Deterministic offline evaluation for research-data and Agent changes.
//!
//! The runner consumes licensed, immutable snapshots. It never reaches the
//! network or calls a model, so a historical dataset/version can always be
//! replayed byte-for-byte. Online collection belongs in a separate capture
//! process and must not silently change an evaluation run.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("无法读取评测文件 {path}: {message}")]
    Read { path: String, message: String },
    #[error("评测 JSON 无效 {path}: {message}")]
    Json { path: String, message: String },
    #[error("评测集无效：{0}")]
    Invalid(String),
    #[error("质量门禁未通过：{0}")]
    Gate(String),
}

pub type Result<T> = std::result::Result<T, EvalError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Dataset {
    pub manifest: DatasetManifest,
    pub cases: Vec<EvalCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DatasetManifest {
    pub dataset_id: String,
    pub version: String,
    pub frozen_at: String,
    pub description: String,
    pub labeling_guidelines: String,
    pub dispute_policy: String,
    pub license_policy: String,
    pub train_end: String,
    pub dev_end: String,
    pub test_start: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvalCase {
    pub id: String,
    pub split: String,
    pub occurred_at: String,
    pub industry: String,
    pub regime: String,
    pub difficulty: Vec<String>,
    pub source_license: String,
    pub snapshot_ref: String,
    #[serde(flatten)]
    pub observation: Observation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "task", rename_all = "snake_case")]
pub enum Observation {
    NewsRetrieval {
        expected_ids: Vec<String>,
        retrieved: Vec<RetrievedNews>,
    },
    EventClustering {
        items: Vec<ClusterItem>,
    },
    EntityLinking {
        mentions: Vec<EntityMention>,
    },
    GraphExtraction {
        gold_edges: Vec<GraphEdge>,
        predicted_edges: Vec<GraphEdge>,
    },
    Propagation {
        expected_paths: Vec<PropagationPath>,
        predicted_paths: Vec<PropagationPath>,
    },
    AgentAnswer {
        claims: Vec<AgentClaim>,
        numeric_checks: Vec<NumericCheck>,
        conflicts_expected: u32,
        conflicts_handled: u32,
        should_abstain: bool,
        did_abstain: bool,
        task_success: bool,
        latency_ms: u64,
        tokens: u64,
        api_cost_microunits: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetrievedNews {
    pub id: String,
    pub canonical_group: String,
    pub published_at: i64,
    pub fetched_at: i64,
    pub is_old: bool,
    pub source_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClusterItem {
    pub id: String,
    pub gold_cluster: String,
    pub predicted_cluster: String,
    pub is_correction: bool,
    pub correction_detected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntityMention {
    pub text: String,
    pub entity_type: String,
    pub gold_id: Option<String>,
    pub predicted_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphEdge {
    pub src: String,
    pub dst: String,
    pub relation: String,
    pub direction: i8,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub evidence_locator: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PropagationPath {
    pub nodes: Vec<String>,
    pub direction: i8,
    pub lag_days: f64,
    pub interval_low: f64,
    pub interval_high: f64,
    pub realized_impact: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentClaim {
    pub text: String,
    pub supported: bool,
    pub citation_correct: bool,
    pub evidence_present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NumericCheck {
    pub name: String,
    pub expected: f64,
    pub predicted: f64,
    pub tolerance: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvalReport {
    pub schema_version: u32,
    pub dataset_id: String,
    pub dataset_version: String,
    pub dataset_fingerprint: String,
    pub frozen_at: String,
    pub split: String,
    pub case_count: usize,
    pub metrics: BTreeMap<String, f64>,
    pub by_industry: BTreeMap<String, SegmentReport>,
    pub by_regime: BTreeMap<String, SegmentReport>,
    pub failures: Vec<FailureMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SegmentReport {
    pub case_count: usize,
    pub metrics: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct FailureMode {
    pub case_id: String,
    pub category: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Thresholds {
    pub dataset_id: String,
    pub dataset_version: String,
    #[serde(default)]
    pub minimum: BTreeMap<String, f64>,
    #[serde(default)]
    pub maximum: BTreeMap<String, f64>,
    #[serde(default)]
    pub max_regression: BTreeMap<String, f64>,
    #[serde(default)]
    pub release_claims: Vec<ReleaseClaim>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReleaseClaim {
    pub claim: String,
    pub metric: String,
    pub minimum: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateResult {
    pub passed: bool,
    pub violations: Vec<String>,
    pub supported_release_claims: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Comparison {
    pub from_dataset_version: String,
    pub to_dataset_version: String,
    pub deltas: BTreeMap<String, f64>,
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).map_err(|error| EvalError::Read {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    serde_json::from_slice(&bytes).map_err(|error| EvalError::Json {
        path: path.display().to_string(),
        message: error.to_string(),
    })
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| EvalError::Read {
            path: parent.display().to_string(),
            message: error.to_string(),
        })?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| EvalError::Json {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    fs::write(path, bytes).map_err(|error| EvalError::Read {
        path: path.display().to_string(),
        message: error.to_string(),
    })
}

pub fn evaluate(dataset: &Dataset, split: &str) -> Result<EvalReport> {
    validate_dataset(dataset)?;
    let cases = dataset
        .cases
        .iter()
        .filter(|case| case.split == split)
        .collect::<Vec<_>>();
    if cases.is_empty() {
        return Err(EvalError::Invalid(format!("切分 {split} 没有样例")));
    }
    let (metrics, failures) = summarize(&cases);
    let industries = segment(&cases, |case| &case.industry);
    let regimes = segment(&cases, |case| &case.regime);
    let canonical = serde_json::to_vec(dataset)
        .map_err(|error| EvalError::Invalid(format!("数据集无法规范化：{error}")))?;
    Ok(EvalReport {
        schema_version: 1,
        dataset_id: dataset.manifest.dataset_id.clone(),
        dataset_version: dataset.manifest.version.clone(),
        dataset_fingerprint: format!("sha256:{:x}", Sha256::digest(canonical)),
        frozen_at: dataset.manifest.frozen_at.clone(),
        split: split.to_string(),
        case_count: cases.len(),
        metrics,
        by_industry: industries,
        by_regime: regimes,
        failures,
    })
}

fn validate_dataset(dataset: &Dataset) -> Result<()> {
    let manifest = &dataset.manifest;
    if manifest.dataset_id.trim().is_empty() || manifest.version.trim().is_empty() {
        return Err(EvalError::Invalid("数据集 ID 和版本不能为空".into()));
    }
    for required in [
        &manifest.labeling_guidelines,
        &manifest.dispute_policy,
        &manifest.license_policy,
        &manifest.train_end,
        &manifest.dev_end,
        &manifest.test_start,
    ] {
        if required.trim().is_empty() {
            return Err(EvalError::Invalid(
                "标注、许可和时间切分元数据不完整".into(),
            ));
        }
    }
    let mut ids = BTreeSet::new();
    for case in &dataset.cases {
        if !ids.insert(&case.id) {
            return Err(EvalError::Invalid(format!("样例 ID 重复：{}", case.id)));
        }
        if !matches!(case.split.as_str(), "train" | "dev" | "test") {
            return Err(EvalError::Invalid(format!("未知切分：{}", case.split)));
        }
        if case.source_license.trim().is_empty() || !valid_snapshot_ref(&case.snapshot_ref) {
            return Err(EvalError::Invalid(format!(
                "{} 缺少许可或有效的 SHA-256 快照引用",
                case.id
            )));
        }
    }
    Ok(())
}

fn valid_snapshot_ref(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn segment<'a>(
    cases: &[&'a EvalCase],
    key: impl Fn(&'a EvalCase) -> &'a str,
) -> BTreeMap<String, SegmentReport> {
    let mut grouped: BTreeMap<String, Vec<&EvalCase>> = BTreeMap::new();
    for case in cases {
        grouped.entry(key(case).to_string()).or_default().push(case);
    }
    grouped
        .into_iter()
        .map(|(name, rows)| {
            let (metrics, _) = summarize(&rows);
            (
                name,
                SegmentReport {
                    case_count: rows.len(),
                    metrics,
                },
            )
        })
        .collect()
}

#[derive(Default)]
struct Score {
    news_expected: usize,
    news_retrieved: usize,
    news_correct: usize,
    news_duplicates: usize,
    news_old: usize,
    news_sources_available: usize,
    news_latency_seconds: i64,
    news_latency_count: usize,
    cluster_tp: usize,
    cluster_fp: usize,
    cluster_fn: usize,
    corrections: usize,
    corrections_detected: usize,
    entity_tp: usize,
    entity_fp: usize,
    entity_fn: usize,
    graph_tp: usize,
    graph_fp: usize,
    graph_fn: usize,
    graph_evidence_correct: usize,
    graph_validity_correct: usize,
    path_tp: usize,
    path_fp: usize,
    path_fn: usize,
    path_direction_correct: usize,
    path_lag_abs_error: f64,
    path_interval_covered: usize,
    claim_count: usize,
    citations_correct: usize,
    evidence_present: usize,
    numeric_count: usize,
    numeric_correct: usize,
    conflicts_expected: usize,
    conflicts_handled: usize,
    abstention_cases: usize,
    abstention_correct: usize,
    agent_cases: usize,
    agent_success: usize,
    latency_ms: u64,
    tokens: u64,
    api_cost_microunits: u64,
}

fn summarize(cases: &[&EvalCase]) -> (BTreeMap<String, f64>, Vec<FailureMode>) {
    let mut score = Score::default();
    let mut failures = Vec::new();
    for case in cases {
        match &case.observation {
            Observation::NewsRetrieval {
                expected_ids,
                retrieved,
            } => score_news(case, expected_ids, retrieved, &mut score, &mut failures),
            Observation::EventClustering { items } => {
                score_clustering(case, items, &mut score, &mut failures)
            }
            Observation::EntityLinking { mentions } => {
                score_entities(case, mentions, &mut score, &mut failures)
            }
            Observation::GraphExtraction {
                gold_edges,
                predicted_edges,
            } => score_graph(case, gold_edges, predicted_edges, &mut score, &mut failures),
            Observation::Propagation {
                expected_paths,
                predicted_paths,
            } => score_paths(
                case,
                expected_paths,
                predicted_paths,
                &mut score,
                &mut failures,
            ),
            Observation::AgentAnswer {
                claims,
                numeric_checks,
                conflicts_expected,
                conflicts_handled,
                should_abstain,
                did_abstain,
                task_success,
                latency_ms,
                tokens,
                api_cost_microunits,
            } => score_agent(
                case,
                claims,
                numeric_checks,
                *conflicts_expected,
                *conflicts_handled,
                *should_abstain,
                *did_abstain,
                *task_success,
                *latency_ms,
                *tokens,
                *api_cost_microunits,
                &mut score,
                &mut failures,
            ),
        }
    }
    failures.sort();
    (metrics(&score), failures)
}

fn score_news(
    case: &EvalCase,
    expected: &[String],
    retrieved: &[RetrievedNews],
    score: &mut Score,
    failures: &mut Vec<FailureMode>,
) {
    let expected = expected.iter().collect::<BTreeSet<_>>();
    let retrieved_ids = retrieved.iter().map(|row| &row.id).collect::<BTreeSet<_>>();
    score.news_expected += expected.len();
    score.news_retrieved += retrieved.len();
    score.news_correct += expected.intersection(&retrieved_ids).count();
    let groups = retrieved
        .iter()
        .map(|row| &row.canonical_group)
        .collect::<BTreeSet<_>>();
    score.news_duplicates += retrieved.len().saturating_sub(groups.len());
    score.news_old += retrieved.iter().filter(|row| row.is_old).count();
    score.news_sources_available += retrieved.iter().filter(|row| row.source_available).count();
    score.news_latency_seconds += retrieved
        .iter()
        .filter(|row| !row.is_old)
        .map(|row| row.fetched_at.saturating_sub(row.published_at).max(0))
        .sum::<i64>();
    score.news_latency_count += retrieved.iter().filter(|row| !row.is_old).count();
    for missing in expected.difference(&retrieved_ids) {
        failure(failures, case, "新闻漏报", (*missing).clone());
    }
    for false_positive in retrieved_ids.difference(&expected) {
        failure(failures, case, "虚假或无关快讯", (*false_positive).clone());
    }
}

fn score_clustering(
    case: &EvalCase,
    items: &[ClusterItem],
    score: &mut Score,
    failures: &mut Vec<FailureMode>,
) {
    for (index, left) in items.iter().enumerate() {
        if left.is_correction {
            score.corrections += 1;
            score.corrections_detected += usize::from(left.correction_detected);
            if !left.correction_detected {
                failure(failures, case, "更正/撤回漏识别", left.id.clone());
            }
        }
        for right in items.iter().skip(index + 1) {
            match (
                left.gold_cluster == right.gold_cluster,
                left.predicted_cluster == right.predicted_cluster,
            ) {
                (true, true) => score.cluster_tp += 1,
                (false, true) => score.cluster_fp += 1,
                (true, false) => score.cluster_fn += 1,
                (false, false) => {}
            }
        }
    }
}

fn score_entities(
    case: &EvalCase,
    mentions: &[EntityMention],
    score: &mut Score,
    failures: &mut Vec<FailureMode>,
) {
    for mention in mentions {
        match (&mention.gold_id, &mention.predicted_id) {
            (gold, predicted) if gold == predicted && gold.is_some() => score.entity_tp += 1,
            (None, None) => {}
            (gold, predicted) => {
                score.entity_fn += usize::from(gold.is_some());
                score.entity_fp += usize::from(predicted.is_some());
                failure(
                    failures,
                    case,
                    "实体链接错误",
                    format!("{}({})", mention.text, mention.entity_type),
                );
            }
        }
    }
}

fn edge_relation_key(edge: &GraphEdge) -> String {
    format!(
        "{}|{}|{}|{}",
        edge.src, edge.dst, edge.relation, edge.direction
    )
}

fn score_graph(
    case: &EvalCase,
    gold: &[GraphEdge],
    predicted: &[GraphEdge],
    score: &mut Score,
    failures: &mut Vec<FailureMode>,
) {
    let gold_map = gold
        .iter()
        .map(|edge| (edge_relation_key(edge), edge))
        .collect::<BTreeMap<_, _>>();
    let predicted_map = predicted
        .iter()
        .map(|edge| (edge_relation_key(edge), edge))
        .collect::<BTreeMap<_, _>>();
    score.graph_tp += gold_map
        .keys()
        .filter(|key| predicted_map.contains_key(*key))
        .count();
    score.graph_fn += gold_map
        .keys()
        .filter(|key| !predicted_map.contains_key(*key))
        .count();
    score.graph_fp += predicted_map
        .keys()
        .filter(|key| !gold_map.contains_key(*key))
        .count();
    for (key, predicted_edge) in &predicted_map {
        if let Some(gold_edge) = gold_map.get(key) {
            score.graph_evidence_correct +=
                usize::from(predicted_edge.evidence_locator == gold_edge.evidence_locator);
            score.graph_validity_correct += usize::from(
                predicted_edge.valid_from == gold_edge.valid_from
                    && predicted_edge.valid_to == gold_edge.valid_to,
            );
        }
    }
    for missing in gold_map
        .keys()
        .filter(|key| !predicted_map.contains_key(*key))
    {
        failure(failures, case, "图谱关系漏抽取", missing.clone());
    }
}

fn path_key(path: &PropagationPath) -> String {
    path.nodes.join("→")
}

fn score_paths(
    case: &EvalCase,
    expected: &[PropagationPath],
    predicted: &[PropagationPath],
    score: &mut Score,
    failures: &mut Vec<FailureMode>,
) {
    let expected_map = expected
        .iter()
        .map(|path| (path_key(path), path))
        .collect::<BTreeMap<_, _>>();
    let predicted_map = predicted
        .iter()
        .map(|path| (path_key(path), path))
        .collect::<BTreeMap<_, _>>();
    score.path_tp += expected_map
        .keys()
        .filter(|key| predicted_map.contains_key(*key))
        .count();
    score.path_fn += expected_map
        .keys()
        .filter(|key| !predicted_map.contains_key(*key))
        .count();
    score.path_fp += predicted_map
        .keys()
        .filter(|key| !expected_map.contains_key(*key))
        .count();
    for (key, predicted_path) in &predicted_map {
        if let Some(expected_path) = expected_map.get(key) {
            score.path_direction_correct +=
                usize::from(predicted_path.direction == expected_path.direction);
            score.path_lag_abs_error += (predicted_path.lag_days - expected_path.lag_days).abs();
            score.path_interval_covered += usize::from(
                predicted_path.realized_impact >= predicted_path.interval_low
                    && predicted_path.realized_impact <= predicted_path.interval_high,
            );
        }
    }
    for missing in expected_map
        .keys()
        .filter(|key| !predicted_map.contains_key(*key))
    {
        failure(failures, case, "传导路径缺失", missing.clone());
    }
}

#[allow(clippy::too_many_arguments)]
fn score_agent(
    case: &EvalCase,
    claims: &[AgentClaim],
    numeric_checks: &[NumericCheck],
    conflicts_expected: u32,
    conflicts_handled: u32,
    should_abstain: bool,
    did_abstain: bool,
    task_success: bool,
    latency_ms: u64,
    tokens: u64,
    api_cost_microunits: u64,
    score: &mut Score,
    failures: &mut Vec<FailureMode>,
) {
    score.claim_count += claims.len();
    score.citations_correct += claims
        .iter()
        .filter(|claim| claim.supported && claim.citation_correct)
        .count();
    score.evidence_present += claims
        .iter()
        .filter(|claim| claim.supported && claim.evidence_present)
        .count();
    for claim in claims
        .iter()
        .filter(|claim| !claim.supported || !claim.citation_correct || !claim.evidence_present)
    {
        failure(failures, case, "Agent 事实或引用错误", claim.text.clone());
    }
    score.numeric_count += numeric_checks.len();
    for check in numeric_checks {
        let correct = (check.expected - check.predicted).abs() <= check.tolerance;
        score.numeric_correct += usize::from(correct);
        if !correct {
            failure(failures, case, "Agent 数值不一致", check.name.clone());
        }
    }
    score.conflicts_expected += conflicts_expected as usize;
    score.conflicts_handled += conflicts_handled.min(conflicts_expected) as usize;
    if should_abstain {
        score.abstention_cases += 1;
        score.abstention_correct += usize::from(did_abstain);
        if !did_abstain {
            failure(failures, case, "无数据时未拒答", "给出了无证据结论".into());
        }
    }
    score.agent_cases += 1;
    score.agent_success += usize::from(task_success);
    score.latency_ms += latency_ms;
    score.tokens += tokens;
    score.api_cost_microunits += api_cost_microunits;
}

fn metrics(score: &Score) -> BTreeMap<String, f64> {
    let mut output = BTreeMap::new();
    let insert = |map: &mut BTreeMap<String, f64>, key: &str, value: f64| {
        map.insert(key.to_string(), round6(value));
    };
    if score.news_expected > 0 || score.news_retrieved > 0 {
        insert(
            &mut output,
            "news.coverage",
            ratio(score.news_correct, score.news_expected),
        );
        insert(
            &mut output,
            "news.precision",
            ratio(score.news_correct, score.news_retrieved),
        );
        insert(
            &mut output,
            "news.duplicate_rate",
            ratio(score.news_duplicates, score.news_retrieved),
        );
        insert(
            &mut output,
            "news.old_news_rate",
            ratio(score.news_old, score.news_retrieved),
        );
        insert(
            &mut output,
            "news.source_availability",
            ratio(score.news_sources_available, score.news_retrieved),
        );
        insert(
            &mut output,
            "news.mean_latency_seconds",
            ratio_i64(score.news_latency_seconds, score.news_latency_count),
        );
    }
    let cluster_precision = ratio(score.cluster_tp, score.cluster_tp + score.cluster_fp);
    let cluster_recall = ratio(score.cluster_tp, score.cluster_tp + score.cluster_fn);
    if score.cluster_tp + score.cluster_fp + score.cluster_fn > 0 {
        insert(
            &mut output,
            "clustering.pairwise_precision",
            cluster_precision,
        );
        insert(&mut output, "clustering.pairwise_recall", cluster_recall);
        insert(
            &mut output,
            "clustering.pairwise_f1",
            f1(cluster_precision, cluster_recall),
        );
    }
    if score.corrections > 0 {
        insert(
            &mut output,
            "clustering.correction_recall",
            ratio(score.corrections_detected, score.corrections),
        );
    }
    let entity_precision = ratio(score.entity_tp, score.entity_tp + score.entity_fp);
    let entity_recall = ratio(score.entity_tp, score.entity_tp + score.entity_fn);
    if score.entity_tp + score.entity_fp + score.entity_fn > 0 {
        insert(&mut output, "entity.precision", entity_precision);
        insert(&mut output, "entity.recall", entity_recall);
        insert(
            &mut output,
            "entity.f1",
            f1(entity_precision, entity_recall),
        );
    }
    let graph_precision = ratio(score.graph_tp, score.graph_tp + score.graph_fp);
    let graph_recall = ratio(score.graph_tp, score.graph_tp + score.graph_fn);
    if score.graph_tp + score.graph_fp + score.graph_fn > 0 {
        insert(&mut output, "graph.precision", graph_precision);
        insert(&mut output, "graph.recall", graph_recall);
        insert(&mut output, "graph.f1", f1(graph_precision, graph_recall));
        insert(
            &mut output,
            "graph.evidence_accuracy",
            ratio(score.graph_evidence_correct, score.graph_tp),
        );
        insert(
            &mut output,
            "graph.validity_accuracy",
            ratio(score.graph_validity_correct, score.graph_tp),
        );
    }
    let path_precision = ratio(score.path_tp, score.path_tp + score.path_fp);
    let path_recall = ratio(score.path_tp, score.path_tp + score.path_fn);
    if score.path_tp + score.path_fp + score.path_fn > 0 {
        insert(&mut output, "propagation.path_precision", path_precision);
        insert(&mut output, "propagation.path_recall", path_recall);
        insert(
            &mut output,
            "propagation.path_f1",
            f1(path_precision, path_recall),
        );
        insert(
            &mut output,
            "propagation.direction_accuracy",
            ratio(score.path_direction_correct, score.path_tp),
        );
        insert(
            &mut output,
            "propagation.lag_mae_days",
            if score.path_tp == 0 {
                0.0
            } else {
                score.path_lag_abs_error / score.path_tp as f64
            },
        );
        insert(
            &mut output,
            "propagation.interval_coverage",
            ratio(score.path_interval_covered, score.path_tp),
        );
    }
    if score.claim_count > 0 {
        insert(
            &mut output,
            "agent.fact_citation_accuracy",
            ratio(score.citations_correct, score.claim_count),
        );
        insert(
            &mut output,
            "agent.evidence_coverage",
            ratio(score.evidence_present, score.claim_count),
        );
    }
    if score.numeric_count > 0 {
        insert(
            &mut output,
            "agent.numeric_consistency",
            ratio(score.numeric_correct, score.numeric_count),
        );
    }
    if score.conflicts_expected > 0 {
        insert(
            &mut output,
            "agent.conflict_handling",
            ratio(score.conflicts_handled, score.conflicts_expected),
        );
    }
    if score.abstention_cases > 0 {
        insert(
            &mut output,
            "agent.abstention_accuracy",
            ratio(score.abstention_correct, score.abstention_cases),
        );
    }
    if score.agent_cases > 0 {
        insert(
            &mut output,
            "agent.task_success",
            ratio(score.agent_success, score.agent_cases),
        );
        insert(
            &mut output,
            "agent.mean_latency_ms",
            score.latency_ms as f64 / score.agent_cases as f64,
        );
        insert(
            &mut output,
            "agent.mean_tokens",
            score.tokens as f64 / score.agent_cases as f64,
        );
        insert(
            &mut output,
            "agent.mean_api_cost_microunits",
            score.api_cost_microunits as f64 / score.agent_cases as f64,
        );
    }
    output
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn ratio_i64(numerator: i64, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn f1(precision: f64, recall: f64) -> f64 {
    if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    }
}

fn round6(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn failure(failures: &mut Vec<FailureMode>, case: &EvalCase, category: &str, detail: String) {
    failures.push(FailureMode {
        case_id: case.id.clone(),
        category: category.to_string(),
        detail,
    });
}

pub fn check_thresholds(
    report: &EvalReport,
    thresholds: &Thresholds,
    baseline: Option<&EvalReport>,
) -> Result<GateResult> {
    if report.dataset_id != thresholds.dataset_id
        || report.dataset_version != thresholds.dataset_version
    {
        return Err(EvalError::Invalid("报告与阈值的数据集版本不一致".into()));
    }
    let mut violations = Vec::new();
    for (metric, required) in &thresholds.minimum {
        match report.metrics.get(metric) {
            Some(actual) if actual + f64::EPSILON >= *required => {}
            Some(actual) => violations.push(format!("{metric}={actual:.6} 低于下限 {required:.6}")),
            None => violations.push(format!("缺少必需指标 {metric}")),
        }
    }
    for (metric, required) in &thresholds.maximum {
        match report.metrics.get(metric) {
            Some(actual) if actual <= &(required + f64::EPSILON) => {}
            Some(actual) => violations.push(format!("{metric}={actual:.6} 高于上限 {required:.6}")),
            None => violations.push(format!("缺少必需指标 {metric}")),
        }
    }
    if baseline.is_none() && !thresholds.max_regression.is_empty() {
        violations.push("配置了相对回退阈值，但没有提供固定基线报告".to_string());
    }
    if let Some(baseline) = baseline {
        if baseline.dataset_id != report.dataset_id || baseline.split != report.split {
            return Err(EvalError::Invalid("基线报告与当前报告不兼容".into()));
        }
        for (metric, allowed_drop) in &thresholds.max_regression {
            if let (Some(current), Some(previous)) =
                (report.metrics.get(metric), baseline.metrics.get(metric))
            {
                let drop = previous - current;
                if drop > *allowed_drop + f64::EPSILON {
                    violations.push(format!(
                        "{metric} 较基线下降 {drop:.6}，超过允许值 {allowed_drop:.6}"
                    ));
                }
            }
        }
    }
    let mut supported_release_claims = Vec::new();
    for claim in &thresholds.release_claims {
        match report.metrics.get(&claim.metric) {
            Some(actual) if actual + f64::EPSILON >= claim.minimum => {
                supported_release_claims.push(claim.claim.clone());
            }
            Some(actual) => violations.push(format!(
                "发布描述“{}”缺少评测支持：{}={actual:.6}，要求 {:.6}",
                claim.claim, claim.metric, claim.minimum
            )),
            None => violations.push(format!(
                "发布描述“{}”引用了缺失指标 {}",
                claim.claim, claim.metric
            )),
        }
    }
    Ok(GateResult {
        passed: violations.is_empty(),
        violations,
        supported_release_claims,
    })
}

pub fn compare(from: &EvalReport, to: &EvalReport) -> Result<Comparison> {
    if from.dataset_id != to.dataset_id || from.split != to.split {
        return Err(EvalError::Invalid("只能比较同一数据集和切分".into()));
    }
    let keys = from
        .metrics
        .keys()
        .chain(to.metrics.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let deltas = keys
        .into_iter()
        .map(|key| {
            let delta = to.metrics.get(&key).copied().unwrap_or(0.0)
                - from.metrics.get(&key).copied().unwrap_or(0.0);
            (key, round6(delta))
        })
        .collect();
    Ok(Comparison {
        from_dataset_version: from.dataset_version.clone(),
        to_dataset_version: to.dataset_version.clone(),
        deltas,
    })
}

pub fn render_html(report: &EvalReport, gate: Option<&GateResult>) -> String {
    let mut rows = String::new();
    for (name, value) in &report.metrics {
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{:.6}</td></tr>",
            escape_html(name),
            value
        ));
    }
    let segments = |title: &str, data: &BTreeMap<String, SegmentReport>| {
        let mut html = format!(
            "<h2>{}</h2><table><tr><th>分组</th><th>样例数</th><th>指标</th></tr>",
            escape_html(title)
        );
        for (name, segment) in data {
            let metrics = segment
                .metrics
                .iter()
                .map(|(key, value)| format!("{}={:.3}", escape_html(key), value))
                .collect::<Vec<_>>()
                .join("<br>");
            html.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(name),
                segment.case_count,
                metrics
            ));
        }
        html.push_str("</table>");
        html
    };
    let gate_html = gate
        .map(|result| {
            let status = if result.passed { "通过" } else { "失败" };
            let details = result
                .violations
                .iter()
                .map(|row| format!("<li>{}</li>", escape_html(row)))
                .collect::<String>();
            let claims = result
                .supported_release_claims
                .iter()
                .map(|row| format!("<li>{}</li>", escape_html(row)))
                .collect::<String>();
            format!(
                "<section class=\"gate {}\"><h2>发布门禁：{}</h2><ul>{}</ul><h3>评测支持的发布描述</h3><ul>{}</ul></section>",
                if result.passed { "pass" } else { "fail" },
                status,
                details,
                claims
            )
        })
        .unwrap_or_default();
    let failures = report
        .failures
        .iter()
        .map(|failure| {
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(&failure.case_id),
                escape_html(&failure.category),
                escape_html(&failure.detail)
            )
        })
        .collect::<String>();
    format!(
        r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>AStock 评测报告</title><style>body{{font:14px/1.55 system-ui;margin:32px;color:#172033}}h1,h2{{margin:24px 0 10px}}.meta{{color:#53627a}}table{{border-collapse:collapse;width:100%}}th,td{{border:1px solid #d9dfeb;padding:8px;text-align:left;vertical-align:top}}th{{background:#f2f5fa}}.gate{{padding:12px;border-radius:8px}}.pass{{background:#e9f8ef}}.fail{{background:#fff0f0}}</style></head><body><h1>AStock 版本化评测报告</h1><p class="meta">数据集：{} / {} · 切分：{} · 固化时间：{} · 指纹：{} · 样例：{}</p>{}<h2>总体指标</h2><table><tr><th>指标</th><th>值</th></tr>{}</table>{}{}<h2>失败模式</h2><table><tr><th>样例</th><th>类别</th><th>详情</th></tr>{}</table></body></html>"#,
        escape_html(&report.dataset_id),
        escape_html(&report.dataset_version),
        escape_html(&report.split),
        escape_html(&report.frozen_at),
        escape_html(&report.dataset_fingerprint),
        report.case_count,
        gate_html,
        rows,
        segments("行业分组", &report.by_industry),
        segments("市场状态分组", &report.by_regime),
        failures
    )
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Dataset {
        Dataset {
            manifest: DatasetManifest {
                dataset_id: "fixture".into(),
                version: "1".into(),
                frozen_at: "2026-01-01".into(),
                description: "test".into(),
                labeling_guidelines: "g".into(),
                dispute_policy: "d".into(),
                license_policy: "l".into(),
                train_end: "2024".into(),
                dev_end: "2025".into(),
                test_start: "2026".into(),
            },
            cases: vec![EvalCase {
                id: "n1".into(),
                split: "test".into(),
                occurred_at: "2026-01-01".into(),
                industry: "银行".into(),
                regime: "震荡".into(),
                difficulty: vec!["虚假快讯".into()],
                source_license: "synthetic".into(),
                snapshot_ref:
                    "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
                observation: Observation::NewsRetrieval {
                    expected_ids: vec!["a".into()],
                    retrieved: vec![RetrievedNews {
                        id: "a".into(),
                        canonical_group: "g".into(),
                        published_at: 10,
                        fetched_at: 20,
                        is_old: false,
                        source_available: true,
                    }],
                },
            }],
        }
    }

    #[test]
    fn deterministic_report_and_html() {
        let dataset = fixture();
        let first = evaluate(&dataset, "test").unwrap();
        let second = evaluate(&dataset, "test").unwrap();
        assert_eq!(first, second);
        assert_eq!(render_html(&first, None), render_html(&second, None));
        assert_eq!(first.metrics["news.coverage"], 1.0);
    }

    #[test]
    fn threshold_and_regression_gate_fail_closed() {
        let report = evaluate(&fixture(), "test").unwrap();
        let thresholds = Thresholds {
            dataset_id: "fixture".into(),
            dataset_version: "1".into(),
            minimum: [("news.coverage".into(), 1.01)].into(),
            maximum: BTreeMap::new(),
            max_regression: BTreeMap::new(),
            release_claims: Vec::new(),
        };
        let gate = check_thresholds(&report, &thresholds, None).unwrap();
        assert!(!gate.passed);
        assert!(gate.violations[0].contains("低于下限"));
    }

    #[test]
    fn configured_regression_gate_requires_baseline() {
        let report = evaluate(&fixture(), "test").unwrap();
        let thresholds = Thresholds {
            dataset_id: "fixture".into(),
            dataset_version: "1".into(),
            minimum: BTreeMap::new(),
            maximum: BTreeMap::new(),
            max_regression: [("news.coverage".into(), 0.01)].into(),
            release_claims: Vec::new(),
        };
        let gate = check_thresholds(&report, &thresholds, None).unwrap();
        assert!(!gate.passed);
        assert!(gate.violations[0].contains("没有提供固定基线"));
    }

    #[test]
    fn historical_reports_compare_without_network_or_clock() {
        let from = evaluate(&fixture(), "test").unwrap();
        let mut to = from.clone();
        to.metrics.insert("news.coverage".into(), 0.75);
        let comparison = compare(&from, &to).unwrap();
        assert_eq!(comparison.deltas["news.coverage"], -0.25);
    }

    #[test]
    fn rejects_duplicate_case_ids() {
        let mut dataset = fixture();
        dataset.cases.push(dataset.cases[0].clone());
        assert!(evaluate(&dataset, "test").is_err());
    }
}
