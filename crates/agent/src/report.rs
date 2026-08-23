//! Strong research-report contract and deterministic publication verifier.
//!
//! The model may write fluent prose, but it cannot decide whether that prose
//! is publishable. Tool snapshots are indexed down to scalar JSON paths and
//! the verifier independently checks references, numbers, units, freshness,
//! source tier, conflicts and unsupported absolute language.

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Provenance of one tool execution that fed the final answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// Stable snapshot-level reference.
    #[serde(default)]
    pub evidence_id: String,
    /// Tool that produced the data.
    pub tool: String,
    /// Cache key under which the full payload is stored.
    pub cache_key: String,
    /// Upstream data source.
    pub source: String,
    /// Fetch time of the underlying data, RFC 3339.
    pub fetched_at: String,
    /// Stable version of the tool contract used to create the snapshot.
    #[serde(default = "default_tool_version")]
    pub tool_version: String,
    /// Content-addressed data snapshot version.
    #[serde(default)]
    pub data_version: String,
    /// Source trust tier: primary / provider / engine / discovery_only.
    #[serde(default = "default_source_tier")]
    pub source_tier: String,
    /// Snapshot freshness state.
    #[serde(default = "default_freshness")]
    pub freshness: String,
    /// A blocking quality gate was raised for this snapshot.
    #[serde(default)]
    pub blocking: bool,
    /// Scalar fields that claims can cite precisely.
    #[serde(default)]
    pub fields: Vec<EvidenceField>,
}

fn default_tool_version() -> String {
    "agent-tool-contract-v2".to_string()
}

fn default_source_tier() -> String {
    "provider".to_string()
}

fn default_freshness() -> String {
    "unknown".to_string()
}

/// One field-level, immutable evidence address.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceField {
    pub evidence_id: String,
    /// RFC 6901-like JSON pointer into the compact tool result.
    pub field_path: String,
    pub value: Value,
    pub unit: Option<String>,
    pub currency: Option<String>,
    pub as_of: String,
    pub freshness: String,
    pub source_tier: String,
    pub blocking: bool,
    /// Present when the field is the output of a deterministic engine.
    pub calculation_id: Option<String>,
}

/// One graded conclusion line parsed from the model's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conclusion {
    /// Grade label: 事实 / 计算 / 外部 / 推断 / 假设 / 未知.
    pub grade: String,
    /// The conclusion text after the grade marker.
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimType {
    Fact,
    Calculation,
    External,
    Inference,
    Assumption,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimConfidence {
    High,
    Medium,
    Low,
    Blocked,
}

/// A claim with explicit links to evidence and deterministic calculations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchClaim {
    pub claim_id: String,
    pub text: String,
    pub claim_type: ClaimType,
    pub evidence_ids: Vec<String>,
    pub calculation_ids: Vec<String>,
    pub as_of: Option<String>,
    pub confidence: ClaimConfidence,
    pub assumptions: Vec<String>,
    pub counter_evidence: Vec<String>,
    pub invalidation: Vec<String>,
    pub unknowns: Vec<String>,
}

