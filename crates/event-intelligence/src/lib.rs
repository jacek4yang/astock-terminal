//! Evidence-bound event research and deterministic market price-in analysis.
//!
//! Source facts, company guidance, market consensus, Agent assumptions and
//! scenarios are different provenance classes. Missing source fields remain
//! missing. A positive fundamental impact is never itself a buy instruction:
//! the market-opportunity conclusion is computed separately from observable
//! price, volume, relative performance, valuation and historical analogues.

use std::collections::BTreeSet;

use astock_storage::Storage;
use chrono::NaiveDate;
use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const EVENT_EXTRACTION_VERSION: &str = "evidence-event-v1";
pub const PRICE_IN_MODEL_VERSION: &str = "deterministic-price-in-v1";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Storage(#[from] astock_storage::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("事件输入无效：{0}")]
    Invalid(String),
    #[error("事件状态迁移无效：{0}")]
    InvalidTransition(String),
}

pub type Result<T> = std::result::Result<T, Error>;

macro_rules! string_enum {
    ($name:ident { $($variant:ident => ($token:literal, $zh:literal)),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub fn token(self) -> &'static str {
                match self { $(Self::$variant => $token),+ }
            }
            pub fn chinese_name(self) -> &'static str {
                match self { $(Self::$variant => $zh),+ }
            }
            pub fn parse(value: &str) -> Option<Self> {
                match value { $($token => Some(Self::$variant)),+, _ => None }
            }
        }
    };
}

string_enum!(EventKind {
    Earnings => ("earnings", "业绩"),
    Guidance => ("guidance", "经营指引"),
    Order => ("order", "订单/中标"),
    PriceIncrease => ("price_increase", "涨价"),
    Shutdown => ("shutdown", "停产"),
    Capacity => ("capacity", "产能"),
    Policy => ("policy", "政策"),
    Sanction => ("sanction", "制裁"),
    Tariff => ("tariff", "关税"),
    Accident => ("accident", "事故"),
    MergerAcquisition => ("merger_acquisition", "并购重组"),
    Repurchase => ("repurchase", "回购"),
    ShareReduction => ("share_reduction", "减持"),
    Unlock => ("unlock", "解禁"),
    Litigation => ("litigation", "诉讼"),
    TechnologyBreakthrough => ("technology_breakthrough", "技术突破"),
    MacroRelease => ("macro_release", "宏观发布"),
    Other => ("other", "其他事件")
});

string_enum!(EvidenceProvenance {
    ObservedFact => ("observed_fact", "已观察事实"),
    CompanyGuidance => ("company_guidance", "公司指引"),
    MarketConsensus => ("market_consensus", "市场一致预期"),
    AgentAssumption => ("agent_assumption", "分析假设"),
    Scenario => ("scenario", "情景推演")
});

string_enum!(EventLifecycle {
    Rumor => ("rumor", "传闻"),
    Unverified => ("unverified", "待核验"),
    Confirmed => ("confirmed", "已确认"),
    Effective => ("effective", "已生效"),
    Completed => ("completed", "已完成"),
    Cancelled => ("cancelled", "已取消"),
    Revised => ("revised", "已修订")
});

impl EventLifecycle {
    pub fn can_transition_to(self, next: Self) -> bool {
        use EventLifecycle::*;
        matches!(
            (self, next),
            (Rumor, Unverified | Confirmed | Cancelled | Revised)
                | (Unverified, Confirmed | Cancelled | Revised)
                | (Confirmed, Effective | Completed | Cancelled | Revised)
                | (Effective, Completed | Cancelled | Revised)
                | (
                    Revised,
                    Unverified | Confirmed | Effective | Completed | Cancelled
                )
        ) || self == next
    }
}

string_enum!(ImpactHorizon {
    Intraday => ("intraday", "日内"),
    Days => ("days", "数日"),
    Quarter => ("quarter", "季度"),
    Year => ("year", "年度")
});

string_enum!(Reversibility {
    Reversible => ("reversible", "可逆"),
    Conditional => ("conditional", "条件可逆"),
    Irreversible => ("irreversible", "不可逆"),
    Unknown => ("unknown", "未知")
});

string_enum!(ImpactDirection {
    Positive => ("positive", "正向"),
    Negative => ("negative", "负向"),
    Neutral => ("neutral", "中性"),
    Unknown => ("unknown", "无法判断")
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEntityRef {
    pub entity_id: String,
    pub name: String,
    pub listed_code: Option<String>,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventFieldEvidence {
    pub evidence_id: String,
    pub event_id: String,
    pub field_name: String,
    pub provenance: EvidenceProvenance,
    pub source_revision_id: Option<String>,
    pub source_version_id: Option<String>,
    pub quote_original: Option<String>,
    pub quote_zh: Option<String>,
    pub location: serde_json::Value,
    pub observed_at: i64,
    pub confidence_bps: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredEvent {
    pub event_id: String,
    pub source_revision_id: String,
    pub kind: EventKind,
    pub title: String,
    pub subjects: Vec<EventEntityRef>,
    pub objects: Vec<EventEntityRef>,
    pub amount_text: Option<String>,
    pub quantity_text: Option<String>,
    pub unit_original: Option<String>,
    pub currency_original: Option<String>,
    pub baseline_period: Option<String>,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
    pub region: Option<String>,
    pub conditions: Vec<String>,
    pub official_effective: Option<bool>,
    pub reversibility: Reversibility,
    pub impact_horizon: ImpactHorizon,
    pub lifecycle: EventLifecycle,
    pub catalyst_path: Vec<String>,
    pub validation_dates: Vec<i64>,
    pub invalidation_conditions: Vec<String>,
    pub missing_fields: Vec<String>,
    pub evidence: Vec<EventFieldEvidence>,
    pub extraction_version: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventExtractionInput {
    pub source_revision_id: String,
    pub source_version_id: Option<String>,
    pub title: String,
    pub factual_summary: String,
    pub original_language: String,
    pub source_is_primary: bool,
    pub event_time_utc: Option<i64>,
    pub first_seen_at: i64,
    pub subjects: Vec<EventEntityRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventStateTransition {
    pub transition_id: String,
    pub event_id: String,
    pub from_status: EventLifecycle,
    pub to_status: EventLifecycle,
    pub reason: String,
    pub evidence_id: Option<String>,
    pub transitioned_at: i64,
}

static AMOUNT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?P<value>\d+(?:\.\d+)?)\s*(?P<unit>万亿元|亿元|万元|亿美元|万美元|美元|人民币|元)",
    )
    .expect("amount regex")
});
static QUANTITY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?P<value>\d+(?:\.\d+)?)\s*(?P<unit>亿股|万股|股|万吨|吨|万辆|辆|万台|台|套|个|项|GWh|GW|MW)")
        .expect("quantity regex")
});
static DATE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?P<y>20\d{2})[-年/.](?P<m>\d{1,2})[-月/.](?P<d>\d{1,2})日?").expect("date regex")
});
static BASELINE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(20\d{2}年(?:第[一二三四1-4]季度|上半年|前三季度|年度)?|去年同期|上年同期|环比|同比)",
    )
    .expect("baseline regex")
});

fn classify_kind(text: &str) -> EventKind {
    let rules: &[(EventKind, &[&str])] = &[
        (EventKind::Earnings, &["业绩", "财报", "营业收入", "净利润"]),
        (EventKind::Order, &["中标", "订单", "签订合同", "框架协议"]),
        (EventKind::PriceIncrease, &["涨价", "提价", "上调价格"]),
        (EventKind::Shutdown, &["停产", "停工", "暂停生产"]),
        (EventKind::Capacity, &["产能", "扩产", "投产", "产线"]),
        (EventKind::Sanction, &["制裁", "实体清单", "出口管制"]),
        (EventKind::Tariff, &["关税", "反倾销", "反补贴"]),
        (EventKind::Accident, &["事故", "爆炸", "火灾", "泄漏"]),
        (
            EventKind::MergerAcquisition,
            &["并购", "收购", "重组", "吸收合并"],
        ),
        (EventKind::Repurchase, &["回购"]),
        (EventKind::ShareReduction, &["减持"]),
        (EventKind::Unlock, &["解禁", "限售股上市流通"]),
        (EventKind::Litigation, &["诉讼", "仲裁", "起诉"]),
        (
            EventKind::TechnologyBreakthrough,
            &["技术突破", "首发", "首款", "研发成功"],
        ),
        (
            EventKind::MacroRelease,
            &["CPI", "PPI", "GDP", "非农", "PMI", "利率决议"],
        ),
        (
            EventKind::Policy,
            &["政策", "条例", "办法", "监管", "国务院", "证监会"],
        ),
        (EventKind::Guidance, &["指引", "预计", "展望", "业绩预告"]),
    ];
    rules
        .iter()
        .find(|(_, words)| words.iter().any(|word| text.contains(word)))
        .map(|(kind, _)| *kind)
        .unwrap_or(EventKind::Other)
}

