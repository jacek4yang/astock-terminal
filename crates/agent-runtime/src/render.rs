//! Deterministic rendering of a validated draft.
//!
//! Two outputs come from one validated draft, so the terminal and the desktop can
//! never disagree about what the report says or what supports it:
//!
//! * a [`RenderedReport`] carrying structured claims and evidence references, which
//!   React consumes directly — it never parses Markdown to discover evidence;
//! * investor-facing Chinese prose with **numbered** citations, which the terminal
//!   prints.
//!
//! The product rule this enforces: `evf_…` identifiers are machine identity and
//! must not appear in the normal investor experience. A reader sees
//! `[腾讯行情 · 2026-08-26 15:00]`, not `【E:evf_abc123】`. The canonical identifier
//! stays attached to the structured object for verification, drill-down and
//! debugging, and the verifier still receives the exact canonical form.
//!
//! Rendering is deterministic: the same draft and registry always produce the same
//! text and the same citation numbering.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::report::{Claim, EvidenceDescriptor, NumericProvenance, VerifiedReportDraft};

/// A citation as the user sees it, with machine identity retained.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceReference {
    /// Position in the report's citation list, starting at 1.
    pub number: usize,
    /// Canonical identity. Present for machines; not rendered into prose.
    pub internal_id: String,
    /// What the user reads, for example `腾讯行情 · 2026-08-26 15:00`.
    pub display_label: String,
    /// Trust state in plain words, for example `公司正式披露`.
    pub trust_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// True when this reference is a deterministic calculation, so the UI can
    /// offer an expandable derivation rather than a source link.
    pub is_calculation: bool,
    pub conflicting: bool,
}

/// One number as presented, with its provenance in words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderedNumber {
    pub label: String,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// `实测数据`, `确定性计算`, `用户情景假设` or `模型估算`.
    pub provenance_label: String,
    pub provenance_kind: String,
    /// Citation numbers backing this number.
    pub evidence_numbers: Vec<usize>,
    /// For a calculation: the operation, so the UI can show the derivation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
}

/// A claim as presented.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderedClaim {
    pub id: String,
    pub kind: String,
    /// `事实`, `计算`, `推断`, `估算`, `情景`, `未知`.
    pub kind_label: String,
    /// User-visible prose with every `{label}` reference replaced by its verified
    /// value, so React and the terminal show the same sentence.
    pub text: String,
    /// Labels the prose already presents inline, so a consumer does not repeat them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inline_numbers: Vec<String>,
    pub numbers: Vec<RenderedNumber>,
    pub evidence_numbers: Vec<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disclosed_conflicts: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderedSection {
    pub heading: String,
    pub claims: Vec<RenderedClaim>,
}

/// The published report in both forms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderedReport {
    pub title: String,
    pub executive_summary: String,
    pub sections: Vec<RenderedSection>,
    /// Ordered citation list; index + 1 equals `EvidenceReference::number`.
    pub references: Vec<EvidenceReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overall_uncertainty: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
    /// Investor-facing prose with numbered citations, for the terminal.
    pub markdown: String,
    /// Canonical form carrying `【E:evf_…】`, submitted to the report verifier.
    ///
    /// Kept separate from `markdown` so the verifier sees exact identities while
    /// the user never does.
    pub verifier_markdown: String,
}

