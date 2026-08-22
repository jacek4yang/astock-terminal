//! Explainable news-event clustering and manual correction commands.

use astock_news_intelligence::{
    AgentConclusionReview, EventCluster, EventClusterDetail, NewsEventClusterer,
};
use tauri::State;

use crate::error::CmdError;
use crate::state::AppState;

fn command_error(error: astock_news_intelligence::Error) -> CmdError {
    CmdError::new("news_clustering", error.to_string())
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