fn classify_lifecycle(text: &str, primary: bool) -> EventLifecycle {
    if ["取消", "终止", "撤回", "作废"]
        .iter()
        .any(|word| text.contains(word))
    {
        EventLifecycle::Cancelled
    } else if ["更正", "修订", "调整"]
        .iter()
        .any(|word| text.contains(word))
    {
        EventLifecycle::Revised
    } else if ["完成", "已实施完毕", "履行完毕"]
        .iter()
        .any(|word| text.contains(word))
    {
        EventLifecycle::Completed
    } else if ["生效", "正式实施", "已投产", "已执行"]
        .iter()
        .any(|word| text.contains(word))
    {
        EventLifecycle::Effective
    } else if ["传闻", "网传", "据悉", "市场消息"]
        .iter()
        .any(|word| text.contains(word))
    {
        EventLifecycle::Rumor
    } else if primary {
        EventLifecycle::Confirmed
    } else {
        EventLifecycle::Unverified
    }
}

fn analytical_defaults(
    kind: EventKind,
) -> (Reversibility, ImpactHorizon, Vec<String>, Vec<String>) {
    use EventKind::*;
    match kind {
        Earnings | Guidance => (
            Reversibility::Conditional,
            ImpactHorizon::Quarter,
            vec![
                "核对正式财报/指引".into(),
                "跟踪下一报告期兑现".into(),
                "比较市场一致预期修订".into(),
            ],
            vec![
                "后续正式财报未兑现".into(),
                "一致预期已先行上调并充分交易".into(),
            ],
        ),
        Order => (
            Reversibility::Conditional,
            ImpactHorizon::Year,
            vec![
                "核对合同生效条件".into(),
                "跟踪收入确认与回款".into(),
                "复核毛利率影响".into(),
            ],
            vec!["合同取消或延期".into(), "订单金额不构成可确认收入".into()],
        ),
        PriceIncrease | Shutdown | Accident => (
            Reversibility::Reversible,
            ImpactHorizon::Days,
            vec![
                "确认事件已生效".into(),
                "观察供需/产量变化".into(),
                "核对价格和库存传导".into(),
            ],
            vec!["供给快速恢复".into(), "价格未持续或下游拒绝传导".into()],
        ),
        Capacity | TechnologyBreakthrough => (
            Reversibility::Conditional,
            ImpactHorizon::Year,
            vec![
                "确认技术/产线验收".into(),
                "跟踪客户导入".into(),
                "核对量产、良率与收入".into(),
            ],
            vec![
                "无法量产或客户验证失败".into(),
                "资本开支回报低于资金成本".into(),
            ],
        ),
        Policy | Sanction | Tariff | MacroRelease => (
            Reversibility::Conditional,
            ImpactHorizon::Quarter,
            vec![
                "核对正式政策原文".into(),
                "确认实施日期与豁免".into(),
                "跟踪经营数据和政策修订".into(),
            ],
            vec![
                "政策延后、撤销或存在关键豁免".into(),
                "市场预期已提前反映".into(),
            ],
        ),
        MergerAcquisition => (
            Reversibility::Conditional,
            ImpactHorizon::Year,
            vec![
                "跟踪董事会/股东会/监管审批".into(),
                "核对交易对价与融资".into(),
                "验证并表协同".into(),
            ],
            vec!["审批失败或交易终止".into(), "商誉及整合成本超过协同".into()],
        ),
        Repurchase | ShareReduction | Unlock | Litigation => (
            Reversibility::Conditional,
            ImpactHorizon::Days,
            vec![
                "确认计划与法定程序".into(),
                "跟踪实际执行数量".into(),
                "比较成交量与估值变化".into(),
            ],
            vec![
                "计划未执行或规模显著低于披露".into(),
                "市场已充分交易".into(),
            ],
        ),
        Other => (
            Reversibility::Unknown,
            ImpactHorizon::Days,
            vec!["补充一级来源事实".into()],
            vec!["关键事实无法核验".into()],
        ),
    }
}

fn direction_hint(kind: EventKind, text: &str) -> ImpactDirection {
    if [
        "下降", "下滑", "亏损", "减持", "事故", "停产", "制裁", "诉讼", "处罚", "取消",
    ]
    .iter()
    .any(|word| text.contains(word))
    {
        return ImpactDirection::Negative;
    }
    if [
        "增长", "上调", "中标", "回购", "突破", "投产", "涨价", "盈利",
    ]
    .iter()
    .any(|word| text.contains(word))
    {
        return ImpactDirection::Positive;
    }
    match kind {
        EventKind::Repurchase | EventKind::Order | EventKind::TechnologyBreakthrough => {
            ImpactDirection::Positive
        }
        EventKind::ShareReduction
        | EventKind::Unlock
        | EventKind::Accident
        | EventKind::Litigation => ImpactDirection::Negative,
        _ => ImpactDirection::Unknown,
    }
}

fn evidence(
    event_id: &str,
    field: &str,
    provenance: EvidenceProvenance,
    input: &EventExtractionInput,
    quote: Option<String>,
    confidence_bps: u16,
) -> EventFieldEvidence {
    let revision = matches!(
        provenance,
        EvidenceProvenance::ObservedFact | EvidenceProvenance::CompanyGuidance
    )
    .then(|| input.source_revision_id.clone());
    let seed = format!(
        "{event_id}|{field}|{}|{}",
        provenance.token(),
        quote.as_deref().unwrap_or("")
    );
    EventFieldEvidence {
        evidence_id: format!("eev:{}", short_hash(&seed)),
        event_id: event_id.into(),
        field_name: field.into(),
        provenance,
        source_revision_id: revision,
        source_version_id: input.source_version_id.clone(),
        quote_original: quote,
        quote_zh: None,
        location: serde_json::json!({"revision_id": input.source_revision_id, "field": field}),
        observed_at: input.first_seen_at,
        confidence_bps,
    }
}

