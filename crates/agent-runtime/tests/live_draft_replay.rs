//! Replay real submitted drafts against the free-text rule, offline.
//!
//! Iterating this rule against the live provider costs a paid run per attempt, and it
//! took fifteen of them to learn that most refusals were masking gaps rather than
//! model misbehaviour: Chinese counters, lookback windows, reporting periods, a
//! distribution-ratio denominator. Each discovery needed another run.
//!
//! The fixture is 49 drafts the model actually submitted during those runs. Replaying
//! them here turns "does the rule accept normal analyst prose?" into a deterministic
//! test, so a masking gap is found in a second instead of a paid round trip. The rule
//! is not relaxed to make them pass — the point is to see precisely *what* is refused
//! and decide whether it is a claim at all.
//!
//! This is a measurement harness, so it reports rather than asserting a target: the
//! only hard assertion is that nothing regresses into flagging a field that carries no
//! figure at all.

use std::collections::BTreeMap;

use astock_agent_runtime::strip_placeholders;
use serde_json::Value;

fn drafts() -> Vec<Value> {
    let raw = include_str!("fixtures/live-submitted-drafts.json");
    serde_json::from_str(raw).expect("the fixture parses")
}

/// Every free-text field of a draft, keyed the way validation names it.
fn free_text_fields(draft: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let text = |value: Option<&Value>| value.and_then(Value::as_str).unwrap_or("").to_owned();
    out.push(("title".to_owned(), text(draft.get("title"))));
    out.push((
        "executive_summary".to_owned(),
        text(draft.get("executive_summary")),
    ));
    out.push((
        "overall_uncertainty".to_owned(),
        text(draft.get("overall_uncertainty")),
    ));
    if let Some(items) = draft.get("limitations").and_then(Value::as_array) {
        for (index, item) in items.iter().enumerate() {
            out.push((format!("limitations[{index}]"), text(Some(item))));
        }
    }
    if let Some(claims) = draft.get("claims").and_then(Value::as_array) {
        for claim in claims {
            let id = text(claim.get("id"));
            out.push((format!("{id}.statement"), text(claim.get("statement"))));
            if claim.get("uncertainty").is_some() {
                out.push((format!("{id}.uncertainty"), text(claim.get("uncertainty"))));
            }
            if let Some(items) = claim.get("assumptions").and_then(Value::as_array) {
                for (index, item) in items.iter().enumerate() {
                    out.push((format!("{id}.assumptions[{index}]"), text(Some(item))));
                }
            }
        }
    }
    out.retain(|(_, value)| !value.trim().is_empty());
    out
}

/// What the rule refuses across every real draft, grouped by field kind.
/// Only drafts written under the placeholder contract, which is what ships.
fn placeholder_era(drafts: &[Value]) -> Vec<Value> {
    drafts
        .iter()
        .filter(|draft| {
            draft
                .get("claims")
                .and_then(Value::as_array)
                .is_some_and(|claims| {
                    claims.iter().any(|claim| {
                        claim
                            .get("statement")
                            .and_then(Value::as_str)
                            .is_some_and(|text| text.contains('{'))
                    })
                })
        })
        .cloned()
        .collect()
}

#[test]
fn report_what_the_free_text_rule_refuses_across_real_drafts() {
    let all = drafts();
    let drafts = placeholder_era(&all);
    println!(
        "all drafts: {} | placeholder-era: {}",
        all.len(),
        drafts.len()
    );
    assert!(drafts.len() >= 20, "the fixture should hold real drafts");

    let mut by_kind: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut clean_drafts = 0usize;
    for draft in &drafts {
        let mut draft_clean = true;
        for (field, text) in free_text_fields(draft) {
            let prose = strip_placeholders(&text);
            let found = astock_engine::financial_numerals(&prose);
            if found.is_empty() {
                continue;
            }
            draft_clean = false;
            let kind = if field.ends_with(".statement") {
                "statement"
            } else if field.contains(".assumptions") {
                "assumptions"
            } else if field.contains(".uncertainty") {
                "uncertainty"
            } else if field.starts_with("limitations") {
                "limitations"
            } else if field == "executive_summary" {
                "executive_summary"
            } else if field == "overall_uncertainty" {
                "overall_uncertainty"
            } else {
                "title"
            };
            for numeral in found {
                by_kind
                    .entry(kind)
                    .or_default()
                    .push(format!("{field}: {}", numeral.raw));
            }
        }
        if draft_clean {
            clean_drafts += 1;
        }
    }

    println!("drafts: {}", drafts.len());
    println!("drafts with no refused figure: {clean_drafts}");
    for (kind, hits) in &by_kind {
        println!("{kind}: {} refused", hits.len());
        for sample in hits.iter().take(6) {
            println!("    {sample}");
        }
    }

    // The invariant: a field the rule flags must actually contain a figure. A masking
    // gap that flags text asserting no quantity is a defect in the rule, not in the
    // report, and this is what the fixture exists to surface.
    for hits in by_kind.values() {
        for hit in hits {
            let numeral = hit.rsplit(": ").next().unwrap_or_default();
            assert!(
                numeral.chars().any(|character| character.is_ascii_digit()),
                "a refusal must name a real numeral, got `{hit}`"
            );
        }
    }
}
