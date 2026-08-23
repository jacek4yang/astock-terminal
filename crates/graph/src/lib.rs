//! Supply-chain knowledge graph + event-propagation engine for the A-share
//! terminal.
//!
//! - [`model`]: typed nodes/edges/events with documented confidence
//!   semantics (source-backed relations only);
//! - [`store`]: [`GraphStore`] over `astock-storage` (migration v3 tables),
//!   with BFS traversal (`paths_from`, `subgraph`);
//! - [`seed`]: built-in seed graph (six industry chains, 79 listed
//!   companies) loaded idempotently via [`seed_if_empty`];
//! - [`enrich`]: live industry classification from EastMoney (`f100`);
//! - [`engine`]: event propagation — maps an event (commodity price change,
//!   policy, accident) to impacted listed companies with full logic chains,
//!   hop levels, heuristic lags, and decaying confidence;
//! - [`analysis`]: degree centrality, PageRank, connected components, and
//!   label-propagation communities for finding 系统重要性节点.
//!
//! Every numeric output of the propagation engine (magnitude, lag,
//! confidence) is a documented heuristic — see the module docs.

pub mod analysis;
pub mod bitemporal;
pub mod engine;
pub mod enrich;
pub mod error;
pub mod model;
pub mod seed;
pub mod store;

pub use bitemporal::{
    EdgeRevision, EdgeRevisionInput, EntityMergeRevision, EvidenceSourceType, GraphHistoryBounds,
    GraphSnapshot, GraphSnapshotDiff, RelationStatus, SnapshotEdge,
};
pub use engine::{Engine, ImpactEntry, ImpactReport, DEFAULT_MAX_HOPS, HOP_CONFIDENCE_DECAY};
pub use enrich::{apply_industry_map, enrich_from_eastmoney, EnrichSummary};
pub use error::{Error, Result};
pub use model::{Edge, Event, ImpactDirection, Node, NodeKind, Relation};
pub use seed::{seed_if_empty, SeedSummary, SEED_GRAPH_JSON};
pub use store::{GraphStore, PathChain, Subgraph};
