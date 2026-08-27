//! Structured finalization: budgets, and repair by claim rather than by rewrite.
//!
//! Two failures of the previous free-form path are addressed here.
//!
//! **Undirected repair.** A live run returned 1,178 textual findings into the model
//! context and asked for a full rewrite. The loop never converged, because nothing
//! told the model *which claim* was wrong or *what would fix it*. Repair here is
//! addressed to a claim identifier and carries a specific action; the model changes
//! the affected claims and resubmits, and unaffected claims stay untouched.
//!
//! **One budget for everything.** A single `max_model_rounds` cannot express
//! "research widely, but do not attempt the report a dozen times". Research rounds
//! and finalization attempts are counted separately, so exhausting one does not
//! silently consume the other, and running out of repair attempts fails closed
//! instead of quietly publishing.
//!
//! Nothing here relaxes a gate. Every path out of a failed submission either
//! produces a corrected draft that passes the same validation and the same
//! independent verifier, or refuses to publish.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::report::{DraftProblem, VerifiedReportDraft};

/// Problems described in one repair response.
///
/// Enough to fix a report in one round, few enough that a pathological draft
/// cannot flood the context. The count of everything found is always reported, so
/// truncation is visible rather than misleading.
pub const MAX_REPORTED_PROBLEMS: usize = 40;

/// Claims addressed by one repair response.
pub const MAX_REPAIR_TARGETS: usize = 24;

/// How a phase's budget was consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExhaustionReason {
    /// Every permitted attempt was used.
    BudgetSpent,
    /// The model resubmitted a draft it had already been told was invalid.
    NoProgress,
}

impl ExhaustionReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::BudgetSpent => "finalization_budget_spent",
            Self::NoProgress => "no_progress_identical_resubmission",
        }
    }
}

/// Whether another finalization attempt is permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairVerdict {
    Retry {
        attempt: usize,
        remaining: usize,
        /// The draft is byte-identical to one already rejected.
        unchanged: bool,
    },
    Exhausted {
        attempt: usize,
        reason: ExhaustionReason,
    },
}

impl RepairVerdict {
    pub fn is_exhausted(self) -> bool {
        matches!(self, Self::Exhausted { .. })
    }
}

/// Finalization attempts for one task, with loop detection.
///
/// A resubmitted draft that is byte-identical to a rejected one is not progress: it
/// is the model asserting the same thing again. The first repeat is answered with a
/// sharper instruction; a second identical submission ends finalization, because
/// spending the remaining budget on a fixed point cannot help.
#[derive(Debug, Clone)]
pub struct FinalizationLedger {
    max_attempts: usize,
    attempts: usize,
    seen: BTreeMap<String, usize>,
}

impl FinalizationLedger {
    pub fn new(max_attempts: usize) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            attempts: 0,
            seen: BTreeMap::new(),
        }
    }

    pub fn attempts(&self) -> usize {
        self.attempts
    }

    /// Record one rejected submission and decide whether repair may continue.
    ///
    /// `fingerprint` is the canonical serialization of the draft. Drafts are bounded
    /// by the contract and at most a handful are retained, so comparing them exactly
    /// is cheaper than being approximately right about whether progress was made.
    pub fn record_rejection(&mut self, fingerprint: String) -> RepairVerdict {
        self.attempts = self.attempts.saturating_add(1);
        let repeats = {
            let entry = self.seen.entry(fingerprint).or_insert(0);
            *entry += 1;
            *entry
        };
        if repeats > 2 {
            return RepairVerdict::Exhausted {
                attempt: self.attempts,
                reason: ExhaustionReason::NoProgress,
            };
        }
        if self.attempts >= self.max_attempts {
            return RepairVerdict::Exhausted {
                attempt: self.attempts,
                reason: ExhaustionReason::BudgetSpent,
            };
        }
        RepairVerdict::Retry {
            attempt: self.attempts,
            remaining: self.max_attempts - self.attempts,
            unchanged: repeats > 1,
        }
    }
}

/// Canonical fingerprint of a draft, for progress detection.
pub fn fingerprint(draft: &VerifiedReportDraft) -> String {
    // `serde_json` orders struct fields deterministically, so equal drafts produce
    // equal strings. A failure to serialize cannot happen for a decoded draft;
    // degrading to a marker keeps this infallible rather than propagating.
    serde_json::to_string(draft).unwrap_or_else(|_| "unserializable-draft".to_owned())
}

