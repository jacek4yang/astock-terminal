//! `GraphStore`: typed access to the supply-chain graph on top of
//! [`astock_storage::Storage`], plus BFS traversal helpers.

use std::collections::{HashMap, HashSet, VecDeque};

use astock_storage::{EventRow, GraphEdgeRow, GraphNodeRow, Storage};

use crate::error::{Error, Result};
use crate::model::{Edge, Event, Node, NodeKind, Relation};

/// Current unix time in seconds (workspace chrono has no `clock` feature).
pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A full path chain discovered by [`GraphStore::paths_from`]: `nodes[0]`
/// is the start node, `edges[i]` connects `nodes[i]` to `nodes[i+1]`.
#[derive(Debug, Clone, PartialEq)]
pub struct PathChain {
    /// Nodes along the path, starting with the origin.
    pub nodes: Vec<Node>,
    /// Edges along the path (`edges.len() == nodes.len() - 1`).
    pub edges: Vec<Edge>,
}

/// A hop-limited neighborhood extracted by [`GraphStore::subgraph`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Subgraph {
    /// Nodes within `hops` of the center (including the center).
    pub nodes: Vec<Node>,
    /// Edges with both endpoints inside the node set.
    pub edges: Vec<Edge>,
}

/// Typed wrapper over [`Storage`] for the graph tables (migration v3).
///
/// All invariants of [`crate::model`] are enforced here: confidence and
/// weight must be in `0.0..=1.0`, every edge needs a non-empty provenance
/// (`source_name`), and both endpoints must exist.
#[derive(Clone)]
pub struct GraphStore {
    storage: Storage,
}

impl GraphStore {
    /// Wrap an existing storage handle.
    pub fn new(storage: Storage) -> Self {
        GraphStore { storage }
    }

    /// The underlying storage handle.
    pub fn storage(&self) -> &Storage {
        &self.storage
    }

    // ------------------------------------------------------------------
    // typed upserts / lookups
    // ------------------------------------------------------------------

