//! Provider credentials backed exclusively by the operating-system keyring.

use astock_minimax::{KeyStore, SecretKey};
use astock_storage::Storage;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{credential_store, storage, Engine, ServiceError};

const SERVICE: &str = "astock-terminal";

#[derive(Debug, Clone, Copy)]
struct Slot {
    id: &'static str,
    account: &'static str,
    env_key: &'static str,
    legacy_key: &'static str,
    captured_at_startup: bool,
}

const TUSHARE: Slot = Slot {
    id: "tushare",
    account: "provider-tushare-token",
    env_key: "TUSHARE_TOKEN",
    legacy_key: "provider.tushare_token",
    captured_at_startup: true,
};
const IWENCAI: Slot = Slot {
    id: "iwencai",
    account: "provider-iwencai-key",
    env_key: "IWENCAI_KEY",
    legacy_key: "provider.iwencai_key",
    captured_at_startup: true,
};
const SEC_EDGAR: Slot = Slot {
    id: "sec_edgar",
    account: "provider-sec-user-agent",
    env_key: "ASTOCK_SEC_USER_AGENT",
    legacy_key: "provider.sec_user_agent",
    captured_at_startup: false,
};
const SOCKS5: Slot = Slot {
    id: "socks5",
    account: "provider-socks5",
    env_key: "ASTOCK_SOCKS5",
    legacy_key: "provider.socks5",
    captured_at_startup: true,
};
const OPTIONAL_SLOTS: &[Slot] = &[TUSHARE, IWENCAI, SEC_EDGAR, SOCKS5];

