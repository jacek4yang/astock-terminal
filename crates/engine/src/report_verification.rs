//! Deterministic, fail-closed verification of model-authored research reports.

use astock_protocol::TaskSpec;
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

use super::{agent_context::EvidenceFact, ServiceError};

const MAX_REPORT_CHARS: usize = 120_000;
// Leave headroom for the report, TaskSpec and framed request envelope.
const MAX_CONTEXT_BYTES: usize = 6 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VerifyReportPayload {
    report: String,
    context: Value,
    task_spec: TaskSpec,
}

fn invalid(message: impl Into<String>) -> ServiceError {
    ServiceError::new("invalid_agent_report_verification", message, false)
}

fn collect_registries(
    value: &Value,
    facts: &mut BTreeMap<String, EvidenceFact>,
    conflicts: &mut BTreeSet<String>,
) {
    match value {
        Value::Object(object) => {
            if let Some(rows) = object
                .get("evidence_registry")
                .and_then(|registry| registry.get("facts"))
                .and_then(Value::as_array)
            {
                for row in rows {
                    if let Ok(fact) = serde_json::from_value::<EvidenceFact>(row.clone()) {
                        if !fact.evidence_id.starts_with("evf_") || fact.evidence_id.len() > 80 {
                            conflicts.insert(fact.evidence_id);
                        } else if let Some(existing) = facts.get(&fact.evidence_id) {
                            if existing != &fact {
                                conflicts.insert(fact.evidence_id);
                            }
                        } else {
                            facts.insert(fact.evidence_id.clone(), fact);
                        }
                    }
                }
            }
            for (key, child) in object {
                if key != "evidence_registry" {
                    collect_registries(child, facts, conflicts);
                }
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_registries(child, facts, conflicts);
            }
        }
        _ => {}
    }
}

fn citations(line: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find("【E:") {
        let after = &rest[start + "【E:".len()..];
        let Some(end) = after.find('】') else { break };
        let id = after[..end].trim();
        if !id.is_empty() {
            result.push(id.to_owned());
        }
        rest = &after[end + '】'.len_utf8()..];
    }
    result
}

fn strip_citation_tokens(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("【E:") {
        output.push_str(&rest[..start]);
        let after = &rest[start + "【E:".len()..];
        let Some(end) = after.find('】') else {
            output.push_str(&rest[start..]);
            return output;
        };
        output.push(' ');
        rest = &after[end + '】'.len_utf8()..];
    }
    output.push_str(rest);
    output
}

fn numeric_value(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        _ => None,
    }
}

fn parse_claim_number(number: &str, unit: Option<&str>) -> Option<f64> {
    let mut value = number.replace(',', "").parse::<f64>().ok()?;
    match unit {
        Some("万") => value *= 10_000.0,
        Some("亿") => value *= 100_000_000.0,
        Some("%") => value /= 100.0,
        _ => {}
    }
    Some(value)
}

fn approximately_equal(left: f64, right: f64) -> bool {
    let tolerance = 1e-8_f64.max(left.abs().max(right.abs()) * 1e-6);
    (left - right).abs() <= tolerance
}

fn fact_supports_token(fact: &EvidenceFact, raw: &str, parsed: f64, unit: Option<&str>) -> bool {
    if let Some(value) = numeric_value(&fact.value) {
        if approximately_equal(value, parsed) {
            return true;
        }
        if unit == Some("%") && approximately_equal(value, parsed * 100.0) {
            return true;
        }
    }
    fact.value.as_str().is_some_and(|value| {
        let normalized_value = value.replace(',', "");
        let normalized_raw = raw.replace(',', "");
        if let Ok(value) = normalized_value.parse::<f64>() {
            return approximately_equal(value, parsed)
                || (unit == Some("%") && approximately_equal(value, parsed * 100.0));
        }
        if unit.is_some() {
            return false;
        }
        if normalized_value == normalized_raw {
            return true;
        }
        let segmented_contract_path = [
            "date",
            "time",
            "published_at",
            "observed_at",
            "retrieved_at",
            "investment_horizon",
        ]
        .iter()
        .any(|segment| fact.path.to_ascii_lowercase().contains(segment));
        segmented_contract_path
            && normalized_value
                .split(|character: char| !character.is_ascii_digit())
                .any(|segment| segment == normalized_raw)
    })
}

fn identifier_adjacent(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    before.is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
        || after.is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
}

