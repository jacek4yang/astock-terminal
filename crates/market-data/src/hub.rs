//! `MarketData`: the user-facing composite that wires the upstreams
//! together exactly like the legacy `fetch_kline` pipeline.
//!
//! Kline strategy: Tencent (qfq, accurate prices) → Sina (unadjusted) →
//! TDX (unadjusted, TCP quote protocol) → EastMoney (last resort), then
//! validation filtering. The validated OHLCV base series is cached separately
//! so a broad scan can warm it without waiting for optional enrichment. A
//! detailed request reuses that base and then attempts EastMoney
//! amount/turnover enrichment once — skipped when EastMoney itself answered, with
//! `klt` matched to the requested period (fixing the legacy always-daily
//! enrichment bug). Quote snapshots fail over through the same chain
//! (Tencent/Sina answer `NoProvider` and are skipped, so effectively
//! TDX → EastMoney). JoinQuant is **not** in the automatic chain — it is an
//! explicit-call source only (strictly low-frequency).
//!
//! Hardening on top of the legacy behavior:
//! - a per-provider [`CircuitBreaker`] skips providers whose circuit is Open
//!   (e.g. the WAF-blocked Tencent endpoint) instead of paying the failing
//!   attempt on every request — see [`crate::breaker`];
//! - single-flight request coalescing: concurrent identical stock/index kline and quote
//!   calls share one in-flight upstream request (the stock page fires 3+
//!   identical kline fetches for the K线图 / 信号卡 / 缠论 views);
//! - `tracing::debug!` timing for every upstream attempt
//!   (provider/host/elapsed_ms/outcome).

use crate::breaker::{BreakerConfig, CircuitBreaker, ProviderHealth};
use crate::cache::{ttl, TtlCache};
use crate::http::HttpClient;
use crate::provider::DataProvider;
use crate::providers::{
    EastMoney, EmDataCenter, FinanceNewsProvider, GlobalAssetProvider, IwencaiOpenApi,
    JoinQuantProvider, SecEdgarProvider, SinaKline, TdxProvider, TencentKline, TushareProvider,
};
use crate::proxy::ProxyConfig;
use crate::security_master::SecurityMaster;
use crate::validate::{filter_valid_bars, filter_valid_index_bars};
use astock_core::{
    Adjust, Bar, DataError, Fetched, FundFlowPoint, KlinePeriod, MarketBreadth, MinuteData, Quote,
    SearchResult, Source, StockListItem, Symbol,
};
use astock_storage::Storage;
use async_trait::async_trait;
use dashmap::DashMap;
use futures::future::{BoxFuture, FutureExt, Shared};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tracing::{debug, warn};

type KlineOutcome = Result<Fetched<Vec<Bar>>, DataError>;
type QuoteOutcome = Result<Fetched<Quote>, DataError>;
type FundFlowOutcome = Result<Fetched<Vec<FundFlowPoint>>, DataError>;
type BreadthOutcome = Result<Fetched<MarketBreadth>, DataError>;
type SharedKline = Shared<BoxFuture<'static, KlineOutcome>>;
type SharedQuote = Shared<BoxFuture<'static, QuoteOutcome>>;
type SharedFundFlow = Shared<BoxFuture<'static, FundFlowOutcome>>;
type SharedBreadth = Shared<BoxFuture<'static, BreadthOutcome>>;

const MARKET_BREADTH_CACHE_KEY: &str = "market_breadth_composite";
const MARKET_BREADTH_LAST_GOOD_KEY: &str = "market_data.market_breadth.last_good.v1";
const MARKET_BREADTH_LAST_GOOD_MAX_AGE: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// Await the in-flight request registered under `key`, becoming the leader
/// (and running `make`) when none exists yet.
///
/// The leader removes the entry once the shared future resolves. If the
/// leader is cancelled mid-flight, any follower keeps driving the shared
/// future (polling any `Shared` handle polls the inner future); a lingering
/// completed entry simply serves its result instantly to the next caller,
/// which is equivalent to a zero-TTL cache hit.
async fn single_flight<T, M>(
    map: &DashMap<String, Shared<BoxFuture<'static, T>>>,
    key: String,
    make: M,
) -> T
where
    T: Clone + Send + 'static,
    M: FnOnce() -> BoxFuture<'static, T>,
{
    let (shared, leader) = match map.entry(key.clone()) {
        dashmap::Entry::Occupied(e) => (e.get().clone(), false),
        dashmap::Entry::Vacant(v) => {
            let shared = make().shared();
            v.insert(shared.clone());
            (shared, true)
        }
    };
    let out = shared.await;
    if leader {
        map.remove(&key);
    }
    out
}

/// Everything the kline/quote single-flight futures need, behind one `Arc`
/// so in-flight requests are `'static` (they outlive the borrow of `&self`).
struct Inner {
    /// Kline/quote failover chain in priority order
    /// (tencent → sina → tdx → eastmoney).
    chain: Vec<Arc<dyn DataProvider>>,
    /// Per-provider circuit breakers; Open providers are skipped.
    breakers: CircuitBreaker,
    cache: Arc<TtlCache>,
    eastmoney: Arc<EastMoney>,
    tdx: Arc<TdxProvider>,
    security_master: Arc<SecurityMaster>,
    storage: Option<Storage>,
    /// Optional enrichment is deliberately serialized. If EastMoney is
    /// unhealthy, one short probe opens its circuit and queued callers then
    /// continue immediately with valid base OHLCV data.
    enrichment_gate: Semaphore,
    fund_flow_gate: Semaphore,
    kline_inflight: DashMap<String, SharedKline>,
    enriched_kline_inflight: DashMap<String, SharedKline>,
    index_kline_inflight: DashMap<String, SharedKline>,
    quote_inflight: DashMap<String, SharedQuote>,
    fund_flow_inflight: DashMap<String, SharedFundFlow>,
    breadth_inflight: DashMap<String, SharedBreadth>,
}

