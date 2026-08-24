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
const kinds = schemas["engine.schema.json"].properties.request_kinds.prefixItems.map((item) => item.const);
const engineRendererKinds = schemas["engine.schema.json"].properties.renderer_request_kinds.prefixItems.map((item) => item.const);
const agentKinds = schemas["agent.schema.json"].properties.request_kinds.prefixItems.map((item) => item.const);
const agentRendererKinds = schemas["agent.schema.json"].properties.renderer_request_kinds.prefixItems.map((item) => item.const);
const hostRendererKinds = schemas["host.schema.json"].properties.renderer_request_kinds.prefixItems.map((item) => item.const);
const header = (comment) => `${comment} GENERATED from protocol/schema; schema-sha256=${hash}\n${comment} Run: node protocol/codegen.mjs\n`;

const rust = `${header("//")}
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PAGE_SIZE: usize = 500;
pub const ENGINE_REQUEST_KINDS: &[&str] = &[${kinds.map((kind) => `\n    "${kind}",`).join("")}\n];
pub const ENGINE_RENDERER_REQUEST_KINDS: &[&str] = &[${engineRendererKinds.map((kind) => `\n    "${kind}",`).join("")}\n];
pub const AGENT_REQUEST_KINDS: &[&str] = &[${agentKinds.map((kind) => `\n    "${kind}",`).join("")}\n];
pub const AGENT_RENDERER_REQUEST_KINDS: &[&str] = &[${agentRendererKinds.map((kind) => `\n    "${kind}",`).join("")}\n];
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
`;

const typescript = `${header("//")}
export const PROTOCOL_VERSION = 1 as const;
export const MAX_FRAME_BYTES = 8 * 1024 * 1024;
export const MAX_PAGE_SIZE = 500;
export const ENGINE_REQUEST_KINDS = ${JSON.stringify(kinds)} as const;
export type EngineRequestKind = (typeof ENGINE_REQUEST_KINDS)[number];
export const ENGINE_RENDERER_REQUEST_KINDS = ${JSON.stringify(engineRendererKinds)} as const;
export type EngineRendererRequestKind = (typeof ENGINE_RENDERER_REQUEST_KINDS)[number];
export const AGENT_REQUEST_KINDS = ${JSON.stringify(agentKinds)} as const;
export type AgentRequestKind = (typeof AGENT_REQUEST_KINDS)[number];
export const AGENT_RENDERER_REQUEST_KINDS = ${JSON.stringify(agentRendererKinds)} as const;
export type AgentRendererRequestKind = (typeof AGENT_RENDERER_REQUEST_KINDS)[number];
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
pub let agent_request_kinds : Array[String] = ${JSON.stringify(agentKinds)}

///|
pub let agent_renderer_request_kinds : Array[String] = ${JSON.stringify(agentRendererKinds)}

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
`;

const moonStringPredicate = (functionName, values) => `///|
fn ${functionName}(kind : String) -> Bool {
  ${values.map((kind) => `kind == "${kind}"`).join(" ||\n  ")}
}
`;

const desktopHostKinds = `${header("///")}
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
