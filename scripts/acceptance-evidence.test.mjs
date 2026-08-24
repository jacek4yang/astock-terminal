import test, { after } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import zlib from "node:zlib";

import { initializeAcceptanceSession, finalizeAcceptanceSession } from "./acceptance-evidence.mjs";
import { validateEvidence } from "./release-evidence-check.mjs";
import { BROWSER_CDP_SCENARIOS, DESKTOP_E2E_SCENARIOS } from "./release-scenarios.mjs";

const commit = "b".repeat(40);
const root = fs.mkdtempSync(path.join(os.tmpdir(), "astock-acceptance-"));
after(() => fs.rmSync(root, { recursive: true, force: true }));

const CRC32_TABLE = Array.from({ length: 256 }, (_, value) => {
  let current = value;
  for (let bit = 0; bit < 8; bit += 1) current = (current & 1) ? (0xedb88320 ^ (current >>> 1)) : (current >>> 1);
  return current >>> 0;
});

function crc32(body) {
  let value = 0xffffffff;
  for (const byte of body) value = CRC32_TABLE[(value ^ byte) & 0xff] ^ (value >>> 8);
  return (value ^ 0xffffffff) >>> 0;
}

function pngChunk(type, data) {
  const name = Buffer.from(type, "ascii");
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const checksum = Buffer.alloc(4);
  checksum.writeUInt32BE(crc32(Buffer.concat([name, data])));
  return Buffer.concat([length, name, data, checksum]);
}

function fakePng(file, width, height) {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8;
  header[9] = 0;
  const pixels = Buffer.alloc((width + 1) * height);
  fs.writeFileSync(file, Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    pngChunk("IHDR", header),
    pngChunk("IDAT", zlib.deflateSync(pixels)),
    pngChunk("IEND", Buffer.alloc(0)),
  ]));
}

function writeBrowserObservation(session, scenario, mutate = (value) => value) {
  const width = scenario === "responsive-1200" ? 1199 : scenario === "responsive-900" ? 899 : 1440;
  const directory = path.join(session, scenario);
  fakePng(path.join(directory, "screenshot.png"), width, 900);
  const value = mutate({
    schema_version: 1,
    scenario,
    status: "PASSED",
    commit,
    surface: "codex-in-app-browser",
    production_data_touched: false,
    started_at_utc: "2026-08-24T00:00:00.000Z",
    completed_at_utc: "2026-08-24T00:00:01.000Z",
    app_url: "http://127.0.0.1:5173/",
    viewport: { width, height: 900, device_scale_factor: 1 },
    bridge: { real_engine: true, real_agent: true },
    console: { errors: [], warnings: [] },
    assertions: [
      { id: "visible", passed: true, expected: "visible", observed: "visible" },
      { id: "interactive", passed: true, expected: true, observed: true },
    ],
    screenshot: "screenshot.png",
  });
  fs.writeFileSync(path.join(directory, "observation.json"), `${JSON.stringify(value, null, 2)}\n`);
}

test("finalizes all browser cases only from detailed commit-bound observations", () => {
  const session = path.join(root, "browser-good");
  initializeAcceptanceSession({ mode: "browser", sessionDirectory: session, commit, buildRoot: root });
  for (const scenario of BROWSER_CDP_SCENARIOS) writeBrowserObservation(session, scenario);
  const output = path.join(root, "browser-good.json");
  const result = finalizeAcceptanceSession({ sessionDirectory: session, outputPath: output, expectedCommit: commit, buildRoot: root });
  assert.equal(result.cases, 12);
  const evidence = JSON.parse(fs.readFileSync(output, "utf8"));
  assert.doesNotThrow(() => validateEvidence(evidence, "browser-cdp", commit));
});

test("rejects pass-only observations and Bridge-token leakage", () => {
  const weak = path.join(root, "browser-weak");
  initializeAcceptanceSession({ mode: "browser", sessionDirectory: weak, commit, buildRoot: root });
  for (const scenario of BROWSER_CDP_SCENARIOS) writeBrowserObservation(weak, scenario);
  writeBrowserObservation(weak, "market-overview", (value) => ({ ...value, assertions: [{ id: "pass", passed: true }] }));
  assert.throws(() => finalizeAcceptanceSession({
    sessionDirectory: weak, outputPath: path.join(root, "weak.json"), expectedCommit: commit, buildRoot: root,
  }), /at least two concrete assertions/);

  const leaked = path.join(root, "browser-leaked");
  initializeAcceptanceSession({ mode: "browser", sessionDirectory: leaked, commit, buildRoot: root });
  for (const scenario of BROWSER_CDP_SCENARIOS) writeBrowserObservation(leaked, scenario);
  writeBrowserObservation(leaked, "market-overview", (value) => ({ ...value, diagnostic: "bridgeToken must never be retained" }));
  assert.throws(() => finalizeAcceptanceSession({
    sessionDirectory: leaked, outputPath: path.join(root, "leaked.json"), expectedCommit: commit, buildRoot: root,
  }), /Bridge-token material/);
});

test("rejects a renamed or corrupt file masquerading as a screenshot", () => {
  const session = path.join(root, "browser-corrupt-png");
  initializeAcceptanceSession({ mode: "browser", sessionDirectory: session, commit, buildRoot: root });
  for (const scenario of BROWSER_CDP_SCENARIOS) writeBrowserObservation(session, scenario);
  const screenshot = path.join(session, "market-overview", "screenshot.png");
  const body = fs.readFileSync(screenshot);
  body[body.length - 5] ^= 0xff;
  fs.writeFileSync(screenshot, body);
  assert.throws(() => finalizeAcceptanceSession({
    sessionDirectory: session, outputPath: path.join(root, "corrupt-png.json"), expectedCommit: commit, buildRoot: root,
  }), /PNG checksum failed/);
});

test("initializes the exact 40-case packaged desktop catalog without claiming a pass", () => {
  const session = path.join(root, "desktop-session");
  const result = initializeAcceptanceSession({ mode: "desktop", sessionDirectory: session, commit, buildRoot: root });
  assert.deepEqual(result.scenarios, DESKTOP_E2E_SCENARIOS);
  assert.equal(fs.readdirSync(session, { withFileTypes: true }).filter((entry) => entry.isDirectory()).length, 40);
  assert.equal(JSON.parse(fs.readFileSync(path.join(session, "observation.template.json"), "utf8")).status, "NOT_RUN");
  assert.equal(fs.existsSync(path.join(root, "desktop-e2e.json")), false);
});