    /// Insert or update a node (kind/name/code are refreshed on conflict).
    pub async fn upsert_node(&self, node: &Node) -> Result<()> {
        if node.kind == NodeKind::Company {
            match &node.code {
                Some(code) if code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()) => {}
                _ => {
                    return Err(Error::Invalid(format!(
                        "company node {} needs a 6-digit code",
                        node.id
                    )))
                }
            }
        }
        let now = now_secs();
        self.storage
            .graph_node_upsert(GraphNodeRow {
                id: node.id.clone(),
                kind: node.kind.as_str().to_string(),
                name: node.name.clone(),
                code: node.code.clone(),
                meta_json: serde_json::to_string(&node.meta)?,
                created_at: now,
                updated_at: now,
            })
            .await?;
        Ok(())
    }

    /// Insert or update an edge after validating weight/confidence ranges,
    /// provenance, and endpoint existence.
    pub async fn upsert_edge(&self, edge: &Edge) -> Result<()> {
        if !(0.0..=1.0).contains(&edge.weight) {
            return Err(Error::Invalid(format!(
                "edge weight {} out of range 0..=1 ({} -> {})",
                edge.weight, edge.src, edge.dst
            )));
        }
        if !(0.0..=1.0).contains(&edge.confidence) {
            return Err(Error::Invalid(format!(
                "edge confidence {} out of range 0..=1 ({} -> {})",
                edge.confidence, edge.src, edge.dst
            )));
        }
        if edge.source_name.trim().is_empty() {
            return Err(Error::Invalid(format!(
                "edge {} -> {} has no provenance (source_name)",
                edge.src, edge.dst
            )));
        }
        for endpoint in [&edge.src, &edge.dst] {
            if self.storage.graph_node_get(endpoint).await?.is_none() {
                return Err(Error::NotFound(endpoint.clone()));
            }
        }
        let now = now_secs();
        self.storage
            .graph_edge_upsert(GraphEdgeRow {
                id: edge.id,
                src: edge.src.clone(),
                dst: edge.dst.clone(),
                relation: edge.relation.as_str().to_string(),
                weight: edge.weight,
                source_name: edge.source_name.clone(),
                source_url: edge.source_url.clone(),
                confidence: edge.confidence,
                valid_from: edge.valid_from,
                valid_to: edge.valid_to,
                created_at: now,
                updated_at: now,
            })
            .await?;
        Ok(())
    }

    /// Persist an event row (id conflict keeps the original).
    pub async fn insert_event(&self, event: &Event) -> Result<()> {
        self.storage
            .event_insert(EventRow {
                id: event.id.clone(),
                kind: event.kind.clone(),
                title: event.title.clone(),
                subject: event.subject.clone(),
                magnitude: event.magnitude,
                direction: event.direction as i64,
                occurred_at: event.occurred_at,
                source_name: event.source_name.clone(),
                source_url: event.source_url.clone(),
                status: event.status.clone(),
                created_at: now_secs(),
            })
            .await?;
        Ok(())
    }

    /// Fetch a node by id.
    pub async fn node(&self, id: &str) -> Result<Option<Node>> {
        Ok(self.storage.graph_node_get(id).await?.map(node_from))
    }

    /// Resolve a free-form subject (node id, company code, or exact name)
    /// to a node. Id match wins, then code, then name.
    pub async fn find_node(&self, query: &str) -> Result<Option<Node>> {
        let query = query.trim();
        if let Some(node) = self.node(query).await? {
            return Ok(Some(node));
        }
        let nodes = self.all_nodes().await?;
        Ok(nodes
            .into_iter()
            .find(|n| n.code.as_deref() == Some(query) || n.name == query))
    }

    /// All nodes, ordered by id.
    pub async fn all_nodes(&self) -> Result<Vec<Node>> {
        Ok(self
            .storage
            .graph_nodes_all()
            .await?
            .into_iter()
            .map(node_from)
            .collect())
    }

    /// All edges, ordered by row id.
    pub async fn all_edges(&self) -> Result<Vec<Edge>> {
        Ok(self
            .storage
            .graph_edges_all()
            .await?
            .into_iter()
            .map(edge_from)
            .collect())
    }

    /// Edges touching `id` in either direction, each paired with the node
    /// at the other end.
    pub async fn neighbors(&self, id: &str) -> Result<Vec<(Edge, Node)>> {
        Ok(self
            .storage
            .graph_neighbors(id)
            .await?
            .into_iter()
            .map(|(e, n)| (edge_from(e), node_from(n)))
            .collect())
    }

    // ------------------------------------------------------------------
    // traversal
    // ------------------------------------------------------------------

    /// Breadth-first traversal from `subject` (any resolvable node query).
    ///
    /// Returns one [`PathChain`] per discovered node (excluding the start),
    /// each holding the full node/edge chain back to the start. A node is
    /// visited at most once (cycle guard; BFS order keeps the shortest
    /// path). Traversal follows edges in both directions — directionality
    /// is interpreted by the engine, not here.
    pub async fn paths_from(&self, subject: &str, max_hops: u32) -> Result<Vec<PathChain>> {
        let start = self
            .find_node(subject)
            .await?
            .ok_or_else(|| Error::NotFound(subject.to_string()))?;
        let adjacency = self.adjacency().await?;

        let mut visited: HashSet<String> = HashSet::from([start.id.clone()]);
        let mut queue: VecDeque<PathChain> = VecDeque::from([PathChain {
            nodes: vec![start],
            edges: vec![],
        }]);
        let mut out = Vec::new();

        while let Some(chain) = queue.pop_front() {
            if chain.edges.len() >= max_hops as usize {
                continue;
            }
            let here = chain.nodes.last().unwrap().id.as_str();
            let Some(edges) = adjacency.get(here) else {
                continue;
            };
            for (edge, next_id) in edges {
                if !visited.insert(next_id.clone()) {
                    continue; // cycle guard
                }
                let next = adjacency_node(&adjacency, next_id);
                let mut next_chain = chain.clone();
                next_chain.edges.push(edge.clone());
                next_chain.nodes.push(next);
                queue.push_back(next_chain.clone());
                out.push(next_chain);
            }
        }
        Ok(out)
    }

    /// Extract the hop-limited neighborhood around `center` (any resolvable
    /// query). Edges with both endpoints inside the neighborhood are kept.
    pub async fn subgraph(&self, center: &str, hops: u32) -> Result<Subgraph> {
        let center_node = self
            .find_node(center)
            .await?
            .ok_or_else(|| Error::NotFound(center.to_string()))?;
        let adjacency = self.adjacency().await?;

        let mut depth: HashMap<String, u32> = HashMap::from([(center_node.id.clone(), 0)]);
        let mut queue = VecDeque::from([center_node.id.clone()]);
        while let Some(id) = queue.pop_front() {
            let d = depth[&id];
            if d >= hops {
                continue;
            }
            if let Some(edges) = adjacency.get(id.as_str()) {
                for (_, next_id) in edges {
                    if !depth.contains_key(next_id.as_str()) {
                        depth.insert(next_id.clone(), d + 1);
                        queue.push_back(next_id.clone());
                    }
                }
            }
        }

        let mut nodes: Vec<Node> = depth
            .keys()
            .map(|id| adjacency_node(&adjacency, id))
            .collect();
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        let mut edges: Vec<Edge> = self
            .all_edges()
            .await?
            .into_iter()
            .filter(|e| depth.contains_key(&e.src) && depth.contains_key(&e.dst))
            .collect();
        edges.sort_by_key(|e| e.id);
        Ok(Subgraph { nodes, edges })
    }

    // ------------------------------------------------------------------
    // internals
    // ------------------------------------------------------------------

    /// Build an in-memory adjacency map: node id → (edge, other end id),
    /// plus every node keyed by id for cheap lookup.
    pub(crate) async fn adjacency(&self) -> Result<Adjacency> {
        let nodes = self.all_nodes().await?;
        let edges = self.all_edges().await?;
        Ok(build_adjacency(nodes, edges))
    }
}