impl Inner {
    fn new(
        chain: Vec<Arc<dyn DataProvider>>,
        cache: Arc<TtlCache>,
        eastmoney: Arc<EastMoney>,
        tdx: Arc<TdxProvider>,
        security_master: Arc<SecurityMaster>,
        breaker_config: BreakerConfig,
        storage: Option<Storage>,
    ) -> Arc<Self> {
        let breakers = CircuitBreaker::new(breaker_config);
        // Pre-register so the health panel lists every provider from boot.
        for p in &chain {
            breakers.register(p.name());
        }
        breakers.register("eastmoney_enrichment");
        breakers.register("eastmoney_fund_flow");
        breakers.register("eastmoney_market_breadth");
        breakers.register("tdx_market_breadth");
        Arc::new(Inner {
            chain,
            breakers,
            cache,
            eastmoney,
            tdx,
            security_master,
            storage,
            enrichment_gate: Semaphore::new(1),
            fund_flow_gate: Semaphore::new(1),
            kline_inflight: DashMap::new(),
            enriched_kline_inflight: DashMap::new(),
            index_kline_inflight: DashMap::new(),
            quote_inflight: DashMap::new(),
            fund_flow_inflight: DashMap::new(),
            breadth_inflight: DashMap::new(),
        })
    }

    /// One real base-kline fetch: failover chain gated by the breakers, then
    /// validation. Optional amount/turnover enrichment is a separate derived
    /// layer so broad scans can warm reusable OHLCV data at full width.
    /// Only successful, non-empty results are cached — failures (WAF,
    /// network, empty payloads) never enter the cache.
    async fn fetch_base_kline(
        &self,
        symbol: &Symbol,
        period: KlinePeriod,
        adjust: Adjust,
        count: u32,
    ) -> KlineOutcome {
        let mut failures = Vec::new();
        let mut fetched: Option<Fetched<Vec<Bar>>> = None;
        for provider in &self.chain {
            let name = provider.name();
            if !self.breakers.allow_request(name) {
                debug!(provider = name, "circuit open; skipping kline provider");
                failures.push(format!("{name}: circuit open"));
                continue;
            }
            let start = Instant::now();
            let attempt = provider.kline(symbol, period, adjust, count).await;
            let elapsed_ms = start.elapsed().as_millis() as u64;
            match attempt {
                Ok(f) if !f.data.is_empty() => {
                    self.breakers.on_success(name);
                    debug!(
                        provider = name,
                        host = provider.primary_host(),
                        elapsed_ms,
                        outcome = "ok",
                        bars = f.data.len(),
                        "kline upstream attempt"
                    );
                    fetched = Some(f);
                    break;
                }
                Ok(_) => {
                    // Responsive but no data: circuit-wise the provider is
                    // alive; failover-wise this attempt is a miss.
                    self.breakers.on_success(name);
                    debug!(
                        provider = name,
                        host = provider.primary_host(),
                        elapsed_ms,
                        outcome = "empty",
                        "kline upstream attempt"
                    );
                    failures.push(format!("{name}: empty"));
                }
                Err(DataError::NoProvider(_)) => {
                    debug!(
                        provider = name,
                        host = provider.primary_host(),
                        elapsed_ms,
                        outcome = "unsupported",
                        "kline upstream attempt"
                    );
                    failures.push(format!("{name}: unsupported period/adjust"));
                }
                Err(e) => {
                    self.breakers.on_failure(name, &e);
                    debug!(provider = name, host = provider.primary_host(), elapsed_ms, outcome = %e, "kline upstream attempt");
                    failures.push(format!("{name}: {e}"));
                }
            }
        }

        let fetched = fetched.ok_or_else(|| DataError::AllFailed {
            op: "kline",
            details: failures.join("; "),
        })?;

        // Legacy: with fewer than 10 bars, return as-is (no validation).
        if fetched.data.len() < 10 {
            warn!(%symbol, bars = fetched.data.len(), "kline returned very few bars");
            self.cache.set(
                &base_kline_cache_key(symbol, period, adjust, count),
                &fetched,
            );
            return Ok(fetched);
        }

        let validated = filter_valid_bars(symbol.code(), fetched.data);
        let out = Fetched {
            data: validated,
            source: fetched.source,
            fetched_at: fetched.fetched_at,
        };
        self.cache
            .set(&base_kline_cache_key(symbol, period, adjust, count), &out);
        Ok(out)
    }

    /// Best-effort enrichment for a detailed view. This never invalidates a
    /// valid base series: unavailable/slow optional data opens a dedicated
    /// circuit and the caller continues with OHLCV.
    async fn enrich_kline(
        &self,
        symbol: &Symbol,
        period: KlinePeriod,
        count: u32,
        bars: &mut [Bar],
    ) {
        if bars.is_empty() || !self.breakers.allow_request("eastmoney_enrichment") {
            return;
        }
        let Ok(_permit) = self.enrichment_gate.acquire().await else {
            return;
        };
        // Re-check after waiting: the caller ahead of us may have opened the
        // circuit, in which case this request must not join the failed queue.
        if !self.breakers.allow_request("eastmoney_enrichment") {
            return;
        }
        match tokio::time::timeout(
            Duration::from_secs(5),
            self.eastmoney.enrich(symbol, period, count, bars),
        )
        .await
        {
            Ok(Ok(matched)) => {
                self.breakers.on_success("eastmoney_enrichment");
                debug!(%symbol, matched, "eastmoney enrichment completed");
            }
            Ok(Err(error)) => {
                self.breakers.trip("eastmoney_enrichment");
                debug!(%symbol, %error, "eastmoney enrichment unavailable; continuing with base kline");
            }
            Err(_) => {
                self.breakers.trip("eastmoney_enrichment");
                debug!(%symbol, "eastmoney enrichment exceeded 5s; continuing with base kline");
            }
        }
    }

