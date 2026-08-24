// GENERATED from protocol/schema; schema-sha256=6946a569e672aadbc444c9a1907eaa65ae50b79711731ad84adf5742c1d1b0ff
// Run: node protocol/codegen.mjs

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PAGE_SIZE: usize = 500;
pub const RELEASE_VERSION: &str = "6.0.0";
pub const ENGINE_STARTUP_REQUIRED_CAPABILITIES: &[&str] = &[
    "market",
    "research",
    "data_quality",
    "agent_advanced_analysis_v1",
    "storage",
    "credentials",
    "agent_event_store_v2",
];
pub const AGENT_STARTUP_REQUIRED_CAPABILITIES: &[&str] = &[
    "pure_reducer",
    "replay",
    "evidence_gate",
    "advanced_tool_planning",
    "closed_engine_effects",
    "deterministic_report_verification",
    "sse_stream_recovery",
];
pub const ENGINE_REQUEST_KINDS: &[&str] = &[
    "system.handshake",
    "system.shutdown",
    "system.cancel",
    "diagnostics.status",
    "diagnostics.data_quality",
    "market.session",
    "market.overview",
    "market.search",
    "market.quote",
    "market.kline",
    "market.index_kline",
    "market.shares.page",
    "market.security_snapshot",
    "market.order_book",
    "market.minute",
    "market.fund_flow.daily",
    "market.fund_flow.realtime",
    "research.market_context",
    "research.agent_prepare_context",
    "research.agent_security_context",
    "research.agent_report_verify",
    "research.global_context",
    "research.global.sync.start",
    "research.global.sync.status",
    "research.global.sync.cancel",
    "research.global.providers",
    "research.global.documents",
    "research.global.chains",
    "research.global.transmission",
    "research.security_events",
    "research.market_candidates",
    "research.market_pool",
    "research.board.constituents",
    "analysis.chanlun.minute",
    "research.disclosures.list",
    "research.disclosures.detail",
    "research.disclosures.providers",
    "research.disclosures.sync.start",
    "research.disclosures.sync.status",
    "research.disclosures.sync.cancel",
    "research.news.providers",
    "research.news.center",
    "research.news.provider.set",
    "research.news.archive.recent",
    "research.news.archive.revisions",
    "research.news.archive.integrity",
    "research.news.archive.observations",
    "research.news.user_state",
    "research.news.clusters.list",
    "research.news.clusters.detail",
    "research.news.clusters.merge",
    "research.news.clusters.split",
    "research.news.reviews.list",
    "research.news.reviews.resolve",
    "research.entities.links",
    "research.entities.reviews",
    "research.entities.resolve",
    "research.events.analysis.start",
    "research.events.analysis.status",
    "research.events.analysis.cancel",
    "research.relations.extraction.start",
    "research.relations.extraction.status",
    "research.relations.extraction.cancel",
    "research.relations.reviews",
    "research.relations.review",
    "research.relations.retract",
    "research.graph.subgraph",
    "research.graph.as_of",
    "research.graph.history_bounds",
    "research.graph.edge_timeline",
    "research.graph.snapshot.get",
    "research.graph.snapshot.diff",
    "research.graph.shock",
    "research.market.relationship",
    "research.quant.start",
    "research.quant.status",
    "research.quant.cancel",
    "research.quant.snapshots.get",
    "research.quant.snapshots.list",
    "research.backtest.strategies",
    "research.backtest.run",
    "research.backtest.start",
    "research.backtest.status",
    "research.backtest.cancel",
    "research.market.regime",
    "research.fundamentals",
    "research.earnings_driver.tree",
    "research.earnings_driver.shock",
    "research.earnings_driver.snapshot",
    "research.news",
    "research.data_reconcile",
    "research.quote_reconcile",
    "research.valuation_reconcile",
    "research.joinquant_context",
    "research.optional_sources",
    "research.sources.list",
    "research.sources.get",
    "research.sources.fetch",
    "research.sources.compare",
    "workspace.watchlist.list",
    "workspace.watchlist.add",
    "workspace.watchlist.remove",
    "workspace.watchlist.pin",
    "credentials.status",
    "credentials.provider.set",
    "credentials.provider.delete",
    "credentials.minimax.set",
    "credentials.minimax.delete",
    "credentials.minimax.quota",
    "credentials.joinquant.set",
    "credentials.joinquant.delete",
    "storage.cache.stats",
    "storage.cache.cleanup",
    "storage.data_root.migrate",
    "storage.data_root.rollback",
    "quant.scan.start",
    "quant.scan.status",
    "quant.scan.cancel",
    "settings.agent_models.get",
    "settings.agent_models.set",
    "agent.task.create",
    "agent.event.append",
    "agent.checkpoint.put",
    "agent.effect.begin",
    "agent.effect.complete",
    "agent.effect.list",
    "agent.task.load",
    "agent.task.list",
    "agent.conversation.save",
    "agent.conversation.load",
    "agent.conversation.list",
    "agent.conversation.rename",
    "agent.conversation.branch",
    "agent.conversation.delete",
];
pub const ENGINE_RENDERER_REQUEST_KINDS: &[&str] = &[
    "diagnostics.status",
    "diagnostics.data_quality",
    "market.session",
    "market.overview",
    "market.search",
    "market.quote",
    "market.kline",
    "market.index_kline",
    "market.shares.page",
    "market.security_snapshot",
    "market.order_book",
    "market.minute",
    "market.fund_flow.daily",
    "market.fund_flow.realtime",
    "research.global.sync.start",
    "research.global.sync.status",
    "research.global.sync.cancel",
    "research.global.providers",
    "research.global.documents",
    "research.global.chains",
    "research.global.transmission",
    "research.market_pool",
    "research.board.constituents",
    "analysis.chanlun.minute",
    "research.disclosures.list",
    "research.disclosures.detail",
    "research.disclosures.providers",
    "research.disclosures.sync.start",
    "research.disclosures.sync.status",
    "research.disclosures.sync.cancel",
    "research.news.providers",
    "research.news.center",
    "research.news.provider.set",
    "research.news.archive.recent",
    "research.news.archive.revisions",
    "research.news.archive.integrity",
    "research.news.archive.observations",
    "research.news.user_state",
    "research.news.clusters.list",
    "research.news.clusters.detail",
    "research.news.clusters.merge",
    "research.news.clusters.split",
    "research.news.reviews.list",
    "research.news.reviews.resolve",
    "research.entities.links",
    "research.entities.reviews",
    "research.entities.resolve",
    "research.events.analysis.start",
    "research.events.analysis.status",
    "research.events.analysis.cancel",
    "research.relations.extraction.start",
    "research.relations.extraction.status",
    "research.relations.extraction.cancel",
    "research.relations.reviews",
    "research.relations.review",
    "research.relations.retract",
    "research.graph.subgraph",
    "research.graph.as_of",
    "research.graph.history_bounds",
    "research.graph.edge_timeline",
    "research.graph.snapshot.get",
    "research.graph.snapshot.diff",
    "research.graph.shock",
    "research.market.relationship",
    "research.quant.start",
    "research.quant.status",
    "research.quant.cancel",
    "research.quant.snapshots.get",
    "research.quant.snapshots.list",
    "research.backtest.strategies",
    "research.backtest.run",
    "research.backtest.start",
    "research.backtest.status",
    "research.backtest.cancel",
    "research.market.regime",
    "research.earnings_driver.tree",
    "research.earnings_driver.shock",
    "research.earnings_driver.snapshot",
    "research.news",
    "research.quote_reconcile",
    "research.valuation_reconcile",
    "research.sources.list",
    "research.sources.get",
    "research.sources.fetch",
    "research.sources.compare",
    "workspace.watchlist.list",
    "workspace.watchlist.add",
    "workspace.watchlist.remove",
    "workspace.watchlist.pin",
    "credentials.status",
    "credentials.provider.set",
    "credentials.provider.delete",
    "credentials.minimax.set",
    "credentials.minimax.delete",
    "credentials.minimax.quota",
    "credentials.joinquant.set",
    "credentials.joinquant.delete",
    "storage.cache.stats",
    "storage.cache.cleanup",
    "storage.data_root.migrate",
    "storage.data_root.rollback",
    "quant.scan.start",
    "quant.scan.status",
    "quant.scan.cancel",
    "settings.agent_models.get",
    "settings.agent_models.set",
    "agent.task.load",
    "agent.conversation.save",
    "agent.conversation.load",
    "agent.conversation.list",
    "agent.conversation.rename",
    "agent.conversation.branch",
    "agent.conversation.delete",
];
pub const AGENT_REQUEST_KINDS: &[&str] = &[
    "system.handshake",
    "diagnostics.status",
    "agent.provider.test",
    "agent.provider.configure",
    "agent.start",
    "agent.restore",
    "agent.event",
    "agent.research.workflow",
    "agent.research.workflow.continue",
    "agent.task.snapshot",
];
pub const AGENT_RENDERER_REQUEST_KINDS: &[&str] = &[
    "diagnostics.status",
    "agent.provider.test",
    "agent.provider.configure",
    "agent.start",
    "agent.event",
    "agent.research.workflow",
];
pub const HOST_RENDERER_REQUEST_KINDS: &[&str] = &[
    "diagnostics.status",
    "window.state",
    "window.minimize",
    "window.toggle_maximize",
    "window.begin_drag",
    "window.system_menu",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope {
    pub protocol_version: u32,
    pub request_id: String,
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub deadline_ms: Option<u64>,
    #[serde(default)]
    pub cancellation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseEnvelope {
    pub protocol_version: u32,
    pub request_id: String,
    pub kind: String,
    pub ok: bool,
    #[serde(default)]
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamEnvelope {
    pub protocol_version: u32,
    pub stream_id: String,
    pub seq: u64,
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPhase {
    Idle,
    Preparing,
    WaitingForUser,
    Reasoning,
    AwaitingTools,
    Reviewing,
    Synthesizing,
    Verifying,
    Suspended,
    Completed,
    VerificationFailed,
    Cancelled,
    HardFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub objective: String,
    pub security_universe: Vec<String>,
    pub as_of: String,
    pub research_start: String,
    pub research_end: String,
    pub investment_horizon: String,
    pub comparison_benchmark: String,
    pub output_type: String,
    pub evidence_requirement: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClarificationOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub recommended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClarificationQuestion {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub question: String,
    pub kind: String,
    pub options: Vec<ClarificationOption>,
    pub allow_other: bool,
    #[serde(default)]
    pub target_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClarificationRequest {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub questions: Vec<ClarificationQuestion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClarificationAnswer {
    pub question_id: String,
    pub option_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    pub decision_mode: String,
}

pub type AgentQuestion = ClarificationQuestion;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationSummary {
    pub conversation_id: String,
    pub title: String,
    pub phase: AgentPhase,
    pub message_count: u64,
    pub evidence_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_from_message_id: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCheckpoint {
    pub task_id: String,
    pub phase: AgentPhase,
    pub accepted_seq: u64,
    pub pending_tool_ids: Vec<String>,
    pub completed_tool_ids: Vec<String>,
    pub evidence_ids: Vec<String>,
    pub state_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolActivity {
    pub call_id: String,
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub status: String,
    pub cache_hit: bool,
    pub evidence_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    pub evidence_id: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_version_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
    pub quality_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationFinding {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub evidence_ids: Vec<String>,
    pub blocking: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderQuota {
    pub provider: String,
    pub model_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_remaining_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_reset_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekly_used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekly_total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekly_remaining_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekly_reset_at_ms: Option<u64>,
}
