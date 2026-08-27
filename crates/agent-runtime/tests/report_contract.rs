//! Structured report contract: validity, fail-closed behaviour and mutation.
//!
//! The contract exists so an invalid report is unrepresentable rather than merely
//! detected. These tests hold that line from both directions: every valid shape
//! must publish, and every invalid shape must be refused with a specific,
//! machine-actionable problem rather than a generic failure.
//!
//! Nothing here needs a model provider, so the whole contract is provable before
//! spending a paid API call.

use std::collections::{BTreeMap, BTreeSet};

use astock_agent_runtime::{
    contains_internal_identifier, render, validate_draft, Claim, ClaimKind, DraftProblem,
    EvidenceDescriptor, NumericItem, NumericProvenance, ReportSection, VerifiedReportDraft,
    REPORT_CONTRACT_VERSION,
};
use serde_json::json;

fn observed(id: &str, source: &str, field: &str, value: f64) -> EvidenceDescriptor {
    EvidenceDescriptor {
        evidence_id: id.into(),
        source: source.into(),
        symbol: Some("601899".into()),
        field: Some(field.into()),
        value: Some(json!(value)),
        unit: Some("元".into()),
        observed_at: Some("2026-08-26T07:00:00Z".into()),
        published_at: None,
        document_title: None,
        document_location: None,
        quality_blocking: false,
        conflicting: false,
    }
}

fn calculation(id: &str) -> EvidenceDescriptor {
    EvidenceDescriptor {
        evidence_id: id.into(),
        source: "astock-compute".into(),
        symbol: Some("601899".into()),
        field: Some("/value".into()),
        value: Some(json!(28.49)),
        unit: Some("倍".into()),
        observed_at: None,
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
        "evf_price".into(),
        observed("evf_price", "tencent", "/quote/last", 34.47),
    );
    map.insert(
        "evf_eps".into(),
        observed("evf_eps", "disclosure", "/fundamentals/eps", 1.21),
    );
    map.insert(
        "evf_shares".into(),
        observed("evf_shares", "disclosure", "/fundamentals/shares", 2.659e9),
    );
    map.insert("evf_pe".into(), calculation("evf_pe"));
    map.insert("evf_cap".into(), calculation("evf_cap"));
    map
}

fn symbols() -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    set.insert("601899".to_owned());
    set
}

fn claim(id: &str, kind: ClaimKind, statement: &str) -> Claim {
    Claim {
        id: id.into(),
        kind,
        statement: statement.into(),
        evidence_ids: Vec::new(),
        numeric_items: Vec::new(),
        confidence: None,
        uncertainty: None,
        assumptions: Vec::new(),
        disclosed_conflicts: Vec::new(),
    }
}

/// A draft exercising every claim kind and every provenance class.
fn valid_draft() -> VerifiedReportDraft {
    let mut price = claim("c_price", ClaimKind::ObservedFact, "最新收盘价见下方数值");
    price.evidence_ids = vec!["evf_price".into()];
    price.numeric_items = vec![NumericItem {
        value: 34.47,
        unit: Some("元".into()),
        label: "最新价".into(),
        provenance: NumericProvenance::Observed {
            evidence_id: "evf_price".into(),
            field: Some("/quote/last".into()),
        },
    }];

    let mut pe = claim(
        "c_pe",
        ClaimKind::DeterministicCalculation,
        "按最新价与每股收益计算市盈率",
    );
    pe.numeric_items = vec![NumericItem {
        value: 28.49,
        unit: Some("倍".into()),
        label: "市盈率".into(),
        provenance: NumericProvenance::Calculated {
            calculation_evidence_id: "evf_pe".into(),
            operation: "divide".into(),
            input_evidence_ids: vec!["evf_price".into(), "evf_eps".into()],
        },
    }];

    let mut inference = claim("c_inf", ClaimKind::Inference, "估值处于历史中枢附近");
    inference.evidence_ids = vec!["evf_price".into(), "evf_eps".into()];

    let mut scenario = claim("c_scn", ClaimKind::Scenario, "铜价下跌情景下毛利承压");
    scenario.evidence_ids = vec!["evf_price".into()];
    // The assumption is a declared numeric item, so the prose names it without a figure.
    scenario.assumptions = vec!["用户设定铜价下跌，跌幅见下方数值".into()];
    scenario.numeric_items = vec![NumericItem {
        value: -15.0,
        unit: Some("%".into()),
        label: "铜价变动".into(),
        provenance: NumericProvenance::UserAssumption {
            stated_in_message_id: Some("msg-7".into()),
        },
    }];

    let mut estimate = claim("c_est", ClaimKind::Estimate, "海外税负影响的区间估计");
    estimate.numeric_items = vec![NumericItem {
        value: 0.03,
        unit: Some("%".into()),
        label: "税负影响".into(),
        provenance: NumericProvenance::Estimated {
            method: "按历史区间上下界推算".into(),
            basis_evidence_ids: vec!["evf_eps".into()],
            range: Some([0.02, 0.04]),
        },
    }];

    let unknown = claim("c_unk", ClaimKind::Unknown, "海外矿权续期结果尚无公开信息");

    VerifiedReportDraft {
        version: REPORT_CONTRACT_VERSION.to_owned(),
        title: "紫金矿业当前投资价值".into(),
        executive_summary: "估值中枢附近，铜价是主要变量。".into(),
        sections: vec![
            ReportSection {
                heading: "当前估值".into(),
                claim_ids: vec!["c_price".into(), "c_pe".into(), "c_inf".into()],
            },
            ReportSection {
                heading: "情景与不确定性".into(),
                claim_ids: vec!["c_scn".into(), "c_est".into(), "c_unk".into()],
            },
        ],
        claims: vec![price, pe, inference, scenario, estimate, unknown],
        overall_uncertainty: Some("铜金价格波动。".into()),
        limitations: vec!["未覆盖海外税务细节。".into()],
    }
}

