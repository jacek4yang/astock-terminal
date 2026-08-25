//! Resilient HTTP layer: one shared `reqwest` client, UA rotation, adaptive
//! per-host rate limiting, and host-pool failover.

use crate::proxy::{ProxyConfig, ProxyRoute};
use astock_core::DataError;
use dashmap::DashMap;
use parking_lot::Mutex;
use reqwest::header::{HeaderMap, HeaderValue};
use std::net::ToSocketAddrs;
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

/// Request timeout, matching the legacy 8s budget.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Pause between host-pool attempts, matching the legacy 0.3s.
pub const RETRY_PAUSE: Duration = Duration::from_millis(300);

const MIN_PENALTY: Duration = Duration::from_millis(100);
const MAX_PENALTY: Duration = Duration::from_secs(30);
/// Healthy per-host spacing shared by every provider and background task.
/// 75ms keeps throughput high while preventing a 15-way market scan from
/// issuing a same-host burst all at once.
const BASE_INTERVAL: Duration = Duration::from_millis(75);

struct HostState {
    /// Current adaptive delay applied before each request to this host.
    delay: Duration,
    /// No request may start before this instant.
    next_allowed: Option<Instant>,
}

impl Default for HostState {
    fn default() -> Self {
        Self {
            delay: BASE_INTERVAL,
            next_allowed: None,
        }
    }
}