    /// Fund-flow is supplementary context, not a reason to hold a complete
    /// technical analysis behind an unhealthy host queue. Calls are
    /// serialized; one eight-second probe either succeeds or opens the
    /// dedicated circuit so waiting peers degrade immediately.
    async fn fetch_fund_flow_daily(&self, symbol: &Symbol, days: u32) -> FundFlowOutcome {
        if !self.breakers.allow_request("eastmoney_fund_flow") {
            return Err(DataError::AllFailed {
                op: "fund_flow_daily",
                details: "eastmoney: circuit open after an unresponsive probe".into(),
            });
        }
        let _permit = self
            .fund_flow_gate
            .acquire()
            .await
            .map_err(|_| DataError::NoProvider("fund_flow_daily"))?;
        if !self.breakers.allow_request("eastmoney_fund_flow") {
            return Err(DataError::AllFailed {
                op: "fund_flow_daily",
                details: "eastmoney: circuit open after an unresponsive probe".into(),
            });
        }
        match tokio::time::timeout(
            Duration::from_secs(8),
            self.eastmoney.fund_flow_daily(symbol, days),
        )
        .await
        {
            Ok(Ok(fetched)) => {
                self.breakers.on_success("eastmoney_fund_flow");
                Ok(fetched)
            }
            Ok(Err(error)) => {
                self.breakers.on_failure("eastmoney_fund_flow", &error);
                Err(error)
            }
            Err(_) => {
                self.breakers.trip("eastmoney_fund_flow");
                Err(DataError::Timeout(format!(
                    "eastmoney fund flow {}",
                    symbol.code()
                )))
            }
        }
    }

    async fn store_last_good_breadth(&self, fetched: &Fetched<MarketBreadth>) {
        self.cache.set(MARKET_BREADTH_CACHE_KEY, fetched);
        let Some(storage) = &self.storage else {
            return;
        };
        let Ok(value) = serde_json::to_string(fetched) else {
            return;
        };
        if let Err(error) = storage.kv_set(MARKET_BREADTH_LAST_GOOD_KEY, &value).await {
            debug!(%error, "failed to persist last-good market breadth");
        }
    }

    async fn load_last_good_breadth(&self) -> Option<Fetched<MarketBreadth>> {
        let storage = self.storage.as_ref()?;
        let row = match storage.kv_get(MARKET_BREADTH_LAST_GOOD_KEY).await {
            Ok(Some(row)) => row,
            Ok(None) => return None,
            Err(error) => {
                debug!(%error, "failed to read last-good market breadth");
                return None;
            }
        };
        let fetched: Fetched<MarketBreadth> = serde_json::from_str(&row.value).ok()?;
        if validate_market_breadth(&fetched.data).is_err() {
            return None;
        }
        let age_seconds = astock_core::time::utc_now()
            .signed_duration_since(fetched.fetched_at)
            .num_seconds();
        if age_seconds < 0 || age_seconds as u64 > MARKET_BREADTH_LAST_GOOD_MAX_AGE.as_secs() {
            return None;
        }
        Some(fetched)
    }

    /// Complete breadth pipeline: EastMoney whole-market snapshot with
    /// host-level retries, TDX batch-quote fallback, then a persisted
    /// last-good snapshot. There is deliberately no overall task timeout;
    /// every individual network request remains bounded by the HTTP layer.
    async fn fetch_market_breadth(&self) -> BreadthOutcome {
        let mut failures = Vec::new();
        if self.breakers.allow_request("eastmoney_market_breadth") {
            let started = Instant::now();
            match self.eastmoney.market_breadth().await {
                Ok(fetched) => match validate_market_breadth(&fetched.data) {
                    Ok(()) => {
                        self.breakers.on_success("eastmoney_market_breadth");
                        debug!(
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            total = fetched.data.total,
                            "EastMoney market breadth completed"
                        );
                        self.store_last_good_breadth(&fetched).await;
                        return Ok(fetched);
                    }
                    Err(error) => {
                        self.breakers.trip("eastmoney_market_breadth");
                        failures.push(format!("eastmoney validation: {error}"));
                    }
                },
                Err(error) => {
                    self.breakers.trip("eastmoney_market_breadth");
                    failures.push(format!("eastmoney retries exhausted: {error}"));
                }
            }
        } else {
            failures.push("eastmoney: circuit open".to_string());
        }

        if self.breakers.allow_request("tdx_market_breadth") {
            let started = Instant::now();
            match self.tdx.market_breadth().await {
                Ok(fetched) => match validate_market_breadth(&fetched.data) {
                    Ok(()) => {
                        self.breakers.on_success("tdx_market_breadth");
                        debug!(
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            total = fetched.data.total,
                            "TDX market breadth fallback completed"
                        );
                        self.store_last_good_breadth(&fetched).await;
                        return Ok(fetched);
                    }
                    Err(error) => {
                        self.breakers.trip("tdx_market_breadth");
                        failures.push(format!("tdx validation: {error}"));
                    }
                },
                Err(error) => {
                    self.breakers.trip("tdx_market_breadth");
                    failures.push(format!("tdx retries exhausted: {error}"));
                }
            }
        } else {
            failures.push("tdx: circuit open".to_string());
        }

        if let Some(last_good) = self.load_last_good_breadth().await {
            debug!(source = %last_good.source, fetched_at = %last_good.fetched_at, "using persisted last-good market breadth");
            self.cache.set(MARKET_BREADTH_CACHE_KEY, &last_good);
            return Ok(last_good);
        }
        Err(DataError::AllFailed {
            op: "market_breadth",
            details: failures.join("; "),
        })
    }

