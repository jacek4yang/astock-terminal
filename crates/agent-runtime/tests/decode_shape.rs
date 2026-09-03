//! Decode-shape repair: every missing field, not the first.
//!
//! A live moderate run (recorded in the durable effect log of task
//! `03ab1442`) failed **5 of 8 finalization attempts at decode**, each
//! reporting only serde's first missing field while the model was otherwise
//! converging — findings per validation round fell 142 → 40 → 20 → 8. The
//! shapes below are those real drafts: a stub claim carrying only
//! `confidence` and `evidence_ids` (twice), an observed numeric item without
//! its `evidence_id`, a calculated numeric item without `value`, `label` or
//! `operation`, and a draft that is only a claim list. One error per attempt
//! is exactly the loop that consumed the budget: the model fixes one field,
//! resubmits, and is told about the next one.
//!
//! The fix is not a relaxation. Every field below is still required; the
//! decode failure now names them all in one round.

use astock_agent_runtime::decode_draft;
use serde_json::json;

/// Every missing field is reported, not just the first.
#[test]
fn all_missing_numeric_item_fields_are_reported_at_once() {
    // Real shape from the run: a calculated item with no value, label or operation.
    let arguments = json!({
        "version": "astock-report-contract-v1",
        "title": "紫金矿业分析",
        "executive_summary": "覆盖估值、趋势与风险。",
        "sections": [{"heading": "一", "claim_ids": ["c_revenue_growth"]}],
        "claims": [{
            "id": "c_revenue_growth",
            "kind": "deterministic_calculation",
            "statement": "营收情况见下方数值",
            "numeric_items": [{
                "calculation_evidence_id": "evf_906f48eda5b5528e39a55c90",
                "operation": "div(sub(revenue_2025, revenue_2024), revenue_2024)",
                "input_evidence_ids": ["evf_643c1b5a4e481bc4edbd095b"]
            }]
        }]
    });
    let error = decode_draft(&arguments).expect_err("the item must still fail to decode");
    assert!(error.contains("value"), "value must be named: {error}");
    assert!(error.contains("label"), "label must be named: {error}");
    assert!(
        error.contains("provenance"),
        "provenance must be named: {error}"
    );
    // Without a tag the calculated branch is inferred from the keys present.
    assert!(
        error.contains("operation"),
        "operation must be named when calculation_evidence_id is present: {error}"
    );
}

/// A claim stub carrying only confidence and evidence lists names every
/// required claim field in one round.
#[test]
fn all_missing_claim_fields_are_reported_at_once() {
    // Real shape from the run: claims[11] with only confidence + evidence_ids.
    let arguments = json!({
        "version": "astock-report-contract-v1",
        "title": "紫金矿业分析",
        "executive_summary": "覆盖估值、趋势与风险。",
        "sections": [{"heading": "一", "claim_ids": ["c11"]}],
        "claims": [
            {"id": "c11", "confidence": "high", "evidence_ids": ["evf_54246b0efea7e1f9f8a93abf"]}
        ]
    });
    let error = decode_draft(&arguments).expect_err("the claim must still fail to decode");
    assert!(error.contains("kind"), "kind must be named: {error}");
    assert!(
        error.contains("statement"),
        "statement must be named: {error}"
    );
}

/// A top-level draft that is only a claim list names all missing report fields.
#[test]
fn all_missing_report_fields_are_reported_at_once() {
    // Real shape from the run: attempt 1 carried `claims` and nothing else.
    let arguments = json!({
        "claims": [{"id": "c1", "kind": "observed_fact", "statement": "见下方数值"}]
    });
    let error = decode_draft(&arguments).expect_err("the draft must still fail to decode");
    assert!(error.contains("title"), "title must be named: {error}");
    assert!(
        error.contains("executive_summary"),
        "executive_summary must be named: {error}"
    );
    assert!(
        error.contains("sections"),
        "sections must be named: {error}"
    );
}

/// An observed numeric item missing only its evidence identifier still names
/// exactly that field — the common single-field case stays one-field cheap.
#[test]
fn a_single_missing_field_is_named_precisely() {
    let arguments = json!({
        "version": "astock-report-contract-v1",
        "title": "紫金矿业分析",
        "executive_summary": "覆盖估值、趋势与风险。",
        "sections": [{"heading": "一", "claim_ids": ["c1"]}],
        "claims": [{
            "id": "c1",
            "kind": "observed_fact",
            "statement": "分红情况见下方数值",
            "numeric_items": [{
                "label": "cash_div_per_10_2025",
                "value": 3.8,
                "provenance": "observed"
            }]
        }]
    });
    let error = decode_draft(&arguments).expect_err("the item must still fail to decode");
    assert!(
        error.contains("evidence_id"),
        "evidence_id must be named: {error}"
    );
    assert!(
        !error.contains("label: missing"),
        "no other field is missing: {error}"
    );
}

/// A fully valid draft still decodes — the scan adds failures to nothing.
#[test]
fn a_complete_draft_still_decodes() {
    let arguments = json!({
        "version": "astock-report-contract-v1",
        "title": "紫金矿业分析",
        "executive_summary": "覆盖估值、趋势与风险。",
        "sections": [{"heading": "一", "claim_ids": ["c1"]}],
        "claims": [{
            "id": "c1",
            "kind": "observed_fact",
            "statement": "最新价见下方数值",
            "numeric_items": [{
                "label": "最新价",
                "value": 34.63,
                "provenance": "observed",
                "evidence_id": "evf_8c5001fbae285f0228c2c0de"
            }]
        }]
    });
    decode_draft(&arguments).expect("a complete draft decodes");
}