/// Registered deterministic calculation used by a report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalculationRecord {
    pub calculation_id: String,
    pub tool: String,
    pub field_path: String,
    pub value: Value,
    pub unit: Option<String>,
    pub data_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Passed,
    Failed,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerificationFinding {
    pub code: String,
    pub severity: VerificationSeverity,
    pub claim_id: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerificationResult {
    pub status: VerificationStatus,
    pub verifier_version: String,
    pub verified_at: i64,
    pub findings: Vec<VerificationFinding>,
}

impl VerificationResult {
    pub fn passed(&self) -> bool {
        self.status == VerificationStatus::Passed
    }

    pub fn repair_instructions(&self) -> String {
        self.findings
            .iter()
            .filter(|finding| finding.severity == VerificationSeverity::Error)
            .map(|finding| format!("{}：{}", finding.code, finding.message))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Strong machine-readable payload saved alongside the human answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchReport {
    pub schema_version: String,
    pub as_of: Option<String>,
    pub confidence: ClaimConfidence,
    pub claims: Vec<ResearchClaim>,
    pub calculations: Vec<CalculationRecord>,
    pub assumptions: Vec<String>,
    pub counter_evidence: Vec<String>,
    pub invalidation: Vec<String>,
    pub unknowns: Vec<String>,
    pub verification: VerificationResult,
}

/// The assembled final report of an agent task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentReport {
    pub task_id: String,
    pub answer: String,
    pub conclusions: Vec<Conclusion>,
    pub evidence: Vec<Evidence>,
    pub generated_at: i64,
    pub research: ResearchReport,
}

/// Recognized conclusion grade markers.
const GRADES: &[&str] = &["事实", "计算", "外部", "推断", "假设", "未知"];

/// Parse `【级别】...` lines out of the model's answer.
pub fn parse_conclusions(text: &str) -> Vec<Conclusion> {
    visible_lines(text)
        .filter_map(|line| parse_graded_line(&line).map(|(grade, text)| Conclusion { grade, text }))
        .collect()
}

/// Turn a compact tool result into stable snapshot- and field-level evidence.
pub fn index_tool_evidence(
    tool: &str,
    cache_key: &str,
    source: &str,
    fetched_at: &str,
    summary: &Value,
) -> Evidence {
    let snapshot_seed = format!("{tool}\u{1f}{cache_key}\u{1f}{fetched_at}\u{1f}{summary}");
    let evidence_id = stable_id("ev", &snapshot_seed);
    let data_version = stable_id("data", &summary.to_string());
    let source_tier = infer_source_tier(tool, source);
    let freshness = summary
        .pointer("/data_quality/freshness")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let quality_blocked = summary
        .pointer("/data_quality/allow_deterministic_compute")
        .and_then(Value::as_bool)
        == Some(false)
        || summary
            .pointer("/data_quality/conflicts")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0;
    let mut fields = Vec::new();
    flatten_fields(
        summary,
        "",
        tool,
        &evidence_id,
        fetched_at,
        &freshness,
        &source_tier,
        quality_blocked,
        &mut fields,
    );
    Evidence {
        evidence_id,
        tool: tool.to_string(),
        cache_key: cache_key.to_string(),
        source: source.to_string(),
        fetched_at: fetched_at.to_string(),
        tool_version: default_tool_version(),
        data_version,
        source_tier,
        freshness,
        blocking: quality_blocked,
        fields,
    }
}

#[allow(clippy::too_many_arguments)]
fn flatten_fields(
    value: &Value,
    path: &str,
    tool: &str,
    parent_id: &str,
    fetched_at: &str,
    freshness: &str,
    source_tier: &str,
    blocking: bool,
    out: &mut Vec<EvidenceField>,
) {
    // Compact summaries can still contain long arrays. The field contract is
    // deliberately bounded so it remains practical to send to the model.
    if out.len() >= 256 {
        return;
    }
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                let child_path = format!("{path}/{escaped}");
                flatten_fields(
                    child,
                    &child_path,
                    tool,
                    parent_id,
                    fetched_at,
                    freshness,
                    source_tier,
                    blocking,
                    out,
                );
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                flatten_fields(
                    child,
                    &format!("{path}/{index}"),
                    tool,
                    parent_id,
                    fetched_at,
                    freshness,
                    source_tier,
                    blocking,
                    out,
                );
                if out.len() >= 256 {
                    break;
                }
            }
        }
        Value::Null => {}
        scalar => {
            let field_id = stable_id("evf", &format!("{parent_id}\u{1f}{path}\u{1f}{scalar}"));
            let calculation_id = is_calculation_tool(tool)
                .then(|| stable_id("calc", &format!("{field_id}\u{1f}{tool}")));
            let (unit, currency) = infer_unit(path);
            out.push(EvidenceField {
                evidence_id: field_id,
                field_path: if path.is_empty() { "/" } else { path }.to_string(),
                value: scalar.clone(),
                unit,
                currency,
                as_of: fetched_at.to_string(),
                freshness: freshness.to_string(),
                source_tier: source_tier.to_string(),
                blocking,
                calculation_id,
            });
        }
    }
}

fn is_calculation_tool(tool: &str) -> bool {
    tool.starts_with("run_")
        || tool.starts_with("compute_")
        || matches!(tool, "compare_stocks" | "scan_market" | "iterate_strategy")
}

fn infer_source_tier(tool: &str, source: &str) -> String {
    if tool == "search_web" || source.contains("discovery") {
        "discovery_only"
    } else if matches!(tool, "fetch_source_document" | "read_document") {
        "primary"
    } else if is_calculation_tool(tool) || source == "engine" {
        "engine"
    } else {
        "provider"
    }
    .to_string()
}

fn infer_unit(path: &str) -> (Option<String>, Option<String>) {
    let path = path.to_ascii_lowercase();
    if path.contains("pct")
        || path.contains("percent")
        || path.contains("ratio")
        || path.contains("rate")
        || path.contains("yield")
        || path.contains("roe")
        || path.contains("margin")
    {
        (Some("percent".to_string()), None)
    } else if path.contains("price")
        || path.ends_with("/open")
        || path.ends_with("/high")
        || path.ends_with("/low")
        || path.ends_with("/close")
        || path.contains("market_cap")
        || path.contains("revenue")
        || path.contains("profit")
        || path.contains("amount")
    {
        (Some("cny".to_string()), Some("CNY".to_string()))
    } else if path.contains("volume") || path.contains("shares") {
        (Some("shares".to_string()), None)
    } else {
        (None, None)
    }
}

/// Assemble and verify the final report. The answer remains human-readable;
/// the strong report is carried alongside it for storage and UI drill-down.
pub fn assemble_report(
    task_id: &str,
    final_text: &str,
    evidence: Vec<Evidence>,
    generated_at: i64,
) -> AgentReport {
    let conclusions = parse_conclusions(final_text);
    let mut research = build_research_report(final_text, &evidence, generated_at);
    research.verification = verify_research_report(&research, &evidence, generated_at);
    AgentReport {
        task_id: task_id.to_string(),
        // The verifier and drill-down report keep the model's internal field
        // references, but the ordinary chat answer is a publication surface.
        // Never expose evidence addresses, tool identifiers or credential
        // variable names there.
        answer: sanitize_public_answer(final_text),
        conclusions,
        evidence,
        generated_at,
        research,
    }
}