/// What a model should do about one validation problem.
///
/// English, like the rest of the control plane, and specific: "attach evidence" is
/// actionable, "the report is invalid" is not.
fn validation_action(code: &str) -> &'static str {
    match code {
        "unknown_evidence" => {
            "This identifier is not in the task's evidence catalog. Call search_evidence and use \
             an identifier it returns, or restate the claim with kind=unknown. Never guess an \
             identifier."
        }
        "missing_evidence" => {
            "Attach at least one evidence_id from search_evidence. If no evidence supports it, \
             either gather the data first or restate the claim with kind=unknown."
        }
        "unsupported_observed_number" => {
            "The claim kind does not permit this number's provenance. An observed_fact carries \
             only observed numbers, a deterministic_calculation only calculated ones, an estimate \
             only estimated ones; inference and unknown carry no numbers at all. Change the kind \
             or change how the number is sourced."
        }
        "missing_calculation_provenance" => {
            "A calculated number needs the calculation that produced it. Run \
             run_financial_calculation, then cite its result as calculation_evidence_id together \
             with operation and input_evidence_ids."
        }
        "invalid_estimate" => {
            "An estimate must name its method and its basis evidence, and must not stand in for a \
             quantity the Engine can compute. Compute it with run_financial_calculation instead, \
             or supply a real method and a range."
        }
        "scenario_without_assumption" => {
            "A scenario must state what is assumed. Put it in assumptions, or mark the driving \
             number with provenance=user_assumption."
        }
        "conflicting_evidence" => {
            "This evidence conflicts with another registration of the same fact. List the \
             identifiers in disclosed_conflicts and say in the statement what disagrees. Do not \
             silently pick one side."
        }
        "undeclared_number_in_statement" => {
            "The statement writes a figure the claim does not declare, so nothing verifies it. \
             Either add it as a numeric_item with real provenance, or remove the figure from the \
             prose and let the rendered numbers carry it. A rounded restatement of a cited value \
             (\"约 79.87 亿元\" for 7,987,376,586) is a separate, unverifiable figure — state the \
             cited value or declare the rounded one."
        }
        "evidence_outside_task_scope" => {
            "This evidence belongs to a different security than the task covers. Remove it, or \
             cite evidence for the security under research."
        }
        "duplicate_claim_id" => "Claim identifiers must be unique within the draft.",
        "section_references_unknown_claim" => {
            "A section lists a claim_id that no claim defines. Add the claim or remove the \
             reference."
        }
        "claim_not_in_any_section" => {
            "Every claim must appear in exactly one section, or it would never be shown. Place it \
             or remove it."
        }
        "empty_statement" => "A claim needs a statement in the task output_language.",
        "oversized" => {
            "Split the content so each bound is respected. Nothing is truncated for you."
        }
        "contract_version_mismatch" => "Send the contract version the schema specifies, unchanged.",
        _ => "Correct this claim before resubmitting.",
    }
}

/// What a model should do about one verifier finding.
fn verification_action(code: &str) -> &'static str {
    match code {
        "numeric_claim_without_evidence" => {
            "This claim states a quantity with no citation on it. Move the figure into a \
             numeric_item with real provenance, or attach the evidence_id it came from."
        }
        "numeric_claim_not_reproduced" => {
            "The independent verifier could not reproduce this figure from the evidence the claim \
             cites. Either cite the evidence that actually contains the value, or compute it with \
             run_financial_calculation and cite that result. Do not restate it as an estimate to \
             get past this."
        }
        "invalid_numeric_claim" => {
            "This figure could not be parsed as a quantity. State the number plainly with a unit."
        }
        "unknown_evidence_id" | "invalid_or_conflicting_evidence_id" => {
            "The verifier rejected this identifier. Re-run search_evidence and cite what it \
             returns; if it reports a conflict, disclose it."
        }
        "quality_blocking_evidence" => {
            "This evidence is marked quality-blocking and cannot support a published claim. Find \
             an adequate source or state the gap."
        }
        "evidence_time_missing" => {
            "This observation carries no time, so it cannot support a dated or current claim. \
             Cite an identifier that has an observation time — search_evidence marks the ones \
             that do not."
        }
        "evidence_version_missing" | "evidence_source_missing" => {
            "This evidence lacks source provenance the verifier requires. Cite a different \
             identifier for the same fact."
        }
        "insufficient_distinct_evidence" => {
            "The report cites too few distinct pieces of evidence for its evidence requirement. \
             Cite the evidence the research actually used rather than summarising it away."
        }
        "report_contains_no_verifiable_numeric_claims" => {
            "The report states no checkable quantity. A research conclusion needs the figures it \
             rests on, as numeric_items with provenance."
        }
        _ => "Resolve this finding before resubmitting.",
    }
}

