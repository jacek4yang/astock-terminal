//! The deep-research tool set (v2): fundamentals, valuation, the supply-chain
//! knowledge graph, event propagation, cross-asset relationship graphs,
//! backtesting and market-regime classification.
//!
//! Every number comes from the deterministic engines (`astock-fundamental`,
//! `astock-graph`, `astock-quant`, `astock-backtest`) or upstream payloads —
//! never from the model. `summary_json` is the compact payload the LLM sees;
//! `full_json` lands in `tool_cache` for `get_cached_detail` drill-down.
//!
//! The pure projection helpers are `pub` so the Tauri command layer
//! (`src-tauri/src/commands/deep.rs`) can expose the same engines to the UI
//! without duplicating the mapping logic.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use futures::StreamExt;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use astock_backtest::data::PriceSeries;
use astock_backtest::engine::{BacktestEngine, EngineConfig as BtConfig};
use astock_backtest::metrics::MetricsConfig;
use astock_backtest::strategy::{BuyHold, MaCross, Strategy, TurtleBreakout};
use astock_core::{Adjust, Bar, KlinePeriod, Symbol};
use astock_fundamental::model::{
    BalanceSheet, CashFlowStatement, FundamentalBundle, IncomeStatement, PeriodMeta, ReportType,
    ValuationPoint,
};
use astock_fundamental::{anomaly, metrics, scores, valuation, FundamentalClient};
use astock_graph::{
    Engine as GraphEngine, Event, GraphStore, ImpactEntry, ImpactReport, Node, Relation,
};
use astock_market_data::DataProvider;
use astock_technical as tech;
use astock_trading_rules::{RuleSet, TradeSide};
use chrono::NaiveDate;

use crate::builtin::{parse_symbol, r2, r4, tool_err};
use crate::error::{AgentError, Result};
use crate::tools::{now_secs, parse_args, schema_value, AgentTool, ToolContext, ToolResult};

/// The supply-chain graph, or a clean capability-unavailable error.
fn require_graph<'a>(ctx: &'a ToolContext, tool: &str) -> Result<&'a GraphStore> {
    ctx.graph
        .as_ref()
        .ok_or_else(|| tool_err(tool, "图谱能力不可用：当前上下文未装配 GraphStore"))
}

/// The fundamental client, or a clean capability-unavailable error.
fn require_fundamental<'a>(ctx: &'a ToolContext, tool: &str) -> Result<&'a FundamentalClient> {
    ctx.fundamental
        .as_deref()
        .ok_or_else(|| tool_err(tool, "基本面能力不可用：当前上下文未装配 FundamentalClient"))
}

/// RFC 3339 "now" without chrono's `clock` feature.
fn now_rfc3339() -> String {
    chrono::DateTime::from_timestamp(now_secs(), 0)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

fn invalid_args(tool: &str, msg: impl Into<String>) -> AgentError {
    AgentError::InvalidArgs {
        tool: tool.to_string(),
        msg: msg.into(),
    }
}

// ---------------------------------------------------------------------
// get_fundamentals
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct FundamentalsArgs {
    /// 6位证券代码，如 600519、000001
    symbol: String,
}

/// Full fundamental snapshot: profile, headline metrics, growth, scores, flags.
pub struct GetFundamentals;

#[async_trait]
impl AgentTool for GetFundamentals {
    fn name(&self) -> &'static str {
        "get_fundamentals"
    }
    fn description(&self) -> &'static str {
        "获取公司基本面全景：概况、最新指标（ROE/毛利率/FCF/收现比/资产负债率）、同比环比、Piotroski F-score/Altman Z-score 与异常预警（完整数据入缓存）"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<FundamentalsArgs>()
    }
    fn cache_ttl_secs(&self) -> i64 {
        3600
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args: FundamentalsArgs = parse_args(self.name(), args)?;
        let symbol = parse_symbol(self.name(), &args.symbol)?;
        let client = require_fundamental(ctx, self.name())?;
        let outcome = client.bundle(&symbol).await;
        let full = fundamentals_full_json(&symbol, &outcome.bundle, &outcome.failures);
        let summary = fundamentals_summary(&full);
        Ok(ToolResult {
            summary_json: summary,
            full_json: Some(full),
            cache_key: String::new(),
            source: "eastmoney_f10".to_string(),
            fetched_at: now_rfc3339(),
        })
    }
}

/// Annual-report rows of one statement vec, oldest first.
fn annual_rows<T>(rows: &[T], meta: impl Fn(&T) -> Option<PeriodMeta>) -> Vec<&T> {
    rows.iter()
        .filter(|r| meta(r).is_some_and(|m| m.report_type == ReportType::Annual))
        .collect()
}

/// YoY map (`period_end → growth`) over the full statement history.
fn yoy_map(
    rows: &[IncomeStatement],
    field: impl Fn(&IncomeStatement) -> Option<f64>,
) -> HashMap<NaiveDate, f64> {
    let series = metrics::series(rows, |s| s.meta, field);
    metrics::yoy_growth(&series).into_iter().collect()
}

/// `q1` / `h1` / `q3` / `annual`.
fn report_type_str(rt: ReportType) -> &'static str {
    match rt {
        ReportType::Q1 => "q1",
        ReportType::H1 => "h1",
        ReportType::Q3 => "q3",
        ReportType::Annual => "annual",
    }
}

/// Section prefixes from `BundleOutcome::failures` (`"income: ..."` → `"income"`).
fn failure_sections(failures: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in failures {
        if let Some(prefix) = f.split(':').next() {
            if !out.iter().any(|m| m == prefix) {
                out.push(prefix.to_string());
            }
        }
    }
    out
}

/// Latest period metadata + headline metrics off the latest income statement.
fn latest_metrics_json(bundle: &FundamentalBundle) -> (Value, Value) {
    let Some((inc, meta)) = bundle.income.iter().rev().find_map(|s| s.meta.map(|m| (s, m))) else {
        return (Value::Null, Value::Null);
    };
    let empty_bs = BalanceSheet::default();
    let empty_cf = CashFlowStatement::default();
    let bs_end = bundle.balance.last().unwrap_or(&empty_bs);
    let bs_begin = match bundle.balance.len().checked_sub(2) {
        Some(i) => &bundle.balance[i],
        None => &empty_bs,
    };
    let cf = bundle.cashflow.last().unwrap_or(&empty_cf);

    let rev_yoy = yoy_map(&bundle.income, |s| s.total_operating_revenue);
    let prof_yoy = yoy_map(&bundle.income, |s| s.net_profit_parent.or(s.net_profit));
    let sq = metrics::to_single_quarters(&bundle.income);
    let rev_qoq: HashMap<NaiveDate, f64> =
        metrics::qoq_growth(&metrics::series(&sq, |s| s.meta, |s| s.total_operating_revenue))
            .into_iter()
            .collect();
    let prof_qoq: HashMap<NaiveDate, f64> = metrics::qoq_growth(&metrics::series(&sq, |s| s.meta, |s| {
        s.net_profit_parent.or(s.net_profit)
    }))
    .into_iter()
    .collect();

    // 收现比 = 销售商品收到的现金 / 营业收入。
    let cash_ratio = metrics::div_public(
        cf.cash_from_sales,
        inc.operating_revenue.filter(|r| *r > 0.0),
    );
    let debt_ratio = metrics::debt_to_assets(bs_end.total_liabilities, bs_end.total_assets)
        .or_else(|| {
            bundle
                .indicators
                .iter()
                .rev()
                .find_map(|i| i.debt_ratio.map(|d| d / 100.0))
        });

    let period = json!({
        "period_end": meta.period_end.to_string(),
        "report_type": report_type_str(meta.report_type),
        "announced": meta.announced.map(|d| d.to_string()),
    });
    let m = json!({
        "revenue": inc.total_operating_revenue,
        "net_profit": inc.net_profit_parent.or(inc.net_profit),
        "revenue_yoy": rev_yoy.get(&meta.period_end).copied(),
        "profit_yoy": prof_yoy.get(&meta.period_end).copied(),
        "revenue_qoq": rev_qoq.get(&meta.period_end).copied(),
        "profit_qoq": prof_qoq.get(&meta.period_end).copied(),
        "gross_margin": metrics::gross_margin(inc.operating_revenue, inc.operating_cost),
        "net_margin": metrics::net_margin(inc.net_profit, inc.total_operating_revenue),
        "roe": metrics::roe(inc.net_profit_parent, bs_begin.total_parent_equity, bs_end.total_parent_equity),
        "roic": metrics::roic(
            metrics::nopat(inc),
            metrics::invested_capital(bs_begin),
            metrics::invested_capital(bs_end),
        ),
        "fcf": metrics::fcf(cf.net_cfo, cf.capex),
        "cash_ratio": cash_ratio,
        "cfo_to_net_income": metrics::cfo_to_net_income(cf.net_cfo, inc.net_profit),
        "debt_ratio": debt_ratio,
        "current_ratio": metrics::current_ratio(bs_end.total_current_assets, bs_end.total_current_liabilities),
    });
    (period, m)
}

/// Last ~12 periods of revenue/profit with YoY and margin.
fn growth_series_json(bundle: &FundamentalBundle) -> Vec<Value> {
    let rows: Vec<(&IncomeStatement, PeriodMeta)> = bundle
        .income
        .iter()
        .filter_map(|s| s.meta.map(|m| (s, m)))
        .collect();
    let rev_yoy = yoy_map(&bundle.income, |s| s.total_operating_revenue);
    let prof_yoy = yoy_map(&bundle.income, |s| s.net_profit_parent.or(s.net_profit));
    let start = rows.len().saturating_sub(12);
    rows[start..]
        .iter()
        .map(|(s, meta)| {
            json!({
                "period_end": meta.period_end.to_string(),
                "revenue": s.total_operating_revenue,
                "net_profit": s.net_profit_parent.or(s.net_profit),
                "revenue_yoy": rev_yoy.get(&meta.period_end).copied(),
                "profit_yoy": prof_yoy.get(&meta.period_end).copied(),
                "gross_margin": metrics::gross_margin(s.operating_revenue, s.operating_cost),
            })
        })
        .collect()
}

