//! Event-propagation engine: maps an event (commodity price change, policy,
//! accident) to affected listed companies with full logic chains.
//!
//! # Propagation model (all heuristics — no fake precision)
//!
//! The engine runs a BFS from the event subject. Each visited node carries
//! one of two states:
//!
//! - `Price(d)` — the node's market price is expected to move in direction
//!   `d` (+1 up / -1 down). Applies to commodities/materials/products.
//! - `Prosperity(s)` — the company's business is expected to benefit
//!   (`s = +1`, 受益) or suffer (`s = -1`, 受损). Applies to companies.
//!
//! Transition rules (edge direction conventions in [`crate::model::Relation`]):
//!
//! From `Price(d)` at node N:
//! - `(C produces N)` → `Prosperity(d)` at company C — producers gain when
//!   their output price rises;
//! - `(C consumes N)` → `Prosperity(-d)` at company C — consumers face cost
//!   pressure (一阶成本影响);
//! - `(X substitutes N)` → `Price(d)` at X — demand shifts to substitutes
//!   (替代受益, one extra hop of indirection);
//! - `(C exposed_to N)` → `Prosperity(d)` at C — generic positive exposure.
//!
//! From `Prosperity` at company C (sign `s`, plus `pass` = the originating
//! price direction when C was reached through a price-driven edge):
//! - `(C produces P)` → `Price(pass)` at P when `pass` is set — input cost
//!   moves are pushed downstream (成本传导; consumers of P are impacted one
//!   hop later, 二阶成本传导); without `pass`, `s < 0` still implies
//!   `Price(+1)` at P (supply disruption);
//! - `(S supplies C)` / `(C customer_of S)` → same-sign impact at supplier
//!   S (demand expansion/contraction reaches suppliers);
//! - `s < 0` and `(C supplies X)` / `(X customer_of C)` → same-sign impact
//!   at customer X (disruption reaches customers);
//! - company-subject events only: `(C competes X)` → opposite-sign impact
//!   at X (one company's loss is its competitor's gain).
//!
//! From `Prosperity(s)` at an industry/segment node: members via
//! `belongs_to` inherit `Prosperity(s)`.
//!
//! # Hop counting, confidence, magnitude, lag
//!
//! - The hop counter increments when a **company** is impacted, plus once
//!   for each `substitutes` indirection; passing through intermediate
//!   product/material nodes is free.
//! - Confidence = product of edge confidences along the path × `0.8^hop`
//!   (documented per-hop decay).
//! - `magnitude_estimate` = |event magnitude| × product of edge weights —
//!   a 粗略估计 (rough estimate), always labeled as such.
//! - `expected_lag_days` = sum of per-relation heuristic lags along the
//!   path (see [`crate::model::Relation::lag_days`]).

use std::collections::{HashMap, VecDeque};

use crate::error::{Error, Result};
use crate::model::{Edge, Event, ImpactDirection, Node, NodeKind, Relation};
use crate::store::GraphStore;

/// Per-hop confidence decay factor (documented in module docs).
pub const HOP_CONFIDENCE_DECAY: f64 = 0.8;

/// Default maximum impact hops (hop >= 3 lands in the 潜在映射 bucket).
pub const DEFAULT_MAX_HOPS: u32 = 3;

/// State carried by a visited node during propagation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Price direction (+1/-1) for commodity/material/product nodes.
    Price(i8),
    /// Business impact sign (+1 受益 / -1 受损) for companies (and group
    /// nodes while expanding `belongs_to`). `pass` carries the originating
    /// price direction when the node was reached through a price-driven
    /// edge, so cost moves can be passed on to its products.
    Prosperity {
        /// +1 受益 / -1 受损.
        sign: i8,
        /// Originating price direction, if price-driven.
        pass: Option<i8>,
    },
}

/// One step of a logic chain: the edge traversed plus a human-readable
/// explanation of the transition.
#[derive(Debug, Clone)]
struct Step {
    /// Node reached by this step.
    node: Node,
    /// Edge traversed to reach it.
    edge: Edge,
    /// Transition explanation, e.g. "自产铜" / "采购铜，成本承压".
    note: String,
    /// Impact sign when the reached node is an impacted company.
    impact: Option<i8>,
}

