//! Evidence-bound earnings driver trees and shock-to-financial bridges.
//!
//! The engine is deliberately conservative. Consolidated statements can
//! support a range forecast, but they cannot reveal product volume, ASP or
//! segment mix. Those missing structural drivers remain explicit and make
//! `exact_eps_available=false`; the engine never fills them with a point
//! estimate. Every calculated line carries a formula and parameter IDs.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::metrics;
use crate::model::{
    BalanceSheet, CashFlowStatement, FundamentalBundle, IncomeStatement, ReportType,
};
use crate::valuation::{self, DcfInputs};

const MODEL_VERSION: &str = "earnings-driver-v1";
const DEFAULT_MONTE_CARLO_SAMPLES: usize = 1000;

/// Sector-specific accounting model. The templates intentionally do not
/// share one fake universal volume/price formula.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndustryTemplate {
    Financial,
    RealEstate,
    Resource,
    Manufacturing,
    Consumer,
    Software,
    Generic,
}

impl IndustryTemplate {
    pub fn label(self) -> &'static str {
        match self {
            Self::Financial => "金融",
            Self::RealEstate => "房地产",
            Self::Resource => "资源品",
            Self::Manufacturing => "制造业",
            Self::Consumer => "消费品",
            Self::Software => "软件与订阅服务",
            Self::Generic => "通用非金融",
        }
    }

    pub fn revenue_formula(self) -> &'static str {
        match self {
            Self::Financial => "净利息收入(生息资产×净息差)+手续费及佣金+投资收益",
            Self::RealEstate => "可结算面积×结算单价×权益比例+租赁及服务收入",
            Self::Resource => "销量×商品价格×汇率+副产品收入",
            Self::Manufacturing => "各产品销量×ASP，受产能×利用率约束",
            Self::Consumer => "各品类销量×ASP×渠道确认比例",
            Self::Software => "订阅席位×ARPU+续费+实施/授权收入",
            Self::Generic => "各业务分部收入之和",
        }
    }

    pub fn cost_formula(self) -> &'static str {
        match self {
            Self::Financial => "资金成本+信用减值+业务及管理费",
            Self::RealEstate => "结算面积×单位土地/建安成本+税费",
            Self::Resource => "销量×单位采选冶成本+能源+运输+资源税",
            Self::Manufacturing => "材料耗用×采购价+能源+人工+折旧+运输",
            Self::Consumer => "原料+包装+生产+渠道履约，考虑提价传导时滞",
            Self::Software => "交付人工+云资源+渠道分成+资本化摊销",
            Self::Generic => "主营成本+人工+折旧+运输",
        }
    }

    fn structural_drivers(self) -> &'static [&'static str] {
        match self {
            Self::Financial => &["生息资产", "净息差", "不良生成率", "信用成本"],
            Self::RealEstate => &["可售货值", "销售面积", "结算面积", "权益比例", "单位成本"],
            Self::Resource => &["产品销量", "商品价格", "品位/回收率", "单位成本", "汇率"],
            Self::Manufacturing => &["分产品销量", "ASP", "产能", "利用率", "材料单耗", "采购价"],
            Self::Consumer => &["分品类销量", "ASP", "渠道库存", "提价传导时滞", "原料成本"],
            Self::Software => &["订阅席位", "ARPU", "续费率", "新增客户", "云资源成本"],
            Self::Generic => &["分部收入", "销量/业务量", "价格", "主要投入成本"],
        }
    }

    fn prior_growth_range(self) -> (f64, f64, f64) {
        match self {
            Self::Financial => (-0.08, 0.04, 0.15),
            Self::RealEstate => (-0.25, -0.03, 0.18),
            Self::Resource => (-0.30, 0.00, 0.30),
            Self::Manufacturing => (-0.12, 0.06, 0.22),
            Self::Consumer => (-0.08, 0.07, 0.18),
            Self::Software => (-0.05, 0.15, 0.35),
            Self::Generic => (-0.12, 0.05, 0.20),
        }
    }
}

/// Provenance class. These classes are serialized separately so neither UI
/// nor Agent can accidentally present an assumption as a reported fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueOrigin {
    HistoricalFact,
    ManagementGuidance,
    MarketConsensus,
    UserAssumption,
    AgentAssumption,
    IndustryPrior,
}

/// Immutable evidence pointer for a driver value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriverEvidence {
    pub source_version_id: String,
    pub source_name: String,
    pub report_period: Option<String>,
    pub announced_date: Option<String>,
    pub locator: String,
    pub unit: String,
    pub confidence_low: f64,
    pub confidence_high: f64,
}

/// One input parameter with an uncertainty interval and provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriverParameter {
    pub id: String,
    pub name: String,
    pub category: String,
    pub value: Option<f64>,
    pub low: Option<f64>,
    pub high: Option<f64>,
    pub unit: String,
    pub origin: ValueOrigin,
    pub report_period: Option<String>,
    pub confidence: f64,
    pub evidence: Vec<DriverEvidence>,
    pub note: String,
}

/// Trace for one calculated forecast line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormulaNode {
    pub id: String,
    pub name: String,
    pub formula: String,
    pub parameter_ids: Vec<String>,
    pub unit: String,
    pub historical_value: Option<f64>,
    pub forecast_low: Option<f64>,
    pub forecast_base: Option<f64>,
    pub forecast_high: Option<f64>,
}

/// Hierarchical operating branch. Missing product/segment/region disclosure
/// is represented by a real node with `status=missing_disclosure`, not by an
/// invented zero-value leaf.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriverBranch {
    pub id: String,
    pub label: String,
    pub dimension: String,
    pub formula: String,
    pub status: String,
    pub parameter_ids: Vec<String>,
    pub children: Vec<DriverBranch>,
}