/// Piotroski F-score over the last two annual reports.
fn piotroski_json(
    inc_a: &[&IncomeStatement],
    cf_a: &[&CashFlowStatement],
    bs_a: &[&BalanceSheet],
) -> Option<Value> {
    let inc_curr = *inc_a.last()?;
    let inc_prev = *inc_a.get(inc_a.len().checked_sub(2)?)?;
    let empty_bs = BalanceSheet::default();
    let empty_cf = CashFlowStatement::default();
    let bs_curr = bs_a.last().copied().unwrap_or(&empty_bs);
    let bs_prev = bs_a.len().checked_sub(2).map(|i| bs_a[i]).unwrap_or(&empty_bs);
    let bs_open_prev = bs_a.len().checked_sub(3).map(|i| bs_a[i]).unwrap_or(&empty_bs);
    let cf_curr = cf_a.last().copied().unwrap_or(&empty_cf);
    let input =
        scores::piotroski_input_from(inc_curr, inc_prev, cf_curr, bs_open_prev, bs_prev, bs_curr);
    let f = scores::piotroski(&input);
    Some(json!({
        "score": f.score,
        "available": f.available,
        "criteria": f.criteria.iter().map(|c| json!({"name": c.name, "passed": c.passed})).collect::<Vec<_>>(),
    }))
}

/// Altman Z (both variants) off the latest annual report.
fn altman_json(bundle: &FundamentalBundle) -> Option<Value> {
    let inc_a = annual_rows(&bundle.income, |s| s.meta);
    let bs_a = annual_rows(&bundle.balance, |s| s.meta);
    let inc = *inc_a.last()?;
    let bs = *bs_a.last()?;
    let z = scores::altman(&scores::AltmanInput {
        working_capital: metrics::working_capital(bs.total_current_assets, bs.total_current_liabilities),
        retained_earnings: bs.retained_earnings,
        ebit: scores::altman_ebit(inc),
        market_cap: bundle.snapshot.as_ref().and_then(|s| s.total_market_cap),
        book_equity: bs.total_equity,
        total_liabilities: bs.total_liabilities,
        total_assets: bs.total_assets,
        revenue: inc.total_operating_revenue,
    });
    let zone = z
        .emerging_zone
        .or(z.classic_zone)
        .map(|zone| match zone {
            scores::AltmanZone::Safe => "safe",
            scores::AltmanZone::Grey => "grey",
            scores::AltmanZone::Distress => "distress",
        });
    Some(json!({
        "z_classic": z.classic,
        "z_emerging": z.z_emerging,
        "zone": zone,
        "note": "Altman Z''(新兴市场版)：>2.60 安全，1.10–2.60 灰色，<1.10 困境",
    }))
}

/// Beneish M-score over the last two annual reports.
fn beneish_json(
    inc_a: &[&IncomeStatement],
    cf_a: &[&CashFlowStatement],
    bs_a: &[&BalanceSheet],
) -> Option<Value> {
    let inc_curr = *inc_a.last()?;
    let inc_prev = *inc_a.get(inc_a.len().checked_sub(2)?)?;
    let empty_bs = BalanceSheet::default();
    let empty_cf = CashFlowStatement::default();
    let bs_curr = bs_a.last().copied().unwrap_or(&empty_bs);
    let bs_prev = bs_a.len().checked_sub(2).map(|i| bs_a[i]).unwrap_or(&empty_bs);
    let cf_curr = cf_a.last().copied().unwrap_or(&empty_cf);
    let cf_prev = cf_a.len().checked_sub(2).map(|i| cf_a[i]).unwrap_or(&empty_cf);
    let m = scores::beneish(&scores::beneish_indices_from(
        inc_curr, inc_prev, cf_curr, cf_prev, bs_curr, bs_prev,
    ));
    Some(json!({
        "m_score": m.total,
        "likely_manipulator": m.likely_manipulator,
        "cutoff": scores::BENEISH_CUTOFF,
    }))
}

/// Score bundle; null when no sub-score is computable.
fn scores_json(bundle: &FundamentalBundle) -> Value {
    let inc_a = annual_rows(&bundle.income, |s| s.meta);
    let cf_a = annual_rows(&bundle.cashflow, |s| s.meta);
    let bs_a = annual_rows(&bundle.balance, |s| s.meta);
    let piotroski = piotroski_json(&inc_a, &cf_a, &bs_a);
    let altman = altman_json(bundle);
    let beneish = beneish_json(&inc_a, &cf_a, &bs_a);
    if piotroski.is_none() && altman.is_none() && beneish.is_none() {
        Value::Null
    } else {
        json!({"piotroski": piotroski, "altman": altman, "beneish": beneish})
    }
}

/// Snake-case flag category.
fn flag_kind_str(kind: anomaly::FlagKind) -> &'static str {
    match kind {
        anomaly::FlagKind::RevenueUpCfoDown => "revenue_up_cfo_down",
        anomaly::FlagKind::ReceivablesOutpaceRevenue => "receivables_outpace_revenue",
        anomaly::FlagKind::InventorySpike => "inventory_spike",
        anomaly::FlagKind::GoodwillHeavy => "goodwill_heavy",
        anomaly::FlagKind::MarginOutlier => "margin_outlier",
        anomaly::FlagKind::CashAndDebtBothHigh => "cash_and_debt_both_high",
    }
}

fn severity_str(severity: anomaly::Severity) -> &'static str {
    match severity {
        anomaly::Severity::Info => "info",
        anomaly::Severity::Warn => "warn",
        anomaly::Severity::High => "high",
    }
}

/// Red flags over the annual history.
fn anomalies_json(bundle: &FundamentalBundle) -> Vec<Value> {
    let inc_a = annual_rows(&bundle.income, |s| s.meta);
    let bs_by_period: HashMap<NaiveDate, &BalanceSheet> = annual_rows(&bundle.balance, |s| s.meta)
        .into_iter()
        .filter_map(|bs| Some((bs.meta?.period_end, bs)))
        .collect();
    let cf_by_period: HashMap<NaiveDate, &CashFlowStatement> =
        annual_rows(&bundle.cashflow, |s| s.meta)
            .into_iter()
            .filter_map(|cf| Some((cf.meta?.period_end, cf)))
            .collect();
    let empty_bs = BalanceSheet::default();
    let empty_cf = CashFlowStatement::default();
    let history: Vec<anomaly::PeriodObservation> = inc_a
        .iter()
        .map(|inc| {
            let period_end = inc.meta.map(|m| m.period_end);
            let bs = period_end
                .and_then(|pe| bs_by_period.get(&pe).copied())
                .unwrap_or(&empty_bs);
            let cf = period_end
                .and_then(|pe| cf_by_period.get(&pe).copied())
                .unwrap_or(&empty_cf);
            anomaly::PeriodObservation {
                revenue: inc.total_operating_revenue,
                cfo: cf.net_cfo,
                receivables: bs.notes_and_accounts_receivable.or(bs.accounts_receivable),
                inventory: bs.inventory,
                operating_cost: inc.operating_cost,
                goodwill: bs.goodwill,
                equity: bs.total_parent_equity,
                monetary_funds: bs.monetary_funds,
                interest_bearing_debt: bs.interest_bearing_debt(),
                total_assets: bs.total_assets,
                gross_margin: metrics::gross_margin(inc.operating_revenue, inc.operating_cost),
                net_margin: metrics::net_margin(inc.net_profit, inc.total_operating_revenue),
            }
        })
        .collect();
    anomaly::detect(&history)
        .iter()
        .map(|f| {
            let evidence: serde_json::Map<String, Value> = f
                .evidence
                .iter()
                .map(|(k, v)| (k.clone(), Value::from(*v)))
                .collect();
            json!({
                "kind": flag_kind_str(f.kind),
                "severity": severity_str(f.severity),
                "explanation": f.explanation,
                "evidence": Value::Object(evidence),
            })
        })
        .collect()
}

/// Pure projection: bundle → the full `get_fundamentals` payload.
pub fn fundamentals_full_json(
    symbol: &Symbol,
    bundle: &FundamentalBundle,
    failures: &[String],
) -> Value {
    let profile = bundle.profile.as_ref().map(|p| {
        json!({
            "name": if p.short_name.is_empty() { p.name.clone() } else { p.short_name.clone() },
            "full_name": p.name,
            "industry": p.industry,
            "industry_csrc": p.industry_csrc,
            "listing_date": p.listing_date.map(|d| d.to_string()),
            "total_shares": p.total_shares,
            "float_shares": p.float_shares,
        })
    });
    let (latest_period, latest_metrics) = latest_metrics_json(bundle);
    let growth_series = growth_series_json(bundle);
    let scores = scores_json(bundle);
    let anomalies = anomalies_json(bundle);
    let dividends: Vec<Value> = bundle
        .dividends
        .iter()
        .rev()
        .take(10)
        .map(|d| {
            json!({
                "report_date": d.report_date.map(|d| d.to_string()),
                "plan": d.plan,
                "ex_date": d.ex_dividend_date.map(|d| d.to_string()),
            })
        })
        .collect();

    let mut missing = failure_sections(failures);
    for (name, absent) in [
        ("profile", profile.is_none()),
        ("metrics", latest_metrics.is_null()),
        ("growth_series", growth_series.is_empty()),
        ("scores", scores.is_null()),
    ] {
        if absent && !missing.iter().any(|m| m == name) {
            missing.push(name.to_string());
        }
    }
    json!({
        "symbol": symbol.code(),
        "profile": profile,
        "latest_period": latest_period,
        "metrics": latest_metrics,
        "growth_series": growth_series,
        "scores": scores,
        "anomalies": anomalies,
        "dividends": dividends,
        "missing": missing,
    })
}

