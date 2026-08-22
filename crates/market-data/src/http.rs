//! Resilient HTTP layer: one shared `reqwest` client, UA rotation, per-client
//! DNS override, adaptive per-host rate limiting, and host-pool failover.

use crate::proxy::{ProxyConfig, ProxyRoute};
use astock_core::DataError;
use dashmap::DashMap;
use parking_lot::Mutex;
use reqwest::header::{HeaderMap, HeaderValue};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// User-Agent pool, copied verbatim from the legacy Python source.
pub const UA_POOL: [&str; 3] = [
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:120.0) Gecko/20100101",
];

/// EastMoney public API token, attached to every EM endpoint.
pub const EM_TOKEN: &str = "fa5fd1943c7b386f172d6893dbfba10b";

/// CDN IP that answers for `push2his`/`push2` (legacy DNS monkey-patch target).
pub const PUSH2DELAY_IP: &str = "117.184.45.167";

/// Request timeout, matching the legacy 8s budget.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Pause between host-pool attempts, matching the legacy 0.3s.
pub const RETRY_PAUSE: Duration = Duration::from_millis(300);

const MIN_PENALTY: Duration = Duration::from_millis(100);
const MAX_PENALTY: Duration = Duration::from_secs(30);

#[derive(Default)]
struct HostState {
    /// Current adaptive delay applied before each request to this host.
    delay: Duration,
    /// No request may start before this instant.
    next_allowed: Option<Instant>,
}

/// Body plus the response Content-Type (needed for Tencent WAF sniffing).
pub struct TextResponse {
    /// Response body, decoded as UTF-8 (lossy).
    pub body: String,
    /// Raw Content-Type header value, if present.
    pub content_type: Option<String>,
}

/// Shared HTTP client with adaptive throttling and host failover.
///
/// Cheap to clone (everything is behind `Arc`s); one instance should be built
/// per process and shared by all providers.
pub struct HttpClient {
    client: reqwest::Client,
    /// SOCKS5-routed client, built only when a proxy is configured.
    proxied: Option<reqwest::Client>,
    proxy: ProxyConfig,
    ua_idx: Arc<AtomicUsize>,
    hosts: Arc<DashMap<String, Mutex<HostState>>>,
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient {
    /// Build the shared client with proxy routing from `ASTOCK_SOCKS5`.
    pub fn new() -> Self {
        Self::with_proxy(ProxyConfig::from_env())
    }

    /// Build the shared client with an explicit proxy policy.
    ///
    /// `push2his.eastmoney.com` and `push2.eastmoney.com` are pinned to the
    /// push2delay CDN IP — the per-client equivalent of the legacy
    /// `getaddrinfo` monkey-patch (SNI/Host headers stay on the original
    /// domain). Domestic hosts always use the direct client; only
    /// `foreign_hosts` URLs use the proxied one (see [`crate::proxy`]).
    pub fn with_proxy(proxy: ProxyConfig) -> Self {
        let client = Self::build_client(None);
        let proxied = match proxy.proxy_url() {
            Some(url) => match reqwest::Proxy::all(&url) {
                Ok(p) => {
                    debug!(proxy = %url, "socks5 proxy configured for foreign hosts");
                    Some(Self::build_client(Some(p)))
                }
                Err(e) => {
                    warn!(proxy = %url, error = %e, "invalid socks5 proxy URL; all traffic direct");
                    None
                }
            },
            None => None,
        };
        HttpClient {
            client,
            proxied,
            proxy,
            ua_idx: Arc::new(AtomicUsize::new(0)),
            hosts: Arc::new(DashMap::new()),
        }
    }

    fn build_client(proxy: Option<reqwest::Proxy>) -> reqwest::Client {
        let mut headers = HeaderMap::new();
        headers.insert(reqwest::header::ACCEPT, HeaderValue::from_static("*/*"));
        headers.insert(
            reqwest::header::ACCEPT_LANGUAGE,
            HeaderValue::from_static("zh-CN,zh;q=0.9"),
        );
        headers.insert(
            reqwest::header::REFERER,
            HeaderValue::from_static("https://quote.eastmoney.com/"),
        );
        headers.insert(
            reqwest::header::CONNECTION,
            HeaderValue::from_static("keep-alive"),
        );

        let dns: SocketAddr = format!("{PUSH2DELAY_IP}:443")
            .parse()
            .expect("static IP parses");
        let mut builder = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(REQUEST_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .pool_max_idle_per_host(8)
            .resolve("push2his.eastmoney.com", dns)
            .resolve("push2.eastmoney.com", dns);
        if let Some(p) = proxy {
            builder = builder.proxy(p);
        }
        builder.build().expect("reqwest client builds")
    }

    /// Pick the direct or proxied client for `url` per the routing policy.
    fn client_for(&self, url: &str) -> &reqwest::Client {
        match (self.proxy.route(url), &self.proxied) {
            (ProxyRoute::Socks5, Some(p)) => p,
            _ => &self.client,
        }
    }

    /// The active proxy policy (for diagnostics / the settings page).
    pub fn proxy_config(&self) -> &ProxyConfig {
        &self.proxy
    }

    fn current_ua(&self) -> &'static str {
        UA_POOL[self.ua_idx.load(Ordering::Relaxed) % UA_POOL.len()]
    }

    /// Advance to the next UA in the pool (called after failed attempts).
    pub fn rotate_ua(&self) {
        self.ua_idx.fetch_add(1, Ordering::Relaxed);
    }

    /// Wait out the host's current adaptive delay, then reserve the next slot.
    async fn throttle(&self, host: &str) {
        let (wait, delay) = {
            let entry = self.hosts.entry(host.to_string()).or_default();
            let s = entry.lock();
            (
                s.next_allowed
                    .map(|t| t.saturating_duration_since(Instant::now()))
                    .unwrap_or(Duration::ZERO),
                s.delay,
            )
        };
        if !wait.is_zero() {
            debug!(host, ?wait, "adaptive throttle sleeping");
            tokio::time::sleep(wait).await;
        }
        let entry = self.hosts.entry(host.to_string()).or_default();
        entry.lock().next_allowed = Some(Instant::now() + delay);
    }

    /// Record a successful request: decay the penalty by 25%.
    pub fn on_success(&self, host: &str) {
        let entry = self.hosts.entry(host.to_string()).or_default();
        let mut s = entry.lock();
        s.delay = s.delay.mul_f64(0.75);
    }

    /// Record a failure (timeout, 429, 5xx, WAF): double the penalty,
    /// floored at 100ms and capped at 30s.
    pub fn on_failure(&self, host: &str) {
        let entry = self.hosts.entry(host.to_string()).or_default();
        let mut s = entry.lock();
        s.delay = (s.delay.max(MIN_PENALTY) * 2).min(MAX_PENALTY);
        debug!(host, delay = ?s.delay, "adaptive penalty increased");
    }

    /// Current adaptive delay for a host (0 if unseen), for diagnostics.
    pub fn current_delay(&self, host: &str) -> Duration {
        self.hosts
            .get(host)
            .map(|s| s.lock().delay)
            .unwrap_or(Duration::ZERO)
    }

    fn host_key(url: &str) -> String {
        reqwest::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| url.to_string())
    }