    /// One real quote fetch: the same breaker-gated failover chain as
    /// kline. Providers without a quote capability answer `NoProvider` and
    /// are skipped (Tencent/Sina), so this is effectively TDX → EastMoney.
    async fn fetch_quote(&self, symbol: &Symbol) -> QuoteOutcome {
        let mut failures = Vec::new();
        let mut base = None;
        for provider in &self.chain {
            let name = provider.name();
            if !self.breakers.allow_request(name) {
                debug!(provider = name, "circuit open; skipping quote provider");
                failures.push(format!("{name}: circuit open"));
                continue;
            }
            let start = Instant::now();
            let attempt = provider.quote(symbol).await;
            let elapsed_ms = start.elapsed().as_millis() as u64;
            match attempt {
                Ok(f) => {
                    self.breakers.on_success(name);
                    debug!(
                        provider = name,
                        host = provider.primary_host(),
                        elapsed_ms,
                        outcome = "ok",
                        "quote upstream attempt"
                    );
                    base = Some(f);
                    break;
                }
                Err(DataError::NoProvider(_)) => {
                    debug!(
                        provider = name,
                        host = provider.primary_host(),
                        elapsed_ms,
                        outcome = "unsupported",
                        "quote upstream attempt"
                    );
                }
                Err(e) => {
                    self.breakers.on_failure(name, &e);
                    debug!(provider = name, host = provider.primary_host(), elapsed_ms, outcome = %e, "quote upstream attempt");
                    failures.push(format!("{name}: {e}"));
                }
            }
        }
        let mut out = base.ok_or_else(|| DataError::AllFailed {
            op: "quote",
            details: failures.join("; "),
        })?;

        // Identity is reference data, not a property of a particular quote
        // response. Lazily hydrate the complete TDX exchange list when this
        // process has not seen the code yet.
        if self.security_master.get(symbol.code()).is_none() {
            if let Ok(list) = self.tdx.all_a_shares().await {
                self.security_master
                    .merge_stock_list(&list.data, &list.source.to_string());
            }
        }

        // TDX owns the fast price/order snapshot, while EastMoney can fill
        // fields TDX does not publish. A supplementary provider failure does
        // not discard the valid TDX snapshot.
        if out.source != Source::EastMoney
            && (out.data.turnover.is_none() || out.data.name.is_empty())
        {
            let start = Instant::now();
            match self.eastmoney.quote(symbol).await {
                Ok(supplement) => {
                    self.breakers.on_success("eastmoney");
                    if !supplement.data.name.trim().is_empty() {
                        self.security_master.upsert(
                            astock_core::SecurityMasterRecord::listed_stock(
                                symbol.code(),
                                supplement.data.name.clone(),
                                "eastmoney_quote",
                            ),
                        );
                    }
                    if out.data.turnover.is_none() {
                        out.data.turnover = supplement.data.turnover;
                        if let Some(provenance) = supplement.data.field_provenance.get("turnover") {
                            out.data
                                .field_provenance
                                .insert("turnover".to_string(), provenance.clone());
                        }
                    }
                    debug!(
                        provider = "eastmoney",
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        outcome = "supplemented",
                        "quote field supplement"
                    );
                }
                Err(error) => {
                    self.breakers.on_failure("eastmoney", &error);
                    debug!(
                        provider = "eastmoney",
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        outcome = %error,
                        "quote field supplement unavailable"
                    );
                }
            }
        }

        if let Some(record) = self.security_master.get(symbol.code()) {
            out.data.name = record.canonical_name;
            out.data.field_provenance.insert(
                "name".to_string(),
                astock_core::FieldProvenance::reference(record.source, record.refreshed_at),
            );
        }
        Ok(out)
    }
}

fn kline_cache_key(symbol: &Symbol, period: KlinePeriod, adjust: Adjust, count: u32) -> String {
    format!("kline_{symbol}_{count}_{period:?}_{adjust:?}")
}

fn base_kline_cache_key(
    symbol: &Symbol,
    period: KlinePeriod,
    adjust: Adjust,
    count: u32,
) -> String {
    format!("kline_base_{symbol}_{count}_{period:?}_{adjust:?}")
}

fn index_kline_cache_key(index_secid: &str, period: KlinePeriod, count: u32) -> String {
    format!("index_kline_{index_secid}_{period:?}_{count}")
}

fn validate_market_breadth(breadth: &MarketBreadth) -> Result<(), DataError> {
    let counted = breadth.up + breadth.down + breadth.flat;
    if breadth.total != counted {
        return Err(DataError::Parse {
            upstream: "market breadth".to_string(),
            message: format!(
                "total {} does not match up/down/flat sum {counted}",
                breadth.total
            ),
        });
    }
    if breadth.total < 4_000 {
        return Err(DataError::Empty(format!(
            "market breadth incomplete: only {} stocks",
            breadth.total
        )));
    }
    Ok(())
}

