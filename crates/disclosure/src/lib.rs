//! Canonical, auditable formal-disclosure data plane.
//!
//! The crate deliberately separates an upstream *entry point* from the
//! canonical disclosure. A media mirror may discover an announcement, but it
//! is never upgraded to an exchange/issuer primary source without an official
//! URL and an archived source version.

use std::collections::BTreeSet;

use astock_storage::Storage;
use regex::Regex;
use rusqlite::{params, params_from_iter, types::Value as SqlValue, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const DISCLOSURE_PARSER_VERSION: &str = "formal-disclosure-v1";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Storage(#[from] astock_storage::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("披露记录缺少标题")]
    EmptyTitle,
    #[error("披露记录不存在：{0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuthority {
    Exchange,
    Regulator,
    Issuer,
    MirrorDiscovery,
}

impl ProviderAuthority {
    pub fn token(self) -> &'static str {
        match self {
            Self::Exchange => "exchange",
            Self::Regulator => "regulator",
            Self::Issuer => "issuer",
            Self::MirrorDiscovery => "mirror_discovery",
        }
    }

    pub fn chinese_name(self) -> &'static str {
        match self {
            Self::Exchange => "交易所正式披露",
            Self::Regulator => "监管机构正式披露",
            Self::Issuer => "上市公司投资者关系披露",
            Self::MirrorDiscovery => "公告镜像（仅作发现）",
        }
    }

    pub fn is_primary(self) -> bool {
        !matches!(self, Self::MirrorDiscovery)
    }

    fn parse(value: &str) -> Self {
        match value {
            "exchange" => Self::Exchange,
            "regulator" => Self::Regulator,
            "issuer" => Self::Issuer,
            _ => Self::MirrorDiscovery,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDefinition {
    pub provider_id: &'static str,
    pub name: &'static str,
    pub authority: ProviderAuthority,
    pub public_index_url: &'static str,
    pub target_latency_secs: u32,
    pub rate_limit_per_minute: u32,
    pub note: &'static str,
}

/// The registry is configuration, not a claim that every endpoint succeeded.
/// Runtime health lives in `disclosure_provider_state` and is visible to users.
pub fn official_provider_catalog() -> Vec<ProviderDefinition> {
    vec![
        ProviderDefinition {
            provider_id: "sse",
            name: "上海证券交易所",
            authority: ProviderAuthority::Exchange,
            public_index_url: "https://www.sse.com.cn/disclosure/listedinfo/announcement/",
            target_latency_secs: 300,
            rate_limit_per_minute: 20,
            note: "按上交所公开规则访问；失败时保留游标并退避",
        },
        ProviderDefinition {
            provider_id: "szse",
            name: "深圳证券交易所",
            authority: ProviderAuthority::Exchange,
            public_index_url: "https://www.szse.cn/disclosure/listed/notice/",
            target_latency_secs: 300,
            rate_limit_per_minute: 20,
            note: "按深交所公开规则访问；失败时不以镜像冒充原文",
        },
        ProviderDefinition {
            provider_id: "bse",
            name: "北京证券交易所",
            authority: ProviderAuthority::Exchange,
            public_index_url: "https://www.bse.cn/disclosure/announcement.html",
            target_latency_secs: 300,
            rate_limit_per_minute: 15,
            note: "按北交所公开规则访问",
        },
        ProviderDefinition {
            provider_id: "cninfo",
            name: "巨潮资讯",
            authority: ProviderAuthority::Exchange,
            public_index_url: "https://www.cninfo.com.cn/new/index",
            target_latency_secs: 300,
            rate_limit_per_minute: 20,
            note: "法定信息披露入口；原文与附件分别归档",
        },
        ProviderDefinition {
            provider_id: "csrc",
            name: "中国证监会",
            authority: ProviderAuthority::Regulator,
            public_index_url: "https://www.csrc.gov.cn/csrc/c100028/common_list.shtml",
            target_latency_secs: 900,
            rate_limit_per_minute: 10,
            note: "监管公告、处罚与问询事项",
        },
        ProviderDefinition {
            provider_id: "issuer_ir",
            name: "上市公司投资者关系网站",
            authority: ProviderAuthority::Issuer,
            public_index_url: "",
            target_latency_secs: 1_800,
            rate_limit_per_minute: 6,
            note: "仅访问证券主数据中已配置且经允许的公司 IR 入口",
        },
        ProviderDefinition {
            provider_id: "eastmoney_notice_mirror",
            name: "东方财富公告镜像",
            authority: ProviderAuthority::MirrorDiscovery,
            public_index_url: "https://data.eastmoney.com/notices/",
            target_latency_secs: 600,
            rate_limit_per_minute: 30,
            note: "只用于补漏和发现；不能提升 Agent 结论置信度",
        },
    ]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisclosureSecurity {
    pub code: String,
    pub name: String,
    pub market: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisclosureAttachmentInput {
    pub name: String,
    pub original_url: String,
    pub media_type: String,
    pub parent_url: Option<String>,
    pub byte_size: Option<u64>,
    pub content_hash: Option<String>,
    pub source_version_id: Option<String>,
    pub page_count: Option<u32>,
    pub extraction_status: String,
    pub review_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisclosureInput {
    pub provider_id: String,
    pub provider_name: String,
    pub authority: ProviderAuthority,
    pub entry_kind: String,
    pub upstream_id: Option<String>,
    pub original_url: String,
    pub title: String,
    pub published_at: Option<i64>,
    pub publication_precision: String,
    pub first_seen_at: i64,
    pub latency_ms: Option<u64>,
    pub securities: Vec<DisclosureSecurity>,
    pub attachments: Vec<DisclosureAttachmentInput>,
    pub source_version_id: Option<String>,
    pub extracted_text: Option<String>,
    pub extraction_status: String,
    pub review_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisclosureSource {
    pub source_id: String,
    pub provider_id: String,
    pub provider_name: String,
    pub authority: ProviderAuthority,
    pub authority_name: String,
    pub entry_kind: String,
    pub upstream_id: Option<String>,
    pub original_url: String,
    pub discovered_at: i64,
    pub latency_ms: Option<u64>,
    pub is_primary: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisclosureAttachment {
    pub attachment_id: String,
    pub parent_attachment_id: Option<String>,
    pub name: String,
    pub original_url: String,
    pub media_type: String,
    pub byte_size: Option<u64>,
    pub content_hash: Option<String>,
    pub source_version_id: Option<String>,
    pub extraction_status: String,
    pub page_count: Option<u32>,
    pub parser_version: String,
    pub review_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredEvent {
    pub event_id: String,
    pub event_type: String,
    pub fields: serde_json::Value,
    pub evidence: serde_json::Value,
    pub parser_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisclosureListItem {
    pub disclosure_id: String,
    pub title: String,
    pub category: String,
    pub category_name: String,
    pub status: String,
    pub status_name: String,
    pub published_at: Option<i64>,
    pub publication_precision: String,
    pub first_seen_at: i64,
    pub discovery_latency_secs: Option<i64>,
    pub revision_of: Option<String>,
    pub cancelled_by: Option<String>,
    pub source_version_id: Option<String>,
    pub extraction_status: String,
    pub review_reason: Option<String>,
    pub securities: Vec<DisclosureSecurity>,
    pub sources: Vec<DisclosureSource>,
    pub primary_verified: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisclosureDetail {
    #[serde(flatten)]
    pub item: DisclosureListItem,
    pub attachments: Vec<DisclosureAttachment>,
    pub events: Vec<StructuredEvent>,
    pub revisions: Vec<DisclosureListItem>,
    pub verification_note: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DisclosureQuery {
    pub security_code: Option<String>,
    pub keyword: Option<String>,
    pub category: Option<String>,
    pub status: Option<String>,
    pub primary_only: bool,
    pub from_utc: Option<i64>,
    pub to_utc: Option<i64>,
    pub page: u32,
    pub page_size: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisclosurePage {
    pub items: Vec<DisclosureListItem>,
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
    pub total_pages: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestOutcome {
    pub disclosure_id: String,
    pub inserted: bool,
    pub duplicate_entry: bool,
    pub status: String,
    pub linked_revision: Option<String>,
    pub structured_event_count: usize,
}

#[derive(Clone)]
pub struct DisclosureStore {
    storage: Storage,
}

impl DisclosureStore {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// Share the same storage worker with source archiving in host layers.
    pub fn storage_clone(&self) -> Storage {
        self.storage.clone()
    }

    pub async fn seed_provider_catalog(&self) -> Result<()> {
        let catalog = official_provider_catalog();
        self.storage.run(move |conn| {
            let now = now_secs();
            for provider in catalog {
                conn.execute(
                    "INSERT INTO disclosure_provider_state
                     (provider_id,provider_name,authority,target_latency_secs,updated_at)
                     VALUES (?1,?2,?3,?4,?5)
                     ON CONFLICT(provider_id) DO UPDATE SET
                       provider_name=excluded.provider_name,authority=excluded.authority,
                       target_latency_secs=excluded.target_latency_secs,updated_at=excluded.updated_at",
                    params![provider.provider_id, provider.name, provider.authority.token(), provider.target_latency_secs, now],
                )?;
            }
            Ok(())
        }).await?;
        Ok(())
    }

    pub async fn ingest(&self, mut input: DisclosureInput) -> Result<IngestOutcome> {
        input.title = clean_text(&input.title);
        if input.title.is_empty() {
            return Err(Error::EmptyTitle);
        }
        input.securities.sort_by(|a, b| a.code.cmp(&b.code));
        input.securities.dedup_by(|a, b| a.code == b.code);
        let status = classify_status(&input.title);
        let category = classify_category(&input.title, input.extracted_text.as_deref());
        let normalized_title = normalize_base_title(&input.title);
        let stable_key = stable_key(&input.title, input.published_at, &input.securities);
        let disclosure_id = format!("disc:{}", &sha256(stable_key.as_bytes())[..28]);
        let events = extract_structured_events(
            &disclosure_id,
            &input.title,
            input.extracted_text.as_deref().unwrap_or_default(),
            input.source_version_id.as_deref(),
        );
        let outcome_id = disclosure_id.clone();
        let source_id = format!(
            "dsrc:{}",
            &sha256(format!("{}|{}", input.provider_id, input.original_url).as_bytes())[..28]
        );
        let now = now_secs();
        let outcome = self.storage.run(move |conn| {
            let tx = conn.transaction()?;
            let existing: Option<String> = tx.query_row(
                "SELECT disclosure_id FROM disclosures WHERE disclosure_id=?1",
                [&disclosure_id], |row| row.get(0),
            ).optional()?;
            let revision_of = if status != "active" {
                find_prior_revision(&tx, &normalized_title, &input.securities, &disclosure_id)?
            } else { None };
            tx.execute(
                "INSERT INTO disclosures
                 (disclosure_id,stable_key,title,normalized_title,category,status,
                  published_at,publication_precision,first_seen_at,last_seen_at,
                  revision_of,source_version_id,parser_version,extraction_status,
                  review_reason,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10,?11,?12,?13,?14,?15,?15)
                 ON CONFLICT(disclosure_id) DO UPDATE SET
                   last_seen_at=MAX(last_seen_at,excluded.last_seen_at),
                   source_version_id=COALESCE(excluded.source_version_id,source_version_id),
                   extraction_status=CASE WHEN excluded.extraction_status='parsed' THEN 'parsed' ELSE extraction_status END,
                   review_reason=COALESCE(excluded.review_reason,review_reason),updated_at=excluded.updated_at",
                params![disclosure_id, stable_key, input.title, normalized_title, category, status,
                    input.published_at, input.publication_precision, input.first_seen_at, revision_of,
                    input.source_version_id, DISCLOSURE_PARSER_VERSION, input.extraction_status,
                    input.review_reason, now],
            )?;
            for security in &input.securities {
                tx.execute(
                    "INSERT INTO disclosure_securities(disclosure_id,security_code,security_name,market)
                     VALUES (?1,?2,?3,?4) ON CONFLICT(disclosure_id,security_code) DO UPDATE SET
                     security_name=CASE WHEN excluded.security_name='' THEN security_name ELSE excluded.security_name END,
                     market=CASE WHEN excluded.market='' THEN market ELSE excluded.market END",
                    params![disclosure_id, security.code, security.name, security.market],
                )?;
            }
            let source_inserted = tx.execute(
                "INSERT OR IGNORE INTO disclosure_sources
                 (source_id,disclosure_id,provider_id,provider_name,authority,entry_kind,
                  upstream_id,original_url,discovered_at,last_success_at,latency_ms,is_primary)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10,?11)",
                params![source_id, disclosure_id, input.provider_id, input.provider_name,
                    input.authority.token(), input.entry_kind, input.upstream_id, input.original_url,
                    input.first_seen_at, input.latency_ms.map(|v| v as i64), input.authority.is_primary()],
            )? > 0;
            let mut attachment_ids = std::collections::BTreeMap::new();
            for attachment in &input.attachments {
                let id = format!("datt:{}", &sha256(format!("{}|{}", disclosure_id, attachment.original_url).as_bytes())[..28]);
                attachment_ids.insert(attachment.original_url.clone(), id);
            }
            for attachment in &input.attachments {
                let id = attachment_ids.get(&attachment.original_url).expect("attachment id");
                let parent = attachment.parent_url.as_ref().and_then(|url| attachment_ids.get(url));
                tx.execute(
                    "INSERT INTO disclosure_attachments
                     (attachment_id,disclosure_id,parent_attachment_id,name,original_url,media_type,
                      byte_size,content_hash,source_version_id,extraction_status,page_count,parser_version,review_reason)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                     ON CONFLICT(disclosure_id,original_url) DO UPDATE SET
                      content_hash=COALESCE(excluded.content_hash,content_hash),
                      source_version_id=COALESCE(excluded.source_version_id,source_version_id),
                      extraction_status=excluded.extraction_status,page_count=COALESCE(excluded.page_count,page_count),
                      review_reason=excluded.review_reason",
                    params![id, disclosure_id, parent, attachment.name, attachment.original_url,
                        attachment.media_type, attachment.byte_size.map(|v| v as i64), attachment.content_hash,
                        attachment.source_version_id, attachment.extraction_status,
                        attachment.page_count.map(i64::from), DISCLOSURE_PARSER_VERSION, attachment.review_reason],
                )?;
            }
            for event in &events {
                tx.execute(
                    "INSERT OR IGNORE INTO disclosure_events
                     (event_id,disclosure_id,event_type,fields_json,evidence_json,parser_version,created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![event.event_id, disclosure_id, event.event_type, event.fields.to_string(),
                        event.evidence.to_string(), event.parser_version, now],
                )?;
            }
            if let Some(prior) = &revision_of {
                if status == "cancelled" {
                    tx.execute("UPDATE disclosures SET cancelled_by=?1,updated_at=?2 WHERE disclosure_id=?3", params![disclosure_id, now, prior])?;
                }
            }
            tx.execute(
                "INSERT INTO disclosure_provider_state
                 (provider_id,provider_name,authority,last_attempt_at,last_success_at,
                  consecutive_failures,target_latency_secs,updated_at)
                 VALUES (?1,?2,?3,?4,?4,0,300,?4)
                 ON CONFLICT(provider_id) DO UPDATE SET last_attempt_at=excluded.last_attempt_at,
                  last_success_at=excluded.last_success_at,consecutive_failures=0,
                  last_error=NULL,retry_after=NULL,updated_at=excluded.updated_at",
                params![input.provider_id, input.provider_name, input.authority.token(), now],
            )?;
            tx.commit()?;
            Ok(IngestOutcome {
                disclosure_id: outcome_id,
                inserted: existing.is_none(),
                duplicate_entry: !source_inserted,
                status: status.into(),
                linked_revision: revision_of,
                structured_event_count: events.len(),
            })
        }).await?;
        Ok(outcome)
    }

    pub async fn query(&self, query: DisclosureQuery) -> Result<DisclosurePage> {
        let page = query.page.max(1);
        let page_size = query.page_size.clamp(10, 200);
        self.storage
            .run(move |conn| {
                let (where_sql, values) = build_query(&query);
                let total: u64 = conn.query_row(
                    &format!(
                        "SELECT COUNT(DISTINCT d.disclosure_id) FROM disclosures d {where_sql}"
                    ),
                    params_from_iter(values.clone()),
                    |row| row.get(0),
                )?;
                let mut page_values = values;
                page_values.push(SqlValue::Integer(i64::from(page_size)));
                page_values.push(SqlValue::Integer(i64::from((page - 1) * page_size)));
                let mut stmt =
                    conn.prepare(&format!(
                "SELECT DISTINCT d.disclosure_id,d.title,d.category,d.status,d.published_at,
                 d.publication_precision,d.first_seen_at,d.revision_of,d.cancelled_by,
                 d.source_version_id,d.extraction_status,d.review_reason
                 FROM disclosures d {where_sql}
                 ORDER BY COALESCE(d.published_at,d.first_seen_at) DESC,d.first_seen_at DESC
                 LIMIT ?{} OFFSET ?{}", page_values.len() - 1, page_values.len()
            ))?;
                let ids = stmt
                    .query_map(params_from_iter(page_values), map_list_row)?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let mut items = Vec::with_capacity(ids.len());
                for partial in ids {
                    items.push(hydrate_item(conn, partial)?);
                }
                let total_pages = if total == 0 {
                    0
                } else {
                    total.div_ceil(u64::from(page_size)) as u32
                };
                Ok(DisclosurePage {
                    items,
                    total,
                    page,
                    page_size,
                    total_pages,
                })
            })
            .await
            .map_err(Into::into)
    }

    pub async fn detail(&self, disclosure_id: &str) -> Result<DisclosureDetail> {
        let id = disclosure_id.to_string();
        let detail = self.storage.run(move |conn| {
            let partial = conn.query_row(
                "SELECT disclosure_id,title,category,status,published_at,publication_precision,
                 first_seen_at,revision_of,cancelled_by,source_version_id,extraction_status,review_reason
                 FROM disclosures WHERE disclosure_id=?1", [&id], map_list_row,
            ).optional()?;
            let Some(partial) = partial else { return Ok(None); };
            let item = hydrate_item(conn, partial)?;
            let mut att_stmt = conn.prepare(
                "SELECT attachment_id,parent_attachment_id,name,original_url,media_type,byte_size,
                 content_hash,source_version_id,extraction_status,page_count,parser_version,review_reason
                 FROM disclosure_attachments WHERE disclosure_id=?1 ORDER BY parent_attachment_id,name")?;
            let attachments = att_stmt.query_map([&id], |row| Ok(DisclosureAttachment {
                attachment_id: row.get(0)?, parent_attachment_id: row.get(1)?, name: row.get(2)?,
                original_url: row.get(3)?, media_type: row.get(4)?,
                byte_size: row.get::<_, Option<i64>>(5)?.map(|v| v as u64), content_hash: row.get(6)?,
                source_version_id: row.get(7)?, extraction_status: row.get(8)?,
                page_count: row.get::<_, Option<i64>>(9)?.map(|v| v as u32), parser_version: row.get(10)?,
                review_reason: row.get(11)?,
            }))?.collect::<std::result::Result<Vec<_>, _>>()?;
            let mut evt_stmt = conn.prepare(
                "SELECT event_id,event_type,fields_json,evidence_json,parser_version
                 FROM disclosure_events WHERE disclosure_id=?1 ORDER BY event_type")?;
            let events = evt_stmt.query_map([&id], |row| {
                let fields: String = row.get(2)?;
                let evidence: String = row.get(3)?;
                Ok(StructuredEvent { event_id: row.get(0)?, event_type: row.get(1)?,
                    fields: serde_json::from_str(&fields).unwrap_or(serde_json::Value::Null),
                    evidence: serde_json::from_str(&evidence).unwrap_or(serde_json::Value::Null),
                    parser_version: row.get(4)? })
            })?.collect::<std::result::Result<Vec<_>, _>>()?;
            let mut revisions = Vec::new();
            let mut rev_stmt = conn.prepare(
                "SELECT disclosure_id,title,category,status,published_at,publication_precision,
                 first_seen_at,revision_of,cancelled_by,source_version_id,extraction_status,review_reason
                 FROM disclosures WHERE revision_of=?1 OR disclosure_id=?2 ORDER BY first_seen_at")?;
            let revision_rows = rev_stmt.query_map(params![id, item.revision_of], map_list_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for partial in revision_rows { revisions.push(hydrate_item(conn, partial)?); }
            let note = if item.primary_verified {
                "已关联交易所、监管机构或公司 IR 正式入口；结构化数字仍应点击证据页码核对。"
            } else {
                "当前仅有镜像发现记录，尚未核验正式原文；智能助手不得据此提高结论置信度。"
            };
            Ok(Some(DisclosureDetail { item, attachments, events, revisions, verification_note: note.into() }))
        }).await?;
        detail.ok_or_else(|| Error::NotFound(disclosure_id.into()))
    }

    pub async fn record_provider_failure(&self, provider_id: &str, message: &str) -> Result<i64> {
        let provider_id = provider_id.to_string();
        let message = message.chars().take(2_000).collect::<String>();
        self.storage
            .run(move |conn| {
                let failures: u32 = conn.query_row(
                "SELECT consecutive_failures FROM disclosure_provider_state WHERE provider_id=?1",
                [&provider_id], |row| row.get(0),
            ).optional()?.unwrap_or(0);
                let next_failures = failures.saturating_add(1);
                let retry_after = retry_after_epoch(now_secs(), next_failures, &provider_id);
                conn.execute(
                "UPDATE disclosure_provider_state SET last_attempt_at=?1,consecutive_failures=?2,
                 retry_after=?3,last_error=?4,updated_at=?1 WHERE provider_id=?5",
                params![now_secs(), next_failures, retry_after, message, provider_id],
            )?;
                Ok(retry_after)
            })
            .await
            .map_err(Into::into)
    }
}

#[derive(Debug)]
struct PartialItem {
    disclosure_id: String,
    title: String,
    category: String,
    status: String,
    published_at: Option<i64>,
    publication_precision: String,
    first_seen_at: i64,
    revision_of: Option<String>,
    cancelled_by: Option<String>,
    source_version_id: Option<String>,
    extraction_status: String,
    review_reason: Option<String>,
}

fn map_list_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PartialItem> {
    Ok(PartialItem {
        disclosure_id: row.get(0)?,
        title: row.get(1)?,
        category: row.get(2)?,
        status: row.get(3)?,
        published_at: row.get(4)?,
        publication_precision: row.get(5)?,
        first_seen_at: row.get(6)?,
        revision_of: row.get(7)?,
        cancelled_by: row.get(8)?,
        source_version_id: row.get(9)?,
        extraction_status: row.get(10)?,
        review_reason: row.get(11)?,
    })
}

fn hydrate_item(
    conn: &rusqlite::Connection,
    partial: PartialItem,
) -> astock_storage::Result<DisclosureListItem> {
    let mut sec_stmt = conn.prepare("SELECT security_code,security_name,market FROM disclosure_securities WHERE disclosure_id=?1 ORDER BY security_code")?;
    let securities = sec_stmt
        .query_map([&partial.disclosure_id], |row| {
            Ok(DisclosureSecurity {
                code: row.get(0)?,
                name: row.get(1)?,
                market: row.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut src_stmt = conn.prepare(
        "SELECT source_id,provider_id,provider_name,authority,entry_kind,upstream_id,
         original_url,discovered_at,latency_ms,is_primary FROM disclosure_sources
         WHERE disclosure_id=?1 ORDER BY is_primary DESC,discovered_at",
    )?;
    let sources = src_stmt
        .query_map([&partial.disclosure_id], |row| {
            let authority_text: String = row.get(3)?;
            let authority = ProviderAuthority::parse(&authority_text);
            Ok(DisclosureSource {
                source_id: row.get(0)?,
                provider_id: row.get(1)?,
                provider_name: row.get(2)?,
                authority,
                authority_name: authority.chinese_name().into(),
                entry_kind: row.get(4)?,
                upstream_id: row.get(5)?,
                original_url: row.get(6)?,
                discovered_at: row.get(7)?,
                latency_ms: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                is_primary: row.get::<_, i64>(9)? != 0,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let primary_verified =
        sources.iter().any(|source| source.is_primary) && partial.source_version_id.is_some();
    Ok(DisclosureListItem {
        disclosure_id: partial.disclosure_id,
        title: partial.title,
        category_name: category_name(&partial.category).into(),
        category: partial.category,
        status_name: status_name(&partial.status).into(),
        status: partial.status,
        published_at: partial.published_at,
        publication_precision: partial.publication_precision,
        first_seen_at: partial.first_seen_at,
        discovery_latency_secs: partial
            .published_at
            .map(|published| partial.first_seen_at.saturating_sub(published)),
        revision_of: partial.revision_of,
        cancelled_by: partial.cancelled_by,
        source_version_id: partial.source_version_id,
        extraction_status: partial.extraction_status,
        review_reason: partial.review_reason,
        securities,
        sources,
        primary_verified,
    })
}

fn build_query(query: &DisclosureQuery) -> (String, Vec<SqlValue>) {
    let mut clauses = Vec::new();
    let mut values = Vec::new();
    if let Some(code) = query
        .security_code
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        clauses.push("EXISTS (SELECT 1 FROM disclosure_securities ds WHERE ds.disclosure_id=d.disclosure_id AND ds.security_code=?)".to_string());
        values.push(SqlValue::Text(code.to_string()));
    }
    if let Some(keyword) = query
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        clauses.push("d.title LIKE ? ESCAPE '\\'".to_string());
        values.push(SqlValue::Text(format!("%{}%", escape_like(keyword))));
    }
    if let Some(category) = query
        .category
        .as_deref()
        .filter(|v| !v.is_empty() && *v != "all")
    {
        clauses.push("d.category=?".to_string());
        values.push(SqlValue::Text(category.into()));
    }
    if let Some(status) = query
        .status
        .as_deref()
        .filter(|v| !v.is_empty() && *v != "all")
    {
        clauses.push("d.status=?".to_string());
        values.push(SqlValue::Text(status.into()));
    }
    if query.primary_only {
        clauses.push("EXISTS (SELECT 1 FROM disclosure_sources dx WHERE dx.disclosure_id=d.disclosure_id AND dx.is_primary=1)".into());
    }
    if let Some(from) = query.from_utc {
        clauses.push("COALESCE(d.published_at,d.first_seen_at)>=?".into());
        values.push(SqlValue::Integer(from));
    }
    if let Some(to) = query.to_utc {
        clauses.push("COALESCE(d.published_at,d.first_seen_at)<=?".into());
        values.push(SqlValue::Integer(to));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    (where_sql, values)
}

fn find_prior_revision(
    conn: &rusqlite::Connection,
    normalized_title: &str,
    securities: &[DisclosureSecurity],
    current: &str,
) -> rusqlite::Result<Option<String>> {
    let codes = securities
        .iter()
        .map(|security| security.code.as_str())
        .collect::<BTreeSet<_>>();
    let mut stmt = conn.prepare(
        "SELECT d.disclosure_id,ds.security_code FROM disclosures d
         LEFT JOIN disclosure_securities ds ON ds.disclosure_id=d.disclosure_id
         WHERE d.normalized_title=?1 AND d.disclosure_id<>?2 ORDER BY d.first_seen_at DESC",
    )?;
    let rows = stmt.query_map(params![normalized_title, current], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    for row in rows {
        let (id, code) = row?;
        if codes.is_empty() || code.as_deref().is_some_and(|code| codes.contains(code)) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

pub fn classify_status(title: &str) -> &'static str {
    if Regex::new(r"(取消|撤回|作废|终止发布)")
        .unwrap()
        .is_match(title)
    {
        "cancelled"
    } else if Regex::new(r"(更正|修订|更新后|补充公告|勘误)")
        .unwrap()
        .is_match(title)
    {
        "revised"
    } else {
        "active"
    }
}

pub fn classify_category(title: &str, text: Option<&str>) -> &'static str {
    let value = format!("{title} {}", text.unwrap_or_default());
    for (category, pattern) in [
        ("periodic_report", r"年度报告|半年度报告|季度报告|年报|季报"),
        ("earnings_forecast", r"业绩预告|业绩快报|盈利预测"),
        ("prospectus", r"招股说明书|募集说明书"),
        ("inquiry_reply", r"问询函.*回复|回复.*问询函|监管工作函"),
        ("ir_activity", r"投资者关系活动|机构调研|业绩说明会"),
        ("contract", r"重大合同|中标|订单"),
        ("buyback", r"回购"),
        ("holding_change", r"增持|减持|持股变动"),
        ("unlock", r"限售股.*上市流通|解除限售|解禁"),
        ("suspension", r"停牌|复牌"),
        ("penalty", r"处罚|纪律处分|监管措施"),
        ("litigation", r"诉讼|仲裁"),
        ("guarantee", r"担保"),
        ("pledge", r"质押"),
        ("operations", r"产量|销量|资本开支|项目投资"),
    ] {
        if Regex::new(pattern).unwrap().is_match(&value) {
            return category;
        }
    }
    "other"
}

fn extract_structured_events(
    disclosure_id: &str,
    title: &str,
    text: &str,
    source_version_id: Option<&str>,
) -> Vec<StructuredEvent> {
    let combined = format!("{title}\n{text}");
    let mut event_types = BTreeSet::new();
    let category = classify_category(title, Some(text));
    if category != "other" {
        event_types.insert(category);
    }
    for (event_type, pattern) in [
        ("contract", r"合同|中标|订单"),
        ("earnings_forecast", r"业绩预告|业绩快报"),
        ("buyback", r"回购"),
        ("holding_change", r"增持|减持|持股变动"),
        ("unlock", r"解除限售|解禁|上市流通"),
        ("suspension", r"停牌|复牌"),
        ("penalty", r"处罚|纪律处分"),
        ("litigation", r"诉讼|仲裁"),
        ("guarantee", r"担保"),
        ("pledge", r"质押"),
        ("operations", r"产量|销量|资本开支|项目投资"),
    ] {
        if Regex::new(pattern).unwrap().is_match(&combined) {
            event_types.insert(event_type);
        }
    }
    let money = Regex::new(r"(?P<value>\d+(?:\.\d+)?)\s*(?P<unit>亿元|万元|元)").unwrap()
        .captures(&combined).map(|capture| serde_json::json!({"raw": capture.get(0).unwrap().as_str(), "value": capture.name("value").unwrap().as_str(), "unit": capture.name("unit").unwrap().as_str()}));
    event_types.into_iter().map(|event_type| {
        let fields = serde_json::json!({ "amount": money, "title": title });
        let evidence = serde_json::json!({ "source_version_id": source_version_id, "page_number": null, "table_cell": null, "requires_source_verification": source_version_id.is_none() });
        let event_id = format!("devt:{}", &sha256(format!("{disclosure_id}|{event_type}|{fields}").as_bytes())[..28]);
        StructuredEvent { event_id, event_type: event_type.into(), fields, evidence, parser_version: DISCLOSURE_PARSER_VERSION.into() }
    }).collect()
}

fn stable_key(title: &str, published_at: Option<i64>, securities: &[DisclosureSecurity]) -> String {
    // Disclosure dates are Shanghai civil dates. CNInfo midnight timestamps
    // can fall on the previous UTC day, so bucket after applying UTC+8.
    let day = published_at
        .map(|value| value.saturating_add(8 * 3_600).div_euclid(86_400))
        .unwrap_or_default();
    let codes = securities
        .iter()
        .map(|security| security.code.as_str())
        .collect::<Vec<_>>()
        .join(",");
    format!("{}|{day}|{codes}", normalize_title(title))
}

fn normalize_title(title: &str) -> String {
    clean_text(title)
        .to_lowercase()
        .replace([' ', '　', '-', '_', '：', ':'], "")
}

fn normalize_base_title(title: &str) -> String {
    let markers =
        Regex::new(r"(?i)(关于|的)?(更正|修订|更新后|补充公告|勘误|取消|撤回|作废)(公告)?")
            .unwrap();
    markers
        .replace_all(&normalize_title(title), "")
        .trim_end_matches("公告")
        .to_string()
}

fn clean_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn retry_delay_secs(consecutive_failures: u32, provider_id: &str) -> u64 {
    let exponent = consecutive_failures.saturating_sub(1).min(8);
    let base = 15_u64.saturating_mul(1_u64 << exponent).min(3_600);
    let jitter = u64::from(Sha256::digest(provider_id.as_bytes())[0]) % (base / 5 + 1);
    base + jitter
}

pub fn retry_after_epoch(now: i64, consecutive_failures: u32, provider_id: &str) -> i64 {
    now.saturating_add(retry_delay_secs(consecutive_failures, provider_id) as i64)
}

fn category_name(value: &str) -> &'static str {
    match value {
        "periodic_report" => "定期报告",
        "earnings_forecast" => "业绩预告/快报",
        "prospectus" => "招股/募集文件",
        "inquiry_reply" => "问询回复",
        "ir_activity" => "投资者关系活动",
        "contract" => "合同/中标",
        "buyback" => "股份回购",
        "holding_change" => "持股变动",
        "unlock" => "限售解禁",
        "suspension" => "停复牌",
        "penalty" => "监管处罚",
        "litigation" => "诉讼仲裁",
        "guarantee" => "对外担保",
        "pledge" => "股份质押",
        "operations" => "产销/资本开支",
        _ => "其他公告",
    }
}
fn status_name(value: &str) -> &'static str {
    match value {
        "revised" => "修订版",
        "cancelled" => "已取消/撤回",
        _ => "有效",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_storage::StorageConfig;

    fn input(
        provider: &str,
        authority: ProviderAuthority,
        title: &str,
        code: &str,
        seen: i64,
    ) -> DisclosureInput {
        DisclosureInput {
            provider_id: provider.into(),
            provider_name: provider.into(),
            authority,
            entry_kind: "index".into(),
            upstream_id: Some(format!("{provider}-{seen}")),
            original_url: format!("https://example.com/{provider}/{seen}"),
            title: title.into(),
            published_at: Some(1_750_000_000),
            publication_precision: "second".into(),
            first_seen_at: seen,
            latency_ms: Some(120),
            securities: vec![DisclosureSecurity {
                code: code.into(),
                name: "测试公司".into(),
                market: "SSE".into(),
            }],
            attachments: vec![],
            source_version_id: authority.is_primary().then(|| "srcver:test".into()),
            extracted_text: Some("合同金额 12.5 亿元，另有股份回购计划。".into()),
            extraction_status: "parsed".into(),
            review_reason: None,
        }
    }

    #[tokio::test]
    async fn canonicalizes_cross_entrance_duplicate_without_promoting_mirror() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let store = DisclosureStore::new(storage);
        let a = store
            .ingest(input(
                "mirror",
                ProviderAuthority::MirrorDiscovery,
                "重大合同公告",
                "600001",
                1_750_000_050,
            ))
            .await
            .unwrap();
        let b = store
            .ingest(input(
                "sse",
                ProviderAuthority::Exchange,
                "重大合同公告",
                "600001",
                1_750_000_060,
            ))
            .await
            .unwrap();
        assert_eq!(a.disclosure_id, b.disclosure_id);
        let page = store
            .query(DisclosureQuery {
                security_code: Some("600001".into()),
                page: 1,
                page_size: 20,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].sources.len(), 2);
        assert!(page.items[0].primary_verified);
    }

    #[tokio::test]
    async fn links_revision_and_cancellation_across_rename_and_multi_security() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let store = DisclosureStore::new(storage);
        let base = store
            .ingest(input(
                "cninfo",
                ProviderAuthority::Exchange,
                "收购资产公告",
                "000001",
                1_750_000_050,
            ))
            .await
            .unwrap();
        let mut revision = input(
            "cninfo",
            ProviderAuthority::Exchange,
            "收购资产更正公告",
            "000001",
            1_750_000_080,
        );
        revision.securities[0].name = "更名后公司".into();
        revision.securities.push(DisclosureSecurity {
            code: "000002".into(),
            name: "交易对方".into(),
            market: "SZSE".into(),
        });
        let revised = store.ingest(revision).await.unwrap();
        assert_eq!(
            revised.linked_revision.as_deref(),
            Some(base.disclosure_id.as_str())
        );
        assert_eq!(revised.status, "revised");
    }

    #[tokio::test]
    async fn timeline_is_filtered_and_pageable() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let store = DisclosureStore::new(storage);
        for idx in 0..25 {
            let mut row = input(
                "sse",
                ProviderAuthority::Exchange,
                &format!("第 {idx} 份年度报告"),
                "600001",
                1_750_000_100 + idx,
            );
            row.published_at = Some(1_750_000_000 + idx);
            store.ingest(row).await.unwrap();
        }
        let page = store
            .query(DisclosureQuery {
                security_code: Some("600001".into()),
                category: Some("periodic_report".into()),
                page: 2,
                page_size: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page.total, 25);
        assert_eq!(page.items.len(), 10);
        assert_eq!(page.total_pages, 3);
    }

    #[test]
    fn extraction_and_retry_contracts_are_deterministic() {
        assert_eq!(classify_status("关于回购方案的修订公告"), "revised");
        assert_eq!(classify_status("撤回公告"), "cancelled");
        assert_eq!(classify_category("2025年年度报告", None), "periodic_report");
        assert!(retry_delay_secs(2, "sse") >= 30);
        assert!(retry_delay_secs(20, "sse") <= 4_321);
        assert_eq!(official_provider_catalog().len(), 7);
    }
}
