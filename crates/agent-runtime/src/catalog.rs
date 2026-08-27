//! The task's evidence catalog.
//!
//! Every tool result carries an `evidence_registry` the Engine built while
//! assembling it. Those rows are the canonical identity of every observation the
//! task has seen, and they are the only identifiers a report may cite. A live deep
//! run registered **6,578 distinct identifiers** and the report cited 37, so the
//! catalog exists to answer one question cheaply — *what is the canonical
//! identifier for the fact I want to state?* — without putting the registry in the
//! model's context.
//!
//! Three responsibilities:
//!
//! * **Attribution.** An `EvidenceFact` carries `path`, `value`, `source` and
//!   `observed_at`, but not the security it belongs to: in a multi-security bundle
//!   the symbol is a sibling field on the enclosing object
//!   (`/securities/0/symbol`). Attribution walks the result once, records what each
//!   enclosing object asserts, and resolves each fact by longest path prefix. That
//!   is what makes the contract's task-scope check meaningful, and what lets a
//!   citation read `公司公告 · 半年度报告` instead of a bare source name.
//! * **Conflict detection.** Mirrors the verifier exactly, including the part that
//!   is *not* a conflict: on the live run 476 of 476 supposed conflicts differed
//!   only in `observed_at` — the same fact retrieved twice, seconds apart. A
//!   retrieval-time difference is not a disagreement about the world, so only
//!   `value`, `source`, `path`, `quality_blocking` and `source_version_id` count.
//! * **Bounded search.** Deterministically ranked, capped result count, capped
//!   rendered values. A search never returns the whole catalog, and a search that
//!   matches nothing returns the available sources and fields rather than a bare
//!   empty list, so the next attempt is informed instead of a guess.
//!
//! The catalog holds durable task state, never private reasoning.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::report::EvidenceDescriptor;

/// Descriptors retained for one task.
///
/// Well above the 6,578 seen on the worked live example, and far below anything
/// that threatens memory. Retention is capped rather than unbounded because a
/// market-wide sweep repeated many times must not grow without limit.
pub const MAX_CATALOG_ENTRIES: usize = 40_000;

/// Rows one search may return, matching the tool schema's `limit` maximum.
pub const MAX_SEARCH_RESULTS: usize = 50;

/// Characters of an evidence value rendered into a search row.
const MAX_VALUE_CHARS: usize = 160;

/// Distinct sources or fields offered when a search matches nothing.
const MAX_HINTS: usize = 12;

/// Length bound on an attributed document title.
const MAX_TITLE_CHARS: usize = 200;

/// What an enclosing object asserts about the facts beneath it.
#[derive(Debug, Clone, Default, PartialEq)]
struct Attribution {
    symbol: Option<String>,
    document_title: Option<String>,
    published_at: Option<String>,
    unit: Option<String>,
}

impl Attribution {
    /// Merge an object's own fields over what it inherited.
    ///
    /// Deliberately conservative. `symbol` is accepted only as exactly six ASCII
    /// digits, because a wrong symbol would make the contract reject a valid claim
    /// as out of scope; that failure mode is worse than no attribution at all.
    fn extended(&self, object: &serde_json::Map<String, Value>) -> Self {
        let text = |keys: &[&str], max: usize| -> Option<String> {
            keys.iter()
                .filter_map(|key| object.get(*key))
                .filter_map(Value::as_str)
                .map(str::trim)
                .find(|value| !value.is_empty() && value.chars().count() <= max)
                .map(str::to_owned)
        };
        Self {
            symbol: object
                .get("symbol")
                .and_then(Value::as_str)
                .filter(|value| value.len() == 6 && value.bytes().all(|b| b.is_ascii_digit()))
                .map(str::to_owned)
                .or_else(|| self.symbol.clone()),
            document_title: text(&["title", "headline"], MAX_TITLE_CHARS)
                .or_else(|| self.document_title.clone()),
            published_at: text(&["published_at", "publish_time", "pub_time"], 40)
                .or_else(|| self.published_at.clone()),
            unit: text(&["unit", "currency"], 20).or_else(|| self.unit.clone()),
        }
    }

    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// Escape a JSON-pointer segment the way the Engine does, so the paths built here
/// line up byte-for-byte with the `path` field of an `EvidenceFact`.
fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn collect_attribution(
    value: &Value,
    path: &str,
    inherited: &Attribution,
    out: &mut BTreeMap<String, Attribution>,
) {
    match value {
        Value::Object(object) => {
            let attribution = inherited.extended(object);
            // Record only objects that add something, keeping the index small.
            if attribution != *inherited && !attribution.is_empty() {
                out.insert(path.to_owned(), attribution.clone());
            }
            for (key, child) in object {
                if key == "evidence_registry" {
                    continue;
                }
                collect_attribution(
                    child,
                    &format!("{path}/{}", escape_pointer(key)),
                    &attribution,
                    out,
                );
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_attribution(child, &format!("{path}/{index}"), inherited, out);
            }
        }
        _ => {}
    }
}

/// Resolve a fact's attribution by longest path prefix.
///
/// The empty key is the root, which carries whatever the call itself established —
/// a single-security tool names its symbol in the arguments, not in the result.
fn attribution_for<'a>(
    path: &str,
    index: &'a BTreeMap<String, Attribution>,
) -> Option<&'a Attribution> {
    let mut candidate = path;
    loop {
        if let Some(found) = index.get(candidate) {
            return Some(found);
        }
        match candidate.rfind('/') {
            Some(cut) => candidate = &candidate[..cut],
            None => return index.get(""),
        }
    }
}