fn validate(draft: &VerifiedReportDraft) -> Vec<DraftProblem> {
    validate_draft(draft, &registry(), &symbols())
}

fn has(problems: &[DraftProblem], code: &str) -> bool {
    problems.iter().any(|p| p.code() == code)
}

// ---------------------------------------------------------------------------
// Valid
// ---------------------------------------------------------------------------

#[test]
fn a_draft_covering_every_claim_kind_validates_and_renders() {
    let draft = valid_draft();
    let problems = validate(&draft);
    assert!(problems.is_empty(), "valid draft was refused: {problems:?}");

    let rendered = render(&draft, &registry());
    assert!(!rendered.markdown.is_empty());
    assert_eq!(rendered.sections.len(), 2);
    // Every claim reaches the rendered output.
    let rendered_claims: usize = rendered.sections.iter().map(|s| s.claims.len()).sum();
    assert_eq!(rendered_claims, 6);
}

/// The product rule: internal identity never reaches the investor.
#[test]
fn the_investor_facing_report_never_contains_a_canonical_identifier() {
    let rendered = render(&valid_draft(), &registry());
    assert!(
        !contains_internal_identifier(&rendered.markdown),
        "investor prose leaked an internal id:\n{}",
        rendered.markdown
    );
    // But the verifier still receives exact identities.
    assert!(contains_internal_identifier(&rendered.verifier_markdown));
}

#[test]
fn every_reference_carries_a_human_label_and_a_trust_label() {
    let rendered = render(&valid_draft(), &registry());
    assert!(!rendered.references.is_empty());
    for reference in &rendered.references {
        assert!(
            !reference.display_label.is_empty()
                && !contains_internal_identifier(&reference.display_label),
            "a display label must be readable and id-free: {reference:?}"
        );
        assert!(!reference.trust_label.is_empty());
        assert!(reference.internal_id.starts_with("evf_"));
    }
}

// ---------------------------------------------------------------------------
// Invalid — each must fail closed with a specific problem
// ---------------------------------------------------------------------------

#[test]
fn an_invented_evidence_label_is_refused_and_never_guessed() {
    let mut draft = valid_draft();
    // The exact shape seen live.
    draft.claims[0].evidence_ids = vec!["计算-BPS".into()];
    let problems = validate(&draft);
    assert!(has(&problems, "unknown_evidence"), "{problems:?}");
    assert!(problems.iter().any(|p| matches!(
        p,
        DraftProblem::UnknownEvidence { supplied_id, .. } if supplied_id == "计算-BPS"
    )));
}

#[test]
fn an_observed_fact_without_evidence_is_refused() {
    let mut draft = valid_draft();
    draft.claims[0].evidence_ids.clear();
    draft.claims[0].numeric_items.clear();
    assert!(has(&validate(&draft), "missing_evidence"));
}

#[test]
fn a_calculation_without_provenance_is_refused() {
    let mut draft = valid_draft();
    draft.claims[1].numeric_items[0].provenance = NumericProvenance::Calculated {
        calculation_evidence_id: "evf_pe".into(),
        operation: String::new(),
        input_evidence_ids: Vec::new(),
    };
    assert!(has(&validate(&draft), "missing_calculation_provenance"));
}

