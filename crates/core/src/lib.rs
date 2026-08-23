//! Domain models, error types, and time utilities for the A-share analysis terminal.
//!
//! This crate is the shared vocabulary of the workspace: symbols, bars, quotes,
//! fund-flow points, provenance metadata, and structured errors. It has no
//! networking dependencies; upstream adapters live in `astock-market-data`.

pub mod adjust;
pub mod bar;
pub mod error;
pub mod flow;
pub mod period;
pub mod provenance;
pub mod quality;
pub mod quote;
pub mod search;
pub mod security;
pub mod symbol;
pub mod time;

pub use adjust::{
    action_factors, apply_adjustment, compute_hfq, compute_qfq, AdjustWarning, Adjusted,
    CorporateAction,
};
pub use bar::{Bar, VolumeUnit};
pub use error::DataError;
pub use flow::FundFlowPoint;
pub use period::{Adjust, KlinePeriod};
pub use provenance::{DataQuality, Fetched, FieldProvenance, Source};
pub use quality::{
    reconcile_numeric, AccountingScope, AdjustmentBasis, ConfidenceCeiling, Currency, DataEnvelope,
    DataQualitySummary, DataUnit, DatasetKind, FreshnessPolicy, FreshnessState, NumericObservation,
    QualityFlag, QualityFlagCode, QualitySeverity, ReconciliationResult, ReconciliationStatus,
    ReconciliationTolerance, SessionRule,
};
pub use quote::{MinuteData, MinutePoint, Quote};
pub use search::{MarketBreadth, SearchResult, StockListItem};
pub use security::{board_for, AssetType, Board, SecurityMasterRecord};
pub use symbol::{Market, Symbol};
