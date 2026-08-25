//! SOCKS5 proxy routing for the HTTP layer.
//!
//! Policy: the application-configured SOCKS5 route never selects domestic
//! market-data platforms (Tencent/Sina/EastMoney/Xueqiu/THS/Tushare/iwencai).
//! The underlying HTTP stack may still honor an OS/process HTTP proxy chosen
//! by the user or managed environment. Only `foreign_hosts` can use the
//! explicit application SOCKS5 route. Production configuration is injected
//! from Windows Credential Manager by the Engine; it is never read from a
//! process environment variable.

/// Domestic platforms that must never be proxied (suffix match).
pub const DOMESTIC_HOSTS: &[&str] = &[
    "gtimg.cn",      // 腾讯行情 (qt./web.ifzq./proxy.finance.)
    "sina.com.cn",   // 新浪行情
    "eastmoney.com", // 东方财富 push2/datacenter
    "xueqiu.com",    // 雪球
    "10jqka.com.cn", // 同花顺
    "iwencai.com",   // 问财 (网页版 + OpenAPI 同域)
    "tushare.pro",   // tushare pro
];

/// Default foreign host list; consulted only when a proxy is configured.
pub const DEFAULT_FOREIGN_HOSTS: &[&str] = &[
    "github.com",
    "githubusercontent.com",
    "githubassets.com",
    "crates.io",
    "pypi.org",
    "huggingface.co",
];

/// Where a request to a given URL should go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyRoute {
    /// Do not use the application-configured SOCKS5 route. System HTTP proxy
    /// behavior remains a trusted environment boundary.
    Direct,
    /// Route through the configured SOCKS5 proxy.
    Socks5,
}

/// Proxy routing configuration.
///
/// Cheap to clone; Engine constructs it only from a Credential Manager value
/// held in memory for the lifetime of the process.
#[derive(Clone, Default)]
pub struct ProxyConfig {
    /// SOCKS5 proxy address (`socks5h://host:port`, `socks5://…`, or a bare
    /// `host:port`). `None` = everything direct.
    socks5: Option<String>,
    /// Hosts eligible for proxying (suffix match); empty = proxy nothing.
    foreign_hosts: Vec<String>,
}

impl ProxyConfig {
    /// Everything direct.
    pub fn direct() -> Self {
        ProxyConfig::default()
    }

    /// Build from a Credential-Manager value supplied in memory by Engine.
    pub fn with_socks5(socks5: Option<String>) -> Self {
        let socks5 = socks5.filter(|value| !value.trim().is_empty());
        if socks5.is_none() {
            return ProxyConfig::direct();
        }
        ProxyConfig {
            socks5,
            foreign_hosts: DEFAULT_FOREIGN_HOSTS
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
        }
    }

    /// Normalized proxy URL for `reqwest::Proxy::all`: a bare `host:port`
    /// becomes `socks5h://host:port` (remote DNS, so foreign domains resolve
    /// at the proxy). `None` when no proxy is configured.
    pub fn proxy_url(&self) -> Option<String> {
        let raw = self.socks5.as_deref()?.trim();
        if raw.is_empty() {
            return None;
        }
        if raw.contains("://") {
            Some(raw.to_string())
        } else {
            Some(format!("socks5h://{raw}"))
        }
    }

    /// Routing decision for `url`: domestic hosts always direct; foreign
    /// hosts proxied only when a proxy is configured; everything else direct.
    pub fn route(&self, url: &str) -> ProxyRoute {
        let Some(host) = host_of(url) else {
            return ProxyRoute::Direct;
        };
        if DOMESTIC_HOSTS.iter().any(|d| host_matches(&host, d)) {
            return ProxyRoute::Direct;
        }
        if self.proxy_url().is_some() && self.foreign_hosts.iter().any(|f| host_matches(&host, f)) {
            return ProxyRoute::Socks5;
        }
        ProxyRoute::Direct
    }
}

/// Lowercased host part of a URL (`None` for unparseable input).
fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
}

/// Exact match or subdomain match (`api.github.com` matches `github.com`).
fn host_matches(host: &str, pattern: &str) -> bool {
    let pattern = pattern.trim_start_matches('.');
    host == pattern || host.ends_with(&format!(".{pattern}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxied() -> ProxyConfig {
        ProxyConfig {
            socks5: Some("127.0.0.1:1080".to_string()),
            foreign_hosts: DEFAULT_FOREIGN_HOSTS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }

    #[test]
    fn domestic_hosts_never_proxied() {
        let cfg = proxied();
        for url in [
            "https://qt.gtimg.cn/q=sh600519",
            "https://web.ifzq.gtimg.cn/appstock/app/fqkline/get",
            "https://push2his.eastmoney.com/api/qt/stock/kline/get",
            "https://hq.sinajs.cn/list=sh600519",
            "https://stock.xueqiu.com/v5/stock/quote.json",
            "http://d.10jqka.com.cn/v6/line/hs_600519/01/last.js",
            "https://api.tushare.pro",
            "https://openapi.iwencai.com/v1/query2data",
        ] {
            assert_eq!(cfg.route(url), ProxyRoute::Direct, "{url}");
        }
    }

    #[test]
    fn domestic_wins_even_if_listed_as_foreign() {
        // Misconfiguration guard: a domestic host in foreign_hosts still
        // goes direct.
        let cfg = ProxyConfig {
            socks5: Some("127.0.0.1:1080".to_string()),
            foreign_hosts: vec!["eastmoney.com".to_string()],
        };
        assert_eq!(
            cfg.route("https://push2.eastmoney.com/api/qt/clist/get"),
            ProxyRoute::Direct
        );
    }

    #[test]
    fn foreign_hosts_proxied_only_with_socks5() {
        let cfg = proxied();
        assert_eq!(
            cfg.route("https://github.com/kunkundi/niuone"),
            ProxyRoute::Socks5
        );
        // Subdomains match.
        assert_eq!(
            cfg.route("https://raw.githubusercontent.com/a/b/main/x.md"),
            ProxyRoute::Socks5
        );
        assert_eq!(
            cfg.route("https://api.github.com/repos/a/b"),
            ProxyRoute::Socks5
        );
        // Without a proxy configured the same hosts go direct.
        let direct = ProxyConfig {
            socks5: None,
            ..cfg.clone()
        };
        assert_eq!(direct.route("https://github.com"), ProxyRoute::Direct);
    }

    #[test]
    fn unlisted_and_garbage_urls_go_direct() {
        let cfg = proxied();
        assert_eq!(cfg.route("https://example.org"), ProxyRoute::Direct);
        assert_eq!(cfg.route("not a url"), ProxyRoute::Direct);
        // Lookalikes must not match the suffix rule.
        assert_eq!(cfg.route("https://notgithub.com"), ProxyRoute::Direct);
        assert_eq!(cfg.route("https://github.com.evil.io"), ProxyRoute::Direct);
    }

    #[test]
    fn proxy_url_normalization() {
        assert_eq!(ProxyConfig::direct().proxy_url(), None);
        assert_eq!(
            ProxyConfig {
                socks5: Some("127.0.0.1:1080".to_string()),
                foreign_hosts: vec![],
            }
            .proxy_url()
            .as_deref(),
            Some("socks5h://127.0.0.1:1080")
        );
        assert_eq!(
            ProxyConfig {
                socks5: Some("socks5://u:p@10.0.0.1:1080".to_string()),
                foreign_hosts: vec![],
            }
            .proxy_url()
            .as_deref(),
            Some("socks5://u:p@10.0.0.1:1080")
        );
    }
}
