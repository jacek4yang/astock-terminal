import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const PROTOCOL_VERSION = 1;
export const MAX_FRAME_BYTES = 8 * 1024 * 1024;
export const MAX_PAGE_SIZE = 500;

const schemaDirectory = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..", "protocol", "schema");

function serviceContract(fileName) {
  const schema = JSON.parse(readFileSync(resolve(schemaDirectory, fileName), "utf8"));
  return Object.freeze({
    version: schema.properties.service_version.const,
    requiredCapabilities: Object.freeze(
      schema.properties.startup_required_capabilities.prefixItems.map((item) => item.const),
    ),
  });
}

export const ENGINE_HANDSHAKE_CONTRACT = serviceContract("engine.schema.json");
export const AGENT_HANDSHAKE_CONTRACT = serviceContract("agent.schema.json");

if (ENGINE_HANDSHAKE_CONTRACT.version !== AGENT_HANDSHAKE_CONTRACT.version) {
  throw new Error("Engine and Agent protocol schemas declare different release versions");
}

export const RELEASE_VERSION = ENGINE_HANDSHAKE_CONTRACT.version;

function record(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : null;
}

function requireCapabilities(role, actual, required) {
  if (!Array.isArray(actual) || actual.some((item) => typeof item !== "string")) {
    throw new Error(`${role} handshake capabilities are malformed`);
  }
  for (const capability of required) {
    if (!actual.includes(capability)) {
      throw new Error(`${role} handshake is missing required startup capability ${capability}`);
    }
  }
}

export function inferHandshakeRole(payload) {
  const value = record(payload);
  if (typeof value?.engine_version === "string" && value.agent_version == null) return "engine";
  if (typeof value?.agent_version === "string" && value.engine_version == null) return "agent";
  throw new Error("Worker handshake payload does not identify exactly one supported role");
}

export function validateHandshakePayload(role, payload) {
  const value = record(payload);
  if (!value || value.protocol_version !== PROTOCOL_VERSION || value.max_frame_bytes !== MAX_FRAME_BYTES) {
    throw new Error(`${role} returned incompatible protocol limits`);
  }
  if (role === "engine") {
    if (value.engine_version !== ENGINE_HANDSHAKE_CONTRACT.version || value.max_page_size !== MAX_PAGE_SIZE) {
      throw new Error("Engine returned an incompatible service version or page limit");
    }
    requireCapabilities("Engine", value.capabilities, ENGINE_HANDSHAKE_CONTRACT.requiredCapabilities);
  } else if (role === "agent") {
    if (value.agent_version !== AGENT_HANDSHAKE_CONTRACT.version || value.reducer_version !== "moonbit-agent-kernel-v1") {
      throw new Error("Agent returned an incompatible service or reducer version");
    }
    requireCapabilities("Agent", value.capabilities, AGENT_HANDSHAKE_CONTRACT.requiredCapabilities);
  } else {
    throw new Error(`Unsupported Worker handshake role ${role}`);
  }
  return value;
}

export function validateHandshakeResponse(response, { role, requestId } = {}) {
  const value = record(response);
  if (!value || value.protocol_version !== PROTOCOL_VERSION || value.request_id !== requestId ||
      value.kind !== "system.handshake" || value.ok !== true) {
    throw new Error(`Invalid ${role ?? "Worker"} handshake response envelope`);
  }
  const resolvedRole = role ?? inferHandshakeRole(value.payload);
  return validateHandshakePayload(resolvedRole, value.payload);
}