/// A single impacted company in an [`ImpactReport`].
#[derive(Debug, Clone, PartialEq)]
pub struct ImpactEntry {
    /// Company node id.
    pub node_id: String,
    /// 6-digit ticker.
    pub code: String,
    /// Company name.
    pub name: String,
    /// 受益 / 受损.
    pub direction: ImpactDirection,
    /// Impact hop (1 = 一级, 2 = 二级, >=3 = 潜在映射).
    pub hop: u32,
    /// Human-readable full chain, e.g.
    /// "铜↑10% → 远东股份（采购铜，成本承压，受损）→ 电线电缆（成本传导提价）→ 格力电器（采购电线电缆，成本承压，受损）".
    pub logic_chain: String,
    /// Heuristic lag in days (sum of per-relation guesses; NOT measured).
    pub expected_lag_days: u32,
    /// Rough magnitude estimate (|event magnitude| × path edge weights).
    /// Labeled 粗略估计; None when the event has no quantified magnitude.
    pub magnitude_estimate: Option<f64>,
    /// Path confidence: product of edge confidences × 0.8^hop.
    pub confidence: f64,
    /// Provenance (source_name, source_url) of every edge along the path.
    pub provenance: Vec<(String, String)>,
}

/// The result of propagating one event through the graph.
#[derive(Debug, Clone, PartialEq)]
pub struct ImpactReport {
    /// Event title as provided.
    pub event_title: String,
    /// Resolved subject node.
    pub subject: Node,
    /// One-line human summary.
    pub summary: String,
    /// 一级受益 (hop 1, benefit), ranked by confidence.
    pub primary_benefit: Vec<ImpactEntry>,
    /// 一级受损 (hop 1, harm), ranked by confidence.
    pub primary_harm: Vec<ImpactEntry>,
    /// 二级受益 (hop 2, benefit).
    pub secondary_benefit: Vec<ImpactEntry>,
    /// 二级受损 (hop 2, harm).
    pub secondary_harm: Vec<ImpactEntry>,
    /// 潜在映射 (hop >= 3, both directions).
    pub potential: Vec<ImpactEntry>,
    /// Fixed disclaimer: every number here is a documented heuristic.
    pub disclaimer: String,
}

/// The propagation engine. Cheap to construct; holds a [`GraphStore`].
pub struct Engine {
    store: GraphStore,
    max_hops: u32,
}

impl Engine {
    /// Engine over `store` with the default hop limit.
    pub fn new(store: GraphStore) -> Self {
        Engine {
            store,
            max_hops: DEFAULT_MAX_HOPS,
        }
    }

    /// Override the hop limit (mostly for tests).
    pub fn with_max_hops(mut self, max_hops: u32) -> Self {
        self.max_hops = max_hops;
        self
    }

