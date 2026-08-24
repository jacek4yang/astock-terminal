import fs from "node:fs";
import crypto from "node:crypto";
import path from "node:path";
import { pathToFileURL } from "node:url";
import {
  BROWSER_CDP_ASSERTION_ANCHORS,
  BROWSER_CDP_SCENARIOS,
  DESKTOP_E2E_ASSERTION_ANCHORS,
  DESKTOP_E2E_SCENARIOS,
  NATIVE_WINDOW_SCENARIOS,
} from "./release-scenarios.mjs";

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
  "desktop-window-native": NATIVE_WINDOW_SCENARIOS,
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
    const requiresArtifacts = gate === "browser-cdp" || gate === "desktop-window-native" || gate === "desktop-e2e-40" || gate === "performance-budgets" ||
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
  invariant(evidence.environment.pinned_proton_version === "0.2.1", "performance-budgets: Proton baseline must be pinned to 0.2.1");
  invariant(evidence.environment.cef_version === "147.0.14+g76d2442", "performance-budgets: CEF baseline version is not pinned");
  invariant(evidence.environment.chromium_version === "147.0.7727.138", "performance-budgets: Chromium baseline version is not pinned");
  for (const field of ["cpu", "gpu", "power_profile"]) {
    invariant(typeof evidence.environment[field] === "string" && evidence.environment[field].trim(), `performance-budgets: environment.${field} is required`);
  }
  invariant(Number.isFinite(evidence.environment.memory_bytes) && evidence.environment.memory_bytes > 0, "performance-budgets: environment.memory_bytes is required");
  invariant(Number.isFinite(evidence.environment.display_scale_pct) && evidence.environment.display_scale_pct > 0, "performance-budgets: environment.display_scale_pct is required");
  invariant(HEX_SHA256.test(evidence.environment.proton_skeleton_sha256 ?? ""), "performance-budgets: Proton skeleton SHA-256 is required");
  invariant(HEX_SHA256.test(evidence.environment.proton_skeleton_source_sha256 ?? ""), "performance-budgets: Proton skeleton source SHA-256 is required");
  invariant(HEX_SHA256.test(evidence.environment.application_package_sha256 ?? ""), "performance-budgets: application package SHA-256 is required");
  invariant(isRecord(evidence.measurement), "performance-budgets: measurement contract is required");
  invariant(evidence.measurement.packaged_application === true, "performance-budgets: packaged application measurement is required");
  invariant(evidence.measurement.browser_preview === false, "performance-budgets: browser preview cannot satisfy packaged performance evidence");
  invariant(evidence.measurement.release_test_fixture === true, "performance-budgets: audited release performance fixture is required");
  invariant(evidence.measurement.logical_rows === 100_000, "performance-budgets: the logical table fixture must contain exactly 100,000 rows");
  invariant(Number.isInteger(evidence.measurement.maximum_dom_rows) && evidence.measurement.maximum_dom_rows > 0 && evidence.measurement.maximum_dom_rows <= 200,
    "performance-budgets: virtualized DOM must contain between 1 and 200 rows");
  validateCaseArtifact(evidence.measurement.raw_samples, "performance-budgets", "raw-samples");
  const caseArtifacts = evidence.cases.flatMap((item) => item.artifacts ?? []);
  invariant(caseArtifacts.some((artifact) => artifact.path === evidence.measurement.raw_samples.path && artifact.sha256 === evidence.measurement.raw_samples.sha256),
    "performance-budgets: raw samples are not bound to the measurement case");
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
  invariant(typeof evidence.attestation === "string" && evidence.attestation.trim(), "credential-rotation: release-operator attestation is required");
  const cases = new Map(evidence.cases.map((item) => [item.id, item]));
  for (const id of ["minimax", "joinquant"]) {
    const details = cases.get(id)?.details;
    invariant(isRecord(details), `credential-rotation: ${id} details are required`);
    invariant(details.operator_confirmed_rotated === true, `credential-rotation: ${id} rotation was not confirmed`);
    invariant(details.credential_manager_readable === true, `credential-rotation: ${id} Credential Manager readback failed`);
  }
}

