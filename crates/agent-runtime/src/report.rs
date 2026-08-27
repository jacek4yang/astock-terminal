//! Structured report contract.
//!
//! The final answer is no longer free-form Markdown that a verifier has to
//! reverse-engineer. The model submits typed claims carrying canonical evidence
//! identifiers and explicit numeric provenance; the Runtime validates them and
//! renders the user-facing prose and citations itself.
//!
//! That inversion is the point. Measured on a live run, a free-form report
//! produced 41 figures with no citation and 82 derived values that could not be
//! reproduced — not because the verifier was wrong, but because attaching valid
//! provenance to every figure was left to the model's formatting discipline. Here
//! a claim cannot carry a number without saying where the number came from, so
//! the invalid state is unrepresentable rather than merely detected.
//!
//! Two separations are deliberate:
//!
//! * **Internal identity versus presentation.** `evf_…` identifiers are machine
//!   identity for persistence, verification and debugging. They are never shown in
//!   the investor-facing report. [`EvidencePresentation`] carries the
//!   human-readable label, and the Runtime owns the mapping.
//! * **Storage versus model context.** The durable evidence archive stays in the
//!   Engine. Only a bounded catalog reaches the model.
//!
//! Nothing here stores private reasoning. A claim holds user-visible research
//! statements and their provenance.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Contract version. Bump when the shape changes in a way consumers must notice.
pub const REPORT_CONTRACT_VERSION: &str = "astock-report-contract-v1";

/// Bounds. A draft that exceeds any of these is rejected rather than truncated,
/// so an oversized submission cannot silently lose claims.
pub const MAX_CLAIMS: usize = 240;
pub const MAX_SECTIONS: usize = 24;
pub const MAX_STATEMENT_CHARS: usize = 2_000;
pub const MAX_EVIDENCE_PER_CLAIM: usize = 12;
pub const MAX_NUMERIC_ITEMS_PER_CLAIM: usize = 16;

/// What kind of assertion a claim makes.
///
/// The distinction is load-bearing: an estimate must never be presentable as an
/// observed fact, and a scenario parameter supplied by the user must never look
/// like something a data source reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    /// A value copied from source evidence.
    ObservedFact,
    /// A value produced by a deterministic Engine calculation.
    DeterministicCalculation,
    /// A judgement drawn from evidence, carrying no new number of its own.
    Inference,
    /// An approximation the Engine cannot reproduce exactly, stated as such.
    Estimate,
    /// A hypothetical conditioned on an assumption.
    Scenario,
    /// Explicitly insufficient evidence.
    Unknown,
}

impl ClaimKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ObservedFact => "observed_fact",
            Self::DeterministicCalculation => "deterministic_calculation",
            Self::Inference => "inference",
            Self::Estimate => "estimate",
            Self::Scenario => "scenario",
            Self::Unknown => "unknown",
        }
    }

    /// User-facing label. Chinese, because the product's reports are Chinese.
    pub fn display_label(self) -> &'static str {
        match self {
            Self::ObservedFact => "事实",
            Self::DeterministicCalculation => "计算",
            Self::Inference => "推断",
            Self::Estimate => "估算",
            Self::Scenario => "情景",
            Self::Unknown => "未知",
        }
    }

    /// Which provenance classes may a claim of this kind carry?
    ///
    /// This pairing is the core of the contract. Checking only the `Observed`
    /// branch left a hole: an `ObservedFact` claim carrying an `Estimated` number
    /// was accepted, so an estimate could masquerade as a measurement — the exact
    /// thing the contract exists to prevent. The mutation suite caught it.
    ///
    /// * `ObservedFact` carries measurements only.
    /// * `DeterministicCalculation` carries computed values only; an input worth
    ///   stating in its own right belongs in its own observed claim.
    /// * `Inference` and `Unknown` introduce no numbers; they reason over claims
    ///   that already carry provenance.
    /// * `Estimate` carries estimates only.
    /// * `Scenario` may carry the user's assumption plus whatever is computed or
    ///   estimated from it, which is what makes a scenario a scenario.
    fn permits(self, provenance: &NumericProvenance) -> bool {
        match self {
            Self::ObservedFact => matches!(provenance, NumericProvenance::Observed { .. }),
            Self::DeterministicCalculation => {
                matches!(provenance, NumericProvenance::Calculated { .. })
            }
            Self::Inference | Self::Unknown => false,
            Self::Estimate => matches!(provenance, NumericProvenance::Estimated { .. }),
            Self::Scenario => matches!(
                provenance,
                NumericProvenance::UserAssumption { .. }
                    | NumericProvenance::Calculated { .. }
                    | NumericProvenance::Estimated { .. }
            ),
        }
    }
}

