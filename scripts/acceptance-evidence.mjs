import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import zlib from "node:zlib";

import { BROWSER_CDP_SCENARIOS, DESKTOP_E2E_SCENARIOS } from "./release-scenarios.mjs";

const MODES = Object.freeze({
  browser: { gate: "browser-cdp", surface: "codex-in-app-browser", scenarios: BROWSER_CDP_SCENARIOS },
  desktop: { gate: "desktop-e2e-40", surface: "packaged-proton-cef", scenarios: DESKTOP_E2E_SCENARIOS },
});
const COMMIT = /^[a-f0-9]{40}$/i;
const PNG_SIGNATURE = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
const CRC32_TABLE = Array.from({ length: 256 }, (_, value) => {
  let current = value;
  for (let bit = 0; bit < 8; bit += 1) current = (current & 1) ? (0xedb88320 ^ (current >>> 1)) : (current >>> 1);
  return current >>> 0;
});
const SECRET_PATTERNS = [
  /bridgeToken/i,
  /x-astock-test-token/i,
  /sk-(?:cp-)?[A-Za-z0-9_-]{16,}/,
];

function invariant(condition, message) {
  if (!condition) throw new Error(message);
}

function absolute(value, label) {
  invariant(typeof value === "string" && path.isAbsolute(value), `${label} must be an absolute path`);
  return path.resolve(value);
}

function inside(root, candidate, label) {
  const normalizedRoot = path.resolve(root);
  const normalizedCandidate = path.resolve(candidate);
  const relative = path.relative(normalizedRoot, normalizedCandidate);
  invariant(relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative), `${label} escapes ${normalizedRoot}`);
  return normalizedCandidate;
}

function defaultBuildRoot() {
  if (process.env.ASTOCK_BUILD_ROOT) return path.resolve(process.env.ASTOCK_BUILD_ROOT);
  if (process.platform === "win32") return "D:\\astock-build\\astock-terminal";
  throw new Error("ASTOCK_BUILD_ROOT is required outside Windows");
}

function readJson(file, label) {
  invariant(fs.existsSync(file) && fs.statSync(file).isFile(), `${label} is missing: ${file}`);
  try {
    return { value: JSON.parse(fs.readFileSync(file, "utf8")), raw: fs.readFileSync(file, "utf8") };
  } catch (error) {
    throw new Error(`${label} is not valid JSON: ${error instanceof Error ? error.message : String(error)}`);
  }
}

