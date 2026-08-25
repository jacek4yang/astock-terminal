//! TDX (通达信) provider adapter over `astock-tdx`.
//!
//! Capabilities: 日/周/月 K 线（只提供**未复权**；服务器不返回复权数据）、
//! 五档快照（映射到 [`Quote`] 的行情字段，档位本身不进核心模型）、全 A
//! 列表（SH/SZ 证券列表按号段过滤）。
//!
//! 单位约定（实盘验证，见 `crates/tdx/tests/protocol_fixtures.rs` golden）：
//! - K 线价格已是元，成交额已是元；
//! - K 线 `vol` 原始语义为**股**（600519 日量 ≈ 287 万股 = 2.87 万手），
//!   统一 ÷100 转为 [`VolumeUnit::Lots`]；
//! - 快照 `vol` 为总手（与 pytdx 语义一致），不再换算。
//!
//! 连接池惰性初始化：首次请求时才跑两阶段探测选路（3–5s），建好的
//! [`TdxClient`] 缓存进 [`tokio::sync::OnceCell`]（探测结果随之常驻内存，
//! 后续请求零探测开销）。探测失败不缓存，下次调用自动重试。

use crate::provider::DataProvider;
use crate::providers::fill_pct;
use astock_core::time::parse_date;
use astock_core::{
    Adjust, Bar, DataError, Fetched, KlinePeriod, MarketBreadth, Quote, Source, StockListItem,
    Symbol, VolumeUnit,
};
use astock_tdx::{
    KlineCategory, Quote as TdxQuote, SecurityBar, SecurityInfo, TdxClient, TdxError,
};
use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OnceCell};
use tracing::debug;

/// tdx 协议市场号：上海。
const MARKET_SH: u8 = 1;
/// tdx 协议市场号：深圳。
const MARKET_SZ: u8 = 0;
const TDX_QUOTE_BATCH: usize = 60;
const TDX_BREADTH_CONCURRENCY: usize = 6;
const TDX_BREADTH_ATTEMPTS: usize = 3;
const TDX_MARKET_SNAPSHOT_TTL: Duration = Duration::from_secs(2);

/// TDX adapter. Constructing it is free (no network); the server probe runs
/// lazily on the first data request.
pub struct TdxProvider {
    client: OnceCell<TdxClient>,
    securities: OnceCell<Vec<StockListItem>>,
    market_snapshot: Mutex<Option<(Instant, Fetched<Vec<StockListItem>>)>>,
}

impl Default for TdxProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl TdxProvider {
    /// Build an adapter with a not-yet-probed connection pool.
    pub fn new() -> Self {
        TdxProvider {
            client: OnceCell::new(),
            securities: OnceCell::new(),
            market_snapshot: Mutex::new(None),
        }
    }

    /// The lazily-initialized client. The first call pays the 3–5s probe;
    /// failures are *not* cached, so the next call retries the probe.
    async fn client(&self) -> Result<&TdxClient, DataError> {
        self.client
            .get_or_try_init(|| async {
                debug!("tdx lazy init: probing servers");
                TdxClient::start().await
            })
            .await
            .map_err(map_err)
    }

    /// Period + adjust → tdx category. 只支持日/周/月未复权；其余返回
    /// [`DataError::NoProvider`]，由故障转移链安静跳过。
    fn category_for(period: KlinePeriod, adjust: Adjust) -> Result<KlineCategory, DataError> {
        if adjust != Adjust::None {
            return Err(DataError::NoProvider("tdx (仅未复权)"));
        }
        match period {
            KlinePeriod::Day => Ok(KlineCategory::Daily),
            KlinePeriod::Week => Ok(KlineCategory::Weekly),
            KlinePeriod::Month => Ok(KlineCategory::Monthly),
            _ => Err(DataError::NoProvider("tdx (仅日/周/月)")),
        }
    }