/// Credential material supplied by Engine after a direct Windows Credential
/// Manager read. This type intentionally does not implement `Debug` or
/// serialization and must never cross IPC.
#[derive(Clone, Default)]
pub struct MarketDataCredentials {
    tushare_token: Option<String>,
    iwencai_key: Option<String>,
    sec_edgar_user_agent: Option<String>,
    socks5: Option<String>,
}

impl MarketDataCredentials {
    /// Construct the non-serializable credential bundle consumed once by
    /// [`MarketData`]. Values remain private to prevent ad-hoc diagnostics.
    pub fn new(
        tushare_token: Option<String>,
        iwencai_key: Option<String>,
        sec_edgar_user_agent: Option<String>,
        socks5: Option<String>,
    ) -> Self {
        Self {
            tushare_token,
            iwencai_key,
            sec_edgar_user_agent,
            socks5,
        }
    }
}

/// Composite market-data facade: kline failover + breaker + single-flight,
/// everything else delegated to EastMoney.
#[derive(Clone)]
pub struct MarketData {
    /// Shared HTTP layer (exposed for diagnostics such as `current_delay`).
    pub http: Arc<HttpClient>,
    /// Shared TTL cache.
    pub cache: Arc<TtlCache>,
    /// Tencent kline adapter.
    pub tencent: Arc<TencentKline>,
    /// Sina kline adapter.
    pub sina: Arc<SinaKline>,
    /// EastMoney adapter.
    pub eastmoney: Arc<EastMoney>,
    /// EastMoney datacenter reports (billboard/block-trade/margin/survey/
    /// holder-num/earnings-predict/lift/suspension/notices/limit-up pools/boards).
    pub em_datacenter: Arc<EmDataCenter>,
    /// TDX (通达信) adapter; its server pool is probed lazily on first use.
    pub tdx: Arc<TdxProvider>,
    /// Optional JoinQuant adapter (configured in memory by Engine);
    /// `available() == false` without them. Explicit-call source only —
    /// never in the automatic failover chain.
    pub joinquant: Arc<JoinQuantProvider>,
    /// Optional Tushare pro adapter (token injected in memory by Engine);
    /// `available() == false` when no token is configured.
    pub tushare: Arc<TushareProvider>,
    /// Optional iwencai OpenAPI adapter (key injected in memory by Engine).
    pub iwencai: Arc<IwencaiOpenApi>,
    /// Optional SEC EDGAR adapter with an in-memory Fair Access identity.
    pub sec_edgar: Arc<SecEdgarProvider>,
    /// Public, credential-free finance headlines with bounded caching/retry.
    pub finance_news: Arc<FinanceNewsProvider>,
    /// Cross-market gold quotes and bounded daily trend history.
    pub global_assets: Arc<GlobalAssetProvider>,
    /// Canonical security identity and classification index.
    pub security_master: Arc<SecurityMaster>,
    inner: Arc<Inner>,
}

impl Default for MarketData {
    fn default() -> Self {
        Self::new()
    }
}

impl MarketData {
    /// Build the full stack with a fresh shared HTTP client and cache.
    pub fn new() -> Self {
        Self::with_credentials(MarketDataCredentials::default())
    }

    /// Build from explicit in-memory credentials. Intended for Engine and
    /// isolated live tests; production callers must source these values from
    /// Windows Credential Manager.
    pub fn with_credentials(credentials: MarketDataCredentials) -> Self {
        let http = Arc::new(HttpClient::with_proxy(ProxyConfig::with_socks5(
            credentials.socks5.clone(),
        )));
        Self::build(
            http,
            Arc::new(TtlCache::default()),
            None,
            BreakerConfig::default(),
            None,
            credentials,
        )
    }

    /// Build from existing shared components.
    pub fn with_shared(http: Arc<HttpClient>, cache: Arc<TtlCache>) -> Self {
        Self::build(
            http,
            cache,
            None,
            BreakerConfig::default(),
            None,
            MarketDataCredentials::default(),
        )
    }

    /// Production constructor with persistent news cursors, provider enable
    /// flags and last-good snapshots in the shared application storage.
    pub fn with_storage(storage: Storage) -> Self {
        Self::with_storage_and_credentials(storage, MarketDataCredentials::default())
    }

    /// Production constructor with explicit credential material supplied by
    /// Engine's Credential Manager boundary.
    pub fn with_storage_and_credentials(
        storage: Storage,
        credentials: MarketDataCredentials,
    ) -> Self {
        let http = Arc::new(HttpClient::with_proxy(ProxyConfig::with_socks5(
            credentials.socks5.clone(),
        )));
        Self::build(
            http,
            Arc::new(TtlCache::default()),
            None,
            BreakerConfig::default(),
            Some(storage),
            credentials,
        )
    }

    /// Test/diagnostic constructor: custom kline failover chain and breaker
    /// tuning (stub providers, shortened cooldowns) on a fresh HTTP/cache.
    pub fn with_kline_chain(
        chain: Vec<Arc<dyn DataProvider>>,
        breaker_config: BreakerConfig,
    ) -> Self {
        Self::build(
            Arc::new(HttpClient::new()),
            Arc::new(TtlCache::default()),
            Some(chain),
            breaker_config,
            None,
            MarketDataCredentials::default(),
        )
    }

