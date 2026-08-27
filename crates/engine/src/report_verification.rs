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

/// Does this pair of registrations of the same identifier actually disagree?
///
/// Only a difference in what the fact *asserts* is a conflict. Two registrations
/// of the same identifier carrying the same value from the same source at
/// different retrieval moments are the same observation seen twice, which happens
/// routinely because several research tools legitimately fetch the same field.
///
/// Measured on the failing live run: 476 identifiers were reported as conflicting
/// and in **every one of them the only differing field was `observed_at`**. A
/// constant such as `/adjustment = "qfq"` from JoinQuant, fetched thirty seconds
/// apart, was treated as contradictory evidence. That produced hundreds of
/// blocking findings with no bearing on the report's correctness.
///
/// A genuine disagreement — the same identifier asserting a different value, unit
/// or source — is still a conflict and still blocks when cited.
fn materially_disagrees(left: &EvidenceFact, right: &EvidenceFact) -> bool {
    left.value != right.value
        || left.source != right.source
        || left.path != right.path
        || left.quality_blocking != right.quality_blocking
        || left.source_version_id != right.source_version_id
}

/// Is this fact a deterministic calculation result rather than an observation?
///
/// A derived value has no observation time: it is computed from inputs that carry
/// their own timestamps. Requiring `observed_at` of a calculation conflates
/// calculation provenance with observation provenance. All 31 facts that tripped
/// `evidence_time_missing` on the live run came from `astock-compute`, with leaf
/// paths such as `kind`, `value`, `fuel_used` and `program_sha256`.
///
/// Calculation results remain fully verifiable — their numeric value must still
/// reproduce a cited claim — they are simply not judged against observation
/// timestamps. A calculation may not, however, support a current/latest claim on
/// its own; that requirement falls on the observed inputs it was derived from.
fn is_derived_calculation(fact: &EvidenceFact) -> bool {
    fact.source == "astock-compute"
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
                            if materially_disagrees(existing, &fact) {
                                conflicts.insert(fact.evidence_id);
                            } else if fact.observed_at > existing.observed_at {
                                // Same assertion seen again, more recently. Keep
                                // the freshest observation so current/latest
                                // claims are judged against the newest timestamp.
                                facts.insert(fact.evidence_id.clone(), fact);
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

/// Blank out digit runs that are not financial quantities.
///
/// The verifier's job is to reject unsupported *financial claims*, not any
/// character sequence containing digits. On the failing live run 20 of 121
/// `numeric_claim_without_evidence` findings were raised against a security code,
/// a calendar date, a Markdown section number or a period label — text that
/// asserts no quantity and therefore cannot be unsupported.
///
/// Masking replaces each with spaces so surrounding real numbers on the same line
/// are still extracted and still checked. This narrows what counts as a claim; it
/// does not relax how a claim is verified.
fn mask_non_financial_tokens(line: &str) -> String {
    // Ordered widest-first so a date inside a heading is masked once.
    //
    // `regex` supports no look-around, so the security-code rule brackets the
    // code with optional non-digit context and restores whatever it captured
    // around the masked run. An earlier draft used `(?<![\d.])`, which fails to
    // compile — and because the loop skipped unusable patterns, it would have
    // silently stopped masking security codes. Patterns are now asserted.
    static PATTERNS: &[&str] = &[
        // Calendar dates and fiscal periods: 2026-08-26, 2026年, 8月, 26日, 2024Q3.
        r"\d{4}-\d{2}-\d{2}|\d{4}/\d{1,2}/\d{1,2}|\d{4}\s*年|\d{1,2}\s*月|\d{1,2}\s*日|\d{4}\s*Q[1-4]|\bQ[1-4]\b",
        // Reporting-period labels: 2025 全年, 2026 上半年, 2024 年度, 2025 财年.
        //
        // `\d{4}\s*年` above only catches a year written immediately before 年. A
        // reporting period names a window, asserts no quantity, and appears in
        // almost every fundamentals claim; a live moderate run was blocked by
        // `2025 全年营业总收入` and `2026Q1 末归母权益` being read as figures.
        r"\d{4}\s*(?:全年|年度|年报|中报|季报|上半年|下半年|财年|财报)",
        // Clock times, including exchange session boundaries.
        r"\d{1,2}:\d{2}(?::\d{2})?",
        // Markdown headings and ordered-list markers.
        r"(?m)^\s{0,3}#{1,6}\s",
        r"(?m)^\s{0,3}\d{1,3}[.、)]\s",
        // Chinese section numbering: 第一步, 第3节, 第二部分.
        r"第\s*[0-9一二三四五六七八九十百]+\s*[步章节条部分项]",
        // Window and horizon labels: 6个月, 20个交易日, 5年期, 3周, 近3年.
        //
        // A one or two digit count before 年 is a duration; a calendar year in this
        // corpus is four digits and is masked by the date rule above.
        r"\d+\s*(?:个月|个交易日|个季度|年期|周|天|日线|分钟)|\d{1,2}\s*年",
    ];
    let mut masked = line.to_owned();
    for pattern in PATTERNS {
        let regex = Regex::new(pattern)
            .expect("non-financial masking patterns are compile-time constants and must compile");
        masked = regex
            .replace_all(&masked, |captures: &regex::Captures<'_>| {
                " ".repeat(captures.get(0).map_or(0, |m| m.as_str().chars().count()))
            })
            .into_owned();
    }
    mask_security_codes(&masked)
}

/// Blank out six-digit A-share codes without look-around.
///
/// A code is a six-digit run with a known market prefix that is not part of a
/// longer number and not a decimal fraction. `601899` is an identifier; `320506`
/// as part of `320,506,024,370` is a quantity, so digit-adjacency and a
/// neighbouring decimal point both disqualify a match.
fn mask_security_codes(line: &str) -> String {
    const PREFIXES: [&str; 8] = ["60", "68", "00", "30", "43", "83", "87", "88"];
    let characters: Vec<char> = line.chars().collect();
    let mut output = characters.clone();
    let mut index = 0usize;
    while index + 6 <= characters.len() {
        let window: String = characters[index..index + 6].iter().collect();
        let is_code = window.chars().all(|c| c.is_ascii_digit())
            && PREFIXES.iter().any(|prefix| window.starts_with(prefix));
        if is_code {
            let before_ok = index == 0
                || !(characters[index - 1].is_ascii_digit() || characters[index - 1] == '.');
            let after = characters.get(index + 6);
            let after_ok = after.is_none_or(|c| !(c.is_ascii_digit() || *c == '.'));
            if before_ok && after_ok {
                for slot in output.iter_mut().skip(index).take(6) {
                    *slot = ' ';
                }
                index += 6;
                continue;
            }
        }
        index += 1;
    }
    output.into_iter().collect()
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
        Some("万亿") => value *= 1_000_000_000_000.0,
        Some("%") => value /= 100.0,
        _ => {}
    }
    Some(value)
}

