//! Auditable overseas primary-source ingestion and Global -> A-share paths.
//!
//! This crate intentionally models overseas information as an input to
//! A-share research. It is not an overseas execution terminal. A transmission
//! relation cannot be activated without an archived primary-source version,
//! a verbatim evidence span, an observation time and an explicit confidence.

use std::collections::{BTreeSet, HashMap, HashSet};

use astock_storage::Storage;
use astock_trading_rules::{
    classify_news_session, EffectiveNewsSession, NewsSessionInput, PublicationPrecision, RuleSet,
};
use chrono::{LocalResult, NaiveDateTime, Offset, TimeZone};
use chrono_tz::Tz;
use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const GLOBAL_SCHEMA_VERSION: &str = "global-transmission-v1";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Storage(#[from] astock_storage::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("未知 IANA 时区：{0}")]
    UnknownTimezone(String),
    #[error("本地时间位于夏令时跳变缺口：{local} {timezone}")]
    NonexistentLocalTime { local: String, timezone: String },
    #[error("本地时间存在夏令时歧义，必须选择较早或较晚偏移：{local} {timezone}")]
    AmbiguousLocalTime { local: String, timezone: String },
    #[error("翻译确定性校验失败，缺少受保护内容：{0}")]
    TranslationProtection(String),
    #[error("数值换算参数无效：{0}")]
    InvalidConversion(String),
    #[error("关系证据无效：{0}")]
    InvalidEvidence(String),
    #[error("全球文档不存在：{0}")]
    DocumentNotFound(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GlobalSourceCategory {
    CompanyDisclosure,
    MacroPolicy,
    TradeRegulation,
    EnergyCommodity,
}

impl GlobalSourceCategory {
    pub fn token(self) -> &'static str {
        match self {
            Self::CompanyDisclosure => "company_disclosure",
            Self::MacroPolicy => "macro_policy",
            Self::TradeRegulation => "trade_regulation",
            Self::EnergyCommodity => "energy_commodity",
        }
    }

    pub fn chinese_name(self) -> &'static str {
        match self {
            Self::CompanyDisclosure => "海外公司正式披露",
            Self::MacroPolicy => "宏观与政策原始数据",
            Self::TradeRegulation => "贸易与监管原文",
            Self::EnergyCommodity => "能源、商品与持仓数据",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalSourceDefinition {
    pub provider_id: &'static str,
    pub provider_name: &'static str,
    pub region: &'static str,
    pub category: GlobalSourceCategory,
    pub official_url: &'static str,
    pub original_timezone: &'static str,
    pub license_policy: &'static str,
    /// Opaque Windows Credential Manager slot. The legacy field name is kept
    /// only for the existing database column and v1 JSON compatibility; this
    /// crate never reads credentials or process environment variables.
    pub credential_env: Option<&'static str>,
    pub target_latency_secs: u32,
    pub rate_limit_per_minute: u32,
}

/// Primary-source catalog. Configuration does not imply runtime success;
/// availability and missing credentials live in `global_provider_state`.
pub fn official_global_sources() -> Vec<GlobalSourceDefinition> {
    use GlobalSourceCategory::*;
    vec![
        GlobalSourceDefinition {
            provider_id: "sec_edgar",
            provider_name: "美国 SEC EDGAR",
            region: "US",
            category: CompanyDisclosure,
            official_url: "https://www.sec.gov/edgar/sec-api-documentation",
            original_timezone: "America/New_York",
            license_policy:
                "美国政府公开披露；自动访问遵守 SEC Fair Access，必须声明机构与联系方式",
            credential_env: Some("provider-sec-user-agent"),
            target_latency_secs: 60,
            rate_limit_per_minute: 300,
        },
        GlobalSourceDefinition {
            provider_id: "hkex",
            provider_name: "香港交易所披露易",
            region: "HK",
            category: CompanyDisclosure,
            official_url: "https://www1.hkexnews.hk/search/titlesearch.xhtml",
            original_timezone: "Asia/Hong_Kong",
            license_policy: "公开披露仅用于研究索引与原文引用；遵守网站条款和频率",
            credential_env: None,
            target_latency_secs: 120,
            rate_limit_per_minute: 12,
        },
        GlobalSourceDefinition {
            provider_id: "edinet",
            provider_name: "日本 EDINET",
            region: "JP",
            category: CompanyDisclosure,
            official_url: "https://disclosure2.edinet-fsa.go.jp/",
            original_timezone: "Asia/Tokyo",
            license_policy: "使用 EDINET API v2；API Key 由用户配置，遵守金融厅使用条款",
            credential_env: Some("provider-edinet-api-key"),
            target_latency_secs: 120,
            rate_limit_per_minute: 1,
        },
        GlobalSourceDefinition {
            provider_id: "dart",
            provider_name: "韩国 OpenDART",
            region: "KR",
            category: CompanyDisclosure,
            official_url: "https://opendart.fss.or.kr/",
            original_timezone: "Asia/Seoul",
            license_policy: "使用官方 OpenDART API；Key 由用户配置",
            credential_env: Some("provider-opendart-api-key"),
            target_latency_secs: 120,
            rate_limit_per_minute: 20,
        },
        GlobalSourceDefinition {
            provider_id: "twse_mops",
            provider_name: "台湾 MOPS/TWSE",
            region: "TW",
            category: CompanyDisclosure,
            official_url: "https://mops.twse.com.tw/mops/web/index",
            original_timezone: "Asia/Taipei",
            license_policy: "公开披露检索；保留原文入口与访问时间",
            credential_env: None,
            target_latency_secs: 180,
            rate_limit_per_minute: 10,
        },
        GlobalSourceDefinition {
            provider_id: "fed",
            provider_name: "美联储",
            region: "US",
            category: MacroPolicy,
            official_url: "https://www.federalreserve.gov/feeds/feeds.htm",
            original_timezone: "America/New_York",
            license_policy: "美国政府公开信息；保留发布修订",
            credential_env: None,
            target_latency_secs: 60,
            rate_limit_per_minute: 30,
        },
        GlobalSourceDefinition {
            provider_id: "bls",
            provider_name: "美国劳工统计局",
            region: "US",
            category: MacroPolicy,
            official_url: "https://www.bls.gov/developers/",
            original_timezone: "America/New_York",
            license_policy: "官方 Public Data API；高频批量查询可配置注册 Key",
            credential_env: Some("provider-bls-api-key"),
            target_latency_secs: 120,
            rate_limit_per_minute: 20,
        },
        GlobalSourceDefinition {
            provider_id: "bea",
            provider_name: "美国经济分析局",
            region: "US",
            category: MacroPolicy,
            official_url: "https://apps.bea.gov/api/",
            original_timezone: "America/New_York",
            license_policy: "官方 API；Key 由用户配置",
            credential_env: Some("provider-bea-api-key"),
            target_latency_secs: 180,
            rate_limit_per_minute: 20,
        },
        GlobalSourceDefinition {
            provider_id: "ecb",
            provider_name: "欧洲央行",
            region: "EU",
            category: MacroPolicy,
            official_url: "https://data.ecb.europa.eu/help/api/overview",
            original_timezone: "Europe/Brussels",
            license_policy: "ECB Data Portal API；保留数据集与修订版本",
            credential_env: None,
            target_latency_secs: 300,
            rate_limit_per_minute: 20,
        },
        GlobalSourceDefinition {
            provider_id: "eurostat",
            provider_name: "Eurostat",
            region: "EU",
            category: MacroPolicy,
            official_url:
                "https://ec.europa.eu/eurostat/web/user-guides/data-browser/api-data-access",
            original_timezone: "Europe/Luxembourg",
            license_policy: "欧盟官方数据 API；保留数据标记与修订",
            credential_env: None,
            target_latency_secs: 600,
            rate_limit_per_minute: 20,
        },
        GlobalSourceDefinition {
            provider_id: "imf",
            provider_name: "国际货币基金组织",
            region: "GLOBAL",
            category: MacroPolicy,
            official_url: "https://data.imf.org/",
            original_timezone: "America/New_York",
            license_policy: "IMF 官方数据；按数据集许可保存元数据",
            credential_env: None,
            target_latency_secs: 3_600,
            rate_limit_per_minute: 10,
        },
        GlobalSourceDefinition {
            provider_id: "world_bank",
            provider_name: "世界银行",
            region: "GLOBAL",
            category: MacroPolicy,
            official_url: "https://api.worldbank.org/v2/",
            original_timezone: "UTC",
            license_policy: "World Bank Indicators API v2；无 Key，保留指标来源组织和脚注",
            credential_env: None,
            target_latency_secs: 3_600,
            rate_limit_per_minute: 30,
        },
        GlobalSourceDefinition {
            provider_id: "bis_export",
            provider_name: "美国商务部 BIS",
            region: "US",
            category: TradeRegulation,
            official_url: "https://www.bis.gov/press-release",
            original_timezone: "America/New_York",
            license_policy: "美国政府公开法规与公告；以正式文件为准",
            credential_env: None,
            target_latency_secs: 180,
            rate_limit_per_minute: 12,
        },
        GlobalSourceDefinition {
            provider_id: "ustr",
            provider_name: "美国贸易代表办公室",
            region: "US",
            category: TradeRegulation,
            official_url: "https://ustr.gov/about-us/policy-offices/press-office/press-releases",
            original_timezone: "America/New_York",
            license_policy: "美国政府公开政策原文",
            credential_env: None,
            target_latency_secs: 300,
            rate_limit_per_minute: 12,
        },
        GlobalSourceDefinition {
            provider_id: "eu_trade",
            provider_name: "欧盟贸易与制裁文件",
            region: "EU",
            category: TradeRegulation,
            official_url: "https://policy.trade.ec.europa.eu/news_en",
            original_timezone: "Europe/Brussels",
            license_policy: "欧盟委员会公开文件；保留 CELEX/文件标识",
            credential_env: None,
            target_latency_secs: 300,
            rate_limit_per_minute: 12,
        },
        GlobalSourceDefinition {
            provider_id: "un_comtrade",
            provider_name: "UN Comtrade",
            region: "GLOBAL",
            category: TradeRegulation,
            official_url: "https://comtradeapi.un.org/",
            original_timezone: "UTC",
            license_policy: "联合国 Comtrade API；大批量/商业使用依账户许可",
            credential_env: Some("provider-un-comtrade-api-key"),
            target_latency_secs: 86_400,
            rate_limit_per_minute: 10,
        },
        GlobalSourceDefinition {
            provider_id: "wto",
            provider_name: "世界贸易组织",
            region: "GLOBAL",
            category: TradeRegulation,
            official_url: "https://apiportal.wto.org/",
            original_timezone: "Europe/Zurich",
            license_policy: "WTO 官方 API；Key 与许可由用户配置",
            credential_env: Some("provider-wto-api-key"),
            target_latency_secs: 3_600,
            rate_limit_per_minute: 10,
        },
        GlobalSourceDefinition {
            provider_id: "eia",
            provider_name: "美国能源信息署",
            region: "US",
            category: EnergyCommodity,
            official_url: "https://www.eia.gov/opendata/",
            original_timezone: "America/New_York",
            license_policy: "美国政府 EIA Open Data API v2；Key 由用户配置",
            credential_env: Some("provider-eia-api-key"),
            target_latency_secs: 600,
            rate_limit_per_minute: 20,
        },
        GlobalSourceDefinition {
            provider_id: "opec",
            provider_name: "OPEC",
            region: "GLOBAL",
            category: EnergyCommodity,
            official_url: "https://www.opec.org/opec_web/en/publications/202.htm",
            original_timezone: "Europe/Vienna",
            license_policy: "官方报告原文；版权内容仅保存允许的证据片段与元数据",
            credential_env: None,
            target_latency_secs: 3_600,
            rate_limit_per_minute: 6,
        },
        GlobalSourceDefinition {
            provider_id: "iea",
            provider_name: "国际能源署",
            region: "GLOBAL",
            category: EnergyCommodity,
            official_url: "https://www.iea.org/data-and-statistics",
            original_timezone: "Europe/Paris",
            license_policy: "按 IEA 数据许可；未获许可时仅保存公开元数据和链接",
            credential_env: Some("provider-iea-api-key"),
            target_latency_secs: 3_600,
            rate_limit_per_minute: 6,
        },
        GlobalSourceDefinition {
            provider_id: "cftc",
            provider_name: "美国商品期货交易委员会",
            region: "US",
            category: EnergyCommodity,
            official_url: "https://www.cftc.gov/MarketReports/CommitmentsofTraders/index.htm",
            original_timezone: "America/New_York",
            license_policy: "美国政府公开持仓报告；保留报告日期与修订",
            credential_env: None,
            target_latency_secs: 3_600,
            rate_limit_per_minute: 20,
        },
        GlobalSourceDefinition {
            provider_id: "sge_gold",
            provider_name: "上海黄金交易所",
            region: "CN",
            category: EnergyCommodity,
            official_url: "https://www.sge.com.cn/",
            original_timezone: "Asia/Shanghai",
            license_policy: "交易所公开行情、公告与统计；保留原始链接、发布时间和品种单位",
            credential_env: None,
            target_latency_secs: 300,
            rate_limit_per_minute: 12,
        },
        GlobalSourceDefinition {
            provider_id: "safe_reserves",
            provider_name: "国家外汇管理局储备数据",
            region: "CN",
            category: MacroPolicy,
            official_url: "https://www.safe.gov.cn/safe/tjsj1/index.html",
            original_timezone: "Asia/Shanghai",
            license_policy: "官方公开储备统计；保留统计期、首次发布时间与修订",
            credential_env: None,
            target_latency_secs: 3_600,
            rate_limit_per_minute: 10,
        },
        GlobalSourceDefinition {
            provider_id: "pbc",
            provider_name: "中国人民银行",
            region: "CN",
            category: MacroPolicy,
            official_url: "https://www.pbc.gov.cn/",
            original_timezone: "Asia/Shanghai",
            license_policy: "央行公开政策、新闻和统计原文；以正式发布页为准",
            credential_env: None,
            target_latency_secs: 600,
            rate_limit_per_minute: 10,
        },
        GlobalSourceDefinition {
            provider_id: "lbma",
            provider_name: "伦敦金银市场协会",
            region: "UK",
            category: EnergyCommodity,
            official_url: "https://www.lbma.org.uk/",
            original_timezone: "Europe/London",
            license_policy: "基准与市场资料按 LBMA 使用条款；未获许可时仅保存公开元数据和链接",
            credential_env: None,
            target_latency_secs: 600,
            rate_limit_per_minute: 10,
        },
        GlobalSourceDefinition {
            provider_id: "world_gold_council",
            provider_name: "世界黄金协会",
            region: "GLOBAL",
            category: EnergyCommodity,
            official_url: "https://www.gold.org/goldhub",
            original_timezone: "Europe/London",
            license_policy: "行业一手研究与统计按网站许可；保留标题、摘要和原文链接",
            credential_env: None,
            target_latency_secs: 3_600,
            rate_limit_per_minute: 10,
        },
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DstDisambiguation {
    Reject,
    Earlier,
    Later,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedGlobalClock {
    pub original_local: String,
    pub timezone: String,
    pub utc_timestamp: i64,
    pub utc_offset_seconds: i32,
    pub utc_iso: String,
}

/// Normalize a source-local publication clock without discarding its original
/// timezone. DST gaps fail closed; repeated clocks require an explicit side.
pub fn normalize_local_publication(
    local: NaiveDateTime,
    timezone: &str,
    disambiguation: DstDisambiguation,
) -> Result<NormalizedGlobalClock> {
    let tz: Tz = timezone
        .parse()
        .map_err(|_| Error::UnknownTimezone(timezone.to_string()))?;
    let value = match tz.from_local_datetime(&local) {
        LocalResult::Single(value) => value,
        LocalResult::None => {
            return Err(Error::NonexistentLocalTime {
                local: local.to_string(),
                timezone: timezone.to_string(),
            })
        }
        LocalResult::Ambiguous(earlier, later) => match disambiguation {
            DstDisambiguation::Earlier => earlier,
            DstDisambiguation::Later => later,
            DstDisambiguation::Reject => {
                return Err(Error::AmbiguousLocalTime {
                    local: local.to_string(),
                    timezone: timezone.to_string(),
                })
            }
        },
    };
    Ok(NormalizedGlobalClock {
        original_local: local.format("%Y-%m-%d %H:%M:%S").to_string(),
        timezone: timezone.to_string(),
        utc_timestamp: value.timestamp(),
        utc_offset_seconds: value.offset().fix().local_minus_utc(),
        utc_iso: value.with_timezone(&chrono::Utc).to_rfc3339(),
    })
}

pub fn map_global_release_to_a_share(
    rules: &RuleSet,
    clock: &NormalizedGlobalClock,
    first_seen_at: i64,
    verified: bool,
) -> astock_trading_rules::Result<EffectiveNewsSession> {
    classify_news_session(
        rules,
        &NewsSessionInput {
            event_time_utc: Some(clock.utc_timestamp),
            publish_time_utc: Some(clock.utc_timestamp),
            first_seen_time_utc: first_seen_at,
            revision_time_utc: None,
            publication_precision: PublicationPrecision::ExactTime,
            stale: false,
            verified,
            discovery_only: !verified,
            old_republication: false,
        },
    )
}

static NUMBER_OR_CODE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?:\b[A-Z]{2,8}(?:[-.:/][A-Z0-9]{1,12})?\b|[-+]?\d[\d,]*(?:\.\d+)?%?)")
        .expect("protected-token regex")
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationAudit {
    pub protected_tokens: Vec<String>,
    pub status: String,
}

/// Translation may improve readability but cannot silently rewrite numbers,
/// identifiers, legal entity names or caller-supplied key terms.
pub fn validate_translation(
    original: &str,
    translated: &str,
    additional_protected: &[String],
) -> Result<TranslationAudit> {
    let mut tokens: BTreeSet<String> = NUMBER_OR_CODE
        .find_iter(original)
        .map(|m| m.as_str().to_string())
        .collect();
    tokens.extend(
        additional_protected
            .iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
    );
    let missing: Vec<String> = tokens
        .iter()
        .filter(|token| !translated.contains(token.as_str()))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(Error::TranslationProtection(missing.join("、")));
    }
    Ok(TranslationAudit {
        protected_tokens: tokens.into_iter().collect(),
        status: "deterministic_checks_passed".into(),
    })
}

/// Fixed-point currency conversion. `amount_scaled / amount_scale` is
/// multiplied by `rate_scaled / rate_scale` and returned in `output_scale`.
pub fn convert_currency_scaled(
    amount_scaled: i64,
    amount_scale: i64,
    rate_scaled: i64,
    rate_scale: i64,
    output_scale: i64,
) -> Result<i64> {
    if amount_scale <= 0 || rate_scale <= 0 || output_scale <= 0 || rate_scaled < 0 {
        return Err(Error::InvalidConversion(
            "scale 必须为正数且汇率不得为负".into(),
        ));
    }
    let numerator = i128::from(amount_scaled)
        .checked_mul(i128::from(rate_scaled))
        .and_then(|value| value.checked_mul(i128::from(output_scale)))
        .ok_or_else(|| Error::InvalidConversion("固定点乘法溢出".into()))?;
    let denominator = i128::from(amount_scale) * i128::from(rate_scale);
    let rounded = if numerator >= 0 {
        (numerator + denominator / 2) / denominator
    } else {
        (numerator - denominator / 2) / denominator
    };
    i64::try_from(rounded).map_err(|_| Error::InvalidConversion("结果超出 i64".into()))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionPoint {
    pub revision_no: u32,
    pub released_at_utc: i64,
    pub value_scaled: i64,
}

/// Select the latest revision that was actually available at `as_of`.
pub fn point_in_time_revision(revisions: &[RevisionPoint], as_of: i64) -> Option<&RevisionPoint> {
    revisions
        .iter()
        .filter(|value| value.released_at_utc <= as_of)
        .max_by_key(|value| (value.released_at_utc, value.revision_no))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GoldenChainTemplate {
    pub chain_id: &'static str,
    pub name: &'static str,
    pub global_sources: &'static [&'static str],
    pub nodes: &'static [&'static str],
    pub activation_requirement: &'static str,
}

/// Four regression chains define required topology, not unsupported facts.
/// A chain becomes active only after each relation is backed by both the
/// overseas source version and a domestic disclosure/industry source version.
pub fn global_a_share_golden_chains() -> Vec<GoldenChainTemplate> {
    vec![
        GoldenChainTemplate {
            chain_id: "semiconductor-controls",
            name: "半导体：出口管制到设备/材料",
            global_sources: &["bis_export", "sec_edgar"],
            nodes: &[
                "海外政策/客户",
                "受限产品与工艺",
                "国产替代或需求冲击",
                "A股半导体设备/材料公司",
            ],
            activation_requirement: "BIS/客户正式原文 + A股公司正式披露 + 字段级关系证据",
        },
        GoldenChainTemplate {
            chain_id: "consumer-electronics",
            name: "消费电子：海外品牌财报到供应链",
            global_sources: &["sec_edgar", "hkex"],
            nodes: &[
                "海外品牌分产品收入/指引",
                "产品需求与库存",
                "零部件/组装环节",
                "A股消费电子公司",
            ],
            activation_requirement: "海外公司财报/供应商文件 + A股年报客户/产品证据",
        },
        GoldenChainTemplate {
            chain_id: "new-energy",
            name: "新能源：海外需求/能源政策到材料与设备",
            global_sources: &["sec_edgar", "eia", "eu_trade"],
            nodes: &[
                "海外新能源需求/政策",
                "电池与能源成本",
                "材料/设备订单",
                "A股新能源公司",
            ],
            activation_requirement: "海外正式数据证据 + 国内公司销量/客户/产能披露证据",
        },
        GoldenChainTemplate {
            chain_id: "resources",
            name: "资源品：供需/库存/持仓到盈利敏感度",
            global_sources: &[
                "eia",
                "opec",
                "cftc",
                "sge_gold",
                "safe_reserves",
                "pbc",
                "lbma",
                "world_gold_council",
            ],
            nodes: &[
                "全球供需/库存/持仓",
                "商品价格冲击",
                "收入/成本暴露",
                "A股资源品或下游公司",
            ],
            activation_requirement: "官方商品数据版本 + A股公司品种/成本暴露证据",
        },
    ]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalDocumentInput {
    pub provider_id: String,
    pub upstream_id: String,
    pub document_type: String,
    pub title_original: String,
    pub title_zh: Option<String>,
    pub original_language: String,
    pub original_url: String,
    pub source_version_id: Option<String>,
    pub content_hash: Option<String>,
    pub published_at_utc: i64,
    pub published_local: String,
    pub published_timezone: String,
    pub utc_offset_seconds: i32,
    pub first_seen_at: i64,
    pub revision_no: u32,
    pub revision_of: Option<String>,
    pub translation_status: String,
    pub gap_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalEntity {
    pub entity_id: String,
    pub entity_type: String,
    pub legal_name: String,
    pub name_zh: Option<String>,
    pub jurisdiction: String,
    pub identifiers: serde_json::Value,
    pub aliases: Vec<String>,
    pub translation_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalRelationInput {
    pub src_entity_id: String,
    pub dst_entity_id: String,
    pub relation_type: String,
    pub direction: String,
    pub confidence_bps: u16,
    pub evidence_document_id: String,
    pub evidence_source_version_id: String,
    pub evidence_quote_original: String,
    pub evidence_quote_zh: Option<String>,
    pub evidence_location: serde_json::Value,
    pub observed_at: i64,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalObservationInput {
    pub document_id: String,
    pub entity_id: Option<String>,
    pub indicator_code: String,
    pub period: String,
    pub value_scaled: Option<i64>,
    pub scale: i64,
    pub value_text: Option<String>,
    pub unit_original: String,
    pub currency_original: Option<String>,
    pub released_at_utc: i64,
    pub revision_no: u32,
    pub replaces_observation_id: Option<String>,
    pub source_version_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalProviderRuntime {
    pub provider_id: String,
    pub provider_name: String,
    pub region: String,
    pub category: String,
    pub official_url: String,
    pub original_timezone: String,
    pub license_policy: String,
    pub credential_env: Option<String>,
    pub enabled: bool,
    pub target_latency_secs: u32,
    pub rate_limit_per_minute: u32,
    pub last_attempt_at: Option<i64>,
    pub last_success_at: Option<i64>,
    pub consecutive_failures: u32,
    pub retry_after: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalDocumentListItem {
    pub document_id: String,
    pub provider_id: String,
    pub provider_name: String,
    pub document_type: String,
    pub title_original: String,
    pub title_zh: Option<String>,
    pub original_language: String,
    pub original_url: String,
    pub source_version_id: Option<String>,
    pub published_at_utc: i64,
    pub published_local: String,
    pub published_timezone: String,
    pub revision_no: u32,
    pub primary_verified: bool,
    pub translation_status: String,
    pub gap_reason: Option<String>,
    pub license_policy: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalDocumentQuery {
    pub provider_id: Option<String>,
    pub keyword: Option<String>,
    pub primary_only: bool,
    pub page: u32,
    pub page_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalDocumentPage {
    pub items: Vec<GlobalDocumentListItem>,
    pub total: u64,
    pub page: u32,
    pub page_size: u32,
    pub total_pages: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalRelation {
    pub relation_id: String,
    pub src_entity_id: String,
    pub dst_entity_id: String,
    pub relation_type: String,
    pub direction: String,
    pub confidence_bps: u16,
    pub evidence_document_id: String,
    pub evidence_source_version_id: String,
    pub evidence_quote_original: String,
    pub evidence_quote_zh: Option<String>,
    pub evidence_location: serde_json::Value,
    pub observed_at: i64,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransmissionPath {
    pub path_id: String,
    pub entities: Vec<GlobalEntity>,
    pub relations: Vec<GlobalRelation>,
    pub path_confidence_bps: u16,
    pub target_a_share_code: String,
}

#[derive(Clone)]
pub struct GlobalStore {
    storage: Storage,
}

impl GlobalStore {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    pub async fn seed_provider_catalog(&self) -> Result<()> {
        let catalog = official_global_sources();
        self.storage
            .run(move |conn| {
                let now = now_secs();
                for source in catalog {
                    // This catalog has no credential capability. Credentialed
                    // sources start disabled; Engine overlays actual runtime
                    // availability after its direct Credential Manager read.
                    let enabled = source.credential_env.is_none();
                    let missing = source
                        .credential_env
                        .map(|key| format!("需要在 Windows Credential Manager 中配置 {key}"));
                    conn.execute(
                        "INSERT INTO global_provider_state
                         (provider_id,provider_name,region,category,official_url,original_timezone,
                          license_policy,credential_env,enabled,target_latency_secs,rate_limit_per_minute,
                          last_error,updated_at)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                         ON CONFLICT(provider_id) DO UPDATE SET
                          provider_name=excluded.provider_name,region=excluded.region,
                          category=excluded.category,official_url=excluded.official_url,
                          original_timezone=excluded.original_timezone,license_policy=excluded.license_policy,
                          credential_env=excluded.credential_env,enabled=excluded.enabled,
                          target_latency_secs=excluded.target_latency_secs,
                          rate_limit_per_minute=excluded.rate_limit_per_minute,
                          last_error=CASE WHEN excluded.enabled=0 THEN excluded.last_error ELSE global_provider_state.last_error END,
                          updated_at=excluded.updated_at",
                        params![source.provider_id,source.provider_name,source.region,source.category.token(),
                            source.official_url,source.original_timezone,source.license_policy,source.credential_env,
                            enabled,source.target_latency_secs,source.rate_limit_per_minute,missing,now],
                    )?;
                }
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn ingest_document(&self, input: GlobalDocumentInput) -> Result<String> {
        let provider = official_global_sources()
            .into_iter()
            .find(|source| source.provider_id == input.provider_id)
            .ok_or_else(|| Error::InvalidEvidence(format!("未登记来源 {}", input.provider_id)))?;
        let primary_verified = input.source_version_id.is_some() && input.content_hash.is_some();
        let id = format!(
            "gdoc:{}",
            short_hash(&format!(
                "{}|{}|{}",
                input.provider_id, input.upstream_id, input.revision_no
            ))
        );
        let output = id.clone();
        self.storage
            .run(move |conn| {
                let now = now_secs();
                conn.execute(
                    "INSERT INTO global_documents
                     (document_id,provider_id,upstream_id,document_type,title_original,title_zh,
                      original_language,original_url,source_version_id,content_hash,published_at_utc,
                      published_local,published_timezone,utc_offset_seconds,first_seen_at,revision_no,
                      revision_of,primary_verified,translation_status,gap_reason,license_policy,created_at,updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,
                             ?18,?19,?20,?21,?22,?22)
                     ON CONFLICT(provider_id,upstream_id,revision_no) DO UPDATE SET
                      title_zh=COALESCE(excluded.title_zh,global_documents.title_zh),
                      source_version_id=COALESCE(excluded.source_version_id,global_documents.source_version_id),
                      content_hash=COALESCE(excluded.content_hash,global_documents.content_hash),
                      primary_verified=MAX(global_documents.primary_verified,excluded.primary_verified),
                      translation_status=excluded.translation_status,gap_reason=excluded.gap_reason,updated_at=excluded.updated_at",
                    params![id,input.provider_id,input.upstream_id,input.document_type,input.title_original,
                        input.title_zh,input.original_language,input.original_url,input.source_version_id,input.content_hash,
                        input.published_at_utc,input.published_local,input.published_timezone,input.utc_offset_seconds,
                        input.first_seen_at,input.revision_no,input.revision_of,primary_verified,input.translation_status,
                        input.gap_reason,provider.license_policy,now],
                )?;
                Ok(())
            })
            .await?;
        Ok(output)
    }

    pub async fn upsert_entity(&self, entity: GlobalEntity) -> Result<()> {
        self.storage
            .run(move |conn| {
                conn.execute(
                    "INSERT INTO global_entities
                     (entity_id,entity_type,legal_name,name_zh,jurisdiction,identifiers_json,aliases_json,
                      translation_status,updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                     ON CONFLICT(entity_id) DO UPDATE SET legal_name=excluded.legal_name,
                      name_zh=excluded.name_zh,jurisdiction=excluded.jurisdiction,
                      identifiers_json=excluded.identifiers_json,aliases_json=excluded.aliases_json,
                      translation_status=excluded.translation_status,updated_at=excluded.updated_at",
                    params![entity.entity_id,entity.entity_type,entity.legal_name,entity.name_zh,
                        entity.jurisdiction,serde_json::to_string(&entity.identifiers)?,
                        serde_json::to_string(&entity.aliases)?,entity.translation_status,now_secs()],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Activate an evidence-backed relation. A mere URL or translated media
    /// summary is rejected: the document must have a matching archived source
    /// version and the verbatim original evidence span must be non-empty.
    pub async fn add_relation(&self, input: GlobalRelationInput) -> Result<String> {
        if input.confidence_bps > 10_000 {
            return Err(Error::InvalidEvidence("置信度必须位于 0..10000".into()));
        }
        if input.evidence_source_version_id.trim().is_empty()
            || input.evidence_quote_original.trim().is_empty()
        {
            return Err(Error::InvalidEvidence(
                "缺少 source_version_id 或原文证据片段".into(),
            ));
        }
        let relation_id = format!(
            "grel:{}",
            short_hash(&format!(
                "{}|{}|{}|{}",
                input.src_entity_id,
                input.dst_entity_id,
                input.relation_type,
                input.evidence_source_version_id
            ))
        );
        let output = relation_id.clone();
        self.storage
            .run(move |conn| {
                let verified: Option<i64> = conn
                    .query_row(
                        "SELECT primary_verified FROM global_documents
                         WHERE document_id=?1 AND source_version_id=?2",
                        params![input.evidence_document_id, input.evidence_source_version_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if verified != Some(1) {
                    return Err(astock_storage::Error::Invalid(
                        "关系只能引用已归档的海外一级来源版本".into(),
                    ));
                }
                let now = now_secs();
                conn.execute(
                    "INSERT INTO global_relations
                     (relation_id,src_entity_id,dst_entity_id,relation_type,direction,confidence_bps,
                      evidence_document_id,evidence_source_version_id,evidence_quote_original,evidence_quote_zh,
                      evidence_location_json,observed_at,valid_from,valid_to,status,created_at,updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,'active',?15,?15)
                     ON CONFLICT(src_entity_id,dst_entity_id,relation_type,evidence_source_version_id)
                     DO UPDATE SET confidence_bps=excluded.confidence_bps,
                      evidence_quote_original=excluded.evidence_quote_original,
                      evidence_quote_zh=excluded.evidence_quote_zh,
                      evidence_location_json=excluded.evidence_location_json,
                      observed_at=excluded.observed_at,valid_from=excluded.valid_from,
                      valid_to=excluded.valid_to,status='active',updated_at=excluded.updated_at",
                    params![relation_id,input.src_entity_id,input.dst_entity_id,input.relation_type,
                        input.direction,input.confidence_bps,input.evidence_document_id,
                        input.evidence_source_version_id,input.evidence_quote_original,input.evidence_quote_zh,
                        serde_json::to_string(&input.evidence_location)?,input.observed_at,input.valid_from,
                        input.valid_to,now],
                )?;
                Ok(())
            })
            .await?;
        Ok(output)
    }

    pub async fn ingest_observation(&self, input: GlobalObservationInput) -> Result<String> {
        if input.scale <= 0 || input.source_version_id.trim().is_empty() {
            return Err(Error::InvalidEvidence(
                "观测值必须声明正数 scale 与 source_version_id".into(),
            ));
        }
        let observation_id = format!(
            "gobs:{}",
            short_hash(&format!(
                "{}|{}|{}|{}",
                input.document_id, input.indicator_code, input.period, input.revision_no
            ))
        );
        let output = observation_id.clone();
        self.storage
            .run(move |conn| {
                let valid: Option<i64> = conn
                    .query_row(
                        "SELECT primary_verified FROM global_documents
                         WHERE document_id=?1 AND source_version_id=?2",
                        params![input.document_id, input.source_version_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if valid != Some(1) {
                    return Err(astock_storage::Error::Invalid(
                        "观测值只能引用已归档的官方数据版本".into(),
                    ));
                }
                conn.execute(
                    "INSERT INTO global_observations
                     (observation_id,document_id,entity_id,indicator_code,period,value_scaled,scale,
                      value_text,unit_original,currency_original,released_at_utc,revision_no,
                      replaces_observation_id,source_version_id,created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
                     ON CONFLICT(document_id,indicator_code,period,revision_no) DO UPDATE SET
                      value_scaled=excluded.value_scaled,value_text=excluded.value_text,
                      released_at_utc=excluded.released_at_utc,source_version_id=excluded.source_version_id",
                    params![observation_id,input.document_id,input.entity_id,input.indicator_code,
                        input.period,input.value_scaled,input.scale,input.value_text,input.unit_original,
                        input.currency_original,input.released_at_utc,input.revision_no,
                        input.replaces_observation_id,input.source_version_id,now_secs()],
                )?;
                Ok(())
            })
            .await?;
        Ok(output)
    }

    pub async fn record_provider_success(&self, provider_id: &str) -> Result<()> {
        let provider_id = provider_id.to_string();
        self.storage
            .run(move |conn| {
                let now = now_secs();
                conn.execute(
                    "UPDATE global_provider_state SET last_attempt_at=?1,last_success_at=?1,
                     consecutive_failures=0,retry_after=NULL,last_error=NULL,updated_at=?1
                     WHERE provider_id=?2",
                    params![now, provider_id],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn record_provider_failure(&self, provider_id: &str, message: &str) -> Result<()> {
        let provider_id = provider_id.to_string();
        let message = message.to_string();
        self.storage
            .run(move |conn| {
                let now = now_secs();
                let failures: u32 = conn
                    .query_row(
                        "SELECT consecutive_failures FROM global_provider_state WHERE provider_id=?1",
                        [&provider_id],
                        |row| row.get(0),
                    )
                    .unwrap_or(0_u32)
                    .saturating_add(1);
                let delay = 30_i64.saturating_mul(1_i64 << failures.min(10));
                conn.execute(
                    "UPDATE global_provider_state SET last_attempt_at=?1,consecutive_failures=?2,
                     retry_after=?3,last_error=?4,updated_at=?1 WHERE provider_id=?5",
                    params![now, failures, now.saturating_add(delay), message, provider_id],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn provider_health(&self) -> Result<Vec<GlobalProviderRuntime>> {
        self.seed_provider_catalog().await?;
        self.storage
            .run(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT provider_id,provider_name,region,category,official_url,original_timezone,
                            license_policy,credential_env,enabled,target_latency_secs,rate_limit_per_minute,
                            last_attempt_at,last_success_at,consecutive_failures,retry_after,last_error
                     FROM global_provider_state ORDER BY category,region,provider_name",
                )?;
                let rows = stmt.query_map([], |row| {
                    Ok(GlobalProviderRuntime {
                        provider_id: row.get(0)?, provider_name: row.get(1)?, region: row.get(2)?,
                        category: row.get(3)?, official_url: row.get(4)?, original_timezone: row.get(5)?,
                        license_policy: row.get(6)?, credential_env: row.get(7)?, enabled: row.get::<_, i64>(8)? != 0,
                        target_latency_secs: row.get(9)?, rate_limit_per_minute: row.get(10)?,
                        last_attempt_at: row.get(11)?, last_success_at: row.get(12)?,
                        consecutive_failures: row.get(13)?, retry_after: row.get(14)?, last_error: row.get(15)?,
                    })
                })?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .map_err(Into::into)
    }

    pub async fn query_documents(&self, query: GlobalDocumentQuery) -> Result<GlobalDocumentPage> {
        let page = query.page.max(1);
        let page_size = query.page_size.clamp(10, 100);
        let provider = query.provider_id.unwrap_or_default();
        let keyword = query.keyword.unwrap_or_default();
        let primary_only = query.primary_only;
        self.storage
            .run(move |conn| {
                let mut where_sql = vec!["1=1".to_string()];
                let mut values = Vec::<rusqlite::types::Value>::new();
                if !provider.is_empty() && provider != "all" {
                    values.push(provider.into());
                    where_sql.push(format!("d.provider_id=?{}", values.len()));
                }
                if !keyword.is_empty() {
                    values.push(format!("%{keyword}%").into());
                    where_sql.push(format!("(d.title_original LIKE ?{0} OR d.title_zh LIKE ?{0})", values.len()));
                }
                if primary_only { where_sql.push("d.primary_verified=1".into()); }
                let where_sql = where_sql.join(" AND ");
                let total_sql = format!("SELECT COUNT(*) FROM global_documents d WHERE {where_sql}");
                let total: u64 = conn.query_row(&total_sql, rusqlite::params_from_iter(values.iter()), |row| row.get(0))?;
                let mut page_values = values;
                page_values.push(i64::from(page_size).into());
                let limit_index = page_values.len();
                page_values.push((i64::from(page - 1) * i64::from(page_size)).into());
                let sql = format!(
                    "SELECT d.document_id,d.provider_id,p.provider_name,d.document_type,d.title_original,
                            d.title_zh,d.original_language,d.original_url,d.source_version_id,
                            d.published_at_utc,d.published_local,d.published_timezone,d.revision_no,
                            d.primary_verified,d.translation_status,d.gap_reason,d.license_policy
                     FROM global_documents d JOIN global_provider_state p ON p.provider_id=d.provider_id
                     WHERE {where_sql} ORDER BY d.published_at_utc DESC,d.first_seen_at DESC
                     LIMIT ?{limit_index} OFFSET ?{}",
                    limit_index + 1
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map(rusqlite::params_from_iter(page_values.iter()), |row| {
                    Ok(GlobalDocumentListItem {
                        document_id: row.get(0)?, provider_id: row.get(1)?, provider_name: row.get(2)?,
                        document_type: row.get(3)?, title_original: row.get(4)?, title_zh: row.get(5)?,
                        original_language: row.get(6)?, original_url: row.get(7)?, source_version_id: row.get(8)?,
                        published_at_utc: row.get(9)?, published_local: row.get(10)?, published_timezone: row.get(11)?,
                        revision_no: row.get(12)?, primary_verified: row.get::<_, i64>(13)? != 0,
                        translation_status: row.get(14)?, gap_reason: row.get(15)?, license_policy: row.get(16)?,
                    })
                })?;
                let items = rows.collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(GlobalDocumentPage { items, total, page, page_size,
                    total_pages: if total == 0 { 0 } else { total.div_ceil(u64::from(page_size)) as u32 } })
            })
            .await
            .map_err(Into::into)
    }

    pub async fn transmission_paths(
        &self,
        root_entity_id: &str,
        as_of: i64,
        max_depth: usize,
    ) -> Result<Vec<TransmissionPath>> {
        let root = root_entity_id.to_string();
        let max_depth = max_depth.clamp(1, 8);
        self.storage
            .run(move |conn| {
                let mut entities = HashMap::new();
                let mut stmt = conn.prepare(
                    "SELECT entity_id,entity_type,legal_name,name_zh,jurisdiction,identifiers_json,
                            aliases_json,translation_status FROM global_entities",
                )?;
                let rows = stmt.query_map([], |row| {
                    Ok(GlobalEntity {
                        entity_id: row.get(0)?,
                        entity_type: row.get(1)?,
                        legal_name: row.get(2)?,
                        name_zh: row.get(3)?,
                        jurisdiction: row.get(4)?,
                        identifiers: serde_json::from_str(&row.get::<_, String>(5)?)
                            .unwrap_or_default(),
                        aliases: serde_json::from_str(&row.get::<_, String>(6)?)
                            .unwrap_or_default(),
                        translation_status: row.get(7)?,
                    })
                })?;
                for row in rows {
                    let entity = row?;
                    entities.insert(entity.entity_id.clone(), entity);
                }

                let mut relations = Vec::new();
                let mut stmt = conn.prepare(
                    "SELECT relation_id,src_entity_id,dst_entity_id,relation_type,direction,
                            confidence_bps,evidence_document_id,evidence_source_version_id,
                            evidence_quote_original,evidence_quote_zh,evidence_location_json,
                            observed_at,valid_from,valid_to
                     FROM global_relations WHERE status='active' AND valid_from<=?1
                       AND (valid_to IS NULL OR valid_to>?1)",
                )?;
                let rows = stmt.query_map([as_of], |row| {
                    Ok(GlobalRelation {
                        relation_id: row.get(0)?,
                        src_entity_id: row.get(1)?,
                        dst_entity_id: row.get(2)?,
                        relation_type: row.get(3)?,
                        direction: row.get(4)?,
                        confidence_bps: row.get(5)?,
                        evidence_document_id: row.get(6)?,
                        evidence_source_version_id: row.get(7)?,
                        evidence_quote_original: row.get(8)?,
                        evidence_quote_zh: row.get(9)?,
                        evidence_location: serde_json::from_str(&row.get::<_, String>(10)?)
                            .unwrap_or_default(),
                        observed_at: row.get(11)?,
                        valid_from: row.get(12)?,
                        valid_to: row.get(13)?,
                    })
                })?;
                for row in rows {
                    relations.push(row?);
                }
                Ok(build_paths(&root, &entities, &relations, max_depth))
            })
            .await
            .map_err(Into::into)
    }
}

fn build_paths(
    root: &str,
    entities: &HashMap<String, GlobalEntity>,
    relations: &[GlobalRelation],
    max_depth: usize,
) -> Vec<TransmissionPath> {
    let mut output = Vec::new();
    let Some(root_entity) = entities.get(root).cloned() else {
        return output;
    };
    let mut frontier = vec![(
        root.to_string(),
        vec![root_entity],
        Vec::<GlobalRelation>::new(),
        10_000_u16,
        HashSet::from([root.to_string()]),
    )];
    while let Some((current, path_entities, path_relations, confidence, visited)) = frontier.pop() {
        if path_relations.len() >= max_depth {
            continue;
        }
        for relation in relations
            .iter()
            .filter(|edge| edge.src_entity_id == current)
        {
            if visited.contains(&relation.dst_entity_id) {
                continue;
            }
            let Some(next_entity) = entities.get(&relation.dst_entity_id).cloned() else {
                continue;
            };
            let mut next_entities = path_entities.clone();
            next_entities.push(next_entity.clone());
            let mut next_relations = path_relations.clone();
            next_relations.push(relation.clone());
            let next_confidence =
                ((u32::from(confidence) * u32::from(relation.confidence_bps)) / 10_000) as u16;
            if next_entity.entity_type == "a_share_security" {
                let code = next_entity
                    .identifiers
                    .get("code")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                output.push(TransmissionPath {
                    path_id: format!(
                        "gpath:{}",
                        short_hash(
                            &next_relations
                                .iter()
                                .map(|r| r.relation_id.as_str())
                                .collect::<Vec<_>>()
                                .join("|")
                        )
                    ),
                    entities: next_entities,
                    relations: next_relations,
                    path_confidence_bps: next_confidence,
                    target_a_share_code: code,
                });
            } else {
                let mut next_visited = visited.clone();
                next_visited.insert(relation.dst_entity_id.clone());
                frontier.push((
                    relation.dst_entity_id.clone(),
                    next_entities,
                    next_relations,
                    next_confidence,
                    next_visited,
                ));
            }
        }
    }
    output.sort_by(|a, b| {
        b.path_confidence_bps
            .cmp(&a.path_confidence_bps)
            .then_with(|| a.path_id.cmp(&b.path_id))
    });
    output
}

fn short_hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))[..28].to_string()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_storage::StorageConfig;
    use chrono::NaiveDate;

    fn entity(id: &str, kind: &str, code: Option<&str>) -> GlobalEntity {
        GlobalEntity {
            entity_id: id.into(),
            entity_type: kind.into(),
            legal_name: id.into(),
            name_zh: None,
            jurisdiction: if kind == "a_share_security" {
                "CN"
            } else {
                "US"
            }
            .into(),
            identifiers: code
                .map(|value| serde_json::json!({"code": value}))
                .unwrap_or_default(),
            aliases: Vec::new(),
            translation_status: "not_required".into(),
        }
    }

    #[test]
    fn credentialed_sources_use_opaque_credential_manager_slots() {
        let catalog = official_global_sources();
        let credential_ids = catalog
            .iter()
            .filter_map(|source| source.credential_env)
            .collect::<Vec<_>>();
        assert!(!credential_ids.is_empty());
        assert!(credential_ids.iter().all(|id| id.starts_with("provider-")));
        assert!(credential_ids
            .iter()
            .all(|id| !id.contains('_') && !id.chars().any(char::is_uppercase)));
    }

    #[test]
    fn daylight_saving_ambiguity_and_gap_are_explicit() {
        let ambiguous =
            NaiveDateTime::parse_from_str("2026-11-01 01:30:00", "%Y-%m-%d %H:%M:%S").unwrap();
        assert!(matches!(
            normalize_local_publication(ambiguous, "America/New_York", DstDisambiguation::Reject),
            Err(Error::AmbiguousLocalTime { .. })
        ));
        let earlier =
            normalize_local_publication(ambiguous, "America/New_York", DstDisambiguation::Earlier)
                .unwrap();
        let later =
            normalize_local_publication(ambiguous, "America/New_York", DstDisambiguation::Later)
                .unwrap();
        assert_eq!(later.utc_timestamp - earlier.utc_timestamp, 3_600);

        let gap =
            NaiveDateTime::parse_from_str("2026-03-08 02:30:00", "%Y-%m-%d %H:%M:%S").unwrap();
        assert!(matches!(
            normalize_local_publication(gap, "America/New_York", DstDisambiguation::Reject),
            Err(Error::NonexistentLocalTime { .. })
        ));
    }

    #[test]
    fn us_after_hours_release_maps_to_next_a_share_session() {
        let rules = RuleSet::from_json(astock_trading_rules::EMBEDDED_RULES_JSON).unwrap();
        let local =
            NaiveDateTime::parse_from_str("2026-08-21 16:05:00", "%Y-%m-%d %H:%M:%S").unwrap();
        let clock =
            normalize_local_publication(local, "America/New_York", DstDisambiguation::Reject)
                .unwrap();
        let session =
            map_global_release_to_a_share(&rules, &clock, clock.utc_timestamp, true).unwrap();
        assert_eq!(
            session.target_trading_date,
            NaiveDate::from_ymd_opt(2026, 8, 24).unwrap()
        );
    }

    #[test]
    fn translation_protects_numbers_codes_entities_and_terms() {
        let protected = vec!["Apple Inc.".into(), "gross margin".into()];
        validate_translation(
            "Apple Inc. reported USD 94.9 billion, +5.2%.",
            "Apple Inc. 报告 USD 94.9 billion，+5.2%，gross margin 改善。",
            &protected,
        )
        .unwrap();
        let error =
            validate_translation("Revenue was USD 94.9 billion.", "收入约 95 亿美元。", &[])
                .unwrap_err();
        assert!(matches!(error, Error::TranslationProtection(_)));
    }

    #[test]
    fn currency_conversion_is_fixed_point_and_revisions_are_pit() {
        // USD 12.34 * 7.1234 CNY/USD = CNY 87.900756 -> 87.90
        assert_eq!(
            convert_currency_scaled(1_234, 100, 71_234, 10_000, 100).unwrap(),
            8_790
        );
        let revisions = vec![
            RevisionPoint {
                revision_no: 1,
                released_at_utc: 100,
                value_scaled: 10,
            },
            RevisionPoint {
                revision_no: 2,
                released_at_utc: 200,
                value_scaled: 12,
            },
        ];
        assert_eq!(
            point_in_time_revision(&revisions, 150)
                .unwrap()
                .value_scaled,
            10
        );
        assert_eq!(
            point_in_time_revision(&revisions, 250)
                .unwrap()
                .value_scaled,
            12
        );
    }

    #[test]
    fn four_golden_chains_require_two_sided_primary_evidence() {
        let chains = global_a_share_golden_chains();
        assert_eq!(chains.len(), 4);
        assert!(chains
            .iter()
            .all(|chain| chain.nodes.len() == 4 && chain.activation_requirement.contains("证据")));
    }

    #[tokio::test]
    async fn relation_rejects_unarchived_document_and_path_keeps_every_evidence_edge() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(StorageConfig::with_base_dir(dir.path())).unwrap();
        let store = GlobalStore::new(storage);
        store.seed_provider_catalog().await.unwrap();
        let unverified = store
            .ingest_document(GlobalDocumentInput {
                provider_id: "sec_edgar".into(),
                upstream_id: "x".into(),
                document_type: "10-k".into(),
                title_original: "Official filing".into(),
                title_zh: None,
                original_language: "en".into(),
                original_url: "https://www.sec.gov/Archives/x".into(),
                source_version_id: None,
                content_hash: None,
                published_at_utc: 100,
                published_local: "1970-01-01 00:01:40".into(),
                published_timezone: "UTC".into(),
                utc_offset_seconds: 0,
                first_seen_at: 100,
                revision_no: 1,
                revision_of: None,
                translation_status: "pending".into(),
                gap_reason: Some("archive missing".into()),
            })
            .await
            .unwrap();
        for item in [
            entity("foreign:a", "legal_entity", None),
            entity("product:p", "product", None),
            entity("cn:600000", "a_share_security", Some("600000")),
        ] {
            store.upsert_entity(item).await.unwrap();
        }
        let rejected = store
            .add_relation(GlobalRelationInput {
                src_entity_id: "foreign:a".into(),
                dst_entity_id: "product:p".into(),
                relation_type: "sells".into(),
                direction: "positive".into(),
                confidence_bps: 8000,
                evidence_document_id: unverified,
                evidence_source_version_id: "missing".into(),
                evidence_quote_original: "official quote".into(),
                evidence_quote_zh: None,
                evidence_location: serde_json::json!({"page": 1}),
                observed_at: 100,
                valid_from: 100,
                valid_to: None,
            })
            .await;
        assert!(rejected.is_err());

        let document = store
            .ingest_document(GlobalDocumentInput {
                provider_id: "sec_edgar".into(),
                upstream_id: "y".into(),
                document_type: "10-k".into(),
                title_original: "Official filing".into(),
                title_zh: None,
                original_language: "en".into(),
                original_url: "https://www.sec.gov/Archives/y".into(),
                source_version_id: Some("sv:1".into()),
                content_hash: Some("abc".into()),
                published_at_utc: 100,
                published_local: "1970-01-01 00:01:40".into(),
                published_timezone: "UTC".into(),
                utc_offset_seconds: 0,
                first_seen_at: 100,
                revision_no: 1,
                revision_of: None,
                translation_status: "verified".into(),
                gap_reason: None,
            })
            .await
            .unwrap();
        for (src, dst, relation) in [
            ("foreign:a", "product:p", "sells"),
            ("product:p", "cn:600000", "supplied_by"),
        ] {
            store
                .add_relation(GlobalRelationInput {
                    src_entity_id: src.into(),
                    dst_entity_id: dst.into(),
                    relation_type: relation.into(),
                    direction: "positive".into(),
                    confidence_bps: 8000,
                    evidence_document_id: document.clone(),
                    evidence_source_version_id: "sv:1".into(),
                    evidence_quote_original: "verbatim official evidence".into(),
                    evidence_quote_zh: None,
                    evidence_location: serde_json::json!({"page": 1}),
                    observed_at: 100,
                    valid_from: 100,
                    valid_to: None,
                })
                .await
                .unwrap();
        }
        let paths = store.transmission_paths("foreign:a", 200, 4).await.unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].target_a_share_code, "600000");
        assert_eq!(paths[0].relations.len(), 2);
        assert_eq!(paths[0].path_confidence_bps, 6_400);
    }
}
