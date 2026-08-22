//! JupyterHub relay: bridge page → hub login → spawn → XSRF token.
//!
//! Protocol facts (docs/data-source-joinquant-v2.md §2.1–§2.3):
//!
//! - `GET /default/research/redirect` returns a bridge page embedding
//!   `var mob = "<internal id>"` (NOT the mobile number) and
//!   `var sessionId = "<PHPSESSID value>"`.
//! - `POST /hub/login` (form `username=<mob>&token=<sessionId>`) → 302,
//!   sets `jupyter-hub-token` (~30 days).
//! - `GET /hub/` follows redirects into `/user/<mob>/` which triggers the
//!   single-user server spawn (~10s cold start); readiness is polled at
//!   `GET /user/<mob>/api` until it answers 200.
//! - All notebook POSTs need XSRF: `GET /user/<mob>/tree` plants the
//!   `_xsrf` cookie, which is then echoed as the `X-XSRFToken` header.

use std::sync::Arc;
use std::time::{Duration, Instant};

use regex::Regex;
use reqwest::cookie::{CookieStore, Jar};
use reqwest::{Client, Url};

use crate::auth::BASE;
use crate::error::JoinQuantError;

/// Poll interval while waiting for the single-user server.
const SPAWN_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Give up waiting for spawn after this long (cold start measured ~10s).
const SPAWN_TIMEOUT: Duration = Duration::from_secs(60);

/// Fields extracted from the research bridge page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BridgeInfo {
    /// Internal numeric user id (path segment of the single-user server).
    pub mob: String,
    /// Hub login token — the same value as the `PHPSESSID` cookie.
    pub session_id: String,
}

/// Parse `var mob = "..."` / `var sessionId = "..."` out of the bridge page.
pub(crate) fn parse_bridge(html: &str) -> Result<BridgeInfo, JoinQuantError> {
    let mob_re = Regex::new(r#"var\s+mob\s*=\s*"(\d+)""#).expect("mob regex");
    let sid_re = Regex::new(r#"var\s+sessionId\s*=\s*"([^"]+)""#).expect("sessionId regex");
    let mob = mob_re
        .captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .ok_or_else(|| JoinQuantError::BridgeParse("mob not found".into()))?;
    let session_id = sid_re
        .captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .ok_or_else(|| JoinQuantError::BridgeParse("sessionId not found".into()))?;
    Ok(BridgeInfo { mob, session_id })
}

/// `GET /default/research/redirect` and extract the relay fields.
pub(crate) async fn fetch_bridge(http: &Client) -> Result<BridgeInfo, JoinQuantError> {
    let body = http
        .get(format!("{BASE}/default/research/redirect"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    parse_bridge(&body)
}

/// `POST /hub/login` with the bridge credentials; redirects are followed
/// (landing on `/hub/`), which sets `jupyter-hub-token` in the jar.
pub(crate) async fn hub_login(http: &Client, bridge: &BridgeInfo) -> Result<(), JoinQuantError> {
    let resp = http
        .post(format!("{BASE}/hub/login"))
        .form(&[
            ("username", bridge.mob.as_str()),
            ("token", bridge.session_id.as_str()),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(JoinQuantError::Protocol(format!(
            "hub login ended with status {}",
            resp.status()
        )));
    }
    Ok(())
}

/// Trigger the single-user server spawn and wait until the notebook REST
/// API answers. Culled (recycled) instances simply re-spawn here.
pub(crate) async fn trigger_spawn_and_wait(http: &Client, mob: &str) -> Result<(), JoinQuantError> {
    // Following /hub/ redirects into /user/<mob>/ is what triggers spawn.
    let _ = http.get(format!("{BASE}/hub/")).send().await;

    let api = format!("{BASE}/user/{mob}/api");
    let start = Instant::now();
    loop {
        match http.get(&api).send().await {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            _ => {
                if start.elapsed() >= SPAWN_TIMEOUT {
                    return Err(JoinQuantError::SpawnTimeout(SPAWN_TIMEOUT.as_secs()));
                }
                tokio::time::sleep(SPAWN_POLL_INTERVAL).await;
            }
        }
    }
}

/// Extract a named cookie value from the shared jar for a given URL.
pub(crate) fn cookie_value(jar: &Jar, url: &Url, name: &str) -> Option<String> {
    let header = jar.cookies(url)?;
    let header = header.to_str().ok()?;
    header.split(';').find_map(|part| {
        let part = part.trim();
        part.strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('='))
            .map(str::to_owned)
    })
}

/// Plant the `_xsrf` cookie (`GET /user/<mob>/tree`) and read it back.
pub(crate) async fn fetch_xsrf(
    http: &Client,
    jar: &Arc<Jar>,
    mob: &str,
) -> Result<String, JoinQuantError> {
    let tree = format!("{BASE}/user/{mob}/tree");
    http.get(&tree).send().await?.error_for_status()?;
    let url = Url::parse(&tree).expect("valid tree url");
    cookie_value(jar, &url, "_xsrf")
        .ok_or_else(|| JoinQuantError::Protocol("_xsrf cookie not planted".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRIDGE_HTML: &str = r#"
        <html><head><script>
        var mob = "29005631157";
        var sessionId = "abc123sessionidxyz";
        Cy.postRedirect('https://www.joinquant.com/hub/login?next=%26url%3D',
                        {username: mob, token: sessionId});
        </script></head></html>
    "#;

    #[test]
    fn parse_bridge_extracts_mob_and_session() {
        let info = parse_bridge(BRIDGE_HTML).unwrap();
        assert_eq!(
            info,
            BridgeInfo {
                mob: "29005631157".into(),
                session_id: "abc123sessionidxyz".into(),
            }
        );
    }

    #[test]
    fn parse_bridge_missing_fields() {
        assert!(parse_bridge("<html>nothing here</html>").is_err());
        assert!(parse_bridge(r#"var mob = "123";"#).is_err());
    }

    #[test]
    fn cookie_value_reads_named_cookie_from_jar() {
        let jar = Jar::default();
        let url = Url::parse("https://www.joinquant.com/user/29005631157/tree").unwrap();
        jar.add_cookie_str("_xsrf=deadbeef|12345; Path=/", &url);
        jar.add_cookie_str("PHPSESSID=sess42; Path=/; HttpOnly", &url);
        assert_eq!(
            cookie_value(&jar, &url, "_xsrf").as_deref(),
            Some("deadbeef|12345")
        );
        assert_eq!(
            cookie_value(&jar, &url, "PHPSESSID").as_deref(),
            Some("sess42")
        );
        assert!(cookie_value(&jar, &url, "nope").is_none());
    }

    #[test]
    fn cookie_value_scoped_by_domain() {
        let jar = Jar::default();
        let jq = Url::parse("https://www.joinquant.com/").unwrap();
        let other = Url::parse("https://example.com/").unwrap();
        jar.add_cookie_str("_xsrf=onlyforjq; Path=/", &jq);
        assert!(cookie_value(&jar, &other, "_xsrf").is_none());
    }
}