/// One scenario's complete income/cash-flow bridge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioFinancials {
    pub scenario: String,
    pub revenue: f64,
    pub gross_profit: f64,
    pub operating_profit: f64,
    pub tax: f64,
    pub minority_profit: f64,
    pub parent_net_profit: f64,
    pub eps: Option<f64>,
    pub operating_cash_flow: f64,
    pub capex: f64,
    pub free_cash_flow: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensitivityCell {
    pub revenue_growth: f64,
    pub gross_margin: f64,
    pub eps: Option<f64>,
    pub free_cash_flow: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonteCarloSummary {
    pub samples: usize,
    pub seed: u64,
    pub eps_p10: Option<f64>,
    pub eps_p50: Option<f64>,
    pub eps_p90: Option<f64>,
    pub fcf_p10: f64,
    pub fcf_p50: f64,
    pub fcf_p90: f64,
    pub method: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImpliedAssumption {
    pub current_price: Option<f64>,
    pub implied_fcf_growth: Option<f64>,
    pub search_low: f64,
    pub search_high: f64,
    pub wacc: f64,
    pub terminal_growth: f64,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriverTreeQuality {
    pub exact_eps_available: bool,
    pub model_completeness: f64,
    pub missing_core_drivers: Vec<String>,
    pub refusal_reason: Option<String>,
    pub warnings: Vec<String>,
}

/// Complete deterministic snapshot shared by driver analysis and valuation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EarningsDriverTree {
    pub snapshot_id: String,
    pub parameter_snapshot_id: String,
    pub model_version: String,
    pub symbol: String,
    pub company_name: Option<String>,
    pub industry: Option<String>,
    pub industry_template: IndustryTemplate,
    pub industry_template_label: String,
    pub revenue_formula: String,
    pub cost_formula: String,
    pub report_period: Option<String>,
    pub knowledge_time: i64,
    pub golden_template_reviewed: bool,
    pub parameters: Vec<DriverParameter>,
    pub revenue_tree: DriverBranch,
    pub cost_tree: DriverBranch,
    pub formula_nodes: Vec<FormulaNode>,
    pub scenarios: Vec<ScenarioFinancials>,
    pub sensitivity: Vec<SensitivityCell>,
    pub monte_carlo: Option<MonteCarloSummary>,
    pub implied_assumption: ImpliedAssumption,
    pub quality: DriverTreeQuality,
    pub provenance_legend: BTreeMap<String, String>,
}

/// Event/supply-chain shock to an operating parameter. Magnitudes are
/// decimals (`0.10` = +10%).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriverShock {
    pub kind: String,
    pub magnitude: f64,
    #[serde(default)]
    pub lag_months: u32,
    #[serde(default)]
    pub pass_through: Option<f64>,
    #[serde(default)]
    pub evidence_version_id: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgeDelta {
    pub revenue: f64,
    pub gross_profit: f64,
    pub operating_profit: f64,
    pub parent_net_profit: f64,
    pub eps: Option<f64>,
    pub operating_cash_flow: f64,
    pub free_cash_flow: f64,
}

/// Supply-chain event translated into a financial bridge, not a sentiment
/// score. Unsupported shocks remain visible in `warnings`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShockBridge {
    pub base_snapshot_id: String,
    pub shocked_snapshot_id: String,
    pub shocks: Vec<DriverShock>,
    pub base: Option<ScenarioFinancials>,
    pub shocked: Option<ScenarioFinancials>,
    pub delta: Option<BridgeDelta>,
    pub changed_parameters: Vec<DriverParameter>,
    pub warnings: Vec<String>,
}

/// Stable ID for the exact statement/market parameter set. The DCF endpoint
/// exposes this same ID, preventing silent use of a different snapshot.
pub fn parameter_snapshot_id(symbol: &str, bundle: &FundamentalBundle) -> String {
    let payload = serde_json::to_vec(&(
        symbol,
        &bundle.income,
        &bundle.balance,
        &bundle.cashflow,
        &bundle.indicators,
        &bundle.snapshot,
    ))
    .unwrap_or_default();
    let mut hash = Sha256::new();
    hash.update(MODEL_VERSION.as_bytes());
    hash.update(payload);
    format!("fp-{:x}", hash.finalize())
}

fn classify_industry(industry: Option<&str>, csrc: Option<&str>) -> IndustryTemplate {
    let text = format!(
        "{} {}",
        industry.unwrap_or_default(),
        csrc.unwrap_or_default()
    );
    if ["银行", "保险", "证券", "金融"]
        .iter()
        .any(|k| text.contains(k))
    {
        IndustryTemplate::Financial
    } else if ["房地产", "地产", "物业"].iter().any(|k| text.contains(k)) {
        IndustryTemplate::RealEstate
    } else if ["煤炭", "石油", "有色", "矿业", "钢铁", "化工原料"]
        .iter()
        .any(|k| text.contains(k))
    {
        IndustryTemplate::Resource
    } else if ["软件", "互联网", "信息服务", "云计算"]
        .iter()
        .any(|k| text.contains(k))
    {
        IndustryTemplate::Software
    } else if ["食品", "饮料", "白酒", "家电", "零售", "纺织", "医药商业"]
        .iter()
        .any(|k| text.contains(k))
    {
        IndustryTemplate::Consumer
    } else if ["制造", "设备", "电子", "汽车", "机械", "半导体", "电气"]
        .iter()
        .any(|k| text.contains(k))
    {
        IndustryTemplate::Manufacturing
    } else {
        IndustryTemplate::Generic
    }
}

/// Six representative templates were manually checked against their sector
/// accounting logic. This marks a template review, never company data review.
fn is_golden_template_symbol(symbol: &str, template: IndustryTemplate) -> bool {
    matches!(
        (symbol, template),
        ("600036", IndustryTemplate::Financial)
            | ("600048", IndustryTemplate::RealEstate)
            | ("601899", IndustryTemplate::Resource)
            | ("300308", IndustryTemplate::Manufacturing)
            | ("600519", IndustryTemplate::Consumer)
            | ("600588", IndustryTemplate::Software)
    )
}

fn annual_income(bundle: &FundamentalBundle) -> Vec<&IncomeStatement> {
    bundle
        .income
        .iter()
        .filter(|row| {
            row.meta
                .is_some_and(|meta| meta.report_type == ReportType::Annual)
        })
        .collect()
}

fn latest_annual_balance(bundle: &FundamentalBundle) -> Option<&BalanceSheet> {
    bundle.balance.iter().rev().find(|row| {
        row.meta
            .is_some_and(|meta| meta.report_type == ReportType::Annual)
    })
}

fn latest_annual_cashflow(bundle: &FundamentalBundle) -> Option<&CashFlowStatement> {
    bundle.cashflow.iter().rev().find(|row| {
        row.meta
            .is_some_and(|meta| meta.report_type == ReportType::Annual)
    })
}

fn finite(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite())
}

fn div(num: Option<f64>, den: Option<f64>) -> Option<f64> {
    let (num, den) = (finite(num)?, finite(den)?);
    (den.abs() > f64::EPSILON).then_some(num / den)
}

fn statement_evidence(
    symbol: &str,
    statement: &str,
    period: Option<String>,
    announced: Option<String>,
    locator: &str,
    unit: &str,
) -> Vec<DriverEvidence> {
    let period_token = period.clone().unwrap_or_else(|| "unknown".into());
    vec![DriverEvidence {
        source_version_id: format!("eastmoney-f10:{statement}:{symbol}:{period_token}"),
        source_name: "东方财富 F10 结构化财务报表".into(),
        report_period: period,
        announced_date: announced,
        locator: locator.into(),
        unit: unit.into(),
        confidence_low: 0.94,
        confidence_high: 0.99,
    }]
}

struct FactMeta<'a> {
    symbol: &'a str,
    statement: &'a str,
    period: Option<String>,
    announced: Option<String>,
    locator: &'a str,
}

fn fact_param(
    id: &str,
    name: &str,
    category: &str,
    value: Option<f64>,
    unit: &str,
    meta: FactMeta<'_>,
) -> DriverParameter {
    DriverParameter {
        id: id.into(),
        name: name.into(),
        category: category.into(),
        value: finite(value),
        low: finite(value),
        high: finite(value),
        unit: unit.into(),
        origin: ValueOrigin::HistoricalFact,
        report_period: meta.period.clone(),
        confidence: if value.is_some() { 0.98 } else { 0.0 },
        evidence: value
            .map(|_| {
                statement_evidence(
                    meta.symbol,
                    meta.statement,
                    meta.period,
                    meta.announced,
                    meta.locator,
                    unit,
                )
            })
            .unwrap_or_default(),
        note: if value.is_some() {
            "已披露历史值，不是预测".into()
        } else {
            "上游未披露，保持缺失".into()
        },
    }
}

fn assumption_param(
    id: &str,
    name: &str,
    value: f64,
    low: f64,
    high: f64,
    origin: ValueOrigin,
    note: String,
) -> DriverParameter {
    DriverParameter {
        id: id.into(),
        name: name.into(),
        category: "forecast_assumption".into(),
        value: Some(value),
        low: Some(low),
        high: Some(high),
        unit: "decimal".into(),
        origin,
        report_period: None,
        confidence: if origin == ValueOrigin::IndustryPrior {
            0.35
        } else {
            0.70
        },
        evidence: Vec::new(),
        note,
    }
}

fn quantile(mut values: Vec<f64>, q: f64) -> Option<f64> {
    values.retain(|v| v.is_finite());
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let index = ((values.len() - 1) as f64 * q.clamp(0.0, 1.0)).round() as usize;
    values.get(index).copied()
}

fn historical_growth(bundle: &FundamentalBundle) -> Vec<f64> {
    let annual = annual_income(bundle);
    annual
        .windows(2)
        .filter_map(|pair| {
            div(
                pair[1].total_operating_revenue,
                pair[0].total_operating_revenue,
            )
            .map(|ratio| ratio - 1.0)
        })
        .filter(|g| (-0.8..=3.0).contains(g))
        .collect()
}

