//! The Runtime-tool boundary.
//!
//! Report finalization and evidence discovery act on Runtime state, not on the
//! Engine. Keeping that in the type system matters: a report submission dispatched
//! as an Engine effect would be a mutation reachable from the model, and the
//! product's whole safety story rests on the Engine surface staying bounded and
//! read-only.

use astock_agent_runtime::{default_registry, ToolHandler, ToolRisk};

#[test]
fn finalization_and_evidence_discovery_are_runtime_operations() {
    let registry = default_registry();
    for name in ["submit_report", "search_evidence"] {
        let tool = registry
            .get(name)
            .unwrap_or_else(|| panic!("`{name}` must be registered"));
        assert_eq!(
            tool.handler,
            ToolHandler::Runtime,
            "`{name}` must be served by the Runtime, never dispatched to the Engine"
        );
        assert!(
            tool.engine_kind.is_empty(),
            "`{name}` must name no Engine request kind, or it could be dispatched"
        );
    }
}

#[test]
fn every_other_tool_still_dispatches_to_a_named_engine_operation() {
    let registry = default_registry();
    for name in registry.names() {
        let tool = registry.get(name).expect("a registered tool resolves");
        if tool.handler == ToolHandler::Runtime {
            continue;
        }
        assert!(
            !tool.engine_kind.is_empty(),
            "`{name}` dispatches to the Engine and must name a request kind"
        );
    }
}

/// The whole registry stays read-only. Finalization does not change that: it
/// validates and renders, and publication remains the Runtime's decision after the
/// independent verifier has run.
#[test]
fn the_registry_exposes_no_mutating_capability() {
    let registry = default_registry();
    for name in registry.names() {
        let tool = registry.get(name).expect("a registered tool resolves");
        assert_eq!(
            tool.risk,
            ToolRisk::ReadOnly,
            "`{name}` must be read-only; the Agent has no mutating capability"
        );
    }
}

/// The model must not be able to submit a fabricated evidence namespace.
///
/// The live failure invented `计算-BPS` and `财报口径-EPS-2024`. The schema now
/// constrains the shape, which rejects those before the contract even checks
/// whether the identifier exists.
#[test]
fn the_submit_report_schema_constrains_evidence_identifier_shape() {
    let registry = default_registry();
    let schema = &registry
        .get("submit_report")
        .expect("submit_report is registered")
        .input_schema;
    let rendered = serde_json::to_string(schema).expect("schema serializes");
    assert!(
        rendered.contains("^evf_[A-Za-z0-9_]+$"),
        "evidence identifiers must be pattern-constrained in the schema"
    );
    // Every provenance class the contract understands must be offered.
    for provenance in ["observed", "calculated", "user_assumption", "estimated"] {
        assert!(
            rendered.contains(provenance),
            "the schema must offer the `{provenance}` provenance class"
        );
    }
    // Every claim kind must be offered.
    for kind in [
        "observed_fact",
        "deterministic_calculation",
        "inference",
        "estimate",
        "scenario",
        "unknown",
    ] {
        assert!(
            rendered.contains(kind),
            "the schema must offer kind `{kind}`"
        );
    }
    // Unknown properties are refused, so a stray field cannot smuggle content.
    assert!(rendered.contains("additionalProperties"));
}

/// Evidence discovery must be bounded, or it recreates the context overflow.
#[test]
fn evidence_discovery_is_bounded() {
    let registry = default_registry();
    let schema = serde_json::to_string(
        &registry
            .get("search_evidence")
            .expect("search_evidence is registered")
            .input_schema,
    )
    .expect("schema serializes");
    assert!(
        schema.contains("maximum"),
        "the result limit must be bounded: {schema}"
    );
    assert!(schema.contains("\"limit\""));
}

/// The shortcut is that the model never needs the whole registry in context.
#[test]
fn the_registry_stays_small_enough_to_advertise_in_full() {
    let registry = default_registry();
    let count = registry.names().count();
    assert!(
        (10..=20).contains(&count),
        "the tool surface should stay compact and purposeful, found {count}"
    );
}

/// Tool descriptions are machine control surface and stay English.
///
/// They are re-sent on every round, so their cost is paid repeatedly, and they sit
/// in the cacheable prefix where byte stability matters. User-visible output
/// language is carried separately by `output_language`; nothing here dictates the
/// language of the report.
#[test]
fn tool_descriptions_are_english_control_surface() {
    let registry = default_registry();
    for name in registry.names() {
        let tool = registry.get(name).expect("a registered tool resolves");
        let cjk = tool
            .description
            .chars()
            .filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c))
            .count();
        assert_eq!(
            cjk, 0,
            "`{name}` description should be English control surface, found {cjk} CJK characters"
        );
        assert!(
            !tool.description.is_empty() && tool.description.len() < 700,
            "`{name}` description should be concise, found {} bytes",
            tool.description.len()
        );
    }
}

/// The finalization description must not reintroduce hand-formatted citations.
#[test]
fn the_finalization_description_forbids_hand_written_citations() {
    let registry = default_registry();
    let description = &registry
        .get("submit_report")
        .expect("submit_report is registered")
        .description;
    assert!(
        !description.contains("【E:"),
        "the description must not show citation markup as something to write"
    );
    assert!(description.contains("Do not write citation markup"));
    assert!(
        description.contains("output_language"),
        "statements must follow the task output language"
    );
    // Every provenance class is named where the model will look for it.
    for class in ["observed", "calculated", "user_assumption", "estimated"] {
        assert!(description.contains(class), "`{class}` must be described");
    }
}
