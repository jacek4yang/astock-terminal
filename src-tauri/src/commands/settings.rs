//! Settings, MiniMax key and cache-maintenance commands
//! (docs/command-contract.md §设置与 MiniKey / §缓存维护).

use astock_minimax::{KeyStore, MinimaxClient, QuotaStatus, Region, SecretKey, ServiceInfo};
use astock_storage::CleanupPolicy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::State;

use crate::error::CmdError;
use crate::state::AppState;

/// Storage settings key for the custom data directory.
const DATA_DIR_SETTING: &str = "data_dir";
const AGENT_MODEL_ROUTING_SETTING: &str = "agent.model_routing.v1";

/// Capability-to-model mapping used by the main analyst and isolated
/// specialist reviewers. `auto` delegates selection to the live MiniMax
/// catalog; arbitrary provider model IDs remain accepted for future releases.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentModelRoutingSettings {
    pub coordinator_model: String,
    pub fast_model: String,
    pub deep_model: String,
    pub verifier_model: String,
    pub multi_agent_enabled: bool,
    pub max_parallel_agents: u8,
}

impl Default for AgentModelRoutingSettings {
    fn default() -> Self {
        Self {
            coordinator_model: "auto".into(),
            fast_model: "auto".into(),
            deep_model: "auto".into(),
            verifier_model: "auto".into(),
            multi_agent_enabled: true,
            max_parallel_agents: 3,
        }
    }
}

impl AgentModelRoutingSettings {
    fn normalize(mut self) -> Result<Self, CmdError> {
        for model in [
            &mut self.coordinator_model,
            &mut self.fast_model,
            &mut self.deep_model,
            &mut self.verifier_model,
        ] {
            *model = model.trim().to_string();
            if model.is_empty() {
                *model = "auto".to_string();
            }
            if model.len() > 128 || model.chars().any(char::is_control) {
                return Err(CmdError::new("invalid_param", "模型 ID 格式无效"));
            }
        }
        if !(1..=4).contains(&self.max_parallel_agents) {
            return Err(CmdError::new(
                "invalid_param",
                "并行专家数量必须在 1 到 4 之间",
            ));
        }
        Ok(self)
    }

    pub fn route_for(&self, research_mode: &str, reasoning_depth: &str) -> Option<String> {
        let chosen = if research_mode == "quick" {
            &self.fast_model
        } else if reasoning_depth == "maximum" || research_mode == "plan" {
            &self.deep_model
        } else {
            &self.coordinator_model
        };
        (chosen != "auto").then(|| chosen.clone())
    }

    pub fn verifier_model(&self, fallback: Option<&str>) -> Option<String> {
        if self.verifier_model != "auto" {
            Some(self.verifier_model.clone())
        } else {
            fallback.map(str::to_string)
        }
    }
}

pub async fn load_agent_model_routing(
    storage: &astock_storage::Storage,
) -> AgentModelRoutingSettings {
    storage
        .settings_get(AGENT_MODEL_ROUTING_SETTING)
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .and_then(|settings: AgentModelRoutingSettings| settings.normalize().ok())
        .unwrap_or_default()
}

/// `ServiceInfo` JSON without any key material: `{region, api_host, www_host}`.
fn service_info_json(info: &ServiceInfo) -> Value {
    json!({
        "region": match info.region {
            Region::Cn => "cn",
            Region::Intl => "intl",
        },
        "api_host": info.api_host,
        "www_host": info.www_host,
    })
}

/// `QuotaStatus` JSON. The engine type intentionally only implements
/// `Deserialize` (it mirrors an upstream payload), so the app layer projects
/// it here. `fetched_at` is epoch milliseconds.
fn quota_to_json(quota: &QuotaStatus) -> Value {
    let fetched_at_ms = quota
        .fetched_at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let models: Vec<Value> = quota
        .models
        .iter()
        .map(|m| {
            let mut obj = json!({
                "model_name": m.model_name,
                "start_time": m.start_time,
                "end_time": m.end_time,
                "remains_time": m.remains_time,
                "current_interval_total_count": m.current_interval_total_count,
                "current_interval_usage_count": m.current_interval_usage_count,
                "current_weekly_total_count": m.current_weekly_total_count,
                "current_weekly_usage_count": m.current_weekly_usage_count,
                "weekly_start_time": m.weekly_start_time,
                "weekly_end_time": m.weekly_end_time,
                "weekly_remains_time": m.weekly_remains_time,
                "current_interval_status": m.current_interval_status,
                "current_interval_remaining_percent": m.current_interval_remaining_percent,
                "current_weekly_status": m.current_weekly_status,
                "current_weekly_remaining_percent": m.current_weekly_remaining_percent,
            });
            // Forward any unmodeled upstream fields.
            if let (Value::Object(target), extra) = (&mut obj, &m.extra) {
                for (k, v) in extra {
                    target.insert(k.clone(), v.clone());
                }
            }
            obj
        })
        .collect();
    json!({ "models": models, "fetched_at": fetched_at_ms })
}