fn ratio_history(
    bundle: &FundamentalBundle,
    f: impl Fn(&IncomeStatement) -> Option<f64>,
) -> Vec<f64> {
    annual_income(bundle)
        .into_iter()
        .filter_map(f)
        .filter(|v| v.is_finite())
        .collect()
}

fn range_assumption(
    id: &str,
    name: &str,
    observed: Vec<f64>,
    fallback: (f64, f64, f64),
    clamp: (f64, f64),
    note: &str,
) -> DriverParameter {
    let (low, base, high, origin, detail) = if observed.len() >= 2 {
        (
            quantile(observed.clone(), 0.20).unwrap(),
            quantile(observed.clone(), 0.50).unwrap(),
            quantile(observed, 0.80).unwrap(),
            ValueOrigin::HistoricalFact,
            "区间取公司已披露年度历史的 20/50/80 分位".to_string(),
        )
    } else if let Some(value) = observed.first().copied() {
        (
            value - 0.03,
            value,
            value + 0.03,
            ValueOrigin::HistoricalFact,
            "仅一个可用历史点，区间按±3个百分点展示，置信度较低".to_string(),
        )
    } else {
        (
            fallback.0,
            fallback.1,
            fallback.2,
            ValueOrigin::IndustryPrior,
            "缺少公司历史，采用明确标注的行业宽区间先验，不作为公司事实".to_string(),
        )
    };
    assumption_param(
        id,
        name,
        base.clamp(clamp.0, clamp.1),
        low.clamp(clamp.0, clamp.1),
        high.clamp(clamp.0, clamp.1),
        origin,
        format!("{note}；{detail}"),
    )
}

fn parameter<'a>(parameters: &'a [DriverParameter], id: &str) -> Option<&'a DriverParameter> {
    parameters.iter().find(|p| p.id == id)
}

fn p_value(parameters: &[DriverParameter], id: &str) -> Option<f64> {
    parameter(parameters, id)?.value
}

fn p_low(parameters: &[DriverParameter], id: &str) -> Option<f64> {
    parameter(parameters, id)?.low
}

fn p_high(parameters: &[DriverParameter], id: &str) -> Option<f64> {
    parameter(parameters, id)?.high
}

#[derive(Clone, Copy)]
struct ScenarioInputs {
    revenue: f64,
    growth: f64,
    gross_margin: f64,
    opex_ratio: f64,
    tax_rate: f64,
    minority_ratio: f64,
    cfo_margin: f64,
    capex_ratio: f64,
    shares: Option<f64>,
}

fn compute_scenario(scenario: &str, inputs: ScenarioInputs) -> ScenarioFinancials {
    let revenue = inputs.revenue * (1.0 + inputs.growth);
    let gross_profit = revenue * inputs.gross_margin;
    let operating_profit = gross_profit - revenue * inputs.opex_ratio;
    let taxable_profit = operating_profit.max(0.0);
    let tax = taxable_profit * inputs.tax_rate;
    let net_profit = operating_profit - tax;
    let minority_profit = net_profit.max(0.0) * inputs.minority_ratio;
    let parent_net_profit = net_profit - minority_profit;
    let operating_cash_flow = revenue * inputs.cfo_margin;
    let capex = revenue * inputs.capex_ratio;
    ScenarioFinancials {
        scenario: scenario.into(),
        revenue,
        gross_profit,
        operating_profit,
        tax,
        minority_profit,
        parent_net_profit,
        eps: inputs
            .shares
            .filter(|s| *s > 0.0)
            .map(|s| parent_net_profit / s),
        operating_cash_flow,
        capex,
        free_cash_flow: operating_cash_flow - capex,
    }
}

fn scenarios_from(parameters: &[DriverParameter]) -> Vec<ScenarioFinancials> {
    let Some(revenue) = p_value(parameters, "base_revenue") else {
        return Vec::new();
    };
    let Some(growth) = p_value(parameters, "revenue_growth") else {
        return Vec::new();
    };
    let Some(gross) = p_value(parameters, "gross_margin") else {
        return Vec::new();
    };
    let Some(opex) = p_value(parameters, "opex_ratio") else {
        return Vec::new();
    };
    let tax = p_value(parameters, "tax_rate").unwrap_or(0.25);
    let minority = p_value(parameters, "minority_ratio").unwrap_or(0.0);
    let cfo = p_value(parameters, "cfo_margin").unwrap_or(0.0);
    let capex = p_value(parameters, "capex_ratio").unwrap_or(0.0);
    let shares = p_value(parameters, "shares");
    let base = ScenarioInputs {
        revenue,
        growth,
        gross_margin: gross,
        opex_ratio: opex,
        tax_rate: tax,
        minority_ratio: minority,
        cfo_margin: cfo,
        capex_ratio: capex,
        shares,
    };
    vec![
        compute_scenario(
            "bear",
            ScenarioInputs {
                growth: p_low(parameters, "revenue_growth").unwrap_or(growth),
                gross_margin: p_low(parameters, "gross_margin").unwrap_or(gross),
                opex_ratio: p_high(parameters, "opex_ratio").unwrap_or(opex),
                ..base
            },
        ),
        compute_scenario("base", base),
        compute_scenario(
            "bull",
            ScenarioInputs {
                growth: p_high(parameters, "revenue_growth").unwrap_or(growth),
                gross_margin: p_high(parameters, "gross_margin").unwrap_or(gross),
                opex_ratio: p_low(parameters, "opex_ratio").unwrap_or(opex),
                ..base
            },
        ),
    ]
}

fn sensitivity_from(parameters: &[DriverParameter]) -> Vec<SensitivityCell> {
    let Some(revenue) = p_value(parameters, "base_revenue") else {
        return Vec::new();
    };
    let Some(growth) = p_value(parameters, "revenue_growth") else {
        return Vec::new();
    };
    let Some(gross) = p_value(parameters, "gross_margin") else {
        return Vec::new();
    };
    let Some(opex) = p_value(parameters, "opex_ratio") else {
        return Vec::new();
    };
    let tax = p_value(parameters, "tax_rate").unwrap_or(0.25);
    let minority = p_value(parameters, "minority_ratio").unwrap_or(0.0);
    let cfo = p_value(parameters, "cfo_margin").unwrap_or(0.0);
    let capex = p_value(parameters, "capex_ratio").unwrap_or(0.0);
    let shares = p_value(parameters, "shares");
    let base = ScenarioInputs {
        revenue,
        growth,
        gross_margin: gross,
        opex_ratio: opex,
        tax_rate: tax,
        minority_ratio: minority,
        cfo_margin: cfo,
        capex_ratio: capex,
        shares,
    };
    let mut cells = Vec::new();
    for growth_delta in [-0.05, 0.0, 0.05] {
        for margin_delta in [-0.03, 0.0, 0.03] {
            let row = compute_scenario(
                "sensitivity",
                ScenarioInputs {
                    growth: growth + growth_delta,
                    gross_margin: (gross + margin_delta).clamp(-0.5, 0.95),
                    ..base
                },
            );
            cells.push(SensitivityCell {
                revenue_growth: growth + growth_delta,
                gross_margin: gross + margin_delta,
                eps: row.eps,
                free_cash_flow: row.free_cash_flow,
            });
        }
    }
    cells
}

fn next_random(state: &mut u64) -> f64 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^= z >> 31;
    (z >> 11) as f64 / ((1_u64 << 53) as f64)
}

fn draw(parameters: &[DriverParameter], id: &str, state: &mut u64) -> Option<f64> {
    let p = parameter(parameters, id)?;
    let (low, high) = (p.low?, p.high?);
    Some(low + (high - low) * next_random(state))
}