/// In-memory graph index shared by traversal, the engine, and analytics.
pub(crate) struct Adjacency {
    /// Node id → node.
    pub nodes: HashMap<String, Node>,
    /// Node id → (edge, other endpoint id), both directions, ordered by
    /// edge row id for deterministic traversal.
    pub links: HashMap<String, Vec<(Edge, String)>>,
}

impl std::ops::Deref for Adjacency {
    type Target = HashMap<String, Vec<(Edge, String)>>;
    fn deref(&self) -> &Self::Target {
        &self.links
    }
}

/// Build the adjacency index from raw node/edge lists (pure, testable).
pub(crate) fn build_adjacency(nodes: Vec<Node>, edges: Vec<Edge>) -> Adjacency {
    let node_map: HashMap<String, Node> = nodes.into_iter().map(|n| (n.id.clone(), n)).collect();
    let mut links: HashMap<String, Vec<(Edge, String)>> = HashMap::new();
    for edge in edges {
        if !node_map.contains_key(&edge.src) || !node_map.contains_key(&edge.dst) {
            continue; // skip dangling edges defensively
        }
        links
            .entry(edge.src.clone())
            .or_default()
            .push((edge.clone(), edge.dst.clone()));
        links
            .entry(edge.dst.clone())
            .or_default()
            .push((edge.clone(), edge.src.clone()));
    }
    Adjacency {
        nodes: node_map,
        links,
    }
}

fn adjacency_node(adj: &Adjacency, id: &str) -> Node {
    adj.nodes.get(id).cloned().unwrap_or_else(|| Node {
        id: id.to_string(),
        kind: NodeKind::Segment,
        name: id.to_string(),
        code: None,
        meta: serde_json::Value::Null,
    })
}

pub(crate) fn node_from(row: GraphNodeRow) -> Node {
    Node {
        id: row.id,
        kind: NodeKind::parse(&row.kind).unwrap_or(NodeKind::Segment),
        name: row.name,
        code: row.code,
        meta: serde_json::from_str(&row.meta_json).unwrap_or(serde_json::Value::Null),
    }
}

