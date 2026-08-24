// GENERATED from protocol/schema; schema-sha256=30951f97fd73e190476ec8161f3728d1ea65fcec41d5f4d4d0975f94f1765b6d
// Run: node protocol/codegen.mjs

export const PROTOCOL_VERSION = 1 as const;
export const MAX_FRAME_BYTES = 8 * 1024 * 1024;
export const MAX_PAGE_SIZE = 500;
export const RELEASE_VERSION = "6.0.0" as const;
export const ENGINE_STARTUP_REQUIRED_CAPABILITIES = ["market","research","data_quality","agent_advanced_analysis_v1","storage","credentials","agent_event_store_v2"] as const;
export const AGENT_STARTUP_REQUIRED_CAPABILITIES = ["pure_reducer","replay","evidence_gate","advanced_tool_planning","closed_engine_effects","deterministic_report_verification","sse_stream_recovery"] as const;
export const ENGINE_REQUEST_KINDS = ["system.handshake","system.shutdown","system.cancel","diagnostics.status","diagnostics.data_quality","market.session","market.overview","market.search","market.quote","market.kline","market.index_kline","market.shares.page","market.security_snapshot","market.order_book","market.minute","market.fund_flow.daily","market.fund_flow.realtime","research.market_context","research.agent_prepare_context","research.agent_security_context","research.agent_report_verify","research.global_context","research.global.sync.start","research.global.sync.status","research.global.sync.cancel","research.global.providers","research.global.documents","research.global.chains","research.global.transmission","research.security_events","research.market_candidates","research.market_pool","research.board.constituents","analysis.chanlun.minute","research.disclosures.list","research.disclosures.detail","research.disclosures.providers","research.disclosures.sync.start","research.disclosures.sync.status","research.disclosures.sync.cancel","research.news.providers","research.news.center","research.news.provider.set","research.news.archive.recent","research.news.archive.revisions","research.news.archive.integrity","research.news.archive.observations","research.news.user_state","research.news.clusters.list","research.news.clusters.detail","research.news.clusters.merge","research.news.clusters.split","research.news.reviews.list","research.news.reviews.resolve","research.entities.links","research.entities.reviews","research.entities.resolve","research.events.analysis.start","research.events.analysis.status","research.events.analysis.cancel","research.relations.extraction.start","research.relations.extraction.status","research.relations.extraction.cancel","research.relations.reviews","research.relations.review","research.relations.retract","research.graph.subgraph","research.graph.as_of","research.graph.history_bounds","research.graph.edge_timeline","research.graph.snapshot.get","research.graph.snapshot.diff","research.graph.shock","research.market.relationship","research.quant.start","research.quant.status","research.quant.cancel","research.quant.snapshots.get","research.quant.snapshots.list","research.backtest.strategies","research.backtest.run","research.backtest.start","research.backtest.status","research.backtest.cancel","research.market.regime","research.fundamentals","research.earnings_driver.tree","research.earnings_driver.shock","research.earnings_driver.snapshot","research.news","research.data_reconcile","research.quote_reconcile","research.valuation_reconcile","research.joinquant_context","research.optional_sources","research.sources.list","research.sources.get","research.sources.fetch","research.sources.compare","workspace.watchlist.list","workspace.watchlist.add","workspace.watchlist.remove","workspace.watchlist.pin","credentials.status","credentials.provider.set","credentials.provider.delete","credentials.minimax.set","credentials.minimax.delete","credentials.minimax.quota","credentials.joinquant.set","credentials.joinquant.delete","storage.cache.stats","storage.cache.cleanup","storage.data_root.migrate","storage.data_root.rollback","quant.scan.start","quant.scan.status","quant.scan.cancel","settings.agent_models.get","settings.agent_models.set","agent.task.create","agent.event.append","agent.checkpoint.put","agent.effect.begin","agent.effect.complete","agent.effect.list","agent.task.load","agent.task.list","agent.conversation.save","agent.conversation.load","agent.conversation.list","agent.conversation.rename","agent.conversation.branch","agent.conversation.delete"] as const;
export type EngineRequestKind = (typeof ENGINE_REQUEST_KINDS)[number];
export const ENGINE_RENDERER_REQUEST_KINDS = ["diagnostics.status","diagnostics.data_quality","market.session","market.overview","market.search","market.quote","market.kline","market.index_kline","market.shares.page","market.security_snapshot","market.order_book","market.minute","market.fund_flow.daily","market.fund_flow.realtime","research.global.sync.start","research.global.sync.status","research.global.sync.cancel","research.global.providers","research.global.documents","research.global.chains","research.global.transmission","research.market_pool","research.board.constituents","analysis.chanlun.minute","research.disclosures.list","research.disclosures.detail","research.disclosures.providers","research.disclosures.sync.start","research.disclosures.sync.status","research.disclosures.sync.cancel","research.news.providers","research.news.center","research.news.provider.set","research.news.archive.recent","research.news.archive.revisions","research.news.archive.integrity","research.news.archive.observations","research.news.user_state","research.news.clusters.list","research.news.clusters.detail","research.news.clusters.merge","research.news.clusters.split","research.news.reviews.list","research.news.reviews.resolve","research.entities.links","research.entities.reviews","research.entities.resolve","research.events.analysis.start","research.events.analysis.status","research.events.analysis.cancel","research.relations.extraction.start","research.relations.extraction.status","research.relations.extraction.cancel","research.relations.reviews","research.relations.review","research.relations.retract","research.graph.subgraph","research.graph.as_of","research.graph.history_bounds","research.graph.edge_timeline","research.graph.snapshot.get","research.graph.snapshot.diff","research.graph.shock","research.market.relationship","research.quant.start","research.quant.status","research.quant.cancel","research.quant.snapshots.get","research.quant.snapshots.list","research.backtest.strategies","research.backtest.run","research.backtest.start","research.backtest.status","research.backtest.cancel","research.market.regime","research.earnings_driver.tree","research.earnings_driver.shock","research.earnings_driver.snapshot","research.news","research.quote_reconcile","research.valuation_reconcile","research.sources.list","research.sources.get","research.sources.fetch","research.sources.compare","workspace.watchlist.list","workspace.watchlist.add","workspace.watchlist.remove","workspace.watchlist.pin","credentials.status","credentials.provider.set","credentials.provider.delete","credentials.minimax.set","credentials.minimax.delete","credentials.minimax.quota","credentials.joinquant.set","credentials.joinquant.delete","storage.cache.stats","storage.cache.cleanup","storage.data_root.migrate","storage.data_root.rollback","quant.scan.start","quant.scan.status","quant.scan.cancel","settings.agent_models.get","settings.agent_models.set","agent.task.load","agent.conversation.save","agent.conversation.load","agent.conversation.list","agent.conversation.rename","agent.conversation.branch","agent.conversation.delete"] as const;
export type EngineRendererRequestKind = (typeof ENGINE_RENDERER_REQUEST_KINDS)[number];
export const AGENT_REQUEST_KINDS = ["system.handshake","diagnostics.status","agent.provider.test","agent.provider.configure","agent.start","agent.restore","agent.event","agent.research.workflow","agent.research.workflow.continue","agent.task.snapshot"] as const;
export type AgentRequestKind = (typeof AGENT_REQUEST_KINDS)[number];
export const AGENT_RENDERER_REQUEST_KINDS = ["diagnostics.status","agent.provider.test","agent.provider.configure","agent.start","agent.event","agent.research.workflow"] as const;
export type AgentRendererRequestKind = (typeof AGENT_RENDERER_REQUEST_KINDS)[number];
export const AGENT_SERVICE_METHODS = ["task.create","task.list","task.get","task.branch","task.resume","task.cancel","task.answer"] as const;
export type AgentServiceMethod = (typeof AGENT_SERVICE_METHODS)[number];
export const HOST_RENDERER_REQUEST_KINDS = ["diagnostics.status","window.state","window.minimize","window.toggle_maximize","window.begin_drag","window.system_menu"] as const;
export type HostRendererRequestKind = (typeof HOST_RENDERER_REQUEST_KINDS)[number];
export type AgentPhase = "idle" | "preparing" | "waiting_for_user" | "reasoning" | "awaiting_tools" | "reviewing" | "synthesizing" | "verifying" | "suspended" | "completed" | "verification_failed" | "cancelled" | "hard_failed";