fn monte_carlo(
    parameters: &[DriverParameter],
    snapshot_id: &str,
    samples: usize,
) -> Option<MonteCarloSummary> {
    let revenue = p_value(parameters, "base_revenue")?;
    let opex = p_value(parameters, "opex_ratio")?;
    let tax = p_value(parameters, "tax_rate").unwrap_or(0.25);
    let minority = p_value(parameters, "minority_ratio").unwrap_or(0.0);
    let cfo = p_value(parameters, "cfo_margin").unwrap_or(0.0);
    let capex = p_value(parameters, "capex_ratio").unwrap_or(0.0);
    let shares = p_value(parameters, "shares");
    let base = ScenarioInputs {
        revenue,
        growth: 0.0,
        gross_margin: 0.0,
        opex_ratio: opex,
        tax_rate: tax,
        minority_ratio: minority,
        cfo_margin: cfo,
        capex_ratio: capex,
        shares,
    };
    let digest = Sha256::digest(snapshot_id.as_bytes());
    let seed = u64::from_le_bytes(digest[0..8].try_into().unwrap_or([0; 8]));
    let samples = samples.clamp(100, 20_000);
    let mut state = seed;
    let mut eps = Vec::new();
    let mut fcf = Vec::with_capacity(samples);
    for _ in 0..samples {
        let row = compute_scenario(
            "monte_carlo",
            ScenarioInputs {
                growth: draw(parameters, "revenue_growth", &mut state)?,
                gross_margin: draw(parameters, "gross_margin", &mut state)?,
                opex_ratio: draw(parameters, "opex_ratio", &mut state).unwrap_or(opex),
                ..base
            },
        );
        if let Some(value) = row.eps {
            eps.push(value);
        }
        fcf.push(row.free_cash_flow);
    }
    Some(MonteCarloSummary {
        samples,
        seed,
        eps_p10: quantile(eps.clone(), 0.10),
        eps_p50: quantile(eps.clone(), 0.50),
        eps_p90: quantile(eps, 0.90),
        fcf_p10: quantile(fcf.clone(), 0.10)?,
        fcf_p50: quantile(fcf.clone(), 0.50)?,
        fcf_p90: quantile(fcf, 0.90)?,
        method: "确定性种子的分层区间抽样；只在已声明参数区间内取值，不代表正态分布".into(),
    })
}

fn implied_assumption(
    bundle: &FundamentalBundle,
    parameters: &[DriverParameter],
) -> ImpliedAssumption {
    let price = bundle
        .snapshot
        .as_ref()
        .map(|s| s.price)
        .filter(|v| *v > 0.0);
    let base_fcf = latest_annual_cashflow(bundle).and_then(|cf| metrics::fcf(cf.net_cfo, cf.capex));
    let shares = p_value(parameters, "shares");
    let bs = latest_annual_balance(bundle);
    let net_debt = bs
        .map(|b| b.interest_bearing_debt().unwrap_or(0.0) - b.monetary_funds.unwrap_or(0.0))
        .unwrap_or(0.0);
    let mut output = ImpliedAssumption {
        current_price: price,
        implied_fcf_growth: None,
        search_low: -0.50,
        search_high: 1.00,
        wacc: 0.09,
        terminal_growth: 0.025,
        explanation: "需要现价、正自由现金流和总股本才能反向求解".into(),
    };
    let (Some(price), Some(base_fcf), Some(shares)) = (price, base_fcf, shares) else {
        return output;
    };
    if base_fcf <= 0.0 || shares <= 0.0 {
        return output;
    }
    let value = |growth: f64| {
        valuation::dcf_fcff(&DcfInputs {
            base_fcf,
            stage1_years: 5,
            stage1_growth: growth,
            terminal_growth: 0.025,
            wacc: 0.09,
            net_debt,
            shares,
        })
        .map(|result| result.per_share)
    };
    let (Some(low_value), Some(high_value)) = (value(output.search_low), value(output.search_high))
    else {
        return output;
    };
    if price < low_value || price > high_value {
        output.explanation = format!(
            "现价不在搜索区间对应的 DCF 值域 {low_value:.2}–{high_value:.2} 元内，不外推伪精度"
        );
        return output;
    }
    let (mut low, mut high) = (output.search_low, output.search_high);
    for _ in 0..80 {
        let mid = (low + high) / 2.0;
        if value(mid).unwrap_or(f64::INFINITY) < price {
            low = mid;
        } else {
            high = mid;
        }
    }
    output.implied_fcf_growth = Some((low + high) / 2.0);
    output.explanation =
        "在 WACC=9%、永续增长=2.5%、五年显式期不变时，反向求解现价隐含的 FCF 年增长率；不是预测"
            .into();
    output
}

struct FormulaDefinition<'a> {
    id: &'a str,
    name: &'a str,
    formula: &'a str,
    parameter_ids: &'a [&'a str],
    value: fn(&ScenarioFinancials) -> Option<f64>,
    historical_value: Option<f64>,
}

fn formula_nodes(
    parameters: &[DriverParameter],
    scenarios: &[ScenarioFinancials],
) -> Vec<FormulaNode> {
    let historical = |id| p_value(parameters, id);
    let pick =
        |idx: usize, f: fn(&ScenarioFinancials) -> Option<f64>| scenarios.get(idx).and_then(f);
    let defs = [
        FormulaDefinition {
            id: "forecast_revenue",
            name: "预测收入",
            formula: "历史收入×(1+收入增长率)",
            parameter_ids: &["base_revenue", "revenue_growth"],
            value: |s| Some(s.revenue),
            historical_value: historical("base_revenue"),
        },
        FormulaDefinition {
            id: "forecast_gross_profit",
            name: "毛利润",
            formula: "预测收入×毛利率",
            parameter_ids: &["base_revenue", "revenue_growth", "gross_margin"],
            value: |s| Some(s.gross_profit),
            historical_value: None,
        },
        FormulaDefinition {
            id: "forecast_operating_profit",
            name: "营业利润",
            formula: "毛利润−预测收入×期间费用率",
            parameter_ids: &[
                "base_revenue",
                "revenue_growth",
                "gross_margin",
                "opex_ratio",
            ],
            value: |s| Some(s.operating_profit),
            historical_value: historical("base_operating_profit"),
        },
        FormulaDefinition {
            id: "forecast_parent_profit",
            name: "归母净利润",
            formula: "(营业利润−所得税)×(1−少数股东损益占比)",
            parameter_ids: &["tax_rate", "minority_ratio"],
            value: |s| Some(s.parent_net_profit),
            historical_value: historical("base_parent_profit"),
        },
        FormulaDefinition {
            id: "forecast_eps",
            name: "每股收益区间",
            formula: "归母净利润÷总股本",
            parameter_ids: &["shares"],
            value: |s| s.eps,
            historical_value: None,
        },
        FormulaDefinition {
            id: "forecast_cfo",
            name: "经营现金流",
            formula: "预测收入×经营现金流率",
            parameter_ids: &["cfo_margin"],
            value: |s| Some(s.operating_cash_flow),
            historical_value: historical("base_cfo"),
        },
        FormulaDefinition {
            id: "forecast_fcf",
            name: "自由现金流",
            formula: "经营现金流−资本开支",
            parameter_ids: &["cfo_margin", "capex_ratio"],
            value: |s| Some(s.free_cash_flow),
            historical_value: None,
        },
    ];
    defs.into_iter()
        .map(|definition| FormulaNode {
            id: definition.id.into(),
            name: definition.name.into(),
            formula: definition.formula.into(),
            parameter_ids: definition
                .parameter_ids
                .iter()
                .map(|id| (*id).into())
                .collect(),
            unit: if definition.id == "forecast_eps" {
                "CNY/share"
            } else {
                "CNY"
            }
            .into(),
            historical_value: definition.historical_value,
            forecast_low: pick(0, definition.value),
            forecast_base: pick(1, definition.value),
            forecast_high: pick(2, definition.value),
        })
        .collect()
}

