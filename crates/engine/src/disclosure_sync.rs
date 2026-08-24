use std::sync::{Arc, Mutex};

use astock_core::Symbol;
use astock_disclosure::{
    official_provider_catalog, DisclosureAttachmentInput, DisclosureInput, DisclosureSecurity,
    DisclosureStore, ProviderAuthority,
};
use astock_market_data::{CninfoDisclosureProvider, MarketData};
use astock_source_verification::SourceVerifier;
use astock_storage::Storage;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

const IMPLEMENTED_PROVIDERS: &[&str] = &["cninfo", "eastmoney_notice_mirror"];

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisclosureSyncRequest {
    #[serde(default)]
    pub security_code: Option<String>,
    #[serde(default)]
    pub days: Option<u32>,
    #[serde(default)]
    pub max_pages: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DisclosureSyncStart {
    pub started: bool,
    pub job_id: String,
    pub estimated_seconds: u32,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DisclosureSyncSnapshot {
    pub job_id: Option<String>,
    pub running: bool,
    pub status: String,
    pub phase: String,
    pub progress: u8,
    pub current_provider: String,
    pub current_item: String,
    pub discovered: u32,
    pub normalized: u32,
    pub inserted: u32,
    pub deduplicated: u32,
    pub primary_verified: u32,
    pub needs_review: u32,
    pub failures: u32,
    pub estimated_remaining_seconds: Option<u32>,
    pub recent_logs: Vec<String>,
    pub started_at: Option<i64>,
    pub updated_at: i64,
    pub error: Option<String>,
}

impl Default for DisclosureSyncSnapshot {
    fn default() -> Self {
        Self {
            job_id: None,
            running: false,
            status: "idle".into(),
            phase: "尚未同步正式披露".into(),
            progress: 0,
            current_provider: String::new(),
            current_item: String::new(),
            discovered: 0,
            normalized: 0,
            inserted: 0,
            deduplicated: 0,
            primary_verified: 0,
            needs_review: 0,
            failures: 0,
            estimated_remaining_seconds: None,
            recent_logs: Vec::new(),
            started_at: None,
            updated_at: 0,
            error: None,
        }
    }
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

struct DisclosureSyncState {
    snapshot: Mutex<DisclosureSyncSnapshot>,
    cancel: Mutex<Option<CancellationToken>>,
}

impl Default for DisclosureSyncState {
    fn default() -> Self {
        Self {
            snapshot: Mutex::new(DisclosureSyncSnapshot::default()),
            cancel: Mutex::new(None),
        }
    }
}

#[derive(Clone, Default)]
pub struct DisclosureSyncService {
    inner: Arc<DisclosureSyncState>,
}

impl DisclosureSyncService {
    pub async fn start(
        &self,
        market: Arc<MarketData>,
        storage: Storage,
        request: DisclosureSyncRequest,
    ) -> Result<DisclosureSyncStart, String> {
        let code = normalize_security_code(request.security_code)?;
        let days = request.days.unwrap_or(365).clamp(1, 3_650);
        let max_pages = request
            .max_pages
            .unwrap_or(if code.is_some() { 10 } else { 3 })
            .clamp(1, 50);
        let estimated_seconds = (max_pages * 8).max(15);
        let job_id = format!("disclosure-sync-{}", now_secs());
        {
            let mut snapshot = self
                .inner
                .snapshot
                .lock()
                .expect("disclosure sync poisoned");
            if snapshot.running {
                return Err("正式披露同步仍在后台运行，可查看详情或取消后重试".into());
            }
            *snapshot = DisclosureSyncSnapshot {
                job_id: Some(job_id.clone()),
                running: true,
                status: "running".into(),
                phase: "正在初始化正式来源与增量游标".into(),
                progress: 2,
                current_provider: "来源注册表".into(),
                current_item: code.clone().unwrap_or_else(|| "全市场".into()),
                estimated_remaining_seconds: Some(estimated_seconds),
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
        *self
            .inner
            .cancel
            .lock()
            .expect("disclosure cancel poisoned") = Some(token.clone());
        let sync = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let store = DisclosureStore::new(storage.clone());
            if let Err(error) = store.seed_provider_catalog().await {
                fail(&sync, format!("初始化正式来源失败：{error}"));
                return;
            }
            sync_cninfo(
                &market,
                &storage,
                &store,
                &sync,
                &token,
                code.as_deref(),
                days,
                max_pages,
            )
            .await;
            if token.is_cancelled() {
                finish_cancelled(&sync);
                return;
            }
            sync_eastmoney_mirror(
                &market,
                &store,
                &sync,
                &token,
                code.as_deref(),
                days,
                max_pages,
            )
            .await;
            if token.is_cancelled() {
                finish_cancelled(&sync);
                return;
            }
            let mut snapshot = sync.snapshot.lock().expect("disclosure sync poisoned");
            snapshot.running = false;
            snapshot.status = if snapshot.failures > 0 || snapshot.needs_review > 0 {
                "completed_with_gaps".into()
            } else {
                "completed".into()
            };
            snapshot.phase = if snapshot.status == "completed" {
                "正式披露增量同步完成".into()
            } else {
                "同步完成，未核验原文或来源失败保持为透明缺口".into()
            };
            snapshot.progress = 100;
            snapshot.current_item.clear();
            snapshot.estimated_remaining_seconds = Some(0);
            snapshot.updated_at = now_secs();
            push_log(
                &mut snapshot,
                "任务结束：镜像记录仍标记为待正式原文核验，不会提升结论置信度。".into(),
            );
            drop(snapshot);
            *sync.cancel.lock().expect("disclosure cancel poisoned") = None;
        });
        Ok(DisclosureSyncStart {
            started: true,
            job_id,
            estimated_seconds,
            note: "任务已转入 Engine 后台；可切换页面、轮询进度或安全取消。".into(),
        })
    }

    pub fn status(&self) -> DisclosureSyncSnapshot {
        self.inner
            .snapshot
            .lock()
            .expect("disclosure sync poisoned")
            .clone()
    }

    pub fn cancel(&self) -> bool {
        if !self
            .inner
            .snapshot
            .lock()
            .expect("disclosure sync poisoned")
            .running
        {
            return false;
        }
        let token = self
            .inner
            .cancel
            .lock()
            .expect("disclosure cancel poisoned")
            .clone();
        if let Some(token) = &token {
            token.cancel();
            let mut snapshot = self
                .inner
                .snapshot
                .lock()
                .expect("disclosure sync poisoned");
            snapshot.phase = "正在安全停止后台同步".into();
            snapshot.current_item = "当前网络请求结束后停止；已归档数据会保留".into();
            snapshot.updated_at = now_secs();
        }
        token.is_some()
    }
}

pub async fn provider_health(storage: Storage) -> Result<Vec<ProviderHealth>, String> {
    let store = DisclosureStore::new(storage.clone());
    store
        .seed_provider_catalog()
        .await
        .map_err(|error| error.to_string())?;
    let catalog = official_provider_catalog();
    storage
        .run(move |connection| {
            let mut output = Vec::new();
            for definition in catalog {
                let runtime = connection
                    .query_row(
                        "SELECT enabled,last_attempt_at,last_success_at,consecutive_failures,retry_after,last_error
                         FROM disclosure_provider_state WHERE provider_id=?1",
                        [definition.provider_id],
                        |row| {
                            Ok((
                                row.get::<_, i64>(0)? != 0,
                                row.get(1)?,
                                row.get(2)?,
                                row.get::<_, u32>(3)?,
                                row.get(4)?,
                                row.get(5)?,
                            ))
                        },
                    )
                    .optional()?;
                let (runtime_enabled, last_attempt, last_success, failures, retry_after, runtime_error) =
                    runtime.unwrap_or((false, None, None, 0, None, Some("NOT VERIFIED：来源状态未初始化".into())));
                let implemented = IMPLEMENTED_PROVIDERS.contains(&definition.provider_id);
                output.push(ProviderHealth {
                    provider_id: definition.provider_id.into(),
                    provider_name: definition.name.into(),
                    authority: definition.authority.token().into(),
                    authority_name: definition.authority.chinese_name().into(),
                    enabled: runtime_enabled && implemented,
                    public_index_url: definition.public_index_url.into(),
                    target_latency_secs: definition.target_latency_secs,
                    rate_limit_per_minute: definition.rate_limit_per_minute,
                    last_attempt_at: last_attempt,
                    last_success_at: last_success,
                    consecutive_failures: failures,
                    retry_after,
                    last_error: if implemented {
                        runtime_error
                    } else {
                        Some("NOT VERIFIED：当前 Engine 尚未实现该官方直连采集器；巨潮法定索引单独显示".into())
                    },
                    note: definition.note.into(),
                });
            }
            Ok(output)
        })
        .await
        .map_err(|error| error.to_string())
}

fn normalize_security_code(raw: Option<String>) -> Result<Option<String>, String> {
    let Some(raw) = raw else { return Ok(None) };
    let value = raw.trim();
    if value.is_empty() {
        return Ok(None);
    }
    Symbol::new(value)
        .map(|symbol| Some(symbol.code().to_string()))
        .map_err(|error| error.to_string())
}

fn market_for_code(code: &str) -> &'static str {
    if code.starts_with('6') {
        "SSE"
    } else if code.starts_with('8') || code.starts_with('4') || code.starts_with("92") {
        "BSE"
    } else {
        "SZSE"
    }
}

fn cninfo_columns(code: Option<&str>) -> Vec<&'static str> {
    match code {
        Some(value) if value.starts_with('6') => vec!["sse"],
        Some(value)
            if value.starts_with('8') || value.starts_with('4') || value.starts_with("92") =>
        {
            vec!["bse"]
        }
        Some(_) => vec!["szse"],
        None => vec!["sse", "szse", "bse"],
    }
}

async fn sync_cninfo(
    market: &Arc<MarketData>,
    storage: &Storage,
    store: &DisclosureStore,
    sync: &Arc<DisclosureSyncState>,
    token: &CancellationToken,
    code: Option<&str>,
    days: u32,
    max_pages: u32,
) {
    let end = astock_core::time::now_china().date_naive();
    let begin = end - chrono::Duration::days(i64::from(days));
    let date_range = format!("{begin}~{end}");
    let provider = CninfoDisclosureProvider::new(market.http.clone());
    let verifier = SourceVerifier::new(storage.clone());
    let columns = cninfo_columns(code);
    let total_slots = (columns.len() as u32).saturating_mul(max_pages).max(1);
    let mut slot = 0_u32;
    let mut provider_failed = false;
    for column in columns {
        let mut page_number = 1_u32;
        loop {
            if token.is_cancelled() {
                return;
            }
            slot += 1;
            update(
                sync,
                5 + ((slot * 45 / total_slots) as u8).min(45),
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
                    provider_failed = true;
                    let _ = store
                        .record_provider_failure("cninfo", &error.to_string())
                        .await;
                    record_gap(
                        sync,
                        format!("巨潮 {column} 第 {page_number} 页失败，已记录来源退避：{error}"),
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
                    10 + ((slot * 40 / total_slots) as u8).min(40),
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
                        match detail.version {
                            Some(version) => (
                                Some(version.source_version_id),
                                Some(version.content_hash),
                                "parsed".into(),
                                detail.document.failure_message,
                                (!text.is_empty()).then_some(text),
                                page_count,
                            ),
                            None => (
                                None,
                                None,
                                detail
                                    .document
                                    .failure_kind
                                    .unwrap_or(detail.document.access_status),
                                detail.document.failure_message,
                                (!text.is_empty()).then_some(text),
                                page_count,
                            ),
                        }
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
                let security_code = row.security_code.clone();
                let input = DisclosureInput {
                    provider_id: "cninfo".into(),
                    provider_name: "巨潮资讯".into(),
                    authority: ProviderAuthority::Exchange,
                    entry_kind: "statutory_index".into(),
                    upstream_id: Some(row.announcement_id),
                    original_url: row.pdf_url.clone(),
                    title: row.title.clone(),
                    published_at: row.announcement_time_ms.map(|value| value / 1_000),
                    publication_precision: "millisecond".into(),
                    first_seen_at: now_secs(),
                    latency_ms: None,
                    securities: (!security_code.is_empty())
                        .then(|| DisclosureSecurity {
                            code: security_code.clone(),
                            name: row.security_name,
                            market: market_for_code(&security_code).into(),
                        })
                        .into_iter()
                        .collect(),
                    attachments: vec![DisclosureAttachmentInput {
                        name: format!("{}.PDF", row.title),
                        original_url: row.pdf_url,
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
                record_ingest(store, sync, input, verified).await;
            }
            if page_number >= page.total_pages.min(max_pages) {
                break;
            }
            page_number += 1;
        }
    }
    if !provider_failed {
        if let Err(error) = store.record_provider_success("cninfo").await {
            record_gap(sync, format!("巨潮来源健康状态写入失败：{error}"));
        }
    }
}

async fn sync_eastmoney_mirror(
    market: &Arc<MarketData>,
    store: &DisclosureStore,
    sync: &Arc<DisclosureSyncState>,
    token: &CancellationToken,
    code: Option<&str>,
    days: u32,
    max_pages: u32,
) {
    if token.is_cancelled() {
        return;
    }
    update(
        sync,
        55,
        "正在读取补漏公告索引",
        "东方财富公告镜像（发现通道）",
        "等待上游返回分页索引",
    );
    let today = astock_core::time::now_china().date_naive();
    let begin = today - chrono::Duration::days(i64::from(days));
    let fetched = market
        .em_datacenter
        .notices(
            code,
            astock_market_data::providers::NoticeNode::All,
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
            record_gap(sync, format!("公告镜像索引读取失败：{error}"));
            return;
        }
    };
    let total = rows.len().max(1);
    {
        let mut snapshot = sync.snapshot.lock().expect("disclosure sync poisoned");
        snapshot.discovered = snapshot.discovered.saturating_add(rows.len() as u32);
        push_log(
            &mut snapshot,
            format!("镜像索引返回 {} 条公告，仅作正式来源补漏发现", rows.len()),
        );
    }
    for (index, row) in rows.into_iter().enumerate() {
        if token.is_cancelled() {
            return;
        }
        let published_at = row
            .notice_date
            .and_then(|date| date.and_hms_opt(0, 0, 0))
            .map(|time| time.and_utc().timestamp());
        let security_code = if row.stock_code.is_empty() {
            code.unwrap_or_default().to_string()
        } else {
            row.stock_code.clone()
        };
        let input = DisclosureInput {
            provider_id: "eastmoney_notice_mirror".into(),
            provider_name: "东方财富公告镜像".into(),
            authority: ProviderAuthority::MirrorDiscovery,
            entry_kind: "mirror_index".into(),
            upstream_id: (!row.art_code.is_empty()).then_some(row.art_code),
            original_url: row.url,
            title: row.title,
            published_at,
            publication_precision: "date".into(),
            first_seen_at: now_secs(),
            latency_ms: None,
            securities: (!security_code.is_empty())
                .then(|| DisclosureSecurity {
                    code: security_code.clone(),
                    name: row.stock_name,
                    market: market_for_code(&security_code).into(),
                })
                .into_iter()
                .collect(),
            attachments: Vec::new(),
            source_version_id: None,
            extracted_text: None,
            extraction_status: "index_only".into(),
            review_reason: Some("镜像仅用于发现；等待交易所、巨潮或公司 IR 正式原文归档".into()),
        };
        record_ingest(store, sync, input, false).await;
        let mut snapshot = sync.snapshot.lock().expect("disclosure sync poisoned");
        snapshot.progress = 60 + (((index + 1) * 35 / total) as u8);
        snapshot.estimated_remaining_seconds = Some(((total - index - 1) as u32).div_ceil(25));
        snapshot.updated_at = now_secs();
    }
    if let Err(error) = store
        .record_provider_success("eastmoney_notice_mirror")
        .await
    {
        record_gap(sync, format!("镜像来源健康状态写入失败：{error}"));
    }
}

async fn record_ingest(
    store: &DisclosureStore,
    sync: &Arc<DisclosureSyncState>,
    input: DisclosureInput,
    verified: bool,
) {
    let title = input.title.clone();
    let outcome = store.ingest(input).await;
    let mut snapshot = sync.snapshot.lock().expect("disclosure sync poisoned");
    snapshot.current_item = title.chars().take(80).collect();
    snapshot.normalized = snapshot.normalized.saturating_add(1);
    match outcome {
        Ok(outcome) => {
            if outcome.inserted {
                snapshot.inserted = snapshot.inserted.saturating_add(1);
            } else {
                snapshot.deduplicated = snapshot.deduplicated.saturating_add(1);
            }
            if verified {
                snapshot.primary_verified = snapshot.primary_verified.saturating_add(1);
            } else {
                snapshot.needs_review = snapshot.needs_review.saturating_add(1);
            }
        }
        Err(error) => {
            snapshot.failures = snapshot.failures.saturating_add(1);
            push_log(&mut snapshot, format!("披露入库失败：{title} · {error}"));
        }
    }
    snapshot.updated_at = now_secs();
}

fn update(sync: &Arc<DisclosureSyncState>, progress: u8, phase: &str, provider: &str, item: &str) {
    let mut snapshot = sync.snapshot.lock().expect("disclosure sync poisoned");
    let changed = snapshot.phase != phase
        || snapshot.current_provider != provider
        || snapshot.current_item != item;
    snapshot.progress = snapshot.progress.max(progress.min(99));
    snapshot.phase = phase.into();
    snapshot.current_provider = provider.into();
    snapshot.current_item = item.into();
    if changed {
        push_log(&mut snapshot, format!("{provider}：{phase} · {item}"));
    }
    snapshot.updated_at = now_secs();
}

fn record_gap(sync: &Arc<DisclosureSyncState>, message: String) {
    let mut snapshot = sync.snapshot.lock().expect("disclosure sync poisoned");
    snapshot.failures = snapshot.failures.saturating_add(1);
    push_log(&mut snapshot, message);
    snapshot.updated_at = now_secs();
}

fn push_log(snapshot: &mut DisclosureSyncSnapshot, message: String) {
    snapshot.recent_logs.push(message);
    if snapshot.recent_logs.len() > 100 {
        snapshot.recent_logs.remove(0);
    }
}

fn fail(sync: &Arc<DisclosureSyncState>, error: String) {
    let mut snapshot = sync.snapshot.lock().expect("disclosure sync poisoned");
    snapshot.running = false;
    snapshot.status = "failed".into();
    snapshot.phase = "正式披露同步失败，可复制诊断信息".into();
    snapshot.error = Some(error.clone());
    push_log(&mut snapshot, error);
    snapshot.updated_at = now_secs();
    drop(snapshot);
    *sync.cancel.lock().expect("disclosure cancel poisoned") = None;
}

fn finish_cancelled(sync: &Arc<DisclosureSyncState>) {
    let mut snapshot = sync.snapshot.lock().expect("disclosure sync poisoned");
    snapshot.running = false;
    snapshot.status = "cancelled".into();
    snapshot.phase = "已按用户要求停止正式披露同步".into();
    snapshot.estimated_remaining_seconds = None;
    push_log(&mut snapshot, "任务已停止，已归档的增量记录会保留。".into());
    snapshot.updated_at = now_secs();
    drop(snapshot);
    *sync.cancel.lock().expect("disclosure cancel poisoned") = None;
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

    #[test]
    fn security_scope_is_strict_and_market_mapping_handles_current_bse_codes() {
        assert_eq!(normalize_security_code(None).unwrap(), None);
        assert_eq!(
            normalize_security_code(Some(" 300308 ".into())).unwrap(),
            Some("300308".into())
        );
        assert!(normalize_security_code(Some("not-a-stock".into())).is_err());
        assert_eq!(market_for_code("600001"), "SSE");
        assert_eq!(market_for_code("300001"), "SZSE");
        assert_eq!(market_for_code("920001"), "BSE");
    }

    #[test]
    fn cancel_is_idempotent_and_never_changes_a_completed_job() {
        let service = DisclosureSyncService::default();
        assert!(!service.cancel());
        let token = CancellationToken::new();
        *service.inner.cancel.lock().unwrap() = Some(token.clone());
        service.inner.snapshot.lock().unwrap().running = true;
        assert!(service.cancel());
        assert!(token.is_cancelled());
        assert!(service.status().phase.contains("安全停止"));
    }

    #[tokio::test]
    async fn provider_health_never_claims_catalog_only_sources_are_ready() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(astock_storage::StorageConfig::with_base_dir(dir.path()))
            .expect("open isolated disclosure storage");
        let providers = provider_health(storage).await.unwrap();
        let sse = providers
            .iter()
            .find(|provider| provider.provider_id == "sse")
            .unwrap();
        let cninfo = providers
            .iter()
            .find(|provider| provider.provider_id == "cninfo")
            .unwrap();
        assert!(!sse.enabled);
        assert!(sse.last_error.as_deref().unwrap().contains("NOT VERIFIED"));
        assert!(cninfo.enabled);
    }
}
