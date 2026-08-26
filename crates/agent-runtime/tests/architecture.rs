//! Architecture enforcement.
//!
//! The product converges on one production Agent implementation with
//! presentation-only adapters. That is a property of the dependency graph, so
//! it is checked here rather than left to review discipline:
//!
//! ```text
//! agent-runtime -> engine                    allowed
//! astock (CLI)  -> agent-runtime             allowed
//! Tauri adapter -> agent-runtime             allowed
//!
//! agent-runtime -> Tauri / React / Ratatui   forbidden
//! engine        -> Tauri / React / Ratatui   forbidden
//! CLI           -> domain crates directly    forbidden
//! ```
//!
//! The check reads the workspace manifests directly. It deliberately does not
//! shell out to `cargo metadata`, so it stays fast, offline and dependency-free.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Repository root, derived from this crate's manifest directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name> lives two levels below the workspace root")
        .to_path_buf()
}

/// Dependency names declared by a crate, across all dependency tables.
///
/// Table-aware parsing keeps `[package]` metadata such as `name` or
/// `description` from being mistaken for a dependency.
fn declared_dependencies(manifest: &str) -> BTreeSet<String> {
    let mut dependencies = BTreeSet::new();
    let mut in_dependency_table = false;
    for raw in manifest.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            // Matches [dependencies], [dev-dependencies], [build-dependencies]
            // and their [target.'cfg(...)'.dependencies] forms.
            in_dependency_table = line.contains("dependencies");
            continue;
        }
        if !in_dependency_table || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = line.split_once('=') {
            let name = name.trim().trim_matches('"');
            if !name.is_empty() {
                dependencies.insert(name.to_string());
            }
        }
    }
    dependencies
}

fn manifest_of(crate_name: &str) -> String {
    let path = workspace_root()
        .join("crates")
        .join(crate_name)
        .join("Cargo.toml");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// Crates that own presentation. No shared runtime or engine crate may depend
/// on one, in any dependency table.
const GUI_AND_TUI_CRATES: &[&str] = &[
    "tauri",
    "tauri-build",
    "tauri-plugin-shell",
    "wry",
    "tao",
    "ratatui",
    "crossterm",
    "cursive",
    "egui",
    "iced",
    "gtk",
    "webkit2gtk",
    "dioxus",
    "slint",
];

#[test]
fn agent_runtime_owns_orchestration_without_any_presentation_dependency() {
    let dependencies = declared_dependencies(&manifest_of("agent-runtime"));
    for forbidden in GUI_AND_TUI_CRATES {
        assert!(
            !dependencies.contains(*forbidden),
            "astock-agent-runtime must stay presentation-independent so the CLI and the \
             desktop adapter can share it, but it depends on `{forbidden}`"
        );
    }
}

#[test]
fn engine_stays_gui_independent() {
    let dependencies = declared_dependencies(&manifest_of("engine"));
    for forbidden in GUI_AND_TUI_CRATES {
        assert!(
            !dependencies.contains(*forbidden),
            "astock-engine is the deterministic computation boundary and must not depend on \
             `{forbidden}`"
        );
    }
}

#[test]
fn agent_runtime_depends_on_the_engine_boundary() {
    let dependencies = declared_dependencies(&manifest_of("agent-runtime"));
    assert!(
        dependencies.contains("astock-engine"),
        "the runtime performs financial effects only through the Engine boundary"
    );
}

#[test]
fn the_cli_drives_the_shared_runtime() {
    let dependencies = declared_dependencies(&manifest_of("astock"));
    assert!(
        dependencies.contains("astock-agent-runtime"),
        "the CLI must be an adapter over the shared runtime, not a second Agent"
    );
}

#[test]
fn the_cli_does_not_reach_domain_crates_directly() {
    // The CLI may use the runtime, the Engine boundary and the provider
    // adapter. Everything else in `crates/` is domain capability that must be
    // reached through the Engine, otherwise the dependency direction that keeps
    // one production Agent implementation is broken.
    const ALLOWED_INTERNAL: &[&str] = &["astock-agent-runtime", "astock-engine", "astock-minimax"];
    let dependencies = declared_dependencies(&manifest_of("astock"));
    let offenders: Vec<&String> = dependencies
        .iter()
        .filter(|name| name.starts_with("astock-"))
        .filter(|name| !ALLOWED_INTERNAL.contains(&name.as_str()))
        .collect();
    assert!(
        offenders.is_empty(),
        "the CLI must reach domain capability through the Engine, but depends directly on \
         {offenders:?}"
    );
}

#[test]
fn the_compute_language_stays_a_pure_evaluator() {
    // The bounded calculation language must not acquire I/O or presentation
    // dependencies, because it is the component that must never permit
    // arbitrary execution.
    let dependencies = declared_dependencies(&manifest_of("compute"));
    for forbidden in GUI_AND_TUI_CRATES
        .iter()
        .chain(["reqwest", "tokio", "std::process"].iter())
    {
        assert!(
            !dependencies.contains(*forbidden),
            "astock-compute is a deterministic fuel-metered evaluator and must not depend on \
             `{forbidden}`"
        );
    }
}

#[test]
fn there_is_exactly_one_agent_runtime_crate_in_the_workspace() {
    // A second runtime crate would reintroduce the duplicate production Agent
    // the architecture exists to prevent.
    let manifest = std::fs::read_to_string(workspace_root().join("Cargo.toml"))
        .expect("read the workspace manifest");
    let runtime_members = manifest
        .lines()
        .filter(|line| line.contains("agent-runtime"))
        .count();
    assert_eq!(
        runtime_members, 1,
        "the workspace must declare exactly one Agent runtime crate"
    );
}

#[test]
fn the_dependency_parser_is_table_aware() {
    // Guards the check itself: package metadata must not be read as a
    // dependency, or the forbidden-edge assertions would silently pass.
    let manifest = r#"
[package]
name = "tauri"
description = "not a dependency"

[dependencies]
serde = "1"

[dev-dependencies]
tempfile = "3"
"#;
    let dependencies = declared_dependencies(manifest);
    assert!(dependencies.contains("serde"));
    assert!(dependencies.contains("tempfile"));
    assert!(
        !dependencies.contains("tauri"),
        "a [package] name must never be treated as a dependency"
    );
    assert!(!dependencies.contains("description"));
}
