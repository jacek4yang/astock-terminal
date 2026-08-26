// Version convergence check for the v7 production surface.
//
// v7 converges on the Rust Agent Runtime, the Rust Engine, the `astock`
// terminal adapter and the thin Tauri v2 desktop adapter. Those are the
// components that carry the release version.
//
// The Proton/CEF host and the MoonBit worker are no longer production. They
// remain in tree as historical, specification and formal-verification assets, so
// they are deliberately *not* required to track the release version any more:
// forcing a frozen archive to carry a version it never shipped would be
// misleading rather than a real gate. Their own manifests keep whatever version
// they were archived at.
//
// Usage: node scripts/release-version-check.mjs 7.0.0

import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const expected = process.argv[2] ?? "7.0.0";
const errors = [];

function read(relative) {
  return fs.readFileSync(path.join(root, relative), "utf8");
}

function requireMatch(relative, expression, description) {
  const source = read(relative);
  const match = source.match(expression);
  if (!match || match[1] !== expected) {
    errors.push(`${relative}: ${description}; actual=${match?.[1] ?? "missing"}, expected=${expected}`);
  }
}

// Frontend presentation layer.
for (const file of ["ui/package.json", "ui/package-lock.json"]) {
  const value = JSON.parse(read(file));
  if (value.version !== expected) errors.push(`${file}: version=${value.version}`);
  if (file.endsWith("package-lock.json") && value.packages?.[""]?.version !== expected) {
    errors.push(`${file}: root package version=${value.packages?.[""]?.version}`);
  }
}

// Rust workspace: one version drives every member crate.
requireMatch("Cargo.toml", /\[workspace\.package\][\s\S]*?version\s*=\s*"([^"]+)"/, "workspace version mismatch");

// Internal path dependencies must request the current workspace version, or a
// stale requirement would silently prevent the workspace from resolving.
const manifests = fs
  .readdirSync(path.join(root, "crates"), { withFileTypes: true })
  .filter((entry) => entry.isDirectory())
  .map((entry) => `crates/${entry.name}/Cargo.toml`);
for (const manifest of manifests) {
  const source = read(manifest);
  for (const [, version] of source.matchAll(/version\s*=\s*"([^"]+)",\s*path\s*=/g)) {
    if (version !== expected) {
      errors.push(`${manifest}: internal path dependency requests ${version}, expected ${expected}`);
    }
  }
}

// Thin Tauri v2 desktop adapter: the version here becomes the installer and
// executable metadata a user actually sees.
const tauri = JSON.parse(read("crates/desktop/tauri.conf.json"));
if (tauri.version !== expected) {
  errors.push(`crates/desktop/tauri.conf.json: version=${tauri.version ?? "missing"}`);
}

// Protocol service contracts shared by both adapters.
for (const file of ["protocol/schema/engine.schema.json", "protocol/schema/agent.schema.json"]) {
  const schema = JSON.parse(read(file));
  if (schema.properties?.service_version?.const !== expected) {
    errors.push(`${file}: service version=${schema.properties?.service_version?.const ?? "missing"}`);
  }
}

// The wire protocol version is independent of the product version and must not
// drift silently when the product version changes.
const envelope = JSON.parse(read("protocol/schema/envelope.schema.json"));
const protocolVersion = envelope?.$defs?.request?.properties?.protocol_version?.const;
if (protocolVersion !== 1) errors.push(`protocol version changed unexpectedly: ${protocolVersion}`);

console.log(JSON.stringify({
  ok: errors.length === 0,
  application_version: expected,
  protocol_version: protocolVersion,
  checked: [
    "ui/package.json",
    "ui/package-lock.json",
    "Cargo.toml",
    `${manifests.length} crate manifests`,
    "crates/desktop/tauri.conf.json",
    "protocol/schema/engine.schema.json",
    "protocol/schema/agent.schema.json",
  ],
  errors,
}, null, 2));
if (errors.length) process.exitCode = 1;