/// Where a number came from. Every number in a published report has one of these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "provenance", rename_all = "snake_case")]
pub enum NumericProvenance {
    /// Read from a source observation.
    Observed {
        /// Canonical evidence identifier the value was read from.
        evidence_id: String,
        /// Field path within that evidence, where the Engine exposes one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
    /// Produced by a deterministic Engine calculation.
    Calculated {
        /// Evidence identifier of the calculation result.
        calculation_evidence_id: String,
        /// Operation identity, so the same inputs can be recomputed.
        operation: String,
        /// Evidence the calculation consumed.
        input_evidence_ids: Vec<String>,
    },
    /// Supplied by the user as a scenario parameter.
    ///
    /// `如果铜价下跌 15%` makes `-15%` an assumption, not an observation. Demanding
    /// evidence that copper actually fell would be wrong; presenting it as
    /// something a source reported would be worse.
    UserAssumption {
        /// The user turn the assumption came from, when the session records one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stated_in_message_id: Option<String>,
    },
    /// An approximation the Engine cannot reproduce exactly.
    ///
    /// Not an escape hatch for arithmetic the Engine can do: validation rejects an
    /// estimate whose method names a computable operation.
    Estimated {
        /// How the figure was arrived at.
        method: String,
        /// Evidence the estimate was based on.
        basis_evidence_ids: Vec<String>,
        /// Range, preferred over false precision.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        range: Option<[f64; 2]>,
    },
}

impl NumericProvenance {
    /// Canonical identifiers this provenance depends on.
    pub fn referenced_evidence(&self) -> Vec<&str> {
        match self {
            Self::Observed { evidence_id, .. } => vec![evidence_id.as_str()],
            Self::Calculated {
                calculation_evidence_id,
                input_evidence_ids,
                ..
            } => {
                let mut ids = vec![calculation_evidence_id.as_str()];
                ids.extend(input_evidence_ids.iter().map(String::as_str));
                ids
            }
            Self::UserAssumption { .. } => Vec::new(),
            Self::Estimated {
                basis_evidence_ids, ..
            } => basis_evidence_ids.iter().map(String::as_str).collect(),
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Observed { .. } => "observed",
            Self::Calculated { .. } => "calculated",
            Self::UserAssumption { .. } => "user_assumption",
            Self::Estimated { .. } => "estimated",
        }
    }

    /// User-facing label, so a reader can tell an assumption from a measurement.
    pub fn display_label(&self) -> &'static str {
        match self {
            Self::Observed { .. } => "实测数据",
            Self::Calculated { .. } => "确定性计算",
            Self::UserAssumption { .. } => "用户情景假设",
            Self::Estimated { .. } => "模型估算",
        }
    }
}

/// One number inside a claim, with its provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NumericItem {
    /// The value as published.
    pub value: f64,
    /// Unit or currency, when one applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Short label, for example `最新价` or `市值`.
    pub label: String,
    #[serde(flatten)]
    pub provenance: NumericProvenance,
}

/// A single research assertion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub id: String,
    pub kind: ClaimKind,
    /// User-visible prose, carrying no citation markup: the Runtime renders that.
    pub statement: String,
    /// Canonical evidence supporting the claim.
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub numeric_items: Vec<NumericItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
    /// Conflicting evidence the claim must disclose rather than silently resolve.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disclosed_conflicts: Vec<String>,
}

/// An ordered group of claims.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportSection {
    pub heading: String,
    pub claim_ids: Vec<String>,
}

