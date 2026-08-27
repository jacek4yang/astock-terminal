//! The Agent tool registry as a committed, machine-readable manifest.
//!
//! Capability parity used to ask a *retired* implementation whether a request kind
//! was reachable: `capability-parity-check.mjs` grepped
//! `app-moon/agent_worker/main.mbt` for `"kind" =>`. A removed architecture cannot
//! be a live oracle for what the product can do, and text-scanning a source file is
//! brittle besides.
//!
//! The Rust registry is now the source of truth and this test keeps a canonical
//! projection of it committed, so Node-only checks can read the real capability
//! surface without building Rust. Regenerate with:
//!
//! ```text
//! ASTOCK_WRITE_TOOL_MANIFEST=1 cargo test -p astock-agent-runtime --test tool_manifest
//! ```

use std::path::PathBuf;

use astock_agent_runtime::{default_registry, ToolHandler};
use serde_json::json;

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../protocol/agent-tool-manifest.json")
}

fn render_manifest() -> String {
    let registry = default_registry();
    let mut tools: Vec<serde_json::Value> = registry
        .names()
        .map(|name| {
            let tool = registry.get(name).expect("a registered tool resolves");
            json!({
                "name": tool.name,
                "handler": match tool.handler {
                    ToolHandler::Engine => "engine",
                    ToolHandler::Runtime => "runtime",
                },
                "engine_kind": tool.engine_kind,
                "freshness": tool.freshness,
            })
        })
        .collect();
    tools.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    let document = json!({
        "note": "Generated from astock_agent_runtime::default_registry(). \
                 Regenerate with ASTOCK_WRITE_TOOL_MANIFEST=1 cargo test -p astock-agent-runtime --test tool_manifest",
        "runtime": "astock-agent-runtime",
        "tools": tools,
    });
    format!(
        "{}\n",
        serde_json::to_string_pretty(&document).expect("the manifest serializes")
    )
}

/// The committed manifest must match the live registry.
#[test]
fn the_committed_tool_manifest_matches_the_runtime_registry() {
    let rendered = render_manifest();
    let path = manifest_path();
    if std::env::var_os("ASTOCK_WRITE_TOOL_MANIFEST").is_some() {
        std::fs::write(&path, &rendered).expect("the manifest is writable");
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}; regenerate the manifest", path.display()));
    assert_eq!(
        committed.replace("\r\n", "\n"),
        rendered,
        "the committed tool manifest drifted from the registry; regenerate it"
    );
}

/// Every Engine-dispatched tool names a request kind, and Runtime tools name none.
///
/// This is what makes the manifest usable as a reachability oracle: a capability is
/// reachable through the Agent exactly when some tool dispatches to its request kind.
#[test]
fn the_manifest_distinguishes_engine_dispatch_from_runtime_handling() {
    let registry = default_registry();
    for name in registry.names() {
        let tool = registry.get(name).expect("a registered tool resolves");
        match tool.handler {
            ToolHandler::Engine => assert!(
                !tool.engine_kind.is_empty(),
                "`{name}` dispatches to the Engine and must name a request kind"
            ),
            ToolHandler::Runtime => assert!(
                tool.engine_kind.is_empty(),
                "`{name}` is Runtime-served and must name no request kind"
            ),
        }
    }
}
