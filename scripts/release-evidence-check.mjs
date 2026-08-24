import fs from "node:fs";
import crypto from "node:crypto";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { BROWSER_CDP_SCENARIOS, DESKTOP_E2E_SCENARIOS } from "./release-scenarios.mjs";

const HEX_SHA256 = /^[a-f0-9]{64}$/i;
const GIT_COMMIT = /^[a-f0-9]{40}$/i;
const STATUS = "PASSED";

const REQUIRED_CASES = Object.freeze({
  "fault-injection-core": [
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
  ],
  "fault-injection": [
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
    "renderer-kill",
    "gpu-failure",
  ],
  "browser-cdp": BROWSER_CDP_SCENARIOS,
  "desktop-e2e-40": DESKTOP_E2E_SCENARIOS,
  "migration-install-upgrade-uninstall": [
    "clean-install",
    "legacy-upgrade",
    "legacy-data-adoption",
    "d-drive-migration",
    "sqlite-integrity",
    "parquet-manifest",
    "rollback",
    "uninstall-preserves-data",
  ],
  "minimax-plus-joinquant-live": [
    "minimax-provider-discovery",
    "minimax-20000-manual-plan",
    "minimax-stream-resume",
    "minimax-quota",
    "joinquant-auth",
    "joinquant-minimal-data",
  ],
});

const REQUIRED_PERFORMANCE_METRICS = Object.freeze({
  workspace_restore_p95_ms: { comparison: "<=", budget: 1500, aggregation: "p95", minSamples: 30, unit: "ms" },
  command_feedback_p95_ms: { comparison: "<=", budget: 100, aggregation: "p95", minSamples: 30, unit: "ms" },
  logical_rows_scroll_fps: { comparison: ">=", budget: 50, aggregation: "p05", minSamples: 10, unit: "fps" },
  agent_render_hz: { comparison: "<=", budget: 10, aggregation: "max", minSamples: 10, unit: "hz" },
  idle_cpu_p95_pct: { comparison: "<", budget: 2, aggregation: "p95", minSamples: 60, unit: "pct" },
  cold_start_regression_pct: { comparison: "<=", budget: 15, aggregation: "p95_regression", minSamples: 10, unit: "pct" },
  memory_regression_pct: { comparison: "<=", budget: 15, aggregation: "p95_regression", minSamples: 10, unit: "pct" },
});

