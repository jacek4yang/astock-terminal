//! Shared Rust runtime for AStock's CLI and desktop adapters.
//!
//! The runtime owns model orchestration, durable task transitions, bounded
//! tool execution and report publication policy. Financial effects are
//! performed only through the GUI-independent Rust Engine.

mod clarify;
mod engine;
mod error;
mod events;
mod intent;
mod minimax;
mod model;
mod plan;
mod prompt;
mod render;
mod report;
mod runtime;
mod session;
mod store;
mod tools;

pub use clarify::{ClarificationAnswer, ClarificationOption, ClarificationRequest};
pub use engine::EngineGateway;
pub use error::{ProviderError, ProviderErrorKind, RuntimeError};
pub use events::{AgentEvent, AgentPhase, VerificationFinding};
pub use intent::{ResearchDepth, UserIntent};
pub use minimax::MinimaxProvider;
pub use model::{
    Message, MessageRole, ModelChunk, ModelProvider, ModelRequest, ModelStream, ModelToolCall,
};
pub use plan::{Plan, PlanMutation, PlanStep, PlanStepStatus};
pub use render::{
    contains_internal_identifier, render, EvidenceReference, RenderedClaim, RenderedNumber,
    RenderedReport, RenderedSection,
};
pub use report::{
    validate_draft, Claim, ClaimKind, DraftProblem, EvidenceDescriptor, NumericItem,
    NumericProvenance, ReportSection, VerifiedReportDraft, MAX_CLAIMS, MAX_EVIDENCE_PER_CLAIM,
    MAX_NUMERIC_ITEMS_PER_CLAIM, MAX_SECTIONS, MAX_STATEMENT_CHARS, REPORT_CONTRACT_VERSION,
};
pub use runtime::{
    AgentRuntime, RunOutcome, RuntimeConfig, RuntimeTask, SessionRunOutcome, SessionTaskStream,
    TaskStream,
};
pub use session::{
    RuntimeSession, SessionBranchRequest, SessionManager, SessionMessage, SessionMessageRole,
    SessionSummary, SessionTaskState, StoredSession, SESSION_VERSION,
};
pub use store::{AgentStore, EffectIntent, StoredCheckpoint};
pub use tools::{
    default_registry, CachePolicy, ToolDefinition, ToolExecutor, ToolRegistry, ToolRisk,
};