/// Build a conservative publication fallback after model-driven repair has
/// been exhausted. Only claims with no verifier error survive; unsupported
/// lines are omitted and disclosed as unknown instead of turning a long,
/// otherwise useful research run into an opaque terminal error.
pub fn verified_subset_answer(report: &AgentReport) -> Option<String> {
    let blocked_claims: BTreeSet<&str> = report
        .research
        .verification
        .findings
        .iter()
        .filter(|finding| finding.severity == VerificationSeverity::Error)
        .filter_map(|finding| finding.claim_id.as_deref())
        .collect();
    let kept = report
        .research
        .claims
        .iter()
        .filter(|claim| !blocked_claims.contains(claim.claim_id.as_str()))
        .collect::<Vec<_>>();
    if report.research.claims.is_empty() {
        return None;
    }

    let omitted = report.research.claims.len().saturating_sub(kept.len());
    let mut answer = if kept.is_empty() {
        String::from(
            "## 本轮暂无可发布结论\n\n现有草稿中的关键数字或表述未能通过字段级证据复现，因此不会将其作为决策依据。\n",
        )
    } else {
        String::from(
            "## 已通过证据校验的部分结论\n\n以下仅保留可由现有字段级证据复现的内容；未通过校验的数字和表述不会作为决策依据。\n",
        )
    };
    for claim in kept {
        let grade = match claim.claim_type {
            ClaimType::Fact => "事实",
            ClaimType::Calculation => "计算",
            ClaimType::External => "外部",
            ClaimType::Inference => "推断",
            ClaimType::Assumption => "假设",
            ClaimType::Unknown => "未知",
        };
        answer.push_str(&format!("\n- 【{grade}】{}", claim.text));
    }
    if omitted > 0 {
        answer.push_str(&format!(
            "\n\n## 尚未核验的内容\n\n- 【未知】另有 {omitted} 条草稿结论未能通过数字、引用或绝对化表述校验，已自动省略；需要补充或刷新证据后再继续分析。"
        ));
    }
    Some(answer)
}

fn build_research_report(text: &str, evidence: &[Evidence], generated_at: i64) -> ResearchReport {
    let mut claims = Vec::new();
    let mut section = String::new();
    let mut assumptions = Vec::new();
    let mut counter_evidence = Vec::new();
    let mut invalidation = Vec::new();
    let mut unknowns = Vec::new();

    for raw in visible_lines(text) {
        let line = clean_markdown_prefix(&raw);
        if line.is_empty() {
            continue;
        }
        if raw.trim_start().starts_with('#') {
            section = line.to_string();
            continue;
        }
        let graded = parse_graded_line(line);
        if graded.is_none()
            && (section.contains("继续追问")
                || section.contains("下一步")
                || line.ends_with('?')
                || line.ends_with('？')
                || line.contains("数据质量门禁：")
                || line.contains("核验状态："))
        {
            continue;
        }
        let (claim_type, claim_text) = if let Some((grade, claim_text)) = graded {
            (claim_type_from_grade(&grade), claim_text)
        } else if is_plan_parameter_section(&section) && looks_like_key_claim(line) {
            // Allocation weights, order sizes and risk budgets are proposed
            // decision parameters, not observed market facts. Treat table
            // rows under an explicit plan/scenario heading as assumptions so
            // the verifier does not demand a provider field for the user's
            // own capital constraint or a not-yet-executed order.
            (ClaimType::Assumption, line.to_string())
        } else if looks_like_key_claim(line) {
            (ClaimType::Inference, line.to_string())
        } else {
            continue;
        };

        let mut evidence_ids = extract_refs(&claim_text, "证据");
        let mut calculation_ids = extract_refs(&claim_text, "计算引用");
        // Exact scalar matches are safe to link automatically; this supports
        // ordinary formatting such as 200,000,000 -> 2亿元 without trusting
        // the model to invent an evidence address.
        for quantity in extract_quantities(&remove_reference_markup(&claim_text)) {
            if let Some((field, _)) = best_field_match(&quantity, evidence, None) {
                evidence_ids.push(field.evidence_id.clone());
                if let Some(calculation_id) = &field.calculation_id {
                    calculation_ids.push(calculation_id.clone());
                }
            }
        }
        dedup(&mut evidence_ids);
        dedup(&mut calculation_ids);
        let cited_fields = cited_fields(&evidence_ids, evidence);
        let as_of = cited_fields
            .iter()
            .map(|field| field.as_of.as_str())
            .max()
            .map(str::to_string);
        let confidence = if cited_fields.iter().any(|field| field.blocking) {
            ClaimConfidence::Blocked
        } else if cited_fields.is_empty() && calculation_ids.is_empty() {
            ClaimConfidence::Low
        } else if cited_fields.iter().any(|field| field.freshness != "fresh") {
            ClaimConfidence::Medium
        } else {
            ClaimConfidence::High
        };
        let claim_id = stable_id("claim", &claim_text);
        let mut claim = ResearchClaim {
            claim_id,
            text: claim_text.clone(),
            claim_type,
            evidence_ids,
            calculation_ids,
            as_of,
            confidence,
            assumptions: Vec::new(),
            counter_evidence: Vec::new(),
            invalidation: Vec::new(),
            unknowns: Vec::new(),
        };
        if claim_type == ClaimType::Assumption || section.contains("假设") {
            assumptions.push(claim_text.clone());
            claim.assumptions.push(claim_text.clone());
        }
        if section.contains("反方") || section.contains("反证") || section.contains("冲突") {
            counter_evidence.push(claim_text.clone());
            claim.counter_evidence.push(claim_text.clone());
        }
        if section.contains("失效") || line.contains("失效条件") {
            invalidation.push(claim_text.clone());
            claim.invalidation.push(claim_text.clone());
        }
        if claim_type == ClaimType::Unknown
            || line.contains("不确定")
            || line.contains("证据不足")
            || line.contains("无法确认")
        {
            unknowns.push(claim_text.clone());
            claim.unknowns.push(claim_text.clone());
        }
        claims.push(claim);
    }

    let calculations = evidence
        .iter()
        .flat_map(|item| {
            item.fields.iter().filter_map(move |field| {
                field
                    .calculation_id
                    .as_ref()
                    .map(|calculation_id| CalculationRecord {
                        calculation_id: calculation_id.clone(),
                        tool: item.tool.clone(),
                        field_path: field.field_path.clone(),
                        value: field.value.clone(),
                        unit: field.unit.clone(),
                        data_version: item.data_version.clone(),
                    })
            })
        })
        .collect();
    let as_of = evidence
        .iter()
        .map(|item| item.fetched_at.as_str())
        .max()
        .map(str::to_string);
    let confidence = if claims
        .iter()
        .any(|claim| claim.confidence == ClaimConfidence::Blocked)
    {
        ClaimConfidence::Blocked
    } else if claims
        .iter()
        .any(|claim| claim.confidence == ClaimConfidence::Low)
    {
        ClaimConfidence::Low
    } else if claims
        .iter()
        .any(|claim| claim.confidence == ClaimConfidence::Medium)
    {
        ClaimConfidence::Medium
    } else if claims.is_empty() {
        ClaimConfidence::Low
    } else {
        ClaimConfidence::High
    };
    ResearchReport {
        schema_version: "astock-research-report/v1".to_string(),
        as_of,
        confidence,
        claims,
        calculations,
        assumptions,
        counter_evidence,
        invalidation,
        unknowns,
        verification: VerificationResult {
            status: VerificationStatus::NotApplicable,
            verifier_version: "report-verifier/v1".to_string(),
            verified_at: generated_at,
            findings: Vec::new(),
        },
    }
}