/// What the model submits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifiedReportDraft {
    #[serde(default = "default_contract_version")]
    pub version: String,
    pub title: String,
    pub executive_summary: String,
    pub sections: Vec<ReportSection>,
    pub claims: Vec<Claim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overall_uncertainty: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

fn default_contract_version() -> String {
    REPORT_CONTRACT_VERSION.to_owned()
}

/// A machine-actionable reason a draft was refused.
///
/// Returned instead of hundreds of raw strings so repair can target the affected
/// claims. The live failure sent 1,178 textual findings back into the context and
/// the loop never converged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "problem", rename_all = "snake_case")]
pub enum DraftProblem {
    ContractVersionMismatch {
        supplied: String,
        expected: String,
    },
    Oversized {
        what: String,
        actual: usize,
        maximum: usize,
    },
    DuplicateClaimId {
        claim_id: String,
    },
    SectionReferencesUnknownClaim {
        heading: String,
        claim_id: String,
    },
    ClaimNotInAnySection {
        claim_id: String,
    },
    EmptyStatement {
        claim_id: String,
    },
    /// The model supplied an identifier the registry does not contain, including
    /// invented labels such as `计算-BPS`. Never repaired by guessing.
    UnknownEvidence {
        claim_id: String,
        supplied_id: String,
    },
    /// An observed fact with no supporting evidence.
    MissingEvidence {
        claim_id: String,
        statement: String,
    },
    /// A number asserted as observed on a claim that may not assert observations.
    UnsupportedObservedNumber {
        claim_id: String,
        label: String,
        value: f64,
    },
    /// A calculated figure whose calculation provenance is absent or incomplete.
    MissingCalculationProvenance {
        claim_id: String,
        label: String,
        value: f64,
    },
    /// An estimate that the Engine could have computed deterministically.
    InvalidEstimate {
        claim_id: String,
        label: String,
        reason: String,
    },
    /// A scenario claim with no assumption recorded.
    ScenarioWithoutAssumption {
        claim_id: String,
    },
    /// Evidence known to conflict, used without disclosure.
    ConflictingEvidence {
        claim_id: String,
        evidence_ids: Vec<String>,
    },
    /// A claim citing evidence outside the task's security universe.
    EvidenceOutsideTaskScope {
        claim_id: String,
        evidence_id: String,
    },
    /// A quantity written into a claim's prose that the claim never declared.
    ///
    /// The renderer places a claim's statement and its citations on the same line of
    /// the canonical form, so the verifier checks every figure in the prose against
    /// the evidence the claim named. A figure written only in prose therefore has no
    /// provenance and cannot be reproduced.
    ///
    /// This is the residual failure of a live run that otherwise converged: `约 79.87
    /// 亿元` as a rounded restatement of the cited `7,987,376,586`, and `单手(100 股)`
    /// as a lot size. Both are refused here, at validation, where repair is one cheap
    /// round rather than a verifier cycle.
    UndeclaredNumberInStatement {
        claim_id: String,
        numeral: String,
    },
    /// A declared number that its own cited evidence does not support.
    ///
    /// The most dangerous shape the contract can carry: a figure with a
    /// well-formed, existing citation that the citation does not actually contain.
    /// Validation used to accept it because it checked that provenance was *present*,
    /// not that it was *true*, leaving the verifier to catch it a round later — and
    /// on a live run it produced a validation/verification disagreement, which is
    /// exactly what the shared numeral rule exists to prevent.
    NumberDisagreesWithEvidence {
        claim_id: String,
        label: String,
        declared: f64,
        evidence_id: String,
    },
}

