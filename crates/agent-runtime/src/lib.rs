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