pub(super) fn verify(payload: VerifyReportPayload) -> Result<Value, ServiceError> {
    let report = payload.report.trim();
    if report.is_empty() || report.chars().count() > MAX_REPORT_CHARS {
        return Err(invalid("报告为空或超过 120000 字符上限"));
    }
    let context_bytes =
        serde_json::to_vec(&payload.context).map_err(|error| invalid(error.to_string()))?;
    if context_bytes.len() > MAX_CONTEXT_BYTES {
        return Err(invalid("研究上下文超过报告校验 IPC 安全预算"));
    }
    if payload.context.get("task_spec")
        != Some(
            &serde_json::to_value(&payload.task_spec)
                .map_err(|error| invalid(format!("任务合同无法序列化: {error}")))?,
        )
    {
        return Err(invalid("研究上下文与报告校验任务合同不一致"));
    }

    let mut facts = BTreeMap::new();
    let mut findings = BTreeSet::new();
    let mut registry_conflicts = BTreeSet::new();
    collect_registries(&payload.context, &mut facts, &mut registry_conflicts);
    for id in registry_conflicts {
        findings.insert(format!("invalid_or_conflicting_evidence_id:{id}"));
    }
    let mut cited = BTreeSet::new();
    let mut numeric_claims = 0usize;
    let numeric =
        Regex::new(r"(?P<number>\d{1,3}(?:,\d{3})+(?:\.\d+)?|\d+(?:\.\d+)?)(?P<unit>%|万|亿)?")
            .map_err(|error| invalid(error.to_string()))?;

    for (line_index, line) in report.lines().enumerate() {
        let ids = citations(line);
        for id in &ids {
            cited.insert(id.clone());
            match facts.get(id) {
                None => {
                    findings.insert(format!("unknown_evidence_id:{id}"));
                }
                Some(fact) if fact.quality_blocking => {
                    findings.insert(format!("quality_blocking_evidence:{id}"));
                }
                Some(fact)
                    if fact
                        .observed_at
                        .as_deref()
                        .is_none_or(|value| value.trim().is_empty()) =>
                {
                    findings.insert(format!("evidence_time_missing:{id}"));
                }
                Some(fact)
                    if fact
                        .source_version_id
                        .as_deref()
                        .is_none_or(|value| value.trim().is_empty()) =>
                {
                    findings.insert(format!("evidence_version_missing:{id}"));
                }
                Some(fact) if fact.source.trim().is_empty() => {
                    findings.insert(format!("evidence_source_missing:{id}"));
                }
                Some(_) => {}
            }
        }
        let claim_text = strip_citation_tokens(line);
        let claims = numeric
            .captures_iter(&claim_text)
            .filter(|claim| {
                let matched = claim.get(0).unwrap();
                !identifier_adjacent(&claim_text, matched.start(), matched.end())
            })
            .collect::<Vec<_>>();
        if claims.is_empty() {
            continue;
        }
        numeric_claims += claims.len();
        if ids.is_empty() {
            findings.insert(format!(
                "numeric_claim_without_evidence:line_{}",
                line_index + 1
            ));
            continue;
        }
        for claim in claims {
            let raw = claim.name("number").unwrap().as_str();
            let unit = claim.name("unit").map(|value| value.as_str());
            let Some(parsed) = parse_claim_number(raw, unit) else {
                findings.insert(format!(
                    "invalid_numeric_claim:line_{}:{raw}",
                    line_index + 1
                ));
                continue;
            };
            if !ids.iter().any(|id| {
                facts
                    .get(id)
                    .is_some_and(|fact| fact_supports_token(fact, raw, parsed, unit))
            }) {
                findings.insert(format!(
                    "numeric_claim_not_reproduced:line_{}:{raw}",
                    line_index + 1
                ));
            }
        }
    }

    let minimum_citations = if payload.task_spec.evidence_requirement == "standard" {
        4
    } else {
        8
    };
    if cited.len() < minimum_citations {
        findings.insert(format!(
            "insufficient_distinct_evidence:{}<{minimum_citations}",
            cited.len()
        ));
    }
    if numeric_claims == 0 {
        findings.insert("report_contains_no_verifiable_numeric_claims".into());
    }

    Ok(json!({
        "passed": findings.is_empty(),
        "findings": findings,
        "distinct_citations": cited.len(),
        "numeric_claims_checked": numeric_claims,
        "registry_facts": facts.len(),
        "verification_version": "engine-report-verifier-v1",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> TaskSpec {
        TaskSpec {
            objective: "2万元人工投资计划".into(),
            security_universe: vec!["000001".into()],
            as_of: "2026-08-25T10:00:00+08:00".into(),
            research_start: "2025-08-25".into(),
            research_end: "2026-08-25".into(),
            investment_horizon: "三个月".into(),
            comparison_benchmark: "000300".into(),
            output_type: "manual_plan".into(),
            evidence_requirement: "standard".into(),
        }
    }

    fn context(blocking: bool) -> Value {
        json!({
            "task_spec": task(),
            "evidence_registry": {"facts": [
                {"evidence_id":"evf_price","path":"/quote/price","value":12.34,"source":"tdx","observed_at":"2026-08-25T10:00:00+08:00","source_version_id":"quote-v1","quality_blocking":blocking},
                {"evidence_id":"evf_code","path":"/quote/symbol","value":"000001","source":"master","observed_at":"2026-08-25T10:00:00+08:00","source_version_id":"master-v1","quality_blocking":false},
                {"evidence_id":"evf_date","path":"/quote/date","value":"2026-08-25","source":"tdx","observed_at":"2026-08-25T10:00:00+08:00","source_version_id":"quote-v1","quality_blocking":false},
                {"evidence_id":"evf_volume","path":"/quote/volume","value":123400.0,"source":"tdx","observed_at":"2026-08-25T10:00:00+08:00","source_version_id":"quote-v1","quality_blocking":false},
                {"evidence_id":"evf_user_capital","path":"/task/capital","value":20000.0,"source":"user_task_spec","observed_at":"2026-08-25T10:00:00+08:00","source_version_id":"task-spec-v1","quality_blocking":false}
            ]}
        })
    }

    #[test]
    fn accepts_reproduced_numbers_and_user_constraints() {
        let report = "预算为2万元【E:evf_user_capital】。\n000001价格12.34元【E:evf_code】【E:evf_price】，成交量12.34万【E:evf_volume】，日期2026-08-25【E:evf_date】。";
        let result = verify(VerifyReportPayload {
            report: report.into(),
            context: context(false),
            task_spec: task(),
        })
        .unwrap();
        assert_eq!(result["passed"], true);
        assert_eq!(result["numeric_claims_checked"], 7);
    }

    #[test]
    fn rejects_unknown_blocked_and_unreproduced_numbers() {
        let report = "价格99.00元【E:evf_price】，目标88元【E:missing】。";
        let result = verify(VerifyReportPayload {
            report: report.into(),
            context: context(true),
            task_spec: task(),
        })
        .unwrap();
        assert_eq!(result["passed"], false);
        let findings = result["findings"].as_array().unwrap();
        assert!(findings.iter().any(|item| item
            .as_str()
            .unwrap()
            .starts_with("quality_blocking_evidence")));
        assert!(findings
            .iter()
            .any(|item| item.as_str().unwrap().starts_with("unknown_evidence_id")));
        assert!(findings.iter().any(|item| item
            .as_str()
            .unwrap()
            .starts_with("numeric_claim_not_reproduced")));
    }

    #[test]
    fn evidence_identifier_hash_digits_are_not_treated_as_claims() {
        assert_eq!(
            strip_citation_tokens("价格12.34【E:evf_0123abcd】"),
            "价格12.34 "
        );
        assert_eq!(citations("价格12.34【E:evf_0123abcd】"), ["evf_0123abcd"]);
    }

    #[test]
    fn a_short_amount_is_not_reproduced_by_incidental_date_digits() {
        let mut only_date = context(false);
        only_date["evidence_registry"]["facts"] = json!([
            {"evidence_id":"evf_date","path":"/quote/date","value":"2026-08-25","source":"tdx","observed_at":"2026-08-25T10:00:00+08:00","source_version_id":"quote-v1","quality_blocking":false},
            {"evidence_id":"evf_code","path":"/quote/symbol","value":"000001","source":"master","observed_at":"2026-08-25T10:00:00+08:00","source_version_id":"master-v1","quality_blocking":false},
            {"evidence_id":"evf_extra_1","path":"/quote/date","value":"2026-08-24","source":"tdx","observed_at":"2026-08-25T10:00:00+08:00","source_version_id":"quote-v1","quality_blocking":false},
            {"evidence_id":"evf_extra_2","path":"/quote/date","value":"2026-08-23","source":"tdx","observed_at":"2026-08-25T10:00:00+08:00","source_version_id":"quote-v1","quality_blocking":false}
        ]);
        let result = verify(VerifyReportPayload {
            report: "投入2万元【E:evf_date】【E:evf_code】【E:evf_extra_1】【E:evf_extra_2】"
                .into(),
            context: only_date,
            task_spec: task(),
        })
        .unwrap();
        assert_eq!(result["passed"], false);
        assert!(result["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item
                .as_str()
                .unwrap()
                .starts_with("numeric_claim_not_reproduced")));
    }

    #[test]
    fn rejects_mismatched_task_contract() {
        let mut mismatched = task();
        mismatched.as_of = "2026-08-24T10:00:00+08:00".into();
        let error = verify(VerifyReportPayload {
            report: "预算为2万元【E:evf_user_capital】".into(),
            context: context(false),
            task_spec: mismatched,
        })
        .unwrap_err();
        assert_eq!(error.code, "invalid_agent_report_verification");
    }
}