function validateExternalServices(evidence) {
  invariant(evidence.trusted_boundary === true, "minimax-plus-joinquant-live: trusted_boundary must be explicit");
  invariant(evidence.secrets_in_evidence === false, "minimax-plus-joinquant-live: evidence must not contain secrets");
  const cases = new Map(evidence.cases.map((item) => [item.id, item]));
  const details = (id) => {
    const value = cases.get(id)?.details;
    invariant(isRecord(value), `minimax-plus-joinquant-live: ${id} details are required`);
    return value;
  };

  const provider = details("minimax-provider-discovery");
  invariant(provider.catalog_verified === true, "minimax-provider-discovery: catalog was not verified");
  invariant(typeof provider.model === "string" && provider.model.trim(), "minimax-provider-discovery: model is required");
  invariant(Number.isInteger(provider.model_count) && provider.model_count > 0, "minimax-provider-discovery: model_count must be positive");
  invariant(["mainland", "international"].includes(provider.api_region), "minimax-provider-discovery: api_region is invalid");

  const plan = details("minimax-20000-manual-plan");
  invariant(plan.capital_cny === 20_000, "minimax-20000-manual-plan: exact capital constraint was not preserved");
  invariant(plan.phase === "completed", "minimax-20000-manual-plan: task did not complete");
  invariant(Number.isInteger(plan.model_rounds) && plan.model_rounds >= 4, "minimax-20000-manual-plan: multi-pass review is missing");
  invariant(Number.isInteger(plan.evidence_count) && plan.evidence_count > 0, "minimax-20000-manual-plan: evidence is missing");
  invariant(Number.isInteger(plan.report_chars) && plan.report_chars >= 800, "minimax-20000-manual-plan: report is too short");
  invariant(HEX_SHA256.test(plan.report_sha256 ?? ""), "minimax-20000-manual-plan: report SHA-256 is missing");
  invariant(plan.verifier_version === "engine-report-verifier-v1", "minimax-20000-manual-plan: independent verifier is missing");
  invariant(Number.isInteger(plan.numeric_claims_checked) && plan.numeric_claims_checked > 0, "minimax-20000-manual-plan: numeric claims were not checked");
  invariant(Number.isInteger(plan.distinct_citations) && plan.distinct_citations >= 8, "minimax-20000-manual-plan: primary-source citations are insufficient");
  invariant(plan.durable_phase === "completed" && Number.isInteger(plan.durable_accepted_seq) && plan.durable_accepted_seq > 0 &&
    plan.durable_accepted_seq === plan.worker_accepted_seq,
  "minimax-20000-manual-plan: durable task does not match the completed Worker state");
  invariant(plan.pending_effects === 0 && Number.isInteger(plan.succeeded_effects) && plan.succeeded_effects >= 2,
    "minimax-20000-manual-plan: durable Effect ledger is incomplete");
  invariant(plan.verifier_effect_status === "succeeded" && plan.verifier_effect_version === "engine-report-verifier-v1",
    "minimax-20000-manual-plan: durable verifier Effect is missing");

  const stream = details("minimax-stream-resume");
  invariant(stream.implementation === "moonbit-agent-worker", "minimax-stream-resume: wrong implementation boundary");
  invariant(stream.transport === "sse" && stream.real_stream_completed === true, "minimax-stream-resume: real SSE completion is missing");
  invariant(stream.incomplete_stream_rejected === true && stream.partial_output_discarded === true && stream.complete_response_retry_tested === true,
    "minimax-stream-resume: safe retry assertions are incomplete");

  const quota = details("minimax-quota");
  invariant(Number.isInteger(quota.model_count) && quota.model_count > 0, "minimax-quota: no model quota was returned");
  invariant(Number.isFinite(quota.fetched_at_ms) && quota.fetched_at_ms > 0, "minimax-quota: fetch timestamp is missing");
  const evidenceStartedAt = Date.parse(evidence.started_at_utc);
  const evidenceCompletedAt = Date.parse(evidence.completed_at_utc);
  invariant(quota.fetched_at_ms >= evidenceStartedAt - 60_000 && quota.fetched_at_ms <= evidenceCompletedAt + 60_000,
    "minimax-quota: snapshot is not bound to this live run");

  const auth = details("joinquant-auth");
  invariant(auth.configured === true, "joinquant-auth: Credential Manager authentication was not confirmed");
  const data = details("joinquant-minimal-data");
  invariant(data.dataset === "qfq_daily", "joinquant-minimal-data: qfq_daily was not tested");
  invariant(Number.isInteger(data.row_count) && data.row_count > 0, "joinquant-minimal-data: no rows were returned");
  invariant(Number.isInteger(data.total_rows) && data.total_rows >= data.row_count, "joinquant-minimal-data: total_rows is invalid");
  invariant(data.source === "JoinQuant", "joinquant-minimal-data: source identity is invalid");
  invariant(typeof data.fetched_at === "string" && Number.isFinite(Date.parse(data.fetched_at)), "joinquant-minimal-data: fetched_at is invalid");
  invariant(Date.parse(data.fetched_at) >= evidenceStartedAt - 60_000 && Date.parse(data.fetched_at) <= evidenceCompletedAt + 60_000,
    "joinquant-minimal-data: fetch timestamp is not bound to this live run");
  invariant(data.symbol === "000725", "joinquant-minimal-data: audited security identity is invalid");
  for (const field of ["requested_start", "requested_end", "first_date", "latest_date"]) {
    invariant(typeof data[field] === "string" && /^\d{4}-\d{2}-\d{2}$/.test(data[field]) &&
      Number.isFinite(Date.parse(`${data[field]}T00:00:00Z`)), `joinquant-minimal-data: ${field} is invalid`);
  }
  invariant(data.requested_start <= data.first_date && data.first_date <= data.latest_date && data.latest_date <= data.requested_end,
    "joinquant-minimal-data: returned dates escape the requested window or are unordered");
  invariant(Number.isInteger(data.latest_lag_days) && data.latest_lag_days >= 0 && data.latest_lag_days <= 14,
    "joinquant-minimal-data: latest qfq bar is stale");
  invariant(data.structural_rows_checked === data.row_count && data.volume_unit === "Lots" && data.truncated === false,
    "joinquant-minimal-data: row structure/unit/pagination audit is incomplete");
  invariant(HEX_SHA256.test(data.data_sha256 ?? ""), "joinquant-minimal-data: audited row digest is missing");
}

