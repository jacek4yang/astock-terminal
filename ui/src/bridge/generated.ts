// GENERATED from protocol/schema; schema-sha256=32f67c82cd37a569555467a4a7a27d104c38f689ffbbe0e87e0ce9828c60cdf9
// Run: node protocol/codegen.mjs

export const PROTOCOL_VERSION = 1 as const;
export const MAX_FRAME_BYTES = 8 * 1024 * 1024;
export const MAX_PAGE_SIZE = 500;
export const ENGINE_REQUEST_KINDS = ["system.handshake","system.shutdown","system.cancel","diagnostics.status","diagnostics.data_quality","market.session","market.overview","market.search","market.quote","market.kline","market.index_kline","market.shares.page","market.security_snapshot","market.order_book","market.minute","market.fund_flow.daily","market.fund_flow.realtime","research.market_context","research.global_context","research.security_events","research.market_candidates","research.fundamentals","research.earnings_driver.tree","research.earnings_driver.shock","research.earnings_driver.snapshot","research.news","research.data_reconcile","research.quote_reconcile","research.valuation_reconcile","research.joinquant_context","research.sources.list","research.sources.get","research.sources.fetch","research.sources.compare","workspace.watchlist.list","workspace.watchlist.add","workspace.watchlist.remove","workspace.watchlist.pin","credentials.status","credentials.provider.set","credentials.provider.delete","credentials.minimax.set","credentials.minimax.delete","credentials.minimax.quota","credentials.joinquant.set","credentials.joinquant.delete","storage.cache.stats","storage.cache.cleanup","quant.scan.start","quant.scan.status","quant.scan.cancel","agent.task.create","agent.event.append","agent.checkpoint.put","agent.effect.begin","agent.effect.complete","agent.effect.list","agent.task.load","agent.task.list","agent.conversation.save","agent.conversation.load","agent.conversation.list","agent.conversation.rename","agent.conversation.branch","agent.conversation.delete"] as const;
export type EngineRequestKind = (typeof ENGINE_REQUEST_KINDS)[number];
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
