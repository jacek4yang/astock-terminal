//! `MarketData`: the user-facing composite that wires the upstreams
//! together exactly like the legacy `fetch_kline` pipeline.
//!
//! Kline strategy: Tencent (qfq, accurate prices) → Sina (unadjusted) →
//! TDX (unadjusted, TCP quote protocol) → EastMoney (last resort), then
//! validation filtering, then EastMoney amount/turnover enrichment — skipped
//! when EastMoney itself answered (fixing the legacy double-fetch), with
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
//! - single-flight request coalescing: concurrent identical kline/quote
//!   calls share one in-flight upstream request (the stock page fires 3+
//!   identical kline fetches for the K线图 / 信号卡 / 缠论 views);
//! - `tracing::debug!` timing for every upstream attempt
//!   (provider/host/elapsed_ms/outcome).

use crate::breaker::{BreakerConfig, CircuitBreaker, ProviderHealth};
use crate::cache::{ttl, TtlCache};
use crate::http::HttpClient;
use crate::provider::DataProvider;
use crate::providers::{
    EastMoney, EmDataCenter, FinanceNewsProvider, IwencaiOpenApi, JoinQuantProvider, SinaKline,
    TdxProvider, TencentKline, TushareProvider,
};
use crate::security_master::SecurityMaster;
use crate::validate::{filter_valid_bars, filter_valid_index_bars};
use astock_core::{
    Adjust, Bar, DataError, Fetched, FundFlowPoint, KlinePeriod, MarketBreadth, MinuteData, Quote,
    SearchResult, Source, StockListItem, Symbol,
};
use async_trait::async_trait;
use dashmap::DashMap;
use futures::future::{BoxFuture, FutureExt, Shared};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, warn};

type KlineOutcome = Result<Fetched<Vec<Bar>>, DataError>;
type QuoteOutcome = Result<Fetched<Quote>, DataError>;
type SharedKline = Shared<BoxFuture<'static, KlineOutcome>>;
type SharedQuote = Shared<BoxFuture<'static, QuoteOutcome>>;

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
    kline_inflight: DashMap<String, SharedKline>,
    quote_inflight: DashMap<String, SharedQuote>,
}

impl Inner {
    fn new(
        chain: Vec<Arc<dyn DataProvider>>,
        cache: Arc<TtlCache>,
        eastmoney: Arc<EastMoney>,
        tdx: Arc<TdxProvider>,
        security_master: Arc<SecurityMaster>,
        breaker_config: BreakerConfig,
    ) -> Arc<Self> {
        let breakers = CircuitBreaker::new(breaker_config);
        // Pre-register so the health panel lists every provider from boot.
        for p in &chain {
            breakers.register(p.name());
        }
        Arc::new(Inner {
            chain,
            breakers,
            cache,
            eastmoney,
            tdx,
            security_master,
            kline_inflight: DashMap::new(),
            quote_inflight: DashMap::new(),
        })
    }

    /// One real kline fetch: failover chain gated by the breakers, then
    /// validation and (for non-EM sources) EM amount/turnover enrichment.
    /// Only successful, non-empty results are cached — failures (WAF,
    /// network, empty payloads) never enter the cache.
    async fn fetch_kline(
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

        // Legacy: with fewer than 10 bars, return as-is (no validation,
        // no enrichment).
        if fetched.data.len() < 10 {
            warn!(%symbol, bars = fetched.data.len(), "kline returned very few bars");
            self.cache
                .set(&kline_cache_key(symbol, period, adjust, count), &fetched);
            return Ok(fetched);
        }

        let from_eastmoney = fetched.source == Source::EastMoney;
        let validated = filter_valid_bars(symbol.code(), fetched.data);
        let mut out = Fetched {
            data: validated,
            source: fetched.source,
            fetched_at: fetched.fetched_at,
        };

        // Enrichment: EM supplies amount/turnover for Tencent/Sina bars.
        // Skipped when EM was already the source (its rows carry both) —
        // the legacy code fetched EM a second time here.
        if !from_eastmoney {
            self.eastmoney
                .enrich(symbol, period, count, &mut out.data)
                .await;
        }

        self.cache
            .set(&kline_cache_key(symbol, period, adjust, count), &out);
        Ok(out)
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

/// Composite market-data facade: kline failover + breaker + single-flight,
/// everything else delegated to EastMoney.
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
    /// Optional JoinQuant adapter (credentials from `JQ_USER`/`JQ_PWD`);
    /// `available() == false` without them. Explicit-call source only —
    /// never in the automatic failover chain.
    pub joinquant: Arc<JoinQuantProvider>,
    /// Optional Tushare pro adapter (token from `TUSHARE_TOKEN`);
    /// `available() == false` when no token is configured.
    pub tushare: Arc<TushareProvider>,
    /// Optional iwencai OpenAPI adapter (key from `IWENCAI_KEY`).
    pub iwencai: Arc<IwencaiOpenApi>,
    /// Public, credential-free finance headlines with bounded caching/retry.
    pub finance_news: Arc<FinanceNewsProvider>,
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
        Self::with_shared(Arc::new(HttpClient::new()), Arc::new(TtlCache::default()))
    }

