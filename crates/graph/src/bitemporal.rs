//! Bitemporal, immutable relation revisions and reproducible graph snapshots.
//!
//! `valid_*` describes when a relation is true in the business world while
//! `recorded_at/superseded_at` describes when this system knew a revision.
//! Queries always provide both clocks, which prevents late discoveries and
//! corrections from leaking into historical research.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::model::{Edge, Node, Relation};
use crate::store::{now_secs, GraphStore};

/// Evidence source class controls revalidation cadence and confidence decay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSourceType {
    AnnualReport,
    Prospectus,
    InvestorResearch,
    Tender,
    Contract,
    Patent,
    RegulatoryApproval,
    CapacityDisclosure,
    Research,
    Manual,
    Legacy,
}

impl EvidenceSourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AnnualReport => "annual_report",
            Self::Prospectus => "prospectus",
            Self::InvestorResearch => "investor_research",
            Self::Tender => "tender",
            Self::Contract => "contract",
            Self::Patent => "patent",
            Self::RegulatoryApproval => "regulatory_approval",
            Self::CapacityDisclosure => "capacity_disclosure",
            Self::Research => "research",
            Self::Manual => "manual",
            Self::Legacy => "legacy",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "annual_report" => Self::AnnualReport,
            "prospectus" => Self::Prospectus,
            "investor_research" => Self::InvestorResearch,
            "tender" => Self::Tender,
            "contract" => Self::Contract,
            "patent" => Self::Patent,
            "regulatory_approval" => Self::RegulatoryApproval,
            "capacity_disclosure" => Self::CapacityDisclosure,
            "research" => Self::Research,
            "manual" => Self::Manual,
            _ => Self::Legacy,
        }
    }

    /// Days after recording at which evidence needs an explicit recheck.
    pub fn revalidation_days(self) -> i64 {
        match self {
            Self::AnnualReport | Self::Prospectus => 400,
            Self::InvestorResearch | Self::Research => 90,
            Self::Tender | Self::Contract => 180,
            Self::Patent | Self::RegulatoryApproval => 730,
            Self::CapacityDisclosure => 365,
            Self::Manual | Self::Legacy => 180,
        }
    }

    /// Evidence confidence half-life in days. This never deletes evidence;
    /// it only makes ageing visible and queues it for review.
    pub fn decay_half_life_days(self) -> i64 {
        match self {
            Self::AnnualReport | Self::Prospectus => 730,
            Self::InvestorResearch | Self::Research => 180,
            Self::Tender | Self::Contract => 365,
            Self::Patent | Self::RegulatoryApproval => 1_460,
            Self::CapacityDisclosure => 730,
            Self::Manual | Self::Legacy => 365,
        }
    }

    pub fn infer(source_name: &str) -> Self {
        if source_name.contains("年报") || source_name.contains("年度报告") {
            Self::AnnualReport
        } else if source_name.contains("招股") {
            Self::Prospectus
        } else if source_name.contains("调研") {
            Self::InvestorResearch
        } else if source_name.contains("招标") || source_name.contains("中标") {
            Self::Tender
        } else if source_name.contains("合同") {
            Self::Contract
        } else if source_name.contains("专利") {
            Self::Patent
        } else if source_name.contains("批文") || source_name.contains("许可") {
            Self::RegulatoryApproval
        } else if source_name.contains("产能") {
            Self::CapacityDisclosure
        } else {
            Self::Manual
        }
    }
}

/// Stored lifecycle state of one immutable relation revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationStatus {
    Candidate,
    Verified,
    Active,
    Stale,
    Contradicted,
    Expired,
    Revoked,
}

impl RelationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Verified => "verified",
            Self::Active => "active",
            Self::Stale => "stale",
            Self::Contradicted => "contradicted",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "candidate" => Self::Candidate,
            "verified" => Self::Verified,
            "active" => Self::Active,
            "stale" => Self::Stale,
            "contradicted" => Self::Contradicted,
            "expired" => Self::Expired,
            "revoked" => Self::Revoked,
            _ => Self::Candidate,
        }
    }

    fn visible_in_active_graph(self) -> bool {
        matches!(self, Self::Verified | Self::Active | Self::Stale)
    }
}

/// Input for one evidence revision. Revisions are idempotent by content hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeRevisionInput {
    pub edge: Edge,
    #[serde(default)]
    pub product_scope: Option<String>,
    #[serde(default)]
    pub region_scope: Option<String>,
    #[serde(default)]
    pub disclosed_share: Option<f64>,
    pub source_type: EvidenceSourceType,
    pub evidence_version: String,
    pub status: RelationStatus,
    pub observed_at: i64,
    pub recorded_at: i64,
    #[serde(default)]
    pub supersedes_revision_id: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl EdgeRevisionInput {
    /// Production default for an already source-validated legacy `Edge`.
    pub fn from_edge(edge: Edge, recorded_at: i64) -> Self {
        let source_type = EvidenceSourceType::infer(&edge.source_name);
        let evidence_version = digest(&format!(
            "{}|{}|{}|{}|{}|{}",
            edge.source_name,
            edge.source_url,
            edge.src,
            edge.dst,
            edge.relation.as_str(),
            edge.valid_from
        ));
        Self {
            disclosed_share: Some(edge.weight),
            edge,
            product_scope: None,
            region_scope: None,
            source_type,
            evidence_version,
            status: RelationStatus::Active,
            observed_at: recorded_at,
            recorded_at,
            supersedes_revision_id: None,
            metadata: serde_json::json!({"projection":"legacy_graph_edges"}),
        }
    }
}

