use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use serde::Serialize;
use thiserror::Error;
use tokio::net::lookup_host;
use url::{Host, Url};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SafeUrl {
    normalized: String,
    host: String,
    port: u16,
}

impl SafeUrl {
    pub fn as_str(&self) -> &str {
        &self.normalized
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn into_url(self) -> Result<Url, UrlSecurityError> {
        Url::parse(&self.normalized).map_err(|error| UrlSecurityError::Malformed(error.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSafeUrl {
    pub url: SafeUrl,
    pub addresses: Vec<SocketAddr>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum UrlSecurityError {
    #[error("URL 格式无效：{0}")]
    Malformed(String),
    #[error("只允许 http/https URL")]
    Scheme,
    #[error("URL 不允许包含用户名或密码")]
    Credentials,
    #[error("URL 缺少可验证主机名")]
    MissingHost,
    #[error("目标主机被安全策略阻止：{0}")]
    BlockedHost(String),
    #[error("目标端口被安全策略阻止：{0}")]
    BlockedPort(u16),
    #[error("DNS 解析失败：{0}")]
    Dns(String),
    #[error("DNS 返回了私网、环回、链路本地或保留地址：{0}")]
    BlockedAddress(IpAddr),
    #[error("DNS 没有返回地址")]
    EmptyDns,
}

#[derive(Debug, Clone)]
pub struct UrlSecurityPolicy {
    allowed_ports: BTreeSet<u16>,
    dns_timeout: Duration,
}

impl Default for UrlSecurityPolicy {
    fn default() -> Self {
        Self {
            allowed_ports: [80, 443].into_iter().collect(),
            dns_timeout: Duration::from_secs(3),
        }
    }
}

impl UrlSecurityPolicy {
    pub fn with_allowed_ports(ports: impl IntoIterator<Item = u16>) -> Self {
        Self {
            allowed_ports: ports.into_iter().collect(),
            dns_timeout: Duration::from_secs(3),
        }
    }

    pub fn validate_static(&self, raw: &str) -> Result<SafeUrl, UrlSecurityError> {
        let mut url = Url::parse(raw.trim())
            .map_err(|error| UrlSecurityError::Malformed(error.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(UrlSecurityError::Scheme);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(UrlSecurityError::Credentials);
        }
        let host = match url.host().ok_or(UrlSecurityError::MissingHost)? {
            Host::Domain(value) => {
                let value = value.trim_end_matches('.').to_ascii_lowercase();
                if value.is_empty() {
                    return Err(UrlSecurityError::MissingHost);
                }
                if blocked_hostname(&value) {
                    return Err(UrlSecurityError::BlockedHost(value));
                }
                value
            }
            Host::Ipv4(address) => {
                validate_public_ip(IpAddr::V4(address))?;
                address.to_string()
            }
            Host::Ipv6(address) => {
                validate_public_ip(IpAddr::V6(address))?;
                address.to_string()
            }
        };
        let port = url
            .port_or_known_default()
            .ok_or(UrlSecurityError::BlockedPort(0))?;
        if !self.allowed_ports.contains(&port) {
            return Err(UrlSecurityError::BlockedPort(port));
        }
        url.set_fragment(None);
        Ok(SafeUrl {
            normalized: url.to_string(),
            host,
            port,
        })
    }

    pub async fn validate_resolved(&self, raw: &str) -> Result<ResolvedSafeUrl, UrlSecurityError> {
        let url = self.validate_static(raw)?;
        let lookup = tokio::time::timeout(self.dns_timeout, lookup_host((url.host(), url.port())))
            .await
            .map_err(|_| UrlSecurityError::Dns("timeout".to_string()))?;
        let addresses = lookup
            .map_err(|error| UrlSecurityError::Dns(error.to_string()))?
            .collect::<Vec<_>>();
        self.validate_addresses(url, addresses)
    }

    fn validate_addresses(
        &self,
        url: SafeUrl,
        mut addresses: Vec<SocketAddr>,
    ) -> Result<ResolvedSafeUrl, UrlSecurityError> {
        if addresses.is_empty() {
            return Err(UrlSecurityError::EmptyDns);
        }
        addresses.sort_unstable();
        addresses.dedup();
        for address in &addresses {
            validate_public_ip(address.ip())?;
        }
        Ok(ResolvedSafeUrl { url, addresses })
    }
}

fn blocked_hostname(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".lan")
        || host.ends_with(".home")
        || matches!(
            host,
            "metadata.google.internal"
                | "metadata"
                | "instance-data"
                | "instance-data.ec2.internal"
        )
}

fn validate_public_ip(address: IpAddr) -> Result<(), UrlSecurityError> {
    if is_blocked_ip(address) {
        Err(UrlSecurityError::BlockedAddress(address))
    } else {
        Ok(())
    }
}

fn is_blocked_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => is_blocked_v4(ip),
        IpAddr::V6(ip) => is_blocked_v6(ip),
    }
}

fn is_blocked_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, d] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || a == 0
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && matches!(b, 18 | 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || (a == 169 && b == 254)
        || (a == 100 && b == 100 && c == 100 && d == 200)
}

fn is_blocked_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || ip.to_ipv4_mapped().is_some_and(is_blocked_v4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn rejects_local_private_metadata_credentials_and_unsafe_schemes() {
        let policy = UrlSecurityPolicy::default();
        for value in [
            "http://127.0.0.1/admin",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data/",
            "http://100.100.100.200/latest/meta-data/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://metadata.google.internal/",
            "http://user:password@example.com/",
            "file:///etc/passwd",
            "ftp://example.com/file",
            "gopher://example.com/",
        ] {
            assert!(policy.validate_static(value).is_err(), "{value}");
        }
    }

    #[test]
    fn accepts_public_http_and_https_but_strips_fragments() {
        let policy = UrlSecurityPolicy::default();
        let value = policy
            .validate_static("https://www.sse.com.cn/disclosure/a.pdf#page=2")
            .unwrap();
        assert_eq!(value.host(), "www.sse.com.cn");
        assert!(!value.as_str().contains('#'));
        assert!(policy.validate_static("http://example.com/news").is_ok());
        assert!(policy.validate_static("https://example.com:8443/").is_err());
    }

    #[test]
    fn rejects_dns_rebinding_when_any_answer_is_private() {
        let policy = UrlSecurityPolicy::default();
        let url = policy
            .validate_static("https://example.com/report")
            .unwrap();
        let error = policy
            .validate_addresses(
                url,
                vec![
                    SocketAddr::from(([93, 184, 216, 34], 443)),
                    SocketAddr::from(([127, 0, 0, 1], 443)),
                ],
            )
            .unwrap_err();
        assert_eq!(
            error,
            UrlSecurityError::BlockedAddress(IpAddr::V4(Ipv4Addr::LOCALHOST))
        );
    }

    #[test]
    fn redirect_target_is_revalidated_instead_of_inheriting_trust() {
        let policy = UrlSecurityPolicy::default();
        let base = Url::parse("https://example.com/start").unwrap();
        let target = base
            .join("http://169.254.169.254/latest/meta-data/")
            .unwrap();
        assert!(matches!(
            policy.validate_static(target.as_str()),
            Err(UrlSecurityError::BlockedAddress(_))
        ));
    }

    proptest! {
        #[test]
        fn every_rfc1918_address_is_blocked(a in 0u8..=255, b in 0u8..=255) {
            let policy = UrlSecurityPolicy::default();
            for ip in [
                Ipv4Addr::new(10, a, b, 1),
                Ipv4Addr::new(172, 16 + a % 16, b, 1),
                Ipv4Addr::new(192, 168, a, b),
            ] {
                let value = format!("http://{ip}/");
                prop_assert!(policy.validate_static(&value).is_err());
            }
        }
    }
}
