//! Graph analytics: degree centrality, hand-rolled PageRank (power
//! iteration), and community detection (connected components + simple
//! label propagation). Used to find 系统重要性节点 — companies/products
//! whose disruption propagates widely.
//!
//! All functions are pure and operate on plain node/edge slices, so they
//! are unit-testable without storage.

use std::collections::HashMap;

use crate::model::{Edge, Node};

/// Degree summary for one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Degree {
    /// Node id.
    pub node_id: String,
    /// Edges where the node is `dst`.
    pub in_degree: usize,
    /// Edges where the node is `src`.
    pub out_degree: usize,
}

impl Degree {
    /// Total degree (in + out).
    pub fn total(&self) -> usize {
        self.in_degree + self.out_degree
    }
}

/// In/out degree for every node, sorted by total degree descending
/// (ties broken by node id for determinism).
pub fn degree_centrality(nodes: &[Node], edges: &[Edge]) -> Vec<Degree> {
    let mut map: HashMap<&str, Degree> = nodes
        .iter()
        .map(|n| {
            (
                n.id.as_str(),
                Degree {
                    node_id: n.id.clone(),
                    in_degree: 0,
                    out_degree: 0,
                },
            )
        })
        .collect();
    for edge in edges {
        if let Some(d) = map.get_mut(edge.src.as_str()) {
            d.out_degree += 1;
        }
        if let Some(d) = map.get_mut(edge.dst.as_str()) {
            d.in_degree += 1;
        }
    }
    let mut out: Vec<Degree> = map.into_values().collect();
    out.sort_by(|a, b| {
        b.total()
            .cmp(&a.total())
            .then_with(|| a.node_id.cmp(&b.node_id))
    });
    out
}

/// PageRank via power iteration on the directed graph (src → dst).
///
/// `rank[i] = (1 - damping) / N + damping * Σ rank[j] / out_degree[j]`
/// over incoming edges; dangling nodes (no outgoing edges) distribute
/// their rank uniformly. Iterates until the L1 change drops below
/// `tolerance` or `max_iterations` is hit. Returns rank per node id;
/// ranks sum to ~1.
pub fn pagerank(
    nodes: &[Node],
    edges: &[Edge],
    damping: f64,
    tolerance: f64,
    max_iterations: usize,
) -> HashMap<String, f64> {
    let n = nodes.len();
    if n == 0 {
        return HashMap::new();
    }
    let index: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node.id.as_str(), i))
        .collect();
    let mut out_degree = vec![0usize; n];
    // incoming[i] = list of source indices pointing at i.
    let mut incoming: Vec<Vec<usize>> = vec![Vec::new(); n];
    for edge in edges {
        let (Some(&s), Some(&d)) = (index.get(edge.src.as_str()), index.get(edge.dst.as_str()))
        else {
            continue;
        };
        out_degree[s] += 1;
        incoming[d].push(s);
    }

    let base = (1.0 - damping) / n as f64;
    let mut rank = vec![1.0 / n as f64; n];
    for _ in 0..max_iterations {
        let dangling: f64 = rank
            .iter()
            .enumerate()
            .filter(|(i, _)| out_degree[*i] == 0)
            .map(|(_, r)| r)
            .sum();
        let mut next = vec![base + damping * dangling / n as f64; n];
        let mut delta = 0.0;
        for (i, sources) in incoming.iter().enumerate() {
            let mut sum = 0.0;
            for &s in sources {
                sum += rank[s] / out_degree[s] as f64;
            }
            next[i] += damping * sum;
        }
        for i in 0..n {
            delta += (next[i] - rank[i]).abs();
        }
        rank = next;
        if delta < tolerance {
            break;
        }
    }
    nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node.id.clone(), rank[i]))
        .collect()
}