impl DraftProblem {
    /// Claim this problem concerns, when it concerns one.
    pub fn claim_id(&self) -> Option<&str> {
        match self {
            Self::ContractVersionMismatch { .. } | Self::Oversized { .. } => None,
            Self::DuplicateClaimId { claim_id }
            | Self::ClaimNotInAnySection { claim_id }
            | Self::EmptyStatement { claim_id }
            | Self::UnknownEvidence { claim_id, .. }
            | Self::MissingEvidence { claim_id, .. }
            | Self::UnsupportedObservedNumber { claim_id, .. }
            | Self::MissingCalculationProvenance { claim_id, .. }
            | Self::InvalidEstimate { claim_id, .. }
            | Self::ScenarioWithoutAssumption { claim_id }
            | Self::ConflictingEvidence { claim_id, .. }
            | Self::UndeclaredNumberInStatement { claim_id, .. }
            | Self::NumberDisagreesWithEvidence { claim_id, .. }
            | Self::EvidenceOutsideTaskScope { claim_id, .. } => Some(claim_id),
            Self::SectionReferencesUnknownClaim { claim_id, .. } => Some(claim_id),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::ContractVersionMismatch { .. } => "contract_version_mismatch",
            Self::Oversized { .. } => "oversized",
            Self::DuplicateClaimId { .. } => "duplicate_claim_id",
            Self::SectionReferencesUnknownClaim { .. } => "section_references_unknown_claim",
            Self::ClaimNotInAnySection { .. } => "claim_not_in_any_section",
            Self::EmptyStatement { .. } => "empty_statement",
            Self::UnknownEvidence { .. } => "unknown_evidence",
            Self::MissingEvidence { .. } => "missing_evidence",
            Self::UnsupportedObservedNumber { .. } => "unsupported_observed_number",
            Self::MissingCalculationProvenance { .. } => "missing_calculation_provenance",
            Self::InvalidEstimate { .. } => "invalid_estimate",
            Self::ScenarioWithoutAssumption { .. } => "scenario_without_assumption",
            Self::ConflictingEvidence { .. } => "conflicting_evidence",
            Self::UndeclaredNumberInStatement { .. } => "undeclared_number_in_statement",
            Self::NumberDisagreesWithEvidence { .. } => "number_disagrees_with_evidence",
            Self::EvidenceOutsideTaskScope { .. } => "evidence_outside_task_scope",
        }
    }
}

/// What the Runtime knows about one piece of evidence, for validation and display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceDescriptor {
    /// Canonical machine identity. Never shown in the investor-facing report.
    pub evidence_id: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_location: Option<String>,
    #[serde(default)]
    pub quality_blocking: bool,
    #[serde(default)]
    pub conflicting: bool,
}

impl EvidenceDescriptor {
    /// Is this a deterministic calculation result rather than an observation?
    pub fn is_calculation(&self) -> bool {
        self.source == "astock-compute"
    }

    /// Human-readable label shown instead of the identifier.
    ///
    /// Built only from real provenance. A friendly label is never invented: when
    /// metadata is thin the label degrades to the source name rather than
    /// inventing a document title the evidence cannot support.
    pub fn display_label(&self) -> String {
        if self.is_calculation() {
            return "确定性计算".to_owned();
        }
        let mut parts: Vec<String> = Vec::new();
        parts.push(source_display_name(&self.source).to_owned());
        if let Some(title) = self.document_title.as_deref().filter(|t| !t.is_empty()) {
            parts.push(title.to_owned());
        } else if let Some(field) = self.field.as_deref().filter(|f| !f.is_empty()) {
            parts.push(friendly_field(field));
        }
        if let Some(location) = self.document_location.as_deref().filter(|l| !l.is_empty()) {
            parts.push(location.to_owned());
        } else if let Some(stamp) = self
            .published_at
            .as_deref()
            .or(self.observed_at.as_deref())
            .and_then(short_timestamp)
        {
            parts.push(stamp);
        }
        parts.join(" · ")
    }

    /// Trust state in words an investor can act on.
    pub fn trust_label(&self) -> &'static str {
        if self.conflicting {
            return "来源存在冲突";
        }
        if self.quality_blocking {
            return "证据质量不足";
        }
        if self.is_calculation() {
            return "确定性计算";
        }
        match self.source.as_str() {
            "disclosure" | "cninfo" | "sse" | "szse" | "csrc" => "公司正式披露",
            "tencent" | "sina" | "tdx" => "实时行情",
            "joinquant" | "tushare" => "量化数据源",
            _ => "单一来源",
        }
    }
}