/// Body plus the response Content-Type (needed for Tencent WAF sniffing).
pub struct TextResponse {
    /// Response body, decoded as UTF-8 (lossy).
    pub body: String,
    /// Original response bytes for explicitly non-UTF-8 providers such as
    /// Tencent's GBK quote endpoint.
    pub body_bytes: Vec<u8>,
    /// Raw Content-Type header value, if present.
    pub content_type: Option<String>,
    /// Successful HTTP status retained for ingestion provenance.
    pub status: u16,
    /// Cache validators retained for incremental document archives.
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

fn compatible_get_url(
    url: &str,
    params: &[(String, String)],
    upstream: &str,
) -> Result<String, DataError> {
    let mut request_url = reqwest::Url::parse(url).map_err(|error| DataError::Parse {
        upstream: upstream.to_string(),
        message: error.to_string(),
    })?;
    // Calling `query_pairs_mut()` on a URL without query parameters adds a
    // trailing `?`. Some quote endpoints treat that character as part of the
    // path-level symbol (for example `sz300308?`) and return an empty response
    // under the wrong identity.
    if !params.is_empty() {
        request_url.query_pairs_mut().extend_pairs(
            params
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );
    }
    // EastMoney's historical endpoint aborts the response when commas in its
    // fields grammar arrive as `%2C`. Comma is a legal query sub-delimiter.
    Ok(request_url.as_str().replace("%2C", ","))
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
    /// Build a direct shared client with no credential-bearing proxy.
    pub fn new() -> Self {
        Self::with_proxy(ProxyConfig::direct())
    }

    /// Build the shared client with an explicit proxy policy.
    ///
    /// EastMoney hosts use current system DNS. The legacy fixed-CDN override
    /// became harmful when that delay node stopped serving historical K
    /// lines; host pools now provide failover without changing TLS identity.
    /// Domestic hosts always use the direct client; only `foreign_hosts` URLs
    /// use the proxied one (see [`crate::proxy`]).
    pub fn with_proxy(proxy: ProxyConfig) -> Self {
        // Several domestic market-data CDNs publish an IPv6 address whose
        // TLS edge closes before sending an HTTP response, while the IPv4
        // edge is healthy. The first release is Windows-only, so bind the
        // direct market-data client to IPv4 and keep proxy-routed foreign
        // traffic on the system default address family.
        let client = Self::build_client(None, cfg!(windows));
        let proxied = match proxy.proxy_url() {
            Some(url) => match reqwest::Proxy::all(&url) {
                Ok(p) => {
                    debug!("credential-backed socks5 proxy configured for foreign hosts");
                    Some(Self::build_client(Some(p), false))
                }
                Err(_) => {
                    warn!("invalid credential-backed socks5 proxy URL; all traffic direct");
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

    fn build_client(proxy: Option<reqwest::Proxy>, force_ipv4: bool) -> reqwest::Client {
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
        let mut builder = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(REQUEST_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .pool_max_idle_per_host(8);
        #[cfg(windows)]
        {
            builder = builder.use_native_tls();
        }
        if force_ipv4 {
            builder = builder.local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
            // Binding 0.0.0.0 does not remove IPv6 DNS candidates from
            // reqwest on Windows. Resolve the affected CDN names at process
            // start and keep only the current A records. This is deliberately
            // not a fixed IP override: every Engine restart re-resolves DNS.
            for domain in [
                "push2his.eastmoney.com",
                "90.push2his.eastmoney.com",
                "82.push2his.eastmoney.com",
            ] {
                let addresses = (domain, 443)
                    .to_socket_addrs()
                    .map(|rows| rows.filter(|address| address.is_ipv4()).collect::<Vec<_>>())
                    .unwrap_or_default();
                if !addresses.is_empty() {
                    builder = builder.resolve_to_addrs(domain, &addresses);
                }
            }
        }
        if let Some(p) = proxy {
            builder = builder.proxy(p);
        } else {
            // `reqwest` otherwise inherits HTTP(S)_PROXY from the desktop
            // process and silently routes domestic providers through it,
            // contradicting ProxyConfig's deny-by-default policy.
            builder = builder.no_proxy();
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

    /// Whether a credential-backed proxy was accepted at startup. The proxy
    /// address itself is deliberately not exposed to diagnostics or IPC.
    pub fn proxy_configured(&self) -> bool {
        self.proxied.is_some()
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
            let mut state = entry.lock();
            let now = Instant::now();
            let reserved_at = state.next_allowed.map_or(now, |instant| instant.max(now));
            let wait = reserved_at.saturating_duration_since(now);
            let delay = state.delay;
            // Reserve atomically while holding the host lock. Otherwise a
            // burst of concurrent scan futures can all observe the same free
            // slot before any one of them updates `next_allowed`.
            state.next_allowed = Some(reserved_at + delay);
            (wait, delay)
        };
        if !wait.is_zero() {
            debug!(host, ?wait, ?delay, "adaptive throttle sleeping");
            tokio::time::sleep(wait).await;
        }
    }

    /// Record a successful request: decay the penalty by 25%, never below
    /// the healthy baseline spacing.
    pub fn on_success(&self, host: &str) {
        let entry = self.hosts.entry(host.to_string()).or_default();
        let mut s = entry.lock();
        s.delay = s.delay.mul_f64(0.75).max(BASE_INTERVAL);
    }

    /// Record a failure (timeout, 429, 5xx, WAF): double the penalty,
    /// floored at 100ms and capped at 30s.
    pub fn on_failure(&self, host: &str) {
        let entry = self.hosts.entry(host.to_string()).or_default();
        let mut s = entry.lock();
        s.delay = (s.delay.max(MIN_PENALTY) * 2).min(MAX_PENALTY);
        debug!(host, delay = ?s.delay, "adaptive penalty increased");
    }

    /// Current adaptive delay for a host (baseline if unseen), for diagnostics.
    pub fn current_delay(&self, host: &str) -> Duration {
        self.hosts
            .get(host)
            .map(|s| s.lock().delay)
            .unwrap_or(BASE_INTERVAL)
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
        self.get_text_with_headers(url, &[], params).await
    }

    /// GET with explicit per-request headers. This is required by official
    /// APIs such as SEC EDGAR, whose Fair Access policy requires a declared
    /// application/contact User-Agent instead of a generic browser identity.
    pub async fn get_text_with_headers(
        &self,
        url: &str,
        headers: &[(String, String)],
        params: &[(String, String)],
    ) -> Result<TextResponse, DataError> {
        let host = Self::host_key(url);
        self.throttle(&host).await;

        let compatible_url = compatible_get_url(url, params, &host)?;
        let mut request = self.client_for(url).get(compatible_url);
        // The EastMoney history cluster currently aborts responses carrying
        // a browser UA on some system-proxy paths. Other market endpoints do
        // need a realistic UA, so keep this compatibility exception narrow.
        if !host.ends_with("push2his.eastmoney.com") {
            request = request.header(reqwest::header::USER_AGENT, self.current_ua());
        }
        for (key, value) in headers {
            request = request.header(key, value);
        }
        let result = request.send().await;

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
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let last_modified = resp
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        match resp.bytes().await {
            Ok(bytes) => {
                self.on_success(&host);
                Ok(TextResponse {
                    body: String::from_utf8_lossy(&bytes).into_owned(),
                    body_bytes: bytes.to_vec(),
                    content_type,
                    status: status.as_u16(),
                    etag,
                    last_modified,
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

    pub async fn get_json_with_headers(
        &self,
        url: &str,
        headers: &[(String, String)],
        params: &[(String, String)],
    ) -> Result<serde_json::Value, DataError> {
        let resp = self.get_text_with_headers(url, headers, params).await?;
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

    /// POST an `application/x-www-form-urlencoded` body and decode JSON.
    /// Used by public disclosure indexes such as CNInfo. It shares the same
    /// per-host adaptive limiter and failure bookkeeping as every market
    /// request, so a disclosure refresh cannot bypass global rate controls.
    pub async fn post_form_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        form: &[(String, String)],
    ) -> Result<serde_json::Value, DataError> {
        let host = Self::host_key(url);
        self.throttle(&host).await;
        let mut request = self
            .client_for(url)
            .post(url)
            .header(reqwest::header::USER_AGENT, self.current_ua())
            .form(form);
        for (key, value) in headers {
            request = request.header(key, value);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                self.on_failure(&host);
                if error.is_timeout() {
                    return Err(DataError::Timeout(host));
                }
                return Err(DataError::Network {
                    host,
                    message: error.to_string(),
                });
            }
        };
        let status = response.status();
        if status.as_u16() == 429 {
            self.on_failure(&host);
            return Err(DataError::RateLimited(host));
        }
        if !status.is_success() {
            self.on_failure(&host);
            return Err(DataError::Network {
                host,
                message: format!("HTTP {status}"),
            });
        }
        match response.json::<serde_json::Value>().await {
            Ok(value) => {
                self.on_success(&host);
                Ok(value)
            }
            Err(error) => {
                self.on_failure(&host);
                Err(DataError::Parse {
                    upstream: host,
                    message: error.to_string(),
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
    fn empty_params_do_not_mutate_path_level_quote_identity() {
        assert_eq!(
            compatible_get_url("https://hq.sinajs.cn/list=sz300308", &[], "sina").unwrap(),
            "https://hq.sinajs.cn/list=sz300308"
        );
        assert_eq!(
            compatible_get_url(
                "https://example.com/kline",
                &[("fields".to_string(), "f1,f2".to_string())],
                "example"
            )
            .unwrap(),
            "https://example.com/kline?fields=f1,f2"
        );
    }

    #[test]
    fn limiter_backs_off_and_recovers() {
        let http = HttpClient::new();
        let host = "example.com";
        assert_eq!(http.current_delay(host), BASE_INTERVAL);
        http.on_failure(host);
        let d1 = http.current_delay(host);
        assert!(d1 >= MIN_PENALTY);
        http.on_failure(host);
        assert!(http.current_delay(host) > d1);
        for _ in 0..20 {
            http.on_success(host);
        }
        assert!(http.current_delay(host) < d1);
        assert_eq!(http.current_delay(host), BASE_INTERVAL);
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
