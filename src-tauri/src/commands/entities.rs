//! Evidence-backed entity-link inspection and manual review commands.

use astock_entity_linking::{DocumentEntityLink, EntityLinkReview, EntityLinker};
use tauri::State;

use crate::error::CmdError;
use crate::state::AppState;

fn command_error(error: astock_entity_linking::Error) -> CmdError {
    CmdError::new("entity_linking", error.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_news_entity_links(
    state: State<'_, AppState>,
    revision_ids: Vec<String>,
) -> Result<Vec<DocumentEntityLink>, CmdError> {
    let linker = EntityLinker::new(state.storage.clone());
    let mut links = Vec::new();
    for revision_id in revision_ids {
        links.extend(
            linker
                .link_revision(&revision_id)
                .await
                .map_err(command_error)?,
        );
    }
    Ok(links)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_entity_link_reviews(
    state: State<'_, AppState>,
    limit: usize,
) -> Result<Vec<EntityLinkReview>, CmdError> {
    EntityLinker::new(state.storage.clone())
        .pending_reviews(limit)
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn resolve_entity_link_review(
    state: State<'_, AppState>,
    link_id: String,
    entity_id: Option<String>,
    accept: bool,
    reason: String,
) -> Result<bool, CmdError> {
    EntityLinker::new(state.storage.clone())
        .resolve_review(&link_id, entity_id.as_deref(), accept, &reason)
        .await
        .map_err(command_error)
}