fn source_display_name(source: &str) -> &str {
    match source {
        "tencent" => "腾讯行情",
        "sina" => "新浪行情",
        "eastmoney" => "东方财富",
        "tdx" => "通达信",
        "joinquant" => "聚宽",
        "tushare" => "Tushare",
        "disclosure" => "公司公告",
        "cninfo" => "巨潮资讯",
        "sse" => "上交所",
        "szse" => "深交所",
        "csrc" => "证监会",
        "astock-compute" => "确定性计算",
        other => other,
    }
}

fn friendly_field(field: &str) -> String {
    let leaf = field.rsplit('/').next().unwrap_or(field);
    match leaf {
        "last" | "price" | "close" => "最新价".to_owned(),
        "pre_close" | "prev_close" | "yesterday_close" => "昨收价".to_owned(),
        "open" => "开盘价".to_owned(),
        "high" => "最高价".to_owned(),
        "low" => "最低价".to_owned(),
        "change" => "涨跌额".to_owned(),
        "change_pct" | "pct" | "pct_chg" => "涨跌幅".to_owned(),
        "turnover" | "turnover_rate" => "换手率".to_owned(),
        "amount" => "成交额".to_owned(),
        "volume" | "vol" => "成交量".to_owned(),
        "timestamp" | "time" | "trade_time" => "行情时间".to_owned(),
        "eps" => "每股收益".to_owned(),
        "pe" | "pe_ttm" => "市盈率".to_owned(),
        "pb" => "市净率".to_owned(),
        "shares" | "total_shares" => "股本".to_owned(),
        "revenue" => "营业收入".to_owned(),
        "net_profit" => "归母净利润".to_owned(),
        other => other.to_owned(),
    }
}

/// Trim an RFC3339 timestamp to something a reader scans quickly.
fn short_timestamp(raw: &str) -> Option<String> {
    let (date, rest) = raw.split_once('T')?;
    let time = rest.get(0..5).unwrap_or("");
    if time.is_empty() {
        Some(date.to_owned())
    } else {
        Some(format!("{date} {time}"))
    }
}

/// Decode a submitted draft, naming the field that failed.
///
/// `serde_json::from_value` reports `invalid type: map, expected a string` with no
/// indication of *where*. A live moderate run spent its entire finalization budget
/// on exactly that: an otherwise complete report whose `limitations` entries were
/// each wrapped in an object, six times, with nothing in the diagnostic that could
/// have located the field. Repair is only possible if the model is told which field
/// is wrong, so the path is part of the contract's diagnostic surface.
pub fn decode_draft(arguments: &Value) -> Result<VerifiedReportDraft, String> {
    let mut track = serde_path_to_error::Track::new();
    let deserializer = serde_path_to_error::Deserializer::new(arguments, &mut track);
    match VerifiedReportDraft::deserialize(deserializer) {
        Ok(draft) => Ok(draft),
        Err(error) => {
            let path = track.path().to_string();
            if path.is_empty() {
                Err(error.to_string())
            } else {
                Err(format!("{path}: {error}"))
            }
        }
    }
}

/// Validate a draft against the evidence registry and the task scope.
///
/// Returns every problem found rather than the first, so one repair round can fix
/// all of them. An empty result means the draft is structurally publishable — the
/// independent report verifier still runs afterwards and remains the final gate.
pub fn validate_draft(
    draft: &VerifiedReportDraft,
    registry: &BTreeMap<String, EvidenceDescriptor>,
    task_symbols: &BTreeSet<String>,
) -> Vec<DraftProblem> {
    let mut problems = Vec::new();

    if draft.version != REPORT_CONTRACT_VERSION {
        problems.push(DraftProblem::ContractVersionMismatch {
            supplied: draft.version.clone(),
            expected: REPORT_CONTRACT_VERSION.to_owned(),
        });
    }
    let bound = |what: &str, actual: usize, maximum: usize, out: &mut Vec<DraftProblem>| {
        if actual > maximum {
            out.push(DraftProblem::Oversized {
                what: what.to_owned(),
                actual,
                maximum,
            });
        }
    };
    bound("claims", draft.claims.len(), MAX_CLAIMS, &mut problems);
    bound(
        "sections",
        draft.sections.len(),
        MAX_SECTIONS,
        &mut problems,
    );

    let mut seen_ids: BTreeSet<&str> = BTreeSet::new();
    for claim in &draft.claims {
        if !seen_ids.insert(claim.id.as_str()) {
            problems.push(DraftProblem::DuplicateClaimId {
                claim_id: claim.id.clone(),
            });
        }
    }

    // Every claim must be placed, and every placement must resolve.
    let mut placed: BTreeSet<&str> = BTreeSet::new();
    for section in &draft.sections {
        for claim_id in &section.claim_ids {
            if !seen_ids.contains(claim_id.as_str()) {
                problems.push(DraftProblem::SectionReferencesUnknownClaim {
                    heading: section.heading.clone(),
                    claim_id: claim_id.clone(),
                });
            }
            placed.insert(claim_id.as_str());
        }
    }
    for claim in &draft.claims {
        if !placed.contains(claim.id.as_str()) {
            problems.push(DraftProblem::ClaimNotInAnySection {
                claim_id: claim.id.clone(),
            });
        }
    }

    for claim in &draft.claims {
        validate_claim(claim, registry, task_symbols, &mut problems);
    }
    problems
}

