//! Kernel lifecycle (Jupyter REST) and code execution (WebSocket, Jupyter
//! wire protocol v5.x).
//!
//! Protocol facts (docs/data-source-joinquant-v2.md §2.3–§2.4):
//!
//! - `POST /user/<mob>/api/kernels {"name":"python3"}` with the
//!   `X-XSRFToken` header → 201 `{"id": ...}`.
//! - Execution runs on
//!   `wss://www.joinquant.com/user/<mob>/api/kernels/<id>/channels?session_id=<uuid>`
//!   authenticated by the same-origin cookies.
//! - **The handshake must not carry an `Origin` header** — openresty
//!   deterministically answers 502 (verified A/B 3× each). tungstenite does
//!   not send one by default; [`build_ws_request`] deliberately keeps it
//!   that way.
//! - Wire protocol: send `execute_request` on shell; on iopub filter by
//!   `parent_header.msg_id`, aggregate `stream` stdout, fail on `error`,
//!   finish on `status: idle`.
//! - Kernels are deleted after use (`DELETE /api/kernels/<id>`).

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rand::RngCore;
use reqwest::cookie::{CookieStore, Jar};
use reqwest::{Client, Url};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::Request;
use tokio_tungstenite::tungstenite::Message;

use crate::auth::{BASE, UA};
use crate::error::JoinQuantError;

/// Hard cap for a single code execution.
const EXEC_TIMEOUT: Duration = Duration::from_secs(120);