fn provenance_legend() -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "historical_fact".into(),
            "历史事实：来自已披露财务报表".into(),
        ),
        (
            "management_guidance".into(),
            "管理层指引：公司明确给出的前瞻区间".into(),
        ),
        (
            "market_consensus".into(),
            "市场一致预期：带日期和供应商版本的预测".into(),
        ),
        ("user_assumption".into(), "用户假设：由用户明确输入".into()),
        (
            "agent_assumption".into(),
            "Agent 假设：必须单独标注并可撤销".into(),
        ),
        (
            "industry_prior".into(),
            "行业先验：数据不足时使用的宽区间，绝不是公司事实".into(),
        ),
    ])
}

fn missing_branch(id: &str, label: &str, dimension: &str, formula: &str) -> DriverBranch {
    DriverBranch {
        id: id.into(),
        label: label.into(),
        dimension: dimension.into(),
        formula: formula.into(),
        status: "missing_disclosure".into(),
        parameter_ids: Vec::new(),
        children: Vec::new(),
    }
}

fn operating_trees(template: IndustryTemplate) -> (DriverBranch, DriverBranch) {
    let revenue_children = match template {
        IndustryTemplate::Financial => vec![
            missing_branch(
                "interest_income",
                "净利息收入",
                "business_segment",
                "生息资产×净息差",
            ),
            missing_branch(
                "fee_income",
                "手续费及佣金",
                "business_segment",
                "业务量×费率",
            ),
            missing_branch(
                "investment_income",
                "投资收益",
                "business_segment",
                "投资资产×收益率",
            ),
        ],
        IndustryTemplate::RealEstate => vec![
            missing_branch(
                "property_delivery",
                "开发项目结算",
                "project_region",
                "结算面积×结算单价×权益比例",
            ),
            missing_branch(
                "rental_service",
                "租赁与服务",
                "business_segment",
                "可租面积×出租率×租金",
            ),
        ],
        IndustryTemplate::Resource => vec![
            missing_branch(
                "primary_product",
                "主要矿产品",
                "product_region",
                "销量×商品价格×汇率",
            ),
            missing_branch("by_product", "副产品", "product", "副产品销量×价格"),
        ],
        IndustryTemplate::Manufacturing => vec![
            missing_branch(
                "manufacturing_products",
                "分产品收入",
                "product_region",
                "销量×ASP",
            ),
            missing_branch(
                "capacity_constraint",
                "产能约束",
                "capacity_region",
                "产能×利用率",
            ),
        ],
        IndustryTemplate::Consumer => vec![
            missing_branch("consumer_categories", "分品类收入", "product", "销量×ASP"),
            missing_branch(
                "consumer_channels",
                "分渠道/地区确认",
                "channel_region",
                "发货×确认比例",
            ),
        ],
        IndustryTemplate::Software => vec![
            missing_branch(
                "subscription",
                "订阅收入",
                "product_region",
                "席位×ARPU×续费率",
            ),
            missing_branch(
                "license_service",
                "授权与实施",
                "business_segment",
                "项目数×客单价×验收率",
            ),
        ],
        IndustryTemplate::Generic => vec![
            missing_branch(
                "generic_segments",
                "业务分部",
                "business_segment",
                "各分部业务量×价格",
            ),
            missing_branch("generic_regions", "地区结构", "region", "各地区收入之和"),
        ],
    };
    let cost_children = match template {
        IndustryTemplate::Financial => vec![
            missing_branch(
                "funding_cost",
                "资金成本",
                "cost_driver",
                "付息负债×资金成本率",
            ),
            missing_branch(
                "credit_cost",
                "信用成本",
                "cost_driver",
                "风险资产×信用成本率",
            ),
            missing_branch(
                "bank_opex",
                "业务及管理费",
                "cost_driver",
                "人员+网点+科技投入",
            ),
        ],
        IndustryTemplate::RealEstate => vec![
            missing_branch(
                "land_cost",
                "土地成本",
                "project_region",
                "结算面积×单位土地成本",
            ),
            missing_branch(
                "construction_cost",
                "建安成本",
                "project_region",
                "结算面积×单位建安成本",
            ),
        ],
        IndustryTemplate::Resource => vec![
            missing_branch(
                "mining_cost",
                "采选冶成本",
                "product_region",
                "销量×单位成本",
            ),
            missing_branch(
                "resource_energy",
                "能源运输与资源税",
                "cost_driver",
                "耗用量×价格+从价税",
            ),
        ],
        IndustryTemplate::Manufacturing => vec![
            missing_branch(
                "materials",
                "主要材料",
                "input_product",
                "产量×材料单耗×采购价",
            ),
            missing_branch(
                "conversion_cost",
                "能源/人工/折旧",
                "capacity_region",
                "产能、利用率与固定成本",
            ),
            missing_branch("freight", "运输", "region", "销量×单位运费"),
        ],
        IndustryTemplate::Consumer => vec![
            missing_branch(
                "consumer_inputs",
                "原料与包装",
                "input_product",
                "销量×单耗×采购价",
            ),
            missing_branch(
                "channel_fulfilment",
                "生产与渠道履约",
                "channel",
                "销量×单位履约成本",
            ),
        ],
        IndustryTemplate::Software => vec![
            missing_branch(
                "delivery_labor",
                "交付人工",
                "business_segment",
                "项目工时×单位人力成本",
            ),
            missing_branch(
                "cloud_cost",
                "云资源与渠道分成",
                "product_region",
                "用量×单价+收入×分成率",
            ),
        ],
        IndustryTemplate::Generic => vec![
            missing_branch(
                "generic_inputs",
                "主要投入",
                "input_product",
                "耗用量×采购价",
            ),
            missing_branch(
                "generic_fixed",
                "人工与折旧",
                "cost_driver",
                "人员成本+资产×折旧率",
            ),
        ],
    };
    (
        DriverBranch {
            id: "consolidated_revenue".into(),
            label: "合并营业收入".into(),
            dimension: "company_total".into(),
            formula: "已披露合并收入；下级分部不得由系统臆测拆分".into(),
            status: "consolidated_fact".into(),
            parameter_ids: vec!["base_revenue".into(), "revenue_growth".into()],
            children: revenue_children,
        },
        DriverBranch {
            id: "consolidated_cost".into(),
            label: "合并营业成本".into(),
            dimension: "company_total".into(),
            formula: "已披露合并成本；下级投入结构缺失时保持缺失".into(),
            status: "consolidated_fact".into(),
            parameter_ids: vec!["base_cost".into(), "gross_margin".into()],
            children: cost_children,
        },
    )
}