/// Split `code:detail` into its parts.
fn split_code(finding: &str) -> (&str, Option<&str>) {
    match finding.split_once(':') {
        Some((code, rest)) => (code, Some(rest)),
        None => (finding, None),
    }
}

/// Claim a positional finding refers to, if it carries a line reference.
fn claim_for_finding(
    detail: Option<&str>,
    line_claims: &BTreeMap<usize, String>,
) -> Option<String> {
    let detail = detail?;
    let line_token = detail.split(':').next()?;
    let number: usize = line_token.strip_prefix("line_")?.parse().ok()?;
    line_claims.get(&number).cloned()
}

/// Common envelope so every repair response reads the same way.
fn envelope(stage: &str, verdict: RepairVerdict, total: usize) -> serde_json::Map<String, Value> {
    let mut payload = serde_json::Map::new();
    payload.insert("ok".into(), Value::Bool(false));
    payload.insert("stage".into(), Value::from(stage));
    payload.insert("problem_count".into(), Value::from(total));
    match verdict {
        RepairVerdict::Retry {
            attempt,
            remaining,
            unchanged,
        } => {
            payload.insert("attempt".into(), Value::from(attempt));
            payload.insert("attempts_remaining".into(), Value::from(remaining));
            payload.insert(
                "instruction".into(),
                Value::from(if unchanged {
                    "You resubmitted a draft identical to the one just rejected, so nothing was \
                     repaired. Change the listed claims themselves. If a claim cannot be \
                     supported, remove it or restate it as kind=unknown — that is a valid report, \
                     an unsupported figure is not."
                } else {
                    "Repair only the listed claims and resubmit the complete draft. Leave every \
                     other claim byte-identical. Do not weaken a claim's kind to get past a \
                     check."
                }),
            );
        }
        RepairVerdict::Exhausted { attempt, reason } => {
            payload.insert("attempt".into(), Value::from(attempt));
            payload.insert("attempts_remaining".into(), Value::from(0));
            payload.insert("exhausted".into(), Value::from(reason.as_str()));
            payload.insert(
                "instruction".into(),
                Value::from(
                    "No finalization attempts remain. The report will not be published. Nothing \
                     further is required from you.",
                ),
            );
        }
    }
    payload
}

/// Group per-claim actions, bounded and deterministically ordered.
fn repair_targets(
    grouped: BTreeMap<String, BTreeSet<&str>>,
    action_for: fn(&str) -> &'static str,
) -> Vec<Value> {
    grouped
        .into_iter()
        .take(MAX_REPAIR_TARGETS)
        .map(|(claim_id, codes)| {
            let actions: Vec<Value> = codes
                .iter()
                .map(|code| Value::from(action_for(code)))
                .collect();
            json!({
                "claim_id": claim_id,
                "codes": codes.iter().map(|c| Value::from(*c)).collect::<Vec<_>>(),
                "actions": actions,
            })
        })
        .collect()
}

/// Response to a draft that failed the contract.
pub fn validation_repair(problems: &[DraftProblem], verdict: RepairVerdict) -> Value {
    let mut payload = envelope("validation", verdict, problems.len());
    let mut grouped: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    let mut report_level: BTreeSet<&str> = BTreeSet::new();
    for problem in problems {
        match problem.claim_id() {
            Some(claim_id) => {
                grouped
                    .entry(claim_id.to_owned())
                    .or_default()
                    .insert(problem.code());
            }
            None => {
                report_level.insert(problem.code());
            }
        }
    }
    payload.insert(
        "problems".into(),
        Value::Array(
            problems
                .iter()
                .take(MAX_REPORTED_PROBLEMS)
                .map(|problem| serde_json::to_value(problem).unwrap_or(Value::Null))
                .collect(),
        ),
    );
    if problems.len() > MAX_REPORTED_PROBLEMS {
        payload.insert(
            "problems_omitted".into(),
            Value::from(problems.len() - MAX_REPORTED_PROBLEMS),
        );
    }
    payload.insert(
        "repair".into(),
        Value::Array(repair_targets(grouped, validation_action)),
    );
    if !report_level.is_empty() {
        payload.insert(
            "report_level".into(),
            Value::Array(
                report_level
                    .iter()
                    .map(|code| json!({"code": code, "action": validation_action(code)}))
                    .collect(),
            ),
        );
    }
    Value::Object(payload)
}

