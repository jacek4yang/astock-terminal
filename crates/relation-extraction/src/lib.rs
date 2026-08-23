//! Auditable supply-chain relation extraction.
//!
//! Language models may propose structured candidates, but cannot publish a
//! relation. Every candidate is re-bound to immutable source text, resolved
//! through the entity master (including listed parents), checked by
//! deterministic rules and queued for human review. Publication is versioned,
//! idempotent and retractable; model upgrades create new runs instead of
//! silently replacing accepted evidence.

use std::collections::BTreeSet;

use astock_graph::{Edge, GraphStore, Node, NodeKind, Relation};
use astock_source_verification::{SourceDocumentDetail, SourceSegment, SourceVerifier};
use astock_storage::Storage;
use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const RELATION_SCHEMA_VERSION: &str = "supply-chain-relation-v1";
pub const DETERMINISTIC_EXTRACTOR_VERSION: &str = "zh-disclosure-rules-v1";
pub const AGENT_CONFIDENCE_THRESHOLD_BPS: u16 = 8_500;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Storage(#[from] astock_storage::Error),
    #[error(transparent)]
    Source(#[from] astock_source_verification::Error),
    #[error(transparent)]
    Graph(#[from] astock_graph::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("关系抽取输入无效：{0}")]
    Invalid(String),
    #[error("记录不存在：{0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    AnnualReport,
    SemiAnnualReport,
    Prospectus,
    InvestorRelations,
    ProductManual,
    Tender,
    MajorContract,
    Patent,
    RegulatoryApproval,
    CapacityEia,
    CustomsIndustry,
    Other,
}

impl DocumentKind {
    fn token(self) -> &'static str {
        serde_token(self)
    }

    pub fn parse(value: &str) -> Self {
        serde_json::from_value(serde_json::Value::String(value.into())).unwrap_or(Self::Other)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationType {
    Supplies,
    CustomerOf,
    Produces,
    Consumes,
    WonBid,
    ContractWith,
    PatentFor,
    ApprovedFor,
    CapacityFor,
}

impl RelationType {
    fn token(self) -> &'static str {
        serde_token(self)
    }

    pub fn parse(value: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(value.into())).ok()
    }

    pub fn chinese_name(self) -> &'static str {
        match self {
            Self::Supplies => "供应",
            Self::CustomerOf => "客户",
            Self::Produces => "生产",
            Self::Consumes => "采购/消耗",
            Self::WonBid => "中标",
            Self::ContractWith => "签约",
            Self::PatentFor => "专利涉及",
            Self::ApprovedFor => "获批用于",
            Self::CapacityFor => "产能对应",
        }
    }

    fn graph_relation(self) -> Relation {
        match self {
            Self::CustomerOf => Relation::CustomerOf,
            Self::Produces | Self::PatentFor | Self::ApprovedFor | Self::CapacityFor => {
                Relation::Produces
            }
            Self::Consumes => Relation::Consumes,
            Self::Supplies | Self::WonBid | Self::ContractWith => Relation::Supplies,
        }
    }
}