    /// Build from existing shared components.
    pub fn with_shared(http: Arc<HttpClient>, cache: Arc<TtlCache>) -> Self {
        Self::build(http, cache, None, BreakerConfig::default())
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
        )
    }

    fn build(
        http: Arc<HttpClient>,
        cache: Arc<TtlCache>,
        chain: Option<Vec<Arc<dyn DataProvider>>>,
        breaker_config: BreakerConfig,
    ) -> Self {
        let tencent = Arc::new(TencentKline::new(http.clone()));
        let sina = Arc::new(SinaKline::new(http.clone()));
        let eastmoney = Arc::new(EastMoney::new(http.clone(), cache.clone()));
        let em_datacenter = Arc::new(EmDataCenter::new(http.clone(), cache.clone()));
        let tdx = Arc::new(TdxProvider::new());
        let security_master = Arc::new(SecurityMaster::default());
        let joinquant = Arc::new(JoinQuantProvider::from_env());
        let tushare = Arc::new(TushareProvider::from_env(http.clone(), cache.clone()));
        let iwencai = Arc::new(IwencaiOpenApi::from_env(http.clone(), cache.clone()));
        let finance_news = Arc::new(FinanceNewsProvider::new(http.clone(), cache.clone()));
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
            finance_news,
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

    /// Kline pipeline: cache lookup, then single-flight over the breaker-
    /// gated failover chain. Concurrent identical calls share one upstream
    /// request keyed by method+symbol+period+adjust+count.
    async fn kline_pipeline(
        &self,
        symbol: &Symbol,
        period: KlinePeriod,
        adjust: Adjust,
        count: u32,
    ) -> KlineOutcome {
        let key = kline_cache_key(symbol, period, adjust, count);
        if let Some(hit) = self.cache.get::<Fetched<Vec<Bar>>>(&key, ttl::KLINE) {
            return Ok(hit);
        }

        let inner = self.inner.clone();
        let symbol = symbol.clone();
        let sf_key = format!("kline|{symbol}|{period:?}|{adjust:?}|{count}");
        single_flight(&self.inner.kline_inflight, sf_key, move || {
            async move { inner.fetch_kline(&symbol, period, adjust, count).await }.boxed()
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
        self.kline_pipeline(symbol, period, adjust, count).await
    }

    async fn quote(&self, symbol: &Symbol) -> Result<Fetched<Quote>, DataError> {
        // Single-flight coalescing only — no TTL cache, quote freshness
        // semantics are unchanged from the legacy pass-through. The fetch
        // itself fails over through the breaker-gated chain (tdx → eastmoney;
        // tencent/sina answer NoProvider and are skipped).
        let inner = self.inner.clone();
        let symbol = symbol.clone();
        let sf_key = format!("quote|{symbol}");
        single_flight(&self.inner.quote_inflight, sf_key, move || {
            async move { inner.fetch_quote(&symbol).await }.boxed()
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
        self.eastmoney.fund_flow_daily(symbol, days).await
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
        let fetched = match self.eastmoney.all_a_shares().await {
            Ok(fetched) => fetched,
            Err(error) => {
                debug!(%error, "EastMoney A-share list unavailable; using TDX security list");
                self.tdx.all_a_shares().await?
            }
        };
        self.security_master
            .merge_stock_list(&fetched.data, &fetched.source.to_string());
        Ok(fetched)
    }

    async fn market_breadth(&self) -> Result<Fetched<MarketBreadth>, DataError> {
        self.eastmoney.market_breadth().await
    }

    async fn index_kline(
        &self,
        index_secid: &str,
        count: u32,
    ) -> Result<Fetched<Vec<Bar>>, DataError> {
        // EastMoney first (fqt=0, daily); Tencent plain `day` key as fallback.
        match self.eastmoney.index_kline(index_secid, count).await {
            Ok(Fetched {
                data,
                source,
                fetched_at,
            }) => {
                let validated = filter_valid_index_bars(index_secid, data);
                Ok(Fetched {
                    data: validated,
                    source,
                    fetched_at,
                })
            }
            Err(e) => {
                debug!(error = %e, index_secid, "EM index kline failed, trying tencent");
                let index_code = index_secid.split('.').next_back().unwrap_or(index_secid);
                let bars = self.tencent.index_kline(index_code, count).await?;
                let validated = filter_valid_index_bars(index_secid, bars);
                // Accept short answers when the caller asked for few bars
                // (e.g. index cards request only the last 2 bars).
                let required = (count as usize).min(10);
                if validated.len() < required {
                    return Err(DataError::Empty(format!(
                        "index kline {index_secid}: {} bars",
                        validated.len()
                    )));
                }
                Ok(Fetched::now(validated, Source::Tencent))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
