//! Consolidated stock-page bundle command (docs/command-contract.md §行情数据).
//!
//! `get_stock_bundle` collapses the stock page's 5+ round trips (quote,
//! kline, fund flow, signal analysis, 缠论) into one invocation: the kline
//! series is fetched exactly once through the persistent read-through cache
//! and both analysis sections are derived from those same bars. Every
//! section degrades independently — failures null out the section and add
//! its name to `missing`; only a quote failure is a hard error.

use astock_core::{FundFlowPoint, Quote};
use astock_market_data::DataProvider;
use serde::Serialize;
use serde_json::Value;
use tauri::State;

use crate::cache_path;
use crate::error::CmdError;
use crate::state::AppState;

use super::analysis::{
    attach_manual_plan, chanlun_from_bars, fetch_shared_context, run_signal_pipeline,
    ANALYZE_FLOW_DAYS,
};
use super::market::{
    clamp_count, parse_adjust, parse_period, parse_symbol, BarJson, FundFlowJson, KlineResponse,
};

/// `get_stock_bundle` response. Every section except `quote` is nullable;
/// `missing` lists the sections that degraded to null.
#[derive(Debug, Serialize)]
pub struct StockBundle {
    /// Realtime quote snapshot (hard error when unavailable).
    pub quote: Quote,
    /// Kline payload `{bars, source}` through the persistent cache
    /// (`source` is the upstream name or `"cache"`).
    pub kline: Option<KlineResponse>,
    /// Daily fund flow of the last 30 trading days (15s upstream TTL).
    pub fund_flow_30d: Option<Vec<FundFlowJson>>,
    /// Signal analysis derived from the same bars as `kline`.
    pub analysis: Option<Value>,
    /// 缠论 daily analysis derived from the same bars as `kline`.
    pub chanlun_daily: Option<Value>,
    /// Names of the sections that failed and were nulled out.
    pub missing: Vec<String>,
}

/// Fold a section result into an `Option`, recording failures in `missing`.
fn record<T>(missing: &mut Vec<String>, name: &str, result: Result<T, CmdError>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(e) => {
            tracing::warn!(section = name, error = %e, "bundle section degraded");
            missing.push(name.to_string());
            None
        }
    }
}

/// One-call stock page payload: quote + cached kline + fund flow + analysis
/// + 缠论, with per-section degradation (`missing`) and quote as the only
/// hard error.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_stock_bundle(
    state: State<'_, AppState>,
    symbol: String,
    period: String,
    adjust: String,
    count: u32,
) -> Result<StockBundle, CmdError> {
    let symbol = parse_symbol(&symbol)?;
    let period = parse_period(&period)?;
    let adjust = parse_adjust(&adjust)?;
    let count = clamp_count(count);

    // Quote is the only hard-failing section (the page is useless without it).
    let quote = state.market.quote(&symbol).await?.data;

    // Kline comes once through the persistent cache; fund flow rides its own
    // 15s TTL path in parallel.
    let (kline, flows) = tokio::join!(
        cache_path::kline_read_through(
            &state.storage,
            &state.market,
            &state.rules,
            &symbol,
            period,
            adjust,
            count,
        ),
        state.market.fund_flow_daily(&symbol, ANALYZE_FLOW_DAYS),
    );

    let mut missing = Vec::new();
    let flows: Option<Vec<FundFlowPoint>> =
        record(&mut missing, "fund_flow_30d", flows.map_err(CmdError::from)).map(|f| f.data);
    let fund_flow_30d = flows
        .as_deref()
        .map(|points| points.iter().map(FundFlowJson::from).collect());

    let kline = record(&mut missing, "kline", kline);

    let (analysis, chanlun_daily) = match &kline {
        Some((bars, _source)) if !bars.is_empty() => {
            let (index_klines, breadth) = fetch_shared_context(&state.market).await;
            let mut analysis = run_signal_pipeline(
                bars,
                Some(&quote),
                flows.as_deref(),
                index_klines.as_deref(),
                breadth.as_ref(),
            );
            attach_manual_plan(&mut analysis, &symbol, &quote, bars, &state.rules, _source);
            let chanlun = record(
                &mut missing,
                "chanlun_daily",
                chanlun_from_bars(&symbol, bars),
            );
            (Some(analysis), chanlun)
        }
        _ => {
            // Analysis and 缠论 derive from the kline bars; without usable
            // bars both degrade together ("kline" itself is already recorded
            // above when the fetch failed).
            missing.push("analysis".to_string());
            missing.push("chanlun_daily".to_string());
            (None, None)
        }
    };

    let kline = kline.map(|(bars, source)| KlineResponse {
        bars: bars.iter().map(BarJson::from).collect(),
        source,
    });

    Ok(StockBundle {
        quote,
        kline,
        fund_flow_30d,
        analysis,
        chanlun_daily,
        missing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn quote() -> Quote {
        Quote {
            symbol: "600519".into(),
            name: "贵州茅台".into(),
            price: 1800.0,
            open: 1790.0,
            high: 1810.0,
            low: 1780.0,
            pre_close: 1795.0,
            volume: 1000.0,
            amount: 1.8e6,
            change: 5.0,
            pct: 0.28,
            turnover: Some(0.3),
            timestamp: Utc.with_ymd_and_hms(2025, 1, 2, 7, 0, 0).unwrap(),
            field_provenance: Default::default(),
        }
    }

    #[test]
    fn record_ok_keeps_value_and_missing_clean() {
        let mut missing = Vec::new();
        let value: Option<u32> = record(&mut missing, "kline", Ok(42));
        assert_eq!(value, Some(42));
        assert!(missing.is_empty());
    }

    #[test]
    fn record_err_nulls_section_and_marks_missing() {
        let mut missing = Vec::new();
        let value: Option<u32> = record(
            &mut missing,
            "fund_flow_30d",
            Err(CmdError::new("network", "boom")),
        );
        assert_eq!(value, None);
        assert_eq!(missing, vec!["fund_flow_30d"]);
    }

    #[test]
    fn partial_bundle_serializes_with_nulls_and_missing_list() {
        let bundle = StockBundle {
            quote: quote(),
            kline: None,
            fund_flow_30d: None,
            analysis: None,
            chanlun_daily: None,
            missing: vec![
                "kline".to_string(),
                "fund_flow_30d".to_string(),
                "analysis".to_string(),
                "chanlun_daily".to_string(),
            ],
        };
        let json = serde_json::to_value(&bundle).unwrap();
        assert_eq!(json["quote"]["symbol"], "600519");
        assert!(json["kline"].is_null());
        assert!(json["fund_flow_30d"].is_null());
        assert!(json["analysis"].is_null());
        assert!(json["chanlun_daily"].is_null());
        assert_eq!(
            json["missing"],
            serde_json::json!(["kline", "fund_flow_30d", "analysis", "chanlun_daily"])
        );
    }

    #[test]
    fn full_bundle_serializes_all_sections() {
        let bundle = StockBundle {
            quote: quote(),
            kline: Some(KlineResponse {
                bars: vec![],
                source: cache_path::CACHE_SOURCE.to_string(),
            }),
            fund_flow_30d: Some(vec![]),
            analysis: Some(serde_json::json!({"score": 80})),
            chanlun_daily: Some(serde_json::json!({"bi": []})),
            missing: vec![],
        };
        let json = serde_json::to_value(&bundle).unwrap();
        assert_eq!(json["kline"]["source"], "cache");
        assert_eq!(json["analysis"]["score"], 80);
        assert!(json["missing"].as_array().unwrap().is_empty());
    }
}
