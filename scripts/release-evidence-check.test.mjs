import test, { after } from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { validateEvidence } from "./release-evidence-check.mjs";
import {
  BROWSER_CDP_ASSERTION_ANCHORS,
  BROWSER_CDP_SCENARIOS,
  DESKTOP_E2E_ASSERTION_ANCHORS,
  DESKTOP_E2E_SCENARIOS,
  NATIVE_WINDOW_SCENARIOS,
} from "./release-scenarios.mjs";

const commit = "a".repeat(40);
const artifactRoot = fs.mkdtempSync(path.join(os.tmpdir(), "astock-release-evidence-"));
const artifactPath = path.join(artifactRoot, "trace.json");
fs.writeFileSync(artifactPath, '{"ok":true}\n', "utf8");
const artifactHash = crypto.createHash("sha256").update(fs.readFileSync(artifactPath)).digest("hex");
after(() => {
  fs.unlinkSync(artifactPath);
  for (let index = 0; index < 4; index += 1) {
    fs.unlinkSync(path.join(artifactRoot, `pe-${index}.exe`));
  }
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

function interactiveCases(caseIds, surface) {
  return caseIds.map((id) => ({
    id,
    status: "PASSED",
    duration_ms: 1,
    assertion_count: 2,
    artifacts: [
      { kind: "interaction-trace", path: artifactPath, sha256: artifactHash, captured_at_utc: "2026-08-24T00:00:30.000Z" },
      { kind: "screenshot", path: artifactPath, sha256: artifactHash, captured_at_utc: "2026-08-24T00:00:30.000Z" },
    ],
    details: {
      recording_schema: 1,
      surface,
      viewport: { width: id === "responsive-1200" ? 1199 : id === "responsive-900" ? 899 : 1440, height: 900 },
      console: { error_count: 0, warning_count: 0 },
      assertions: (surface === "codex-in-app-browser"
        ? BROWSER_CDP_ASSERTION_ANCHORS[id]
        : DESKTOP_E2E_ASSERTION_ANCHORS[id]).map((anchor) => ({
        id: anchor, passed: true, expected: true, observed: true,
      })),
      ...(surface === "codex-in-app-browser"
        ? { bridge: { real_engine: true, real_agent: true } }
        : { package: { application_version: "6.0.0", commit, isolated_data_root: true } }),
    },
  }));
}

function completePerformance() {
  const metric = (id, value, comparison, budget, aggregation, unit, count) => ({
    id, value, comparison, budget, aggregation, unit, status: "PASSED", samples: Array(count).fill(value),
  });
  return {
    ...base("performance-budgets", ["packaged-proton-cef-measurement"]),
    cases: evidencedCases(["packaged-proton-cef-measurement"]),
    environment: {
      mode: "packaged-proton-cef",
      pinned_proton_version: "0.2.1",
      cef_version: "147.0.14+g76d2442",
      chromium_version: "147.0.7727.138",
      cpu: "test cpu",
      gpu: "test gpu",
      power_profile: "balanced",
      memory_bytes: 16 * 1024 ** 3,
      display_scale_pct: 100,
      proton_skeleton_sha256: "b".repeat(64),
      proton_skeleton_source_sha256: "d".repeat(64),
      application_package_sha256: "c".repeat(64),
    },
    measurement: {
      packaged_application: true,
      browser_preview: false,
      release_test_fixture: true,
      logical_rows: 100_000,
      maximum_dom_rows: 52,
      raw_samples: {
        kind: "packaged-performance-raw-samples",
        path: artifactPath,
        sha256: artifactHash,
        captured_at_utc: "2026-08-24T00:00:30.000Z",
      },
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

function completeExternalServices() {
  const evidence = base("minimax-plus-joinquant-live", [
    "minimax-provider-discovery",
    "minimax-20000-manual-plan",
    "minimax-stream-resume",
    "minimax-quota",
    "joinquant-auth",
    "joinquant-minimal-data",
  ]);
  const byId = new Map(evidence.cases.map((item) => [item.id, item]));
  byId.get("minimax-provider-discovery").details = {
    catalog_verified: true, model: "MiniMax-M3", model_count: 2, api_region: "mainland",
  };
  byId.get("minimax-20000-manual-plan").details = {
    capital_cny: 20_000,
    phase: "completed",
    model_rounds: 4,
    evidence_count: 12,
    report_chars: 1200,
    report_sha256: "d".repeat(64),
    verifier_version: "engine-report-verifier-v1",
    numeric_claims_checked: 8,
    distinct_citations: 8,
    durable_phase: "completed",
    durable_accepted_seq: 42,
    worker_accepted_seq: 42,
    pending_effects: 0,
    succeeded_effects: 12,
    verifier_effect_status: "succeeded",
    verifier_effect_version: "engine-report-verifier-v1",
  };
  byId.get("minimax-stream-resume").details = {
    implementation: "moonbit-agent-worker",
    transport: "sse",
    real_stream_completed: true,
    incomplete_stream_rejected: true,
    partial_output_discarded: true,
    complete_response_retry_tested: true,
  };
  byId.get("minimax-quota").details = { model_count: 2, fetched_at_ms: Date.parse("2026-08-24T00:00:30Z") };
  byId.get("joinquant-auth").details = { configured: true };
  byId.get("joinquant-minimal-data").details = {
    dataset: "qfq_daily", row_count: 20, total_rows: 20, source: "JoinQuant", fetched_at: "2026-08-24T00:00:30Z",
    symbol: "000725", requested_start: "2026-04-26", requested_end: "2026-08-24",
    first_date: "2026-04-27", latest_date: "2026-08-21", latest_lag_days: 3,
    structural_rows_checked: 20, volume_unit: "Lots", truncated: false, data_sha256: "e".repeat(64),
  };
  return { ...evidence, trusted_boundary: true, secrets_in_evidence: false };
}

function completeSignedArtifacts() {
  const roles = ["host", "engine", "agent", "cef-helper", "nsis"];
  const paths = [artifactPath, ...Array.from({ length: 4 }, (_, index) => path.join(artifactRoot, `pe-${index}.exe`))];
  const artifacts = roles.map((kind, index) => ({
    kind,
    path: paths[index],
    authenticode_status: "Valid",
    sha256: artifactHash,
  }));
  return {
    ...base("authenticode-valid-all-pe", ["packaged-pe-verification"]),
    inventory_scope: "packaged-app-pe-plus-installer",
    packaged_pe_count: 4,
    artifacts,
    pe_inventory: paths.map((artifact) => ({
      path: artifact,
      authenticode_status: "Valid",
      sha256: artifactHash,
    })),
  };
}

for (let index = 0; index < 4; index += 1) {
  fs.copyFileSync(artifactPath, path.join(artifactRoot, `pe-${index}.exe`));
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
  evidence.secrets_in_evidence = false;
  evidence.production_data_touched = false;
  evidence.runner.surface = "packaged-proton-cef";
  evidence.runner.session_id = "desktop-session";
  evidence.cases = interactiveCases(DESKTOP_E2E_SCENARIOS, "packaged-proton-cef");
  assert.doesNotThrow(() => validateEvidence(evidence, "desktop-e2e-40", commit));
});

test("rejects complete interactive catalogs whose assertions prove the wrong behavior", () => {
  const evidence = base("browser-cdp", BROWSER_CDP_SCENARIOS);
  evidence.secrets_in_evidence = false;
  evidence.production_data_touched = false;
  evidence.runner.surface = "codex-in-app-browser";
  evidence.runner.session_id = "browser-session";
  evidence.cases = interactiveCases(BROWSER_CDP_SCENARIOS, "codex-in-app-browser");
  evidence.cases.find((item) => item.id === "stock-detail").details.assertions = [
    { id: "market-index-values-visible", passed: true, expected: true, observed: true },
    { id: "market-quality-state-visible", passed: true, expected: true, observed: true },
  ];
  assert.throws(() => validateEvidence(evidence, "browser-cdp", commit), /canonical-security-identity/);
});

test("rejects duplicate interactive assertion ids", () => {
  const evidence = base("browser-cdp", BROWSER_CDP_SCENARIOS);
  evidence.secrets_in_evidence = false;
  evidence.production_data_touched = false;
  evidence.runner.surface = "codex-in-app-browser";
  evidence.runner.session_id = "browser-session";
  evidence.cases = interactiveCases(BROWSER_CDP_SCENARIOS, "codex-in-app-browser");
  const assertions = evidence.cases[0].details.assertions;
  assertions[1].id = assertions[0].id;
  assert.throws(() => validateEvidence(evidence, "browser-cdp", commit), /duplicate assertion ids/);
});

test("rejects pass-only browser evidence without real Worker and visual provenance", () => {
  const evidence = base("browser-cdp", BROWSER_CDP_SCENARIOS);
  evidence.cases = evidencedCases(BROWSER_CDP_SCENARIOS);
  evidence.secrets_in_evidence = false;
  evidence.production_data_touched = false;
  evidence.runner.surface = "codex-in-app-browser";
  evidence.runner.session_id = "browser-session";
  assert.throws(() => validateEvidence(evidence, "browser-cdp", commit), /detailed interactive recording/);
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

test("rejects browser-only or unbounded performance fixtures", () => {
  const evidence = completePerformance();
  evidence.measurement.browser_preview = true;
  evidence.measurement.maximum_dom_rows = 100_000;
  assert.throws(() => validateEvidence(evidence, "performance-budgets", commit), /browser preview/);
});

test("requires performance raw samples to be hashed and bound to its case", () => {
  const evidence = completePerformance();
  evidence.measurement.raw_samples.sha256 = "f".repeat(64);
  assert.throws(() => validateEvidence(evidence, "performance-budgets", commit), /SHA-256 does not match/);
});

test("accepts complete credential-rotation evidence without embedded secrets", () => {
  const evidence = {
    ...base("credential-rotation", ["minimax", "joinquant"]),
    minimax_rotated: true,
    joinquant_rotated: true,
    old_credentials_revoked: true,
    credential_manager_readback_verified: true,
    secrets_in_evidence: false,
    attestation: "operator confirmed rotation and revocation",
  };
  for (const item of evidence.cases) {
    item.details = { operator_confirmed_rotated: true, credential_manager_readable: true };
  }
  assert.equal(validateEvidence(evidence, "credential-rotation", commit).cases, 2);
});

test("accepts detailed secret-free MiniMax Plus and JoinQuant live evidence", () => {
  assert.equal(validateEvidence(completeExternalServices(), "minimax-plus-joinquant-live", commit).cases, 6);
});

test("rejects a pass-only label for MiniMax stream recovery", () => {
  const evidence = completeExternalServices();
  delete evidence.cases.find((item) => item.id === "minimax-stream-resume").details.partial_output_discarded;
  assert.throws(() => validateEvidence(evidence, "minimax-plus-joinquant-live", commit), /safe retry assertions/);
});

test("rejects live evidence that does not preserve the exact 20,000 CNY constraint", () => {
  const evidence = completeExternalServices();
  evidence.cases.find((item) => item.id === "minimax-20000-manual-plan").details.capital_cny = 10_000;
  assert.throws(() => validateEvidence(evidence, "minimax-plus-joinquant-live", commit), /capital constraint/);
});

test("rejects insufficient primary-source citations or stale live snapshots", () => {
  const evidence = completeExternalServices();
  const plan = evidence.cases.find((item) => item.id === "minimax-20000-manual-plan").details;
  plan.distinct_citations = 7;
  assert.throws(() => validateEvidence(evidence, "minimax-plus-joinquant-live", commit), /primary-source citations are insufficient/);
  plan.distinct_citations = 8;
  const quota = evidence.cases.find((item) => item.id === "minimax-quota").details;
  quota.fetched_at_ms = Date.parse("2025-01-01T00:00:00Z");
  assert.throws(() => validateEvidence(evidence, "minimax-plus-joinquant-live", commit), /snapshot is not bound to this live run/);
});

test("rejects a completed MiniMax response without a reconciled durable Effect ledger", () => {
  const evidence = completeExternalServices();
  const details = evidence.cases.find((item) => item.id === "minimax-20000-manual-plan").details;
  details.pending_effects = 1;
  assert.throws(() => validateEvidence(evidence, "minimax-plus-joinquant-live", commit), /durable Effect ledger is incomplete/);
  details.pending_effects = 0;
  details.durable_accepted_seq += 1;
  assert.throws(() => validateEvidence(evidence, "minimax-plus-joinquant-live", commit), /durable task does not match/);
});

test("rejects stale or structurally unaudited JoinQuant live data", () => {
  const evidence = completeExternalServices();
  const details = evidence.cases.find((item) => item.id === "joinquant-minimal-data").details;
  details.latest_lag_days = 45;
  assert.throws(() => validateEvidence(evidence, "minimax-plus-joinquant-live", commit), /latest qfq bar is stale/);
  details.latest_lag_days = 3;
  details.structural_rows_checked = 0;
  assert.throws(() => validateEvidence(evidence, "minimax-plus-joinquant-live", commit), /row structure\/unit\/pagination audit is incomplete/);
});

test("recomputes every signed release artifact hash", () => {
  const evidence = completeSignedArtifacts();
  assert.doesNotThrow(() => validateEvidence(evidence, "authenticode-valid-all-pe", commit));
  evidence.artifacts[2].sha256 = "0".repeat(64);
  assert.throws(() => validateEvidence(evidence, "authenticode-valid-all-pe", commit), /SHA-256 does not match/);
});

test("rejects duplicate signed artifact labels", () => {
  const evidence = completeSignedArtifacts();
  evidence.artifacts[4].kind = "host";
  assert.throws(() => validateEvidence(evidence, "authenticode-valid-all-pe", commit), /duplicate artifact kind/);
});

test("rejects an incomplete or forged packaged PE inventory", () => {
  const evidence = completeSignedArtifacts();
  evidence.pe_inventory.pop();
  assert.throws(() => validateEvidence(evidence, "authenticode-valid-all-pe", commit), /every packaged PE plus the installer/);
});