/// Render a validated draft.
///
/// Call only after `validate_draft` returns no problems: rendering assumes every
/// referenced identifier resolves.
pub fn render(
    draft: &VerifiedReportDraft,
    registry: &BTreeMap<String, EvidenceDescriptor>,
) -> RenderedReport {
    // Assign citation numbers in first-appearance order, so numbering is stable
    // and a reader encounters [1] before [2].
    let mut order: Vec<String> = Vec::new();
    let mut number_of: BTreeMap<String, usize> = BTreeMap::new();
    let note = |id: &str, order: &mut Vec<String>, number_of: &mut BTreeMap<String, usize>| {
        if !number_of.contains_key(id) {
            order.push(id.to_owned());
            number_of.insert(id.to_owned(), order.len());
        }
    };
    for section in &draft.sections {
        for claim_id in &section.claim_ids {
            if let Some(claim) = draft.claims.iter().find(|c| &c.id == claim_id) {
                for id in &claim.evidence_ids {
                    note(id, &mut order, &mut number_of);
                }
                for item in &claim.numeric_items {
                    for id in item.provenance.referenced_evidence() {
                        note(id, &mut order, &mut number_of);
                    }
                }
            }
        }
    }

    let references: Vec<EvidenceReference> = order
        .iter()
        .enumerate()
        .map(|(index, id)| {
            let descriptor = registry.get(id);
            EvidenceReference {
                number: index + 1,
                internal_id: id.clone(),
                display_label: descriptor
                    .map(EvidenceDescriptor::display_label)
                    // A missing descriptor cannot happen for a validated draft;
                    // degrade to a neutral label rather than leaking the id.
                    .unwrap_or_else(|| "未知来源".to_owned()),
                trust_label: descriptor
                    .map(|d| d.trust_label().to_owned())
                    .unwrap_or_else(|| "证据不足".to_owned()),
                source: descriptor.map(|d| d.source.clone()),
                observed_at: descriptor.and_then(|d| d.observed_at.clone()),
                published_at: descriptor.and_then(|d| d.published_at.clone()),
                document_title: descriptor.and_then(|d| d.document_title.clone()),
                document_location: descriptor.and_then(|d| d.document_location.clone()),
                field: descriptor.and_then(|d| d.field.clone()),
                unit: descriptor.and_then(|d| d.unit.clone()),
                is_calculation: descriptor.is_some_and(EvidenceDescriptor::is_calculation),
                conflicting: descriptor.is_some_and(|d| d.conflicting),
            }
        })
        .collect();

    let numbers_for = |ids: &[&str]| -> Vec<usize> {
        ids.iter()
            .filter_map(|id| number_of.get(*id).copied())
            .collect()
    };

    let mut sections = Vec::new();
    for section in &draft.sections {
        let mut claims = Vec::new();
        for claim_id in &section.claim_ids {
            let Some(claim) = draft.claims.iter().find(|c| &c.id == claim_id) else {
                continue;
            };
            claims.push(render_claim(claim, &numbers_for));
        }
        sections.push(RenderedSection {
            heading: section.heading.clone(),
            claims,
        });
    }

    let markdown = render_markdown(draft, &sections, &references);
    let (verifier_markdown, _) = render_verifier_form(draft);

    RenderedReport {
        title: draft.title.clone(),
        executive_summary: draft.executive_summary.clone(),
        sections,
        references,
        overall_uncertainty: draft.overall_uncertainty.clone(),
        limitations: draft.limitations.clone(),
        markdown,
        verifier_markdown,
    }
}

fn render_claim(claim: &Claim, numbers_for: &impl Fn(&[&str]) -> Vec<usize>) -> RenderedClaim {
    let evidence: Vec<&str> = claim.evidence_ids.iter().map(String::as_str).collect();
    let numbers = claim
        .numeric_items
        .iter()
        .map(|item| RenderedNumber {
            label: item.label.clone(),
            value: item.value,
            unit: item.unit.clone(),
            provenance_label: item.provenance.display_label().to_owned(),
            provenance_kind: item.provenance.kind_name().to_owned(),
            evidence_numbers: numbers_for(&item.provenance.referenced_evidence()),
            operation: match &item.provenance {
                NumericProvenance::Calculated { operation, .. } => Some(operation.clone()),
                _ => None,
            },
        })
        .collect();
    let conflicts: Vec<&str> = claim
        .disclosed_conflicts
        .iter()
        .map(String::as_str)
        .collect();
    let declared = declared_numbers(claim);
    let referenced = crate::report::placeholder_labels(&claim.statement);
    RenderedClaim {
        id: claim.id.clone(),
        kind: claim.kind.as_str().to_owned(),
        kind_label: claim.kind.display_label().to_owned(),
        text: substitute_numbers(&claim.statement, &declared),
        inline_numbers: referenced,
        numbers,
        evidence_numbers: numbers_for(&evidence),
        confidence: claim.confidence.clone(),
        uncertainty: claim.uncertainty.clone(),
        assumptions: claim.assumptions.clone(),
        disclosed_conflicts: numbers_for(&conflicts),
    }
}

