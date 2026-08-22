//! Deterministic, explainable Chinese financial-news event clustering.
//!
//! The engine uses canonical URLs, normalized titles, SimHash/MinHash,
//! hashed semantic vectors, entities, facts and four-time constraints. Every
//! automatic or manual decision is appended to SQLite with a model version;
//! upgrading the engine never silently rewrites historical membership.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use astock_storage::{ArchivedNewsRevision, EvidenceTimestamp, Storage};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use url::Url;

const VECTOR_DIMENSIONS: usize = 256;
const MINHASH_PERMUTATIONS: usize = 24;
pub const CLUSTER_MODEL_VERSION: &str = "zh-fin-event-v1";

static ASSIGN_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static STOCK_CODE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)(?:SH|SZ|BJ)?[0368]\d{5}").expect("stock-code regex"));
static ENTITY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[\p{Han}A-Za-z]{2,16}(?:公司|集团|股份|银行|证券|科技|能源|汽车|医药)")
        .expect("entity regex")
});
static DATE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?P<y>20\d{2})[-年/.](?P<m>\d{1,2})[-月/.](?P<d>\d{1,2})日?").expect("date regex")
});
static FACT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?P<key>营收|营业收入|净利润|回购|增持|减持|持股|目标价|成交价|发行价|产量|销量)[^\d+-]{0,10}(?P<value>[+-]?\d+(?:\.\d+)?(?:%|亿元|万元|万股|元|吨|辆)?)",
    )
    .expect("fact regex")
});

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Storage(#[from] astock_storage::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("invalid news clustering input: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterConfig {
    pub model_version: String,
    pub merge_threshold: f64,
    pub max_event_gap_days: i64,
    pub old_news_days: i64,
    pub evaluation_precision_threshold: f64,
    pub evaluation_recall_threshold: f64,
    pub evaluation_f1_threshold: f64,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            model_version: CLUSTER_MODEL_VERSION.into(),
            merge_threshold: 0.72,
            max_event_gap_days: 14,
            old_news_days: 7,
            evaluation_precision_threshold: 0.90,
            evaluation_recall_threshold: 0.85,
            evaluation_f1_threshold: 0.87,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentRelationship {
    FirstPublication,
    Reprint,
    Summary,
    FollowUp,
    Commentary,
    Correction,
    Retraction,
    DuplicateFetch,
}

impl DocumentRelationship {
    pub fn chinese_name(self) -> &'static str {
        match self {
            Self::FirstPublication => "首发",
            Self::Reprint => "转载",
            Self::Summary => "摘要",
            Self::FollowUp => "跟进",
            Self::Commentary => "评论/解读",
            Self::Correction => "更正",
            Self::Retraction => "撤回",
            Self::DuplicateFetch => "重复抓取",
        }
    }

    fn token(self) -> String {
        serde_json::to_string(&self)
            .unwrap_or_else(|_| "\"follow_up\"".into())
            .trim_matches('"')
            .to_string()
    }

    fn parse(value: &str) -> Option<Self> {
        serde_json::from_str(&format!("\"{value}\"")).ok()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentFingerprint {
    pub canonical_url: String,
    pub normalized_title: String,
    pub tokens: BTreeSet<String>,
    pub simhash: u64,
    pub minhash: Vec<u64>,
    pub semantic_vector: Vec<f32>,
    pub entities: BTreeSet<String>,
    pub action_terms: BTreeSet<String>,
    pub event_time_utc: Option<i64>,
    pub facts: BTreeMap<String, BTreeSet<String>>,
    pub direction: Option<i8>,
    pub content_hash: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SimilarityFeatures {
    pub same_url: bool,
    pub same_content: bool,
    pub title_exact: bool,
    pub simhash_similarity: f64,
    pub minhash_similarity: f64,
    pub semantic_similarity: f64,
    pub entity_overlap: f64,
    pub action_overlap: f64,
    pub time_proximity: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterExplanation {
    pub score: f64,
    pub merge_threshold: f64,
    pub reasons: Vec<String>,
    pub separation_reasons: Vec<String>,
    pub features: SimilarityFeatures,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairDecision {
    pub merge: bool,
    pub relationship: DocumentRelationship,
    pub old_republication: bool,
    pub explanation: ClusterExplanation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventCluster {
    pub cluster_id: String,
    pub canonical_title: String,
    pub event_time_utc: Option<i64>,
    pub first_seen_time_utc: i64,
    pub primary_revision_id: String,
    pub first_source_id: String,
    pub independent_sources: u64,
    pub evidence_diversity: f64,
    pub latest_revision_id: String,
    pub conflict_fields: Vec<String>,
    pub model_version: String,
    pub status: String,
    pub merged_into_cluster_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventClusterMember {
    pub cluster_id: String,
    pub revision_id: String,
    pub relationship: DocumentRelationship,
    pub merge_score: f64,
    pub explanation: ClusterExplanation,
    pub old_republication: bool,
    pub assigned_by: String,
    pub model_version: String,
    pub active: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventFactConflict {
    pub cluster_id: String,
    pub field_name: String,
    pub values: Vec<String>,
    pub authoritative_revision_id: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventClusterDetail {
    pub cluster: EventCluster,
    pub members: Vec<EventClusterMember>,
    pub revisions: Vec<ArchivedNewsRevision>,
    pub conflicts: Vec<EventFactConflict>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterAssignment {
    pub cluster_id: String,
    pub revision_id: String,
    pub relationship: DocumentRelationship,
    pub old_republication: bool,
    pub independent_sources: u64,
    pub explanation: ClusterExplanation,
    pub model_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConclusionReview {
    pub task_id: String,
    pub conclusion_key: String,
    pub triggering_revision: String,
    pub trigger_relation: String,
    pub status: String,
    pub created_at: i64,
    pub reviewed_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabeledDocument {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub url: String,
    pub source_id: String,
    pub event_time_utc: Option<i64>,
    pub first_seen_time_utc: i64,
    pub group: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PairwiseMetrics {
    pub true_positive: u64,
    pub false_positive: u64,
    pub false_negative: u64,
    pub true_negative: u64,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayAssignment {
    pub document_id: String,
    pub event_cluster_id: String,
    pub relationship: DocumentRelationship,
    pub explanation: ClusterExplanation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayResult {
    pub model_version: String,
    pub input_digest: String,
    pub assignments: Vec<ReplayAssignment>,
}

pub struct NewsEventClusterer {
    storage: Storage,
    config: ClusterConfig,
}

impl NewsEventClusterer {
    pub fn new(storage: Storage) -> Self {
        Self {
            storage,
            config: ClusterConfig::default(),
        }
    }

    pub fn with_config(storage: Storage, config: ClusterConfig) -> Result<Self> {
        if !(0.0..=1.0).contains(&config.merge_threshold) || config.model_version.trim().is_empty()
        {
            return Err(Error::Invalid("聚类阈值或模型版本无效".into()));
        }
        Ok(Self { storage, config })
    }

    /// Assign one immutable revision. Existing active membership is returned
    /// unchanged even when the configured model version is newer.
    pub async fn assign_revision(&self, revision_id: &str) -> Result<ClusterAssignment> {
        let _guard = ASSIGN_LOCK.lock().await;
        if let Some(existing) = self.member_for_revision(revision_id).await? {
            let cluster = self.cluster(&existing.cluster_id).await?.ok_or_else(|| {
                Error::Invalid(format!("聚类 {} 缺少主记录", existing.cluster_id))
            })?;
            return Ok(assignment(existing, cluster.independent_sources));
        }
        let revision = self
            .storage
            .news_archive_revision(revision_id)
            .await?
            .ok_or_else(|| Error::Invalid(format!("未知资讯修订 {revision_id}")))?;
        let fingerprint = fingerprint_revision(&revision);
        let candidates = self.storage.news_archive_all_revisions(5_000).await?;
        let assignments = self.active_assignment_map().await?;
        let mut best: Option<(String, PairDecision)> = None;
        for candidate in candidates {
            if candidate.revision_id == revision.revision_id {
                continue;
            }
            let Some(cluster_id) = assignments.get(&candidate.revision_id) else {
                continue;
            };
            let decision = compare_fingerprints(
                &fingerprint,
                &fingerprint_revision(&candidate),
                &revision,
                &candidate,
                &self.config,
            );
            if decision.merge
                && best.as_ref().is_none_or(|(_, previous)| {
                    decision.explanation.score > previous.explanation.score
                })
            {
                best = Some((cluster_id.clone(), decision));
            }
        }
        let (cluster_id, relationship, explanation, old_republication) = match best {
            Some((cluster_id, decision)) => (
                cluster_id,
                classify_relationship(&revision, &fingerprint, Some(decision.relationship)),
                decision.explanation,
                decision.old_republication,
            ),
            None => {
                let cluster_id = cluster_id_for(&revision.revision_id);
                let explanation = ClusterExplanation {
                    score: 1.0,
                    merge_threshold: self.config.merge_threshold,
                    reasons: vec!["未找到达到阈值的既有事件，建立新事件簇".into()],
                    separation_reasons: Vec::new(),
                    features: SimilarityFeatures::default(),
                };
                self.create_cluster(&cluster_id, &revision).await?;
                (
                    cluster_id,
                    DocumentRelationship::FirstPublication,
                    explanation,
                    old_news(&revision, fingerprint.event_time_utc, &self.config),
                )
            }
        };
        let member = EventClusterMember {
            cluster_id: cluster_id.clone(),
            revision_id: revision.revision_id.clone(),
            relationship,
            merge_score: explanation.score,
            explanation: explanation.clone(),
            old_republication,
            assigned_by: "automatic".into(),
            model_version: self.config.model_version.clone(),
            active: true,
            created_at: now_secs(),
        };
        self.insert_member(&member, "assign").await?;
        self.storage
            .news_event_evidence_link(&cluster_id, &revision.revision_id, "supports")
            .await?;
        let detail = self.refresh_cluster(&cluster_id).await?;
        if matches!(
            relationship,
            DocumentRelationship::Correction | DocumentRelationship::Retraction
        ) {
            self.mark_dependent_conclusions(&cluster_id, &revision.revision_id, relationship)
                .await?;
        }
        Ok(ClusterAssignment {
            cluster_id,
            revision_id: revision.revision_id,
            relationship,
            old_republication,
            independent_sources: detail.cluster.independent_sources,
            explanation,
            model_version: self.config.model_version.clone(),
        })
    }

    pub async fn cluster(&self, cluster_id: &str) -> Result<Option<EventCluster>> {
        let cluster_id = cluster_id.to_string();
        self.storage
            .run(move |conn| {
                conn.query_row(
                    "SELECT cluster_id,canonical_title,event_time_utc,
                            first_seen_time_utc,primary_revision_id,first_source_id,
                            independent_sources,evidence_diversity,latest_revision_id,
                            conflict_fields_json,model_version,status,
                            merged_into_cluster_id,created_at,updated_at
                     FROM event_clusters WHERE cluster_id=?1",
                    [cluster_id],
                    map_cluster,
                )
                .optional()
                .map_err(Into::into)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn clusters_recent(&self, limit: usize) -> Result<Vec<EventCluster>> {
        let limit = limit.clamp(1, 10_000) as i64;
        self.storage
            .run(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT cluster_id,canonical_title,event_time_utc,
                            first_seen_time_utc,primary_revision_id,first_source_id,
                            independent_sources,evidence_diversity,latest_revision_id,
                            conflict_fields_json,model_version,status,
                            merged_into_cluster_id,created_at,updated_at
                     FROM event_clusters ORDER BY updated_at DESC LIMIT ?1",
                )?;
                let rows = stmt.query_map([limit], map_cluster)?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn cluster_detail(&self, cluster_id: &str) -> Result<EventClusterDetail> {
        let cluster = self
            .cluster(cluster_id)
            .await?
            .ok_or_else(|| Error::Invalid(format!("未知事件簇 {cluster_id}")))?;
        let members = self.members(cluster_id).await?;
        let mut revisions = Vec::new();
        for member in &members {
            if let Some(revision) = self
                .storage
                .news_archive_revision(&member.revision_id)
                .await?
            {
                revisions.push(revision);
            }
        }
        let conflicts = self.conflicts(cluster_id).await?;
        Ok(EventClusterDetail {
            cluster,
            members,
            revisions,
            conflicts,
        })
    }

    /// Manual merge is append-only: old membership becomes inactive and a
    /// human decision row records the move.
    pub async fn manual_merge(
        &self,
        from_cluster_id: &str,
        to_cluster_id: &str,
        reason: &str,
    ) -> Result<EventClusterDetail> {
        if from_cluster_id == to_cluster_id || reason.trim().is_empty() {
            return Err(Error::Invalid("人工合并目标或理由无效".into()));
        }
        let _guard = ASSIGN_LOCK.lock().await;
        let from = self.cluster_detail(from_cluster_id).await?;
        self.cluster_detail(to_cluster_id).await?;
        let from_id = from_cluster_id.to_string();
        let to_id = to_cluster_id.to_string();
        let reason_json = serde_json::to_string(&manual_explanation(
            reason,
            "用户人工合并事件簇",
            self.config.merge_threshold,
        ))?;
        let version = self.config.model_version.clone();
        let members = from.members;
        self.storage
            .run(move |conn| {
                let tx = conn.transaction()?;
                for member in members {
                    tx.execute(
                        "UPDATE event_cluster_members SET active=0
                         WHERE cluster_id=?1 AND revision_id=?2 AND active=1",
                        rusqlite::params![from_id, member.revision_id],
                    )?;
                    tx.execute(
                        "INSERT INTO event_cluster_members
                         (cluster_id,revision_id,relationship,merge_score,
                          explanation_json,old_republication,assigned_by,
                          model_version,active,created_at)
                         VALUES (?1,?2,?3,1.0,?4,?5,'manual',?6,1,?7)",
                        rusqlite::params![
                            to_id,
                            member.revision_id,
                            member.relationship.token(),
                            reason_json,
                            member.old_republication,
                            version,
                            now_secs(),
                        ],
                    )?;
                }
                tx.execute(
                    "UPDATE event_clusters SET status='merged',merged_into_cluster_id=?2,
                     updated_at=?3 WHERE cluster_id=?1",
                    rusqlite::params![from_id, to_id, now_secs()],
                )?;
                tx.execute(
                    "INSERT INTO event_cluster_decisions
                     (from_cluster_id,to_cluster_id,action,explanation_json,
                      model_version,actor,created_at)
                     VALUES (?1,?2,'manual_merge',?3,?4,'user',?5)",
                    rusqlite::params![from_id, to_id, reason_json, version, now_secs()],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.refresh_cluster(to_cluster_id).await
    }

    pub async fn manual_split(
        &self,
        revision_id: &str,
        reason: &str,
    ) -> Result<EventClusterDetail> {
        if reason.trim().is_empty() {
            return Err(Error::Invalid("人工拆分理由不能为空".into()));
        }
        let _guard = ASSIGN_LOCK.lock().await;
        let old = self
            .member_for_revision(revision_id)
            .await?
            .ok_or_else(|| Error::Invalid("该修订尚未进入事件簇".into()))?;
        let revision = self
            .storage
            .news_archive_revision(revision_id)
            .await?
            .ok_or_else(|| Error::Invalid("资讯修订不存在".into()))?;
        let new_cluster_id = format!("{}-manual-{}", cluster_id_for(revision_id), now_secs());
        self.create_cluster(&new_cluster_id, &revision).await?;
        let reason_json = serde_json::to_string(&manual_explanation(
            reason,
            "用户人工拆分事件",
            self.config.merge_threshold,
        ))?;
        let old_cluster_id = old.cluster_id.clone();
        let revision_id = revision_id.to_string();
        let version = self.config.model_version.clone();
        let target = new_cluster_id.clone();
        self.storage
            .run(move |conn| {
                let tx = conn.transaction()?;
                tx.execute(
                    "UPDATE event_cluster_members SET active=0
                     WHERE revision_id=?1 AND active=1",
                    [&revision_id],
                )?;
                tx.execute(
                    "INSERT INTO event_cluster_members
                     (cluster_id,revision_id,relationship,merge_score,
                      explanation_json,old_republication,assigned_by,
                      model_version,active,created_at)
                     VALUES (?1,?2,'first_publication',1.0,?3,?4,'manual',?5,1,?6)",
                    rusqlite::params![
                        target,
                        revision_id,
                        reason_json,
                        old.old_republication,
                        version,
                        now_secs()
                    ],
                )?;
                tx.execute(
                    "INSERT INTO event_cluster_decisions
                     (revision_id,from_cluster_id,to_cluster_id,action,
                      explanation_json,model_version,actor,created_at)
                     VALUES (?1,?2,?3,'manual_split',?4,?5,'user',?6)",
                    rusqlite::params![
                        revision_id,
                        old_cluster_id,
                        target,
                        reason_json,
                        version,
                        now_secs()
                    ],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.refresh_cluster(&old.cluster_id).await?;
        self.refresh_cluster(&new_cluster_id).await
    }

    pub async fn pending_reviews(&self, limit: usize) -> Result<Vec<AgentConclusionReview>> {
        let limit = limit.clamp(1, 10_000) as i64;
        self.storage
            .run(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT task_id,conclusion_key,triggering_revision,
                            trigger_relation,status,created_at,reviewed_at
                     FROM agent_conclusion_reviews
                     WHERE status='pending_review' ORDER BY created_at DESC LIMIT ?1",
                )?;
                let rows = stmt.query_map([limit], |row| {
                    Ok(AgentConclusionReview {
                        task_id: row.get(0)?,
                        conclusion_key: row.get(1)?,
                        triggering_revision: row.get(2)?,
                        trigger_relation: row.get(3)?,
                        status: row.get(4)?,
                        created_at: row.get(5)?,
                        reviewed_at: row.get(6)?,
                    })
                })?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn resolve_review(
        &self,
        task_id: &str,
        conclusion_key: &str,
        triggering_revision: &str,
    ) -> Result<bool> {
        let task_id = task_id.to_string();
        let conclusion_key = conclusion_key.to_string();
        let triggering_revision = triggering_revision.to_string();
        self.storage
            .run(move |conn| {
                Ok(conn.execute(
                    "UPDATE agent_conclusion_reviews SET status='reviewed',reviewed_at=?4
                     WHERE task_id=?1 AND conclusion_key=?2 AND triggering_revision=?3
                       AND status='pending_review'",
                    rusqlite::params![task_id, conclusion_key, triggering_revision, now_secs()],
                )? > 0)
            })
            .await
            .map_err(Into::into)
    }

    async fn create_cluster(
        &self,
        cluster_id: &str,
        revision: &ArchivedNewsRevision,
    ) -> Result<()> {
        let cluster_id = cluster_id.to_string();
        let revision = revision.clone();
        let version = self.config.model_version.clone();
        self.storage
            .run(move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO event_clusters
                     (cluster_id,canonical_title,event_time_utc,
                      first_seen_time_utc,primary_revision_id,first_source_id,
                      independent_sources,evidence_diversity,latest_revision_id,
                      conflict_fields_json,model_version,status,created_at,updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,1,1.0,?5,'[]',?7,'active',?8,?8)",
                    rusqlite::params![
                        cluster_id,
                        revision.title,
                        revision.event_time.utc.or(revision.publish_time.utc),
                        revision.first_seen_time_utc,
                        revision.revision_id,
                        revision.source_id,
                        version,
                        now_secs(),
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn insert_member(&self, member: &EventClusterMember, action: &str) -> Result<()> {
        let member = member.clone();
        let action = action.to_string();
        self.storage
            .run(move |conn| {
                let tx = conn.transaction()?;
                let explanation = serde_json::to_string(&member.explanation)?;
                tx.execute(
                    "INSERT INTO event_cluster_members
                     (cluster_id,revision_id,relationship,merge_score,
                      explanation_json,old_republication,assigned_by,
                      model_version,active,created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,1,?9)",
                    rusqlite::params![
                        member.cluster_id,
                        member.revision_id,
                        member.relationship.token(),
                        member.merge_score,
                        explanation,
                        member.old_republication,
                        member.assigned_by,
                        member.model_version,
                        member.created_at,
                    ],
                )?;
                tx.execute(
                    "INSERT INTO event_cluster_decisions
                     (revision_id,to_cluster_id,action,explanation_json,
                      model_version,actor,created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    rusqlite::params![
                        member.revision_id,
                        member.cluster_id,
                        action,
                        explanation,
                        member.model_version,
                        member.assigned_by,
                        member.created_at,
                    ],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn refresh_cluster(&self, cluster_id: &str) -> Result<EventClusterDetail> {
        let members = self.members(cluster_id).await?;
        if members.is_empty() {
            let cluster_id = cluster_id.to_string();
            let stored_cluster_id = cluster_id.clone();
            self.storage
                .run(move |conn| {
                    conn.execute(
                        "UPDATE event_clusters SET status='empty',updated_at=?2 WHERE cluster_id=?1",
                        rusqlite::params![stored_cluster_id, now_secs()],
                    )?;
                    Ok(())
                })
                .await?;
            return self.cluster_detail(cluster_id.as_str()).await;
        }
        let mut revisions = Vec::new();
        for member in &members {
            if let Some(revision) = self
                .storage
                .news_archive_revision(&member.revision_id)
                .await?
            {
                revisions.push(revision);
            }
        }
        revisions
            .sort_by_key(|revision| (revision.first_seen_time_utc, revision.revision_time.utc));
        let sources = revisions
            .iter()
            .map(|revision| revision.source_id.clone())
            .collect::<BTreeSet<_>>();
        let first = revisions
            .first()
            .ok_or_else(|| Error::Invalid("事件簇没有修订".into()))?;
        let latest = revisions.last().unwrap_or(first);
        let primary = revisions
            .iter()
            .find(|revision| authority_rank(&revision.source_id) == 0)
            .unwrap_or(first);
        let conflicts = detect_conflicts(cluster_id, &revisions);
        let conflict_names = conflicts
            .iter()
            .map(|conflict| conflict.field_name.clone())
            .collect::<Vec<_>>();
        let diversity = (sources.len() as f64 / revisions.len() as f64).clamp(0.0, 1.0);
        let cluster = cluster_id.to_string();
        let canonical_title = primary.title.clone();
        let event_time = revisions
            .iter()
            .filter_map(|revision| revision.event_time.utc.or(revision.publish_time.utc))
            .min();
        let first_seen = first.first_seen_time_utc;
        let primary_revision = primary.revision_id.clone();
        let first_source = first.source_id.clone();
        let latest_revision = latest.revision_id.clone();
        let independent_sources = sources.len() as i64;
        let conflict_json = serde_json::to_string(&conflict_names)?;
        let conflicts_for_db = conflicts.clone();
        self.storage
            .run(move |conn| {
                let tx = conn.transaction()?;
                tx.execute(
                    "UPDATE event_clusters SET canonical_title=?2,event_time_utc=?3,
                     first_seen_time_utc=?4,primary_revision_id=?5,first_source_id=?6,
                     independent_sources=?7,evidence_diversity=?8,latest_revision_id=?9,
                     conflict_fields_json=?10,updated_at=?11 WHERE cluster_id=?1",
                    rusqlite::params![
                        cluster,
                        canonical_title,
                        event_time,
                        first_seen,
                        primary_revision,
                        first_source,
                        independent_sources,
                        diversity,
                        latest_revision,
                        conflict_json,
                        now_secs(),
                    ],
                )?;
                tx.execute(
                    "DELETE FROM event_fact_conflicts WHERE cluster_id=?1",
                    [&cluster],
                )?;
                for conflict in conflicts_for_db {
                    tx.execute(
                        "INSERT INTO event_fact_conflicts
                         (cluster_id,field_name,values_json,authoritative_revision_id,
                          status,created_at,updated_at)
                         VALUES (?1,?2,?3,?4,?5,?6,?6)",
                        rusqlite::params![
                            conflict.cluster_id,
                            conflict.field_name,
                            serde_json::to_string(&conflict.values)?,
                            conflict.authoritative_revision_id,
                            conflict.status,
                            now_secs(),
                        ],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.cluster_detail_without_refresh(cluster_id).await
    }

    async fn cluster_detail_without_refresh(&self, cluster_id: &str) -> Result<EventClusterDetail> {
        self.cluster_detail(cluster_id).await
    }

    async fn member_for_revision(&self, revision_id: &str) -> Result<Option<EventClusterMember>> {
        let revision_id = revision_id.to_string();
        self.storage
            .run(move |conn| {
                conn.query_row(
                    "SELECT cluster_id,revision_id,relationship,merge_score,
                            explanation_json,old_republication,assigned_by,
                            model_version,active,created_at
                     FROM event_cluster_members WHERE revision_id=?1 AND active=1",
                    [revision_id],
                    map_member,
                )
                .optional()
                .map_err(Into::into)
            })
            .await
            .map_err(Into::into)
    }

    async fn members(&self, cluster_id: &str) -> Result<Vec<EventClusterMember>> {
        let cluster_id = cluster_id.to_string();
        self.storage
            .run(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT cluster_id,revision_id,relationship,merge_score,
                            explanation_json,old_republication,assigned_by,
                            model_version,active,created_at
                     FROM event_cluster_members WHERE cluster_id=?1 AND active=1
                     ORDER BY created_at,rowid",
                )?;
                let rows = stmt.query_map([cluster_id], map_member)?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(Into::into)
    }

    async fn conflicts(&self, cluster_id: &str) -> Result<Vec<EventFactConflict>> {
        let cluster_id = cluster_id.to_string();
        self.storage
            .run(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT cluster_id,field_name,values_json,
                            authoritative_revision_id,status
                     FROM event_fact_conflicts WHERE cluster_id=?1 ORDER BY field_name",
                )?;
                let rows = stmt.query_map([cluster_id], |row| {
                    let values: String = row.get(2)?;
                    Ok(EventFactConflict {
                        cluster_id: row.get(0)?,
                        field_name: row.get(1)?,
                        values: serde_json::from_str(&values).unwrap_or_default(),
                        authoritative_revision_id: row.get(3)?,
                        status: row.get(4)?,
                    })
                })?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(Into::into)
    }

    async fn active_assignment_map(&self) -> Result<HashMap<String, String>> {
        self.storage
            .run(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT revision_id,cluster_id FROM event_cluster_members WHERE active=1",
                )?;
                let rows = stmt.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                Ok(rows
                    .collect::<std::result::Result<Vec<_>, _>>()?
                    .into_iter()
                    .collect())
            })
            .await
            .map_err(Into::into)
    }

    async fn mark_dependent_conclusions(
        &self,
        cluster_id: &str,
        triggering_revision: &str,
        relationship: DocumentRelationship,
    ) -> Result<()> {
        let cluster_id = cluster_id.to_string();
        let triggering_revision = triggering_revision.to_string();
        let relation = relationship.token();
        self.storage
            .run(move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO agent_conclusion_reviews
                     (task_id,conclusion_key,triggering_revision,trigger_relation,
                      status,created_at)
                     SELECT DISTINCT refs.task_id,refs.conclusion_key,?2,?3,
                            'pending_review',?4
                     FROM agent_evidence_refs refs
                     JOIN event_cluster_members members
                       ON members.revision_id=refs.revision_id AND members.active=1
                     WHERE members.cluster_id=?1 AND refs.revision_id<>?2",
                    rusqlite::params![cluster_id, triggering_revision, relation, now_secs()],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }
}

fn assignment(member: EventClusterMember, independent_sources: u64) -> ClusterAssignment {
    ClusterAssignment {
        cluster_id: member.cluster_id,
        revision_id: member.revision_id,
        relationship: member.relationship,
        old_republication: member.old_republication,
        independent_sources,
        explanation: member.explanation,
        model_version: member.model_version,
    }
}

fn manual_explanation(reason: &str, action: &str, threshold: f64) -> ClusterExplanation {
    ClusterExplanation {
        score: 1.0,
        merge_threshold: threshold,
        reasons: vec![format!("{action}：{reason}")],
        separation_reasons: Vec::new(),
        features: SimilarityFeatures::default(),
    }
}

pub fn canonicalize_url(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw.trim()) else {
        return raw.trim().to_string();
    };
    if !matches!(url.scheme(), "http" | "https") {
        return raw.trim().to_string();
    }
    url.set_fragment(None);
    if (url.scheme() == "http" && url.port() == Some(80))
        || (url.scheme() == "https" && url.port() == Some(443))
    {
        let _ = url.set_port(None);
    }
    let tracking = [
        "utm_source",
        "utm_medium",
        "utm_campaign",
        "utm_term",
        "utm_content",
        "spm",
        "from",
        "source",
        "share",
    ];
    let mut pairs = url
        .query_pairs()
        .filter(|(key, _)| !tracking.contains(&key.to_ascii_lowercase().as_str()))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    pairs.sort();
    url.set_query(None);
    if !pairs.is_empty() {
        url.query_pairs_mut().extend_pairs(pairs);
    }
    if url.path().len() > 1 && url.path().ends_with('/') {
        let path = url.path().trim_end_matches('/').to_string();
        url.set_path(&path);
    }
    url.to_string()
}

pub fn normalize_title(value: &str) -> String {
    value
        .replace("【", "")
        .replace("】", "")
        .chars()
        .filter(|character| character.is_alphanumeric() || is_han(*character))
        .flat_map(char::to_lowercase)
        .collect()
}

pub fn fingerprint_revision(revision: &ArchivedNewsRevision) -> DocumentFingerprint {
    fingerprint_text(
        &revision.title,
        &revision.factual_summary,
        &revision.canonical_url,
        revision.event_time.utc.or(revision.publish_time.utc),
        &revision.content_hash,
    )
}

fn fingerprint_text(
    title: &str,
    summary: &str,
    url: &str,
    event_time_utc: Option<i64>,
    content_hash: &str,
) -> DocumentFingerprint {
    let normalized_title = normalize_title(title);
    let combined = format!("{title} {summary}");
    let tokens = tokenize(&combined);
    DocumentFingerprint {
        canonical_url: canonicalize_url(url),
        normalized_title,
        simhash: simhash(&tokens),
        minhash: minhash(&tokens),
        semantic_vector: semantic_vector(&tokens),
        entities: extract_entities(&combined),
        action_terms: extract_action_terms(&combined),
        event_time_utc: event_time_utc.or_else(|| extract_date(&combined)),
        facts: extract_facts(&combined),
        direction: direction(&combined),
        content_hash: content_hash.to_string(),
        tokens,
    }
}

pub fn compare_fingerprints(
    left: &DocumentFingerprint,
    right: &DocumentFingerprint,
    left_revision: &ArchivedNewsRevision,
    right_revision: &ArchivedNewsRevision,
    config: &ClusterConfig,
) -> PairDecision {
    let same_url = !left.canonical_url.is_empty() && left.canonical_url == right.canonical_url;
    let same_content = !left.content_hash.is_empty() && left.content_hash == right.content_hash;
    let title_exact =
        !left.normalized_title.is_empty() && left.normalized_title == right.normalized_title;
    let simhash_similarity = 1.0 - f64::from((left.simhash ^ right.simhash).count_ones()) / 64.0;
    let minhash_similarity = minhash_similarity(&left.minhash, &right.minhash);
    let semantic_similarity = cosine(&left.semantic_vector, &right.semantic_vector);
    let entity_overlap = jaccard(&left.entities, &right.entities);
    let action_overlap = jaccard(&left.action_terms, &right.action_terms);
    let time_gap = match (left.event_time_utc, right.event_time_utc) {
        (Some(left), Some(right)) => Some((left - right).abs()),
        _ => None,
    };
    let time_proximity = time_gap
        .map(|gap| 1.0 - (gap as f64 / (config.max_event_gap_days * 86_400) as f64).min(1.0))
        .unwrap_or(0.5);
    let features = SimilarityFeatures {
        same_url,
        same_content,
        title_exact,
        simhash_similarity,
        minhash_similarity,
        semantic_similarity,
        entity_overlap,
        action_overlap,
        time_proximity,
    };
    let entities_compatible =
        left.entities.is_empty() || right.entities.is_empty() || entity_overlap > 0.0;
    let time_compatible = time_gap.is_none_or(|gap| gap <= config.max_event_gap_days * 86_400)
        || same_url
        || same_content;
    let mut reasons = Vec::new();
    let mut separation_reasons = Vec::new();
    let mut score = if same_url {
        reasons.push("规范网址相同".into());
        0.99
    } else if same_content {
        reasons.push("内容哈希相同".into());
        0.98
    } else if title_exact && entities_compatible && time_compatible {
        reasons.push("标题规范化后完全相同".into());
        0.95
    } else {
        0.16 * simhash_similarity
            + 0.15 * minhash_similarity
            + 0.24 * semantic_similarity
            + 0.18 * entity_overlap
            + 0.20 * action_overlap
            + 0.07 * time_proximity
    };
    if entity_overlap > 0.0 && action_overlap > 0.0 && time_compatible {
        score = score.max(0.82);
        reasons.push("关键主体、事件动作与时间窗口一致".into());
    }
    if simhash_similarity >= 0.82 {
        reasons.push(format!("SimHash 相似度 {:.2}", simhash_similarity));
    }
    if minhash_similarity >= 0.65 {
        reasons.push(format!("MinHash 相似度 {:.2}", minhash_similarity));
    }
    if semantic_similarity >= 0.78 {
        reasons.push(format!("中文语义向量相似度 {:.2}", semantic_similarity));
    }
    if !entities_compatible {
        separation_reasons.push("关键主体完全不同".into());
    }
    if !time_compatible {
        separation_reasons.push("事件时间间隔超过允许窗口".into());
    }
    if score < config.merge_threshold {
        separation_reasons.push(format!(
            "综合分 {:.2} 低于发布阈值 {:.2}",
            score, config.merge_threshold
        ));
    }
    let merge = score >= config.merge_threshold && entities_compatible && time_compatible;
    PairDecision {
        merge,
        relationship: if same_content || title_exact {
            DocumentRelationship::Reprint
        } else {
            DocumentRelationship::FollowUp
        },
        old_republication: old_news(left_revision, left.event_time_utc, config)
            || old_news(right_revision, right.event_time_utc, config),
        explanation: ClusterExplanation {
            score,
            merge_threshold: config.merge_threshold,
            reasons,
            separation_reasons,
            features,
        },
    }
}

pub fn evaluate_pairwise(documents: &[LabeledDocument], config: &ClusterConfig) -> PairwiseMetrics {
    let fingerprints = documents
        .iter()
        .map(|document| {
            fingerprint_text(
                &document.title,
                &document.summary,
                &document.url,
                document.event_time_utc,
                &sha256(document.summary.as_bytes()),
            )
        })
        .collect::<Vec<_>>();
    let revisions = documents.iter().map(labeled_revision).collect::<Vec<_>>();
    let mut metrics = PairwiseMetrics::default();
    for left in 0..documents.len() {
        for right in (left + 1)..documents.len() {
            let expected = documents[left].group == documents[right].group;
            let predicted = compare_fingerprints(
                &fingerprints[left],
                &fingerprints[right],
                &revisions[left],
                &revisions[right],
                config,
            )
            .merge;
            match (expected, predicted) {
                (true, true) => metrics.true_positive += 1,
                (false, true) => metrics.false_positive += 1,
                (true, false) => metrics.false_negative += 1,
                (false, false) => metrics.true_negative += 1,
            }
        }
    }
    metrics.precision = ratio(
        metrics.true_positive,
        metrics.true_positive + metrics.false_positive,
    );
    metrics.recall = ratio(
        metrics.true_positive,
        metrics.true_positive + metrics.false_negative,
    );
    metrics.f1 = if metrics.precision + metrics.recall == 0.0 {
        0.0
    } else {
        2.0 * metrics.precision * metrics.recall / (metrics.precision + metrics.recall)
    };
    metrics.passed = metrics.precision >= config.evaluation_precision_threshold
        && metrics.recall >= config.evaluation_recall_threshold
        && metrics.f1 >= config.evaluation_f1_threshold;
    metrics
}

/// Replays clustering without touching persistent state. Inputs are ordered by
/// first observation time and stable document id, so the same snapshot and
/// model version always produce byte-for-byte equivalent assignments.
pub fn replay_documents(documents: &[LabeledDocument], config: &ClusterConfig) -> ReplayResult {
    let mut ordered = documents.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|document| (document.first_seen_time_utc, document.id.as_str()));
    let input_digest = sha256(
        &serde_json::to_vec(&ordered).expect("serializing a labeled replay fixture cannot fail"),
    );
    let fingerprints = ordered
        .iter()
        .map(|document| {
            fingerprint_text(
                &document.title,
                &document.summary,
                &document.url,
                document.event_time_utc,
                &sha256(document.summary.as_bytes()),
            )
        })
        .collect::<Vec<_>>();
    let revisions = ordered
        .iter()
        .map(|document| labeled_revision(document))
        .collect::<Vec<_>>();
    let mut assignments: Vec<ReplayAssignment> = Vec::with_capacity(ordered.len());

    for current in 0..ordered.len() {
        let mut best: Option<(usize, PairDecision)> = None;
        for previous in 0..current {
            let decision = compare_fingerprints(
                &fingerprints[current],
                &fingerprints[previous],
                &revisions[current],
                &revisions[previous],
                config,
            );
            if decision.merge
                && best.as_ref().is_none_or(|(best_index, best_decision)| {
                    decision.explanation.score > best_decision.explanation.score
                        || (decision.explanation.score == best_decision.explanation.score
                            && ordered[previous].id < ordered[*best_index].id)
                })
            {
                best = Some((previous, decision));
            }
        }

        let (event_cluster_id, relationship, explanation) = match best {
            Some((previous, decision)) => (
                assignments[previous].event_cluster_id.clone(),
                classify_relationship(
                    &revisions[current],
                    &fingerprints[current],
                    Some(decision.relationship),
                ),
                decision.explanation,
            ),
            None => (
                cluster_id_for(&ordered[current].id),
                DocumentRelationship::FirstPublication,
                ClusterExplanation {
                    score: 1.0,
                    merge_threshold: config.merge_threshold,
                    reasons: vec!["离线重放中未找到达到阈值的既有事件，建立新事件簇".into()],
                    separation_reasons: Vec::new(),
                    features: SimilarityFeatures::default(),
                },
            ),
        };
        assignments.push(ReplayAssignment {
            document_id: ordered[current].id.clone(),
            event_cluster_id,
            relationship,
            explanation,
        });
    }

    ReplayResult {
        model_version: config.model_version.clone(),
        input_digest,
        assignments,
    }
}

fn labeled_revision(document: &LabeledDocument) -> ArchivedNewsRevision {
    ArchivedNewsRevision {
        document_id: document.id.clone(),
        canonical_url: document.url.clone(),
        source_id: document.source_id.clone(),
        source_name: document.source_id.clone(),
        license: "fixture".into(),
        content_type: "news".into(),
        language: "zh-CN".into(),
        parser_version: CLUSTER_MODEL_VERSION.into(),
        content_hash: sha256(document.summary.as_bytes()),
        current_revision_id: Some(document.id.clone()),
        document_first_seen_time_utc: document.first_seen_time_utc,
        last_observed_at: document.first_seen_time_utc,
        retention_class: "fixture".into(),
        revision_id: document.id.clone(),
        revision_hash: sha256(document.title.as_bytes()),
        title: document.title.clone(),
        factual_summary: document.summary.clone(),
        supersedes_revision_id: None,
        event_time: EvidenceTimestamp {
            utc: document.event_time_utc,
            original: None,
        },
        publish_time: EvidenceTimestamp {
            utc: Some(document.first_seen_time_utc),
            original: None,
        },
        first_seen_time_utc: document.first_seen_time_utc,
        revision_time: EvidenceTimestamp {
            utc: Some(document.first_seen_time_utc),
            original: None,
        },
        raw_snapshot_hash: None,
    }
}

fn classify_relationship(
    revision: &ArchivedNewsRevision,
    fingerprint: &DocumentFingerprint,
    fallback: Option<DocumentRelationship>,
) -> DocumentRelationship {
    let text = format!("{} {}", revision.title, revision.factual_summary);
    if ["撤回", "作废", "取消原公告"]
        .iter()
        .any(|word| text.contains(word))
    {
        DocumentRelationship::Retraction
    } else if ["更正", "修订", "勘误", "以此为准"]
        .iter()
        .any(|word| text.contains(word))
    {
        DocumentRelationship::Correction
    } else if ["评论", "解读", "点评", "观点"]
        .iter()
        .any(|word| text.contains(word))
    {
        DocumentRelationship::Commentary
    } else if ["摘要", "一图看懂", "要点"]
        .iter()
        .any(|word| text.contains(word))
    {
        DocumentRelationship::Summary
    } else if fingerprint.normalized_title.is_empty() {
        DocumentRelationship::FollowUp
    } else {
        fallback.unwrap_or(DocumentRelationship::FollowUp)
    }
}

fn detect_conflicts(
    cluster_id: &str,
    revisions: &[ArchivedNewsRevision],
) -> Vec<EventFactConflict> {
    let mut values: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    let mut directions: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for revision in revisions {
        let fingerprint = fingerprint_revision(revision);
        for (field, facts) in fingerprint.facts {
            for fact in facts {
                values
                    .entry(field.clone())
                    .or_default()
                    .entry(fact)
                    .or_default()
                    .push(revision.revision_id.clone());
            }
        }
        if let Some(direction) = fingerprint.direction {
            directions
                .entry(direction.to_string())
                .or_default()
                .push(revision.revision_id.clone());
        }
    }
    let mut conflicts = values
        .into_iter()
        .filter(|(_, options)| options.len() > 1)
        .map(|(field, options)| EventFactConflict {
            cluster_id: cluster_id.to_string(),
            field_name: field,
            values: options.keys().cloned().collect(),
            authoritative_revision_id: authoritative_revision(revisions, &options),
            status: "open".into(),
        })
        .collect::<Vec<_>>();
    if directions.len() > 1 {
        conflicts.push(EventFactConflict {
            cluster_id: cluster_id.to_string(),
            field_name: "direction".into(),
            values: directions.keys().cloned().collect(),
            authoritative_revision_id: authoritative_revision(revisions, &directions),
            status: "open".into(),
        });
    }
    conflicts
}

fn authoritative_revision(
    revisions: &[ArchivedNewsRevision],
    options: &BTreeMap<String, Vec<String>>,
) -> Option<String> {
    revisions
        .iter()
        .filter(|revision| {
            options
                .values()
                .any(|ids| ids.contains(&revision.revision_id))
        })
        .min_by_key(|revision| authority_rank(&revision.source_id))
        .map(|revision| revision.revision_id.clone())
}

fn authority_rank(source_id: &str) -> u8 {
    let lower = source_id.to_ascii_lowercase();
    if lower.contains("official") || lower.contains("exchange") || lower.contains("cninfo") {
        0
    } else if lower.contains("licensed") {
        1
    } else {
        2
    }
}

fn old_news(
    revision: &ArchivedNewsRevision,
    event_time: Option<i64>,
    config: &ClusterConfig,
) -> bool {
    event_time.is_some_and(|event| {
        revision.first_seen_time_utc.saturating_sub(event) > config.old_news_days * 86_400
    })
}

fn tokenize(text: &str) -> BTreeSet<String> {
    let compact = text
        .chars()
        .filter(|character| character.is_alphanumeric() || is_han(*character))
        .flat_map(char::to_lowercase)
        .collect::<Vec<_>>();
    let mut tokens = BTreeSet::new();
    for character in &compact {
        tokens.insert(character.to_string());
    }
    for pair in compact.windows(2) {
        tokens.insert(pair.iter().collect());
    }
    tokens
}

fn is_han(character: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&character)
}

fn simhash(tokens: &BTreeSet<String>) -> u64 {
    let mut weights = [0_i32; 64];
    for token in tokens {
        let hash = hash64(token.as_bytes(), 0);
        for (bit, weight) in weights.iter_mut().enumerate() {
            if hash & (1_u64 << bit) == 0 {
                *weight -= 1;
            } else {
                *weight += 1;
            }
        }
    }
    weights
        .iter()
        .enumerate()
        .fold(0_u64, |value, (bit, weight)| {
            value | ((*weight >= 0) as u64) << bit
        })
}

fn minhash(tokens: &BTreeSet<String>) -> Vec<u64> {
    (0..MINHASH_PERMUTATIONS)
        .map(|seed| {
            tokens
                .iter()
                .map(|token| hash64(token.as_bytes(), seed as u64 + 1))
                .min()
                .unwrap_or_default()
        })
        .collect()
}

fn semantic_vector(tokens: &BTreeSet<String>) -> Vec<f32> {
    let mut vector = vec![0.0_f32; VECTOR_DIMENSIONS];
    for token in tokens {
        let hash = hash64(token.as_bytes(), 97);
        let index = hash as usize % VECTOR_DIMENSIONS;
        vector[index] += if hash & (1 << 63) == 0 { 1.0 } else { -1.0 };
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

fn minhash_similarity(left: &[u64], right: &[u64]) -> f64 {
    let total = left.len().min(right.len());
    if total == 0 {
        return 0.0;
    }
    let equal = left
        .iter()
        .zip(right)
        .take(total)
        .filter(|(left, right)| left == right)
        .count();
    equal as f64 / total as f64
}

fn cosine(left: &[f32], right: &[f32]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left, right)| f64::from(left * right))
        .sum::<f64>()
        .clamp(-1.0, 1.0)
        .max(0.0)
}

fn jaccard<T: Ord>(left: &BTreeSet<T>, right: &BTreeSet<T>) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    let union = left.union(right).count();
    if union == 0 {
        0.0
    } else {
        left.intersection(right).count() as f64 / union as f64
    }
}

fn extract_entities(text: &str) -> BTreeSet<String> {
    STOCK_CODE
        .find_iter(text)
        .map(|value| value.as_str().to_ascii_uppercase())
        .chain(
            ENTITY
                .find_iter(text)
                .map(|value| value.as_str().to_string()),
        )
        .collect()
}

fn extract_date(text: &str) -> Option<i64> {
    let captures = DATE.captures(text)?;
    let year = captures.name("y")?.as_str().parse().ok()?;
    let month = captures.name("m")?.as_str().parse().ok()?;
    let day = captures.name("d")?.as_str().parse().ok()?;
    chrono::NaiveDate::from_ymd_opt(year, month, day)?
        .and_hms_opt(0, 0, 0)?
        .and_utc()
        .timestamp()
        .into()
}

fn extract_facts(text: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut facts: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for capture in FACT.captures_iter(text) {
        let Some(key) = capture.name("key") else {
            continue;
        };
        let Some(value) = capture.name("value") else {
            continue;
        };
        let matched = capture
            .get(0)
            .map(|value| value.as_str())
            .unwrap_or_default();
        let mut normalized = value.as_str().to_string();
        if !normalized.starts_with(['+', '-']) {
            if ["下降", "减少", "下调", "亏损", "减持"]
                .iter()
                .any(|word| matched.contains(word))
            {
                normalized.insert(0, '-');
            } else if ["增长", "增加", "上调", "盈利", "增持"]
                .iter()
                .any(|word| matched.contains(word))
            {
                normalized.insert(0, '+');
            }
        }
        facts
            .entry(key.as_str().to_string())
            .or_default()
            .insert(normalized);
    }
    facts
}

fn extract_action_terms(text: &str) -> BTreeSet<String> {
    [
        "回购",
        "业绩",
        "净利润",
        "营收",
        "减持",
        "增持",
        "中标",
        "处罚",
        "分红",
        "停牌",
        "复牌",
        "并购",
        "收购",
        "事故",
        "政策",
        "临床",
        "诉讼",
        "解禁",
        "定增",
        "重组",
        "破产",
        "撤回",
    ]
    .iter()
    .filter(|term| text.contains(**term))
    .map(|term| (*term).to_string())
    .collect()
}

fn direction(text: &str) -> Option<i8> {
    let positive = ["增长", "增持", "上调", "中标", "回购", "盈利", "利好"]
        .iter()
        .any(|word| text.contains(word));
    let negative = ["下降", "减持", "下调", "亏损", "处罚", "终止", "利空"]
        .iter()
        .any(|word| text.contains(word));
    match (positive, negative) {
        (true, false) => Some(1),
        (false, true) => Some(-1),
        _ => None,
    }
}

fn cluster_id_for(revision_id: &str) -> String {
    format!("evt:{}", &sha256(revision_id.as_bytes())[..24])
}

fn hash64(value: &[u8], seed: u64) -> u64 {
    let mut digest = Sha256::new();
    digest.update(seed.to_le_bytes());
    digest.update(value);
    let bytes = digest.finalize();
    u64::from_le_bytes(bytes[..8].try_into().unwrap_or_default())
}

fn sha256(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn map_cluster(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventCluster> {
    let conflicts: String = row.get(9)?;
    Ok(EventCluster {
        cluster_id: row.get(0)?,
        canonical_title: row.get(1)?,
        event_time_utc: row.get(2)?,
        first_seen_time_utc: row.get(3)?,
        primary_revision_id: row.get(4)?,
        first_source_id: row.get(5)?,
        independent_sources: row.get::<_, i64>(6)? as u64,
        evidence_diversity: row.get(7)?,
        latest_revision_id: row.get(8)?,
        conflict_fields: serde_json::from_str(&conflicts).unwrap_or_default(),
        model_version: row.get(10)?,
        status: row.get(11)?,
        merged_into_cluster_id: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

fn map_member(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventClusterMember> {
    let relationship: String = row.get(2)?;
    let explanation: String = row.get(4)?;
    Ok(EventClusterMember {
        cluster_id: row.get(0)?,
        revision_id: row.get(1)?,
        relationship: DocumentRelationship::parse(&relationship)
            .unwrap_or(DocumentRelationship::FollowUp),
        merge_score: row.get(3)?,
        explanation: serde_json::from_str(&explanation).unwrap_or(ClusterExplanation {
            score: 0.0,
            merge_threshold: 0.0,
            reasons: vec!["历史人工决策".into()],
            separation_reasons: Vec::new(),
            features: SimilarityFeatures::default(),
        }),
        old_republication: row.get(5)?,
        assigned_by: row.get(6)?,
        model_version: row.get(7)?,
        active: row.get(8)?,
        created_at: row.get(9)?,
    })
}

trait OptionalRow<T> {
    fn optional(self) -> rusqlite::Result<Option<T>>;
}

impl<T> OptionalRow<T> for rusqlite::Result<T> {
    fn optional(self) -> rusqlite::Result<Option<T>> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_storage::{
        EvidenceTimestamp, NewsArchiveInput, NewsObservationInput, StorageConfig,
    };

    fn storage() -> (tempfile::TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        (dir, storage)
    }

    fn archive_input(
        url: &str,
        source: &str,
        title: &str,
        summary: &str,
        event: i64,
        seen: i64,
    ) -> NewsArchiveInput {
        NewsArchiveInput {
            canonical_url: canonicalize_url(url),
            source_id: source.into(),
            source_name: source.into(),
            license: "fixture".into(),
            content_type: "news".into(),
            language: "zh-CN".into(),
            parser_version: "fixture-v1".into(),
            title: title.into(),
            factual_summary: summary.into(),
            raw_snapshot: None,
            raw_snapshot_permitted: false,
            event_time: EvidenceTimestamp {
                utc: Some(event),
                original: None,
            },
            publish_time: EvidenceTimestamp {
                utc: Some(seen - 60),
                original: None,
            },
            first_seen_time_utc: seen,
            revision_time: EvidenceTimestamp {
                utc: Some(seen),
                original: None,
            },
            retention_class: "fixture".into(),
            observation: {
                let mut observation = NewsObservationInput::success(source, url);
                observation.fetched_at = seen;
                observation
            },
        }
    }

    #[test]
    fn canonical_url_and_fingerprints_are_deterministic() {
        assert_eq!(
            canonicalize_url("HTTPS://Example.com/a/?utm_source=x&b=2&a=1#top"),
            "https://example.com/a?a=1&b=2"
        );
        let tokens = tokenize("紫金矿业601899拟回购股份");
        assert_eq!(simhash(&tokens), simhash(&tokens));
        assert_eq!(minhash(&tokens), minhash(&tokens));
        assert_eq!(semantic_vector(&tokens), semantic_vector(&tokens));
    }

    #[test]
    fn annotated_pairwise_fixture_clears_release_thresholds() {
        let docs: Vec<LabeledDocument> =
            serde_json::from_str(include_str!("../tests/fixtures/event_clusters.json")).unwrap();
        let metrics = evaluate_pairwise(&docs, &ClusterConfig::default());
        assert!(metrics.passed, "metrics={metrics:?}");
        assert!(metrics.precision >= 0.90);
        assert!(metrics.recall >= 0.85);
        assert!(metrics.f1 >= 0.87);
    }

    #[test]
    fn offline_replay_is_deterministic_and_explains_separation() {
        let docs: Vec<LabeledDocument> =
            serde_json::from_str(include_str!("../tests/fixtures/event_clusters.json")).unwrap();
        let config = ClusterConfig::default();
        let first = replay_documents(&docs, &config);
        let second = replay_documents(&docs, &config);
        assert_eq!(first, second);
        assert_eq!(first.model_version, CLUSTER_MODEL_VERSION);

        let left = docs.iter().find(|document| document.id == "c1").unwrap();
        let right = docs.iter().find(|document| document.id == "d1").unwrap();
        let left_revision = labeled_revision(left);
        let right_revision = labeled_revision(right);
        let decision = compare_fingerprints(
            &fingerprint_revision(&left_revision),
            &fingerprint_revision(&right_revision),
            &left_revision,
            &right_revision,
            &config,
        );
        assert!(!decision.merge);
        assert!(!decision.explanation.separation_reasons.is_empty());
    }

    #[tokio::test]
    async fn reprints_cluster_once_and_expose_explanation_and_old_news() {
        let (_dir, storage) = storage();
        let event = 1_700_000_000;
        let official = storage
            .news_archive_upsert(archive_input(
                "https://official.example/buyback",
                "official",
                "紫金矿业拟回购股份",
                "紫金矿业601899拟回购不超过10亿元股份",
                event,
                event + 60,
            ))
            .await
            .unwrap();
        let media = storage
            .news_archive_upsert(archive_input(
                "https://media.example/reprint?utm_source=app",
                "licensed",
                "紫金矿业：拟斥资不超10亿元回购",
                "601899公告拟回购公司股份",
                event,
                event + 120,
            ))
            .await
            .unwrap();
        let clusterer = NewsEventClusterer::new(storage.clone());
        let first = clusterer
            .assign_revision(&official.revision_id)
            .await
            .unwrap();
        let second = clusterer.assign_revision(&media.revision_id).await.unwrap();
        assert_eq!(first.cluster_id, second.cluster_id);
        assert_eq!(second.independent_sources, 2);
        assert!(!second.explanation.reasons.is_empty());

        let old = storage
            .news_archive_upsert(archive_input(
                "https://another.example/old",
                "public",
                "紫金矿业回购旧闻回顾",
                "601899此前拟回购公司股份",
                event,
                event + 10 * 86_400,
            ))
            .await
            .unwrap();
        let assigned = clusterer.assign_revision(&old.revision_id).await.unwrap();
        assert!(assigned.old_republication);
    }

    #[tokio::test]
    async fn correction_marks_old_agent_conclusion_pending_review() {
        let (_dir, storage) = storage();
        let event = 1_700_000_000;
        let first = storage
            .news_archive_upsert(archive_input(
                "https://official.example/result",
                "official",
                "某公司净利润增长20%",
                "某公司600000净利润增长20%",
                event,
                event + 60,
            ))
            .await
            .unwrap();
        let clusterer = NewsEventClusterer::new(storage.clone());
        let assigned = clusterer.assign_revision(&first.revision_id).await.unwrap();
        storage
            .news_agent_evidence_link("task", "final_answer", &first.revision_id)
            .await
            .unwrap();

        let correction = storage
            .news_archive_upsert(archive_input(
                "https://official.example/result",
                "official",
                "更正公告：某公司净利润下降20%",
                "以此为准，某公司600000净利润下降20%",
                event,
                event + 120,
            ))
            .await
            .unwrap();
        let corrected = clusterer
            .assign_revision(&correction.revision_id)
            .await
            .unwrap();
        assert_eq!(corrected.cluster_id, assigned.cluster_id);
        assert_eq!(corrected.relationship, DocumentRelationship::Correction);
        let reviews = clusterer.pending_reviews(10).await.unwrap();
        assert_eq!(reviews.len(), 1);
        let detail = clusterer
            .cluster_detail(&assigned.cluster_id)
            .await
            .unwrap();
        assert!(detail.cluster.conflict_fields.contains(&"净利润".into()));
    }

    #[tokio::test]
    async fn model_upgrade_does_not_silently_rewrite_and_manual_split_is_audited() {
        let (_dir, storage) = storage();
        let event = 1_700_000_000;
        let saved = storage
            .news_archive_upsert(archive_input(
                "https://official.example/a",
                "official",
                "公司公告",
                "某公司600000发布公告",
                event,
                event + 60,
            ))
            .await
            .unwrap();
        let v1 = NewsEventClusterer::new(storage.clone());
        let assigned = v1.assign_revision(&saved.revision_id).await.unwrap();
        let config = ClusterConfig {
            model_version: "zh-fin-event-v2".into(),
            ..ClusterConfig::default()
        };
        let v2 = NewsEventClusterer::with_config(storage.clone(), config).unwrap();
        let unchanged = v2.assign_revision(&saved.revision_id).await.unwrap();
        assert_eq!(unchanged.cluster_id, assigned.cluster_id);
        assert_eq!(unchanged.model_version, CLUSTER_MODEL_VERSION);

        let split = v2
            .manual_split(&saved.revision_id, "人工确认属于独立事件")
            .await
            .unwrap();
        assert_ne!(split.cluster.cluster_id, assigned.cluster_id);
        v2.manual_merge(
            &split.cluster.cluster_id,
            &assigned.cluster_id,
            "复核证据后重新归并",
        )
        .await
        .unwrap();
        let split_again = v2
            .manual_split(&saved.revision_id, "第二次人工拆分验证审计链")
            .await
            .unwrap();
        v2.manual_merge(
            &split_again.cluster.cluster_id,
            &assigned.cluster_id,
            "第二次复核后重新归并",
        )
        .await
        .unwrap();
        let decisions: i64 = storage
            .run(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM event_cluster_decisions WHERE action='manual_split'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .await
            .unwrap();
        assert_eq!(decisions, 2);
    }
}