/// One evidence row as the Engine registered it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RawFact {
    evidence_id: String,
    path: String,
    value: Value,
    source: String,
    #[serde(default)]
    observed_at: Option<String>,
    #[serde(default)]
    source_version_id: Option<String>,
    #[serde(default)]
    quality_blocking: bool,
}

/// Do two registrations of the same identifier disagree about the world?
///
/// Mirrors the Engine verifier's rule, including its most important omission:
/// `observed_at` is absent, because the same fact retrieved twice thirty seconds
/// apart is not a conflict. Treating it as one produced 476 spurious conflicts
/// live and blocked every report.
fn materially_disagrees(left: &RawFact, right: &RawFact) -> bool {
    left.value != right.value
        || left.source != right.source
        || left.path != right.path
        || left.quality_blocking != right.quality_blocking
        || left.source_version_id != right.source_version_id
}

/// A bounded, deterministic query over the catalog.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceQuery {
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub keyword: Option<String>,
    #[serde(default)]
    pub only_calculations: Option<bool>,
    pub limit: usize,
}

/// Evidence identifiers accumulated for one task.
#[derive(Debug, Clone, Default)]
pub struct EvidenceCatalog {
    descriptors: BTreeMap<String, EvidenceDescriptor>,
    /// Kept alongside the descriptor so a later registration can be compared
    /// against the first without re-deriving it from presentation fields, and so
    /// the exact registered form can be handed to the independent verifier.
    raw: BTreeMap<String, RawFact>,
    /// For an identifier registered twice with a material disagreement, the
    /// divergent registration.
    ///
    /// Retained so the verifier can reach its **own** conflict conclusion from the
    /// same two registrations, rather than being told about it. Bounded to one
    /// variant per identifier: a second disagreement adds no information the first
    /// does not already carry.
    conflicting_variants: BTreeMap<String, RawFact>,
    /// Facts dropped because the retention bound was reached.
    dropped: usize,
}