/// Independent deterministic verifier. It never invokes a model.
pub fn verify_research_report(
    report: &ResearchReport,
    evidence: &[Evidence],
    verified_at: i64,
) -> VerificationResult {
    let mut findings = Vec::new();
    let all_evidence_ids: BTreeSet<&str> = evidence
        .iter()
        .flat_map(|item| {
            std::iter::once(item.evidence_id.as_str())
                .chain(item.fields.iter().map(|field| field.evidence_id.as_str()))
        })
        .collect();
    let all_calculation_ids: BTreeSet<&str> = report
        .calculations
        .iter()
        .map(|item| item.calculation_id.as_str())
        .collect();

    for claim in &report.claims {
        for reference in &claim.evidence_ids {
            if !all_evidence_ids.contains(reference.as_str()) {
                push_error(
                    &mut findings,
                    "missing_reference",
                    claim,
                    format!("引用 {reference} 不存在，不能用于支撑该结论"),
                );
            }
        }
        for reference in &claim.calculation_ids {
            if !all_calculation_ids.contains(reference.as_str()) {
                push_error(
                    &mut findings,
                    "missing_calculation",
                    claim,
                    format!("计算引用 {reference} 不存在或没有确定性计算记录"),
                );
            }
        }
        let can_be_unreferenced =
            matches!(claim.claim_type, ClaimType::Assumption | ClaimType::Unknown);
        if !can_be_unreferenced && claim.evidence_ids.is_empty() && claim.calculation_ids.is_empty()
        {
            push_error(
                &mut findings,
                "unsupported_claim",
                claim,
                "关键结论没有字段级证据或确定性计算；应补充引用，或明确标为【假设】/【未知】"
                    .to_string(),
            );
        }

        let fields = cited_fields(&claim.evidence_ids, evidence);
        // Assumption/unknown numbers are explicitly not asserted as observed
        // market facts (for example a user-provided 2万元 capital constraint).
        // They remain labelled in the report but do not require a provider
        // field. Every fact/calculation/inference number remains strict.
        if !matches!(claim.claim_type, ClaimType::Assumption | ClaimType::Unknown) {
            for quantity in extract_quantities(&remove_reference_markup(&claim.text)) {
                let matching = fields
                    .iter()
                    .copied()
                    .find(|field| quantity_matches_field(&quantity, field));
                if matching.is_none() {
                    let same_number_wrong_unit = fields.iter().copied().any(|field| {
                        quantity_numeric_match(&quantity, field)
                            && !units_compatible(quantity.kind, field.unit.as_deref())
                    });
                    push_error(
                        &mut findings,
                        if same_number_wrong_unit {
                            "unit_mismatch"
                        } else {
                            "unsupported_number"
                        },
                        claim,
                        if same_number_wrong_unit {
                            format!("数字“{}”与所引字段的单位或币种不一致", quantity.raw)
                        } else {
                            format!("数字“{}”无法在所引字段或确定性计算中复现", quantity.raw)
                        },
                    );
                }
            }
        }
        for field in fields {
            if field.blocking {
                push_error(
                    &mut findings,
                    "blocked_evidence",
                    claim,
                    format!("字段 {} 存在未解决冲突或质量阻断", field.field_path),
                );
            }
            if is_price_path(&field.field_path)
                && matches!(field.freshness.as_str(), "stale" | "expired")
            {
                push_error(
                    &mut findings,
                    "stale_price",
                    claim,
                    format!(
                        "价格字段 {} 已{}，必须重新取行情",
                        field.field_path,
                        if field.freshness == "expired" {
                            "过期"
                        } else {
                            "陈旧"
                        }
                    ),
                );
            }
            if claim.claim_type == ClaimType::Fact && field.source_tier == "discovery_only" {
                push_error(
                    &mut findings,
                    "discovery_is_not_evidence",
                    claim,
                    "搜索标题或摘要只可发现来源，不能发布为【事实】".to_string(),
                );
            }
        }
        if contains_unsupported_absolute(&claim.text)
            && !matches!(claim.claim_type, ClaimType::Assumption | ClaimType::Unknown)
        {
            push_error(
                &mut findings,
                "unsupported_absolute_language",
                claim,
                "使用了保证性或绝对化表述；必须改为有条件、可失效的判断".to_string(),
            );
        }
    }

    dedup_findings(&mut findings);
    let status = if findings
        .iter()
        .any(|finding| finding.severity == VerificationSeverity::Error)
    {
        VerificationStatus::Failed
    } else {
        VerificationStatus::Passed
    };
    VerificationResult {
        status,
        verifier_version: "report-verifier/v1".to_string(),
        verified_at,
        findings,
    }
}

