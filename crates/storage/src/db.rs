//! SQLite metadata store: connection worker and migrations.
//!
//! `rusqlite` is synchronous, so the single [`Connection`] lives on a
//! dedicated blocking thread. Callers submit closures through
//! [`Db::run`] (async) and get their result back over a oneshot channel.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::{backup::Backup, Connection};
use tokio::sync::oneshot;

use crate::error::{Error, Result};

/// Numbered migrations, applied in order. Each runs inside a transaction and
/// bumps the `user_version` pragma, making re-runs idempotent.
pub(crate) const MIGRATIONS: &[(u32, &str)] = &[
    (
        1,
        r#"
    CREATE TABLE IF NOT EXISTS securities (
        code       TEXT PRIMARY KEY,
        name       TEXT NOT NULL,
        market     TEXT NOT NULL,
        classify   TEXT,
        updated_at INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS kv (
        key        TEXT PRIMARY KEY,
        value      TEXT NOT NULL,
        updated_at INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS watchlist (
        group_name TEXT NOT NULL,
        code       TEXT NOT NULL,
        added_at   INTEGER NOT NULL,
        pinned     INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (group_name, code)
    );

    CREATE TABLE IF NOT EXISTS conversations (
        id         TEXT PRIMARY KEY,
        title      TEXT,
        created_at INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS messages (
        id              TEXT PRIMARY KEY,
        conversation_id TEXT NOT NULL REFERENCES conversations(id),
        role            TEXT NOT NULL,
        content         TEXT NOT NULL,
        tool_calls      TEXT,
        created_at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_messages_conversation
        ON messages(conversation_id, created_at);

    CREATE TABLE IF NOT EXISTS tool_cache (
        cache_key    TEXT PRIMARY KEY,
        tool         TEXT NOT NULL,
        params_json  TEXT NOT NULL,
        result_json  TEXT NOT NULL,
        data_version TEXT,
        created_at   INTEGER NOT NULL,
        ttl_seconds  INTEGER NOT NULL,
        accessed_at  INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS reports (
        id           TEXT PRIMARY KEY,
        kind         TEXT NOT NULL,
        title        TEXT NOT NULL,
        content_json TEXT NOT NULL,
        created_at   INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS predictions (
        id            TEXT PRIMARY KEY,
        symbol        TEXT NOT NULL,
        thesis        TEXT NOT NULL,
        expectation   TEXT,
        probability   REAL,
        horizon       TEXT,
        invalidation  TEXT,
        snapshot_json TEXT,
        created_at    INTEGER NOT NULL,
        outcome       TEXT,
        reviewed_at   INTEGER
    );

    CREATE TABLE IF NOT EXISTS meta_kv (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    "#,
    ),
    (
        2,
        r#"
    CREATE TABLE IF NOT EXISTS agent_tasks (
        id         TEXT PRIMARY KEY,
        kind       TEXT NOT NULL,
        status     TEXT NOT NULL,
        state_json TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
    "#,
    ),
    (
        3,
        r#"
    -- Supply-chain knowledge graph (see the astock-graph crate).
    -- node kind: company|product|segment|material|commodity|industry|region|policy
    CREATE TABLE IF NOT EXISTS graph_nodes (
        id         TEXT PRIMARY KEY,
        kind       TEXT NOT NULL,
        name       TEXT NOT NULL,
        code       TEXT,
        meta_json  TEXT NOT NULL DEFAULT '{}',
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_graph_nodes_code ON graph_nodes(code);

    -- edge relation: supplies|customer_of|competes|substitutes|exposed_to|
    --                belongs_to|produces|consumes
    CREATE TABLE IF NOT EXISTS graph_edges (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        src         TEXT NOT NULL,
        dst         TEXT NOT NULL,
        relation    TEXT NOT NULL,
        weight      REAL NOT NULL DEFAULT 1.0,
        source_name TEXT NOT NULL DEFAULT '',
        source_url  TEXT NOT NULL DEFAULT '',
        confidence  REAL NOT NULL DEFAULT 0.5,
        valid_from  INTEGER NOT NULL DEFAULT 0,
        valid_to    INTEGER,
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL,
        UNIQUE(src, dst, relation)
    );
    CREATE INDEX IF NOT EXISTS idx_graph_edges_src ON graph_edges(src);
    CREATE INDEX IF NOT EXISTS idx_graph_edges_dst ON graph_edges(dst);

    -- events.subject references a graph node id (or a company code / name
    -- resolvable to one); direction: +1 up/positive, -1 down/negative.
    CREATE TABLE IF NOT EXISTS events (
        id          TEXT PRIMARY KEY,
        kind        TEXT NOT NULL,
        title       TEXT NOT NULL,
        subject     TEXT NOT NULL,
        magnitude   REAL,
        direction   INTEGER NOT NULL,
        occurred_at INTEGER NOT NULL,
        source_name TEXT NOT NULL DEFAULT '',
        source_url  TEXT NOT NULL DEFAULT '',
        status      TEXT NOT NULL DEFAULT 'new',
        created_at  INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_events_occurred ON events(occurred_at);
    "#,
    ),
    (
        4,
        r#"
    -- Corporate actions (分红/送转/配股), one row per (code, ex_date).
    -- Source: EastMoney RPT_SHAREBONUS_DET (cash/bonus/transfer) extended
    -- with rights-issue data when a source is available. Dates are TEXT
    -- 'YYYY-MM-DD'; ratios are per share (10送10 -> bonus_share = 1.0).
    -- See docs/data-foundation-v2.md §Schema 变更.
    CREATE TABLE IF NOT EXISTS corporate_actions (
        code         TEXT NOT NULL,
        ex_date      TEXT NOT NULL,
        notice_date  TEXT,
        cash_div     REAL NOT NULL DEFAULT 0,
        bonus_share  REAL NOT NULL DEFAULT 0,
        rights_ratio REAL NOT NULL DEFAULT 0,
        rights_price REAL,
        source       TEXT NOT NULL DEFAULT '',
        source_url   TEXT NOT NULL DEFAULT '',
        fetched_at   INTEGER NOT NULL,
        PRIMARY KEY (code, ex_date)
    );
    "#,
    ),
    (
        5,
        r#"
    -- Generic kv settings gain a write timestamp so the typed kv_* API can
    -- report when a value was stored (used e.g. by provider credentials).
    -- Rows predating this migration keep fetched_at = 0 (unknown).
    ALTER TABLE kv ADD COLUMN fetched_at INTEGER NOT NULL DEFAULT 0;
    "#,
    ),
    (
        6,
        r#"
    -- SecurityMaster v2: canonical identity, classification, aliases,
    -- source lineage and validity windows. Existing v1 rows remain valid.
    ALTER TABLE securities ADD COLUMN board TEXT NOT NULL DEFAULT 'other';
    ALTER TABLE securities ADD COLUMN asset_type TEXT NOT NULL DEFAULT 'unknown';
    ALTER TABLE securities ADD COLUMN aliases_json TEXT NOT NULL DEFAULT '[]';
    ALTER TABLE securities ADD COLUMN industry TEXT;
    ALTER TABLE securities ADD COLUMN concepts_json TEXT NOT NULL DEFAULT '[]';
    ALTER TABLE securities ADD COLUMN region TEXT;
    ALTER TABLE securities ADD COLUMN source TEXT NOT NULL DEFAULT '';
    ALTER TABLE securities ADD COLUMN source_url TEXT;
    ALTER TABLE securities ADD COLUMN valid_from INTEGER;
    ALTER TABLE securities ADD COLUMN valid_to INTEGER;
    ALTER TABLE securities ADD COLUMN refreshed_at INTEGER NOT NULL DEFAULT 0;
    CREATE INDEX IF NOT EXISTS idx_securities_name ON securities(name);
    CREATE INDEX IF NOT EXISTS idx_securities_board ON securities(board);
    "#,
    ),
    (
        7,
        r#"
    -- Append-only Agent tool permission audit. Deliberately stores only
    -- metadata and a one-way arguments fingerprint: never request/response
    -- bodies, credentials, raw parameters or provider error text.
    CREATE TABLE IF NOT EXISTS agent_tool_audit (
        id                INTEGER PRIMARY KEY AUTOINCREMENT,
        task_id           TEXT NOT NULL,
        call_id           TEXT NOT NULL,
        tool              TEXT NOT NULL,
        permission_domain TEXT NOT NULL,
        origin            TEXT NOT NULL,
        args_fingerprint  TEXT NOT NULL,
        event             TEXT NOT NULL,
        elapsed_ms        INTEGER,
        created_at        INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_agent_tool_audit_task
        ON agent_tool_audit(task_id, id);
    CREATE INDEX IF NOT EXISTS idx_agent_tool_audit_call
        ON agent_tool_audit(call_id, id);
    "#,
    ),
    (
        8,
        r#"
    -- Durable, revisioned research evidence archive. Documents identify a
    -- source URL while immutable revisions preserve every observed content
    -- change. Four independent clocks prevent publish/event-time leakage.
    CREATE TABLE IF NOT EXISTS source_documents (
        document_id          TEXT PRIMARY KEY,
        canonical_url        TEXT NOT NULL,
        source_id            TEXT NOT NULL,
        source_name          TEXT NOT NULL,
        license              TEXT NOT NULL,
        content_type         TEXT NOT NULL,
        language             TEXT NOT NULL,
        parser_version       TEXT NOT NULL,
        content_hash         TEXT NOT NULL,
        current_revision_id  TEXT,
        first_seen_time_utc  INTEGER NOT NULL,
        last_observed_at     INTEGER NOT NULL,
        retention_class      TEXT NOT NULL DEFAULT 'research_evidence',
        created_at           INTEGER NOT NULL,
        updated_at           INTEGER NOT NULL,
        UNIQUE(source_id, canonical_url)
    );
    CREATE INDEX IF NOT EXISTS idx_source_documents_seen
        ON source_documents(first_seen_time_utc DESC);
    CREATE INDEX IF NOT EXISTS idx_source_documents_source
        ON source_documents(source_id, last_observed_at DESC);

    CREATE TABLE IF NOT EXISTS document_revisions (
        revision_id              TEXT PRIMARY KEY,
        document_id              TEXT NOT NULL REFERENCES source_documents(document_id),
        revision_hash            TEXT NOT NULL,
        title                    TEXT NOT NULL,
        factual_summary          TEXT NOT NULL,
        raw_snapshot_gzip        BLOB,
        raw_snapshot_hash        TEXT,
        supersedes_revision_id   TEXT,
        event_time_utc           INTEGER,
        event_time_original      TEXT,
        publish_time_utc         INTEGER,
        publish_time_original    TEXT,
        first_seen_time_utc      INTEGER NOT NULL,
        revision_time_utc        INTEGER NOT NULL,
        revision_time_original   TEXT,
        created_at               INTEGER NOT NULL,
        UNIQUE(document_id, revision_hash)
    );
    CREATE INDEX IF NOT EXISTS idx_document_revisions_document
        ON document_revisions(document_id, revision_time_utc DESC);
    CREATE INDEX IF NOT EXISTS idx_document_revisions_event
        ON document_revisions(event_time_utc, first_seen_time_utc);
    CREATE INDEX IF NOT EXISTS idx_document_revisions_publish
        ON document_revisions(publish_time_utc, first_seen_time_utc);

    CREATE TABLE IF NOT EXISTS ingest_observations (
        observation_id       INTEGER PRIMARY KEY AUTOINCREMENT,
        document_id          TEXT,
        revision_id          TEXT,
        provider_id          TEXT NOT NULL,
        endpoint             TEXT NOT NULL DEFAULT '',
        fetched_at           INTEGER NOT NULL,
        http_status          INTEGER,
        etag                 TEXT,
        last_modified        TEXT,
        latency_ms           INTEGER,
        parse_status         TEXT NOT NULL,
        parse_error          TEXT,
        raw_evidence_gzip    BLOB,
        raw_evidence_hash    TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_ingest_observations_provider
        ON ingest_observations(provider_id, fetched_at DESC);
    CREATE INDEX IF NOT EXISTS idx_ingest_observations_revision
        ON ingest_observations(revision_id, fetched_at DESC);

    CREATE TABLE IF NOT EXISTS document_event_evidence (
        event_id        TEXT NOT NULL,
        revision_id     TEXT NOT NULL REFERENCES document_revisions(revision_id),
        relation        TEXT NOT NULL DEFAULT 'supports',
        created_at      INTEGER NOT NULL,
        PRIMARY KEY(event_id, revision_id, relation)
    );

    CREATE TABLE IF NOT EXISTS agent_evidence_refs (
        task_id         TEXT NOT NULL,
        conclusion_key  TEXT NOT NULL,
        revision_id     TEXT NOT NULL REFERENCES document_revisions(revision_id),
        created_at      INTEGER NOT NULL,
        PRIMARY KEY(task_id, conclusion_key, revision_id)
    );
    CREATE INDEX IF NOT EXISTS idx_agent_evidence_refs_task
        ON agent_evidence_refs(task_id, created_at);

    CREATE TABLE IF NOT EXISTS news_provider_state (
        provider_id         TEXT PRIMARY KEY,
        last_success_at     INTEGER,
        last_observation_at INTEGER,
        last_latency_ms     INTEGER,
        attempts            INTEGER NOT NULL DEFAULT 0,
        failures            INTEGER NOT NULL DEFAULT 0,
        last_error_kind     TEXT,
        updated_at          INTEGER NOT NULL
    );
    "#,
    ),
    (
        9,
        r#"
    -- Versioned, explainable article-to-event clustering. Assignments are
    -- append-only decisions: model upgrades and manual corrections never
    -- silently rewrite historical membership.
    CREATE TABLE IF NOT EXISTS event_clusters (
        cluster_id             TEXT PRIMARY KEY,
        canonical_title        TEXT NOT NULL,
        event_time_utc         INTEGER,
        first_seen_time_utc    INTEGER NOT NULL,
        primary_revision_id    TEXT NOT NULL,
        first_source_id        TEXT NOT NULL,
        independent_sources    INTEGER NOT NULL DEFAULT 1,
        evidence_diversity     REAL NOT NULL DEFAULT 1.0,
        latest_revision_id     TEXT NOT NULL,
        conflict_fields_json   TEXT NOT NULL DEFAULT '[]',
        model_version          TEXT NOT NULL,
        status                 TEXT NOT NULL DEFAULT 'active',
        merged_into_cluster_id TEXT,
        created_at             INTEGER NOT NULL,
        updated_at             INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_event_clusters_seen
        ON event_clusters(first_seen_time_utc DESC);

    CREATE TABLE IF NOT EXISTS event_cluster_members (
        membership_id       INTEGER PRIMARY KEY AUTOINCREMENT,
        cluster_id          TEXT NOT NULL REFERENCES event_clusters(cluster_id),
        revision_id         TEXT NOT NULL REFERENCES document_revisions(revision_id),
        relationship        TEXT NOT NULL,
        merge_score         REAL NOT NULL,
        explanation_json    TEXT NOT NULL,
        old_republication   INTEGER NOT NULL DEFAULT 0,
        assigned_by         TEXT NOT NULL,
        model_version       TEXT NOT NULL,
        active              INTEGER NOT NULL DEFAULT 1,
        created_at          INTEGER NOT NULL
    );
    CREATE UNIQUE INDEX IF NOT EXISTS idx_event_cluster_member_active
        ON event_cluster_members(revision_id) WHERE active=1;
    CREATE INDEX IF NOT EXISTS idx_event_cluster_members_cluster
        ON event_cluster_members(cluster_id, active, created_at);

    CREATE TABLE IF NOT EXISTS event_cluster_decisions (
        decision_id       INTEGER PRIMARY KEY AUTOINCREMENT,
        revision_id       TEXT,
        from_cluster_id   TEXT,
        to_cluster_id     TEXT,
        action            TEXT NOT NULL,
        explanation_json  TEXT NOT NULL,
        model_version     TEXT NOT NULL,
        actor             TEXT NOT NULL,
        created_at        INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_event_cluster_decisions_revision
        ON event_cluster_decisions(revision_id, decision_id);

    CREATE TABLE IF NOT EXISTS event_fact_conflicts (
        cluster_id                 TEXT NOT NULL REFERENCES event_clusters(cluster_id),
        field_name                 TEXT NOT NULL,
        values_json                TEXT NOT NULL,
        authoritative_revision_id TEXT,
        status                     TEXT NOT NULL DEFAULT 'open',
        created_at                 INTEGER NOT NULL,
        updated_at                 INTEGER NOT NULL,
        PRIMARY KEY(cluster_id, field_name)
    );

    CREATE TABLE IF NOT EXISTS agent_conclusion_reviews (
        task_id             TEXT NOT NULL,
        conclusion_key      TEXT NOT NULL,
        triggering_revision TEXT NOT NULL,
        trigger_relation    TEXT NOT NULL,
        status               TEXT NOT NULL DEFAULT 'pending_review',
        created_at           INTEGER NOT NULL,
        reviewed_at          INTEGER,
        PRIMARY KEY(task_id, conclusion_key, triggering_revision)
    );
    CREATE INDEX IF NOT EXISTS idx_agent_conclusion_reviews_status
        ON agent_conclusion_reviews(status, created_at DESC);
    "#,
    ),
    (
        10,
        r#"
    -- Unified entity master and explainable document entity-link decisions.
    CREATE TABLE IF NOT EXISTS research_entities (
        entity_id          TEXT PRIMARY KEY,
        entity_type        TEXT NOT NULL,
        canonical_name     TEXT NOT NULL,
        listed_code        TEXT,
        market             TEXT,
        parent_entity_id   TEXT,
        source_name        TEXT NOT NULL,
        source_url         TEXT,
        valid_from         INTEGER,
        valid_to           INTEGER,
        metadata_json      TEXT NOT NULL DEFAULT '{}',
        created_at         INTEGER NOT NULL,
        updated_at         INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_research_entities_code
        ON research_entities(listed_code);
    CREATE INDEX IF NOT EXISTS idx_research_entities_name
        ON research_entities(canonical_name);

    CREATE TABLE IF NOT EXISTS research_entity_names (
        name_id         INTEGER PRIMARY KEY AUTOINCREMENT,
        entity_id       TEXT NOT NULL REFERENCES research_entities(entity_id),
        name_text       TEXT NOT NULL,
        normalized_name TEXT NOT NULL,
        name_type       TEXT NOT NULL,
        valid_from      INTEGER,
        valid_to        INTEGER,
        source_name     TEXT NOT NULL,
        source_url      TEXT,
        UNIQUE(entity_id, normalized_name, name_type, valid_from, valid_to)
    );
    CREATE INDEX IF NOT EXISTS idx_research_entity_names_normalized
        ON research_entity_names(normalized_name);

    CREATE TABLE IF NOT EXISTS research_entity_relations (
        relation_id       INTEGER PRIMARY KEY AUTOINCREMENT,
        from_entity_id    TEXT NOT NULL REFERENCES research_entities(entity_id),
        to_entity_id      TEXT NOT NULL REFERENCES research_entities(entity_id),
        relation_type     TEXT NOT NULL,
        confidence        REAL NOT NULL,
        evidence_revision_id TEXT,
        source_name       TEXT NOT NULL,
        source_url        TEXT,
        valid_from        INTEGER,
        valid_to          INTEGER,
        status            TEXT NOT NULL DEFAULT 'accepted',
        created_at        INTEGER NOT NULL,
        UNIQUE(from_entity_id,to_entity_id,relation_type,source_name)
    );

    CREATE TABLE IF NOT EXISTS document_entity_links (
        link_id             TEXT PRIMARY KEY,
        revision_id         TEXT NOT NULL REFERENCES document_revisions(revision_id),
        span_start          INTEGER NOT NULL,
        span_end            INTEGER NOT NULL,
        span_text           TEXT NOT NULL,
        candidates_json     TEXT NOT NULL,
        final_entity_id     TEXT,
        confidence          REAL NOT NULL,
        explanation_json    TEXT NOT NULL,
        linker_version      TEXT NOT NULL,
        evidence_revision_id TEXT NOT NULL,
        status              TEXT NOT NULL,
        proposed_by_model   INTEGER NOT NULL DEFAULT 0,
        created_at          INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_document_entity_links_revision
        ON document_entity_links(revision_id,linker_version,span_start);
    CREATE INDEX IF NOT EXISTS idx_document_entity_links_review
        ON document_entity_links(status,created_at DESC);

    CREATE TABLE IF NOT EXISTS entity_link_reviews (
        review_id        INTEGER PRIMARY KEY AUTOINCREMENT,
        link_id          TEXT NOT NULL REFERENCES document_entity_links(link_id),
        proposed_entity_id TEXT,
        decision         TEXT NOT NULL DEFAULT 'pending',
        reason           TEXT,
        reviewer         TEXT,
        created_at       INTEGER NOT NULL,
        reviewed_at      INTEGER,
        UNIQUE(link_id)
    );
    "#,
    ),
    (
        11,
        r#"
    -- Controlled source-document reads and field-level evidence.
    CREATE TABLE IF NOT EXISTS research_source_documents (
        source_document_id TEXT PRIMARY KEY,
        canonical_url      TEXT NOT NULL UNIQUE,
        current_version_id TEXT,
        authority_tier     TEXT NOT NULL,
        authority_name     TEXT NOT NULL,
        access_status      TEXT NOT NULL,
        failure_kind       TEXT,
        failure_message    TEXT,
        first_fetched_at   INTEGER NOT NULL,
        last_fetched_at    INTEGER NOT NULL,
        created_at         INTEGER NOT NULL,
        updated_at         INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS research_source_versions (
        source_version_id  TEXT PRIMARY KEY,
        source_document_id TEXT NOT NULL REFERENCES research_source_documents(source_document_id),
        content_hash       TEXT NOT NULL,
        extracted_hash     TEXT NOT NULL,
        media_type         TEXT NOT NULL,
        title              TEXT,
        published_at       INTEGER,
        fetched_at         INTEGER NOT NULL,
        parser_version     TEXT NOT NULL,
        supersedes_version_id TEXT,
        raw_snapshot_gzip  BLOB,
        raw_snapshot_hash  TEXT,
        reliability_score REAL NOT NULL,
        independence_score REAL NOT NULL,
        freshness_score   REAL NOT NULL,
        prompt_injection_detected INTEGER NOT NULL DEFAULT 0,
        UNIQUE(source_document_id,content_hash,parser_version)
    );
    CREATE INDEX IF NOT EXISTS idx_research_source_versions_fetched
        ON research_source_versions(fetched_at DESC);

    CREATE TABLE IF NOT EXISTS source_document_segments (
        segment_id        TEXT PRIMARY KEY,
        source_version_id TEXT NOT NULL REFERENCES research_source_versions(source_version_id),
        page_number       INTEGER,
        paragraph_index   INTEGER NOT NULL,
        selector          TEXT,
        span_start        INTEGER NOT NULL,
        span_end          INTEGER NOT NULL,
        text              TEXT NOT NULL,
        text_hash         TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_source_segments_version
        ON source_document_segments(source_version_id,paragraph_index);

    CREATE TABLE IF NOT EXISTS source_fact_evidence (
        fact_id           TEXT PRIMARY KEY,
        source_version_id TEXT NOT NULL REFERENCES research_source_versions(source_version_id),
        segment_id        TEXT NOT NULL REFERENCES source_document_segments(segment_id),
        fact_type         TEXT NOT NULL,
        field_name        TEXT NOT NULL,
        subject           TEXT,
        raw_value         TEXT NOT NULL,
        normalized_value  REAL,
        original_unit     TEXT,
        normalized_unit   TEXT,
        page_number       INTEGER,
        paragraph_index   INTEGER NOT NULL,
        span_start        INTEGER NOT NULL,
        span_end          INTEGER NOT NULL,
        created_at        INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_source_facts_version
        ON source_fact_evidence(source_version_id,field_name);

    CREATE TABLE IF NOT EXISTS source_fetch_observations (
        observation_id    INTEGER PRIMARY KEY AUTOINCREMENT,
        source_document_id TEXT NOT NULL REFERENCES research_source_documents(source_document_id),
        source_version_id TEXT,
        requested_url     TEXT NOT NULL,
        final_url         TEXT,
        media_type        TEXT,
        status            TEXT NOT NULL,
        failure_kind      TEXT,
        failure_message   TEXT,
        redirects_json    TEXT NOT NULL DEFAULT '[]',
        fetched_at        INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS agent_source_evidence_refs (
        task_id           TEXT NOT NULL,
        conclusion_key    TEXT NOT NULL,
        source_version_id TEXT NOT NULL REFERENCES research_source_versions(source_version_id),
        fact_id           TEXT NOT NULL DEFAULT '',
        created_at        INTEGER NOT NULL,
        PRIMARY KEY(task_id,conclusion_key,source_version_id,fact_id)
    );
    "#,
    ),
    (
        12,
        r#"
    -- Cross-dataset freshness, field lineage and reconciliation audit.
    CREATE TABLE IF NOT EXISTS data_quality_observations (
        observation_id       INTEGER PRIMARY KEY AUTOINCREMENT,
        dataset               TEXT NOT NULL,
        provider              TEXT NOT NULL,
        entity_key           TEXT,
        operation             TEXT NOT NULL,
        success               INTEGER NOT NULL,
        latency_ms            INTEGER,
        freshness_state       TEXT NOT NULL,
        age_secs              INTEGER NOT NULL,
        expected_cadence_secs INTEGER NOT NULL,
        stale_after_secs      INTEGER NOT NULL,
        hard_expiry_secs      INTEGER NOT NULL,
        missing_fields        INTEGER NOT NULL DEFAULT 0,
        conflicts             INTEGER NOT NULL DEFAULT 0,
        quality_flags_json    TEXT NOT NULL DEFAULT '[]',
        error_kind            TEXT,
        recorded_at           INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_quality_observations_slo
        ON data_quality_observations(dataset,provider,recorded_at DESC);

    CREATE TABLE IF NOT EXISTS field_lineage_records (
        lineage_id          INTEGER PRIMARY KEY AUTOINCREMENT,
        dataset             TEXT NOT NULL,
        entity_key          TEXT NOT NULL,
        field_path          TEXT NOT NULL,
        source              TEXT NOT NULL,
        source_url          TEXT,
        event_time          INTEGER,
        as_of_time          INTEGER,
        publish_time        INTEGER,
        fetched_at          INTEGER NOT NULL,
        parser_version      TEXT NOT NULL,
        schema_version      TEXT NOT NULL,
        license             TEXT NOT NULL,
        unit                TEXT,
        currency            TEXT,
        adjustment          TEXT NOT NULL,
        revision            TEXT,
        accounting_scope    TEXT NOT NULL,
        quality_flags_json  TEXT NOT NULL DEFAULT '[]',
        created_at          INTEGER NOT NULL,
        UNIQUE(dataset,entity_key,field_path,source,fetched_at,revision)
    );
    CREATE INDEX IF NOT EXISTS idx_field_lineage_lookup
        ON field_lineage_records(dataset,entity_key,field_path,created_at DESC);

    CREATE TABLE IF NOT EXISTS data_reconciliation_results (
        reconciliation_id INTEGER PRIMARY KEY AUTOINCREMENT,
        dataset            TEXT NOT NULL,
        entity_key         TEXT NOT NULL,
        field_path         TEXT NOT NULL,
        left_provider      TEXT NOT NULL,
        right_provider     TEXT NOT NULL,
        status             TEXT NOT NULL,
        blocking           INTEGER NOT NULL,
        result_json        TEXT NOT NULL,
        compared_at        INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_reconciliation_lookup
        ON data_reconciliation_results(dataset,entity_key,compared_at DESC);
    "#,
    ),
    (
        13,
        r#"
    -- Durable personal state for the professional news center. This table
    -- stores no upstream content and remains valid across provider refreshes.
    CREATE TABLE IF NOT EXISTS news_user_state (
        document_id TEXT PRIMARY KEY REFERENCES source_documents(document_id) ON DELETE CASCADE,
        is_read     INTEGER NOT NULL DEFAULT 0,
        pinned      INTEGER NOT NULL DEFAULT 0,
        favorite    INTEGER NOT NULL DEFAULT 0,
        ignored     INTEGER NOT NULL DEFAULT 0,
        updated_at  INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_news_user_state_flags
        ON news_user_state(pinned DESC,favorite DESC,ignored,is_read,updated_at DESC);
    "#,
    ),
    (
        14,
        r#"
    -- Canonical formal-disclosure data plane. Upstream entry points are kept
    -- separately so a mirror can never silently impersonate an official URL.
    CREATE TABLE IF NOT EXISTS disclosures (
        disclosure_id       TEXT PRIMARY KEY,
        stable_key          TEXT NOT NULL,
        title               TEXT NOT NULL,
        normalized_title    TEXT NOT NULL,
        category            TEXT NOT NULL,
        status              TEXT NOT NULL,
        published_at        INTEGER,
        publication_precision TEXT NOT NULL,
        first_seen_at       INTEGER NOT NULL,
        last_seen_at        INTEGER NOT NULL,
        revision_of         TEXT REFERENCES disclosures(disclosure_id),
        cancelled_by        TEXT REFERENCES disclosures(disclosure_id),
        source_version_id   TEXT,
        parser_version      TEXT NOT NULL,
        extraction_status   TEXT NOT NULL,
        review_reason       TEXT,
        created_at          INTEGER NOT NULL,
        updated_at          INTEGER NOT NULL,
        UNIQUE(stable_key,status)
    );
    CREATE INDEX IF NOT EXISTS idx_disclosures_timeline
        ON disclosures(published_at DESC,first_seen_at DESC);
    CREATE INDEX IF NOT EXISTS idx_disclosures_category
        ON disclosures(category,status,published_at DESC);

    CREATE TABLE IF NOT EXISTS disclosure_securities (
        disclosure_id TEXT NOT NULL REFERENCES disclosures(disclosure_id) ON DELETE CASCADE,
        security_code TEXT NOT NULL,
        security_name TEXT NOT NULL DEFAULT '',
        market        TEXT NOT NULL DEFAULT '',
        PRIMARY KEY(disclosure_id,security_code)
    );
    CREATE INDEX IF NOT EXISTS idx_disclosure_security_timeline
        ON disclosure_securities(security_code,disclosure_id);

    CREATE TABLE IF NOT EXISTS disclosure_sources (
        source_id       TEXT PRIMARY KEY,
        disclosure_id   TEXT NOT NULL REFERENCES disclosures(disclosure_id) ON DELETE CASCADE,
        provider_id     TEXT NOT NULL,
        provider_name   TEXT NOT NULL,
        authority       TEXT NOT NULL,
        entry_kind      TEXT NOT NULL,
        upstream_id     TEXT,
        original_url    TEXT NOT NULL,
        discovered_at   INTEGER NOT NULL,
        last_success_at INTEGER,
        latency_ms      INTEGER,
        is_primary      INTEGER NOT NULL DEFAULT 0,
        UNIQUE(provider_id,original_url)
    );
    CREATE INDEX IF NOT EXISTS idx_disclosure_sources_document
        ON disclosure_sources(disclosure_id,is_primary DESC,discovered_at);

    CREATE TABLE IF NOT EXISTS disclosure_attachments (
        attachment_id    TEXT PRIMARY KEY,
        disclosure_id    TEXT NOT NULL REFERENCES disclosures(disclosure_id) ON DELETE CASCADE,
        parent_attachment_id TEXT REFERENCES disclosure_attachments(attachment_id),
        name             TEXT NOT NULL,
        original_url     TEXT NOT NULL,
        media_type       TEXT NOT NULL,
        byte_size        INTEGER,
        content_hash     TEXT,
        source_version_id TEXT,
        extraction_status TEXT NOT NULL,
        page_count       INTEGER,
        parser_version   TEXT NOT NULL,
        review_reason    TEXT,
        UNIQUE(disclosure_id,original_url)
    );

    CREATE TABLE IF NOT EXISTS disclosure_events (
        event_id          TEXT PRIMARY KEY,
        disclosure_id     TEXT NOT NULL REFERENCES disclosures(disclosure_id) ON DELETE CASCADE,
        event_type        TEXT NOT NULL,
        fields_json       TEXT NOT NULL,
        evidence_json     TEXT NOT NULL,
        parser_version    TEXT NOT NULL,
        created_at        INTEGER NOT NULL,
        UNIQUE(disclosure_id,event_type,fields_json)
    );
    CREATE INDEX IF NOT EXISTS idx_disclosure_events_type
        ON disclosure_events(event_type,created_at DESC);

    CREATE TABLE IF NOT EXISTS disclosure_provider_state (
        provider_id       TEXT PRIMARY KEY,
        provider_name     TEXT NOT NULL,
        authority         TEXT NOT NULL,
        enabled           INTEGER NOT NULL DEFAULT 1,
        cursor_json       TEXT NOT NULL DEFAULT '{}',
        last_attempt_at   INTEGER,
        last_success_at   INTEGER,
        consecutive_failures INTEGER NOT NULL DEFAULT 0,
        retry_after       INTEGER,
        target_latency_secs INTEGER NOT NULL,
        last_error        TEXT,
        updated_at        INTEGER NOT NULL
    );

    -- Evidence coordinates are nullable because many HTML and legacy PDFs
    -- expose only spans. A scan with no reliable text is explicitly reviewed.
    ALTER TABLE source_document_segments ADD COLUMN attachment_id TEXT;
    ALTER TABLE source_document_segments ADD COLUMN page_x REAL;
    ALTER TABLE source_document_segments ADD COLUMN page_y REAL;
    ALTER TABLE source_document_segments ADD COLUMN page_width REAL;
    ALTER TABLE source_document_segments ADD COLUMN page_height REAL;
    ALTER TABLE source_document_segments ADD COLUMN table_index INTEGER;
    ALTER TABLE source_document_segments ADD COLUMN row_index INTEGER;
    ALTER TABLE source_document_segments ADD COLUMN column_index INTEGER;
    "#,
    ),
    (
        15,
        r#"
    -- Overseas primary-source documents and deterministic Global -> A-share
    -- transmission mappings. Original clocks, units and currencies are
    -- immutable; translations are stored beside, never over, source text.
    CREATE TABLE IF NOT EXISTS global_provider_state (
        provider_id       TEXT PRIMARY KEY,
        provider_name     TEXT NOT NULL,
        region            TEXT NOT NULL,
        category          TEXT NOT NULL,
        official_url      TEXT NOT NULL,
        original_timezone TEXT NOT NULL,
        license_policy    TEXT NOT NULL,
        credential_env    TEXT,
        enabled           INTEGER NOT NULL DEFAULT 1,
        target_latency_secs INTEGER NOT NULL,
        rate_limit_per_minute INTEGER NOT NULL,
        cursor_json       TEXT NOT NULL DEFAULT '{}',
        last_attempt_at   INTEGER,
        last_success_at   INTEGER,
        consecutive_failures INTEGER NOT NULL DEFAULT 0,
        retry_after       INTEGER,
        last_error        TEXT,
        updated_at        INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS global_documents (
        document_id       TEXT PRIMARY KEY,
        provider_id       TEXT NOT NULL REFERENCES global_provider_state(provider_id),
        upstream_id       TEXT NOT NULL,
        document_type     TEXT NOT NULL,
        title_original    TEXT NOT NULL,
        title_zh          TEXT,
        original_language TEXT NOT NULL,
        original_url      TEXT NOT NULL,
        source_version_id TEXT,
        content_hash      TEXT,
        published_at_utc  INTEGER NOT NULL,
        published_local   TEXT NOT NULL,
        published_timezone TEXT NOT NULL,
        utc_offset_seconds INTEGER NOT NULL,
        first_seen_at     INTEGER NOT NULL,
        revision_no       INTEGER NOT NULL DEFAULT 1,
        revision_of       TEXT REFERENCES global_documents(document_id),
        primary_verified  INTEGER NOT NULL DEFAULT 0,
        translation_status TEXT NOT NULL,
        gap_reason        TEXT,
        license_policy    TEXT NOT NULL,
        created_at        INTEGER NOT NULL,
        updated_at        INTEGER NOT NULL,
        UNIQUE(provider_id,upstream_id,revision_no)
    );
    CREATE INDEX IF NOT EXISTS idx_global_documents_timeline
        ON global_documents(published_at_utc DESC,first_seen_at DESC);

    CREATE TABLE IF NOT EXISTS global_entities (
        entity_id         TEXT PRIMARY KEY,
        entity_type       TEXT NOT NULL,
        legal_name        TEXT NOT NULL,
        name_zh           TEXT,
        jurisdiction      TEXT NOT NULL,
        identifiers_json  TEXT NOT NULL DEFAULT '{}',
        aliases_json      TEXT NOT NULL DEFAULT '[]',
        translation_status TEXT NOT NULL,
        updated_at        INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS global_relations (
        relation_id       TEXT PRIMARY KEY,
        src_entity_id     TEXT NOT NULL REFERENCES global_entities(entity_id),
        dst_entity_id     TEXT NOT NULL REFERENCES global_entities(entity_id),
        relation_type     TEXT NOT NULL,
        direction         TEXT NOT NULL,
        confidence_bps    INTEGER NOT NULL CHECK(confidence_bps BETWEEN 0 AND 10000),
        evidence_document_id TEXT NOT NULL REFERENCES global_documents(document_id),
        evidence_source_version_id TEXT NOT NULL,
        evidence_quote_original TEXT NOT NULL,
        evidence_quote_zh TEXT,
        evidence_location_json TEXT NOT NULL,
        observed_at       INTEGER NOT NULL,
        valid_from        INTEGER NOT NULL,
        valid_to          INTEGER,
        status            TEXT NOT NULL,
        created_at        INTEGER NOT NULL,
        updated_at        INTEGER NOT NULL,
        UNIQUE(src_entity_id,dst_entity_id,relation_type,evidence_source_version_id)
    );
    CREATE INDEX IF NOT EXISTS idx_global_relations_src
        ON global_relations(src_entity_id,status,valid_from);
    CREATE INDEX IF NOT EXISTS idx_global_relations_dst
        ON global_relations(dst_entity_id,status,valid_from);

    CREATE TABLE IF NOT EXISTS global_observations (
        observation_id    TEXT PRIMARY KEY,
        document_id       TEXT NOT NULL REFERENCES global_documents(document_id),
        entity_id         TEXT REFERENCES global_entities(entity_id),
        indicator_code    TEXT NOT NULL,
        period            TEXT NOT NULL,
        value_scaled      INTEGER,
        scale             INTEGER NOT NULL DEFAULT 1,
        value_text        TEXT,
        unit_original     TEXT NOT NULL,
        currency_original TEXT,
        released_at_utc   INTEGER NOT NULL,
        revision_no       INTEGER NOT NULL DEFAULT 1,
        replaces_observation_id TEXT REFERENCES global_observations(observation_id),
        source_version_id TEXT NOT NULL,
        created_at        INTEGER NOT NULL,
        UNIQUE(document_id,indicator_code,period,revision_no)
    );
    CREATE INDEX IF NOT EXISTS idx_global_observations_pit
        ON global_observations(indicator_code,period,released_at_utc,revision_no);

    CREATE TABLE IF NOT EXISTS global_fx_rates (
        rate_id            TEXT PRIMARY KEY,
        base_currency      TEXT NOT NULL,
        quote_currency     TEXT NOT NULL,
        rate_scaled        INTEGER NOT NULL,
        scale              INTEGER NOT NULL,
        effective_at_utc   INTEGER NOT NULL,
        released_at_utc    INTEGER NOT NULL,
        revision_no        INTEGER NOT NULL DEFAULT 1,
        source_version_id  TEXT NOT NULL,
        UNIQUE(base_currency,quote_currency,effective_at_utc,revision_no)
    );
    "#,
    ),
    (
        16,
        r#"
    -- Evidence-bound event ontology and market price-in research. These
    -- tables deliberately separate source facts, analytical assumptions,
    -- lifecycle transitions and market assessments.
    CREATE TABLE IF NOT EXISTS structured_events (
        event_id                TEXT PRIMARY KEY,
        source_revision_id      TEXT NOT NULL UNIQUE,
        ontology_kind           TEXT NOT NULL,
        title                   TEXT NOT NULL,
        subjects_json           TEXT NOT NULL DEFAULT '[]',
        objects_json            TEXT NOT NULL DEFAULT '[]',
        amount_text             TEXT,
        quantity_text           TEXT,
        unit_original           TEXT,
        currency_original       TEXT,
        baseline_period         TEXT,
        starts_at               INTEGER,
        ends_at                 INTEGER,
        region                  TEXT,
        conditions_json         TEXT NOT NULL DEFAULT '[]',
        official_effective      INTEGER,
        reversibility           TEXT NOT NULL,
        impact_horizon          TEXT NOT NULL,
        lifecycle_status        TEXT NOT NULL,
        catalyst_path_json      TEXT NOT NULL DEFAULT '[]',
        validation_dates_json   TEXT NOT NULL DEFAULT '[]',
        invalidation_json       TEXT NOT NULL DEFAULT '[]',
        missing_fields_json     TEXT NOT NULL DEFAULT '[]',
        extraction_version      TEXT NOT NULL,
        created_at              INTEGER NOT NULL,
        updated_at              INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_structured_events_timeline
        ON structured_events(updated_at DESC,ontology_kind,lifecycle_status);

    CREATE TABLE IF NOT EXISTS event_field_evidence (
        evidence_id             TEXT PRIMARY KEY,
        event_id                TEXT NOT NULL REFERENCES structured_events(event_id),
        field_name              TEXT NOT NULL,
        provenance_kind         TEXT NOT NULL,
        source_revision_id      TEXT,
        source_version_id       TEXT,
        quote_original          TEXT,
        quote_zh                TEXT,
        location_json           TEXT NOT NULL DEFAULT '{}',
        observed_at             INTEGER NOT NULL,
        confidence_bps          INTEGER NOT NULL CHECK(confidence_bps BETWEEN 0 AND 10000),
        created_at              INTEGER NOT NULL,
        UNIQUE(event_id,field_name,provenance_kind,source_revision_id,quote_original)
    );
    CREATE INDEX IF NOT EXISTS idx_event_field_evidence_event
        ON event_field_evidence(event_id,field_name);

    CREATE TABLE IF NOT EXISTS event_state_transitions (
        transition_id           TEXT PRIMARY KEY,
        event_id                TEXT NOT NULL REFERENCES structured_events(event_id),
        from_status             TEXT NOT NULL,
        to_status               TEXT NOT NULL,
        reason                  TEXT NOT NULL,
        evidence_id             TEXT REFERENCES event_field_evidence(evidence_id),
        transitioned_at         INTEGER NOT NULL,
        created_at              INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_event_state_transitions_event
        ON event_state_transitions(event_id,transitioned_at);

    CREATE TABLE IF NOT EXISTS event_market_assessments (
        assessment_id           TEXT PRIMARY KEY,
        event_id                TEXT NOT NULL REFERENCES structured_events(event_id),
        security_code           TEXT NOT NULL,
        as_of_date              TEXT NOT NULL,
        fundamental_json        TEXT NOT NULL,
        market_opportunity_json TEXT NOT NULL,
        expectation_gap_json    TEXT NOT NULL,
        diagnostics_json        TEXT NOT NULL,
        missing_inputs_json     TEXT NOT NULL DEFAULT '[]',
        data_versions_json      TEXT NOT NULL DEFAULT '{}',
        created_at              INTEGER NOT NULL,
        UNIQUE(event_id,security_code,as_of_date)
    );
    CREATE INDEX IF NOT EXISTS idx_event_market_assessments_latest
        ON event_market_assessments(event_id,security_code,created_at DESC);

    CREATE TABLE IF NOT EXISTS event_study_samples (
        sample_id               TEXT PRIMARY KEY,
        event_id                TEXT NOT NULL REFERENCES structured_events(event_id),
        ontology_kind           TEXT NOT NULL,
        security_code           TEXT NOT NULL,
        event_date              TEXT NOT NULL,
        pre_window_days         INTEGER NOT NULL,
        post_window_days        INTEGER NOT NULL,
        pre_abnormal_return_bps INTEGER,
        post_abnormal_return_bps INTEGER,
        abnormal_volume_bps     INTEGER,
        valuation_change_bps    INTEGER,
        fundamental_direction  TEXT NOT NULL,
        source_revision_id      TEXT NOT NULL,
        data_version            TEXT NOT NULL,
        created_at              INTEGER NOT NULL,
        UNIQUE(event_id,security_code,post_window_days,data_version)
    );
    CREATE INDEX IF NOT EXISTS idx_event_study_calibration
        ON event_study_samples(ontology_kind,post_window_days,event_date);
    "#,
    ),
    (
        17,
        r#"
    -- Versioned supply-chain relation extraction. Model output is only a
    -- candidate: exact evidence, entity hierarchy and human publication are
    -- recorded independently so re-extraction never overwrites old evidence.
    CREATE TABLE IF NOT EXISTS relation_extraction_runs (
        run_id              TEXT PRIMARY KEY,
        source_version_id   TEXT NOT NULL,
        document_kind       TEXT NOT NULL,
        extractor_kind      TEXT NOT NULL,
        model_id            TEXT,
        model_version       TEXT,
        schema_version      TEXT NOT NULL,
        input_hash          TEXT NOT NULL,
        status              TEXT NOT NULL,
        candidate_count     INTEGER NOT NULL DEFAULT 0,
        validation_errors   INTEGER NOT NULL DEFAULT 0,
        started_at          INTEGER NOT NULL,
        completed_at        INTEGER,
        error               TEXT,
        UNIQUE(source_version_id,extractor_kind,model_id,model_version,schema_version,input_hash)
    );
    CREATE INDEX IF NOT EXISTS idx_relation_runs_source
        ON relation_extraction_runs(source_version_id,started_at DESC);

    CREATE TABLE IF NOT EXISTS relation_candidates (
        candidate_id             TEXT PRIMARY KEY,
        run_id                   TEXT NOT NULL REFERENCES relation_extraction_runs(run_id),
        source_version_id        TEXT NOT NULL,
        document_kind            TEXT NOT NULL,
        subject_text             TEXT NOT NULL,
        object_text              TEXT NOT NULL,
        relation_type            TEXT NOT NULL,
        product_text             TEXT,
        amount_text              TEXT,
        share_bps                INTEGER,
        report_period            TEXT,
        region                   TEXT,
        subject_entity_id        TEXT,
        object_entity_id         TEXT,
        subject_parent_entity_id TEXT,
        object_parent_entity_id  TEXT,
        disclosure_mode          TEXT NOT NULL DEFAULT 'named',
        confidence_bps           INTEGER NOT NULL CHECK(confidence_bps BETWEEN 0 AND 10000),
        validation_status        TEXT NOT NULL,
        validation_json          TEXT NOT NULL DEFAULT '[]',
        review_status            TEXT NOT NULL DEFAULT 'pending_review',
        confidential             INTEGER NOT NULL DEFAULT 0,
        non_inferable            INTEGER NOT NULL DEFAULT 0,
        candidate_version        INTEGER NOT NULL DEFAULT 1,
        proposed_by_model        INTEGER NOT NULL DEFAULT 0,
        created_at               INTEGER NOT NULL,
        updated_at               INTEGER NOT NULL,
        UNIQUE(run_id,subject_text,object_text,relation_type,product_text,source_version_id)
    );
    CREATE INDEX IF NOT EXISTS idx_relation_candidates_review
        ON relation_candidates(review_status,confidence_bps DESC,created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_relation_candidates_entities
        ON relation_candidates(subject_parent_entity_id,object_parent_entity_id,relation_type);

    CREATE TABLE IF NOT EXISTS relation_candidate_evidence (
        evidence_id          TEXT PRIMARY KEY,
        candidate_id         TEXT NOT NULL REFERENCES relation_candidates(candidate_id),
        source_version_id    TEXT NOT NULL,
        segment_id           TEXT NOT NULL,
        page_number          INTEGER,
        paragraph_index      INTEGER NOT NULL,
        span_start           INTEGER NOT NULL,
        span_end             INTEGER NOT NULL,
        quote_original       TEXT NOT NULL,
        independent_group    TEXT NOT NULL,
        polarity             TEXT NOT NULL DEFAULT 'supports',
        created_at           INTEGER NOT NULL,
        UNIQUE(candidate_id,source_version_id,segment_id,span_start,span_end,polarity)
    );
    CREATE INDEX IF NOT EXISTS idx_relation_evidence_candidate
        ON relation_candidate_evidence(candidate_id,created_at);

    CREATE TABLE IF NOT EXISTS relation_candidate_reviews (
        review_id            INTEGER PRIMARY KEY AUTOINCREMENT,
        candidate_id         TEXT NOT NULL REFERENCES relation_candidates(candidate_id),
        decision             TEXT NOT NULL,
        reviewer             TEXT NOT NULL,
        reason               TEXT NOT NULL,
        modified_json        TEXT,
        merged_entity_id     TEXT,
        dataset_split        TEXT,
        training_eligible    INTEGER NOT NULL DEFAULT 0,
        reviewed_at          INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_relation_reviews_candidate
        ON relation_candidate_reviews(candidate_id,reviewed_at DESC);

    CREATE TABLE IF NOT EXISTS relation_publications (
        publication_id       TEXT PRIMARY KEY,
        candidate_id         TEXT NOT NULL REFERENCES relation_candidates(candidate_id),
        graph_edge_id        INTEGER,
        projection_key       TEXT NOT NULL,
        publication_version  INTEGER NOT NULL,
        status               TEXT NOT NULL,
        published_at         INTEGER NOT NULL,
        retracted_at         INTEGER,
        retraction_reason    TEXT,
        UNIQUE(candidate_id,publication_version)
    );
    CREATE INDEX IF NOT EXISTS idx_relation_publications_projection
        ON relation_publications(projection_key,status,published_at DESC);
    "#,
    ),
    (
        18,
        r#"
    -- Bitemporal graph: stable relation identities are separated from
    -- immutable evidence revisions. Business validity and system knowledge
    -- time can therefore be queried independently without overwriting history.
    CREATE TABLE IF NOT EXISTS graph_edge_identities (
        identity_id    TEXT PRIMARY KEY,
        src            TEXT NOT NULL REFERENCES graph_nodes(id),
        dst            TEXT NOT NULL REFERENCES graph_nodes(id),
        relation       TEXT NOT NULL,
        product_scope  TEXT NOT NULL DEFAULT '',
        region_scope   TEXT NOT NULL DEFAULT '',
        created_at     INTEGER NOT NULL,
        UNIQUE(src,dst,relation,product_scope,region_scope)
    );
    CREATE INDEX IF NOT EXISTS idx_graph_edge_identity_nodes
        ON graph_edge_identities(src,dst,relation);

    CREATE TABLE IF NOT EXISTS graph_edge_revisions (
        revision_id          TEXT PRIMARY KEY,
        identity_id          TEXT NOT NULL REFERENCES graph_edge_identities(identity_id),
        revision_no          INTEGER NOT NULL,
        weight               REAL NOT NULL CHECK(weight BETWEEN 0 AND 1),
        confidence           REAL NOT NULL CHECK(confidence BETWEEN 0 AND 1),
        disclosed_share      REAL,
        source_type          TEXT NOT NULL,
        source_name          TEXT NOT NULL,
        source_url           TEXT NOT NULL DEFAULT '',
        evidence_version     TEXT NOT NULL,
        status               TEXT NOT NULL CHECK(status IN
                              ('candidate','verified','active','stale','contradicted','expired','revoked')),
        valid_from           INTEGER NOT NULL,
        valid_to             INTEGER,
        observed_at          INTEGER NOT NULL,
        recorded_at          INTEGER NOT NULL,
        superseded_at        INTEGER,
        revalidate_after     INTEGER NOT NULL,
        decay_half_life_days INTEGER NOT NULL,
        supersedes_revision_id TEXT REFERENCES graph_edge_revisions(revision_id),
        metadata_json        TEXT NOT NULL DEFAULT '{}',
        UNIQUE(identity_id,revision_no),
        CHECK(valid_to IS NULL OR valid_to > valid_from),
        CHECK(superseded_at IS NULL OR superseded_at >= recorded_at)
    );
    CREATE INDEX IF NOT EXISTS idx_graph_revision_asof
        ON graph_edge_revisions(recorded_at,superseded_at,valid_from,valid_to,status);
    CREATE INDEX IF NOT EXISTS idx_graph_revision_identity
        ON graph_edge_revisions(identity_id,revision_no DESC);
    CREATE INDEX IF NOT EXISTS idx_graph_revision_revalidate
        ON graph_edge_revisions(revalidate_after,status);

    CREATE TABLE IF NOT EXISTS graph_revalidation_events (
        event_id            TEXT PRIMARY KEY,
        identity_id         TEXT NOT NULL REFERENCES graph_edge_identities(identity_id),
        revision_id         TEXT NOT NULL REFERENCES graph_edge_revisions(revision_id),
        trigger_type        TEXT NOT NULL,
        related_identity_id TEXT REFERENCES graph_edge_identities(identity_id),
        status              TEXT NOT NULL DEFAULT 'pending'
                            CHECK(status IN ('pending','completed','dismissed')),
        reason              TEXT NOT NULL,
        created_at          INTEGER NOT NULL,
        resolved_at         INTEGER
    );
    CREATE INDEX IF NOT EXISTS idx_graph_revalidation_pending
        ON graph_revalidation_events(status,created_at DESC);

    CREATE TABLE IF NOT EXISTS graph_entity_merges (
        merge_id       TEXT PRIMARY KEY,
        from_node_id   TEXT NOT NULL REFERENCES graph_nodes(id),
        to_node_id     TEXT NOT NULL REFERENCES graph_nodes(id),
        valid_from     INTEGER NOT NULL,
        valid_to       INTEGER,
        recorded_at   INTEGER NOT NULL,
        superseded_at INTEGER,
        reason         TEXT NOT NULL,
        CHECK(from_node_id <> to_node_id),
        CHECK(valid_to IS NULL OR valid_to > valid_from),
        CHECK(superseded_at IS NULL OR superseded_at >= recorded_at)
    );
    CREATE INDEX IF NOT EXISTS idx_graph_entity_merge_asof
        ON graph_entity_merges(from_node_id,recorded_at,superseded_at,valid_from,valid_to);

    CREATE TABLE IF NOT EXISTS graph_snapshot_records (
        snapshot_id       TEXT PRIMARY KEY,
        business_time     INTEGER NOT NULL,
        knowledge_time    INTEGER NOT NULL,
        revision_ids_json TEXT NOT NULL,
        merge_ids_json    TEXT NOT NULL DEFAULT '[]',
        created_at        INTEGER NOT NULL
    );

    -- Preserve legacy data without pretending that process startup was a
    -- business-valid date. Values within five seconds of row creation came
    -- from the old seed loader and are migrated as unknown/always (0).
    INSERT OR IGNORE INTO graph_edge_identities
        (identity_id,src,dst,relation,product_scope,region_scope,created_at)
    SELECT 'legacy:' || id,src,dst,relation,'','',created_at
      FROM graph_edges;

    INSERT OR IGNORE INTO graph_edge_revisions
        (revision_id,identity_id,revision_no,weight,confidence,disclosed_share,
         source_type,source_name,source_url,evidence_version,status,
         valid_from,valid_to,observed_at,recorded_at,superseded_at,
         revalidate_after,decay_half_life_days,supersedes_revision_id,metadata_json)
    SELECT 'legacy-rev:' || id,'legacy:' || id,1,weight,confidence,NULL,
           CASE
             WHEN source_name LIKE '%年报%' THEN 'annual_report'
             WHEN source_name LIKE '%招股%' THEN 'prospectus'
             ELSE 'legacy'
           END,
           source_name,source_url,'legacy-graph-edge:' || id,'active',
           CASE WHEN ABS(valid_from-created_at) <= 5 THEN 0 ELSE valid_from END,
           valid_to,created_at,created_at,NULL,
           created_at + CASE
             WHEN source_name LIKE '%年报%' THEN 34560000
             WHEN source_name LIKE '%招股%' THEN 34560000
             ELSE 15552000
           END,
           CASE
             WHEN source_name LIKE '%年报%' OR source_name LIKE '%招股%' THEN 730
             ELSE 365
           END,
           NULL,'{"migrated_from":"graph_edges"}'
      FROM graph_edges;
    "#,
    ),
    (
        19,
        r#"
    -- Immutable earnings-driver snapshots bind every forecast, scenario and
    -- valuation to one exact statement/assumption parameter set.
    CREATE TABLE IF NOT EXISTS earnings_driver_snapshots (
        snapshot_id           TEXT PRIMARY KEY,
        parameter_snapshot_id TEXT NOT NULL,
        symbol                TEXT NOT NULL,
        model_version         TEXT NOT NULL,
        report_period         TEXT,
        knowledge_time        INTEGER NOT NULL,
        tree_json             TEXT NOT NULL,
        created_at            INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_earnings_driver_symbol
        ON earnings_driver_snapshots(symbol,knowledge_time DESC);
    CREATE INDEX IF NOT EXISTS idx_earnings_driver_parameter_snapshot
        ON earnings_driver_snapshots(parameter_snapshot_id);

    CREATE TABLE IF NOT EXISTS earnings_driver_shock_bridges (
        bridge_id             TEXT PRIMARY KEY,
        base_snapshot_id      TEXT NOT NULL REFERENCES earnings_driver_snapshots(snapshot_id),
        evidence_version_ids_json TEXT NOT NULL DEFAULT '[]',
        shocks_json           TEXT NOT NULL,
        bridge_json           TEXT NOT NULL,
        created_at            INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_earnings_driver_bridge_base
        ON earnings_driver_shock_bridges(base_snapshot_id,created_at DESC);
    "#,
    ),
    (
        20,
        r#"
    -- Reproducible Quant Lab results. The snapshot body includes the exact
    -- data/function versions, preprocessing conventions, inference budget,
    -- multiple-testing method and all stability slices.
    CREATE TABLE IF NOT EXISTS quant_research_snapshots (
        snapshot_id       TEXT PRIMARY KEY,
        function_version  TEXT NOT NULL,
        metric            TEXT NOT NULL,
        symbols_json      TEXT NOT NULL,
        data_versions_json TEXT NOT NULL,
        config_json       TEXT NOT NULL,
        snapshot_json     TEXT NOT NULL,
        created_at        INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_quant_research_created
        ON quant_research_snapshots(created_at DESC);

    CREATE TABLE IF NOT EXISTS quant_research_jobs (
        job_id             TEXT PRIMARY KEY,
        status             TEXT NOT NULL,
        phase              TEXT NOT NULL,
        progress_json      TEXT NOT NULL,
        snapshot_id        TEXT REFERENCES quant_research_snapshots(snapshot_id),
        error              TEXT,
        created_at         INTEGER NOT NULL,
        updated_at         INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_quant_research_jobs_updated
        ON quant_research_jobs(updated_at DESC);
    "#,
    ),
];

/// Current unix time in seconds. All timestamps in this crate are stored as
/// INTEGER unix seconds (the workspace chrono build has no `clock` feature).
pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// Handle to the SQLite worker thread. Dropping it closes the channel and
/// joins the thread.
pub(crate) struct Db {
    tx: Mutex<Option<mpsc::Sender<Job>>>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Db {
    /// Open (creating if needed) the database at `path` and run migrations.
    pub(crate) fn open(path: &Path) -> Result<Db> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        backup_before_schema_upgrade(path, &conn)?;
        migrate(&mut conn)?;
        let (tx, rx) = mpsc::channel::<Job>();
        let handle = std::thread::Builder::new()
            .name("astock-storage-db".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job(&mut conn);
                }
            })
            .map_err(Error::Io)?;
        Ok(Db {
            tx: Mutex::new(Some(tx)),
            handle: Mutex::new(Some(handle)),
        })
    }

    /// Run `f` with the connection on the worker thread and await its result.
    pub(crate) async fn run<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let (rtx, rrx) = oneshot::channel();
        let job: Job = Box::new(move |conn| {
            // Receiver may be gone if the caller was cancelled; ignore.
            let _ = rtx.send(f(conn));
        });
        {
            let guard = self.tx.lock().unwrap();
            let tx = guard.as_ref().ok_or(Error::WorkerClosed)?;
            tx.send(job).map_err(|_| Error::WorkerClosed)?;
        }
        rrx.await.map_err(|_| Error::WorkerClosed)?
    }

    /// Close the channel and join the worker thread. Idempotent.
    pub(crate) fn shutdown(&self) {
        // Dropping the sender closes the channel, ending the worker loop.
        self.tx.lock().unwrap().take();
        if let Some(handle) = self.handle.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Create a verified, immutable recovery point before changing an existing
/// schema. SQLite's online backup API includes committed WAL content and does
/// not require copying a live database file byte-for-byte.
fn backup_before_schema_upgrade(path: &Path, source: &Connection) -> Result<Option<PathBuf>> {
    let current: u32 = source.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let target = MIGRATIONS.last().map(|(number, _)| *number).unwrap_or(0);
    if current == 0 || current >= target || !path.is_file() {
        return Ok(None);
    }

    let backup_directory = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("backups")
        .join("schema");
    std::fs::create_dir_all(&backup_directory)?;
    let backup_path = backup_directory.join(format!(
        "meta-v{current}-before-v{target}-{}-{}.sqlite",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id()
    ));
    let mut destination = Connection::open(&backup_path)?;
    let backup_result = (|| -> Result<()> {
        {
            let backup = Backup::new(source, &mut destination)?;
            backup.run_to_completion(128, Duration::from_millis(20), None)?;
        }
        let integrity: String =
            destination.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(Error::Invalid(format!(
                "schema-upgrade backup integrity_check returned {integrity}"
            )));
        }
        let backed_up_version: u32 =
            destination.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if backed_up_version != current {
            return Err(Error::Invalid(format!(
                "schema-upgrade backup version mismatch: expected {current}, got {backed_up_version}"
            )));
        }
        Ok(())
    })();
    drop(destination);
    if let Err(error) = backup_result {
        let _ = std::fs::remove_file(&backup_path);
        return Err(error);
    }
    Ok(Some(backup_path))
}

/// Apply every pending migration in one transaction. A failure therefore
/// leaves the pre-upgrade database version intact; the verified backup above
/// remains available for recovery from storage or filesystem failures.
fn migrate(conn: &mut Connection) -> Result<()> {
    let current: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let pending = MIGRATIONS
        .iter()
        .copied()
        .filter(|(number, _)| *number > current)
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction()?;
    for (number, sql) in pending {
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", number)?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.db");
        let db = Db::open(&path).unwrap();
        drop(db);
        // Reopening must succeed and not re-apply anything.
        let db = Db::open(&path).unwrap();
        drop(db);
        let conn = Connection::open(&path).unwrap();
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.last().unwrap().0);
        for table in [
            "securities",
            "kv",
            "watchlist",
            "conversations",
            "messages",
            "tool_cache",
            "reports",
            "predictions",
            "meta_kv",
            "agent_tasks",
            "agent_tool_audit",
            "graph_nodes",
            "graph_edges",
            "events",
            "corporate_actions",
            "source_documents",
            "document_revisions",
            "ingest_observations",
            "document_event_evidence",
            "agent_evidence_refs",
            "news_provider_state",
            "event_clusters",
            "event_cluster_members",
            "event_cluster_decisions",
            "event_fact_conflicts",
            "agent_conclusion_reviews",
            "research_entities",
            "research_entity_names",
            "research_entity_relations",
            "document_entity_links",
            "entity_link_reviews",
            "research_source_documents",
            "research_source_versions",
            "source_document_segments",
            "source_fact_evidence",
            "source_fetch_observations",
            "agent_source_evidence_refs",
            "data_quality_observations",
            "field_lineage_records",
            "data_reconciliation_results",
            "news_user_state",
            "disclosures",
            "disclosure_securities",
            "disclosure_sources",
            "disclosure_attachments",
            "disclosure_events",
            "disclosure_provider_state",
            "global_provider_state",
            "global_documents",
            "global_entities",
            "global_relations",
            "global_observations",
            "global_fx_rates",
            "structured_events",
            "event_field_evidence",
            "event_state_transitions",
            "event_market_assessments",
            "event_study_samples",
            "relation_extraction_runs",
            "relation_candidates",
            "relation_candidate_evidence",
            "relation_candidate_reviews",
            "relation_publications",
            "graph_edge_identities",
            "graph_edge_revisions",
            "graph_revalidation_events",
            "graph_entity_merges",
            "graph_snapshot_records",
            "earnings_driver_snapshots",
            "earnings_driver_shock_bridges",
            "quant_research_snapshots",
            "quant_research_jobs",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing table {table}");
        }
    }

    #[test]
    fn migration_v5_adds_kv_fetched_at_on_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.db");
        // Simulate a database that only ran migrations 1..=4.
        {
            let mut conn = Connection::open(&path).unwrap();
            for &(number, sql) in MIGRATIONS.iter().take_while(|(n, _)| *n <= 4) {
                let tx = conn.transaction().unwrap();
                tx.execute_batch(sql).unwrap();
                tx.commit().unwrap();
                conn.pragma_update(None, "user_version", number).unwrap();
            }
            // v4 kv rows have no fetched_at column.
            conn.execute(
                "INSERT INTO kv (key, value, updated_at) VALUES ('legacy', 'v', 1)",
                [],
            )
            .unwrap();
        }
        // Reopening applies every migration after v4.
        let db = Db::open(&path).unwrap();
        drop(db);
        let backups = std::fs::read_dir(dir.path().join("backups/schema"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(
            backups.len(),
            1,
            "an existing schema must be backed up once"
        );
        let backup = Connection::open(&backups[0]).unwrap();
        let backup_version: u32 = backup
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let backup_integrity: String = backup
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(backup_version, 4);
        assert_eq!(backup_integrity, "ok");
        drop(backup);
        let conn = Connection::open(&path).unwrap();
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.last().unwrap().0);
        // Pre-existing rows keep fetched_at = 0 (unknown write time).
        let fetched_at: i64 = conn
            .query_row(
                "SELECT fetched_at FROM kv WHERE key = 'legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fetched_at, 0);
    }

    #[test]
    fn migration_v18_preserves_legacy_edge_and_removes_fake_startup_validity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.db");
        {
            let mut conn = Connection::open(&path).unwrap();
            for &(number, sql) in MIGRATIONS.iter().take_while(|(n, _)| *n <= 17) {
                let tx = conn.transaction().unwrap();
                tx.execute_batch(sql).unwrap();
                tx.commit().unwrap();
                conn.pragma_update(None, "user_version", number).unwrap();
            }
            for (id, code) in [("company:a", "600001"), ("company:b", "600002")] {
                conn.execute(
                    "INSERT INTO graph_nodes
                     (id,kind,name,code,meta_json,created_at,updated_at)
                     VALUES (?1,'company',?1,?2,'{}',1000,1000)",
                    rusqlite::params![id, code],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO graph_edges
                 (src,dst,relation,weight,source_name,source_url,confidence,
                  valid_from,valid_to,created_at,updated_at)
                 VALUES ('company:a','company:b','supplies',0.3,'公司年报2024',
                         'https://example.com',0.9,1000,NULL,1000,1000)",
                [],
            )
            .unwrap();
        }

        let db = Db::open(&path).unwrap();
        drop(db);
        let conn = Connection::open(&path).unwrap();
        let (identity_count, revision_count, valid_from, source_type): (i64, i64, i64, String) =
            conn.query_row(
                "SELECT
                   (SELECT COUNT(*) FROM graph_edge_identities),
                   (SELECT COUNT(*) FROM graph_edge_revisions),
                   valid_from,source_type
                 FROM graph_edge_revisions LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(identity_count, 1);
        assert_eq!(revision_count, 1);
        assert_eq!(valid_from, 0, "startup time must not become business time");
        assert_eq!(source_type, "annual_report");
    }
}
