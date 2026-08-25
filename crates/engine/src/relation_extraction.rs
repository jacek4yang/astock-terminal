//! Background relation extraction and audited human review orchestration.
//!
//! The extraction and publication rules live in `astock-relation-extraction`;
//! this module only owns cancellable job supervision and the stable Engine
//! response shapes used by Proton and the Agent worker.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use astock_entity_linking::EntityLinker;
use astock_relation_extraction::{
    DocumentKind, ExtractionRunDetail, ModelRelationCandidate, RelationExtractionStore,
};
use astock_storage::Storage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

const MAX_MODEL_CANDIDATES: usize = 2_000;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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
pub struct RelationExtractionStart {
    pub job_id: String,
    pub started: bool,
    pub reused: bool,
    pub estimated_seconds: u32,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelationExtractionSnapshot {
    pub job_id: String,
    pub source_version_id: String,
    pub running: bool,
    pub status: String,
    pub phase: String,
    pub progress: u8,
    pub current_item: String,
    pub segments_scanned: usize,
    pub candidates_found: usize,
    pub validated: usize,
    pub needs_review: usize,
    pub estimated_remaining_seconds: Option<u32>,
    pub recent_logs: Vec<String>,
    pub result: Option<ExtractionRunDetail>,
    pub error: Option<String>,
    pub started_at: i64,
    pub updated_at: i64,
}

#[derive(Default)]
struct RelationExtractionState {
    jobs: Mutex<HashMap<String, RelationExtractionSnapshot>>,
    cancels: Mutex<HashMap<String, CancellationToken>>,
}

#[derive(Clone, Default)]
pub struct RelationExtractionService {
    inner: Arc<RelationExtractionState>,
}

impl RelationExtractionService {
    pub async fn start(
        &self,
        storage: Storage,
        mut request: RelationExtractionRequest,
    ) -> Result<RelationExtractionStart, String> {
        request.source_version_id =
            validate_text(&request.source_version_id, "source_version_id", 512)?;
        request.model_id = normalize_optional(request.model_id, "model_id", 160)?;
        request.model_version = normalize_optional(request.model_version, "model_version", 160)?;
        if request.model_candidates.len() > MAX_MODEL_CANDIDATES {
            return Err(format!(
                "model_candidates 超过单次上限 {MAX_MODEL_CANDIDATES}；请分页提交并保留来源版本"
            ));
        }
        let job_id = job_key(&request)?;
        if let Some(existing) = self
            .inner
            .jobs
            .lock()
            .expect("relation jobs poisoned")
            .get(&job_id)
            .cloned()
        {
            if !matches!(existing.status.as_str(), "failed" | "cancelled") {
                return Ok(RelationExtractionStart {
                    job_id,
                    started: existing.running,
                    reused: true,
                    estimated_seconds: existing.estimated_remaining_seconds.unwrap_or(0),
                    note: "已恢复同一来源、模型版本与候选集的后台任务。".into(),
                });
            }
        }
        self.inner
            .jobs
            .lock()
            .expect("relation jobs poisoned")
            .remove(&job_id);
        self.inner
            .cancels
            .lock()
            .expect("relation cancels poisoned")
            .remove(&job_id);

        let estimated_seconds = 8_u32
            .saturating_add(u32::try_from(request.model_candidates.len() / 10).unwrap_or(u32::MAX));
        let now = now_secs();
        self.inner
            .jobs
            .lock()
            .expect("relation jobs poisoned")
            .insert(
                job_id.clone(),
                RelationExtractionSnapshot {
                    job_id: job_id.clone(),
                    source_version_id: request.source_version_id.clone(),
                    running: true,
                    status: "running".into(),
                    phase: "正在读取不可变原文和页码定位".into(),
                    progress: 5,
                    current_item: request.source_version_id.clone(),
                    segments_scanned: 0,
                    candidates_found: 0,
                    validated: 0,
                    needs_review: 0,
                    estimated_remaining_seconds: Some(estimated_seconds),
                    recent_logs: vec![
                        format!("来源版本：{}", request.source_version_id),
                        format!(
                            "文档类型：{:?}；模型候选 {} 条；任务没有强制业务截止时间",
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
        self.inner
            .cancels
            .lock()
            .expect("relation cancels poisoned")
            .insert(job_id.clone(), cancel.clone());
        let state = Arc::clone(&self.inner);
        let task_id = job_id.clone();
        tokio::spawn(async move {
            run_job(state, storage, cancel, task_id, request, estimated_seconds).await;
        });
        Ok(RelationExtractionStart {
            job_id,
            started: true,
            reused: false,
            estimated_seconds,
            note: "已进入 Engine 后台；预计时间只用于展示。切换页面后可按任务 ID 恢复。".into(),
        })
    }

    pub fn status(&self, job_id: &str) -> Result<RelationExtractionSnapshot, String> {
        let job_id = validate_text(job_id, "job_id", 160)?;
        self.inner
            .jobs
            .lock()
            .expect("relation jobs poisoned")
            .get(&job_id)
            .cloned()
            .ok_or_else(|| "关系抽取任务不存在或已清理".into())
    }

    pub fn cancel(&self, job_id: &str) -> Result<bool, String> {
        let job_id = validate_text(job_id, "job_id", 160)?;
        let running = self
            .inner
            .jobs
            .lock()
            .expect("relation jobs poisoned")
            .get(&job_id)
            .is_some_and(|snapshot| snapshot.running);
        if !running {
            return Ok(false);
        }
        let token = self
            .inner
            .cancels
            .lock()
            .expect("relation cancels poisoned")
            .get(&job_id)
            .cloned();
        if let Some(token) = token {
            token.cancel();
            update(
                &self.inner,
                &job_id,
                99,
                "正在安全停止关系抽取",
                "已保存的来源与候选不会删除",
                None,
            );
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

async fn run_job(
    state: Arc<RelationExtractionState>,
    storage: Storage,
    cancel: CancellationToken,
    task_id: String,
    request: RelationExtractionRequest,
    estimated_seconds: u32,
) {
    update(
        &state,
        &task_id,
        15,
        "正在加载证券主数据、公司别名和子公司层级",
        "实体主数据",
        Some(estimated_seconds.saturating_sub(1)),
    );
    if let Err(error) = EntityLinker::new(storage.clone())
        .resolve_query("000001")
        .await
    {
        log(
            &state,
            &task_id,
            format!("实体索引预热失败：{error}；后续确定性实体校验仍会独立执行"),
        );
    }
    if stop_if_cancelled(&state, &task_id, &cancel) {
        return;
    }
    update(
        &state,
        &task_id,
        35,
        "正在逐段发现供应、客户、中标、合同、专利和产能关系",
        "规则候选与模型结构化候选",
        Some(estimated_seconds.saturating_sub(3)),
    );
    let store = RelationExtractionStore::new(storage);
    let result = store
        .extract_source(
            &request.source_version_id,
            request.document_kind,
            request.model_id.as_deref(),
            request.model_version.as_deref(),
            request.model_candidates,
        )
        .await;
    if stop_if_cancelled(&state, &task_id, &cancel) {
        return;
    }
    match result {
        Ok(detail) => finish(&state, &task_id, detail),
        Err(error) => fail(&state, &task_id, error.to_string()),
    }
}

fn finish(state: &Arc<RelationExtractionState>, job_id: &str, detail: ExtractionRunDetail) {
    let validated = detail
        .candidates
        .iter()
        .filter(|candidate| candidate.validation_status == "validated")
        .count();
    let needs_review = detail.candidates.len().saturating_sub(validated);
    let segments = detail
        .candidates
        .iter()
        .flat_map(|candidate| {
            candidate
                .evidence
                .iter()
                .map(|evidence| &evidence.segment_id)
        })
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    if let Some(snapshot) = state
        .jobs
        .lock()
        .expect("relation jobs poisoned")
        .get_mut(job_id)
    {
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
            "完成：{} 个候选；{} 个规则校验通过；{} 个需要补充映射或证据",
            detail.candidates.len(),
            validated,
            needs_review
        ));
        snapshot.result = Some(detail);
        snapshot.updated_at = now_secs();
    }
    state
        .cancels
        .lock()
        .expect("relation cancels poisoned")
        .remove(job_id);
}

fn update(
    state: &Arc<RelationExtractionState>,
    job_id: &str,
    progress: u8,
    phase: &str,
    item: &str,
    estimate: Option<u32>,
) {
    if let Some(snapshot) = state
        .jobs
        .lock()
        .expect("relation jobs poisoned")
        .get_mut(job_id)
    {
        snapshot.progress = progress;
        snapshot.phase = phase.into();
        snapshot.current_item = item.into();
        snapshot.estimated_remaining_seconds = estimate;
        snapshot.updated_at = now_secs();
        snapshot
            .recent_logs
            .push(format!("{progress}% · {phase} · {item}"));
    }
}

fn log(state: &Arc<RelationExtractionState>, job_id: &str, message: String) {
    if let Some(snapshot) = state
        .jobs
        .lock()
        .expect("relation jobs poisoned")
        .get_mut(job_id)
    {
        snapshot.recent_logs.push(message);
        if snapshot.recent_logs.len() > 120 {
            snapshot.recent_logs.remove(0);
        }
        snapshot.updated_at = now_secs();
    }
}

fn fail(state: &Arc<RelationExtractionState>, job_id: &str, error: String) {
    if let Some(snapshot) = state
        .jobs
        .lock()
        .expect("relation jobs poisoned")
        .get_mut(job_id)
    {
        snapshot.running = false;
        snapshot.status = "failed".into();
        snapshot.phase = "关系抽取失败".into();
        snapshot.error = Some(error.clone());
        snapshot.estimated_remaining_seconds = None;
        snapshot.recent_logs.push(format!("错误：{error}"));
        snapshot.updated_at = now_secs();
    }
    state
        .cancels
        .lock()
        .expect("relation cancels poisoned")
        .remove(job_id);
}

fn stop_if_cancelled(
    state: &Arc<RelationExtractionState>,
    job_id: &str,
    token: &CancellationToken,
) -> bool {
    if !token.is_cancelled() {
        return false;
    }
    if let Some(snapshot) = state
        .jobs
        .lock()
        .expect("relation jobs poisoned")
        .get_mut(job_id)
    {
        snapshot.running = false;
        snapshot.status = "cancelled".into();
        snapshot.phase = "任务已安全取消".into();
        snapshot.estimated_remaining_seconds = None;
        snapshot
            .recent_logs
            .push("用户取消；已保存的来源和候选不会删除".into());
        snapshot.updated_at = now_secs();
    }
    state
        .cancels
        .lock()
        .expect("relation cancels poisoned")
        .remove(job_id);
    true
}

fn job_key(request: &RelationExtractionRequest) -> Result<String, String> {
    let payload =
        serde_json::to_vec(request).map_err(|error| format!("关系任务参数编码失败：{error}"))?;
    let digest = format!("{:x}", Sha256::digest(payload));
    Ok(format!("relation-{}", &digest[..24]))
}

fn validate_text(raw: &str, field: &str, max_len: usize) -> Result<String, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(format!("{field} 不能为空"));
    }
    if value.len() > max_len || value.chars().any(char::is_control) {
        return Err(format!("{field} 包含控制字符或超过 {max_len} 字节"));
    }
    Ok(value.into())
}

fn normalize_optional(
    raw: Option<String>,
    field: &str,
    max_len: usize,
) -> Result<Option<String>, String> {
    raw.map(|value| validate_text(&value, field, max_len))
        .transpose()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(source: &str) -> RelationExtractionRequest {
        RelationExtractionRequest {
            source_version_id: source.into(),
            document_kind: DocumentKind::AnnualReport,
            model_id: None,
            model_version: None,
            model_candidates: Vec::new(),
        }
    }

    #[test]
    fn stable_job_identity_uses_the_complete_request() {
        let first = request("srcver:one");
        let second = request("srcver:two");
        assert_eq!(job_key(&first).unwrap(), job_key(&first).unwrap());
        assert_ne!(job_key(&first).unwrap(), job_key(&second).unwrap());
    }

    #[tokio::test]
    async fn missing_verified_source_becomes_a_visible_failure() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(astock_storage::StorageConfig::with_base_dir(dir.path()))
            .expect("open isolated relation storage");
        let service = RelationExtractionService::default();
        let started = service
            .start(storage, request("srcver:missing"))
            .await
            .unwrap();
        let snapshot = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let snapshot = service.status(&started.job_id).unwrap();
                if !snapshot.running {
                    break snapshot;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("missing source relation job did not finish");
        assert_eq!(snapshot.status, "failed");
        assert!(snapshot.error.is_some());
    }

    #[test]
    fn cancellation_does_not_claim_unknown_jobs() {
        let service = RelationExtractionService::default();
        assert!(!service.cancel("relation-missing").unwrap());
    }
}