    /// Raw TDX five-level snapshot for the professional order-book surface.
    /// The core quote pipeline consumes the same snapshot but deliberately
    /// projects away the five levels.
    pub async fn order_book(&self, symbol: &Symbol) -> Result<TdxQuote, DataError> {
        let code = symbol.code();
        let market = astock_tdx::protocol::types::auto_market(code)
            .ok_or(DataError::NoProvider("tdx (不支持该市场)"))?;
        let quote = self
            .client()
            .await?
            .quotes(&[(market, code)])
            .await
            .map_err(map_err)?
            .into_iter()
            .find(|quote| quote.code == code)
            .ok_or_else(|| DataError::Empty(format!("tdx order book {symbol}")))?;
        validate_tdx_quote(&quote, symbol)?;
        Ok(quote)
    }

    async fn quote_batch_with_retry(
        client: &TdxClient,
        batch: &[(u8, String)],
    ) -> Result<Vec<TdxQuote>, String> {
        let refs: Vec<(u8, &str)> = batch
            .iter()
            .map(|(market, code)| (*market, code.as_str()))
            .collect();
        let mut failures = Vec::new();
        for attempt in 1..=TDX_BREADTH_ATTEMPTS {
            match client.quotes(&refs).await {
                Ok(quotes) if !quotes.is_empty() => return Ok(quotes),
                Ok(_) => failures.push(format!("attempt {attempt}: empty response")),
                Err(error) => failures.push(format!("attempt {attempt}: {error}")),
            }
            if attempt < TDX_BREADTH_ATTEMPTS {
                tokio::time::sleep(Duration::from_millis(200 * attempt as u64)).await;
            }
        }
        Err(failures.join(", "))
    }

    async fn fetch_market_quotes(
        &self,
        securities: &[StockListItem],
    ) -> Result<Vec<TdxQuote>, DataError> {
        let batches: Vec<Vec<(u8, String)>> = securities
            .chunks(TDX_QUOTE_BATCH)
            .map(|chunk| {
                chunk
                    .iter()
                    .filter_map(|item| {
                        astock_tdx::protocol::types::auto_market(&item.code)
                            .map(|market| (market, item.code.clone()))
                    })
                    .collect()
            })
            .filter(|batch: &Vec<_>| !batch.is_empty())
            .collect();
        let client = self.client().await?;
        let results: Vec<_> = stream::iter(batches)
            .map(|batch| async move {
                let requested = batch.len();
                (
                    requested,
                    Self::quote_batch_with_retry(client, &batch).await,
                )
            })
            .buffer_unordered(TDX_BREADTH_CONCURRENCY)
            .collect()
            .await;
        let requested: usize = results.iter().map(|(count, _)| *count).sum();
        let mut quotes = Vec::with_capacity(requested);
        let mut failures = Vec::new();
        for (count, result) in results {
            match result {
                Ok(mut batch) => quotes.append(&mut batch),
                Err(error) => failures.push(format!("{count}-stock batch: {error}")),
            }
        }
        let required = requested.saturating_mul(95).div_ceil(100);
        if quotes.len() < required || quotes.len() < 4_000 {
            return Err(DataError::AllFailed {
                op: "tdx market snapshot",
                details: format!(
                    "coverage {}/{requested}, required {required}; {}",
                    quotes.len(),
                    failures.join("; ")
                ),
            });
        }
        Ok(quotes)
    }

    /// Complete Shanghai/Shenzhen snapshot with real price, percentage and
    /// amount fields. The 2-second cache single-flights concurrent breadth and
    /// Agent candidate requests without turning a process-lifetime security
    /// list cache into stale market data.
    pub async fn market_snapshot(&self) -> Result<Fetched<Vec<StockListItem>>, DataError> {
        let mut cache = self.market_snapshot.lock().await;
        if let Some((stored_at, fetched)) = cache.as_ref() {
            if stored_at.elapsed() <= TDX_MARKET_SNAPSHOT_TTL {
                return Ok(fetched.clone());
            }
        }
        let securities = self.all_a_shares().await?.data;
        let quotes = self.fetch_market_quotes(&securities).await?;
        let rows = snapshot_items(securities, quotes);
        if rows.len() < 4_000 {
            return Err(DataError::Empty(format!(
                "tdx market snapshot identity coverage is only {} rows",
                rows.len()
            )));
        }
        let price_present = rows.iter().filter(|row| row.price.is_some()).count();
        let amount_present = rows.iter().filter(|row| row.amount.is_some()).count();
        if price_present * 10 < rows.len() * 9 || amount_present * 5 < rows.len() * 4 {
            return Err(DataError::Empty(format!(
                "tdx market snapshot contains placeholder trade fields: price {price_present}/{}, amount {amount_present}/{}",
                rows.len(),
                rows.len()
            )));
        }
        let fetched = Fetched::now(rows, Source::Tdx);
        *cache = Some((Instant::now(), fetched.clone()));
        Ok(fetched)
    }
}