fn approximately_equal(left: f64, right: f64) -> bool {
    let tolerance = 1e-8_f64.max(left.abs().max(right.abs()) * 1e-6);
    (left - right).abs() <= tolerance
}

fn fact_supports_numeral(fact: &EvidenceFact, numeral: &ReportNumeral) -> bool {
    if let Some(value) = numeric_value(&fact.value) {
        if numeral.supported_by(value) {
            return true;
        }
    }
    let raw = numeral.raw.as_str();
    let parsed = numeral.value;
    let unit = numeral.unit.as_deref();
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

/// Is the digit run at `start` preceded by a minus sign that is really a sign?
///
/// The number pattern deliberately matches no sign, because a hyphen in a report is
/// usually a separator — `20-30 元`, `2026-08-26`. The consequence was that a
/// negative quantity could **never** be reproduced: a maximum drawdown registered
/// as `-0.0999` was extracted from the report as `0.0999`, compared against
/// `-0.0999`, and reported as `numeric_claim_not_reproduced`. Every signed figure a
/// deterministic calculation produces hit this.
///
/// A leading `-` counts as a sign only when what precedes it is not itself part of a
/// number, which keeps `20-30` two positive quantities and makes `=-0.0999` and
/// `下跌 -15%` negative. An unsigned token is still compared unsigned, so this adds
/// no new way for a claim to pass: it only lets a correctly signed claim match the
/// evidence it cites.
fn negative_sign_before(text: &str, start: usize) -> bool {
    let mut characters = text[..start].chars().rev();
    match characters.next() {
        Some('-') | Some('\u{2212}') => {}
        _ => return false,
    }
    // Skip nothing else: a sign binds tightly to its number. Whatever sits before
    // the sign decides whether it is a sign or a separator.
    match characters.next() {
        Some(previous) => !previous.is_ascii_digit() && previous != '.',
        // Start of line: `-5%` is negative.
        None => true,
    }
}

/// One financial quantity found in a line of report prose.
///
/// Produced by [`financial_numerals`] and matched by [`ReportNumeral::supported_by`].
/// Both the verifier and the Runtime's report contract use these, so "what counts as
/// a financial claim" and "when does evidence support it" have exactly one
/// implementation. Two implementations of that rule would drift, and the drift would
/// show up as a report that validation accepted and verification refused.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportNumeral {
    /// The characters as written, without sign or unit.
    pub raw: String,
    /// Scaled and signed value: `-3.5%` is `-0.035`, `2万` is `20000`.
    pub value: f64,
    /// The unit suffix, when one was written.
    pub unit: Option<String>,
}

