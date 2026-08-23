//! Unified data envelopes, dataset-specific freshness rules and reconciliation.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Dataset families have different update rhythms and must never share one TTL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetKind {
    RealtimeQuote,
    IntradayMinute,
    DailyKline,
    WeeklyKline,
    MonthlyKline,
    FundFlow,
    Fundamentals,
    Valuation,
    Announcement,
    News,
    KnowledgeGraph,
    Macro,
    Backtest,
    SearchDiscovery,
    Other,
}

impl DatasetKind {
    pub fn chinese_name(self) -> &'static str {
        match self {
            Self::RealtimeQuote => "实时行情",
            Self::IntradayMinute => "分时行情",
            Self::DailyKline => "日线",
            Self::WeeklyKline => "周线",
            Self::MonthlyKline => "月线",
            Self::FundFlow => "资金流",
            Self::Fundamentals => "财务报表",
            Self::Valuation => "估值",
            Self::Announcement => "正式公告",
            Self::News => "财经资讯",
            Self::KnowledgeGraph => "产业链图谱",
            Self::Macro => "宏观数据",
            Self::Backtest => "回测结果",
            Self::SearchDiscovery => "搜索发现",
            Self::Other => "其他数据",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataUnit {
    Price,
    Money,
    Percentage,
    Ratio,
    PerShare,
    Shares,
    Lots,
    FundUnits,
    Count,
    Tonnes,
    Megawatts,
    MegawattHours,
    Date,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Currency {
    Cny,
    Usd,
    Hkd,
    Eur,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdjustmentBasis {
    None,
    Forward,
    Backward,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountingScope {
    Consolidated,
    ParentCompany,
    PerShare,
    Total,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualitySeverity {
    Info,
    Warning,
    Blocking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityFlagCode {
    Stale,
    HardExpired,
    SourceConflict,
    MissingField,
    UnitMismatch,
    CurrencyMismatch,
    AdjustmentMismatch,
    AccountingScopeMismatch,
    Unverified,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityFlag {
    pub code: QualityFlagCode,
    pub severity: QualitySeverity,
    pub field: Option<String>,
    pub message: String,
}

impl QualityFlag {
    pub fn blocking(
        code: QualityFlagCode,
        field: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity: QualitySeverity::Blocking,
            field: field.map(str::to_string),
            message: message.into(),
        }
    }

    pub fn warning(code: QualityFlagCode, field: Option<&str>, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: QualitySeverity::Warning,
            field: field.map(str::to_string),
            message: message.into(),
        }
    }
}

/// A value plus its complete source, temporal, schema and accounting contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataEnvelope<T> {
    pub data: T,
    pub dataset: DatasetKind,
    pub source: String,
    pub source_url: Option<String>,
    pub event_time: Option<DateTime<Utc>>,
    pub as_of_time: Option<DateTime<Utc>>,
    pub publish_time: Option<DateTime<Utc>>,
    pub fetched_at: DateTime<Utc>,
    pub parser_version: String,
    pub schema_version: String,
    pub license: String,
    #[serde(default)]
    pub quality_flags: Vec<QualityFlag>,
    pub unit: Option<DataUnit>,
    pub currency: Option<Currency>,
    pub adjustment: AdjustmentBasis,
    pub revision: Option<String>,
    pub accounting_scope: AccountingScope,
}

impl<T> DataEnvelope<T> {
    /// Reject incompatible unit/currency/adjustment/accounting combinations.
    pub fn validate_contract(&self) -> Vec<QualityFlag> {
        let mut flags = Vec::new();
        if matches!(
            self.unit,
            Some(DataUnit::Price | DataUnit::Money | DataUnit::PerShare)
        ) && self.currency.is_none()
        {
            flags.push(QualityFlag::blocking(
                QualityFlagCode::CurrencyMismatch,
                None,
                "价格、金额或每股值必须声明币种",
            ));
        }
        if matches!(
            self.unit,
            Some(
                DataUnit::Percentage
                    | DataUnit::Ratio
                    | DataUnit::Shares
                    | DataUnit::Lots
                    | DataUnit::FundUnits
                    | DataUnit::Count
                    | DataUnit::Tonnes
                    | DataUnit::Megawatts
                    | DataUnit::MegawattHours
                    | DataUnit::Date
                    | DataUnit::Text
            )
        ) && self.currency.is_some()
        {
            flags.push(QualityFlag::blocking(
                QualityFlagCode::CurrencyMismatch,
                None,
                "非金额字段不能携带币种",
            ));
        }
        if self.dataset == DatasetKind::DailyKline
            && self.adjustment == AdjustmentBasis::NotApplicable
        {
            flags.push(QualityFlag::blocking(
                QualityFlagCode::AdjustmentMismatch,
                None,
                "价格序列必须声明不复权、前复权或后复权",
            ));
        }
        if self.unit == Some(DataUnit::PerShare)
            && self.accounting_scope != AccountingScope::PerShare
        {
            flags.push(QualityFlag::blocking(
                QualityFlagCode::AccountingScopeMismatch,
                None,
                "每股字段必须使用每股口径",
            ));
        }
        flags
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRule {
    TradingSession,
    TradingDayClose,
    Calendar,
    DisclosureDriven,
    Versioned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreshnessPolicy {
    pub expected_cadence_secs: u64,
    pub stale_after_secs: u64,
    pub hard_expiry_secs: u64,
    pub session_rule: SessionRule,
}

impl FreshnessPolicy {
    pub fn for_dataset(dataset: DatasetKind) -> Self {
        match dataset {
            DatasetKind::RealtimeQuote => Self::new(2, 30, 300, SessionRule::TradingSession),
            DatasetKind::IntradayMinute => Self::new(60, 180, 900, SessionRule::TradingSession),
            DatasetKind::DailyKline | DatasetKind::FundFlow => {
                Self::new(86_400, 129_600, 604_800, SessionRule::TradingDayClose)
            }
            DatasetKind::WeeklyKline => {
                Self::new(604_800, 777_600, 2_419_200, SessionRule::TradingDayClose)
            }
            DatasetKind::MonthlyKline => Self::new(
                2_419_200,
                3_196_800,
                8_035_200,
                SessionRule::TradingDayClose,
            ),
            DatasetKind::Fundamentals => Self::new(
                7_776_000,
                10_368_000,
                47_520_000,
                SessionRule::DisclosureDriven,
            ),
            DatasetKind::Valuation => {
                Self::new(86_400, 172_800, 604_800, SessionRule::TradingDayClose)
            }
            DatasetKind::Announcement => {
                Self::new(300, 86_400, 604_800, SessionRule::DisclosureDriven)
            }
            DatasetKind::News => Self::new(60, 600, 86_400, SessionRule::Calendar),
            DatasetKind::KnowledgeGraph => {
                Self::new(2_592_000, 15_552_000, 63_072_000, SessionRule::Versioned)
            }
            DatasetKind::Macro => Self::new(
                2_592_000,
                5_184_000,
                31_536_000,
                SessionRule::DisclosureDriven,
            ),
            DatasetKind::Backtest => Self::new(0, 0, 0, SessionRule::Versioned),
            DatasetKind::SearchDiscovery => Self::new(60, 300, 3_600, SessionRule::Calendar),
            DatasetKind::Other => Self::new(3_600, 86_400, 604_800, SessionRule::Calendar),
        }
    }

    const fn new(
        expected_cadence_secs: u64,
        stale_after_secs: u64,
        hard_expiry_secs: u64,
        session_rule: SessionRule,
    ) -> Self {
        Self {
            expected_cadence_secs,
            stale_after_secs,
            hard_expiry_secs,
            session_rule,
        }
    }

    pub fn evaluate(self, age_secs: u64, in_trading_session: bool) -> FreshnessState {
        if self.session_rule == SessionRule::Versioned || self.hard_expiry_secs == 0 {
            return FreshnessState::Fresh;
        }
        if self.session_rule == SessionRule::TradingSession && !in_trading_session {
            return if age_secs >= self.hard_expiry_secs.max(86_400) {
                FreshnessState::Expired
            } else {
                FreshnessState::Fresh
            };
        }
        if age_secs >= self.hard_expiry_secs {
            FreshnessState::Expired
        } else if age_secs >= self.stale_after_secs {
            FreshnessState::Stale
        } else {
            FreshnessState::Fresh
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessState {
    Fresh,
    Stale,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceCeiling {
    High,
    Medium,
    Low,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataQualitySummary {
    pub dataset: DatasetKind,
    pub dataset_name: String,
    pub freshness: FreshnessState,
    pub age_secs: u64,
    pub expected_cadence_secs: u64,
    pub stale_after_secs: u64,
    pub hard_expiry_secs: u64,
    pub quality_flags: Vec<QualityFlag>,
    pub confidence_ceiling: ConfidenceCeiling,
    pub allow_high_confidence: bool,
    pub allow_deterministic_compute: bool,
}

impl DataQualitySummary {
    pub fn evaluate(
        dataset: DatasetKind,
        age_secs: u64,
        in_trading_session: bool,
        mut flags: Vec<QualityFlag>,
    ) -> Self {
        let policy = FreshnessPolicy::for_dataset(dataset);
        let freshness = policy.evaluate(age_secs, in_trading_session);
        match freshness {
            FreshnessState::Fresh => {}
            FreshnessState::Stale => flags.push(QualityFlag::warning(
                QualityFlagCode::Stale,
                None,
                format!("数据已超过陈旧阈值 {} 秒", policy.stale_after_secs),
            )),
            FreshnessState::Expired => flags.push(QualityFlag::blocking(
                QualityFlagCode::HardExpired,
                None,
                format!("数据已超过硬过期阈值 {} 秒", policy.hard_expiry_secs),
            )),
        }
        let blocking = flags
            .iter()
            .any(|flag| flag.severity == QualitySeverity::Blocking);
        let warning = flags
            .iter()
            .any(|flag| flag.severity == QualitySeverity::Warning);
        let confidence_ceiling = if blocking {
            ConfidenceCeiling::Blocked
        } else if freshness == FreshnessState::Stale || warning {
            ConfidenceCeiling::Medium
        } else {
            ConfidenceCeiling::High
        };
        Self {
            dataset,
            dataset_name: dataset.chinese_name().into(),
            freshness,
            age_secs,
            expected_cadence_secs: policy.expected_cadence_secs,
            stale_after_secs: policy.stale_after_secs,
            hard_expiry_secs: policy.hard_expiry_secs,
            quality_flags: flags,
            confidence_ceiling,
            allow_high_confidence: confidence_ceiling == ConfidenceCeiling::High,
            allow_deterministic_compute: !blocking,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NumericObservation {
    pub provider: String,
    pub field: String,
    pub value: f64,
    pub unit: DataUnit,
    pub currency: Option<Currency>,
    pub adjustment: AdjustmentBasis,
    pub accounting_scope: AccountingScope,
    pub as_of_time: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ReconciliationTolerance {
    pub absolute: f64,
    pub relative: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationStatus {
    Matched,
    WithinTolerance,
    Conflict,
    IncompatibleContract,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReconciliationResult {
    pub field: String,
    pub left: NumericObservation,
    pub right: NumericObservation,
    pub absolute_diff: f64,
    pub relative_diff: f64,
    pub tolerance: ReconciliationTolerance,
    pub status: ReconciliationStatus,
    pub explanation: String,
    pub quality_flags: Vec<QualityFlag>,
}

pub fn reconcile_numeric(
    left: NumericObservation,
    right: NumericObservation,
    tolerance: ReconciliationTolerance,
) -> ReconciliationResult {
    let absolute_diff = (left.value - right.value).abs();
    let relative_diff = absolute_diff / left.value.abs().max(right.value.abs()).max(f64::EPSILON);
    let mut flags = Vec::new();
    if left.unit != right.unit {
        flags.push(QualityFlag::blocking(
            QualityFlagCode::UnitMismatch,
            Some(&left.field),
            "跨源字段单位不一致，禁止自动换算后掩盖冲突",
        ));
    }
    if left.currency != right.currency {
        flags.push(QualityFlag::blocking(
            QualityFlagCode::CurrencyMismatch,
            Some(&left.field),
            "跨源字段币种不一致",
        ));
    }
    if left.adjustment != right.adjustment {
        flags.push(QualityFlag::blocking(
            QualityFlagCode::AdjustmentMismatch,
            Some(&left.field),
            "跨源字段复权口径不一致",
        ));
    }
    if left.accounting_scope != right.accounting_scope {
        flags.push(QualityFlag::blocking(
            QualityFlagCode::AccountingScopeMismatch,
            Some(&left.field),
            "跨源字段合并/母公司或每股/总额口径不一致",
        ));
    }
    let status = if !flags.is_empty() {
        ReconciliationStatus::IncompatibleContract
    } else if absolute_diff <= f64::EPSILON {
        ReconciliationStatus::Matched
    } else if absolute_diff <= tolerance.absolute || relative_diff <= tolerance.relative {
        ReconciliationStatus::WithinTolerance
    } else {
        flags.push(QualityFlag::blocking(
            QualityFlagCode::SourceConflict,
            Some(&left.field),
            format!(
                "跨源偏差 {:.4}% 超过相对容差 {:.4}%",
                relative_diff * 100.0,
                tolerance.relative * 100.0
            ),
        ));
        ReconciliationStatus::Conflict
    };
    let explanation = match status {
        ReconciliationStatus::Matched => "两个来源取值完全一致",
        ReconciliationStatus::WithinTolerance => "偏差在声明的绝对或相对容差内，保留双方原值",
        ReconciliationStatus::Conflict => "偏差超过容差，阻止升级为高置信结论",
        ReconciliationStatus::IncompatibleContract => {
            "单位、币种、复权或财务口径不兼容，禁止直接比较"
        }
    }
    .into();
    ReconciliationResult {
        field: left.field.clone(),
        left,
        right,
        absolute_diff,
        relative_diff,
        tolerance,
        status,
        explanation,
        quality_flags: flags,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(provider: &str, value: f64) -> NumericObservation {
        NumericObservation {
            provider: provider.into(),
            field: "close".into(),
            value,
            unit: DataUnit::Price,
            currency: Some(Currency::Cny),
            adjustment: AdjustmentBasis::Forward,
            accounting_scope: AccountingScope::NotApplicable,
            as_of_time: None,
        }
    }

    #[test]
    fn dataset_policies_differ_and_hard_expiry_blocks() {
        let quote = FreshnessPolicy::for_dataset(DatasetKind::RealtimeQuote);
        let fundamentals = FreshnessPolicy::for_dataset(DatasetKind::Fundamentals);
        assert_ne!(quote.stale_after_secs, fundamentals.stale_after_secs);
        let summary = DataQualitySummary::evaluate(
            DatasetKind::RealtimeQuote,
            quote.hard_expiry_secs,
            true,
            Vec::new(),
        );
        assert_eq!(summary.freshness, FreshnessState::Expired);
        assert_eq!(summary.confidence_ceiling, ConfidenceCeiling::Blocked);
        assert!(!summary.allow_deterministic_compute);
    }

    #[test]
    fn fault_injection_rejects_unit_currency_adjustment_and_scope_mismatches() {
        let left = observation("tdx", 10.0);
        let mut right = observation("eastmoney", 10.0);
        right.unit = DataUnit::Money;
        right.currency = Some(Currency::Usd);
        right.adjustment = AdjustmentBasis::Backward;
        right.accounting_scope = AccountingScope::ParentCompany;
        let result = reconcile_numeric(
            left,
            right,
            ReconciliationTolerance {
                absolute: 0.01,
                relative: 0.001,
            },
        );
        assert_eq!(result.status, ReconciliationStatus::IncompatibleContract);
        assert_eq!(result.quality_flags.len(), 4);
    }

    #[test]
    fn cross_source_deviation_beyond_tolerance_is_blocking() {
        let result = reconcile_numeric(
            observation("tdx", 10.0),
            observation("eastmoney", 10.5),
            ReconciliationTolerance {
                absolute: 0.01,
                relative: 0.002,
            },
        );
        assert_eq!(result.status, ReconciliationStatus::Conflict);
        assert!(result
            .quality_flags
            .iter()
            .any(|flag| flag.code == QualityFlagCode::SourceConflict));
    }

    #[test]
    fn envelope_contract_detects_missing_currency_and_wrong_per_share_scope() {
        let envelope = DataEnvelope {
            data: 1.2,
            dataset: DatasetKind::Fundamentals,
            source: "fixture".into(),
            source_url: None,
            event_time: None,
            as_of_time: None,
            publish_time: None,
            fetched_at: crate::time::utc_now(),
            parser_version: "fixture".into(),
            schema_version: "1".into(),
            license: "fixture".into(),
            quality_flags: Vec::new(),
            unit: Some(DataUnit::PerShare),
            currency: None,
            adjustment: AdjustmentBasis::NotApplicable,
            revision: None,
            accounting_scope: AccountingScope::Total,
        };
        let flags = envelope.validate_contract();
        assert!(flags
            .iter()
            .any(|flag| flag.code == QualityFlagCode::CurrencyMismatch));
        assert!(flags
            .iter()
            .any(|flag| flag.code == QualityFlagCode::AccountingScopeMismatch));
    }
}
