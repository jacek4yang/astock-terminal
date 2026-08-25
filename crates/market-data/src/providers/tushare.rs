//! Tushare Pro provider (optional, token-gated).
//!
//! Protocol per `docs/data-source-tushare.md`: a single endpoint
//! `POST https://api.tushare.pro` with body `{api_name, token, params,
//! fields}`, response `{code, msg, data:{fields, items}}`. Column order is
//! never assumed — rows are mapped through the returned `fields` index.
//!
//! Capability tiers (积分门槛制, non-consuming):
//! - 120 (free): `daily` raw klines only;
//! - 2000+: `adj_factor` / `dividend` / `daily_basic` / `trade_cal` etc.
//!
//! The token is optional: `None` marks the provider unavailable (all calls
//! return [`DataError::NoProvider`]) and the hub surfaces that on the health
//! panel. Tier detection probes `adj_factor` once and caches the outcome.
//!
//! Unit conventions (doc §3.1): `vol` is in **lots (手)** = our
//! [`VolumeUnit::Lots`] natively; `amount` is **thousand CNY** → ×1000 to
//! yuan; `daily_basic` share/market-value fields are 万股/万元 → ×10⁴.

use crate::cache::{ttl, TtlCache};
use crate::http::HttpClient;
use crate::providers::json_f64;
use astock_core::time::parse_date;
use astock_core::{
    compute_qfq, Bar, CorporateAction, DataError, Fetched, Source, Symbol, VolumeUnit,
};
use chrono::NaiveDate;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, warn};

/// The single tushare pro endpoint.
pub const TUSHARE_URL: &str = "https://api.tushare.pro";

/// Business error codes (doc §1.2): 2002 = 权限不足 (never retried),
/// 40203 = rate limited (back off via the shared adaptive limiter).
const CODE_NO_PERMISSION: i64 = 2002;
const CODE_RATE_LIMITED: i64 = 40203;

/// Detected capability tier of the configured token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TushareTier {
    /// Not probed yet.
    Unknown,
    /// Free 120-point tier: `daily` only.
    Free120,
    /// 2000+ tier: `adj_factor` / `dividend` / `daily_basic` / `trade_cal`.
    Pro2000,
}

/// One `adj_factor` row: the cumulative backward-adjustment factor
/// (上市首日 ≈ 1, monotonically non-decreasing) for one trading date.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AdjFactorPoint {
    /// Trading date.
    pub date: NaiveDate,
    /// `hfq_price(t) = raw(t) × factor`.
    pub factor: f64,
}

/// One `trade_cal` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeCalDay {
    /// Calendar date.
    pub date: NaiveDate,
    /// Whether the exchange is open that day.
    pub is_open: bool,
    /// Previous trading day, when the upstream provides it.
    pub pretrade_date: Option<NaiveDate>,
}

/// `daily_basic` valuation/turnover snapshot for one trading date.
/// Share counts are in shares and market values in yuan (×10⁴ from 万股/万元).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DailyBasic {
    /// Trading date.
    pub date: NaiveDate,
    /// Turnover rate in percent (流通股口径).
    pub turnover_rate: Option<f64>,
    /// P/E (TTM).
    pub pe_ttm: Option<f64>,
    /// P/B.
    pub pb: Option<f64>,
    /// Total market value in yuan.
    pub total_mv: Option<f64>,
    /// Circulating market value in yuan.
    pub circ_mv: Option<f64>,
}

/// Raw tushare table: column names plus row arrays, order preserved.
struct Table {
    fields: Vec<String>,
    items: Vec<Vec<serde_json::Value>>,
}

impl Table {
    /// Column index by name.
    fn col(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f == name)
    }

    fn get<'a>(
        &'a self,
        row: &'a [serde_json::Value],
        name: &str,
    ) -> Option<&'a serde_json::Value> {
        self.col(name).and_then(|i| row.get(i))
    }

    fn get_str<'a>(&'a self, row: &'a [serde_json::Value], name: &str) -> Option<&'a str> {
        match self.get(row, name) {
            Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s),
            _ => None,
        }
    }

    fn get_f64(&self, row: &[serde_json::Value], name: &str) -> Option<f64> {
        self.get(row, name).and_then(json_f64)
    }

    fn get_date(&self, row: &[serde_json::Value], name: &str) -> Option<NaiveDate> {
        self.get_str(row, name).and_then(parse_date)
    }
}

