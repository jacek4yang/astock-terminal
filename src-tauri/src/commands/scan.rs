//! Full-market buy-signal scan (docs/command-contract.md §扫描).
//!
//! Replicates the legacy scanner: fetch the full A-share list, drop
//! ST/*ST/退 names and zero-priced (suspended) stocks, run the same
//! analysis pipeline as `analyze` per stock with bounded concurrency, keep
//! buy signals, sort by score and keep the top 50. Progress is streamed via
//! the `scan-progress` / `scan-result` Tauri events.
//!
//! The market-data layer self-throttles through its adaptive per-host rate
//! limiter, so no extra sleeps are added here.

use std::sync::Arc;

use astock_core::{KlinePeriod, StockListItem, Symbol};
use astock_market_data::{DataProvider, MarketData};
use futures::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tokio_util::sync::CancellationToken;

use crate::error::CmdError;
use crate::state::{AppState, ScanSnapshot, ScanState};

use super::analysis::{analyze_symbol, fetch_shared_context};

/// Maximum concurrent per-stock analyses.
const SCAN_CONCURRENCY: usize = 15;
/// Number of kept results after sorting by score.
const SCAN_TOP_N: usize = 50;
/// Minimum bars required for a meaningful analysis (legacy value).
const MIN_BARS: usize = 30;

/// One kept scan result: `{symbol,name,score,action,confidence}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScanHit {
    /// Bare 6-digit code.
    pub symbol: String,
    /// Display name (quote name, falling back to the list name).
    pub name: String,
    /// Composite signal score.
    pub score: f64,
    /// Signal action (买入 / 强烈买入 / 谨慎买入).
    pub action: String,
    /// Signal confidence.
    pub confidence: f64,
}

/// `scan_start` response.
#[derive(Debug, Serialize)]
pub struct ScanStartResponse {
    /// Always true on success.
    pub started: bool,
}

/// `scan_cancel` response.
#[derive(Debug, Serialize)]
pub struct ScanCancelResponse {
    /// Whether a running scan was signalled to stop.
    pub cancelled: bool,
}

/// Legacy pre-filter: drop ST/*ST/退 names and suspended (price ≤ 0) stocks.
/// `"ST"` also matches `"*ST"`.
pub(crate) fn scan_prefilter(items: &[StockListItem]) -> Vec<StockListItem> {
    items
        .iter()
        .filter(|s| !s.name.contains("ST") && !s.name.contains('退') && s.price > 0.0)
        .cloned()
        .collect()
}

/// Whether an action string is a kept buy signal.
pub(crate) fn is_buy_action(action: &str) -> bool {
    matches!(action, "强烈买入" | "买入" | "谨慎买入")
}

/// Extract a scan hit from a signal JSON; `None` when it is not a buy.
pub(crate) fn hit_from_signal(
    symbol: &str,
    name: String,
    signal: &serde_json::Value,
) -> Option<ScanHit> {
    let action = signal.get("action").and_then(serde_json::Value::as_str)?;
    if !is_buy_action(action) {
        return None;
    }
    Some(ScanHit {
        symbol: symbol.to_string(),
        name,
        score: signal
            .get("score")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
        action: action.to_string(),
        confidence: signal
            .get("confidence")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
    })
}