fn snapshot_items(securities: Vec<StockListItem>, quotes: Vec<TdxQuote>) -> Vec<StockListItem> {
    let names = securities
        .into_iter()
        .map(|item| (item.code, item.name))
        .collect::<std::collections::HashMap<_, _>>();
    quotes
        .into_iter()
        .filter_map(|quote| {
            let name = names.get(&quote.code)?.clone();
            let price = (quote.price > 0.0).then_some(quote.price);
            let pct = (quote.price > 0.0 && quote.last_close > 0.0)
                .then_some((quote.price - quote.last_close) / quote.last_close * 100.0);
            let amount = (quote.amount > 0.0).then_some(quote.amount);
            Some(StockListItem {
                code: quote.code,
                name,
                price,
                pct,
                amount,
            })
        })
        .collect()
}

/// Map a tdx error onto [`DataError`]: transport failures count toward the
/// circuit breaker; protocol errors are data-level.
fn map_err(e: TdxError) -> DataError {
    match e {
        TdxError::Timeout => DataError::Timeout("tdx".to_string()),
        TdxError::NoServerAvailable => DataError::Network {
            host: "tdx".to_string(),
            message: "no usable tdx server".to_string(),
        },
        TdxError::Io(e) => DataError::Network {
            host: "tdx".to_string(),
            message: e.to_string(),
        },
        TdxError::Disconnected => DataError::Network {
            host: "tdx".to_string(),
            message: "connection closed by server".to_string(),
        },
        TdxError::Protocol(m) => DataError::Parse {
            upstream: "tdx".to_string(),
            message: m,
        },
    }
}

/// SecurityBar（价：元，量：股，额：元）→ [`Bar`]（量 ÷100 → 手）。
/// 日期无法解析时返回 `None`（调用方丢弃该行）。
fn bar_from_tdx(raw: &SecurityBar) -> Option<Bar> {
    let date = parse_date(&raw.datetime)?;
    let mut bar = Bar::new(
        date,
        raw.open,
        raw.close,
        raw.high,
        raw.low,
        raw.vol / 100.0, // 股 → 手
        VolumeUnit::Lots,
    );
    bar.amount = Some(raw.amount);
    Some(bar)
}

/// 五档快照 → 核心 [`Quote`]。核心模型不带档位字段，盘口五档在此处丢弃；
/// `turnover`（换手率）快照不含，保持缺失；`name` 由证券主数据补齐。
fn quote_from_tdx(raw: &TdxQuote) -> Quote {
    let change = raw.price - raw.last_close;
    let pct = if raw.last_close > 0.0 {
        change / raw.last_close * 100.0
    } else {
        0.0
    };
    let timestamp = astock_core::time::utc_now();
    let mut field_provenance = std::collections::BTreeMap::new();
    for field in [
        "price",
        "open",
        "high",
        "low",
        "pre_close",
        "volume",
        "amount",
    ] {
        field_provenance.insert(
            field.to_string(),
            astock_core::FieldProvenance::reported("tdx", timestamp),
        );
    }
    for field in ["change", "pct"] {
        let mut derived = astock_core::FieldProvenance::reported("tdx", timestamp);
        derived.quality = astock_core::DataQuality::Derived;
        field_provenance.insert(field.to_string(), derived);
    }
    field_provenance.insert(
        "name".to_string(),
        astock_core::FieldProvenance::missing("tdx", "TDX 快照不包含证券名称"),
    );
    field_provenance.insert(
        "turnover".to_string(),
        astock_core::FieldProvenance::missing("tdx", "TDX 快照不包含换手率"),
    );
    Quote {
        symbol: raw.code.clone(),
        name: String::new(),
        price: raw.price,
        open: raw.open,
        high: raw.high,
        low: raw.low,
        pre_close: raw.last_close,
        volume: raw.vol, // 总手
        amount: raw.amount,
        change,
        pct,
        turnover: None,
        timestamp,
        field_provenance,
    }
}