/// Tushare pro adapter. All methods return [`DataError::NoProvider`] when
/// no token is configured.
pub struct TushareProvider {
    http: Arc<HttpClient>,
    cache: Arc<TtlCache>,
    token: Option<String>,
    tier: Mutex<TushareTier>,
}

impl TushareProvider {
    /// Wrap the shared HTTP layer with an optional token.
    pub fn new(http: Arc<HttpClient>, cache: Arc<TtlCache>, token: Option<String>) -> Self {
        TushareProvider {
            http,
            cache,
            token: token.filter(|t| !t.trim().is_empty()),
            tier: Mutex::new(TushareTier::Unknown),
        }
    }

    /// Whether a token is configured.
    pub fn available(&self) -> bool {
        self.token.is_some()
    }

    /// Cached capability tier ([`TushareTier::Unknown`] until probed).
    pub fn tier(&self) -> TushareTier {
        *self.tier.lock()
    }

    fn token(&self) -> Result<&str, DataError> {
        self.token
            .as_deref()
            .ok_or(DataError::NoProvider("tushare (no token)"))
    }

    /// Symbol → tushare `ts_code` (`600519` → `600519.SH`, `920xxx` → `.BJ`).
    pub fn ts_code(symbol: &Symbol) -> String {
        format!("{}.{}", symbol.code(), symbol.market())
    }

