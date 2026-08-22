//! Overseas primary-source collection and Global -> A-share transmission.

use std::sync::Arc;

use astock_global_intelligence::{
    global_a_share_golden_chains, normalize_local_publication, DstDisambiguation,
    GlobalDocumentInput, GlobalDocumentPage, GlobalDocumentQuery, GlobalEntity,
    GlobalObservationInput, GlobalProviderRuntime, GlobalStore, NormalizedGlobalClock,
    TransmissionPath,
};
use astock_market_data::{SecEdgarProvider, WorldBankProvider};
use astock_source_verification::SourceVerifier;
use chrono::{Datelike, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use tauri::State;
use tokio_util::sync::CancellationToken;

use crate::error::CmdError;
use crate::state::{AppState, GlobalSyncSnapshot, GlobalSyncState};

#[derive(Debug, Clone, Deserialize)]
pub struct GlobalSyncRequest {
    pub sec_cik: Option<String>,
    pub include_world_bank: Option<bool>,
    pub max_sec_filings: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GlobalSyncStartResponse {
    pub started: bool,
    pub job_id: String,
    pub estimated_seconds: u32,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GlobalSyncCancelResponse {
    pub cancelled: bool,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn global_sync_start(
    state: State<'_, AppState>,
    request: Option<GlobalSyncRequest>,
) -> Result<GlobalSyncStartResponse, CmdError> {
    let request = request.unwrap_or(GlobalSyncRequest {
        sec_cik: None,
        include_world_bank: Some(true),
        max_sec_filings: Some(20),
    });
    let sec_cik = request
        .sec_cik
        .map(|value| {
            value
                .chars()
                .filter(char::is_ascii_digit)
                .collect::<String>()
        })
        .filter(|value| !value.is_empty());
    if sec_cik.as_ref().is_some_and(|value| value.len() > 10) {
        return Err(CmdError::new(
            "invalid_cik",
            "SEC CIK 只能包含 1 至 10 位数字",
        ));
    }
    let include_world_bank = request.include_world_bank.unwrap_or(true);
    let max_sec_filings = request.max_sec_filings.unwrap_or(20).clamp(1, 100);
    let estimated_seconds = if sec_cik.is_some() { 180 } else { 45 };
    let job_id = format!("global-sync-{}", now_secs());
    {
        let mut snapshot = state
            .global_sync
            .snapshot
            .lock()
            .expect("global sync poisoned");
        if snapshot.running {
            return Err(CmdError::new(
                "already_running",
                "海外一级来源同步仍在后台运行，可展开详情或取消",
            ));
        }
        *snapshot = GlobalSyncSnapshot {
            job_id: Some(job_id.clone()),
            running: true,
            status: "running".into(),
            phase: "正在核对海外来源、许可和凭据".into(),
            progress: 2,
            current_provider: "全球来源注册表".into(),
            current_item: "检查 21 个官方入口".into(),
            estimated_remaining_seconds: Some(estimated_seconds),
            started_at: Some(now_secs()),
            updated_at: now_secs(),
            recent_logs: vec![format!(
                "任务参数：World Bank={}；SEC CIK={}；SEC 最多 {} 份",
                if include_world_bank {
                    "启用"
                } else {
                    "关闭"
                },
                sec_cik.as_deref().unwrap_or("未指定"),
                max_sec_filings
            )],
            ..GlobalSyncSnapshot::default()
        };
    }
    let token = CancellationToken::new();
    *state
        .global_sync
        .cancel
        .lock()
        .expect("global cancel poisoned") = Some(token.clone());
    let market = Arc::clone(&state.market);
    let storage = state.storage.clone();
    let sync = Arc::clone(&state.global_sync);
    tauri::async_runtime::spawn(async move {
        let store = GlobalStore::new(storage.clone());
        if let Err(error) = store.seed_provider_catalog().await {
            fail(&sync, format!("海外来源注册失败：{error}"));
            return;
        }
        match store.provider_health().await {
            Ok(providers) => {
                let mut snapshot = sync.snapshot.lock().expect("global sync poisoned");
                snapshot.sources_total = providers.len() as u32;
                snapshot.sources_ready =
                    providers.iter().filter(|source| source.enabled).count() as u32;
                snapshot.source_gaps =
                    providers.iter().filter(|source| !source.enabled).count() as u32;
                let ready = snapshot.sources_ready;
                let gaps = snapshot.source_gaps;
                push_log(
                    &mut snapshot,
                    format!(
                        "来源登记完成：{} 个可直接访问，{} 个等待用户配置凭据",
                        ready, gaps
                    ),
                );
            }
            Err(error) => {
                fail(&sync, format!("读取海外来源状态失败：{error}"));
                return;
            }
        }

        if include_world_bank && !token.is_cancelled() {
            sync_world_bank(&market, &storage, &store, &sync, &token).await;
        }
        if let Some(cik) = sec_cik.as_deref() {
            if !token.is_cancelled() {
                sync_sec(
                    &market,
                    &storage,
                    &store,
                    &sync,
                    &token,
                    cik,
                    max_sec_filings,
                )
                .await;
            }
        } else {
            let mut snapshot = sync.snapshot.lock().expect("global sync poisoned");
            push_log(
                &mut snapshot,
                "SEC EDGAR：本轮未指定 CIK，保留为按公司显式同步，避免全库扫描".into(),
            );
        }
        if token.is_cancelled() {
            finish_cancelled(&sync);
            return;
        }
        let mut snapshot = sync.snapshot.lock().expect("global sync poisoned");
        snapshot.running = false;
        snapshot.status = if snapshot.failures > 0 || snapshot.source_gaps > 0 {
            "completed_with_gaps".into()
        } else {
            "completed".into()
        };
        snapshot.phase = if snapshot.status == "completed" {
            "海外一级来源增量同步完成".into()
        } else {
            "同步完成，部分来源存在透明缺口".into()
        };
        snapshot.progress = 100;
        snapshot.current_item.clear();
        snapshot.estimated_remaining_seconds = Some(0);
        push_log(
            &mut snapshot,
            "任务结束：缺口保持可见，未用中文二次报道补写一级数据。".into(),
        );
        snapshot.updated_at = now_secs();
    });
    Ok(GlobalSyncStartResponse {
        started: true,
        job_id,
        estimated_seconds,
        note: "任务已转入后台；预估仅供参考，不设置整任务强制超时，可切换页面继续使用。".into(),
    })
}

#[tauri::command(rename_all = "snake_case")]
pub async fn global_sync_status(
    state: State<'_, AppState>,
) -> Result<GlobalSyncSnapshot, CmdError> {
    Ok(state
        .global_sync
        .snapshot
        .lock()
        .expect("global sync poisoned")
        .clone())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn global_sync_cancel(
    state: State<'_, AppState>,
) -> Result<GlobalSyncCancelResponse, CmdError> {
    let token = state
        .global_sync
        .cancel
        .lock()
        .expect("global cancel poisoned")
        .clone();
    if let Some(token) = &token {
        token.cancel();
        let mut snapshot = state
            .global_sync
            .snapshot
            .lock()
            .expect("global sync poisoned");
        snapshot.phase = "正在安全停止后台同步".into();
        snapshot.current_item = "当前网络请求结束后停止；已归档数据会保留".into();
        snapshot.updated_at = now_secs();
    }
    Ok(GlobalSyncCancelResponse {
        cancelled: token.is_some(),
    })
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_global_provider_health(
    state: State<'_, AppState>,
) -> Result<Vec<GlobalProviderRuntime>, CmdError> {
    GlobalStore::new(state.storage.clone())
        .provider_health()
        .await
        .map_err(global_error)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn query_global_documents(
    state: State<'_, AppState>,
    query: GlobalDocumentQuery,
) -> Result<GlobalDocumentPage, CmdError> {
    GlobalStore::new(state.storage.clone())
        .query_documents(query)
        .await
        .map_err(global_error)
}

#[tauri::command]
pub fn get_global_golden_chains() -> Vec<astock_global_intelligence::GoldenChainTemplate> {
    global_a_share_golden_chains()
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_global_transmission_paths(
    state: State<'_, AppState>,
    root_entity_id: String,
    as_of: Option<i64>,
    max_depth: Option<usize>,
) -> Result<Vec<TransmissionPath>, CmdError> {
    GlobalStore::new(state.storage.clone())
        .transmission_paths(
            root_entity_id.trim(),
            as_of.unwrap_or_else(now_secs),
            max_depth.unwrap_or(6),
        )
        .await
        .map_err(global_error)
}

async fn sync_world_bank(
    market: &Arc<astock_market_data::MarketData>,
    storage: &astock_storage::Storage,
    store: &GlobalStore,
    sync: &GlobalSyncState,
    token: &CancellationToken,
) {
    let provider = WorldBankProvider::new(market.http.clone());
    let verifier = SourceVerifier::new(storage.clone());
    let indicators = [
        ("NY.GDP.MKTP.CD", "国内生产总值（现价美元）", "USD"),
        ("NV.IND.MANF.CD", "制造业增加值（现价美元）", "USD"),
    ];
    for (index, (indicator, name_zh, currency)) in indicators.iter().enumerate() {
        if token.is_cancelled() {
            return;
        }
        update(
            sync,
            12 + index as u8 * 20,
            "正在读取并归档世界银行官方指标",
            "World Bank Indicators API v2",
            &format!("{indicator} · USA/JPN/KOR/DEU 最近 3 期"),
        );
        let rows = match provider
            .latest(&["USA", "JPN", "KOR", "DEU"], indicator, 3)
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                provider_failure(store, sync, "world_bank", &error.to_string()).await;
                continue;
            }
        };
        {
            let mut snapshot = sync.snapshot.lock().expect("global sync poisoned");
            snapshot.documents_discovered += 1;
            push_log(
                &mut snapshot,
                format!("World Bank {indicator} 返回 {} 条原始观测", rows.len()),
            );
        }
        let source_url = format!(
            "https://api.worldbank.org/v2/country/USA;JPN;KOR;DEU/indicator/{indicator}?format=json&mrv=3&per_page=500&footnote=y"
        );
        let archived = verifier.fetch_source_document(&source_url).await;
        let (source_version_id, content_hash, gap_reason) = match archived {
            Ok(detail) => match detail.version {
                Some(version) => (
                    Some(version.source_version_id),
                    Some(version.content_hash),
                    None,
                ),
                None => (None, None, detail.document.failure_message),
            },
            Err(error) => (None, None, Some(error.to_string())),
        };
        let now = now_secs();
        let upstream_id = format!("{indicator}:{}", chrono::Utc::now().year());
        let document = store
            .ingest_document(GlobalDocumentInput {
                provider_id: "world_bank".into(),
                upstream_id,
                document_type: "official_indicator_api".into(),
                title_original: format!("World Bank indicator {indicator}"),
                title_zh: Some((*name_zh).into()),
                original_language: "en".into(),
                original_url: source_url,
                source_version_id: source_version_id.clone(),
                content_hash,
                published_at_utc: now,
                published_local: chrono::DateTime::from_timestamp(now, 0)
                    .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_else(|| now.to_string()),
                published_timezone: "UTC".into(),
                utc_offset_seconds: 0,
                first_seen_at: now,
                revision_no: 1,
                revision_of: None,
                translation_status: "deterministic_fields_protected".into(),
                gap_reason: gap_reason.or_else(|| {
                    Some("官方 API 不提供统一发布秒级时钟；以首次发现作为 PIT 可用时点".into())
                }),
            })
            .await;
        let document = match document {
            Ok(document) => document,
            Err(error) => {
                provider_failure(store, sync, "world_bank", &error.to_string()).await;
                continue;
            }
        };
        let Some(version_id) = source_version_id else {
            provider_failure(store, sync, "world_bank", "官方响应未形成可核验归档版本").await;
            continue;
        };
        {
            let mut snapshot = sync.snapshot.lock().expect("global sync poisoned");
            snapshot.documents_archived += 1;
        }
        for row in rows {
            let entity_id = format!("global:country:{}", row.country_iso3.to_ascii_lowercase());
            let _ = store
                .upsert_entity(GlobalEntity {
                    entity_id: entity_id.clone(),
                    entity_type: "country".into(),
                    legal_name: row.country_name.clone(),
                    name_zh: None,
                    jurisdiction: row.country_iso3.clone(),
                    identifiers: serde_json::json!({"world_bank_id": row.country_id, "iso3": row.country_iso3}),
                    aliases: Vec::new(),
                    translation_status: "pending".into(),
                })
                .await;
            let scale = 10_i64.pow(row.decimal_places.min(6));
            let scaled = row.value.and_then(|value| {
                let scaled = value * scale as f64;
                (scaled.is_finite() && scaled >= i64::MIN as f64 && scaled <= i64::MAX as f64)
                    .then_some(scaled.round() as i64)
            });
            let saved = store
                .ingest_observation(GlobalObservationInput {
                    document_id: document.clone(),
                    entity_id: Some(entity_id),
                    indicator_code: row.indicator_code,
                    period: row.period,
                    value_scaled: scaled,
                    scale,
                    value_text: row.value.map(|value| value.to_string()),
                    unit_original: if row.unit.is_empty() {
                        (*currency).into()
                    } else {
                        row.unit
                    },
                    currency_original: Some((*currency).into()),
                    released_at_utc: now,
                    revision_no: 1,
                    replaces_observation_id: None,
                    source_version_id: version_id.clone(),
                })
                .await;
            let mut snapshot = sync.snapshot.lock().expect("global sync poisoned");
            if saved.is_ok() {
                snapshot.observations_saved += 1;
            } else {
                snapshot.failures += 1;
            }
        }
        let _ = store.record_provider_success("world_bank").await;
    }
}

async fn sync_sec(
    market: &Arc<astock_market_data::MarketData>,
    storage: &astock_storage::Storage,
    store: &GlobalStore,
    sync: &GlobalSyncState,
    token: &CancellationToken,
    cik: &str,
    max_filings: u32,
) {
    update(
        sync,
        55,
        "正在读取 SEC EDGAR 公司提交历史",
        "SEC EDGAR",
        &format!("CIK {cik}"),
    );
    let provider = SecEdgarProvider::new(market.http.clone());
    let filings = match provider.submissions(cik).await {
        Ok(rows) => rows,
        Err(error) => {
            provider_failure(store, sync, "sec_edgar", &error.to_string()).await;
            return;
        }
    };
    let mut filings: Vec<_> = filings
        .into_iter()
        .filter(|row| matches!(row.form.as_str(), "10-K" | "10-Q" | "8-K" | "20-F" | "6-K"))
        .take(max_filings as usize)
        .collect();
    let total = filings.len().max(1);
    {
        let mut snapshot = sync.snapshot.lock().expect("global sync poisoned");
        snapshot.documents_discovered += filings.len() as u32;
        push_log(
            &mut snapshot,
            format!(
                "SEC CIK {cik}：筛选出 {} 份 10-K/10-Q/8-K/20-F/6-K",
                filings.len()
            ),
        );
    }
    let verifier = SourceVerifier::new(storage.clone());
    let user_agent = std::env::var("ASTOCK_SEC_USER_AGENT").ok();
    for (index, filing) in filings.drain(..).enumerate() {
        if token.is_cancelled() {
            return;
        }
        update(
            sync,
            58 + ((index + 1) * 37 / total) as u8,
            "正在归档 SEC 正式原文并保留原始发布时间",
            "SEC EDGAR",
            &format!(
                "{} · {} · {}",
                filing.form, filing.filing_date, filing.primary_document
            ),
        );
        let archived = verifier
            .fetch_source_document_with_user_agent(
                &filing.primary_document_url,
                user_agent.as_deref(),
            )
            .await;
        let (source_version_id, content_hash, archive_gap) = match archived {
            Ok(detail) => match detail.version {
                Some(version) => (
                    Some(version.source_version_id),
                    Some(version.content_hash),
                    None,
                ),
                None => (None, None, detail.document.failure_message),
            },
            Err(error) => (None, None, Some(error.to_string())),
        };
        let (clock, clock_gap) = sec_clock(&filing.acceptance_datetime, &filing.filing_date);
        let entity_id = format!("global:sec:{}", filing.cik);
        let _ = store
            .upsert_entity(GlobalEntity {
                entity_id,
                entity_type: "legal_entity".into(),
                legal_name: filing.legal_name.clone(),
                name_zh: None,
                jurisdiction: "US".into(),
                identifiers: serde_json::json!({"cik": filing.cik, "tickers": filing.tickers, "exchanges": filing.exchanges}),
                aliases: Vec::new(),
                translation_status: "pending".into(),
            })
            .await;
        let gap_reason = [archive_gap, clock_gap]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("；");
        let ingested = store
            .ingest_document(GlobalDocumentInput {
                provider_id: "sec_edgar".into(),
                upstream_id: filing.accession_number.clone(),
                document_type: filing.form.clone(),
                title_original: format!(
                    "{} {} ({})",
                    filing.legal_name, filing.form, filing.report_date
                ),
                title_zh: None,
                original_language: "en".into(),
                original_url: filing.primary_document_url,
                source_version_id: source_version_id.clone(),
                content_hash,
                published_at_utc: clock.utc_timestamp,
                published_local: clock.original_local,
                published_timezone: clock.timezone,
                utc_offset_seconds: clock.utc_offset_seconds,
                first_seen_at: now_secs(),
                revision_no: 1,
                revision_of: None,
                translation_status: "pending_deterministic_validation".into(),
                gap_reason: (!gap_reason.is_empty()).then_some(gap_reason),
            })
            .await;
        let mut snapshot = sync.snapshot.lock().expect("global sync poisoned");
        match ingested {
            Ok(_) if source_version_id.is_some() => snapshot.documents_archived += 1,
            Ok(_) => snapshot.source_gaps += 1,
            Err(error) => {
                snapshot.failures += 1;
                push_log(
                    &mut snapshot,
                    format!("SEC {} 入库失败：{error}", filing.accession_number),
                );
            }
        }
    }
    let _ = store.record_provider_success("sec_edgar").await;
}

fn sec_clock(acceptance: &str, filing_date: &str) -> (NormalizedGlobalClock, Option<String>) {
    if let Ok(value) = chrono::DateTime::parse_from_rfc3339(acceptance) {
        return (
            NormalizedGlobalClock {
                original_local: acceptance.to_string(),
                timezone: "UTC offset from SEC acceptanceDateTime".into(),
                utc_timestamp: value.timestamp(),
                utc_offset_seconds: value.offset().local_minus_utc(),
                utc_iso: value.with_timezone(&chrono::Utc).to_rfc3339(),
            },
            None,
        );
    }
    if let Ok(local) = NaiveDateTime::parse_from_str(acceptance, "%Y-%m-%dT%H:%M:%S%.f") {
        if let Ok(clock) =
            normalize_local_publication(local, "America/New_York", DstDisambiguation::Reject)
        {
            return (clock, None);
        }
    }
    let local = NaiveDate::parse_from_str(filing_date, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(23, 59, 59))
        .unwrap_or_else(|| {
            chrono::DateTime::from_timestamp(now_secs(), 0)
                .unwrap()
                .naive_utc()
        });
    let clock = normalize_local_publication(local, "America/New_York", DstDisambiguation::Later)
        .unwrap_or(NormalizedGlobalClock {
            original_local: local.to_string(),
            timezone: "UTC".into(),
            utc_timestamp: local.and_utc().timestamp(),
            utc_offset_seconds: 0,
            utc_iso: local.and_utc().to_rfc3339(),
        });
    (
        clock,
        Some("SEC acceptanceDateTime 缺失或不可解析；保守按 filingDate 当地日终处理".into()),
    )
}

async fn provider_failure(
    store: &GlobalStore,
    sync: &GlobalSyncState,
    provider: &str,
    error: &str,
) {
    let _ = store.record_provider_failure(provider, error).await;
    let mut snapshot = sync.snapshot.lock().expect("global sync poisoned");
    snapshot.failures += 1;
    snapshot.source_gaps += 1;
    push_log(
        &mut snapshot,
        format!("{provider} 失败，已记录独立退避；不会用媒体摘要补位：{error}"),
    );
}

fn update(sync: &GlobalSyncState, progress: u8, phase: &str, provider: &str, item: &str) {
    let mut snapshot = sync.snapshot.lock().expect("global sync poisoned");
    let changed = snapshot.phase != phase
        || snapshot.current_provider != provider
        || snapshot.current_item != item;
    snapshot.progress = progress;
    snapshot.phase = phase.into();
    snapshot.current_provider = provider.into();
    snapshot.current_item = item.into();
    if changed {
        push_log(&mut snapshot, format!("{provider}：{phase} · {item}"));
    }
    snapshot.updated_at = now_secs();
}

fn push_log(snapshot: &mut GlobalSyncSnapshot, message: String) {
    snapshot.recent_logs.push(message);
    if snapshot.recent_logs.len() > 100 {
        snapshot.recent_logs.remove(0);
    }
}

fn fail(sync: &GlobalSyncState, error: String) {
    let mut snapshot = sync.snapshot.lock().expect("global sync poisoned");
    snapshot.running = false;
    snapshot.status = "failed".into();
    snapshot.phase = "海外同步失败，可复制诊断信息".into();
    snapshot.error = Some(error.clone());
    push_log(&mut snapshot, error);
    snapshot.updated_at = now_secs();
}

fn finish_cancelled(sync: &GlobalSyncState) {
    let mut snapshot = sync.snapshot.lock().expect("global sync poisoned");
    snapshot.running = false;
    snapshot.status = "cancelled".into();
    snapshot.phase = "已按用户要求停止海外同步".into();
    snapshot.estimated_remaining_seconds = None;
    push_log(
        &mut snapshot,
        "任务已停止，已归档的官方版本与观测值会保留。".into(),
    );
    snapshot.updated_at = now_secs();
}

fn global_error(error: astock_global_intelligence::Error) -> CmdError {
    CmdError::new("global_intelligence", error.to_string())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sec_fallback_is_conservative_and_preserves_timezone() {
        let (clock, gap) = sec_clock("", "2026-11-01");
        assert!(gap.is_some());
        assert_eq!(clock.timezone, "America/New_York");
        assert_eq!(clock.original_local, "2026-11-01 23:59:59");
    }
}
