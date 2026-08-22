//! Durable, revisioned news and external-document evidence archive.
//!
//! The archive separates a stable source document from immutable content
//! revisions and fetch observations. Event, publication, first-seen and
//! revision clocks remain independent so point-in-time research can avoid
//! accidentally learning from a document before the system observed it.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::PathBuf;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db::now_secs;
use crate::{Error, Result, Storage};

const MAX_URL_BYTES: usize = 4 * 1024;
const MAX_TITLE_BYTES: usize = 8 * 1024;
const MAX_SUMMARY_BYTES: usize = 128 * 1024;
const MAX_RAW_BYTES: usize = 2 * 1024 * 1024;

/// One timestamp retaining both normalized UTC seconds and the provider's
/// original representation/time zone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceTimestamp {
    pub utc: Option<i64>,
    pub original: Option<String>,
}

/// Parsed document revision plus fetch metadata to ingest atomically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsArchiveInput {
    pub canonical_url: String,
    pub source_id: String,
    pub source_name: String,
    pub license: String,
    pub content_type: String,
    pub language: String,
    pub parser_version: String,
    pub title: String,
    /// A factual, licence-compatible summary; never generated marketing copy.
    pub factual_summary: String,
    /// Original provider row/body. Persisted only when
    /// `raw_snapshot_permitted` is true, and always gzip compressed.
    pub raw_snapshot: Option<Vec<u8>>,
    pub raw_snapshot_permitted: bool,
    pub event_time: EvidenceTimestamp,
    pub publish_time: EvidenceTimestamp,
    pub first_seen_time_utc: i64,
    pub revision_time: EvidenceTimestamp,
    pub retention_class: String,
    pub observation: NewsObservationInput,
}

/// One upstream fetch/parse observation. It can be stored without a document
/// when parsing failed, preserving bounded raw evidence and the error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsObservationInput {
    pub document_id: Option<String>,
    pub revision_id: Option<String>,
    pub provider_id: String,
    pub endpoint: String,
    pub fetched_at: i64,
    pub http_status: Option<u16>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub latency_ms: Option<u64>,
    pub parse_status: String,
    pub parse_error: Option<String>,
    pub raw_evidence: Option<Vec<u8>>,
}

impl NewsObservationInput {
    pub fn success(provider_id: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            document_id: None,
            revision_id: None,
            provider_id: provider_id.into(),
            endpoint: public_endpoint(&endpoint.into()),
            fetched_at: now_secs(),
            http_status: Some(200),
            etag: None,
            last_modified: None,
            latency_ms: None,
            parse_status: "ok".into(),
            parse_error: None,
            raw_evidence: None,
        }
    }
}

/// Result of an idempotent document upsert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsArchiveUpsert {
    pub document_id: String,
    pub revision_id: String,
    pub inserted_revision: bool,
    pub supersedes_revision_id: Option<String>,
}

/// Query clock for point-in-time replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewsArchiveClock {
    Event,
    Publish,
    FirstSeen,
    Revision,
}

/// A fully traceable archived revision. Raw snapshots are loaded separately
/// so normal UI and Agent queries cannot accidentally include full content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedNewsRevision {
    pub document_id: String,
    pub canonical_url: String,
    pub source_id: String,
    pub source_name: String,
    pub license: String,
    pub content_type: String,
    pub language: String,
    pub parser_version: String,
    pub content_hash: String,
    pub current_revision_id: Option<String>,
    pub document_first_seen_time_utc: i64,
    pub last_observed_at: i64,
    pub retention_class: String,
    pub revision_id: String,
    pub revision_hash: String,
    pub title: String,
    pub factual_summary: String,
    pub supersedes_revision_id: Option<String>,
    pub event_time: EvidenceTimestamp,
    pub publish_time: EvidenceTimestamp,
    pub first_seen_time_utc: i64,
    pub revision_time: EvidenceTimestamp,
    pub raw_snapshot_hash: Option<String>,
}

impl ArchivedNewsRevision {
    pub fn stale_age_secs(&self, now: i64) -> i64 {
        now.saturating_sub(self.last_observed_at).max(0)
    }
}

/// A precise Agent conclusion → immutable document revision reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEvidenceRef {
    pub task_id: String,
    pub conclusion_key: String,
    pub revision_id: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentEventEvidence {
    pub event_id: String,
    pub revision_id: String,
    pub relation: String,
    pub created_at: i64,
}

/// Persistent source health fields restored after application restart.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsProviderArchiveState {
    pub provider_id: String,
    pub last_success_at: Option<i64>,
    pub last_observation_at: Option<i64>,
    pub last_latency_ms: Option<u64>,
    pub attempts: u64,
    pub failures: u64,
    pub last_error_kind: Option<String>,
    pub updated_at: i64,
}

/// Retention policy. Immutable revision metadata and hashes are never
/// deleted automatically; only large snapshots and old fetch observations
/// are eligible for pruning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsArchiveRetentionPolicy {
    pub raw_snapshot_days: u32,
    pub observation_days: u32,
}