    fn build(
        http: Arc<HttpClient>,
        cache: Arc<TtlCache>,
        chain: Option<Vec<Arc<dyn DataProvider>>>,
        breaker_config: BreakerConfig,
        storage: Option<Storage>,
        credentials: MarketDataCredentials,
    ) -> Self {
        let MarketDataCredentials {
            tushare_token,
            iwencai_key,
            sec_edgar_user_agent,
            socks5: _,
        } = credentials;
        let tencent = Arc::new(TencentKline::new(http.clone()));
        let sina = Arc::new(SinaKline::new(http.clone()));
        let eastmoney = Arc::new(EastMoney::new(http.clone(), cache.clone()));
        let em_datacenter = Arc::new(EmDataCenter::new(http.clone(), cache.clone()));
        let tdx = Arc::new(TdxProvider::new());
        let security_master = Arc::new(SecurityMaster::default());
        let joinquant = Arc::new(JoinQuantProvider::new(None));
        let tushare = Arc::new(TushareProvider::new(
            http.clone(),
            cache.clone(),
            tushare_token,
        ));
        let iwencai = Arc::new(IwencaiOpenApi::new(
            http.clone(),
            cache.clone(),
            iwencai_key,
        ));
        let sec_edgar = Arc::new(SecEdgarProvider::new(http.clone(), sec_edgar_user_agent));
        let finance_news = Arc::new(match storage.clone() {
            Some(storage) => FinanceNewsProvider::with_storage(
                http.clone(),
                cache.clone(),
                storage,
                em_datacenter.clone(),
            ),
            None => FinanceNewsProvider::new(http.clone(), cache.clone()),
        });
        let global_assets = Arc::new(GlobalAssetProvider::new(http.clone(), cache.clone()));
        let chain = chain.unwrap_or_else(|| {
            vec![
                tencent.clone() as Arc<dyn DataProvider>,
                sina.clone() as Arc<dyn DataProvider>,
                tdx.clone() as Arc<dyn DataProvider>,
                eastmoney.clone() as Arc<dyn DataProvider>,
            ]
        });
        let inner = Inner::new(
            chain,
            cache.clone(),
            eastmoney.clone(),
            tdx.clone(),
            security_master.clone(),
            breaker_config,
            storage,
        );
        // Optional token-gated providers: always on the health panel, marked
        // unavailable (and refused traffic) when their token/key is missing.
        for (name, available) in [
            ("tushare", tushare.available()),
            ("iwencai", iwencai.available()),
            ("joinquant", joinquant.available()),
        ] {
            inner.breakers.register(name);
            inner.breakers.set_available(name, available);
        }
        MarketData {
            inner,
            http,
            cache,
            tencent,
            sina,
            eastmoney,
            em_datacenter,
            tdx,
            joinquant,
            tushare,
            iwencai,
            sec_edgar,
            finance_news,
            global_assets,
            security_master,
        }
    }

    /// Per-provider circuit-breaker snapshot for the data-source health
    /// panel: name, state (closed/open/half_open), remaining cooldown.
    pub fn provider_health(&self) -> Vec<ProviderHealth> {
        self.inner.breakers.health()
    }