/// Build a conservative driver tree from one immutable fundamental bundle.
pub fn build_earnings_driver_tree(
    symbol: &str,
    bundle: &FundamentalBundle,
    knowledge_time: i64,
) -> EarningsDriverTree {
    let profile = bundle.profile.as_ref();
    let template = classify_industry(
        profile.and_then(|p| p.industry.as_deref()),
        profile.and_then(|p| p.industry_csrc.as_deref()),
    );
    let annual = annual_income(bundle);
    let income = annual.last().copied().or_else(|| bundle.income.last());
    let balance = latest_annual_balance(bundle).or_else(|| bundle.balance.last());
    let cashflow = latest_annual_cashflow(bundle).or_else(|| bundle.cashflow.last());
    let meta = income.and_then(|row| row.meta);
    let period = meta.map(|m| m.period_end.to_string());
    let announced = meta.and_then(|m| m.announced).map(|d| d.to_string());
    let revenue = income.and_then(|row| row.total_operating_revenue.or(row.operating_revenue));
    let cost = income.and_then(|row| row.operating_cost);
    let opex = income.and_then(|row| {
        let values = [
            row.selling_expense,
            row.admin_expense,
            row.rd_expense,
            row.finance_expense,
        ];
        (!values.iter().all(Option::is_none)).then(|| values.iter().flatten().sum())
    });
    let gross_history = ratio_history(bundle, |row| {
        metrics::gross_margin(
            row.operating_revenue.or(row.total_operating_revenue),
            row.operating_cost,
        )
    });
    let opex_history = ratio_history(bundle, |row| {
        let values = [
            row.selling_expense,
            row.admin_expense,
            row.rd_expense,
            row.finance_expense,
        ];
        let sum = (!values.iter().all(Option::is_none)).then(|| values.iter().flatten().sum())?;
        div(
            Some(sum),
            row.total_operating_revenue.or(row.operating_revenue),
        )
    });
    let growth_history = historical_growth(bundle);
    let growth_fallback = template.prior_growth_range();
    let gross_fallback = match template {
        IndustryTemplate::Software => (0.40, 0.60, 0.80),
        IndustryTemplate::Consumer => (0.20, 0.38, 0.60),
        IndustryTemplate::Financial => (0.20, 0.35, 0.55),
        _ => (0.10, 0.25, 0.45),
    };
    let opex_fallback = match template {
        IndustryTemplate::Software => (0.25, 0.40, 0.60),
        IndustryTemplate::Consumer => (0.10, 0.20, 0.35),
        _ => (0.08, 0.18, 0.35),
    };
    let tax_rate = income
        .and_then(|row| div(row.income_tax, row.total_profit))
        .unwrap_or(0.25)
        .clamp(0.0, 0.60);
    let minority_ratio = income
        .and_then(|row| div(row.minority_profit, row.net_profit))
        .unwrap_or(0.0)
        .clamp(-0.5, 0.8);
    let cfo_margin = cashflow
        .and_then(|row| div(row.net_cfo, revenue))
        .unwrap_or(0.0)
        .clamp(-1.0, 2.0);
    let capex_ratio = cashflow
        .and_then(|row| div(row.capex, revenue))
        .unwrap_or(0.0)
        .clamp(0.0, 2.0);
    let shares = bundle
        .snapshot
        .as_ref()
        .and_then(|s| s.total_shares)
        .or_else(|| profile.and_then(|p| p.total_shares))
        .or_else(|| balance.and_then(|b| b.share_capital));
    let fact = |statement, locator| FactMeta {
        symbol,
        statement,
        period: period.clone(),
        announced: announced.clone(),
        locator,
    };
    let mut parameters = vec![
        fact_param(
            "base_revenue",
            "历史营业收入",
            "revenue",
            revenue,
            "CNY",
            fact("income", "TOTAL_OPERATE_INCOME/OPERATE_INCOME"),
        ),
        fact_param(
            "base_cost",
            "历史营业成本",
            "cost",
            cost,
            "CNY",
            fact("income", "OPERATE_COST"),
        ),
        fact_param(
            "base_opex",
            "历史期间费用",
            "cost",
            opex,
            "CNY",
            fact("income", "SALE+MANAGE+RESEARCH+FINANCE_EXPENSE"),
        ),
        fact_param(
            "base_operating_profit",
            "历史营业利润",
            "profit",
            income.and_then(|r| r.operating_profit),
            "CNY",
            fact("income", "OPERATE_PROFIT"),
        ),
        fact_param(
            "base_parent_profit",
            "历史归母净利润",
            "profit",
            income.and_then(|r| r.net_profit_parent),
            "CNY",
            fact("income", "PARENT_NETPROFIT"),
        ),
        fact_param(
            "base_cfo",
            "历史经营现金流",
            "cash_flow",
            cashflow.and_then(|r| r.net_cfo),
            "CNY",
            fact("cashflow", "NETCASH_OPERATE"),
        ),
        fact_param(
            "base_capex",
            "历史资本开支",
            "cash_flow",
            cashflow.and_then(|r| r.capex),
            "CNY",
            fact("cashflow", "CONSTRUCT_LONG_ASSET"),
        ),
        fact_param(
            "base_depreciation",
            "历史折旧",
            "cash_flow",
            cashflow.and_then(|r| r.depreciation),
            "CNY",
            fact("cashflow", "FA_IR_DEPR"),
        ),
        fact_param(
            "inventory",
            "期末存货",
            "working_capital",
            balance.and_then(|r| r.inventory),
            "CNY",
            fact("balance", "INVENTORY"),
        ),
        fact_param(
            "receivables",
            "期末应收",
            "working_capital",
            balance.and_then(|r| r.notes_and_accounts_receivable.or(r.accounts_receivable)),
            "CNY",
            fact("balance", "NOTE_ACCOUNTS_RECE/ACCOUNTS_RECE"),
        ),
        fact_param(
            "payables",
            "期末应付",
            "working_capital",
            balance.and_then(|r| r.notes_and_accounts_payable.or(r.accounts_payable)),
            "CNY",
            fact("balance", "NOTE_ACCOUNTS_PAYABLE/ACCOUNTS_PAYABLE"),
        ),
        fact_param(
            "shares",
            "总股本",
            "per_share",
            shares,
            "share",
            fact("quote/profile/balance", "TOTAL_SHARES/SHARE_CAPITAL"),
        ),
        range_assumption(
            "revenue_growth",
            "收入增长率",
            growth_history,
            growth_fallback,
            (-0.80, 1.50),
            "收入增长假设",
        ),
        range_assumption(
            "gross_margin",
            "毛利率",
            gross_history,
            gross_fallback,
            (-0.50, 0.95),
            "毛利率假设",
        ),
        range_assumption(
            "opex_ratio",
            "期间费用率",
            opex_history,
            opex_fallback,
            (-0.20, 1.50),
            "期间费用率假设",
        ),
        assumption_param(
            "tax_rate",
            "有效税率",
            tax_rate,
            (tax_rate - 0.03).max(0.0),
            (tax_rate + 0.03).min(0.60),
            ValueOrigin::HistoricalFact,
            "历史所得税÷利润总额；异常或缺失时使用明确默认25%".into(),
        ),
        assumption_param(
            "minority_ratio",
            "少数股东损益占比",
            minority_ratio,
            (minority_ratio - 0.02).max(-0.5),
            (minority_ratio + 0.02).min(0.8),
            ValueOrigin::HistoricalFact,
            "历史少数股东损益÷净利润".into(),
        ),
        assumption_param(
            "cfo_margin",
            "经营现金流率",
            cfo_margin,
            cfo_margin - 0.05,
            cfo_margin + 0.05,
            ValueOrigin::HistoricalFact,
            "历史经营现金流÷营业收入".into(),
        ),
        assumption_param(
            "capex_ratio",
            "资本开支率",
            capex_ratio,
            (capex_ratio - 0.03).max(0.0),
            capex_ratio + 0.03,
            ValueOrigin::HistoricalFact,
            "历史资本开支÷营业收入".into(),
        ),
    ];
    // Assumption rows that fell back to a default must not masquerade as facts.
    if cashflow.and_then(|row| row.net_cfo).is_none() {
        if let Some(p) = parameters.iter_mut().find(|p| p.id == "cfo_margin") {
            p.origin = ValueOrigin::IndustryPrior;
            p.confidence = 0.2;
        }
    }
    if cashflow.and_then(|row| row.capex).is_none() {
        if let Some(p) = parameters.iter_mut().find(|p| p.id == "capex_ratio") {
            p.origin = ValueOrigin::IndustryPrior;
            p.confidence = 0.2;
        }
    }
    let parameter_snapshot_id = parameter_snapshot_id(symbol, bundle);
    let scenarios = scenarios_from(&parameters);
    let sensitivity = sensitivity_from(&parameters);
    let mut snapshot_hash = Sha256::new();
    snapshot_hash.update(parameter_snapshot_id.as_bytes());
    snapshot_hash.update(knowledge_time.to_le_bytes());
    snapshot_hash.update(serde_json::to_vec(&parameters).unwrap_or_default());
    let snapshot_id = format!("edt-{:x}", snapshot_hash.finalize());
    let structural_missing: Vec<String> = template
        .structural_drivers()
        .iter()
        .map(|s| (*s).into())
        .collect();
    let required_missing: Vec<String> = [
        ("营业收入", revenue),
        ("营业成本/毛利率", cost),
        ("总股本", shares),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.is_none().then_some(name.into()))
    .collect();
    let mut missing = structural_missing;
    missing.extend(required_missing.clone());
    let completeness = 1.0 - required_missing.len() as f64 / 3.0;
    let refusal_reason = if !required_missing.is_empty() {
        Some(format!(
            "缺少{}，拒绝输出精确 EPS；仅保留可验证的历史参数",
            required_missing.join("、")
        ))
    } else {
        None
    };
    let quality = DriverTreeQuality {
        exact_eps_available: false,
        model_completeness: completeness.clamp(0.0, 1.0),
        missing_core_drivers: missing,
        refusal_reason,
        warnings: vec![
            "当前结构化 F10 主要是合并报表；未取得分产品/地区披露前，只输出宽区间，不给单点盈利承诺".into(),
            "情景与 Monte Carlo 是假设传播，不是收益概率或投资建议".into(),
        ],
    };
    let nodes = formula_nodes(&parameters, &scenarios);
    let (revenue_tree, cost_tree) = operating_trees(template);
    let monte_carlo = monte_carlo(&parameters, &snapshot_id, DEFAULT_MONTE_CARLO_SAMPLES);
    let implied = implied_assumption(bundle, &parameters);
    EarningsDriverTree {
        snapshot_id,
        parameter_snapshot_id,
        model_version: MODEL_VERSION.into(),
        symbol: symbol.into(),
        company_name: profile.map(|p| {
            if p.short_name.is_empty() {
                p.name.clone()
            } else {
                p.short_name.clone()
            }
        }),
        industry: profile.and_then(|p| p.industry.clone()),
        industry_template: template,
        industry_template_label: template.label().into(),
        revenue_formula: template.revenue_formula().into(),
        cost_formula: template.cost_formula().into(),
        report_period: period,
        knowledge_time,
        golden_template_reviewed: is_golden_template_symbol(symbol, template),
        parameters,
        revenue_tree,
        cost_tree,
        formula_nodes: nodes,
        scenarios,
        sensitivity,
        monte_carlo,
        implied_assumption: implied,
        quality,
        provenance_legend: provenance_legend(),
    }
}