fn validate_tdx_quote(raw: &TdxQuote, symbol: &Symbol) -> Result<(), DataError> {
    if raw.code != symbol.code() {
        return Err(DataError::Parse {
            upstream: format!("tdx quote {symbol}"),
            message: format!(
                "security identity mismatch: expected {}, received {}",
                symbol.code(),
                raw.code
            ),
        });
    }
    for (label, value) in [
        ("price", raw.price),
        ("pre-close", raw.last_close),
        ("open", raw.open),
        ("high", raw.high),
        ("low", raw.low),
    ] {
        if !value.is_finite() || value <= 0.0 {
            return Err(DataError::Empty(format!(
                "tdx quote {symbol}: missing or non-positive {label}"
            )));
        }
    }
    Ok(())
}

/// 号段过滤：是否沪深 A 股（剔除指数/基金/债券/B 股，北交所不在 tdx 覆盖内）。
/// SH: 60xxxx/68xxxx（主板+科创）；SZ: 00xxxx/30xxxx（主板+创业）。
fn is_a_share(market: u8, code: &str) -> bool {
    match market {
        MARKET_SH => code.starts_with("60") || code.starts_with("68"),
        MARKET_SZ => code.starts_with("00") || code.starts_with("30"),
        _ => false,
    }
}

/// 证券列表条目 → 全 A 列表行。tdx 列表接口只带昨收，最新价/涨跌幅/成交额
/// 不可得，置 0（本方法是故障转移兜底；主路径仍走 EastMoney）。
fn item_from_tdx(info: &SecurityInfo) -> StockListItem {
    StockListItem {
        code: info.code.clone(),
        name: info.name.clone(),
        price: None,
        pct: None,
        amount: None,
    }
}