    /// Kline/quote failover chain in priority order (diagnostics + tests).
    pub fn chain_names(&self) -> Vec<&'static str> {
        self.inner.chain.iter().map(|p| p.name()).collect()
    }

    /// Read the exact cache entry first, then reuse a fresh longer series when
    /// one was pre-warmed. A 250-bar scan can therefore satisfy later 60/120-
    /// bar requests without another API call.
    fn cached_kline(
        &self,
        symbol: &Symbol,
        period: KlinePeriod,
        adjust: Adjust,
        count: u32,
        base: bool,
    ) -> Option<Fetched<Vec<Bar>>> {
        let mut candidates = vec![count];
        for candidate in [500_u32, 250, 120] {
            if candidate > count && !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
        for candidate in candidates {
            let key = if base {
                base_kline_cache_key(symbol, period, adjust, candidate)
            } else {
                kline_cache_key(symbol, period, adjust, candidate)
            };
            let ttl = if base { ttl::KLINE_BASE } else { ttl::KLINE };
            let Some(mut hit) = self.cache.get::<Fetched<Vec<Bar>>>(&key, ttl) else {
                continue;
            };
            if candidate == count || hit.data.len() >= count as usize {
                if hit.data.len() > count as usize {
                    hit.data = hit.data.split_off(hit.data.len() - count as usize);
                }
                return Some(hit);
            }
        }
        None
    }

    /// Base OHLCV pipeline: reusable cache lookup, then single-flight over
    /// the breaker-gated failover chain.
    async fn base_kline_pipeline(
        &self,
        symbol: &Symbol,
        period: KlinePeriod,
        adjust: Adjust,
        count: u32,
    ) -> KlineOutcome {
        if let Some(hit) = self.cached_kline(symbol, period, adjust, count, true) {
            return Ok(hit);
        }

        let inner = self.inner.clone();
        let symbol = symbol.clone();
        let sf_key = format!("kline_base|{symbol}|{period:?}|{adjust:?}|{count}");
        single_flight(&self.inner.kline_inflight, sf_key, move || {
            async move { inner.fetch_base_kline(&symbol, period, adjust, count).await }.boxed()
        })
        .await
    }

    /// Detailed Kline pipeline. It reuses the warmed base series and only
    /// derives optional amount/turnover fields once per cache window.
    async fn kline_pipeline(
        &self,
        symbol: &Symbol,
        period: KlinePeriod,
        adjust: Adjust,
        count: u32,
    ) -> KlineOutcome {
        if let Some(hit) = self.cached_kline(symbol, period, adjust, count, false) {
            return Ok(hit);
        }
        let market = self.clone();
        let inner = self.inner.clone();
        let symbol = symbol.clone();
        let sf_key = format!("kline_enriched|{symbol}|{period:?}|{adjust:?}|{count}");
        single_flight(&self.inner.enriched_kline_inflight, sf_key, move || {
            async move {
                let mut fetched = market
                    .base_kline_pipeline(&symbol, period, adjust, count)
                    .await?;
                if fetched.source != Source::EastMoney
                    && fetched
                        .data
                        .iter()
                        .any(|bar| bar.amount.is_none() || bar.turnover.is_none())
                {
                    inner
                        .enrich_kline(&symbol, period, count, &mut fetched.data)
                        .await;
                }
                inner
                    .cache
                    .set(&kline_cache_key(&symbol, period, adjust, count), &fetched);
                Ok(fetched)
            }
            .boxed()
        })
        .await
    }

    async fn index_kline_period_pipeline(
        &self,
        index_secid: &str,
        period: KlinePeriod,
        count: u32,
    ) -> KlineOutcome {
        let cache_key = index_kline_cache_key(index_secid, period, count);
        if let Some(hit) = self.cache.get::<Fetched<Vec<Bar>>>(&cache_key, ttl::KLINE) {
            return Ok(hit);
        }

        let eastmoney = self.eastmoney.clone();
        let tencent = self.tencent.clone();
        let cache = self.cache.clone();
        let index_secid = index_secid.to_string();
        let sf_key = format!("index_kline|{index_secid}|{period:?}|{count}");
        single_flight(&self.inner.index_kline_inflight, sf_key, move || {
            async move {
                let em_attempt = tokio::time::timeout(
                    Duration::from_secs(5),
                    eastmoney.index_kline_period(&index_secid, period, count),
                )
                .await;
                let fetched = match em_attempt {
                    Ok(Ok(Fetched {
                        data,
                        source,
                        fetched_at,
                    })) => Fetched {
                        data: filter_valid_index_bars(&index_secid, data),
                        source,
                        fetched_at,
                    },
                    Ok(Err(error)) => {
                        debug!(%error, %index_secid, ?period, "EM index kline failed, trying tencent");
                        let index_code = index_secid.split('.').next_back().unwrap_or(&index_secid);
                        let bars = tencent
                            .index_kline_period(index_code, period, count)
                            .await?;
                        let validated = filter_valid_index_bars(&index_secid, bars);
                        let required = (count as usize).min(10);
                        if validated.len() < required {
                            return Err(DataError::Empty(format!(
                                "index kline {index_secid}: {} bars",
                                validated.len()
                            )));
                        }
                        Fetched::now(validated, Source::Tencent)
                    }
                    Err(_) => {
                        debug!(%index_secid, ?period, "EM index kline probe exceeded 5s, trying tencent");
                        let index_code = index_secid.split('.').next_back().unwrap_or(&index_secid);
                        let bars = tencent
                            .index_kline_period(index_code, period, count)
                            .await?;
                        let validated = filter_valid_index_bars(&index_secid, bars);
                        let required = (count as usize).min(10);
                        if validated.len() < required {
                            return Err(DataError::Empty(format!(
                                "index kline {index_secid}: {} bars",
                                validated.len()
                            )));
                        }
                        Fetched::now(validated, Source::Tencent)
                    }
                };
                if fetched.data.is_empty() {
                    return Err(DataError::Empty(format!(
                        "index kline {index_secid}: 0 bars"
                    )));
                }
                cache.set(
                    &index_kline_cache_key(&index_secid, period, count),
                    &fetched,
                );
                Ok(fetched)
            }
            .boxed()
        })
        .await
    }
}