function validateSignedArtifacts(evidence) {
  invariant(Array.isArray(evidence.artifacts) && evidence.artifacts.length > 0, "authenticode-valid-all-pe: artifacts must be non-empty");
  const allowedKinds = new Set(["host", "engine", "agent", "cef-helper", "nsis"]);
  const requiredKinds = new Set(allowedKinds);
  const seenKinds = new Set();
  const requiredPaths = new Set();
  for (const artifact of evidence.artifacts) {
    invariant(isRecord(artifact), "authenticode-valid-all-pe: invalid artifact entry");
    invariant(typeof artifact.path === "string" && path.isAbsolute(artifact.path), "authenticode-valid-all-pe: artifact path must be absolute");
    invariant(allowedKinds.has(artifact.kind), `authenticode-valid-all-pe: unexpected artifact kind ${artifact.kind}`);
    invariant(!seenKinds.has(artifact.kind), `authenticode-valid-all-pe: duplicate artifact kind ${artifact.kind}`);
    seenKinds.add(artifact.kind);
    invariant(artifact.authenticode_status === "Valid", `authenticode-valid-all-pe: ${artifact.path} is not Valid`);
    invariant(HEX_SHA256.test(artifact.sha256 ?? ""), `authenticode-valid-all-pe: ${artifact.path} has no SHA-256`);
    invariant(fs.existsSync(artifact.path), `authenticode-valid-all-pe: ${artifact.path} does not exist`);
    const stat = fs.statSync(artifact.path);
    invariant(stat.isFile() && stat.size > 0, `authenticode-valid-all-pe: ${artifact.path} must be a non-empty file`);
    const actualHash = crypto.createHash("sha256").update(fs.readFileSync(artifact.path)).digest("hex");
    invariant(actualHash.toLowerCase() === artifact.sha256.toLowerCase(), `authenticode-valid-all-pe: ${artifact.path} SHA-256 does not match`);
    const normalizedPath = path.resolve(artifact.path).toLowerCase();
    invariant(!requiredPaths.has(normalizedPath), `authenticode-valid-all-pe: duplicate required artifact path ${artifact.path}`);
    requiredPaths.add(normalizedPath);
    requiredKinds.delete(artifact.kind);
  }
  invariant(requiredKinds.size === 0, `authenticode-valid-all-pe: missing signed artifact kinds: ${[...requiredKinds].join(", ")}`);
  invariant(evidence.inventory_scope === "packaged-app-pe-plus-installer", "authenticode-valid-all-pe: PE inventory scope is invalid");
  invariant(Number.isInteger(evidence.packaged_pe_count) && evidence.packaged_pe_count >= 4,
    "authenticode-valid-all-pe: packaged_pe_count must include the application PE set");
  invariant(Array.isArray(evidence.pe_inventory) && evidence.pe_inventory.length === evidence.packaged_pe_count + 1,
    "authenticode-valid-all-pe: PE inventory must contain every packaged PE plus the installer");
  const inventoryPaths = new Set();
  for (const artifact of evidence.pe_inventory) {
    invariant(isRecord(artifact), "authenticode-valid-all-pe: invalid PE inventory entry");
    invariant(typeof artifact.path === "string" && path.isAbsolute(artifact.path), "authenticode-valid-all-pe: PE inventory path must be absolute");
    const normalizedPath = path.resolve(artifact.path).toLowerCase();
    invariant(!inventoryPaths.has(normalizedPath), `authenticode-valid-all-pe: duplicate PE inventory path ${artifact.path}`);
    inventoryPaths.add(normalizedPath);
    invariant(artifact.authenticode_status === "Valid", `authenticode-valid-all-pe: ${artifact.path} inventory status is not Valid`);
    invariant(HEX_SHA256.test(artifact.sha256 ?? ""), `authenticode-valid-all-pe: ${artifact.path} inventory entry has no SHA-256`);
    invariant(fs.existsSync(artifact.path), `authenticode-valid-all-pe: ${artifact.path} inventory entry does not exist`);
    const stat = fs.statSync(artifact.path);
    invariant(stat.isFile() && stat.size > 0, `authenticode-valid-all-pe: ${artifact.path} inventory entry must be a non-empty file`);
    const actualHash = crypto.createHash("sha256").update(fs.readFileSync(artifact.path)).digest("hex");
    invariant(actualHash.toLowerCase() === artifact.sha256.toLowerCase(), `authenticode-valid-all-pe: ${artifact.path} inventory SHA-256 does not match`);
  }
  for (const requiredPath of requiredPaths) {
    invariant(inventoryPaths.has(requiredPath), `authenticode-valid-all-pe: required artifact is absent from PE inventory: ${requiredPath}`);
  }
}