#[async_trait]
impl DataProvider for TdxProvider {
    fn name(&self) -> &'static str {
        "tdx"
    }

    fn primary_host(&self) -> &'static str {
        // 池化多台服务器，无单一主 host。
        ""
    }

    async fn kline(
        &self,
        symbol: &Symbol,
        period: KlinePeriod,
        adjust: Adjust,
        count: u32,
    ) -> Result<Fetched<Vec<Bar>>, DataError> {
        let category = Self::category_for(period, adjust)?;
        let code = symbol.code();
        let market = astock_tdx::protocol::types::auto_market(code)
            .ok_or(DataError::NoProvider("tdx (不支持该市场)"))?;
        let count = count.min(u16::MAX as u32) as u16;

        let raw = self
            .client()
            .await?
            .kline(market, code, category, count)
            .await
            .map_err(map_err)?;

        let mut bars: Vec<Bar> = raw.iter().filter_map(bar_from_tdx).collect();
        if bars.is_empty() {
            return Err(DataError::Empty(format!("tdx kline {symbol}")));
        }
        fill_pct(&mut bars);
        Ok(Fetched::now(bars, Source::Tdx))
    }

    async fn quote(&self, symbol: &Symbol) -> Result<Fetched<Quote>, DataError> {
        let raw = self.order_book(symbol).await?;
        Ok(Fetched::now(quote_from_tdx(&raw), Source::Tdx))
    }

    async fn all_a_shares(&self) -> Result<Fetched<Vec<StockListItem>>, DataError> {
        let items = self
            .securities
            .get_or_try_init(|| async {
                let client = self.client().await?;
                let mut items = Vec::new();
                for market in [MARKET_SH, MARKET_SZ] {
                    let list = client.security_list(market).await.map_err(map_err)?;
                    items.extend(
                        list.iter()
                            .filter(|s| is_a_share(market, &s.code))
                            .map(item_from_tdx),
                    );
                }
                if items.is_empty() {
                    return Err(DataError::Empty("tdx all_a_shares".to_string()));
                }
                Ok(items)
            })
            .await?;
        Ok(Fetched::now(items.clone(), Source::Tdx))
    }

    /// Full-market advance/decline fallback. TDX accepts at most 60 symbols
    /// per quote request, so batches are bounded, retried independently and
    /// checked for coverage before the result is accepted.
    async fn market_breadth(&self) -> Result<Fetched<MarketBreadth>, DataError> {
        let snapshot = self.market_snapshot().await?;
        let mut up = 0_u32;
        let mut down = 0_u32;
        let mut flat = 0_u32;
        for row in snapshot.data {
            match row.pct {
                Some(pct) if pct > 0.0 => up += 1,
                Some(pct) if pct < 0.0 => down += 1,
                _ => flat += 1,
            }
        }
        Ok(Fetched::now(
            MarketBreadth {
                up,
                down,
                flat,
                total: up + down + flat,
            },
            Source::Tdx,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn security_bar() -> SecurityBar {
        SecurityBar {
            datetime: "2026-08-21".to_string(),
            open: 1291.5,
            close: 1272.83,
            high: 1291.5,
            low: 1272.01,
            vol: 3_347_231.0, // 股（实盘 golden 值）
            amount: 4_260_000_000.0,
        }
    }

    #[test]
    fn category_mapping_only_daily_weekly_monthly_unadjusted() {
        assert_eq!(
            TdxProvider::category_for(KlinePeriod::Day, Adjust::None).unwrap(),
            KlineCategory::Daily
        );
        assert_eq!(
            TdxProvider::category_for(KlinePeriod::Week, Adjust::None).unwrap(),
            KlineCategory::Weekly
        );
        assert_eq!(
            TdxProvider::category_for(KlinePeriod::Month, Adjust::None).unwrap(),
            KlineCategory::Monthly
        );
        assert!(matches!(
            TdxProvider::category_for(KlinePeriod::Min5, Adjust::None),
            Err(DataError::NoProvider(_))
        ));
        assert!(matches!(
            TdxProvider::category_for(KlinePeriod::Day, Adjust::Qfq),
            Err(DataError::NoProvider(_))
        ));
        assert!(matches!(
            TdxProvider::category_for(KlinePeriod::Day, Adjust::Hfq),
            Err(DataError::NoProvider(_))
        ));
    }

    #[test]
    fn bar_mapping_converts_shares_to_lots() {
        let bar = bar_from_tdx(&security_bar()).unwrap();
        assert_eq!(bar.date, d("2026-08-21"));
        assert_eq!(bar.open, 1291.5);
        assert_eq!(bar.close, 1272.83);
        assert_eq!(bar.volume, 33_472.31); // 股 ÷ 100 → 手
        assert_eq!(bar.volume_unit, VolumeUnit::Lots);
        assert_eq!(bar.amount, Some(4_260_000_000.0)); // 元，原样
        assert!(bar.is_valid());
    }

    #[test]
    fn bar_mapping_rejects_bad_date() {
        let mut raw = security_bar();
        raw.datetime = "not a date".to_string();
        assert!(bar_from_tdx(&raw).is_none());
    }

    #[test]
    fn quote_mapping_computes_change_and_pct() {
        let raw = TdxQuote {
            market: MARKET_SH,
            code: "600519".to_string(),
            price: 1272.83,
            last_close: 1291.5,
            open: 1291.5,
            high: 1291.5,
            low: 1272.01,
            servertime: "15:00:00".to_string(),
            vol: 33_472.0, // 总手
            cur_vol: 0.0,
            amount: 4_260_000_000.0,
            s_vol: 0.0,
            b_vol: 0.0,
            bid: {
                let mut l = [(0.0, 0.0); 5];
                l[0] = (1272.83, 10.0);
                l
            },
            ask: {
                let mut l = [(0.0, 0.0); 5];
                l[0] = (1272.9, 5.0);
                l
            },
        };
        let q = quote_from_tdx(&raw);
        assert_eq!(q.symbol, "600519");
        assert_eq!(q.price, 1272.83);
        assert_eq!(q.pre_close, 1291.5);
        assert_eq!(q.volume, 33_472.0); // 快照量已是手，不再换算
        assert!((q.change - (1272.83 - 1291.5)).abs() < 1e-9);
        assert!((q.pct - (1272.83 - 1291.5) / 1291.5 * 100.0).abs() < 1e-9);
        assert_eq!(q.turnover, None);
    }

    #[test]
    fn quote_mapping_zero_preclose_no_div_by_zero() {
        let raw = TdxQuote {
            market: MARKET_SZ,
            code: "000001".to_string(),
            price: 10.0,
            last_close: 0.0,
            open: 10.0,
            high: 10.0,
            low: 10.0,
            servertime: "09:30:00".to_string(),
            vol: 1.0,
            cur_vol: 0.0,
            amount: 0.0,
            s_vol: 0.0,
            b_vol: 0.0,
            bid: [(0.0, 0.0); 5],
            ask: [(0.0, 0.0); 5],
        };
        let q = quote_from_tdx(&raw);
        assert_eq!(q.pct, 0.0);
        assert!(matches!(
            validate_tdx_quote(&raw, &Symbol::new("000001").unwrap()),
            Err(DataError::Empty(message)) if message.contains("non-positive pre-close")
        ));
    }

    #[test]
    fn market_snapshot_keeps_identity_and_real_quote_fields() {
        let securities = vec![StockListItem {
            code: "300308".to_string(),
            name: "中际旭创".to_string(),
            price: None,
            pct: None,
            amount: None,
        }];
        let quotes = vec![TdxQuote {
            market: MARKET_SZ,
            code: "300308".to_string(),
            price: 870.22,
            last_close: 943.0,
            open: 945.0,
            high: 949.73,
            low: 850.0,
            servertime: "15:00:00".to_string(),
            vol: 389_093.0,
            cur_vol: 0.0,
            amount: 34_491_670_420.0,
            s_vol: 0.0,
            b_vol: 0.0,
            bid: [(0.0, 0.0); 5],
            ask: [(0.0, 0.0); 5],
        }];
        let rows = snapshot_items(securities, quotes);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].code, "300308");
        assert_eq!(rows[0].name, "中际旭创");
        assert_eq!(rows[0].price, Some(870.22));
        assert_eq!(rows[0].amount, Some(34_491_670_420.0));
        assert!((rows[0].pct.unwrap() - (-7.717_921_527_041_355)).abs() < 1e-9);
    }

    #[test]
    fn a_share_segment_filter() {
        // SH：60/68 为 A 股；指数 000、基金 5xx、债券 11/13、B 股 90 排除
        assert!(is_a_share(MARKET_SH, "600519"));
        assert!(is_a_share(MARKET_SH, "601318"));
        assert!(is_a_share(MARKET_SH, "688981"));
        assert!(!is_a_share(MARKET_SH, "000001")); // 上证指数
        assert!(!is_a_share(MARKET_SH, "510300")); // ETF
        assert!(!is_a_share(MARKET_SH, "113044")); // 债券
        assert!(!is_a_share(MARKET_SH, "900901")); // B 股
                                                   // SZ：00/30 为 A 股；指数 399、基金 15/16、B 股 20 排除
        assert!(is_a_share(MARKET_SZ, "000001"));
        assert!(is_a_share(MARKET_SZ, "002594"));
        assert!(is_a_share(MARKET_SZ, "300750"));
        assert!(!is_a_share(MARKET_SZ, "399001")); // 深证成指
        assert!(!is_a_share(MARKET_SZ, "159915")); // ETF
        assert!(!is_a_share(MARKET_SZ, "200002")); // B 股
        assert!(!is_a_share(2, "600519")); // 未知市场
    }

    #[test]
    fn error_mapping_feeds_breaker_classification() {
        // 网络类错误要被熔断器计数；协议错误视为数据级。
        assert!(matches!(map_err(TdxError::Timeout), DataError::Timeout(_)));
        assert!(matches!(
            map_err(TdxError::NoServerAvailable),
            DataError::Network { .. }
        ));
        assert!(matches!(
            map_err(TdxError::Protocol("bad".into())),
            DataError::Parse { .. }
        ));
    }
}