export interface RequestEnvelope<T = unknown> {
  protocol_version: typeof PROTOCOL_VERSION;
  request_id: string;
  kind: string;
  payload: T;
  deadline_ms?: number | null;
  cancellation_id?: string | null;
}

export interface ProtocolError {
  code: string;
  message: string;
  retryable: boolean;
  details?: unknown;
}

export interface ResponseEnvelope<T = unknown> {
  protocol_version: typeof PROTOCOL_VERSION;
  request_id: string;
  kind: string;
  ok: boolean;
  payload: T;
  error?: ProtocolError;
}

export interface StreamEnvelope<T = unknown> {
  protocol_version: typeof PROTOCOL_VERSION;
  stream_id: string;
  seq: number;
  kind: string;
  payload: T;
}

export interface TaskSpec {
  objective: string;
  security_universe: string[];
  as_of: string;
  research_start: string;
  research_end: string;
  investment_horizon: string;
  comparison_benchmark: string;
  output_type: "research_report" | "manual_plan" | "evidence_review";
  evidence_requirement: "standard" | "strict" | "primary_sources";
}

export interface ClarificationOption {
  id: string;
  label: string;
  description?: string | null;
  recommended: boolean;
}

export interface ClarificationQuestion {
  id: string;
  header?: string | null;
  question: string;
  kind: "single" | "multiple";
  options: ClarificationOption[];
  allow_other: boolean;
  target_fields?: string[];
}

