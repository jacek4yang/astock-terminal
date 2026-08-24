//! JoinQuant (聚宽) provider adapter over `astock-joinquant` (optional,
//! credential-gated).
//!
//! Credentials come from the `JQ_USER` / `JQ_PWD` env vars; without them the
//! provider is `available() == false` — listed on the health panel but every
//! call returns [`DataError::NoProvider`]. **Not** part of the automatic
//! failover chain: it is an explicit-call source only (daily bars, index
//! components, valuation snapshots, macro CPI), like [`super::tushare`].
//!
//! Strictly low-frequency: every call is serialized behind one gate and
//! spaced at least [`MIN_INTERVAL`] apart (the research environment spawns a
//! remote kernel per query — doc `data-source-joinquant-v2.md` §4.6).
//!
//! Unit conventions: `daily` uses `fq='pre'` (前复权) prices, `volume` is
//! 股 → ÷100 to [`VolumeUnit::Lots`], `money` is 元 already. Suspended
//! sessions (None OHLC) are dropped.

use astock_core::time::parse_date;
use astock_core::{Bar, DataError, Fetched, Source, Symbol, VolumeUnit};
use astock_joinquant::{Credentials, DailyBar, JoinQuantClient, JoinQuantError, ValuationSnapshot};
use chrono::NaiveDate;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::{Mutex, MutexGuard};
use tokio::time::Instant;

/// Env var carrying the JoinQuant login name.
pub const USER_ENV: &str = "JQ_USER";
/// Env var carrying the JoinQuant password.
pub const PWD_ENV: &str = "JQ_PWD";

/// Minimum spacing between upstream calls (strict low-frequency policy).
pub const MIN_INTERVAL: Duration = Duration::from_secs(2);

/// JoinQuant adapter. All methods return [`DataError::NoProvider`] when no
/// credentials are configured.
pub struct JoinQuantProvider {
    client: RwLock<Option<Arc<JoinQuantClient>>>,
    /// Last upstream-call timestamp; the guard is held for the whole call so
    /// requests are serialized process-wide on top of the 2s spacing.
    gate: Mutex<Option<Instant>>,
}

impl JoinQuantProvider {
    /// Wrap an optional client. `None` marks the provider unavailable.
    pub fn new(client: Option<JoinQuantClient>) -> Self {
        JoinQuantProvider {
            client: RwLock::new(client.map(Arc::new)),
            gate: Mutex::new(None),
        }
    }

    /// Build from the `JQ_USER` / `JQ_PWD` env vars (unavailable when either
    /// is missing or blank).
    pub fn from_env() -> Self {
        let client = match (std::env::var(USER_ENV), std::env::var(PWD_ENV)) {
            (Ok(u), Ok(p)) if !u.trim().is_empty() && !p.is_empty() => {
                JoinQuantClient::new(Credentials::new(u, p)).ok()
            }
            _ => None,
        };
        Self::new(client)
    }

    /// Whether credentials are configured.
    pub fn available(&self) -> bool {
        self.client.read().is_ok_and(|client| client.is_some())
    }

    /// Replace credentials without restarting the desktop Engine. Credentials
    /// remain inside the provider and are never exposed through diagnostics.
    pub fn configure(&self, username: String, password: String) -> Result<(), DataError> {
        let client = JoinQuantClient::new(Credentials::new(username, password)).map_err(map_err)?;
        *self.client.write().map_err(|_| DataError::Parse {
            upstream: "joinquant credentials".to_string(),
            message: "credential lock poisoned".to_string(),
        })? = Some(Arc::new(client));
        Ok(())
    }

    /// Remove the active client immediately after credentials are deleted.
    pub fn clear_credentials(&self) {
        if let Ok(mut client) = self.client.write() {
            *client = None;
        }
    }

    fn client(&self) -> Result<Arc<JoinQuantClient>, DataError> {
        self.client
            .read()
            .map_err(|_| DataError::Parse {
                upstream: "joinquant credentials".to_string(),
                message: "credential lock poisoned".to_string(),
            })?
            .clone()
            .ok_or(DataError::NoProvider("joinquant (no JQ_USER/JQ_PWD)"))
    }

