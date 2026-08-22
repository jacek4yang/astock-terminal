//! Secure API key handling: OS keyring storage and log-safe redaction.
//!
//! Key material is always wrapped in [`SecretKey`], whose `Debug` and
//! `Display` implementations are redacted (`sk-...XXXX`). `SecretKey`
//! deliberately does not implement `serde::Deserialize`, and its `Serialize`
//! implementation always fails, so a key can never leak into a serialized
//! payload or config file by accident.

use std::fmt;

use keyring::Entry;
use serde::Serializer;

use crate::error::MinimaxError;

/// Default keyring service name.
pub const KEYRING_SERVICE: &str = "astock-terminal";
/// Default keyring account name.
pub const KEYRING_ACCOUNT: &str = "minimax-api-key";

/// An API key that cannot be logged or serialized in plain text.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretKey(String);

impl SecretKey {
    /// Wrap raw key material.
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// Expose the raw key. Only use this to build `Authorization` headers.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Mask every occurrence of this key inside `text`.
    pub fn redact(&self, text: &str) -> String {
        redact(text, &self.0)
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretKey({})", mask_key(&self.0))
    }
}

impl fmt::Display for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", mask_key(&self.0))
    }
}

impl serde::Serialize for SecretKey {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Err(serde::ser::Error::custom(
            "SecretKey must never be serialized; refusing to leak key material",
        ))
    }
}

/// Render a key as `sk-...XXXX` (scheme prefix plus last 4 characters only).
pub fn mask_key(key: &str) -> String {
    let n = key.chars().count();
    if n <= 4 {
        return "****".to_string();
    }
    let last4: String = key.chars().skip(n - 4).collect();
    match key.split('-').next() {
        Some(prefix) if !prefix.is_empty() && prefix.len() <= 8 && prefix.len() + 1 < n => {
            format!("{prefix}-...{last4}")
        }
        _ => format!("...{last4}"),
    }
}

/// Replace every occurrence of `key` inside `text` with its masked form.
///
/// Keys shorter than 8 characters are not masked, to avoid mangling logs with
/// a too-generic pattern; such a key would be rejected by the service anyway.
pub fn redact(text: &str, key: &str) -> String {
    if key.len() < 8 || !text.contains(key) {
        return text.to_string();
    }
    text.replace(key, &mask_key(key))
}

/// CRUD for the MiniMax API key in the OS credential store
/// (Windows Credential Manager on this target).
pub struct KeyStore {
    service: String,
    account: String,
}

impl Default for KeyStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyStore {
    /// The store used by the app: service `astock-terminal`, account
    /// `minimax-api-key`.
    pub fn new() -> Self {
        Self {
            service: KEYRING_SERVICE.to_string(),
            account: KEYRING_ACCOUNT.to_string(),
        }
    }

    /// A store with custom coordinates. Useful for tests so they never touch
    /// the real account slot.
    pub fn with_service(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }

    fn entry(&self) -> Result<Entry, MinimaxError> {
        Entry::new(&self.service, &self.account)
            .map_err(|e| MinimaxError::KeyStore(e.to_string()))
    }

    /// Persist the key, overwriting any existing value.
    pub fn store_key(&self, key: &SecretKey) -> Result<(), MinimaxError> {
        self.entry()?
            .set_password(key.expose())
            .map_err(|e| MinimaxError::KeyStore(e.to_string()))?;
        tracing::debug!(service = %self.service, "stored MiniMax API key in OS keyring");
        Ok(())
    }

    /// Load the key, or `Ok(None)` when no key has been stored.
    pub fn load_key(&self) -> Result<Option<SecretKey>, MinimaxError> {
        match self.entry()?.get_password() {
            Ok(raw) => {
                tracing::debug!(service = %self.service, "loaded MiniMax API key from OS keyring");
                Ok(Some(SecretKey::new(raw)))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(MinimaxError::KeyStore(e.to_string())),
        }
    }

    /// Remove the key. Succeeds silently when no key is stored.
    pub fn delete_key(&self) -> Result<(), MinimaxError> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {
                tracing::debug!(service = %self.service, "deleted MiniMax API key from OS keyring");
                Ok(())
            }
            Err(e) => Err(MinimaxError::KeyStore(e.to_string())),
        }
    }
}