    /// Propagate `event` through the graph and rank the impacted companies.
    pub async fn propagate(&self, event: &Event) -> Result<ImpactReport> {
        let subject = self
            .store
            .find_node(&event.subject)
            .await?
            .ok_or_else(|| Error::NotFound(event.subject.clone()))?;
        if event.direction != 1 && event.direction != -1 {
            return Err(Error::Invalid(format!(
                "event direction must be +1 or -1, got {}",
                event.direction
            )));
        }
        let adjacency = self.store.adjacency().await?;

        // BFS frontier item.
        struct Frontier {
            node: Node,
            state: State,
            hop: u32,
            steps: Vec<Step>,
            /// Product of edge confidences along the path.
            confidence: f64,
            /// Product of edge weights along the path.
            weight: f64,
            /// Sum of per-relation lag heuristics.
            lag_days: u32,
        }

        let mut entries: Vec<ImpactEntry> = Vec::new();
        let mut visited: HashMap<String, u32> = HashMap::new();
        let mut queue: VecDeque<Frontier> = VecDeque::new();

        // Seed the frontier by subject kind.
        let company_subject = subject.kind == NodeKind::Company;
        let seed_state = match subject.kind {
            NodeKind::Commodity | NodeKind::Material | NodeKind::Product => {
                State::Price(event.direction)
            }
            NodeKind::Industry
            | NodeKind::Segment
            | NodeKind::Policy
            | NodeKind::Region
            | NodeKind::Company => State::Prosperity {
                sign: event.direction,
                pass: None,
            },
        };
        visited.insert(subject.id.clone(), 0);
        queue.push_back(Frontier {
            node: subject.clone(),
            state: seed_state,
            hop: 0,
            steps: vec![],
            confidence: 1.0,
            weight: 1.0,
            lag_days: 0,
        });

        while let Some(cur) = queue.pop_front() {
            let Some(links) = adjacency.get(cur.node.id.as_str()) else {
                continue;
            };
            for (edge, other_id) in links {
                let Some(next) = adjacency.nodes.get(other_id).cloned() else {
                    continue;
                };
                let Some((note, next_st)) =
                    transition(&cur.node, cur.state, edge, &next, company_subject)
                else {
                    continue;
                };
                // Hop increments when a company is impacted, plus one for
                // each substitutes indirection (documented in module docs).
                let next_hop = cur.hop
                    + u32::from(next.kind == NodeKind::Company)
                    + u32::from(edge.relation == Relation::Substitutes);
                if next_hop > self.max_hops {
                    continue;
                }
                // Cycle guard: first (shortest-hop) visit wins.
                if visited.contains_key(other_id) {
                    continue;
                }
                visited.insert(other_id.clone(), next_hop);

                let impact = match next_st {
                    State::Prosperity { sign, .. } if next.kind == NodeKind::Company => Some(sign),
                    _ => None,
                };
                let step = Step {
                    node: next.clone(),
                    edge: edge.clone(),
                    note,
                    impact,
                };
                let mut steps = cur.steps.clone();
                steps.push(step);
                let next_frontier = Frontier {
                    node: next.clone(),
                    state: next_st,
                    hop: next_hop,
                    confidence: cur.confidence * edge.confidence,
                    weight: cur.weight * edge.weight,
                    lag_days: cur.lag_days + edge.relation.lag_days(),
                    steps,
                };

                if next.kind == NodeKind::Company {
                    if let State::Prosperity { sign, .. } = next_frontier.state {
                        entries.push(ImpactEntry {
                            node_id: next.id.clone(),
                            code: next.code.clone().unwrap_or_default(),
                            name: next.name.clone(),
                            direction: ImpactDirection::from_sign(sign),
                            hop: next_hop,
                            logic_chain: render_chain(&subject, event, &next_frontier.steps),
                            expected_lag_days: next_frontier.lag_days,
                            magnitude_estimate: event
                                .magnitude
                                .map(|m| m.abs() * next_frontier.weight),
                            confidence: next_frontier.confidence
                                * HOP_CONFIDENCE_DECAY.powi(next_hop as i32),
                            provenance: next_frontier
                                .steps
                                .iter()
                                .map(|s| (s.edge.source_name.clone(), s.edge.source_url.clone()))
                                .collect(),
                        });
                    }
                }
                queue.push_back(next_frontier);
            }
        }

        // Rank by hop, then confidence (desc), then code for stability.
        entries.sort_by(|a, b| {
            a.hop
                .cmp(&b.hop)
                .then_with(|| b.confidence.total_cmp(&a.confidence))
                .then_with(|| a.code.cmp(&b.code))
        });

        let mut report = ImpactReport {
            event_title: event.title.clone(),
            subject: subject.clone(),
            summary: String::new(),
            primary_benefit: vec![],
            primary_harm: vec![],
            secondary_benefit: vec![],
            secondary_harm: vec![],
            potential: vec![],
            disclaimer: "影响方向、幅度与滞后天数均为基于公开产业链关系的粗略启发式估计，\
                         不构成投资建议；置信度随传导层级按 0.8 逐跳衰减。"
                .to_string(),
        };
        for entry in entries {
            match (entry.hop, entry.direction) {
                (1, ImpactDirection::Benefit) => report.primary_benefit.push(entry),
                (1, ImpactDirection::Harm) => report.primary_harm.push(entry),
                (2, ImpactDirection::Benefit) => report.secondary_benefit.push(entry),
                (2, ImpactDirection::Harm) => report.secondary_harm.push(entry),
                _ => report.potential.push(entry),
            }
        }
        report.summary = format!(
            "事件「{}」影响 {} 家公司：一级受益 {}、一级受损 {}、二级受益 {}、二级受损 {}、潜在映射 {}",
            event.title,
            report.primary_benefit.len()
                + report.primary_harm.len()
                + report.secondary_benefit.len()
                + report.secondary_harm.len()
                + report.potential.len(),
            report.primary_benefit.len(),
            report.primary_harm.len(),
            report.secondary_benefit.len(),
            report.secondary_harm.len(),
            report.potential.len(),
        );
        Ok(report)
    }
}