/// Compact LLM-facing summary of the full fundamentals payload.
fn fundamentals_summary(full: &Value) -> Value {
    let get = |k: &str| full.get(k).cloned().unwrap_or(Value::Null);
    let metrics = &full["metrics"];
    let mget = |k: &str| metrics.get(k).cloned().unwrap_or(Value::Null);
    let fcf_yi = metrics
        .get("fcf")
        .and_then(Value::as_f64)
        .map(|v| r2(v / 1e8));
    let scores = &full["scores"];
    let piotroski = scores
        .get("piotroski")
        .filter(|p| !p.is_null())
        .map(|p| format!("{}/{}", p["score"].as_u64().unwrap_or(0), p["available"].as_u64().unwrap_or(0)));
    let anomalies = full["anomalies"].as_array().cloned().unwrap_or_default();
    let max_severity = anomalies
        .iter()
        .filter_map(|a| a["severity"].as_str())
        .max_by_key(|s| match *s {
            "high" => 2,
            "warn" => 1,
            _ => 0,
        });
    json!({
        "symbol": get("symbol"),
        "profile": get("profile"),
        "latest_period": get("latest_period"),
        "metrics": {
            "roe": mget("roe"),
            "gross_margin": mget("gross_margin"),
            "fcf_yi": fcf_yi,
            "cash_ratio": mget("cash_ratio"),
            "debt_ratio": mget("debt_ratio"),
        },
        "growth": {
            "revenue_yoy": mget("revenue_yoy"),
            "profit_yoy": mget("profit_yoy"),
            "revenue_qoq": mget("revenue_qoq"),
            "profit_qoq": mget("profit_qoq"),
        },
        "scores": {
            "piotroski": piotroski,
            "altman_z_emerging": scores.get("altman").and_then(|a| a.get("z_emerging")).cloned(),
            "altman_zone": scores.get("altman").and_then(|a| a.get("zone")).cloned(),
        },
        "anomalies": {
            "count": anomalies.len(),
            "max_severity": max_severity,
            "kinds": anomalies.iter().filter_map(|a| a["kind"].as_str()).collect::<Vec<_>>(),
        },
        "missing": get("missing"),
        "note": "比率为小数（0.1=10%）；fcf_yi 单位为亿元；完整数据见缓存",
    })
}

// ---------------------------------------------------------------------
// run_valuation
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct ValuationArgs {
    /// 6位证券代码
    symbol: String,
    /// 可选：DCF 一阶段 FCF 年增速（小数，如 0.10=10%）；默认取近5年年度营收同比均值并钳制在 0–25%
    growth: Option<f64>,
    /// 可选：DCF 折现率 WACC（小数）；默认 0.09
    wacc: Option<f64>,
}

/// Default DCF assumptions (documented in the tool description).
const DCF_STAGE1_YEARS: u32 = 5;
/// Default discount rate.
const DCF_WACC: f64 = 0.09;
/// Perpetuity growth after stage 1 (below the WACC).
const DCF_TERMINAL_GROWTH: f64 = 0.025;
/// Bull/bear shift applied to stage-1 growth and WACC.
const DCF_SPREAD: f64 = 0.02;
/// Default growth clamps.
const DCF_GROWTH_FLOOR: f64 = 0.0;
/// Upper clamp for the default stage-1 growth.
const DCF_GROWTH_CAP: f64 = 0.25;
/// Sensitivity grid axes (rows = WACC, columns = terminal growth).
const DCF_SENSITIVITY_WACCS: [f64; 5] = [0.07, 0.08, 0.09, 0.10, 0.11];
/// Sensitivity terminal-growth axis.
const DCF_SENSITIVITY_GROWTHS: [f64; 5] = [0.015, 0.02, 0.025, 0.03, 0.035];
/// Percentile method label.
const PERCENTILE_METHOD: &str =
    "历史分位 = 日频估值序列(RPT_VALUEANALYSIS_DET)中 ≤ 当前值的占比 × 100";
/// DCF caveat.
const DCF_CAVEAT: &str =
    "DCF 对折现率与永续增长率高度敏感，且基于自由现金流代理值(CFO−capex)，区间仅供参考";

/// Current multiples + historical percentiles + DCF scenario range.
pub struct RunValuation;

#[async_trait]
impl AgentTool for RunValuation {
    fn name(&self) -> &'static str {
        "run_valuation"
    }
    fn description(&self) -> &'static str {
        "估值分析：当前 PE_TTM/PB/PS 与历史分位（日频估值序列占比法），两阶段 FCFF DCF 三情景（bear/base/bull）区间；默认假设：一阶段5年、永续增长2.5%、WACC 9%、情景点差±2%、基准增速取近5年营收同比均值钳制0–25%（可用 growth/wacc 覆盖）"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<ValuationArgs>()
    }
    fn cache_ttl_secs(&self) -> i64 {
        3600
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args: ValuationArgs = parse_args(self.name(), args)?;
        let symbol = parse_symbol(self.name(), &args.symbol)?;
        let client = require_fundamental(ctx, self.name())?;
        let outcome = client.bundle(&symbol).await;
        let full = valuation_full_json(&symbol, &outcome.bundle, args.growth, args.wacc, &outcome.failures);
        let summary = valuation_summary(&full);
        Ok(ToolResult {
            summary_json: summary,
            full_json: Some(full),
            cache_key: String::new(),
            source: "eastmoney_f10".to_string(),
            fetched_at: now_rfc3339(),
        })
    }
}

/// Historical percentiles over the full daily valuation series.
fn percentile_json(bundle: &FundamentalBundle) -> Value {
    if bundle.valuation_history.is_empty() {
        return Value::Null;
    }
    let hist = |field: fn(&ValuationPoint) -> Option<f64>| -> Vec<f64> {
        bundle.valuation_history.iter().filter_map(field).collect()
    };
    let pe_hist = hist(|p| p.pe_ttm);
    let pb_hist = hist(|p| p.pb_mrq);
    let ps_hist = hist(|p| p.ps_ttm);
    let last = bundle.valuation_history.last();
    let cur_pe = bundle
        .snapshot
        .as_ref()
        .and_then(|s| s.pe_ttm)
        .or_else(|| last.and_then(|h| h.pe_ttm));
    let cur_pb = bundle
        .snapshot
        .as_ref()
        .and_then(|s| s.pb)
        .or_else(|| last.and_then(|h| h.pb_mrq));
    let cur_ps = last.and_then(|h| h.ps_ttm);
    json!({
        "pe_ttm_pct": cur_pe.and_then(|c| valuation::percentile(&pe_hist, c)),
        "pb_pct": cur_pb.and_then(|c| valuation::percentile(&pb_hist, c)),
        "ps_pct": cur_ps.and_then(|c| valuation::percentile(&ps_hist, c)),
        "days": bundle.valuation_history.len(),
        "method": PERCENTILE_METHOD,
    })
}

/// Default stage-1 growth: mean of the last 5 annual revenue YoY values,
/// clamped into 0–25% (0% when no history exists).
fn capped_base_growth(bundle: &FundamentalBundle) -> f64 {
    let series: Vec<metrics::PeriodValue> = annual_rows(&bundle.income, |s| s.meta)
        .iter()
        .filter_map(|s| {
            Some(metrics::PeriodValue {
                period_end: s.meta?.period_end,
                value: s.total_operating_revenue?,
            })
        })
        .collect();
    let yoys = metrics::yoy_growth(&series);
    let start = yoys.len().saturating_sub(5);
    let window = &yoys[start..];
    let avg = if window.is_empty() {
        0.0
    } else {
        window.iter().map(|(_, g)| g).sum::<f64>() / window.len() as f64
    };
    avg.clamp(DCF_GROWTH_FLOOR, DCF_GROWTH_CAP)
}

fn dcf_scenario_json(r: &valuation::DcfResult) -> Value {
    json!({
        "per_share": r2(r.per_share),
        "enterprise_value": r.enterprise_value,
        "equity_value": r.equity_value,
        "terminal_share": r4(r.terminal_share),
    })
}

/// Two-stage FCFF DCF scenario set. Null when base FCF or share count is
/// missing, or when the scenarios are incoherent (e.g. negative FCF).
fn dcf_json(bundle: &FundamentalBundle, growth: Option<f64>, wacc: Option<f64>) -> Value {
    let run = || {
        let cf_a = annual_rows(&bundle.cashflow, |s| s.meta);
        let cf = *cf_a.last()?;
        let base_fcf = metrics::fcf(cf.net_cfo, cf.capex)?;
        let shares = bundle
            .snapshot
            .as_ref()
            .and_then(|s| s.total_shares)
            .or_else(|| bundle.profile.as_ref().and_then(|p| p.total_shares))?;
        let net_debt = bundle
            .balance
            .last()
            .map(|bs| bs.interest_bearing_debt().unwrap_or(0.0) - bs.monetary_funds.unwrap_or(0.0))
            .unwrap_or(0.0);
        let inputs = valuation::DcfInputs {
            base_fcf,
            stage1_years: DCF_STAGE1_YEARS,
            stage1_growth: growth.unwrap_or_else(|| capped_base_growth(bundle)),
            terminal_growth: DCF_TERMINAL_GROWTH,
            wacc: wacc.unwrap_or(DCF_WACC),
            net_debt,
            shares,
        };
        let sc = valuation::scenarios(&inputs, DCF_SPREAD)?;
        let grid = valuation::sensitivity(&inputs, &DCF_SENSITIVITY_WACCS, &DCF_SENSITIVITY_GROWTHS);
        Some(json!({
            "assumptions": {
                "base_fcf": base_fcf,
                "stage1_years": inputs.stage1_years,
                "stage1_growth": inputs.stage1_growth,
                "terminal_growth": inputs.terminal_growth,
                "wacc": inputs.wacc,
                "net_debt": net_debt,
                "shares": shares,
            },
            "bear": dcf_scenario_json(&sc.bear),
            "base": dcf_scenario_json(&sc.base),
            "bull": dcf_scenario_json(&sc.bull),
            "sensitivity": {
                "wacc": DCF_SENSITIVITY_WACCS,
                "terminal_growth": DCF_SENSITIVITY_GROWTHS,
                "values": grid,
            },
            "caveat": DCF_CAVEAT,
        }))
    };
    run().unwrap_or(Value::Null)
}

/// Pure projection: bundle → the full `run_valuation` payload.
pub fn valuation_full_json(
    symbol: &Symbol,
    bundle: &FundamentalBundle,
    growth: Option<f64>,
    wacc: Option<f64>,
    failures: &[String],
) -> Value {
    let last_hist = bundle.valuation_history.last();
    let current = bundle.snapshot.as_ref().map(|s| {
        json!({
            "price": s.price,
            "pe_ttm": s.pe_ttm,
            "pe_static": s.pe_static,
            "pb": s.pb,
            "ps_ttm": last_hist.and_then(|h| h.ps_ttm),
            "pcf_ttm": last_hist.and_then(|h| h.pcf_ocf_ttm),
            "market_cap": s.total_market_cap,
        })
    });
    let percentile = percentile_json(bundle);
    let dcf = dcf_json(bundle, growth, wacc);

    let mut missing = failure_sections(failures);
    for (name, absent) in [
        ("current", current.is_none()),
        ("percentile", percentile.is_null()),
        ("dcf", dcf.is_null()),
    ] {
        if absent && !missing.iter().any(|m| m == name) {
            missing.push(name.to_string());
        }
    }
    json!({
        "symbol": symbol.code(),
        "current": current,
        "percentile": percentile,
        "dcf": dcf,
        "missing": missing,
    })
}