#[derive(Debug, Clone, Default)]
pub(super) struct BootStatus {
    pub socks5: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderCredentialPayload {
    pub provider: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProviderIdPayload {
    pub provider: String,
}

fn store(slot: Slot) -> KeyStore {
    KeyStore::with_service(SERVICE, slot.account)
}

fn slot(provider: &str) -> Result<Slot, ServiceError> {
    OPTIONAL_SLOTS
        .iter()
        .copied()
        .find(|slot| slot.id == provider)
        .ok_or_else(|| {
            ServiceError::new(
                "invalid_provider",
                "provider must be tushare, iwencai, sec_edgar or socks5",
                false,
            )
        })
}

fn load(slot: Slot) -> Result<Option<SecretKey>, ServiceError> {
    store(slot).load_key().map_err(credential_store)
}

fn configured(slot: Slot) -> Result<bool, ServiceError> {
    Ok(load(slot)?.is_some_and(|value| !value.expose().trim().is_empty()))
}

fn validate(slot: Slot, value: String) -> Result<String, ServiceError> {
    let value = value.trim().to_string();
    if value.is_empty() || value.len() > 4_096 || value.chars().any(char::is_control) {
        return Err(ServiceError::new(
            "invalid_credential",
            "credential is empty, too long or contains control characters",
            false,
        ));
    }
    match slot.id {
        "tushare" | "iwencai" if value.len() < 8 => Err(ServiceError::new(
            "invalid_credential",
            "provider token is shorter than the supported contract",
            false,
        )),
        "sec_edgar" if !value.contains('@') || value.len() < 8 => Err(ServiceError::new(
            "invalid_credential",
            "SEC Fair Access User-Agent must identify the application and a contact email",
            false,
        )),
        "socks5" => normalize_socks5(&value),
        _ => Ok(value),
    }
}

fn normalize_socks5(value: &str) -> Result<String, ServiceError> {
    let candidate = if value.contains("://") {
        value.to_string()
    } else {
        format!("socks5h://{value}")
    };
    let parsed = url::Url::parse(&candidate)
        .map_err(|_| ServiceError::new("invalid_proxy", "SOCKS5 proxy URL is invalid", false))?;
    if !matches!(parsed.scheme(), "socks5" | "socks5h")
        || parsed.host_str().is_none()
        || parsed.port().is_none()
    {
        return Err(ServiceError::new(
            "invalid_proxy",
            "SOCKS5 proxy requires socks5/socks5h, a host and an explicit port",
            false,
        ));
    }
    Ok(candidate)
}

/// Import legacy SQLite/base64 records. A legacy row is deleted only after
/// Credential Manager read-back succeeds. Values are never logged.
pub(super) async fn migrate_legacy(storage_ref: &Storage) -> Result<(), ServiceError> {
    for slot in OPTIONAL_SLOTS {
        let current = load(*slot)?;
        let legacy = storage_ref.kv_get(slot.legacy_key).await.map_err(storage)?;
        let Some(legacy) = legacy else { continue };
        if current.is_none() {
            let decoded = if matches!(slot.id, "tushare" | "iwencai") {
                base64::engine::general_purpose::STANDARD
                    .decode(legacy.value.as_bytes())
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .unwrap_or(legacy.value)
            } else {
                legacy.value
            };
            let normalized = validate(*slot, decoded)?;
            store(*slot)
                .store_key(&SecretKey::new(normalized.clone()))
                .map_err(credential_store)?;
            let verified = load(*slot)?.is_some_and(|value| value.expose() == normalized);
            if !verified {
                return Err(ServiceError::new(
                    "credential_migration_verification_failed",
                    format!("Credential Manager read-back failed for {}", slot.id),
                    false,
                ));
            }
        }
        if configured(*slot)? {
            storage_ref
                .kv_delete(slot.legacy_key)
                .await
                .map_err(storage)?;
        }
    }
    migrate_legacy_joinquant(storage_ref).await
}

async fn migrate_legacy_joinquant(storage_ref: &Storage) -> Result<(), ServiceError> {
    let username_store = KeyStore::with_service(SERVICE, "joinquant-username");
    let password_store = KeyStore::with_service(SERVICE, "joinquant-password");
    let existing_user = username_store.load_key().map_err(credential_store)?;
    let existing_password = password_store.load_key().map_err(credential_store)?;
    let legacy_user = storage_ref
        .kv_get("provider.jq_user")
        .await
        .map_err(storage)?;
    let legacy_password = storage_ref
        .kv_get("provider.jq_pwd")
        .await
        .map_err(storage)?;
    if existing_user.is_none() && existing_password.is_none() {
        if let (Some(user), Some(password)) = (&legacy_user, &legacy_password) {
            let decoded_password = base64::engine::general_purpose::STANDARD
                .decode(password.value.as_bytes())
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .unwrap_or_else(|| password.value.clone());
            let username = user.value.trim();
            if username.is_empty() || decoded_password.is_empty() {
                return Ok(());
            }
            username_store
                .store_key(&SecretKey::new(username))
                .map_err(credential_store)?;
            if let Err(error) = password_store.store_key(&SecretKey::new(&decoded_password)) {
                let _ = username_store.delete_key();
                return Err(credential_store(error));
            }
        }
    }
    let verified = username_store
        .load_key()
        .map_err(credential_store)?
        .is_some()
        && password_store
            .load_key()
            .map_err(credential_store)?
            .is_some();
    if verified {
        if legacy_user.is_some() {
            storage_ref
                .kv_delete("provider.jq_user")
                .await
                .map_err(storage)?;
        }
        if legacy_password.is_some() {
            storage_ref
                .kv_delete("provider.jq_pwd")
                .await
                .map_err(storage)?;
        }
    }
    Ok(())
}

pub(super) fn load_into_environment() -> Result<BootStatus, ServiceError> {
    let mut boot = BootStatus::default();
    for slot in OPTIONAL_SLOTS {
        match load(*slot)? {
            Some(value) if !value.expose().trim().is_empty() => {
                std::env::set_var(slot.env_key, value.expose());
                if slot.id == "socks5" {
                    boot.socks5 = true;
                }
            }
            _ => std::env::remove_var(slot.env_key),
        }
    }
    Ok(boot)
}

pub(super) fn status(engine: &Engine) -> Result<Value, ServiceError> {
    let row = |slot: Slot, active: bool| -> Result<Value, ServiceError> {
        Ok(json!({
            "configured": configured(slot)?,
            "active": active,
            "restart_required": slot.captured_at_startup,
        }))
    };
    Ok(json!({
        "tushare": row(TUSHARE, engine.market.tushare.available())?,
        "iwencai": row(IWENCAI, engine.market.iwencai.available())?,
        "sec_edgar": row(SEC_EDGAR, std::env::var(SEC_EDGAR.env_key).is_ok())?,
        "socks5": row(SOCKS5, engine.provider_boot.socks5)?,
    }))
}

pub(super) fn set(payload: ProviderCredentialPayload) -> Result<Value, ServiceError> {
    let slot = slot(payload.provider.trim())?;
    let value = validate(slot, payload.value)?;
    let key = SecretKey::new(value.clone());
    store(slot).store_key(&key).map_err(credential_store)?;
    let read_back = load(slot)?;
    if !read_back.is_some_and(|stored| stored.expose() == value) {
        let _ = store(slot).delete_key();
        return Err(ServiceError::new(
            "credential_verification_failed",
            "Credential Manager read-back failed; the credential was not retained",
            false,
        ));
    }
    std::env::set_var(slot.env_key, key.expose());
    Ok(json!({
        "stored": true,
        "provider": slot.id,
        "restart_required": slot.captured_at_startup,
        "message": if slot.captured_at_startup {
            "凭据已安全保存；重启桌面应用后该数据源生效"
        } else {
            "凭据已安全保存并在当前进程生效"
        }
    }))
}

pub(super) fn delete(payload: ProviderIdPayload) -> Result<Value, ServiceError> {
    let slot = slot(payload.provider.trim())?;
    store(slot).delete_key().map_err(credential_store)?;
    std::env::remove_var(slot.env_key);
    Ok(json!({
        "deleted": true,
        "provider": slot.id,
        "restart_required": slot.captured_at_startup,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_requires_supported_scheme_host_and_port() {
        assert_eq!(
            normalize_socks5("127.0.0.1:1080").unwrap(),
            "socks5h://127.0.0.1:1080"
        );
        assert!(normalize_socks5("http://127.0.0.1:1080").is_err());
        assert!(normalize_socks5("socks5://127.0.0.1").is_err());
    }

    #[test]
    fn sec_identity_is_never_invented() {
        assert!(validate(SEC_EDGAR, "AStock Terminal".into()).is_err());
        assert!(validate(SEC_EDGAR, "AStock Terminal research@example.com".into()).is_ok());
    }
}