    /// Raw `POST {api_name, token, params, fields}` call; maps business
    /// error codes onto [`DataError`] (2002 → `NoProvider`, 40203 →
    /// `RateLimited` so the adaptive limiter backs off).
    async fn call(
        &self,
        api_name: &str,
        params: serde_json::Map<String, serde_json::Value>,
        fields: &str,
    ) -> Result<Table, DataError> {
        let token = self.token()?.to_string();
        let body = serde_json::json!({
            "api_name": api_name,
            "token": token,
            "params": params,
            "fields": fields,
        });
        let resp = self.http.post_json(TUSHARE_URL, &[], &body).await?;

        let code = resp.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 0 {
            let msg = resp
                .get("msg")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string();
            return Err(match code {
                CODE_NO_PERMISSION => {
                    warn!(api_name, "tushare token lacks permission (2002)");
                    DataError::NoProvider("tushare permission (积分不足)")
                }
                CODE_RATE_LIMITED => DataError::RateLimited("api.tushare.pro".to_string()),
                _ => DataError::Empty(format!("tushare {api_name} code={code} msg={msg}")),
            });
        }

        let data = resp.get("data").cloned().unwrap_or(serde_json::Value::Null);
        let fields: Vec<String> = data
            .get("fields")
            .and_then(|f| f.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let items: Vec<Vec<serde_json::Value>> = data
            .get("items")
            .and_then(|i| i.as_array())
            .map(|rows| rows.iter().filter_map(|r| r.as_array().cloned()).collect())
            .unwrap_or_default();
        if fields.is_empty() {
            return Err(DataError::Empty(format!(
                "tushare {api_name}: missing fields header"
            )));
        }
        Ok(Table { fields, items })
    }

    /// Gate for 2000-tier APIs: probe the tier once, then allow/deny.
    async fn require_pro(&self, api_name: &'static str) -> Result<(), DataError> {
        self.token()?;
        if *self.tier.lock() == TushareTier::Unknown {
            self.detect_tier().await?;
        }
        match *self.tier.lock() {
            TushareTier::Pro2000 => Ok(()),
            _ => Err(DataError::NoProvider(api_name)),
        }
    }

    /// Probe the token tier with a one-day `adj_factor` request: success →
    /// `Pro2000`, 2002 → `Free120`. The result is cached in-process.
    pub async fn detect_tier(&self) -> Result<TushareTier, DataError> {
        self.token()?;
        let mut params = serde_json::Map::new();
        params.insert("ts_code".to_string(), serde_json::json!("000001.SZ"));
        params.insert("start_date".to_string(), serde_json::json!("20240102"));
        params.insert("end_date".to_string(), serde_json::json!("20240105"));
        let tier = match self
            .call("adj_factor", params, "ts_code,trade_date,adj_factor")
            .await
        {
            Ok(_) => TushareTier::Pro2000,
            Err(DataError::NoProvider(_)) => TushareTier::Free120,
            Err(e) => return Err(e),
        };
        debug!(?tier, "tushare tier detected");
        *self.tier.lock() = tier;
        Ok(tier)
    }

    /// Raw (未复权) daily bars, oldest first. `vol` is lots natively;
    /// `amount` is converted from thousand CNY to yuan. `pct_chg` is kept
    /// verbatim (it is already computed against the ex-adjusted previous
    /// close, matching our adjust engine's ex-day semantics).
    pub async fn daily(
        &self,
        symbol: &Symbol,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Fetched<Vec<Bar>>, DataError> {
        let ts_code = Self::ts_code(symbol);
        let key = format!("tushare_daily_{ts_code}_{start}_{end}");
        if let Some(hit) = self.cache.get::<Fetched<Vec<Bar>>>(&key, ttl::KLINE) {
            return Ok(hit);
        }

        let mut params = serde_json::Map::new();
        params.insert("ts_code".to_string(), serde_json::json!(ts_code));
        params.insert(
            "start_date".to_string(),
            serde_json::json!(start.format("%Y%m%d").to_string()),
        );
        params.insert(
            "end_date".to_string(),
            serde_json::json!(end.format("%Y%m%d").to_string()),
        );
        let table = self
            .call(
                "daily",
                params,
                "ts_code,trade_date,open,high,low,close,pre_close,vol,amount,pct_chg",
            )
            .await?;

        let mut bars = Vec::with_capacity(table.items.len());
        for row in &table.items {
            let Some(date) = table.get_date(row, "trade_date") else {
                continue;
            };
            let (Some(open), Some(high), Some(low), Some(close)) = (
                table.get_f64(row, "open"),
                table.get_f64(row, "high"),
                table.get_f64(row, "low"),
                table.get_f64(row, "close"),
            ) else {
                continue;
            };
            let mut bar = Bar::new(
                date,
                open,
                close,
                high,
                low,
                table.get_f64(row, "vol").unwrap_or(0.0),
                VolumeUnit::Lots,
            );
            bar.amount = table.get_f64(row, "amount").map(|a| a * 1000.0);
            bar.pct = table.get_f64(row, "pct_chg");
            bars.push(bar);
        }
        // tushare returns newest first.
        bars.sort_by_key(|b| b.date);
        if bars.is_empty() {
            return Err(DataError::Empty(format!("tushare daily {ts_code}")));
        }
        let out = Fetched::now(bars, Source::Tushare);
        self.cache.set(&key, &out);
        Ok(out)
    }

    /// Trading calendar (`trade_cal`, SSE) over `[start, end]`.
    pub async fn trade_cal(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<TradeCalDay>, DataError> {
        let mut params = serde_json::Map::new();
        params.insert("exchange".to_string(), serde_json::json!("SSE"));
        params.insert(
            "start_date".to_string(),
            serde_json::json!(start.format("%Y%m%d").to_string()),
        );
        params.insert(
            "end_date".to_string(),
            serde_json::json!(end.format("%Y%m%d").to_string()),
        );
        let table = self
            .call(
                "trade_cal",
                params,
                "exchange,cal_date,is_open,pretrade_date",
            )
            .await?;

        let mut days = Vec::with_capacity(table.items.len());
        for row in &table.items {
            let Some(date) = table.get_date(row, "cal_date") else {
                continue;
            };
            days.push(TradeCalDay {
                date,
                is_open: table.get_f64(row, "is_open").unwrap_or(0.0) == 1.0,
                pretrade_date: table.get_date(row, "pretrade_date"),
            });
        }
        days.sort_by_key(|d| d.date);
        if days.is_empty() {
            return Err(DataError::Empty("tushare trade_cal".to_string()));
        }
        Ok(days)
    }

    /// Cumulative adjustment-factor series (`adj_factor`), 2000-tier only.
    pub async fn adj_factor(
        &self,
        symbol: &Symbol,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<AdjFactorPoint>, DataError> {
        self.require_pro("tushare adj_factor (积分不足 2000)")
            .await?;
        let ts_code = Self::ts_code(symbol);
        let mut params = serde_json::Map::new();
        params.insert("ts_code".to_string(), serde_json::json!(ts_code));
        params.insert(
            "start_date".to_string(),
            serde_json::json!(start.format("%Y%m%d").to_string()),
        );
        params.insert(
            "end_date".to_string(),
            serde_json::json!(end.format("%Y%m%d").to_string()),
        );
        let table = self
            .call("adj_factor", params, "ts_code,trade_date,adj_factor")
            .await?;

        let mut out = Vec::with_capacity(table.items.len());
        for row in &table.items {
            let (Some(date), Some(factor)) = (
                table.get_date(row, "trade_date"),
                table.get_f64(row, "adj_factor"),
            ) else {
                continue;
            };
            out.push(AdjFactorPoint { date, factor });
        }
        out.sort_by_key(|p| p.date);
        if out.is_empty() {
            return Err(DataError::Empty(format!("tushare adj_factor {ts_code}")));
        }
        Ok(out)
    }

    /// Dividend/bonus history (`dividend`, 2000-tier only) mapped onto
    /// per-share [`CorporateAction`]s. Only 实施 rows (those with an
    /// `ex_date`) are kept; `cash_div_tax` is the pre-tax D our engine
    /// wants; `notice_date` prefers 实施公告日 (`imp_ann_date`). Tushare has
    /// no rights-issue (配股) fields, so `rights_*` stay zero — same gap as
    /// the EastMoney source.
    pub async fn dividend(&self, symbol: &Symbol) -> Result<Vec<CorporateAction>, DataError> {
        self.require_pro("tushare dividend (积分不足 2000)").await?;
        let ts_code = Self::ts_code(symbol);
        let mut params = serde_json::Map::new();
        params.insert("ts_code".to_string(), serde_json::json!(ts_code));
        let table = self
            .call(
                "dividend",
                params,
                "ts_code,ex_date,imp_ann_date,ann_date,cash_div_tax,stk_div,stk_bo_rate,stk_co_rate,div_proc",
            )
            .await?;

        let mut out = Vec::new();
        for row in &table.items {
            let Some(ex_date) = table.get_date(row, "ex_date") else {
                continue; // 预案 rows carry no ex_date.
            };
            let notice_date = table
                .get_date(row, "imp_ann_date")
                .or_else(|| table.get_date(row, "ann_date"));
            let cash_div = table.get_f64(row, "cash_div_tax").unwrap_or(0.0);
            let bonus_share = table.get_f64(row, "stk_div").unwrap_or_else(|| {
                table.get_f64(row, "stk_bo_rate").unwrap_or(0.0)
                    + table.get_f64(row, "stk_co_rate").unwrap_or(0.0)
            });
            out.push(CorporateAction {
                ex_date,
                notice_date,
                cash_div,
                bonus_share,
                rights_ratio: 0.0,
                rights_price: None,
            });
        }
        out.sort_by_key(|a| a.ex_date);
        if out.is_empty() {
            return Err(DataError::Empty(format!("tushare dividend {ts_code}")));
        }
        Ok(out)
    }

    /// Daily valuation snapshot (`daily_basic`, 2000-tier only). 万元 → 元.
    pub async fn daily_basic(
        &self,
        symbol: &Symbol,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<DailyBasic>, DataError> {
        self.require_pro("tushare daily_basic (积分不足 2000)")
            .await?;
        let ts_code = Self::ts_code(symbol);
        let mut params = serde_json::Map::new();
        params.insert("ts_code".to_string(), serde_json::json!(ts_code));
        params.insert(
            "start_date".to_string(),
            serde_json::json!(start.format("%Y%m%d").to_string()),
        );
        params.insert(
            "end_date".to_string(),
            serde_json::json!(end.format("%Y%m%d").to_string()),
        );
        let table = self
            .call(
                "daily_basic",
                params,
                "ts_code,trade_date,turnover_rate,pe_ttm,pb,total_mv,circ_mv",
            )
            .await?;

        let mut out = Vec::with_capacity(table.items.len());
        for row in &table.items {
            let Some(date) = table.get_date(row, "trade_date") else {
                continue;
            };
            out.push(DailyBasic {
                date,
                turnover_rate: table.get_f64(row, "turnover_rate"),
                pe_ttm: table.get_f64(row, "pe_ttm"),
                pb: table.get_f64(row, "pb"),
                total_mv: table.get_f64(row, "total_mv").map(|v| v * 1e4),
                circ_mv: table.get_f64(row, "circ_mv").map(|v| v * 1e4),
            });
        }
        out.sort_by_key(|d| d.date);
        if out.is_empty() {
            return Err(DataError::Empty(format!("tushare daily_basic {ts_code}")));
        }
        Ok(out)
    }
}

/// PIT-safe qfq factor from tushare cumulative factors (doc §3.2):
/// `factor_qfq(t, anchor T) = adj(t) / adj(T)`. The ratio only reflects
/// corporate actions inside `(t, T]`, so anchoring at a historical `T` is
/// point-in-time safe for backtests.
pub fn qfq_factor_from_adj(factor_t: f64, factor_anchor: f64) -> f64 {
    factor_t / factor_anchor
}

/// One per-date divergence between the tushare-derived qfq close and the
/// core adjust engine's qfq close.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QfqMismatch {
    /// Trading date.
    pub date: NaiveDate,
    /// `raw.close × adj(t)/adj(T)` from tushare factors.
    pub tushare_qfq: f64,
    /// Close from `astock_core::compute_qfq`.
    pub computed_qfq: f64,
    /// `|tushare - computed| / computed`.
    pub rel_diff: f64,
}

/// Golden cross-check of the core adjust engine against tushare
/// `adj_factor` (data-foundation-v2 首选金标源).
///
/// `raw` are unadjusted bars; `adj` the tushare factor series covering the
/// same dates. The qfq anchor `T` is the last raw bar with a factor. Rows
/// missing a factor are skipped. Returns every date where the two qfq
/// closes differ by more than `tolerance` (relative; 0.005 = the
/// data-foundation-v2 0.5% 容差).
pub fn compare_qfq_golden(
    raw: &[Bar],
    adj: &[AdjFactorPoint],
    actions: &[CorporateAction],
    tolerance: f64,
) -> Vec<QfqMismatch> {
    let Some(anchor) = raw
        .last()
        .and_then(|last| adj.iter().rev().find(|p| p.date <= last.date))
    else {
        return Vec::new();
    };
    if anchor.factor <= 0.0 {
        return Vec::new();
    }
    let computed = compute_qfq(raw, actions, anchor.date, None);
    let mut mismatches = Vec::new();
    for (bar, qfq) in raw.iter().zip(computed.bars.iter()) {
        let Some(point) = adj.iter().find(|p| p.date == bar.date) else {
            continue;
        };
        let tushare_qfq = bar.close * qfq_factor_from_adj(point.factor, anchor.factor);
        let rel_diff = if qfq.close > 0.0 {
            (tushare_qfq - qfq.close).abs() / qfq.close
        } else {
            f64::INFINITY
        };
        if rel_diff > tolerance {
            mismatches.push(QfqMismatch {
                date: bar.date,
                tushare_qfq,
                computed_qfq: qfq.close,
                rel_diff,
            });
        }
    }
    mismatches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn bar(date: &str, close: f64) -> Bar {
        Bar::new(d(date), close, close, close, close, 1.0, VolumeUnit::Lots)
    }

    #[test]
    fn ts_code_mapping() {
        assert_eq!(
            TushareProvider::ts_code(&Symbol::new("600519").unwrap()),
            "600519.SH"
        );
        assert_eq!(
            TushareProvider::ts_code(&Symbol::new("000001").unwrap()),
            "000001.SZ"
        );
        assert_eq!(
            TushareProvider::ts_code(&Symbol::new("920001").unwrap()),
            "920001.BJ"
        );
    }

    #[test]
    fn unavailable_without_token() {
        let p = TushareProvider::new(
            Arc::new(HttpClient::new()),
            Arc::new(TtlCache::default()),
            None,
        );
        assert!(!p.available());
        assert_eq!(p.tier(), TushareTier::Unknown);
    }

    /// Golden: 10派5元 (D = 0.5/share), prev close C = 10 → X = 9.5,
    /// core factor r = X/C = 0.95. The tushare cumulative factor jumps by
    /// C/X on the ex-date, so `adj(t)/adj(T)` must reproduce the core qfq
    /// closes exactly (identity from doc §3.2).
    #[test]
    fn golden_qfq_matches_core_engine_cash_dividend() {
        let raw = vec![bar("2025-06-10", 10.0), bar("2025-06-11", 9.6)];
        // Ex-date 2025-06-11; adj factor goes 1.0 → 10/9.5 on the ex-date.
        let adj = vec![
            AdjFactorPoint {
                date: d("2025-06-10"),
                factor: 1.0,
            },
            AdjFactorPoint {
                date: d("2025-06-11"),
                factor: 10.0 / 9.5,
            },
        ];
        let actions = vec![CorporateAction::new(d("2025-06-11"), 0.5, 0.0)];

        let computed = compute_qfq(&raw, &actions, d("2025-06-11"), None);
        assert!((computed.bars[0].close - 9.5).abs() < 1e-12);
        assert!((computed.bars[1].close - 9.6).abs() < 1e-12);

        let mismatches = compare_qfq_golden(&raw, &adj, &actions, 0.005);
        assert!(mismatches.is_empty(), "{mismatches:?}");
    }

    /// Golden: compounded actions (cash dividend then 10送10 bonus).
    /// r1 = 0.95 (X=9.5/C=10), r2 = 0.5 (X=4.8/C=9.6); cumulative tushare
    /// factors are the running 1/r products.
    #[test]
    fn golden_qfq_matches_core_engine_compound() {
        let raw = vec![
            bar("2025-06-10", 10.0),
            bar("2025-06-11", 9.6),
            bar("2025-06-12", 4.9),
        ];
        let adj = vec![
            AdjFactorPoint {
                date: d("2025-06-10"),
                factor: 1.0,
            },
            AdjFactorPoint {
                date: d("2025-06-11"),
                factor: 10.0 / 9.5,
            },
            AdjFactorPoint {
                date: d("2025-06-12"),
                factor: (10.0 / 9.5) * (9.6 / 4.8),
            },
        ];
        let actions = vec![
            CorporateAction::new(d("2025-06-11"), 0.5, 0.0),
            CorporateAction::new(d("2025-06-12"), 0.0, 1.0),
        ];

        let mismatches = compare_qfq_golden(&raw, &adj, &actions, 0.005);
        assert!(mismatches.is_empty(), "{mismatches:?}");

        // Spot-check the anchor-day passthrough and the compounded factor.
        let computed = compute_qfq(&raw, &actions, d("2025-06-12"), None);
        assert!((computed.bars[2].close - 4.9).abs() < 1e-12);
        assert!((computed.bars[0].close - 10.0 * 0.95 * 0.5).abs() < 1e-9);
    }

    /// A wrong core-side action set must surface as mismatches above the
    /// tolerance (the comparator actually detects divergence).
    #[test]
    fn golden_comparator_detects_divergence() {
        let raw = vec![bar("2025-06-10", 10.0), bar("2025-06-11", 9.6)];
        let adj = vec![
            AdjFactorPoint {
                date: d("2025-06-10"),
                factor: 1.0,
            },
            AdjFactorPoint {
                date: d("2025-06-11"),
                factor: 10.0 / 9.5,
            },
        ];
        // Missing action on the core side → its qfq stays raw on day 1.
        let mismatches = compare_qfq_golden(&raw, &adj, &[], 0.005);
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].date, d("2025-06-10"));
        assert!((mismatches[0].tushare_qfq - 9.5).abs() < 1e-12);
        assert!((mismatches[0].computed_qfq - 10.0).abs() < 1e-12);
    }

    #[test]
    fn qfq_factor_ratio() {
        assert_eq!(qfq_factor_from_adj(2.0, 4.0), 0.5);
    }
}