/// Compact LLM-facing summary of the full valuation payload.
fn valuation_summary(full: &Value) -> Value {
    let current = &full["current"];
    let cget = |k: &str| current.get(k).cloned().unwrap_or(Value::Null);
    let pct = &full["percentile"];
    let dcf = &full["dcf"];
    let dcf_range = if dcf.is_null() {
        Value::Null
    } else {
        json!({
            "bear": dcf["bear"]["per_share"],
            "base": dcf["base"]["per_share"],
            "bull": dcf["bull"]["per_share"],
        })
    };
    let assumptions = if dcf.is_null() {
        Value::Null
    } else {
        json!({
            "stage1_growth": dcf["assumptions"]["stage1_growth"],
            "wacc": dcf["assumptions"]["wacc"],
            "terminal_growth": dcf["assumptions"]["terminal_growth"],
        })
    };
    json!({
        "symbol": full["symbol"],
        "price": cget("price"),
        "pe_ttm": cget("pe_ttm"),
        "pb": cget("pb"),
        "ps_ttm": cget("ps_ttm"),
        "pe_ttm_pct": pct.get("pe_ttm_pct").cloned().unwrap_or(Value::Null),
        "pb_pct": pct.get("pb_pct").cloned().unwrap_or(Value::Null),
        "ps_pct": pct.get("ps_pct").cloned().unwrap_or(Value::Null),
        "percentile_days": pct.get("days").cloned().unwrap_or(Value::Null),
        "dcf_per_share": dcf_range,
        "dcf_assumptions": assumptions,
        "missing": full["missing"],
        "note": "分位为 0–100；DCF 单位为元/股；完整敏感性与方法标注见缓存",
    })
}

// ---------------------------------------------------------------------
// get_industry_chain
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct IndustryChainArgs {
    /// 6位证券代码
    symbol: String,
}

/// The company's position in the supply-chain graph.
pub struct GetIndustryChain;

#[async_trait]
impl AgentTool for GetIndustryChain {
    fn name(&self) -> &'static str {
        "get_industry_chain"
    }
    fn description(&self) -> &'static str {
        "查询公司在产业链图谱中的位置：上游供应商/原料、下游客户/产出、竞争对手、所属行业（每条边带来源与置信度）"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<IndustryChainArgs>()
    }
    fn cache_ttl_secs(&self) -> i64 {
        3600
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args: IndustryChainArgs = parse_args(self.name(), args)?;
        let symbol = parse_symbol(self.name(), &args.symbol)?;
        let graph = require_graph(ctx, self.name())?;
        let node = graph
            .find_node(symbol.code())
            .await
            .map_err(|e| tool_err(self.name(), e.to_string()))?
            .ok_or_else(|| {
                tool_err(
                    self.name(),
                    format!("图谱中未找到 {} 对应的节点（当前图谱覆盖有限）", symbol.code()),
                )
            })?;
        let neighbors = graph
            .neighbors(&node.id)
            .await
            .map_err(|e| tool_err(self.name(), e.to_string()))?;
        let payload = industry_chain_json(&node, &neighbors);
        Ok(ToolResult {
            summary_json: payload.clone(),
            full_json: Some(payload),
            cache_key: String::new(),
            source: "graph".to_string(),
            fetched_at: now_rfc3339(),
        })
    }
}

fn chain_entry(edge: &astock_graph::Edge, other: &Node) -> Value {
    json!({
        "id": other.id,
        "name": other.name,
        "code": other.code,
        "kind": other.kind.as_str(),
        "relation": edge.relation.as_str(),
        "weight": r4(edge.weight),
        "confidence": r4(edge.confidence),
        "source": edge.source_name,
    })
}

/// Pure projection: node + neighbors → the industry-chain payload.
pub fn industry_chain_json(node: &Node, neighbors: &[(astock_graph::Edge, Node)]) -> Value {
    let (mut upstream, mut downstream, mut competitors, mut industries, mut other_edges) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (edge, other) in neighbors {
        let entry = chain_entry(edge, other);
        let node_is_src = edge.src == node.id;
        match edge.relation {
            // src supplies dst / src is a customer of dst.
            Relation::Supplies => {
                if node_is_src { downstream.push(entry) } else { upstream.push(entry) }
            }
            Relation::CustomerOf => {
                if node_is_src { upstream.push(entry) } else { downstream.push(entry) }
            }
            // Consumed inputs are upstream; produced outputs are downstream.
            Relation::Consumes => {
                if node_is_src { upstream.push(entry) } else { downstream.push(entry) }
            }
            Relation::Produces => {
                if node_is_src { downstream.push(entry) } else { upstream.push(entry) }
            }
            Relation::Competes => competitors.push(entry),
            Relation::BelongsTo => {
                if node_is_src { industries.push(entry) } else { other_edges.push(entry) }
            }
            Relation::Substitutes | Relation::ExposedTo => other_edges.push(entry),
        }
    }
    let counts = json!({
        "upstream": upstream.len(),
        "downstream": downstream.len(),
        "competitors": competitors.len(),
        "industries": industries.len(),
    });
    json!({
        "node": {"id": node.id, "name": node.name, "code": node.code, "kind": node.kind.as_str()},
        "upstream": upstream,
        "downstream": downstream,
        "competitors": competitors,
        "industries": industries,
        "other_relations": other_edges,
        "counts": counts,
    })
}

// ---------------------------------------------------------------------
// run_supply_chain_shock
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct ShockArgs {
    /// 冲击主体：图谱节点 id、6位公司代码或节点名称（如 "铜"、"600362"、"江西铜业"）
    subject: String,
    /// 方向：up=上涨/利好，down=下跌/利空
    direction: String,
    /// 变动幅度（百分数，如 10 表示 10%）；可选，缺省时只做定性传导
    magnitude_pct: Option<f64>,
}

/// Event propagation through the supply-chain graph.
pub struct RunSupplyChainShock;

#[async_trait]
impl AgentTool for RunSupplyChainShock {
    fn name(&self) -> &'static str {
        "run_supply_chain_shock"
    }
    fn description(&self) -> &'static str {
        "事件传导分析：输入冲击主体（商品/公司/行业）、方向与幅度，输出一级受益/受损、二级与潜在映射公司清单，每条含完整逻辑链、预期滞后天数与置信度（启发式估计）"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<ShockArgs>()
    }
    fn cache_ttl_secs(&self) -> i64 {
        3600
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args: ShockArgs = parse_args(self.name(), args)?;
        let graph = require_graph(ctx, self.name())?;
        let direction: i8 = match args.direction.to_ascii_lowercase().as_str() {
            "up" | "涨" | "上涨" => 1,
            "down" | "跌" | "下跌" => -1,
            other => {
                return Err(invalid_args(
                    self.name(),
                    format!("direction 只能是 up/down，收到 `{other}`"),
                ))
            }
        };
        let word = if direction > 0 { "上涨" } else { "下跌" };
        let title = match args.magnitude_pct {
            Some(p) => format!("{}{}{}%", args.subject, word, r2(p.abs())),
            None => format!("{}{}", args.subject, word),
        };
        let event = Event::new(
            format!("shock-{}", now_secs()),
            "manual",
            title,
            args.subject,
            args.magnitude_pct.map(|p| p.abs() / 100.0),
            direction,
            now_secs(),
        );
        let engine = GraphEngine::new(graph.clone());
        let report = engine
            .propagate(&event)
            .await
            .map_err(|e| tool_err(self.name(), e.to_string()))?;
        let summary = shock_summary(&report);
        let full = impact_report_json(&report);
        Ok(ToolResult {
            summary_json: summary,
            full_json: Some(full),
            cache_key: String::new(),
            source: "graph".to_string(),
            fetched_at: now_rfc3339(),
        })
    }
}

/// Full JSON of one impacted company (all engine fields, provenance included).
fn impact_entry_json(e: &ImpactEntry) -> Value {
    json!({
        "node_id": e.node_id,
        "code": e.code,
        "name": e.name,
        "direction": e.direction.label(),
        "hop": e.hop,
        "logic_chain": e.logic_chain,
        "expected_lag_days": e.expected_lag_days,
        "magnitude_estimate_pct": e.magnitude_estimate.map(|m| r4(m * 100.0)),
        "confidence": r4(e.confidence),
        "provenance": e.provenance.iter().map(|(n, u)| json!({"source": n, "url": u})).collect::<Vec<_>>(),
    })
}

/// Full JSON of an [`ImpactReport`] — shared with the Tauri command layer.
pub fn impact_report_json(r: &ImpactReport) -> Value {
    let bucket = |v: &[ImpactEntry]| v.iter().map(impact_entry_json).collect::<Vec<_>>();
    json!({
        "event_title": r.event_title,
        "subject": {"id": r.subject.id, "name": r.subject.name, "kind": r.subject.kind.as_str()},
        "summary": r.summary,
        "primary_benefit": bucket(&r.primary_benefit),
        "primary_harm": bucket(&r.primary_harm),
        "secondary_benefit": bucket(&r.secondary_benefit),
        "secondary_harm": bucket(&r.secondary_harm),
        "potential": bucket(&r.potential),
        "disclaimer": r.disclaimer,
    })
}

/// Compact summary: every impacted company with its one-line logic chain.
fn shock_summary(r: &ImpactReport) -> Value {
    let compact = |e: &ImpactEntry| {
        json!({
            "code": e.code,
            "name": e.name,
            "direction": e.direction.label(),
            "hop": e.hop,
            "logic_chain": e.logic_chain,
        })
    };
    let all: Vec<Value> = r
        .primary_benefit
        .iter()
        .chain(&r.primary_harm)
        .chain(&r.secondary_benefit)
        .chain(&r.secondary_harm)
        .chain(&r.potential)
        .map(compact)
        .collect();
    json!({
        "event": r.event_title,
        "subject": {"id": r.subject.id, "name": r.subject.name},
        "counts": {
            "primary_benefit": r.primary_benefit.len(),
            "primary_harm": r.primary_harm.len(),
            "secondary_benefit": r.secondary_benefit.len(),
            "secondary_harm": r.secondary_harm.len(),
            "potential": r.potential.len(),
        },
        "impacted": all,
        "disclaimer": r.disclaimer,
        "note": "滞后/置信度/幅度等完整字段见缓存；均为启发式估计",
    })
}