function sha256(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function crc32(body) {
  let value = 0xffffffff;
  for (const byte of body) value = CRC32_TABLE[(value ^ byte) & 0xff] ^ (value >>> 8);
  return (value ^ 0xffffffff) >>> 0;
}

function pngDimensions(file) {
  const body = fs.readFileSync(file);
  invariant(body.length >= 128, `screenshot is implausibly small: ${file}`);
  invariant(body.subarray(0, PNG_SIGNATURE.length).equals(PNG_SIGNATURE), `screenshot is not a PNG: ${file}`);
  let offset = PNG_SIGNATURE.length;
  let header;
  const imageData = [];
  let sawEnd = false;
  while (offset + 12 <= body.length) {
    const length = body.readUInt32BE(offset);
    const typeStart = offset + 4;
    const dataStart = typeStart + 4;
    const crcOffset = dataStart + length;
    const next = crcOffset + 4;
    invariant(next <= body.length, `screenshot contains a truncated PNG chunk: ${file}`);
    const type = body.subarray(typeStart, dataStart).toString("ascii");
    const actualCrc = crc32(body.subarray(typeStart, crcOffset));
    invariant(actualCrc === body.readUInt32BE(crcOffset), `screenshot PNG checksum failed for ${type}: ${file}`);
    const data = body.subarray(dataStart, crcOffset);
    if (type === "IHDR") {
      invariant(!header && offset === PNG_SIGNATURE.length && length === 13, `screenshot PNG header is invalid: ${file}`);
      header = Buffer.from(data);
    } else if (type === "IDAT") {
      imageData.push(Buffer.from(data));
    } else if (type === "IEND") {
      invariant(length === 0, `screenshot PNG end marker is invalid: ${file}`);
      sawEnd = true;
      offset = next;
      break;
    }
    offset = next;
  }
  invariant(header && imageData.length > 0 && sawEnd && offset === body.length, `screenshot PNG structure is incomplete: ${file}`);
  const width = header.readUInt32BE(0);
  const height = header.readUInt32BE(4);
  invariant(width >= 320 && height >= 240, `screenshot dimensions are too small: ${width}x${height}`);
  const bitDepth = header[8];
  const colorType = header[9];
  const interlace = header[12];
  const channels = new Map([[0, 1], [2, 3], [4, 2], [6, 4]]).get(colorType);
  invariant(bitDepth === 8 && channels && interlace === 0, `screenshot PNG encoding is unsupported: ${file}`);
  const expectedBytes = (width * channels + 1) * height;
  invariant(expectedBytes <= 128 * 1024 * 1024, `screenshot expands beyond the 128 MiB safety limit: ${file}`);
  let pixels;
  try {
    pixels = zlib.inflateSync(Buffer.concat(imageData), { maxOutputLength: expectedBytes });
  } catch (error) {
    throw new Error(`screenshot PNG pixel data is corrupt: ${file}: ${error instanceof Error ? error.message : String(error)}`);
  }
  invariant(pixels.length === expectedBytes, `screenshot PNG pixel length is invalid: ${file}`);
  const rowBytes = width * channels + 1;
  for (let row = 0; row < height; row += 1) {
    invariant(pixels[row * rowBytes] <= 4, `screenshot PNG row filter is invalid: ${file}`);
  }
  return { width, height };
}

function safeObservation(raw, scenario) {
  for (const pattern of SECRET_PATTERNS) invariant(!pattern.test(raw), `${scenario}: observation contains credential or Bridge-token material`);
}

function assertNoSecretFields(value, scenario, parent = "root") {
  if (!value || typeof value !== "object") return;
  if (Array.isArray(value)) {
    value.forEach((item, index) => assertNoSecretFields(item, scenario, `${parent}[${index}]`));
    return;
  }
  for (const [key, item] of Object.entries(value)) {
    const normalized = key.toLowerCase().replaceAll("-", "_");
    const sensitive = ["password", "api_key", "token", "bridge_token", "credentials", "secret"].includes(normalized) ||
      /_(?:password|api_key|token|secret)$/.test(normalized);
    if (sensitive && typeof item === "string") {
      invariant(["redacted", "configured", "not-configured", "absent"].includes(item),
        `${scenario}: observation contains a string value in sensitive field ${parent}.${key}`);
    }
    assertNoSecretFields(item, scenario, `${parent}.${key}`);
  }
}

function normalizedUrl(value, mode, scenario) {
  invariant(typeof value === "string" && value.length > 0, `${scenario}: app_url is required`);
  const url = new URL(value);
  invariant(!url.username && !url.password && !url.search && !url.hash, `${scenario}: app_url must be sanitized and contain no credentials, query or fragment`);
  if (mode === "browser") {
    invariant(url.protocol === "http:" && url.hostname === "127.0.0.1" && url.port === "5173" && url.pathname === "/",
      `${scenario}: browser evidence must target the audited 127.0.0.1:5173 workbench`);
  }
  return url.toString();
}

function validateAssertions(assertions, scenario) {
  invariant(Array.isArray(assertions) && assertions.length >= 2, `${scenario}: at least two concrete assertions are required`);
  const ids = new Set();
  for (const assertion of assertions) {
    invariant(assertion && typeof assertion === "object" && !Array.isArray(assertion), `${scenario}: invalid assertion record`);
    invariant(typeof assertion.id === "string" && assertion.id.trim(), `${scenario}: assertion id is required`);
    invariant(!ids.has(assertion.id), `${scenario}: duplicate assertion ${assertion.id}`);
    ids.add(assertion.id);
    invariant(assertion.passed === true, `${scenario}: assertion ${assertion.id} did not pass`);
    invariant(Object.hasOwn(assertion, "expected") && Object.hasOwn(assertion, "observed"),
      `${scenario}: assertion ${assertion.id} must retain expected and observed values`);
  }
  return assertions;
}

function validateObservation({ mode, scenario, caseDirectory, commit, surface }) {
  const observationPath = inside(caseDirectory, path.join(caseDirectory, "observation.json"), `${scenario} observation`);
  const { value: observation, raw } = readJson(observationPath, `${scenario} observation`);
  safeObservation(raw, scenario);
  assertNoSecretFields(observation, scenario);
  invariant(observation.schema_version === 1 && observation.scenario === scenario, `${scenario}: observation identity is invalid`);
  invariant(observation.status === "PASSED" && observation.commit === commit, `${scenario}: observation is not PASSED for the exact commit`);
  invariant(observation.surface === surface, `${scenario}: wrong acceptance surface`);
  invariant(observation.production_data_touched === false, `${scenario}: acceptance must use an isolated data root`);
  const started = Date.parse(observation.started_at_utc);
  const completed = Date.parse(observation.completed_at_utc);
  invariant(Number.isFinite(started) && Number.isFinite(completed) && completed >= started, `${scenario}: observation timestamps are invalid`);
  const assertions = validateAssertions(observation.assertions, scenario);
  invariant(observation.console && Array.isArray(observation.console.errors) && Array.isArray(observation.console.warnings),
    `${scenario}: console capture is required`);
  invariant(observation.console.errors.length === 0 && observation.console.warnings.length === 0,
    `${scenario}: console errors or warnings were observed`);
  invariant(observation.viewport && Number.isInteger(observation.viewport.width) && Number.isInteger(observation.viewport.height) &&
    observation.viewport.width >= 320 && observation.viewport.height >= 240 && Number.isFinite(observation.viewport.device_scale_factor) &&
    observation.viewport.device_scale_factor > 0, `${scenario}: viewport evidence is invalid`);
  if (scenario === "responsive-1200") invariant(observation.viewport.width === 1199, `${scenario}: viewport width must exercise <1200px`);
  if (scenario === "responsive-900") invariant(observation.viewport.width === 899, `${scenario}: viewport width must exercise <900px`);
  if (mode === "browser") {
    invariant(observation.bridge?.real_engine === true && observation.bridge?.real_agent === true,
      `${scenario}: browser acceptance did not use the real Engine and Agent Workers`);
  } else {
    invariant(observation.package?.application_version === "6.0.0" && observation.package?.commit === commit &&
      observation.package?.isolated_data_root === true, `${scenario}: packaged application identity/isolation is invalid`);
  }
  const screenshotPath = inside(caseDirectory, path.resolve(caseDirectory, observation.screenshot), `${scenario} screenshot`);
  invariant(fs.existsSync(screenshotPath) && fs.statSync(screenshotPath).isFile(), `${scenario}: screenshot is missing`);
  const png = pngDimensions(screenshotPath);
  invariant(png.width >= observation.viewport.width, `${scenario}: screenshot is narrower than the recorded viewport`);
  const appUrl = normalizedUrl(observation.app_url, mode, scenario);
  const capturedAt = new Date(completed).toISOString();
  return {
    id: scenario,
    status: "PASSED",
    duration_ms: completed - started,
    assertion_count: assertions.length,
    artifacts: [
      { kind: "interaction-trace", path: observationPath, sha256: sha256(observationPath), captured_at_utc: capturedAt },
      { kind: "screenshot", path: screenshotPath, sha256: sha256(screenshotPath), captured_at_utc: capturedAt },
    ],
    details: {
      recording_schema: 1,
      surface,
      app_url: appUrl,
      viewport: observation.viewport,
      bridge: mode === "browser" ? observation.bridge : undefined,
      package: mode === "desktop" ? observation.package : undefined,
      console: { error_count: 0, warning_count: 0 },
      assertions,
    },
  };
}

export function initializeAcceptanceSession({ mode, sessionDirectory, commit, buildRoot = defaultBuildRoot() }) {
  const policy = MODES[mode];
  invariant(policy, `unsupported acceptance mode: ${mode}`);
  invariant(COMMIT.test(commit ?? ""), "acceptance session requires a full Git commit");
  const root = absolute(buildRoot, "buildRoot");
  const sessionRoot = inside(root, absolute(sessionDirectory, "sessionDirectory"), "acceptance session");
  invariant(!fs.existsSync(path.join(sessionRoot, "session.json")), `acceptance session already exists: ${sessionRoot}`);
  fs.mkdirSync(sessionRoot, { recursive: true });
  for (const scenario of policy.scenarios) fs.mkdirSync(path.join(sessionRoot, scenario), { recursive: true });
  const session = {
    schema_version: 1,
    mode,
    gate: policy.gate,
    commit,
    session_id: crypto.randomUUID(),
    started_at_utc: new Date().toISOString(),
    surface: policy.surface,
    production_data_touched: false,
  };
  fs.writeFileSync(path.join(sessionRoot, "session.json"), `${JSON.stringify(session, null, 2)}\n`, "utf8");
  const observationTemplate = {
    schema_version: 1,
    scenario: "replace-with-catalog-case-id",
    status: "NOT_RUN",
    commit,
    surface: policy.surface,
    production_data_touched: false,
    started_at_utc: null,
    completed_at_utc: null,
    app_url: mode === "browser" ? "http://127.0.0.1:5173/" : "app://astock/",
    viewport: { width: 1440, height: 900, device_scale_factor: 1 },
    ...(mode === "browser"
      ? { bridge: { real_engine: false, real_agent: false } }
      : { package: { application_version: "6.0.0", commit, isolated_data_root: false } }),
    console: { errors: ["replace with captured console entries"], warnings: ["replace with captured console entries"] },
    assertions: [
      { id: "replace-with-concrete-assertion", passed: false, expected: null, observed: null },
      { id: "replace-with-second-assertion", passed: false, expected: null, observed: null },
    ],
    screenshot: "screenshot.png",
  };
  fs.writeFileSync(path.join(sessionRoot, "observation.template.json"), `${JSON.stringify(observationTemplate, null, 2)}\n`, "utf8");
  return { session_directory: sessionRoot, scenarios: policy.scenarios };
}

export function finalizeAcceptanceSession({ sessionDirectory, outputPath, expectedCommit, buildRoot = defaultBuildRoot() }) {
  const root = absolute(buildRoot, "buildRoot");
  const sessionRoot = inside(root, absolute(sessionDirectory, "sessionDirectory"), "acceptance session");
  const output = inside(root, absolute(outputPath, "outputPath"), "acceptance evidence output");
  const { value: session, raw: sessionRaw } = readJson(path.join(sessionRoot, "session.json"), "acceptance session");
  safeObservation(sessionRaw, "session");
  assertNoSecretFields(session, "session");
  const policy = MODES[session.mode];
  invariant(policy && session.schema_version === 1 && session.gate === policy.gate && session.surface === policy.surface,
    "acceptance session policy is invalid");
  invariant(COMMIT.test(expectedCommit ?? "") && session.commit === expectedCommit, "acceptance session commit does not match the expected commit");
  invariant(typeof session.session_id === "string" && /^[0-9a-f-]{36}$/i.test(session.session_id), "acceptance session id is invalid");
  invariant(session.production_data_touched === false, "acceptance session touched production data");
  const cases = policy.scenarios.map((scenario) => validateObservation({
    mode: session.mode,
    scenario,
    caseDirectory: inside(sessionRoot, path.join(sessionRoot, scenario), `${scenario} case directory`),
    commit: expectedCommit,
    surface: policy.surface,
  }));
  const completed = new Date().toISOString();
  const evidence = {
    schema_version: 1,
    gate: policy.gate,
    status: "PASSED",
    commit: expectedCommit,
    started_at_utc: new Date(session.started_at_utc).toISOString(),
    completed_at_utc: completed,
    runner: { os: `${os.platform()} ${os.release()}`, arch: os.arch(), surface: policy.surface, session_id: session.session_id },
    secrets_in_evidence: false,
    production_data_touched: false,
    cases,
  };
  fs.mkdirSync(path.dirname(output), { recursive: true });
  fs.writeFileSync(output, `${JSON.stringify(evidence, null, 2)}\n`, { encoding: "utf8", flag: "wx" });
  return { output, gate: policy.gate, cases: cases.length, commit: expectedCommit };
}

function usage() {
  throw new Error("usage: node scripts/acceptance-evidence.mjs init <browser|desktop> <session-dir> <commit> [build-root] | finalize <session-dir> <output-json> <commit> [build-root]");
}

function main(argv) {
  const [command, first, second, third, fourth] = argv;
  if (command === "init") {
    if (!first || !second || !third) usage();
    return initializeAcceptanceSession({ mode: first, sessionDirectory: second, commit: third, buildRoot: fourth || defaultBuildRoot() });
  }
  if (command === "finalize") {
    if (!first || !second || !third) usage();
    return finalizeAcceptanceSession({ sessionDirectory: first, outputPath: second, expectedCommit: third, buildRoot: fourth || defaultBuildRoot() });
  }
  return usage();
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {
    process.stdout.write(`${JSON.stringify(main(process.argv.slice(2)))}\n`);
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  }
}