/// Immutable revision joined with its stable identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeRevision {
    pub revision_id: String,
    pub identity_id: String,
    pub revision_no: u32,
    pub src: String,
    pub dst: String,
    pub relation: Relation,
    pub product_scope: Option<String>,
    pub region_scope: Option<String>,
    pub weight: f64,
    pub confidence: f64,
    pub disclosed_share: Option<f64>,
    pub source_type: EvidenceSourceType,
    pub source_name: String,
    pub source_url: String,
    pub evidence_version: String,
    pub status: RelationStatus,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    pub observed_at: i64,
    pub recorded_at: i64,
    pub superseded_at: Option<i64>,
    pub revalidate_after: i64,
    pub decay_half_life_days: i64,
    pub supersedes_revision_id: Option<String>,
    pub metadata: serde_json::Value,
}

/// One edge as seen in a specific business-time + knowledge-time snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotEdge {
    pub revision_id: String,
    pub identity_id: String,
    pub revision_no: u32,
    pub src: String,
    pub original_src: String,
    pub dst: String,
    pub original_dst: String,
    pub relation: Relation,
    pub product_scope: Option<String>,
    pub region_scope: Option<String>,
    pub weight: f64,
    pub disclosed_share: Option<f64>,
    pub confidence: f64,
    pub effective_confidence: f64,
    pub source_type: EvidenceSourceType,
    pub source_name: String,
    pub source_url: String,
    pub evidence_version: String,
    pub status: RelationStatus,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    pub observed_at: i64,
    pub recorded_at: i64,
    pub revalidate_after: i64,
}

impl SnapshotEdge {
    pub fn as_edge(&self) -> Edge {
        Edge {
            id: None,
            src: self.src.clone(),
            dst: self.dst.clone(),
            relation: self.relation,
            weight: self.weight,
            source_name: self.source_name.clone(),
            source_url: self.source_url.clone(),
            confidence: self.effective_confidence,
            valid_from: self.valid_from,
            valid_to: self.valid_to,
        }
    }
}