fn serde_token<T: Serialize>(value: T) -> &'static str {
    // Only called for the closed enums above. Keeping the token match local
    // avoids leaking display labels into persistent identifiers.
    let encoded = serde_json::to_string(&value).unwrap_or_default();
    match encoded.as_str() {
        "\"annual_report\"" => "annual_report",
        "\"semi_annual_report\"" => "semi_annual_report",
        "\"prospectus\"" => "prospectus",
        "\"investor_relations\"" => "investor_relations",
        "\"product_manual\"" => "product_manual",
        "\"tender\"" => "tender",
        "\"major_contract\"" => "major_contract",
        "\"patent\"" => "patent",
        "\"regulatory_approval\"" => "regulatory_approval",
        "\"capacity_eia\"" => "capacity_eia",
        "\"customs_industry\"" => "customs_industry",
        "\"supplies\"" => "supplies",
        "\"customer_of\"" => "customer_of",
        "\"produces\"" => "produces",
        "\"consumes\"" => "consumes",
        "\"won_bid\"" => "won_bid",
        "\"contract_with\"" => "contract_with",
        "\"patent_for\"" => "patent_for",
        "\"approved_for\"" => "approved_for",
        "\"capacity_for\"" => "capacity_for",
        _ => "other",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateEvidenceInput {
    pub segment_id: String,
    pub span_start: usize,
    pub span_end: usize,
    pub quote_original: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRelationCandidate {
    pub subject_text: String,
    pub object_text: String,
    pub relation: RelationType,
    pub product_text: Option<String>,
    pub amount_text: Option<String>,
    pub share_bps: Option<u16>,
    pub report_period: Option<String>,
    pub region: Option<String>,
    pub evidence: CandidateEvidenceInput,
    pub confidence_bps: u16,
    #[serde(default)]
    pub consortium_members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationEvidence {
    pub evidence_id: String,
    pub source_version_id: String,
    pub segment_id: String,
    pub page_number: Option<u32>,
    pub paragraph_index: usize,
    pub span_start: usize,
    pub span_end: usize,
    pub quote_original: String,
    pub independent_group: String,
    pub polarity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationCheck {
    pub field: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationCandidate {
    pub candidate_id: String,
    pub run_id: String,
    pub source_version_id: String,
    pub document_kind: DocumentKind,
    pub subject_text: String,
    pub object_text: String,
    pub relation: RelationType,
    pub product_text: Option<String>,
    pub amount_text: Option<String>,
    pub share_bps: Option<u16>,
    pub report_period: Option<String>,
    pub region: Option<String>,
    pub subject_entity_id: Option<String>,
    pub object_entity_id: Option<String>,
    pub subject_parent_entity_id: Option<String>,
    pub object_parent_entity_id: Option<String>,
    pub disclosure_mode: String,
    pub confidence_bps: u16,
    pub validation_status: String,
    pub validation: Vec<ValidationCheck>,
    pub review_status: String,
    pub confidential: bool,
    pub non_inferable: bool,
    pub candidate_version: u32,
    pub proposed_by_model: bool,
    pub publication_status: Option<String>,
    pub eligible_for_agent: bool,
    pub evidence: Vec<RelationEvidence>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionRun {
    pub run_id: String,
    pub source_version_id: String,
    pub document_kind: DocumentKind,
    pub extractor_kind: String,
    pub model_id: Option<String>,
    pub model_version: Option<String>,
    pub schema_version: String,
    pub input_hash: String,
    pub status: String,
    pub candidate_count: usize,
    pub validation_errors: usize,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionRunDetail {
    pub run: ExtractionRun,
    pub source_title: Option<String>,
    pub source_url: String,
    pub candidates: Vec<RelationCandidate>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewPage {
    pub items: Vec<RelationCandidate>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
    pub total_pages: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationReviewRequest {
    pub candidate_id: String,
    pub decision: String,
    pub reviewer: String,
    pub reason: String,
    pub subject_text: Option<String>,
    pub object_text: Option<String>,
    pub relation: Option<RelationType>,
    pub product_text: Option<String>,
    pub merged_entity_id: Option<String>,
    pub confidential: bool,
    pub non_inferable: bool,
    pub publish: bool,
    pub dataset_split: Option<String>,
    pub training_eligible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationResult {
    pub candidate_id: String,
    pub publication_id: Option<String>,
    pub projection_key: Option<String>,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone)]
struct EntityRecord {
    entity_id: String,
    entity_type: String,
    canonical_name: String,
    listed_code: Option<String>,
    parent_entity_id: Option<String>,
    names: Vec<String>,
}

#[derive(Debug, Clone)]
struct CandidateDraft {
    model: ModelRelationCandidate,
    proposed_by_model: bool,
    disclosure_mode: String,
}

#[derive(Clone)]
pub struct RelationExtractionStore {
    storage: Storage,
    graph: GraphStore,
}

impl RelationExtractionStore {
    pub fn new(storage: Storage) -> Self {
        Self {
            graph: GraphStore::new(storage.clone()),
            storage,
        }
    }

    pub async fn extract_source(
        &self,
        source_version_id: &str,
        document_kind: DocumentKind,
        model_id: Option<&str>,
        model_version: Option<&str>,
        model_candidates: Vec<ModelRelationCandidate>,
    ) -> Result<ExtractionRunDetail> {
        if source_version_id.trim().is_empty() {
            return Err(Error::Invalid("source_version_id 不能为空".into()));
        }
        let detail = SourceVerifier::new(self.storage.clone())
            .read_document(source_version_id)
            .await?;
        if detail.document.access_status != "verified" || detail.version.is_none() {
            return Err(Error::Invalid(
                "来源正文尚未核验，不能从搜索摘要或访问失败页面抽取关系".into(),
            ));
        }
        if detail.segments.is_empty() {
            return Err(Error::Invalid("来源没有可定位的正文段落".into()));
        }
        let input_hash = extraction_hash(&detail, &model_candidates)?;
        let extractor_kind = if model_candidates.is_empty() {
            DETERMINISTIC_EXTRACTOR_VERSION
        } else {
            "model_candidates_plus_rules"
        };
        if let Some(run_id) = self
            .find_existing_run(
                source_version_id,
                extractor_kind,
                model_id,
                model_version,
                &input_hash,
            )
            .await?
        {
            return self.run_detail(&run_id).await;
        }

        let now = now_secs();
        let run_id = format!(
            "relrun:{}",
            &hash(&format!(
                "{source_version_id}|{extractor_kind}|{input_hash}|{now}"
            ))[..28]
        );
        self.insert_run(&ExtractionRun {
            run_id: run_id.clone(),
            source_version_id: source_version_id.into(),
            document_kind,
            extractor_kind: extractor_kind.into(),
            model_id: model_id.map(str::to_string),
            model_version: model_version.map(str::to_string),
            schema_version: RELATION_SCHEMA_VERSION.into(),
            input_hash,
            status: "running".into(),
            candidate_count: 0,
            validation_errors: 0,
            started_at: now,
            completed_at: None,
            error: None,
        })
        .await?;

        let entities = self.load_entities().await?;
        let mut drafts = deterministic_drafts(&detail, document_kind, &entities);
        for model in model_candidates {
            if model.consortium_members.is_empty() {
                drafts.push(CandidateDraft {
                    model,
                    proposed_by_model: true,
                    disclosure_mode: "named".into(),
                });
            } else {
                for member in &model.consortium_members {
                    let mut split = model.clone();
                    split.subject_text = member.clone();
                    split.consortium_members.clear();
                    drafts.push(CandidateDraft {
                        model: split,
                        proposed_by_model: true,
                        disclosure_mode: "consortium_member".into(),
                    });
                }
            }
        }
        deduplicate_drafts(&mut drafts);

        let mut validation_errors = 0usize;
        for draft in drafts {
            let candidate = validate_draft(
                &run_id,
                source_version_id,
                document_kind,
                draft,
                &detail,
                &entities,
                now,
            )?;
            validation_errors += usize::from(candidate.validation_status != "validated");
            self.persist_candidate(&candidate).await?;
        }
        self.reconcile_run_evidence(&run_id).await?;
        self.finish_run(&run_id, validation_errors).await?;
        self.run_detail(&run_id).await
    }

    pub async fn run_detail(&self, run_id: &str) -> Result<ExtractionRunDetail> {
        let run = self
            .load_run(run_id)
            .await?
            .ok_or_else(|| Error::NotFound(run_id.into()))?;
        let source = SourceVerifier::new(self.storage.clone())
            .read_document(&run.source_version_id)
            .await?;
        let candidates = self.candidates_for_run(run_id).await?;
        let diagnostics = vec![
            format!("共扫描 {} 个可定位段落", source.segments.len()),
            format!(
                "生成 {} 个候选，其中 {} 个需要人工补充映射或证据",
                candidates.len(),
                run.validation_errors
            ),
            "模型只负责结构化候选；原文 span、实体层级、金额比例和发布权限均由确定性规则复核"
                .into(),
            "未审核、低于 85% 置信度、匿名客户、保密或不可推断候选不会进入 Agent 高置信结论".into(),
        ];
        Ok(ExtractionRunDetail {
            run,
            source_title: source
                .version
                .as_ref()
                .and_then(|value| value.title.clone()),
            source_url: source.document.canonical_url,
            candidates,
            diagnostics,
        })
    }

    pub async fn review_page(
        &self,
        status: Option<&str>,
        document_kind: Option<DocumentKind>,
        min_confidence_bps: Option<u16>,
        page: usize,
        page_size: usize,
    ) -> Result<ReviewPage> {
        let page = page.max(1);
        let page_size = page_size.clamp(1, 100);
        let status = status.unwrap_or("all").to_string();
        let kind = document_kind.map(|value| value.token().to_string());
        let min_confidence = i64::from(min_confidence_bps.unwrap_or(0));
        let offset = ((page - 1) * page_size) as i64;
        let limit = page_size as i64;
        let (ids, total) = self
            .storage
            .run(move |conn| {
                let where_sql = "(?1='all' OR review_status=?1) AND (?2 IS NULL OR document_kind=?2) AND confidence_bps>=?3";
                let total: i64 = conn.query_row(
                    &format!("SELECT COUNT(*) FROM relation_candidates WHERE {where_sql}"),
                    params![status, kind, min_confidence],
                    |row| row.get(0),
                )?;
                let mut stmt = conn.prepare(&format!(
                    "SELECT candidate_id FROM relation_candidates WHERE {where_sql} ORDER BY CASE review_status WHEN 'pending_review' THEN 0 ELSE 1 END,confidence_bps DESC,created_at DESC LIMIT ?4 OFFSET ?5"
                ))?;
                let ids = stmt
                    .query_map(params![status, kind, min_confidence, limit, offset], |row| row.get(0))?
                    .collect::<std::result::Result<Vec<String>, _>>()?;
                Ok((ids, total as usize))
            })
            .await?;
        let mut items = Vec::new();
        for id in ids {
            if let Some(item) = self.candidate(&id).await? {
                items.push(item);
            }
        }
        Ok(ReviewPage {
            items,
            total,
            page,
            page_size,
            total_pages: total.div_ceil(page_size),
        })
    }

    pub async fn review(&self, request: RelationReviewRequest) -> Result<PublicationResult> {
        let reason = request.reason.trim();
        let reviewer = request.reviewer.trim();
        if reason.is_empty() || reviewer.is_empty() {
            return Err(Error::Invalid("审核人和审核理由不能为空".into()));
        }
        if request.training_eligible && request.dataset_split.as_deref() == Some("test") {
            return Err(Error::Invalid(
                "测试集审核结果不能标为训练可用，防止训练/测试泄漏".into(),
            ));
        }
        let decision = request.decision.as_str();
        if !matches!(
            decision,
            "accepted"
                | "modified"
                | "rejected"
                | "confidential"
                | "non_inferable"
                | "merge_entity"
        ) {
            return Err(Error::Invalid("未知审核决定".into()));
        }
        let current = self
            .candidate(&request.candidate_id)
            .await?
            .ok_or_else(|| Error::NotFound(request.candidate_id.clone()))?;
        let relation = request.relation.unwrap_or(current.relation);
        let subject_text = request
            .subject_text
            .as_deref()
            .unwrap_or(&current.subject_text)
            .trim()
            .to_string();
        let object_text = request
            .object_text
            .as_deref()
            .unwrap_or(&current.object_text)
            .trim()
            .to_string();
        if subject_text.is_empty() || object_text.is_empty() {
            return Err(Error::Invalid("主体与对象不能为空".into()));
        }
        let entities = self.load_entities().await?;
        if let Some(entity_id) = request.merged_entity_id.as_deref() {
            if !entities.iter().any(|value| value.entity_id == entity_id) {
                return Err(Error::Invalid(format!(
                    "合并目标实体不存在于主数据：{entity_id}"
                )));
            }
        }
        let subject = resolve_entity(&subject_text, &entities);
        let object = resolve_entity(&object_text, &entities);
        let status = match decision {
            "accepted" | "modified" | "merge_entity" => decision,
            "confidential" => "confidential",
            "non_inferable" => "non_inferable",
            _ => "rejected",
        }
        .to_string();
        let confidential = request.confidential || decision == "confidential";
        let non_inferable = request.non_inferable || decision == "non_inferable";
        let modified = serde_json::json!({
            "subject_text": subject_text,
            "object_text": object_text,
            "relation_type": relation.token(),
            "product_text": request.product_text,
            "merged_entity_id": request.merged_entity_id,
            "confidential": confidential,
            "non_inferable": non_inferable,
        });
        let candidate_id = request.candidate_id.clone();
        let decision_db = request.decision.clone();
        let status_db = status.clone();
        let reviewer = reviewer.to_string();
        let reason = reason.to_string();
        let subject_entity = request
            .merged_entity_id
            .clone()
            .or_else(|| subject.map(|value| value.entity_id.clone()));
        let subject_parent = subject_entity
            .as_ref()
            .and_then(|id| listed_parent_id(id, &entities));
        let object_entity = object.map(|value| value.entity_id.clone());
        let object_parent = object_entity
            .as_ref()
            .and_then(|id| listed_parent_id(id, &entities));
        let split = request.dataset_split.clone();
        let training = request.training_eligible;
        let now = now_secs();
        self.storage
            .run(move |conn| {
                let tx = conn.transaction()?;
                tx.execute(
                    "UPDATE relation_candidates SET subject_text=?2,object_text=?3,relation_type=?4,
                     product_text=COALESCE(?5,product_text),subject_entity_id=?6,object_entity_id=?7,
                     subject_parent_entity_id=?8,object_parent_entity_id=?9,review_status=?10,
                     confidential=?11,non_inferable=?12,candidate_version=candidate_version+1,updated_at=?13
                     ,confidence_bps=CASE WHEN validation_status='validated' AND ?10 IN ('accepted','modified','merge_entity')
                                          THEN MAX(confidence_bps,8500) ELSE confidence_bps END
                     WHERE candidate_id=?1",
                    params![candidate_id, subject_text, object_text, relation.token(), request.product_text,
                        subject_entity, object_entity, subject_parent, object_parent, status_db,
                        confidential, non_inferable, now],
                )?;
                tx.execute(
                    "INSERT INTO relation_candidate_reviews
                     (candidate_id,decision,reviewer,reason,modified_json,merged_entity_id,
                      dataset_split,training_eligible,reviewed_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![candidate_id, decision_db, reviewer, reason, modified.to_string(),
                        request.merged_entity_id, split, training, now],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        if request.publish && matches!(decision, "accepted" | "modified" | "merge_entity") {
            self.publish(&request.candidate_id).await
        } else {
            Ok(PublicationResult {
                candidate_id: request.candidate_id,
                publication_id: None,
                projection_key: None,
                status,
                note: "审核结果已写入不可变审计记录；尚未发布到关系图".into(),
            })
        }
    }

    pub async fn publish(&self, candidate_id: &str) -> Result<PublicationResult> {
        let candidate = self
            .candidate(candidate_id)
            .await?
            .ok_or_else(|| Error::NotFound(candidate_id.into()))?;
        if !matches!(
            candidate.review_status.as_str(),
            "accepted" | "modified" | "merge_entity"
        ) {
            return Err(Error::Invalid("候选尚未通过人工审核".into()));
        }
        if candidate.confidential
            || candidate.non_inferable
            || candidate.disclosure_mode == "anonymous_customer"
        {
            return Err(Error::Invalid("保密、匿名或不可推断关系不能发布".into()));
        }
        if candidate.confidence_bps < AGENT_CONFIDENCE_THRESHOLD_BPS {
            return Err(Error::Invalid("候选置信度低于高置信发布阈值 85%".into()));
        }
        let entities = self.load_entities().await?;
        let (src_node, dst_node) = projection_nodes(&candidate, &entities)?;
        self.graph.upsert_node(&src_node).await?;
        self.graph.upsert_node(&dst_node).await?;
        let relation = candidate.relation.graph_relation();
        let projection_key = format!("{}|{}|{}", src_node.id, dst_node.id, relation.as_str());
        let now = now_secs();
        self.graph
            .upsert_edge(&Edge {
                id: None,
                src: src_node.id.clone(),
                dst: dst_node.id.clone(),
                relation,
                weight: candidate
                    .share_bps
                    .map_or(0.7, |value| f64::from(value) / 10_000.0),
                source_name: format!("人工审核关系 · {}", candidate.evidence[0].source_version_id),
                source_url: SourceVerifier::new(self.storage.clone())
                    .read_document(&candidate.source_version_id)
                    .await?
                    .document
                    .canonical_url,
                confidence: f64::from(candidate.confidence_bps) / 10_000.0,
                valid_from: now,
                valid_to: None,
            })
            .await?;
        let graph_edge_id = self
            .graph
            .all_edges()
            .await?
            .into_iter()
            .find(|edge| {
                edge.src == src_node.id && edge.dst == dst_node.id && edge.relation == relation
            })
            .and_then(|edge| edge.id);
        let existing = self.active_publication(candidate_id).await?;
        if let Some((id, _)) = existing {
            return Ok(PublicationResult {
                candidate_id: candidate_id.into(),
                publication_id: Some(id),
                projection_key: Some(projection_key),
                status: "published".into(),
                note: "该候选已经发布，本次为幂等复用".into(),
            });
        }
        let publication_id = format!(
            "relpub:{}",
            &hash(&format!("{candidate_id}|{}", candidate.candidate_version))[..28]
        );
        let candidate_id_db = candidate_id.to_string();
        let projection_db = projection_key.clone();
        let publication_db = publication_id.clone();
        let version = i64::from(candidate.candidate_version);
        self.storage
            .run(move |conn| {
                conn.execute(
                    "INSERT INTO relation_publications
                     (publication_id,candidate_id,graph_edge_id,projection_key,publication_version,status,published_at)
                     VALUES (?1,?2,?3,?4,?5,'published',?6)",
                    params![publication_db, candidate_id_db, graph_edge_id, projection_db, version, now],
                )?;
                Ok(())
            })
            .await?;
        Ok(PublicationResult {
            candidate_id: candidate_id.into(),
            publication_id: Some(publication_id),
            projection_key: Some(projection_key),
            status: "published".into(),
            note: "已发布到关系图；原始候选、证据和审核记录保持独立可回放".into(),
        })
    }

    pub async fn retract(&self, candidate_id: &str, reason: &str) -> Result<PublicationResult> {
        if reason.trim().is_empty() {
            return Err(Error::Invalid("撤回理由不能为空".into()));
        }
        let (publication_id, projection_key) = self
            .active_publication(candidate_id)
            .await?
            .ok_or_else(|| Error::Invalid("候选没有有效发布记录".into()))?;
        let now = now_secs();
        let publication_db = publication_id.clone();
        let reason_db = reason.trim().to_string();
        let projection_db = projection_key.clone();
        let active_supports = self
            .storage
            .run(move |conn| {
                conn.execute(
                    "UPDATE relation_publications SET status='retracted',retracted_at=?2,retraction_reason=?3 WHERE publication_id=?1",
                    params![publication_db, now, reason_db],
                )?;
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM relation_publications WHERE projection_key=?1 AND status='published'",
                    [projection_db],
                    |row| row.get(0),
                )?;
                Ok(count)
            })
            .await?;
        if active_supports == 0 {
            let parts = projection_key.split('|').collect::<Vec<_>>();
            if parts.len() == 3 {
                if let Some(mut edge) = self.graph.all_edges().await?.into_iter().find(|edge| {
                    edge.src == parts[0]
                        && edge.dst == parts[1]
                        && edge.relation.as_str() == parts[2]
                }) {
                    edge.valid_to = Some(now);
                    edge.source_name = format!("{} · 已撤回", edge.source_name);
                    self.graph.upsert_edge(&edge).await?;
                }
            }
        }
        Ok(PublicationResult {
            candidate_id: candidate_id.into(),
            publication_id: Some(publication_id),
            projection_key: Some(projection_key),
            status: "retracted".into(),
            note: if active_supports == 0 {
                "已撤回最后一条有效证据，关系图投影已失效".into()
            } else {
                format!("已撤回该证据；仍有 {active_supports} 条独立有效发布支持关系投影")
            },
        })
    }

    pub async fn agent_relations(
        &self,
        entity_query: &str,
        limit: usize,
    ) -> Result<Vec<RelationCandidate>> {
        let query = format!("%{}%", entity_query.trim());
        let raw = entity_query.trim().to_string();
        let limit = limit.clamp(1, 100) as i64;
        let ids = self
            .storage
            .run(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT c.candidate_id FROM relation_candidates c
                     JOIN relation_publications p ON p.candidate_id=c.candidate_id AND p.status='published'
                     WHERE c.confidence_bps>=8500 AND c.confidential=0 AND c.non_inferable=0
                       AND c.review_status IN ('accepted','modified','merge_entity')
                       AND (c.subject_text LIKE ?1 OR c.object_text LIKE ?1 OR c.product_text LIKE ?1
                            OR c.subject_entity_id=?2 OR c.object_entity_id=?2
                            OR c.subject_parent_entity_id=?2 OR c.object_parent_entity_id=?2)
                     ORDER BY c.confidence_bps DESC,c.updated_at DESC LIMIT ?3",
                )?;
                let rows = stmt.query_map(params![query, raw, limit], |row| row.get(0))?;
                Ok(rows.collect::<std::result::Result<Vec<String>, _>>()?)
            })
            .await?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(candidate) = self.candidate(&id).await? {
                out.push(candidate);
            }
        }
        Ok(out)
    }

    /// Export human-reviewed relation labels for evaluation or training.
    /// Test labels are permanently barred from the training export path.
    pub async fn reviewed_annotations(
        &self,
        dataset_split: &str,
        for_training: bool,
    ) -> Result<Vec<RelationAnnotation>> {
        let split = dataset_split.trim();
        if !matches!(split, "train" | "dev" | "test") {
            return Err(Error::Invalid("数据集切分只能是 train/dev/test".into()));
        }
        if for_training && split == "test" {
            return Err(Error::Invalid("test 标注禁止导出到训练集".into()));
        }
        let split = split.to_string();
        let ids = self.storage.run(move |conn| {
            let mut stmt=conn.prepare("SELECT DISTINCT c.candidate_id FROM relation_candidates c JOIN relation_candidate_reviews r ON r.candidate_id=c.candidate_id WHERE r.dataset_split=?1 AND (?2=0 OR r.training_eligible=1) AND c.review_status IN ('accepted','modified','merge_entity') ORDER BY c.candidate_id")?;
            let rows=stmt.query_map(params![split,for_training],|row|row.get(0))?;
            Ok(rows.collect::<std::result::Result<Vec<String>,_>>()?)
        }).await?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(candidate) = self.candidate(&id).await? {
                if let (Some(subject), Some(object), Some(evidence)) = (
                    candidate.subject_entity_id.clone(),
                    candidate.object_entity_id.clone(),
                    candidate.evidence.first(),
                ) {
                    out.push(RelationAnnotation {
                        subject_entity_id: subject,
                        object_entity_id: object,
                        relation: candidate.relation,
                        segment_id: evidence.segment_id.clone(),
                        span_start: evidence.span_start,
                        span_end: evidence.span_end,
                    });
                }
            }
        }
        Ok(out)
    }

    pub async fn candidate(&self, candidate_id: &str) -> Result<Option<RelationCandidate>> {
        let id = candidate_id.to_string();
        let mut candidate = self
            .storage
            .run(move |conn| {
                conn.query_row(CANDIDATE_SELECT, [id], map_candidate)
                    .optional()
                    .map_err(Into::into)
            })
            .await?;
        if let Some(value) = &mut candidate {
            value.evidence = self.evidence_for(&value.candidate_id).await?;
            value.publication_status = self
                .active_publication(&value.candidate_id)
                .await?
                .map(|_| "published".into());
            value.eligible_for_agent = eligible_for_agent(value);
        }
        Ok(candidate)
    }

    async fn candidates_for_run(&self, run_id: &str) -> Result<Vec<RelationCandidate>> {
        let run_id = run_id.to_string();
        let ids = self
            .storage
            .run(move |conn| {
                let mut stmt = conn.prepare("SELECT candidate_id FROM relation_candidates WHERE run_id=?1 ORDER BY created_at,candidate_id")?;
                let rows = stmt.query_map([run_id], |row| row.get(0))?;
                Ok(rows.collect::<std::result::Result<Vec<String>, _>>()?)
            })
            .await?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(candidate) = self.candidate(&id).await? {
                out.push(candidate);
            }
        }
        Ok(out)
    }

    async fn all_candidate_ids(&self) -> Result<Vec<String>> {
        self.storage
            .run(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT candidate_id FROM relation_candidates ORDER BY created_at,candidate_id",
                )?;
                let rows = stmt.query_map([], |row| row.get(0))?;
                Ok(rows.collect::<std::result::Result<Vec<String>, _>>()?)
            })
            .await
            .map_err(Into::into)
    }

    async fn reconcile_run_evidence(&self, run_id: &str) -> Result<()> {
        let current = self.candidates_for_run(run_id).await?;
        let mut all = Vec::new();
        for id in self.all_candidate_ids().await? {
            if let Some(candidate) = self.candidate(&id).await? {
                all.push(candidate);
            }
        }
        for candidate in current {
            let subject_key = candidate
                .subject_parent_entity_id
                .as_ref()
                .or(candidate.subject_entity_id.as_ref());
            let object_key = candidate
                .object_parent_entity_id
                .as_ref()
                .or(candidate.object_entity_id.as_ref());
            if subject_key.is_none() || object_key.is_none() {
                continue;
            }
            let mut supports = BTreeSet::new();
            let mut conflicts = BTreeSet::new();
            let mut linked = Vec::new();
            for other in &all {
                if other.candidate_id == candidate.candidate_id
                    || other.source_version_id == candidate.source_version_id
                {
                    continue;
                }
                let other_subject = other
                    .subject_parent_entity_id
                    .as_ref()
                    .or(other.subject_entity_id.as_ref());
                let other_object = other
                    .object_parent_entity_id
                    .as_ref()
                    .or(other.object_entity_id.as_ref());
                if subject_key != other_subject || object_key != other_object {
                    continue;
                }
                let polarity = if other.relation == candidate.relation {
                    "supports"
                } else {
                    "conflicts"
                };
                for evidence in &other.evidence {
                    if polarity == "supports" {
                        supports.insert(evidence.independent_group.clone());
                    } else {
                        conflicts.insert(evidence.independent_group.clone());
                    }
                    let mut copy = evidence.clone();
                    copy.evidence_id = format!(
                        "relev:{}",
                        &hash(&format!(
                            "{}|{}|{}",
                            candidate.candidate_id, evidence.evidence_id, polarity
                        ))[..28]
                    );
                    copy.polarity = polarity.into();
                    linked.push(copy);
                }
            }
            if linked.is_empty() {
                continue;
            }
            let candidate_id = candidate.candidate_id.clone();
            let now = now_secs();
            let mut validation = candidate.validation.clone();
            if !supports.is_empty() {
                validation.push(ValidationCheck {
                    field: "独立材料交叉支持".into(),
                    passed: true,
                    detail: format!(
                        "另有 {} 组独立来源支持同一实体、方向和关系类型",
                        supports.len()
                    ),
                });
            }
            if !conflicts.is_empty() {
                validation.push(ValidationCheck {
                    field: "独立材料关系冲突".into(),
                    passed: false,
                    detail: format!(
                        "另有 {} 组独立来源对同一实体对给出不同关系，必须人工解释口径/时点",
                        conflicts.len()
                    ),
                });
            }
            let validation_json = serde_json::to_string(&validation)?;
            let support_bonus = (supports.len() * 200).min(800) as i64;
            let has_conflict = !conflicts.is_empty();
            self.storage.run(move |conn| { let tx=conn.transaction()?; for e in linked { tx.execute("INSERT OR IGNORE INTO relation_candidate_evidence (evidence_id,candidate_id,source_version_id,segment_id,page_number,paragraph_index,span_start,span_end,quote_original,independent_group,polarity,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![e.evidence_id,candidate_id,e.source_version_id,e.segment_id,e.page_number.map(i64::from),e.paragraph_index as i64,e.span_start as i64,e.span_end as i64,e.quote_original,e.independent_group,e.polarity,now])?; }
                tx.execute("UPDATE relation_candidates SET confidence_bps=CASE WHEN proposed_by_model=1 THEN MIN(confidence_bps+?2,8400) ELSE MIN(confidence_bps+?2,9800) END,validation_status=CASE WHEN ?3=1 THEN 'needs_review' ELSE validation_status END,validation_json=?4,updated_at=?5 WHERE candidate_id=?1",params![candidate_id,support_bonus,has_conflict,validation_json,now])?; tx.commit()?; Ok(()) }).await?;
        }
        Ok(())
    }

    async fn evidence_for(&self, candidate_id: &str) -> Result<Vec<RelationEvidence>> {
        let id = candidate_id.to_string();
        self.storage
            .run(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT evidence_id,source_version_id,segment_id,page_number,paragraph_index,
                     span_start,span_end,quote_original,independent_group,polarity
                     FROM relation_candidate_evidence WHERE candidate_id=?1 ORDER BY created_at,evidence_id",
                )?;
                let rows = stmt.query_map([id], |row| {
                    Ok(RelationEvidence {
                        evidence_id: row.get(0)?, source_version_id: row.get(1)?, segment_id: row.get(2)?,
                        page_number: row.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                        paragraph_index: row.get::<_, i64>(4)? as usize,
                        span_start: row.get::<_, i64>(5)? as usize, span_end: row.get::<_, i64>(6)? as usize,
                        quote_original: row.get(7)?, independent_group: row.get(8)?, polarity: row.get(9)?,
                    })
                })?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(Into::into)
    }

    async fn load_entities(&self) -> Result<Vec<EntityRecord>> {
        self.storage
            .run(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT e.entity_id,e.entity_type,e.canonical_name,e.listed_code,e.parent_entity_id,
                     COALESCE(GROUP_CONCAT(n.name_text,'\u{1f}'),'')
                     FROM research_entities e LEFT JOIN research_entity_names n ON n.entity_id=e.entity_id
                     GROUP BY e.entity_id ORDER BY LENGTH(e.canonical_name) DESC,e.entity_id",
                )?;
                let rows = stmt.query_map([], |row| {
                    let aliases: String = row.get(5)?;
                    let canonical: String = row.get(2)?;
                    let mut names = aliases.split('\u{1f}').filter(|v| !v.trim().is_empty()).map(str::to_string).collect::<Vec<_>>();
                    if !names.iter().any(|value| value == &canonical) { names.push(canonical.clone()); }
                    names.sort_by_key(|value| std::cmp::Reverse(value.len()));
                    names.dedup();
                    Ok(EntityRecord { entity_id: row.get(0)?, entity_type: row.get(1)?, canonical_name: canonical,
                        listed_code: row.get(3)?, parent_entity_id: row.get(4)?, names })
                })?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(Into::into)
    }

    async fn insert_run(&self, run: &ExtractionRun) -> Result<()> {
        let run = run.clone();
        self.storage.run(move |conn| { conn.execute(
            "INSERT INTO relation_extraction_runs (run_id,source_version_id,document_kind,extractor_kind,model_id,model_version,schema_version,input_hash,status,started_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'running',?9)",
            params![run.run_id,run.source_version_id,run.document_kind.token(),run.extractor_kind,run.model_id,run.model_version,run.schema_version,run.input_hash,run.started_at])?; Ok(()) }).await?;
        Ok(())
    }

    async fn find_existing_run(
        &self,
        source: &str,
        extractor: &str,
        model: Option<&str>,
        version: Option<&str>,
        input_hash: &str,
    ) -> Result<Option<String>> {
        let values = (
            source.to_string(),
            extractor.to_string(),
            model.map(str::to_string),
            version.map(str::to_string),
            input_hash.to_string(),
        );
        self.storage.run(move |conn| conn.query_row(
            "SELECT run_id FROM relation_extraction_runs WHERE source_version_id=?1 AND extractor_kind=?2 AND model_id IS ?3 AND model_version IS ?4 AND schema_version=?5 AND input_hash=?6 AND status='completed'",
            params![values.0,values.1,values.2,values.3,RELATION_SCHEMA_VERSION,values.4], |row| row.get(0)).optional().map_err(Into::into)).await.map_err(Into::into)
    }

    async fn finish_run(&self, run_id: &str, validation_errors: usize) -> Result<()> {
        let id = run_id.to_string();
        let now = now_secs();
        self.storage.run(move |conn| { conn.execute(
            "UPDATE relation_extraction_runs SET status='completed',candidate_count=(SELECT COUNT(*) FROM relation_candidates WHERE run_id=?1),validation_errors=(SELECT COUNT(*) FROM relation_candidates WHERE run_id=?1 AND validation_status!='validated'),completed_at=?3 WHERE run_id=?1",
            params![id,validation_errors as i64,now])?; Ok(()) }).await?;
        Ok(())
    }

    async fn load_run(&self, run_id: &str) -> Result<Option<ExtractionRun>> {
        let id = run_id.to_string();
        self.storage.run(move |conn| conn.query_row(
            "SELECT run_id,source_version_id,document_kind,extractor_kind,model_id,model_version,schema_version,input_hash,status,candidate_count,validation_errors,started_at,completed_at,error FROM relation_extraction_runs WHERE run_id=?1",
            [id], |row| Ok(ExtractionRun { run_id: row.get(0)?, source_version_id: row.get(1)?, document_kind: DocumentKind::parse(&row.get::<_,String>(2)?), extractor_kind: row.get(3)?, model_id: row.get(4)?, model_version: row.get(5)?, schema_version: row.get(6)?, input_hash: row.get(7)?, status: row.get(8)?, candidate_count: row.get::<_,i64>(9)? as usize, validation_errors: row.get::<_,i64>(10)? as usize, started_at: row.get(11)?, completed_at: row.get(12)?, error: row.get(13)? })).optional().map_err(Into::into)).await.map_err(Into::into)
    }

    async fn persist_candidate(&self, candidate: &RelationCandidate) -> Result<()> {
        let c = candidate.clone();
        self.storage.run(move |conn| { let tx = conn.transaction()?; tx.execute(
            "INSERT INTO relation_candidates (candidate_id,run_id,source_version_id,document_kind,subject_text,object_text,relation_type,product_text,amount_text,share_bps,report_period,region,subject_entity_id,object_entity_id,subject_parent_entity_id,object_parent_entity_id,disclosure_mode,confidence_bps,validation_status,validation_json,review_status,confidential,non_inferable,candidate_version,proposed_by_model,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,'pending_review',?21,?22,?23,?24,?25,?25)",
            params![c.candidate_id,c.run_id,c.source_version_id,c.document_kind.token(),c.subject_text,c.object_text,c.relation.token(),c.product_text,c.amount_text,c.share_bps.map(i64::from),c.report_period,c.region,c.subject_entity_id,c.object_entity_id,c.subject_parent_entity_id,c.object_parent_entity_id,c.disclosure_mode,i64::from(c.confidence_bps),c.validation_status,serde_json::to_string(&c.validation)?,c.confidential,c.non_inferable,i64::from(c.candidate_version),c.proposed_by_model,c.created_at])?;
            for e in c.evidence { tx.execute("INSERT INTO relation_candidate_evidence (evidence_id,candidate_id,source_version_id,segment_id,page_number,paragraph_index,span_start,span_end,quote_original,independent_group,polarity,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)", params![e.evidence_id,c.candidate_id,e.source_version_id,e.segment_id,e.page_number.map(i64::from),e.paragraph_index as i64,e.span_start as i64,e.span_end as i64,e.quote_original,e.independent_group,e.polarity,c.created_at])?; }
            tx.commit()?; Ok(()) }).await?;
        Ok(())
    }

    async fn active_publication(&self, candidate_id: &str) -> Result<Option<(String, String)>> {
        let id = candidate_id.to_string();
        self.storage.run(move |conn| conn.query_row("SELECT publication_id,projection_key FROM relation_publications WHERE candidate_id=?1 AND status='published' ORDER BY publication_version DESC LIMIT 1", [id], |row| Ok((row.get(0)?,row.get(1)?))).optional().map_err(Into::into)).await.map_err(Into::into)
    }
}

const CANDIDATE_SELECT: &str = "SELECT candidate_id,run_id,source_version_id,document_kind,subject_text,object_text,relation_type,product_text,amount_text,share_bps,report_period,region,subject_entity_id,object_entity_id,subject_parent_entity_id,object_parent_entity_id,disclosure_mode,confidence_bps,validation_status,validation_json,review_status,confidential,non_inferable,candidate_version,proposed_by_model,created_at,updated_at FROM relation_candidates WHERE candidate_id=?1";

fn map_candidate(row: &Row<'_>) -> rusqlite::Result<RelationCandidate> {
    let relation_raw: String = row.get(6)?;
    let checks: String = row.get(19)?;
    Ok(RelationCandidate {
        candidate_id: row.get(0)?,
        run_id: row.get(1)?,
        source_version_id: row.get(2)?,
        document_kind: DocumentKind::parse(&row.get::<_, String>(3)?),
        subject_text: row.get(4)?,
        object_text: row.get(5)?,
        relation: RelationType::parse(&relation_raw).unwrap_or(RelationType::Supplies),
        product_text: row.get(7)?,
        amount_text: row.get(8)?,
        share_bps: row
            .get::<_, Option<i64>>(9)?
            .map(|v| v.clamp(0, 10_000) as u16),
        report_period: row.get(10)?,
        region: row.get(11)?,
        subject_entity_id: row.get(12)?,
        object_entity_id: row.get(13)?,
        subject_parent_entity_id: row.get(14)?,
        object_parent_entity_id: row.get(15)?,
        disclosure_mode: row.get(16)?,
        confidence_bps: row.get::<_, i64>(17)?.clamp(0, 10_000) as u16,
        validation_status: row.get(18)?,
        validation: serde_json::from_str(&checks).unwrap_or_default(),
        review_status: row.get(20)?,
        confidential: row.get::<_, bool>(21)?,
        non_inferable: row.get::<_, bool>(22)?,
        candidate_version: row.get::<_, i64>(23)? as u32,
        proposed_by_model: row.get::<_, bool>(24)?,
        publication_status: None,
        eligible_for_agent: false,
        evidence: Vec::new(),
        created_at: row.get(25)?,
        updated_at: row.get(26)?,
    })
}

fn validate_draft(
    run_id: &str,
    source_version_id: &str,
    document_kind: DocumentKind,
    draft: CandidateDraft,
    detail: &SourceDocumentDetail,
    entities: &[EntityRecord],
    now: i64,
) -> Result<RelationCandidate> {
    let model = draft.model;
    let segment = detail
        .segments
        .iter()
        .find(|value| value.segment_id == model.evidence.segment_id);
    let mut checks = Vec::new();
    let evidence_valid = segment.is_some_and(|segment| {
        segment
            .text
            .get(model.evidence.span_start..model.evidence.span_end)
            == Some(model.evidence.quote_original.as_str())
    });
    checks.push(ValidationCheck {
        field: "原文证据".into(),
        passed: evidence_valid,
        detail: if evidence_valid {
            "quote 与不可变段落的 UTF-8 span 完全一致".into()
        } else {
            "quote/span 无法在指定段落复现".into()
        },
    });
    let subject = resolve_entity(&model.subject_text, entities);
    let object = resolve_entity(&model.object_text, entities);
    let anonymous =
        is_anonymous(&model.object_text) || draft.disclosure_mode == "anonymous_customer";
    checks.push(ValidationCheck {
        field: "主体实体".into(),
        passed: subject.is_some(),
        detail: subject.map_or_else(
            || "未在证券主数据、公司别名或子公司层级中唯一命中".into(),
            |v| format!("{} → {}", model.subject_text, v.canonical_name),
        ),
    });
    checks.push(ValidationCheck {
        field: "对象实体".into(),
        passed: object.is_some() || anonymous || model.product_text.is_some(),
        detail: if anonymous {
            "公告依法匿名披露，不允许反推客户身份".into()
        } else {
            object.map_or_else(
                || {
                    if model.product_text.is_some() {
                        "对象作为产品节点保留".into()
                    } else {
                        "未在实体主数据中唯一命中".into()
                    }
                },
                |v| format!("{} → {}", model.object_text, v.canonical_name),
            )
        },
    });
    let share_valid = model.share_bps.is_none_or(|value| value <= 10_000);
    checks.push(ValidationCheck {
        field: "金额/比例单位".into(),
        passed: share_valid,
        detail: if share_valid {
            "原单位保留，比例在 0%～100% 范围".into()
        } else {
            "比例超出 100%，未自动修正".into()
        },
    });
    let period_valid = model.report_period.as_deref().is_none_or(valid_period);
    checks.push(ValidationCheck {
        field: "报告期".into(),
        passed: period_valid,
        detail: if period_valid {
            "报告期格式可复核或来源未披露".into()
        } else {
            "报告期格式无法确定".into()
        },
    });
    if draft.disclosure_mode == "consortium_member" {
        checks.push(ValidationCheck {
            field: "联合体拆分".into(),
            passed: true,
            detail: "联合体成员逐一建候选，不把牵头方关系复制给未披露成员".into(),
        });
    }
    let validation_status = if checks.iter().all(|value| value.passed) {
        "validated"
    } else {
        "needs_review"
    };
    let confidence = if draft.proposed_by_model {
        model.confidence_bps.min(8_400)
    } else {
        model.confidence_bps
    }
    .min(10_000);
    let subject_id = subject.map(|v| v.entity_id.clone());
    let object_id = object.map(|v| v.entity_id.clone());
    let subject_parent = subject_id
        .as_ref()
        .and_then(|id| listed_parent_id(id, entities));
    let object_parent = object_id
        .as_ref()
        .and_then(|id| listed_parent_id(id, entities));
    let segment = segment
        .ok_or_else(|| Error::Invalid(format!("证据段落不存在：{}", model.evidence.segment_id)))?;
    let signature = format!(
        "{run_id}|{}|{}|{}|{}|{}",
        model.subject_text,
        model.object_text,
        model.relation.token(),
        model.evidence.segment_id,
        model.evidence.span_start
    );
    let candidate_id = format!("relcand:{}", &hash(&signature)[..28]);
    let evidence_id = format!(
        "relev:{}",
        &hash(&format!(
            "{candidate_id}|{}|{}",
            model.evidence.segment_id, model.evidence.span_start
        ))[..28]
    );
    Ok(RelationCandidate {
        candidate_id,
        run_id: run_id.into(),
        source_version_id: source_version_id.into(),
        document_kind,
        subject_text: model.subject_text,
        object_text: model.object_text,
        relation: model.relation,
        product_text: model.product_text,
        amount_text: model.amount_text,
        share_bps: model.share_bps,
        report_period: model.report_period,
        region: model.region,
        subject_entity_id: subject_id,
        object_entity_id: object_id,
        subject_parent_entity_id: subject_parent,
        object_parent_entity_id: object_parent,
        disclosure_mode: if anonymous {
            "anonymous_customer".into()
        } else {
            draft.disclosure_mode
        },
        confidence_bps: confidence,
        validation_status: validation_status.into(),
        validation: checks,
        review_status: "pending_review".into(),
        confidential: false,
        non_inferable: anonymous,
        candidate_version: 1,
        proposed_by_model: draft.proposed_by_model,
        publication_status: None,
        eligible_for_agent: false,
        evidence: vec![RelationEvidence {
            evidence_id,
            source_version_id: source_version_id.into(),
            segment_id: segment.segment_id.clone(),
            page_number: segment.page_number,
            paragraph_index: segment.paragraph_index,
            span_start: model.evidence.span_start,
            span_end: model.evidence.span_end,
            quote_original: model.evidence.quote_original,
            independent_group: detail.document.source_document_id.clone(),
            polarity: "supports".into(),
        }],
        created_at: now,
        updated_at: now,
    })
}

fn deterministic_drafts(
    detail: &SourceDocumentDetail,
    kind: DocumentKind,
    entities: &[EntityRecord],
) -> Vec<CandidateDraft> {
    let mut drafts = Vec::new();
    for segment in &detail.segments {
        let text = segment.text.trim();
        if text.is_empty() {
            continue;
        }
        let mentions = entity_mentions(text, entities);
        if TOP_CUSTOMER.is_match(text) {
            if let Some(subject) = mentions.first() {
                drafts.push(rule_draft(
                    segment,
                    &subject.2,
                    "未披露客户",
                    RelationType::Supplies,
                    None,
                    "anonymous_customer",
                    8_700,
                ));
            }
        }
        let relation = if text.contains("中标") {
            Some(RelationType::WonBid)
        } else if text.contains("签订") && (text.contains("合同") || text.contains("协议")) {
            Some(RelationType::ContractWith)
        } else if text.contains("供应") || text.contains("销售给") {
            Some(RelationType::Supplies)
        } else if text.contains("采购") {
            Some(RelationType::Consumes)
        } else if text.contains("专利") {
            Some(RelationType::PatentFor)
        } else if text.contains("获批") || text.contains("批准") {
            Some(RelationType::ApprovedFor)
        } else if text.contains("产能") || text.contains("投产") {
            Some(RelationType::CapacityFor)
        } else if text.contains("生产") || text.contains("产品") {
            Some(RelationType::Produces)
        } else {
            None
        };
        let Some(relation) = relation else {
            continue;
        };
        if mentions.len() >= 2 {
            let suppliers = if text.contains("联合体") && matches!(relation, RelationType::WonBid)
            {
                mentions.iter().take(mentions.len() - 1).collect::<Vec<_>>()
            } else {
                vec![&mentions[0]]
            };
            let object = &mentions[mentions.len() - 1];
            for subject in suppliers {
                if subject.2 != object.2 {
                    drafts.push(rule_draft(
                        segment,
                        &subject.2,
                        &object.2,
                        relation,
                        None,
                        if text.contains("联合体") {
                            "consortium_member"
                        } else {
                            "named"
                        },
                        9_000,
                    ));
                }
            }
        } else if let Some(subject) = mentions.first() {
            if let Some(product) = PRODUCT_CAPTURE
                .captures(text)
                .and_then(|c| c.name("product"))
                .map(|m| {
                    m.as_str()
                        .trim_matches(&['，', '。', '；', '、'][..])
                        .to_string()
                })
                .filter(|v| v.chars().count() >= 2)
            {
                drafts.push(rule_draft(
                    segment,
                    &subject.2,
                    &product,
                    relation,
                    Some(product.clone()),
                    "named",
                    if matches!(
                        kind,
                        DocumentKind::AnnualReport
                            | DocumentKind::Prospectus
                            | DocumentKind::Tender
                            | DocumentKind::MajorContract
                    ) {
                        8_800
                    } else {
                        8_600
                    },
                ));
            }
        }
    }
    drafts
}

fn rule_draft(
    segment: &SourceSegment,
    subject: &str,
    object: &str,
    relation: RelationType,
    product: Option<String>,
    mode: &str,
    confidence: u16,
) -> CandidateDraft {
    let quote = segment.text.clone();
    CandidateDraft {
        model: ModelRelationCandidate {
            subject_text: subject.into(),
            object_text: object.into(),
            relation,
            product_text: product,
            amount_text: AMOUNT.find(&quote).map(|m| m.as_str().into()),
            share_bps: PERCENT
                .captures(&quote)
                .and_then(|c| c.name("value"))
                .and_then(|v| v.as_str().parse::<f64>().ok())
                .map(|v| (v * 100.0).round().clamp(0.0, 10_000.0) as u16),
            report_period: PERIOD.find(&quote).map(|m| m.as_str().into()),
            region: None,
            evidence: CandidateEvidenceInput {
                segment_id: segment.segment_id.clone(),
                span_start: 0,
                span_end: quote.len(),
                quote_original: quote,
            },
            confidence_bps: confidence,
            consortium_members: Vec::new(),
        },
        proposed_by_model: false,
        disclosure_mode: mode.into(),
    }
}

fn entity_mentions(text: &str, entities: &[EntityRecord]) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    for entity in entities {
        for name in &entity.names {
            if name.chars().count() < 2 {
                continue;
            }
            for (start, value) in text.match_indices(name) {
                out.push((start, start + value.len(), entity.canonical_name.clone()));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| (b.1 - b.0).cmp(&(a.1 - a.0))));
    let mut filtered: Vec<(usize, usize, String)> = Vec::new();
    for item in out {
        if filtered
            .iter()
            .any(|old| item.0 >= old.0 && item.1 <= old.1)
        {
            continue;
        }
        if !filtered.iter().any(|old| old.2 == item.2) {
            filtered.push(item);
        }
    }
    filtered
}

fn resolve_entity<'a>(text: &str, entities: &'a [EntityRecord]) -> Option<&'a EntityRecord> {
    let normalized = normalize(text);
    let found = entities
        .iter()
        .filter(|entity| {
            entity
                .names
                .iter()
                .any(|name| normalize(name) == normalized)
        })
        .collect::<Vec<_>>();
    (found.len() == 1).then_some(found[0])
}

fn listed_parent_id(id: &str, entities: &[EntityRecord]) -> Option<String> {
    let mut current = entities.iter().find(|value| value.entity_id == id)?;
    let mut seen = BTreeSet::new();
    loop {
        if !seen.insert(current.entity_id.clone()) {
            return None;
        }
        if current.listed_code.is_some() {
            return Some(current.entity_id.clone());
        }
        let parent = current.parent_entity_id.as_ref()?;
        current = entities.iter().find(|value| &value.entity_id == parent)?;
    }
}

fn projection_nodes(
    candidate: &RelationCandidate,
    entities: &[EntityRecord],
) -> Result<(Node, Node)> {
    let src = projection_node(
        candidate
            .subject_parent_entity_id
            .as_ref()
            .or(candidate.subject_entity_id.as_ref()),
        None,
        &candidate.subject_text,
        entities,
    )?;
    let dst = projection_node(
        candidate
            .object_parent_entity_id
            .as_ref()
            .or(candidate.object_entity_id.as_ref()),
        candidate.product_text.as_deref(),
        &candidate.object_text,
        entities,
    )?;
    Ok((src, dst))
}

fn projection_node(
    entity_id: Option<&String>,
    product: Option<&str>,
    fallback: &str,
    entities: &[EntityRecord],
) -> Result<Node> {
    if let Some(id) = entity_id {
        if let Some(entity) = entities.iter().find(|value| &value.entity_id == id) {
            if let Some(code) = &entity.listed_code {
                return Ok(Node {
                    id: format!("company:{code}"),
                    kind: NodeKind::Company,
                    name: entity.canonical_name.clone(),
                    code: Some(code.clone()),
                    meta: serde_json::json!({"research_entity_id": entity.entity_id}),
                });
            }
            if entity.entity_type == "product" || entity.entity_type == "commodity" {
                return Ok(Node {
                    id: entity.entity_id.clone(),
                    kind: if entity.entity_type == "commodity" {
                        NodeKind::Commodity
                    } else {
                        NodeKind::Product
                    },
                    name: entity.canonical_name.clone(),
                    code: None,
                    meta: serde_json::json!({"research_entity_id": entity.entity_id}),
                });
            }
        }
    }
    if let Some(name) = product.filter(|value| !value.trim().is_empty()) {
        return Ok(Node {
            id: format!("product:{}", &hash(name)[..16]),
            kind: NodeKind::Product,
            name: name.into(),
            code: None,
            meta: serde_json::json!({"created_from_reviewed_relation": true}),
        });
    }
    Err(Error::Invalid(format!(
        "{fallback} 无法映射为上市公司或产品节点"
    )))
}

fn eligible_for_agent(c: &RelationCandidate) -> bool {
    c.publication_status.as_deref() == Some("published")
        && c.confidence_bps >= AGENT_CONFIDENCE_THRESHOLD_BPS
        && c.validation_status == "validated"
        && matches!(
            c.review_status.as_str(),
            "accepted" | "modified" | "merge_entity"
        )
        && !c.confidential
        && !c.non_inferable
        && !c.evidence.is_empty()
}

fn deduplicate_drafts(drafts: &mut Vec<CandidateDraft>) {
    let mut seen = BTreeSet::new();
    drafts.retain(|d| {
        seen.insert(format!(
            "{}|{}|{}|{}|{}",
            normalize(&d.model.subject_text),
            normalize(&d.model.object_text),
            d.model.relation.token(),
            d.model.evidence.segment_id,
            d.model.evidence.span_start
        ))
    });
}
fn is_anonymous(value: &str) -> bool {
    [
        "未披露客户",
        "客户一",
        "客户二",
        "客户三",
        "客户四",
        "客户五",
        "第一大客户",
        "第二大客户",
        "前五大客户",
    ]
    .iter()
    .any(|token| value.contains(token))
}
fn valid_period(value: &str) -> bool {
    PERIOD.is_match(value)
}
fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '（' | '）' | '(' | ')' | '，' | ',' | '。'))
        .flat_map(char::to_lowercase)
        .collect()
}
fn extraction_hash(
    detail: &SourceDocumentDetail,
    model: &[ModelRelationCandidate],
) -> Result<String> {
    Ok(hash(&format!(
        "{}|{}",
        detail
            .version
            .as_ref()
            .map(|v| v.extracted_hash.as_str())
            .unwrap_or_default(),
        serde_json::to_string(model)?
    )))
}
fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|v| v.as_secs() as i64)
        .unwrap_or(0)
}