/// Store the MiniMax API key in the OS credential store (never echoed back),
/// then verify it against the service and cache a ready client.
/// On verification failure the key is removed again to avoid half-state.
#[tauri::command(rename_all = "snake_case")]
pub async fn minimax_set_key(state: State<'_, AppState>, key: String) -> Result<Value, CmdError> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(CmdError::new("invalid_param", "api key must not be empty"));
    }
    let store = KeyStore::new();
    let secret = SecretKey::new(trimmed.to_string());
    store.store_key(&secret)?;

    let client = MinimaxClient::new(secret);
    match client.detect_service().await {
        Ok(info) => {
            *state.minimax.write().await = Some(std::sync::Arc::new(client));
            Ok(service_info_json(&info))
        }
        Err(e) => {
            let _ = store.delete_key();
            *state.minimax.write().await = None;
            Err(CmdError::from(e))
        }
    }
}

/// MiniMax panel state: `{has_key, region?, api_host?, model?, quota?}`.
/// Region/model/quota are best-effort; a network failure drops the
/// individual field instead of failing the whole command.
#[tauri::command(rename_all = "snake_case")]
pub async fn minimax_status(state: State<'_, AppState>) -> Result<Value, CmdError> {
    if !state.ensure_minimax().await? {
        return Ok(json!({ "has_key": false }));
    }
    let guard = state.minimax.read().await;
    let client = guard.as_ref().expect("ensure_minimax just built it");
    let info = match client.detect_service().await {
        Ok(info) => Some(info),
        Err(e) => {
            tracing::warn!(error = %e, "minimax service detection failed");
            None
        }
    };
    let model = match client.selected_model().await {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::warn!(error = %e, "minimax model probe failed");
            None
        }
    };
    let quota = match client.quota().await {
        Ok(q) => Some(quota_to_json(&q)),
        Err(e) => {
            tracing::warn!(error = %e, "minimax quota fetch failed");
            None
        }
    };
    let available_models = match client.available_models().await {
        Ok(models) => Some(models),
        Err(e) => {
            tracing::warn!(error = %e, "minimax model discovery failed");
            None
        }
    };
    let model_routing = load_agent_model_routing(&state.storage).await;
    let mut out = json!({ "has_key": true });
    let obj = out.as_object_mut().expect("object");
    if let Some(info) = &info {
        obj.insert(
            "region".into(),
            match info.region {
                Region::Cn => "cn".into(),
                Region::Intl => "intl".into(),
            },
        );
        obj.insert("api_host".into(), info.api_host.clone().into());
    }
    if let Some(model) = model {
        obj.insert("model".into(), model.into());
    }
    if let Some(quota) = quota {
        obj.insert("quota".into(), quota);
    }
    if let Some(models) = available_models {
        obj.insert("available_models".into(), json!(models));
    }
    obj.insert("model_routing".into(), json!(model_routing));
    Ok(out)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn settings_get_agent_model_routing(
    state: State<'_, AppState>,
) -> Result<AgentModelRoutingSettings, CmdError> {
    Ok(load_agent_model_routing(&state.storage).await)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn settings_set_agent_model_routing(
    state: State<'_, AppState>,
    settings: AgentModelRoutingSettings,
) -> Result<AgentModelRoutingSettings, CmdError> {
    let settings = settings.normalize()?;
    let encoded = serde_json::to_string(&settings)
        .map_err(|error| CmdError::new("settings", error.to_string()))?;
    state
        .storage
        .settings_set(AGENT_MODEL_ROUTING_SETTING, &encoded)
        .await?;
    Ok(settings)
}

/// Current Token Plan quota for all models.
#[tauri::command(rename_all = "snake_case")]
pub async fn minimax_quota(state: State<'_, AppState>) -> Result<Value, CmdError> {
    if !state.ensure_minimax().await? {
        return Err(CmdError::new(
            "no_key",
            "no MiniMax api key stored; call minimax_set_key first",
        ));
    }
    let guard = state.minimax.read().await;
    let quota = guard
        .as_ref()
        .expect("ensure_minimax just built it")
        .quota()
        .await?;
    Ok(quota_to_json(&quota))
}

/// On-disk cache footprint by category, plus free space on the data volume.
#[tauri::command(rename_all = "snake_case")]
pub async fn cache_stats(state: State<'_, AppState>) -> Result<Value, CmdError> {
    let stats = state.storage.cache_stats().await?;
    Ok(json!({
        "kline_bytes": stats.kline_parquet_bytes,
        "sqlite_bytes": stats.sqlite_bytes,
        "tool_cache_bytes": stats.tool_cache_bytes,
        "chat_bytes": stats.chat_bytes,
        "total_bytes": stats.total_bytes(),
        "disk_free_bytes": state.storage.disk_free_bytes(),
    }))
}

/// Evict expired tool-cache rows and least-recently-used parquet files until
/// the total cache size is under `target_mb` MiB.
#[tauri::command(rename_all = "snake_case")]
pub async fn cache_cleanup(
    state: State<'_, AppState>,
    target_mb: u64,
) -> Result<CleanupResponse, CmdError> {
    let report = state
        .storage
        .cleanup(CleanupPolicy {
            target_total_bytes: target_mb.saturating_mul(1024 * 1024),
        })
        .await?;
    Ok(CleanupResponse {
        freed_bytes: report.bytes_freed,
        removed_files: report.parquet_files_deleted,
    })
}

/// `cache_cleanup` response: `{freed_bytes, removed_files}`.
#[derive(Debug, Serialize)]
pub struct CleanupResponse {
    /// Bytes freed by parquet eviction.
    pub freed_bytes: u64,
    /// Parquet files evicted.
    pub removed_files: u64,
}

/// The directory all local data lives in.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_data_dir(state: State<'_, AppState>) -> Result<String, CmdError> {
    Ok(state.storage.base_dir().to_string_lossy().into_owned())
}