fn push_error(
    findings: &mut Vec<VerificationFinding>,
    code: &str,
    claim: &ResearchClaim,
    message: String,
) {
    findings.push(VerificationFinding {
        code: code.to_string(),
        severity: VerificationSeverity::Error,
        claim_id: Some(claim.claim_id.clone()),
        message,
    });
}

fn dedup_findings(findings: &mut Vec<VerificationFinding>) {
    let mut seen = BTreeSet::new();
    findings.retain(|item| {
        seen.insert((
            item.code.clone(),
            item.claim_id.clone(),
            item.message.clone(),
        ))
    });
}

fn cited_fields<'a>(ids: &[String], evidence: &'a [Evidence]) -> Vec<&'a EvidenceField> {
    let ids: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    evidence
        .iter()
        .flat_map(|item| item.fields.iter())
        .filter(|field| ids.contains(field.evidence_id.as_str()))
        .collect()
}

fn parse_graded_line(line: &str) -> Option<(String, String)> {
    let line = clean_markdown_prefix(line);
    for grade in GRADES {
        let marker = format!("【{grade}】");
        if let Some(index) = line.find(&marker) {
            if index <= 4 {
                let text = line[index + marker.len()..].trim();
                if !text.is_empty() {
                    return Some(((*grade).to_string(), text.to_string()));
                }
            }
        }
    }
    None
}

fn claim_type_from_grade(grade: &str) -> ClaimType {
    match grade {
        "事实" => ClaimType::Fact,
        "计算" => ClaimType::Calculation,
        "外部" => ClaimType::External,
        "假设" => ClaimType::Assumption,
        "未知" => ClaimType::Unknown,
        _ => ClaimType::Inference,
    }
}

fn clean_markdown_prefix(line: &str) -> &str {
    let line = line
        .trim()
        .trim_start_matches('#')
        .trim_start()
        .trim_start_matches(['-', '*', '>', '•'])
        .trim_start();
    let digit_count = line.chars().take_while(char::is_ascii_digit).count();
    let after_digits = &line[digit_count..];
    if digit_count > 0
        && after_digits
            .chars()
            .next()
            .is_some_and(|ch| matches!(ch, '.' | ')' | '、'))
    {
        after_digits[after_digits.chars().next().unwrap().len_utf8()..].trim_start()
    } else {
        line
    }
}

fn visible_lines(text: &str) -> impl Iterator<Item = String> + '_ {
    let mut fenced = false;
    text.lines().filter_map(move |line| {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            return None;
        }
        (!fenced).then(|| line.to_string())
    })
}

fn looks_like_key_claim(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    extract_quantities(line)
        .iter()
        .any(|quantity| quantity.kind != QuantityKind::Plain)
        || (["价格", "评分", "市值", "营收", "利润", "目标价", "估值"]
            .iter()
            .any(|term| line.contains(term))
            && !extract_quantities(line).is_empty())
        || contains_unsupported_absolute(line)
        || lower.contains("target price")
        || line.contains("目标价")
}

fn is_plan_parameter_section(section: &str) -> bool {
    [
        "配置方案",
        "执行计划",
        "仓位计划",
        "分批计划",
        "三种情景",
        "情景方案",
        "风险预算",
    ]
    .iter()
    .any(|needle| section.contains(needle))
}

fn contains_unsupported_absolute(text: &str) -> bool {
    [
        "保证",
        "必然",
        "绝对",
        "稳赚",
        "肯定上涨",
        "不会下跌",
        "零风险",
        "100%确定",
    ]
    .iter()
    .any(|word| text.contains(word))
}

fn extract_refs(text: &str, label: &str) -> Vec<String> {
    let pattern = format!(
        r"(?:\[|〔|【)?{}[:：]\s*([A-Za-z0-9_.-]+)",
        regex::escape(label)
    );
    Regex::new(&pattern)
        .expect("static reference regex")
        .captures_iter(text)
        .filter_map(|capture| capture.get(1).map(|value| value.as_str().to_string()))
        .collect()
}

fn remove_reference_markup(text: &str) -> String {
    Regex::new(r"(?:\[|〔|【)?(?:证据|计算引用)[:：]\s*[A-Za-z0-9_.-]+(?:\]|〕|】)?")
        .expect("static reference markup regex")
        .replace_all(text, "")
        .into_owned()
}

