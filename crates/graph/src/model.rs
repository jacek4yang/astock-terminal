//! Typed graph model: nodes, edges, events, and the relation/direction
//! vocabulary shared by the store, the seeder, and the propagation engine.
//!
//! # Confidence semantics
//!
//! Every edge carries `confidence` in `0.0..=1.0`. Only source-backed
//! relations may be inserted (a non-empty `source_name` is enforced by
//! [`crate::store::GraphStore`]); the value expresses how solid that backing
//! is:
//!
//! - `0.85..=1.0` — audited filing (annual report / prospectus disclosures);
//! - `0.70..=0.85` — public industry-chain material (association data,
//!   reputable research summaries);
//! - `0.60..=0.70` — analyst inference from public data, needs review.
//!
//! Confidence below `0.6` is not accepted for seed data. During propagation
//! the engine multiplies edge confidences along the path and applies an
//! additional `0.8` decay per hop, so deeper impacts are always reported
//! with strictly lower confidence — no fake precision.

use serde::{Deserialize, Serialize};

/// Node kinds allowed in the supply-chain graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// A listed company (`code` holds the 6-digit ticker).
    Company,
    /// A tradable product (e.g. 动力电池, 组件).
    Product,
    /// A business segment (e.g. 半导体设备).
    Segment,
    /// A processed material (e.g. 碳酸锂, 光伏玻璃).
    Material,
    /// A raw commodity (e.g. 铜, 高粱).
    Commodity,
    /// An industry classification (e.g. 锂电池, 光伏).
    Industry,
    /// A region (reserved for geo-tagged events).
    Region,
    /// A policy node (reserved for policy events).
    Policy,
}

impl NodeKind {
    /// Stable string form stored in SQLite.
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::Company => "company",
            NodeKind::Product => "product",
            NodeKind::Segment => "segment",
            NodeKind::Material => "material",
            NodeKind::Commodity => "commodity",
            NodeKind::Industry => "industry",
            NodeKind::Region => "region",
            NodeKind::Policy => "policy",
        }
    }

    /// Parse the stored string form.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "company" => NodeKind::Company,
            "product" => NodeKind::Product,
            "segment" => NodeKind::Segment,
            "material" => NodeKind::Material,
            "commodity" => NodeKind::Commodity,
            "industry" => NodeKind::Industry,
            "region" => NodeKind::Region,
            "policy" => NodeKind::Policy,
            _ => return None,
        })
    }
}

/// Edge relations allowed in the supply-chain graph.
///
/// Direction convention: an edge reads `src --relation--> dst`, e.g.
/// `(赣锋锂业, produces, 碳酸锂)`, `(当升科技, consumes, 碳酸锂)`,
/// `(宁德时代, supplies, 长安汽车)` = "宁德时代 supplies 长安汽车",
/// `(长安汽车, customer_of, 宁德时代)` = "长安汽车 is a customer of 宁德时代",
/// `(铝, substitutes, 铜)` = "铝 is a substitute for 铜".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// src supplies dst (company → company/product trade).
    Supplies,
    /// src is a customer of dst.
    CustomerOf,
    /// src competes with dst (treated as symmetric by the engine).
    Competes,
    /// src is a substitute for dst.
    Substitutes,
    /// src has a generic risk exposure to dst.
    ExposedTo,
    /// src belongs to dst (company → industry/segment).
    BelongsTo,
    /// src produces dst (company → product/material/commodity).
    Produces,
    /// src consumes dst (company → product/material/commodity).
    Consumes,
}

impl Relation {
    /// Stable string form stored in SQLite.
    pub fn as_str(self) -> &'static str {
        match self {
            Relation::Supplies => "supplies",
            Relation::CustomerOf => "customer_of",
            Relation::Competes => "competes",
            Relation::Substitutes => "substitutes",
            Relation::ExposedTo => "exposed_to",
            Relation::BelongsTo => "belongs_to",
            Relation::Produces => "produces",
            Relation::Consumes => "consumes",
        }
    }

    /// Parse the stored string form.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "supplies" => Relation::Supplies,
            "customer_of" => Relation::CustomerOf,
            "competes" => Relation::Competes,
            "substitutes" => Relation::Substitutes,
            "exposed_to" => Relation::ExposedTo,
            "belongs_to" => Relation::BelongsTo,
            "produces" => Relation::Produces,
            "consumes" => Relation::Consumes,
            _ => return None,
        })
    }

    /// Heuristic impact-transmission lag in days for this relation.
    ///
    /// These are rough order-of-magnitude guesses, documented and returned
    /// as `expected_lag_days` by the propagation engine (summed along the
    /// path). They are NOT measured values:
    /// - `produces`: 5d — producers reprice output quickly;
    /// - `consumes`: 30d — inventory and contract buffers delay cost hits;
    /// - `supplies`/`customer_of`: 45d — order/delivery cycle;
    /// - `competes`: 3d — mostly sentiment-driven repricing;
    /// - `substitutes`: 20d — switching needs qualification;
    /// - `belongs_to`: 2d — sector sentiment is near-immediate;
    /// - `exposed_to`: 10d — generic exposure.
    pub fn lag_days(self) -> u32 {
        match self {
            Relation::Produces => 5,
            Relation::Consumes => 30,
            Relation::Supplies | Relation::CustomerOf => 45,
            Relation::Competes => 3,
            Relation::Substitutes => 20,
            Relation::BelongsTo => 2,
            Relation::ExposedTo => 10,
        }
    }
}