/// Substitute `{label}` references with the declared value and unit.
///
/// This is what lets the model write natural prose without duplicating numeric
/// truth. The model writes `收盘价{收盘价}`, declares `收盘价` as a numeric item with
/// provenance, and the runtime prints the verified value — so the reader sees a
/// normal sentence, the figure is checked, and there is exactly one place where the
/// number lives.
///
/// The same substitution is applied to the verifier form, so the figure the reader
/// sees is the figure the independent verifier reproduced against the claim's
/// citations. Rendering one thing and verifying another would defeat the point.
fn substitute_numbers(text: &str, numbers: &[(String, f64, Option<String>)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let label = after[..close].trim();
        match numbers.iter().find(|(name, _, _)| name == label) {
            Some((_, value, unit)) => {
                out.push_str(&format_number(*value));
                if let Some(unit) = unit {
                    out.push_str(unit);
                }
            }
            // Validation refuses an unresolvable reference, so this is unreachable for
            // a validated draft. Degrade to the label rather than showing braces.
            None => out.push_str(label),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// The `(label, value, unit)` triples a claim declares.
fn declared_numbers(claim: &Claim) -> Vec<(String, f64, Option<String>)> {
    claim
        .numeric_items
        .iter()
        .map(|item| (item.label.clone(), item.value, item.unit.clone()))
        .collect()
}

/// Render a value the way the verifier will read it back.
///
/// Rust's shortest round-tripping form, so an integral value prints as `100` rather
/// than `100.0` and a decimal keeps exactly the digits it carries. Deliberately not
/// rounded for presentation: the published figure and the figure the deterministic
/// verifier reproduces must be the same characters, or the report would show a
/// number that nothing checked.
fn format_number(value: f64) -> String {
    format!("{value}")
}

/// Investor-facing prose. Numbered citations only; no `evf_` anywhere.
fn render_markdown(
    draft: &VerifiedReportDraft,
    sections: &[RenderedSection],
    references: &[EvidenceReference],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", draft.title));
    if !draft.executive_summary.trim().is_empty() {
        out.push_str(&format!("{}\n\n", draft.executive_summary.trim()));
    }
    for section in sections {
        out.push_str(&format!("## {}\n\n", section.heading));
        for claim in &section.claims {
            let citations = if claim.evidence_numbers.is_empty() {
                String::new()
            } else {
                let joined = claim
                    .evidence_numbers
                    .iter()
                    .map(|n| format!("[{n}]"))
                    .collect::<Vec<_>>()
                    .join("");
                format!(" {joined}")
            };
            out.push_str(&format!(
                "- 【{}】{}{}\n",
                claim.kind_label,
                claim.text.trim(),
                citations
            ));
            // Numbers must appear in the report the investor reads.
            //
            // They previously did not: the renderer emitted only the claim's prose,
            // so a figure that lived in `numeric_items` — the only place a figure
            // carries provenance — never reached the page. That made the contract
            // and the presentation disagree about what the report says, and it
            // pushed the model towards writing figures into prose instead, which is
            // exactly what has no provenance.
            for number in claim
                .numbers
                .iter()
                .filter(|number| !claim.inline_numbers.contains(&number.label))
            {
                let unit = number
                    .unit
                    .as_deref()
                    .map(|unit| format!(" {unit}"))
                    .unwrap_or_default();
                let citations = if number.evidence_numbers.is_empty() {
                    String::new()
                } else {
                    format!(
                        " {}",
                        number
                            .evidence_numbers
                            .iter()
                            .map(|n| format!("[{n}]"))
                            .collect::<Vec<_>>()
                            .join("")
                    )
                };
                // The operation is shown for a calculation so a reader can see how
                // the figure was derived rather than taking it on trust.
                let derivation = number
                    .operation
                    .as_deref()
                    .map(|operation| format!(" · {operation}"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "    - {}：{}{}（{}{}{}）\n",
                    number.label,
                    format_number(number.value),
                    unit,
                    number.provenance_label,
                    derivation,
                    citations
                ));
            }
            // Surface an assumption inline so a scenario parameter can never read
            // as something a data source reported.
            for assumption in &claim.assumptions {
                out.push_str(&format!("    - 假设：{assumption}\n"));
            }
            if let Some(uncertainty) = &claim.uncertainty {
                out.push_str(&format!("    - 不确定性：{uncertainty}\n"));
            }
            if !claim.disclosed_conflicts.is_empty() {
                let joined = claim
                    .disclosed_conflicts
                    .iter()
                    .map(|n| format!("[{n}]"))
                    .collect::<Vec<_>>()
                    .join("");
                out.push_str(&format!("    - 来源冲突已披露：{joined}\n"));
            }
        }
        out.push('\n');
    }
    if let Some(uncertainty) = &draft.overall_uncertainty {
        out.push_str(&format!("## 主要不确定性\n\n{}\n\n", uncertainty.trim()));
    }
    if !draft.limitations.is_empty() {
        out.push_str("## 局限性\n\n");
        for limitation in &draft.limitations {
            out.push_str(&format!("- {limitation}\n"));
        }
        out.push('\n');
    }
    if !references.is_empty() {
        out.push_str("## 证据\n\n");
        for reference in references {
            out.push_str(&format!(
                "[{}] {} · {}\n",
                reference.number, reference.display_label, reference.trust_label
            ));
        }
    }
    out
}

/// Canonical form for the verifier: every claim line carries its exact
/// `【E:evf_…】` citations, which is what the deterministic verifier matches
/// numbers against. Users never see this string.
///
/// Returns the line index alongside the text. The verifier reports positional
/// findings — `numeric_claim_without_evidence:line_7` — and a positional finding is
/// useless for repair unless it can be turned back into a claim. Producing both
/// from one traversal is what makes that mapping trustworthy: there is no second
/// implementation of the layout to drift out of step.
///
/// Keys are 1-based line numbers, matching the verifier's `line_index + 1`.
fn render_verifier_form(draft: &VerifiedReportDraft) -> (String, BTreeMap<usize, String>) {
    let mut out = String::new();
    let mut index = BTreeMap::new();
    let mut line_number = 0usize;
    let mut push = |out: &mut String, text: &str, claim: Option<&str>| {
        line_number += 1;
        out.push_str(text);
        out.push('\n');
        if let Some(claim) = claim {
            index.insert(line_number, claim.to_owned());
        }
    };

    push(&mut out, &draft.title, None);
    for section in &draft.sections {
        push(&mut out, &section.heading, None);
        for claim_id in &section.claim_ids {
            let Some(claim) = draft.claims.iter().find(|c| &c.id == claim_id) else {
                continue;
            };
            // Numbers are emitted explicitly with their citations so the verifier
            // reproduces each against the evidence the claim actually named,
            // rather than against whatever happened to share a prose line.
            let mut line = String::new();
            let declared = declared_numbers(claim);
            let referenced = crate::report::placeholder_labels(&claim.statement);
            line.push_str(&format!(
                "【{}】{}",
                claim.kind.display_label(),
                substitute_numbers(&claim.statement, &declared).trim()
            ));
            // Numbers the prose did not present inline are emitted explicitly. Every
            // item's provenance is cited either way: a figure substituted into the
            // prose still has to be reproduced against the evidence it names, so
            // omitting its identifier would fail the very claim it supports.
            for item in &claim.numeric_items {
                if !referenced.contains(&item.label) {
                    let unit = item.unit.as_deref().unwrap_or("");
                    line.push_str(&format!(" {}={}{}", item.label, item.value, unit));
                }
                for id in item.provenance.referenced_evidence() {
                    line.push_str(&format!("【E:{id}】"));
                }
            }
            for id in &claim.evidence_ids {
                line.push_str(&format!("【E:{id}】"));
            }
            push(&mut out, &line, Some(&claim.id));
        }
    }
    (out, index)
}

/// Which claim occupies each line of the verifier form.
///
/// Exposed so finalization can turn a positional verifier finding into a targeted
/// claim repair instead of asking the model to rewrite the whole report.
pub fn verifier_line_claims(draft: &VerifiedReportDraft) -> BTreeMap<usize, String> {
    render_verifier_form(draft).1
}

/// Does this text leak a canonical identifier?
///
/// Used by tests and by the adapters' own assertions: the investor-facing string
/// must never contain one.
pub fn contains_internal_identifier(text: &str) -> bool {
    text.contains("evf_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ClaimKind, NumericItem, ReportSection};
    use serde_json::json;

    fn descriptor(id: &str, source: &str, field: &str) -> EvidenceDescriptor {
        EvidenceDescriptor {
            evidence_id: id.into(),
            source: source.into(),
            symbol: Some("601899".into()),
            field: Some(field.into()),
            value: Some(json!(34.47)),
            unit: Some("元".into()),
            observed_at: Some("2026-08-26T07:00:00Z".into()),
            published_at: None,
            document_title: None,
            document_location: None,
            quality_blocking: false,
            conflicting: false,
        }
    }

    fn registry() -> BTreeMap<String, EvidenceDescriptor> {
        let mut map = BTreeMap::new();
        map.insert(
            "evf_price".to_owned(),
            descriptor("evf_price", "tencent", "/quote/last"),
        );
        let mut calc = descriptor("evf_pe", "astock-compute", "/value");
        calc.unit = Some("倍".into());
        calc.observed_at = None;
        map.insert("evf_pe".to_owned(), calc);
        map
    }

    fn draft() -> VerifiedReportDraft {
        VerifiedReportDraft {
            version: crate::report::REPORT_CONTRACT_VERSION.to_owned(),
            title: "紫金矿业当前估值".to_owned(),
            executive_summary: "估值处于历史中枢附近。".to_owned(),
            sections: vec![ReportSection {
                heading: "当前估值".to_owned(),
                claim_ids: vec!["c1".to_owned(), "c2".to_owned()],
            }],
            claims: vec![
                Claim {
                    id: "c1".to_owned(),
                    kind: ClaimKind::ObservedFact,
                    statement: "最新收盘价为 34.47 元".to_owned(),
                    evidence_ids: vec!["evf_price".to_owned()],
                    numeric_items: vec![NumericItem {
                        value: 34.47,
                        unit: Some("元".to_owned()),
                        label: "最新价".to_owned(),
                        provenance: NumericProvenance::Observed {
                            evidence_id: "evf_price".to_owned(),
                            field: Some("/quote/last".to_owned()),
                        },
                    }],
                    confidence: Some("高".to_owned()),
                    uncertainty: None,
                    assumptions: Vec::new(),
                    disclosed_conflicts: Vec::new(),
                },
                Claim {
                    id: "c2".to_owned(),
                    kind: ClaimKind::DeterministicCalculation,
                    statement: "按最新价与每股收益计算的市盈率".to_owned(),
                    evidence_ids: Vec::new(),
                    numeric_items: vec![NumericItem {
                        value: 28.49,
                        unit: Some("倍".to_owned()),
                        label: "市盈率".to_owned(),
                        provenance: NumericProvenance::Calculated {
                            calculation_evidence_id: "evf_pe".to_owned(),
                            operation: "divide".to_owned(),
                            input_evidence_ids: vec!["evf_price".to_owned()],
                        },
                    }],
                    confidence: None,
                    uncertainty: Some("盈利口径变化会影响结果".to_owned()),
                    assumptions: Vec::new(),
                    disclosed_conflicts: Vec::new(),
                },
            ],
            overall_uncertainty: Some("铜价波动是主要变量。".to_owned()),
            limitations: vec!["未覆盖海外税务细节。".to_owned()],
        }
    }

    /// The investor-facing report must not contain a canonical identifier.
    #[test]
    fn user_facing_markdown_never_leaks_an_internal_identifier() {
        let rendered = render(&draft(), &registry());
        assert!(
            !contains_internal_identifier(&rendered.markdown),
            "investor prose leaked an internal id:\n{}",
            rendered.markdown
        );
        // Numbered citations are what the reader sees.
        assert!(rendered.markdown.contains("[1]"));
        assert!(rendered.markdown.contains("腾讯行情"));
    }

    /// The verifier must still receive exact canonical identities.
    #[test]
    fn the_verifier_form_carries_canonical_identifiers() {
        let rendered = render(&draft(), &registry());
        assert!(rendered.verifier_markdown.contains("【E:evf_price】"));
        assert!(rendered.verifier_markdown.contains("【E:evf_pe】"));
        // Each number is emitted next to the evidence it came from.
        assert!(rendered.verifier_markdown.contains("最新价=34.47元"));
    }

    /// Structured references let React render chips without parsing prose.
    #[test]
    fn references_expose_display_metadata_alongside_machine_identity() {
        let rendered = render(&draft(), &registry());
        let price = rendered
            .references
            .iter()
            .find(|r| r.internal_id == "evf_price")
            .expect("the price reference is listed");
        assert_eq!(price.number, 1);
        assert_eq!(price.trust_label, "实时行情");
        assert!(price.display_label.contains("腾讯行情"));
        assert!(!price.is_calculation);

        let pe = rendered
            .references
            .iter()
            .find(|r| r.internal_id == "evf_pe")
            .expect("the calculation reference is listed");
        assert!(pe.is_calculation, "a calculation must be marked as one");
        assert_eq!(pe.display_label, "确定性计算");
    }

    /// Citation numbering is deterministic and first-appearance ordered.
    #[test]
    fn citation_numbering_is_stable_and_first_appearance_ordered() {
        let first = render(&draft(), &registry());
        let second = render(&draft(), &registry());
        assert_eq!(first.markdown, second.markdown);
        assert_eq!(
            first
                .references
                .iter()
                .map(|r| r.internal_id.as_str())
                .collect::<Vec<_>>(),
            vec!["evf_price", "evf_pe"]
        );
    }

    /// A user assumption must be visually distinct from an observation.
    #[test]
    fn a_user_assumption_renders_as_an_assumption_not_an_observation() {
        let mut d = draft();
        d.sections[0].claim_ids.push("c3".to_owned());
        d.claims.push(Claim {
            id: "c3".to_owned(),
            kind: ClaimKind::Scenario,
            statement: "铜价下跌情景下毛利承压".to_owned(),
            evidence_ids: vec!["evf_price".to_owned()],
            numeric_items: vec![NumericItem {
                value: -15.0,
                unit: Some("%".to_owned()),
                label: "铜价变动".to_owned(),
                provenance: NumericProvenance::UserAssumption {
                    stated_in_message_id: Some("msg-7".to_owned()),
                },
            }],
            confidence: None,
            uncertainty: Some("弹性系数为区间估计".to_owned()),
            assumptions: vec!["用户设定铜价下跌 15%".to_owned()],
            disclosed_conflicts: Vec::new(),
        });
        let rendered = render(&d, &registry());
        assert!(rendered.markdown.contains("假设：用户设定铜价下跌 15%"));
        let scenario = rendered.sections[0]
            .claims
            .iter()
            .find(|c| c.id == "c3")
            .expect("the scenario claim is rendered");
        assert_eq!(scenario.kind_label, "情景");
        assert_eq!(scenario.numbers[0].provenance_label, "用户情景假设");
        assert!(!contains_internal_identifier(&rendered.markdown));
    }

    /// A calculation exposes its operation so the UI can show the derivation.
    #[test]
    fn a_calculation_exposes_its_operation_and_inputs() {
        let rendered = render(&draft(), &registry());
        let calculation = rendered.sections[0]
            .claims
            .iter()
            .find(|c| c.id == "c2")
            .expect("the calculation claim is rendered");
        let number = &calculation.numbers[0];
        assert_eq!(number.provenance_kind, "calculated");
        assert_eq!(number.operation.as_deref(), Some("divide"));
        // Both the result and its input are reachable as citations.
        assert_eq!(number.evidence_numbers.len(), 2);
    }
}