    /// Single GET with throttling, UA header, and outcome bookkeeping.
    pub async fn get_text(
        &self,
        url: &str,
        params: &[(String, String)],
    ) -> Result<TextResponse, DataError> {
        let host = Self::host_key(url);
        self.throttle(&host).await;

        let result = self
            .client_for(url)
            .get(url)
            .header(reqwest::header::USER_AGENT, self.current_ua())
            .query(params)
            .send()
            .await;

        let resp = match result {
            Ok(r) => r,
            Err(e) => {
                self.on_failure(&host);
                if e.is_timeout() {
                    return Err(DataError::Timeout(host));
                }
                return Err(DataError::Network {
                    host,
                    message: e.to_string(),
                });
            }
        };

        let status = resp.status();
        if status.as_u16() == 429 {
            self.on_failure(&host);
            return Err(DataError::RateLimited(host));
        }
        // HTTP 501 is the Tencent WAF's challenge status (`501page.html`):
        // none of our endpoints legitimately answer 501, so surface it as a
        // WAF block (lets the provider circuit breaker trip immediately)
        // rather than a generic server error.
        if status.as_u16() == 501 {
            self.on_failure(&host);
            return Err(DataError::WafBlocked(format!("{host} (HTTP 501)")));
        }
        if status.is_server_error() {
            self.on_failure(&host);
            return Err(DataError::Network {
                host,
                message: format!("HTTP {status}"),
            });
        }
        if !status.is_success() {
            return Err(DataError::Network {
                host,
                message: format!("HTTP {status}"),
            });
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        match resp.bytes().await {
            Ok(bytes) => {
                self.on_success(&host);
                Ok(TextResponse {
                    body: String::from_utf8_lossy(&bytes).into_owned(),
                    content_type,
                })
            }
            Err(e) => {
                self.on_failure(&host);
                Err(DataError::Network {
                    host,
                    message: e.to_string(),
                })
            }
        }
    }

    /// GET expecting a JSON body.
    pub async fn get_json(
        &self,
        url: &str,
        params: &[(String, String)],
    ) -> Result<serde_json::Value, DataError> {
        let resp = self.get_text(url, params).await?;
        serde_json::from_str(&resp.body).map_err(|e| DataError::Parse {
            upstream: Self::host_key(url),
            message: e.to_string(),
        })
    }

    /// POST a JSON body, expecting a JSON body back (Tushare pro, iwencai
    /// OpenAPI). `headers` carries per-request headers such as
    /// `Authorization`; the adaptive throttle / failure bookkeeping / proxy
    /// routing are identical to GET.
    pub async fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, DataError> {
        let host = Self::host_key(url);
        self.throttle(&host).await;

        let mut req = self
            .client_for(url)
            .post(url)
            .header(reqwest::header::USER_AGENT, self.current_ua())
            .json(body);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                self.on_failure(&host);
                if e.is_timeout() {
                    return Err(DataError::Timeout(host));
                }
                return Err(DataError::Network {
                    host,
                    message: e.to_string(),
                });
            }
        };