/// Decide whether traversing `edge` from `node` (in `state`) reaches
/// `other`, and if so produce the transition note and the state at the
/// reached node. Pure and testable.
fn transition(
    node: &Node,
    state: State,
    edge: &Edge,
    other: &Node,
    company_subject: bool,
) -> Option<(String, State)> {
    let incoming = edge.dst == node.id && edge.src == other.id;
    let outgoing = edge.src == node.id && edge.dst == other.id;
    if !incoming && !outgoing {
        return None;
    }
    match state {
        State::Price(d) => {
            let dir_word = if d > 0 { "上升" } else { "回落" };
            match (edge.relation, incoming) {
                (Relation::Produces, true) => Some((
                    format!("自产{}", node.name),
                    State::Prosperity {
                        sign: d,
                        pass: Some(d),
                    },
                )),
                (Relation::Consumes, true) => Some((
                    format!("采购{}，成本{dir_word}", node.name),
                    State::Prosperity {
                        sign: -d,
                        pass: Some(d),
                    },
                )),
                (Relation::Substitutes, true) => {
                    Some((format!("{}的替代品，需求转移", node.name), State::Price(d)))
                }
                (Relation::ExposedTo, true) => Some((
                    format!("对{}有风险敞口", node.name),
                    State::Prosperity {
                        sign: d,
                        pass: None,
                    },
                )),
                _ => None,
            }
        }
        State::Prosperity { sign: s, pass } => match (edge.relation, incoming, outgoing) {
            // Group nodes expand to their members.
            (Relation::BelongsTo, true, false)
                if matches!(node.kind, NodeKind::Industry | NodeKind::Segment) =>
            {
                Some((
                    format!("属于{}板块", node.name),
                    State::Prosperity {
                        sign: s,
                        pass: None,
                    },
                ))
            }
            // Cost pass-through: a price-driven impact moves the company's
            // output price in the same direction as its input.
            (Relation::Produces, false, true) if pass.is_some() => {
                let d = pass.unwrap();
                let note = if d > 0 {
                    "成本传导提价"
                } else {
                    "成本传导降价"
                };
                Some((note.to_string(), State::Price(d)))
            }
            // Non-price-driven harm (accident etc.): supply disruption
            // pushes output prices up.
            (Relation::Produces, false, true) if s < 0 => {
                Some(("供给收缩，提价压力".to_string(), State::Price(1)))
            }
            // Suppliers feel the same direction (demand expansion/contraction).
            (Relation::Supplies, true, false) | (Relation::CustomerOf, false, true) => Some((
                format!("是{}的供应商", node.name),
                State::Prosperity {
                    sign: s,
                    pass: None,
                },
            )),
            // Customers only inherit harm (disruption / cost pass-through).
            (Relation::Supplies, false, true) if s < 0 => Some((
                format!("是{}的客户", node.name),
                State::Prosperity {
                    sign: s,
                    pass: None,
                },
            )),
            (Relation::CustomerOf, true, false) if s < 0 => Some((
                format!("是{}的客户", node.name),
                State::Prosperity {
                    sign: s,
                    pass: None,
                },
            )),
            // Competitors move opposite, only for company-subject events.
            (Relation::Competes, _, _) if company_subject => Some((
                format!("与{}竞争", node.name),
                State::Prosperity {
                    sign: -s,
                    pass: None,
                },
            )),
            _ => None,
        },
    }
}