/// `set_data_dir` response.
#[derive(Debug, Serialize)]
pub struct SetDataDirResponse {
    /// The persisted directory.
    pub data_dir: String,
    /// Human-readable note: the change applies after restart.
    pub message: String,
}

/// Persist a custom data directory. Takes effect after the app restarts
/// (the running instance keeps its current storage).
#[tauri::command(rename_all = "snake_case")]
pub async fn set_data_dir(
    state: State<'_, AppState>,
    path: String,
) -> Result<SetDataDirResponse, CmdError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(CmdError::new("invalid_param", "data dir must not be empty"));
    }
    state
        .storage
        .settings_set(DATA_DIR_SETTING, trimmed)
        .await?;
    Ok(SetDataDirResponse {
        data_dir: trimmed.to_string(),
        message: "数据目录已保存,重启应用后生效".into(),
    })
}

// ---------------------------------------------------------------------
// 可选数据源凭证与代理(docs/command-contract.md §设置 → 数据源凭证)
//
// 每个可选项映射到一对 (kv key, env var)。market-data / joinquant 的可选
// provider 全部从进程环境变量读配置,所以这里:启动时从 kv 注入 env(见
// `load_provider_credentials_into_env`,在构造 MarketData 之前调用),
// settings_set 时同步 set_var/remove_var 即时生效。
// 敏感值(token/key/密码)在 kv 里用 base64 包一层 —— 仅防 shoulder-surfing,
// 不是加密;任何能读到 meta.db 的人都能还原。凭证本体绝不写日志、绝不回传
// 前端(status 只回布尔)。
// ---------------------------------------------------------------------

/// kv key prefix shared by all provider credential entries.
const PROVIDER_KV_PREFIX: &str = "provider.";