/// Impact direction on an affected company.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactDirection {
    /// 受益 — the event is expected to help the company.
    Benefit,
    /// 受损 — the event is expected to hurt the company.
    Harm,
}

impl ImpactDirection {
    /// Chinese label used in logic chains and reports.
    pub fn label(self) -> &'static str {
        match self {
            ImpactDirection::Benefit => "受益",
            ImpactDirection::Harm => "受损",
        }
    }

    /// Build from a sign: positive → Benefit, negative → Harm.
    pub fn from_sign(sign: i8) -> Self {
        if sign >= 0 {
            ImpactDirection::Benefit
        } else {
            ImpactDirection::Harm
        }
    }
}

/// A graph node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Node id, e.g. `company:600362` or `commodity:copper`.
    pub id: String,
    /// Node kind.
    pub kind: NodeKind,
    /// Display name (Chinese).
    pub name: String,
    /// 6-digit ticker for [`NodeKind::Company`], else None.
    #[serde(default)]
    pub code: Option<String>,
    /// Free-form metadata.
    #[serde(default)]
    pub meta: serde_json::Value,
}

/// A provenance-tracked graph edge. See module docs for confidence semantics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    /// Database row id; None before persistence.
    #[serde(default)]
    pub id: Option<i64>,
    /// Source node id.
    pub src: String,
    /// Destination node id.
    pub dst: String,
    /// Relation type.
    pub relation: Relation,
    /// Relation strength 0..=1 (heuristic, e.g. revenue share of the link).
    pub weight: f64,
    /// Provenance: human-readable source name (required, non-empty).
    pub source_name: String,
    /// Provenance: public URL backing the relation.
    pub source_url: String,
    /// Confidence 0..=1 (source-backed only).
    pub confidence: f64,
    /// Valid-from, unix seconds (0 = unknown/always).
    #[serde(default)]
    pub valid_from: i64,
    /// Valid-to, unix seconds; None = still valid.
    #[serde(default)]
    pub valid_to: Option<i64>,
}

/// A market event whose impact can be propagated through the graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Event id (caller-provided).
    pub id: String,
    /// Event kind: `commodity_price` | `policy` | `accident` | ...
    pub kind: String,
    /// Human-readable title, e.g. "铜价上涨10%".
    pub title: String,
    /// Subject: a graph node id, company code, or node name.
    pub subject: String,
    /// Magnitude as a fraction (e.g. 0.10 = +10%); None when unquantified.
    #[serde(default)]
    pub magnitude: Option<f64>,
    /// Direction: +1 up/positive, -1 down/negative.
    pub direction: i8,
    /// When the event occurred, unix seconds.
    pub occurred_at: i64,
    /// Provenance: source name.
    #[serde(default)]
    pub source_name: String,
    /// Provenance: source URL.
    #[serde(default)]
    pub source_url: String,
    /// Lifecycle status: `new` | `processed` | `archived`.
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "new".to_string()
}

impl Event {
    /// Construct a minimal event with `status = "new"` and no provenance.
    pub fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        title: impl Into<String>,
        subject: impl Into<String>,
        magnitude: Option<f64>,
        direction: i8,
        occurred_at: i64,
    ) -> Self {
        Event {
            id: id.into(),
            kind: kind.into(),
            title: title.into(),
            subject: subject.into(),
            magnitude,
            direction,
            occurred_at,
            source_name: String::new(),
            source_url: String::new(),
            status: default_status(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_and_relation_roundtrip() {
        for kind in [
            NodeKind::Company,
            NodeKind::Product,
            NodeKind::Segment,
            NodeKind::Material,
            NodeKind::Commodity,
            NodeKind::Industry,
            NodeKind::Region,
            NodeKind::Policy,
        ] {
            assert_eq!(NodeKind::parse(kind.as_str()), Some(kind));
        }
        for rel in [
            Relation::Supplies,
            Relation::CustomerOf,
            Relation::Competes,
            Relation::Substitutes,
            Relation::ExposedTo,
            Relation::BelongsTo,
            Relation::Produces,
            Relation::Consumes,
        ] {
            assert_eq!(Relation::parse(rel.as_str()), Some(rel));
        }
        assert!(NodeKind::parse("nope").is_none());
        assert!(Relation::parse("nope").is_none());
    }

    #[test]
    fn serde_uses_snake_case() {
        let node = Node {
            id: "company:600519".into(),
            kind: NodeKind::Company,
            name: "贵州茅台".into(),
            code: Some("600519".into()),
            meta: serde_json::json!({}),
        };
        let json = serde_json::to_string(&node).unwrap();
        assert!(json.contains("\"kind\":\"company\""));
        let back: Node = serde_json::from_str(&json).unwrap();
        assert_eq!(back, node);

        let edge: Edge = serde_json::from_str(
            r#"{"src":"a","dst":"b","relation":"customer_of","weight":0.5,
                "source_name":"s","source_url":"u","confidence":0.7}"#,
        )
        .unwrap();
        assert_eq!(edge.relation, Relation::CustomerOf);
        assert_eq!(edge.id, None);
        assert_eq!(edge.valid_to, None);
    }

    #[test]
    fn impact_direction_labels() {
        assert_eq!(ImpactDirection::from_sign(1).label(), "受益");
        assert_eq!(ImpactDirection::from_sign(-1).label(), "受损");
    }
}