impl ReportNumeral {
    /// Would this evidence value support the quantity as written?
    ///
    /// Mirrors the verifier's numeric acceptance exactly, including the percentage
    /// convention: evidence recording `3.5` supports a written `3.5%`, because
    /// sources publish percentages both scaled and unscaled.
    pub fn supported_by(&self, value: f64) -> bool {
        if approximately_equal(value, self.value) {
            return true;
        }
        self.unit.as_deref() == Some("%") && approximately_equal(value, self.value * 100.0)
    }
}

/// Extract the financial quantities asserted by one line of report prose.
///
/// Narrows what counts as a claim before anything is judged: a security code, a
/// calendar date, a Markdown heading number, a clock time and a window label assert
/// no quantity, so they are masked out first. Twenty of 121 findings on a live run
/// were raised against exactly those.
pub fn financial_numerals(line: &str) -> Vec<ReportNumeral> {
    let claim_text = mask_non_financial_tokens(&strip_citation_tokens(line));
    let Ok(pattern) = numeric_pattern() else {
        return Vec::new();
    };
    pattern
        .captures_iter(&claim_text)
        .filter_map(|claim| {
            let matched = claim.get(0)?;
            if identifier_adjacent(&claim_text, matched.start(), matched.end()) {
                return None;
            }
            let raw = claim.name("number")?.as_str();
            let unit = claim.name("unit").map(|value| value.as_str());
            let negative = negative_sign_before(&claim_text, matched.start());
            let value =
                parse_claim_number(raw, unit).map(|value| if negative { -value } else { value })?;
            Some(ReportNumeral {
                raw: raw.to_owned(),
                value,
                unit: unit.map(str::to_owned),
            })
        })
        .collect()
}