/// One configurable provider credential / proxy slot.
struct ProviderSlot {
    /// kv table key (also the `ProviderStatus` field identity).
    kv_key: &'static str,
    /// Process environment variable the provider reads.
    env_key: &'static str,
    /// Secret values are base64-wrapped in kv (obfuscation, not encryption).
    secret: bool,
}

const SLOT_TUSHARE_TOKEN: ProviderSlot = ProviderSlot {
    kv_key: "provider.tushare_token",
    env_key: "TUSHARE_TOKEN",
    secret: true,
};
const SLOT_IWENCAI_KEY: ProviderSlot = ProviderSlot {
    kv_key: "provider.iwencai_key",
    env_key: "IWENCAI_KEY",
    secret: true,
};
const SLOT_JQ_USER: ProviderSlot = ProviderSlot {
    kv_key: "provider.jq_user",
    env_key: "JQ_USER",
    secret: false,
};
const SLOT_JQ_PWD: ProviderSlot = ProviderSlot {
    kv_key: "provider.jq_pwd",
    env_key: "JQ_PWD",
    secret: true,
};
const SLOT_SOCKS5: ProviderSlot = ProviderSlot {
    kv_key: "provider.socks5",
    env_key: "ASTOCK_SOCKS5",
    secret: false,
};

const PROVIDER_SLOTS: &[&ProviderSlot] = &[
    &SLOT_TUSHARE_TOKEN,
    &SLOT_IWENCAI_KEY,
    &SLOT_JQ_USER,
    &SLOT_JQ_PWD,
    &SLOT_SOCKS5,
];

/// Base64-wrap a secret for at-rest storage (obfuscation only, see above).
fn obfuscate(value: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(value)
}

/// Undo [`obfuscate`]; a value that is not valid base64/UTF-8 is treated as
/// legacy plaintext and returned as-is.
fn deobfuscate(stored: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(stored)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| stored.to_string())
}

