//! Built-in seed supply-chain graph, embedded from `data/seed_graph.json`.
//!
//! The JSON carries only source-backed relations (every edge references a
//! provenance entry from its `sources` map); [`seed_if_empty`] loads it
//! idempotently — it does nothing when the graph already has nodes, and the
//! underlying upserts are conflict-safe regardless.

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::model::{Edge, Node, NodeKind, Relation};
use crate::store::{now_secs, GraphStore};

/// The embedded seed graph JSON.
pub const SEED_GRAPH_JSON: &str = include_str!("../data/seed_graph.json");

/// Minimum confidence accepted in the built-in seed (see confidence
/// semantics in [`crate::model`]).
pub const SEED_MIN_CONFIDENCE: f64 = 0.6;

#[derive(Debug, Deserialize)]
struct SeedFile {
    sources: std::collections::HashMap<String, SeedSource>,
    nodes: Vec<SeedNode>,
    edges: Vec<SeedEdge>,
}

#[derive(Debug, Deserialize)]
struct SeedSource {
    name: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct SeedNode {
    id: String,
    kind: NodeKind,
    name: String,
    #[serde(default)]
    code: Option<String>,
}

/// Positional edge row: [src, dst, relation, weight, confidence, source_key].
#[derive(Debug, Deserialize)]
struct SeedEdge(String, String, Relation, f64, f64, String);

/// Summary of a seeding run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedSummary {
    /// Nodes upserted (0 when skipped).
    pub nodes: usize,
    /// Edges upserted (0 when skipped).
    pub edges: usize,
    /// True when the graph was already populated and nothing was done.
    pub skipped: bool,
}

/// Parse and validate the embedded seed graph. Pure; exposed for tests and
/// for the enrichment module's consistency checks.
pub fn parse_seed() -> Result<(Vec<Node>, Vec<Edge>)> {
    let file: SeedFile = serde_json::from_str(SEED_GRAPH_JSON)?;
    let nodes: Vec<Node> = file
        .nodes
        .into_iter()
        .map(|n| Node {
            id: n.id,
            kind: n.kind,
            name: n.name,
            code: n.code,
            meta: serde_json::json!({"seed": true}),
        })
        .collect();

    let mut edges = Vec::with_capacity(file.edges.len());
    for SeedEdge(src, dst, relation, weight, confidence, source_key) in file.edges {
        let source = file.sources.get(&source_key).ok_or_else(|| {
            Error::Invalid(format!(
                "seed edge {src} -> {dst} uses unknown source {source_key}"
            ))
        })?;
        edges.push(Edge {
            id: None,
            src,
            dst,
            relation,
            weight,
            source_name: source.name.clone(),
            source_url: source.url.clone(),
            confidence,
            // Seed relations describe the current industry structure.
            valid_from: now_secs(),
            valid_to: None,
        });
    }
    Ok((nodes, edges))
}

/// Validate structural invariants of the seed graph; returns a list of
/// human-readable violations (empty = valid).
pub fn validate_seed(nodes: &[Node], edges: &[Edge]) -> Vec<String> {
    let mut problems = Vec::new();
    let ids: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    for node in nodes {
        if node.kind == NodeKind::Company {
            match &node.code {
                Some(code) if code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()) => {}
                _ => problems.push(format!("company {} has no valid 6-digit code", node.id)),
            }
        }
    }
    for edge in edges {
        if !ids.contains(edge.src.as_str()) {
            problems.push(format!("edge src {} not in nodes", edge.src));
        }
        if !ids.contains(edge.dst.as_str()) {
            problems.push(format!("edge dst {} not in nodes", edge.dst));
        }
        if !(SEED_MIN_CONFIDENCE..=1.0).contains(&edge.confidence) {
            problems.push(format!(
                "edge {} -> {} confidence {} outside {SEED_MIN_CONFIDENCE}..=1",
                edge.src, edge.dst, edge.confidence
            ));
        }
        if !(0.0..=1.0).contains(&edge.weight) {
            problems.push(format!(
                "edge {} -> {} weight {} outside 0..=1",
                edge.src, edge.dst, edge.weight
            ));
        }
        if edge.source_name.trim().is_empty() || edge.source_url.trim().is_empty() {
            problems.push(format!(
                "edge {} -> {} missing provenance",
                edge.src, edge.dst
            ));
        }
    }
    problems
}

/// Load the seed graph into storage when (and only when) the graph is
/// empty. Idempotent: a populated graph is left untouched, and even a
/// partial previous load converges because all writes are upserts.
pub async fn seed_if_empty(store: &GraphStore) -> Result<SeedSummary> {
    if !store.storage().graph_nodes_all().await?.is_empty() {
        return Ok(SeedSummary {
            nodes: 0,
            edges: 0,
            skipped: true,
        });
    }
    let (nodes, edges) = parse_seed()?;
    let problems = validate_seed(&nodes, &edges);
    if !problems.is_empty() {
        return Err(Error::Invalid(format!(
            "seed graph invalid: {}",
            problems.join("; ")
        )));
    }
    for node in &nodes {
        store.upsert_node(node).await?;
    }
    for edge in &edges {
        store.upsert_edge(edge).await?;
    }
    tracing::info!(
        nodes = nodes.len(),
        edges = edges.len(),
        "seed graph loaded"
    );
    Ok(SeedSummary {
        nodes: nodes.len(),
        edges: edges.len(),
        skipped: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_storage::{Storage, StorageConfig};

    #[test]
    fn seed_json_is_valid_and_sourced() {
        let (nodes, edges) = parse_seed().unwrap();
        // Every edge endpoint exists, confidence in range, provenance
        // non-empty — this enforces "no fabricated relations".
        assert_eq!(validate_seed(&nodes, &edges), Vec::<String>::new());

        let companies = nodes.iter().filter(|n| n.kind == NodeKind::Company).count();
        assert!(companies >= 60, "only {companies} company nodes");
        assert!(edges.len() >= 120, "only {} edges", edges.len());

        // Node ids are unique.
        let mut ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "duplicate node ids");

        // The six flagship chains are all present.
        for industry in [
            "industry:lithium_battery",
            "industry:semiconductor",
            "industry:photovoltaic",
            "industry:liquor",
            "industry:pig_chain",
            "industry:nonferrous",
        ] {
            assert!(
                nodes.iter().any(|n| n.id == industry),
                "missing industry node {industry}"
            );
        }
    }

    #[tokio::test]
    async fn seed_if_empty_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let store = GraphStore::new(storage);

        let first = seed_if_empty(&store).await.unwrap();
        assert!(!first.skipped);
        assert!(first.nodes >= 60);
        assert!(first.edges >= 120);

        let second = seed_if_empty(&store).await.unwrap();
        assert!(second.skipped);
        assert_eq!(second.nodes, 0);

        // Row counts match the first load exactly (no duplicates).
        assert_eq!(store.all_nodes().await.unwrap().len(), first.nodes);
        assert_eq!(store.all_edges().await.unwrap().len(), first.edges);
    }
}