// ---------------------------------------------------------------------
// build_relationship_graph
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct RelationshipArgs {
    /// 2-12 个 6 位证券代码
    symbols: Vec<String>,
    /// 回看交易日数，默认 250（60-500）
    window_days: Option<u32>,
}

/// Lead-lag scan window (trading days, both directions).
const REL_MAX_LAG: usize = 5;
/// Circular block size for the bootstrap p-value.
const REL_BOOT_BLOCK: usize = 10;
/// Bootstrap replicates (≥ 99 required by the engine).
const REL_BOOT_REPS: usize = 199;
/// Minimum aligned trading days for a meaningful estimate.
const REL_MIN_ALIGNED: usize = 60;
/// "correlation ≠ causation" note attached to every relationship payload.
const REL_NOTE: &str =
    "相关性不等于因果；小样本与行情风格(regime)切换会使相关结构不稳定，解读时请提示该风险";

/// Pairwise Pearson correlation + lead-lag graph over daily returns.
pub struct BuildRelationshipGraph;

#[async_trait]
impl AgentTool for BuildRelationshipGraph {
    fn name(&self) -> &'static str {
        "build_relationship_graph"
    }
    fn description(&self) -> &'static str {
        "构建股票关系网络：拉取日K收益率，计算两两 Pearson 相关与 lead-lag 最优滞后（含 bootstrap 显著性 p 值）；输出节点/边与相关矩阵（完整数据入缓存）"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<RelationshipArgs>()
    }
    fn cache_ttl_secs(&self) -> i64 {
        1800
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args: RelationshipArgs = parse_args(self.name(), args)?;
        if args.symbols.len() < 2 || args.symbols.len() > 12 {
            return Err(invalid_args(self.name(), "symbols 需包含 2-12 个代码"));
        }
        let window_days = args.window_days.unwrap_or(250).clamp(60, 500);
        let full = relationship_graph_json(&*ctx.market, &args.symbols, window_days).await?;
        let summary = json!({
            "edges": full["edges"],
            "aligned_bars": full["aligned_bars"],
            "period": full["period"],
            "errors": full["errors"],
            "note": REL_NOTE,
        });
        Ok(ToolResult {
            summary_json: summary,
            full_json: Some(full),
            cache_key: String::new(),
            source: "engine".to_string(),
            fetched_at: now_rfc3339(),
        })
    }
}

/// One pair edge: Pearson correlation plus the lead-lag scan result.
fn pair_edge_json(x: &[f64], y: &[f64], a: &str, b: &str) -> std::result::Result<Value, String> {
    let corr = astock_quant::correlation::pearson(x, y).map_err(|e| e.to_string())?;
    let scan = astock_quant::leadlag::cross_correlation_scan(x, y, REL_MAX_LAG)
        .map_err(|e| e.to_string())?;
    let p = astock_quant::leadlag::leadlag_bootstrap_pvalue(
        x,
        y,
        scan.best_lag,
        REL_BOOT_BLOCK,
        REL_BOOT_REPS,
        42,
    )
    .ok();
    let leader = match scan.best_lag.cmp(&0) {
        std::cmp::Ordering::Greater => Value::from(a),
        std::cmp::Ordering::Less => Value::from(b),
        std::cmp::Ordering::Equal => Value::Null,
    };
    Ok(json!({
        "pair": [a, b],
        "pearson": r4(corr),
        "best_lag": scan.best_lag,
        "lag_corr": r4(scan.best_value),
        "p_value": p.map(r4),
        "significant": p.is_some_and(|p| p < 0.05),
        "leader": leader,
    }))
}

/// Shared engine for the agent tool and the Tauri `relationship_graph`
/// command: fetch daily klines, align by common dates, compute pairwise
/// Pearson + lead-lag over returns. Returns the full payload.
pub async fn relationship_graph_json(
    market: &dyn DataProvider,
    symbols: &[String],
    window_days: u32,
) -> Result<Value> {
    let tool = "build_relationship_graph";
    let parsed: Vec<Symbol> = symbols
        .iter()
        .map(|s| parse_symbol(tool, s))
        .collect::<Result<_>>()?;

    let fetched: Vec<Result<(String, Vec<Bar>)>> = futures::stream::iter(parsed)
        .map(|sym| async move {
            let f = market
                .kline(&sym, KlinePeriod::Day, Adjust::Qfq, window_days)
                .await?;
            Ok((sym.code().to_string(), f.data))
        })
        .buffer_unordered(4)
        .collect()
        .await;
    let mut series: Vec<(String, Vec<Bar>)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for item in fetched {
        match item {
            Ok(pair) => series.push(pair),
            Err(e) => errors.push(e.to_string()),
        }
    }
    if series.len() < 2 {
        return Err(tool_err(
            tool,
            format!("可用K线序列不足 2 条：{}", errors.join("; ")),
        ));
    }

    // Align on common trading dates (inner join).
    let mut common: HashSet<NaiveDate> = series[0].1.iter().map(|b| b.date).collect();
    for (_, bars) in &series[1..] {
        let dates: HashSet<NaiveDate> = bars.iter().map(|b| b.date).collect();
        common.retain(|d| dates.contains(d));
    }
    if common.len() < REL_MIN_ALIGNED {
        return Err(tool_err(
            tool,
            format!("重叠交易日不足：仅 {} 天，至少需要 {}", common.len(), REL_MIN_ALIGNED),
        ));
    }
    let mut dates: Vec<NaiveDate> = common.into_iter().collect();
    dates.sort();
    let mut returns: Vec<Vec<f64>> = Vec::new();
    for (code, bars) in &series {
        let by_date: HashMap<NaiveDate, f64> = bars.iter().map(|b| (b.date, b.close)).collect();
        let closes: Vec<f64> = dates.iter().map(|d| by_date[d]).collect();
        let rets = astock_quant::returns::arithmetic_returns(&closes)
            .map_err(|e| tool_err(tool, format!("{code} 收益率计算失败：{e}")))?;
        returns.push(rets);
    }

    let labels: Vec<String> = series.iter().map(|(c, _)| c.clone()).collect();
    let n = labels.len();
    let mut matrix: Vec<Vec<f64>> = vec![vec![0.0; n]; n];
    for (i, row) in matrix.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    let mut edges = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            match pair_edge_json(&returns[i], &returns[j], &labels[i], &labels[j]) {
                Ok(edge) => {
                    matrix[i][j] = edge["pearson"].as_f64().unwrap_or(0.0);
                    matrix[j][i] = matrix[i][j];
                    edges.push(edge);
                }
                Err(e) => errors.push(format!("{}-{}: {e}", labels[i], labels[j])),
            }
        }
    }
    Ok(json!({
        "window_days": window_days,
        "aligned_bars": dates.len(),
        "period": {
            "start": dates.first().map(|d| d.to_string()),
            "end": dates.last().map(|d| d.to_string()),
        },
        "nodes": labels.iter().map(|c| json!({"symbol": c})).collect::<Vec<_>>(),
        "edges": edges,
        "matrix": {"labels": labels, "pearson": matrix},
        "method": "日频收益率(前复权收盘)两两 Pearson 相关 + 交叉相关扫描最优滞后(±5日) + 循环块 bootstrap p 值(199次, 固定种子)",
        "note": REL_NOTE,
        "errors": errors,
    }))
}

// ---------------------------------------------------------------------
// run_backtest
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct BacktestArgs {
    /// 6位证券代码
    symbol: String,
    /// 策略：ma_cross（双均线，默认）/ turtle（海龟突破）/ buy_hold（买入持有基准）
    strategy: Option<String>,
    /// ma_cross 快线窗口（交易日），默认 5
    fast: Option<u32>,
    /// ma_cross 慢线窗口（交易日），默认 20
    slow: Option<u32>,
    /// turtle 入场通道长度，默认 20
    entry_n: Option<u32>,
    /// turtle 出场通道长度，默认 10
    exit_n: Option<u32>,
    /// 回测日K根数，默认 750（60-2000）
    bars: Option<u32>,
}

/// Daily-bar backtest with A-share trading rules (T+1, lots, limits, fees).
pub struct RunBacktest;

#[async_trait]
impl AgentTool for RunBacktest {
    fn name(&self) -> &'static str {
        "run_backtest"
    }
    fn description(&self) -> &'static str {
        "策略回测：双均线(ma_cross)/海龟突破(turtle)/买入持有(buy_hold)，含 T+1、整手、涨跌停与费用约束；输出 CAGR/Sharpe/最大回撤/胜率/交易次数（交易明细入缓存）"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<BacktestArgs>()
    }
    fn cache_ttl_secs(&self) -> i64 {
        1800
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let args: BacktestArgs = parse_args(self.name(), args)?;
        let bars = args.bars.unwrap_or(750).clamp(60, 2000);
        let full = run_backtest_json(
            &*ctx.market,
            &args.symbol,
            args.strategy.as_deref(),
            args.fast,
            args.slow,
            args.entry_n,
            args.exit_n,
            bars,
        )
        .await?;
        let summary = json!({
            "symbol": full["symbol"],
            "strategy": full["strategy"],
            "params": full["params"],
            "period": full["data"],
            "total_return": full["total_return"],
            "cagr": full["cagr"],
            "sharpe": full["sharpe"],
            "max_drawdown": full["max_drawdown"],
            "hit_rate": full["hit_rate"],
            "round_trips": full["round_trips"],
            "trades_count": full["trades_count"],
            "note": full["note"],
        });
        Ok(ToolResult {
            summary_json: summary,
            full_json: Some(full),
            cache_key: String::new(),
            source: "engine".to_string(),
            fetched_at: now_rfc3339(),
        })
    }
}

/// Initial cash for every agent-driven backtest.
const BACKTEST_INITIAL_CASH: f64 = 1_000_000.0;
/// Minimum bars for a meaningful backtest.
const BACKTEST_MIN_BARS: usize = 60;
/// Trade rows kept in the full payload (newest kept, chronological order).
const BACKTEST_TRADES_TAIL: usize = 50;