/// Trim an incoming value; `None` and empty/whitespace strings both mean
/// "clear this slot".
fn normalize(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

/// Persist (or clear) one slot and apply it to the process environment.
/// The raw value never leaves this function and is never logged.
async fn apply_slot(
    storage: &astock_storage::Storage,
    slot: &ProviderSlot,
    value: Option<String>,
) -> Result<(), CmdError> {
    match normalize(value) {
        Some(v) => {
            let stored = if slot.secret {
                obfuscate(&v)
            } else {
                v.clone()
            };
            storage.kv_set(slot.kv_key, &stored).await?;
            std::env::set_var(slot.env_key, v);
        }
        None => {
            storage.kv_delete(slot.kv_key).await?;
            std::env::remove_var(slot.env_key);
        }
    }
    Ok(())
}

/// Read persisted provider credentials from kv into the process environment.
/// Must run before the market-data stack is constructed (providers capture
/// env vars at build time). Presence is logged, values never are.
pub async fn load_provider_credentials_into_env(storage: &astock_storage::Storage) {
    let mut loaded: Vec<&str> = Vec::new();
    for slot in PROVIDER_SLOTS {
        match storage.kv_get(slot.kv_key).await {
            Ok(Some(entry)) => {
                let value = if slot.secret {
                    deobfuscate(&entry.value)
                } else {
                    entry.value
                };
                if !value.is_empty() {
                    std::env::set_var(slot.env_key, value);
                    loaded.push(slot.env_key);
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(kv_key = slot.kv_key, error = %e, "provider credential read failed (skipped)");
            }
        }
    }
    if !loaded.is_empty() {
        tracing::info!(providers = ?loaded, "provider credentials loaded from settings into env");
    }
}

/// Which optional providers have credentials configured. Booleans only —
/// the credential values themselves are never returned to the frontend.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProviderStatus {
    /// Tushare pro token (`TUSHARE_TOKEN`).
    pub tushare_token: bool,
    /// iwencai OpenAPI key (`IWENCAI_KEY`).
    pub iwencai_key: bool,
    /// JoinQuant username (`JQ_USER`).
    pub jq_user: bool,
    /// JoinQuant password (`JQ_PWD`).
    pub jq_pwd: bool,
    /// SOCKS5 proxy for foreign endpoints (`ASTOCK_SOCKS5`).
    pub socks5: bool,
}

/// Build the status from the kv keys currently present.
async fn provider_status(storage: &astock_storage::Storage) -> Result<ProviderStatus, CmdError> {
    let entries = storage.kv_list_prefix(PROVIDER_KV_PREFIX).await?;
    let has = |kv_key: &str| entries.iter().any(|e| e.key == kv_key);
    Ok(ProviderStatus {
        tushare_token: has(SLOT_TUSHARE_TOKEN.kv_key),
        iwencai_key: has(SLOT_IWENCAI_KEY.kv_key),
        jq_user: has(SLOT_JQ_USER.kv_key),
        jq_pwd: has(SLOT_JQ_PWD.kv_key),
        socks5: has(SLOT_SOCKS5.kv_key),
    })
}

/// `settings_set_provider_credentials` response.
#[derive(Debug, Serialize)]
pub struct SetProviderCredentialsResponse {
    /// Status after the update (booleans only).
    pub status: ProviderStatus,
    /// Human-readable note about reconnection semantics.
    pub message: String,
}

/// Persist optional-provider credentials and the SOCKS5 proxy, then apply
/// them to the process environment immediately. Every field is optional:
/// `None` (or an empty string) clears that item. Values are never logged.
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_set_provider_credentials(
    state: State<'_, AppState>,
    tushare_token: Option<String>,
    iwencai_key: Option<String>,
    jq_user: Option<String>,
    jq_pwd: Option<String>,
    socks5: Option<String>,
) -> Result<SetProviderCredentialsResponse, CmdError> {
    let storage = &state.storage;
    apply_slot(storage, &SLOT_TUSHARE_TOKEN, tushare_token).await?;
    apply_slot(storage, &SLOT_IWENCAI_KEY, iwencai_key).await?;
    apply_slot(storage, &SLOT_JQ_USER, jq_user).await?;
    apply_slot(storage, &SLOT_JQ_PWD, jq_pwd).await?;
    apply_slot(storage, &SLOT_SOCKS5, socks5).await?;
    Ok(SetProviderCredentialsResponse {
        status: provider_status(storage).await?,
        message: "已保存并写入进程环境变量;部分 provider 需重启后重新建连".into(),
    })
}

/// Report which optional providers have credentials configured (booleans
/// only; credential values are never returned).
#[tauri::command(rename_all = "snake_case")]
pub async fn settings_get_provider_status(
    state: State<'_, AppState>,
) -> Result<ProviderStatus, CmdError> {
    provider_status(&state.storage).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_minimax::quota::ModelQuota;

    /// Restore an env var to its prior state on drop (tests mutate the
    /// process environment).
    struct EnvGuard(&'static str, Option<String>);

    impl EnvGuard {
        fn capture(key: &'static str) -> Self {
            EnvGuard(key, std::env::var(key).ok())
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.1 {
                Some(v) => std::env::set_var(self.0, v),
                None => std::env::remove_var(self.0),
            }
        }
    }

    fn test_storage() -> (tempfile::TempDir, astock_storage::Storage) {
        let dir = tempfile::tempdir().unwrap();
        let storage =
            astock_storage::Storage::open(astock_storage::StorageConfig::with_base_dir(dir.path()))
                .unwrap();
        (dir, storage)
    }

    #[test]
    fn obfuscate_roundtrip_and_legacy_passthrough() {
        let wrapped = obfuscate("dummy-password-123!@#");
        assert_ne!(wrapped, "dummy-password-123!@#");
        assert_eq!(deobfuscate(&wrapped), "dummy-password-123!@#");
        // Legacy plaintext values pass through untouched.
        assert_eq!(deobfuscate("plain text !!"), "plain text !!");
    }

    #[test]
    fn normalize_clears_on_none_and_empty() {
        assert_eq!(normalize(None), None);
        assert_eq!(normalize(Some("".into())), None);
        assert_eq!(normalize(Some("   ".into())), None);
        assert_eq!(normalize(Some("  tok  ".into())), Some("tok".into()));
    }

    #[test]
    fn model_routing_accepts_future_ids_and_routes_by_difficulty() {
        let settings = AgentModelRoutingSettings {
            coordinator_model: " MiniMax-M4-future ".into(),
            fast_model: "MiniMax-M3-highspeed".into(),
            deep_model: "MiniMax-M4-deep".into(),
            verifier_model: "auto".into(),
            multi_agent_enabled: true,
            max_parallel_agents: 4,
        }
        .normalize()
        .unwrap();
        assert_eq!(
            settings.route_for("quick", "deep").as_deref(),
            Some("MiniMax-M3-highspeed")
        );
        assert_eq!(
            settings.route_for("deep", "maximum").as_deref(),
            Some("MiniMax-M4-deep")
        );
        assert_eq!(
            settings.route_for("deep", "deep").as_deref(),
            Some("MiniMax-M4-future")
        );
        assert_eq!(
            settings
                .verifier_model(Some("MiniMax-M4-future"))
                .as_deref(),
            Some("MiniMax-M4-future")
        );
    }

    #[test]
    fn model_routing_rejects_unsafe_or_unbounded_values() {
        let settings = AgentModelRoutingSettings {
            max_parallel_agents: 5,
            ..Default::default()
        };
        assert!(settings.normalize().is_err());
        let settings = AgentModelRoutingSettings {
            deep_model: "bad\nmodel".into(),
            ..Default::default()
        };
        assert!(settings.normalize().is_err());
    }

    #[tokio::test]
    async fn apply_slot_persists_and_applies_env() {
        let (_dir, storage) = test_storage();
        let _guard = EnvGuard::capture(SLOT_JQ_PWD.env_key);

        apply_slot(&storage, &SLOT_JQ_PWD, Some("s3cret".into()))
            .await
            .unwrap();
        // kv holds the base64-wrapped value, env holds the raw value.
        let entry = storage.kv_get(SLOT_JQ_PWD.kv_key).await.unwrap().unwrap();
        assert_eq!(entry.value, obfuscate("s3cret"));
        assert_eq!(std::env::var(SLOT_JQ_PWD.env_key).unwrap(), "s3cret");
        assert!(provider_status(&storage).await.unwrap().jq_pwd);

        // Empty string clears: kv row deleted, env var removed.
        apply_slot(&storage, &SLOT_JQ_PWD, Some(String::new()))
            .await
            .unwrap();
        assert!(storage.kv_get(SLOT_JQ_PWD.kv_key).await.unwrap().is_none());
        assert!(std::env::var(SLOT_JQ_PWD.env_key).is_err());
        assert!(!provider_status(&storage).await.unwrap().jq_pwd);
    }

    #[tokio::test]
    async fn load_credentials_into_env_decodes_secrets() {
        let (_dir, storage) = test_storage();
        let _guards: Vec<EnvGuard> = PROVIDER_SLOTS
            .iter()
            .map(|slot| EnvGuard::capture(slot.env_key))
            .collect();
        for slot in PROVIDER_SLOTS {
            std::env::remove_var(slot.env_key);
        }

        storage
            .kv_set(SLOT_TUSHARE_TOKEN.kv_key, &obfuscate("ts-token"))
            .await
            .unwrap();
        storage
            .kv_set(SLOT_SOCKS5.kv_key, "127.0.0.1:1080")
            .await
            .unwrap();

        load_provider_credentials_into_env(&storage).await;
        assert_eq!(
            std::env::var(SLOT_TUSHARE_TOKEN.env_key).unwrap(),
            "ts-token"
        );
        assert_eq!(
            std::env::var(SLOT_SOCKS5.env_key).unwrap(),
            "127.0.0.1:1080"
        );
        assert!(std::env::var(SLOT_JQ_USER.env_key).is_err());
    }

    #[test]
    fn quota_json_shape() {
        let quota = QuotaStatus {
            models: vec![ModelQuota {
                model_name: "MiniMax-M2.5".into(),
                current_interval_remaining_percent: Some(87.5),
                ..Default::default()
            }],
            fetched_at: std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_000_000),
        };
        let json = quota_to_json(&quota);
        assert_eq!(json["fetched_at"], 1_700_000_000_000_u64);
        assert_eq!(json["models"][0]["model_name"], "MiniMax-M2.5");
        assert_eq!(
            json["models"][0]["current_interval_remaining_percent"],
            87.5
        );
        // Unknown optional fields serialize as null, never missing.
        assert!(json["models"][0]["weekly_end_time"].is_null());
    }
}