    /// Serialize calls and enforce [`MIN_INTERVAL`] spacing; the returned
    /// guard must be held until the upstream call finishes.
    async fn wait_turn(&self) -> MutexGuard<'_, Option<Instant>> {
        let mut guard = self.gate.lock().await;
        if let Some(last) = *guard {
            let elapsed = last.elapsed();
            if elapsed < MIN_INTERVAL {
                tokio::time::sleep(MIN_INTERVAL - elapsed).await;
            }
        }
        *guard = Some(Instant::now());
        guard
    }

    /// 前复权 daily bars over `[start, end]`, oldest first. Volume converted
    /// 股 → 手; suspended days (None OHLC) dropped.
    pub async fn daily(
        &self,
        symbol: &Symbol,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Fetched<Vec<Bar>>, DataError> {
        let jq = symbol_to_jq(symbol)?;
        let start = start.format("%Y-%m-%d").to_string();
        let end = end.format("%Y-%m-%d").to_string();
        let _turn = self.wait_turn().await;
        let raw = self
            .client()?
            .daily(&jq, &start, &end)
            .await
            .map_err(map_err)?;
        let bars: Vec<Bar> = raw.iter().filter_map(bar_from_jq).collect();
        if bars.is_empty() {
            return Err(DataError::Empty(format!("joinquant daily {symbol}")));
        }
        Ok(Fetched::now(bars, Source::JoinQuant))
    }

    /// Index components (internal `SHxxxxxx`/`SZxxxxxx` codes) of an index
    /// on `date`. `index` accepts `"000300"`, `"SH000300"` or `"000300.XSHG"`.
    pub async fn index_components(
        &self,
        index: &str,
        date: NaiveDate,
    ) -> Result<Vec<String>, DataError> {
        let jq = index_to_jq(index)?;
        let date = date.format("%Y-%m-%d").to_string();
        let _turn = self.wait_turn().await;
        let codes = self
            .client()?
            .index_components(&jq, &date)
            .await
            .map_err(map_err)?;
        if codes.is_empty() {
            return Err(DataError::Empty(format!("joinquant components {index}")));
        }
        Ok(codes)
    }

    /// Valuation snapshot (PE/PB/PS/PCF + market caps) for `symbols` on `date`.
    pub async fn valuation(
        &self,
        symbols: &[Symbol],
        date: NaiveDate,
    ) -> Result<Vec<ValuationSnapshot>, DataError> {
        if symbols.is_empty() {
            return Err(DataError::Empty(
                "joinquant valuation: no symbols".to_string(),
            ));
        }
        let codes: Vec<String> = symbols.iter().map(symbol_to_jq).collect::<Result<_, _>>()?;
        let date = date.format("%Y-%m-%d").to_string();
        let _turn = self.wait_turn().await;
        let rows = self
            .client()?
            .valuation(&codes, &date)
            .await
            .map_err(map_err)?;
        if rows.is_empty() {
            return Err(DataError::Empty("joinquant valuation".to_string()));
        }
        Ok(rows)
    }

    /// Latest `limit` monthly CPI rows (`MAC_CPI_MONTH`, newest first).
    pub async fn macro_cpi(&self, limit: usize) -> Result<Vec<serde_json::Value>, DataError> {
        let _turn = self.wait_turn().await;
        let rows = self.client()?.macro_cpi(limit).await.map_err(map_err)?;
        if rows.is_empty() {
            return Err(DataError::Empty("joinquant macro_cpi".to_string()));
        }
        Ok(rows)
    }
}

/// Symbol → JoinQuant code: SH → `.XSHG`, SZ → `.XSHE`. BJ is not covered
/// by the research API.
fn symbol_to_jq(symbol: &Symbol) -> Result<String, DataError> {
    match symbol.market() {
        astock_core::Market::SH => Ok(format!("{}.XSHG", symbol.code())),
        astock_core::Market::SZ => Ok(format!("{}.XSHE", symbol.code())),
        astock_core::Market::BJ => Err(DataError::NoProvider("joinquant (不支持北交所)")),
    }
}

/// Loose index code → JoinQuant code: `"000300"` / `"SH000300"` /
/// `"000300.XSHG"` → `"000300.XSHG"`; `"399006"` → `"399006.XSHE"`.
fn index_to_jq(index: &str) -> Result<String, DataError> {
    let bare = index
        .trim()
        .trim_start_matches("SH")
        .trim_start_matches("SZ")
        .split('.')
        .next()
        .unwrap_or("");
    if bare.len() == 6 && bare.bytes().all(|b| b.is_ascii_digit()) {
        let suffix = if bare.starts_with("399") {
            "XSHE"
        } else {
            "XSHG"
        };
        Ok(format!("{bare}.{suffix}"))
    } else {
        Err(DataError::InvalidSymbol(index.to_string()))
    }
}

