use std::time::Duration;

use futures::StreamExt;
use reqwest::header::{
    ACCEPT, ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, LOCATION, USER_AGENT,
};
use reqwest::{redirect::Policy, StatusCode};
use thiserror::Error;

use crate::{ResolvedSafeUrl, UrlSecurityError, UrlSecurityPolicy};

#[derive(Debug, Clone)]
pub struct SafeFetchLimits {
    pub max_bytes: usize,
    pub max_redirects: usize,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    pub allowed_mime: Vec<&'static str>,
}

impl Default for SafeFetchLimits {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            max_redirects: 5,
            request_timeout: Duration::from_secs(20),
            connect_timeout: Duration::from_secs(5),
            allowed_mime: vec![
                "text/html",
                "text/plain",
                "application/json",
                "application/pdf",
                "application/xhtml+xml",
                "application/xml",
                "text/xml",
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeFetchResult {
    pub final_url: String,
    pub media_type: String,
    pub body: Vec<u8>,
    pub redirects: Vec<String>,
}

#[derive(Debug, Error)]
pub enum SafeFetchError {
    #[error(transparent)]
    Url(#[from] UrlSecurityError),
    #[error("安全 HTTP 客户端初始化失败：{0}")]
    Client(String),
    #[error("网络请求失败：{0}")]
    Request(String),
    #[error("重定向缺少有效 Location")]
    RedirectLocation,
    #[error("重定向次数超过上限 {0}")]
    TooManyRedirects(usize),
    #[error("HTTP 状态不可用：{0}")]
    Http(StatusCode),
    #[error("压缩响应被拒绝；安全抓取器只接受 identity 编码")]
    CompressedResponse,
    #[error("响应 MIME 不在白名单：{0}")]
    Mime(String),
    #[error("响应超过大小上限 {limit} 字节（已读取 {actual} 字节）")]
    TooLarge { limit: usize, actual: usize },
    #[error("请求头不合法：{0}")]
    InvalidHeader(String),
}

#[derive(Debug, Clone)]
pub struct SafeFetcher {
    policy: UrlSecurityPolicy,
    limits: SafeFetchLimits,
}

impl SafeFetcher {
    pub fn new(policy: UrlSecurityPolicy, limits: SafeFetchLimits) -> Self {
        Self { policy, limits }
    }

    pub fn standard() -> Self {
        Self::new(UrlSecurityPolicy::default(), SafeFetchLimits::default())
    }

    /// Bounded GET with redirects disabled in reqwest. Every hop is parsed,
    /// DNS-resolved, checked for private/reserved addresses and pinned to the
    /// validated IP to close the usual validation/connect DNS-rebinding gap.
    pub async fn fetch(&self, raw_url: &str) -> Result<SafeFetchResult, SafeFetchError> {
        self.fetch_with_user_agent(raw_url, None).await
    }

    /// Same bounded/SSRF-safe fetch path with an optional declared User-Agent.
    /// The only override is User-Agent, preventing callers from injecting
    /// authorization or cookie headers into redirect chains.
    pub async fn fetch_with_user_agent(
        &self,
        raw_url: &str,
        user_agent: Option<&str>,
    ) -> Result<SafeFetchResult, SafeFetchError> {
        let user_agent = user_agent
            .map(reqwest::header::HeaderValue::from_str)
            .transpose()
            .map_err(|error| SafeFetchError::InvalidHeader(error.to_string()))?;
        let mut current = raw_url.to_string();
        let mut redirects = Vec::new();
        for redirect_count in 0..=self.limits.max_redirects {
            let resolved = self.policy.validate_resolved(&current).await?;
            let (client, requested_url) = self.client_for(&resolved)?;
            let mut request = client
                .get(requested_url.clone())
                .header(ACCEPT, self.limits.allowed_mime.join(", "))
                .header(ACCEPT_ENCODING, "identity");
            if let Some(value) = &user_agent {
                request = request.header(USER_AGENT, value.clone());
            }
            let response = request
                .send()
                .await
                .map_err(|error| SafeFetchError::Request(error.to_string()))?;
            if response.status().is_redirection() {
                if redirect_count == self.limits.max_redirects {
                    return Err(SafeFetchError::TooManyRedirects(self.limits.max_redirects));
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(SafeFetchError::RedirectLocation)?;
                let target = requested_url
                    .join(location)
                    .map_err(|error| UrlSecurityError::Malformed(error.to_string()))?;
                // Static validation happens before recording the target. DNS
                // validation happens at the beginning of the next loop.
                let target = self.policy.validate_static(target.as_str())?;
                redirects.push(target.as_str().to_string());
                current = target.as_str().to_string();
                continue;
            }
            if !response.status().is_success() {
                return Err(SafeFetchError::Http(response.status()));
            }
            if response
                .headers()
                .get(CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
            {
                return Err(SafeFetchError::CompressedResponse);
            }
            let media_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| SafeFetchError::Mime("missing".to_string()))?
                .to_ascii_lowercase();
            if !self
                .limits
                .allowed_mime
                .iter()
                .any(|allowed| media_type == *allowed)
            {
                return Err(SafeFetchError::Mime(media_type));
            }
            if let Some(declared) = response
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<usize>().ok())
            {
                if declared > self.limits.max_bytes {
                    return Err(SafeFetchError::TooLarge {
                        limit: self.limits.max_bytes,
                        actual: declared,
                    });
                }
            }
            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| SafeFetchError::Request(error.to_string()))?;
                append_bounded(&mut body, &chunk, self.limits.max_bytes)?;
            }
            return Ok(SafeFetchResult {
                final_url: requested_url.to_string(),
                media_type,
                body,
                redirects,
            });
        }
        Err(SafeFetchError::TooManyRedirects(self.limits.max_redirects))
    }

    fn client_for(
        &self,
        resolved: &ResolvedSafeUrl,
    ) -> Result<(reqwest::Client, url::Url), SafeFetchError> {
        let host = resolved.url.host().to_string();
        let address = *resolved
            .addresses
            .first()
            .ok_or(UrlSecurityError::EmptyDns)?;
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(self.limits.request_timeout)
            .connect_timeout(self.limits.connect_timeout)
            .resolve(&host, address)
            .build()
            .map_err(|error| SafeFetchError::Client(error.to_string()))?;
        Ok((client, resolved.url.clone().into_url()?))
    }
}

fn append_bounded(body: &mut Vec<u8>, chunk: &[u8], limit: usize) -> Result<(), SafeFetchError> {
    let next_len = body.len().saturating_add(chunk.len());
    if next_len > limit {
        return Err(SafeFetchError::TooLarge {
            limit,
            actual: next_len,
        });
    }
    body.extend_from_slice(chunk);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blocks_loopback_before_any_network_request() {
        let error = SafeFetcher::standard()
            .fetch("http://127.0.0.1:80/private")
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            SafeFetchError::Url(UrlSecurityError::BlockedAddress(_))
        ));
    }

    #[test]
    fn default_limits_are_bounded_and_reject_binary_mime() {
        let limits = SafeFetchLimits::default();
        assert!(limits.max_bytes <= 8 * 1024 * 1024);
        assert!(limits.max_redirects <= 5);
        assert!(!limits.allowed_mime.contains(&"application/zip"));
        assert!(!limits.allowed_mime.contains(&"application/octet-stream"));
    }

    #[test]
    fn streamed_body_is_stopped_at_the_exact_size_boundary() {
        let mut body = vec![1, 2, 3, 4];
        append_bounded(&mut body, &[5, 6], 6).unwrap();
        let error = append_bounded(&mut body, &[7], 6).unwrap_err();
        assert!(matches!(
            error,
            SafeFetchError::TooLarge {
                limit: 6,
                actual: 7
            }
        ));
        assert_eq!(body, vec![1, 2, 3, 4, 5, 6]);
    }
}
