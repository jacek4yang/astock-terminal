//! Regression fixtures for the report-verifier failure classes observed live.
//!
//! Shapes are taken from the run recorded in
//! `docs/releases/v7.0.0-live-acceptance.md`, which produced 1,178 blocking
//! findings. Values are real research data only — no credential, cookie or
//! authorization material is present, and none is needed to reproduce any class.
//!
//! Each class has three cases: the shape that used to be wrongly rejected must now
//! pass, a genuinely invalid variant must still be rejected, and the valid variant
//! must survive mutation only where the mutation is semantically harmless.

use serde_json::{json, Value};

/// Build one evidence-registry fact.
fn fact(id: &str, path: &str, value: Value, source: &str, observed_at: Option<&str>) -> Value {
    let mut row = json!({
        "evidence_id": id,
        "path": path,
        "value": value,
        "source": source,
        "quality_blocking": false,
        "source_version_id": "srcver-1",
    });
    row["observed_at"] = match observed_at {
        Some(value) => json!(value),
        None => Value::Null,
    };
    row
}

fn task_spec() -> Value {
    json!({
        "objective": "分析紫金矿业当前投资价值",
        "security_universe": ["601899"],
        "as_of": "2026-08-26",
        "research_start": "2026-02-26",
        "research_end": "2026-08-26",
        "investment_horizon": "medium",
        "comparison_benchmark": "000300",
        "output_type": "research_report",
        "evidence_requirement": "standard",
    })
}

/// Wrap facts into the context envelope the verifier receives.
fn context(facts: Vec<Value>) -> Value {
    json!({
        "task_spec": task_spec(),
        "securities": [{ "evidence_registry": { "facts": facts } }],
    })
}

fn verify(report: &str, context: Value) -> Value {
    let payload = json!({ "report": report, "context": context, "task_spec": task_spec() });
    let request = astock_protocol::RequestEnvelope {
        protocol_version: astock_protocol::PROTOCOL_VERSION,
        request_id: "verify-fixture".into(),
        kind: "research.agent_report_verify".into(),
        payload,
        deadline_ms: None,
        cancellation_id: None,
    };
    let response = futures::executor::block_on(async {
        let engine = astock_engine::Engine::initialize_at(
            tempfile::tempdir().expect("temp data root").path(),
        )
        .await
        .expect("engine initializes");
        engine.dispatch(&request).await
    });
    assert!(
        response.ok,
        "verification request itself failed: {:?}",
        response.error
    );
    response.payload
}