/// The quantity pattern.
///
/// Whitespace is allowed before the magnitude suffix. Chinese financial prose writes
/// both `79.87亿元` and `79.87 亿元`, and requiring adjacency made the spaced form
/// parse as `79.87` — a number three orders of magnitude from what the text says,
/// which no evidence could reproduce. Reading the suffix makes the comparison
/// stricter, not looser: a figure written `2,314,388 万手` is now judged as
/// 23.1 billion rather than as 2.3 million.
fn numeric_pattern() -> Result<Regex, regex::Error> {
    Regex::new(r"(?P<number>\d{1,3}(?:,\d{3})+(?:\.\d+)?|\d+(?:\.\d+)?)\s*(?P<unit>%|万亿|万|亿)?")
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
    let mut cited = BTreeSet::new();
    let mut numeric_claims = 0usize;

    for (line_index, line) in report.lines().enumerate() {
        let ids = citations(line);
        for id in &ids {
            cited.insert(id.clone());
            // A conflicting identifier only invalidates a report that relies on
            // it. Registry duplicates the report never cites cannot make its
            // claims wrong, and on the live run 473 of 476 conflicts were never
            // cited, so reporting them blocked publication over evidence the
            // report did not use. Citing a genuinely conflicting fact still
            // fails closed, here.
            if registry_conflicts.contains(id) {
                findings.insert(format!("invalid_or_conflicting_evidence_id:{id}"));
                continue;
            }
            match facts.get(id) {
                None => {
                    findings.insert(format!("unknown_evidence_id:{id}"));
                }
                Some(fact) if fact.quality_blocking => {
                    findings.insert(format!("quality_blocking_evidence:{id}"));
                }
                // A deterministic calculation has no observation time; its
                // inputs carry the timestamps. Judging it against observation
                // semantics is a category error.
                Some(fact) if is_derived_calculation(fact) => {}
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
        // Extraction and matching both come from `financial_numerals` and
        // `ReportNumeral::supported_by`, which the Runtime's report contract also
        // uses. One implementation of "what is a financial claim" and "when is it
        // supported" means validation and verification cannot disagree; two would
        // drift, and the drift would show up as a report that validation accepted
        // and verification refused.
        //
        // `invalid_numeric_claim` is unreachable through this path: the pattern only
        // matches digit runs, which always parse. It is retained as a finding code
        // because the contract may still surface one from a different source.
        let claims = financial_numerals(line);
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
            if !ids.iter().any(|id| {
                facts
                    .get(id)
                    .is_some_and(|fact| fact_supports_numeral(fact, &claim))
            }) {
                findings.insert(format!(
                    "numeric_claim_not_reproduced:line_{}:{}{}",
                    line_index + 1,
                    if claim.value < 0.0 { "-" } else { "" },
                    claim.raw
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
        // Three genuine financial quantities are checked: the 2万 budget, the
        // 12.34 price and the 12.34万 volume.
        //
        // This previously expected 7, which counted the security code `000001`
        // and the three digit groups of the date `2026-08-25` as financial
        // claims. Those assert no quantity, so counting them overstated what had
        // been verified and made any report mentioning a code or a date look
        // unsupported. The report still passes; only the count of things that are
        // actually claims changed.
        assert_eq!(result["numeric_claims_checked"], 3);
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

    /// A negative quantity must be reproducible against its negative evidence.
    ///
    /// The number pattern matches no sign, so a drawdown registered as `-0.0999`
    /// was extracted from the report as `0.0999` and reported as unreproduced. Every
    /// signed figure a deterministic calculation produces hit this, which made a
    /// correct report unpublishable.
    #[test]
    fn a_negative_quantity_is_reproduced_by_its_negative_evidence() {
        let mut signed = context(false);
        signed["evidence_registry"]["facts"] = json!([
            {"evidence_id":"evf_drawdown","path":"/execution/outputs/max_drawdown/value","value":-0.09999999999999998,"source":"astock-compute","observed_at":null,"source_version_id":"program-v1","quality_blocking":false},
            {"evidence_id":"evf_change","path":"/quote/change_pct","value":-3.5,"source":"tdx","observed_at":"2026-08-25T10:00:00+08:00","source_version_id":"quote-v1","quality_blocking":false},
            {"evidence_id":"evf_code","path":"/quote/symbol","value":"000001","source":"master","observed_at":"2026-08-25T10:00:00+08:00","source_version_id":"master-v1","quality_blocking":false},
            {"evidence_id":"evf_date","path":"/quote/date","value":"2026-08-25","source":"tdx","observed_at":"2026-08-25T10:00:00+08:00","source_version_id":"quote-v1","quality_blocking":false}
        ]);
        let result = verify(VerifyReportPayload {
            report: "最大回撤 max_drawdown=-0.09999999999999998【E:evf_drawdown】\n\
                     当日涨跌幅 -3.5%【E:evf_change】【E:evf_code】【E:evf_date】"
                .into(),
            context: signed,
            task_spec: task(),
        })
        .unwrap();
        let findings = result["findings"].as_array().unwrap();
        assert!(
            !findings.iter().any(|item| item
                .as_str()
                .unwrap()
                .starts_with("numeric_claim_not_reproduced")),
            "a signed figure must reproduce against signed evidence: {findings:?}"
        );
    }

    /// A hyphen between two numbers stays a separator, not a sign.
    #[test]
    fn a_range_separator_is_not_read_as_a_negative_sign() {
        assert!(!negative_sign_before("区间 20-30 元", "区间 20-".len()));
        assert!(!negative_sign_before("0.5-0.8", "0.5-".len()));
        // A sign after an operator, a space or at the start of a line is a sign.
        assert!(negative_sign_before(
            "max_drawdown=-0.1",
            "max_drawdown=-".len()
        ));
        assert!(negative_sign_before("涨跌幅 -3.5%", "涨跌幅 -".len()));
        assert!(negative_sign_before("-15", "-".len()));
    }

    /// A reporting-period label is not a financial quantity.
    ///
    /// `\d{4}\s*年` only catches a year written immediately before 年. A live moderate
    /// run was blocked because `2025 全年营业总收入`, `2026Q1 末归母权益` and `近 3 年`
    /// were read as unsupported figures. A period names a window and asserts no
    /// quantity, so counting one narrows what a claim is; it does not relax how a
    /// claim is verified.
    #[test]
    fn reporting_period_and_horizon_labels_are_not_treated_as_figures() {
        for label in [
            "2025 全年营业总收入",
            "2024 年度归母净利润",
            "2026 上半年营业收入",
            "2025 财年现金流",
            "近 3 年复合增速",
            "过去 5 年",
            "2026Q1 末归母权益",
        ] {
            assert!(
                financial_numerals(label).is_empty(),
                "`{label}` asserts no quantity, found {:?}",
                financial_numerals(label)
            );
        }
    }

    /// Narrowing must not swallow a real quantity that happens to sit near a period.
    #[test]
    fn a_real_quantity_beside_a_period_label_is_still_extracted() {
        let found = financial_numerals("2025 全年营业总收入 3490.79 亿元");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].raw, "3490.79");
        assert_eq!(found[0].unit.as_deref(), Some("亿"));
    }

    /// An unsigned claim still fails against negative evidence, as before.
    ///
    /// The sign fix removes a false positive; it must not become a way for a claim
    /// asserting a gain to be supported by evidence of a loss.
    #[test]
    fn an_unsigned_claim_is_not_supported_by_negative_evidence() {
        let mut signed = context(false);
        signed["evidence_registry"]["facts"] = json!([
            {"evidence_id":"evf_change","path":"/quote/change_pct","value":-3.5,"source":"tdx","observed_at":"2026-08-25T10:00:00+08:00","source_version_id":"quote-v1","quality_blocking":false}
        ]);
        let result = verify(VerifyReportPayload {
            report: "当日上涨 3.5%【E:evf_change】".into(),
            context: signed,
            task_spec: task(),
        })
        .unwrap();
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
