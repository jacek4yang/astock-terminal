//! Formal-disclosure timeline and background incremental synchronization.

use std::sync::Arc;

use astock_disclosure::{
    official_provider_catalog, DisclosureAttachmentInput, DisclosureInput, DisclosurePage,
    DisclosureQuery, DisclosureSecurity, DisclosureStore, ProviderAuthority,
};
use astock_market_data::providers::{CninfoDisclosureProvider, NoticeNode};
use astock_source_verification::SourceVerifier;
use serde::{Deserialize, Serialize};
use tauri::State;
use tokio_util::sync::CancellationToken;

use crate::cache_path::shanghai_now;
use crate::error::CmdError;
use crate::state::{AppState, DisclosureSyncSnapshot, DisclosureSyncState};

#[derive(Debug, Clone, Deserialize)]
pub struct DisclosureSyncRequest {
    pub security_code: Option<String>,
    pub days: Option<u32>,
    pub max_pages: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DisclosureSyncStartResponse {
    pub started: bool,
    pub job_id: String,
    pub estimated_seconds: u32,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DisclosureSyncCancelResponse {
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderHealth {
    pub provider_id: String,
    pub provider_name: String,
    pub authority: String,
    pub authority_name: String,
    pub enabled: bool,
    pub public_index_url: String,
    pub target_latency_secs: u32,
    pub rate_limit_per_minute: u32,
    pub last_attempt_at: Option<i64>,
    pub last_success_at: Option<i64>,
    pub consecutive_failures: u32,
    pub retry_after: Option<i64>,
    pub last_error: Option<String>,
    pub note: String,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn disclosure_sync_start(
    state: State<'_, AppState>,
    request: Option<DisclosureSyncRequest>,
) -> Result<DisclosureSyncStartResponse, CmdError> {
    let request = request.unwrap_or(DisclosureSyncRequest {
        security_code: None,
        days: None,
        max_pages: None,
    });
    let code = request
        .security_code
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(value) = &code {
        astock_core::Symbol::new(value)?;
    }
    let days = request.days.unwrap_or(365).clamp(1, 3_650);
    let max_pages = request
        .max_pages
        .unwrap_or(if code.is_some() { 10 } else { 3 })
        .clamp(1, 50);
    let job_id = format!("disclosure-sync-{}", now_secs());
    {
        let mut snapshot = state
            .disclosure_sync
            .snapshot
            .lock()
            .expect("disclosure sync poisoned");
        if snapshot.running {
            return Err(CmdError::new(
                "already_running",
                "正式披露同步仍在后台运行，可查看详情或取消后重试",
            ));
        }
        *snapshot = DisclosureSyncSnapshot {
            job_id: Some(job_id.clone()),
            running: true,
            status: "running".into(),
            phase: "正在初始化正式来源与增量游标".into(),
            progress: 2,
            current_provider: "来源注册表".into(),
            current_item: code.clone().unwrap_or_else(|| "全市场".into()),
            estimated_remaining_seconds: Some((max_pages * 8).max(15)),
            started_at: Some(now_secs()),
            updated_at: now_secs(),
            recent_logs: vec![format!(
                "同步范围：{}，最近 {days} 天，最多 {max_pages} 页",
                code.as_deref().unwrap_or("全市场")
            )],
            ..DisclosureSyncSnapshot::default()
        };
    }
    let token = CancellationToken::new();
    *state
        .disclosure_sync
        .cancel
        .lock()
        .expect("disclosure cancel poisoned") = Some(token.clone());
    let market = Arc::clone(&state.market);
    let storage = state.storage.clone();
    let sync = Arc::clone(&state.disclosure_sync);
    tauri::async_runtime::spawn(async move {
        let store = DisclosureStore::new(storage);
        update(
            &sync,
            3,
            "正在登记正式来源与增量游标",
            "本地来源注册表",
            "核对来源启用状态、频率与失败退避记录",
        );
        if let Err(error) = store.seed_provider_catalog().await {
            fail(&sync, format!("初始化正式来源失败：{error}"));
            return;
        }
        sync_cninfo_official(
            &market,
            &store,
            &sync,
            &token,
            code.as_deref(),
            begin_end(days),
            max_pages,
        )
        .await;
        if token.is_cancelled() {
            finish_cancelled(&sync);
            return;
        }
        update(
            &sync,
            52,
            "正在读取补漏公告索引",
            "东方财富公告镜像（发现通道）",
            "等待上游返回分页索引",
        );
        let today = shanghai_now().date_naive();
        let begin = today - chrono::Duration::days(i64::from(days));
        let fetched = market
            .em_datacenter
            .notices(
                code.as_deref(),
                NoticeNode::All,
                Some(begin),
                Some(today),
                max_pages,
            )
            .await;
        let rows = match fetched {
            Ok(value) => value.data,
            Err(error) => {
                let _ = store
                    .record_provider_failure("eastmoney_notice_mirror", &error.to_string())
                    .await;
                fail(&sync, format!("公告镜像索引读取失败：{error}"));
                return;
            }
        };
        {
            let mut snapshot = sync.snapshot.lock().expect("disclosure sync poisoned");
            snapshot.discovered = rows.len() as u32;
            snapshot.progress = 60;
            snapshot.phase = "正在规范化、去重并建立修订关系".into();
            snapshot
                .recent_logs
                .push(format!("索引返回 {} 条公告，开始逐条规范化", rows.len()));
            snapshot.updated_at = now_secs();
        }
        let total = rows.len().max(1);
        for (index, row) in rows.into_iter().enumerate() {
            if token.is_cancelled() {
                finish_cancelled(&sync);
                return;
            }
            let published_at = row
                .notice_date
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .map(|time| time.and_utc().timestamp());
            let security_code = if row.stock_code.is_empty() {
                code.clone().unwrap_or_default()
            } else {
                row.stock_code.clone()
            };
            let input = DisclosureInput {
                provider_id: "eastmoney_notice_mirror".into(),
                provider_name: "东方财富公告镜像".into(),
                authority: ProviderAuthority::MirrorDiscovery,
                entry_kind: "mirror_index".into(),
                upstream_id: (!row.art_code.is_empty()).then_some(row.art_code.clone()),
                original_url: row.url.clone(),
                title: row.title.clone(),
                published_at,
                publication_precision: "date".into(),
                first_seen_at: now_secs(),
                latency_ms: None,
                securities: (!security_code.is_empty())
                    .then(|| DisclosureSecurity {
                        code: security_code,
                        name: row.stock_name.clone(),
                        market: market_for_code(&row.stock_code).into(),
                    })
                    .into_iter()
                    .collect(),
                attachments: Vec::new(),
                source_version_id: None,
                extracted_text: None,
                extraction_status: "index_only".into(),
                review_reason: Some(
                    "镜像仅用于发现；等待交易所、巨潮或公司 IR 正式原文归档".into(),
                ),
            };
            let outcome = store.ingest(input).await;
            let mut snapshot = sync.snapshot.lock().expect("disclosure sync poisoned");
            snapshot.current_item = row.title.chars().take(80).collect();
            snapshot.normalized += 1;
            match outcome {
                Ok(outcome) => {
                    if outcome.inserted {
                        snapshot.inserted += 1;
                    } else {
                        snapshot.deduplicated += 1;
                    }
                    snapshot.needs_review += 1;
                    if outcome.linked_revision.is_some() {
                        push_log(&mut snapshot, format!("已建立修订/撤回关系：{}", row.title));
                    }
                }
                Err(error) => {
                    snapshot.failures += 1;
                    push_log(
                        &mut snapshot,
                        format!("规范化失败：{} · {error}", row.title),
                    );
                }
            }
            snapshot.progress = 60 + (((index + 1) * 35 / total) as u8);
            snapshot.estimated_remaining_seconds = Some(((total - index - 1) as u32).div_ceil(25));
            snapshot.updated_at = now_secs();
        }
        let mut snapshot = sync.snapshot.lock().expect("disclosure sync poisoned");
        snapshot.running = false;
        snapshot.status = "completed".into();
        snapshot.phase = "增量索引同步完成".into();
        snapshot.progress = 100;
        snapshot.current_item.clear();
        snapshot.estimated_remaining_seconds = Some(0);
        push_log(
            &mut snapshot,
            "同步完成。镜像记录保持“待正式原文核验”，不会冒充一级来源。".into(),
        );
        snapshot.updated_at = now_secs();
    });
    Ok(DisclosureSyncStartResponse {
        started: true,
        job_id,
        estimated_seconds: (max_pages * 8).max(15),
        note: "任务已转入后台；预估仅供参考，不设强制超时，可切换页面后继续查看。".into(),
    })
}

#[tauri::command(rename_all = "snake_case")]
pub async fn disclosure_sync_status(
    state: State<'_, AppState>,
) -> Result<DisclosureSyncSnapshot, CmdError> {
    Ok(state
        .disclosure_sync
        .snapshot
        .lock()
        .expect("disclosure sync poisoned")
        .clone())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn disclosure_sync_cancel(
    state: State<'_, AppState>,
) -> Result<DisclosureSyncCancelResponse, CmdError> {
    let token = state
        .disclosure_sync
        .cancel
        .lock()
        .expect("disclosure cancel poisoned")
        .clone();
    let cancelled = token.is_some();
    if let Some(token) = token {
        token.cancel();
    }
    Ok(DisclosureSyncCancelResponse { cancelled })
}

#[tauri::command(rename_all = "snake_case")]
pub async fn query_disclosures(
    state: State<'_, AppState>,
    query: DisclosureQuery,
) -> Result<DisclosurePage, CmdError> {
    DisclosureStore::new(state.storage.clone())
        .query(query)
        .await
        .map_err(disclosure_error)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_disclosure_detail(
    state: State<'_, AppState>,
    disclosure_id: String,
) -> Result<astock_disclosure::DisclosureDetail, CmdError> {
    DisclosureStore::new(state.storage.clone())
        .detail(&disclosure_id)
        .await
        .map_err(disclosure_error)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_disclosure_provider_health(
    state: State<'_, AppState>,
) -> Result<Vec<ProviderHealth>, CmdError> {
    DisclosureStore::new(state.storage.clone())
        .seed_provider_catalog()
        .await
        .map_err(disclosure_error)?;
    let catalog = official_provider_catalog();
    state.storage.run(move |conn| {
        let mut output = Vec::new();
        for definition in catalog {
            let runtime = conn.query_row(
                "SELECT enabled,last_attempt_at,last_success_at,consecutive_failures,retry_after,last_error
                 FROM disclosure_provider_state WHERE provider_id=?1", [definition.provider_id],
                |row| Ok((row.get::<_, i64>(0)? != 0, row.get(1)?, row.get(2)?, row.get::<_, u32>(3)?, row.get(4)?, row.get(5)?)),
            )?;
            output.push(ProviderHealth { provider_id: definition.provider_id.into(), provider_name: definition.name.into(),
                authority: definition.authority.token().into(), authority_name: definition.authority.chinese_name().into(),
                enabled: runtime.0, public_index_url: definition.public_index_url.into(), target_latency_secs: definition.target_latency_secs,
                rate_limit_per_minute: definition.rate_limit_per_minute, last_attempt_at: runtime.1, last_success_at: runtime.2,
                consecutive_failures: runtime.3, retry_after: runtime.4, last_error: runtime.5, note: definition.note.into() });
        }
        Ok(output)
    }).await.map_err(Into::into)
}

fn disclosure_error(error: astock_disclosure::Error) -> CmdError {
    CmdError::new("disclosure", error.to_string())
}
fn market_for_code(code: &str) -> &'static str {
    if code.starts_with('6') {
        "SSE"
    } else if code.starts_with('8') || code.starts_with('4') {
        "BSE"
    } else {
        "SZSE"
    }
}
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}
fn update(sync: &DisclosureSyncState, progress: u8, phase: &str, provider: &str, item: &str) {
    let mut s = sync.snapshot.lock().expect("disclosure sync poisoned");
    let changed = s.phase != phase || s.current_provider != provider || s.current_item != item;
    s.progress = progress;
    s.phase = phase.into();
    s.current_provider = provider.into();
    s.current_item = item.into();
    if changed {
        push_log(&mut s, format!("{provider}：{phase} · {item}"));
    }
    s.updated_at = now_secs();
}
fn push_log(snapshot: &mut DisclosureSyncSnapshot, message: String) {
    snapshot.recent_logs.push(message);
    if snapshot.recent_logs.len() > 80 {
        snapshot.recent_logs.remove(0);
    }
}
fn fail(sync: &DisclosureSyncState, error: String) {
    let mut s = sync.snapshot.lock().expect("disclosure sync poisoned");
    s.running = false;
    s.status = "failed".into();
    s.phase = "同步失败，可展开复制诊断信息".into();
    s.error = Some(error.clone());
    push_log(&mut s, error);
    s.updated_at = now_secs();
}
fn finish_cancelled(sync: &DisclosureSyncState) {
    let mut s = sync.snapshot.lock().expect("disclosure sync poisoned");
    s.running = false;
    s.status = "cancelled".into();
    s.phase = "已按用户要求取消".into();
    s.estimated_remaining_seconds = None;
    push_log(&mut s, "任务已取消，已入库的增量记录会保留。".into());
    s.updated_at = now_secs();
}

fn begin_end(days: u32) -> String {
    let end = shanghai_now().date_naive();
    let begin = end - chrono::Duration::days(i64::from(days));
    format!("{begin}~{end}")
}

async fn sync_cninfo_official(
    market: &Arc<astock_market_data::MarketData>,
    store: &DisclosureStore,
    sync: &DisclosureSyncState,
    token: &CancellationToken,
    code: Option<&str>,
    date_range: String,
    max_pages: u32,
) {
    let provider = CninfoDisclosureProvider::new(market.http.clone());
    let columns: Vec<&str> = match code {
        Some(value) if value.starts_with('6') => vec!["sse"],
        Some(value) if value.starts_with('8') || value.starts_with('4') => vec!["bse"],
        Some(_) => vec!["szse"],
        None => vec!["sse", "szse", "bse"],
    };
    let verifier = SourceVerifier::new(store_storage(store));
    let mut index_position = 0_u32;
    let index_total = max_pages.saturating_mul(columns.len() as u32).max(1);
    for column in columns {
        let mut page_number = 1_u32;
        loop {
            if token.is_cancelled() {
                return;
            }
            index_position += 1;
            update(
                sync,
                5 + ((index_position * 12 / index_total) as u8).min(12),
                "正在读取巨潮法定披露索引",
                "巨潮资讯",
                &format!("{column} 市场第 {page_number} 页"),
            );
            let page = match provider
                .query(code, column, page_number, 50, Some(&date_range))
                .await
            {
                Ok(page) => page,
                Err(error) => {
                    let _ = store
                        .record_provider_failure("cninfo", &error.to_string())
                        .await;
                    let mut snapshot = sync.snapshot.lock().expect("disclosure sync poisoned");
                    snapshot.failures += 1;
                    push_log(
                        &mut snapshot,
                        format!("巨潮 {column} 第 {page_number} 页失败，已按来源独立退避：{error}"),
                    );
                    break;
                }
            };
            if page.rows.is_empty() {
                break;
            }
            {
                let mut snapshot = sync.snapshot.lock().expect("disclosure sync poisoned");
                snapshot.discovered = snapshot.discovered.saturating_add(page.rows.len() as u32);
                push_log(
                    &mut snapshot,
                    format!(
                        "巨潮 {column} 第 {page_number} 页发现 {} 份正式披露",
                        page.rows.len()
                    ),
                );
            }
            for row in page.rows {
                if token.is_cancelled() {
                    return;
                }
                update(
                    sync,
                    20,
                    "正在归档正式 PDF 并提取证据",
                    "巨潮资讯",
                    &row.title,
                );
                let source = verifier.fetch_source_document(&row.pdf_url).await;
                let (
                    source_version_id,
                    content_hash,
                    extraction_status,
                    review_reason,
                    extracted_text,
                    page_count,
                ) = match source {
                    Ok(detail) => {
                        let page_count = detail
                            .segments
                            .iter()
                            .filter_map(|segment| segment.page_number)
                            .max();
                        let text = detail
                            .segments
                            .iter()
                            .map(|segment| segment.text.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                            .chars()
                            .take(120_000)
                            .collect::<String>();
                        let version = detail
                            .version
                            .map(|version| (version.source_version_id, version.content_hash));
                        let verified = version.is_some();
                        let (version_id, content_hash) = version
                            .map(|value| (Some(value.0), Some(value.1)))
                            .unwrap_or((None, None));
                        (
                            version_id,
                            content_hash,
                            if verified {
                                "parsed".into()
                            } else {
                                detail
                                    .document
                                    .failure_kind
                                    .unwrap_or(detail.document.access_status)
                            },
                            detail.document.failure_message,
                            (!text.is_empty()).then_some(text),
                            page_count,
                        )
                    }
                    Err(error) => (
                        None,
                        None,
                        "archive_failed".into(),
                        Some(error.to_string()),
                        None,
                        None,
                    ),
                };
                let verified = source_version_id.is_some();
                let published_at = row.announcement_time_ms.map(|value| value / 1_000);
                let security_code = row.security_code.clone();
                let input = DisclosureInput {
                    provider_id: "cninfo".into(),
                    provider_name: "巨潮资讯".into(),
                    authority: ProviderAuthority::Exchange,
                    entry_kind: "statutory_index".into(),
                    upstream_id: Some(row.announcement_id.clone()),
                    original_url: row.pdf_url.clone(),
                    title: row.title.clone(),
                    published_at,
                    publication_precision: "millisecond".into(),
                    first_seen_at: now_secs(),
                    latency_ms: None,
                    securities: (!security_code.is_empty())
                        .then(|| DisclosureSecurity {
                            code: security_code.clone(),
                            name: row.security_name.clone(),
                            market: market_for_code(&security_code).into(),
                        })
                        .into_iter()
                        .collect(),
                    attachments: vec![DisclosureAttachmentInput {
                        name: format!("{}.PDF", row.title),
                        original_url: row.pdf_url.clone(),
                        media_type: "application/pdf".into(),
                        parent_url: None,
                        byte_size: row.adjunct_size_kb.map(|value| value.saturating_mul(1_024)),
                        content_hash,
                        source_version_id: source_version_id.clone(),
                        page_count,
                        extraction_status: extraction_status.clone(),
                        review_reason: review_reason.clone(),
                    }],
                    source_version_id,
                    extracted_text,
                    extraction_status,
                    review_reason,
                };
                let outcome = store.ingest(input).await;
                let mut snapshot = sync.snapshot.lock().expect("disclosure sync poisoned");
                snapshot.normalized += 1;
                match outcome {
                    Ok(outcome) => {
                        if outcome.inserted {
                            snapshot.inserted += 1;
                        } else {
                            snapshot.deduplicated += 1;
                        }
                        if verified {
                            snapshot.primary_verified += 1;
                        } else {
                            snapshot.needs_review += 1;
                        }
                    }
                    Err(error) => {
                        snapshot.failures += 1;
                        push_log(
                            &mut snapshot,
                            format!("正式披露入库失败：{} · {error}", row.title),
                        );
                    }
                }
                snapshot.progress = 20 + ((snapshot.normalized % 30) as u8).min(28);
                snapshot.updated_at = now_secs();
            }
            if page_number >= page.total_pages.min(max_pages) {
                break;
            }
            page_number += 1;
        }
    }
}

fn store_storage(store: &DisclosureStore) -> astock_storage::Storage {
    store.storage_clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn market_mapping_is_explicit() {
        assert_eq!(market_for_code("600001"), "SSE");
        assert_eq!(market_for_code("000001"), "SZSE");
        assert_eq!(market_for_code("830001"), "BSE");
    }
}
