import { readFile, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const schemaDir = resolve(root, "protocol/schema");
const schemaFiles = ["envelope.schema.json", "agent.schema.json", "engine.schema.json", "host.schema.json"];
const canonicalText = (value) => value.replace(/\r\n?/g, "\n");
const sources = await Promise.all(
  schemaFiles.map(async (name) => canonicalText(await readFile(resolve(schemaDir, name), "utf8"))),
);
const schemas = Object.fromEntries(schemaFiles.map((name, index) => [name, JSON.parse(sources[index])]));
const hash = createHash("sha256").update(sources.join("\n")).digest("hex");
const phases = schemas["agent.schema.json"].$defs.agent_phase.enum;
const engineVersion = schemas["engine.schema.json"].properties.service_version.const;
const agentVersion = schemas["agent.schema.json"].properties.service_version.const;
if (engineVersion !== agentVersion) throw new Error("Engine and Agent service versions must match");
const releaseVersion = engineVersion;
const engineRequiredCapabilities = schemas["engine.schema.json"].properties.startup_required_capabilities.prefixItems.map((item) => item.const);
const agentRequiredCapabilities = schemas["agent.schema.json"].properties.startup_required_capabilities.prefixItems.map((item) => item.const);
const kinds = schemas["engine.schema.json"].properties.request_kinds.prefixItems.map((item) => item.const);
const engineRendererKinds = schemas["engine.schema.json"].properties.renderer_request_kinds.prefixItems.map((item) => item.const);
const agentKinds = schemas["agent.schema.json"].properties.request_kinds.prefixItems.map((item) => item.const);
const agentRendererKinds = schemas["agent.schema.json"].properties.renderer_request_kinds.prefixItems.map((item) => item.const);
const agentServiceMethods = schemas["agent.schema.json"].properties.service_methods.prefixItems.map((item) => item.const);
const hostRendererKinds = schemas["host.schema.json"].properties.renderer_request_kinds.prefixItems.map((item) => item.const);
const header = (comment) => `${comment} GENERATED from protocol/schema; schema-sha256=${hash}\n${comment} Run: node protocol/codegen.mjs\n`;

const rust = `${header("//")}
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PAGE_SIZE: usize = 500;
pub const RELEASE_VERSION: &str = "${releaseVersion}";
pub const ENGINE_STARTUP_REQUIRED_CAPABILITIES: &[&str] = &[${engineRequiredCapabilities.map((capability) => `\n    "${capability}",`).join("")}\n];
pub const AGENT_STARTUP_REQUIRED_CAPABILITIES: &[&str] = &[${agentRequiredCapabilities.map((capability) => `\n    "${capability}",`).join("")}\n];
pub const ENGINE_REQUEST_KINDS: &[&str] = &[${kinds.map((kind) => `\n    "${kind}",`).join("")}\n];
pub const ENGINE_RENDERER_REQUEST_KINDS: &[&str] = &[${engineRendererKinds.map((kind) => `\n    "${kind}",`).join("")}\n];
pub const AGENT_REQUEST_KINDS: &[&str] = &[${agentKinds.map((kind) => `\n    "${kind}",`).join("")}\n];
pub const AGENT_RENDERER_REQUEST_KINDS: &[&str] = &[${agentRendererKinds.map((kind) => `\n    "${kind}",`).join("")}\n];
pub const AGENT_SERVICE_METHODS: &[&str] = &[${agentServiceMethods.map((method) => `\n    "${method}",`).join("")}\n];
pub const HOST_RENDERER_REQUEST_KINDS: &[&str] = &[${hostRendererKinds.map((kind) => `\n    "${kind}",`).join("")}\n];

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
pub enum AgentPhase {${phases.map((phase) => `\n    ${phase.split("_").map((part) => part[0].toUpperCase() + part.slice(1)).join("")},`).join("")}\n}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_from_checkpoint_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_from_checkpoint_seq: Option<u64>,
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
`;

const typescript = `${header("//")}
export const PROTOCOL_VERSION = 1 as const;
export const MAX_FRAME_BYTES = 8 * 1024 * 1024;
export const MAX_PAGE_SIZE = 500;
export const RELEASE_VERSION = "${releaseVersion}" as const;
export const ENGINE_STARTUP_REQUIRED_CAPABILITIES = ${JSON.stringify(engineRequiredCapabilities)} as const;
export const AGENT_STARTUP_REQUIRED_CAPABILITIES = ${JSON.stringify(agentRequiredCapabilities)} as const;
export const ENGINE_REQUEST_KINDS = ${JSON.stringify(kinds)} as const;
export type EngineRequestKind = (typeof ENGINE_REQUEST_KINDS)[number];
export const ENGINE_RENDERER_REQUEST_KINDS = ${JSON.stringify(engineRendererKinds)} as const;
export type EngineRendererRequestKind = (typeof ENGINE_RENDERER_REQUEST_KINDS)[number];
export const AGENT_REQUEST_KINDS = ${JSON.stringify(agentKinds)} as const;
export type AgentRequestKind = (typeof AGENT_REQUEST_KINDS)[number];
export const AGENT_RENDERER_REQUEST_KINDS = ${JSON.stringify(agentRendererKinds)} as const;
export type AgentRendererRequestKind = (typeof AGENT_RENDERER_REQUEST_KINDS)[number];
export const AGENT_SERVICE_METHODS = ${JSON.stringify(agentServiceMethods)} as const;
export type AgentServiceMethod = (typeof AGENT_SERVICE_METHODS)[number];
export const HOST_RENDERER_REQUEST_KINDS = ${JSON.stringify(hostRendererKinds)} as const;
export type HostRendererRequestKind = (typeof HOST_RENDERER_REQUEST_KINDS)[number];
export type AgentPhase = ${phases.map((phase) => `"${phase}"`).join(" | ")};

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
  branch_from_checkpoint_task_id?: string | null;
  branch_from_checkpoint_seq?: number | null;
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
`;

const moonbit = `${header("//")}
///|
pub let protocol_version : Int = 1

///|
pub let max_frame_bytes : Int = 8 * 1024 * 1024

///|
pub let max_page_size : Int = 500

///|
pub let release_version : String = "${releaseVersion}"

///|
pub let engine_startup_required_capabilities : Array[String] = ${JSON.stringify(engineRequiredCapabilities)}

///|
pub let agent_startup_required_capabilities : Array[String] = ${JSON.stringify(agentRequiredCapabilities)}

///|
pub let agent_request_kinds : Array[String] = ${JSON.stringify(agentKinds)}

///|
pub let agent_renderer_request_kinds : Array[String] = ${JSON.stringify(agentRendererKinds)}

///|
pub let agent_service_methods : Array[String] = ${JSON.stringify(agentServiceMethods)}

///|
pub let host_renderer_request_kinds : Array[String] = ${JSON.stringify(hostRendererKinds)}

///|
pub(all) enum AgentPhase {${phases.map((phase) => `\n  ${phase.split("_").map((part) => part[0].toUpperCase() + part.slice(1)).join("")}`).join("")}\n} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend AgentPhase with ToJson::{to_json}

///|
pub extend AgentPhase with FromJson::{from_json}

///|
pub(all) struct TaskSpec {
  objective : String
  security_universe : Array[String]
  as_of : String
  research_start : String
  research_end : String
  investment_horizon : String
  comparison_benchmark : String
  output_type : String
  evidence_requirement : String
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend TaskSpec with ToJson::{to_json}

///|
pub extend TaskSpec with FromJson::{from_json}

///|
pub(all) struct ClarificationOption {
  id : String
  label : String
  description : String?
  recommended : Bool
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend ClarificationOption with ToJson::{to_json}

///|
pub extend ClarificationOption with FromJson::{from_json}

///|
pub(all) struct ClarificationQuestion {
  id : String
  header : String?
  question : String
  kind : String
  options : Array[ClarificationOption]
  allow_other : Bool
  target_fields : Array[String]
} derive(Debug, Eq, ToJson, FromJson)

///|
pub type AgentQuestion = ClarificationQuestion

///|
pub extend ClarificationQuestion with ToJson::{to_json}

///|
pub extend ClarificationQuestion with FromJson::{from_json}

///|
pub(all) struct ClarificationRequest {
  title : String
  description : String?
  questions : Array[ClarificationQuestion]
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend ClarificationRequest with ToJson::{to_json}

///|
pub extend ClarificationRequest with FromJson::{from_json}

///|
pub(all) struct ClarificationAnswer {
  question_id : String
  option_ids : Array[String]
  answer : String?
  decision_mode : String
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend ClarificationAnswer with ToJson::{to_json}

///|
pub extend ClarificationAnswer with FromJson::{from_json}

///|
pub(all) struct ConversationSummary {
  conversation_id : String
  title : String
  phase : AgentPhase
  message_count : Int
  evidence_count : Int
  parent_conversation_id : String?
  branch_from_message_id : String?
  branch_from_checkpoint_task_id : String?
  branch_from_checkpoint_seq : Int?
  created_at : Int64
  updated_at : Int64
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend ConversationSummary with ToJson::{to_json}

///|
pub extend ConversationSummary with FromJson::{from_json}

///|
pub(all) struct TaskCheckpoint {
  task_id : String
  phase : AgentPhase
  accepted_seq : Int
  pending_tool_ids : Array[String]
  completed_tool_ids : Array[String]
  evidence_ids : Array[String]
  state_version : String
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend TaskCheckpoint with ToJson::{to_json}

///|
pub extend TaskCheckpoint with FromJson::{from_json}

///|
pub(all) struct ToolActivity {
  call_id : String
  kind : String
  title : String
  detail : String
  status : String
  cache_hit : Bool
  evidence_count : Int
  started_at_ms : Int64?
  finished_at_ms : Int64?
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend ToolActivity with ToJson::{to_json}

///|
pub extend ToolActivity with FromJson::{from_json}

///|
pub(all) struct EvidenceRef {
  evidence_id : String
  source : String
  source_version_id : String?
  as_of : String?
  fetched_at : String?
  quality_status : String
  original_url : String?
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend EvidenceRef with ToJson::{to_json}

///|
pub extend EvidenceRef with FromJson::{from_json}

///|
pub(all) struct VerificationFinding {
  code : String
  severity : String
  message : String
  evidence_ids : Array[String]
  blocking : Bool
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend VerificationFinding with ToJson::{to_json}

///|
pub extend VerificationFinding with FromJson::{from_json}

///|
pub(all) struct ProviderQuota {
  provider : String
  model_name : String
  interval_used : Int64?
  interval_total : Int64?
  interval_remaining_percent : Double?
  interval_reset_at_ms : Int64?
  weekly_used : Int64?
  weekly_total : Int64?
  weekly_remaining_percent : Double?
  weekly_reset_at_ms : Int64?
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend ProviderQuota with ToJson::{to_json}

///|
pub extend ProviderQuota with FromJson::{from_json}
`;

const moonStringPredicate = (functionName, values) => `///|
fn ${functionName}(kind : String) -> Bool {
  ${values.map((kind) => `kind == "${kind}"`).join(" ||\n  ")}
}
`;

const desktopHostKinds = `${header("///")}
let protocol_release_version : String = "${releaseVersion}"

///|
let engine_startup_required_capabilities : Array[String] = ${JSON.stringify(engineRequiredCapabilities)}

///|
let agent_startup_required_capabilities : Array[String] = ${JSON.stringify(agentRequiredCapabilities)}

///|
${moonStringPredicate("renderer_engine_request_kind", engineRendererKinds)}
${moonStringPredicate("renderer_agent_request_kind", agentRendererKinds)}
${moonStringPredicate("renderer_host_request_kind", hostRendererKinds)}
`;

const outputs = [
  [resolve(root, "crates/protocol/src/generated.rs"), rust],
  [resolve(root, "ui/src/bridge/generated.ts"), typescript],
  [resolve(root, "app-moon/protocol/generated.mbt"), moonbit],
  [resolve(root, "desktop-moon/backend/host/request_kinds.g.mbt"), desktopHostKinds],
];

const check = process.argv.includes("--check");
let drift = false;
for (const [path, content] of outputs) {
  if (check) {
    let existing = "";
    try { existing = await readFile(path, "utf8"); } catch { drift = true; console.error(`missing ${path}`); continue; }
    if (canonicalText(existing) !== canonicalText(content)) {
      drift = true;
      console.error(`out of date ${path}`);
    }
  } else {
    await writeFile(path, content, "utf8");
  }
}
if (drift) process.exitCode = 1;