/// Shared engine for the agent tool and the Tauri `run_backtest` command.
/// Returns the full payload (equity curve + last-50 trade detail included).
#[allow(clippy::too_many_arguments)]
pub async fn run_backtest_json(
    market: &dyn DataProvider,
    symbol_raw: &str,
    strategy: Option<&str>,
    fast: Option<u32>,
    slow: Option<u32>,
    entry_n: Option<u32>,
    exit_n: Option<u32>,
    bars: u32,
) -> Result<Value> {
    let tool = "run_backtest";
    let symbol = parse_symbol(tool, symbol_raw)?;
    let fetched = market
        .kline(&symbol, KlinePeriod::Day, Adjust::Qfq, bars)
        .await?;
    if fetched.data.len() < BACKTEST_MIN_BARS {
        return Err(tool_err(
            tool,
            format!("k线数据不足：仅{}根，至少需要{}根", fetched.data.len(), BACKTEST_MIN_BARS),
        ));
    }
    let series = PriceSeries::new(
        symbol.code(),
        fetched
            .data
            .iter()
            .map(astock_backtest::data::Bar::from)
            .collect::<Vec<_>>(),
    )
    .map_err(|e| tool_err(tool, e.to_string()))?;

    let name = strategy.unwrap_or("ma_cross").to_ascii_lowercase();
    let mut strat: Box<dyn Strategy> = match name.as_str() {
        "ma_cross" | "ma" => {
            let f = fast.unwrap_or(5) as usize;
            let s = slow.unwrap_or(20) as usize;
            if f == 0 || f >= s {
                return Err(invalid_args(tool, format!("ma_cross 需要 1 <= fast({f}) < slow({s})")));
            }
            Box::new(MaCross::new(f, s))
        }
        "turtle" | "turtle_breakout" => {
            let e = entry_n.unwrap_or(20) as usize;
            let x = exit_n.unwrap_or(10) as usize;
            if e < 2 || x < 1 {
                return Err(invalid_args(tool, format!("turtle 需要 entry_n({e}) >= 2 且 exit_n({x}) >= 1")));
            }
            Box::new(TurtleBreakout::new(e, x))
        }
        "buy_hold" | "buyhold" => Box::new(BuyHold),
        other => {
            return Err(invalid_args(
                tool,
                format!("未知策略 `{other}`：可选 ma_cross / turtle / buy_hold"),
            ))
        }
    };
    let params = match name.as_str() {
        "ma_cross" | "ma" => json!({"fast": fast.unwrap_or(5), "slow": slow.unwrap_or(20)}),
        "turtle" | "turtle_breakout" => {
            json!({"entry_n": entry_n.unwrap_or(20), "exit_n": exit_n.unwrap_or(10)})
        }
        _ => json!({}),
    };

    let rules = RuleSet::load(None).map_err(|e| tool_err(tool, format!("加载交易规则失败：{e}")))?;
    let engine = BacktestEngine::new(rules, BtConfig::new(symbol.code(), BACKTEST_INITIAL_CASH))
        .map_err(|e| tool_err(tool, e.to_string()))?;
    let result = engine
        .run(&series, strat.as_mut())
        .map_err(|e| tool_err(tool, e.to_string()))?;
    let report = result
        .performance_report(None, &MetricsConfig::default())
        .ok_or_else(|| tool_err(tool, "回测区间过短，无法生成绩效报告"))?;

    let tail_start = result.trades.len().saturating_sub(BACKTEST_TRADES_TAIL);
    let trades: Vec<Value> = result.trades[tail_start..]
        .iter()
        .map(|f| {
            json!({
                "date": f.date.to_string(),
                "side": match f.side { TradeSide::Buy => "buy", TradeSide::Sell => "sell" },
                "shares": f.shares,
                "price": r2(f.price),
                "amount": r2(f.amount),
                "fees": r2(f.fees.total),
                "reason": f.reason,
            })
        })
        .collect();
    let equity_curve: Vec<Value> = result
        .equity
        .iter()
        .map(|p| json!([p.date.to_string(), r2(p.equity)]))
        .collect();

    Ok(json!({
        "symbol": symbol.code(),
        "strategy": strat.name(),
        "params": params,
        "data": {
            "start": report.start.to_string(),
            "end": report.end.to_string(),
            "bars": series.len(),
        },
        "initial_cash": BACKTEST_INITIAL_CASH,
        "final_equity": r2(result.final_equity()),
        "total_return": r4(report.total_return),
        "cagr": r4(report.cagr),
        "annualized_volatility": r4(report.annualized_volatility),
        "sharpe": r4(report.sharpe),
        "sortino": r4(report.sortino),
        "calmar": r4(report.calmar),
        "max_drawdown": r4(report.max_drawdown),
        "max_drawdown_duration_bars": report.max_drawdown_duration_bars,
        "round_trips": report.round_trips,
        "hit_rate": r4(report.hit_rate),
        "payoff_ratio": r4(report.payoff_ratio),
        "profit_factor": r4(report.profit_factor),
        "trades_count": result.trades.len(),
        "rejections": result.rejections.len(),
        "fees_total": r2(result.total_fees()),
        "equity_curve": equity_curve,
        "trades_tail": trades,
        "note": "单组参数的历史回测不代表未来收益；未做参数网格与过拟合检验",
    }))
}

// ---------------------------------------------------------------------
// get_market_regime
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct RegimeArgs {}

/// Market-regime classification: breadth + index trend + up-day share.
pub struct GetMarketRegime;

#[async_trait]
impl AgentTool for GetMarketRegime {
    fn name(&self) -> &'static str {
        "get_market_regime"
    }
    fn description(&self) -> &'static str {
        "判断市场状态（进攻/中性/防守）：综合市场宽度（涨跌家数比）、上证指数相对 MA20/MA60 的位置与近20日上涨占比，全部附支撑数据"
    }
    fn parameters_schema(&self) -> Value {
        schema_value::<RegimeArgs>()
    }
    fn cache_ttl_secs(&self) -> i64 {
        300
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let _args: RegimeArgs = parse_args(self.name(), args)?;
        let (payload, source, fetched_at) = market_regime_json(&*ctx.market).await?;
        Ok(ToolResult {
            summary_json: payload.clone(),
            full_json: Some(payload),
            cache_key: String::new(),
            source,
            fetched_at,
        })
    }
}

/// 上证指数 EastMoney secid.
const SH_INDEX_SECID: &str = "1.000001";