/// Start the background full-market scan. Fails with kind
/// `already_running` when a scan is in flight.
#[tauri::command(rename_all = "snake_case")]
pub async fn scan_start(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<ScanStartResponse, CmdError> {
    {
        let mut snapshot = state.scan.snapshot.lock().expect("scan snapshot poisoned");
        if snapshot.running {
            return Err(CmdError::new(
                "already_running",
                "a scan is already running; call scan_cancel first",
            ));
        }
        *snapshot = ScanSnapshot {
            running: true,
            ..Default::default()
        };
    }
    let token = CancellationToken::new();
    *state.scan.cancel.lock().expect("scan cancel poisoned") = Some(token.clone());

    let market = Arc::clone(&state.market);
    let scan = Arc::clone(&state.scan);
    tauri::async_runtime::spawn(run_scan(market, scan, app, token));
    Ok(ScanStartResponse { started: true })
}

/// Poll the current scan snapshot.
#[tauri::command(rename_all = "snake_case")]
pub async fn scan_status(state: State<'_, AppState>) -> Result<ScanSnapshot, CmdError> {
    Ok(state
        .scan
        .snapshot
        .lock()
        .expect("scan snapshot poisoned")
        .clone())
}

/// Cancel a running scan (cooperative; in-flight analyses finish).
#[tauri::command(rename_all = "snake_case")]
pub async fn scan_cancel(state: State<'_, AppState>) -> Result<ScanCancelResponse, CmdError> {
    let token = state
        .scan
        .cancel
        .lock()
        .expect("scan cancel poisoned")
        .clone();
    let cancelled = if let Some(token) = token {
        token.cancel();
        true
    } else {
        false
    };
    Ok(ScanCancelResponse { cancelled })
}

/// The scan task body: pre-filter → bounded-concurrency analysis → keep
/// buy signals → sort by score → top 50.
async fn run_scan(
    market: Arc<MarketData>,
    scan: Arc<ScanState>,
    app: AppHandle,
    token: CancellationToken,
) {
    let list = match market.all_a_shares().await {
        Ok(f) => f.data,
        Err(e) => {
            tracing::error!(error = %e, "scan aborted: failed to fetch A-share list");
            finish_scan(&scan);
            return;
        }
    };
    let filtered = scan_prefilter(&list);
    tracing::info!(
        total = list.len(),
        filtered = filtered.len(),
        "scan started (excluded ST/退/停牌)"
    );
    let total = filtered.len() as u32;
    {
        let mut snapshot = scan.snapshot.lock().expect("scan snapshot poisoned");
        snapshot.total = total;
    }

    // Shared context fetched once for all stocks (legacy behaviour).
    let (index_klines, breadth) = fetch_shared_context(&market).await;

    let stream = futures::stream::iter(filtered)
        .map(|stock| {
            let market = Arc::clone(&market);
            let index_klines = index_klines.clone();
            let breadth = breadth.clone();
            async move {
                let code = stock.code.clone();
                let hit = match Symbol::new(&stock.code) {
                    Ok(symbol) => {
                        match analyze_symbol(
                            &market,
                            &symbol,
                            KlinePeriod::Day,
                            index_klines.as_deref(),
                            breadth.as_ref(),
                            MIN_BARS,
                        )
                        .await
                        {
                            Ok((signal, quote)) => {
                                let name = quote
                                    .as_ref()
                                    .map(|q| q.name.clone())
                                    .filter(|n| !n.is_empty())
                                    .unwrap_or(stock.name);
                                hit_from_signal(&code, name, &signal)
                            }
                            Err(e) => {
                                tracing::debug!(%code, error = %e, "scan: analysis failed, skipped");
                                None
                            }
                        }
                    }
                    Err(_) => None,
                };
                (code, hit)
            }
        })
        .buffer_unordered(SCAN_CONCURRENCY)
        .take_until(token.clone().cancelled_owned());
    tokio::pin!(stream);

    while let Some((code, hit)) = stream.next().await {
        let (done, emitted_hit) = {
            let mut snapshot = scan.snapshot.lock().expect("scan snapshot poisoned");
            snapshot.done += 1;
            snapshot.current_symbol = code.clone();
            if let Some(hit) = hit {
                snapshot.results.push(hit.clone());
                (snapshot.done, Some(hit))
            } else {
                (snapshot.done, None)
            }
        };
        let _ = app.emit(
            "scan-progress",
            serde_json::json!({
                "done": done,
                "total": total,
                "current_symbol": code,
            }),
        );
        if let Some(hit) = emitted_hit {
            let _ = app.emit("scan-result", &hit);
        }
    }

    // Final ranking: score desc, top 50.
    {
        let mut snapshot = scan.snapshot.lock().expect("scan snapshot poisoned");
        snapshot.results.sort_by(|a, b| b.score.total_cmp(&a.score));
        snapshot.results.truncate(SCAN_TOP_N);
    }
    finish_scan(&scan);
    tracing::info!("scan finished");
}

/// Mark the scan as no longer running and drop the cancel token.
fn finish_scan(scan: &ScanState) {
    scan.snapshot
        .lock()
        .expect("scan snapshot poisoned")
        .running = false;
    *scan.cancel.lock().expect("scan cancel poisoned") = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(code: &str, name: &str, price: f64) -> StockListItem {
        StockListItem {
            code: code.into(),
            name: name.into(),
            price,
            pct: 0.0,
            amount: 0.0,
        }
    }

    #[test]
    fn prefilter_drops_st_delisted_and_suspended() {
        let items = vec![
            item("600519", "贵州茅台", 1800.0),
            item("000001", "平安银行", 10.0),
            item("600001", "ST股", 2.0),
            item("600002", "*ST股", 2.0),
            item("600003", "退市股", 1.0),
            item("600004", "停牌股", 0.0),
        ];
        let kept = scan_prefilter(&items);
        let codes: Vec<&str> = kept.iter().map(|s| s.code.as_str()).collect();
        assert_eq!(codes, ["600519", "000001"]);
    }

    #[test]
    fn buy_action_classification() {
        assert!(is_buy_action("强烈买入"));
        assert!(is_buy_action("买入"));
        assert!(is_buy_action("谨慎买入"));
        assert!(!is_buy_action("观望"));
        assert!(!is_buy_action("卖出"));
        assert!(!is_buy_action(""));
    }

    #[test]
    fn hit_extraction_from_signal_json() {
        let signal = serde_json::json!({
            "action": "买入",
            "score": 66.5,
            "confidence": 70.0,
        });
        let hit = hit_from_signal("600519", "贵州茅台".into(), &signal).unwrap();
        assert_eq!(hit.symbol, "600519");
        assert_eq!(hit.score, 66.5);
        assert_eq!(hit.confidence, 70.0);

        let hold = serde_json::json!({"action": "观望", "score": 50.0, "confidence": 50.0});
        assert!(hit_from_signal("600519", "x".into(), &hold).is_none());

        // Missing action -> not kept.
        assert!(hit_from_signal("600519", "x".into(), &serde_json::json!({})).is_none());
    }

    #[test]
    fn hit_serializes_to_contract_shape() {
        let hit = ScanHit {
            symbol: "600519".into(),
            name: "贵州茅台".into(),
            score: 80.0,
            action: "强烈买入".into(),
            confidence: 85.0,
        };
        let json = serde_json::to_value(&hit).unwrap();
        for key in ["symbol", "name", "score", "action", "confidence"] {
            assert!(json.get(key).is_some(), "missing {key}");
        }
    }
}