static TOP_CUSTOMER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"前五大客户|第[一二三四五]大客户|客户[一二三四五]").expect("top customer regex")
});
static AMOUNT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\d+(?:\.\d+)?\s*(?:万亿元|亿元|万元|元|万美元|亿美元|美元)").expect("amount regex")
});
static PERCENT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?P<value>\d+(?:\.\d+)?)\s*%").expect("percent regex"));
static PERIOD: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"20\d{2}年(?:第[一二三四1-4]季度|上半年|半年度|年度)?").expect("period regex")
});
static PRODUCT_CAPTURE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?:供应|生产|产品为|涉及|用于|产能为?)\s*(?P<product>[\p{Han}A-Za-z0-9+\-]{2,24})")
        .expect("product regex")
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationAnnotation {
    pub subject_entity_id: String,
    pub object_entity_id: String,
    pub relation: RelationType,
    pub segment_id: String,
    pub span_start: usize,
    pub span_end: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Prf {
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    pub true_positives: usize,
    pub predicted: usize,
    pub gold: usize,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationEvaluation {
    pub relation: Prf,
    pub entity: Prf,
    pub evidence_span: Prf,
    pub note: String,
}

pub fn evaluate_relations(
    gold: &[RelationAnnotation],
    predicted: &[RelationAnnotation],
) -> RelationEvaluation {
    let relation_tp = predicted
        .iter()
        .filter(|p| {
            gold.iter().any(|g| {
                g.subject_entity_id == p.subject_entity_id
                    && g.object_entity_id == p.object_entity_id
                    && g.relation == p.relation
            })
        })
        .count();
    let gold_entities = gold
        .iter()
        .flat_map(|v| [&v.subject_entity_id, &v.object_entity_id])
        .collect::<BTreeSet<_>>();
    let predicted_entities = predicted
        .iter()
        .flat_map(|v| [&v.subject_entity_id, &v.object_entity_id])
        .collect::<BTreeSet<_>>();
    let entity_tp = predicted_entities.intersection(&gold_entities).count();
    let span_tp = predicted
        .iter()
        .filter(|p| {
            gold.iter().any(|g| {
                g.segment_id == p.segment_id
                    && span_iou(g.span_start, g.span_end, p.span_start, p.span_end) >= 0.5
            })
        })
        .count();
    RelationEvaluation { relation: prf(relation_tp,predicted.len(),gold.len()), entity: prf(entity_tp,predicted_entities.len(),gold_entities.len()), evidence_span: prf(span_tp,predicted.len(),gold.len()), note: "关系采用实体对+方向+关系类型精确匹配；证据 span 在同一不可变段落 IoU≥0.5；评测集必须按 source_version_id 固定 train/dev/test，test 审核结果禁止训练".into() }
}
fn span_iou(a0: usize, a1: usize, b0: usize, b1: usize) -> f64 {
    let intersection = a1.min(b1).saturating_sub(a0.max(b0));
    let union = a1.max(b1).saturating_sub(a0.min(b0));
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}
fn prf(tp: usize, predicted: usize, gold: usize) -> Prf {
    let precision = if predicted == 0 {
        0.0
    } else {
        tp as f64 / predicted as f64
    };
    let recall = if gold == 0 {
        0.0
    } else {
        tp as f64 / gold as f64
    };
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    Prf {
        precision,
        recall,
        f1,
        true_positives: tp,
        predicted,
        gold,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_storage::StorageConfig;

    async fn seed_verified_source(
        storage: &Storage,
        document_id: &str,
        version_id: &str,
        segment_id: &str,
        url: &str,
        text: &str,
        now: i64,
    ) {
        let values = (
            document_id.to_string(),
            version_id.to_string(),
            segment_id.to_string(),
            url.to_string(),
            text.to_string(),
        );
        storage
            .run(move |conn| {
                conn.execute("INSERT INTO research_source_documents (source_document_id,canonical_url,current_version_id,authority_tier,authority_name,access_status,first_fetched_at,last_fetched_at,created_at,updated_at) VALUES (?1,?2,?3,'company_disclosure','公司正式披露','verified',?4,?4,?4,?4)",params![values.0,values.3,values.1,now])?;
                conn.execute("INSERT INTO research_source_versions (source_version_id,source_document_id,content_hash,extracted_hash,media_type,title,published_at,fetched_at,parser_version,reliability_score,independence_score,freshness_score) VALUES (?1,?2,?3,?3,'application/pdf','交叉验证材料',?4,?4,'test-parser',1,0.8,1)",params![values.1,values.0,format!("hash:{}",values.2),now])?;
                conn.execute("INSERT INTO source_document_segments (segment_id,source_version_id,page_number,paragraph_index,span_start,span_end,text,text_hash) VALUES (?1,?2,7,1,0,?3,?4,?5)",params![values.2,values.1,values.4.len() as i64,values.4,format!("segment:{}",values.2)])?;
                Ok(())
            })
            .await
            .unwrap();
    }

    #[test]
    fn relation_entity_and_evidence_metrics_are_separate() {
        let gold = vec![RelationAnnotation {
            subject_entity_id: "sub:a".into(),
            object_entity_id: "listed:b".into(),
            relation: RelationType::Supplies,
            segment_id: "s1".into(),
            span_start: 10,
            span_end: 30,
        }];
        let predicted = vec![RelationAnnotation {
            subject_entity_id: "sub:a".into(),
            object_entity_id: "listed:b".into(),
            relation: RelationType::Supplies,
            segment_id: "s1".into(),
            span_start: 12,
            span_end: 28,
        }];
        let metrics = evaluate_relations(&gold, &predicted);
        assert_eq!(metrics.relation.f1, 1.0);
        assert_eq!(metrics.entity.f1, 1.0);
        assert_eq!(metrics.evidence_span.f1, 1.0);
    }

    #[test]
    fn anonymous_customer_is_never_inferable() {
        assert!(is_anonymous("前五大客户之一"));
        assert!(is_anonymous("客户一"));
        assert!(!is_anonymous("宁德时代"));
    }

    #[test]
    fn model_enum_tokens_are_stable() {
        assert_eq!(DocumentKind::AnnualReport.token(), "annual_report");
        assert_eq!(RelationType::WonBid.token(), "won_bid");
    }

    #[tokio::test]
    async fn verified_document_replays_through_review_publish_query_and_retract() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let text = "星海动力有限公司与远航汽车股份有限公司签订动力电池供应合同，合同金额2亿元，占2025年度收入10%。";
        let now = 1_800_000_000i64;
        storage.run({ let text=text.to_string(); move |conn| {
            conn.execute("INSERT INTO research_source_documents (source_document_id,canonical_url,current_version_id,authority_tier,authority_name,access_status,first_fetched_at,last_fetched_at,created_at,updated_at) VALUES ('doc:1','https://example.com/report.pdf','srcver:1','company_disclosure','公司正式披露','verified',?1,?1,?1,?1)",[now])?;
            conn.execute("INSERT INTO research_source_versions (source_version_id,source_document_id,content_hash,extracted_hash,media_type,title,published_at,fetched_at,parser_version,reliability_score,independence_score,freshness_score) VALUES ('srcver:1','doc:1','raw','text','application/pdf','2025年年度报告',?1,?1,'test-parser',1,0.8,1)",[now])?;
            conn.execute("INSERT INTO source_document_segments (segment_id,source_version_id,page_number,paragraph_index,span_start,span_end,text,text_hash) VALUES ('seg:1','srcver:1',42,1,0,?1,?2,'segment-hash')",params![text.len() as i64,text])?;
            for (id,kind,name,code,parent) in [
                ("listed:star","listed_security","星海科技股份有限公司",Some("600001"),None),
                ("sub:power","subsidiary","星海动力有限公司",None,Some("listed:star")),
                ("listed:far","listed_security","远航汽车股份有限公司",Some("600002"),None),
            ] { conn.execute("INSERT INTO research_entities (entity_id,entity_type,canonical_name,listed_code,parent_entity_id,source_name,metadata_json,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,'test','{}',?6,?6)",params![id,kind,name,code,parent,now])?; conn.execute("INSERT INTO research_entity_names (entity_id,name_text,normalized_name,name_type,source_name) VALUES (?1,?2,?2,'canonical','test')",params![id,name])?; }
            Ok(())
        }}).await.unwrap();
        let evidence = CandidateEvidenceInput {
            segment_id: "seg:1".into(),
            span_start: 0,
            span_end: text.len(),
            quote_original: text.into(),
        };
        let proposal = ModelRelationCandidate {
            subject_text: "星海动力有限公司".into(),
            object_text: "远航汽车股份有限公司".into(),
            relation: RelationType::Supplies,
            product_text: Some("动力电池".into()),
            amount_text: Some("2亿元".into()),
            share_bps: Some(1000),
            report_period: Some("2025年度".into()),
            region: None,
            evidence,
            confidence_bps: 9600,
            consortium_members: vec![],
        };
        let store = RelationExtractionStore::new(storage.clone());
        let run = store
            .extract_source(
                "srcver:1",
                DocumentKind::AnnualReport,
                Some("test-model"),
                Some("v1"),
                vec![proposal.clone()],
            )
            .await
            .unwrap();
        let candidate = run.candidates.iter().find(|c| c.proposed_by_model).unwrap();
        assert_eq!(
            candidate.subject_parent_entity_id.as_deref(),
            Some("listed:star")
        );
        assert_eq!(
            candidate.object_parent_entity_id.as_deref(),
            Some("listed:far")
        );
        assert_eq!(candidate.evidence[0].page_number, Some(42));
        let replay = store
            .extract_source(
                "srcver:1",
                DocumentKind::AnnualReport,
                Some("test-model"),
                Some("v1"),
                vec![proposal.clone()],
            )
            .await
            .unwrap();
        assert_eq!(replay.run.run_id, run.run.run_id);
        seed_verified_source(
            &storage,
            "doc:2",
            "srcver:2",
            "seg:2",
            "https://example.com/ir.pdf",
            text,
            now + 1,
        )
        .await;
        let mut supporting = proposal.clone();
        supporting.evidence.segment_id = "seg:2".into();
        let support_run = store
            .extract_source(
                "srcver:2",
                DocumentKind::InvestorRelations,
                Some("test-model"),
                Some("v1"),
                vec![supporting],
            )
            .await
            .unwrap();
        let supported = support_run
            .candidates
            .iter()
            .find(|value| value.relation == RelationType::Supplies)
            .unwrap();
        assert!(supported.evidence.iter().any(|value| {
            value.source_version_id == "srcver:1" && value.polarity == "supports"
        }));
        assert!(supported
            .validation
            .iter()
            .any(|value| value.field == "独立材料交叉支持"));

        seed_verified_source(
            &storage,
            "doc:3",
            "srcver:3",
            "seg:3",
            "https://example.com/correction.pdf",
            text,
            now + 2,
        )
        .await;
        let mut conflicting = proposal;
        conflicting.relation = RelationType::Consumes;
        conflicting.evidence.segment_id = "seg:3".into();
        let conflict_run = store
            .extract_source(
                "srcver:3",
                DocumentKind::Other,
                Some("test-model"),
                Some("v1"),
                vec![conflicting],
            )
            .await
            .unwrap();
        let conflicted = conflict_run
            .candidates
            .iter()
            .find(|value| value.relation == RelationType::Consumes)
            .unwrap();
        assert_eq!(conflicted.validation_status, "needs_review");
        assert!(conflicted
            .evidence
            .iter()
            .any(|value| value.polarity == "conflicts"));
        let result = store
            .review(RelationReviewRequest {
                candidate_id: candidate.candidate_id.clone(),
                decision: "accepted".into(),
                reviewer: "reviewer-a".into(),
                reason: "核对第42页原文、子公司层级和合同对象".into(),
                subject_text: None,
                object_text: None,
                relation: None,
                product_text: None,
                merged_entity_id: None,
                confidential: false,
                non_inferable: false,
                publish: true,
                dataset_split: Some("dev".into()),
                training_eligible: true,
            })
            .await
            .unwrap();
        assert_eq!(result.status, "published");
        let agent = store.agent_relations("星海动力", 10).await.unwrap();
        assert_eq!(agent.len(), 1);
        assert!(agent[0].eligible_for_agent);
        assert_eq!(
            store.reviewed_annotations("dev", true).await.unwrap().len(),
            1
        );
        assert!(store
            .reviewed_annotations("test", true)
            .await
            .unwrap_err()
            .to_string()
            .contains("禁止"));
        let retracted = store
            .retract(&candidate.candidate_id, "年度报告后续更正")
            .await
            .unwrap();
        assert_eq!(retracted.status, "retracted");
        assert!(store
            .agent_relations("星海动力", 10)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn test_split_cannot_feed_training() {
        let dir = tempfile::tempdir().unwrap();
        let store = RelationExtractionStore::new(
            Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap(),
        );
        let error = store
            .review(RelationReviewRequest {
                candidate_id: "missing".into(),
                decision: "accepted".into(),
                reviewer: "r".into(),
                reason: "x".into(),
                subject_text: None,
                object_text: None,
                relation: None,
                product_text: None,
                merged_entity_id: None,
                confidential: false,
                non_inferable: false,
                publish: false,
                dataset_split: Some("test".into()),
                training_eligible: true,
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("训练/测试泄漏"));
    }
}
