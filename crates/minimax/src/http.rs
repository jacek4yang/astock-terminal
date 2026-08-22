//! Minimal HTTP abstraction over `reqwest`.
//!
//! The provider talks HTTP exclusively through the [`Http`] trait so tests can
//! point the client at a local `TcpListener`-based mini server (or an in-memory
//! stub) instead of the real MiniMax endpoints. [`ReqwestHttp`] is the
//! production implementation.

use std::pin::Pin;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{Stream, StreamExt};

use crate::error::MinimaxError;
use crate::key::SecretKey;

/// A byte stream of a response body, used for SSE streaming.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, MinimaxError>> + Send>>;

/// A fully-buffered HTTP response.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers with lowercased names.
    pub headers: Vec<(String, String)>,
    /// Raw response body.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Value of a header, looked up case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == lower)
            .map(|(_, v)| v.as_str())
    }
}

/// Transport used by the provider. Implementations must never log the bearer
/// key or raw request bodies.
pub trait Http: Send + Sync {
    /// `GET` with an optional `Authorization: Bearer` header.
    fn get<'a>(
        &'a self,
        url: &'a str,
        bearer: Option<&'a SecretKey>,
    ) -> BoxFuture<'a, Result<HttpResponse, MinimaxError>>;

    /// `POST` a JSON body with an optional `Authorization: Bearer` header.
    fn post<'a>(
        &'a self,
        url: &'a str,
        bearer: Option<&'a SecretKey>,
        body: Option<&'a serde_json::Value>,
    ) -> BoxFuture<'a, Result<HttpResponse, MinimaxError>>;

    /// `POST` a JSON body and return the response body as a byte stream.
    ///
    /// Non-2xx statuses are consumed into a typed error before streaming
    /// starts, so a yielded stream always belongs to a 2xx response.
    fn post_stream<'a>(
        &'a self,
        url: &'a str,
        bearer: &'a SecretKey,
        body: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<ByteStream, MinimaxError>>;
}

/// Production [`Http`] implementation backed by `reqwest` (rustls).
pub struct ReqwestHttp {
    client: reqwest::Client,
    stream_client: reqwest::Client,
}

impl Default for ReqwestHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl ReqwestHttp {
    /// Build a client with sane timeouts: 10 s connect, 60 s overall for
    /// buffered requests; streaming requests have no overall timeout because a
    /// reasoning model may stream for minutes (cancel by dropping the stream).
    pub fn new() -> Self {
        let builder = || {
            reqwest::Client::builder()
                .user_agent(concat!("astock-terminal/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(10))
        };
        let client = builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("failed to build reqwest client");
        let stream_client = builder()
            .build()
            .expect("failed to build reqwest streaming client");
        Self {
            client,
            stream_client,
        }
    }
}

fn map_transport(e: reqwest::Error) -> MinimaxError {
    // reqwest error strings carry the URL and cause, never request headers,
    // so no key material can leak here.
    MinimaxError::Network(e.to_string())
}

impl Http for ReqwestHttp {
    fn get<'a>(
        &'a self,
        url: &'a str,
        bearer: Option<&'a SecretKey>,
    ) -> BoxFuture<'a, Result<HttpResponse, MinimaxError>> {
        Box::pin(async move {
            let mut req = self.client.get(url);
            if let Some(key) = bearer {
                req = req.bearer_auth(key.expose());
            }
            let resp = req.send().await.map_err(map_transport)?;
            buffered(resp).await
        })
    }

    fn post<'a>(
        &'a self,
        url: &'a str,
        bearer: Option<&'a SecretKey>,
        body: Option<&'a serde_json::Value>,
    ) -> BoxFuture<'a, Result<HttpResponse, MinimaxError>> {
        Box::pin(async move {
            let mut req = self.client.post(url);
            if let Some(key) = bearer {
                req = req.bearer_auth(key.expose());
            }
            if let Some(json) = body {
                req = req.json(json);
            }
            let resp = req.send().await.map_err(map_transport)?;
            buffered(resp).await
        })
    }

    fn post_stream<'a>(
        &'a self,
        url: &'a str,
        bearer: &'a SecretKey,
        body: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<ByteStream, MinimaxError>> {
        Box::pin(async move {
            let resp = self
                .stream_client
                .post(url)
                .bearer_auth(bearer.expose())
                .json(body)
                .send()
                .await
                .map_err(map_transport)?;
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                let buffered = buffered(resp).await?;
                return Err(map_http_error(status, &buffered.headers, &buffered.body));
            }
            let stream = resp
                .bytes_stream()
                .map(|r| r.map(|b| b.to_vec()).map_err(map_transport));
            Ok(Box::pin(stream) as ByteStream)
        })
    }
}

async fn buffered(resp: reqwest::Response) -> Result<HttpResponse, MinimaxError> {
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_lowercase(),
                v.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let body = resp.bytes().await.map_err(map_transport)?.to_vec();
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

/// Map a non-2xx HTTP response to a typed error, sniffing MiniMax's
/// `base_resp` envelope and OpenAI-style `error` bodies for detail.
pub(crate) fn map_http_error(
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
) -> MinimaxError {
    let (code, msg) = sniff_error_body(body);
    let code = code.unwrap_or(i64::from(status));
    match status {
        401 | 403 => MinimaxError::Auth(msg),
        429 => {
            let retry_after = headers
                .iter()
                .find(|(k, _)| k == "retry-after")
                .and_then(|(_, v)| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            MinimaxError::RateLimited { retry_after }
        }
        500..=599 => MinimaxError::Network(format!("http {status}: {msg}")),
        _ => map_base_resp(code, &msg),
    }
}

/// Map a non-zero `base_resp.status_code` to a typed error.
///
/// `2049` ("invalid api key") is an auth failure; `1002` is MiniMax's
/// rate-limit code; everything else is a generic API error.
pub(crate) fn map_base_resp(code: i64, msg: &str) -> MinimaxError {
    match code {
        2049 => MinimaxError::Auth(msg.to_string()),
        1002 => MinimaxError::RateLimited { retry_after: None },
        other => MinimaxError::Api {
            code: other,
            msg: msg.to_string(),
        },
    }
}

/// Extract `(status_code, message)` from a MiniMax `base_resp` envelope or an
/// OpenAI-style `error` object; falls back to truncated raw text.
fn sniff_error_body(body: &[u8]) -> (Option<i64>, String) {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        let text = String::from_utf8_lossy(body);
        return (None, text.chars().take(200).collect());
    };
    if let Some(base) = value.get("base_resp") {
        let code = base.get("status_code").and_then(serde_json::Value::as_i64);
        let msg = base
            .get("status_msg")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown error")
            .to_string();
        return (code, msg);
    }
    if let Some(err) = value.get("error") {
        let msg = err
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown error")
            .to_string();
        let code = err.get("code").and_then(serde_json::Value::as_i64);
        return (code, msg);
    }
    (None, value.to_string().chars().take(200).collect())
}
