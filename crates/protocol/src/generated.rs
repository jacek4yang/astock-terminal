// GENERATED from protocol/schema; schema-sha256=8f1603bac38077febe95d9d410a95cd62c2a19cb08c4537f54bdd3e4f24af703
// Run: node protocol/codegen.mjs

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PAGE_SIZE: usize = 500;
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
