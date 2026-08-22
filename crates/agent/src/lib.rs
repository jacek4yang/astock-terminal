//! # astock-agent
//!
//! The MiniMax-driven agent brain of the A-share terminal:
//!
//! - a strongly-typed [tool system](tools) over the deterministic Rust
//!   engines ([`builtin::default_registry`]): the LLM only ever sees compact
//!   summaries plus provenance; full payloads land in `tool_cache`;
//! - [prompt discipline](prompt): a compact, stable Chinese system prompt
//!   with conclusion grading and a mandatory disclaimer;
//! - the [orchestrator](orchestrator): a streaming tool-calling loop with
//!   per-round persistence and **resumable workflows** — when the MiniMax
//!   quota runs out the task suspends and later resumes from storage without
//!   re-executing completed tool calls;
//! - [report assembly](report): graded conclusions, enforced disclaimer and
//!   an evidence list of tool provenance.
//!
//! The model is reached through the [`backend::ChatBackend`] seam, so tests
//! drive the loop with scripted fakes ([`testing`]).

pub mod backend;
pub mod builtin;
pub mod deep;
pub mod error;
pub mod indicators;
pub mod orchestrator;
pub mod prompt;
pub mod report;
pub mod testing;
pub mod tools;

pub use backend::{ChatBackend, ChatChunkStream};
pub use builtin::default_registry;
pub use error::{AgentError, Result};
pub use orchestrator::{
    compact_history, AgentEngine, AgentEvent, EngineConfig, SpecialistRoute, SuspendReason,
    TaskSpec, TaskStream, SNAPSHOT_MARKER,
};
pub use report::{AgentReport, Conclusion, Evidence};
pub use tools::{
    AgentTool, ToolContext, ToolProgressDetail, ToolRegistry, ToolResult, ToolWorkItem,
};
