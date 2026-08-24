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

function completePerformance() {
  const metric = (id, value, comparison, budget, aggregation, unit, count) => ({
    id, value, comparison, budget, aggregation, unit, status: "PASSED", samples: Array(count).fill(value),
  });
  return {
    ...base("performance-budgets", ["measurement-environment"]),
    environment: {
      mode: "packaged-proton-cef",
      cpu: "test cpu",
      gpu: "test gpu",
      power_profile: "balanced",
      memory_bytes: 16 * 1024 ** 3,
      display_scale_pct: 100,
      proton_skeleton_sha256: "b".repeat(64),
      application_package_sha256: "c".repeat(64),
    },
    metrics: [
      metric("workspace_restore_p95_ms", 1000, "<=", 1500, "p95", "ms", 30),
      metric("command_feedback_p95_ms", 50, "<=", 100, "p95", "ms", 30),
      metric("logical_rows_scroll_fps", 60, ">=", 50, "p05", "fps", 10),
      metric("agent_render_hz", 5, "<=", 10, "max", "hz", 10),
      metric("idle_cpu_p95_pct", 1, "<", 2, "p95", "pct", 60),
      { ...metric("cold_start_regression_pct", 10, "<=", 15, "p95_regression", "pct", 10), samples: Array(10).fill(110), baseline_samples: Array(10).fill(100) },
      { ...metric("memory_regression_pct", 10, "<=", 15, "p95_regression", "pct", 10), samples: Array(10).fill(110), baseline_samples: Array(10).fill(100) },
    ],
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

test("accepts complete core fault evidence without pretending renderer coverage", () => {
  const ids = [
    "engine-kill",
    "agent-kill",
    "checkpoint-before-crash",
    "checkpoint-after-crash",
    "provider-stream-break",
    "quota-suspension-resume",
    "oversized-ipc",
    "corrupt-ipc",
    "duplicate-ipc",
    "out-of-order-ipc",
    "cancel-safety",
    "sqlite-lock",
  ];
  assert.equal(validateEvidence(base("fault-injection-core", ids), "fault-injection-core", commit).cases, 12);
});

test("full fault evidence still requires desktop renderer and GPU failures", () => {
  const evidence = base("fault-injection", [
    "engine-kill",
    "agent-kill",
    "checkpoint-before-crash",
    "checkpoint-after-crash",
    "provider-stream-break",
    "quota-suspension-resume",
    "oversized-ipc",
    "corrupt-ipc",
    "duplicate-ipc",
    "out-of-order-ipc",
    "cancel-safety",
    "sqlite-lock",
  ]);
  assert.throws(() => validateEvidence(evidence, "fault-injection", commit), /renderer-kill/);
});

test("rejects a performance metric outside its budget", () => {
  const evidence = completePerformance();
  evidence.metrics[0].value = 1600;
  evidence.metrics[0].samples = Array(30).fill(1600);
  assert.throws(() => validateEvidence(evidence, "performance-budgets", commit), /exceeds its release budget/);
});

test("rejects a performance claim without a statistically meaningful sample", () => {
  const evidence = completePerformance();
  evidence.metrics[1].samples = [50];
  assert.throws(() => validateEvidence(evidence, "performance-budgets", commit), /at least 30 samples/);
});

test("recomputes performance aggregates instead of trusting the claimed value", () => {
  const evidence = completePerformance();
  evidence.metrics[2].value = 90;
  assert.throws(() => validateEvidence(evidence, "performance-budgets", commit), /does not match its p05 samples/);
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