pub fn extract_structured_event(input: EventExtractionInput) -> Result<StructuredEvent> {
    if input.source_revision_id.trim().is_empty() || input.title.trim().is_empty() {
        return Err(Error::Invalid("来源修订和标题不能为空".into()));
    }
    let combined = format!("{}。{}", input.title, input.factual_summary);
    let event_id = format!("sevt:{}", short_hash(&input.source_revision_id));
    let kind = classify_kind(&combined);
    let lifecycle = classify_lifecycle(&combined, input.source_is_primary);
    let (reversibility, impact_horizon, catalyst_path, invalidation_conditions) =
        analytical_defaults(kind);
    let amount_match = AMOUNT.find(&combined).map(|m| m.as_str().to_string());
    let quantity_match = QUANTITY.find(&combined).map(|m| m.as_str().to_string());
    let amount_caps = AMOUNT.captures(&combined);
    let quantity_caps = QUANTITY.captures(&combined);
    let unit_original = amount_caps
        .as_ref()
        .and_then(|caps| caps.name("unit"))
        .or_else(|| quantity_caps.as_ref().and_then(|caps| caps.name("unit")))
        .map(|value| value.as_str().to_string());
    let currency_original = unit_original.as_deref().and_then(|unit| {
        if unit.contains("美元") {
            Some("USD".into())
        } else if unit.contains('元') || unit.contains("人民币") {
            Some("CNY".into())
        } else {
            None
        }
    });
    let baseline_period = BASELINE.find(&combined).map(|m| m.as_str().to_string());
    let mut dates = DATE
        .captures_iter(&combined)
        .filter_map(|caps| {
            let y = caps.name("y")?.as_str().parse().ok()?;
            let m = caps.name("m")?.as_str().parse().ok()?;
            let d = caps.name("d")?.as_str().parse().ok()?;
            NaiveDate::from_ymd_opt(y, m, d)?
                .and_hms_opt(0, 0, 0)
                .map(|v| v.and_utc().timestamp())
        })
        .collect::<Vec<_>>();
    dates.sort_unstable();
    dates.dedup();
    let starts_at = dates.first().copied();
    let ends_at = dates.get(1).copied();
    let region = ["中国", "美国", "欧盟", "日本", "韩国", "东南亚", "全球"]
        .into_iter()
        .find(|region| combined.contains(region))
        .map(str::to_string);
    let conditions = combined
        .split(['。', '；', ';'])
        .filter(|part| {
            ["若", "如", "条件", "取决于", "前提"]
                .iter()
                .any(|word| part.contains(word))
        })
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .take(8)
        .collect::<Vec<_>>();
    let official_effective = if ["生效", "正式实施", "已执行", "已投产"]
        .iter()
        .any(|w| combined.contains(w))
    {
        Some(input.source_is_primary)
    } else if ["拟", "计划", "草案", "征求意见"]
        .iter()
        .any(|w| combined.contains(w))
    {
        Some(false)
    } else {
        None
    };

    let mut output = StructuredEvent {
        event_id: event_id.clone(),
        source_revision_id: input.source_revision_id.clone(),
        kind,
        title: input.title.clone(),
        subjects: input.subjects.clone(),
        objects: Vec::new(),
        amount_text: amount_match,
        quantity_text: quantity_match,
        unit_original,
        currency_original,
        baseline_period,
        starts_at,
        ends_at,
        region,
        conditions,
        official_effective,
        reversibility,
        impact_horizon,
        lifecycle,
        catalyst_path,
        validation_dates: dates,
        invalidation_conditions,
        missing_fields: Vec::new(),
        evidence: Vec::new(),
        extraction_version: EVENT_EXTRACTION_VERSION.into(),
        created_at: input.first_seen_at,
        updated_at: input.first_seen_at,
    };

    let fact_provenance = if kind == EventKind::Guidance {
        EvidenceProvenance::CompanyGuidance
    } else {
        EvidenceProvenance::ObservedFact
    };
    output.evidence.push(evidence(
        &event_id,
        "title",
        fact_provenance,
        &input,
        Some(input.title.clone()),
        10_000,
    ));
    output.evidence.push(evidence(
        &event_id,
        "kind",
        EvidenceProvenance::AgentAssumption,
        &input,
        Some(format!(
            "规则 {EVENT_EXTRACTION_VERSION} 根据关键词分类为 {}",
            kind.chinese_name()
        )),
        7_000,
    ));
    output.evidence.push(evidence(
        &event_id,
        "lifecycle",
        fact_provenance,
        &input,
        Some(input.title.clone()),
        if input.source_is_primary {
            9_500
        } else {
            5_000
        },
    ));
    for (field, present, quote) in [
        (
            "subjects",
            !output.subjects.is_empty(),
            Some(input.title.clone()),
        ),
        (
            "amount_text",
            output.amount_text.is_some(),
            output.amount_text.clone(),
        ),
        (
            "quantity_text",
            output.quantity_text.is_some(),
            output.quantity_text.clone(),
        ),
        (
            "unit_original",
            output.unit_original.is_some(),
            output.unit_original.clone(),
        ),
        (
            "currency_original",
            output.currency_original.is_some(),
            output.amount_text.clone(),
        ),
        (
            "baseline_period",
            output.baseline_period.is_some(),
            output.baseline_period.clone(),
        ),
        (
            "starts_at",
            output.starts_at.is_some(),
            DATE.find(&combined).map(|m| m.as_str().into()),
        ),
        (
            "ends_at",
            output.ends_at.is_some(),
            DATE.find_iter(&combined).nth(1).map(|m| m.as_str().into()),
        ),
        ("region", output.region.is_some(), output.region.clone()),
        (
            "conditions",
            !output.conditions.is_empty(),
            (!output.conditions.is_empty()).then(|| output.conditions.join("；")),
        ),
        (
            "official_effective",
            output.official_effective.is_some(),
            Some(input.title.clone()),
        ),
    ] {
        if present {
            output.evidence.push(evidence(
                &event_id,
                field,
                fact_provenance,
                &input,
                quote,
                if input.source_is_primary {
                    9_000
                } else {
                    5_000
                },
            ));
        }
    }
    for (field, quote, provenance) in [
        (
            "reversibility",
            format!(
                "{}类事件默认评估为{}",
                kind.chinese_name(),
                reversibility.chinese_name()
            ),
            EvidenceProvenance::AgentAssumption,
        ),
        (
            "impact_horizon",
            format!(
                "{}类事件默认影响期限为{}",
                kind.chinese_name(),
                impact_horizon.chinese_name()
            ),
            EvidenceProvenance::AgentAssumption,
        ),
        (
            "catalyst_path",
            output.catalyst_path.join(" → "),
            EvidenceProvenance::Scenario,
        ),
        (
            "invalidation_conditions",
            output.invalidation_conditions.join("；"),
            EvidenceProvenance::Scenario,
        ),
    ] {
        output.evidence.push(evidence(
            &event_id,
            field,
            provenance,
            &input,
            Some(quote),
            6_000,
        ));
    }
    output.missing_fields = missing_fields(&output);
    validate_evidence_coverage(&output)?;
    Ok(output)
}

fn missing_fields(event: &StructuredEvent) -> Vec<String> {
    let mut missing = Vec::new();
    for (name, absent) in [
        ("对象", event.objects.is_empty()),
        ("金额", event.amount_text.is_none()),
        ("数量", event.quantity_text.is_none()),
        ("单位", event.unit_original.is_none()),
        ("基准期", event.baseline_period.is_none()),
        ("开始时间", event.starts_at.is_none()),
        ("结束时间", event.ends_at.is_none()),
        ("地域", event.region.is_none()),
        ("生效状态", event.official_effective.is_none()),
    ] {
        if absent {
            missing.push(name.into());
        }
    }
    missing
}