/// Shared engine for the agent tool and the Tauri `get_market_regime`
/// command. Returns `(payload, source, fetched_at)`.
pub async fn market_regime_json(market: &dyn DataProvider) -> Result<(Value, String, String)> {
    let tool = "get_market_regime";
    let breadth = market.market_breadth().await.ok().map(|f| f.data);
    let index = market.index_kline(SH_INDEX_SECID, 120).await?;
    if index.data.len() < 61 {
        return Err(tool_err(
            tool,
            format!("指数K线数据不足：仅{}根，至少需要 61 根", index.data.len()),
        ));
    }
    let (source, fetched_at) = (index.source.to_string(), index.fetched_at.to_rfc3339());
    let closes: Vec<f64> = index.data.iter().map(|b| b.close).collect();
    let last = *closes.last().unwrap();
    let ma20 = tech::indicators::sma_series(&closes, 20).last().copied().flatten();
    let ma60 = tech::indicators::sma_series(&closes, 60).last().copied().flatten();
    let window = &closes[closes.len() - 21..];
    let up_days = window.windows(2).filter(|w| w[1] > w[0]).count();
    let up_ratio_20 = up_days as f64 / 20.0;

    // Deterministic scoring: each of the four signals votes ±1.
    let mut score = 0i32;
    let above_ma20 = ma20.map(|m| last > m);
    let ma20_above_ma60 = ma20.zip(ma60).map(|(a, b)| a > b);
    if let Some(v) = above_ma20 {
        score += if v { 1 } else { -1 };
    }
    if let Some(v) = ma20_above_ma60 {
        score += if v { 1 } else { -1 };
    }
    let breadth_bullish = breadth.as_ref().map(|b| b.ratio() >= 0.5);
    if let Some(v) = breadth_bullish {
        score += if v { 1 } else { -1 };
    }
    score += if up_ratio_20 >= 0.5 { 1 } else { -1 };
    let regime = if score >= 2 {
        "进攻"
    } else if score <= -2 {
        "防守"
    } else {
        "中性"
    };

    let payload = json!({
        "regime": regime,
        "score": score,
        "scoring": "四项信号各 ±1 票：收盘>MA20、MA20>MA60、涨跌家数比≥0.5、近20日上涨占比≥0.5；总分 ≥2 进攻，≤-2 防守，其余中性",
        "index": {"secid": SH_INDEX_SECID, "close": r2(last), "as_of": index.data.last().map(|b| b.date.to_string())},
        "trend": {
            "ma20": ma20.map(r2),
            "ma60": ma60.map(r2),
            "above_ma20": above_ma20,
            "ma20_above_ma60": ma20_above_ma60,
            "dist_ma20_pct": ma20.map(|m| r2((last - m) / m * 100.0)),
            "dist_ma60_pct": ma60.map(|m| r2((last - m) / m * 100.0)),
        },
        "breadth": breadth.as_ref().map(|b| json!({
            "up": b.up, "down": b.down, "flat": b.flat, "total": b.total, "ratio": r4(b.ratio()),
        })),
        "up_days_20": up_days,
        "up_ratio_20": r4(up_ratio_20),
    });
    Ok((payload, source, fetched_at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use astock_core::{DataError, Fetched, MarketBreadth, Quote, Source, VolumeUnit};
    use astock_fundamental::model::{CompanyProfile, KeyIndicators, ValuationSnapshot};
    use astock_graph::{Edge, NodeKind};
    use astock_market_data::DataProvider;
    use astock_storage::{Storage, StorageConfig};
    use serde_json::json;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn meta(y: i32, m: u32, day: u32, rt: ReportType) -> Option<PeriodMeta> {
        Some(PeriodMeta {
            period_end: d(y, m, day),
            report_type: rt,
            announced: Some(d(y, m, day) + chrono::Duration::days(45)),
        })
    }

    fn income(y: i32, m: u32, day: u32, rt: ReportType, rev: f64, cost: f64, np: f64) -> IncomeStatement {
        IncomeStatement {
            meta: meta(y, m, day, rt),
            total_operating_revenue: Some(rev),
            operating_revenue: Some(rev),
            operating_cost: Some(cost),
            operating_profit: Some(np * 1.1),
            total_profit: Some(np * 1.2),
            income_tax: Some(np * 0.2),
            net_profit: Some(np),
            net_profit_parent: Some(np * 0.9),
            finance_expense: Some(10.0),
            ..Default::default()
        }
    }

    fn balance(y: i32, m: u32, day: u32, rt: ReportType, scale: f64) -> BalanceSheet {
        BalanceSheet {
            meta: meta(y, m, day, rt),
            monetary_funds: Some(300.0 * scale),
            total_current_assets: Some(800.0 * scale),
            total_current_liabilities: Some(400.0 * scale),
            total_assets: Some(2000.0 * scale),
            long_term_debt: Some(100.0 * scale),
            total_liabilities: Some(800.0 * scale),
            share_capital: Some(100.0),
            retained_earnings: Some(500.0 * scale),
            total_parent_equity: Some(1100.0 * scale),
            total_equity: Some(1200.0 * scale),
            ..Default::default()
        }
    }

    fn cashflow(y: i32, m: u32, day: u32, rt: ReportType, cfo: f64, capex: f64) -> CashFlowStatement {
        CashFlowStatement {
            meta: meta(y, m, day, rt),
            cash_from_sales: Some(cfo * 6.0),
            net_cfo: Some(cfo),
            capex: Some(capex),
            ..Default::default()
        }
    }

    /// Two annual years plus one H1 — enough for every section to compute.
    fn sample_bundle() -> FundamentalBundle {
        FundamentalBundle {
            profile: Some(CompanyProfile {
                code: "600519".into(),
                name: "贵州茅台酒股份有限公司".into(),
                short_name: "贵州茅台".into(),
                industry: Some("酿酒行业".into()),
                listing_date: Some(d(2001, 8, 27)),
                total_shares: Some(100.0),
                float_shares: Some(100.0),
                ..Default::default()
            }),
            income: vec![
                income(2023, 12, 31, ReportType::Annual, 1000.0, 600.0, 100.0),
                income(2024, 12, 31, ReportType::Annual, 1200.0, 700.0, 130.0),
                income(2025, 6, 30, ReportType::H1, 700.0, 400.0, 80.0),
            ],
            balance: vec![
                balance(2023, 12, 31, ReportType::Annual, 1.0),
                balance(2024, 12, 31, ReportType::Annual, 1.1),
                balance(2025, 6, 30, ReportType::H1, 1.2),
            ],
            cashflow: vec![
                cashflow(2023, 12, 31, ReportType::Annual, 150.0, 50.0),
                cashflow(2024, 12, 31, ReportType::Annual, 180.0, 60.0),
                cashflow(2025, 6, 30, ReportType::H1, 90.0, 25.0),
            ],
            indicators: vec![KeyIndicators {
                meta: meta(2024, 12, 31, ReportType::Annual),
                roe_weighted: Some(9.5),
                debt_ratio: Some(40.0),
                ..Default::default()
            }],
            snapshot: Some(ValuationSnapshot {
                price: 100.0,
                name: "贵州茅台".into(),
                pe_ttm: Some(20.0),
                pe_static: Some(22.0),
                pb: Some(3.0),
                total_shares: Some(100.0),
                total_market_cap: Some(10_000.0),
                ..Default::default()
            }),
            valuation_history: (0..10)
                .map(|i| ValuationPoint {
                    date: d(2025, 1, 2) + chrono::Duration::days(i),
                    pe_ttm: Some(18.0 + i as f64),
                    pb_mrq: Some(2.8),
                    ps_ttm: Some(5.0),
                    pcf_ocf_ttm: Some(15.0),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn fundamentals_full_and_summary() {
        let sym = Symbol::new("600519").unwrap();
        let full = fundamentals_full_json(&sym, &sample_bundle(), &[]);
        assert_eq!(full["missing"].as_array().unwrap().len(), 0, "{full}");
        assert_eq!(full["profile"]["name"], json!("贵州茅台"));
        assert_eq!(full["latest_period"]["period_end"], json!("2025-06-30"));
        // 收现比 = 540 / 700.
        assert!((full["metrics"]["cash_ratio"].as_f64().unwrap() - 540.0 / 700.0).abs() < 1e-9);
        assert_eq!(full["metrics"]["fcf"], json!(65.0));
        assert!(full["metrics"]["roe"].is_number());
        assert!(full["metrics"]["debt_ratio"].is_number());
        // No 2024 H1 row → latest-period YoY is Missing, never fabricated.
        assert!(full["metrics"]["revenue_yoy"].is_null());
        assert_eq!(full["scores"]["piotroski"]["available"].as_u64().unwrap(), 9);
        assert!(full["scores"]["altman"]["z_emerging"].is_number());

        let summary = fundamentals_summary(&full);
        assert_eq!(summary["metrics"]["fcf_yi"], json!(r2(65.0 / 1e8)));
        assert!(summary["scores"]["piotroski"].as_str().unwrap().contains('/'));
        assert_eq!(summary["anomalies"]["count"], json!(0));
        assert!(summary["anomalies"]["max_severity"].is_null());
        // Bulky sections stay out of the summary.
        assert!(summary.get("growth_series").is_none());
        assert!(summary.get("dividends").is_none());
    }

    #[test]
    fn fundamentals_empty_bundle_degrades() {
        let sym = Symbol::new("600519").unwrap();
        let full = fundamentals_full_json(&sym, &FundamentalBundle::default(), &["income: timeout".to_string()]);
        let missing: Vec<&str> = full["missing"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        for section in ["income", "profile", "metrics", "growth_series", "scores"] {
            assert!(missing.contains(&section), "missing {section}");
        }
        let summary = fundamentals_summary(&full);
        assert!(summary["metrics"]["roe"].is_null());
        assert!(summary["scores"]["piotroski"].is_null());
    }

    #[test]
    fn valuation_full_and_overrides() {
        let sym = Symbol::new("600519").unwrap();
        let full = valuation_full_json(&sym, &sample_bundle(), None, None, &[]);
        assert_eq!(full["current"]["pe_ttm"], json!(20.0));
        assert_eq!(full["current"]["ps_ttm"], json!(5.0));
        // PE history 18..=27, current 20 → 3 of 10 ≤ 20 → 30%.
        assert!((full["percentile"]["pe_ttm_pct"].as_f64().unwrap() - 30.0).abs() < 1e-9);
        assert!(full["percentile"]["method"].as_str().unwrap().contains("历史分位"));
        let dcf = &full["dcf"];
        assert_eq!(dcf["assumptions"]["base_fcf"], json!(120.0)); // 180 − 60
        assert!((dcf["assumptions"]["stage1_growth"].as_f64().unwrap() - 0.2).abs() < 1e-9);
        let bear = dcf["bear"]["per_share"].as_f64().unwrap();
        let base = dcf["base"]["per_share"].as_f64().unwrap();
        let bull = dcf["bull"]["per_share"].as_f64().unwrap();
        assert!(bear < base && base < bull);

        // growth / wacc overrides land in the assumptions.
        let custom = valuation_full_json(&sym, &sample_bundle(), Some(0.05), Some(0.10), &[]);
        assert_eq!(custom["dcf"]["assumptions"]["stage1_growth"], json!(0.05));
        assert_eq!(custom["dcf"]["assumptions"]["wacc"], json!(0.10));

        let summary = valuation_summary(&full);
        assert_eq!(summary["dcf_per_share"]["base"], json!(r2(base)));
        assert_eq!(summary["pe_ttm_pct"], json!(30.0));
        // Sensitivity grid stays in the full payload only.
        assert!(summary.get("dcf").is_none() || summary["dcf"].get("sensitivity").is_none());
    }

    // -----------------------------------------------------------------
    // graph-backed tools
    // -----------------------------------------------------------------

    fn company(code: &str, name: &str) -> Node {
        Node {
            id: format!("company:{code}"),
            kind: NodeKind::Company,
            name: name.into(),
            code: Some(code.into()),
            meta: json!({}),
        }
    }

    fn plain_node(id: &str, kind: NodeKind, name: &str) -> Node {
        Node {
            id: id.into(),
            kind,
            name: name.into(),
            code: None,
            meta: json!({}),
        }
    }

    fn edge(src: &str, dst: &str, relation: Relation) -> Edge {
        Edge {
            id: None,
            src: src.into(),
            dst: dst.into(),
            relation,
            weight: 0.8,
            source_name: "公司年报2024".into(),
            source_url: "https://example.com".into(),
            confidence: 0.85,
            valid_from: 0,
            valid_to: None,
        }
    }

    /// Tiny graph: 铜 chain + a liquor industry with two competitors.
    async fn seeded_graph() -> (tempfile::TempDir, Storage, GraphStore) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let store = GraphStore::new(storage.clone());
        store.upsert_node(&plain_node("commodity:cu", NodeKind::Commodity, "铜")).await.unwrap();
        store.upsert_node(&plain_node("commodity:sorghum", NodeKind::Commodity, "高粱")).await.unwrap();
        store.upsert_node(&plain_node("product:cable", NodeKind::Product, "电线电缆")).await.unwrap();
        store.upsert_node(&plain_node("industry:liquor", NodeKind::Industry, "白酒")).await.unwrap();
        store.upsert_node(&company("600362", "江西铜业")).await.unwrap();
        store.upsert_node(&company("600869", "远东股份")).await.unwrap();
        store.upsert_node(&company("000651", "格力电器")).await.unwrap();
        store.upsert_node(&company("600519", "贵州茅台")).await.unwrap();
        store.upsert_node(&company("000858", "五粮液")).await.unwrap();
        store.upsert_node(&company("600859", "经销商")).await.unwrap();
        store.upsert_edge(&edge("company:600362", "commodity:cu", Relation::Produces)).await.unwrap();
        store.upsert_edge(&edge("company:600869", "commodity:cu", Relation::Consumes)).await.unwrap();
        store.upsert_edge(&edge("company:600869", "product:cable", Relation::Produces)).await.unwrap();
        store.upsert_edge(&edge("company:000651", "product:cable", Relation::Consumes)).await.unwrap();
        store.upsert_edge(&edge("company:600519", "industry:liquor", Relation::BelongsTo)).await.unwrap();
        store.upsert_edge(&edge("company:000858", "industry:liquor", Relation::BelongsTo)).await.unwrap();
        store.upsert_edge(&edge("company:600519", "company:000858", Relation::Competes)).await.unwrap();
        store.upsert_edge(&edge("company:600519", "commodity:sorghum", Relation::Consumes)).await.unwrap();
        store.upsert_edge(&edge("company:600519", "company:600859", Relation::Supplies)).await.unwrap();
        (dir, storage, store)
    }

    /// Deterministic per-symbol bars: shared market factor + idiosyncratic
    /// component + mild upward drift (so buy-and-hold gains).
    fn series_bars(code: &str, n: usize) -> Vec<Bar> {
        let seed: f64 = code.bytes().map(f64::from).sum::<f64>() / 97.0;
        let start = d(2025, 1, 2);
        (0..n)
            .map(|i| {
                let t = i as f64;
                let close = 10.0 + (t * 0.11).sin() * 0.8 + (t * 0.37 + seed).cos() * 0.3 + t * 0.02;
                Bar {
                    date: start + chrono::Duration::days(i as i64),
                    open: close - 0.05,
                    close,
                    high: close + 0.1,
                    low: close - 0.1,
                    volume: 10_000.0,
                    volume_unit: VolumeUnit::Lots,
                    amount: Some(close * 10_000.0),
                    turnover: Some(1.0),
                    pct: Some(0.2),
                }
            })
            .collect()
    }

    /// Strictly rising bars (index for the regime test).
    fn trend_bars(n: usize) -> Vec<Bar> {
        let start = d(2025, 1, 2);
        (0..n)
            .map(|i| {
                let close = 3000.0 + i as f64 * 5.0;
                Bar {
                    date: start + chrono::Duration::days(i as i64),
                    open: close - 2.0,
                    close,
                    high: close + 2.0,
                    low: close - 3.0,
                    volume: 1e6,
                    volume_unit: VolumeUnit::Lots,
                    amount: None,
                    turnover: None,
                    pct: Some(0.15),
                }
            })
            .collect()
    }

    struct DeepMarket;

    #[async_trait]
    impl DataProvider for DeepMarket {
        fn name(&self) -> &'static str {
            "deep-mock"
        }
        async fn kline(
            &self,
            symbol: &Symbol,
            _period: KlinePeriod,
            _adjust: Adjust,
            count: u32,
        ) -> std::result::Result<Fetched<Vec<Bar>>, DataError> {
            Ok(Fetched::now(series_bars(symbol.code(), count as usize), Source::EastMoney))
        }
        async fn quote(&self, _symbol: &Symbol) -> std::result::Result<Fetched<Quote>, DataError> {
            Err(DataError::NoProvider("quote"))
        }
        async fn market_breadth(&self) -> std::result::Result<Fetched<MarketBreadth>, DataError> {
            Ok(Fetched::now(
                MarketBreadth {
                    up: 3000,
                    down: 2000,
                    flat: 100,
                    total: 5100,
                },
                Source::EastMoney,
            ))
        }
        async fn index_kline(
            &self,
            _index_secid: &str,
            _count: u32,
        ) -> std::result::Result<Fetched<Vec<Bar>>, DataError> {
            Ok(Fetched::now(trend_bars(120), Source::EastMoney))
        }
    }

    fn deep_ctx(storage: Storage, graph: Option<GraphStore>) -> ToolContext {
        ToolContext {
            market: Arc::new(DeepMarket),
            storage,
            graph,
            fundamental: None,
        }
    }

    #[tokio::test]
    async fn industry_chain_buckets_neighbors() {
        let (_dir, storage, store) = seeded_graph().await;
        let ctx = deep_ctx(storage, Some(store));
        let registry = crate::default_registry();
        let r = registry
            .dispatch("get_industry_chain", json!({"symbol": "600519"}), &ctx)
            .await
            .unwrap();
        let s = &r.summary_json;
        assert_eq!(s["node"]["name"], json!("贵州茅台"));
        let industries = s["industries"].as_array().unwrap();
        assert_eq!(industries[0]["name"], json!("白酒"));
        let competitors = s["competitors"].as_array().unwrap();
        assert_eq!(competitors[0]["name"], json!("五粮液"));
        assert_eq!(competitors[0]["confidence"], json!(0.85));
        assert_eq!(competitors[0]["source"], json!("公司年报2024"));
        let upstream = s["upstream"].as_array().unwrap();
        assert_eq!(upstream[0]["name"], json!("高粱"));
        let downstream = s["downstream"].as_array().unwrap();
        assert_eq!(downstream[0]["name"], json!("经销商"));

        // Unknown symbol → clean tool error, not a panic.
        let missing = registry
            .dispatch("get_industry_chain", json!({"symbol": "300750"}), &ctx)
            .await;
        assert!(missing.is_err());
    }

    #[tokio::test]
    async fn supply_chain_shock_propagates_with_chains() {
        let (_dir, storage, store) = seeded_graph().await;
        let ctx = deep_ctx(storage, Some(store));
        let registry = crate::default_registry();
        let r = registry
            .dispatch(
                "run_supply_chain_shock",
                json!({"subject": "铜", "direction": "up", "magnitude_pct": 10}),
                &ctx,
            )
            .await
            .unwrap();
        let s = &r.summary_json;
        assert_eq!(s["counts"]["primary_benefit"], json!(1));
        assert_eq!(s["counts"]["primary_harm"], json!(1));
        let impacted = s["impacted"].as_array().unwrap();
        let jxt = impacted.iter().find(|e| e["code"] == json!("600362")).unwrap();
        assert_eq!(jxt["direction"], json!("受益"));
        assert_eq!(jxt["hop"], json!(1));
        assert!(jxt["logic_chain"].as_str().unwrap().contains("铜↑10%"));
        let yd = impacted.iter().find(|e| e["code"] == json!("600869")).unwrap();
        assert_eq!(yd["direction"], json!("受损"));
        // Full report carries lag / confidence / magnitude.
        let full = r.full_json.unwrap();
        let pb = full["primary_benefit"].as_array().unwrap();
        assert!(pb[0]["expected_lag_days"].is_number());
        assert!(pb[0]["confidence"].is_number());
        assert!(pb[0]["magnitude_estimate_pct"].is_number());
        assert!(!full["disclaimer"].as_str().unwrap().is_empty());

        // Bad direction → typed arg error.
        let bad = registry
            .dispatch(
                "run_supply_chain_shock",
                json!({"subject": "铜", "direction": "sideways"}),
                &ctx,
            )
            .await;
        assert!(matches!(bad, Err(AgentError::InvalidArgs { .. })));
    }

    #[tokio::test]
    async fn graph_and_fundamental_tools_fail_cleanly_when_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let ctx = deep_ctx(storage, None);
        let registry = crate::default_registry();
        for (tool, args) in [
            ("get_industry_chain", json!({"symbol": "600519"})),
            ("run_supply_chain_shock", json!({"subject": "铜", "direction": "up"})),
            ("get_fundamentals", json!({"symbol": "600519"})),
            ("run_valuation", json!({"symbol": "600519"})),
        ] {
            let err = registry.dispatch(tool, args, &ctx).await.unwrap_err();
            assert!(err.to_string().contains("不可用"), "{tool}: {err}");
        }
    }

    #[tokio::test]
    async fn relationship_graph_builds_edges_and_matrix() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let ctx = deep_ctx(storage, None);
        let registry = crate::default_registry();
        let r = registry
            .dispatch(
                "build_relationship_graph",
                json!({"symbols": ["600519", "000001", "600362"], "window_days": 250}),
                &ctx,
            )
            .await
            .unwrap();
        let s = &r.summary_json;
        assert_eq!(s["edges"].as_array().unwrap().len(), 3);
        assert_eq!(s["aligned_bars"], json!(250));
        assert!(s["note"].as_str().unwrap().contains("因果"));
        let edge = &s["edges"][0];
        assert!(edge["pearson"].is_number());
        assert!(edge["best_lag"].is_number());
        assert!(edge["significant"].is_boolean());

        let full = r.full_json.unwrap();
        let matrix = full["matrix"]["pearson"].as_array().unwrap();
        assert_eq!(matrix.len(), 3);
        assert_eq!(matrix[0][0], json!(1.0));
        // Matrix symmetric.
        assert_eq!(matrix[0][1], matrix[1][0]);

        // One symbol → typed arg error.
        let bad = registry
            .dispatch("build_relationship_graph", json!({"symbols": ["600519"]}), &ctx)
            .await;
        assert!(matches!(bad, Err(AgentError::InvalidArgs { .. })));
    }

    #[tokio::test]
    async fn backtest_buy_hold_and_ma_cross() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let ctx = deep_ctx(storage, None);
        let registry = crate::default_registry();

        let r = registry
            .dispatch(
                "run_backtest",
                json!({"symbol": "600519", "strategy": "buy_hold", "bars": 300}),
                &ctx,
            )
            .await
            .unwrap();
        let s = &r.summary_json;
        assert_eq!(s["strategy"], json!("buy_hold"));
        assert!(s["cagr"].is_number());
        assert!(s["sharpe"].is_number());
        assert!(s["max_drawdown"].is_number());
        assert_eq!(s["period"]["bars"], json!(300));
        // Mild uptrend → buy & hold gains.
        assert!(s["total_return"].as_f64().unwrap() > 0.0);
        let full = r.full_json.unwrap();
        assert_eq!(full["trades_count"], json!(1));
        assert!(full["trades_tail"].as_array().unwrap().len() <= 50);
        assert!(!full["equity_curve"].as_array().unwrap().is_empty());

        let ma = registry
            .dispatch(
                "run_backtest",
                json!({"symbol": "600519", "strategy": "ma_cross", "fast": 5, "slow": 20, "bars": 300}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(ma.summary_json["params"]["fast"], json!(5));

        // fast >= slow → typed arg error (never a panic from the engine).
        let bad = registry
            .dispatch(
                "run_backtest",
                json!({"symbol": "600519", "strategy": "ma_cross", "fast": 20, "slow": 5}),
                &ctx,
            )
            .await;
        assert!(matches!(bad, Err(AgentError::InvalidArgs { .. })));
        let unknown = registry
            .dispatch("run_backtest", json!({"symbol": "600519", "strategy": "magic"}), &ctx)
            .await;
        assert!(matches!(unknown, Err(AgentError::InvalidArgs { .. })));
    }

    #[tokio::test]
    async fn market_regime_scores_uptrend_as_offensive() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let ctx = deep_ctx(storage, None);
        let registry = crate::default_registry();
        let r = registry
            .dispatch("get_market_regime", json!({}), &ctx)
            .await
            .unwrap();
        let s = &r.summary_json;
        // Strictly rising index + bullish breadth → all four votes positive.
        assert_eq!(s["regime"], json!("进攻"));
        assert_eq!(s["score"], json!(4));
        assert_eq!(s["trend"]["above_ma20"], json!(true));
        assert_eq!(s["trend"]["ma20_above_ma60"], json!(true));
        assert!(s["trend"]["ma20"].is_number());
        assert_eq!(s["breadth"]["up"], json!(3000));
        assert_eq!(s["up_days_20"], json!(20));
        assert!(!r.source.is_empty());
    }
}