/// Connected components treating edges as undirected (union-find).
/// Components are sorted by size descending; members sorted by id.
pub fn connected_components(nodes: &[Node], edges: &[Edge]) -> Vec<Vec<String>> {
    let index: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node.id.as_str(), i))
        .collect();
    let mut parent: Vec<usize> = (0..nodes.len()).collect();

    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }

    for edge in edges {
        let (Some(&a), Some(&b)) = (index.get(edge.src.as_str()), index.get(edge.dst.as_str()))
        else {
            continue;
        };
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent[ra] = rb;
        }
    }

    let mut groups: HashMap<usize, Vec<String>> = HashMap::new();
    for (i, node) in nodes.iter().enumerate() {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(node.id.clone());
    }
    let mut components: Vec<Vec<String>> = groups.into_values().collect();
    for component in &mut components {
        component.sort();
    }
    components.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    components
}

/// Community detection via simple label propagation on the undirected
/// projection.
///
/// Every node starts with its own id as label. Each round, every node
/// (processed in sorted-id order, updated in place) adopts the most
/// frequent label among its neighbors; ties break to the lexicographically
/// smallest label, making the result deterministic. Stops when a full
/// round changes nothing or `max_rounds` is hit. Returns node id →
/// community label (the label is the id of some member of the community).
pub fn label_propagation(
    nodes: &[Node],
    edges: &[Edge],
    max_rounds: usize,
) -> HashMap<String, String> {
    let mut neighbors: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in edges {
        neighbors
            .entry(edge.src.as_str())
            .or_default()
            .push(edge.dst.as_str());
        neighbors
            .entry(edge.dst.as_str())
            .or_default()
            .push(edge.src.as_str());
    }
    let mut labels: HashMap<&str, &str> =
        nodes.iter().map(|n| (n.id.as_str(), n.id.as_str())).collect();
    let mut order: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    order.sort_unstable();

    for _ in 0..max_rounds {
        let mut changed = false;
        for id in &order {
            let Some(neigh) = neighbors.get(id) else {
                continue;
            };
            if neigh.is_empty() {
                continue;
            }
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for n in neigh {
                if let Some(&label) = labels.get(n) {
                    *counts.entry(label).or_default() += 1;
                }
            }
            // Most frequent label; ties → smallest label (deterministic).
            let best = counts
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)));
            if let Some((label, _)) = best {
                if labels[id] != label {
                    labels.insert(id, label);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    labels
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// 系统重要性节点: top-`limit` nodes by PageRank, with rank scores,
/// descending.
pub fn system_importance(nodes: &[Node], edges: &[Edge], limit: usize) -> Vec<(String, f64)> {
    let ranks = pagerank(nodes, edges, 0.85, 1e-9, 200);
    let mut out: Vec<(String, f64)> = ranks.into_iter().collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Relation;

    fn node(id: &str) -> Node {
        Node {
            id: id.into(),
            kind: crate::model::NodeKind::Company,
            name: id.into(),
            code: None,
            meta: serde_json::json!({}),
        }
    }

    fn edge(src: &str, dst: &str) -> Edge {
        Edge {
            id: None,
            src: src.into(),
            dst: dst.into(),
            relation: Relation::Supplies,
            weight: 1.0,
            source_name: "test".into(),
            source_url: "https://example.com".into(),
            confidence: 1.0,
            valid_from: 0,
            valid_to: None,
        }
    }

    #[test]
    fn degree_counts_in_and_out() {
        let nodes = vec![node("a"), node("b"), node("c")];
        let edges = vec![edge("a", "b"), edge("a", "c")];
        let degrees = degree_centrality(&nodes, &edges);
        assert_eq!(degrees[0].node_id, "a");
        assert_eq!(degrees[0].out_degree, 2);
        assert_eq!(degrees[0].in_degree, 0);
        assert_eq!(degrees[1].in_degree, 1);
    }

    #[test]
    fn pagerank_three_node_cycle_is_uniform() {
        let nodes = vec![node("a"), node("b"), node("c")];
        let edges = vec![edge("a", "b"), edge("b", "c"), edge("c", "a")];
        let ranks = pagerank(&nodes, &edges, 0.85, 1e-12, 1000);
        for id in ["a", "b", "c"] {
            assert!((ranks[id] - 1.0 / 3.0).abs() < 1e-6, "{}: {}", id, ranks[id]);
        }
    }

    #[test]
    fn pagerank_hand_computed_asymmetric() {
        // Edges: a→b, b→a, b→c, c→b. By symmetry rank(a) = rank(c) = x,
        // rank(b) = y, 2x + y = 1. With d = 0.85, N = 3:
        //   y = 0.15/3 + 0.85 * (x/1 + x/1)  →  1 - 2x = 0.05 + 1.7x
        //   → x = 0.95 / 3.7 ≈ 0.256757, y ≈ 0.486486.
        let nodes = vec![node("a"), node("b"), node("c")];
        let edges = vec![edge("a", "b"), edge("b", "a"), edge("b", "c"), edge("c", "b")];
        let ranks = pagerank(&nodes, &edges, 0.85, 1e-12, 1000);
        let x = 0.95 / 3.7;
        let y = 1.0 - 2.0 * x;
        assert!((ranks["a"] - x).abs() < 1e-6, "a: {}", ranks["a"]);
        assert!((ranks["c"] - x).abs() < 1e-6, "c: {}", ranks["c"]);
        assert!((ranks["b"] - y).abs() < 1e-6, "b: {}", ranks["b"]);
        let sum: f64 = ranks.values().sum();
        assert!((sum - 1.0).abs() < 1e-6);
    }

    #[test]
    fn pagerank_dangling_nodes_redistribute() {
        // a→b, b→c, c has no outgoing edge (dangling).
        let nodes = vec![node("a"), node("b"), node("c")];
        let edges = vec![edge("a", "b"), edge("b", "c")];
        let ranks = pagerank(&nodes, &edges, 0.85, 1e-12, 1000);
        let sum: f64 = ranks.values().sum();
        assert!((sum - 1.0).abs() < 1e-6, "sum {sum}");
        // c accumulates the most rank (sink receiving from b plus its own
        // dangling redistribution).
        assert!(ranks["c"] > ranks["a"]);
    }

    #[test]
    fn connected_components_two_triangles() {
        let nodes: Vec<Node> = ["a", "b", "c", "x", "y", "z"].into_iter().map(node).collect();
        let edges = vec![
            edge("a", "b"),
            edge("b", "c"),
            edge("c", "a"),
            edge("x", "y"),
            edge("y", "z"),
        ];
        let components = connected_components(&nodes, &edges);
        assert_eq!(components.len(), 2);
        assert_eq!(components[0].len(), 3);
    }

    #[test]
    fn label_propagation_finds_two_communities() {
        // Two disjoint triangles: each must converge to a single shared
        // label, and the two communities keep distinct labels.
        let nodes: Vec<Node> = ["a", "b", "c", "x", "y", "z"].into_iter().map(node).collect();
        let edges = vec![
            edge("a", "b"),
            edge("b", "c"),
            edge("c", "a"),
            edge("x", "y"),
            edge("y", "z"),
            edge("z", "x"),
        ];
        let labels = label_propagation(&nodes, &edges, 50);
        assert_eq!(labels["a"], labels["b"]);
        assert_eq!(labels["b"], labels["c"]);
        assert_eq!(labels["x"], labels["y"]);
        assert_eq!(labels["y"], labels["z"]);
        assert_ne!(labels["a"], labels["x"]);
    }

    #[test]
    fn system_importance_ranks_hubs_first() {
        let nodes: Vec<Node> = ["hub", "a", "b", "c"].into_iter().map(node).collect();
        let edges = vec![edge("a", "hub"), edge("b", "hub"), edge("c", "hub")];
        let top = system_importance(&nodes, &edges, 1);
        assert_eq!(top[0].0, "hub");
    }
}