/// DailyBar（价：元，前复权；量：股；额：元）→ [`Bar`]（量 ÷100 → 手）。
/// 停牌日（任一 OHLC 为 None）返回 `None` 由调用方丢弃。
fn bar_from_jq(raw: &DailyBar) -> Option<Bar> {
    let date = parse_date(&raw.date)?;
    let (open, high, low, close) = (raw.open?, raw.high?, raw.low?, raw.close?);
    let mut bar = Bar::new(
        date,
        open,
        close,
        high,
        low,
        raw.volume.unwrap_or(0.0) / 100.0, // 股 → 手
        VolumeUnit::Lots,
    );
    bar.amount = raw.money;
    Some(bar)
}

/// Map a JoinQuant error onto [`DataError`].
fn map_err(e: JoinQuantError) -> DataError {
    match e {
        JoinQuantError::Http(e) => DataError::Network {
            host: "joinquant.com".to_string(),
            message: e.to_string(),
        },
        JoinQuantError::Ws(e) => DataError::Network {
            host: "joinquant.com".to_string(),
            message: e.to_string(),
        },
        JoinQuantError::SpawnTimeout(s) => DataError::Timeout(format!("joinquant spawn ({s}s)")),
        other => DataError::Empty(format!("joinquant: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_core::Market;

    #[test]
    fn symbol_to_jq_mapping() {
        let sh = Symbol::new("600519").unwrap();
        let sz = Symbol::new("000001").unwrap();
        let bj = Symbol::new("920001").unwrap();
        assert_eq!(symbol_to_jq(&sh).unwrap(), "600519.XSHG");
        assert_eq!(symbol_to_jq(&sz).unwrap(), "000001.XSHE");
        assert!(matches!(symbol_to_jq(&bj), Err(DataError::NoProvider(_))));
        assert_eq!(sh.market(), Market::SH);
    }

    #[test]
    fn index_code_normalization() {
        assert_eq!(index_to_jq("000300").unwrap(), "000300.XSHG");
        assert_eq!(index_to_jq("SH000300").unwrap(), "000300.XSHG");
        assert_eq!(index_to_jq("000300.XSHG").unwrap(), "000300.XSHG");
        assert_eq!(index_to_jq("399006").unwrap(), "399006.XSHE");
        assert_eq!(index_to_jq("SZ399006").unwrap(), "399006.XSHE");
        assert!(index_to_jq("not-an-index").is_err());
    }

    #[test]
    fn bar_mapping_converts_shares_to_lots_and_drops_suspended() {
        let raw = DailyBar {
            date: "2026-08-21".to_string(),
            open: Some(1291.5),
            high: Some(1291.5),
            low: Some(1272.01),
            close: Some(1272.83),
            volume: Some(3_347_231.0), // 股
            money: Some(4_260_000_000.0),
        };
        let bar = bar_from_jq(&raw).unwrap();
        assert_eq!(bar.close, 1272.83);
        assert_eq!(bar.volume, 33_472.31); // 股 ÷100 → 手
        assert_eq!(bar.volume_unit, VolumeUnit::Lots);
        assert_eq!(bar.amount, Some(4_260_000_000.0)); // 元，原样
        assert!(bar.is_valid());

        // 停牌日：OHLC 为 None → 丢弃
        let suspended = DailyBar {
            date: "2026-08-20".to_string(),
            open: None,
            high: None,
            low: None,
            close: None,
            volume: Some(0.0),
            money: Some(0.0),
        };
        assert!(bar_from_jq(&suspended).is_none());
    }

    #[test]
    fn unavailable_without_credentials() {
        let p = JoinQuantProvider::new(None);
        assert!(!p.available());
        assert!(matches!(p.client(), Err(DataError::NoProvider(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn rate_gate_spaces_calls_and_serializes() {
        let p = JoinQuantProvider::new(None);
        // 第一次立即放行
        let t0 = Instant::now();
        drop(p.wait_turn().await);
        assert_eq!(t0.elapsed(), Duration::ZERO);
        // 第二次补齐 2s 间隔
        drop(p.wait_turn().await);
        assert_eq!(t0.elapsed(), MIN_INTERVAL);
        // 第三次再隔 2s
        drop(p.wait_turn().await);
        assert_eq!(t0.elapsed(), 2 * MIN_INTERVAL);
    }
}
