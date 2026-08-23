//! Background supply-chain relation extraction and transparent review queue.

use std::sync::Arc;

use astock_entity_linking::EntityLinker;
use astock_relation_extraction::{
    DocumentKind, ModelRelationCandidate, PublicationResult, RelationExtractionStore,
    RelationReviewRequest, ReviewPage,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::State;
use tokio_util::sync::CancellationToken;

use crate::error::CmdError;
use crate::state::{AppState, RelationExtractionSnapshot, RelationExtractionState};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RelationExtractionRequest {
    pub source_version_id: String,
    pub document_kind: DocumentKind,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub model_version: Option<String>,
    #[serde(default)]
    pub model_candidates: Vec<ModelRelationCandidate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelationExtractionStartResponse {
    pub job_id: String,
    pub started: bool,
    pub reused: bool,
    pub estimated_seconds: u32,
    pub note: String,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn relation_extraction_start(
    state: State<'_, AppState>,
    request: RelationExtractionRequest,
) -> Result<RelationExtractionStartResponse, CmdError> {
    let source_version_id = request.source_version_id.trim().to_string();
    if source_version_id.is_empty() {
        return Err(CmdError::new("invalid_source", "来源版本不能为空"));
    }
    let job_id = job_key(&request);
    if let Some(existing) = state
        .relation_extraction
        .jobs
        .lock()
        .expect("relation jobs poisoned")
        .get(&job_id)
        .cloned()
    {
        if !matches!(existing.status.as_str(), "failed" | "cancelled") {
            return Ok(RelationExtractionStartResponse {
                job_id,
                started: existing.running,
                reused: true,
                estimated_seconds: existing.estimated_remaining_seconds.unwrap_or(0),
                note: "已恢复同一来源、同一模型版本和同一候选集的后台任务。".into(),
            });
        }
    }
    let estimated_seconds = 8u32.saturating_add(request.model_candidates.len() as u32 / 10);
    let now = now_secs();
    state
        .relation_extraction
        .jobs
        .lock()
        .expect("relation jobs poisoned")
        .insert(
            job_id.clone(),
            RelationExtractionSnapshot {
                job_id: job_id.clone(),
                source_version_id: source_version_id.clone(),
                running: true,
                status: "running".into(),
                phase: "正在读取不可变原文和页码定位".into(),
                progress: 5,
                current_item: source_version_id.clone(),
                segments_scanned: 0,
                candidates_found: 0,
                validated: 0,
                needs_review: 0,
                estimated_remaining_seconds: Some(estimated_seconds),
                recent_logs: vec![
                    format!("来源版本：{source_version_id}"),
                    format!(
                        "文档类型：{:?}；模型候选 {} 条；任务没有强制截止时间",
                        request.document_kind,
                        request.model_candidates.len()
                    ),
                ],
                result: None,
                error: None,
                started_at: now,
                updated_at: now,
            },
        );
    let cancel = CancellationToken::new();
    state
        .relation_extraction
        .cancels
        .lock()
        .expect("relation cancels poisoned")
        .insert(job_id.clone(), cancel.clone());
    let jobs = Arc::clone(&state.relation_extraction);
    let storage = state.storage.clone();
    let task_id = job_id.clone();
    tauri::async_runtime::spawn(async move {
        update(
            &jobs,
            &task_id,
            15,
            "正在加载证券主数据、公司别名和子公司层级",
            "实体主数据",
            Some(estimated_seconds.saturating_sub(1)),
        );
        // Materialize/refresh the unified entity index before relation validation.
        let _ = EntityLinker::new(storage.clone())
            .resolve_query("000001")
            .await;
        if cancel.is_cancelled() {
            cancelled(&jobs, &task_id);
            return;
        }
        update(
            &jobs,
            &task_id,
            35,
            "正在逐段发现供应、客户、中标、合同、专利和产能关系",
            "规则候选与模型结构化候选",
            Some(estimated_seconds.saturating_sub(3)),
        );
        let store = RelationExtractionStore::new(storage);
        let result = store
            .extract_source(
                &source_version_id,
                request.document_kind,
                request.model_id.as_deref(),
                request.model_version.as_deref(),
                request.model_candidates,
            )
            .await;
        if cancel.is_cancelled() {
            cancelled(&jobs, &task_id);
            return;
        }
        match result {
            Ok(detail) => {
                let validated = detail
                    .candidates
                    .iter()
                    .filter(|c| c.validation_status == "validated")
                    .count();
                let needs_review = detail.candidates.len().saturating_sub(validated);
                let segments = detail
                    .candidates
                    .iter()
                    .flat_map(|c| c.evidence.iter().map(|e| e.segment_id.as_str()))
                    .collect::<std::collections::BTreeSet<_>>()
                    .len();
                let mut guard = jobs.jobs.lock().expect("relation jobs poisoned");
                if let Some(snapshot) = guard.get_mut(&task_id) {
                    snapshot.running = false;
                    snapshot.status = "completed".into();
                    snapshot.phase = "候选抽取和确定性校验已完成，等待人工审核".into();
                    snapshot.progress = 100;
                    snapshot.current_item = format!("{} 个候选", detail.candidates.len());
                    snapshot.segments_scanned = segments;
                    snapshot.candidates_found = detail.candidates.len();
                    snapshot.validated = validated;
                    snapshot.needs_review = needs_review;
                    snapshot.estimated_remaining_seconds = None;
                    snapshot.recent_logs.push(format!(
                        "完成：{} 个候选；{} 个规则校验通过；{} 个需要补充映射/证据",
                        detail.candidates.len(),
                        validated,
                        needs_review
                    ));
                    snapshot.result = Some(detail);
                    snapshot.updated_at = now_secs();
                }
            }
            Err(error) => failed(&jobs, &task_id, error.to_string()),
        }
        jobs.cancels
            .lock()
            .expect("relation cancels poisoned")
            .remove(&task_id);
    });
    Ok(RelationExtractionStartResponse {
        job_id,
        started: true,
        reused: false,
        estimated_seconds,
        note: "已进入后台；预计时间只用于展示，不设置超时。切换页面后仍可恢复进度和结果。".into(),
    })
}

#[tauri::command(rename_all = "snake_case")]
pub async fn relation_extraction_status(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<RelationExtractionSnapshot, CmdError> {
    state
        .relation_extraction
        .jobs
        .lock()
        .expect("relation jobs poisoned")
        .get(job_id.trim())
        .cloned()
        .ok_or_else(|| CmdError::new("relation_job_not_found", "关系抽取任务不存在或已清理"))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn relation_extraction_cancel(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<bool, CmdError> {
    if let Some(token) = state
        .relation_extraction
        .cancels
        .lock()
        .expect("relation cancels poisoned")
        .get(job_id.trim())
        .cloned()
    {
        token.cancel();
        Ok(true)
    } else {
        Ok(false)
    }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn query_relation_reviews(
    state: State<'_, AppState>,
    status: Option<String>,
    document_kind: Option<DocumentKind>,
    min_confidence_bps: Option<u16>,
    page: usize,
    page_size: usize,
) -> Result<ReviewPage, CmdError> {
    RelationExtractionStore::new(state.storage.clone())
        .review_page(
            status.as_deref(),
            document_kind,
            min_confidence_bps,
            page,
            page_size,
        )
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn review_relation_candidate(
    state: State<'_, AppState>,
    request: RelationReviewRequest,
) -> Result<PublicationResult, CmdError> {
    RelationExtractionStore::new(state.storage.clone())
        .review(request)
        .await
        .map_err(command_error)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn retract_relation_candidate(
    state: State<'_, AppState>,
    candidate_id: String,
    reason: String,
) -> Result<PublicationResult, CmdError> {
    RelationExtractionStore::new(state.storage.clone())
        .retract(&candidate_id, &reason)
        .await
        .map_err(command_error)
}

fn command_error(error: astock_relation_extraction::Error) -> CmdError {
    CmdError::new("relation_extraction", error.to_string())
}
fn job_key(request: &RelationExtractionRequest) -> String {
    let payload = serde_json::to_vec(request).unwrap_or_default();
    format!(
        "relation-{}",
        &format!("{:x}", Sha256::digest(payload))[..24]
    )
}
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|v| v.as_secs() as i64)
        .unwrap_or(0)
}
fn update(
    state: &RelationExtractionState,
    id: &str,
    progress: u8,
    phase: &str,
    item: &str,
    estimate: Option<u32>,
) {
    let mut guard = state.jobs.lock().expect("relation jobs poisoned");
    if let Some(s) = guard.get_mut(id) {
        s.progress = progress;
        s.phase = phase.into();
        s.current_item = item.into();
        s.estimated_remaining_seconds = estimate;
        s.updated_at = now_secs();
        s.recent_logs
            .push(format!("{progress}% · {phase} · {item}"));
    }
}
fn failed(state: &RelationExtractionState, id: &str, error: String) {
    let mut guard = state.jobs.lock().expect("relation jobs poisoned");
    if let Some(s) = guard.get_mut(id) {
        s.running = false;
        s.status = "failed".into();
        s.phase = "关系抽取失败".into();
        s.error = Some(error.clone());
        s.estimated_remaining_seconds = None;
        s.recent_logs.push(format!("错误：{error}"));
        s.updated_at = now_secs();
    }
}
fn cancelled(state: &RelationExtractionState, id: &str) {
    let mut guard = state.jobs.lock().expect("relation jobs poisoned");
    if let Some(s) = guard.get_mut(id) {
        s.running = false;
        s.status = "cancelled".into();
        s.phase = "任务已安全取消".into();
        s.estimated_remaining_seconds = None;
        s.recent_logs
            .push("用户取消；已保存的来源和候选不会删除".into());
        s.updated_at = now_secs();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn same_request_has_stable_job_key() {
        let request = RelationExtractionRequest {
            source_version_id: "srcver:1".into(),
            document_kind: DocumentKind::AnnualReport,
            model_id: None,
            model_version: None,
            model_candidates: vec![],
        };
        assert_eq!(job_key(&request), job_key(&request));
    }
}