/// Render the human-readable logic chain for one impacted company.
/// Company steps carry their 受益/受损 label; intermediate material/product
/// steps show only the transition note.
fn render_chain(subject: &Node, event: &Event, steps: &[Step]) -> String {
    let mut chain = match event.magnitude {
        Some(m) => {
            let arrow = if event.direction > 0 { "↑" } else { "↓" };
            format!("{}{}{:.0}%", subject.name, arrow, m.abs() * 100.0)
        }
        None => {
            let arrow = if event.direction > 0 { "↑" } else { "↓" };
            format!("{}{}", subject.name, arrow)
        }
    };
    for step in steps {
        match step.impact {
            Some(sign) => chain.push_str(&format!(
                " → {}（{}，{}）",
                step.node.name,
                step.note,
                ImpactDirection::from_sign(sign).label()
            )),
            None => chain.push_str(&format!(" → {}（{}）", step.node.name, step.note)),
        }
    }
    chain
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeKind;
    use astock_storage::{Storage, StorageConfig};

    /// Synthetic graph: 铜 → 江铜 (produces) / 线缆厂 (consumes) → 线缆 → 家电厂,
    /// plus 铝 substitutes 铜 → 铝厂, and a cycle 线缆厂 ⇄ 家电厂 (competes-free
    /// back edge) to prove the cycle guard terminates.
    async fn synthetic() -> (tempfile::TempDir, Engine) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let store = GraphStore::new(storage);
        let nodes = [
            ("commodity:cu", NodeKind::Commodity, "铜", None),
            ("material:al", NodeKind::Material, "铝", None),
            ("product:cable", NodeKind::Product, "线缆", None),
            ("company:600362", NodeKind::Company, "江铜", Some("600362")),
            (
                "company:600869",
                NodeKind::Company,
                "线缆厂",
                Some("600869"),
            ),
            (
                "company:000651",
                NodeKind::Company,
                "家电厂",
                Some("000651"),
            ),
            ("company:601600", NodeKind::Company, "铝厂", Some("601600")),
        ];
        for (id, kind, name, code) in nodes {
            store
                .upsert_node(&Node {
                    id: id.into(),
                    kind,
                    name: name.into(),
                    code: code.map(str::to_string),
                    meta: serde_json::json!({}),
                })
                .await
                .unwrap();
        }
        let edge = |src: &str, dst: &str, rel: Relation, w: f64, c: f64| Edge {
            id: None,
            src: src.into(),
            dst: dst.into(),
            relation: rel,
            weight: w,
            source_name: "公司年报2024".into(),
            source_url: "https://example.com".into(),
            confidence: c,
            valid_from: 0,
            valid_to: None,
        };
        store
            .upsert_edge(&edge(
                "company:600362",
                "commodity:cu",
                Relation::Produces,
                0.9,
                0.9,
            ))
            .await
            .unwrap();
        store
            .upsert_edge(&edge(
                "company:600869",
                "commodity:cu",
                Relation::Consumes,
                0.8,
                0.8,
            ))
            .await
            .unwrap();
        store
            .upsert_edge(&edge(
                "company:600869",
                "product:cable",
                Relation::Produces,
                0.9,
                0.9,
            ))
            .await
            .unwrap();
        store
            .upsert_edge(&edge(
                "company:000651",
                "product:cable",
                Relation::Consumes,
                0.7,
                0.7,
            ))
            .await
            .unwrap();
        store
            .upsert_edge(&edge(
                "material:al",
                "commodity:cu",
                Relation::Substitutes,
                0.5,
                0.6,
            ))
            .await
            .unwrap();
        store
            .upsert_edge(&edge(
                "company:601600",
                "material:al",
                Relation::Produces,
                0.8,
                0.8,
            ))
            .await
            .unwrap();
        // Back edge forming a cycle; `competes` is never traversed for
        // commodity-subject events, so the BFS must rely on the guard.
        store
            .upsert_edge(&edge(
                "company:000651",
                "company:600869",
                Relation::Competes,
                0.5,
                0.6,
            ))
            .await
            .unwrap();
        (dir, Engine::new(store))
    }

    #[tokio::test]
    async fn propagation_direction_rules_and_hop_decay() {
        let (_dir, engine) = synthetic().await;
        let event = Event::new(
            "e1",
            "commodity_price",
            "铜价上涨10%",
            "铜",
            Some(0.10),
            1,
            0,
        );
        let report = engine.propagate(&event).await.unwrap();

        // 一级受益: producer of 铜.
        assert_eq!(report.primary_benefit.len(), 1);
        assert_eq!(report.primary_benefit[0].name, "江铜");
        assert_eq!(report.primary_benefit[0].hop, 1);
        assert_eq!(
            report.primary_benefit[0].logic_chain,
            "铜↑10% → 江铜（自产铜，受益）"
        );

        // 一级受损: consumer of 铜.
        assert_eq!(report.primary_harm.len(), 1);
        assert_eq!(report.primary_harm[0].name, "线缆厂");

        // 二级: 家电厂 via cost pass-through (受损), 铝厂 via substitutes (受益).
        assert_eq!(report.secondary_harm.len(), 1);
        assert_eq!(report.secondary_harm[0].name, "家电厂");
        assert_eq!(report.secondary_harm[0].hop, 2);
        assert_eq!(report.secondary_benefit.len(), 1);
        assert_eq!(report.secondary_benefit[0].name, "铝厂");
        assert_eq!(report.secondary_benefit[0].hop, 2);

        // Confidence decays per hop: hop2 < hop1 for equal edge confidence.
        let producer_conf = report.primary_benefit[0].confidence; // 0.9 * 0.8
        assert!((producer_conf - 0.72).abs() < 1e-9);
        let appliance = &report.secondary_harm[0];
        // 0.8 * 0.9 * 0.7 (edges) * 0.8^2 (hops)
        let expected = 0.8 * 0.9 * 0.7 * 0.64;
        assert!((appliance.confidence - expected).abs() < 1e-9);

        // Magnitude estimate: |0.10| * path weights.
        let expected_mag = 0.10 * 0.8 * 0.9 * 0.7;
        assert!((appliance.magnitude_estimate.unwrap() - expected_mag).abs() < 1e-9);

        // Lag: consumes 30 + produces 5 + consumes 30 = 65.
        assert_eq!(appliance.expected_lag_days, 65);

        // Cycle guard: each company appears at most once.
        let mut ids: Vec<&str> = report
            .primary_benefit
            .iter()
            .chain(&report.primary_harm)
            .chain(&report.secondary_benefit)
            .chain(&report.secondary_harm)
            .chain(&report.potential)
            .map(|e| e.node_id.as_str())
            .collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total);
    }

    #[tokio::test]
    async fn direction_down_flips_benefit_and_harm() {
        let (_dir, engine) = synthetic().await;
        let event = Event::new(
            "e2",
            "commodity_price",
            "铜价下跌10%",
            "铜",
            Some(0.10),
            -1,
            0,
        );
        let report = engine.propagate(&event).await.unwrap();
        // Falling copper hurts the producer, helps the consumer.
        assert_eq!(report.primary_harm[0].name, "江铜");
        assert_eq!(report.primary_benefit[0].name, "线缆厂");
    }

    #[tokio::test]
    async fn unknown_subject_is_an_error() {
        let (_dir, engine) = synthetic().await;
        let event = Event::new("e3", "commodity_price", "金价上涨", "金", Some(0.05), 1, 0);
        assert!(engine.propagate(&event).await.is_err());
    }
}