pub fn validate_evidence_coverage(event: &StructuredEvent) -> Result<()> {
    let evidence_fields = event
        .evidence
        .iter()
        .map(|row| row.field_name.as_str())
        .collect::<BTreeSet<_>>();
    let required = [
        ("title", true),
        ("kind", true),
        ("lifecycle", true),
        ("subjects", !event.subjects.is_empty()),
        ("amount_text", event.amount_text.is_some()),
        ("quantity_text", event.quantity_text.is_some()),
        ("unit_original", event.unit_original.is_some()),
        ("currency_original", event.currency_original.is_some()),
        ("baseline_period", event.baseline_period.is_some()),
        ("starts_at", event.starts_at.is_some()),
        ("ends_at", event.ends_at.is_some()),
        ("region", event.region.is_some()),
        ("conditions", !event.conditions.is_empty()),
        ("official_effective", event.official_effective.is_some()),
        ("reversibility", true),
        ("impact_horizon", true),
        ("catalyst_path", true),
        ("invalidation_conditions", true),
    ];
    if let Some((field, _)) = required
        .into_iter()
        .find(|(field, present)| *present && !evidence_fields.contains(field))
    {
        return Err(Error::Invalid(format!("字段 {field} 缺少逐项证据")));
    }
    if event.evidence.iter().any(|row| {
        matches!(
            row.provenance,
            EvidenceProvenance::ObservedFact | EvidenceProvenance::CompanyGuidance
        ) && (row
            .source_revision_id
            .as_deref()
            .unwrap_or_default()
            .is_empty()
            || row.quote_original.as_deref().unwrap_or_default().is_empty())
    }) {
        return Err(Error::Invalid(
            "事实/公司指引证据必须包含来源修订和原文摘录".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceSeriesPoint {
    pub date: String,
    pub close: f64,
    pub volume: f64,
    pub pe_ttm: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoricalAnalog {
    pub sample_id: String,
    pub post_abnormal_return_bps: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceInInput {
    pub event: StructuredEvent,
    pub security_code: String,
    pub event_date: String,
    pub as_of_date: String,
    pub stock: Vec<PriceSeriesPoint>,
    pub benchmark: Vec<PriceSeriesPoint>,
    pub sector: Vec<PriceSeriesPoint>,
    pub valuation: Vec<PriceSeriesPoint>,
    pub structured_impact_bps: Option<i64>,
    pub consensus_impact_bps: Option<i64>,
    pub historical_analogs: Vec<HistoricalAnalog>,
    pub data_versions: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FundamentalConclusion {
    pub direction: ImpactDirection,
    pub impact_bps: Option<i64>,
    pub quantifiable: bool,
    pub rationale: String,
    pub provenance: EvidenceProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricContribution {
    pub metric: String,
    pub available: bool,
    pub value_bps: Option<i64>,
    pub score_contribution: i64,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceInDiagnostics {
    pub pre_stock_return_bps: Option<i64>,
    pub pre_benchmark_return_bps: Option<i64>,
    pub pre_abnormal_return_bps: Option<i64>,
    pub sector_relative_bps: Option<i64>,
    pub abnormal_volume_bps: Option<i64>,
    pub valuation_change_bps: Option<i64>,
    pub historical_median_post_bps: Option<i64>,
    pub historical_sample_count: usize,
    pub components: Vec<MetricContribution>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketOpportunityConclusion {
    pub price_in_state: String,
    pub opportunity: String,
    pub price_in_score: Option<i64>,
    pub rationale: String,
    pub no_trade_directive: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectationGap {
    pub structured_impact_bps: Option<i64>,
    pub consensus_impact_bps: Option<i64>,
    pub gap_bps: Option<i64>,
    pub quantifiable: bool,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventMarketAssessment {
    pub assessment_id: String,
    pub event_id: String,
    pub security_code: String,
    pub as_of_date: String,
    pub fundamental: FundamentalConclusion,
    pub market_opportunity: MarketOpportunityConclusion,
    pub expectation_gap: ExpectationGap,
    pub diagnostics: PriceInDiagnostics,
    pub missing_inputs: Vec<String>,
    pub data_versions: serde_json::Value,
    pub created_at: i64,
}

fn return_bps(points: &[PriceSeriesPoint], event_date: &str, window: usize) -> Option<i64> {
    if points.len() < 2 {
        return None;
    }
    let end = points
        .iter()
        .position(|p| p.date.as_str() >= event_date)
        .unwrap_or(points.len())
        .saturating_sub(1);
    if end == 0 {
        return None;
    }
    let start = end.saturating_sub(window);
    let a = points.get(start)?.close;
    let b = points.get(end)?.close;
    (a.is_finite() && b.is_finite() && a > 0.0)
        .then_some((((b / a) - 1.0) * 10_000.0).round() as i64)
}

fn abnormal_volume_bps(points: &[PriceSeriesPoint], event_date: &str) -> Option<i64> {
    let end = points
        .iter()
        .position(|p| p.date.as_str() >= event_date)
        .unwrap_or(points.len())
        .saturating_sub(1);
    if end < 3 {
        return None;
    }
    let start = end.saturating_sub(20);
    let history = &points[start..end];
    let values = history
        .iter()
        .map(|p| p.volume)
        .filter(|v| v.is_finite() && *v >= 0.0)
        .collect::<Vec<_>>();
    if values.is_empty() || points[end].volume <= 0.0 {
        return None;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    (mean > 0.0).then_some((((points[end].volume / mean) - 1.0) * 10_000.0).round() as i64)
}

fn valuation_change_bps(points: &[PriceSeriesPoint], event_date: &str) -> Option<i64> {
    let values = points
        .iter()
        .filter(|p| p.date.as_str() <= event_date)
        .filter_map(|p| p.pe_ttm.filter(|v| v.is_finite() && *v > 0.0))
        .collect::<Vec<_>>();
    if values.len() < 2 {
        return None;
    }
    let end = *values.last()?;
    let start = values[values.len().saturating_sub(6)];
    Some((((end / start) - 1.0) * 10_000.0).round() as i64)
}

fn median(values: &mut [i64]) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2
    } else {
        values[mid]
    })
}

pub fn analyze_price_in(input: PriceInInput) -> Result<EventMarketAssessment> {
    if input.security_code.trim().is_empty()
        || NaiveDate::parse_from_str(&input.event_date, "%Y-%m-%d").is_err()
    {
        return Err(Error::Invalid(
            "price-in 必须提供证券代码和 YYYY-MM-DD 事件日".into(),
        ));
    }
    let text = format!("{} {}", input.event.title, input.event.kind.chinese_name());
    let inferred_direction = direction_hint(input.event.kind, &text);
    let fundamental = FundamentalConclusion {
        direction: input
            .structured_impact_bps
            .map(|v| {
                if v > 0 {
                    ImpactDirection::Positive
                } else if v < 0 {
                    ImpactDirection::Negative
                } else {
                    ImpactDirection::Neutral
                }
            })
            .unwrap_or(inferred_direction),
        impact_bps: input.structured_impact_bps,
        quantifiable: input.structured_impact_bps.is_some(),
        rationale: if let Some(value) = input.structured_impact_bps {
            format!("结构化经营影响为 {value} 个基点；这是经营影响，不等同于股价机会。")
        } else {
            format!(
                "未取得可核验的经营影响量化值；仅按事件本体给出{}方向假设。",
                inferred_direction.chinese_name()
            )
        },
        provenance: if input.structured_impact_bps.is_some() {
            EvidenceProvenance::ObservedFact
        } else {
            EvidenceProvenance::AgentAssumption
        },
    };
    let stock_return = return_bps(&input.stock, &input.event_date, 5);
    let benchmark_return = return_bps(&input.benchmark, &input.event_date, 5);
    let abnormal = stock_return.zip(benchmark_return).map(|(a, b)| a - b);
    let sector_return = return_bps(&input.sector, &input.event_date, 5);
    let sector_relative = stock_return.zip(sector_return).map(|(a, b)| a - b);
    let volume = abnormal_volume_bps(&input.stock, &input.event_date);
    let valuation = valuation_change_bps(&input.valuation, &input.event_date);
    let mut analog_values = input
        .historical_analogs
        .iter()
        .map(|v| v.post_abnormal_return_bps)
        .collect::<Vec<_>>();
    let analog_median = median(&mut analog_values);
    let gap = input
        .structured_impact_bps
        .zip(input.consensus_impact_bps)
        .map(|(a, b)| a - b);
    let expectation_gap = ExpectationGap {
        structured_impact_bps: input.structured_impact_bps,
        consensus_impact_bps: input.consensus_impact_bps,
        gap_bps: gap,
        quantifiable: gap.is_some(),
        rationale: gap
            .map(|v| format!("结构化经营影响减去市场一致预期为 {v} 个基点。"))
            .unwrap_or_else(|| "经营影响或市场一致预期缺失，预期差不可量化，禁止补造。".into()),
    };
    let sign = match fundamental.direction {
        ImpactDirection::Positive => 1,
        ImpactDirection::Negative => -1,
        _ => 0,
    };
    let mut score = 35_i64;
    let mut components = Vec::new();
    let mut push = |metric: &str, value: Option<i64>, contribution: i64, explanation: String| {
        if value.is_some() {
            score += contribution;
        }
        components.push(MetricContribution {
            metric: metric.into(),
            available: value.is_some(),
            value_bps: value,
            score_contribution: if value.is_some() { contribution } else { 0 },
            explanation,
        });
    };
    let abnormal_contribution = abnormal
        .map(|v| (v * sign / 25).clamp(-20, 35))
        .unwrap_or(0);
    push(
        "事件前异常收益",
        abnormal,
        abnormal_contribution,
        "个股事件前五日收益减市场基准；正向事件提前上涨会提高已交易程度。".into(),
    );
    let volume_contribution = volume.map(|v| (v / 1_000).clamp(-5, 15)).unwrap_or(0);
    push(
        "异常成交量",
        volume,
        volume_contribution,
        "事件前最近交易日成交量相对过去二十日均值。".into(),
    );
    let sector_contribution = sector_relative
        .map(|v| (v * sign / 40).clamp(-10, 15))
        .unwrap_or(0);
    push(
        "板块相对表现",
        sector_relative,
        sector_contribution,
        "个股相对同板块组合的事件前表现。".into(),
    );
    let valuation_contribution = valuation
        .map(|v| (v * sign / 50).clamp(-10, 15))
        .unwrap_or(0);
    push(
        "估值变化",
        valuation,
        valuation_contribution,
        "事件前 PE(TTM) 的五期变化；数据缺失时不使用价格代替。".into(),
    );
    let analog_contribution = analog_median
        .map(|v| {
            abnormal
                .map(|a| ((a - v) * sign / 50).clamp(-10, 10))
                .unwrap_or(0)
        })
        .unwrap_or(0);
    push(
        "历史同类事件",
        analog_median,
        analog_contribution,
        format!(
            "{} 个同类事件的事后异常收益中位数。",
            input.historical_analogs.len()
        ),
    );
    let gap_contribution = gap.map(|v| (-v * sign / 50).clamp(-15, 15)).unwrap_or(0);
    push(
        "结构化预期差",
        gap,
        gap_contribution,
        "经营影响高于一致预期会降低“已交易”评分；反之提高。".into(),
    );

    let available = components.iter().filter(|row| row.available).count();
    let mut missing_inputs = Vec::new();
    for (name, value) in [
        ("市场基准收益", benchmark_return),
        ("板块历史序列", sector_relative),
        ("事件前成交量", volume),
        ("历史估值序列", valuation),
        ("历史同类事件校准", analog_median),
        ("结构化经营影响", input.structured_impact_bps),
        ("市场一致预期", input.consensus_impact_bps),
    ] {
        if value.is_none() {
            missing_inputs.push(name.into());
        }
    }
    let score_value = (available >= 3 && sign != 0).then_some(score.clamp(0, 100));
    let (price_in_state, opportunity, rationale) = match (fundamental.direction, score_value) {
        (ImpactDirection::Positive, Some(value)) if value >= 80 => (
            "over_priced",
            "追高风险",
            "基本面方向偏正，但事件前异常收益、成交和估值显示市场可能已过度交易。",
        ),
        (ImpactDirection::Positive, Some(value)) if value >= 60 => (
            "mostly_priced",
            "机会偏中性",
            "基本面方向偏正，但多数预期可能已计入价格，需等待兑现或新预期差。",
        ),
        (ImpactDirection::Positive, Some(_)) => (
            "partially_priced",
            "仍需验证",
            "基本面方向偏正且未见充分提前交易，但仍需验证正式生效与经营兑现。",
        ),
        (ImpactDirection::Negative, Some(value)) if value >= 70 => (
            "risk_mostly_priced",
            "风险或已部分释放",
            "负面经营影响存在，但价格与成交已提前反应；不代表风险结束。",
        ),
        (ImpactDirection::Negative, Some(_)) => (
            "risk_not_fully_priced",
            "风险仍待消化",
            "负面经营影响与市场反应尚未充分匹配。",
        ),
        (_, _) => (
            "insufficient_data",
            "无法量化",
            "方向或定量输入不足，不能把情绪标签转换为买卖方向。",
        ),
    };
    let market_opportunity = MarketOpportunityConclusion {
        price_in_state: price_in_state.into(),
        opportunity: opportunity.into(),
        price_in_score: score_value,
        rationale: rationale.into(),
        no_trade_directive: "该结论只比较经营影响与市场定价，不生成买入/卖出指令。".into(),
    };
    let diagnostics = PriceInDiagnostics {
        pre_stock_return_bps: stock_return,
        pre_benchmark_return_bps: benchmark_return,
        pre_abnormal_return_bps: abnormal,
        sector_relative_bps: sector_relative,
        abnormal_volume_bps: volume,
        valuation_change_bps: valuation,
        historical_median_post_bps: analog_median,
        historical_sample_count: input.historical_analogs.len(),
        components,
    };
    let created_at = now_secs();
    let assessment_id = format!(
        "eass:{}",
        short_hash(&format!(
            "{}|{}|{}|{}",
            input.event.event_id, input.security_code, input.as_of_date, PRICE_IN_MODEL_VERSION
        ))
    );
    Ok(EventMarketAssessment {
        assessment_id,
        event_id: input.event.event_id,
        security_code: input.security_code,
        as_of_date: input.as_of_date,
        fundamental,
        market_opportunity,
        expectation_gap,
        diagnostics,
        missing_inputs,
        data_versions: input.data_versions,
        created_at,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventStudySample {
    pub sample_id: String,
    pub event_id: String,
    pub ontology_kind: EventKind,
    pub security_code: String,
    pub event_date: String,
    pub pre_window_days: u32,
    pub post_window_days: u32,
    pub pre_abnormal_return_bps: Option<i64>,
    pub post_abnormal_return_bps: Option<i64>,
    pub abnormal_volume_bps: Option<i64>,
    pub valuation_change_bps: Option<i64>,
    pub fundamental_direction: ImpactDirection,
    pub source_revision_id: String,
    pub data_version: String,
    pub created_at: i64,
}

pub struct EventStudyInput<'a> {
    pub event: &'a StructuredEvent,
    pub security_code: &'a str,
    pub event_date: &'a str,
    pub stock: &'a [PriceSeriesPoint],
    pub benchmark: &'a [PriceSeriesPoint],
    pub valuation: &'a [PriceSeriesPoint],
    pub post_window_days: usize,
    pub data_version: &'a str,
}

pub fn build_event_study_sample(input: EventStudyInput<'_>) -> Option<EventStudySample> {
    let event_index = input
        .stock
        .iter()
        .position(|point| point.date.as_str() >= input.event_date)?;
    if event_index == 0 || event_index + input.post_window_days >= input.stock.len() {
        return None;
    }
    let stock_post = {
        let a = input.stock[event_index].close;
        let b = input.stock[event_index + input.post_window_days].close;
        (a.is_finite() && b.is_finite() && a > 0.0)
            .then_some((((b / a) - 1.0) * 10_000.0).round() as i64)?
    };
    let benchmark_index = input
        .benchmark
        .iter()
        .position(|point| point.date.as_str() >= input.event_date)?;
    if benchmark_index + input.post_window_days >= input.benchmark.len() {
        return None;
    }
    let benchmark_post = {
        let a = input.benchmark[benchmark_index].close;
        let b = input.benchmark[benchmark_index + input.post_window_days].close;
        (a.is_finite() && b.is_finite() && a > 0.0)
            .then_some((((b / a) - 1.0) * 10_000.0).round() as i64)?
    };
    let text = format!("{} {}", input.event.title, input.event.kind.chinese_name());
    Some(EventStudySample {
        sample_id: format!(
            "ests:{}",
            short_hash(&format!(
                "{}|{}|{}|{}",
                input.event.event_id,
                input.security_code,
                input.post_window_days,
                input.data_version
            ))
        ),
        event_id: input.event.event_id.clone(),
        ontology_kind: input.event.kind,
        security_code: input.security_code.into(),
        event_date: input.event_date.into(),
        pre_window_days: 5,
        post_window_days: input.post_window_days as u32,
        pre_abnormal_return_bps: return_bps(input.stock, input.event_date, 5)
            .zip(return_bps(input.benchmark, input.event_date, 5))
            .map(|(stock, market)| stock - market),
        post_abnormal_return_bps: Some(stock_post - benchmark_post),
        abnormal_volume_bps: abnormal_volume_bps(input.stock, input.event_date),
        valuation_change_bps: valuation_change_bps(input.valuation, input.event_date),
        fundamental_direction: direction_hint(input.event.kind, &text),
        source_revision_id: input.event.source_revision_id.clone(),
        data_version: input.data_version.into(),
        created_at: now_secs(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationSummary {
    pub ontology_kind: EventKind,
    pub sample_count: usize,
    pub median_post_abnormal_return_bps: Option<i64>,
    pub positive_sample_ratio_bps: Option<i64>,
    pub data_versions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventResearchBundle {
    pub event: StructuredEvent,
    pub timeline: Vec<EventStateTransition>,
    pub assessment: Option<EventMarketAssessment>,
    pub calibration: CalibrationSummary,
}

pub struct EventStore {
    storage: Storage,
}

impl EventStore {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    pub async fn upsert_event(&self, event: StructuredEvent) -> Result<String> {
        validate_evidence_coverage(&event)?;
        let output = event.event_id.clone();
        self.storage.run(move |conn| {
            let now = now_secs();
            conn.execute(
                "INSERT INTO structured_events
                 (event_id,source_revision_id,ontology_kind,title,subjects_json,objects_json,
                  amount_text,quantity_text,unit_original,currency_original,baseline_period,
                  starts_at,ends_at,region,conditions_json,official_effective,reversibility,
                  impact_horizon,lifecycle_status,catalyst_path_json,validation_dates_json,
                  invalidation_json,missing_fields_json,extraction_version,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26)
                 ON CONFLICT(event_id) DO UPDATE SET source_revision_id=excluded.source_revision_id,
                  ontology_kind=excluded.ontology_kind,title=excluded.title,subjects_json=excluded.subjects_json,
                  objects_json=excluded.objects_json,amount_text=excluded.amount_text,
                  quantity_text=excluded.quantity_text,unit_original=excluded.unit_original,
                  currency_original=excluded.currency_original,baseline_period=excluded.baseline_period,
                  starts_at=excluded.starts_at,ends_at=excluded.ends_at,region=excluded.region,
                  conditions_json=excluded.conditions_json,official_effective=excluded.official_effective,
                  reversibility=excluded.reversibility,impact_horizon=excluded.impact_horizon,
                  lifecycle_status=excluded.lifecycle_status,catalyst_path_json=excluded.catalyst_path_json,
                  validation_dates_json=excluded.validation_dates_json,invalidation_json=excluded.invalidation_json,
                  missing_fields_json=excluded.missing_fields_json,extraction_version=excluded.extraction_version,
                  updated_at=excluded.updated_at",
                params![event.event_id,event.source_revision_id,event.kind.token(),event.title,
                    serde_json::to_string(&event.subjects)?,serde_json::to_string(&event.objects)?,
                    event.amount_text,event.quantity_text,event.unit_original,event.currency_original,
                    event.baseline_period,event.starts_at,event.ends_at,event.region,
                    serde_json::to_string(&event.conditions)?,event.official_effective.map(i64::from),
                    event.reversibility.token(),event.impact_horizon.token(),event.lifecycle.token(),
                    serde_json::to_string(&event.catalyst_path)?,serde_json::to_string(&event.validation_dates)?,
                    serde_json::to_string(&event.invalidation_conditions)?,serde_json::to_string(&event.missing_fields)?,
                    event.extraction_version,event.created_at,now],
            )?;
            for evidence in event.evidence {
                conn.execute(
                    "INSERT INTO event_field_evidence
                     (evidence_id,event_id,field_name,provenance_kind,source_revision_id,
                      source_version_id,quote_original,quote_zh,location_json,observed_at,
                      confidence_bps,created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
                     ON CONFLICT(evidence_id) DO UPDATE SET quote_original=excluded.quote_original,
                      quote_zh=excluded.quote_zh,location_json=excluded.location_json,
                      observed_at=excluded.observed_at,confidence_bps=excluded.confidence_bps",
                    params![evidence.evidence_id,evidence.event_id,evidence.field_name,
                        evidence.provenance.token(),evidence.source_revision_id,evidence.source_version_id,
                        evidence.quote_original,evidence.quote_zh,serde_json::to_string(&evidence.location)?,
                        evidence.observed_at,evidence.confidence_bps,now],
                )?;
            }
            Ok(())
        }).await?;
        Ok(output)
    }

    pub async fn event_by_revision(&self, revision_id: &str) -> Result<Option<StructuredEvent>> {
        let revision_id = revision_id.to_string();
        let row = self.storage.run(move |conn| {
            conn.query_row("SELECT event_id,source_revision_id,ontology_kind,title,subjects_json,objects_json,
                amount_text,quantity_text,unit_original,currency_original,baseline_period,starts_at,ends_at,
                region,conditions_json,official_effective,reversibility,impact_horizon,lifecycle_status,
                catalyst_path_json,validation_dates_json,invalidation_json,missing_fields_json,
                extraction_version,created_at,updated_at FROM structured_events WHERE source_revision_id=?1",
                params![revision_id], map_event_row).optional().map_err(Into::into)
        }).await?;
        match row {
            Some(event) => Ok(Some(self.with_evidence(event).await?)),
            None => Ok(None),
        }
    }

    pub async fn event(&self, event_id: &str) -> Result<Option<StructuredEvent>> {
        let event_id = event_id.to_string();
        let row = self.storage.run(move |conn| {
            conn.query_row("SELECT event_id,source_revision_id,ontology_kind,title,subjects_json,objects_json,
                amount_text,quantity_text,unit_original,currency_original,baseline_period,starts_at,ends_at,
                region,conditions_json,official_effective,reversibility,impact_horizon,lifecycle_status,
                catalyst_path_json,validation_dates_json,invalidation_json,missing_fields_json,
                extraction_version,created_at,updated_at FROM structured_events WHERE event_id=?1",
                params![event_id], map_event_row).optional().map_err(Into::into)
        }).await?;
        match row {
            Some(event) => Ok(Some(self.with_evidence(event).await?)),
            None => Ok(None),
        }
    }

    async fn with_evidence(&self, mut event: StructuredEvent) -> Result<StructuredEvent> {
        let event_id = event.event_id.clone();
        event.evidence = self.storage.run(move |conn| {
            let mut stmt = conn.prepare("SELECT evidence_id,event_id,field_name,provenance_kind,
                source_revision_id,source_version_id,quote_original,quote_zh,location_json,
                observed_at,confidence_bps FROM event_field_evidence WHERE event_id=?1 ORDER BY field_name,evidence_id")?;
            let rows = stmt.query_map(params![event_id], |row| {
                let provenance: String = row.get(3)?;
                Ok(EventFieldEvidence { evidence_id: row.get(0)?, event_id: row.get(1)?,
                    field_name: row.get(2)?, provenance: EvidenceProvenance::parse(&provenance).unwrap_or(EvidenceProvenance::AgentAssumption),
                    source_revision_id: row.get(4)?, source_version_id: row.get(5)?, quote_original: row.get(6)?,
                    quote_zh: row.get(7)?, location: serde_json::from_str(&row.get::<_, String>(8)?).unwrap_or_default(),
                    observed_at: row.get(9)?, confidence_bps: row.get(10)? })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        }).await?;
        Ok(event)
    }

    pub async fn transition(
        &self,
        event_id: &str,
        next: EventLifecycle,
        reason: &str,
        evidence_id: Option<String>,
        at: i64,
    ) -> Result<EventStateTransition> {
        let current = self
            .event(event_id)
            .await?
            .ok_or_else(|| Error::Invalid(format!("事件不存在：{event_id}")))?;
        if !current.lifecycle.can_transition_to(next) {
            return Err(Error::InvalidTransition(format!(
                "{} → {}",
                current.lifecycle.token(),
                next.token()
            )));
        }
        if reason.trim().is_empty() {
            return Err(Error::InvalidTransition("迁移原因不能为空".into()));
        }
        let transition = EventStateTransition {
            transition_id: format!(
                "etr:{}",
                short_hash(&format!("{event_id}|{}|{at}", next.token()))
            ),
            event_id: event_id.into(),
            from_status: current.lifecycle,
            to_status: next,
            reason: reason.into(),
            evidence_id,
            transitioned_at: at,
        };
        let saved = transition.clone();
        self.storage.run(move |conn| {
            conn.execute("INSERT OR IGNORE INTO event_state_transitions
                (transition_id,event_id,from_status,to_status,reason,evidence_id,transitioned_at,created_at)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?7)", params![saved.transition_id,saved.event_id,saved.from_status.token(),saved.to_status.token(),saved.reason,saved.evidence_id,saved.transitioned_at])?;
            conn.execute("UPDATE structured_events SET lifecycle_status=?2,updated_at=?3 WHERE event_id=?1", params![saved.event_id,saved.to_status.token(),saved.transitioned_at])?;
            Ok(())
        }).await?;
        Ok(transition)
    }

    pub async fn timeline(&self, event_id: &str) -> Result<Vec<EventStateTransition>> {
        let event_id = event_id.to_string();
        self.storage.run(move |conn| {
            let mut stmt = conn.prepare("SELECT transition_id,event_id,from_status,to_status,reason,evidence_id,transitioned_at FROM event_state_transitions WHERE event_id=?1 ORDER BY transitioned_at")?;
            let rows = stmt.query_map(params![event_id], |row| {
                let from: String = row.get(2)?; let to: String = row.get(3)?;
                Ok(EventStateTransition { transition_id: row.get(0)?, event_id: row.get(1)?, from_status: EventLifecycle::parse(&from).unwrap_or(EventLifecycle::Unverified), to_status: EventLifecycle::parse(&to).unwrap_or(EventLifecycle::Unverified), reason: row.get(4)?, evidence_id: row.get(5)?, transitioned_at: row.get(6)? })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        }).await.map_err(Into::into)
    }

    pub async fn save_assessment(&self, assessment: EventMarketAssessment) -> Result<()> {
        self.storage.run(move |conn| {
            conn.execute("INSERT INTO event_market_assessments
                (assessment_id,event_id,security_code,as_of_date,fundamental_json,market_opportunity_json,
                 expectation_gap_json,diagnostics_json,missing_inputs_json,data_versions_json,created_at)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
                ON CONFLICT(event_id,security_code,as_of_date) DO UPDATE SET
                 assessment_id=excluded.assessment_id,fundamental_json=excluded.fundamental_json,
                 market_opportunity_json=excluded.market_opportunity_json,expectation_gap_json=excluded.expectation_gap_json,
                 diagnostics_json=excluded.diagnostics_json,missing_inputs_json=excluded.missing_inputs_json,
                 data_versions_json=excluded.data_versions_json,created_at=excluded.created_at",
                params![assessment.assessment_id,assessment.event_id,assessment.security_code,assessment.as_of_date,
                    serde_json::to_string(&assessment.fundamental)?,serde_json::to_string(&assessment.market_opportunity)?,
                    serde_json::to_string(&assessment.expectation_gap)?,serde_json::to_string(&assessment.diagnostics)?,
                    serde_json::to_string(&assessment.missing_inputs)?,serde_json::to_string(&assessment.data_versions)?,assessment.created_at])?;
            Ok(())
        }).await.map_err(Into::into)
    }

    pub async fn latest_assessment(
        &self,
        event_id: &str,
        security_code: Option<&str>,
    ) -> Result<Option<EventMarketAssessment>> {
        let event_id = event_id.to_string();
        let code = security_code.unwrap_or_default().to_string();
        self.storage.run(move |conn| {
            conn.query_row("SELECT assessment_id,event_id,security_code,as_of_date,fundamental_json,
                market_opportunity_json,expectation_gap_json,diagnostics_json,missing_inputs_json,data_versions_json,created_at
                FROM event_market_assessments WHERE event_id=?1 AND (?2='' OR security_code=?2)
                ORDER BY created_at DESC LIMIT 1", params![event_id,code], |row| {
                    Ok(EventMarketAssessment { assessment_id: row.get(0)?, event_id: row.get(1)?, security_code: row.get(2)?, as_of_date: row.get(3)?,
                        fundamental: serde_json::from_str(&row.get::<_, String>(4)?).unwrap(), market_opportunity: serde_json::from_str(&row.get::<_, String>(5)?).unwrap(),
                        expectation_gap: serde_json::from_str(&row.get::<_, String>(6)?).unwrap(), diagnostics: serde_json::from_str(&row.get::<_, String>(7)?).unwrap(),
                        missing_inputs: serde_json::from_str(&row.get::<_, String>(8)?).unwrap_or_default(), data_versions: serde_json::from_str(&row.get::<_, String>(9)?).unwrap_or_default(), created_at: row.get(10)? })
                }).optional().map_err(Into::into)
        }).await.map_err(Into::into)
    }

    pub async fn save_study_sample(&self, sample: EventStudySample) -> Result<()> {
        self.storage.run(move |conn| {
            conn.execute("INSERT INTO event_study_samples
                (sample_id,event_id,ontology_kind,security_code,event_date,pre_window_days,post_window_days,
                 pre_abnormal_return_bps,post_abnormal_return_bps,abnormal_volume_bps,valuation_change_bps,
                 fundamental_direction,source_revision_id,data_version,created_at)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
                ON CONFLICT(event_id,security_code,post_window_days,data_version) DO UPDATE SET
                 pre_abnormal_return_bps=excluded.pre_abnormal_return_bps,
                 post_abnormal_return_bps=excluded.post_abnormal_return_bps,
                 abnormal_volume_bps=excluded.abnormal_volume_bps,valuation_change_bps=excluded.valuation_change_bps,
                 fundamental_direction=excluded.fundamental_direction,created_at=excluded.created_at",
                params![sample.sample_id,sample.event_id,sample.ontology_kind.token(),sample.security_code,
                    sample.event_date,sample.pre_window_days,sample.post_window_days,sample.pre_abnormal_return_bps,
                    sample.post_abnormal_return_bps,sample.abnormal_volume_bps,sample.valuation_change_bps,
                    sample.fundamental_direction.token(),sample.source_revision_id,sample.data_version,sample.created_at])?;
            Ok(())
        }).await.map_err(Into::into)
    }

    pub async fn historical_analogs(
        &self,
        kind: EventKind,
        limit: usize,
    ) -> Result<Vec<HistoricalAnalog>> {
        self.storage.run(move |conn| {
            let mut stmt = conn.prepare("SELECT sample_id,post_abnormal_return_bps FROM event_study_samples
                WHERE ontology_kind=?1 AND post_abnormal_return_bps IS NOT NULL ORDER BY event_date DESC LIMIT ?2")?;
            let rows = stmt.query_map(params![kind.token(),limit.clamp(1,500)], |row| Ok(HistoricalAnalog { sample_id: row.get(0)?, post_abnormal_return_bps: row.get(1)? }))?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        }).await.map_err(Into::into)
    }

    pub async fn calibration_summary(&self, kind: EventKind) -> Result<CalibrationSummary> {
        let kind_token = kind.token().to_string();
        let rows = self.storage.run(move |conn| {
            let mut stmt = conn.prepare("SELECT post_abnormal_return_bps,data_version FROM event_study_samples
                WHERE ontology_kind=?1 AND post_abnormal_return_bps IS NOT NULL ORDER BY event_date")?;
            let values = stmt.query_map(params![kind_token], |row| Ok((row.get::<_, i64>(0)?,row.get::<_, String>(1)?)))?;
            Ok(values.collect::<std::result::Result<Vec<_>, _>>()?)
        }).await?;
        let mut returns = rows.iter().map(|(value, _)| *value).collect::<Vec<_>>();
        let positives = returns.iter().filter(|value| **value > 0).count();
        let versions = rows
            .iter()
            .map(|(_, version)| version.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(CalibrationSummary {
            ontology_kind: kind,
            sample_count: returns.len(),
            median_post_abnormal_return_bps: median(&mut returns),
            positive_sample_ratio_bps: (!rows.is_empty())
                .then_some((positives as i64 * 10_000) / rows.len() as i64),
            data_versions: versions,
        })
    }
}

fn map_event_row(row: &Row<'_>) -> rusqlite::Result<StructuredEvent> {
    let kind: String = row.get(2)?;
    let reversibility: String = row.get(16)?;
    let horizon: String = row.get(17)?;
    let lifecycle: String = row.get(18)?;
    Ok(StructuredEvent {
        event_id: row.get(0)?,
        source_revision_id: row.get(1)?,
        kind: EventKind::parse(&kind).unwrap_or(EventKind::Other),
        title: row.get(3)?,
        subjects: serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
        objects: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
        amount_text: row.get(6)?,
        quantity_text: row.get(7)?,
        unit_original: row.get(8)?,
        currency_original: row.get(9)?,
        baseline_period: row.get(10)?,
        starts_at: row.get(11)?,
        ends_at: row.get(12)?,
        region: row.get(13)?,
        conditions: serde_json::from_str(&row.get::<_, String>(14)?).unwrap_or_default(),
        official_effective: row.get::<_, Option<i64>>(15)?.map(|value| value != 0),
        reversibility: Reversibility::parse(&reversibility).unwrap_or(Reversibility::Unknown),
        impact_horizon: ImpactHorizon::parse(&horizon).unwrap_or(ImpactHorizon::Days),
        lifecycle: EventLifecycle::parse(&lifecycle).unwrap_or(EventLifecycle::Unverified),
        catalyst_path: serde_json::from_str(&row.get::<_, String>(19)?).unwrap_or_default(),
        validation_dates: serde_json::from_str(&row.get::<_, String>(20)?).unwrap_or_default(),
        invalidation_conditions: serde_json::from_str(&row.get::<_, String>(21)?)
            .unwrap_or_default(),
        missing_fields: serde_json::from_str(&row.get::<_, String>(22)?).unwrap_or_default(),
        evidence: Vec::new(),
        extraction_version: row.get(23)?,
        created_at: row.get(24)?,
        updated_at: row.get(25)?,
    })
}

fn short_hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))[..28].to_string()
}
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|v| v.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_storage::StorageConfig;

    fn extraction(primary: bool) -> EventExtractionInput {
        EventExtractionInput {
            source_revision_id: "rev:official:1".into(),
            source_version_id: Some("sv:1".into()),
            title: "某公司公告中标 12.5 亿元项目，2026年9月1日生效".into(),
            factual_summary: "合同若取得业主开工令后执行，预计履行两年。".into(),
            original_language: "zh-CN".into(),
            source_is_primary: primary,
            event_time_utc: Some(1_780_000_000),
            first_seen_at: 1_780_000_100,
            subjects: vec![EventEntityRef {
                entity_id: "cn:600000".into(),
                name: "某公司".into(),
                listed_code: Some("600000".into()),
                role: "subject".into(),
            }],
        }
    }

    fn series(pre_gain: f64, volume_spike: f64, pe_gain: f64) -> Vec<PriceSeriesPoint> {
        (0..30)
            .map(|index| {
                let progress = index as f64 / 29.0;
                PriceSeriesPoint {
                    date: format!("2026-08-{:02}", index + 1),
                    close: 100.0 * (1.0 + pre_gain * progress),
                    volume: if index == 29 {
                        100.0 * volume_spike
                    } else {
                        100.0
                    },
                    pe_ttm: Some(20.0 * (1.0 + pe_gain * progress)),
                }
            })
            .collect()
    }

    #[test]
    fn ontology_extracts_only_evidenced_fields_and_marks_every_gap() {
        let event = extract_structured_event(extraction(true)).unwrap();
        assert_eq!(event.kind, EventKind::Order);
        assert_eq!(event.lifecycle, EventLifecycle::Effective);
        assert_eq!(event.amount_text.as_deref(), Some("12.5 亿元"));
        assert!(event.conditions.iter().any(|value| value.contains("若")));
        assert!(event.missing_fields.contains(&"对象".to_string()));
        assert!(event.missing_fields.contains(&"数量".to_string()));
        validate_evidence_coverage(&event).unwrap();
        assert!(event
            .evidence
            .iter()
            .filter(|row| row.provenance == EvidenceProvenance::ObservedFact)
            .all(|row| row.source_revision_id.is_some() && row.quote_original.is_some()));
    }

    #[test]
    fn rumor_and_primary_confirmation_are_not_collapsed() {
        let mut input = extraction(false);
        input.title = "网传某公司拟收购资产".into();
        let rumor = extract_structured_event(input).unwrap();
        assert_eq!(rumor.lifecycle, EventLifecycle::Rumor);
        assert_eq!(rumor.kind, EventKind::MergerAcquisition);
    }

    #[test]
    fn positive_fundamental_can_be_a_poor_market_opportunity() {
        let event = extract_structured_event(extraction(true)).unwrap();
        let assessment = analyze_price_in(PriceInInput {
            event,
            security_code: "600000".into(),
            event_date: "2026-08-30".into(),
            as_of_date: "2026-08-30".into(),
            stock: series(0.35, 3.0, 0.30),
            benchmark: series(0.02, 1.0, 0.0),
            sector: series(0.05, 1.0, 0.0),
            valuation: series(0.35, 3.0, 0.30),
            structured_impact_bps: Some(800),
            consensus_impact_bps: Some(700),
            historical_analogs: vec![
                HistoricalAnalog {
                    sample_id: "a".into(),
                    post_abnormal_return_bps: 300,
                },
                HistoricalAnalog {
                    sample_id: "b".into(),
                    post_abnormal_return_bps: 400,
                },
            ],
            data_versions: serde_json::json!({"bars":"fixture-v1"}),
        })
        .unwrap();
        assert_eq!(assessment.fundamental.direction, ImpactDirection::Positive);
        assert!(matches!(
            assessment.market_opportunity.price_in_state.as_str(),
            "mostly_priced" | "over_priced"
        ));
        assert_ne!(assessment.market_opportunity.opportunity, "买入");
        assert_eq!(assessment.diagnostics.components.len(), 6);
    }

    #[test]
    fn insufficient_price_in_inputs_stay_unquantified() {
        let event = extract_structured_event(extraction(true)).unwrap();
        let assessment = analyze_price_in(PriceInInput {
            event,
            security_code: "600000".into(),
            event_date: "2026-08-30".into(),
            as_of_date: "2026-08-30".into(),
            stock: Vec::new(),
            benchmark: Vec::new(),
            sector: Vec::new(),
            valuation: Vec::new(),
            structured_impact_bps: None,
            consensus_impact_bps: None,
            historical_analogs: Vec::new(),
            data_versions: serde_json::json!({}),
        })
        .unwrap();
        assert_eq!(
            assessment.market_opportunity.price_in_state,
            "insufficient_data"
        );
        assert!(!assessment.expectation_gap.quantifiable);
        assert!(assessment.missing_inputs.len() >= 6);
    }

    #[tokio::test]
    async fn lifecycle_store_and_historical_calibration_are_auditable() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let store = EventStore::new(storage);
        let event = extract_structured_event(extraction(true)).unwrap();
        store.upsert_event(event.clone()).await.unwrap();
        let invalid = store
            .transition(
                &event.event_id,
                EventLifecycle::Rumor,
                "倒退",
                None,
                1_780_000_200,
            )
            .await;
        assert!(matches!(invalid, Err(Error::InvalidTransition(_))));
        store
            .transition(
                &event.event_id,
                EventLifecycle::Completed,
                "合同履行完毕公告",
                event.evidence.first().map(|v| v.evidence_id.clone()),
                1_790_000_000,
            )
            .await
            .unwrap();
        assert_eq!(store.timeline(&event.event_id).await.unwrap().len(), 1);
        store
            .save_study_sample(EventStudySample {
                sample_id: "sample:1".into(),
                event_id: event.event_id.clone(),
                ontology_kind: EventKind::Order,
                security_code: "600000".into(),
                event_date: "2026-08-30".into(),
                pre_window_days: 5,
                post_window_days: 20,
                pre_abnormal_return_bps: Some(100),
                post_abnormal_return_bps: Some(500),
                abnormal_volume_bps: Some(2000),
                valuation_change_bps: Some(300),
                fundamental_direction: ImpactDirection::Positive,
                source_revision_id: event.source_revision_id,
                data_version: "fixture-v1".into(),
                created_at: 1_800_000_000,
            })
            .await
            .unwrap();
        let summary = store.calibration_summary(EventKind::Order).await.unwrap();
        assert_eq!(summary.sample_count, 1);
        assert_eq!(summary.median_post_abnormal_return_bps, Some(500));
        assert_eq!(summary.positive_sample_ratio_bps, Some(10_000));
    }
}