impl Default for NewsArchiveRetentionPolicy {
    fn default() -> Self {
        Self {
            raw_snapshot_days: 365,
            observation_days: 180,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsArchiveCleanupReport {
    pub raw_snapshots_pruned: u64,
    pub raw_observations_pruned: u64,
    pub observations_deleted: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsArchiveExportReport {
    pub path: PathBuf,
    pub revisions: u64,
    pub compressed_bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsArchiveSourceStats {
    pub documents: u64,
    pub revisions: u64,
    pub last_observed_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsIngestObservation {
    pub observation_id: i64,
    pub document_id: Option<String>,
    pub revision_id: Option<String>,
    pub provider_id: String,
    pub endpoint: String,
    pub fetched_at: i64,
    pub http_status: Option<u16>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub latency_ms: Option<u64>,
    pub parse_status: String,
    pub parse_error: Option<String>,
    pub raw_evidence_hash: Option<String>,
    pub raw_evidence_present: bool,
}

#[derive(Debug, Serialize)]
struct ExportEntry {
    revision: ArchivedNewsRevision,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_snapshot: Option<Vec<u8>>,
}

impl Storage {
    /// Idempotently insert a source document, immutable revision and fetch
    /// observation in one transaction. A changed hash creates a new revision
    /// linked to the previous current revision; unchanged input adds only an
    /// observation and refreshes `last_observed_at`.
    pub async fn news_archive_upsert(
        &self,
        mut input: NewsArchiveInput,
    ) -> Result<NewsArchiveUpsert> {
        validate_input(&input)?;
        input.observation.endpoint = public_endpoint(&input.observation.endpoint);
        let now = now_secs();
        if input.first_seen_time_utc <= 0 {
            input.first_seen_time_utc = now;
        }
        if input.revision_time.utc.is_none() {
            input.revision_time.utc = Some(now);
        }
        let document_id = format!(
            "doc:{}",
            sha256(&[
                input.source_id.as_bytes(),
                b"\0",
                input.canonical_url.as_bytes(),
            ])
        );
        let permitted_raw = input
            .raw_snapshot
            .take()
            .filter(|_| input.raw_snapshot_permitted);
        let raw_snapshot_hash = permitted_raw.as_deref().map(|raw| sha256(&[raw]));
        let raw_snapshot_gzip = permitted_raw.as_deref().map(gzip).transpose()?;
        let content_hash = sha256(&[
            input.title.as_bytes(),
            b"\0",
            input.factual_summary.as_bytes(),
            b"\0",
            raw_snapshot_hash.as_deref().unwrap_or("").as_bytes(),
        ]);
        let revision_hash = sha256(&[
            input.parser_version.as_bytes(),
            b"\0",
            content_hash.as_bytes(),
        ]);
        let revision_id = format!(
            "rev:{}",
            sha256(&[document_id.as_bytes(), b"\0", revision_hash.as_bytes()])
        );
        let raw_observation_hash = input
            .observation
            .raw_evidence
            .as_deref()
            .map(|raw| sha256(&[raw]));
        let raw_observation_gzip = input
            .observation
            .raw_evidence
            .as_deref()
            .map(gzip)
            .transpose()?;
        self.run(move |conn| {
            let tx = conn.transaction()?;
            let existing = tx
                .query_row(
                    "SELECT current_revision_id, first_seen_time_utc
                     FROM source_documents WHERE document_id=?1",
                    params![document_id],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?;
            let supersedes = existing.as_ref().and_then(|value| value.0.clone());
            let first_seen = existing
                .as_ref()
                .map(|value| value.1)
                .unwrap_or(input.first_seen_time_utc);
            tx.execute(
                "INSERT INTO source_documents
                 (document_id,canonical_url,source_id,source_name,license,
                  content_type,language,parser_version,content_hash,
                  current_revision_id,first_seen_time_utc,last_observed_at,
                  retention_class,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,NULL,?10,?11,?12,?11,?11)
                 ON CONFLICT(document_id) DO UPDATE SET
                  source_name=excluded.source_name, license=excluded.license,
                  content_type=excluded.content_type, language=excluded.language,
                  parser_version=excluded.parser_version,
                  last_observed_at=excluded.last_observed_at,
                  retention_class=excluded.retention_class,
                  updated_at=excluded.updated_at",
                params![
                    document_id,
                    input.canonical_url,
                    input.source_id,
                    input.source_name,
                    input.license,
                    input.content_type,
                    input.language,
                    input.parser_version,
                    content_hash,
                    first_seen,
                    input.observation.fetched_at,
                    input.retention_class,
                ],
            )?;
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO document_revisions
                 (revision_id,document_id,revision_hash,title,factual_summary,
                  raw_snapshot_gzip,raw_snapshot_hash,supersedes_revision_id,
                  event_time_utc,event_time_original,publish_time_utc,
                  publish_time_original,first_seen_time_utc,revision_time_utc,
                  revision_time_original,created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                params![
                    revision_id,
                    document_id,
                    revision_hash,
                    input.title,
                    input.factual_summary,
                    raw_snapshot_gzip,
                    raw_snapshot_hash,
                    supersedes,
                    input.event_time.utc,
                    input.event_time.original,
                    input.publish_time.utc,
                    input.publish_time.original,
                    first_seen,
                    input.revision_time.utc.unwrap_or(now),
                    input.revision_time.original,
                    now,
                ],
            )? > 0;
            if inserted {
                tx.execute(
                    "UPDATE source_documents SET current_revision_id=?2,
                     content_hash=?3, updated_at=?4 WHERE document_id=?1",
                    params![document_id, revision_id, content_hash, now],
                )?;
            }
            tx.execute(
                "INSERT INTO ingest_observations
                 (document_id,revision_id,provider_id,endpoint,fetched_at,
                  http_status,etag,last_modified,latency_ms,parse_status,
                  parse_error,raw_evidence_gzip,raw_evidence_hash)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    document_id,
                    revision_id,
                    input.observation.provider_id,
                    input.observation.endpoint,
                    input.observation.fetched_at,
                    input.observation.http_status,
                    input.observation.etag,
                    input.observation.last_modified,
                    input.observation.latency_ms.map(|value| value as i64),
                    input.observation.parse_status,
                    input.observation.parse_error,
                    raw_observation_gzip,
                    raw_observation_hash,
                ],
            )?;
            tx.commit()?;
            Ok(NewsArchiveUpsert {
                document_id,
                revision_id,
                inserted_revision: inserted,
                supersedes_revision_id: supersedes,
            })
        })
        .await
    }

    /// Preserve a failed/unattached fetch observation. Raw evidence is
    /// bounded and compressed, and normal queries never return it.
    pub async fn news_observation_record(&self, mut input: NewsObservationInput) -> Result<i64> {
        validate_observation(&input)?;
        input.endpoint = public_endpoint(&input.endpoint);
        let raw_hash = input.raw_evidence.as_deref().map(|raw| sha256(&[raw]));
        let raw_gzip = input.raw_evidence.as_deref().map(gzip).transpose()?;
        self.run(move |conn| {
            conn.execute(
                "INSERT INTO ingest_observations
                 (document_id,revision_id,provider_id,endpoint,fetched_at,
                  http_status,etag,last_modified,latency_ms,parse_status,
                  parse_error,raw_evidence_gzip,raw_evidence_hash)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    input.document_id,
                    input.revision_id,
                    input.provider_id,
                    input.endpoint,
                    input.fetched_at,
                    input.http_status,
                    input.etag,
                    input.last_modified,
                    input.latency_ms.map(|value| value as i64),
                    input.parse_status,
                    input.parse_error,
                    raw_gzip,
                    raw_hash,
                ],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await
    }

    pub async fn news_archive_recent(&self, limit: usize) -> Result<Vec<ArchivedNewsRevision>> {
        let limit = limit.clamp(1, 100_000) as i64;
        self.run(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "{} WHERE r.revision_id=d.current_revision_id
                 ORDER BY COALESCE(r.publish_time_utc,r.event_time_utc,
                                   r.first_seen_time_utc) DESC LIMIT ?1",
                archive_select()
            ))?;
            let rows = stmt.query_map(params![limit], map_archive_row)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    pub async fn news_archive_all_revisions(
        &self,
        limit: usize,
    ) -> Result<Vec<ArchivedNewsRevision>> {
        let limit = limit.clamp(1, 100_000) as i64;
        self.run(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "{} ORDER BY r.revision_time_utc DESC,r.rowid DESC LIMIT ?1",
                archive_select()
            ))?;
            let rows = stmt.query_map(params![limit], map_archive_row)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    pub async fn news_archive_source_stats(
        &self,
        source_id: &str,
    ) -> Result<NewsArchiveSourceStats> {
        let source_id = source_id.to_string();
        self.run(move |conn| {
            conn.query_row(
                "SELECT COUNT(DISTINCT d.document_id),COUNT(r.revision_id),
                        MAX(d.last_observed_at)
                 FROM source_documents d LEFT JOIN document_revisions r
                   ON r.document_id=d.document_id WHERE d.source_id=?1",
                params![source_id],
                |row| {
                    Ok(NewsArchiveSourceStats {
                        documents: row.get::<_, i64>(0)? as u64,
                        revisions: row.get::<_, i64>(1)? as u64,
                        last_observed_at: row.get(2)?,
                    })
                },
            )
            .map_err(Into::into)
        })
        .await
    }

    pub async fn news_ingest_observations(
        &self,
        provider_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<NewsIngestObservation>> {
        let provider_id = provider_id.map(str::to_string);
        let limit = limit.clamp(1, 1_000) as i64;
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT observation_id,document_id,revision_id,provider_id,
                        endpoint,fetched_at,http_status,etag,last_modified,
                        latency_ms,parse_status,parse_error,raw_evidence_hash,
                        raw_evidence_gzip IS NOT NULL
                 FROM ingest_observations
                 WHERE (?1 IS NULL OR provider_id=?1)
                 ORDER BY fetched_at DESC,observation_id DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![provider_id, limit], |row| {
                Ok(NewsIngestObservation {
                    observation_id: row.get(0)?,
                    document_id: row.get(1)?,
                    revision_id: row.get(2)?,
                    provider_id: row.get(3)?,
                    endpoint: row.get(4)?,
                    fetched_at: row.get(5)?,
                    http_status: row.get::<_, Option<i64>>(6)?.map(|value| value as u16),
                    etag: row.get(7)?,
                    last_modified: row.get(8)?,
                    latency_ms: row.get::<_, Option<i64>>(9)?.map(|value| value as u64),
                    parse_status: row.get(10)?,
                    parse_error: row.get(11)?,
                    raw_evidence_hash: row.get(12)?,
                    raw_evidence_present: row.get(13)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    pub async fn news_archive_revisions(
        &self,
        document_id: &str,
    ) -> Result<Vec<ArchivedNewsRevision>> {
        let document_id = document_id.to_string();
        self.run(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "{} WHERE d.document_id=?1 ORDER BY r.revision_time_utc ASC, r.rowid ASC",
                archive_select()
            ))?;
            let rows = stmt.query_map(params![document_id], map_archive_row)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    pub async fn news_archive_revision(
        &self,
        revision_id: &str,
    ) -> Result<Option<ArchivedNewsRevision>> {
        let revision_id = revision_id.to_string();
        self.run(move |conn| {
            conn.query_row(
                &format!("{} WHERE r.revision_id=?1", archive_select()),
                params![revision_id],
                map_archive_row,
            )
            .optional()
            .map_err(Into::into)
        })
        .await
    }

    /// Point-in-time replay. Regardless of the selected sort/filter clock, a
    /// revision is never visible before both first-seen and revision time.
    pub async fn news_archive_as_of(
        &self,
        cutoff: i64,
        clock: NewsArchiveClock,
        limit: usize,
    ) -> Result<Vec<ArchivedNewsRevision>> {
        let clock_expr = match clock {
            NewsArchiveClock::Event => {
                "COALESCE(r.event_time_utc,r.publish_time_utc,r.first_seen_time_utc)"
            }
            NewsArchiveClock::Publish => "COALESCE(r.publish_time_utc,r.first_seen_time_utc)",
            NewsArchiveClock::FirstSeen => "r.first_seen_time_utc",
            NewsArchiveClock::Revision => "r.revision_time_utc",
        };
        let limit = limit.clamp(1, 100_000) as i64;
        self.run(move |conn| {
            let sql = format!(
                "{} WHERE r.first_seen_time_utc<=?1 AND r.revision_time_utc<=?1
                 AND {clock_expr}<=?1
                 AND r.revision_time_utc=(
                   SELECT MAX(r2.revision_time_utc) FROM document_revisions r2
                   WHERE r2.document_id=r.document_id
                     AND r2.first_seen_time_utc<=?1 AND r2.revision_time_utc<=?1)
                 ORDER BY {clock_expr} DESC LIMIT ?2",
                archive_select()
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params![cutoff, limit], map_archive_row)?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    /// Decompress one raw snapshot on explicit request. Corruption is local
    /// to that revision and returns a typed error; metadata remains queryable.
    pub async fn news_revision_snapshot(&self, revision_id: &str) -> Result<Option<Vec<u8>>> {
        let revision_id = revision_id.to_string();
        let compressed = self
            .run(move |conn| {
                conn.query_row(
                    "SELECT raw_snapshot_gzip FROM document_revisions WHERE revision_id=?1",
                    params![revision_id],
                    |row| row.get::<_, Option<Vec<u8>>>(0),
                )
                .optional()
                .map(|row| row.flatten())
                .map_err(Into::into)
            })
            .await?;
        compressed.map(|raw| gunzip(&raw)).transpose()
    }

    pub async fn news_agent_evidence_link(
        &self,
        task_id: &str,
        conclusion_key: &str,
        revision_id: &str,
    ) -> Result<()> {
        let task_id = task_id.to_string();
        let conclusion_key = conclusion_key.to_string();
        let revision_id = revision_id.to_string();
        self.run(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO agent_evidence_refs
                 (task_id,conclusion_key,revision_id,created_at) VALUES (?1,?2,?3,?4)",
                params![task_id, conclusion_key, revision_id, now_secs()],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn news_event_evidence_link(
        &self,
        event_id: &str,
        revision_id: &str,
        relation: &str,
    ) -> Result<()> {
        if event_id.trim().is_empty() || relation.trim().is_empty() {
            return Err(Error::Invalid("事件编号和证据关系不能为空".into()));
        }
        let event_id = event_id.to_string();
        let revision_id = revision_id.to_string();
        let relation = relation.to_string();
        self.run(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO document_event_evidence
                 (event_id,revision_id,relation,created_at) VALUES (?1,?2,?3,?4)",
                params![event_id, revision_id, relation, now_secs()],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn news_event_evidence(&self, event_id: &str) -> Result<Vec<DocumentEventEvidence>> {
        let event_id = event_id.to_string();
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT event_id,revision_id,relation,created_at
                 FROM document_event_evidence WHERE event_id=?1 ORDER BY created_at,rowid",
            )?;
            let rows = stmt.query_map(params![event_id], |row| {
                Ok(DocumentEventEvidence {
                    event_id: row.get(0)?,
                    revision_id: row.get(1)?,
                    relation: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    pub async fn news_agent_evidence(&self, task_id: &str) -> Result<Vec<AgentEvidenceRef>> {
        let task_id = task_id.to_string();
        self.run(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT task_id,conclusion_key,revision_id,created_at
                 FROM agent_evidence_refs WHERE task_id=?1 ORDER BY created_at,rowid",
            )?;
            let rows = stmt.query_map(params![task_id], |row| {
                Ok(AgentEvidenceRef {
                    task_id: row.get(0)?,
                    conclusion_key: row.get(1)?,
                    revision_id: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    pub async fn news_provider_state_put(&self, state: NewsProviderArchiveState) -> Result<()> {
        self.run(move |conn| {
            conn.execute(
                "INSERT INTO news_provider_state
                 (provider_id,last_success_at,last_observation_at,last_latency_ms,
                  attempts,failures,last_error_kind,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(provider_id) DO UPDATE SET
                  last_success_at=excluded.last_success_at,
                  last_observation_at=excluded.last_observation_at,
                  last_latency_ms=excluded.last_latency_ms,
                  attempts=excluded.attempts,failures=excluded.failures,
                  last_error_kind=excluded.last_error_kind,updated_at=excluded.updated_at",
                params![
                    state.provider_id,
                    state.last_success_at,
                    state.last_observation_at,
                    state.last_latency_ms.map(|value| value as i64),
                    state.attempts as i64,
                    state.failures as i64,
                    state.last_error_kind,
                    state.updated_at,
                ],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn news_provider_state_get(
        &self,
        provider_id: &str,
    ) -> Result<Option<NewsProviderArchiveState>> {
        let provider_id = provider_id.to_string();
        self.run(move |conn| {
            conn.query_row(
                "SELECT provider_id,last_success_at,last_observation_at,last_latency_ms,
                        attempts,failures,last_error_kind,updated_at
                 FROM news_provider_state WHERE provider_id=?1",
                params![provider_id],
                |row| {
                    Ok(NewsProviderArchiveState {
                        provider_id: row.get(0)?,
                        last_success_at: row.get(1)?,
                        last_observation_at: row.get(2)?,
                        last_latency_ms: row.get::<_, Option<i64>>(3)?.map(|value| value as u64),
                        attempts: row.get::<_, i64>(4)? as u64,
                        failures: row.get::<_, i64>(5)? as u64,
                        last_error_kind: row.get(6)?,
                        updated_at: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
        })
        .await
    }

    pub async fn news_archive_cleanup(
        &self,
        policy: NewsArchiveRetentionPolicy,
        now: i64,
    ) -> Result<NewsArchiveCleanupReport> {
        let raw_cutoff = now.saturating_sub(i64::from(policy.raw_snapshot_days) * 86_400);
        let observation_cutoff = now.saturating_sub(i64::from(policy.observation_days) * 86_400);
        self.run(move |conn| {
            let tx = conn.transaction()?;
            let raw_snapshots_pruned = tx.execute(
                "UPDATE document_revisions SET raw_snapshot_gzip=NULL
                 WHERE raw_snapshot_gzip IS NOT NULL AND revision_time_utc<?1",
                params![raw_cutoff],
            )? as u64;
            let raw_observations_pruned = tx.execute(
                "UPDATE ingest_observations SET raw_evidence_gzip=NULL
                 WHERE raw_evidence_gzip IS NOT NULL AND fetched_at<?1",
                params![observation_cutoff],
            )? as u64;
            let observations_deleted = tx.execute(
                "DELETE FROM ingest_observations WHERE fetched_at<?1",
                params![observation_cutoff],
            )? as u64;
            tx.commit()?;
            Ok(NewsArchiveCleanupReport {
                raw_snapshots_pruned,
                raw_observations_pruned,
                observations_deleted,
            })
        })
        .await
    }

    /// Export immutable revision metadata as gzip JSON Lines. The destination
    /// must not already exist, preventing accidental overwrite. Full snapshots
    /// are excluded unless explicitly requested.
    pub async fn news_archive_export_gzip(
        &self,
        path: PathBuf,
        include_snapshots: bool,
    ) -> Result<NewsArchiveExportReport> {
        if path.exists() {
            return Err(Error::Invalid("导出目标已存在，拒绝覆盖".into()));
        }
        let revisions = self.news_archive_all_revisions(100_000).await?;
        let mut entries = Vec::with_capacity(revisions.len());
        for revision in revisions {
            let raw_snapshot = if include_snapshots {
                self.news_revision_snapshot(&revision.revision_id).await?
            } else {
                None
            };
            entries.push(ExportEntry {
                revision,
                raw_snapshot,
            });
        }
        let output = path.clone();
        let count = entries.len() as u64;
        tokio::task::spawn_blocking(move || -> Result<()> {
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&output)?;
            let mut encoder = GzEncoder::new(file, Compression::default());
            for entry in entries {
                serde_json::to_writer(&mut encoder, &entry)?;
                encoder.write_all(b"\n")?;
            }
            encoder.finish()?.sync_all()?;
            Ok(())
        })
        .await
        .map_err(|error| Error::Invalid(format!("导出任务失败：{error}")))??;
        Ok(NewsArchiveExportReport {
            compressed_bytes: std::fs::metadata(&path)?.len(),
            path,
            revisions: count,
        })
    }

    /// SQLite integrity check used by diagnostics and recovery workflows.
    pub async fn news_archive_integrity_check(&self) -> Result<String> {
        self.run(move |conn| {
            conn.query_row("PRAGMA quick_check", [], |row| row.get(0))
                .map_err(Into::into)
        })
        .await
    }
}

fn archive_select() -> &'static str {
    "SELECT d.document_id,d.canonical_url,d.source_id,d.source_name,d.license,
            d.content_type,d.language,d.parser_version,d.content_hash,
            d.current_revision_id,d.first_seen_time_utc,d.last_observed_at,
            d.retention_class,r.revision_id,r.revision_hash,r.title,
            r.factual_summary,r.supersedes_revision_id,r.event_time_utc,
            r.event_time_original,r.publish_time_utc,r.publish_time_original,
            r.first_seen_time_utc,r.revision_time_utc,r.revision_time_original,
            r.raw_snapshot_hash
     FROM source_documents d JOIN document_revisions r ON r.document_id=d.document_id"
}

fn map_archive_row(row: &Row<'_>) -> rusqlite::Result<ArchivedNewsRevision> {
    Ok(ArchivedNewsRevision {
        document_id: row.get(0)?,
        canonical_url: row.get(1)?,
        source_id: row.get(2)?,
        source_name: row.get(3)?,
        license: row.get(4)?,
        content_type: row.get(5)?,
        language: row.get(6)?,
        parser_version: row.get(7)?,
        content_hash: row.get(8)?,
        current_revision_id: row.get(9)?,
        document_first_seen_time_utc: row.get(10)?,
        last_observed_at: row.get(11)?,
        retention_class: row.get(12)?,
        revision_id: row.get(13)?,
        revision_hash: row.get(14)?,
        title: row.get(15)?,
        factual_summary: row.get(16)?,
        supersedes_revision_id: row.get(17)?,
        event_time: EvidenceTimestamp {
            utc: row.get(18)?,
            original: row.get(19)?,
        },
        publish_time: EvidenceTimestamp {
            utc: row.get(20)?,
            original: row.get(21)?,
        },
        first_seen_time_utc: row.get(22)?,
        revision_time: EvidenceTimestamp {
            utc: row.get(23)?,
            original: row.get(24)?,
        },
        raw_snapshot_hash: row.get(25)?,
    })
}

fn validate_input(input: &NewsArchiveInput) -> Result<()> {
    for (name, value) in [
        ("canonical_url", input.canonical_url.as_str()),
        ("source_id", input.source_id.as_str()),
        ("source_name", input.source_name.as_str()),
        ("license", input.license.as_str()),
        ("content_type", input.content_type.as_str()),
        ("language", input.language.as_str()),
        ("parser_version", input.parser_version.as_str()),
        ("title", input.title.as_str()),
        ("retention_class", input.retention_class.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(Error::Invalid(format!("{name} 不能为空")));
        }
    }
    if input.canonical_url.len() > MAX_URL_BYTES {
        return Err(Error::Invalid("canonical_url 过长".into()));
    }
    if input.title.len() > MAX_TITLE_BYTES || input.factual_summary.len() > MAX_SUMMARY_BYTES {
        return Err(Error::Invalid("标题或事实摘要超过归档上限".into()));
    }
    if input
        .raw_snapshot
        .as_ref()
        .is_some_and(|raw| raw.len() > MAX_RAW_BYTES)
    {
        return Err(Error::Invalid("原始快照超过 2 MiB".into()));
    }
    validate_observation(&input.observation)
}

fn validate_observation(input: &NewsObservationInput) -> Result<()> {
    if input.provider_id.trim().is_empty() || input.parse_status.trim().is_empty() {
        return Err(Error::Invalid("观测来源和解析状态不能为空".into()));
    }
    if input
        .raw_evidence
        .as_ref()
        .is_some_and(|raw| raw.len() > MAX_RAW_BYTES)
    {
        return Err(Error::Invalid("失败原始证据超过 2 MiB".into()));
    }
    Ok(())
}

fn sha256(parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part);
    }
    format!("{:x}", digest.finalize())
}

fn gzip(raw: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(raw)?;
    Ok(encoder.finish()?)
}

fn gunzip(raw: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = GzDecoder::new(raw);
    let mut output = Vec::new();
    decoder
        .read_to_end(&mut output)
        .map_err(|error| Error::Invalid(format!("原始快照损坏：{error}")))?;
    Ok(output)
}

fn public_endpoint(endpoint: &str) -> String {
    endpoint
        .split(['?', '#'])
        .next()
        .unwrap_or(endpoint)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StorageConfig;

    fn storage() -> (tempfile::TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        (dir, storage)
    }

    fn input(url: &str, title: &str, summary: &str, first_seen: i64) -> NewsArchiveInput {
        NewsArchiveInput {
            canonical_url: url.into(),
            source_id: "official".into(),
            source_name: "交易所公告".into(),
            license: "official-public-disclosure".into(),
            content_type: "announcement".into(),
            language: "zh-CN".into(),
            parser_version: "fixture-v1".into(),
            title: title.into(),
            factual_summary: summary.into(),
            raw_snapshot: Some(format!("{title}:{summary}").into_bytes()),
            raw_snapshot_permitted: true,
            event_time: EvidenceTimestamp {
                utc: Some(first_seen - 3_600),
                original: Some("2026-08-22 10:00:00 +08:00".into()),
            },
            publish_time: EvidenceTimestamp {
                utc: Some(first_seen - 1_800),
                original: Some("2026-08-22T10:30:00+08:00".into()),
            },
            first_seen_time_utc: first_seen,
            revision_time: EvidenceTimestamp {
                utc: Some(first_seen),
                original: Some("2026-08-22T11:00:00+08:00".into()),
            },
            retention_class: "research_evidence".into(),
            observation: {
                let mut observation = NewsObservationInput::success(
                    "official",
                    "https://example.com/feed?token=secret",
                );
                observation.fetched_at = first_seen;
                observation
            },
        }
    }

    #[tokio::test]
    async fn idempotent_upsert_creates_revision_chain_without_overwrite() {
        let (dir, storage) = storage();
        let mut first_input = input("https://example.com/a", "首次公告", "事实一", 2_000);
        first_input.observation.etag = Some("fixture-etag".into());
        first_input.observation.last_modified = Some("Sat, 22 Aug 2026 02:00:00 GMT".into());
        let first = storage.news_archive_upsert(first_input).await.unwrap();
        assert!(first.inserted_revision);
        let duplicate = storage
            .news_archive_upsert(input("https://example.com/a", "首次公告", "事实一", 2_100))
            .await
            .unwrap();
        assert!(!duplicate.inserted_revision);
        assert_eq!(first.revision_id, duplicate.revision_id);

        let mut correction = input("https://example.com/a", "更正公告", "事实二", 2_200);
        correction.revision_time.utc = Some(2_200);
        let second = storage.news_archive_upsert(correction).await.unwrap();
        assert!(second.inserted_revision);
        assert_eq!(
            second.supersedes_revision_id,
            Some(first.revision_id.clone())
        );
        let revisions = storage
            .news_archive_revisions(&first.document_id)
            .await
            .unwrap();
        assert_eq!(revisions.len(), 2);
        assert_eq!(revisions[0].title, "首次公告");
        assert_eq!(revisions[1].title, "更正公告");
        assert_eq!(
            storage
                .news_revision_snapshot(&first.revision_id)
                .await
                .unwrap()
                .unwrap(),
            "首次公告:事实一".as_bytes()
        );
        storage
            .news_agent_evidence_link("task-1", "final_answer", &second.revision_id)
            .await
            .unwrap();
        let evidence = storage.news_agent_evidence("task-1").await.unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].revision_id, second.revision_id);
        storage
            .news_event_evidence_link("event-1", &second.revision_id, "corrects")
            .await
            .unwrap();
        assert_eq!(
            storage.news_event_evidence("event-1").await.unwrap().len(),
            1
        );
        let report = storage
            .news_archive_export_gzip(dir.path().join("full-history.jsonl.gz"), false)
            .await
            .unwrap();
        assert_eq!(report.revisions, 2, "export includes superseded revisions");
        let observations = storage
            .news_ingest_observations(Some("official"), 10)
            .await
            .unwrap();
        assert_eq!(observations.len(), 3);
        assert!(observations
            .iter()
            .all(|row| !row.endpoint.contains("token=")));
        assert!(observations
            .iter()
            .any(|row| row.etag.as_deref() == Some("fixture-etag")));
    }

    #[tokio::test]
    async fn four_clock_replay_never_exposes_unseen_future_revision() {
        let (_dir, storage) = storage();
        storage
            .news_archive_upsert(input(
                "https://example.com/time",
                "旧事件晚发现",
                "事实",
                2_000,
            ))
            .await
            .unwrap();
        assert!(storage
            .news_archive_as_of(1_999, NewsArchiveClock::Event, 10)
            .await
            .unwrap()
            .is_empty());
        let by_event = storage
            .news_archive_as_of(2_000, NewsArchiveClock::Event, 10)
            .await
            .unwrap();
        let by_seen = storage
            .news_archive_as_of(2_000, NewsArchiveClock::FirstSeen, 10)
            .await
            .unwrap();
        assert_eq!(by_event.len(), 1);
        assert_eq!(by_seen.len(), 1);
        assert_eq!(by_event[0].event_time.utc, Some(-1_600));
        assert_eq!(by_seen[0].first_seen_time_utc, 2_000);
    }

    #[tokio::test]
    async fn restart_restores_archive_and_provider_state_with_exact_stale_age() {
        let dir = tempfile::tempdir().unwrap();
        let config = StorageConfig::with_base_dir(dir.path());
        let storage = Storage::open(config.clone()).unwrap();
        storage
            .news_archive_upsert(input("https://example.com/restart", "公告", "事实", 10_000))
            .await
            .unwrap();
        storage
            .news_provider_state_put(NewsProviderArchiveState {
                provider_id: "official".into(),
                last_success_at: Some(10_000),
                last_observation_at: Some(10_000),
                last_latency_ms: Some(42),
                attempts: 3,
                failures: 1,
                last_error_kind: None,
                updated_at: 10_000,
            })
            .await
            .unwrap();
        drop(storage);
        let reopened = Storage::open(config).unwrap();
        let rows = reopened.news_archive_recent(10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].stale_age_secs(10_123), 123);
        assert_eq!(
            reopened
                .news_provider_state_get("official")
                .await
                .unwrap()
                .unwrap()
                .last_latency_ms,
            Some(42)
        );
    }

    #[tokio::test]
    async fn concurrent_writes_are_serialized_and_remain_idempotent() {
        let (_dir, storage) = storage();
        let mut tasks = tokio::task::JoinSet::new();
        for index in 0..32 {
            let storage = storage.clone();
            tasks.spawn(async move {
                storage
                    .news_archive_upsert(input(
                        &format!("https://example.com/concurrent/{}", index % 8),
                        &format!("公告 {}", index % 8),
                        "同一事实",
                        20_000,
                    ))
                    .await
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap().unwrap();
        }
        assert_eq!(storage.news_archive_recent(100).await.unwrap().len(), 8);
        assert_eq!(storage.news_archive_integrity_check().await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn corrupt_snapshot_does_not_hide_metadata_and_is_diagnosable() {
        let (_dir, storage) = storage();
        let saved = storage
            .news_archive_upsert(input("https://example.com/corrupt", "公告", "事实", 30_000))
            .await
            .unwrap();
        let revision = saved.revision_id.clone();
        storage
            .run(move |conn| {
                conn.execute(
                    "UPDATE document_revisions SET raw_snapshot_gzip=x'000102' WHERE revision_id=?1",
                    params![revision],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(storage
            .news_revision_snapshot(&saved.revision_id)
            .await
            .is_err());
        assert_eq!(storage.news_archive_recent(10).await.unwrap().len(), 1);
        assert_eq!(storage.news_archive_integrity_check().await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn large_archive_exports_compressed_and_cleanup_preserves_revision_metadata() {
        let (dir, storage) = storage();
        for index in 0..500 {
            let mut row = input(
                &format!("https://example.com/bulk/{index}"),
                &format!("公告 {index}"),
                "批量归档事实摘要",
                40_000 + index,
            );
            row.revision_time.utc = Some(40_000 + index);
            row.observation.fetched_at = 40_000 + index;
            storage.news_archive_upsert(row).await.unwrap();
        }
        let export = dir.path().join("news-archive.jsonl.gz");
        let report = storage
            .news_archive_export_gzip(export.clone(), false)
            .await
            .unwrap();
        assert_eq!(report.revisions, 500);
        assert!(report.compressed_bytes > 0);
        let mut decoder = GzDecoder::new(std::fs::File::open(export).unwrap());
        let mut text = String::new();
        decoder.read_to_string(&mut text).unwrap();
        assert_eq!(text.lines().count(), 500);

        let cleanup = storage
            .news_archive_cleanup(
                NewsArchiveRetentionPolicy {
                    raw_snapshot_days: 0,
                    observation_days: 0,
                },
                100_000,
            )
            .await
            .unwrap();
        assert_eq!(cleanup.raw_snapshots_pruned, 500);
        assert_eq!(cleanup.observations_deleted, 500);
        assert_eq!(storage.news_archive_recent(1_000).await.unwrap().len(), 500);
    }
}