/// Convert an internally auditable draft into ordinary user-facing Chinese.
///
/// Evidence IDs and configuration variable names remain available in the
/// structured report/diagnostic layer. They are implementation details and
/// must never be presented as prose to an ordinary investor.
pub fn sanitize_public_answer(text: &str) -> String {
    let mut answer = remove_reference_markup(text);
    let replacements = [
        ("research_global_transmission", "海外一手信息检索"),
        ("fetch_source_document", "原始资料核验"),
        ("compare_source_evidence", "多来源交叉核验"),
        ("research_disclosures", "公司公告检索"),
        ("research_news", "财经新闻检索"),
        ("search_web", "联网检索"),
        ("source_version_id", "原始资料版本"),
        ("document_revision_id", "资讯修订版本"),
        ("fact_id", "事实记录"),
        ("status=no_match", "未找到匹配内容"),
        ("total_documents=0", "暂未取得可核验原文"),
    ];
    for (internal, public) in replacements {
        answer = answer.replace(&format!("`{internal}`"), public);
        answer = answer.replace(internal, public);
    }
    answer = Regex::new(r"(?i)\b(?:evf|ev|calc|claim|data)_[a-z0-9_.-]+\b")
        .expect("static internal id regex")
        .replace_all(&answer, "")
        .into_owned();
    answer = Regex::new(
        r"(?i)\b[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)*_(?:API_KEY|TOKEN|SECRET|PASSWORD|PWD|USER_AGENT)\b",
    )
    .expect("static credential variable regex")
    .replace_all(&answer, "相应数据源配置")
    .into_owned();
    answer = answer
        .replace("〔〕", "")
        .replace("【】", "")
        .replace("[]", "");
    Regex::new(r"[ \t]+([，。；、：！？])")
        .expect("static punctuation cleanup regex")
        .replace_all(answer.trim_end(), "$1")
        .into_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuantityKind {
    Plain,
    Percent,
    Money,
    Date,
    Code,
    Shares,
    Multiple,
}

#[derive(Debug, Clone)]
struct Quantity {
    raw: String,
    normalized: Option<f64>,
    text: Option<String>,
    kind: QuantityKind,
}

fn extract_quantities(text: &str) -> Vec<Quantity> {
    let expression = Regex::new(
        r"(?x)
        (?:20\d{2}[-/.年]\d{1,2}(?:[-/.月]\d{1,2}日?)?)
        |(?:\b\d{6}\b)
        |(?:[-+]?\d[\d,]*(?:\.\d+)?\s*(?:万亿|亿元|万元|亿|万|元|%|％|倍|股|手|点)?)",
    )
    .expect("static quantity regex");
    expression
        .find_iter(text)
        .filter_map(|found| parse_quantity(found.as_str()))
        .collect()
}

fn parse_quantity(raw: &str) -> Option<Quantity> {
    let compact = raw.trim().replace([',', ' '], "");
    if compact.starts_with("20")
        && (compact.contains('-')
            || compact.contains('/')
            || compact.contains('.')
            || compact.contains('年'))
    {
        return Some(Quantity {
            raw: raw.to_string(),
            normalized: None,
            text: Some(normalize_date(&compact)),
            kind: QuantityKind::Date,
        });
    }
    if compact.len() == 6 && compact.chars().all(|ch| ch.is_ascii_digit()) {
        return Some(Quantity {
            raw: raw.to_string(),
            normalized: None,
            text: Some(compact),
            kind: QuantityKind::Code,
        });
    }
    let unit_start = compact
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.' && ch != '-' && ch != '+')
        .unwrap_or(compact.len());
    let number = compact[..unit_start].parse::<f64>().ok()?;
    let suffix = &compact[unit_start..];
    let (factor, kind) = match suffix {
        "万亿" => (1e12, QuantityKind::Money),
        "亿元" | "亿" => (1e8, QuantityKind::Money),
        "万元" => (1e4, QuantityKind::Money),
        "万" => (1e4, QuantityKind::Money),
        "元" => (1.0, QuantityKind::Money),
        "%" | "％" => (1.0, QuantityKind::Percent),
        "股" | "手" => (1.0, QuantityKind::Shares),
        "倍" => (1.0, QuantityKind::Multiple),
        _ => (1.0, QuantityKind::Plain),
    };
    Some(Quantity {
        raw: raw.to_string(),
        normalized: Some(number * factor),
        text: None,
        kind,
    })
}

fn normalize_date(value: &str) -> String {
    value
        .replace(['年', '/', '.'], "-")
        .replace('月', "-")
        .replace('日', "")
}

fn best_field_match<'a>(
    quantity: &Quantity,
    evidence: &'a [Evidence],
    allowed_ids: Option<&BTreeSet<&str>>,
) -> Option<(&'a EvidenceField, usize)> {
    evidence
        .iter()
        .flat_map(|item| item.fields.iter())
        .filter(|field| {
            allowed_ids.is_none_or(|allowed| allowed.contains(field.evidence_id.as_str()))
        })
        .filter(|field| quantity_matches_field(quantity, field))
        .map(|field| {
            let score = usize::from(field.freshness == "fresh") * 4
                + usize::from(!field.blocking) * 4
                + usize::from(field.source_tier == "primary") * 2
                + usize::from(field.source_tier == "provider");
            (field, score)
        })
        .max_by_key(|(_, score)| *score)
}

