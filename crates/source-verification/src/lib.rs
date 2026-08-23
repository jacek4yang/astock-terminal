//! Controlled source-document fetching, parsing and field-level evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use astock_security::{
    inspect_external_text, SafeFetchError, SafeFetchResult, SafeFetcher, UrlSecurityPolicy,
};
use astock_storage::Storage;
use flate2::write::GzEncoder;
use flate2::Compression;
use once_cell::sync::Lazy;
use regex::Regex;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

pub const SOURCE_PARSER_VERSION: &str = "source-evidence-v2";
const MAX_EXTRACTED_CHARS: usize = 400_000;

static MONEY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?P<label>订单金额|合同金额|中标金额|处罚金额|罚款|营业收入|营收|净利润|投资金额|回购金额|金额)?[^\d]{0,12}(?P<value>\d+(?:\.\d+)?)\s*(?P<unit>亿元|万元|元)")
        .expect("money regex")
});
static PERCENT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?P<label>持股比例|同比|环比|增长|下降|毛利率|净利率|占比)?[^\d]{0,10}(?P<value>\d+(?:\.\d+)?)\s*(?P<unit>%|％)")
        .expect("percent regex")
});
static CAPACITY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?P<label>产能|产量|销量|订单数量)?[^\d]{0,10}(?P<value>\d+(?:\.\d+)?)\s*(?P<unit>万吨|吨|万台|台|GW|MW|GWh|MWh)")
        .expect("capacity regex")
});
static DATE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?P<value>20\d{2}[-年/.]\d{1,2}[-月/.]\d{1,2}日?)").expect("date regex")
});
static ACCESS_WALL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(paywall|subscribe to continue|sign in to continue|登录后查看|付费阅读|会员专享|访问受限|robots\.txt)")
        .expect("access-wall regex")
});

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Storage(#[from] astock_storage::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("来源 URL 不符合安全策略：{0}")]
    UnsafeUrl(String),
    #[error("文档解析失败：{0}")]
    Parse(String),
    #[error("来源文档不存在：{0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAuthority {
    RegulatoryExchangeGovernment,
    CompanyDisclosure,
    LicensedMedia,
    Aggregator,
    SocialLead,
    Unknown,
}