function validateInteractiveAcceptance(evidence, gate) {
  const browser = gate === "browser-cdp";
  const expectedSurface = browser ? "codex-in-app-browser" : "packaged-proton-cef";
  const assertionCatalog = browser ? BROWSER_CDP_ASSERTION_ANCHORS : DESKTOP_E2E_ASSERTION_ANCHORS;
  invariant(evidence.secrets_in_evidence === false, `${gate}: evidence may not retain credentials or Bridge tokens`);
  invariant(evidence.production_data_touched === false, `${gate}: acceptance may not touch production data`);
  invariant(evidence.runner.surface === expectedSurface && typeof evidence.runner.session_id === "string" && evidence.runner.session_id.trim(),
    `${gate}: acceptance runner surface/session is invalid`);
  for (const item of evidence.cases) {
    const details = item.details;
    invariant(isRecord(details) && details.recording_schema === 1 && details.surface === expectedSurface,
      `${gate}: case ${item.id} has no detailed interactive recording`);
    invariant(Array.isArray(details.assertions) && details.assertions.length >= 2 &&
      details.assertions.every((assertion) => isRecord(assertion) && assertion.passed === true &&
        typeof assertion.id === "string" && assertion.id.trim() &&
        Object.hasOwn(assertion, "expected") && assertion.expected !== null && assertion.expected !== undefined &&
        Object.hasOwn(assertion, "observed") && assertion.observed !== null && assertion.observed !== undefined),
    `${gate}: case ${item.id} contains only a pass label or incomplete assertions`);
    const requiredAnchors = assertionCatalog[item.id];
    invariant(Array.isArray(requiredAnchors), `${gate}: unapproved interactive scenario ${item.id}`);
    const assertionIds = new Set(details.assertions.map((assertion) => assertion.id));
    invariant(assertionIds.size === details.assertions.length, `${gate}: case ${item.id} contains duplicate assertion ids`);
    for (const anchor of requiredAnchors) {
      invariant(assertionIds.has(anchor), `${gate}: case ${item.id} is missing required assertion anchor ${anchor}`);
    }
    invariant(isRecord(details.viewport) && Number.isInteger(details.viewport.width) && Number.isInteger(details.viewport.height),
      `${gate}: case ${item.id} has no viewport recording`);
    invariant(details.console?.error_count === 0 && details.console?.warning_count === 0,
      `${gate}: case ${item.id} contains console errors or warnings`);
    const artifactKinds = new Set(item.artifacts.map((artifact) => artifact.kind));
    invariant(artifactKinds.has("interaction-trace") && artifactKinds.has("screenshot"),
      `${gate}: case ${item.id} requires an interaction trace and screenshot`);
    if (browser) {
      invariant(details.bridge?.real_engine === true && details.bridge?.real_agent === true,
        `${gate}: case ${item.id} did not use the real Engine and Agent Workers`);
      if (item.id === "responsive-1200") invariant(details.viewport.width === 1199, `${gate}: responsive-1200 must exercise 1199px`);
      if (item.id === "responsive-900") invariant(details.viewport.width === 899, `${gate}: responsive-900 must exercise 899px`);
    } else {
      invariant(details.package?.application_version === "6.0.0" && details.package?.commit === evidence.commit &&
        details.package?.isolated_data_root === true, `${gate}: case ${item.id} is not bound to the isolated packaged application`);
    }
  }
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
  if (expectedGate === "minimax-plus-joinquant-live") validateExternalServices(evidence);
  if (expectedGate === "credential-rotation") validateCredentialRotation(evidence);
  if (expectedGate === "authenticode-valid-all-pe") validateSignedArtifacts(evidence);
  if (expectedGate === "browser-cdp" || expectedGate === "desktop-e2e-40") validateInteractiveAcceptance(evidence, expectedGate);
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
