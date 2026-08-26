// Architecture gate for the v7 production shape.
//
// This replaces the v6 gate, which asserted the *opposite* architecture: it
// required Proton/CEF plus the MoonBit worker to be the production orchestrator
// and failed if a Tauri adapter existed at all. The recovery map listed exactly
// that check for removal "once the Rust replacement has equivalent evidence for
// that specific behavior", and it now does:
// `crates/agent-runtime/tests/architecture.rs` enforces the dependency edges from
// the workspace manifests, including that only the desktop adapter may depend on
// Tauri and that the renderer capability grant is deny-by-default.
//
// This script covers what a Rust test cannot see: the renderer tree, the
// workflow files and cross-language version pinning.
//
// Proton, CEF and MoonBit remain in tree as historical, specification and
// formal-verification assets. They are deliberately no longer *required* to be
// the production path, and this gate no longer asserts that they are.

import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const expectedVersion = process.argv[2] ?? "7.0.0";
const failures = [];

function read(relative) {
  return fs.readFileSync(path.join(root, relative), "utf8");
}

function exists(relative) {
  return fs.existsSync(path.join(root, relative));
}

function listFiles(relative, extensions) {
  const base = path.join(root, relative);
  if (!fs.existsSync(base)) return [];
  const out = [];
  const walk = (dir) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(full);
      else if (extensions.some((ext) => entry.name.endsWith(ext))) out.push(full);
    }
  };
  walk(base);
  return out;
}

// ---------------------------------------------------------------------------
// One production Agent runtime, driven by two adapters.
// ---------------------------------------------------------------------------

if (!exists("crates/agent-runtime")) {
  failures.push("the shared Agent runtime crate is missing");
}
if (!exists("crates/astock")) {
  failures.push("the terminal adapter is missing");
}
if (!exists("crates/desktop/tauri.conf.json")) {
  failures.push("the thin Tauri v2 desktop adapter is missing");
}
if (exists("crates/agent")) {
  failures.push("the v5 Rust Agent crate is present again, which would be a second Agent runtime");
}
if (exists("src-tauri")) {
  failures.push("the v5 src-tauri tree is present again; the desktop adapter lives in crates/desktop");
}

// ---------------------------------------------------------------------------
// React is presentation only.
// ---------------------------------------------------------------------------

// The renderer must reach native capability through the bridge module, never by
// importing Tauri directly from a page or component. That indirection is what
// keeps the request-kind allowlist and envelope correlation in one place.
const rendererSources = listFiles("ui/src", [".ts", ".tsx"]).filter(
  (file) => !file.includes(`${path.sep}bridge${path.sep}`) && !file.endsWith(".test.ts") && !file.endsWith(".test.tsx"),
);
for (const file of rendererSources) {
  const source = fs.readFileSync(file, "utf8");
  if (source.includes("@tauri-apps/api")) {
    failures.push(
      `${path.relative(root, file)} imports @tauri-apps/api directly; native access belongs in ui/src/bridge`,
    );
  }
}

// The renderer must not run its own Agent tool loop or planner.
const orchestrationMarkers = ["requestDurableTool", "runToolLoop", "planResearchSteps"];
for (const file of listFiles("ui/src", [".ts", ".tsx"])) {
  const source = fs.readFileSync(file, "utf8");
  for (const marker of orchestrationMarkers) {
    if (source.includes(marker)) {
      failures.push(
        `${path.relative(root, file)} contains \`${marker}\`; orchestration belongs to the shared runtime`,
      );
    }
  }
}

// ---------------------------------------------------------------------------
// The desktop adapter owns no Agent logic.
// ---------------------------------------------------------------------------

const desktopSource = read("crates/desktop/src/main.rs");
for (const forbidden of ["ToolRegistry", "default_registry", "ModelProvider for"]) {
  if (desktopSource.includes(forbidden)) {
    failures.push(`crates/desktop/src/main.rs references \`${forbidden}\`; the adapter must not orchestrate`);
  }
}
// Release builds must not open a console window beside the GUI.
if (!desktopSource.includes('windows_subsystem = "windows"')) {
  failures.push("the desktop adapter does not suppress the Windows console subsystem in release builds");
}

// Renderer permissions stay deny-by-default.
const capability = read("crates/desktop/capabilities/default.json");
for (const forbidden of ["fs:", "shell:", "process:", "http:", "allow-execute"]) {
  if (capability.includes(forbidden)) {
    failures.push(`the desktop capability grant exposes \`${forbidden}\` to the renderer`);
  }
}

// ---------------------------------------------------------------------------
// Cross-language version pinning.
// ---------------------------------------------------------------------------

for (const file of ["protocol/schema/engine.schema.json", "protocol/schema/agent.schema.json"]) {
  const schema = JSON.parse(read(file));
  if (schema.properties?.service_version?.const !== expectedVersion) {
    failures.push(`${file}: service_version is not pinned to ${expectedVersion}`);
  }
}
const tauriConfig = JSON.parse(read("crates/desktop/tauri.conf.json"));
if (tauriConfig.version !== expectedVersion) {
  failures.push(`crates/desktop/tauri.conf.json: version is not ${expectedVersion}`);
}
const generatedTs = read("ui/src/bridge/generated.ts");
if (!generatedTs.includes(`RELEASE_VERSION = "${expectedVersion}"`)) {
  failures.push(`ui/src/bridge/generated.ts: RELEASE_VERSION is not ${expectedVersion}; run protocol/codegen.mjs`);
}

// ---------------------------------------------------------------------------
// Archived assets are preserved, not required as production.
// ---------------------------------------------------------------------------

const archived = ["app-moon", "desktop-moon", "packaging-moon", "formal"];
const missingArchives = archived.filter((entry) => !exists(entry));
if (missingArchives.length) {
  failures.push(
    `historical/specification assets were removed rather than preserved: ${missingArchives.join(", ")}`,
  );
}

console.log(
  JSON.stringify(
    {
      ok: failures.length === 0,
      production_architecture: {
        agent_runtime: "crates/agent-runtime",
        engine: "crates/engine",
        terminal_adapter: "crates/astock",
        desktop_adapter: "crates/desktop (Tauri v2)",
        renderer: "ui (React, presentation only)",
      },
      application_version: expectedVersion,
      preserved_assets: archived.filter((entry) => exists(entry)),
      renderer_files_checked: rendererSources.length,
      failures,
    },
    null,
    2,
  ),
);
if (failures.length) process.exitCode = 1;
