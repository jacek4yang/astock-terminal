//! Model fallback chain with probing.
//!
//! The app never hardcodes one permanent model: [`ModelCatalog`] holds an
//! ordered preference chain and [`ModelCatalog::probe_models`] walks it with a
//! minimal completion (`max_tokens: 1`) until a model answers successfully.
//! The winner is cached for the lifetime of the catalog.

use crate::chat::ChatResponse;
use crate::error::MinimaxError;
use crate::http::{map_http_error, Http};
use crate::key::SecretKey;

/// Default preference chain, best first.
pub const DEFAULT_CHAIN: &[&str] = &["MiniMax-M2.5", "MiniMax-M2", "MiniMax-M1"];

/// An ordered model fallback chain plus a cached probe result.
pub struct ModelCatalog {
    chain: Vec<String>,
    cached: tokio::sync::OnceCell<String>,
}

impl Default for ModelCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelCatalog {
    /// Catalog with [`DEFAULT_CHAIN`].
    pub fn new() -> Self {
        Self::with_chain(DEFAULT_CHAIN.iter().map(|s| s.to_string()).collect())
    }

    /// Catalog with a custom preference chain (best first).
    pub fn with_chain(chain: Vec<String>) -> Self {
        Self {
            chain,
            cached: tokio::sync::OnceCell::new(),
        }
    }

    /// The preference chain.
    pub fn chain(&self) -> &[String] {
        &self.chain
    }

    /// The probed model, if probing already succeeded.
    pub fn selected(&self) -> Option<&str> {
        self.cached.get().map(String::as_str)
    }

    /// Probe the chain against `api_host` and return the first model that
    /// accepts a minimal completion. The result is cached.
    ///
    /// Auth failures abort immediately (the key, not the model, is the
    /// problem); anything else falls through to the next candidate.
    pub async fn probe_models(
        &self,
        http: &dyn Http,
        api_host: &str,
        key: &SecretKey,
    ) -> Result<String, MinimaxError> {
        self.cached
            .get_or_try_init(|| self.probe_uncached(http, api_host, key))
            .await
            .cloned()
    }

    async fn probe_uncached(
        &self,
        http: &dyn Http,
        api_host: &str,
        key: &SecretKey,
    ) -> Result<String, MinimaxError> {
        let url = format!("{api_host}/v1/chat/completions");
        let mut last_err: Option<MinimaxError> = None;
        for model in &self.chain {
            let body = serde_json::json!({
                "model": model,
                "messages": [{"role": "user", "content": "ping"}],
                "max_tokens": 1,
            });
            let result = self.probe_one(http, &url, key, &body).await;
            match result {
                Ok(()) => {
                    tracing::info!(%model, "selected MiniMax model");
                    return Ok(model.clone());
                }
                Err(e @ MinimaxError::Auth(_)) => return Err(e),
                Err(e) => {
                    tracing::debug!(%model, error = %e, "model probe failed; trying next fallback");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| MinimaxError::Api {
            code: 0,
            msg: "model fallback chain is empty".to_string(),
        }))
    }

    async fn probe_one(
        &self,
        http: &dyn Http,
        url: &str,
        key: &SecretKey,
        body: &serde_json::Value,
    ) -> Result<(), MinimaxError> {
        let resp = http.post(url, Some(key), Some(body)).await?;
        if resp.status != 200 {
            return Err(map_http_error(resp.status, &resp.headers, &resp.body));
        }
        let parsed: ChatResponse = serde_json::from_slice(&resp.body)
            .map_err(|e| MinimaxError::Parse(format!("model probe: {e}")))?;
        parsed.check_base_resp()
    }
}