fn findings(result: &Value) -> Vec<String> {
    result["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .map(|value| value.as_str().unwrap_or_default().to_owned())
        .collect()
}

fn has_class(result: &Value, class: &str) -> bool {
    findings(result)
        .iter()
        .any(|finding| finding.starts_with(class))
}

// ---------------------------------------------------------------------------
// Class 1: retrieval-time drift must not read as a value conflict.
// ---------------------------------------------------------------------------

/// The exact live shape: `/adjustment = "qfq"` from JoinQuant, registered twice
/// thirty seconds apart. Identical assertion, different retrieval moment.
#[test]
fn the_same_fact_seen_twice_at_different_times_is_not_a_conflict() {
    let facts = vec![
        fact(
            "evf_7591ab29c48b93945f419123",
            "/adjustment",
            json!("qfq"),
            "joinquant",
            Some("2026-08-26T16:43:22.437463817Z"),
        ),
        fact(
            "evf_7591ab29c48b93945f419123",
            "/adjustment",
            json!("qfq"),
            "joinquant",
            Some("2026-08-26T16:43:52.064713428Z"),
        ),
        fact(
            "evf_price",
            "/quote/last",
            json!(34.47),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_pct",
            "/quote/change_pct",
            json!(0.0235),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_turnover",
            "/quote/turnover",
            json!(10_920_000_000.0_f64),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
    ];
    let report = "\
最新价 34.47 元【E:evf_price】
涨跌幅 2.35%【E:evf_pct】
成交额 109.2 亿元【E:evf_turnover】
复权口径 qfq【E:evf_7591ab29c48b93945f419123】
";
    let result = verify(report, context(facts));
    assert!(
        !has_class(&result, "invalid_or_conflicting_evidence_id"),
        "a re-observation of an identical assertion must not be a conflict: {:?}",
        findings(&result)
    );
}

/// A genuine disagreement on the same identifier must still block.
#[test]
fn the_same_identifier_asserting_a_different_value_still_conflicts() {
    let facts = vec![
        fact(
            "evf_price",
            "/quote/last",
            json!(34.47),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        // Same identifier, contradictory value.
        fact(
            "evf_price",
            "/quote/last",
            json!(31.10),
            "tencent",
            Some("2026-08-26T07:00:05Z"),
        ),
    ];
    let report = "最新价 34.47 元【E:evf_price】\n";
    let result = verify(report, context(facts));
    assert!(
        has_class(&result, "invalid_or_conflicting_evidence_id"),
        "a contradictory value on one identifier must block: {:?}",
        findings(&result)
    );
}

/// A conflict the report never cites must not block it.
///
/// On the live run 473 of 476 conflicts were never cited, so they blocked
/// publication over evidence the report did not rely on.
#[test]
fn an_uncited_conflict_does_not_block_publication() {
    let mut facts = vec![
        fact(
            "evf_price",
            "/quote/last",
            json!(34.47),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_pct",
            "/quote/change_pct",
            json!(0.0235),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_turnover",
            "/quote/turnover",
            json!(10_920_000_000.0_f64),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_eps",
            "/fundamentals/eps",
            json!(1.21),
            "disclosure",
            Some("2026-04-20T00:00:00Z"),
        ),
    ];
    // An unrelated identifier genuinely disagrees, and is never cited.
    facts.push(fact(
        "evf_unrelated",
        "/other/metric",
        json!(1.0),
        "eastmoney",
        Some("2026-08-26T07:00:00Z"),
    ));
    facts.push(fact(
        "evf_unrelated",
        "/other/metric",
        json!(2.0),
        "eastmoney",
        Some("2026-08-26T07:00:01Z"),
    ));

    let report = "\
最新价 34.47 元【E:evf_price】
涨跌幅 2.35%【E:evf_pct】
成交额 109.2 亿元【E:evf_turnover】
每股收益 1.21 元【E:evf_eps】
";
    let result = verify(report, context(facts));
    assert!(
        !has_class(&result, "invalid_or_conflicting_evidence_id"),
        "an uncited conflict must not block: {:?}",
        findings(&result)
    );
}

// ---------------------------------------------------------------------------
// Class 2: a deterministic calculation has no observation time.
// ---------------------------------------------------------------------------

/// All 31 live `evidence_time_missing` facts came from `astock-compute`.
#[test]
fn a_calculation_result_is_not_required_to_carry_an_observation_time() {
    let facts = vec![
        fact(
            "evf_price",
            "/quote/last",
            json!(34.47),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_eps",
            "/fundamentals/eps",
            json!(1.21),
            "disclosure",
            Some("2026-04-20T00:00:00Z"),
        ),
        fact(
            "evf_shares",
            "/fundamentals/shares",
            json!(2.659e9),
            "disclosure",
            Some("2026-04-20T00:00:00Z"),
        ),
        // Derived: computed from the inputs above, so it has no observation time.
        fact("evf_pe", "/value", json!(28.49), "astock-compute", None),
    ];
    let report = "\
最新价 34.47 元【E:evf_price】
每股收益 1.21 元【E:evf_eps】
总股本 2.659 亿股【E:evf_shares】
市盈率 28.49 倍【E:evf_pe】
";
    let result = verify(report, context(facts));
    assert!(
        !has_class(&result, "evidence_time_missing"),
        "a calculation result must not be judged against observation semantics: {:?}",
        findings(&result)
    );
}

/// An *observed* fact without a timestamp must still block.
#[test]
fn an_observation_without_a_timestamp_still_blocks() {
    let facts = vec![
        fact("evf_price", "/quote/last", json!(34.47), "tencent", None),
        fact(
            "evf_pct",
            "/quote/change_pct",
            json!(0.0235),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_turnover",
            "/quote/turnover",
            json!(1.092e10),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_eps",
            "/fundamentals/eps",
            json!(1.21),
            "disclosure",
            Some("2026-04-20T00:00:00Z"),
        ),
    ];
    let report = "\
最新价 34.47 元【E:evf_price】
涨跌幅 2.35%【E:evf_pct】
成交额 109.2 亿元【E:evf_turnover】
每股收益 1.21 元【E:evf_eps】
";
    let result = verify(report, context(facts));
    assert!(
        has_class(&result, "evidence_time_missing"),
        "an observed market fact with no timestamp must block: {:?}",
        findings(&result)
    );
}

// ---------------------------------------------------------------------------
// Class 3: non-financial digit runs are not financial claims.
// ---------------------------------------------------------------------------

/// Security codes, dates, headings and period labels assert no quantity.
#[test]
fn identifiers_dates_headings_and_period_labels_are_not_numeric_claims() {
    let facts = vec![
        fact(
            "evf_price",
            "/quote/last",
            json!(34.47),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_pct",
            "/quote/change_pct",
            json!(0.0235),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_turnover",
            "/quote/turnover",
            json!(1.092e10),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_eps",
            "/fundamentals/eps",
            json!(1.21),
            "disclosure",
            Some("2026-04-20T00:00:00Z"),
        ),
    ];
    // Every uncited line here is drawn from the live report and asserts no
    // quantity: a heading with a security code, a data date, a window label,
    // a session time and Chinese section numbering.
    let report = "\
## 紫金矿业（601899）当前投资价值分析报告
**报告时间：2026-08-26（数据观察日）；前复权口径**
### 6个月股价走势
行情更新至 15:00:00 CST
第一步：证据采集
1. 概览
最新价 34.47 元【E:evf_price】
涨跌幅 2.35%【E:evf_pct】
成交额 109.2 亿元【E:evf_turnover】
每股收益 1.21 元【E:evf_eps】
";
    let result = verify(report, context(facts));
    assert!(
        !has_class(&result, "numeric_claim_without_evidence"),
        "non-financial digit runs must not be treated as unsupported claims: {:?}",
        findings(&result)
    );
}

/// A genuine uncited financial figure must still block.
#[test]
fn an_uncited_financial_figure_still_blocks() {
    let facts = vec![
        fact(
            "evf_price",
            "/quote/last",
            json!(34.47),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_pct",
            "/quote/change_pct",
            json!(0.0235),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_turnover",
            "/quote/turnover",
            json!(1.092e10),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_eps",
            "/fundamentals/eps",
            json!(1.21),
            "disclosure",
            Some("2026-04-20T00:00:00Z"),
        ),
    ];
    let report = "\
最新价 34.47 元【E:evf_price】
涨跌幅 2.35%【E:evf_pct】
成交额 109.2 亿元【E:evf_turnover】
每股收益 1.21 元【E:evf_eps】
流通市值约 8928 亿元
";
    let result = verify(report, context(facts));
    assert!(
        has_class(&result, "numeric_claim_without_evidence"),
        "an uncited market-capitalisation figure must block: {:?}",
        findings(&result)
    );
}

/// A six-digit run that is part of a larger quantity is not a security code.
#[test]
fn a_six_digit_run_inside_a_larger_quantity_is_still_a_claim() {
    let facts = vec![
        fact(
            "evf_price",
            "/quote/last",
            json!(34.47),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_pct",
            "/quote/change_pct",
            json!(0.0235),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_turnover",
            "/quote/turnover",
            json!(1.092e10),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_eps",
            "/fundamentals/eps",
            json!(1.21),
            "disclosure",
            Some("2026-04-20T00:00:00Z"),
        ),
    ];
    // 320,506,024,370 contains "320506" but is a revenue figure, not a code.
    let report = "\
最新价 34.47 元【E:evf_price】
涨跌幅 2.35%【E:evf_pct】
成交额 109.2 亿元【E:evf_turnover】
每股收益 1.21 元【E:evf_eps】
营业收入 320,506,024,370 元
";
    let result = verify(report, context(facts));
    assert!(
        has_class(&result, "numeric_claim_without_evidence"),
        "masking must not swallow a quantity that merely contains six digits: {:?}",
        findings(&result)
    );
}

// ---------------------------------------------------------------------------
// Model-invented citation labels must remain rejected.
// ---------------------------------------------------------------------------

/// The live run invented `计算-BPS` and `财报口径-EPS-2024`.
#[test]
fn invented_citation_labels_are_still_rejected() {
    let facts = vec![fact(
        "evf_eps",
        "/fundamentals/eps",
        json!(1.21),
        "disclosure",
        Some("2026-04-20T00:00:00Z"),
    )];
    let report = "每股收益 1.21 元【E:财报口径-EPS-2024】\n每股净资产 8.65 元【E:计算-BPS】\n";
    let result = verify(report, context(facts));
    let found = findings(&result);
    assert!(
        found.iter().any(|f| f.contains("财报口径-EPS-2024")),
        "an invented label must be reported: {found:?}"
    );
    assert!(
        found.iter().any(|f| f.contains("计算-BPS")),
        "an invented label must be reported: {found:?}"
    );
    assert_eq!(result["passed"], json!(false));
}

// ---------------------------------------------------------------------------
// Mutation: a valid report must fail once its provenance is broken.
// ---------------------------------------------------------------------------

#[test]
fn mutating_a_valid_report_breaks_verification_in_the_expected_way() {
    let facts = vec![
        fact(
            "evf_price",
            "/quote/last",
            json!(34.47),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_pct",
            "/quote/change_pct",
            json!(0.0235),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_turnover",
            "/quote/turnover",
            json!(1.092e10),
            "tencent",
            Some("2026-08-26T07:00:00Z"),
        ),
        fact(
            "evf_eps",
            "/fundamentals/eps",
            json!(1.21),
            "disclosure",
            Some("2026-04-20T00:00:00Z"),
        ),
    ];
    let valid = "\
最新价 34.47 元【E:evf_price】
涨跌幅 2.35%【E:evf_pct】
成交额 109.2 亿元【E:evf_turnover】
每股收益 1.21 元【E:evf_eps】
";

    // Removing a citation leaves an unsupported figure.
    let without_citation = valid.replace("【E:evf_eps】", "");
    assert!(
        has_class(
            &verify(&without_citation, context(facts.clone())),
            "numeric_claim_without_evidence"
        ),
        "removing a citation must be caught"
    );

    // Replacing an identifier with a fake one must be caught.
    let fake = valid.replace("evf_eps", "evf_does_not_exist");
    assert!(
        has_class(
            &verify(&fake, context(facts.clone())),
            "unknown_evidence_id"
        ),
        "a fabricated identifier must be caught"
    );

    // Changing a value away from its evidence must be caught.
    let altered = valid.replace("34.47", "41.90");
    assert!(
        has_class(
            &verify(&altered, context(facts)),
            "numeric_claim_not_reproduced"
        ),
        "an altered figure must be caught"
    );
}