export interface ClarificationRequest {
  title: string;
  description?: string | null;
  questions: ClarificationQuestion[];
}

export interface ClarificationAnswer {
  question_id: string;
  option_ids: string[];
  answer?: string | null;
  decision_mode: "user_selected" | "agent_best_with_evidence";
}

export type AgentQuestion = ClarificationQuestion;

export interface ConversationSummary {
  conversation_id: string;
  title: string;
  phase: AgentPhase;
  message_count: number;
  evidence_count: number;
  parent_conversation_id?: string | null;
  branch_from_message_id?: string | null;
  created_at: number;
  updated_at: number;
}

export interface TaskCheckpoint {
  task_id: string;
  phase: AgentPhase;
  accepted_seq: number;
  pending_tool_ids: string[];
  completed_tool_ids: string[];
  evidence_ids: string[];
  state_version: string;
}

export interface ToolActivity {
  call_id: string;
  kind: string;
  title: string;
  detail: string;
  status: "pending" | "running" | "succeeded" | "failed" | "skipped";
  cache_hit: boolean;
  evidence_count: number;
  started_at_ms?: number | null;
  finished_at_ms?: number | null;
}

export interface EvidenceRef {
  evidence_id: string;
  source: string;
  source_version_id?: string | null;
  as_of?: string | null;
  fetched_at?: string | null;
  quality_status: "verified" | "single_source" | "stale" | "conflicting" | "missing" | "blocked";
  original_url?: string | null;
}

export interface VerificationFinding {
  code: string;
  severity: "info" | "warning" | "error";
  message: string;
  evidence_ids: string[];
  blocking: boolean;
}

export interface ProviderQuota {
  provider: string;
  model_name: string;
  interval_used?: number | null;
  interval_total?: number | null;
  interval_remaining_percent?: number | null;
  interval_reset_at_ms?: number | null;
  weekly_used?: number | null;
  weekly_total?: number | null;
  weekly_remaining_percent?: number | null;
  weekly_reset_at_ms?: number | null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function isRequestEnvelope(value: unknown): value is RequestEnvelope {
  if (!isRecord(value)) return false;
  return value.protocol_version === PROTOCOL_VERSION &&
    typeof value.request_id === "string" && value.request_id.length > 0 &&
    typeof value.kind === "string" && value.kind.length > 0 && "payload" in value &&
    (value.deadline_ms == null || (typeof value.deadline_ms === "number" && Number.isSafeInteger(value.deadline_ms) && value.deadline_ms >= 0)) &&
    (value.cancellation_id == null || typeof value.cancellation_id === "string");
}

export function parseResponseEnvelope<T = unknown>(value: unknown): ResponseEnvelope<T> {
  if (!isRecord(value) || value.protocol_version !== PROTOCOL_VERSION ||
      typeof value.request_id !== "string" || typeof value.kind !== "string" ||
      typeof value.ok !== "boolean" || !("payload" in value)) {
    throw new Error("Invalid native response envelope");
  }
  if (value.error != null && (!isRecord(value.error) || typeof value.error.code !== "string" ||
      typeof value.error.message !== "string" || typeof value.error.retryable !== "boolean")) {
    throw new Error("Invalid native protocol error");
  }
  return value as unknown as ResponseEnvelope<T>;
}