/// Deterministic bitemporal snapshot. `snapshot_id` is a hash of the clocks,
/// selected revision ids and entity-merge ids, not of query execution time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphSnapshot {
    pub snapshot_id: String,
    pub business_time: i64,
    pub knowledge_time: i64,
    pub nodes: Vec<Node>,
    pub edges: Vec<SnapshotEdge>,
    pub revision_ids: Vec<String>,
    pub merge_ids: Vec<String>,
    pub stale_count: usize,
    pub excluded_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphHistoryBounds {
    pub business_min: i64,
    pub business_max: i64,
    pub knowledge_min: i64,
    pub knowledge_max: i64,
    pub revision_count: usize,
    pub revalidation_due_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityMergeRevision {
    pub merge_id: String,
    pub from_node_id: String,
    pub to_node_id: String,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    pub recorded_at: i64,
    pub superseded_at: Option<i64>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphSnapshotDiff {
    pub left_snapshot_id: String,
    pub right_snapshot_id: String,
    pub added_revision_ids: Vec<String>,
    pub removed_revision_ids: Vec<String>,
    pub changed_identity_ids: Vec<String>,
}

impl GraphStore {
    /// Record a revision at the actual system time.
    pub async fn record_revision(&self, mut input: EdgeRevisionInput) -> Result<EdgeRevision> {
        input.recorded_at = now_secs();
        if input.observed_at <= 0 || input.observed_at > input.recorded_at {
            input.observed_at = input.recorded_at;
        }
        self.record_revision_at(input).await
    }

    /// Replay/import entry point with an explicit transaction time. Production
    /// callers should prefer [`Self::record_revision`].
    pub async fn record_revision_at(&self, input: EdgeRevisionInput) -> Result<EdgeRevision> {
        validate_revision_input(&input)?;
        for endpoint in [&input.edge.src, &input.edge.dst] {
            if self.storage().graph_node_get(endpoint).await?.is_none() {
                return Err(Error::NotFound(endpoint.clone()));
            }
        }
        let proposed_identity = identity_hash(&input);
        let product = input.product_scope.clone().unwrap_or_default();
        let region = input.region_scope.clone().unwrap_or_default();
        let src = input.edge.src.clone();
        let dst = input.edge.dst.clone();
        let relation = input.edge.relation.as_str().to_string();
        let storage = self.storage().clone();
        storage
            .run(move |conn| {
                let tx = conn.transaction()?;
                tx.execute(
                    "INSERT INTO graph_edge_identities
                     (identity_id,src,dst,relation,product_scope,region_scope,created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)
                     ON CONFLICT(src,dst,relation,product_scope,region_scope) DO NOTHING",
                    params![proposed_identity, src, dst, relation, product, region, input.recorded_at],
                )?;
                let identity_id: String = tx.query_row(
                    "SELECT identity_id FROM graph_edge_identities
                     WHERE src=?1 AND dst=?2 AND relation=?3 AND product_scope=?4 AND region_scope=?5",
                    params![input.edge.src, input.edge.dst, input.edge.relation.as_str(),
                        input.product_scope.as_deref().unwrap_or(""), input.region_scope.as_deref().unwrap_or("")],
                    |row| row.get(0),
                )?;
                let revision_id = revision_hash(&identity_id, &input);
                if let Some(existing) = select_revision(&tx, &revision_id)? {
                    tx.commit()?;
                    return Ok(existing);
                }
                let revision_no: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(revision_no),0)+1 FROM graph_edge_revisions WHERE identity_id=?1",
                    [&identity_id],
                    |row| row.get(0),
                )?;
                let previous_revision: Option<(String, String)> = tx
                    .query_row(
                        "SELECT revision_id,evidence_version FROM graph_edge_revisions
                         WHERE identity_id=?1 ORDER BY revision_no DESC LIMIT 1",
                        [&identity_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                if let Some(previous) = input.supersedes_revision_id.as_deref() {
                    let previous_identity: Option<String> = tx
                        .query_row(
                            "SELECT identity_id FROM graph_edge_revisions WHERE revision_id=?1",
                            [previous],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if previous_identity.as_deref() != Some(identity_id.as_str()) {
                        return Err(astock_storage::Error::Invalid(format!(
                            "superseded revision {previous} does not belong to identity {identity_id}"
                        )));
                    }
                    tx.execute(
                        "UPDATE graph_edge_revisions
                         SET superseded_at=?2
                         WHERE revision_id=?1 AND (superseded_at IS NULL OR superseded_at>?2)",
                        params![previous, input.recorded_at],
                    )?;
                }
                let revalidate_after = input.recorded_at.saturating_add(
                    input.source_type.revalidation_days().saturating_mul(86_400),
                );
                tx.execute(
                    "INSERT INTO graph_edge_revisions
                     (revision_id,identity_id,revision_no,weight,confidence,disclosed_share,
                      source_type,source_name,source_url,evidence_version,status,
                      valid_from,valid_to,observed_at,recorded_at,superseded_at,
                      revalidate_after,decay_half_life_days,supersedes_revision_id,metadata_json)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,NULL,?16,?17,?18,?19)",
                    params![revision_id, identity_id, revision_no, input.edge.weight,
                        input.edge.confidence, input.disclosed_share, input.source_type.as_str(),
                        input.edge.source_name, input.edge.source_url, input.evidence_version,
                        input.status.as_str(), input.edge.valid_from, input.edge.valid_to,
                        input.observed_at, input.recorded_at, revalidate_after,
                        input.source_type.decay_half_life_days(), input.supersedes_revision_id,
                        serde_json::to_string(&input.metadata)?],
                )?;
                if let Some((_, previous_evidence)) = previous_revision {
                    if previous_evidence != input.evidence_version
                        && matches!(
                            input.source_type,
                            EvidenceSourceType::AnnualReport | EvidenceSourceType::Prospectus
                        )
                    {
                        tx.execute(
                            "INSERT OR IGNORE INTO graph_revalidation_events
                             (event_id,identity_id,revision_id,trigger_type,related_identity_id,status,reason,created_at)
                             VALUES (?1,?2,?3,'annual_report_update',NULL,'pending',?4,?5)",
                            params![format!("reval:{revision_id}:annual"),identity_id,revision_id,
                                "新年报/招股书证据进入系统，需要核对报告期、占比和客户变化",input.recorded_at],
                        )?;
                    }
                }
                if input.status == RelationStatus::Contradicted {
                    tx.execute(
                        "INSERT OR IGNORE INTO graph_revalidation_events
                         (event_id,identity_id,revision_id,trigger_type,related_identity_id,status,reason,created_at)
                         VALUES (?1,?2,?3,'contradictory_evidence',NULL,'pending',?4,?5)",
                        params![format!("reval:{revision_id}:conflict"),identity_id,revision_id,
                            "出现反方证据，关系已从有效图排除并等待复核",input.recorded_at],
                    )?;
                }
                if matches!(input.edge.relation, Relation::Supplies | Relation::CustomerOf)
                    && matches!(
                        input.source_type,
                        EvidenceSourceType::AnnualReport | EvidenceSourceType::Prospectus
                    )
                {
                    let related: Option<String> = tx
                        .query_row(
                            "SELECT identity_id FROM graph_edge_identities
                             WHERE src=?1 AND relation=?2 AND product_scope=?3
                               AND dst<>?4 AND identity_id<>?5
                             ORDER BY created_at DESC LIMIT 1",
                            params![input.edge.src,input.edge.relation.as_str(),
                                input.product_scope.as_deref().unwrap_or(""),input.edge.dst,identity_id],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if let Some(related) = related {
                        tx.execute(
                            "INSERT OR IGNORE INTO graph_revalidation_events
                             (event_id,identity_id,revision_id,trigger_type,related_identity_id,status,reason,created_at)
                             VALUES (?1,?2,?3,'counterparty_change',?4,'pending',?5,?6)",
                            params![format!("reval:{revision_id}:counterparty"),identity_id,revision_id,
                                related,"同一主体与产品范围出现不同客户/供应商，需要核对是否为变更、并存或实体别名",input.recorded_at],
                        )?;
                    }
                }
                let saved = select_revision(&tx, &revision_id)?.ok_or_else(|| {
                    astock_storage::Error::Invalid("saved graph revision missing".into())
                })?;
                tx.commit()?;
                Ok(saved)
            })
            .await
            .map_err(Error::from)
    }

    /// Record a PIT-visible entity merge. Original edge revisions remain
    /// unchanged; only snapshot projection resolves the node alias.
    pub async fn record_entity_merge_at(
        &self,
        from_node_id: &str,
        to_node_id: &str,
        valid_from: i64,
        valid_to: Option<i64>,
        recorded_at: i64,
        reason: &str,
    ) -> Result<EntityMergeRevision> {
        if from_node_id == to_node_id || reason.trim().is_empty() {
            return Err(Error::Invalid("实体合并目标必须不同且理由不能为空".into()));
        }
        if valid_to.is_some_and(|end| end <= valid_from) || recorded_at < 0 {
            return Err(Error::Invalid("实体合并时间区间无效".into()));
        }
        for node in [from_node_id, to_node_id] {
            if self.storage().graph_node_get(node).await?.is_none() {
                return Err(Error::NotFound(node.into()));
            }
        }
        let merge_id = format!(
            "merge:{}",
            &digest(&format!(
                "{from_node_id}|{to_node_id}|{valid_from}|{valid_to:?}|{recorded_at}|{reason}"
            ))[..32]
        );
        let saved = EntityMergeRevision {
            merge_id: merge_id.clone(),
            from_node_id: from_node_id.into(),
            to_node_id: to_node_id.into(),
            valid_from,
            valid_to,
            recorded_at,
            superseded_at: None,
            reason: reason.trim().into(),
        };
        let row = saved.clone();
        self.storage()
            .run(move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO graph_entity_merges
                     (merge_id,from_node_id,to_node_id,valid_from,valid_to,recorded_at,superseded_at,reason)
                     VALUES (?1,?2,?3,?4,?5,?6,NULL,?7)",
                    params![row.merge_id,row.from_node_id,row.to_node_id,row.valid_from,row.valid_to,
                        row.recorded_at,row.reason],
                )?;
                Ok(())
            })
            .await?;
        Ok(saved)
    }

    /// Rebuild exactly what was business-valid and system-known at two clocks.
    pub async fn graph_as_of(
        &self,
        business_time: i64,
        knowledge_time: i64,
    ) -> Result<GraphSnapshot> {
        if business_time < 0 || knowledge_time < 0 {
            return Err(Error::Invalid("图谱时间不能为负数".into()));
        }
        let revisions = load_as_of_revisions(self, business_time, knowledge_time).await?;
        let merges = load_as_of_merges(self, business_time, knowledge_time).await?;
        let merge_map: HashMap<String, String> = merges
            .iter()
            .map(|row| (row.from_node_id.clone(), row.to_node_id.clone()))
            .collect();
        let mut selected: BTreeMap<String, EdgeRevision> = BTreeMap::new();
        for revision in revisions {
            let replace = selected
                .get(&revision.identity_id)
                .is_none_or(|old| revision_rank(&revision) > revision_rank(old));
            if replace {
                selected.insert(revision.identity_id.clone(), revision);
            }
        }
        let mut edges = Vec::new();
        let mut revision_ids = Vec::new();
        let mut stale_count = 0;
        let mut excluded_count = 0;
        for revision in selected.values() {
            revision_ids.push(revision.revision_id.clone());
            let mut status = revision.status;
            if matches!(status, RelationStatus::Active | RelationStatus::Verified)
                && knowledge_time >= revision.revalidate_after
            {
                status = RelationStatus::Stale;
            }
            if status == RelationStatus::Stale {
                stale_count += 1;
            }
            if !status.visible_in_active_graph() {
                excluded_count += 1;
                continue;
            }
            let age_days =
                knowledge_time.saturating_sub(revision.observed_at).max(0) as f64 / 86_400.0;
            let effective_confidence = (revision.confidence
                * 0.5_f64.powf(age_days / revision.decay_half_life_days.max(1) as f64))
            .clamp(0.0, 1.0);
            edges.push(SnapshotEdge {
                revision_id: revision.revision_id.clone(),
                identity_id: revision.identity_id.clone(),
                revision_no: revision.revision_no,
                src: resolve_merge(&revision.src, &merge_map),
                original_src: revision.src.clone(),
                dst: resolve_merge(&revision.dst, &merge_map),
                original_dst: revision.dst.clone(),
                relation: revision.relation,
                product_scope: revision.product_scope.clone(),
                region_scope: revision.region_scope.clone(),
                weight: revision.weight,
                disclosed_share: revision.disclosed_share,
                confidence: revision.confidence,
                effective_confidence,
                source_type: revision.source_type,
                source_name: revision.source_name.clone(),
                source_url: revision.source_url.clone(),
                evidence_version: revision.evidence_version.clone(),
                status,
                valid_from: revision.valid_from,
                valid_to: revision.valid_to,
                observed_at: revision.observed_at,
                recorded_at: revision.recorded_at,
                revalidate_after: revision.revalidate_after,
            });
        }
        edges.sort_by(|a, b| a.identity_id.cmp(&b.identity_id));
        revision_ids.sort();
        let mut merge_ids: Vec<String> = merges.into_iter().map(|row| row.merge_id).collect();
        merge_ids.sort();
        let snapshot_id = snapshot_hash(business_time, knowledge_time, &revision_ids, &merge_ids);
        let endpoint_ids: HashSet<&str> = edges
            .iter()
            .flat_map(|edge| [edge.src.as_str(), edge.dst.as_str()])
            .collect();
        let mut nodes: Vec<Node> = self
            .all_nodes()
            .await?
            .into_iter()
            .filter(|node| endpoint_ids.contains(node.id.as_str()))
            .collect();
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        let ids_json = serde_json::to_string(&revision_ids)?;
        let merges_json = serde_json::to_string(&merge_ids)?;
        let snapshot_db = snapshot_id.clone();
        self.storage()
            .run(move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO graph_snapshot_records
                     (snapshot_id,business_time,knowledge_time,revision_ids_json,merge_ids_json,created_at)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![snapshot_db,business_time,knowledge_time,ids_json,merges_json,now_secs()],
                )?;
                Ok(())
            })
            .await?;
        Ok(GraphSnapshot {
            snapshot_id,
            business_time,
            knowledge_time,
            nodes,
            edges,
            revision_ids,
            merge_ids,
            stale_count,
            excluded_count,
        })
    }

    /// Replay a previously materialised snapshot by its deterministic id.
    pub async fn graph_snapshot(&self, snapshot_id: &str) -> Result<Option<GraphSnapshot>> {
        let id = snapshot_id.to_string();
        let clocks = self
            .storage()
            .run(move |conn| {
                conn.query_row(
                    "SELECT business_time,knowledge_time FROM graph_snapshot_records WHERE snapshot_id=?1",
                    [id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()
                .map_err(Into::into)
            })
            .await?;
        let Some((business_time, knowledge_time)) = clocks else {
            return Ok(None);
        };
        let snapshot = self.graph_as_of(business_time, knowledge_time).await?;
        if snapshot.snapshot_id != snapshot_id {
            return Err(Error::Invalid(format!(
                "快照 {snapshot_id} 已无法按相同输入重建，数据库存在回溯写入"
            )));
        }
        Ok(Some(snapshot))
    }

    pub async fn graph_history_bounds(&self) -> Result<GraphHistoryBounds> {
        let now = now_secs();
        self.storage()
            .run(move |conn| {
                let (business_min, knowledge_min, revision_count): (Option<i64>, Option<i64>, i64) =
                    conn.query_row(
                        "SELECT MIN(CASE WHEN valid_from>0 THEN valid_from END),MIN(recorded_at),COUNT(*)
                         FROM graph_edge_revisions",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )?;
                let due: i64 = conn.query_row(
                    "SELECT
                       (SELECT COUNT(*) FROM graph_edge_revisions
                        WHERE status IN ('active','verified','stale')
                          AND recorded_at<=?1 AND (superseded_at IS NULL OR superseded_at>?1)
                          AND revalidate_after<=?1)
                       + (SELECT COUNT(*) FROM graph_revalidation_events
                          WHERE status='pending' AND created_at<=?1)",
                    [now],
                    |row| row.get(0),
                )?;
                Ok(GraphHistoryBounds {
                    business_min: business_min.or(knowledge_min).unwrap_or(now),
                    business_max: now,
                    knowledge_min: knowledge_min.unwrap_or(now),
                    knowledge_max: now,
                    revision_count: revision_count.max(0) as usize,
                    revalidation_due_count: due.max(0) as usize,
                })
            })
            .await
            .map_err(Error::from)
    }

    pub async fn edge_timeline(&self, identity_id: &str) -> Result<Vec<EdgeRevision>> {
        let identity_id = identity_id.to_string();
        self.storage()
            .run(move |conn| {
                let mut stmt = conn.prepare(&format!(
                    "{REVISION_SELECT} WHERE r.identity_id=?1 ORDER BY r.recorded_at,r.revision_no"
                ))?;
                let rows = stmt.query_map([identity_id], revision_from_row)?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(Error::from)
    }

    pub async fn revalidation_due(&self, knowledge_time: i64) -> Result<Vec<EdgeRevision>> {
        let all = load_as_of_revisions(self, knowledge_time, knowledge_time).await?;
        let mut latest: BTreeMap<String, EdgeRevision> = BTreeMap::new();
        for revision in all {
            if revision.revalidate_after > knowledge_time
                || !matches!(
                    revision.status,
                    RelationStatus::Active | RelationStatus::Verified | RelationStatus::Stale
                )
            {
                continue;
            }
            let replace = latest
                .get(&revision.identity_id)
                .is_none_or(|old| revision_rank(&revision) > revision_rank(old));
            if replace {
                latest.insert(revision.identity_id.clone(), revision);
            }
        }
        Ok(latest.into_values().collect())
    }

    pub async fn compare_graph_snapshots(
        &self,
        left_business_time: i64,
        left_knowledge_time: i64,
        right_business_time: i64,
        right_knowledge_time: i64,
    ) -> Result<GraphSnapshotDiff> {
        let left = self
            .graph_as_of(left_business_time, left_knowledge_time)
            .await?;
        let right = self
            .graph_as_of(right_business_time, right_knowledge_time)
            .await?;
        let left_ids: BTreeSet<&String> = left.revision_ids.iter().collect();
        let right_ids: BTreeSet<&String> = right.revision_ids.iter().collect();
        let added_revision_ids = right_ids
            .difference(&left_ids)
            .map(|id| (*id).clone())
            .collect();
        let removed_revision_ids = left_ids
            .difference(&right_ids)
            .map(|id| (*id).clone())
            .collect();
        let left_by_identity: BTreeMap<&str, &str> = left
            .edges
            .iter()
            .map(|edge| (edge.identity_id.as_str(), edge.revision_id.as_str()))
            .collect();
        let right_by_identity: BTreeMap<&str, &str> = right
            .edges
            .iter()
            .map(|edge| (edge.identity_id.as_str(), edge.revision_id.as_str()))
            .collect();
        let changed_identity_ids = left_by_identity
            .keys()
            .chain(right_by_identity.keys())
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|id| left_by_identity.get(id) != right_by_identity.get(id))
            .map(str::to_string)
            .collect();
        Ok(GraphSnapshotDiff {
            left_snapshot_id: left.snapshot_id,
            right_snapshot_id: right.snapshot_id,
            added_revision_ids,
            removed_revision_ids,
            changed_identity_ids,
        })
    }
}

const REVISION_SELECT: &str =
    "SELECT r.revision_id,r.identity_id,r.revision_no,i.src,i.dst,i.relation,
            i.product_scope,i.region_scope,r.weight,r.confidence,r.disclosed_share,
            r.source_type,r.source_name,r.source_url,r.evidence_version,r.status,
            r.valid_from,r.valid_to,r.observed_at,r.recorded_at,r.superseded_at,
            r.revalidate_after,r.decay_half_life_days,r.supersedes_revision_id,r.metadata_json
       FROM graph_edge_revisions r
       JOIN graph_edge_identities i ON i.identity_id=r.identity_id";

fn revision_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EdgeRevision> {
    let relation: String = row.get(5)?;
    let product: String = row.get(6)?;
    let region: String = row.get(7)?;
    let source_type: String = row.get(11)?;
    let status: String = row.get(15)?;
    let metadata_json: String = row.get(24)?;
    Ok(EdgeRevision {
        revision_id: row.get(0)?,
        identity_id: row.get(1)?,
        revision_no: row.get::<_, i64>(2)?.max(0) as u32,
        src: row.get(3)?,
        dst: row.get(4)?,
        relation: Relation::parse(&relation).unwrap_or(Relation::ExposedTo),
        product_scope: (!product.is_empty()).then_some(product),
        region_scope: (!region.is_empty()).then_some(region),
        weight: row.get(8)?,
        confidence: row.get(9)?,
        disclosed_share: row.get(10)?,
        source_type: EvidenceSourceType::parse(&source_type),
        source_name: row.get(12)?,
        source_url: row.get(13)?,
        evidence_version: row.get(14)?,
        status: RelationStatus::parse(&status),
        valid_from: row.get(16)?,
        valid_to: row.get(17)?,
        observed_at: row.get(18)?,
        recorded_at: row.get(19)?,
        superseded_at: row.get(20)?,
        revalidate_after: row.get(21)?,
        decay_half_life_days: row.get(22)?,
        supersedes_revision_id: row.get(23)?,
        metadata: serde_json::from_str(&metadata_json).unwrap_or(serde_json::Value::Null),
    })
}

fn select_revision(
    conn: &rusqlite::Connection,
    revision_id: &str,
) -> astock_storage::Result<Option<EdgeRevision>> {
    let mut stmt = conn.prepare(&format!("{REVISION_SELECT} WHERE r.revision_id=?1"))?;
    Ok(stmt
        .query_row([revision_id], revision_from_row)
        .optional()?)
}

async fn load_as_of_revisions(
    store: &GraphStore,
    business_time: i64,
    knowledge_time: i64,
) -> Result<Vec<EdgeRevision>> {
    store
        .storage()
        .run(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "{REVISION_SELECT}
                 WHERE r.recorded_at<=?1
                   AND (r.superseded_at IS NULL OR r.superseded_at>?1)
                   AND r.valid_from<=?2
                   AND (r.valid_to IS NULL OR r.valid_to>?2)
                 ORDER BY r.identity_id,r.valid_from,r.observed_at,r.recorded_at,r.revision_no"
            ))?;
            let rows = stmt.query_map(params![knowledge_time, business_time], revision_from_row)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
        .map_err(Error::from)
}

async fn load_as_of_merges(
    store: &GraphStore,
    business_time: i64,
    knowledge_time: i64,
) -> Result<Vec<EntityMergeRevision>> {
    store
        .storage()
        .run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT merge_id,from_node_id,to_node_id,valid_from,valid_to,
                        recorded_at,superseded_at,reason
                 FROM graph_entity_merges
                 WHERE recorded_at<=?1
                   AND (superseded_at IS NULL OR superseded_at>?1)
                   AND valid_from<=?2 AND (valid_to IS NULL OR valid_to>?2)
                 ORDER BY recorded_at,merge_id",
            )?;
            let rows = stmt.query_map(params![knowledge_time, business_time], |row| {
                Ok(EntityMergeRevision {
                    merge_id: row.get(0)?,
                    from_node_id: row.get(1)?,
                    to_node_id: row.get(2)?,
                    valid_from: row.get(3)?,
                    valid_to: row.get(4)?,
                    recorded_at: row.get(5)?,
                    superseded_at: row.get(6)?,
                    reason: row.get(7)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
        .map_err(Error::from)
}

fn validate_revision_input(input: &EdgeRevisionInput) -> Result<()> {
    if input.edge.source_name.trim().is_empty() || input.evidence_version.trim().is_empty() {
        return Err(Error::Invalid("关系修订必须包含来源与证据版本".into()));
    }
    if !(0.0..=1.0).contains(&input.edge.weight)
        || !(0.0..=1.0).contains(&input.edge.confidence)
        || input
            .disclosed_share
            .is_some_and(|share| !(0.0..=1.0).contains(&share))
    {
        return Err(Error::Invalid("关系权重、置信度或占比超出 0..=1".into()));
    }
    if input
        .edge
        .valid_to
        .is_some_and(|end| end <= input.edge.valid_from)
        || input.recorded_at < 0
        || input.observed_at < 0
        || input.observed_at > input.recorded_at
    {
        return Err(Error::Invalid("业务有效期或系统观测时间无效".into()));
    }
    Ok(())
}

fn revision_rank(revision: &EdgeRevision) -> (i64, i64, i64, u32) {
    (
        revision.valid_from,
        revision.observed_at,
        revision.recorded_at,
        revision.revision_no,
    )
}

fn identity_hash(input: &EdgeRevisionInput) -> String {
    format!(
        "edge:{}",
        &digest(&format!(
            "{}|{}|{}|{}|{}",
            input.edge.src,
            input.edge.dst,
            input.edge.relation.as_str(),
            input.product_scope.as_deref().unwrap_or(""),
            input.region_scope.as_deref().unwrap_or("")
        ))[..32]
    )
}

fn revision_hash(identity_id: &str, input: &EdgeRevisionInput) -> String {
    format!(
        "edge-rev:{}",
        &digest(&format!(
            "{identity_id}|{}|{}|{}|{}|{:?}|{}|{}|{}|{}|{}|{:?}|{}",
            input.evidence_version,
            input.edge.valid_from,
            input.edge.valid_to.unwrap_or(i64::MAX),
            input.observed_at,
            input.status,
            input.edge.weight.to_bits(),
            input.edge.confidence.to_bits(),
            input.disclosed_share.map(f64::to_bits).unwrap_or_default(),
            input.edge.source_name,
            input.edge.source_url,
            input.supersedes_revision_id,
            input.metadata
        ))[..40]
    )
}

fn snapshot_hash(
    business_time: i64,
    knowledge_time: i64,
    revision_ids: &[String],
    merge_ids: &[String],
) -> String {
    format!(
        "graph-snapshot:{}",
        &digest(&format!(
            "{business_time}|{knowledge_time}|{}|{}",
            revision_ids.join(","),
            merge_ids.join(",")
        ))[..40]
    )
}

fn digest(value: &str) -> String {
    let bytes = Sha256::digest(value.as_bytes());
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn resolve_merge(id: &str, merges: &HashMap<String, String>) -> String {
    let mut current = id;
    let mut seen = HashSet::new();
    while let Some(next) = merges.get(current) {
        if !seen.insert(current.to_string()) || seen.len() > 16 {
            break;
        }
        current = next;
    }
    current.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeKind, Relation};
    use astock_storage::{Storage, StorageConfig};

    async fn store() -> (tempfile::TempDir, GraphStore) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let graph = GraphStore::new(storage);
        for (code, name) in [
            ("600001", "供应商甲"),
            ("600002", "客户乙"),
            ("600003", "客户乙新主体"),
        ] {
            graph
                .upsert_node(&Node {
                    id: format!("company:{code}"),
                    kind: NodeKind::Company,
                    name: name.into(),
                    code: Some(code.into()),
                    meta: serde_json::json!({}),
                })
                .await
                .unwrap();
        }
        (dir, graph)
    }

    fn revision(
        valid_from: i64,
        valid_to: Option<i64>,
        observed_at: i64,
        recorded_at: i64,
        share: f64,
        evidence: &str,
        status: RelationStatus,
    ) -> EdgeRevisionInput {
        EdgeRevisionInput {
            edge: Edge {
                id: None,
                src: "company:600001".into(),
                dst: "company:600002".into(),
                relation: Relation::Supplies,
                weight: share,
                source_name: format!("{evidence}年报"),
                source_url: format!("https://example.com/{evidence}"),
                confidence: 0.92,
                valid_from,
                valid_to,
            },
            product_scope: Some("动力电池".into()),
            region_scope: Some("中国".into()),
            disclosed_share: Some(share),
            source_type: EvidenceSourceType::AnnualReport,
            evidence_version: evidence.into(),
            status,
            observed_at,
            recorded_at,
            supersedes_revision_id: None,
            metadata: serde_json::json!({"report": evidence}),
        }
    }

    #[tokio::test]
    async fn report_periods_and_late_knowledge_never_overwrite_history() {
        let (_dir, graph) = store().await;
        let y2024 = graph
            .record_revision_at(revision(
                100,
                Some(200),
                120,
                130,
                0.21,
                "2024",
                RelationStatus::Active,
            ))
            .await
            .unwrap();
        let y2025 = graph
            .record_revision_at(revision(
                200,
                None,
                220,
                230,
                0.37,
                "2025",
                RelationStatus::Active,
            ))
            .await
            .unwrap();
        assert_eq!(y2024.identity_id, y2025.identity_id);
        assert_eq!(y2025.revision_no, y2024.revision_no + 1);

        let before_discovery = graph.graph_as_of(210, 225).await.unwrap();
        assert!(before_discovery.edges.is_empty());
        let old_period = graph.graph_as_of(150, 300).await.unwrap();
        assert_eq!(old_period.edges[0].disclosed_share, Some(0.21));
        let new_period = graph.graph_as_of(210, 300).await.unwrap();
        assert_eq!(new_period.edges[0].disclosed_share, Some(0.37));
        assert_eq!(
            graph.graph_as_of(210, 300).await.unwrap().snapshot_id,
            new_period.snapshot_id
        );
        assert_eq!(
            graph
                .graph_snapshot(&new_period.snapshot_id)
                .await
                .unwrap()
                .unwrap(),
            new_period
        );
    }

    #[tokio::test]
    async fn correction_conflict_revocation_and_merge_are_bitemporal() {
        let (_dir, graph) = store().await;
        let original = graph
            .record_revision_at(revision(
                100,
                None,
                110,
                120,
                0.30,
                "original",
                RelationStatus::Active,
            ))
            .await
            .unwrap();
        let mut correction = revision(
            100,
            None,
            110,
            180,
            0.18,
            "correction",
            RelationStatus::Active,
        );
        correction.supersedes_revision_id = Some(original.revision_id.clone());
        graph.record_revision_at(correction).await.unwrap();
        let mut conflict = revision(
            100,
            None,
            190,
            200,
            0.0,
            "conflict",
            RelationStatus::Contradicted,
        );
        conflict.supersedes_revision_id = graph
            .edge_timeline(&original.identity_id)
            .await
            .unwrap()
            .last()
            .map(|row| row.revision_id.clone());
        graph.record_revision_at(conflict).await.unwrap();

        let revalidation_triggers: i64 = graph
            .storage()
            .run(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM graph_revalidation_events WHERE status='pending'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .await
            .unwrap();
        assert!(revalidation_triggers >= 2);

        assert_eq!(
            graph.graph_as_of(150, 150).await.unwrap().edges[0].disclosed_share,
            Some(0.30)
        );
        assert_eq!(
            graph.graph_as_of(150, 190).await.unwrap().edges[0].disclosed_share,
            Some(0.18)
        );
        assert!(graph.graph_as_of(150, 210).await.unwrap().edges.is_empty());

        graph
            .record_entity_merge_at(
                "company:600002",
                "company:600003",
                100,
                None,
                170,
                "工商主体合并",
            )
            .await
            .unwrap();
        let before_merge = graph.graph_as_of(150, 160).await.unwrap();
        assert_eq!(before_merge.edges[0].dst, "company:600002");
        let after_merge = graph.graph_as_of(150, 190).await.unwrap();
        assert_eq!(after_merge.edges[0].dst, "company:600003");
        assert_eq!(after_merge.edges[0].original_dst, "company:600002");
    }

    #[tokio::test]
    async fn source_cadence_marks_stale_without_erasing_revision() {
        let (_dir, graph) = store().await;
        graph
            .record_revision_at(revision(
                100,
                None,
                100,
                100,
                0.3,
                "stale",
                RelationStatus::Verified,
            ))
            .await
            .unwrap();
        let later = 100 + EvidenceSourceType::AnnualReport.revalidation_days() * 86_400 + 1;
        let snapshot = graph.graph_as_of(150, later).await.unwrap();
        assert_eq!(snapshot.stale_count, 1);
        assert_eq!(snapshot.edges[0].status, RelationStatus::Stale);
        assert!(snapshot.edges[0].effective_confidence < snapshot.edges[0].confidence);
        assert_eq!(graph.revalidation_due(later).await.unwrap().len(), 1);
    }
}