function invariant(condition, message) {
  if (!condition) throw new Error(message);
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function validUtc(value) {
  return typeof value === "string" && value.endsWith("Z") && Number.isFinite(Date.parse(value));
}

function validateCases(evidence, gate) {
  invariant(Array.isArray(evidence.cases) && evidence.cases.length > 0, `${gate}: cases must be non-empty`);
  const ids = new Set();
  for (const [index, item] of evidence.cases.entries()) {
    invariant(isRecord(item), `${gate}: cases[${index}] must be an object`);
    invariant(typeof item.id === "string" && item.id.trim(), `${gate}: cases[${index}] has no id`);
    invariant(!ids.has(item.id), `${gate}: duplicate case id ${item.id}`);
    ids.add(item.id);
    invariant(item.status === STATUS, `${gate}: case ${item.id} is not PASSED`);
    invariant(Number.isFinite(item.duration_ms) && item.duration_ms >= 0, `${gate}: case ${item.id} has invalid duration_ms`);
    const requiresArtifacts = gate === "browser-cdp" || gate === "desktop-e2e-40" ||
      (gate === "fault-injection" && (item.id === "renderer-kill" || item.id === "gpu-failure"));
    if (requiresArtifacts) {
      invariant(Number.isInteger(item.assertion_count) && item.assertion_count > 0, `${gate}: case ${item.id} has no assertions`);
      invariant(Array.isArray(item.artifacts) && item.artifacts.length > 0, `${gate}: case ${item.id} has no immutable artifacts`);
      for (const artifact of item.artifacts) validateCaseArtifact(artifact, gate, item.id);
    }
  }
  for (const required of REQUIRED_CASES[gate] ?? []) {
    invariant(ids.has(required), `${gate}: required case is missing: ${required}`);
  }
  if (gate === "desktop-e2e-40") {
    invariant(evidence.cases.length >= 40, `${gate}: expected at least 40 passed desktop scenarios`);
  }
}

function validateCaseArtifact(artifact, gate, caseId) {
  invariant(isRecord(artifact), `${gate}: case ${caseId} has an invalid artifact`);
  invariant(typeof artifact.kind === "string" && artifact.kind.trim(), `${gate}: case ${caseId} artifact.kind is required`);
  invariant(typeof artifact.path === "string" && (path.win32.isAbsolute(artifact.path) || path.posix.isAbsolute(artifact.path)), `${gate}: case ${caseId} artifact.path must be absolute`);
  invariant(HEX_SHA256.test(artifact.sha256 ?? ""), `${gate}: case ${caseId} artifact has no SHA-256`);
  invariant(validUtc(artifact.captured_at_utc), `${gate}: case ${caseId} artifact timestamp is invalid`);
  invariant(fs.existsSync(artifact.path), `${gate}: case ${caseId} artifact does not exist`);
  const stat = fs.statSync(artifact.path);
  invariant(stat.isFile(), `${gate}: case ${caseId} artifact is not a file`);
  invariant(stat.size > 0 && stat.size <= 64 * 1024 * 1024, `${gate}: case ${caseId} artifact must be non-empty and at most 64 MiB`);
  const actualHash = crypto.createHash("sha256").update(fs.readFileSync(artifact.path)).digest("hex");
  invariant(actualHash.toLowerCase() === artifact.sha256.toLowerCase(), `${gate}: case ${caseId} artifact SHA-256 does not match`);
}

function validatePerformance(evidence) {
  invariant(isRecord(evidence.environment), "performance-budgets: measurement environment is required");
  invariant(evidence.environment.mode === "packaged-proton-cef", "performance-budgets: measurements must use the packaged Proton/CEF application");
  for (const field of ["cpu", "gpu", "power_profile"]) {
    invariant(typeof evidence.environment[field] === "string" && evidence.environment[field].trim(), `performance-budgets: environment.${field} is required`);
  }
  invariant(Number.isFinite(evidence.environment.memory_bytes) && evidence.environment.memory_bytes > 0, "performance-budgets: environment.memory_bytes is required");
  invariant(Number.isFinite(evidence.environment.display_scale_pct) && evidence.environment.display_scale_pct > 0, "performance-budgets: environment.display_scale_pct is required");
  invariant(HEX_SHA256.test(evidence.environment.proton_skeleton_sha256 ?? ""), "performance-budgets: Proton skeleton SHA-256 is required");
  invariant(HEX_SHA256.test(evidence.environment.application_package_sha256 ?? ""), "performance-budgets: application package SHA-256 is required");
  invariant(Array.isArray(evidence.metrics), "performance-budgets: metrics must be an array");
  const metrics = new Map(evidence.metrics.map((metric) => [metric.id, metric]));
  for (const [id, policy] of Object.entries(REQUIRED_PERFORMANCE_METRICS)) {
    const metric = metrics.get(id);
    invariant(isRecord(metric), `performance-budgets: required metric is missing: ${id}`);
    invariant(Number.isFinite(metric.value), `performance-budgets: ${id} has invalid value`);
    invariant(metric.comparison === policy.comparison, `performance-budgets: ${id} comparison must be ${policy.comparison}`);
    invariant(metric.budget === policy.budget, `performance-budgets: ${id} budget must be ${policy.budget}`);
    invariant(metric.aggregation === policy.aggregation, `performance-budgets: ${id} aggregation must be ${policy.aggregation}`);
    invariant(metric.unit === policy.unit, `performance-budgets: ${id} unit must be ${policy.unit}`);
    invariant(Array.isArray(metric.samples) && metric.samples.length >= policy.minSamples, `performance-budgets: ${id} requires at least ${policy.minSamples} samples`);
    invariant(metric.samples.every((sample) => Number.isFinite(sample) && sample >= 0), `performance-budgets: ${id} samples must be finite and non-negative`);
    let calculated;
    if (policy.aggregation === "p95_regression") {
      invariant(Array.isArray(metric.baseline_samples) && metric.baseline_samples.length >= policy.minSamples, `performance-budgets: ${id} requires at least ${policy.minSamples} baseline samples`);
      invariant(metric.baseline_samples.every((sample) => Number.isFinite(sample) && sample > 0), `performance-budgets: ${id} baseline samples must be finite and positive`);
      calculated = ((quantile(metric.samples, 0.95) / quantile(metric.baseline_samples, 0.95)) - 1) * 100;
    } else if (policy.aggregation === "p95") {
      calculated = quantile(metric.samples, 0.95);
    } else if (policy.aggregation === "p05") {
      calculated = quantile(metric.samples, 0.05);
    } else {
      calculated = Math.max(...metric.samples);
    }
    const tolerance = Math.max(0.01, Math.abs(calculated) * 0.001);
    invariant(Math.abs(metric.value - calculated) <= tolerance, `performance-budgets: ${id} value does not match its ${policy.aggregation} samples`);
    const passed = policy.comparison === ">="
      ? metric.value >= policy.budget
      : policy.comparison === "<"
        ? metric.value < policy.budget
        : metric.value <= policy.budget;
    invariant(passed && metric.status === STATUS, `performance-budgets: ${id} exceeds its release budget`);
  }
}

function quantile(values, probability) {
  const sorted = [...values].sort((left, right) => left - right);
  const index = Math.max(0, Math.min(sorted.length - 1, Math.ceil(probability * sorted.length) - 1));
  return sorted[index];
}

function validateCredentialRotation(evidence) {
  for (const field of [
    "minimax_rotated",
    "joinquant_rotated",
    "old_credentials_revoked",
    "credential_manager_readback_verified",
  ]) {
    invariant(evidence[field] === true, `credential-rotation: ${field} must be true`);
  }
  invariant(evidence.secrets_in_evidence === false, "credential-rotation: evidence must not contain secrets");
}

function validateSignedArtifacts(evidence) {
  invariant(Array.isArray(evidence.artifacts) && evidence.artifacts.length > 0, "authenticode-valid-all-pe: artifacts must be non-empty");
  const requiredKinds = new Set(["host", "engine", "agent", "cef-helper", "nsis"]);
  for (const artifact of evidence.artifacts) {
    invariant(isRecord(artifact), "authenticode-valid-all-pe: invalid artifact entry");
    invariant(typeof artifact.path === "string" && path.isAbsolute(artifact.path), "authenticode-valid-all-pe: artifact path must be absolute");
    invariant(artifact.authenticode_status === "Valid", `authenticode-valid-all-pe: ${artifact.path} is not Valid`);
    invariant(HEX_SHA256.test(artifact.sha256 ?? ""), `authenticode-valid-all-pe: ${artifact.path} has no SHA-256`);
    requiredKinds.delete(artifact.kind);
  }
  invariant(requiredKinds.size === 0, `authenticode-valid-all-pe: missing signed artifact kinds: ${[...requiredKinds].join(", ")}`);
}

export function validateEvidence(evidence, expectedGate, expectedCommit) {
  invariant(isRecord(evidence), "release evidence root must be an object");
  invariant(evidence.schema_version === 1, `${expectedGate}: schema_version must be 1`);
  invariant(evidence.gate === expectedGate, `expected gate ${expectedGate}, received ${evidence.gate}`);
  invariant(evidence.status === STATUS, `${expectedGate}: status must be PASSED`);
  invariant(GIT_COMMIT.test(evidence.commit ?? ""), `${expectedGate}: commit must be a full Git SHA`);
  invariant(evidence.commit === expectedCommit, `${expectedGate}: evidence commit does not match ${expectedCommit}`);
  invariant(validUtc(evidence.started_at_utc), `${expectedGate}: started_at_utc must be an ISO UTC timestamp`);
  invariant(validUtc(evidence.completed_at_utc), `${expectedGate}: completed_at_utc must be an ISO UTC timestamp`);
  invariant(Date.parse(evidence.completed_at_utc) >= Date.parse(evidence.started_at_utc), `${expectedGate}: completion precedes start`);
  invariant(isRecord(evidence.runner), `${expectedGate}: runner metadata is required`);
  invariant(typeof evidence.runner.os === "string" && evidence.runner.os.trim(), `${expectedGate}: runner.os is required`);
  invariant(typeof evidence.runner.arch === "string" && evidence.runner.arch.trim(), `${expectedGate}: runner.arch is required`);
  validateCases(evidence, expectedGate);

  if (expectedGate === "performance-budgets") validatePerformance(evidence);
  if (expectedGate === "minimax-plus-joinquant-live") {
    invariant(evidence.trusted_boundary === true, `${expectedGate}: trusted_boundary must be explicit`);
  }
  if (expectedGate === "credential-rotation") validateCredentialRotation(evidence);
  if (expectedGate === "authenticode-valid-all-pe") validateSignedArtifacts(evidence);
  return { gate: expectedGate, cases: evidence.cases.length, completed_at_utc: evidence.completed_at_utc };
}

function main(argv) {
  const [fileName, gate, commit] = argv;
  invariant(fileName && gate && commit, "usage: node scripts/release-evidence-check.mjs <file> <gate> <commit>");
  const file = path.resolve(fileName);
  const evidence = JSON.parse(fs.readFileSync(file, "utf8"));
  const result = validateEvidence(evidence, gate, commit);
  console.log(JSON.stringify({ ok: true, file, ...result }));
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}
