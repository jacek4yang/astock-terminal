//! Market-data layer for the A-share analysis terminal.
//!
//! Ports the legacy Python `kline_fetcher.py`: Tencent/Sina/EastMoney kline
//! failover, EastMoney quote/fund-flow/minute/search/clist endpoints, host
//! pools with failover, UA rotation, a per-client DNS override for the
//! push2his/push2 hosts, adaptive per-host rate limiting, a bounded TTL
//! cache, per-provider circuit breakers, and single-flight request
//! coalescing.
//!
//! Most callers want [`MarketData`], which composes the providers the same
//! way the legacy `fetch_kline` did.
//!
//! Data-foundation-v2 additions (raw-first pipeline): EastMoney klines
//! support `fqt=0/1/2` via [`astock_core::Adjust`], and
//! [`EastMoneyF10::corporate_actions`] parses `RPT_SHAREBONUS_DET` dividend
//! rows into per-share [`astock_core::CorporateAction`]s for the
//! `astock_core::adjust` engine. Live cross-validation of self-computed
//! qfq/hfq against provider series lives in `tests/adjust_live.rs`.
//!
//! Optional token-gated providers (always on the health panel, unavailable
//! without credentials): [`TushareProvider`] (`TUSHARE_TOKEN`; raw daily,
//! trade calendar, and — at the 2000-point tier — `adj_factor` golden
//! cross-checks via [`providers::tushare::compare_qfq_golden`], dividends,
//! daily basics), [`IwencaiOpenApi`] (`IWENCAI_KEY`; 龙虎榜 / 公告新闻
//! 事件 / 板块归属 over the official Bearer-key gateway), and
//! [`JoinQuantProvider`] (`JQ_USER`/`JQ_PWD`; 前复权 daily / 指数成分 /
//! 估值 / 宏观 CPI, strictly low-frequency, explicit-call only).
//!
//! [`TdxProvider`] is the always-available TCP quote-protocol fallback:
//! unadjusted day/week/month klines (volume normalized 股 → 手), five-level
//! quote snapshots, and the segment-filtered full A-share list. Its server
//! pool is probed lazily on first use and cached in-process.
//!
//! Outbound routing: domestic platforms always connect directly; only
//! configured `foreign_hosts` use the optional SOCKS5 proxy from
//! `ASTOCK_SOCKS5` (see [`proxy`]).

pub mod breaker;
pub mod cache;
pub mod http;
pub mod hub;
pub mod provider;
pub mod providers;
pub mod proxy;
mod security_master;
pub mod validate;

pub use breaker::{BreakerConfig, CircuitBreaker, CircuitState, ProviderHealth};
pub use cache::TtlCache;
pub use http::HttpClient;
pub use hub::MarketData;
pub use provider::{DataProvider, Failover};
pub use providers::{
    EastMoney, EastMoneyF10, EmDataCenter, F10Report, FinanceNewsBatch, FinanceNewsItem,
    FinanceNewsProvider, IndustryClassified, IwencaiOpenApi, JoinQuantProvider, NewsCapabilities,
    NewsDeliveryMode, NewsErrorKind, NewsProviderHealth, NewsTrustTier, SinaKline, TdxProvider,
    TencentKline, TushareProvider, FINANCE_NEWS_SOURCES,
};
pub use proxy::{ProxyConfig, ProxyRoute};
pub use security_master::SecurityMaster;
pub use validate::filter_valid_bars;