#[test]
fn an_estimate_of_a_computable_quantity_is_refused() {
    let mut draft = valid_draft();
    // Market capitalisation is deterministically computable, so calling it an
    // estimate is using Estimate as an escape hatch.
    draft.claims[4].numeric_items[0] = NumericItem {
        value: 9.16e11,
        unit: Some("元".into()),
        label: "市值".into(),
        provenance: NumericProvenance::Estimated {
            method: "按股本与价格粗算市值".into(),
            basis_evidence_ids: vec!["evf_price".into()],
            range: None,
        },
    };
    let problems = validate(&draft);
    assert!(has(&problems, "invalid_estimate"), "{problems:?}");
}

#[test]
fn an_estimate_presented_as_an_observation_is_refused() {
    let mut draft = valid_draft();
    // Turn the estimate claim into a fact while keeping estimated provenance.
    draft.claims[4].kind = ClaimKind::ObservedFact;
    draft.claims[4].evidence_ids = vec!["evf_eps".into()];
    let problems = validate(&draft);
    // An observed-fact claim may not carry an estimated number as if measured;
    // the estimate itself is still flagged.
    assert!(
        has(&problems, "invalid_estimate") || has(&problems, "unsupported_observed_number"),
        "{problems:?}"
    );
}

#[test]
fn a_user_assumption_on_a_non_scenario_claim_is_refused() {
    let mut draft = valid_draft();
    draft.claims[3].kind = ClaimKind::ObservedFact;
    let problems = validate(&draft);
    assert!(
        has(&problems, "unsupported_observed_number"),
        "an assumption must not be presentable as an observation: {problems:?}"
    );
}

#[test]
fn a_scenario_without_an_assumption_is_refused() {
    let mut draft = valid_draft();
    draft.claims[3].assumptions.clear();
    draft.claims[3].numeric_items.clear();
    assert!(has(&validate(&draft), "scenario_without_assumption"));
}

#[test]
fn citing_a_calculation_as_an_observation_is_refused() {
    let mut draft = valid_draft();
    draft.claims[0].numeric_items[0].provenance = NumericProvenance::Observed {
        evidence_id: "evf_pe".into(),
        field: None,
    };
    assert!(has(&validate(&draft), "missing_calculation_provenance"));
}

#[test]
fn undisclosed_conflicting_evidence_is_refused() {
    let mut map = registry();
    map.get_mut("evf_price").unwrap().conflicting = true;
    let problems = validate_draft(&valid_draft(), &map, &symbols());
    assert!(has(&problems, "conflicting_evidence"), "{problems:?}");

    // Disclosing the conflict makes the same draft acceptable.
    let mut draft = valid_draft();
    for c in &mut draft.claims {
        if c.evidence_ids.iter().any(|id| id == "evf_price") {
            c.disclosed_conflicts.push("evf_price".into());
        }
        if c.numeric_items
            .iter()
            .any(|i| i.provenance.referenced_evidence().contains(&"evf_price"))
            && !c.disclosed_conflicts.iter().any(|d| d == "evf_price")
        {
            c.disclosed_conflicts.push("evf_price".into());
        }
    }
    let problems = validate_draft(&draft, &map, &symbols());
    assert!(
        !has(&problems, "conflicting_evidence"),
        "a disclosed conflict must be acceptable: {problems:?}"
    );
}

#[test]
fn evidence_from_another_security_is_refused() {
    let mut map = registry();
    map.get_mut("evf_eps").unwrap().symbol = Some("600519".into());
    let problems = validate_draft(&valid_draft(), &map, &symbols());
    assert!(
        has(&problems, "evidence_outside_task_scope"),
        "{problems:?}"
    );
}

#[test]
fn a_wrong_contract_version_is_refused() {
    let mut draft = valid_draft();
    draft.version = "astock-report-contract-v0".into();
    assert!(has(&validate(&draft), "contract_version_mismatch"));
}

#[test]
fn structural_defects_are_refused() {
    let mut duplicate = valid_draft();
    duplicate.claims[1].id = "c_price".into();
    assert!(has(&validate(&duplicate), "duplicate_claim_id"));

    let mut dangling = valid_draft();
    dangling.sections[0].claim_ids.push("c_missing".into());
    assert!(has(
        &validate(&dangling),
        "section_references_unknown_claim"
    ));

    let mut orphan = valid_draft();
    orphan.sections[1].claim_ids.retain(|id| id != "c_unk");
    assert!(has(&validate(&orphan), "claim_not_in_any_section"));

    let mut empty = valid_draft();
    empty.claims[0].statement = "   ".into();
    assert!(has(&validate(&empty), "empty_statement"));
}

