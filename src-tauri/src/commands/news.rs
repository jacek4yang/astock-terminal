//! Professional news-center queries plus explainable clustering controls.

use std::collections::{BTreeMap, HashMap};

use astock_entity_linking::{DocumentEntityLink, EntityLinker};
use astock_market_data::{FinanceNewsBatch, FINANCE_NEWS_SOURCES};
use astock_news_intelligence::{
    AgentConclusionReview, EventCluster, EventClusterDetail, NewsEventClusterer,
};
use astock_storage::{ArchivedNewsRevision, NewsUserAction, NewsUserState};
use astock_trading_rules::{
    classify_news_session, publication_precision_from_source, EffectiveNewsSession,
    NewsSessionInput,
};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::error::CmdError;
use crate::state::AppState;

fn command_error(error: astock_news_intelligence::Error) -> CmdError {
    CmdError::new("news_clustering", error.to_string())
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
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

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
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
        return (true, "存在多来源证据或待解决冲突".to_string());
    }
    if classify_verification(revision).0 == "primary" {
        return (true, "正式披露优先".to_string());
    }
    (false, "普通资讯".to_string())
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

/// Backend-filtered, paginated view over as many as 100k durable documents.
/// Only the requested page and its evidence links cross the IPC boundary.
#[tauri::command(rename_all = "snake_case")]
pub async fn query_news_center(
    state: State<'_, AppState>,
    query: NewsCenterQuery,
) -> Result<NewsCenterPage, CmdError> {
    let revisions = state.storage.news_archive_recent(100_000).await?;
    let states: HashMap<String, NewsUserState> = state
        .storage
        .news_user_states()
        .await?
        .into_iter()
        .map(|item| (item.document_id.clone(), item))
        .collect();
    let event_rows: HashMap<String, EventMetaRow> = state
        .storage
        .run(|conn| {
            let mut stmt = conn.prepare(
                "SELECT m.revision_id,m.cluster_id,c.independent_sources,
                        m.old_republication,c.conflict_fields_json,c.status
                 FROM event_cluster_members m JOIN event_clusters c
                   ON c.cluster_id=m.cluster_id WHERE m.active=1",
            )?;
            let rows = stmt.query_map([], |row| {
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
        .await?;

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
    let entity_text: HashMap<String, String> = if entity_keywords.is_empty() {
        HashMap::new()
    } else {
        state
            .storage
            .run(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT l.revision_id,
                            GROUP_CONCAT(l.span_text || ' ' ||
                              COALESCE(e.canonical_name,'') || ' ' ||
                              COALESCE(e.listed_code,''), ' ')
                     FROM document_entity_links l
                     LEFT JOIN research_entities e ON e.entity_id=l.final_entity_id
                     WHERE l.status='accepted'
                     GROUP BY l.revision_id",
                )?;
                let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
                Ok(rows.collect::<std::result::Result<HashMap<_, _>, _>>()?)
            })
            .await?
    };
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
            &state.rules,
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
        .map_err(|error| CmdError::new("news_session", error.to_string()))?;
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
    let total = matched.len();
    let page_size = query.page_size.clamp(20, 500);
    let page = query.page.max(1);
    let start = (page - 1).saturating_mul(page_size).min(total);
    let end = start.saturating_add(page_size).min(total);
    let page_rows = &matched[start..end];
    let revision_ids: Vec<String> = page_rows
        .iter()
        .map(|row| row.0.revision_id.clone())
        .collect();
    let links = EntityLinker::new(state.storage.clone())
        .links_for_revisions(&revision_ids)
        .await
        .map_err(|error| CmdError::new("news_entity_links", error.to_string()))?;
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

#[tauri::command(rename_all = "snake_case")]
pub async fn refresh_news_center(
    state: State<'_, AppState>,
    sources: Vec<String>,
    keyword: Option<String>,
    symbol: Option<String>,
    limit: usize,
) -> Result<FinanceNewsBatch, CmdError> {
    let sources = if sources.is_empty() {
        FINANCE_NEWS_SOURCES
            .iter()
            .map(|(id, _, _)| (*id).to_string())
            .collect()
    } else {
        sources
    };
    state
        .market
        .finance_news
        .research(
            &sources,
            symbol.as_deref(),
            keyword.as_deref(),
            limit.clamp(1, 200),
        )
        .await
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_news_item_state(
    state: State<'_, AppState>,
    document_id: String,
    action: String,
    value: bool,
) -> Result<NewsUserState, CmdError> {
    let action = match action.as_str() {
        "read" => NewsUserAction::Read,
        "pinned" => NewsUserAction::Pinned,
        "favorite" => NewsUserAction::Favorite,
        "ignored" => NewsUserAction::Ignored,
        _ => return Err(CmdError::new("invalid_param", "不支持的资讯状态操作")),
    };
    Ok(state
        .storage
        .news_user_state_set(&document_id, action, value)
        .await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_news_event_clusters(
    state: State<'_, AppState>,
    limit: usize,
) -> Result<Vec<EventCluster>, CmdError> {
    NewsEventClusterer::new(state.storage.clone())
        .clusters_recent(limit)
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_news_event_cluster_detail(
    state: State<'_, AppState>,
    cluster_id: String,
) -> Result<EventClusterDetail, CmdError> {
    NewsEventClusterer::new(state.storage.clone())
        .cluster_detail(&cluster_id)
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn merge_news_event_clusters(
    state: State<'_, AppState>,
    from_cluster_id: String,
    to_cluster_id: String,
    reason: String,
) -> Result<EventClusterDetail, CmdError> {
    NewsEventClusterer::new(state.storage.clone())
        .manual_merge(&from_cluster_id, &to_cluster_id, &reason)
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn split_news_event_revision(
    state: State<'_, AppState>,
    revision_id: String,
    reason: String,
) -> Result<EventClusterDetail, CmdError> {
    NewsEventClusterer::new(state.storage.clone())
        .manual_split(&revision_id, &reason)
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_pending_news_evidence_reviews(
    state: State<'_, AppState>,
    limit: usize,
) -> Result<Vec<AgentConclusionReview>, CmdError> {
    NewsEventClusterer::new(state.storage.clone())
        .pending_reviews(limit)
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn resolve_news_evidence_review(
    state: State<'_, AppState>,
    task_id: String,
    conclusion_key: String,
    triggering_revision: String,
) -> Result<bool, CmdError> {
    NewsEventClusterer::new(state.storage.clone())
        .resolve_review(&task_id, &conclusion_key, &triggering_revision)
        .await
        .map_err(command_error)
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
}
