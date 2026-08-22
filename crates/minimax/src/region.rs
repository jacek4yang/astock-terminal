//! Region and endpoint detection.
//!
//! MiniMax operates two independent services: mainland China
//! (`www.minimaxi.com` / `api.minimaxi.com`) and international
//! (`www.minimax.io` / `api.minimax.io`). A key is only valid on one of them.
//! [`RegionDetector`] probes both `token_plan/remains` endpoints in parallel
//! and picks the service where `base_resp.status_code == 0`. The key string
//! itself is never inspected to guess the region.

use std::sync::Arc;

use crate::error::MinimaxError;
use crate::http::{map_base_resp, map_http_error, Http};
use crate::key::SecretKey;

/// Which MiniMax service a key belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Region {
    /// Mainland China service (`minimaxi.com`).
    Cn,
    /// International service (`minimax.io`).
    Intl,
}

/// Resolved endpoints for one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInfo {
    /// Detected region.
    pub region: Region,
    /// Base URL of the web/quota host, e.g. `https://www.minimaxi.com`.
    pub www_host: String,
    /// Base URL of the API host, e.g. `https://api.minimaxi.com`.
    pub api_host: String,
}

impl Region {
    /// Well-known endpoints for this region's production service.
    pub fn service_info(self) -> ServiceInfo {
        match self {
            Region::Cn => ServiceInfo {
                region: Region::Cn,
                www_host: "https://www.minimaxi.com".to_string(),
                api_host: "https://api.minimaxi.com".to_string(),
            },
            Region::Intl => ServiceInfo {
                region: Region::Intl,
                www_host: "https://www.minimax.io".to_string(),
                api_host: "https://api.minimax.io".to_string(),
            },
        }
    }
}

/// Probes both MiniMax services and caches the winning one.
///
/// Detection result is cached for the lifetime of the detector; construct a
/// new detector if the key changes.
pub struct RegionDetector {
    http: Arc<dyn Http>,
    /// `(region, www_host, api_host)` candidates, probed in parallel; the
    /// first candidate in list order wins when several accept the key.
    hosts: Vec<(Region, String, String)>,
    cache: tokio::sync::OnceCell<ServiceInfo>,
}

impl RegionDetector {
    /// Detector for the two production services (China preferred on ties,
    /// which should not happen in practice).
    pub fn new(http: Arc<dyn Http>) -> Self {
        Self {
            http,
            hosts: [Region::Cn, Region::Intl]
                .into_iter()
                .map(|r| {
                    let info = r.service_info();
                    (r, info.www_host, info.api_host)
                })
                .collect(),
            cache: tokio::sync::OnceCell::new(),
        }
    }

    /// Detector with custom candidate hosts. Used by tests to point at a
    /// local stub server.
    pub fn with_hosts(http: Arc<dyn Http>, hosts: Vec<(Region, String, String)>) -> Self {
        Self {
            http,
            hosts,
            cache: tokio::sync::OnceCell::new(),
        }
    }

    /// Probe both services in parallel and return the one accepting the key.
    ///
    /// Errors with [`MinimaxError::Auth`] when both services reject the key
    /// (`status_code == 2049`), or with the first transport/API error when at
    /// least one service failed for a non-auth reason.
    pub async fn detect_service(&self, key: &SecretKey) -> Result<ServiceInfo, MinimaxError> {
        self.cache
            .get_or_try_init(|| self.detect_uncached(key))
            .await
            .cloned()
    }

    async fn detect_uncached(&self, key: &SecretKey) -> Result<ServiceInfo, MinimaxError> {
        let results = futures::future::join_all(self.hosts.iter().map(|(region, www, api)| {
            let key = key.clone();
            async move {
                match self.probe(www, &key).await {
                    Ok(()) => Ok(ServiceInfo {
                        region: *region,
                        www_host: www.clone(),
                        api_host: api.clone(),
                    }),
                    Err(e) => Err((*region, e)),
                }
            }
        }))
        .await;

        let mut errors = Vec::new();
        for result in results {
            match result {
                Ok(info) => {
                    tracing::info!(region = ?info.region, api_host = %info.api_host, "detected MiniMax service");
                    return Ok(info);
                }
                Err((region, e)) => {
                    tracing::debug!(?region, error = %e, "service probe failed");
                    errors.push(e);
                }
            }
        }

        // All candidates failed. Auth everywhere means a bad key; otherwise
        // surface the first non-auth error (network trouble, outage, ...).
        let first_non_auth = errors.iter().find(|e| !matches!(e, MinimaxError::Auth(_)));
        match first_non_auth {
            Some(e) => Err(MinimaxError::Network(format!(
                "no MiniMax service reachable: {e}"
            ))),
            None => Err(MinimaxError::Auth(
                "invalid api key on all MiniMax services".to_string(),
            )),
        }
    }

    /// One probe against a `token_plan/remains` endpoint, retried once on
    /// transport errors.
    async fn probe(&self, www_host: &str, key: &SecretKey) -> Result<(), MinimaxError> {
        let url = format!("{www_host}/v1/token_plan/remains");
        let mut attempt = 0;
        loop {
            attempt += 1;
            let resp = match self.http.get(&url, Some(key)).await {
                Ok(resp) => resp,
                Err(e @ MinimaxError::Network(_)) if attempt == 1 => {
                    tracing::debug!(%url, "probe failed with network error; retrying once");
                    let _ = e;
                    continue;
                }
                Err(e) => return Err(e),
            };
            if resp.status != 200 {
                return Err(map_http_error(resp.status, &resp.headers, &resp.body));
            }
            let body: serde_json::Value = serde_json::from_slice(&resp.body)
                .map_err(|e| MinimaxError::Parse(format!("token_plan/remains: {e}")))?;
            let code = body
                .get("base_resp")
                .and_then(|b| b.get("status_code"))
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(-1);
            if code == 0 {
                return Ok(());
            }
            let msg = body
                .get("base_resp")
                .and_then(|b| b.get("status_msg"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error");
            return Err(map_base_resp(code, msg));
        }
    }
}
