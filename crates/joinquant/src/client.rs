//! High-level client tying login, hub relay and kernel execution together.

use std::sync::Arc;
use std::time::Duration;

use reqwest::cookie::Jar;
use reqwest::Client;
use serde_json::Value;
use tokio::sync::{Mutex, MutexGuard};

use crate::error::JoinQuantError;
use crate::{auth, hub, kernel, query};
use crate::{Credentials, DailyBar, ValuationSnapshot};

/// JoinQuant research-environment client.
///
/// - Credentials are constructor parameters only; nothing is persisted.
/// - Sessions are reused (`isLogin` probe before re-login) and logins are
///   serialized through an internal lock (login endpoints are rate-limited).
/// - Research sessions are serialized process-wide: one kernel at a time,
///   low-frequency usage (doc §4.6).
pub struct JoinQuantClient {
    http: Client,
    jar: Arc<Jar>,
    creds: Credentials,
    login_lock: Mutex<()>,
    session_lock: Mutex<()>,
}

impl JoinQuantClient {
    /// Build a client with a fresh cookie jar.
    pub fn new(creds: Credentials) -> Result<Self, JoinQuantError> {
        let jar = Arc::new(Jar::default());
        let http = Client::builder()
            .cookie_provider(jar.clone())
            .user_agent(auth::UA)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            jar,
            creds,
            login_lock: Mutex::new(()),
            session_lock: Mutex::new(()),
        })
    }

    /// Ensure the cookie jar holds a live web session (`PHPSESSID`/`token`).
    pub async fn ensure_logged_in(&self) -> Result<(), JoinQuantError> {
        let _guard = self.login_lock.lock().await;
        if auth::is_logged_in(&self.http).await.unwrap_or(false) {
            return Ok(());
        }
        auth::login(&self.http, &self.creds).await
    }

    /// Open a research session: login → bridge → hub login → spawn → XSRF →
    /// create a `python3` kernel. Serialized process-wide; the kernel is
    /// deleted by [`ResearchSession::close`].
    pub async fn research_session(&self) -> Result<ResearchSession<'_>, JoinQuantError> {
        let permit = self.session_lock.lock().await;
        self.ensure_logged_in().await?;
        let bridge = hub::fetch_bridge(&self.http).await?;
        hub::hub_login(&self.http, &bridge).await?;
        hub::trigger_spawn_and_wait(&self.http, &bridge.mob).await?;
        let xsrf = hub::fetch_xsrf(&self.http, &self.jar, &bridge.mob).await?;
        let kernel_id = kernel::create_kernel(&self.http, &bridge.mob, &xsrf).await?;
        Ok(ResearchSession {
            client: self,
            _permit: permit,
            mob: bridge.mob,
            kernel_id,
            xsrf,
            closed: false,
        })
    }

    /// Daily bars for `security` (JQ code, e.g. `000300.XSHG`) over the
    /// inclusive `[start, end]` range (`YYYY-MM-DD`). Dates are always
    /// passed explicitly — the research context date defaults to 2015-12-31.
    pub async fn daily(
        &self,
        security: &str,
        start: &str,
        end: &str,
    ) -> Result<Vec<DailyBar>, JoinQuantError> {
        let code = query::daily_code(security, start, end)?;
        let payload = self.run_once(&code).await?;
        Ok(query::parse_daily_bars(&payload))
    }

    /// Index components (internal codes) of `index` on `date`.
    pub async fn index_components(
        &self,
        index: &str,
        date: &str,
    ) -> Result<Vec<String>, JoinQuantError> {
        let code = query::index_components_code(index, date)?;
        let payload = self.run_once(&code).await?;
        Ok(query::parse_components(&payload))
    }

    /// Valuation snapshot for `codes` (JQ codes) on `date`.
    pub async fn valuation(
        &self,
        codes: &[String],
        date: &str,
    ) -> Result<Vec<ValuationSnapshot>, JoinQuantError> {
        let code = query::valuation_code(codes, date)?;
        let payload = self.run_once(&code).await?;
        Ok(query::parse_valuations(&payload))
    }

    /// Latest `limit` rows of the monthly CPI macro table
    /// (`MAC_CPI_MONTH`), newest first, as raw JSON records (columns vary:
    /// `stat_month`, `area_name`, `cpi_yoy`, …).
    pub async fn macro_cpi(&self, limit: usize) -> Result<Vec<Value>, JoinQuantError> {
        let code = query::macro_cpi_code(limit);
        let payload = self.run_once(&code).await?;
        Ok(payload.as_array().cloned().unwrap_or_default())
    }

    /// Open a session, execute one template, close the kernel (always, even
    /// on execution error).
    async fn run_once(&self, code: &str) -> Result<Value, JoinQuantError> {
        let mut session = self.research_session().await?;
        let result = session.execute_json(code).await;
        if let Err(e) = session.close().await {
            tracing::warn!(error = %e, "kernel cleanup failed");
        }
        result
    }
}

/// An open research session holding one remote `python3` kernel.
///
/// Call [`ResearchSession::close`] when done — the kernel is a shared
/// server-side resource and this crate's contract is "delete after use"
/// (dropping without closing leaks it until the server culls the instance).
pub struct ResearchSession<'a> {
    client: &'a JoinQuantClient,
    _permit: MutexGuard<'a, ()>,
    mob: String,
    kernel_id: String,
    xsrf: String,
    closed: bool,
}

impl ResearchSession<'_> {
    /// Execute arbitrary Python 3.6 code on the kernel; returns aggregated
    /// stdout.
    pub async fn execute(&mut self, code: &str) -> Result<String, JoinQuantError> {
        kernel::ws_execute(&self.client.jar, &self.mob, &self.kernel_id, code).await
    }

    /// Execute a template and extract its `JQJSON:` payload.
    pub async fn execute_json(&mut self, code: &str) -> Result<Value, JoinQuantError> {
        let stdout = self.execute(code).await?;
        query::extract_payload(&stdout)
    }

    /// Delete the remote kernel.
    pub async fn close(mut self) -> Result<(), JoinQuantError> {
        self.closed = true;
        kernel::delete_kernel(&self.client.http, &self.mob, &self.kernel_id, &self.xsrf).await
    }
}

impl Drop for ResearchSession<'_> {
    fn drop(&mut self) {
        if !self.closed {
            // Reaching Drop unclosed means the kernel lingers server-side
            // until the instance is culled.
            tracing::warn!(
                kernel_id = %self.kernel_id,
                "ResearchSession dropped without close(); kernel left for server culling"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_builds_with_empty_jar() {
        use reqwest::cookie::CookieStore;
        let client = JoinQuantClient::new(Credentials::new("user", "pass")).unwrap();
        let url = reqwest::Url::parse(auth::BASE).unwrap();
        assert!(client.jar.cookies(&url).is_none());
    }
}