fn update_assumption(
    parameters: &mut [DriverParameter],
    id: &str,
    value: f64,
    shock: &DriverShock,
) -> Option<DriverParameter> {
    let parameter = parameters.iter_mut().find(|p| p.id == id)?;
    parameter.value = Some(value);
    parameter.low = Some(value);
    parameter.high = Some(value);
    parameter.origin = ValueOrigin::AgentAssumption;
    parameter.confidence = 0.50;
    parameter.note = format!(
        "由事件冲击映射：{}，幅度{:.2}%，滞后{}个月；{}",
        shock.kind,
        shock.magnitude * 100.0,
        shock.lag_months,
        shock.note.as_deref().unwrap_or("无补充说明")
    );
    if let Some(version) = &shock.evidence_version_id {
        parameter.evidence = vec![DriverEvidence {
            source_version_id: version.clone(),
            source_name: "事件/供应链证据".into(),
            report_period: None,
            announced_date: None,
            locator: "shock_input".into(),
            unit: "decimal".into(),
            confidence_low: 0.50,
            confidence_high: 0.80,
        }];
    }
    Some(parameter.clone())
}

/// Apply evidence-bound supply-chain shocks and rebuild the complete bridge.
pub fn apply_driver_shocks(tree: &EarningsDriverTree, shocks: &[DriverShock]) -> ShockBridge {
    let mut parameters = tree.parameters.clone();
    let mut changed = Vec::new();
    let mut warnings = Vec::new();
    for shock in shocks {
        if !shock.magnitude.is_finite() || !(-2.0..=5.0).contains(&shock.magnitude) {
            warnings.push(format!("{} 幅度超出可计算范围，已忽略", shock.kind));
            continue;
        }
        let updated = match shock.kind.as_str() {
            "volume" | "product_price" | "revenue" => {
                let base = p_value(&parameters, "revenue_growth").unwrap_or(0.0);
                update_assumption(
                    &mut parameters,
                    "revenue_growth",
                    (base + shock.magnitude).clamp(-0.9, 2.0),
                    shock,
                )
            }
            "capacity" => {
                let base = p_value(&parameters, "revenue_growth").unwrap_or(0.0);
                update_assumption(
                    &mut parameters,
                    "revenue_growth",
                    (base + shock.magnitude * 0.8).clamp(-0.9, 2.0),
                    shock,
                )
            }
            "raw_material" | "energy" | "transport" => {
                let gross = p_value(&parameters, "gross_margin").unwrap_or(0.0);
                let cost_ratio = 1.0 - gross;
                let pass = shock.pass_through.unwrap_or(0.0).clamp(0.0, 1.0);
                update_assumption(
                    &mut parameters,
                    "gross_margin",
                    (gross - shock.magnitude * cost_ratio * (1.0 - pass)).clamp(-0.5, 0.95),
                    shock,
                )
            }
            "fx" => {
                let base = p_value(&parameters, "revenue_growth").unwrap_or(0.0);
                let exposure = shock.pass_through.unwrap_or(0.10).clamp(-1.0, 1.0);
                update_assumption(
                    &mut parameters,
                    "revenue_growth",
                    (base + shock.magnitude * exposure).clamp(-0.9, 2.0),
                    shock,
                )
            }
            "opex" => {
                let base = p_value(&parameters, "opex_ratio").unwrap_or(0.0);
                update_assumption(
                    &mut parameters,
                    "opex_ratio",
                    (base * (1.0 + shock.magnitude)).clamp(-0.2, 1.5),
                    shock,
                )
            }
            "working_capital" => {
                let base = p_value(&parameters, "cfo_margin").unwrap_or(0.0);
                update_assumption(
                    &mut parameters,
                    "cfo_margin",
                    (base - shock.magnitude).clamp(-1.0, 2.0),
                    shock,
                )
            }
            other => {
                warnings.push(format!("暂不支持冲击类型 {other}，未静默套用错误映射"));
                None
            }
        };
        if let Some(parameter) = updated {
            changed.push(parameter);
        }
    }
    let base = tree
        .scenarios
        .iter()
        .find(|s| s.scenario == "base")
        .cloned();
    let shocked = scenarios_from(&parameters)
        .into_iter()
        .find(|s| s.scenario == "base");
    let delta = base
        .as_ref()
        .zip(shocked.as_ref())
        .map(|(base, shocked)| BridgeDelta {
            revenue: shocked.revenue - base.revenue,
            gross_profit: shocked.gross_profit - base.gross_profit,
            operating_profit: shocked.operating_profit - base.operating_profit,
            parent_net_profit: shocked.parent_net_profit - base.parent_net_profit,
            eps: shocked.eps.zip(base.eps).map(|(a, b)| a - b),
            operating_cash_flow: shocked.operating_cash_flow - base.operating_cash_flow,
            free_cash_flow: shocked.free_cash_flow - base.free_cash_flow,
        });
    let mut hash = Sha256::new();
    hash.update(tree.snapshot_id.as_bytes());
    hash.update(serde_json::to_vec(shocks).unwrap_or_default());
    ShockBridge {
        base_snapshot_id: tree.snapshot_id.clone(),
        shocked_snapshot_id: format!("shock-{:x}", hash.finalize()),
        shocks: shocks.to_vec(),
        base,
        shocked,
        delta,
        changed_parameters: changed,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;
    use crate::model::{CompanyProfile, PeriodMeta, ValuationSnapshot};

    fn bundle(industry: &str, code: &str) -> FundamentalBundle {
        let mk_meta = |year| PeriodMeta {
            period_end: NaiveDate::from_ymd_opt(year, 12, 31).unwrap(),
            report_type: ReportType::Annual,
            announced: NaiveDate::from_ymd_opt(year + 1, 3, 30),
        };
        FundamentalBundle {
            profile: Some(CompanyProfile {
                code: code.into(),
                name: "黄金样本公司".into(),
                short_name: "黄金样本".into(),
                industry: Some(industry.into()),
                industry_csrc: None,
                total_shares: Some(100.0),
                ..Default::default()
            }),
            income: vec![
                IncomeStatement {
                    meta: Some(mk_meta(2023)),
                    total_operating_revenue: Some(1000.0),
                    operating_revenue: Some(1000.0),
                    operating_cost: Some(600.0),
                    selling_expense: Some(80.0),
                    admin_expense: Some(70.0),
                    rd_expense: Some(50.0),
                    finance_expense: Some(10.0),
                    operating_profit: Some(190.0),
                    total_profit: Some(190.0),
                    income_tax: Some(47.5),
                    net_profit: Some(142.5),
                    net_profit_parent: Some(140.0),
                    minority_profit: Some(2.5),
                    ..Default::default()
                },
                IncomeStatement {
                    meta: Some(mk_meta(2024)),
                    total_operating_revenue: Some(1100.0),
                    operating_revenue: Some(1100.0),
                    operating_cost: Some(638.0),
                    selling_expense: Some(82.0),
                    admin_expense: Some(72.0),
                    rd_expense: Some(55.0),
                    finance_expense: Some(8.0),
                    operating_profit: Some(245.0),
                    total_profit: Some(245.0),
                    income_tax: Some(61.25),
                    net_profit: Some(183.75),
                    net_profit_parent: Some(180.0),
                    minority_profit: Some(3.75),
                    ..Default::default()
                },
            ],
            balance: vec![BalanceSheet {
                meta: Some(mk_meta(2024)),
                monetary_funds: Some(300.0),
                inventory: Some(100.0),
                accounts_receivable: Some(90.0),
                accounts_payable: Some(70.0),
                share_capital: Some(100.0),
                ..Default::default()
            }],
            cashflow: vec![CashFlowStatement {
                meta: Some(mk_meta(2024)),
                net_cfo: Some(220.0),
                capex: Some(60.0),
                depreciation: Some(35.0),
                ..Default::default()
            }],
            snapshot: Some(ValuationSnapshot {
                price: 20.0,
                total_shares: Some(100.0),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn six_manually_reviewed_golden_templates_use_sector_specific_formulas() {
        for (code, industry, expected) in [
            ("600036", "银行", IndustryTemplate::Financial),
            ("600048", "房地产开发", IndustryTemplate::RealEstate),
            ("601899", "有色金属矿业", IndustryTemplate::Resource),
            ("300308", "电子制造", IndustryTemplate::Manufacturing),
            ("600519", "白酒", IndustryTemplate::Consumer),
            ("600588", "软件服务", IndustryTemplate::Software),
        ] {
            let tree = build_earnings_driver_tree(code, &bundle(industry, code), 1_800_000_000);
            assert_eq!(tree.industry_template, expected);
            assert!(tree.golden_template_reviewed);
            assert!(!tree.revenue_formula.is_empty());
            assert!(!tree.cost_formula.is_empty());
            assert!(!tree.revenue_tree.children.is_empty());
            assert!(!tree.cost_tree.children.is_empty());
            assert!(tree
                .revenue_tree
                .children
                .iter()
                .all(|branch| branch.status == "missing_disclosure"));
        }
    }

    #[test]
    fn every_forecast_line_has_formula_parameters_and_evidence_is_traceable() {
        let tree =
            build_earnings_driver_tree("300308", &bundle("电子制造", "300308"), 1_800_000_000);
        assert!(tree
            .formula_nodes
            .iter()
            .all(|node| !node.formula.is_empty() && !node.parameter_ids.is_empty()));
        let revenue = parameter(&tree.parameters, "base_revenue").unwrap();
        assert_eq!(revenue.origin, ValueOrigin::HistoricalFact);
        assert_eq!(
            revenue.evidence[0].report_period.as_deref(),
            Some("2024-12-31")
        );
        assert_eq!(revenue.evidence[0].unit, "CNY");
        assert!(tree
            .parameters
            .iter()
            .any(|p| p.origin == ValueOrigin::IndustryPrior
                || p.origin == ValueOrigin::HistoricalFact));
    }

    #[test]
    fn missing_segment_data_never_claims_exact_eps_but_returns_a_range() {
        let tree =
            build_earnings_driver_tree("300308", &bundle("电子制造", "300308"), 1_800_000_000);
        assert!(!tree.quality.exact_eps_available);
        assert!(tree
            .quality
            .missing_core_drivers
            .contains(&"分产品销量".to_string()));
        let eps: Vec<f64> = tree.scenarios.iter().filter_map(|s| s.eps).collect();
        assert_eq!(eps.len(), 3);
        assert!(eps[0] < eps[2]);
    }

    #[test]
    fn missing_revenue_refuses_forecast_instead_of_fabricating() {
        let mut sample = bundle("软件服务", "600588");
        for row in &mut sample.income {
            row.total_operating_revenue = None;
            row.operating_revenue = None;
        }
        let tree = build_earnings_driver_tree("600588", &sample, 1_800_000_000);
        assert!(tree.scenarios.is_empty());
        assert!(tree
            .quality
            .refusal_reason
            .as_deref()
            .unwrap()
            .contains("营业收入"));
    }

    #[test]
    fn valuation_and_driver_tree_share_the_same_parameter_snapshot() {
        let sample = bundle("白酒", "600519");
        let tree = build_earnings_driver_tree("600519", &sample, 1_800_000_000);
        assert_eq!(
            tree.parameter_snapshot_id,
            parameter_snapshot_id("600519", &sample)
        );
    }

    #[test]
    fn shock_changes_operating_parameters_and_builds_financial_bridge() {
        let tree =
            build_earnings_driver_tree("300308", &bundle("电子制造", "300308"), 1_800_000_000);
        let shock = DriverShock {
            kind: "raw_material".into(),
            magnitude: 0.20,
            lag_months: 3,
            pass_through: Some(0.25),
            evidence_version_id: Some("doc-v1".into()),
            note: Some("铜价上涨".into()),
        };
        let bridge = apply_driver_shocks(&tree, &[shock]);
        assert!(bridge.delta.as_ref().unwrap().gross_profit < 0.0);
        assert_eq!(
            bridge.changed_parameters[0].origin,
            ValueOrigin::AgentAssumption
        );
        assert_eq!(
            bridge.changed_parameters[0].evidence[0].source_version_id,
            "doc-v1"
        );
    }

    #[test]
    fn monte_carlo_is_deterministic_for_same_snapshot() {
        let a = build_earnings_driver_tree("600519", &bundle("白酒", "600519"), 1_800_000_000);
        let b = build_earnings_driver_tree("600519", &bundle("白酒", "600519"), 1_800_000_000);
        assert_eq!(a.monte_carlo, b.monte_carlo);
        assert_eq!(a.snapshot_id, b.snapshot_id);
        let later = build_earnings_driver_tree("600519", &bundle("白酒", "600519"), 1_800_000_001);
        assert_ne!(a.snapshot_id, later.snapshot_id);
        assert_eq!(a.parameter_snapshot_id, later.parameter_snapshot_id);
    }
}
