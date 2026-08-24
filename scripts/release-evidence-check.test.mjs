import test from "node:test";
import assert from "node:assert/strict";

import { validateEvidence } from "./release-evidence-check.mjs";

const commit = "a".repeat(40);

function base(gate, caseIds) {
  return {
    schema_version: 1,
    gate,
    status: "PASSED",
    commit,
    started_at_utc: "2026-08-24T00:00:00.000Z",
    completed_at_utc: "2026-08-24T00:01:00.000Z",
    runner: { os: "Windows 11", arch: "x64" },
    cases: caseIds.map((id) => ({ id, status: "PASSED", duration_ms: 1 })),
  };
}

test("rejects a four-field placeholder masquerading as browser evidence", () => {
  assert.throws(
    () => validateEvidence({ gate: "browser-cdp", status: "PASSED", commit, completed_at_utc: "2026-08-24T00:01:00.000Z" }, "browser-cdp", commit),
    /schema_version/,
  );
});

test("requires all named browser scenarios", () => {
  const evidence = base("browser-cdp", ["market-overview"]);
  assert.throws(() => validateEvidence(evidence, "browser-cdp", commit), /stock-detail/);
});

test("rejects a performance metric outside its budget", () => {
  const evidence = base("performance-budgets", ["measurement-environment"]);
  evidence.metrics = [
    { id: "workspace_restore_p95_ms", value: 1600, comparison: "<=", budget: 1500, status: "PASSED" },
  ];
  assert.throws(() => validateEvidence(evidence, "performance-budgets", commit), /exceeds its release budget/);
});

test("accepts complete credential-rotation evidence without embedded secrets", () => {
  const evidence = {
    ...base("credential-rotation", ["minimax", "joinquant"]),
    minimax_rotated: true,
    joinquant_rotated: true,
    old_credentials_revoked: true,
    credential_manager_readback_verified: true,
    secrets_in_evidence: false,
  };
  assert.equal(validateEvidence(evidence, "credential-rotation", commit).cases, 2);
});
