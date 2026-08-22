//! Versioned, explainable entity linking for Chinese financial documents.
//!
//! Candidate recall is deterministic. Codes and formal names win, contextual
//! rules disambiguate aliases, and model proposals are always review-only.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use astock_storage::Storage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::OnceCell;

pub const LINKER_VERSION: &str = "zh-fin-entity-v1";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Storage(#[from] astock_storage::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("实体链接输入无效：{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    LegalEntity,
    ListedSecurity,
    Subsidiary,
    Brand,
    Person,
    Product,
    Industry,
    Commodity,
    Region,
    Policy,
}

impl EntityKind {
    pub fn chinese_name(self) -> &'static str {
        match self {
            Self::LegalEntity => "法人主体",
            Self::ListedSecurity => "上市证券",
            Self::Subsidiary => "子公司",
            Self::Brand => "品牌",
            Self::Person => "人物",
            Self::Product => "产品",
            Self::Industry => "行业",
            Self::Commodity => "商品",
            Self::Region => "地区",
            Self::Policy => "政策",
        }
    }

    fn token(self) -> String {
        serde_json::to_string(&self)
            .unwrap_or_else(|_| "\"legal_entity\"".into())
            .trim_matches('"')
            .to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityName {
    pub text: String,
    #[serde(rename = "kind")]
    pub name_type: String,
    pub valid_from: Option<i64>,
    pub valid_to: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchEntity {
    pub entity_id: String,
    pub kind: EntityKind,
    pub canonical_name: String,
    pub listed_code: Option<String>,
    pub market: Option<String>,
    pub parent_entity_id: Option<String>,
    pub names: Vec<EntityName>,
    pub context_terms: Vec<String>,
    pub source_name: String,
    pub source_url: Option<String>,
    pub valid_from: Option<i64>,
    pub valid_to: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityRelation {
    pub from_entity_id: String,
    pub to_entity_id: String,
    pub relation_type: String,
    pub confidence: f64,
    pub source_name: String,
    pub source_url: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelatedListedEntity {
    pub entity_id: String,
    pub code: String,
    pub name: String,
    pub relation_path: Vec<String>,
    pub confidence: f64,
    pub eligible_for_agent: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkCandidate {
    pub entity_id: String,
    pub canonical_name: String,
    pub entity_kind: EntityKind,
    pub listed_code: Option<String>,
    pub matched_name_type: String,
    pub score: f64,
    pub reasons: Vec<String>,
    pub related_listed: Vec<RelatedListedEntity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkStatus {
    Accepted,
    PendingReview,
    Rejected,
}

impl LinkStatus {
    fn token(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::PendingReview => "pending_review",
            Self::Rejected => "rejected",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "accepted" => Self::Accepted,
            "pending_review" => Self::PendingReview,
            "rejected" => Self::Rejected,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentEntityLink {
    pub link_id: String,
    pub revision_id: String,
    pub span_start: usize,
    pub span_end: usize,
    pub span_text: String,
    pub candidates: Vec<LinkCandidate>,
    pub final_entity_id: Option<String>,
    pub final_entity_name: Option<String>,
    pub final_entity_kind: Option<EntityKind>,
    pub listed_code: Option<String>,
    pub confidence: f64,
    pub reasons: Vec<String>,
    pub linker_version: String,
    pub evidence_revision_id: String,
    pub status: LinkStatus,
    pub proposed_by_model: bool,
    pub eligible_for_agent: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityLinkSummary {
    pub entity_id: String,
    pub entity_name: String,
    pub entity_kind: EntityKind,
    pub entity_kind_name: String,
    pub listed_code: Option<String>,
    pub span_text: String,
    pub confidence: f64,
    pub reasons: Vec<String>,
    pub evidence_revision_id: String,
    pub related_listed: Vec<RelatedListedEntity>,
}

impl DocumentEntityLink {
    pub fn agent_summary(&self) -> Option<EntityLinkSummary> {
        if !self.eligible_for_agent {
            return None;
        }
        let candidate = self
            .candidates
            .iter()
            .find(|candidate| Some(&candidate.entity_id) == self.final_entity_id.as_ref())?;
        Some(EntityLinkSummary {
            entity_id: candidate.entity_id.clone(),
            entity_name: candidate.canonical_name.clone(),
            entity_kind: candidate.entity_kind,
            entity_kind_name: candidate.entity_kind.chinese_name().into(),
            listed_code: candidate.listed_code.clone(),
            span_text: self.span_text.clone(),
            confidence: self.confidence,
            reasons: self.reasons.clone(),
            evidence_revision_id: self.evidence_revision_id.clone(),
            related_listed: candidate
                .related_listed
                .iter()
                .filter(|related| related.eligible_for_agent)
                .cloned()
                .collect(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityLinkReview {
    pub review_id: i64,
    pub link: DocumentEntityLink,
    pub proposed_entity_id: Option<String>,
    pub decision: String,
    pub reason: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkerConfig {
    pub linker_version: String,
    pub acceptance_threshold: f64,
    pub relation_threshold: f64,
}

impl Default for LinkerConfig {
    fn default() -> Self {
        Self {
            linker_version: LINKER_VERSION.into(),
            acceptance_threshold: 0.85,
            relation_threshold: 0.85,
        }
    }
}

#[derive(Debug, Clone)]
struct IndexedName {
    entity_id: String,
    normalized: String,
    name: EntityName,
}

type SpanMatches<'a> = ((usize, usize, String), Vec<&'a IndexedName>);

struct LinkDraft<'a> {
    revision_id: &'a str,
    span_start: usize,
    span_end: usize,
    span_text: &'a str,
    candidates: Vec<LinkCandidate>,
    confidence: f64,
    status: LinkStatus,
    proposed_by_model: bool,
}

#[derive(Debug, Clone, Default)]
struct EntityIndex {
    entities: BTreeMap<String, ResearchEntity>,
    names: Vec<IndexedName>,
    relations: Vec<EntityRelation>,
}

pub struct EntityLinker {
    storage: Storage,
    config: LinkerConfig,
    index: OnceCell<EntityIndex>,
}

impl EntityLinker {
    pub fn new(storage: Storage) -> Self {
        Self {
            storage,
            config: LinkerConfig::default(),
            index: OnceCell::new(),
        }
    }

    pub fn with_config(storage: Storage, config: LinkerConfig) -> Result<Self> {
        if config.linker_version.trim().is_empty()
            || !(0.0..=1.0).contains(&config.acceptance_threshold)
            || !(0.0..=1.0).contains(&config.relation_threshold)
        {
            return Err(Error::Invalid("链接器版本或阈值无效".into()));
        }
        Ok(Self {
            storage,
            config,
            index: OnceCell::new(),
        })
    }

    async fn index(&self) -> Result<&EntityIndex> {
        self.index
            .get_or_try_init(|| async {
                let mut index = seed_index()?;
                merge_security_master(&mut index, self.storage.securities_list().await?);
                merge_graph_nodes(&mut index, self.storage.graph_nodes_all().await?);
                rebuild_names(&mut index);
                persist_master(&self.storage, &index).await?;
                Ok(index)
            })
            .await
    }

    pub async fn link_revision(&self, revision_id: &str) -> Result<Vec<DocumentEntityLink>> {
        let existing = self.links_for_revision(revision_id).await?;
        if !existing.is_empty() {
            return Ok(existing);
        }
        let revision = self
            .storage
            .news_archive_revision(revision_id)
            .await?
            .ok_or_else(|| Error::Invalid(format!("未知资讯修订 {revision_id}")))?;
        let index = self.index().await?;
        let text = format!("{}\n{}", revision.title, revision.factual_summary);
        let event_time = revision.event_time.utc.or(revision.publish_time.utc);
        let links = link_text(index, revision_id, &text, event_time, &self.config, false);
        persist_links(&self.storage, &links).await?;
        Ok(links)
    }

    /// A model may suggest a candidate, but the persisted confidence is
    /// capped below the automatic threshold and always enters review.
    pub async fn propose_model_candidate(
        &self,
        revision_id: &str,
        span_start: usize,
        span_end: usize,
        entity_id: &str,
    ) -> Result<DocumentEntityLink> {
        let revision = self
            .storage
            .news_archive_revision(revision_id)
            .await?
            .ok_or_else(|| Error::Invalid("资讯修订不存在".into()))?;
        let text = format!("{}\n{}", revision.title, revision.factual_summary);
        let span = text
            .get(span_start..span_end)
            .ok_or_else(|| Error::Invalid("模型候选 span 不是有效 UTF-8 边界".into()))?;
        let index = self.index().await?;
        let entity = index
            .entities
            .get(entity_id)
            .ok_or_else(|| Error::Invalid("模型候选实体不存在于主数据".into()))?;
        let candidate = candidate_for(
            index,
            entity,
            &EntityName {
                text: span.into(),
                name_type: "model_candidate".into(),
                valid_from: None,
                valid_to: None,
            },
            &text,
            revision.event_time.utc.or(revision.publish_time.utc),
            1,
            &self.config,
        );
        let confidence = candidate.score.min(self.config.acceptance_threshold - 0.01);
        let link = make_link(
            LinkDraft {
                revision_id,
                span_start,
                span_end,
                span_text: span,
                candidates: vec![candidate],
                confidence,
                status: LinkStatus::PendingReview,
                proposed_by_model: true,
            },
            &self.config,
        );
        persist_links(&self.storage, std::slice::from_ref(&link)).await?;
        Ok(link)
    }

    pub async fn resolve_query(&self, query: &str) -> Result<Vec<String>> {
        let normalized = normalize_name(query);
        if normalized.is_empty() {
            return Ok(Vec::new());
        }
        let mut ids = self
            .index()
            .await?
            .names
            .iter()
            .filter(|name| name.normalized == normalized)
            .map(|name| name.entity_id.clone())
            .collect::<BTreeSet<_>>();
        if let Some(code) = six_digit_code(query) {
            ids.extend(
                self.index()
                    .await?
                    .entities
                    .values()
                    .filter(|entity| entity.listed_code.as_deref() == Some(code.as_str()))
                    .map(|entity| entity.entity_id.clone()),
            );
        }
        Ok(ids.into_iter().collect())
    }

    pub async fn links_for_revision(&self, revision_id: &str) -> Result<Vec<DocumentEntityLink>> {
        self.links_for_revisions(&[revision_id.to_string()]).await
    }

    pub async fn links_for_revisions(
        &self,
        revision_ids: &[String],
    ) -> Result<Vec<DocumentEntityLink>> {
        let ids = revision_ids.to_vec();
        let version = self.config.linker_version.clone();
        self.storage
            .run(move |conn| {
                let mut rows_out = Vec::new();
                let mut stmt = conn.prepare(
                    "SELECT link_id,revision_id,span_start,span_end,span_text,
                            candidates_json,final_entity_id,confidence,
                            explanation_json,linker_version,evidence_revision_id,
                            status,proposed_by_model
                     FROM document_entity_links
                     WHERE revision_id=?1 AND linker_version=?2
                     ORDER BY span_start,span_end,link_id",
                )?;
                for revision_id in ids {
                    let rows = stmt.query_map(rusqlite::params![revision_id, version], map_link)?;
                    rows_out.extend(rows.collect::<std::result::Result<Vec<_>, _>>()?);
                }
                Ok(rows_out)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn pending_reviews(&self, limit: usize) -> Result<Vec<EntityLinkReview>> {
        let limit = limit.clamp(1, 1_000) as i64;
        self.storage
            .run(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT r.review_id,l.link_id,l.revision_id,l.span_start,l.span_end,
                            l.span_text,l.candidates_json,l.final_entity_id,l.confidence,
                            l.explanation_json,l.linker_version,l.evidence_revision_id,
                            l.status,l.proposed_by_model,r.proposed_entity_id,r.decision,
                            r.reason,r.created_at
                     FROM entity_link_reviews r
                     JOIN document_entity_links l ON l.link_id=r.link_id
                     WHERE r.decision='pending' ORDER BY r.created_at DESC LIMIT ?1",
                )?;
                let rows = stmt.query_map([limit], |row| {
                    let link = map_link_offset(row, 1)?;
                    Ok(EntityLinkReview {
                        review_id: row.get(0)?,
                        link,
                        proposed_entity_id: row.get(14)?,
                        decision: row.get(15)?,
                        reason: row.get(16)?,
                        created_at: row.get(17)?,
                    })
                })?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn resolve_review(
        &self,
        link_id: &str,
        entity_id: Option<&str>,
        accept: bool,
        reason: &str,
    ) -> Result<bool> {
        if reason.trim().is_empty() {
            return Err(Error::Invalid("人工审核理由不能为空".into()));
        }
        if accept {
            let entity_id = entity_id.ok_or_else(|| Error::Invalid("通过时必须选择实体".into()))?;
            if !self.index().await?.entities.contains_key(entity_id) {
                return Err(Error::Invalid("所选实体不存在".into()));
            }
            let link = self
                .storage
                .run({
                    let link_id = link_id.to_string();
                    move |conn| {
                        conn.query_row(
                            "SELECT candidates_json FROM document_entity_links WHERE link_id=?1",
                            [link_id],
                            |row| row.get::<_, String>(0),
                        )
                        .map_err(Into::into)
                    }
                })
                .await?;
            let candidates: Vec<LinkCandidate> = serde_json::from_str(&link)?;
            if !candidates
                .iter()
                .any(|candidate| candidate.entity_id == entity_id)
            {
                return Err(Error::Invalid("所选实体不在该 span 的候选列表中".into()));
            }
        }
        let link_id = link_id.to_string();
        let entity_id = entity_id.map(str::to_string);
        let reason = reason.trim().to_string();
        let threshold = self.config.acceptance_threshold;
        self.storage
            .run(move |conn| {
                let tx = conn.transaction()?;
                let changed = if accept {
                    tx.execute(
                        "UPDATE document_entity_links
                         SET final_entity_id=?2,status='accepted',confidence=MAX(confidence,?3),
                             proposed_by_model=0
                         WHERE link_id=?1",
                        rusqlite::params![link_id, entity_id, threshold],
                    )?
                } else {
                    tx.execute(
                        "UPDATE document_entity_links
                         SET final_entity_id=NULL,status='rejected' WHERE link_id=?1",
                        [&link_id],
                    )?
                };
                tx.execute(
                    "UPDATE entity_link_reviews SET decision=?2,reason=?3,
                     reviewer='user',reviewed_at=?4 WHERE link_id=?1",
                    rusqlite::params![
                        link_id,
                        if accept { "accepted" } else { "rejected" },
                        reason,
                        now_secs(),
                    ],
                )?;
                tx.commit()?;
                Ok(changed > 0)
            })
            .await
            .map_err(Into::into)
    }
}

fn seed_index() -> Result<EntityIndex> {
    let seed: SeedFile = serde_json::from_str(include_str!("../data/entity_seed.json"))?;
    let mut index = EntityIndex::default();
    for row in seed.entities {
        index.entities.insert(
            row.id.clone(),
            ResearchEntity {
                entity_id: row.id,
                kind: row.kind,
                canonical_name: row.name,
                listed_code: row.code,
                market: row.market,
                parent_entity_id: row.parent,
                names: row.aliases,
                context_terms: row.context,
                source_name: seed.source_name.clone(),
                source_url: Some(seed.source_url.clone()),
                valid_from: None,
                valid_to: None,
            },
        );
    }
    index.relations = seed
        .relations
        .into_iter()
        .map(|relation| EntityRelation {
            from_entity_id: relation.from,
            to_entity_id: relation.to,
            relation_type: relation.relation_type,
            confidence: relation.confidence,
            source_name: relation.source,
            source_url: Some(seed.source_url.clone()),
            status: "accepted".into(),
        })
        .collect();
    rebuild_names(&mut index);
    Ok(index)
}

fn merge_security_master(index: &mut EntityIndex, records: Vec<astock_core::SecurityMasterRecord>) {
    for record in records {
        if record.code.len() != 6 || record.canonical_name.trim().is_empty() {
            continue;
        }
        let entity_id = format!("listed:cn:{}", record.code);
        let entry = index
            .entities
            .entry(entity_id.clone())
            .or_insert_with(|| ResearchEntity {
                entity_id,
                kind: EntityKind::ListedSecurity,
                canonical_name: record.canonical_name.clone(),
                listed_code: Some(record.code.clone()),
                market: Some(record.market.to_string()),
                parent_entity_id: None,
                names: Vec::new(),
                context_terms: Vec::new(),
                source_name: record.source.clone(),
                source_url: record.source_url.clone(),
                valid_from: record.valid_from.map(|time| time.timestamp()),
                valid_to: record.valid_to.map(|time| time.timestamp()),
            });
        entry.canonical_name = record.canonical_name.clone();
        push_name(&mut entry.names, &record.code, "security_code", None, None);
        push_name(
            &mut entry.names,
            &record.canonical_name,
            "short_name",
            record.valid_from.map(|time| time.timestamp()),
            record.valid_to.map(|time| time.timestamp()),
        );
        for alias in record.aliases {
            push_name(&mut entry.names, &alias, "alias", None, None);
        }
        if let Some(industry) = record.industry {
            entry.context_terms.push(industry);
        }
        entry.context_terms.extend(record.concepts);
    }
}

fn merge_graph_nodes(index: &mut EntityIndex, nodes: Vec<astock_storage::GraphNodeRow>) {
    for node in nodes {
        if node.kind == "company" {
            continue;
        }
        let Some(kind) = graph_kind(&node.kind) else {
            continue;
        };
        index
            .entities
            .entry(node.id.clone())
            .or_insert_with(|| ResearchEntity {
                entity_id: node.id,
                kind,
                canonical_name: node.name.clone(),
                listed_code: node.code,
                market: None,
                parent_entity_id: None,
                names: vec![EntityName {
                    text: node.name,
                    name_type: "graph_name".into(),
                    valid_from: None,
                    valid_to: None,
                }],
                context_terms: Vec::new(),
                source_name: "本地产链图谱".into(),
                source_url: None,
                valid_from: None,
                valid_to: None,
            });
    }
}

fn graph_kind(kind: &str) -> Option<EntityKind> {
    Some(match kind {
        "product" | "segment" | "material" => EntityKind::Product,
        "commodity" => EntityKind::Commodity,
        "industry" => EntityKind::Industry,
        "region" => EntityKind::Region,
        "policy" => EntityKind::Policy,
        _ => return None,
    })
}

fn push_name(
    names: &mut Vec<EntityName>,
    text: &str,
    name_type: &str,
    valid_from: Option<i64>,
    valid_to: Option<i64>,
) {
    if text.trim().is_empty()
        || names
            .iter()
            .any(|name| normalize_name(&name.text) == normalize_name(text))
    {
        return;
    }
    names.push(EntityName {
        text: text.trim().into(),
        name_type: name_type.into(),
        valid_from,
        valid_to,
    });
}

fn rebuild_names(index: &mut EntityIndex) {
    index.names.clear();
    for entity in index.entities.values_mut() {
        let canonical_name = entity.canonical_name.clone();
        push_name(
            &mut entity.names,
            &canonical_name,
            "canonical",
            entity.valid_from,
            entity.valid_to,
        );
        if let Some(code) = entity.listed_code.clone() {
            push_name(
                &mut entity.names,
                &code,
                "security_code",
                entity.valid_from,
                entity.valid_to,
            );
        }
        for name in &entity.names {
            let normalized = normalize_name(&name.text);
            if !normalized.is_empty() {
                index.names.push(IndexedName {
                    entity_id: entity.entity_id.clone(),
                    normalized,
                    name: name.clone(),
                });
            }
        }
    }
    index.names.sort_by(|left, right| {
        right
            .normalized
            .len()
            .cmp(&left.normalized.len())
            .then_with(|| left.entity_id.cmp(&right.entity_id))
    });
}

fn link_text(
    index: &EntityIndex,
    revision_id: &str,
    text: &str,
    event_time: Option<i64>,
    config: &LinkerConfig,
    proposed_by_model: bool,
) -> Vec<DocumentEntityLink> {
    let haystack = text.to_lowercase();
    let mut found: BTreeMap<(usize, usize, String), Vec<&IndexedName>> = BTreeMap::new();
    for indexed in &index.names {
        let needle = indexed.name.text.to_lowercase();
        if needle.chars().count() == 1
            && index
                .entities
                .get(&indexed.entity_id)
                .is_none_or(|entity| context_hits(entity, &haystack) == 0)
        {
            continue;
        }
        for (start, matched) in haystack.match_indices(&needle) {
            let end = start + matched.len();
            let mention = text.get(start..end).unwrap_or(matched).to_string();
            found
                .entry((start, end, mention))
                .or_default()
                .push(indexed);
        }
    }

    // Prefer the longest overlapping mention (e.g. 长城汽车 over 长城).
    let mut spans = found.into_iter().collect::<Vec<_>>();
    spans.sort_by(|left, right| {
        left.0
             .0
            .cmp(&right.0 .0)
            .then_with(|| (right.0 .1 - right.0 .0).cmp(&(left.0 .1 - left.0 .0)))
    });
    let mut kept: Vec<SpanMatches<'_>> = Vec::new();
    for row in spans {
        if kept.iter().any(|existing| {
            row.0 .0 < existing.0 .1
                && row.0 .1 > existing.0 .0
                && (row.0 .1 - row.0 .0) < (existing.0 .1 - existing.0 .0)
        }) {
            continue;
        }
        kept.push(row);
    }

    kept.into_iter()
        .filter_map(|((start, end, mention), indexed_names)| {
            let mut by_entity: BTreeMap<String, &IndexedName> = BTreeMap::new();
            for indexed in indexed_names {
                by_entity
                    .entry(indexed.entity_id.clone())
                    .or_insert(indexed);
            }
            let ambiguity = by_entity.len();
            let mut candidates = by_entity
                .into_values()
                .filter_map(|indexed| {
                    index.entities.get(&indexed.entity_id).map(|entity| {
                        candidate_for(
                            index,
                            entity,
                            &indexed.name,
                            text,
                            event_time,
                            ambiguity,
                            config,
                        )
                    })
                })
                .collect::<Vec<_>>();
            candidates.sort_by(|left, right| {
                right
                    .score
                    .total_cmp(&left.score)
                    .then_with(|| left.entity_id.cmp(&right.entity_id))
            });
            let best = candidates.first()?;
            let margin = candidates
                .get(1)
                .map(|next| best.score - next.score)
                .unwrap_or(1.0);
            let confidence = if ambiguity > 1 && margin < 0.08 {
                best.score.min(0.69)
            } else {
                best.score
            };
            let status = if !proposed_by_model && confidence >= config.acceptance_threshold {
                LinkStatus::Accepted
            } else {
                LinkStatus::PendingReview
            };
            Some(make_link(
                LinkDraft {
                    revision_id,
                    span_start: start,
                    span_end: end,
                    span_text: &mention,
                    candidates,
                    confidence,
                    status,
                    proposed_by_model,
                },
                config,
            ))
        })
        .collect()
}

fn candidate_for(
    index: &EntityIndex,
    entity: &ResearchEntity,
    name: &EntityName,
    text: &str,
    event_time: Option<i64>,
    ambiguity: usize,
    config: &LinkerConfig,
) -> LinkCandidate {
    let mut reasons = Vec::new();
    let mut score: f64 = match name.name_type.as_str() {
        "security_code" => {
            reasons.push("证券代码精确命中".into());
            1.0
        }
        "legal_name" => {
            reasons.push("法定全称精确命中".into());
            0.98
        }
        "canonical" | "short_name" => {
            reasons.push("主数据名称或正式简称命中".into());
            0.90
        }
        "former_name" => {
            reasons.push("证券历史名称命中".into());
            0.90
        }
        "english_name" => {
            reasons.push("已登记英文名命中".into());
            0.88
        }
        "brand_name" => {
            reasons.push("已登记品牌名命中".into());
            0.86
        }
        "model_candidate" => {
            reasons.push("模型仅提出候选，必须人工审核".into());
            0.60
        }
        _ => {
            reasons.push("已登记别名命中".into());
            0.86
        }
    };
    if !valid_at(name.valid_from, name.valid_to, event_time) {
        score = score.min(0.54);
        reasons.push("名称在文档事件时间已不处于有效期".into());
    }
    let hits = context_hits(entity, &text.to_lowercase());
    if hits > 0 {
        score = (score + 0.08 * hits.min(2) as f64).min(1.0);
        reasons.push(format!("上下文命中 {hits} 个主体特征词"));
    } else if ambiguity > 1 {
        score = score.min(0.65);
        reasons.push("该名称对应多个主体，正文缺少可消歧上下文".into());
    }
    if entity.parent_entity_id.is_some() {
        reasons.push("该主体是独立子公司，不自动等同于母公司".into());
    }
    LinkCandidate {
        entity_id: entity.entity_id.clone(),
        canonical_name: entity.canonical_name.clone(),
        entity_kind: entity.kind,
        listed_code: entity.listed_code.clone(),
        matched_name_type: name.name_type.clone(),
        score,
        reasons,
        related_listed: related_listed(index, &entity.entity_id, config.relation_threshold),
    }
}

fn related_listed(
    index: &EntityIndex,
    start: &str,
    relation_threshold: f64,
) -> Vec<RelatedListedEntity> {
    let mut results: BTreeMap<String, RelatedListedEntity> = BTreeMap::new();
    let mut visited = BTreeSet::from([start.to_string()]);
    let mut queue = VecDeque::from([(start.to_string(), Vec::<String>::new(), 1.0_f64, 0usize)]);
    while let Some((current, path, confidence, depth)) = queue.pop_front() {
        if depth >= 2 {
            continue;
        }
        for relation in index.relations.iter().filter(|relation| {
            relation.status == "accepted"
                && (relation.from_entity_id == current || relation.to_entity_id == current)
        }) {
            let next = if relation.from_entity_id == current {
                &relation.to_entity_id
            } else {
                &relation.from_entity_id
            };
            let mut next_path = path.clone();
            next_path.push(relation.relation_type.clone());
            let next_confidence = confidence.min(relation.confidence);
            if let Some(entity) = index.entities.get(next) {
                if let Some(code) = &entity.listed_code {
                    results
                        .entry(entity.entity_id.clone())
                        .or_insert(RelatedListedEntity {
                            entity_id: entity.entity_id.clone(),
                            code: code.clone(),
                            name: entity.canonical_name.clone(),
                            relation_path: next_path.clone(),
                            confidence: next_confidence,
                            eligible_for_agent: next_confidence >= relation_threshold,
                        });
                }
            }
            if visited.insert(next.clone()) {
                queue.push_back((next.clone(), next_path, next_confidence, depth + 1));
            }
        }
    }
    results.into_values().collect()
}

fn make_link(draft: LinkDraft<'_>, config: &LinkerConfig) -> DocumentEntityLink {
    let LinkDraft {
        revision_id,
        span_start,
        span_end,
        span_text,
        candidates,
        confidence,
        status,
        proposed_by_model,
    } = draft;
    let best = candidates.first();
    let final_entity_id = best.map(|candidate| candidate.entity_id.clone());
    let reasons = best
        .map(|candidate| candidate.reasons.clone())
        .unwrap_or_default();
    let eligible_for_agent = status == LinkStatus::Accepted
        && confidence >= config.acceptance_threshold
        && !proposed_by_model
        && !revision_id.trim().is_empty();
    let link_id = format!(
        "el:{}",
        &sha256(
            format!(
                "{}|{}|{}|{}|{}|{}",
                revision_id,
                span_start,
                span_end,
                final_entity_id.as_deref().unwrap_or(""),
                config.linker_version,
                proposed_by_model
            )
            .as_bytes()
        )[..24]
    );
    DocumentEntityLink {
        link_id,
        revision_id: revision_id.into(),
        span_start,
        span_end,
        span_text: span_text.into(),
        final_entity_id,
        final_entity_name: best.map(|candidate| candidate.canonical_name.clone()),
        final_entity_kind: best.map(|candidate| candidate.entity_kind),
        listed_code: best.and_then(|candidate| candidate.listed_code.clone()),
        confidence,
        candidates,
        reasons,
        linker_version: config.linker_version.clone(),
        evidence_revision_id: revision_id.into(),
        status,
        proposed_by_model,
        eligible_for_agent,
    }
}

async fn persist_master(storage: &Storage, index: &EntityIndex) -> Result<()> {
    let entities = index.entities.values().cloned().collect::<Vec<_>>();
    let relations = index.relations.clone();
    storage
        .run(move |conn| {
            let tx = conn.transaction()?;
            for entity in entities {
                let now = now_secs();
                tx.execute(
                    "INSERT INTO research_entities
                     (entity_id,entity_type,canonical_name,listed_code,market,
                      parent_entity_id,source_name,source_url,valid_from,valid_to,
                      metadata_json,created_at,updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?12)
                     ON CONFLICT(entity_id) DO UPDATE SET
                       canonical_name=excluded.canonical_name,listed_code=excluded.listed_code,
                       market=excluded.market,parent_entity_id=excluded.parent_entity_id,
                       source_name=excluded.source_name,source_url=excluded.source_url,
                       valid_from=excluded.valid_from,valid_to=excluded.valid_to,
                       metadata_json=excluded.metadata_json,updated_at=excluded.updated_at",
                    rusqlite::params![
                        entity.entity_id,
                        entity.kind.token(),
                        entity.canonical_name,
                        entity.listed_code,
                        entity.market,
                        entity.parent_entity_id,
                        entity.source_name,
                        entity.source_url,
                        entity.valid_from,
                        entity.valid_to,
                        serde_json::to_string(&entity.context_terms)?,
                        now,
                    ],
                )?;
                for name in entity.names {
                    tx.execute(
                        "INSERT OR IGNORE INTO research_entity_names
                         (entity_id,name_text,normalized_name,name_type,valid_from,
                          valid_to,source_name,source_url)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                        rusqlite::params![
                            entity.entity_id,
                            name.text,
                            normalize_name(&name.text),
                            name.name_type,
                            name.valid_from,
                            name.valid_to,
                            entity.source_name,
                            entity.source_url,
                        ],
                    )?;
                }
            }
            for relation in relations {
                tx.execute(
                    "INSERT OR IGNORE INTO research_entity_relations
                     (from_entity_id,to_entity_id,relation_type,confidence,
                      source_name,source_url,status,created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    rusqlite::params![
                        relation.from_entity_id,
                        relation.to_entity_id,
                        relation.relation_type,
                        relation.confidence,
                        relation.source_name,
                        relation.source_url,
                        relation.status,
                        now_secs(),
                    ],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(())
}

async fn persist_links(storage: &Storage, links: &[DocumentEntityLink]) -> Result<()> {
    let links = links.to_vec();
    storage
        .run(move |conn| {
            let tx = conn.transaction()?;
            for link in links {
                tx.execute(
                    "INSERT OR IGNORE INTO document_entity_links
                     (link_id,revision_id,span_start,span_end,span_text,candidates_json,
                      final_entity_id,confidence,explanation_json,linker_version,
                      evidence_revision_id,status,proposed_by_model,created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                    rusqlite::params![
                        link.link_id,
                        link.revision_id,
                        link.span_start as i64,
                        link.span_end as i64,
                        link.span_text,
                        serde_json::to_string(&link.candidates)?,
                        link.final_entity_id,
                        link.confidence,
                        serde_json::to_string(&link.reasons)?,
                        link.linker_version,
                        link.evidence_revision_id,
                        link.status.token(),
                        link.proposed_by_model,
                        now_secs(),
                    ],
                )?;
                if link.status == LinkStatus::PendingReview {
                    tx.execute(
                        "INSERT OR IGNORE INTO entity_link_reviews
                         (link_id,proposed_entity_id,decision,created_at)
                         VALUES (?1,?2,'pending',?3)",
                        rusqlite::params![link.link_id, link.final_entity_id, now_secs()],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(())
}

fn map_link(row: &rusqlite::Row<'_>) -> rusqlite::Result<DocumentEntityLink> {
    map_link_offset(row, 0)
}

fn map_link_offset(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<DocumentEntityLink> {
    let candidates_json: String = row.get(offset + 5)?;
    let reasons_json: String = row.get(offset + 8)?;
    let candidates: Vec<LinkCandidate> = serde_json::from_str(&candidates_json).unwrap_or_default();
    let final_entity_id: Option<String> = row.get(offset + 6)?;
    let status_text: String = row.get(offset + 11)?;
    let status = LinkStatus::parse(&status_text).unwrap_or(LinkStatus::PendingReview);
    let confidence: f64 = row.get(offset + 7)?;
    let proposed_by_model: bool = row.get(offset + 12)?;
    let final_candidate = candidates
        .iter()
        .find(|candidate| Some(&candidate.entity_id) == final_entity_id.as_ref());
    let final_entity_name = final_candidate.map(|candidate| candidate.canonical_name.clone());
    let final_entity_kind = final_candidate.map(|candidate| candidate.entity_kind);
    let listed_code = final_candidate.and_then(|candidate| candidate.listed_code.clone());
    Ok(DocumentEntityLink {
        link_id: row.get(offset)?,
        revision_id: row.get(offset + 1)?,
        span_start: row.get::<_, i64>(offset + 2)? as usize,
        span_end: row.get::<_, i64>(offset + 3)? as usize,
        span_text: row.get(offset + 4)?,
        candidates,
        final_entity_id,
        final_entity_name,
        final_entity_kind,
        listed_code,
        confidence,
        reasons: serde_json::from_str(&reasons_json).unwrap_or_default(),
        linker_version: row.get(offset + 9)?,
        evidence_revision_id: row.get(offset + 10)?,
        status,
        proposed_by_model,
        eligible_for_agent: status == LinkStatus::Accepted
            && confidence >= LinkerConfig::default().acceptance_threshold
            && !proposed_by_model,
    })
}

fn normalize_name(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn context_hits(entity: &ResearchEntity, text: &str) -> usize {
    entity
        .context_terms
        .iter()
        .filter(|term| !term.is_empty() && text.contains(&term.to_lowercase()))
        .count()
}

fn valid_at(from: Option<i64>, to: Option<i64>, at: Option<i64>) -> bool {
    let Some(at) = at else {
        return from.is_none() && to.is_none();
    };
    from.is_none_or(|from| at >= from) && to.is_none_or(|to| at <= to)
}

fn six_digit_code(value: &str) -> Option<String> {
    value
        .split(|character: char| !character.is_ascii_digit())
        .find(|part| part.len() == 6)
        .map(str::to_string)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Debug, Deserialize)]
struct SeedFile {
    source_name: String,
    source_url: String,
    entities: Vec<SeedEntity>,
    relations: Vec<SeedRelation>,
}

#[derive(Debug, Deserialize)]
struct SeedEntity {
    id: String,
    kind: EntityKind,
    name: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    market: Option<String>,
    #[serde(default)]
    parent: Option<String>,
    #[serde(default)]
    aliases: Vec<EntityName>,
    #[serde(default)]
    context: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SeedRelation {
    from: String,
    to: String,
    #[serde(rename = "type")]
    relation_type: String,
    confidence: f64,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabeledEntityDocument {
    pub id: String,
    pub text: String,
    pub event_time_utc: Option<i64>,
    pub expected: Vec<String>,
    pub kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EntityMetrics {
    pub true_positive: u64,
    pub false_positive: u64,
    pub false_negative: u64,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EntityEvaluation {
    pub overall: EntityMetrics,
    pub by_type: BTreeMap<String, EntityMetrics>,
    pub passed: bool,
}

pub fn evaluate_fixture(documents: &[LabeledEntityDocument]) -> Result<EntityEvaluation> {
    let index = seed_index()?;
    let config = LinkerConfig::default();
    let mut evaluation = EntityEvaluation::default();
    for document in documents {
        let predicted = link_text(
            &index,
            &document.id,
            &document.text,
            document.event_time_utc,
            &config,
            false,
        )
        .into_iter()
        .filter(|link| link.eligible_for_agent)
        .filter_map(|link| link.final_entity_id)
        .collect::<BTreeSet<_>>();
        let expected = document.expected.iter().cloned().collect::<BTreeSet<_>>();
        add_counts(&mut evaluation.overall, &predicted, &expected);
        add_counts(
            evaluation.by_type.entry(document.kind.clone()).or_default(),
            &predicted,
            &expected,
        );
    }
    finalize_metrics(&mut evaluation.overall);
    for metrics in evaluation.by_type.values_mut() {
        finalize_metrics(metrics);
    }
    evaluation.passed = evaluation.overall.precision >= 0.90
        && evaluation.overall.recall >= 0.85
        && evaluation.overall.f1 >= 0.87;
    Ok(evaluation)
}

fn add_counts(
    metrics: &mut EntityMetrics,
    predicted: &BTreeSet<String>,
    expected: &BTreeSet<String>,
) {
    metrics.true_positive += predicted.intersection(expected).count() as u64;
    metrics.false_positive += predicted.difference(expected).count() as u64;
    metrics.false_negative += expected.difference(predicted).count() as u64;
}

fn finalize_metrics(metrics: &mut EntityMetrics) {
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
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_storage::{
        EvidenceTimestamp, NewsArchiveInput, NewsObservationInput, StorageConfig,
    };

    fn fixture() -> Vec<LabeledEntityDocument> {
        serde_json::from_str(include_str!("../tests/fixtures/entity_linking.json")).unwrap()
    }

    #[test]
    fn benchmark_reports_overall_and_per_type_metrics() {
        let evaluation = evaluate_fixture(&fixture()).unwrap();
        assert!(evaluation.passed, "evaluation={evaluation:?}");
        assert_eq!(evaluation.overall.precision, 1.0);
        assert_eq!(evaluation.overall.recall, 1.0);
        assert_eq!(evaluation.overall.f1, 1.0);
        assert!(evaluation.by_type.contains_key("listed_security"));
        assert!(evaluation.by_type.contains_key("brand"));
        assert!(evaluation.by_type.contains_key("industry"));
    }

    #[test]
    fn seed_covers_representative_a_shares_aliases_brands_and_subsidiaries() {
        let index = seed_index().unwrap();
        assert!(
            index
                .entities
                .values()
                .filter(|entity| entity.kind == EntityKind::ListedSecurity)
                .count()
                >= 12
        );
        assert!(index
            .entities
            .values()
            .any(|entity| entity.kind == EntityKind::Subsidiary));
        assert!(index
            .entities
            .values()
            .any(|entity| entity.kind == EntityKind::Brand));
        for code in ["600519", "300750", "002594", "601899", "688981", "300308"] {
            assert!(index.entities.contains_key(&format!("listed:cn:{code}")));
        }
    }

    #[test]
    fn ambiguous_short_name_uses_context_and_former_name_uses_event_time() {
        let index = seed_index().unwrap();
        let config = LinkerConfig::default();
        let auto = link_text(
            &index,
            "auto",
            "长城汽车新能源SUV销量增长",
            Some(1_800_000_000),
            &config,
            false,
        );
        assert!(auto.iter().any(|link| {
            link.final_entity_id.as_deref() == Some("listed:cn:601633") && link.eligible_for_agent
        }));
        let wine = link_text(
            &index,
            "wine",
            "长城葡萄酒酒庄推出新品",
            Some(1_800_000_000),
            &config,
            false,
        );
        assert!(wine.iter().any(|link| {
            link.final_entity_id.as_deref() == Some("listed:cn:600084") && link.eligible_for_agent
        }));
        let expired = link_text(
            &index,
            "old-name",
            "江南嘉捷披露网络安全业务",
            Some(1_800_000_000),
            &config,
            false,
        );
        assert!(expired.iter().all(|link| !link.eligible_for_agent));
    }

    #[tokio::test]
    async fn links_persist_with_span_candidates_evidence_and_review_gate() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let mut observation = NewsObservationInput::success("fixture", "https://example/entity");
        observation.fetched_at = 1_800_000_100;
        let archived = storage
            .news_archive_upsert(NewsArchiveInput {
                canonical_url: "https://example/entity".into(),
                source_id: "fixture".into(),
                source_name: "fixture".into(),
                license: "fixture".into(),
                content_type: "news".into(),
                language: "zh-CN".into(),
                parser_version: "fixture".into(),
                title: "腾势新车型交付量创新高".into(),
                factual_summary: "比亚迪旗下新能源汽车品牌发布数据".into(),
                raw_snapshot: None,
                raw_snapshot_permitted: false,
                event_time: EvidenceTimestamp {
                    utc: Some(1_800_000_000),
                    original: None,
                },
                publish_time: EvidenceTimestamp {
                    utc: Some(1_800_000_060),
                    original: None,
                },
                first_seen_time_utc: 1_800_000_100,
                revision_time: EvidenceTimestamp {
                    utc: Some(1_800_000_100),
                    original: None,
                },
                retention_class: "fixture".into(),
                observation,
            })
            .await
            .unwrap();
        let linker = EntityLinker::new(storage.clone());
        let links = linker.link_revision(&archived.revision_id).await.unwrap();
        let brand = links
            .iter()
            .find(|link| link.final_entity_id.as_deref() == Some("brand:denza"))
            .unwrap();
        assert_eq!(brand.span_text, "腾势");
        assert_eq!(brand.evidence_revision_id, archived.revision_id);
        assert!(brand.eligible_for_agent);
        assert!(brand
            .candidates
            .first()
            .unwrap()
            .related_listed
            .iter()
            .any(|related| related.code == "002594"));

        let model = linker
            .propose_model_candidate(&archived.revision_id, 0, "腾势".len(), "brand:denza")
            .await
            .unwrap();
        assert_eq!(model.status, LinkStatus::PendingReview);
        assert!(!model.eligible_for_agent);
        assert!(!linker.pending_reviews(10).await.unwrap().is_empty());
        assert!(linker
            .resolve_review(
                &model.link_id,
                Some("brand:denza"),
                true,
                "人工核对公司披露后确认",
            )
            .await
            .unwrap());
        let reviewed = linker
            .links_for_revision(&archived.revision_id)
            .await
            .unwrap();
        assert!(reviewed
            .iter()
            .any(|link| link.link_id == model.link_id && link.eligible_for_agent));
    }
}
