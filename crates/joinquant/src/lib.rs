//! # astock-joinquant
//!
//! JoinQuant (聚宽) data channel through the **research environment**
//! (JoinQuant Research, a JupyterHub + classic notebook server) — pure Rust,
//! no browser, no thrift/pickle. See
//! `docs/data-source-joinquant-v2.md` for the full protocol research.
//!
//! Pipeline:
//!
//! 1. **Login** ([`auth`]): `POST /user/login/doLoginByText` with plaintext
//!    `username`/`pwd` over HTTPS; on code `105` a self-developed jigsaw
//!    slider captcha is solved via [`astock_captcha::solve_slider`] and the
//!    login is replayed with `valideCode` (the validate endpoint only takes
//!    a single `axisX` scalar — no drag trajectory).
//! 2. **Hub relay** ([`hub`]): `GET /default/research/redirect` yields the
//!    internal user id (`mob`) and the session id; `POST /hub/login`
//!    exchanges them for a `jupyter-hub-token`; visiting `/hub/` triggers the
//!    single-user server spawn (polled until `/user/<mob>/api` answers).
//! 3. **Kernel execution** ([`kernel`]): a `python3` kernel is created via
//!    the Jupyter REST API (XSRF header required) and code runs over the
//!    WebSocket channels endpoint using the Jupyter wire protocol.
//!    **The WS handshake must NOT carry an `Origin` header** — openresty
//!    deterministically answers 502 if it does (verified A/B).
//! 4. **Queries** ([`query`]): Python templates call the pre-installed
//!    research API (`get_price`, `get_index_stocks`, `get_fundamentals`,
//!    `jqdata.macro`, …) and print the result as one base64-wrapped
//!    `JQJSON:<base64(json)>` stdout line (base64 because the kernel stdout
//!    encoding mangles non-ASCII text).
//!
//! Gotchas honored here (all from the research doc):
//!
//! - The research environment's default context date is stuck at
//!   **2015-12-31** — every query template passes explicit dates.
//! - The kernel is **Python 3.6.7** — templates avoid 3.8+ syntax.
//! - Kernels are deleted after use ([`ResearchSession::close`]); sessions
//!   are serialized process-wide (single kernel, low frequency).
//!
//! Credentials are supplied by the caller ([`Credentials`]); nothing is
//! persisted by this crate. Tests read `JQ_USER` / `JQ_PWD` from the
//! environment (see `tests/live.rs`).

mod auth;
mod client;
mod error;
mod hub;
mod kernel;
mod query;

pub use client::{JoinQuantClient, ResearchSession};
pub use error::JoinQuantError;
pub use query::{jq_to_internal, DailyBar, ValuationSnapshot};

/// JoinQuant account credentials (plaintext password — the site itself
/// transmits it plaintext over HTTPS; stored only in memory).
#[derive(Debug, Clone)]
pub struct Credentials {
    /// Login name (usually a mobile number).
    pub username: String,
    /// Plaintext password.
    pub password: String,
}

impl Credentials {
    /// Build credentials from explicit values.
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
        }
    }
}