/// Response to a draft the independent verifier refused.
///
/// The draft was structurally valid, so this is the harder case: the verifier could
/// not reproduce something, or the evidence does not support what was said. Findings
/// are mapped back to claims through the line index of the form the verifier read,
/// so the model is told which claim to fix rather than being handed line numbers of
/// a string it never saw.
pub fn verification_repair(
    findings: &[String],
    line_claims: &BTreeMap<usize, String>,
    verdict: RepairVerdict,
) -> Value {
    let mut payload = envelope("verification", verdict, findings.len());
    let mut grouped: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    let mut evidence_level: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut report_level: BTreeSet<&str> = BTreeSet::new();
    let mut unreproduced: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for finding in findings {
        let (code, detail) = split_code(finding);
        match claim_for_finding(detail, line_claims) {
            Some(claim_id) => {
                grouped.entry(claim_id.clone()).or_default().insert(code);
                // The offending figure is the actionable part of a reproduction
                // failure, so carry it rather than making the model re-derive it.
                if let Some(raw) = detail.and_then(|d| d.split_once(':')).map(|(_, raw)| raw) {
                    if !raw.is_empty() {
                        unreproduced
                            .entry(claim_id)
                            .or_default()
                            .insert(raw.to_owned());
                    }
                }
            }
            None => match detail {
                Some(id) if id.starts_with("evf_") => {
                    evidence_level
                        .entry(code)
                        .or_default()
                        .insert(id.to_owned());
                }
                _ => {
                    report_level.insert(code);
                }
            },
        }
    }

    payload.insert(
        "findings".into(),
        Value::Array(
            findings
                .iter()
                .take(MAX_REPORTED_PROBLEMS)
                .map(|finding| Value::from(finding.clone()))
                .collect(),
        ),
    );
    if findings.len() > MAX_REPORTED_PROBLEMS {
        payload.insert(
            "findings_omitted".into(),
            Value::from(findings.len() - MAX_REPORTED_PROBLEMS),
        );
    }

    let mut repair = repair_targets(grouped, verification_action);
    for target in &mut repair {
        let Some(claim_id) = target.get("claim_id").and_then(Value::as_str) else {
            continue;
        };
        if let Some(values) = unreproduced.get(claim_id) {
            let listed: Vec<Value> = values
                .iter()
                .take(8)
                .map(|v| Value::from(v.clone()))
                .collect();
            if let Some(object) = target.as_object_mut() {
                object.insert("unverified_values".into(), Value::Array(listed));
            }
        }
    }
    payload.insert("repair".into(), Value::Array(repair));

    if !evidence_level.is_empty() {
        payload.insert(
            "evidence_level".into(),
            Value::Array(
                evidence_level
                    .into_iter()
                    .map(|(code, ids)| {
                        json!({
                            "code": code,
                            "evidence_ids": ids.iter().take(12).collect::<Vec<_>>(),
                            "action": verification_action(code),
                        })
                    })
                    .collect(),
            ),
        );
    }
    if !report_level.is_empty() {
        payload.insert(
            "report_level".into(),
            Value::Array(
                report_level
                    .iter()
                    .map(|code| json!({"code": code, "action": verification_action(code)}))
                    .collect(),
            ),
        );
    }
    Value::Object(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Claim, ClaimKind, ReportSection};

    fn draft() -> VerifiedReportDraft {
        VerifiedReportDraft {
            version: crate::report::REPORT_CONTRACT_VERSION.to_owned(),
            title: "标题".into(),
            executive_summary: "摘要".into(),
            sections: vec![ReportSection {
                heading: "第一节".into(),
                claim_ids: vec!["c1".into()],
            }],
            claims: vec![Claim {
                id: "c1".into(),
                kind: ClaimKind::ObservedFact,
                statement: "最新价 34.47 元".into(),
                evidence_ids: vec!["evf_price".into()],
                numeric_items: Vec::new(),
                confidence: None,
                uncertainty: None,
                assumptions: Vec::new(),
                disclosed_conflicts: Vec::new(),
            }],
            overall_uncertainty: None,
            limitations: Vec::new(),
        }
    }

    /// Repair is addressed to a claim, with a specific action.
    #[test]
    fn a_validation_problem_names_the_claim_and_the_fix() {
        let problems = vec![DraftProblem::MissingEvidence {
            claim_id: "c7".into(),
            statement: "毛利率提升".into(),
        }];
        let response = validation_repair(
            &problems,
            RepairVerdict::Retry {
                attempt: 1,
                remaining: 2,
                unchanged: false,
            },
        );
        assert_eq!(response["stage"], json!("validation"));
        assert_eq!(response["repair"][0]["claim_id"], json!("c7"));
        assert_eq!(response["repair"][0]["codes"][0], json!("missing_evidence"));
        assert!(response["repair"][0]["actions"][0]
            .as_str()
            .expect("an action")
            .contains("search_evidence"));
        assert!(response["instruction"]
            .as_str()
            .expect("an instruction")
            .contains("Repair only the listed claims"));
    }

    /// A positional verifier finding becomes a claim-level repair.
    #[test]
    fn a_line_based_verifier_finding_is_mapped_back_to_its_claim() {
        let line_claims = crate::render::verifier_line_claims(&draft());
        // Line 1 is the title, line 2 the heading, line 3 the only claim.
        assert_eq!(line_claims.get(&3).map(String::as_str), Some("c1"));
        let findings = vec!["numeric_claim_not_reproduced:line_3:34.47".to_owned()];
        let response = verification_repair(
            &findings,
            &line_claims,
            RepairVerdict::Retry {
                attempt: 1,
                remaining: 1,
                unchanged: false,
            },
        );
        assert_eq!(response["repair"][0]["claim_id"], json!("c1"));
        assert_eq!(
            response["repair"][0]["unverified_values"][0],
            json!("34.47")
        );
        assert!(response["repair"][0]["actions"][0]
            .as_str()
            .expect("an action")
            .contains("run_financial_calculation"));
    }

    /// An evidence-scoped finding is not attributed to a claim it cannot identify.
    #[test]
    fn an_evidence_scoped_finding_is_reported_against_the_evidence() {
        let response = verification_repair(
            &["evidence_time_missing:evf_abc".to_owned()],
            &BTreeMap::new(),
            RepairVerdict::Retry {
                attempt: 1,
                remaining: 1,
                unchanged: false,
            },
        );
        assert_eq!(
            response["evidence_level"][0]["code"],
            json!("evidence_time_missing")
        );
        assert_eq!(
            response["evidence_level"][0]["evidence_ids"][0],
            json!("evf_abc")
        );
        assert!(response["repair"].as_array().is_some_and(Vec::is_empty));
    }

    /// A report-scoped finding stays report-scoped.
    #[test]
    fn a_report_scoped_finding_is_reported_at_report_level() {
        let response = verification_repair(
            &["insufficient_distinct_evidence:3<8".to_owned()],
            &BTreeMap::new(),
            RepairVerdict::Retry {
                attempt: 1,
                remaining: 1,
                unchanged: false,
            },
        );
        assert_eq!(
            response["report_level"][0]["code"],
            json!("insufficient_distinct_evidence")
        );
    }

    /// The budget is finite and running out never publishes.
    #[test]
    fn the_finalization_budget_is_finite() {
        let mut ledger = FinalizationLedger::new(3);
        assert!(matches!(
            ledger.record_rejection("a".into()),
            RepairVerdict::Retry {
                attempt: 1,
                remaining: 2,
                ..
            }
        ));
        assert!(matches!(
            ledger.record_rejection("b".into()),
            RepairVerdict::Retry {
                attempt: 2,
                remaining: 1,
                ..
            }
        ));
        assert!(matches!(
            ledger.record_rejection("c".into()),
            RepairVerdict::Exhausted {
                reason: ExhaustionReason::BudgetSpent,
                ..
            }
        ));
    }

    /// Resubmitting the same draft is answered once, then stopped.
    #[test]
    fn an_identical_resubmission_is_detected_and_then_ends_finalization() {
        let mut ledger = FinalizationLedger::new(10);
        let print = fingerprint(&draft());
        assert!(matches!(
            ledger.record_rejection(print.clone()),
            RepairVerdict::Retry {
                unchanged: false,
                ..
            }
        ));
        // Second identical submission: told plainly that nothing changed.
        assert!(matches!(
            ledger.record_rejection(print.clone()),
            RepairVerdict::Retry {
                unchanged: true,
                ..
            }
        ));
        // Third: a fixed point, so finalization ends rather than burning budget.
        assert!(matches!(
            ledger.record_rejection(print),
            RepairVerdict::Exhausted {
                reason: ExhaustionReason::NoProgress,
                ..
            }
        ));
    }

    /// A different draft after an identical pair is progress, not a loop.
    #[test]
    fn a_changed_draft_is_treated_as_progress() {
        let mut ledger = FinalizationLedger::new(10);
        let print = fingerprint(&draft());
        ledger.record_rejection(print.clone());
        ledger.record_rejection(print);
        let mut changed = draft();
        changed.claims[0].statement = "最新价 34.47 元（已修订）".into();
        assert!(matches!(
            ledger.record_rejection(fingerprint(&changed)),
            RepairVerdict::Retry {
                unchanged: false,
                ..
            }
        ));
    }

    /// A pathological draft cannot flood the context, and truncation is visible.
    #[test]
    fn a_flood_of_problems_is_bounded_and_the_omission_is_reported() {
        let problems: Vec<DraftProblem> = (0..500)
            .map(|index| DraftProblem::MissingEvidence {
                claim_id: format!("c{index}"),
                statement: "x".into(),
            })
            .collect();
        let response = validation_repair(
            &problems,
            RepairVerdict::Retry {
                attempt: 1,
                remaining: 1,
                unchanged: false,
            },
        );
        assert_eq!(response["problem_count"], json!(500));
        assert_eq!(
            response["problems"].as_array().map(Vec::len),
            Some(MAX_REPORTED_PROBLEMS)
        );
        assert_eq!(
            response["problems_omitted"],
            json!(500 - MAX_REPORTED_PROBLEMS)
        );
        assert_eq!(
            response["repair"].as_array().map(Vec::len),
            Some(MAX_REPAIR_TARGETS)
        );
    }

    /// An exhausted budget says so plainly and asks for nothing further.
    #[test]
    fn an_exhausted_budget_stops_asking_for_repairs() {
        let response = validation_repair(
            &[DraftProblem::MissingEvidence {
                claim_id: "c1".into(),
                statement: "x".into(),
            }],
            RepairVerdict::Exhausted {
                attempt: 3,
                reason: ExhaustionReason::BudgetSpent,
            },
        );
        assert_eq!(response["attempts_remaining"], json!(0));
        assert_eq!(response["exhausted"], json!("finalization_budget_spent"));
        assert!(response["instruction"]
            .as_str()
            .expect("an instruction")
            .contains("will not be published"));
    }

    /// Every action a model may receive must actually tell it what to do.
    #[test]
    fn every_problem_code_has_a_specific_action() {
        for code in [
            "unknown_evidence",
            "missing_evidence",
            "unsupported_observed_number",
            "missing_calculation_provenance",
            "invalid_estimate",
            "scenario_without_assumption",
            "conflicting_evidence",
            "undeclared_number_in_statement",
            "evidence_outside_task_scope",
            "duplicate_claim_id",
            "section_references_unknown_claim",
            "claim_not_in_any_section",
            "empty_statement",
            "oversized",
            "contract_version_mismatch",
        ] {
            let action = validation_action(code);
            assert_ne!(
                action, "Correct this claim before resubmitting.",
                "`{code}` needs a specific action"
            );
        }
        for code in [
            "numeric_claim_without_evidence",
            "numeric_claim_not_reproduced",
            "invalid_numeric_claim",
            "unknown_evidence_id",
            "invalid_or_conflicting_evidence_id",
            "quality_blocking_evidence",
            "evidence_time_missing",
            "evidence_version_missing",
            "evidence_source_missing",
            "insufficient_distinct_evidence",
            "report_contains_no_verifiable_numeric_claims",
        ] {
            let action = verification_action(code);
            assert_ne!(
                action, "Resolve this finding before resubmitting.",
                "`{code}` needs a specific action"
            );
        }
    }

    /// Repair guidance must never offer a way around a gate.
    #[test]
    fn no_repair_action_suggests_weakening_a_claim_to_pass() {
        let all: Vec<&str> = [
            "unknown_evidence",
            "missing_evidence",
            "unsupported_observed_number",
            "missing_calculation_provenance",
            "invalid_estimate",
            "conflicting_evidence",
        ]
        .iter()
        .map(|code| validation_action(code))
        .chain(
            ["numeric_claim_not_reproduced", "quality_blocking_evidence"]
                .iter()
                .map(|code| verification_action(code)),
        )
        .collect();
        for action in all {
            let lowered = action.to_lowercase();
            assert!(
                !lowered.contains("remove the citation")
                    && !lowered.contains("delete the evidence")
                    && !lowered.contains("call it an estimate"),
                "an action must not offer an escape from the gate: {action}"
            );
        }
        // Restating as unknown is a legitimate honest outcome and stays available.
        assert!(validation_action("missing_evidence").contains("kind=unknown"));
    }
}