fn validate_claim(
    claim: &Claim,
    registry: &BTreeMap<String, EvidenceDescriptor>,
    task_symbols: &BTreeSet<String>,
    problems: &mut Vec<DraftProblem>,
) {
    if claim.statement.trim().is_empty() {
        problems.push(DraftProblem::EmptyStatement {
            claim_id: claim.id.clone(),
        });
    }
    if claim.statement.chars().count() > MAX_STATEMENT_CHARS {
        problems.push(DraftProblem::Oversized {
            what: format!("statement of {}", claim.id),
            actual: claim.statement.chars().count(),
            maximum: MAX_STATEMENT_CHARS,
        });
    }
    if claim.evidence_ids.len() > MAX_EVIDENCE_PER_CLAIM {
        problems.push(DraftProblem::Oversized {
            what: format!("evidence of {}", claim.id),
            actual: claim.evidence_ids.len(),
            maximum: MAX_EVIDENCE_PER_CLAIM,
        });
    }
    if claim.numeric_items.len() > MAX_NUMERIC_ITEMS_PER_CLAIM {
        problems.push(DraftProblem::Oversized {
            what: format!("numeric items of {}", claim.id),
            actual: claim.numeric_items.len(),
            maximum: MAX_NUMERIC_ITEMS_PER_CLAIM,
        });
    }

    // Every referenced identifier must exist. Invented labels land here.
    let mut referenced: Vec<&str> = claim.evidence_ids.iter().map(String::as_str).collect();
    for item in &claim.numeric_items {
        referenced.extend(item.provenance.referenced_evidence());
    }
    let mut conflicting: Vec<String> = Vec::new();
    for id in &referenced {
        match registry.get(*id) {
            None => problems.push(DraftProblem::UnknownEvidence {
                claim_id: claim.id.clone(),
                supplied_id: (*id).to_owned(),
            }),
            Some(descriptor) => {
                if descriptor.conflicting {
                    conflicting.push((*id).to_owned());
                }
                // Scope: a claim must not lean on another security's evidence.
                if let Some(symbol) = descriptor.symbol.as_deref() {
                    if !task_symbols.is_empty() && !task_symbols.contains(symbol) {
                        problems.push(DraftProblem::EvidenceOutsideTaskScope {
                            claim_id: claim.id.clone(),
                            evidence_id: (*id).to_owned(),
                        });
                    }
                }
            }
        }
    }
    // A known conflict must be stated, not quietly resolved.
    if !conflicting.is_empty() {
        let disclosed: BTreeSet<&str> = claim
            .disclosed_conflicts
            .iter()
            .map(String::as_str)
            .collect();
        let undisclosed: Vec<String> = conflicting
            .iter()
            .filter(|id| !disclosed.contains(id.as_str()))
            .cloned()
            .collect();
        if !undisclosed.is_empty() {
            problems.push(DraftProblem::ConflictingEvidence {
                claim_id: claim.id.clone(),
                evidence_ids: undisclosed,
            });
        }
    }

    match claim.kind {
        ClaimKind::ObservedFact | ClaimKind::Inference => {
            if claim.evidence_ids.is_empty() {
                problems.push(DraftProblem::MissingEvidence {
                    claim_id: claim.id.clone(),
                    statement: claim.statement.clone(),
                });
            }
        }
        ClaimKind::Scenario => {
            let has_assumption = !claim.assumptions.is_empty()
                || claim.numeric_items.iter().any(|item| {
                    matches!(item.provenance, NumericProvenance::UserAssumption { .. })
                });
            if !has_assumption {
                problems.push(DraftProblem::ScenarioWithoutAssumption {
                    claim_id: claim.id.clone(),
                });
            }
        }
        ClaimKind::DeterministicCalculation | ClaimKind::Estimate | ClaimKind::Unknown => {}
    }

    for item in &claim.numeric_items {
        validate_numeric_item(claim, item, registry, problems);
    }
    validate_statement_numerals(claim, registry, problems);
}

