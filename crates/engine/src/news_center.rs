use std::collections::{BTreeMap, HashMap};

use astock_entity_linking::{DocumentEntityLink, EntityLinker};
use astock_storage::{ArchivedNewsRevision, NewsUserState, Storage};
use astock_trading_rules::{
    classify_news_session, publication_precision_from_source, EffectiveNewsSession,
    NewsSessionInput, RuleSet,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NewsCenterQuery {
    pub keyword: String,
    pub category: String,
    pub source_id: String,
    pub importance: String,
    pub entity_keywords: Vec<String>,
    pub event_type: String,
    pub language: String,
    pub verification: String,
    pub user_state: String,
    pub from_utc: Option<i64>,
    pub to_utc: Option<i64>,
    pub page: usize,
    pub page_size: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct NewsCenterEventMeta {
    pub cluster_id: String,
    pub independent_sources: u64,
    pub old_republication: bool,
    pub conflict_fields: Vec<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NewsCenterItem {
    pub revision: ArchivedNewsRevision,
    pub user_state: NewsUserState,
    pub important: bool,
    pub importance_reason: String,
    pub event_type: String,
    pub verification: String,
    pub verification_name: String,
    pub event: Option<NewsCenterEventMeta>,
    pub entity_links: Vec<DocumentEntityLink>,
    pub effective_session: EffectiveNewsSession,
}

#[derive(Debug, Clone, Serialize)]
pub struct NewsCenterSourceFacet {
    pub source_id: String,
    pub source_name: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct NewsCenterPage {
    pub items: Vec<NewsCenterItem>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
    pub has_more: bool,
    pub generated_at: i64,
    pub newest_first_seen: Option<i64>,
    pub newest_observed_at: Option<i64>,
    pub archive_age_secs: Option<i64>,
    pub source_facets: Vec<NewsCenterSourceFacet>,
}

#[derive(Debug, Clone)]
struct EventMetaRow {
    cluster_id: String,
    independent_sources: u64,
    old_republication: bool,
    conflict_fields: Vec<String>,
    status: String,
}

pub async fn query(
    storage: Storage,
    rules: &RuleSet,
    query: NewsCenterQuery,
) -> Result<NewsCenterPage, String> {
    validate_query(&query)?;
    let revisions = storage
        .news_archive_recent(100_000)
        .await
        .map_err(|error| error.to_string())?;
    let states: HashMap<String, NewsUserState> = storage
        .news_user_states()
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|item| (item.document_id.clone(), item))
        .collect();
    let event_rows: HashMap<String, EventMetaRow> = storage
        .run(|connection| {
            let mut statement = connection.prepare(
                "SELECT m.revision_id,m.cluster_id,c.independent_sources,
                        m.old_republication,c.conflict_fields_json,c.status
                 FROM event_cluster_members m JOIN event_clusters c
                   ON c.cluster_id=m.cluster_id WHERE m.active=1",
            )?;
            let rows = statement.query_map([], |row| {
                let conflicts: String = row.get(4)?;
                Ok((
                    row.get::<_, String>(0)?,
                    EventMetaRow {
                        cluster_id: row.get(1)?,
                        independent_sources: row.get::<_, i64>(2)?.max(0) as u64,
                        old_republication: row.get::<_, i64>(3)? != 0,
                        conflict_fields: serde_json::from_str(&conflicts).unwrap_or_default(),
                        status: row.get(5)?,
                    },
                ))
            })?;
            Ok(rows.collect::<std::result::Result<HashMap<_, _>, _>>()?)
        })
        .await
        .map_err(|error| error.to_string())?;

    let now = now_secs();
    let newest_first_seen = revisions.iter().map(|item| item.first_seen_time_utc).max();
    let newest_observed_at = revisions.iter().map(|item| item.last_observed_at).max();
    let mut source_counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for revision in &revisions {
        *source_counts
            .entry((revision.source_id.clone(), revision.source_name.clone()))
            .or_default() += 1;
    }
    let source_facets = source_counts
        .into_iter()
        .map(|((source_id, source_name), count)| NewsCenterSourceFacet {
            source_id,
            source_name,
            count,
        })
        .collect();

    let keyword = query.keyword.trim().to_ascii_lowercase();
    let entity_keywords: Vec<String> = query
        .entity_keywords
        .iter()
        .map(|item| item.trim().to_ascii_lowercase())
        .filter(|item| !item.is_empty())
        .collect();
    let entity_text = load_entity_text(&storage, !entity_keywords.is_empty()).await?;
    let mut matched = Vec::new();
    for revision in revisions {
        let state_row = states
            .get(&revision.document_id)
            .cloned()
            .unwrap_or_else(|| NewsUserState {
                document_id: revision.document_id.clone(),
                ..Default::default()
            });
        if state_row.ignored && query.user_state != "ignored" {
            continue;
        }
        let event = event_rows.get(&revision.revision_id);
        let (important, importance_reason) = importance(&revision, event);
        let event_type = classify_event(&revision.title).to_string();
        let (verification, verification_name) = classify_verification(&revision);
        let stale_age = now.saturating_sub(revision.last_observed_at).max(0);
        let effective_session = classify_news_session(
            rules,
            &NewsSessionInput {
                event_time_utc: revision.event_time.utc,
                publish_time_utc: revision.publish_time.utc,
                first_seen_time_utc: revision.first_seen_time_utc,
                revision_time_utc: revision.revision_time.utc,
                publication_precision: publication_precision_from_source(
                    revision.publish_time.utc,
                    revision.publish_time.original.as_deref(),
                ),
                stale: stale_age > 600,
                verified: matches!(verification, "primary" | "verified_media"),
                discovery_only: verification == "discovery_only",
                old_republication: event.is_some_and(|meta| meta.old_republication)
                    || stale_age > 86_400,
            },
        )
        .map_err(|error| error.to_string())?;
        let haystack = format!(
            "{} {} {} {} {}",
            revision.title,
            revision.factual_summary,
            revision.source_name,
            revision.source_id,
            entity_text
                .get(&revision.revision_id)
                .map(String::as_str)
                .unwrap_or_default()
        )
        .to_ascii_lowercase();
        if !keyword.is_empty() && !haystack.contains(&keyword) {
            continue;
        }
        if entity_keywords.iter().any(|term| !haystack.contains(term)) {
            continue;
        }
        if !query.source_id.is_empty() && revision.source_id != query.source_id {
            continue;
        }
        if query.importance == "important" && !important {
            continue;
        }
        if !query.event_type.is_empty() && event_type != query.event_type {
            continue;
        }
        if !query.language.is_empty() && revision.language != query.language {
            continue;
        }
        if !query.verification.is_empty() && verification != query.verification {
            continue;
        }
        if !matches_category(&query.category, &revision, important, &event_type) {
            continue;
        }
        match query.user_state.as_str() {
            "unread" if state_row.is_read => continue,
            "favorite" if !state_row.favorite => continue,
            "pinned" if !state_row.pinned => continue,
            "ignored" if !state_row.ignored => continue,
            _ => {}
        }
        let timestamp = item_time(&revision);
        if query.from_utc.is_some_and(|from| timestamp < from)
            || query.to_utc.is_some_and(|to| timestamp > to)
        {
            continue;
        }
        matched.push((
            revision,
            state_row,
            important,
            importance_reason,
            event_type,
            verification.to_string(),
            verification_name.to_string(),
            event.cloned(),
            effective_session,
        ));
    }
    matched.sort_by(|left, right| {
        right
            .1
            .pinned
            .cmp(&left.1.pinned)
            .then_with(|| item_time(&right.0).cmp(&item_time(&left.0)))
            .then_with(|| right.0.revision_id.cmp(&left.0.revision_id))
    });
    build_page(
        storage,
        query,
        matched,
        source_facets,
        newest_first_seen,
        newest_observed_at,
        now,
    )
    .await
}

type MatchedRow = (
    ArchivedNewsRevision,
    NewsUserState,
    bool,
    String,
    String,
    String,
    String,
    Option<EventMetaRow>,
    EffectiveNewsSession,
);

async fn build_page(
    storage: Storage,
    query: NewsCenterQuery,
    matched: Vec<MatchedRow>,
    source_facets: Vec<NewsCenterSourceFacet>,
    newest_first_seen: Option<i64>,
    newest_observed_at: Option<i64>,
    now: i64,
) -> Result<NewsCenterPage, String> {
    let total = matched.len();
    let page_size = query.page_size.clamp(20, 500);
    let page = query.page.max(1);
    let start = (page - 1).saturating_mul(page_size).min(total);
    let end = start.saturating_add(page_size).min(total);
    let page_rows = &matched[start..end];
    let revision_ids = page_rows
        .iter()
        .map(|row| row.0.revision_id.clone())
        .collect::<Vec<_>>();
    let links = EntityLinker::new(storage)
        .links_for_revisions(&revision_ids)
        .await
        .map_err(|error| error.to_string())?;
    let mut links_by_revision: HashMap<String, Vec<DocumentEntityLink>> = HashMap::new();
    for link in links {
        links_by_revision
            .entry(link.revision_id.clone())
            .or_default()
            .push(link);
    }
    let items = page_rows
        .iter()
        .cloned()
        .map(
            |(
                revision,
                user_state,
                important,
                importance_reason,
                event_type,
                verification,
                verification_name,
                event,
                effective_session,
            )| {
                let entity_links = links_by_revision
                    .remove(&revision.revision_id)
                    .unwrap_or_default();
                NewsCenterItem {
                    revision,
                    user_state,
                    important,
                    importance_reason,
                    event_type,
                    verification,
                    verification_name,
                    event: event.map(|meta| NewsCenterEventMeta {
                        cluster_id: meta.cluster_id,
                        independent_sources: meta.independent_sources,
                        old_republication: meta.old_republication,
                        conflict_fields: meta.conflict_fields,
                        status: meta.status,
                    }),
                    entity_links,
                    effective_session,
                }
            },
        )
        .collect();
    Ok(NewsCenterPage {
        items,
        total,
        page,
        page_size,
        has_more: end < total,
        generated_at: now,
        newest_first_seen,
        newest_observed_at,
        archive_age_secs: newest_observed_at.map(|time| now.saturating_sub(time).max(0)),
        source_facets,
    })
}

async fn load_entity_text(
    storage: &Storage,
    required: bool,
) -> Result<HashMap<String, String>, String> {
    if !required {
        return Ok(HashMap::new());
    }
    storage
        .run(|connection| {
            let mut statement = connection.prepare(
                "SELECT l.revision_id,
                        GROUP_CONCAT(l.span_text || ' ' ||
                          COALESCE(e.canonical_name,'') || ' ' ||
                          COALESCE(e.listed_code,''), ' ')
                 FROM document_entity_links l
                 LEFT JOIN research_entities e ON e.entity_id=l.final_entity_id
                 WHERE l.status='accepted'
                 GROUP BY l.revision_id",
            )?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<std::result::Result<HashMap<_, _>, _>>()?)
        })
        .await
        .map_err(|error| error.to_string())
}

fn validate_query(query: &NewsCenterQuery) -> Result<(), String> {
    for (field, value) in [
        ("keyword", query.keyword.as_str()),
        ("category", query.category.as_str()),
        ("source_id", query.source_id.as_str()),
        ("importance", query.importance.as_str()),
        ("event_type", query.event_type.as_str()),
        ("language", query.language.as_str()),
        ("verification", query.verification.as_str()),
        ("user_state", query.user_state.as_str()),
    ] {
        if value.chars().count() > 500 || value.chars().any(char::is_control) {
            return Err(format!("{field} 过长或包含控制字符"));
        }
    }
    if query.entity_keywords.len() > 50
        || query
            .entity_keywords
            .iter()
            .any(|value| value.chars().count() > 200 || value.chars().any(char::is_control))
    {
        return Err("entity_keywords 最多 50 项，每项最多 200 字符且不得含控制字符".into());
    }
    if query
        .from_utc
        .zip(query.to_utc)
        .is_some_and(|(from, to)| from > to)
    {
        return Err("from_utc 不能晚于 to_utc".into());
    }
    Ok(())
}

fn item_time(revision: &ArchivedNewsRevision) -> i64 {
    revision
        .publish_time
        .utc
        .or(revision.event_time.utc)
        .unwrap_or(revision.first_seen_time_utc)
}

fn classify_event(title: &str) -> &'static str {
    for (kind, words) in [
        ("earnings", &["业绩", "财报", "营收", "净利润"][..]),
        ("policy", &["政策", "国务院", "证监会", "监管", "条例"]),
        ("announcement", &["公告", "披露", "问询", "回复"]),
        ("order", &["订单", "合同", "中标"]),
        ("capital", &["回购", "增持", "减持", "解禁", "定增"]),
        ("risk", &["处罚", "诉讼", "事故", "停产", "风险提示"]),
        (
            "global",
            &["美联储", "关税", "制裁", "海外", "美元", "原油"],
        ),
        ("market", &["涨停", "跌停", "成交", "指数", "行情"]),
    ] {
        if words.iter().any(|word| title.contains(word)) {
            return kind;
        }
    }
    "other"
}

fn classify_verification(revision: &ArchivedNewsRevision) -> (&'static str, &'static str) {
    let source = revision.source_id.to_ascii_lowercase();
    let content = revision.content_type.to_ascii_lowercase();
    if source.contains("official")
        || content.contains("announcement")
        || content.contains("disclosure")
    {
        ("primary", "一手披露已归档")
    } else if revision.license.contains("授权") || revision.license.contains("licensed") {
        ("verified_media", "媒体来源已归档")
    } else if source.contains("newsnow") || revision.license.contains("发现") {
        ("discovery_only", "聚合发现线索，原文待核验")
    } else {
        ("archived", "来源与修订已归档")
    }
}

fn importance(revision: &ArchivedNewsRevision, event: Option<&EventMetaRow>) -> (bool, String) {
    let high_signal = [
        "重大",
        "突发",
        "业绩预告",
        "停复牌",
        "处罚",
        "回购",
        "并购",
        "中标",
        "制裁",
        "降息",
        "加息",
        "风险提示",
    ]
    .iter()
    .find(|word| revision.title.contains(*word));
    if let Some(word) = high_signal {
        return (true, format!("标题命中高影响词“{word}”"));
    }
    if event.is_some_and(|meta| meta.independent_sources >= 2 || !meta.conflict_fields.is_empty()) {
        return (true, "存在多来源证据或待解决冲突".into());
    }
    if classify_verification(revision).0 == "primary" {
        return (true, "正式披露优先".into());
    }
    (false, "普通资讯".into())
}

fn matches_category(
    category: &str,
    revision: &ArchivedNewsRevision,
    important: bool,
    event_type: &str,
) -> bool {
    match category {
        "important" => important,
        "disclosure" => classify_verification(revision).0 == "primary",
        "company" => matches!(
            event_type,
            "earnings" | "announcement" | "order" | "capital" | "risk"
        ),
        "macro" => event_type == "policy",
        "global" => event_type == "global" || revision.language != "zh-CN",
        _ => true,
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_classification_is_deterministic_and_chinese_first() {
        assert_eq!(classify_event("公司发布年度业绩预告"), "earnings");
        assert_eq!(classify_event("监管部门公布资本市场新政策"), "policy");
        assert_eq!(classify_event("企业收到重大项目中标通知"), "order");
        assert_eq!(classify_event("美联储加息影响海外市场"), "global");
        assert_eq!(classify_event("无法归类的普通消息"), "other");
    }

    #[test]
    fn query_limits_reject_control_text_and_inverted_ranges() {
        let mut query = NewsCenterQuery {
            keyword: "x\n".into(),
            ..Default::default()
        };
        assert!(validate_query(&query).is_err());
        query.keyword.clear();
        query.from_utc = Some(20);
        query.to_utc = Some(10);
        assert!(validate_query(&query).is_err());
    }

    #[tokio::test]
    async fn empty_archive_returns_a_stable_first_page() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(astock_storage::StorageConfig::with_base_dir(dir.path()))
            .expect("open isolated news archive");
        let rules = RuleSet::load(None).unwrap();
        let page = query(
            storage,
            &rules,
            NewsCenterQuery {
                page: 1,
                page_size: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(page.total, 0);
        assert_eq!(page.page, 1);
        assert_eq!(page.page_size, 50);
        assert!(!page.has_more);
    }
}
