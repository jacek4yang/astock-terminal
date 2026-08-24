import test, { after } from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { validateEvidence } from "./release-evidence-check.mjs";
import { BROWSER_CDP_SCENARIOS, DESKTOP_E2E_SCENARIOS, NATIVE_WINDOW_SCENARIOS } from "./release-scenarios.mjs";

const commit = "a".repeat(40);
const artifactRoot = fs.mkdtempSync(path.join(os.tmpdir(), "astock-release-evidence-"));
const artifactPath = path.join(artifactRoot, "trace.json");
fs.writeFileSync(artifactPath, '{"ok":true}\n', "utf8");
const artifactHash = crypto.createHash("sha256").update(fs.readFileSync(artifactPath)).digest("hex");
after(() => {
  fs.unlinkSync(artifactPath);
  fs.rmdirSync(artifactRoot);
});

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

function evidencedCases(caseIds) {
  return caseIds.map((id) => ({
    id,
    status: "PASSED",
    duration_ms: 1,
    assertion_count: 1,
    artifacts: [{
      kind: "test-trace",
      path: artifactPath,
      sha256: artifactHash,
      captured_at_utc: "2026-08-24T00:00:30.000Z",
    }],
  }));
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
  evidence.cases = evidencedCases(["market-overview"]);
  assert.throws(() => validateEvidence(evidence, "browser-cdp", commit), /stock-detail/);
});

test("pins exactly the approved v6 browser and desktop scenario catalogs", () => {
  assert.equal(BROWSER_CDP_SCENARIOS.length, 12);
  assert.equal(new Set(BROWSER_CDP_SCENARIOS).size, 12);
  assert.equal(NATIVE_WINDOW_SCENARIOS.length, 8);
  assert.equal(new Set(NATIVE_WINDOW_SCENARIOS).size, 8);
  assert.equal(DESKTOP_E2E_SCENARIOS.length, 40);
  assert.equal(new Set(DESKTOP_E2E_SCENARIOS).size, 40);
  assert.ok(DESKTOP_E2E_SCENARIOS.includes("normal-agent-research"));
  assert.ok(DESKTOP_E2E_SCENARIOS.includes("window-double-click-maximize"));
  assert.ok(DESKTOP_E2E_SCENARIOS.includes("release-no-debug-leakage-local-gate-disclosure"));
});

test("requires every native window case to carry a hashed trace", () => {
  const evidence = base("desktop-window-native", NATIVE_WINDOW_SCENARIOS);
  assert.throws(() => validateEvidence(evidence, "desktop-window-native", commit), /no assertions/);
  evidence.cases = evidencedCases(NATIVE_WINDOW_SCENARIOS);
  assert.doesNotThrow(() => validateEvidence(evidence, "desktop-window-native", commit));
});

test("rejects desktop cases padded with unnamed placeholders", () => {
  const ids = Array.from({ length: 40 }, (_, index) => `placeholder-${index}`);
  const evidence = base("desktop-e2e-40", ids);
  evidence.cases = evidencedCases(ids);
  assert.throws(() => validateEvidence(evidence, "desktop-e2e-40", commit), /packaged-launch/);
});

test("requires immutable per-case browser artifacts", () => {
  const evidence = base("browser-cdp", BROWSER_CDP_SCENARIOS);
  assert.throws(() => validateEvidence(evidence, "browser-cdp", commit), /no assertions/);
});

test("recomputes artifact hashes instead of trusting the evidence JSON", () => {
  const evidence = base("browser-cdp", BROWSER_CDP_SCENARIOS);
  evidence.cases = evidencedCases(BROWSER_CDP_SCENARIOS);
  evidence.cases[0].artifacts[0].sha256 = "e".repeat(64);
  assert.throws(() => validateEvidence(evidence, "browser-cdp", commit), /SHA-256 does not match/);
});

test("accepts the complete desktop catalog only with auditable artifacts", () => {
  const evidence = base("desktop-e2e-40", DESKTOP_E2E_SCENARIOS);
  evidence.cases = evidencedCases(DESKTOP_E2E_SCENARIOS);
  assert.doesNotThrow(() => validateEvidence(evidence, "desktop-e2e-40", commit));
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

test("desktop fault cases require hashed traces, not only pass labels", () => {
  const ids = [
    "engine-kill", "agent-kill", "checkpoint-before-crash", "checkpoint-after-crash",
    "provider-stream-break", "quota-suspension-resume", "oversized-ipc", "corrupt-ipc",
    "duplicate-ipc", "out-of-order-ipc", "cancel-safety", "sqlite-lock", "renderer-kill", "gpu-failure",
  ];
  const evidence = base("fault-injection", ids);
  assert.throws(() => validateEvidence(evidence, "fault-injection", commit), /renderer-kill has no assertions/);
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