        let status = resp.status();
        if status.as_u16() == 429 {
            self.on_failure(&host);
            return Err(DataError::RateLimited(host));
        }
        if status.is_server_error() {
            self.on_failure(&host);
            return Err(DataError::Network {
                host,
                message: format!("HTTP {status}"),
            });
        }
        if !status.is_success() {
            return Err(DataError::Network {
                host,
                message: format!("HTTP {status}"),
            });
        }

        match resp.json::<serde_json::Value>().await {
            Ok(v) => {
                self.on_success(&host);
                Ok(v)
            }
            Err(e) => {
                self.on_failure(&host);
                Err(DataError::Parse {
                    upstream: host,
                    message: e.to_string(),
                })
            }
        }
    }

    /// Try each host in `hosts` in order until one yields a usable payload.
    ///
    /// A payload is usable when `data` is non-null and — for endpoints
    /// returning klines — `data.klines` is not an empty list (the push2delay
    /// kline quirk from the legacy code). Between attempts the UA is rotated
    /// and a short pause is inserted. If no host yields a usable payload but
    /// at least one answered with parseable JSON, that payload is returned
    /// anyway — callers distinguish "empty off-market" (fine) from "no data"
    /// (error); only a total transport/parse failure across the pool yields
    /// [`DataError::AllFailed`].
    pub async fn get_json_pool(
        &self,
        path: &str,
        params: &[(String, String)],
        hosts: &[&str],
        op: &'static str,
    ) -> Result<serde_json::Value, DataError> {
        let mut failures: Vec<String> = Vec::new();
        let mut last_parseable: Option<serde_json::Value> = None;
        for (attempt, host) in hosts.iter().enumerate() {
            if attempt > 0 {
                self.rotate_ua();
                tokio::time::sleep(RETRY_PAUSE).await;
            }
            let url = format!("{host}{path}");
            match self.get_json(&url, params).await {
                Ok(value) => match payload_usable(&value) {
                    Ok(()) => return Ok(value),
                    Err(reason) => {
                        debug!(host, %reason, "host returned unusable payload, trying next");
                        failures.push(format!("{host}: {reason}"));
                        last_parseable = Some(value);
                    }
                },
                Err(e) => {
                    debug!(host, error = %e, "host request failed, trying next");
                    failures.push(format!("{host}: {e}"));
                }
            }
        }
        if let Some(value) = last_parseable {
            debug!(
                op,
                "pool exhausted; returning last parseable (empty) payload"
            );
            return Ok(value);
        }
        warn!(op, failures = %failures.join("; "), "all hosts failed");
        Err(DataError::AllFailed {
            op,
            details: failures.join("; "),
        })
    }
}

/// Legacy `_get_json_eastmoney` acceptance rule: `data` must be non-null, and
/// if `data` carries a `klines` field it must be non-empty.
fn payload_usable(value: &serde_json::Value) -> Result<(), String> {
    let data = value.get("data").ok_or("missing `data` field")?;
    if data.is_null() {
        return Err("`data` is null".to_string());
    }
    if let Some(klines) = data.get("klines") {
        if klines.as_array().is_some_and(|k| k.is_empty()) {
            return Err("empty `klines`".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_rule_matches_legacy() {
        assert!(payload_usable(&serde_json::json!({"data": {"klines": ["a"]}})).is_ok());
        assert!(payload_usable(&serde_json::json!({"data": {"f43": 1}})).is_ok());
        assert!(payload_usable(&serde_json::json!({"data": {"klines": []}})).is_err());
        assert!(payload_usable(&serde_json::json!({"data": null})).is_err());
        assert!(payload_usable(&serde_json::json!({"rc": 0})).is_err());
    }

    #[test]
    fn limiter_backs_off_and_recovers() {
        let http = HttpClient::new();
        let host = "example.com";
        assert_eq!(http.current_delay(host), Duration::ZERO);
        http.on_failure(host);
        let d1 = http.current_delay(host);
        assert!(d1 >= MIN_PENALTY);
        http.on_failure(host);
        assert!(http.current_delay(host) > d1);
        for _ in 0..20 {
            http.on_success(host);
        }
        assert!(http.current_delay(host) < d1);
    }

    #[test]
    fn ua_rotates_through_pool() {
        let http = HttpClient::new();
        let first = http.current_ua();
        http.rotate_ua();
        assert_ne!(http.current_ua(), first);
        http.rotate_ua();
        http.rotate_ua();
        assert_eq!(http.current_ua(), first);
    }
}
