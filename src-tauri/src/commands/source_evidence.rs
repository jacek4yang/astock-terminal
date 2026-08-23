//! Controlled source-document retrieval and field-level evidence inspection.

use astock_source_verification::{
    EvidenceConflict, SourceDocumentDetail, SourceDocumentSummary, SourceVerifier,
};
use tauri::State;

use crate::error::CmdError;
use crate::state::AppState;

fn command_error(error: astock_source_verification::Error) -> CmdError {
    CmdError::new("source_verification", error.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn fetch_source_document(
    state: State<'_, AppState>,
    url: String,
) -> Result<SourceDocumentDetail, CmdError> {
    SourceVerifier::new(state.storage.clone())
        .fetch_source_document(url.trim())
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_source_documents(
    state: State<'_, AppState>,
    limit: usize,
) -> Result<Vec<SourceDocumentSummary>, CmdError> {
    SourceVerifier::new(state.storage.clone())
        .recent_documents(limit)
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_source_document(
    state: State<'_, AppState>,
    source_version_id: String,
) -> Result<SourceDocumentDetail, CmdError> {
    SourceVerifier::new(state.storage.clone())
        .read_document(&source_version_id)
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn compare_source_evidence(
    state: State<'_, AppState>,
    source_version_ids: Vec<String>,
) -> Result<Vec<EvidenceConflict>, CmdError> {
    SourceVerifier::new(state.storage.clone())
        .compare_source_evidence(&source_version_ids)
        .await
        .map_err(command_error)
}