pub(crate) fn edge_from(row: GraphEdgeRow) -> Edge {
    Edge {
        id: row.id,
        src: row.src,
        dst: row.dst,
        relation: Relation::parse(&row.relation).unwrap_or(Relation::ExposedTo),
        weight: row.weight,
        source_name: row.source_name,
        source_url: row.source_url,
        confidence: row.confidence,
        valid_from: row.valid_from,
        valid_to: row.valid_to,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeKind;
    use astock_storage::StorageConfig;

    async fn test_store() -> (tempfile::TempDir, GraphStore) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        (dir, GraphStore::new(storage))
    }

    fn company(code: &str, name: &str) -> Node {
        Node {
            id: format!("company:{code}"),
            kind: NodeKind::Company,
            name: name.into(),
            code: Some(code.into()),
            meta: serde_json::json!({}),
        }
    }

    fn commodity(id: &str, name: &str) -> Node {
        Node {
            id: format!("commodity:{id}"),
            kind: NodeKind::Commodity,
            name: name.into(),
            code: None,
            meta: serde_json::json!({}),
        }
    }

    fn edge(src: &str, dst: &str, relation: Relation) -> Edge {
        Edge {
            id: None,
            src: src.into(),
            dst: dst.into(),
            relation,
            weight: 0.8,
            source_name: "公司年报2024".into(),
            source_url: "https://example.com".into(),
            confidence: 0.85,
            valid_from: 0,
            valid_to: None,
        }
    }

    #[tokio::test]
    async fn upsert_validates_invariants() {
        let (_dir, store) = test_store().await;
        store
            .upsert_node(&company("600362", "江西铜业"))
            .await
            .unwrap();
        store.upsert_node(&commodity("copper", "铜")).await.unwrap();

        // Bad confidence rejected.
        let mut bad = edge("company:600362", "commodity:copper", Relation::Produces);
        bad.confidence = 1.5;
        assert!(store.upsert_edge(&bad).await.is_err());
        // Missing provenance rejected.
        let mut no_source = edge("company:600362", "commodity:copper", Relation::Produces);
        no_source.source_name = String::new();
        assert!(store.upsert_edge(&no_source).await.is_err());
        // Unknown endpoint rejected.
        let dangling = edge("company:600362", "commodity:gold", Relation::Produces);
        assert!(store.upsert_edge(&dangling).await.is_err());
        // Company without a valid code rejected.
        let mut no_code = company("600519", "贵州茅台");
        no_code.code = None;
        assert!(store.upsert_node(&no_code).await.is_err());

        store
            .upsert_edge(&edge(
                "company:600362",
                "commodity:copper",
                Relation::Produces,
            ))
            .await
            .unwrap();
        assert_eq!(store.all_edges().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn find_node_by_id_code_or_name() {
        let (_dir, store) = test_store().await;
        store
            .upsert_node(&company("600362", "江西铜业"))
            .await
            .unwrap();
        for query in ["company:600362", "600362", "江西铜业"] {
            let found = store.find_node(query).await.unwrap().unwrap();
            assert_eq!(found.id, "company:600362");
        }
        assert!(store.find_node("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn paths_from_bfs_with_cycle_guard() {
        let (_dir, store) = test_store().await;
        // Triangle a-b-c-a plus tail c-d: the cycle must not loop forever
        // and each node must appear exactly once with its shortest chain.
        store.upsert_node(&commodity("a", "甲")).await.unwrap();
        store.upsert_node(&commodity("b", "乙")).await.unwrap();
        store.upsert_node(&commodity("c", "丙")).await.unwrap();
        store.upsert_node(&commodity("d", "丁")).await.unwrap();
        store
            .upsert_edge(&edge("commodity:a", "commodity:b", Relation::Substitutes))
            .await
            .unwrap();
        store
            .upsert_edge(&edge("commodity:b", "commodity:c", Relation::Substitutes))
            .await
            .unwrap();
        store
            .upsert_edge(&edge("commodity:c", "commodity:a", Relation::Substitutes))
            .await
            .unwrap();
        store
            .upsert_edge(&edge("commodity:c", "commodity:d", Relation::Substitutes))
            .await
            .unwrap();

        let paths = store.paths_from("commodity:a", 3).await.unwrap();
        assert_eq!(paths.len(), 3); // b, c, d — a is the start, not revisited
        let by_target: HashMap<&str, &PathChain> = paths
            .iter()
            .map(|p| (p.nodes.last().unwrap().id.as_str(), p))
            .collect();
        assert_eq!(by_target["commodity:b"].nodes.len(), 2);
        // c is a direct neighbor of a; BFS keeps the shortest chain.
        assert_eq!(by_target["commodity:c"].nodes.len(), 2);
        assert_eq!(by_target["commodity:d"].nodes.len(), 3);
        assert_eq!(by_target["commodity:d"].edges.len(), 2);

        // max_hops limits chain length.
        let shallow = store.paths_from("commodity:a", 1).await.unwrap();
        assert_eq!(shallow.len(), 2); // b and c (direct neighbors)
    }

    #[tokio::test]
    async fn subgraph_collects_hop_neighborhood() {
        let (_dir, store) = test_store().await;
        store.upsert_node(&commodity("a", "甲")).await.unwrap();
        store.upsert_node(&commodity("b", "乙")).await.unwrap();
        store.upsert_node(&commodity("c", "丙")).await.unwrap();
        store
            .upsert_edge(&edge("commodity:a", "commodity:b", Relation::Substitutes))
            .await
            .unwrap();
        store
            .upsert_edge(&edge("commodity:b", "commodity:c", Relation::Substitutes))
            .await
            .unwrap();

        let one = store.subgraph("commodity:a", 1).await.unwrap();
        assert_eq!(one.nodes.len(), 2);
        assert_eq!(one.edges.len(), 1);

        let two = store.subgraph("commodity:a", 2).await.unwrap();
        assert_eq!(two.nodes.len(), 3);
        assert_eq!(two.edges.len(), 2);
    }
}