impl EvidenceCatalog {
    /// Absorb one tool result.
    ///
    /// `fallback_symbol` covers single-security tools whose result carries no
    /// `symbol` field of its own; the call arguments named it, so attributing it is
    /// accurate rather than a guess.
    pub fn ingest(&mut self, result: &Value, fallback_symbol: Option<&str>) {
        let mut index = BTreeMap::new();
        let root = Attribution {
            symbol: fallback_symbol.map(str::to_owned),
            ..Attribution::default()
        };
        if !root.is_empty() {
            index.insert(String::new(), root.clone());
        }
        collect_attribution(result, "", &root, &mut index);

        let mut facts = Vec::new();
        collect_facts(result, &mut facts);
        for fact in facts {
            if !fact.evidence_id.starts_with("evf_") || fact.evidence_id.len() > 80 {
                // A malformed identifier is not admitted, so it cannot be cited.
                // The verifier treats one as a conflict; here it simply does not
                // exist, and `validate_draft` reports `unknown_evidence`.
                continue;
            }
            match self.raw.get(&fact.evidence_id) {
                Some(existing) => {
                    if materially_disagrees(existing, &fact) {
                        if let Some(descriptor) = self.descriptors.get_mut(&fact.evidence_id) {
                            descriptor.conflicting = true;
                        }
                        self.conflicting_variants
                            .entry(fact.evidence_id.clone())
                            .or_insert(fact);
                    } else if fact.observed_at > existing.observed_at {
                        // Same assertion, seen more recently. Keep the freshest
                        // timestamp so a current/latest claim is judged against it.
                        if let Some(descriptor) = self.descriptors.get_mut(&fact.evidence_id) {
                            descriptor.observed_at = fact.observed_at.clone();
                        }
                        self.raw.insert(fact.evidence_id.clone(), fact);
                    }
                }
                None => {
                    if self.descriptors.len() >= MAX_CATALOG_ENTRIES {
                        self.dropped = self.dropped.saturating_add(1);
                        continue;
                    }
                    let attribution = attribution_for(&fact.path, &index);
                    let descriptor = EvidenceDescriptor {
                        evidence_id: fact.evidence_id.clone(),
                        source: fact.source.clone(),
                        symbol: attribution.and_then(|a| a.symbol.clone()),
                        field: Some(fact.path.clone()),
                        value: Some(fact.value.clone()),
                        unit: attribution.and_then(|a| a.unit.clone()),
                        observed_at: fact.observed_at.clone(),
                        published_at: attribution.and_then(|a| a.published_at.clone()),
                        document_title: attribution.and_then(|a| a.document_title.clone()),
                        document_location: None,
                        quality_blocking: fact.quality_blocking,
                        conflicting: false,
                    };
                    self.descriptors
                        .insert(fact.evidence_id.clone(), descriptor);
                    self.raw.insert(fact.evidence_id.clone(), fact);
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        self.descriptors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.descriptors.is_empty()
    }

    /// Facts refused because retention was full. Surfaced rather than hidden.
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// The map `validate_draft` and `render` consume.
    pub fn descriptors(&self) -> &BTreeMap<String, EvidenceDescriptor> {
        &self.descriptors
    }

    /// The exact registered facts backing a set of citations.
    ///
    /// This is what the independent verifier receives, and it replaces handing it
    /// every tool result the task produced. That mattered for two reasons.
    ///
    /// **It was unbounded.** The verifier context grew with every successful tool,
    /// carrying full payload bodies it never reads — a market snapshot is megabytes
    /// of rows around a few kilobytes of registry — against a 6 MiB ceiling that
    /// fails the whole verification rather than producing a finding.
    ///
    /// **It is exactly equivalent.** Every check the verifier performs is either
    /// per-citation (existence, quality, observation time, source version,
    /// numeric reproduction, conflict) or derived from the report itself (distinct
    /// citation count, presence of checkable quantities). A fact the report does
    /// not cite cannot change any of them. So projecting to the cited set preserves
    /// every gate while making the transfer proportional to the report rather than
    /// to the research.
    ///
    /// Facts are emitted in their registered form, including `source_version_id`,
    /// and a conflicting identifier is emitted twice — the retained registration
    /// and the divergent one — so the verifier reaches its own conflict conclusion
    /// from the same evidence rather than trusting this one.
    pub fn verifier_facts(&self, cited: &BTreeSet<String>) -> Vec<Value> {
        let mut facts = Vec::new();
        for id in cited {
            if let Some(fact) = self.raw.get(id) {
                if let Ok(value) = serde_json::to_value(fact) {
                    facts.push(value);
                }
            }
            if let Some(variant) = self.conflicting_variants.get(id) {
                if let Ok(value) = serde_json::to_value(variant) {
                    facts.push(value);
                }
            }
        }
        facts
    }

    /// Run one bounded search.
    pub fn search(&self, query: &EvidenceQuery) -> Value {
        let limit = query.limit.clamp(1, MAX_SEARCH_RESULTS);
        let field_query = query.field.as_deref().map(str::to_lowercase);
        let source_query = query.source.as_deref().map(str::to_lowercase);
        let keyword = query.keyword.as_deref().map(str::to_lowercase);

        let mut matched: Vec<&EvidenceDescriptor> = self
            .descriptors
            .values()
            .filter(|descriptor| {
                if let Some(symbol) = query.symbol.as_deref() {
                    if descriptor.symbol.as_deref() != Some(symbol) {
                        return false;
                    }
                }
                if let Some(source) = source_query.as_deref() {
                    if !descriptor.source.to_lowercase().contains(source) {
                        return false;
                    }
                }
                if let Some(field) = field_query.as_deref() {
                    if !descriptor
                        .field
                        .as_deref()
                        .is_some_and(|path| path.to_lowercase().contains(field))
                    {
                        return false;
                    }
                }
                if query.only_calculations == Some(true) && !descriptor.is_calculation() {
                    return false;
                }
                if let Some(keyword) = keyword.as_deref() {
                    if !matches_keyword(descriptor, keyword) {
                        return false;
                    }
                }
                true
            })
            .collect();

        // Deterministic total order. Every component is a stable property of the
        // descriptor and the last key is the identifier, so equal candidates never
        // reorder between two identical searches.
        matched.sort_by(|left, right| {
            rank_key(left, field_query.as_deref())
                .cmp(&rank_key(right, field_query.as_deref()))
                .then_with(|| right.observed_at.cmp(&left.observed_at))
                .then_with(|| left.evidence_id.cmp(&right.evidence_id))
        });

        let total = matched.len();
        let rows: Vec<Value> = matched.iter().take(limit).map(|d| search_row(d)).collect();
        let mut payload = serde_json::Map::new();
        payload.insert("ok".into(), Value::Bool(true));
        payload.insert("catalog_size".into(), Value::from(self.descriptors.len()));
        payload.insert("matched".into(), Value::from(total));
        payload.insert("returned".into(), Value::from(rows.len()));
        payload.insert("truncated".into(), Value::Bool(total > rows.len()));
        payload.insert("results".into(), Value::Array(rows));
        if total > limit {
            payload.insert(
                "note".into(),
                Value::from(
                    "More evidence matched than was returned. Narrow the query by \
                     symbol, source or field rather than raising the limit.",
                ),
            );
        }
        if total == 0 {
            // An empty result with no orientation is what drives a model to invent
            // an identifier. Offer what the catalog actually holds instead.
            payload.insert(
                "available_sources".into(),
                Value::Array(self.hint_sources()),
            );
            payload.insert("available_fields".into(), Value::Array(self.hint_fields()));
            payload.insert(
                "note".into(),
                Value::from(
                    "No evidence matched. Use one of the listed sources or field \
                     fragments, or gather the data first. Never invent an identifier.",
                ),
            );
        }
        Value::Object(payload)
    }

    /// Sources present in the catalog, most populated first.
    fn hint_sources(&self) -> Vec<Value> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for descriptor in self.descriptors.values() {
            *counts.entry(descriptor.source.as_str()).or_default() += 1;
        }
        rank_hints(counts)
    }

    /// Leaf field names present in the catalog, most populated first.
    fn hint_fields(&self) -> Vec<Value> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for descriptor in self.descriptors.values() {
            if let Some(path) = descriptor.field.as_deref() {
                let leaf = path.rsplit('/').next().unwrap_or(path);
                if !leaf.is_empty() && !leaf.bytes().all(|b| b.is_ascii_digit()) {
                    *counts.entry(leaf).or_default() += 1;
                }
            }
        }
        rank_hints(counts)
    }
}

fn rank_hints(counts: BTreeMap<&str, usize>) -> Vec<Value> {
    let mut ranked: Vec<(&str, usize)> = counts.into_iter().collect();
    // Count descending, then name ascending, so the hint list is deterministic.
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    ranked
        .into_iter()
        .take(MAX_HINTS)
        .map(|(name, count)| serde_json::json!({"name": name, "facts": count}))
        .collect()
}

/// Ordering rank: an exact leaf-field match first, then trustworthy evidence.
///
/// Conflicting and quality-blocking evidence is ranked last rather than hidden.
/// A conflict is a research finding the model may need to disclose, so removing it
/// from view would hide exactly what the contract requires be stated.
///
/// Undated observations rank after dated ones. The verifier refuses an observation
/// with no time as support for a dated or current claim, so surfacing a dated
/// identifier first avoids a repair round that a ranking decision can prevent. A
/// calculation is exempt: it has no observation time by nature, and the verifier
/// already treats it that way.
fn rank_key(descriptor: &EvidenceDescriptor, field_query: Option<&str>) -> (u8, u8, u8, u8) {
    let exact = match (field_query, descriptor.field.as_deref()) {
        (Some(query), Some(path)) => {
            let leaf = path.rsplit('/').next().unwrap_or(path).to_lowercase();
            u8::from(leaf != query)
        }
        _ => 1,
    };
    (
        exact,
        u8::from(descriptor.conflicting),
        u8::from(descriptor.quality_blocking),
        u8::from(lacks_observation_time(descriptor)),
    )
}

/// Would the verifier refuse this evidence for want of an observation time?
fn lacks_observation_time(descriptor: &EvidenceDescriptor) -> bool {
    !descriptor.is_calculation()
        && descriptor
            .observed_at
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
}

fn matches_keyword(descriptor: &EvidenceDescriptor, keyword: &str) -> bool {
    let haystacks = [
        Some(descriptor.source.to_lowercase()),
        descriptor.field.as_ref().map(|f| f.to_lowercase()),
        descriptor.document_title.as_ref().map(|t| t.to_lowercase()),
        descriptor.symbol.clone(),
        descriptor.value.as_ref().map(render_value),
    ];
    haystacks
        .iter()
        .flatten()
        .any(|text| text.to_lowercase().contains(keyword))
}

fn render_value(value: &Value) -> String {
    let rendered = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    if rendered.chars().count() <= MAX_VALUE_CHARS {
        return rendered;
    }
    let truncated: String = rendered.chars().take(MAX_VALUE_CHARS).collect();
    format!("{truncated}…")
}

fn search_row(descriptor: &EvidenceDescriptor) -> Value {
    let mut row = serde_json::Map::new();
    row.insert(
        "evidence_id".into(),
        Value::from(descriptor.evidence_id.clone()),
    );
    row.insert("source".into(), Value::from(descriptor.source.clone()));
    if let Some(field) = &descriptor.field {
        row.insert("field".into(), Value::from(field.clone()));
    }
    if let Some(value) = &descriptor.value {
        row.insert("value".into(), Value::from(render_value(value)));
    }
    if let Some(symbol) = &descriptor.symbol {
        row.insert("symbol".into(), Value::from(symbol.clone()));
    }
    if let Some(unit) = &descriptor.unit {
        row.insert("unit".into(), Value::from(unit.clone()));
    }
    if let Some(observed_at) = &descriptor.observed_at {
        row.insert("observed_at".into(), Value::from(observed_at.clone()));
    }
    if let Some(published_at) = &descriptor.published_at {
        row.insert("published_at".into(), Value::from(published_at.clone()));
    }
    if let Some(title) = &descriptor.document_title {
        row.insert("document_title".into(), Value::from(title.clone()));
    }
    row.insert(
        "is_calculation".into(),
        Value::Bool(descriptor.is_calculation()),
    );
    if descriptor.conflicting {
        row.insert("conflicting".into(), Value::Bool(true));
    }
    if descriptor.quality_blocking {
        row.insert("quality_blocking".into(), Value::Bool(true));
    }
    // Named so the model can avoid an identifier the verifier will refuse for a
    // dated claim, rather than discovering it a repair round later.
    if lacks_observation_time(descriptor) {
        row.insert("no_observation_time".into(), Value::Bool(true));
    }
    Value::Object(row)
}

fn collect_facts(value: &Value, out: &mut Vec<RawFact>) {
    match value {
        Value::Object(object) => {
            if let Some(rows) = object
                .get("evidence_registry")
                .and_then(|registry| registry.get("facts"))
                .and_then(Value::as_array)
            {
                for row in rows {
                    if let Ok(fact) = serde_json::from_value::<RawFact>(row.clone()) {
                        out.push(fact);
                    }
                }
            }
            for (key, child) in object {
                if key != "evidence_registry" {
                    collect_facts(child, out);
                }
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_facts(child, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fact(id: &str, path: &str, value: Value, source: &str, observed_at: Option<&str>) -> Value {
        json!({
            "evidence_id": id,
            "path": path,
            "value": value,
            "source": source,
            "observed_at": observed_at,
            "source_version_id": null,
            "quality_blocking": false,
        })
    }

    /// A multi-security bundle attributes each fact to the right symbol.
    ///
    /// The symbol is a sibling field, not part of the path, so without attribution
    /// the contract's scope check has nothing to check.
    #[test]
    fn a_facts_symbol_is_resolved_from_the_enclosing_bundle() {
        let result = json!({
            "securities": [
                {"symbol": "601899", "market": {"quote": {"last": 34.47}}},
                {"symbol": "600036", "market": {"quote": {"last": 41.2}}},
            ],
            "evidence_registry": {"facts": [
                fact("evf_a", "/securities/0/market/quote/last", json!(34.47), "tencent", Some("2026-08-26T07:00:00Z")),
                fact("evf_b", "/securities/1/market/quote/last", json!(41.2), "tencent", Some("2026-08-26T07:00:00Z")),
            ]},
        });
        let mut catalog = EvidenceCatalog::default();
        catalog.ingest(&result, None);
        assert_eq!(
            catalog.descriptors()["evf_a"].symbol.as_deref(),
            Some("601899")
        );
        assert_eq!(
            catalog.descriptors()["evf_b"].symbol.as_deref(),
            Some("600036")
        );
    }

    /// A single-security tool result inherits the symbol from the call.
    #[test]
    fn a_single_security_result_inherits_the_requested_symbol() {
        let result = json!({
            "quote": {"last": 34.47},
            "evidence_registry": {"facts": [
                fact("evf_a", "/quote/last", json!(34.47), "tencent", None),
            ]},
        });
        let mut catalog = EvidenceCatalog::default();
        catalog.ingest(&result, Some("601899"));
        assert_eq!(
            catalog.descriptors()["evf_a"].symbol.as_deref(),
            Some("601899")
        );
    }

    /// The same fact retrieved twice is not a conflict.
    ///
    /// This is the exact live failure: 476 of 476 supposed conflicts differed only
    /// in retrieval time, and they blocked every report.
    #[test]
    fn a_retrieval_time_difference_is_not_a_conflict() {
        let first = json!({"evidence_registry": {"facts": [
            fact("evf_a", "/adjustment", json!("qfq"), "joinquant", Some("2026-08-26T07:00:00Z")),
        ]}});
        let second = json!({"evidence_registry": {"facts": [
            fact("evf_a", "/adjustment", json!("qfq"), "joinquant", Some("2026-08-26T07:00:30Z")),
        ]}});
        let mut catalog = EvidenceCatalog::default();
        catalog.ingest(&first, None);
        catalog.ingest(&second, None);
        let descriptor = &catalog.descriptors()["evf_a"];
        assert!(!descriptor.conflicting, "retrieval drift is not a conflict");
        // The freshest observation wins, so a latest-state claim is judged against it.
        assert_eq!(
            descriptor.observed_at.as_deref(),
            Some("2026-08-26T07:00:30Z")
        );
    }

    /// A genuine disagreement is recorded as a conflict.
    #[test]
    fn a_different_value_for_the_same_identifier_is_a_conflict() {
        let first = json!({"evidence_registry": {"facts": [
            fact("evf_a", "/quote/last", json!(34.47), "tencent", Some("2026-08-26T07:00:00Z")),
        ]}});
        let second = json!({"evidence_registry": {"facts": [
            fact("evf_a", "/quote/last", json!(35.10), "tencent", Some("2026-08-26T07:00:00Z")),
        ]}});
        let mut catalog = EvidenceCatalog::default();
        catalog.ingest(&first, None);
        catalog.ingest(&second, None);
        assert!(catalog.descriptors()["evf_a"].conflicting);
    }

    /// A malformed identifier is never admitted, so it can never be cited.
    #[test]
    fn a_fabricated_identifier_namespace_is_refused() {
        let result = json!({"evidence_registry": {"facts": [
            fact("计算-BPS", "/x", json!(1), "engine", None),
            fact("evf_ok", "/y", json!(2), "engine", None),
        ]}});
        let mut catalog = EvidenceCatalog::default();
        catalog.ingest(&result, None);
        assert_eq!(catalog.len(), 1);
        assert!(catalog.descriptors().contains_key("evf_ok"));
    }

    fn populated() -> EvidenceCatalog {
        let mut facts = Vec::new();
        for index in 0..200 {
            facts.push(fact(
                &format!("evf_bar{index:03}"),
                &format!("/securities/0/market/kline/bars/{index}/close"),
                json!(30.0 + index as f64),
                "tencent",
                Some("2026-08-26T07:00:00Z"),
            ));
        }
        facts.push(fact(
            "evf_price",
            "/securities/0/market/quote/last",
            json!(34.47),
            "tencent",
            Some("2026-08-26T07:05:00Z"),
        ));
        facts.push(fact(
            "evf_calc",
            "/result/value",
            json!(28.4),
            "astock-compute",
            None,
        ));
        let result = json!({
            "securities": [{"symbol": "601899"}],
            "evidence_registry": {"facts": facts},
        });
        let mut catalog = EvidenceCatalog::default();
        catalog.ingest(&result, None);
        catalog
    }

    /// A search never returns the whole catalog.
    #[test]
    fn search_results_are_bounded_and_report_truncation() {
        let catalog = populated();
        let response = catalog.search(&EvidenceQuery {
            limit: 5,
            ..EvidenceQuery::default()
        });
        assert_eq!(response["returned"], json!(5));
        assert_eq!(response["truncated"], json!(true));
        assert_eq!(response["matched"], json!(202));
        assert!(response["note"]
            .as_str()
            .is_some_and(|n| n.contains("Narrow")));
    }

    /// A limit above the cap is clamped, not honoured.
    #[test]
    fn an_oversized_limit_is_clamped() {
        let catalog = populated();
        let response = catalog.search(&EvidenceQuery {
            limit: 10_000,
            ..EvidenceQuery::default()
        });
        assert_eq!(response["returned"], json!(MAX_SEARCH_RESULTS));
    }

    /// Ranking puts an exact leaf match first, deterministically.
    #[test]
    fn an_exact_field_match_ranks_first_and_ranking_is_deterministic() {
        let catalog = populated();
        let query = EvidenceQuery {
            field: Some("last".into()),
            limit: 3,
            ..EvidenceQuery::default()
        };
        let first = catalog.search(&query);
        let second = catalog.search(&query);
        assert_eq!(first, second, "ranking must be deterministic");
        assert_eq!(first["results"][0]["evidence_id"], json!("evf_price"));
    }

    /// Calculation evidence is reachable on its own, for a calculated number.
    #[test]
    fn calculations_can_be_searched_separately() {
        let catalog = populated();
        let response = catalog.search(&EvidenceQuery {
            only_calculations: Some(true),
            limit: 10,
            ..EvidenceQuery::default()
        });
        assert_eq!(response["matched"], json!(1));
        assert_eq!(response["results"][0]["evidence_id"], json!("evf_calc"));
        assert_eq!(response["results"][0]["is_calculation"], json!(true));
    }

    /// An empty result orients the next attempt instead of inviting invention.
    #[test]
    fn an_empty_result_reports_what_the_catalog_actually_holds() {
        let catalog = populated();
        let response = catalog.search(&EvidenceQuery {
            field: Some("nonexistent_field".into()),
            limit: 10,
            ..EvidenceQuery::default()
        });
        assert_eq!(response["matched"], json!(0));
        let sources = response["available_sources"]
            .as_array()
            .expect("sources listed");
        assert!(sources.iter().any(|s| s["name"] == json!("tencent")));
        assert!(response["available_fields"]
            .as_array()
            .is_some_and(|f| !f.is_empty()));
        assert!(response["note"]
            .as_str()
            .is_some_and(|n| n.contains("Never invent")));
    }

    /// A search row must not carry an unbounded value.
    #[test]
    fn a_long_value_is_truncated_in_a_search_row() {
        let long = "第".repeat(5_000);
        let result = json!({"evidence_registry": {"facts": [
            fact("evf_long", "/news/0/summary", json!(long), "cls-telegraph", None),
        ]}});
        let mut catalog = EvidenceCatalog::default();
        catalog.ingest(&result, None);
        let response = catalog.search(&EvidenceQuery {
            limit: 1,
            ..EvidenceQuery::default()
        });
        let rendered = response["results"][0]["value"]
            .as_str()
            .expect("a value row");
        assert!(rendered.chars().count() <= MAX_VALUE_CHARS + 1);
    }

    /// Retention is bounded and the shortfall is reported, never silent.
    #[test]
    fn retention_is_bounded_and_the_shortfall_is_visible() {
        let mut catalog = EvidenceCatalog {
            descriptors: BTreeMap::new(),
            raw: BTreeMap::new(),
            conflicting_variants: BTreeMap::new(),
            dropped: 0,
        };
        // Fill to the cap with synthetic descriptors, then try to add one more.
        for index in 0..MAX_CATALOG_ENTRIES {
            let id = format!("evf_fill{index}");
            catalog.raw.insert(
                id.clone(),
                RawFact {
                    evidence_id: id.clone(),
                    path: "/x".into(),
                    value: json!(1),
                    source: "engine".into(),
                    observed_at: None,
                    source_version_id: None,
                    quality_blocking: false,
                },
            );
            catalog.descriptors.insert(
                id.clone(),
                EvidenceDescriptor {
                    evidence_id: id,
                    source: "engine".into(),
                    symbol: None,
                    field: Some("/x".into()),
                    value: Some(json!(1)),
                    unit: None,
                    observed_at: None,
                    published_at: None,
                    document_title: None,
                    document_location: None,
                    quality_blocking: false,
                    conflicting: false,
                },
            );
        }
        catalog.ingest(
            &json!({"evidence_registry": {"facts": [
                fact("evf_overflow", "/y", json!(2), "engine", None),
            ]}}),
            None,
        );
        assert_eq!(catalog.len(), MAX_CATALOG_ENTRIES);
        assert_eq!(catalog.dropped(), 1);
    }

    /// The verifier receives only the facts the report cites, in registered form.
    ///
    /// Handing it every tool result was unbounded — full payload bodies it never
    /// reads, against a 6 MiB ceiling that fails the whole verification rather than
    /// producing a finding.
    #[test]
    fn the_verifier_projection_carries_only_cited_facts_in_registered_form() {
        let catalog = populated();
        let mut cited = BTreeSet::new();
        cited.insert("evf_price".to_owned());
        cited.insert("evf_calc".to_owned());
        let facts = catalog.verifier_facts(&cited);
        assert_eq!(
            facts.len(),
            2,
            "202 facts exist; only the cited two are sent"
        );
        let price = facts
            .iter()
            .find(|fact| fact["evidence_id"] == json!("evf_price"))
            .expect("the cited price fact is present");
        // Registered form, not presentation form: the verifier rejects a fact whose
        // source version is missing, so the projection must preserve it.
        assert_eq!(price["source"], json!("tencent"));
        assert_eq!(price["path"], json!("/securities/0/market/quote/last"));
        assert_eq!(price["value"], json!(34.47));
        assert!(price.get("source_version_id").is_some());
        assert!(price.get("quality_blocking").is_some());
    }

    /// An uncited identifier is not sent, so it cannot be reported against.
    #[test]
    fn an_uncited_identifier_is_absent_from_the_projection() {
        let catalog = populated();
        let facts = catalog.verifier_facts(&BTreeSet::new());
        assert!(facts.is_empty());
    }

    /// A conflicting identifier is sent twice, so the verifier reaches its own
    /// conclusion from the same two registrations rather than being told.
    #[test]
    fn a_conflicting_identifier_is_projected_with_its_divergent_registration() {
        let first = json!({"evidence_registry": {"facts": [
            fact("evf_a", "/quote/last", json!(34.47), "tencent", Some("2026-08-26T07:00:00Z")),
        ]}});
        let second = json!({"evidence_registry": {"facts": [
            fact("evf_a", "/quote/last", json!(35.10), "tencent", Some("2026-08-26T07:00:00Z")),
        ]}});
        let mut catalog = EvidenceCatalog::default();
        catalog.ingest(&first, None);
        catalog.ingest(&second, None);
        let mut cited = BTreeSet::new();
        cited.insert("evf_a".to_owned());
        let facts = catalog.verifier_facts(&cited);
        assert_eq!(facts.len(), 2, "both registrations must reach the verifier");
        let values: Vec<&Value> = facts.iter().map(|fact| &fact["value"]).collect();
        assert!(values.contains(&&json!(34.47)));
        assert!(values.contains(&&json!(35.10)));
    }

    /// A fact registered without an observation time is ranked after one that has
    /// it, because the verifier refuses an undated observation as support for a
    /// dated claim. Ranking the trap last is cheaper than repairing the report.
    #[test]
    fn evidence_without_an_observation_time_ranks_after_evidence_that_has_one() {
        let result = json!({"evidence_registry": {"facts": [
            fact("evf_undated", "/quote/last", json!(34.47), "tencent", None),
            fact("evf_dated", "/snapshot/last", json!(34.47), "tencent", Some("2026-08-26T07:00:00Z")),
        ]}});
        let mut catalog = EvidenceCatalog::default();
        catalog.ingest(&result, None);
        let response = catalog.search(&EvidenceQuery {
            field: Some("last".into()),
            limit: 5,
            ..EvidenceQuery::default()
        });
        assert_eq!(response["results"][0]["evidence_id"], json!("evf_dated"));
        assert_eq!(
            response["results"][1]["no_observation_time"],
            json!(true),
            "the model must be able to see why an identifier is risky"
        );
    }

    /// A document title reaches the descriptor, so a citation can name the source.
    #[test]
    fn a_document_title_is_attributed_to_the_facts_beneath_it() {
        let result = json!({
            "disclosures": [{
                "title": "紫金矿业2026年半年度报告",
                "published_at": "2026-08-20T09:30:00Z",
                "metrics": {"revenue": 1_234.5},
            }],
            "evidence_registry": {"facts": [
                fact("evf_rev", "/disclosures/0/metrics/revenue", json!(1234.5), "disclosure", None),
            ]},
        });
        let mut catalog = EvidenceCatalog::default();
        catalog.ingest(&result, None);
        let descriptor = &catalog.descriptors()["evf_rev"];
        assert_eq!(
            descriptor.document_title.as_deref(),
            Some("紫金矿业2026年半年度报告")
        );
        assert_eq!(
            descriptor.published_at.as_deref(),
            Some("2026-08-20T09:30:00Z")
        );
        // The rendered citation then names a document rather than a bare source.
        assert!(descriptor.display_label().contains("半年度报告"));
    }
}
