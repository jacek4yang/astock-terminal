//! The `DataProvider` trait and a failover composite.

use astock_core::{
    Adjust, Bar, DataError, Fetched, FundFlowPoint, KlinePeriod, MarketBreadth, MinuteData, Quote,
    SearchResult, StockListItem, Symbol,
};
use async_trait::async_trait;
use std::sync::Arc;

/// A market-data source. Methods default to [`DataError::NoProvider`] so
/// partial providers (e.g. Tencent kline-only) implement just what they serve.
#[async_trait]
pub trait DataProvider: Send + Sync {
    /// Stable provider name for logs and diagnostics.
    fn name(&self) -> &'static str;

    /// Primary upstream host, for timing/diagnostic logs (empty when the
    /// provider fans out over a pool).
    fn primary_host(&self) -> &'static str {
        ""
    }

    /// Historical kline bars, oldest first.
    async fn kline(
        &self,
        _symbol: &Symbol,
        _period: KlinePeriod,
        _adjust: Adjust,
        _count: u32,
    ) -> Result<Fetched<Vec<Bar>>, DataError> {
        Err(DataError::NoProvider("kline"))
    }

    /// Historical bars for high-throughput screening and cache warming.
    ///
    /// The default preserves provider compatibility. The composite market
    /// hub overrides this to return the complete OHLCV series without waiting
    /// for optional amount/turnover enrichment; a later detailed request can
    /// reuse the warmed base series and enrich it once.
    async fn scan_kline(
        &self,
        symbol: &Symbol,
        period: KlinePeriod,
        adjust: Adjust,
        count: u32,
    ) -> Result<Fetched<Vec<Bar>>, DataError> {
        self.kline(symbol, period, adjust, count).await
    }

    /// Realtime quote snapshot.
    async fn quote(&self, _symbol: &Symbol) -> Result<Fetched<Quote>, DataError> {
        Err(DataError::NoProvider("quote"))
    }

    /// Symbol search by keyword or code.
    async fn search(&self, _keyword: &str) -> Result<Fetched<Vec<SearchResult>>, DataError> {
        Err(DataError::NoProvider("search"))
    }

    /// Daily fund flow for the last `days` trading days.
    async fn fund_flow_daily(
        &self,
        _symbol: &Symbol,
        _days: u32,
    ) -> Result<Fetched<Vec<FundFlowPoint>>, DataError> {
        Err(DataError::NoProvider("fund_flow_daily"))
    }

    /// Intraday 1-minute cumulative fund flow; empty off-market is normal.
    async fn fund_flow_realtime(
        &self,
        _symbol: &Symbol,
    ) -> Result<Fetched<Vec<FundFlowPoint>>, DataError> {
        Err(DataError::NoProvider("fund_flow_realtime"))
    }

    /// Intraday minute (分时) series for the current session.
    async fn minute(&self, _symbol: &Symbol) -> Result<Fetched<MinuteData>, DataError> {
        Err(DataError::NoProvider("minute"))
    }

    /// Full A-share list for scanner pre-filtering.
    async fn all_a_shares(&self) -> Result<Fetched<Vec<StockListItem>>, DataError> {
        Err(DataError::NoProvider("all_a_shares"))
    }

    /// Market-wide advance/decline counts.
    async fn market_breadth(&self) -> Result<Fetched<MarketBreadth>, DataError> {
        Err(DataError::NoProvider("market_breadth"))
    }

    /// Index kline given an EastMoney index secid such as `1.000001`.
    async fn index_kline(
        &self,
        _index_secid: &str,
        _count: u32,
    ) -> Result<Fetched<Vec<Bar>>, DataError> {
        Err(DataError::NoProvider("index_kline"))
    }
}

/// Try each provider in order, returning the first success.
///
/// `NoProvider` answers (capability gaps) are skipped quietly; real failures
/// are collected and reported together in [`DataError::AllFailed`].
pub struct Failover {
    providers: Vec<Arc<dyn DataProvider>>,
}

impl Failover {
    /// Build a chain from providers in priority order.
    pub fn new(providers: Vec<Arc<dyn DataProvider>>) -> Self {
        Failover { providers }
    }

    async fn try_each<'a, T, Fut>(
        &'a self,
        op: &'static str,
        call: impl Fn(&'a Arc<dyn DataProvider>) -> Fut,
    ) -> Result<T, DataError>
    where
        Fut: std::future::Future<Output = Result<T, DataError>> + 'a,
    {
        let mut failures = Vec::new();
        for p in &self.providers {
            match call(p).await {
                Ok(v) => return Ok(v),
                Err(DataError::NoProvider(_)) => continue,
                Err(e) => {
                    tracing::debug!(provider = p.name(), op, error = %e, "provider failed, trying next");
                    failures.push(format!("{}: {e}", p.name()));
                }
            }
        }
        Err(DataError::AllFailed {
            op,
            details: failures.join("; "),
        })
    }
}

#[async_trait]
impl DataProvider for Failover {
    fn name(&self) -> &'static str {
        "failover"
    }

    async fn kline(
        &self,
        symbol: &Symbol,
        period: KlinePeriod,
        adjust: Adjust,
        count: u32,
    ) -> Result<Fetched<Vec<Bar>>, DataError> {
        self.try_each("kline", |p| async move {
            p.kline(symbol, period, adjust, count).await
        })
        .await
    }

    async fn quote(&self, symbol: &Symbol) -> Result<Fetched<Quote>, DataError> {
        self.try_each("quote", |p| async move { p.quote(symbol).await })
            .await
    }

    async fn search(&self, keyword: &str) -> Result<Fetched<Vec<SearchResult>>, DataError> {
        self.try_each("search", |p| async move { p.search(keyword).await })
            .await
    }

    async fn fund_flow_daily(
        &self,
        symbol: &Symbol,
        days: u32,
    ) -> Result<Fetched<Vec<FundFlowPoint>>, DataError> {
        self.try_each("fund_flow_daily", |p| async move {
            p.fund_flow_daily(symbol, days).await
        })
        .await
    }

    async fn fund_flow_realtime(
        &self,
        symbol: &Symbol,
    ) -> Result<Fetched<Vec<FundFlowPoint>>, DataError> {
        self.try_each("fund_flow_realtime", |p| async move {
            p.fund_flow_realtime(symbol).await
        })
        .await
    }

    async fn minute(&self, symbol: &Symbol) -> Result<Fetched<MinuteData>, DataError> {
        self.try_each("minute", |p| async move { p.minute(symbol).await })
            .await
    }

    async fn all_a_shares(&self) -> Result<Fetched<Vec<StockListItem>>, DataError> {
        self.try_each("all_a_shares", |p| async move { p.all_a_shares().await })
            .await
    }

    async fn market_breadth(&self) -> Result<Fetched<MarketBreadth>, DataError> {
        self.try_each(
            "market_breadth",
            |p| async move { p.market_breadth().await },
        )
        .await
    }

    async fn index_kline(
        &self,
        index_secid: &str,
        count: u32,
    ) -> Result<Fetched<Vec<Bar>>, DataError> {
        self.try_each("index_kline", |p| async move {
            p.index_kline(index_secid, count).await
        })
        .await
    }
}
