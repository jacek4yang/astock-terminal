import assert from "node:assert/strict";
import test from "node:test";
import {
  AGENT_HANDSHAKE_CONTRACT,
  ENGINE_HANDSHAKE_CONTRACT,
  MAX_FRAME_BYTES,
  RELEASE_VERSION,
  validateHandshakePayload,
  validateHandshakeResponse,
} from "./handshake-contract.mjs";

const enginePayload = {
  protocol_version: 1,
  engine_version: RELEASE_VERSION,
  capabilities: [...ENGINE_HANDSHAKE_CONTRACT.requiredCapabilities, "future_capability"],
  max_frame_bytes: MAX_FRAME_BYTES,
  max_page_size: 500,
};

const agentPayload = {
  protocol_version: 1,
  agent_version: RELEASE_VERSION,
  reducer_version: "moonbit-agent-kernel-v1",
  capabilities: [...AGENT_HANDSHAKE_CONTRACT.requiredCapabilities],
  max_frame_bytes: MAX_FRAME_BYTES,
};

test("accepts exact versions, limits, and required capability subsets", () => {
  assert.equal(validateHandshakePayload("engine", enginePayload), enginePayload);
  assert.equal(validateHandshakePayload("agent", agentPayload), agentPayload);
  assert.equal(validateHandshakeResponse({
    protocol_version: 1,
    request_id: "correlated",
    kind: "system.handshake",
    ok: true,
    payload: enginePayload,
  }, { requestId: "correlated" }), enginePayload);
});

test("rejects version drift, missing capabilities, and uncorrelated envelopes", () => {
  assert.throws(
    () => validateHandshakePayload("engine", { ...enginePayload, engine_version: "5.9.9" }),
    /incompatible service version/,
  );
  assert.throws(
    () => validateHandshakePayload("agent", { ...agentPayload, capabilities: ["pure_reducer"] }),
    /missing required startup capability/,
  );
  assert.throws(
    () => validateHandshakeResponse({
      protocol_version: 1,
      request_id: "wrong",
      kind: "system.handshake",
      ok: true,
      payload: agentPayload,
    }, { role: "agent", requestId: "expected" }),
    /Invalid agent handshake response envelope/,
  );
});