impl SourceAuthority {
    pub fn chinese_name(self) -> &'static str {
        match self {
            Self::RegulatoryExchangeGovernment => "监管/交易所/政府一级来源",
            Self::CompanyDisclosure => "公司正式披露",
            Self::LicensedMedia => "授权媒体",
            Self::Aggregator => "聚合快讯",
            Self::SocialLead => "社交线索",
            Self::Unknown => "未分类来源",
        }
    }

    pub fn is_primary(self) -> bool {
        matches!(
            self,
            Self::RegulatoryExchangeGovernment | Self::CompanyDisclosure
        )
    }

    fn token(self) -> &'static str {
        match self {
            Self::RegulatoryExchangeGovernment => "regulatory_exchange_government",
            Self::CompanyDisclosure => "company_disclosure",
            Self::LicensedMedia => "licensed_media",
            Self::Aggregator => "aggregator",
            Self::SocialLead => "social_lead",
            Self::Unknown => "unknown",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "regulatory_exchange_government" => Self::RegulatoryExchangeGovernment,
            "company_disclosure" => Self::CompanyDisclosure,
            "licensed_media" => Self::LicensedMedia,
            "aggregator" => Self::Aggregator,
            "social_lead" => Self::SocialLead,
            _ => Self::Unknown,
        }
    }

    fn reliability(self) -> f64 {
        match self {
            Self::RegulatoryExchangeGovernment => 1.0,
            Self::CompanyDisclosure => 0.95,
            Self::LicensedMedia => 0.78,
            Self::Aggregator => 0.55,
            Self::SocialLead => 0.25,
            Self::Unknown => 0.40,
        }
    }

    fn independence(self) -> f64 {
        match self {
            Self::RegulatoryExchangeGovernment | Self::CompanyDisclosure => 1.0,
            Self::LicensedMedia => 0.80,
            Self::Aggregator => 0.45,
            Self::SocialLead => 0.30,
            Self::Unknown => 0.40,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceScores {
    pub reliability: f64,
    pub independence: f64,
    pub freshness: f64,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceDocumentSummary {
    pub source_document_id: String,
    pub canonical_url: String,
    pub current_version_id: Option<String>,
    pub authority: SourceAuthority,
    pub authority_name: String,
    pub is_primary_source: bool,
    pub access_status: String,
    pub failure_kind: Option<String>,
    pub failure_message: Option<String>,
    pub first_fetched_at: i64,
    pub last_fetched_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceVersion {
    pub source_version_id: String,
    pub source_document_id: String,
    pub canonical_url: String,
    pub content_hash: String,
    pub extracted_hash: String,
    pub media_type: String,
    pub title: Option<String>,
    pub published_at: Option<i64>,
    pub fetched_at: i64,
    pub parser_version: String,
    pub supersedes_version_id: Option<String>,
    pub scores: SourceScores,
    pub authority: SourceAuthority,
    pub authority_name: String,
    pub is_primary_source: bool,
    pub prompt_injection_detected: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceSegment {
    pub segment_id: String,
    pub source_version_id: String,
    pub page_number: Option<u32>,
    pub paragraph_index: usize,
    pub selector: Option<String>,
    pub attachment_id: Option<String>,
    /// Optional PDF page coordinates. They remain `None` when the parser
    /// cannot prove coordinates; callers must never invent a location.
    pub page_x: Option<f64>,
    pub page_y: Option<f64>,
    pub page_width: Option<f64>,
    pub page_height: Option<f64>,
    /// Deterministic table coordinates for HTML/PDF tables when available.
    pub table_index: Option<u32>,
    pub row_index: Option<u32>,
    pub column_index: Option<u32>,
    pub span_start: usize,
    pub span_end: usize,
    pub text: String,
    pub text_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactEvidence {
    pub fact_id: String,
    pub source_version_id: String,
    pub segment_id: String,
    pub fact_type: String,
    pub field_name: String,
    pub subject: Option<String>,
    pub raw_value: String,
    pub normalized_value: Option<f64>,
    pub original_unit: Option<String>,
    pub normalized_unit: Option<String>,
    pub page_number: Option<u32>,
    pub paragraph_index: usize,
    pub span_start: usize,
    pub span_end: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceDocumentDetail {
    pub document: SourceDocumentSummary,
    pub version: Option<SourceVersion>,
    pub segments: Vec<SourceSegment>,
    pub facts: Vec<FactEvidence>,
    pub verification_note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceConflict {
    pub field_name: String,
    pub values: Vec<FactEvidence>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedDocument {
    pub title: Option<String>,
    pub published_at: Option<i64>,
    pub segments: Vec<SourceSegment>,
    pub facts: Vec<FactEvidence>,
    pub extracted_text: String,
    pub dynamic_shell: bool,
    /// `parsed` or `ocr_review_required`. A scan is never treated as empty
    /// verified text and OCR output is never accepted silently.
    pub extraction_status: String,
    pub review_reason: Option<String>,
    pub access_wall: bool,
    pub prompt_injection_detected: bool,
}

pub struct SourceVerifier {
    storage: Storage,
    fetcher: SafeFetcher,
}

impl SourceVerifier {
    pub fn new(storage: Storage) -> Self {
        Self {
            storage,
            fetcher: SafeFetcher::standard(),
        }
    }

    pub fn with_fetcher(storage: Storage, fetcher: SafeFetcher) -> Self {
        Self { storage, fetcher }
    }

    pub async fn fetch_source_document(&self, raw_url: &str) -> Result<SourceDocumentDetail> {
        self.fetch_source_document_with_user_agent(raw_url, None)
            .await
    }

    /// Archive through the same SSRF-safe, bounded fetcher while declaring a
    /// policy-required application identity (for example SEC Fair Access).
    pub async fn fetch_source_document_with_user_agent(
        &self,
        raw_url: &str,
        user_agent: Option<&str>,
    ) -> Result<SourceDocumentDetail> {
        let safe = UrlSecurityPolicy::default()
            .validate_static(raw_url)
            .map_err(|error| Error::UnsafeUrl(error.to_string()))?;
        let requested_url = safe.as_str().to_string();
        let authority = classify_source(&requested_url);
        let fetched_at = now_secs();
        match self
            .fetcher
            .fetch_with_user_agent(&requested_url, user_agent)
            .await
        {
            Ok(fetched) => {
                self.persist_verified(&requested_url, authority, fetched, fetched_at)
                    .await
            }
            Err(error) => {
                let (kind, message) = fetch_failure(&error);
                self.persist_unverified(&requested_url, authority, &kind, &message, fetched_at)
                    .await
            }
        }
    }

    pub async fn read_document(&self, source_version_id: &str) -> Result<SourceDocumentDetail> {
        let version_id = source_version_id.to_string();
        self.storage
            .run(move |conn| read_detail(conn, &version_id))
            .await
            .map_err(Into::into)
    }

    pub async fn recent_documents(&self, limit: usize) -> Result<Vec<SourceDocumentSummary>> {
        let limit = limit.clamp(1, 1_000) as i64;
        self.storage
            .run(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT source_document_id,canonical_url,current_version_id,
                            authority_tier,authority_name,access_status,failure_kind,
                            failure_message,first_fetched_at,last_fetched_at
                     FROM research_source_documents
                     ORDER BY last_fetched_at DESC LIMIT ?1",
                )?;
                let rows = stmt.query_map([limit], map_document)?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn compare_source_evidence(
        &self,
        source_version_ids: &[String],
    ) -> Result<Vec<EvidenceConflict>> {
        let mut by_field: BTreeMap<String, Vec<FactEvidence>> = BTreeMap::new();
        for version in source_version_ids {
            for fact in self.read_document(version).await?.facts {
                by_field
                    .entry(fact.field_name.clone())
                    .or_default()
                    .push(fact);
            }
        }
        Ok(by_field
            .into_iter()
            .filter_map(|(field_name, values)| {
                let distinct = values
                    .iter()
                    .map(|fact| {
                        format!(
                            "{:?}|{}|{:?}",
                            fact.normalized_value, fact.raw_value, fact.normalized_unit
                        )
                    })
                    .collect::<BTreeSet<_>>();
                (distinct.len() > 1).then_some(EvidenceConflict {
                    field_name,
                    values,
                    note: "保留各来源原值、时点与位置；系统不自动选择最有利结果".into(),
                })
            })
            .collect())
    }

    pub async fn link_agent_evidence(
        &self,
        task_id: &str,
        conclusion_key: &str,
        source_version_id: &str,
        fact_id: Option<&str>,
    ) -> Result<()> {
        let task_id = task_id.to_string();
        let conclusion_key = conclusion_key.to_string();
        let source_version_id = source_version_id.to_string();
        let fact_id = fact_id.unwrap_or_default().to_string();
        self.storage
            .run(move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO agent_source_evidence_refs
                     (task_id,conclusion_key,source_version_id,fact_id,created_at)
                     VALUES (?1,?2,?3,?4,?5)",
                    rusqlite::params![
                        task_id,
                        conclusion_key,
                        source_version_id,
                        fact_id,
                        now_secs(),
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn persist_verified(
        &self,
        requested_url: &str,
        authority: SourceAuthority,
        fetched: SafeFetchResult,
        fetched_at: i64,
    ) -> Result<SourceDocumentDetail> {
        let canonical_url = canonicalize_url(&fetched.final_url);
        let document_id = document_id(&canonical_url);
        let mut parsed = parse_source_bytes(
            &canonical_url,
            &fetched.media_type,
            &fetched.body,
            fetched_at,
        )?;
        if parsed.extraction_status == "ocr_review_required" {
            return self
                .persist_unverified(
                    &canonical_url,
                    authority,
                    "ocr_review_required",
                    parsed
                        .review_reason
                        .as_deref()
                        .unwrap_or("扫描型 PDF 无可靠文本层，需要受控 OCR 与人工复核"),
                    fetched_at,
                )
                .await;
        }
        if parsed.access_wall || parsed.dynamic_shell {
            let kind = if parsed.access_wall {
                "access_wall"
            } else {
                "dynamic_page"
            };
            let message = if parsed.access_wall {
                "页面要求登录、订阅或付费，正文未标记为已核验"
            } else {
                "页面只有动态应用外壳，没有可核验正文"
            };
            return self
                .persist_unverified(&canonical_url, authority, kind, message, fetched_at)
                .await;
        }
        let content_hash = sha256(&fetched.body);
        let extracted_hash = sha256(parsed.extracted_text.as_bytes());
        let version_id = format!(
            "srcver:{}",
            &sha256(format!("{document_id}|{content_hash}|{SOURCE_PARSER_VERSION}").as_bytes())
                [..32]
        );
        for segment in &mut parsed.segments {
            segment.source_version_id = version_id.clone();
            segment.segment_id =
                segment_id(&version_id, segment.paragraph_index, &segment.text_hash);
        }
        parsed.facts = extract_facts(&version_id, &parsed.segments);
        let published_at = parsed.published_at;
        let scores = SourceScores {
            reliability: authority.reliability(),
            independence: authority.independence(),
            freshness: freshness_score(published_at, fetched_at),
            note: "评分仅用于排序和风险提示，不能替代原始页面、页码或 span 证据".into(),
        };
        let raw_gzip = authority
            .is_primary()
            .then(|| gzip(&fetched.body))
            .transpose()?;
        let title = parsed.title.clone();
        let media_type = fetched.media_type.clone();
        let redirects = fetched.redirects.clone();
        let version_id_db = version_id.clone();
        let document_id_db = document_id.clone();
        let canonical_db = canonical_url.clone();
        let content_hash_db = content_hash.clone();
        let extracted_hash_db = extracted_hash.clone();
        let scores_db = scores.clone();
        let segments = parsed.segments.clone();
        let facts = parsed.facts.clone();
        let authority_name = authority.chinese_name().to_string();
        let injection = parsed.prompt_injection_detected;
        let requested_url = requested_url.to_string();
        let raw_snapshot_hash = authority.is_primary().then(|| content_hash_db.clone());
        self.storage
            .run(move |conn| {
                let tx = conn.transaction()?;
                let previous: Option<String> = conn_optional(tx.query_row(
                    "SELECT current_version_id FROM research_source_documents
                     WHERE source_document_id=?1",
                    [&document_id_db],
                    |row| row.get(0),
                ))?;
                tx.execute(
                    "INSERT INTO research_source_documents
                     (source_document_id,canonical_url,current_version_id,authority_tier,
                      authority_name,access_status,first_fetched_at,last_fetched_at,
                      created_at,updated_at)
                     VALUES (?1,?2,?3,?4,?5,'verified',?6,?6,?6,?6)
                     ON CONFLICT(source_document_id) DO UPDATE SET
                       canonical_url=excluded.canonical_url,current_version_id=excluded.current_version_id,
                       authority_tier=excluded.authority_tier,authority_name=excluded.authority_name,
                       access_status='verified',failure_kind=NULL,failure_message=NULL,
                       last_fetched_at=excluded.last_fetched_at,updated_at=excluded.updated_at",
                    rusqlite::params![
                        document_id_db,
                        canonical_db,
                        version_id_db,
                        authority.token(),
                        authority_name,
                        fetched_at,
                    ],
                )?;
                tx.execute(
                    "INSERT OR IGNORE INTO research_source_versions
                     (source_version_id,source_document_id,content_hash,extracted_hash,
                      media_type,title,published_at,fetched_at,parser_version,
                      supersedes_version_id,raw_snapshot_gzip,raw_snapshot_hash,
                      reliability_score,independence_score,freshness_score,
                      prompt_injection_detected)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                    rusqlite::params![
                        version_id_db,
                        document_id_db,
                        content_hash_db,
                        extracted_hash_db,
                        media_type,
                        title,
                        published_at,
                        fetched_at,
                        SOURCE_PARSER_VERSION,
                        previous,
                        raw_gzip,
                        raw_snapshot_hash,
                        scores_db.reliability,
                        scores_db.independence,
                        scores_db.freshness,
                        injection,
                    ],
                )?;
                for segment in segments {
                    tx.execute(
                        "INSERT OR IGNORE INTO source_document_segments
                         (segment_id,source_version_id,page_number,paragraph_index,
                          selector,span_start,span_end,text,text_hash,attachment_id,
                          page_x,page_y,page_width,page_height,table_index,row_index,column_index)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                        rusqlite::params![
                            segment.segment_id,
                            segment.source_version_id,
                            segment.page_number.map(i64::from),
                            segment.paragraph_index as i64,
                            segment.selector,
                            segment.span_start as i64,
                            segment.span_end as i64,
                            segment.text,
                            segment.text_hash,
                            segment.attachment_id,
                            segment.page_x,
                            segment.page_y,
                            segment.page_width,
                            segment.page_height,
                            segment.table_index.map(i64::from),
                            segment.row_index.map(i64::from),
                            segment.column_index.map(i64::from),
                        ],
                    )?;
                }
                for fact in facts {
                    tx.execute(
                        "INSERT OR IGNORE INTO source_fact_evidence
                         (fact_id,source_version_id,segment_id,fact_type,field_name,
                          subject,raw_value,normalized_value,original_unit,normalized_unit,
                          page_number,paragraph_index,span_start,span_end,created_at)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                        rusqlite::params![
                            fact.fact_id,
                            fact.source_version_id,
                            fact.segment_id,
                            fact.fact_type,
                            fact.field_name,
                            fact.subject,
                            fact.raw_value,
                            fact.normalized_value,
                            fact.original_unit,
                            fact.normalized_unit,
                            fact.page_number.map(i64::from),
                            fact.paragraph_index as i64,
                            fact.span_start as i64,
                            fact.span_end as i64,
                            fetched_at,
                        ],
                    )?;
                }
                tx.execute(
                    "INSERT INTO source_fetch_observations
                     (source_document_id,source_version_id,requested_url,final_url,
                      media_type,status,redirects_json,fetched_at)
                     VALUES (?1,?2,?3,?4,?5,'verified',?6,?7)",
                    rusqlite::params![
                        document_id_db,
                        version_id_db,
                        requested_url,
                        canonical_db,
                        media_type,
                        serde_json::to_string(&redirects)?,
                        fetched_at,
                    ],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        self.read_document(&version_id).await
    }

    async fn persist_unverified(
        &self,
        canonical_url: &str,
        authority: SourceAuthority,
        failure_kind: &str,
        failure_message: &str,
        fetched_at: i64,
    ) -> Result<SourceDocumentDetail> {
        let canonical_url = canonicalize_url(canonical_url);
        let document_id = document_id(&canonical_url);
        let document_id_db = document_id.clone();
        let canonical_db = canonical_url.clone();
        let kind = failure_kind.to_string();
        let message = failure_message.to_string();
        let authority_name = authority.chinese_name().to_string();
        self.storage
            .run(move |conn| {
                let tx = conn.transaction()?;
                tx.execute(
                    "INSERT INTO research_source_documents
                     (source_document_id,canonical_url,authority_tier,authority_name,
                      access_status,failure_kind,failure_message,first_fetched_at,
                      last_fetched_at,created_at,updated_at)
                     VALUES (?1,?2,?3,?4,'unverified',?5,?6,?7,?7,?7,?7)
                     ON CONFLICT(source_document_id) DO UPDATE SET
                       authority_tier=excluded.authority_tier,authority_name=excluded.authority_name,
                       access_status='unverified',failure_kind=excluded.failure_kind,
                       failure_message=excluded.failure_message,last_fetched_at=excluded.last_fetched_at,
                       updated_at=excluded.updated_at",
                    rusqlite::params![
                        document_id_db,
                        canonical_db,
                        authority.token(),
                        authority_name,
                        kind,
                        message,
                        fetched_at,
                    ],
                )?;
                tx.execute(
                    "INSERT INTO source_fetch_observations
                     (source_document_id,requested_url,status,failure_kind,
                      failure_message,fetched_at)
                     VALUES (?1,?2,'unverified',?3,?4,?5)",
                    rusqlite::params![document_id_db, canonical_db, kind, message, fetched_at],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        Ok(SourceDocumentDetail {
            document: SourceDocumentSummary {
                source_document_id: document_id,
                canonical_url,
                current_version_id: None,
                authority,
                authority_name: authority.chinese_name().into(),
                is_primary_source: authority.is_primary(),
                access_status: "unverified".into(),
                failure_kind: Some(failure_kind.into()),
                failure_message: Some(failure_message.into()),
                first_fetched_at: fetched_at,
                last_fetched_at: fetched_at,
            },
            version: None,
            segments: Vec::new(),
            facts: Vec::new(),
            verification_note:
                "原始页面不可访问或不可解析；不得根据搜索摘要补全正文，也不得标记为【事实】".into(),
        })
    }
}

pub fn parse_source_bytes(
    source_url: &str,
    media_type: &str,
    bytes: &[u8],
    _fetched_at: i64,
) -> Result<ParsedDocument> {
    let (title, published_at, raw_segments, dynamic_shell) = match media_type {
        "text/html" | "application/xhtml+xml" => parse_html(bytes),
        "application/json" => parse_json(bytes),
        "application/pdf" => parse_pdf(bytes)?,
        "text/plain" | "application/xml" | "text/xml" => (
            None,
            None,
            text_segments(&String::from_utf8_lossy(bytes), None),
            false,
        ),
        other => return Err(Error::Parse(format!("不支持的 MIME {other}"))),
    };
    let mut extracted_text = String::new();
    let mut segments = Vec::new();
    for (index, raw) in raw_segments.into_iter().enumerate() {
        let text = normalize_text(&raw.text);
        if text.is_empty() {
            continue;
        }
        let start = extracted_text.len();
        if !extracted_text.is_empty() {
            extracted_text.push('\n');
        }
        let start = if start == 0 { 0 } else { start + 1 };
        extracted_text.extend(
            text.chars()
                .take(MAX_EXTRACTED_CHARS.saturating_sub(extracted_text.chars().count())),
        );
        let end = extracted_text.len();
        let text_hash = sha256(text.as_bytes());
        segments.push(SourceSegment {
            segment_id: String::new(),
            source_version_id: String::new(),
            page_number: raw.page_number,
            paragraph_index: index,
            selector: raw.selector,
            attachment_id: None,
            page_x: raw.page_x,
            page_y: raw.page_y,
            page_width: raw.page_width,
            page_height: raw.page_height,
            table_index: raw.table_index,
            row_index: raw.row_index,
            column_index: raw.column_index,
            span_start: start,
            span_end: end,
            text,
            text_hash,
        });
        if extracted_text.chars().count() >= MAX_EXTRACTED_CHARS {
            break;
        }
    }
    let access_wall = ACCESS_WALL.is_match(&extracted_text);
    let inspected =
        inspect_external_text(source_url, media_type, &extracted_text, MAX_EXTRACTED_CHARS);
    let scanned_pdf = media_type == "application/pdf" && segments.is_empty() && !bytes.is_empty();
    let dynamic_shell = dynamic_shell || (!scanned_pdf && segments.is_empty() && !bytes.is_empty());
    let mut parsed = ParsedDocument {
        title,
        published_at,
        segments,
        facts: Vec::new(),
        extracted_text,
        dynamic_shell,
        extraction_status: if scanned_pdf {
            "ocr_review_required"
        } else {
            "parsed"
        }
        .into(),
        review_reason: scanned_pdf
            .then(|| "PDF 没有可验证文本层；已暂停结构化抽取，等待受控 OCR 与人工复核".into()),
        access_wall,
        prompt_injection_detected: inspected.prompt_injection_detected,
    };
    // Fixture parsing has no persistent version yet, but still exposes exact
    // spans for deterministic parser tests.
    let provisional = format!("fixture:{}", sha256(bytes));
    for segment in &mut parsed.segments {
        segment.source_version_id = provisional.clone();
        segment.segment_id = segment_id(&provisional, segment.paragraph_index, &segment.text_hash);
    }
    parsed.facts = extract_facts(&provisional, &parsed.segments);
    Ok(parsed)
}

#[derive(Debug)]
struct RawSegment {
    page_number: Option<u32>,
    selector: Option<String>,
    page_x: Option<f64>,
    page_y: Option<f64>,
    page_width: Option<f64>,
    page_height: Option<f64>,
    table_index: Option<u32>,
    row_index: Option<u32>,
    column_index: Option<u32>,
    text: String,
}

type ParsedParts = (Option<String>, Option<i64>, Vec<RawSegment>, bool);

fn parse_html(bytes: &[u8]) -> (Option<String>, Option<i64>, Vec<RawSegment>, bool) {
    let source = String::from_utf8_lossy(bytes);
    let document = Html::parse_document(&source);
    let content_selector = Selector::parse("h1,h2,h3,p,li").unwrap();
    let title_selector = Selector::parse("title,h1").unwrap();
    let meta_selector = Selector::parse("meta").unwrap();
    let title = document.select(&title_selector).find_map(|node| {
        let value = normalize_text(&node.text().collect::<Vec<_>>().join(" "));
        (!value.is_empty()).then_some(value)
    });
    let published_at = document.select(&meta_selector).find_map(|node| {
        let value = node.value();
        let key = value
            .attr("property")
            .or_else(|| value.attr("name"))
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(
            key.as_str(),
            "article:published_time" | "publishdate" | "date" | "datepublished"
        ) {
            value.attr("content").and_then(parse_time)
        } else {
            None
        }
    });
    let mut seen = BTreeSet::new();
    let mut segments = document
        .select(&content_selector)
        .filter_map(|node| {
            let text = normalize_text(&node.text().collect::<Vec<_>>().join(" "));
            if text.chars().count() < 2 || !seen.insert(text.clone()) {
                return None;
            }
            Some(RawSegment {
                page_number: None,
                selector: Some(node.value().name().into()),
                page_x: None,
                page_y: None,
                page_width: None,
                page_height: None,
                table_index: None,
                row_index: None,
                column_index: None,
                text,
            })
        })
        .collect::<Vec<_>>();
    // Preserve exact table cell coordinates instead of flattening every cell
    // into an indistinguishable paragraph.
    let table_selector = Selector::parse("table").unwrap();
    let row_selector = Selector::parse("tr").unwrap();
    let cell_selector = Selector::parse("th,td").unwrap();
    for (table_index, table) in document.select(&table_selector).enumerate() {
        for (row_index, row) in table.select(&row_selector).enumerate() {
            for (column_index, cell) in row.select(&cell_selector).enumerate() {
                let text = normalize_text(&cell.text().collect::<Vec<_>>().join(" "));
                if text.is_empty() {
                    continue;
                }
                segments.push(RawSegment {
                    page_number: None,
                    selector: Some(format!(
                        "table:nth-of-type({}) tr:nth-of-type({}) {}:nth-of-type({})",
                        table_index + 1,
                        row_index + 1,
                        cell.value().name(),
                        column_index + 1
                    )),
                    page_x: None,
                    page_y: None,
                    page_width: None,
                    page_height: None,
                    table_index: Some(table_index as u32),
                    row_index: Some(row_index as u32),
                    column_index: Some(column_index as u32),
                    text,
                });
            }
        }
    }
    let script_selector = Selector::parse("script").unwrap();
    let dynamic_shell = segments.is_empty() && document.select(&script_selector).count() > 0;
    (title, published_at, segments, dynamic_shell)
}

fn parse_json(bytes: &[u8]) -> (Option<String>, Option<i64>, Vec<RawSegment>, bool) {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return (None, None, Vec::new(), false);
    };
    let title = value
        .get("title")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let published_at = ["published_at", "publish_time", "date"]
        .iter()
        .find_map(|key| value.get(key).and_then(json_time));
    let mut segments = Vec::new();
    flatten_json("$", &value, &mut segments);
    (title, published_at, segments, false)
}

fn flatten_json(path: &str, value: &serde_json::Value, output: &mut Vec<RawSegment>) {
    if output.len() >= 5_000 {
        return;
    }
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                flatten_json(&format!("{path}.{key}"), value, output);
            }
        }
        serde_json::Value::Array(values) => {
            for (index, value) in values.iter().enumerate().take(1_000) {
                flatten_json(&format!("{path}[{index}]"), value, output);
            }
        }
        serde_json::Value::Null => {}
        primitive => output.push(RawSegment {
            page_number: None,
            selector: Some(path.into()),
            page_x: None,
            page_y: None,
            page_width: None,
            page_height: None,
            table_index: None,
            row_index: None,
            column_index: None,
            text: primitive
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| primitive.to_string()),
        }),
    }
}

fn parse_pdf(bytes: &[u8]) -> Result<ParsedParts> {
    let document = lopdf::Document::load_mem(bytes)
        .map_err(|error| Error::Parse(format!("PDF 结构无效：{error}")))?;
    let mut segments = Vec::new();
    for page_number in document.get_pages().keys().copied().take(500) {
        let text = document
            .extract_text(&[page_number])
            .map_err(|error| Error::Parse(format!("PDF 第 {page_number} 页提取失败：{error}")))?;
        segments.extend(text_segments(&text, Some(page_number)));
    }
    Ok((None, None, segments, false))
}

fn text_segments(text: &str, page_number: Option<u32>) -> Vec<RawSegment> {
    text.lines()
        .map(normalize_text)
        .filter(|line| !line.is_empty())
        .map(|text| RawSegment {
            page_number,
            selector: None,
            page_x: None,
            page_y: None,
            page_width: None,
            page_height: None,
            table_index: None,
            row_index: None,
            column_index: None,
            text,
        })
        .collect()
}

fn extract_facts(source_version_id: &str, segments: &[SourceSegment]) -> Vec<FactEvidence> {
    let mut output = Vec::new();
    for segment in segments {
        for (fact_type, pattern) in [
            ("money", &*MONEY),
            ("percentage", &*PERCENT),
            ("capacity", &*CAPACITY),
            ("date", &*DATE),
        ] {
            for captures in pattern.captures_iter(&segment.text).take(100) {
                let Some(matched) = captures.get(0) else {
                    continue;
                };
                let raw_value = captures
                    .name("value")
                    .map(|value| value.as_str())
                    .unwrap_or(matched.as_str())
                    .to_string();
                let unit = captures
                    .name("unit")
                    .map(|value| value.as_str().to_string());
                let label = captures
                    .name("label")
                    .map(|value| value.as_str())
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| infer_label(&segment.text, matched.end(), fact_type));
                let (normalized_value, normalized_unit) =
                    normalize_fact(fact_type, &raw_value, unit.as_deref());
                let start = segment.span_start + matched.start();
                let end = segment.span_start + matched.end();
                let fact_id = format!(
                    "fact:{}",
                    &sha256(
                        format!(
                            "{source_version_id}|{}|{start}|{end}|{label}",
                            segment.segment_id
                        )
                        .as_bytes()
                    )[..28]
                );
                output.push(FactEvidence {
                    fact_id,
                    source_version_id: source_version_id.into(),
                    segment_id: segment.segment_id.clone(),
                    fact_type: fact_type.into(),
                    field_name: label,
                    subject: subject_before(&segment.text, matched.start()),
                    raw_value,
                    normalized_value,
                    original_unit: unit,
                    normalized_unit,
                    page_number: segment.page_number,
                    paragraph_index: segment.paragraph_index,
                    span_start: start,
                    span_end: end,
                });
            }
        }
    }
    output
}

fn infer_label(text: &str, start: usize, fallback: &str) -> String {
    let prefix = text.get(..start).unwrap_or_default();
    [
        "订单金额",
        "合同金额",
        "中标金额",
        "处罚金额",
        "罚款",
        "营业收入",
        "营收",
        "净利润",
        "投资金额",
        "回购金额",
        "持股比例",
        "同比",
        "环比",
        "增长",
        "下降",
        "毛利率",
        "净利率",
        "占比",
        "产能",
        "产量",
        "销量",
        "订单数量",
    ]
    .iter()
    .filter_map(|label| prefix.rfind(label).map(|position| (position, *label)))
    .max_by_key(|(position, _)| *position)
    .filter(|(position, _)| prefix.len().saturating_sub(*position) <= 36)
    .map(|(_, label)| label.to_string())
    .unwrap_or_else(|| fallback.to_string())
}

fn normalize_fact(
    fact_type: &str,
    raw_value: &str,
    unit: Option<&str>,
) -> (Option<f64>, Option<String>) {
    if fact_type == "date" {
        return (None, Some("date".into()));
    }
    let Ok(value) = raw_value.parse::<f64>() else {
        return (None, unit.map(str::to_string));
    };
    match unit.unwrap_or_default() {
        "亿元" => (Some(value * 100_000_000.0), Some("元".into())),
        "万元" => (Some(value * 10_000.0), Some("元".into())),
        "元" => (Some(value), Some("元".into())),
        "%" | "％" => (Some(value / 100.0), Some("比例".into())),
        "万吨" => (Some(value * 10_000.0), Some("吨".into())),
        "万台" => (Some(value * 10_000.0), Some("台".into())),
        "GW" => (Some(value * 1_000.0), Some("MW".into())),
        "GWh" => (Some(value * 1_000.0), Some("MWh".into())),
        other => (Some(value), (!other.is_empty()).then(|| other.into())),
    }
}

fn subject_before(text: &str, start: usize) -> Option<String> {
    let prefix = text.get(..start).unwrap_or_default();
    let subject = prefix
        .rsplit(['。', '；', ';', '，', ','])
        .next()
        .unwrap_or_default()
        .chars()
        .rev()
        .take(40)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    (!subject.trim().is_empty()).then(|| subject.trim().to_string())
}

pub fn classify_source(raw_url: &str) -> SourceAuthority {
    let host = Url::parse(raw_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    if [
        "cninfo.com.cn",
        "sse.com.cn",
        "szse.cn",
        "bse.cn",
        "csrc.gov.cn",
        "stats.gov.cn",
        "gov.cn",
        "pbc.gov.cn",
        "sec.gov",
        "federalreserve.gov",
        "bls.gov",
        "bea.gov",
        "eia.gov",
        "cftc.gov",
        "bis.gov",
        "ustr.gov",
        "worldbank.org",
        "imf.org",
        "ecb.europa.eu",
        "europa.eu",
        "un.org",
        "wto.org",
        "fsa.go.jp",
        "fss.or.kr",
        "twse.com.tw",
        "hkexnews.hk",
        "opec.org",
        "iea.org",
    ]
    .iter()
    .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    {
        SourceAuthority::RegulatoryExchangeGovernment
    } else if ["moutaichina.com", "catl.com", "bydglobal.com", "pingan.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    {
        SourceAuthority::CompanyDisclosure
    } else if ["reuters.com", "bloomberg.com", "caixin.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    {
        SourceAuthority::LicensedMedia
    } else if ["cls.cn", "jin10.com", "wallstreetcn.com", "gelonghui.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    {
        SourceAuthority::Aggregator
    } else if ["weibo.com", "x.com", "twitter.com", "stocktwits.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    {
        SourceAuthority::SocialLead
    } else {
        SourceAuthority::Unknown
    }
}

fn read_detail(
    conn: &rusqlite::Connection,
    version_id: &str,
) -> astock_storage::Result<SourceDocumentDetail> {
    let version = conn.query_row(
        "SELECT v.source_version_id,v.source_document_id,d.canonical_url,v.content_hash,
                v.extracted_hash,v.media_type,v.title,v.published_at,v.fetched_at,
                v.parser_version,v.supersedes_version_id,v.reliability_score,
                v.independence_score,v.freshness_score,d.authority_tier,d.authority_name,
                v.prompt_injection_detected
         FROM research_source_versions v JOIN research_source_documents d
           ON d.source_document_id=v.source_document_id WHERE v.source_version_id=?1",
        [version_id],
        |row| {
            let authority_text: String = row.get(14)?;
            let authority = SourceAuthority::parse(&authority_text);
            Ok(SourceVersion {
                source_version_id: row.get(0)?,
                source_document_id: row.get(1)?,
                canonical_url: row.get(2)?,
                content_hash: row.get(3)?,
                extracted_hash: row.get(4)?,
                media_type: row.get(5)?,
                title: row.get(6)?,
                published_at: row.get(7)?,
                fetched_at: row.get(8)?,
                parser_version: row.get(9)?,
                supersedes_version_id: row.get(10)?,
                scores: SourceScores {
                    reliability: row.get(11)?,
                    independence: row.get(12)?,
                    freshness: row.get(13)?,
                    note: "评分不替代原始证据".into(),
                },
                authority,
                authority_name: row.get(15)?,
                is_primary_source: authority.is_primary(),
                prompt_injection_detected: row.get(16)?,
            })
        },
    )?;
    let document = conn.query_row(
        "SELECT source_document_id,canonical_url,current_version_id,
                authority_tier,authority_name,access_status,failure_kind,
                failure_message,first_fetched_at,last_fetched_at
         FROM research_source_documents WHERE source_document_id=?1",
        [&version.source_document_id],
        map_document,
    )?;
    let mut segment_stmt = conn.prepare(
        "SELECT segment_id,source_version_id,page_number,paragraph_index,
                selector,span_start,span_end,text,text_hash,attachment_id,
                page_x,page_y,page_width,page_height,table_index,row_index,column_index
         FROM source_document_segments WHERE source_version_id=?1 ORDER BY paragraph_index",
    )?;
    let segments = segment_stmt
        .query_map([version_id], |row| {
            Ok(SourceSegment {
                segment_id: row.get(0)?,
                source_version_id: row.get(1)?,
                page_number: row.get::<_, Option<i64>>(2)?.map(|value| value as u32),
                paragraph_index: row.get::<_, i64>(3)? as usize,
                selector: row.get(4)?,
                attachment_id: row.get(9)?,
                page_x: row.get(10)?,
                page_y: row.get(11)?,
                page_width: row.get(12)?,
                page_height: row.get(13)?,
                table_index: row.get::<_, Option<i64>>(14)?.map(|value| value as u32),
                row_index: row.get::<_, Option<i64>>(15)?.map(|value| value as u32),
                column_index: row.get::<_, Option<i64>>(16)?.map(|value| value as u32),
                span_start: row.get::<_, i64>(5)? as usize,
                span_end: row.get::<_, i64>(6)? as usize,
                text: row.get(7)?,
                text_hash: row.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut fact_stmt = conn.prepare(
        "SELECT fact_id,source_version_id,segment_id,fact_type,field_name,subject,
                raw_value,normalized_value,original_unit,normalized_unit,page_number,
                paragraph_index,span_start,span_end
         FROM source_fact_evidence WHERE source_version_id=?1 ORDER BY paragraph_index,span_start",
    )?;
    let facts = fact_stmt
        .query_map([version_id], |row| {
            Ok(FactEvidence {
                fact_id: row.get(0)?,
                source_version_id: row.get(1)?,
                segment_id: row.get(2)?,
                fact_type: row.get(3)?,
                field_name: row.get(4)?,
                subject: row.get(5)?,
                raw_value: row.get(6)?,
                normalized_value: row.get(7)?,
                original_unit: row.get(8)?,
                normalized_unit: row.get(9)?,
                page_number: row.get::<_, Option<i64>>(10)?.map(|value| value as u32),
                paragraph_index: row.get::<_, i64>(11)? as usize,
                span_start: row.get::<_, i64>(12)? as usize,
                span_end: row.get::<_, i64>(13)? as usize,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(SourceDocumentDetail {
        document,
        version: Some(version),
        segments,
        facts,
        verification_note:
            "已读取原始来源；引用数字时仍须携带 source_version_id、fact_id 和页码/段落/span".into(),
    })
}

fn map_document(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceDocumentSummary> {
    let authority_text: String = row.get(3)?;
    let authority = SourceAuthority::parse(&authority_text);
    Ok(SourceDocumentSummary {
        source_document_id: row.get(0)?,
        canonical_url: row.get(1)?,
        current_version_id: row.get(2)?,
        authority,
        authority_name: row.get(4)?,
        is_primary_source: authority.is_primary(),
        access_status: row.get(5)?,
        failure_kind: row.get(6)?,
        failure_message: row.get(7)?,
        first_fetched_at: row.get(8)?,
        last_fetched_at: row.get(9)?,
    })
}

fn fetch_failure(error: &SafeFetchError) -> (String, String) {
    let kind = match error {
        SafeFetchError::Http(status) if status.as_u16() == 401 => "login_required",
        SafeFetchError::Http(status) if status.as_u16() == 402 => "paywall",
        SafeFetchError::Http(status) if status.as_u16() == 403 => "robots_or_forbidden",
        SafeFetchError::Http(_) => "http_error",
        SafeFetchError::TooLarge { .. } => "too_large",
        SafeFetchError::Mime(_) => "unsupported_mime",
        SafeFetchError::Request(_) => "network_error",
        _ => "security_or_fetch_error",
    };
    (kind.into(), error.to_string())
}

fn freshness_score(published_at: Option<i64>, fetched_at: i64) -> f64 {
    let Some(published_at) = published_at else {
        return 0.5;
    };
    let age_days = (fetched_at - published_at).max(0) as f64 / 86_400.0;
    (1.0 - age_days / 30.0).clamp(0.0, 1.0)
}

fn parse_time(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.timestamp())
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .map(|time| time.and_utc().timestamp())
        })
}

fn json_time(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .map(|time| {
            if time > 10_000_000_000 {
                time / 1_000
            } else {
                time
            }
        })
        .or_else(|| value.as_str().and_then(parse_time))
}

fn canonicalize_url(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        return raw.to_string();
    };
    url.set_fragment(None);
    let tracking = ["utm_source", "utm_medium", "utm_campaign", "from", "source"];
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
    url.to_string()
}

fn document_id(url: &str) -> String {
    format!("srcdoc:{}", &sha256(url.as_bytes())[..28])
}

fn segment_id(version: &str, paragraph: usize, hash: &str) -> String {
    format!(
        "segment:{}",
        &sha256(format!("{version}|{paragraph}|{hash}").as_bytes())[..28]
    )
}

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn gzip(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(bytes)
        .map_err(|error| Error::Parse(error.to_string()))?;
    encoder
        .finish()
        .map_err(|error| Error::Parse(error.to_string()))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn conn_optional<T>(result: rusqlite::Result<T>) -> rusqlite::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_storage::StorageConfig;

    #[test]
    fn html_and_json_keep_exact_segments_units_and_dynamic_state() {
        let html = include_str!("../tests/fixtures/announcement.html");
        let parsed = parse_source_bytes(
            "https://www.sse.com.cn/a",
            "text/html",
            html.as_bytes(),
            1_800_000_000,
        )
        .unwrap();
        assert!(!parsed.dynamic_shell);
        assert!(parsed.facts.iter().any(|fact| fact.field_name == "回购金额"
            && fact.normalized_value == Some(1_000_000_000.0)
            && fact.original_unit.as_deref() == Some("亿元")));
        assert!(parsed
            .facts
            .iter()
            .all(|fact| fact.span_end > fact.span_start));
        assert!(parsed.segments.iter().any(|segment| {
            segment.table_index == Some(0)
                && segment.row_index == Some(1)
                && segment.column_index == Some(1)
                && segment.text == "20万吨"
        }));

        let json = include_str!("../tests/fixtures/source.json");
        let parsed = parse_source_bytes(
            "https://example.com/a.json",
            "application/json",
            json.as_bytes(),
            1_800_000_000,
        )
        .unwrap();
        assert!(parsed
            .segments
            .iter()
            .any(|segment| segment.selector.as_deref() == Some("$.contract.订单金额")));

        let shell = parse_source_bytes(
            "https://example.com/app",
            "text/html",
            include_bytes!("../tests/fixtures/dynamic_page.html"),
            1,
        )
        .unwrap();
        assert!(shell.dynamic_shell);
    }

    #[test]
    fn pdf_table_fixture_retains_page_number_and_original_unit() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/pdf_table_rows.json")).unwrap();
        let rows = fixture["rows"].as_array().unwrap();
        let mut document = lopdf::Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(lopdf::dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica"
        });
        let resources_id = document.add_object(lopdf::dictionary! {
            "Font" => lopdf::dictionary! { "F1" => font_id }
        });
        let mut operations = vec![
            lopdf::content::Operation::new("BT", vec![]),
            lopdf::content::Operation::new("Tf", vec!["F1".into(), 12.into()]),
            lopdf::content::Operation::new("Td", vec![20.into(), 800.into()]),
        ];
        for (index, row) in rows.iter().enumerate() {
            if index > 0 {
                operations.push(lopdf::content::Operation::new(
                    "Td",
                    vec![0.into(), (-20).into()],
                ));
            }
            operations.push(lopdf::content::Operation::new(
                "Tj",
                vec![lopdf::Object::string_literal(row.as_str().unwrap())],
            ));
        }
        operations.push(lopdf::content::Operation::new("ET", vec![]));
        let content = lopdf::content::Content { operations };
        let content_id = document.add_object(lopdf::Stream::new(
            lopdf::dictionary! {},
            content.encode().unwrap(),
        ));
        let page_id = document.add_object(lopdf::dictionary! {
            "Type" => "Page", "Parent" => pages_id,
            "Contents" => content_id, "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()]
        });
        document.objects.insert(
            pages_id,
            lopdf::Object::Dictionary(lopdf::dictionary! {
                "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1
            }),
        );
        let catalog_id =
            document.add_object(lopdf::dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        document.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        let parsed =
            parse_source_bytes("https://www.sse.com.cn/a.pdf", "application/pdf", &bytes, 1)
                .unwrap();
        assert!(parsed
            .segments
            .iter()
            .any(|segment| segment.page_number == Some(1)));
        assert!(parsed.facts.iter().any(|fact| {
            fact.fact_type == "capacity"
                && fact.page_number == Some(1)
                && fact.original_unit.as_deref() == Some("GW")
        }));
    }

    #[test]
    fn image_only_pdf_is_stopped_for_controlled_ocr_review() {
        let mut document = lopdf::Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let page_id = document.add_object(lopdf::dictionary! {
            "Type" => "Page", "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()]
        });
        document.objects.insert(
            pages_id,
            lopdf::Object::Dictionary(lopdf::dictionary! {
                "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1
            }),
        );
        let catalog_id =
            document.add_object(lopdf::dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        document.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        let parsed = parse_source_bytes(
            "https://www.cninfo.com.cn/scan.pdf",
            "application/pdf",
            &bytes,
            1,
        )
        .unwrap();
        assert_eq!(parsed.extraction_status, "ocr_review_required");
        assert!(parsed.review_reason.is_some());
        assert!(parsed.segments.is_empty());
        assert!(!parsed.dynamic_shell);
    }

    #[test]
    fn source_priority_and_access_wall_are_explicit() {
        assert!(classify_source("https://www.sse.com.cn/a.pdf").is_primary());
        assert!(classify_source("https://data.sec.gov/submissions/a.json").is_primary());
        assert!(classify_source("https://api.worldbank.org/v2/a").is_primary());
        assert_eq!(
            classify_source("https://weibo.com/a"),
            SourceAuthority::SocialLead
        );
        let parsed = parse_source_bytes(
            "https://example.com/paywall",
            "text/html",
            include_bytes!("../tests/fixtures/paywall.html"),
            1,
        )
        .unwrap();
        assert!(parsed.access_wall);
    }

    #[tokio::test]
    async fn official_revision_chain_and_unverified_observation_persist() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let verifier = SourceVerifier::new(storage.clone());
        let first = verifier
            .persist_verified(
                "https://www.sse.com.cn/disclosure/a.html",
                SourceAuthority::RegulatoryExchangeGovernment,
                SafeFetchResult {
                    final_url: "https://www.sse.com.cn/disclosure/a.html".into(),
                    media_type: "text/html".into(),
                    body: "<p>公司合同金额1亿元。</p>".as_bytes().to_vec(),
                    redirects: Vec::new(),
                },
                1_800_000_000,
            )
            .await
            .unwrap();
        let first_version = first.version.unwrap();
        assert!(first_version.is_primary_source);
        assert_eq!(first.facts[0].page_number, None);
        verifier
            .link_agent_evidence(
                "task-source-1",
                "final_answer",
                &first_version.source_version_id,
                Some(&first.facts[0].fact_id),
            )
            .await
            .unwrap();
        let linked = storage
            .run(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM agent_source_evidence_refs WHERE task_id='task-source-1'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(Into::into)
            })
            .await
            .unwrap();
        assert_eq!(linked, 1);

        let second = verifier
            .persist_verified(
                "https://www.sse.com.cn/disclosure/a.html",
                SourceAuthority::RegulatoryExchangeGovernment,
                SafeFetchResult {
                    final_url: "https://www.sse.com.cn/disclosure/a.html".into(),
                    media_type: "text/html".into(),
                    body: "<p>更正：公司合同金额2亿元。</p>".as_bytes().to_vec(),
                    redirects: Vec::new(),
                },
                1_800_000_100,
            )
            .await
            .unwrap();
        assert_eq!(
            second
                .version
                .as_ref()
                .unwrap()
                .supersedes_version_id
                .as_deref(),
            Some(first_version.source_version_id.as_str())
        );
        let conflicts = verifier
            .compare_source_evidence(&[
                first_version.source_version_id,
                second.version.unwrap().source_version_id,
            ])
            .await
            .unwrap();
        assert!(conflicts
            .iter()
            .any(|conflict| conflict.field_name == "合同金额"));

        let failed = verifier
            .persist_unverified(
                "https://example.com/paywall",
                SourceAuthority::Unknown,
                "paywall",
                "需要订阅",
                1_800_000_200,
            )
            .await
            .unwrap();
        assert_eq!(failed.document.access_status, "unverified");
        assert!(failed.version.is_none());
    }
}