#[async_trait]
impl DataProvider for MarketData {
    fn name(&self) -> &'static str {
        "composite"
    }

    async fn kline(
        &self,
        symbol: &Symbol,
        period: KlinePeriod,
        adjust: Adjust,
        count: u32,
    ) -> KlineOutcome {
        if symbol.is_unambiguous_index() {
            return self
                .index_kline_period_pipeline(&Symbol::index_secid(symbol.code()), period, count)
                .await;
        }
        self.kline_pipeline(symbol, period, adjust, count).await
    }

    async fn scan_kline(
        &self,
        symbol: &Symbol,
        period: KlinePeriod,
        adjust: Adjust,
        count: u32,
    ) -> KlineOutcome {
        if symbol.is_unambiguous_index() {
            return self
                .index_kline_period_pipeline(&Symbol::index_secid(symbol.code()), period, count)
                .await;
        }
        self.base_kline_pipeline(symbol, period, adjust, count)
            .await
    }

    async fn quote(&self, symbol: &Symbol) -> Result<Fetched<Quote>, DataError> {
        // Single-flight coalescing only — no TTL cache, quote freshness
        // semantics are unchanged from the legacy pass-through. The fetch
        // itself fails over through the breaker-gated chain (tdx → eastmoney;
        // tencent/sina answer NoProvider and are skipped).
        let inner = self.inner.clone();
        let eastmoney = self.eastmoney.clone();
        let symbol = symbol.clone();
        let sf_key = format!("quote|{symbol}");
        single_flight(&self.inner.quote_inflight, sf_key, move || {
            async move {
                if symbol.is_unambiguous_index() {
                    eastmoney.index_quote(symbol.code()).await
                } else {
                    inner.fetch_quote(&symbol).await
                }
            }
            .boxed()
        })
        .await
    }

    async fn search(&self, keyword: &str) -> Result<Fetched<Vec<SearchResult>>, DataError> {
        let local = self.security_master.search(keyword, 10);
        match self.eastmoney.search(keyword).await {
            Ok(mut fetched) => {
                for hit in &mut fetched.data {
                    if let Some(record) = self.security_master.get(&hit.code) {
                        hit.name = record.canonical_name;
                    } else if !hit.name.trim().is_empty() {
                        self.security_master.upsert(
                            astock_core::SecurityMasterRecord::listed_stock(
                                hit.code.clone(),
                                hit.name.clone(),
                                "eastmoney_search",
                            ),
                        );
                    }
                }
                if fetched.data.is_empty() && !local.is_empty() {
                    Ok(Fetched::now(local, Source::Tdx))
                } else {
                    Ok(fetched)
                }
            }
            Err(error) if !local.is_empty() => {
                debug!(%error, "remote search unavailable; using security master");
                Ok(Fetched::now(local, Source::Tdx))
            }
            Err(error) => Err(error),
        }
    }

    async fn fund_flow_daily(
        &self,
        symbol: &Symbol,
        days: u32,
    ) -> Result<Fetched<Vec<FundFlowPoint>>, DataError> {
        let inner = self.inner.clone();
        let symbol = symbol.clone();
        let sf_key = format!("fund_flow_daily|{symbol}|{days}");
        single_flight(&self.inner.fund_flow_inflight, sf_key, move || {
            async move { inner.fetch_fund_flow_daily(&symbol, days).await }.boxed()
        })
        .await
    }

    async fn fund_flow_realtime(
        &self,
        symbol: &Symbol,
    ) -> Result<Fetched<Vec<FundFlowPoint>>, DataError> {
        self.eastmoney.fund_flow_realtime(symbol).await
    }

    async fn minute(&self, symbol: &Symbol) -> Result<Fetched<MinuteData>, DataError> {
        self.eastmoney.minute(symbol).await
    }

    async fn all_a_shares(&self) -> Result<Fetched<Vec<StockListItem>>, DataError> {
        // Do not wrap the complete host-retry sequence in a shorter outer
        // timeout. Each HTTP request is already bounded; cancelling the
        // sequence used to prevent the second/third host from ever running.
        let fetched = match self.eastmoney.all_a_shares().await {
            Ok(fetched) => fetched,
            Err(error) => {
                debug!(%error, "EastMoney A-share list retries exhausted; using TDX security list");
                self.tdx.all_a_shares().await?
            }
        };
        self.security_master
            .merge_stock_list(&fetched.data, &fetched.source.to_string());
        Ok(fetched)
    }

    async fn market_breadth(&self) -> Result<Fetched<MarketBreadth>, DataError> {
        if let Some(hit) = self
            .cache
            .get::<Fetched<MarketBreadth>>(MARKET_BREADTH_CACHE_KEY, ttl::BREADTH)
        {
            return Ok(hit);
        }
        let inner = self.inner.clone();
        single_flight(
            &self.inner.breadth_inflight,
            MARKET_BREADTH_CACHE_KEY.to_string(),
            move || async move { inner.fetch_market_breadth().await }.boxed(),
        )
        .await
    }

    async fn index_kline(
        &self,
        index_secid: &str,
        count: u32,
    ) -> Result<Fetched<Vec<Bar>>, DataError> {
        self.index_kline_period_pipeline(index_secid, KlinePeriod::Day, count)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breadth_validation_requires_complete_consistent_counts() {
        assert!(validate_market_breadth(&MarketBreadth {
            up: 2_100,
            down: 2_000,
            flat: 900,
            total: 5_000,
        })
        .is_ok());
        assert!(validate_market_breadth(&MarketBreadth {
            up: 2_100,
            down: 2_000,
            flat: 900,
            total: 4_999,
        })
        .is_err());
        assert!(validate_market_breadth(&MarketBreadth {
            up: 1_500,
            down: 1_400,
            flat: 100,
            total: 3_000,
        })
        .is_err());
    }

    #[tokio::test]
    async fn last_good_breadth_survives_runtime_restart() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(astock_storage::StorageConfig::with_base_dir(dir.path()))
            .expect("storage opens");
        let first = MarketData::with_storage(storage.clone());
        let expected = Fetched::now(
            MarketBreadth {
                up: 2_100,
                down: 2_000,
                flat: 900,
                total: 5_000,
            },
            Source::EastMoney,
        );
        first.inner.store_last_good_breadth(&expected).await;

        let restarted = MarketData::with_storage(storage);
        let restored = restarted
            .inner
            .load_last_good_breadth()
            .await
            .expect("last-good snapshot restored");
        assert_eq!(restored, expected);
    }

    #[test]
    fn default_failover_chain_order() {
        let md = MarketData::new();
        assert_eq!(
            md.chain_names(),
            vec!["tencent", "sina", "tdx", "eastmoney"],
            "kline/quote failover chain must be tencent → sina → tdx → eastmoney"
        );
    }

    #[test]
    fn health_panel_lists_tdx_and_joinquant() {
        let md = MarketData::new();
        let health = md.provider_health();
        let tdx = health
            .iter()
            .find(|h| h.name == "tdx")
            .expect("tdx missing from health panel");
        assert!(tdx.available, "tdx is not credential-gated");
        let jq = health
            .iter()
            .find(|h| h.name == "joinquant")
            .expect("joinquant missing from health panel");
        assert_eq!(jq.available, md.joinquant.available());
    }

    #[test]
    fn joinquant_not_in_failover_chain() {
        let md = MarketData::new();
        assert!(
            !md.chain_names().contains(&"joinquant"),
            "joinquant is explicit-call only, never in the automatic chain"
        );
    }
}