fn quantity_matches_field(quantity: &Quantity, field: &EvidenceField) -> bool {
    quantity_numeric_match(quantity, field)
        && units_compatible(quantity.kind, field.unit.as_deref())
        || match (&quantity.text, &field.value) {
            (Some(expected), Value::String(actual)) => normalize_date(actual) == *expected,
            (Some(expected), Value::Number(actual)) if quantity.kind == QuantityKind::Code => {
                actual.to_string() == *expected
            }
            _ => false,
        }
}

fn quantity_numeric_match(quantity: &Quantity, field: &EvidenceField) -> bool {
    let Some(expected) = quantity.normalized else {
        return false;
    };
    let Some(mut actual) = value_as_f64(&field.value) else {
        return false;
    };
    if quantity.kind == QuantityKind::Percent && field.unit.as_deref() == Some("percent") {
        // Providers use either ratios (0.15) or percentage points (15).
        if actual.abs() <= 1.0 && expected.abs() > 1.0 {
            actual *= 100.0;
        }
    }
    approx_equal(expected, actual)
}

fn value_as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().replace(',', "").parse().ok(),
        _ => None,
    }
}

fn units_compatible(kind: QuantityKind, unit: Option<&str>) -> bool {
    match kind {
        QuantityKind::Money => matches!(unit, Some("cny") | None),
        QuantityKind::Percent => matches!(unit, Some("percent") | None),
        QuantityKind::Shares => matches!(unit, Some("shares") | None),
        QuantityKind::Multiple | QuantityKind::Plain => unit != Some("percent"),
        QuantityKind::Date | QuantityKind::Code => true,
    }
}

fn approx_equal(left: f64, right: f64) -> bool {
    let tolerance = left.abs().max(right.abs()).max(1.0) * 1e-6;
    (left - right).abs() <= tolerance
}

fn is_price_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.contains("price")
        || path.ends_with("/open")
        || path.ends_with("/high")
        || path.ends_with("/low")
        || path.ends_with("/close")
        || path.contains("target_price")
}

