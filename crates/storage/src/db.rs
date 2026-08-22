//! SQLite metadata store: connection worker and migrations.
//!
//! `rusqlite` is synchronous, so the single [`Connection`] lives on a
//! dedicated blocking thread. Callers submit closures through
//! [`Db::run`] (async) and get their result back over a oneshot channel.

use std::sync::mpsc;
use std::sync::Mutex;
use std::thread::JoinHandle;

use rusqlite::Connection;
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
    pub(crate) fn open(path: &std::path::Path) -> Result<Db> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
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

/// Apply pending migrations, tracking progress in the `user_version` pragma.
fn migrate(conn: &mut Connection) -> Result<()> {
    let current: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    for &(number, sql) in MIGRATIONS {
        if number <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.commit()?;
        // user_version lives in the database header; set it after commit.
        conn.pragma_update(None, "user_version", number)?;
    }
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
}