/// Every quantity in a claim's prose must be one the claim actually declared.
///
/// The canonical form the verifier reads places the statement and the claim's
/// citations on one line, so a figure written into prose is checked against that
/// evidence exactly as a declared number would be. Checking it here rather than
/// waiting for the verifier costs one cheap validation round instead of a
/// verification cycle, and the repair is addressed to a claim.
///
/// The rule is the Engine verifier's own: [`astock_engine::financial_numerals`]
/// decides what counts as a financial quantity — masking security codes, dates,
/// headings, clock times and window labels — and `supported_by` decides when a value
/// backs it, including the percentage convention. Reimplementing either here would
/// let validation and verification drift, and the drift would surface as a report
/// validation accepted and verification refused.
///
/// A prose figure is accepted when it matches one of the claim's own numeric items,
/// or the value of evidence the claim cites. That is exactly as permissive as the
/// verifier, so this rejects nothing the verifier would have passed.
fn validate_statement_numerals(
    claim: &Claim,
    registry: &BTreeMap<String, EvidenceDescriptor>,
    problems: &mut Vec<DraftProblem>,
) {
    let numerals = astock_engine::financial_numerals(&claim.statement);
    if numerals.is_empty() {
        return;
    }
    let cited_values: Vec<f64> = claim
        .evidence_ids
        .iter()
        .filter_map(|id| registry.get(id))
        .filter_map(|descriptor| descriptor.value.as_ref())
        .filter_map(evidence_number)
        .collect();
    for numeral in numerals {
        let declared = claim
            .numeric_items
            .iter()
            .any(|item| numeral.supported_by(item.value));
        if declared
            || cited_values
                .iter()
                .any(|value| numeral.supported_by(*value))
        {
            continue;
        }
        problems.push(DraftProblem::UndeclaredNumberInStatement {
            claim_id: claim.id.clone(),
            numeral: numeral.raw.clone(),
        });
    }
}

/// Does a declared number actually appear in the evidence it cites?
///
/// Validation used to check that provenance was *present*, not that it was *true*,
/// so a figure carrying a well-formed existing citation was accepted even when the
/// citation contained something else. The verifier caught it a round later, which
/// showed up on a live run as a validation/verification disagreement — the one thing
/// the shared numeral rule exists to prevent.
///
/// The comparison is performed on the exact token the renderer will emit, run
/// through the Engine's own extractor, so validation and verification cannot read
/// the same figure differently. Evidence with no numeric value is not judged here: a
/// claim may legitimately cite a document, a source name or a timestamp.
fn check_value_against_evidence(
    claim: &Claim,
    item: &NumericItem,
    evidence_id: &str,
    registry: &BTreeMap<String, EvidenceDescriptor>,
    problems: &mut Vec<DraftProblem>,
) {
    let Some(descriptor) = registry.get(evidence_id) else {
        // A missing identifier is already reported as `unknown_evidence`.
        return;
    };
    let Some(evidence_value) = descriptor.value.as_ref().and_then(evidence_number) else {
        return;
    };
    let token = format!("{}{}", item.value, item.unit.as_deref().unwrap_or_default());
    let numerals = astock_engine::financial_numerals(&token);
    // A token the extractor does not read as a quantity cannot be checked; the
    // verifier will not read it as a claim either, so there is nothing to disagree
    // about.
    let Some(numeral) = numerals.first() else {
        return;
    };
    if !numeral.supported_by(evidence_value) {
        problems.push(DraftProblem::NumberDisagreesWithEvidence {
            claim_id: claim.id.clone(),
            label: item.label.clone(),
            declared: item.value,
            evidence_id: evidence_id.to_owned(),
        });
    }
}