#[test]
fn an_oversized_draft_is_refused_rather_than_truncated() {
    let mut draft = valid_draft();
    draft.claims[0].statement = "很".repeat(3_000);
    assert!(has(&validate(&draft), "oversized"));

    let mut wide = valid_draft();
    wide.claims[0].evidence_ids = (0..40).map(|n| format!("evf_{n}")).collect();
    assert!(has(&validate(&wide), "oversized"));
}

// ---------------------------------------------------------------------------
// Mutation: one element at a time
// ---------------------------------------------------------------------------

#[test]
fn every_inappropriate_mutation_of_a_valid_draft_is_refused() {
    // The valid baseline must pass, or the mutations prove nothing.
    assert!(validate(&valid_draft()).is_empty());

    type Mutation = (&'static str, fn(&mut VerifiedReportDraft));
    let mutations: Vec<Mutation> = vec![
        ("remove citation", |d| {
            d.claims[0].evidence_ids.clear();
            d.claims[0].numeric_items.clear();
        }),
        ("fake evidence id", |d| {
            d.claims[0].evidence_ids = vec!["evf_does_not_exist".into()];
        }),
        ("observed number on an inference", |d| {
            d.claims[2].numeric_items = vec![NumericItem {
                value: 12.0,
                unit: None,
                label: "推断数值".into(),
                provenance: NumericProvenance::Observed {
                    evidence_id: "evf_price".into(),
                    field: None,
                },
            }];
        }),
        ("delete calculation provenance", |d| {
            d.claims[1].numeric_items[0].provenance = NumericProvenance::Calculated {
                calculation_evidence_id: "evf_pe".into(),
                operation: String::new(),
                input_evidence_ids: Vec::new(),
            };
        }),
        ("estimate becomes fact", |d| {
            d.claims[4].kind = ClaimKind::ObservedFact;
            d.claims[4].evidence_ids = vec!["evf_eps".into()];
        }),
        ("assumption becomes observation", |d| {
            d.claims[3].kind = ClaimKind::ObservedFact;
        }),
        ("scenario loses its assumption", |d| {
            d.claims[3].assumptions.clear();
            d.claims[3].numeric_items.clear();
        }),
        ("calculation cited as observation", |d| {
            d.claims[0].numeric_items[0].provenance = NumericProvenance::Observed {
                evidence_id: "evf_cap".into(),
                field: None,
            };
        }),
        ("inverted estimate range", |d| {
            d.claims[4].numeric_items[0].provenance = NumericProvenance::Estimated {
                method: "按历史区间推算".into(),
                basis_evidence_ids: vec!["evf_eps".into()],
                range: Some([0.09, 0.01]),
            };
        }),
        ("contract version drift", |d| {
            d.version = "something-else".into();
        }),
    ];

    for (name, mutate) in mutations {
        let mut draft = valid_draft();
        mutate(&mut draft);
        let problems = validate(&draft);
        assert!(
            !problems.is_empty(),
            "mutation `{name}` was accepted but must be refused"
        );
    }

    // Restoring the baseline must pass again, proving the mutations, not the
    // harness, caused the failures.
    assert!(validate(&valid_draft()).is_empty());
}

/// Altering a published number must be caught by the verifier stage.
///
/// The contract validates provenance shape; reproducing the value against the
/// evidence is the independent verifier's job, and the two together are what make
/// an altered figure unpublishable. This asserts the contract hands the verifier a
/// form where the check is possible: the number appears beside its own citation.
#[test]
fn an_altered_number_is_presented_to_the_verifier_beside_its_own_evidence() {
    let mut draft = valid_draft();
    draft.claims[0].numeric_items[0].value = 41.90;
    let rendered = render(&draft, &registry());
    let line = rendered
        .verifier_markdown
        .lines()
        .find(|l| l.contains("最新价="))
        .expect("the numeric claim reaches the verifier form");
    assert!(line.contains("41.9"), "the altered value must be present");
    assert!(
        line.contains("【E:evf_price】"),
        "and it must sit beside the evidence it claims to come from, so the \
         verifier can reproduce it: {line}"
    );
}

// ---------------------------------------------------------------------------
// High cardinality
// ---------------------------------------------------------------------------

#[test]
fn validation_and_rendering_stay_bounded_with_thousands_of_evidence_items() {
    for count in [10usize, 100, 1_000, 5_000] {
        let mut map = registry();
        for n in 0..count {
            let id = format!("evf_bulk_{n}");
            map.insert(
                id.clone(),
                observed(&id, "eastmoney", "/bulk/value", n as f64),
            );
        }
        let draft = valid_draft();
        let problems = validate_draft(&draft, &map, &symbols());
        assert!(
            problems.is_empty(),
            "a large registry must not affect a valid draft ({count} items): {problems:?}"
        );

        let rendered = render(&draft, &map);
        // Only cited evidence becomes a citation, regardless of registry size.
        assert!(
            rendered.references.len() <= 8,
            "citations must reflect the report, not the registry ({count} items, got {})",
            rendered.references.len()
        );
        assert!(!contains_internal_identifier(&rendered.markdown));
        // No identifier is truncated on the way through.
        for reference in &rendered.references {
            assert!(map.contains_key(&reference.internal_id));
        }
    }
}

#[test]
fn a_draft_survives_a_serialization_round_trip() {
    let draft = valid_draft();
    let encoded = serde_json::to_string(&draft).expect("draft serializes");
    let restored: VerifiedReportDraft = serde_json::from_str(&encoded).expect("draft deserializes");
    assert_eq!(restored, draft);

    let rendered = render(&draft, &registry());
    let encoded = serde_json::to_string(&rendered).expect("rendered report serializes");
    let restored: astock_agent_runtime::RenderedReport =
        serde_json::from_str(&encoded).expect("rendered report deserializes");
    assert_eq!(restored, rendered);
}

// ---------------------------------------------------------------------------
// Figures live in numeric_items, never in free text
//
// The rule is absolute: prose carries meaning, `numeric_items` carry figures, and
// the renderer decides how a verified number appears. The earlier rule was
// conditional — a figure was permitted in a statement if the same claim also
// declared it — and it was the single largest source of live failure: 49 of 75 and
// 34 of 40 findings on two runs. A conditional rule makes the model cross-check
// every figure against its own declarations; an absolute one is checkable before
// submitting.
//
// It is also stricter, and it closes a hole. The verifier reads only the title, the
// section headings and the claim lines, so a figure in `executive_summary`,
// `limitations`, `assumptions` or `uncertainty` was published without ever being
// checked.
// ---------------------------------------------------------------------------

fn amount_registry() -> BTreeMap<String, EvidenceDescriptor> {
    let mut map = registry();
    let mut amount = observed("evf_amount", "tencent", "/quote/amount", 7_987_376_586.0);
    amount.unit = Some("元".into());
    map.insert("evf_amount".into(), amount);
    map
}

fn one_claim_draft(claim: Claim) -> VerifiedReportDraft {
    VerifiedReportDraft {
        version: REPORT_CONTRACT_VERSION.to_owned(),
        title: "紫金矿业行情".to_owned(),
        executive_summary: "仅覆盖行情事实。".to_owned(),
        sections: vec![ReportSection {
            heading: "行情".to_owned(),
            claim_ids: vec![claim.id.clone()],
        }],
        claims: vec![claim],
        overall_uncertainty: None,
        limitations: Vec::new(),
    }
}

fn observed_claim(statement: &str, items: Vec<NumericItem>, evidence: Vec<&str>) -> Claim {
    Claim {
        id: "c1".to_owned(),
        kind: ClaimKind::ObservedFact,
        statement: statement.to_owned(),
        evidence_ids: evidence.into_iter().map(str::to_owned).collect(),
        numeric_items: items,
        confidence: None,
        uncertainty: None,
        assumptions: Vec::new(),
        disclosed_conflicts: Vec::new(),
    }
}

fn amount_item() -> NumericItem {
    NumericItem {
        value: 7_987_376_586.0,
        unit: Some("元".to_owned()),
        label: "成交额".to_owned(),
        provenance: NumericProvenance::Observed {
            evidence_id: "evf_amount".to_owned(),
            field: Some("/quote/amount".to_owned()),
        },
    }
}

fn figures(problems: &[DraftProblem]) -> Vec<(String, String)> {
    problems
        .iter()
        .filter_map(|problem| match problem {
            DraftProblem::FigureInFreeText { field, numeral, .. } => {
                Some((field.clone(), numeral.clone()))
            }
            _ => None,
        })
        .collect()
}

/// A figure in a statement is refused even when the claim declares it.
///
/// This is the change: duplicating numeric truth in prose and in `numeric_items` was
/// what the model could not do reliably, so it is no longer allowed at all.
#[test]
fn a_figure_in_a_statement_is_refused_even_when_declared() {
    let draft = one_claim_draft(observed_claim(
        "紫金矿业当日成交额为 7,987,376,586.00 元人民币。",
        vec![amount_item()],
        vec!["evf_amount"],
    ));
    let problems = validate_draft(&draft, &amount_registry(), &BTreeSet::new());
    assert_eq!(
        figures(&problems),
        vec![("statement".to_owned(), "7,987,376,586.00".to_owned())],
        "{problems:?}"
    );
}

/// A figure-free statement with declared numbers is publishable.
#[test]
fn a_figure_free_statement_with_declared_numbers_validates() {
    let draft = one_claim_draft(observed_claim(
        "紫金矿业当日成交额已披露，具体数值见下。",
        vec![amount_item()],
        vec!["evf_amount"],
    ));
    let problems = validate_draft(&draft, &amount_registry(), &BTreeSet::new());
    assert!(problems.is_empty(), "{problems:?}");
}

/// Free text the verifier never reads is checked too.
///
/// The verifier form contains the title, the section headings and the claim lines. A
/// figure anywhere else was published without any check at all.
#[test]
fn a_figure_in_free_text_the_verifier_never_reads_is_refused() {
    let mut draft = one_claim_draft(observed_claim(
        "成交额已披露。",
        vec![amount_item()],
        vec!["evf_amount"],
    ));
    draft.executive_summary = "当日成交额约 79.87 亿元。".to_owned();
    draft.overall_uncertainty = Some("盘中波动可达 3%。".to_owned());
    draft.limitations = vec!["未覆盖成交额低于 5 亿元的标的。".to_owned()];
    draft.claims[0].uncertainty = Some("单一来源，误差可能 0.5%。".to_owned());
    draft.claims[0].assumptions = vec!["假设换手率维持 1.12%。".to_owned()];
    let problems = validate_draft(&draft, &amount_registry(), &BTreeSet::new());
    let fields: Vec<String> = figures(&problems).into_iter().map(|(f, _)| f).collect();
    for expected in [
        "executive_summary",
        "overall_uncertainty",
        "limitations[0]",
        "uncertainty",
        "assumptions[0]",
    ] {
        assert!(
            fields.iter().any(|field| field == expected),
            "`{expected}` must be checked, got {fields:?}"
        );
    }
}

/// A report-level figure is reported without a claim, so repair routes correctly.
#[test]
fn a_report_level_figure_carries_no_claim_id() {
    let mut draft = one_claim_draft(observed_claim(
        "成交额已披露。",
        vec![amount_item()],
        vec!["evf_amount"],
    ));
    draft.executive_summary = "成交额 79.87 亿元。".to_owned();
    let problems = validate_draft(&draft, &amount_registry(), &BTreeSet::new());
    let report_level = problems
        .iter()
        .find(|problem| problem.code() == "figure_in_free_text")
        .expect("the summary figure is refused");
    assert_eq!(report_level.claim_id(), None);
}

/// A security code, a date and a window label assert no quantity.
///
/// The masking rule is the verifier's own, so validation cannot become stricter than
/// the gate it pre-checks. Without this the absolute rule would reject ordinary prose.
#[test]
fn identifiers_dates_and_window_labels_are_not_treated_as_figures() {
    let draft = one_claim_draft(observed_claim(
        "紫金矿业(601899) 于 2026-08-26 收盘，近 6个月 处于上行结构，近 250 日均线未转正。",
        vec![NumericItem {
            value: 34.47,
            unit: Some("元".to_owned()),
            label: "最新价".to_owned(),
            provenance: NumericProvenance::Observed {
                evidence_id: "evf_price".to_owned(),
                field: Some("/quote/last".to_owned()),
            },
        }],
        vec!["evf_price"],
    ));
    let problems = validate_draft(&draft, &registry(), &BTreeSet::new());
    assert!(
        problems.is_empty(),
        "codes, dates and window labels are not figures: {problems:?}"
    );
}

/// Numbers reach the investor-facing report.
///
/// They did not before: the renderer emitted only claim prose, so a figure that
/// lived in `numeric_items` — the only place a figure carries provenance — never
/// reached the page, and the contract and the presentation disagreed about what
/// the report said.
#[test]
fn declared_numbers_appear_in_the_published_report() {
    let draft = one_claim_draft(observed_claim(
        "紫金矿业当日成交额已披露。",
        vec![amount_item()],
        vec!["evf_amount"],
    ));
    let registry = amount_registry();
    assert!(validate_draft(&draft, &registry, &BTreeSet::new()).is_empty());
    let rendered = render(&draft, &registry);
    assert!(
        rendered.markdown.contains("成交额：7987376586 元"),
        "a declared figure must be visible to the reader:\n{}",
        rendered.markdown
    );
    assert!(rendered.markdown.contains("实测数据"));
    assert!(!contains_internal_identifier(&rendered.markdown));
}

/// A decode failure must name the field, or it cannot be repaired.
///
/// `serde_json::from_value` reports `invalid type: map, expected a string` with no
/// indication of where. A live moderate run spent its entire finalization budget on
/// exactly that: an otherwise complete report whose `limitations` entries were each
/// wrapped in an object, six submissions in a row, with nothing in the diagnostic
/// that could have located the field.
#[test]
fn a_decode_failure_names_the_offending_field() {
    let malformed = json!({
        "version": REPORT_CONTRACT_VERSION,
        "title": "紫金矿业估值",
        "executive_summary": "摘要。",
        "sections": [{"heading": "估值", "claim_ids": ["c1"]}],
        "claims": [{
            "id": "c1",
            "kind": "observed_fact",
            "statement": "最新价已披露。",
            "evidence_ids": ["evf_price"]
        }],
        // The exact live shape: a string field wrapped in an object.
        "limitations": [{"limit": "未取得 PE(TTM)。"}]
    });
    let error = astock_agent_runtime::decode_draft(&malformed)
        .expect_err("a wrapped limitation must not decode");
    assert!(
        error.contains("limitations"),
        "the diagnostic must name the field, got: {error}"
    );
    assert!(
        error.contains('0'),
        "the diagnostic should locate the element, got: {error}"
    );
}

/// Incomplete numeric provenance is named at the item that lacks it.
#[test]
fn a_decode_failure_locates_incomplete_provenance() {
    let malformed = json!({
        "version": REPORT_CONTRACT_VERSION,
        "title": "计算",
        "executive_summary": "摘要。",
        "sections": [{"heading": "计算", "claim_ids": ["c1"]}],
        "claims": [{
            "id": "c1",
            "kind": "deterministic_calculation",
            "statement": "市盈率。",
            "numeric_items": [{
                "label": "市盈率", "value": 28.4,
                "provenance": "calculated"
            }]
        }]
    });
    let error = astock_agent_runtime::decode_draft(&malformed)
        .expect_err("calculated provenance without its fields must not decode");
    assert!(
        error.contains("claims") && error.contains("numeric_items"),
        "the diagnostic must locate the item, got: {error}"
    );
}

/// A well-formed draft decodes unchanged.
#[test]
fn a_well_formed_draft_decodes() {
    let good = json!({
        "version": REPORT_CONTRACT_VERSION,
        "title": "紫金矿业估值",
        "executive_summary": "摘要。",
        "sections": [{"heading": "估值", "claim_ids": ["c1"]}],
        "claims": [{
            "id": "c1",
            "kind": "observed_fact",
            "statement": "最新价已披露。",
            "evidence_ids": ["evf_price"]
        }],
        "limitations": ["未取得 PE(TTM)。"]
    });
    let draft = astock_agent_runtime::decode_draft(&good).expect("a valid draft decodes");
    assert_eq!(draft.claims.len(), 1);
    assert_eq!(draft.limitations.len(), 1);
}

/// A declared number must actually appear in the evidence it cites.
///
/// The most dangerous shape the contract can carry: a figure with a well-formed,
/// existing citation that the citation does not contain. Validation used to check
/// that provenance was *present*, not that it was *true*, which showed up on a live
/// run as a validation/verification disagreement — the one thing the shared numeral
/// rule exists to prevent.
#[test]
fn a_declared_number_that_its_evidence_does_not_contain_is_refused() {
    let draft = one_claim_draft(observed_claim(
        "最新价已披露。",
        vec![NumericItem {
            // The registry records 34.47 for `evf_price`.
            value: 34.99,
            unit: Some("元".to_owned()),
            label: "最新价".to_owned(),
            provenance: NumericProvenance::Observed {
                evidence_id: "evf_price".to_owned(),
                field: Some("/quote/last".to_owned()),
            },
        }],
        vec!["evf_price"],
    ));
    let problems = validate_draft(&draft, &registry(), &BTreeSet::new());
    assert!(
        problems.iter().any(|problem| matches!(
            problem,
            DraftProblem::NumberDisagreesWithEvidence { claim_id, declared, evidence_id, .. }
                if claim_id == "c1" && (*declared - 34.99).abs() < 1e-9 && evidence_id == "evf_price"
        )),
        "a figure its citation does not contain must be refused: {problems:?}"
    );
}

/// A rounded declared value is a different value.
#[test]
fn a_rounded_declared_value_is_refused() {
    let draft = one_claim_draft(observed_claim(
        "当日成交额已披露。",
        vec![NumericItem {
            // The registry records 7_987_376_586.
            value: 7_987_000_000.0,
            unit: Some("元".to_owned()),
            label: "成交额".to_owned(),
            provenance: NumericProvenance::Observed {
                evidence_id: "evf_amount".to_owned(),
                field: None,
            },
        }],
        vec!["evf_amount"],
    ));
    let problems = validate_draft(&draft, &amount_registry(), &BTreeSet::new());
    assert!(
        problems
            .iter()
            .any(|problem| problem.code() == "number_disagrees_with_evidence"),
        "{problems:?}"
    );
}

/// A percentage declared unscaled matches evidence recorded unscaled.
#[test]
fn a_declared_percentage_matches_its_evidence() {
    let mut map = registry();
    map.insert(
        "evf_change".into(),
        observed("evf_change", "tencent", "/quote/change_pct", 1.12),
    );
    let draft = one_claim_draft(observed_claim(
        "当日涨跌幅已披露。",
        vec![NumericItem {
            value: 1.12,
            unit: Some("%".to_owned()),
            label: "涨跌幅".to_owned(),
            provenance: NumericProvenance::Observed {
                evidence_id: "evf_change".to_owned(),
                field: None,
            },
        }],
        vec!["evf_change"],
    ));
    let problems = validate_draft(&draft, &map, &BTreeSet::new());
    assert!(problems.is_empty(), "{problems:?}");
}

/// Citing a document, a source name or a timestamp is not a numeric disagreement.
#[test]
fn evidence_with_no_numeric_value_is_not_judged_as_a_disagreement() {
    let mut map = registry();
    let mut timestamp = observed("evf_time", "tencent", "/fetched_at", 0.0);
    timestamp.value = Some(json!("2026-08-26T07:00:00Z"));
    map.insert("evf_time".into(), timestamp);
    let draft = one_claim_draft(observed_claim(
        "报价时间已披露。",
        vec![NumericItem {
            value: 34.47,
            unit: Some("元".to_owned()),
            label: "最新价".to_owned(),
            provenance: NumericProvenance::Observed {
                evidence_id: "evf_price".to_owned(),
                field: None,
            },
        }],
        vec!["evf_price", "evf_time"],
    ));
    let problems = validate_draft(&draft, &map, &BTreeSet::new());
    assert!(problems.is_empty(), "{problems:?}");
}

/// A calculation claim may state the observed inputs it was computed from.
///
/// Refusing them forced a claim like "市盈率 = 最新价 ÷ 每股收益" to be split across
/// claims that only make sense together, and a live run spent much of its repair
/// budget on that with no correctness benefit: every number still declares its own
/// provenance, the renderer labels each one, and the verifier still checks each
/// against its own citation.
#[test]
fn a_calculation_claim_may_state_its_observed_inputs() {
    let draft = one_claim_draft(Claim {
        id: "c1".to_owned(),
        kind: ClaimKind::DeterministicCalculation,
        statement: "市盈率由最新价与每股收益计算得出。".to_owned(),
        evidence_ids: Vec::new(),
        numeric_items: vec![
            NumericItem {
                value: 28.49,
                unit: Some("倍".to_owned()),
                label: "市盈率".to_owned(),
                provenance: NumericProvenance::Calculated {
                    calculation_evidence_id: "evf_pe".to_owned(),
                    operation: "divide".to_owned(),
                    input_evidence_ids: vec!["evf_price".to_owned()],
                },
            },
            NumericItem {
                value: 34.47,
                unit: Some("元".to_owned()),
                label: "最新价".to_owned(),
                provenance: NumericProvenance::Observed {
                    evidence_id: "evf_price".to_owned(),
                    field: Some("/quote/last".to_owned()),
                },
            },
        ],
        confidence: None,
        uncertainty: None,
        assumptions: Vec::new(),
        disclosed_conflicts: Vec::new(),
    });
    let problems = validate_draft(&draft, &registry(), &BTreeSet::new());
    assert!(
        problems.is_empty(),
        "a calculation may state its inputs: {problems:?}"
    );
    // Each number is still labelled by its own provenance, which is what makes the
    // relaxation safe.
    let rendered = render(&draft, &registry());
    assert!(rendered.markdown.contains("确定性计算"));
    assert!(rendered.markdown.contains("实测数据"));
}

/// An observed fact still may not carry a derived value.
#[test]
fn an_observed_fact_still_may_not_carry_a_calculated_value() {
    let draft = one_claim_draft(Claim {
        id: "c1".to_owned(),
        kind: ClaimKind::ObservedFact,
        statement: "市盈率见下方数值。".to_owned(),
        evidence_ids: vec!["evf_price".to_owned()],
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
        uncertainty: None,
        assumptions: Vec::new(),
        disclosed_conflicts: Vec::new(),
    });
    let problems = validate_draft(&draft, &registry(), &BTreeSet::new());
    assert!(
        problems
            .iter()
            .any(|problem| problem.code() == "unsupported_observed_number"),
        "a derived value must never be presentable as an observation: {problems:?}"
    );
}