/// Random 16-byte id rendered as lowercase hex (uuid4 without the dashes).
pub(crate) fn new_msg_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    let mut s = String::with_capacity(32);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Create a `python3` kernel; returns its id.
pub(crate) async fn create_kernel(
    http: &Client,
    mob: &str,
    xsrf: &str,
) -> Result<String, JoinQuantError> {
    let resp: Value = http
        .post(format!("{BASE}/user/{mob}/api/kernels"))
        .header("X-XSRFToken", xsrf)
        .json(&json!({"name": "python3"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    resp.get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| JoinQuantError::Protocol("kernel create: missing id".into()))
}

/// Delete a kernel (best-effort cleanup; 404 is fine).
pub(crate) async fn delete_kernel(
    http: &Client,
    mob: &str,
    kernel_id: &str,
    xsrf: &str,
) -> Result<(), JoinQuantError> {
    http.delete(format!("{BASE}/user/{mob}/api/kernels/{kernel_id}"))
        .header("X-XSRFToken", xsrf)
        .send()
        .await?;
    Ok(())
}

/// Assemble the `Cookie` header value for the notebook server from the jar.
pub(crate) fn cookie_header(jar: &Jar, url: &Url) -> Option<String> {
    jar.cookies(url)
        .and_then(|v| v.to_str().ok().map(str::to_owned))
}

/// Build the WS handshake request.
///
/// The request is derived from the URL so tungstenite generates the full
/// handshake header set (`Sec-WebSocket-Key` etc.); we only add `Cookie`
/// and `User-Agent`. **Do not add an `Origin` header here** — see module
/// docs. The unit test `ws_request_has_no_origin_header` guards this
/// regression.
pub(crate) fn build_ws_request(ws_url: &str, cookie: &str) -> Result<Request<()>, JoinQuantError> {
    let mut request = ws_url.into_client_request().map_err(JoinQuantError::from)?;
    let headers = request.headers_mut();
    headers.append(
        "Cookie",
        cookie
            .parse()
            .map_err(|e| JoinQuantError::Protocol(format!("invalid cookie header: {e}")))?,
    );
    headers.append(
        "User-Agent",
        UA.parse()
            .map_err(|e| JoinQuantError::Protocol(format!("invalid UA header: {e}")))?,
    );
    Ok(request)
}

/// Serialize a Jupyter `execute_request` message.
pub(crate) fn build_execute_request(msg_id: &str, session: &str, code: &str) -> String {
    json!({
        "header": {
            "msg_id": msg_id,
            "username": "astock",
            "session": session,
            "msg_type": "execute_request",
            "version": "5.3",
            "date": "",
        },
        "parent_header": {},
        "metadata": {},
        "content": {
            "code": code,
            "silent": false,
            "store_history": false,
            "user_expressions": {},
            "allow_stdin": false,
            "stop_on_error": true,
        },
        "channel": "shell",
        "buffers": [],
    })
    .to_string()
}

/// Whether a received message is a reply to our request.
pub(crate) fn is_reply_to(msg: &Value, msg_id: &str) -> bool {
    msg.pointer("/parent_header/msg_id").and_then(Value::as_str) == Some(msg_id)
}

/// Execute `code` on the given kernel and return the aggregated stdout.
///
/// A fresh WS connection is opened per call: usage is serial and
/// low-frequency (doc §4.6), so connection reuse is not worth the stale-
/// connection failure modes.
pub(crate) async fn ws_execute(
    jar: &Arc<Jar>,
    mob: &str,
    kernel_id: &str,
    code: &str,
) -> Result<String, JoinQuantError> {
    let session_id = new_msg_id();
    let ws_url = format!(
        "wss://www.joinquant.com/user/{mob}/api/kernels/{kernel_id}/channels?session_id={session_id}"
    );
    let cookie_url = Url::parse(&format!("{BASE}/user/{mob}/"))
        .map_err(|e| JoinQuantError::Protocol(format!("cookie url: {e}")))?;
    let cookie = cookie_header(jar, &cookie_url)
        .ok_or_else(|| JoinQuantError::Protocol("no cookies for notebook server".into()))?;

    let request = build_ws_request(&ws_url, &cookie)?;
    let (mut ws, _resp) = tokio_tungstenite::connect_async(request).await?;

    let msg_id = new_msg_id();
    ws.send(Message::Text(build_execute_request(
        &msg_id,
        &session_id,
        code,
    )))
    .await?;

    let mut stdout = String::new();
    let mut kernel_err: Option<JoinQuantError> = None;

    tokio::time::timeout(EXEC_TIMEOUT, async {
        while let Some(frame) = ws.next().await {
            let frame = frame?;
            let text = match frame {
                Message::Text(t) => t,
                Message::Close(_) => {
                    return Err(JoinQuantError::Protocol("ws closed mid-execution".into()))
                }
                _ => continue,
            };
            let v: Value = serde_json::from_str(&text)?;
            if !is_reply_to(&v, &msg_id) {
                continue;
            }
            match v.pointer("/header/msg_type").and_then(Value::as_str) {
                Some("stream")
                    if v.pointer("/content/name").and_then(Value::as_str) == Some("stdout") =>
                {
                    if let Some(t) = v.pointer("/content/text").and_then(Value::as_str) {
                        stdout.push_str(t);
                    }
                }
                Some("error") => {
                    let content = &v["content"];
                    kernel_err = Some(JoinQuantError::Kernel {
                        ename: content
                            .get("ename")
                            .and_then(Value::as_str)
                            .unwrap_or("Error")
                            .to_string(),
                        evalue: content
                            .get("evalue")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        traceback: content
                            .get("traceback")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    });
                }
                Some("status")
                    if v.pointer("/content/execution_state")
                        .and_then(Value::as_str)
                        == Some("idle") =>
                {
                    break;
                }
                _ => {}
            }
        }
        Ok::<(), JoinQuantError>(())
    })
    .await
    .map_err(|_| {
        JoinQuantError::Protocol(format!("execution timed out after {EXEC_TIMEOUT:?}"))
    })??;

    match kernel_err {
        Some(e) => Err(e),
        None => Ok(stdout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msg_id_is_32_hex_chars() {
        let id = new_msg_id();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(new_msg_id(), new_msg_id());
    }

    #[test]
    fn cookie_header_assembles_from_jar() {
        let jar = Jar::default();
        let url = Url::parse("https://www.joinquant.com/user/29005631157/").unwrap();
        jar.add_cookie_str("PHPSESSID=sess42; Path=/; HttpOnly", &url);
        jar.add_cookie_str("token=deadbeef; Path=/; HttpOnly", &url);
        let header = cookie_header(&jar, &url).unwrap();
        assert!(header.contains("PHPSESSID=sess42"), "header: {header}");
        assert!(header.contains("token=deadbeef"), "header: {header}");
    }

    #[test]
    fn ws_request_has_no_origin_header() {
        // Regression guard: an Origin header makes openresty answer 502
        // deterministically (doc §2.4). Never let one sneak in.
        let req = build_ws_request(
            "wss://www.joinquant.com/user/1/api/kernels/k/channels?session_id=s",
            "PHPSESSID=x",
        )
        .unwrap();
        assert!(!req.headers().contains_key("origin"));
        assert!(!req.headers().contains_key("Origin"));
        assert_eq!(
            req.headers().get("Cookie").and_then(|v| v.to_str().ok()),
            Some("PHPSESSID=x")
        );
    }

    #[test]
    fn execute_request_shape() {
        let raw = build_execute_request("mid", "sid", "print(1)");
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["header"]["msg_id"], "mid");
        assert_eq!(v["header"]["session"], "sid");
        assert_eq!(v["header"]["msg_type"], "execute_request");
        assert_eq!(v["content"]["code"], "print(1)");
        assert_eq!(v["content"]["silent"], false);
        assert_eq!(v["channel"], "shell");
    }

    #[test]
    fn reply_filter_matches_parent_msg_id() {
        let ours = json!({"header": {"msg_type": "stream"}, "parent_header": {"msg_id": "a1"}});
        let other = json!({"header": {"msg_type": "stream"}, "parent_header": {"msg_id": "b2"}});
        let orphan = json!({"header": {"msg_type": "stream"}});
        assert!(is_reply_to(&ours, "a1"));
        assert!(!is_reply_to(&other, "a1"));
        assert!(!is_reply_to(&orphan, "a1"));
    }
}