fn dedup(values: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

fn stable_id(prefix: &str, seed: &str) -> String {
    // FNV-1a is used as a compact deterministic address, not as a security
    // primitive. The full cache key + field path remain available for audit.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in seed.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{prefix}_{hash:016x}")
}

/// Versions persisted with the report for reproducibility diagnostics.
pub fn report_versions(
    report: &AgentReport,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let tool_versions = report
        .evidence
        .iter()
        .map(|item| (item.tool.clone(), item.tool_version.clone()))
        .collect();
    let data_versions = report
        .evidence
        .iter()
        .map(|item| (item.evidence_id.clone(), item.data_version.clone()))
        .collect();
    (tool_versions, data_versions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn evidence(summary: Value) -> Vec<Evidence> {
        vec![index_tool_evidence(
            "get_quote",
            "get_quote:600519",
            "eastmoney",
            "2026-08-23T01:00:00Z",
            &summary,
        )]
    }

    fn tagged_report(text: &str, evidence: Vec<Evidence>) -> AgentReport {
        assemble_report("t1", text, evidence, 1_777_000_000)
    }

    #[test]
    fn parses_graded_conclusions() {
        let text = "分析如下\n- 【事实】收盘价 1800.5 元\n【计算】综合评分 72 分\n【推断】短线偏强\n备注行不解析";
        let c = parse_conclusions(text);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].grade, "事实");
        assert_eq!(c[1].grade, "计算");
        assert_eq!(c[2].grade, "推断");
        assert_eq!(c[2].text, "短线偏强");
    }

    #[test]
    fn answer_is_used_without_boilerplate() {
        let report = assemble_report("t1", "结论。【事实】x\n", vec![], 0);
        assert_eq!(report.answer, "结论。【事实】x");
        assert!(!report.answer.contains("免责声明"));
    }

    #[test]
    fn public_answer_hides_internal_research_metadata() {
        let draft = "【推断】金价偏强〔证据:evf_5e298283〕；`research_news` 返回 status=no_match，`research_global_transmission` 返回 total_documents=0，建议配置 BLS_API_KEY 后再调用 fetch_source_document，并核对 source_version_id 与 fact_id。";
        let answer = sanitize_public_answer(draft);
        assert!(answer.contains("金价偏强"));
        assert!(answer.contains("财经新闻检索"));
        assert!(answer.contains("海外一手信息检索"));
        assert!(answer.contains("相应数据源配置"));
        for internal in [
            "evf_",
            "research_news",
            "research_global_transmission",
            "status=no_match",
            "total_documents=0",
            "BLS_API_KEY",
            "fetch_source_document",
            "source_version_id",
            "fact_id",
        ] {
            assert!(!answer.contains(internal), "leaked {internal}: {answer}");
        }
    }

    #[test]
    fn report_roundtrip_includes_versions_and_verification() {
        let evidence = evidence(
            json!({"close": 1800.5, "data_quality": {"freshness": "fresh", "allow_deterministic_compute": true}}),
        );
        let report = tagged_report("【事实】收盘价 1800.5 元", evidence);
        assert_eq!(report.conclusions.len(), 1);
        assert_eq!(report.research.schema_version, "astock-research-report/v1");
        assert!(report.research.verification.passed());
        let json = serde_json::to_string(&report).unwrap();
        let back: AgentReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
    }

    #[test]
    fn fake_number_is_blocked() {
        let report = tagged_report(
            "【事实】最新价 11 元",
            evidence(
                json!({"price": 10.0, "data_quality": {"freshness": "fresh", "allow_deterministic_compute": true}}),
            ),
        );
        assert_eq!(
            report.research.verification.status,
            VerificationStatus::Failed
        );
        assert!(report
            .research
            .verification
            .findings
            .iter()
            .any(|item| item.code == "unsupported_number"));
    }

    #[test]
    fn verified_subset_keeps_supported_claims_and_omits_bad_numbers() {
        let items = evidence(
            json!({"price": 10.0, "data_quality": {"freshness": "fresh", "allow_deterministic_compute": true}}),
        );
        let report = tagged_report(
            "【事实】最新价 10 元\n【推断】未经验证的目标价 99 元",
            items.clone(),
        );
        assert_eq!(
            report.research.verification.status,
            VerificationStatus::Failed
        );

        let fallback = verified_subset_answer(&report).expect("one claim is publishable");
        assert!(fallback.contains("最新价 10 元"));
        assert!(!fallback.contains("目标价 99 元"));
        assert!(fallback.contains("已自动省略"));
        let repaired = tagged_report(&fallback, items);
        assert!(
            repaired.research.verification.passed(),
            "{:?}",
            repaired.research.verification.findings
        );
    }

    #[test]
    fn verified_subset_returns_safe_answer_when_every_claim_is_blocked() {
        let items = evidence(
            json!({"price": 10.0, "data_quality": {"freshness": "fresh", "allow_deterministic_compute": true}}),
        );
        let report = tagged_report("【事实】目标价 99 元", items.clone());
        assert!(!report.research.verification.passed());

        let fallback = verified_subset_answer(&report).expect("safe empty result");
        assert!(fallback.contains("本轮暂无可发布结论"));
        assert!(fallback.contains("1 条草稿结论"));
        assert!(!fallback.contains("目标价 99 元"));
        let repaired = tagged_report(&fallback, items);
        assert!(
            repaired.research.verification.passed(),
            "{:?}",
            repaired.research.verification.findings
        );
    }

    #[test]
    fn execution_plan_parameters_are_assumptions_not_fake_market_facts() {
        let report = tagged_report(
            "## 执行计划\n\n| 标的 | 仓位 | 股数 |\n|---|---:|---:|\n| 计划A | 35% | 200股 |",
            Vec::new(),
        );
        assert!(report.research.verification.passed());
        assert!(report
            .research
            .claims
            .iter()
            .all(|claim| claim.claim_type == ClaimType::Assumption));
    }

    #[test]
    fn bad_reference_is_blocked() {
        let report = tagged_report(
            "【事实】最新价 10 元〔证据:evf_missing〕",
            evidence(
                json!({"price": 10.0, "data_quality": {"freshness": "fresh", "allow_deterministic_compute": true}}),
            ),
        );
        assert!(report
            .research
            .verification
            .findings
            .iter()
            .any(|item| item.code == "missing_reference"));
    }

    #[test]
    fn stale_price_is_blocked() {
        let report = tagged_report(
            "【事实】最新价 10 元",
            evidence(
                json!({"price": 10.0, "data_quality": {"freshness": "stale", "allow_deterministic_compute": true}}),
            ),
        );
        assert!(report
            .research
            .verification
            .findings
            .iter()
            .any(|item| item.code == "stale_price"));
    }

    #[test]
    fn unit_mismatch_is_blocked() {
        let items = evidence(
            json!({"return_pct": 10.0, "data_quality": {"freshness": "fresh", "allow_deterministic_compute": true}}),
        );
        let field_id = items[0]
            .fields
            .iter()
            .find(|field| field.field_path == "/return_pct")
            .unwrap()
            .evidence_id
            .clone();
        let report = tagged_report(&format!("【事实】最新价 10 元〔证据:{field_id}〕"), items);
        assert!(report
            .research
            .verification
            .findings
            .iter()
            .any(|item| item.code == "unit_mismatch"));
    }

    #[test]
    fn reasonable_amount_formatting_passes() {
        let report = tagged_report(
            "【事实】成交额为 2 亿元",
            evidence(
                json!({"amount": 200_000_000.0, "data_quality": {"freshness": "fresh", "allow_deterministic_compute": true}}),
            ),
        );
        assert!(
            report.research.verification.passed(),
            "{:?}",
            report.research.verification.findings
        );
    }

    #[test]
    fn user_constraint_number_passes_only_when_explicitly_an_assumption() {
        let report = tagged_report("【假设】按用户提供的 2万元 可用资金规划仓位", vec![]);
        assert!(
            report.research.verification.passed(),
            "{:?}",
            report.research.verification.findings
        );
        assert_eq!(report.research.assumptions.len(), 1);

        let asserted = tagged_report("【事实】可用资金为 2万元", vec![]);
        assert_eq!(
            asserted.research.verification.status,
            VerificationStatus::Failed
        );
    }

    #[test]
    fn counter_evidence_and_invalidation_are_preserved() {
        let report = tagged_report(
            "## 反方证据\n【假设】行业需求可能回落\n## 失效条件\n【未知】若公告修订则需重算",
            vec![],
        );
        assert_eq!(report.research.counter_evidence.len(), 1);
        assert_eq!(report.research.invalidation.len(), 1);
        assert_eq!(report.research.unknowns.len(), 1);
    }
}