/// Read a number out of an evidence value, including one recorded as a string.
fn evidence_number(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => text.replace(',', "").parse::<f64>().ok(),
        _ => None,
    }
}

fn validate_numeric_item(
    claim: &Claim,
    item: &NumericItem,
    registry: &BTreeMap<String, EvidenceDescriptor>,
    problems: &mut Vec<DraftProblem>,
) {
    // The claim's kind and the number's provenance must agree before anything
    // else is checked. Without this an estimate can be relabelled a fact, or an
    // assumption presented as a measurement.
    if !claim.kind.permits(&item.provenance) {
        problems.push(DraftProblem::UnsupportedObservedNumber {
            claim_id: claim.id.clone(),
            label: item.label.clone(),
            value: item.value,
        });
    }

    match &item.provenance {
        NumericProvenance::Observed { evidence_id, .. } => {
            // A calculation result is not an observation.
            if registry
                .get(evidence_id)
                .is_some_and(EvidenceDescriptor::is_calculation)
            {
                problems.push(DraftProblem::MissingCalculationProvenance {
                    claim_id: claim.id.clone(),
                    label: item.label.clone(),
                    value: item.value,
                });
            }
            check_value_against_evidence(claim, item, evidence_id, registry, problems);
        }
        NumericProvenance::Calculated {
            calculation_evidence_id,
            operation,
            input_evidence_ids,
        } => {
            if operation.trim().is_empty() || input_evidence_ids.is_empty() {
                problems.push(DraftProblem::MissingCalculationProvenance {
                    claim_id: claim.id.clone(),
                    label: item.label.clone(),
                    value: item.value,
                });
            }
            // The cited result must actually be a calculation.
            if registry
                .get(calculation_evidence_id)
                .is_some_and(|descriptor| !descriptor.is_calculation())
            {
                problems.push(DraftProblem::MissingCalculationProvenance {
                    claim_id: claim.id.clone(),
                    label: item.label.clone(),
                    value: item.value,
                });
            }
            check_value_against_evidence(claim, item, calculation_evidence_id, registry, problems);
        }
        // The kind/provenance pairing above already rejects an assumption on a
        // non-scenario claim, and an assumption references no evidence.
        NumericProvenance::UserAssumption { .. } => {}
        NumericProvenance::Estimated {
            method,
            basis_evidence_ids,
            range,
        } => {
            if method.trim().is_empty() || basis_evidence_ids.is_empty() {
                problems.push(DraftProblem::InvalidEstimate {
                    claim_id: claim.id.clone(),
                    label: item.label.clone(),
                    reason: "an estimate must name its method and its basis evidence".to_owned(),
                });
            }
            // Estimate is not a way to avoid a computation the Engine can do.
            const COMPUTABLE: [&str; 8] = [
                "市值",
                "market_cap",
                "pe",
                "市盈率",
                "pb",
                "市净率",
                "涨跌幅",
                "change_pct",
            ];
            if COMPUTABLE
                .iter()
                .any(|needle| method.contains(needle) || item.label.contains(needle))
            {
                problems.push(DraftProblem::InvalidEstimate {
                    claim_id: claim.id.clone(),
                    label: item.label.clone(),
                    reason: "this quantity is deterministically computable; use a calculation \
                             rather than an estimate"
                        .to_owned(),
                });
            }
            if let Some([low, high]) = range {
                if low > high {
                    problems.push(DraftProblem::InvalidEstimate {
                        claim_id: claim.id.clone(),
                        label: item.label.clone(),
                        reason: "range bounds are inverted".to_owned(),
                    });
                }
            }
        }
    }
}
